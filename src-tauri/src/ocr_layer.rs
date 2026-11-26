//! OCR baris-demi-baris dan penulisan ulang lapisan teks invisible.
//!
//! Gambar halaman tidak disentuh: PDF dibuka, halaman dirender hanya untuk
//! dibaca Tesseract, objek teks lama dihapus, lalu kata hasil koreksi ditulis
//! kembali dengan render mode 3 (invisible) pada bbox masing-masing.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    time::Instant,
};

use pdfium_render::prelude::*;

use crate::{
    ocr_postprocess::{BBox, Correction, OcrWord, PostprocessConfig, WordRefiner, postprocess},
    process::hide_console,
};

const TESSERACT_PSM_PAGE: &str = "3";
const TESSERACT_PSM_WORD: &str = "8";
const TESSERACT_PSM_OSD: &str = "0";
const MAX_TSV_BYTES: usize = 32 * 1024 * 1024;
/// OSD dijalankan pada thumbnail supaya biayanya cuma beberapa persen.
const OSD_MAX_DIM: usize = 3200;
const OSD_MIN_CONFIDENCE: f32 = 8.0;

/// Font Unicode untuk lapisan teks. Font standar Helvetica memakai WinAnsi;
/// karakter seperti `″` dan newline tidak punya glyph dan PDFium menggantinya
/// dengan byte `0xFF` yang terbaca sebagai `ÿ` saat diekstrak.
const TEXT_FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
    r"C:\Windows\Fonts\DejaVuSans.ttf",
];

#[derive(Debug, Clone)]
pub struct PageImage {
    pub width: usize,
    pub height: usize,
    /// RGB 8-bit, tanpa padding (P6-friendly).
    pub rgb: Vec<u8>,
}

impl PageImage {
    fn from_rgba(width: usize, height: usize, rgba: &[u8]) -> Self {
        let mut rgb = Vec::with_capacity(width * height * 3);
        for pixel in rgba.as_chunks::<4>().0 {
            rgb.extend_from_slice(&pixel[..3]);
        }
        Self { width, height, rgb }
    }

    /// Thumbnail untuk OSD. Sampel nearest cukup untuk membaca orientasi dan
    /// jauh lebih murah daripada menjalankan `--psm 0` pada render 300 DPI.
    fn osd_thumbnail(&self) -> Self {
        let scale = self
            .width
            .max(self.height)
            .max(1)
            .div_ceil(OSD_MAX_DIM)
            .max(1);
        let width = (self.width / scale).max(1);
        let height = (self.height / scale).max(1);
        let mut rgb = Vec::with_capacity(width * height * 3);
        for row in 0..height {
            let source_row = (row * self.height / height).min(self.height - 1);
            for column in 0..width {
                let source_column = (column * self.width / width).min(self.width - 1);
                let source = (source_row * self.width + source_column) * 3;
                rgb.extend_from_slice(&self.rgb[source..source + 3]);
            }
        }
        Self { width, height, rgb }
    }

    /// Rotasi searah jarum jam. Koordinat hasil OCR nanti dikembalikan ke
    /// koordinat halaman asli oleh `PageRotation`.
    fn rotated_cw(&self, degrees: u32) -> Option<Self> {
        let degrees = degrees % 360;
        if !matches!(degrees, 90 | 180 | 270) {
            return None;
        }
        let (width, height) = if degrees == 180 {
            (self.width, self.height)
        } else {
            (self.height, self.width)
        };
        let mut rgb = vec![0u8; width * height * 3];
        for destination_y in 0..height {
            for destination_x in 0..width {
                let (source_x, source_y) = match degrees {
                    90 => (destination_y, self.height - 1 - destination_x),
                    180 => (
                        self.width - 1 - destination_x,
                        self.height - 1 - destination_y,
                    ),
                    270 => (self.width - 1 - destination_y, destination_x),
                    _ => unreachable!(),
                };
                let source = (source_y * self.width + source_x) * 3;
                let destination = (destination_y * width + destination_x) * 3;
                rgb[destination..destination + 3].copy_from_slice(&self.rgb[source..source + 3]);
            }
        }
        Some(Self { width, height, rgb })
    }

    /// Crop heading diberi padding kecil di dalam batas halaman. Tanpa padding,
    /// kata di tepi halaman (mis. `LINN UR`) bisa kehilangan huruf terakhir
    /// ketika bbox Tesseract berhenti tepat di edge.
    fn crop_upscaled(&self, bbox: &BBox, upscale: u32) -> Option<Self> {
        let pad_x = ((bbox.width.max(1) as f32 * 0.12).round() as usize).max(4);
        let pad_y = ((bbox.height.max(1) as f32 * 0.12).round() as usize).max(2);
        let x = bbox.left.saturating_sub(pad_x as i32).max(0) as usize;
        let y = bbox.top.saturating_sub(pad_y as i32).max(0) as usize;
        let right = ((bbox.left as i64 + bbox.width.max(1) as i64 + pad_x as i64)
            .min(self.width as i64)
            .max(x as i64)) as usize;
        let bottom = ((bbox.top as i64 + bbox.height.max(1) as i64 + pad_y as i64)
            .min(self.height as i64)
            .max(y as i64)) as usize;
        let w = right.saturating_sub(x);
        let h = bottom.saturating_sub(y);
        if w == 0 || h == 0 {
            return None;
        }
        let scale = upscale.max(1) as usize;
        let width = w * scale;
        let height = h * scale;
        let mut rgb = Vec::with_capacity(width * height * 3);
        for row in 0..height {
            let source_row = y + row / scale;
            for column in 0..width {
                let source_column = x + column / scale;
                let source = (source_row * self.width + source_column) * 3;
                rgb.extend_from_slice(&self.rgb[source..source + 3]);
            }
        }
        Some(Self { width, height, rgb })
    }

    fn write_ppm(&self, path: &Path) -> Result<(), String> {
        let mut bytes = Vec::with_capacity(self.rgb.len() + 64);
        bytes.extend_from_slice(format!("P6\n{} {}\n255\n", self.width, self.height).as_bytes());
        bytes.extend_from_slice(&self.rgb);
        fs::write(path, bytes).map_err(|error| format!("Gagal menulis crop OCR: {error}"))
    }
}

/// Informasi rotasi yang dipakai untuk mengembalikan bbox OCR ke koordinat
/// halaman asli. `width`/`height` adalah ukuran gambar sebelum diputar.
#[derive(Debug, Clone, Copy)]
struct PageRotation {
    degrees: u32,
    width: i32,
    height: i32,
}

impl PageRotation {
    fn unrotate_bbox(&self, bbox: &BBox) -> BBox {
        match self.degrees {
            90 => BBox {
                left: bbox.top,
                top: self.height - bbox.left - bbox.width,
                width: bbox.height,
                height: bbox.width,
            },
            180 => BBox {
                left: self.width - bbox.left - bbox.width,
                top: self.height - bbox.top - bbox.height,
                width: bbox.width,
                height: bbox.height,
            },
            270 => BBox {
                left: self.width - bbox.top - bbox.height,
                top: bbox.left,
                width: bbox.height,
                height: bbox.width,
            },
            _ => *bbox,
        }
    }
}

pub struct Tesseract {
    executable: PathBuf,
    language: String,
}

impl Tesseract {
    pub fn new(executable: PathBuf, language: String) -> Self {
        Self {
            executable,
            language,
        }
    }

    fn run(&self, image: &PageImage, psm: &str, temp: &Path) -> Result<String, String> {
        let input = temp.with_extension("ppm");
        image.write_ppm(&input)?;
        let mut command = Command::new(&self.executable);
        hide_console(&mut command);
        let output = command
            .arg(&input)
            .arg("stdout")
            .args(["-l", &self.language, "--psm", psm, "tsv"])
            .output()
            .map_err(|error| format!("Gagal menjalankan Tesseract: {error}"))?;
        let _ = fs::remove_file(&input);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Tesseract gagal: {}",
                stderr.trim().chars().take(300).collect::<String>()
            ));
        }
        let bounded = &output.stdout[..output.stdout.len().min(MAX_TSV_BYTES)];
        Ok(String::from_utf8_lossy(bounded).into_owned())
    }

    /// Deteksi orientasi halaman lewat Tesseract OSD (`--psm 0`) pada thumbnail.
    /// Mengembalikan derajat rotasi searah jarum jam yang dibutuhkan supaya teks
    /// tegak, plus confidence-nya.
    fn orientation(&self, image: &PageImage, temp: &Path) -> Option<(u32, f32)> {
        let input = temp.join("orientation.ppm");
        image.osd_thumbnail().write_ppm(&input).ok()?;
        let mut command = Command::new(&self.executable);
        hide_console(&mut command);
        let output = command
            .arg(&input)
            .arg("stdout")
            .args(["--psm", TESSERACT_PSM_OSD])
            .output()
            .ok()?;
        let _ = fs::remove_file(&input);
        if !output.status.success() {
            return None;
        }
        let mut rotate = None;
        let mut confidence = None;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(value) = line
                .strip_prefix("Rotate:")
                .or_else(|| line.strip_prefix("Rotate image:"))
            {
                rotate = value.trim().parse::<u32>().ok();
            } else if let Some(value) = line.strip_prefix("Orientation confidence:") {
                confidence = value.trim().parse::<f32>().ok();
            }
        }
        let degrees = rotate?;
        if !matches!(degrees, 0 | 90 | 180 | 270) {
            return None;
        }
        let confidence = confidence.unwrap_or(0.0);
        (confidence >= OSD_MIN_CONFIDENCE).then_some((degrees, confidence))
    }
}

/// Parsing TSV level 5 (word) dari Tesseract. Koordinat masih dalam piksel.
pub fn parse_tsv(tsv: &str, page: usize) -> Vec<OcrWord> {
    let mut headers = HashMap::new();
    let mut words = Vec::new();
    for (line_number, line) in tsv.lines().enumerate() {
        if line_number == 0 {
            for (index, name) in line.split('\t').enumerate() {
                headers.insert(name.trim().to_string(), index);
            }
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let get = |name: &str| -> Option<usize> {
            headers
                .get(name)
                .and_then(|index| fields.get(*index))?
                .parse()
                .ok()
        };
        let Some(level) = get("level") else {
            continue;
        };
        if level != 5 {
            continue;
        }
        let Some(text) = headers
            .get("text")
            .and_then(|index| fields.get(*index))
            .map(|value| value.trim().to_string())
        else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        let confidence = headers
            .get("conf")
            .and_then(|index| fields.get(*index))
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(0.0);
        words.push(OcrWord {
            page,
            block: get("block_num").unwrap_or(0),
            paragraph: get("par_num").unwrap_or(0),
            line: get("line_num").unwrap_or(0),
            word: get("word_num").unwrap_or(0),
            bbox: BBox {
                left: get("left").unwrap_or(0) as i32,
                top: get("top").unwrap_or(0) as i32,
                width: get("width").unwrap_or(0) as i32,
                height: get("height").unwrap_or(0) as i32,
            },
            text,
            confidence,
        });
    }
    words
}

fn orient_page(
    tesseract: &Tesseract,
    image: PageImage,
    temp: &Path,
    config: &PostprocessConfig,
    page: usize,
    log: &mut Vec<Correction>,
) -> (PageImage, Option<PageRotation>) {
    if !config.enabled || !config.rotation_pass {
        return (image, None);
    }
    let Some((degrees, _confidence)) = tesseract.orientation(&image, temp) else {
        return (image, None);
    };
    if degrees == 0 {
        return (image, None);
    }
    let Some(rotated) = image.rotated_cw(degrees) else {
        return (image, None);
    };
    log.push(Correction {
        page,
        before: "0°".to_string(),
        after: format!("{degrees}°"),
        rule: "page-rotation",
    });
    (
        rotated,
        Some(PageRotation {
            degrees,
            width: image.width as i32,
            height: image.height as i32,
        }),
    )
}

struct CropRefiner {
    tesseract: Tesseract,
    images: HashMap<usize, PageImage>,
    temp: PathBuf,
    refine_calls: AtomicUsize,
    refine_millis: AtomicU64,
}

fn timing_enabled() -> bool {
    std::env::var("DOCUMORPH_TIMING").is_ok()
}

fn print_timing(label: &str, duration: std::time::Duration) {
    if timing_enabled() {
        println!("[timing] {label}: {:.2}s", duration.as_secs_f32());
    }
}

impl WordRefiner for CropRefiner {
    fn refine(&self, page: usize, bbox: &BBox, upscale: u32) -> Option<(String, f32)> {
        let started = Instant::now();
        let result = (|| {
            let image = self.images.get(&page)?.crop_upscaled(bbox, upscale)?;
            let tsv = self
                .tesseract
                .run(&image, TESSERACT_PSM_WORD, &self.temp)
                .ok()?;
            let candidates = parse_tsv(&tsv, page);
            let best = candidates
                .into_iter()
                .filter(|word| word.confidence >= 0.0)
                .max_by(|a, b| a.confidence.total_cmp(&b.confidence))?;
            Some((best.text, best.confidence))
        })();
        self.refine_calls.fetch_add(1, Ordering::Relaxed);
        self.refine_millis
            .fetch_add(started.elapsed().as_millis() as u64, Ordering::Relaxed);
        result
    }
}

pub struct LayerRewrite {
    pub word_count: usize,
    pub page_count: usize,
    pub corrections: Vec<Correction>,
}

/// Baca OCR, koreksi kata, hapus text layer lama, tulis ulang sebagai text
/// invisible per kata. File `path` ditimpa secara atomik.
pub fn rewrite_searchable_layer(
    pdfium: &Pdfium,
    path: &Path,
    tesseract: Tesseract,
    config: &PostprocessConfig,
    cancel: &AtomicBool,
    temp_dir: &Path,
) -> Result<LayerRewrite, String> {
    let mut document = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|error| format!("PDF hasil OCR tidak dapat dibuka: {error}"))?;
    let page_count = document.pages().len() as usize;
    if page_count == 0 {
        return Err("PDF hasil OCR tidak memiliki halaman.".into());
    }

    let mut images = HashMap::new();
    let mut rotations = HashMap::new();
    let mut rotation_log = Vec::new();
    let mut words = Vec::new();
    let mut render_total = 0.0f32;
    let mut convert_total = 0.0f32;
    let mut osd_total = 0.0f32;
    let mut ocr_total = 0.0f32;
    for index in 0..page_count {
        if cancel.load(Ordering::SeqCst) {
            return Err("Konversi dibatalkan.".into());
        }
        let page = document
            .pages()
            .get(index as i32)
            .map_err(|error| format!("Gagal membaca halaman {}: {error}", index + 1))?;
        let page_width = page.width().value.max(1.0);
        let page_height = page.height().value.max(1.0);
        let dpi = config.render_dpi.max(300);
        let target_width = ((page_width / 72.0) * dpi as f32).round().max(1.0) as usize;
        let target_height = ((page_height / 72.0) * dpi as f32).round().max(1.0) as usize;
        let started = Instant::now();
        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new().set_target_size(target_width as i32, target_height as i32),
            )
            .map_err(|error| format!("Gagal merender halaman {}: {error}", index + 1))?;
        render_total += started.elapsed().as_secs_f32();
        let started = Instant::now();
        let rgba = bitmap.as_rgba_bytes();
        let image = PageImage::from_rgba(target_width, target_height, &rgba);
        convert_total += started.elapsed().as_secs_f32();
        let started = Instant::now();
        let (image, rotation) = orient_page(
            &tesseract,
            image,
            temp_dir,
            config,
            index + 1,
            &mut rotation_log,
        );
        osd_total += started.elapsed().as_secs_f32();
        if let Some(rotation) = rotation {
            rotations.insert(index + 1, rotation);
        }
        let started = Instant::now();
        let tsv = tesseract.run(&image, TESSERACT_PSM_PAGE, temp_dir)?;
        ocr_total += started.elapsed().as_secs_f32();
        words.extend(parse_tsv(&tsv, index + 1));
        images.insert(index + 1, image);
    }
    if timing_enabled() {
        println!(
            "[timing] render={render_total:.2}s rgba->rgb={convert_total:.2}s osd={osd_total:.2}s ocr-full={ocr_total:.2}s"
        );
    }

    let refiner = CropRefiner {
        tesseract,
        images,
        temp: temp_dir.to_path_buf(),
        refine_calls: AtomicUsize::new(0),
        refine_millis: AtomicU64::new(0),
    };
    let started = Instant::now();
    let (words, postprocess_corrections) = postprocess(words, config, Some(&refiner));
    print_timing("postprocess-total", started.elapsed());
    if timing_enabled() {
        println!(
            "[timing] refine calls={} total={:.2}s",
            refiner.refine_calls.load(Ordering::Relaxed),
            refiner.refine_millis.load(Ordering::Relaxed) as f32 / 1000.0
        );
    }
    let word_count = words.len();
    let mut corrections = rotation_log;
    corrections.extend(postprocess_corrections);

    let staged = temp_dir.join("searchable-layer.pdf");
    let write_started = Instant::now();
    let mut regenerate_total = 0.0f32;
    let mut remove_old_total = 0.0f32;
    let mut create_object_total = 0.0f32;
    let mut visible_cleanup_total = 0.0f32;
    {
        for index in 0..page_count {
            if cancel.load(Ordering::SeqCst) {
                return Err("Konversi dibatalkan.".into());
            }
            // Baseline dan ukuran font disamakan per baris Tesseract. Bila tiap
            // kata memakai tinggi sendiri, ekstraktor mengira ada baris baru di
            // tengah kalimat (mis. "performance \ntool").
            let mut line_bottom: HashMap<(usize, usize, usize), i32> = HashMap::new();
            let mut line_height: HashMap<(usize, usize, usize), i32> = HashMap::new();
            let rotation = rotations.get(&(index + 1));
            let bbox_of = |word: &OcrWord| -> BBox {
                rotation
                    .map(|value| value.unrotate_bbox(&word.bbox))
                    .unwrap_or(word.bbox)
            };
            for word in words.iter().filter(|word| word.page == index + 1) {
                let key = (word.block, word.paragraph, word.line);
                let bbox = bbox_of(word);
                line_bottom
                    .entry(key)
                    .and_modify(|bottom| *bottom = (*bottom).max(bbox.top + bbox.height))
                    .or_insert(bbox.top + bbox.height);
                line_height
                    .entry(key)
                    .and_modify(|height| *height = (*height).max(bbox.height))
                    .or_insert(bbox.height);
            }
            let unicode_font = text_font(&mut document);
            let ascii_font = document.fonts_mut().helvetica();
            let mut page = document.pages_mut().get(index as i32).map_err(|error| {
                format!(
                    "Gagal membuka halaman {} untuk penulisan: {error}",
                    index + 1
                )
            })?;
            // Default pdfium-render adalah regenerate content Setiap perubahan objek.
            // Pada scan 300 DPI itu berarti ~730× regenerate content stream hanya
            // untuk menulis 730 kata. Kita stage semua perubahan dan regenerate
            // sekali di bawah, hasil PDF identik.
            page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
            let page_width = page.width().value.max(1.0);
            let page_height = page.height().value.max(1.0);
            let render_width = refiner_width(&page, config.render_dpi.max(300));
            let render_height = refiner_height(&page, config.render_dpi.max(300));

            let old_text: Vec<usize> = page
                .objects()
                .iter()
                .enumerate()
                .filter_map(|(object_index, object)| object.as_text_object().map(|_| object_index))
                .collect();
            let remove_old_started = Instant::now();
            for object_index in old_text.into_iter().rev() {
                page.objects_mut()
                    .remove_object_at_index(object_index)
                    .map_err(|error| {
                        format!("Gagal menghapus teks lama halaman {}: {error}", index + 1)
                    })?;
            }
            remove_old_total += remove_old_started.elapsed().as_secs_f32();

            for word in words.iter().filter(|word| word.page == index + 1) {
                let scale_x = page_width / render_width as f32;
                let scale_y = page_height / render_height as f32;
                let bbox = bbox_of(word);
                // Rotasi 90/270 mengubah baris Tesseract menjadi kolom pada
                // halaman asli. Baseline/lebar per baris tidak lagi relevan:
                // tiap kata memakai bbox hasil transformasi balik masing-masing.
                let rotated_axis = rotation.is_some_and(|value| matches!(value.degrees, 90 | 270));
                let left = bbox.left as f32 * scale_x;
                let span = if rotated_axis {
                    bbox.height
                } else {
                    bbox.width
                };
                let width = (span.max(1) as f32 * scale_x).max(0.5);
                let line_key = (word.block, word.paragraph, word.line);
                let line_bottom = line_bottom
                    .get(&line_key)
                    .copied()
                    .unwrap_or(bbox.top + bbox.height);
                let line_height = line_height.get(&line_key).copied().unwrap_or(bbox.height);
                let bottom = if rotated_axis {
                    bbox.top + bbox.height
                } else {
                    line_bottom
                };
                let baseline = (page_height - bottom as f32 * scale_y).max(0.0);
                let glyph_height = if rotated_axis {
                    bbox.width
                } else {
                    line_height
                };
                let font_size = (glyph_height.max(1) as f32 * scale_y * 0.85).max(1.0);
                // Jangan menaruh `\n` di dalam string text object: font WinAnsi
                // tidak punya glyph newline sehingga PDFium menulis 0xFF (`ÿ`).
                // Spasi aman; pemisah baris tetap dibaca dari posisi kata.
                let text = format!("{} ", word.text);
                let font = if !word.text.is_ascii() {
                    unicode_font
                } else {
                    ascii_font
                };
                let create_object_started = Instant::now();
                let mut object = page
                    .objects_mut()
                    .create_text_object(
                        PdfPoints::ZERO,
                        PdfPoints::ZERO,
                        &text,
                        font,
                        PdfPoints::new(font_size),
                    )
                    .map_err(|error| {
                        format!("Gagal menulis kata halaman {}: {error}", index + 1)
                    })?;
                if let Some(text) = object.as_text_object_mut() {
                    text.set_render_mode(PdfPageTextRenderMode::Invisible)
                        .map_err(|error| {
                            format!(
                                "Gagal mengatur render mode invisible halaman {}: {error}",
                                index + 1
                            )
                        })?;
                }
                let estimated = text.chars().count() as f32 * font_size * 0.5;
                let horizontal_scale = if estimated > 0.0 {
                    (width / estimated).clamp(0.4, 2.5)
                } else {
                    1.0
                };
                object
                    .apply_matrix(PdfMatrix::new(
                        horizontal_scale,
                        0.0,
                        0.0,
                        1.0,
                        left,
                        baseline,
                    ))
                    .map_err(|error| {
                        format!("Gagal mengatur bbox kata halaman {}: {error}", index + 1)
                    })?;
                create_object_total += create_object_started.elapsed().as_secs_f32();
            }

            // Pengaman terakhir: semua objek teks di halaman harus invisible.
            // Objek yang tetap visible (misalnya sisa lapisan OCRmyPDF) dibuang.
            let visible_cleanup_started = Instant::now();
            let mut visible_text: Vec<usize> = Vec::new();
            for (object_index, object) in page.objects().iter().enumerate() {
                if let Some(text) = object.as_text_object()
                    && text.is_visible()
                {
                    visible_text.push(object_index);
                }
            }
            for object_index in visible_text.into_iter().rev() {
                page.objects_mut()
                    .remove_object_at_index(object_index)
                    .map_err(|error| {
                        format!("Gagal membuang teks visible halaman {}: {error}", index + 1)
                    })?;
            }
            visible_cleanup_total += visible_cleanup_started.elapsed().as_secs_f32();
            let regenerate_started = Instant::now();
            page.regenerate_content().map_err(|error| {
                format!("Gagal menyegarkan konten halaman {}: {error}", index + 1)
            })?;
            regenerate_total += regenerate_started.elapsed().as_secs_f32();
        }
        let save_started = Instant::now();
        document
            .save_to_file(&staged)
            .map_err(|error| format!("Gagal menyimpan PDF dengan lapisan teks baru: {error}"))?;
        print_timing("save", save_started.elapsed());
    }
    print_timing("write-layer", write_started.elapsed());
    if timing_enabled() {
        println!(
            "[timing] remove-old={remove_old_total:.2}s create-objects={create_object_total:.2}s visible-cleanup={visible_cleanup_total:.2}s regenerate={regenerate_total:.2}s"
        );
    }

    let rename_started = Instant::now();
    fs::rename(&staged, path)
        .or_else(|_| {
            fs::copy(&staged, path)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .map_err(|error| format!("Gagal mengganti PDF hasil OCR: {error}"))?;
    print_timing("rename", rename_started.elapsed());
    let _ = fs::remove_file(&staged);

    Ok(LayerRewrite {
        word_count,
        page_count,
        corrections,
    })
}

/// Tesseract TSV tidak punya token spasi. Pemisah mengikuti geometri bbox,
/// bukan `block_num`/`line_num`, supaya kata kapital dan baris lanjutan bullet
/// tetap terpisah saat di-copy.
pub fn word_separator(words: &[OcrWord], index: usize) -> &'static str {
    let Some(word) = words.get(index) else {
        return "\n";
    };
    let Some(next) = words.get(index + 1) else {
        return "\n";
    };
    if next.page != word.page {
        return "\n";
    }
    let top = word.bbox.top.max(next.bbox.top);
    let bottom = (word.bbox.top + word.bbox.height).min(next.bbox.top + next.bbox.height);
    if bottom - top >= word.bbox.height.min(next.bbox.height) / 2 {
        " "
    } else {
        "\n"
    }
}

/// Rekonstruksi teks persis seperti yang ditulis ke text layer. Dipakai test
/// metrik supaya CER/WER mengukur apa yang benar-benar di-copy user.
#[allow(dead_code)]
pub fn words_to_text(words: &[OcrWord]) -> String {
    words
        .iter()
        .enumerate()
        .map(|(index, word)| format!("{}{}", word.text, word_separator(words, index)))
        .collect()
}

/// Pilih font TrueType/CID yang mampu meng-encode `″` sebagai U+2033.
/// Fallback Helvetica hanya dipakai bila tidak ada font sistem yang bisa dimuat.
fn text_font(document: &mut PdfDocument<'_>) -> PdfFontToken {
    for candidate in TEXT_FONT_CANDIDATES {
        if !Path::new(candidate).exists() {
            continue;
        }
        if let Ok(font) = document
            .fonts_mut()
            .load_true_type_from_file(candidate, true)
        {
            return font;
        }
    }
    document.fonts_mut().helvetica()
}

fn refiner_width(page: &PdfPage<'_>, dpi: u32) -> usize {
    ((page.width().value / 72.0) * dpi.max(300) as f32)
        .round()
        .max(1.0) as usize
}

fn refiner_height(page: &PdfPage<'_>, dpi: u32) -> usize {
    ((page.height().value / 72.0) * dpi.max(300) as f32)
        .round()
        .max(1.0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ocr_metrics, ocr_postprocess::postprocess_stages};

    #[test]
    fn tsv_word_rows_are_parsed_with_bbox_and_confidence() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
5\t1\t1\t1\t1\t1\t100\t200\t30\t20\t89.334503\tLmnSequencer\n\
5\t1\t1\t1\t1\t2\t140\t200\t20\t20\t93.510422\t32\n";
        let words = parse_tsv(tsv, 1);
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "LmnSequencer");
        assert_eq!(
            words[0].bbox,
            BBox {
                left: 100,
                top: 200,
                width: 30,
                height: 20
            }
        );
        assert!((words[1].confidence - 93.510_42).abs() < 0.001);
    }

    #[test]
    fn rotated_page_bbox_returns_to_original_coordinates() {
        let original = BBox {
            left: 1,
            top: 1,
            width: 2,
            height: 1,
        };
        let info = PageRotation {
            width: 4,
            height: 3,
            degrees: 90,
        };
        let rotated_90 = BBox {
            left: 1,
            top: 1,
            width: 1,
            height: 2,
        };
        assert_eq!(info.unrotate_bbox(&rotated_90), original);

        let info = PageRotation {
            degrees: 180,
            ..info
        };
        let rotated_180 = BBox {
            left: 1,
            top: 1,
            width: 2,
            height: 1,
        };
        assert_eq!(info.unrotate_bbox(&rotated_180), original);

        let info = PageRotation {
            degrees: 270,
            ..info
        };
        let rotated_270 = BBox {
            left: 1,
            top: 1,
            width: 1,
            height: 2,
        };
        assert_eq!(info.unrotate_bbox(&rotated_270), original);
    }

    #[test]
    fn page_image_rotates_cw_and_keeps_dimensions() {
        let mut rgb = vec![255u8; 4 * 3 * 3];
        rgb[0] = 10; // piksel kiri-atas asli
        let image = PageImage {
            width: 4,
            height: 3,
            rgb,
        };
        let rotated = image.rotated_cw(90).expect("rotasi 90");
        assert_eq!((rotated.width, rotated.height), (3, 4));
        // (0,0) asli berpindah ke kanan-atas setelah rotasi 90° CW.
        assert_eq!(rotated.rgb[2 * 3], 10);
        let back = rotated.rotated_cw(270).expect("rotasi balik");
        assert_eq!((back.width, back.height), (4, 3));
        assert_eq!(back.rgb[0], 10);
    }

    fn sample_pdf_path() -> String {
        std::env::var("DOCUMORPH_OCR_SAMPLE_PDF").unwrap_or_else(|_| {
            r"C:\Data\01_Development\Projek\documorph-test-docs\generated\image-only-no-text.pdf"
                .to_string()
        })
    }

    fn tesseract_path() -> PathBuf {
        PathBuf::from(
            std::env::var("TESSERACT_PATH")
                .unwrap_or_else(|_| r"C:\Program Files\Tesseract-OCR\tesseract.exe".to_string()),
        )
    }

    fn pdfium_for_test() -> Pdfium {
        let dll = Path::new(env!("CARGO_MANIFEST_DIR")).join("bin/pdfium.dll");
        Pdfium::new(Pdfium::bind_to_library(&dll).expect("PDFium DLL"))
    }

    fn render_bytes(pdfium: &Pdfium, path: &Path, width: i32, height: i32) -> Vec<u8> {
        let document = pdfium.load_pdf_from_file(path, None).expect("PDF render");
        let page = document.pages().get(0).expect("halaman render");
        page.render_with_config(&PdfRenderConfig::new().set_target_size(width, height))
            .expect("render")
            .as_rgba_bytes()
            .to_vec()
    }

    fn command_text(program: &str, args: &[&str]) -> String {
        let output = Command::new(program)
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("jalankan {program}: {error}"));
        assert!(
            output.status.success(),
            "{program} gagal: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn assert_layer_quality(pdfium: &Pdfium, path: &Path, rewrite: &LayerRewrite) {
        let document = pdfium.load_pdf_from_file(path, None).expect("output pdf");
        let page = document.pages().get(0).expect("first page");
        let text = page.text().expect("text page").all();
        assert!(!text.contains('\u{00ff}'), "masih ada U+00FF: {text}");
        assert!(
            !text.contains("performance \ntool"),
            "newline nyasar di tengah baris: {text}"
        );
        assert!(text.contains("Sequencer"), "text: {text}");
        assert!(text.contains("3½"), "text: {text}");
        // Logo `linn` tidak boleh menempel pada alamat.
        assert!(!text.contains("linn18720"), "text: {text}");
        assert!(text.contains("UR"), "text: {text}");
        // Urutan copy-paste mengikuti judul → subjudul → isi → pecahan.
        let title = text.find("The LinnSequencer").expect("judul");
        let subtitle = text.find("Sequence Recorder").expect("subjudul");
        let fraction = text.find("3½").expect("pecahan");
        assert!(
            title < subtitle && subtitle < fraction,
            "posisi title={title} subtitle={subtitle} fraction={fraction}; \
             koreksi={:?}; text: {text}",
            rewrite.corrections
        );
        // Pencarian PDFium tetap menemukan kata kunci, termasuk "function"
        // dari kemunculan lain (fragmen `func-`/`tion.` sengaja disisakan).
        let text_page = page.text().expect("text page untuk pencarian");
        for needle in ["Sequencer", "3½", "function", "is"] {
            let found = match text_page.search(needle, &PdfSearchOptions::new()) {
                Ok(search) => search.find_next().is_some(),
                Err(_) => false,
            };
            assert!(found, "pencarian '{needle}' gagal; text: {text}");
        }

        // Ekstraktor pihak ketiga memakai ToUnicode/font encoding, jadi ini
        // menangkap sumber `ÿ` yang tidak terlihat dari `PdfPageText::all()`.
        let path_arg = path.to_string_lossy().into_owned();
        let pdftotext = std::env::var("DOCUMORPH_PDFTOTEXT")
            .unwrap_or_else(|_| r"C:\Data\01_Development\Git\mingw64\bin\pdftotext.exe".into());
        let extracted = command_text(&pdftotext, &["-enc", "UTF-8", &path_arg, "-"]);
        assert!(
            !extracted.contains('\u{00ff}'),
            "pdftotext mengandung U+00FF: {extracted}"
        );
        let python = std::env::var("DOCUMORPH_PYTHON")
            .unwrap_or_else(|_| r"C:\Data\01_Development\python3.10.11\python.exe".to_string());
        let pymupdf = command_text(
            &python,
            &[
                "-c",
                "import pymupdf,sys; sys.stdout.buffer.write(pymupdf.open(sys.argv[1])[0].get_text().encode('utf-8'))",
                &path_arg,
            ],
        );
        assert!(
            !pymupdf.contains('\u{00ff}'),
            "PyMuPDF mengandung U+00FF: {pymupdf}"
        );
        assert!(
            pymupdf.contains('\u{2033}') && extracted.contains('\u{2033}'),
            "tanda inci tidak ter-encode sebagai U+2033; pdftotext: {extracted}; PyMuPDF: {pymupdf}"
        );
        // Pdfium `text().all()` mengganti '-' dengan U+0002 bila font Helvetica
        // dan CID dicampur, jadi fragmen hyphen diverifikasi lewat PyMuPDF.
        assert!(pymupdf.contains("func-"), "PyMuPDF: {pymupdf}");
        assert!(pymupdf.contains("tion."), "PyMuPDF: {pymupdf}");
        assert!(!pymupdf.contains("function tion"), "PyMuPDF: {pymupdf}");
        assert!(!pymupdf.contains("functiontion"), "PyMuPDF: {pymupdf}");
        let title = pymupdf.find("The LinnSequencer").expect("judul PyMuPDF");
        let hyphen = pymupdf.find("func-").expect("hyphen PyMuPDF");
        assert!(title < hyphen, "urutan PyMuPDF: {title} vs {hyphen}");

        let mut text_objects = 0usize;
        for object in page.objects().iter() {
            if let Some(text) = object.as_text_object() {
                text_objects += 1;
                assert!(!text.is_visible());
            }
        }
        assert!(text_objects > 100);
        assert!(rewrite.word_count > 100);
    }

    #[test]
    fn every_word_gets_a_separator_including_all_caps() {
        let make = |text: &str, top: i32, left: i32| OcrWord {
            page: 1,
            block: 1,
            paragraph: 1,
            line: 1,
            word: 1,
            bbox: BBox {
                left,
                top,
                width: 20,
                height: 10,
            },
            text: text.to_string(),
            confidence: 95.0,
        };
        let words = vec![
            make("RECORD", 10, 10),
            make("LOCATE", 10, 40),
            make("CREATE", 10, 70),
            make("SONG", 10, 100),
            make("SMPTE", 40, 10),
        ];
        assert_eq!(word_separator(&words, 0), " ");
        assert_eq!(word_separator(&words, 3), "\n");
        assert_eq!(words_to_text(&words), "RECORD LOCATE CREATE SONG\nSMPTE\n");
    }

    fn grayscale(image: &PageImage) -> PageImage {
        let rgb = image
            .rgb
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|pixel| {
                let value = ((pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32) / 3) as u8;
                [value, value, value]
            })
            .collect();
        PageImage {
            width: image.width,
            height: image.height,
            rgb,
        }
    }

    fn binarized(image: &PageImage, threshold: u8) -> PageImage {
        let mut result = grayscale(image);
        for pixel in result.rgb.as_chunks_mut::<3>().0 {
            let value = if pixel[0] < threshold { 0 } else { 255 };
            pixel.copy_from_slice(&[value, value, value]);
        }
        result
    }

    fn denoised(image: &PageImage) -> PageImage {
        let gray = grayscale(image);
        let mut rgb = Vec::with_capacity(gray.rgb.len());
        for row in 0..gray.height {
            for column in 0..gray.width {
                let mut values = Vec::with_capacity(9);
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let y = (row as i32 + dy).clamp(0, gray.height as i32 - 1) as usize;
                        let x = (column as i32 + dx).clamp(0, gray.width as i32 - 1) as usize;
                        values.push(gray.rgb[(y * gray.width + x) * 3]);
                    }
                }
                values.sort_unstable();
                let value = values[values.len() / 2];
                rgb.extend_from_slice(&[value, value, value]);
            }
        }
        PageImage {
            width: gray.width,
            height: gray.height,
            rgb,
        }
    }

    fn upscaled_nearest(image: &PageImage, scale: usize) -> PageImage {
        let width = image.width * scale;
        let height = image.height * scale;
        let mut rgb = Vec::with_capacity(width * height * 3);
        for row in 0..height {
            for column in 0..width {
                let source = ((row / scale) * image.width + column / scale) * 3;
                rgb.extend_from_slice(&image.rgb[source..source + 3]);
            }
        }
        PageImage { width, height, rgb }
    }

    /// Diagnosa cepat urutan layout dari TSV sample yang sudah ada. Tidak
    /// merender PDF dan tidak memanggil Tesseract, jadi aktif dalam hitungan
    /// detik:
    /// `cargo test --manifest-path src-tauri/Cargo.toml --lib -- --ignored --nocapture layout_diag`
    #[test]
    #[ignore = "diagnosa urutan layout"]
    fn layout_diag_prints_token_order_from_sample_tsv() {
        let tsv = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/linn-sequencer.raw.tsv"),
        )
        .expect("fixture TSV");
        let words = parse_tsv(&tsv, 1);
        let config = PostprocessConfig {
            dictionary_pass: false,
            ..PostprocessConfig::default()
        };
        let (before, _) = postprocess(
            words.clone(),
            &PostprocessConfig {
                layout_pass: false,
                ..config.clone()
            },
            None,
        );
        let (after, _) = postprocess(words, &config, None);
        let before: Vec<String> = before.iter().map(|word| word.text.clone()).collect();
        let after: Vec<String> = after.iter().map(|word| word.text.clone()).collect();
        println!("sebelum (80): {}", before[..80.min(before.len())].join(" "));
        println!("sesudah (80): {}", after[..80.min(after.len())].join(" "));
        for needle in [
            "LinnSequencer",
            "professional",
            "RECORD",
            "32",
            "technology",
            "dealer",
            "demonstration",
            "linn",
            "18720",
            "LINN",
            "UR",
            "func-",
        ] {
            println!(
                "{needle}: sebelum={:?} sesudah={:?}",
                before.iter().position(|text| text == needle),
                after.iter().position(|text| text == needle)
            );
        }
        for index in 400..after.len().min(470) {
            if before.get(index) != after.get(index) {
                println!(
                    "indeks {index}: sebelum={:?} sesudah={:?}",
                    before.get(index),
                    after.get(index)
                );
            }
        }
        println!(
            "ekor sebelum: {}",
            before[before.len().saturating_sub(45)..].join(" ")
        );
        println!(
            "ekor sesudah: {}",
            after[after.len().saturating_sub(45)..].join(" ")
        );
    }

    #[test]
    fn layout_keeps_sample_reading_order() {
        let tsv = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/linn-sequencer.raw.tsv"),
        )
        .expect("fixture TSV");
        let words = parse_tsv(&tsv, 1);
        let config = PostprocessConfig {
            dictionary_pass: false,
            ..PostprocessConfig::default()
        };
        let (before, _) = postprocess(
            words.clone(),
            &PostprocessConfig {
                layout_pass: false,
                ..config.clone()
            },
            None,
        );
        let (after, _) = postprocess(words, &config, None);
        let limit = 80.min(before.len()).min(after.len());
        let before: Vec<&str> = before[..limit]
            .iter()
            .map(|word| word.text.as_str())
            .collect();
        let after: Vec<&str> = after[..limit]
            .iter()
            .map(|word| word.text.as_str())
            .collect();
        assert_eq!(before, after, "layout mengubah urutan baca sample");
    }

    /// Diagnosa ground truth. Dijalankan manual:
    /// `cargo test --lib -- --ignored ground_truth_metrics`
    ///
    /// Mencetak CER/WER sebelum vs sesudah tiap langkah. Langkah yang menaikkan
    /// CER body text membuat test ini gagal, jadi tidak boleh di-merge.
    #[test]
    #[ignore = "butuh Tesseract + PDF sample + ground truth"]
    fn ground_truth_metrics_track_preprocessing_and_each_stage() {
        let pdfium = pdfium_for_test();
        let temp = tempfile::tempdir().expect("tempdir");
        let document = pdfium
            .load_pdf_from_file(&sample_pdf_path(), None)
            .expect("PDF sample");
        let page = document.pages().get(0).expect("halaman pertama");
        let dpi = PostprocessConfig::default().render_dpi.max(300);
        let width = refiner_width(&page, dpi);
        let height = refiner_height(&page, dpi);
        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new().set_target_size(width as i32, height as i32),
            )
            .expect("render 300 DPI");
        let raw = PageImage::from_rgba(width, height, &bitmap.as_rgba_bytes());

        let reference = ocr_metrics::full_reference();
        let body_reference = ocr_metrics::body_reference();
        let variants = [
            ("raw-300", raw.clone()),
            ("gray-300", grayscale(&raw)),
            ("binary-300", binarized(&raw, 160)),
            ("denoise-300", denoised(&raw)),
            ("upscale-2x", upscaled_nearest(&raw, 2)),
        ];
        let mut raw_words = Vec::new();
        for (name, image) in variants {
            let engine = Tesseract::new(tesseract_path(), "ind+eng".to_string());
            let tsv = engine
                .run(&image, TESSERACT_PSM_PAGE, temp.path())
                .expect("TSV OCR");
            let words = parse_tsv(&tsv, 1);
            if name == "raw-300" {
                raw_words = words.clone();
            }
            ocr_metrics::print_metrics(name, &reference, &words_to_text(&words), &body_reference);
        }

        let config = PostprocessConfig::default();
        let refiner = CropRefiner {
            tesseract: Tesseract::new(tesseract_path(), "ind+eng".to_string()),
            images: HashMap::from([(1, raw)]),
            temp: temp.path().to_path_buf(),
            refine_calls: AtomicUsize::new(0),
            refine_millis: AtomicU64::new(0),
        };
        let (_, corrections, stages) = postprocess_stages(raw_words, &config, Some(&refiner));
        let mut previous_body = ocr_metrics::cer(
            &body_reference,
            &ocr_metrics::body_only(&words_to_text(&stages[0].words)),
        );
        for stage in &stages {
            let text = words_to_text(&stage.words);
            ocr_metrics::print_metrics(stage.name, &reference, &text, &body_reference);
            let body = ocr_metrics::cer(&body_reference, &ocr_metrics::body_only(&text));
            assert!(
                body <= previous_body + 0.005,
                "langkah {} menaikkan CER body: {:.4} → {:.4}",
                stage.name,
                previous_body,
                body
            );
            previous_body = body;
        }
        println!("total koreksi: {}", corrections.len());
    }

    /// Diagnosa lintas dokumen. Dijalankan manual:
    /// `cargo test --lib -- --ignored cross_document_metrics`
    ///
    /// Memakai 11 scan dengan ground truth dari teks sumber: bersih, noisy,
    /// miring, rot90, tabel, lowres, blur, tabel miring, dua kolom, linn asli,
    /// dan linn+noise.
    /// Setiap tahap post-processing tidak boleh menaikkan CER body.
    #[test]
    #[ignore = "butuh Tesseract + PDF sample + ground truth lintas dokumen"]
    fn cross_document_metrics_do_not_regress_any_stage() {
        let pdfium = pdfium_for_test();
        let temp = tempfile::tempdir().expect("tempdir");
        let config = PostprocessConfig::default();
        let dpi = config.render_dpi.max(300);
        let filter = std::env::var("DOCUMORPH_CROSS_FILTER").unwrap_or_default();

        for sample in ocr_metrics::cross_documents() {
            if !sample.name.contains(&filter) {
                continue;
            }
            assert!(
                sample.pdf.exists(),
                "sample {} tidak ada: {}",
                sample.name,
                sample.pdf.display()
            );
            let document = pdfium
                .load_pdf_from_file(&sample.pdf, None)
                .unwrap_or_else(|error| panic!("PDF {}: {error}", sample.name));
            let page = document.pages().get(0).expect("halaman pertama");
            let width = refiner_width(&page, dpi);
            let height = refiner_height(&page, dpi);
            let bitmap = page
                .render_with_config(
                    &PdfRenderConfig::new().set_target_size(width as i32, height as i32),
                )
                .unwrap_or_else(|error| panic!("render {}: {error}", sample.name));
            let raw = PageImage::from_rgba(width, height, &bitmap.as_rgba_bytes());

            let engine = Tesseract::new(tesseract_path(), "ind+eng".to_string());
            let mut rotation_log = Vec::new();
            let (oriented, _rotation) =
                orient_page(&engine, raw, temp.path(), &config, 1, &mut rotation_log);
            let tsv = engine
                .run(&oriented, TESSERACT_PSM_PAGE, temp.path())
                .unwrap_or_else(|error| panic!("OCR {}: {error}", sample.name));
            let words = parse_tsv(&tsv, 1);

            println!("=== {} ({} kata) ===", sample.name, words.len());
            let raw_text = words_to_text(&words);
            let body_text = |text: &str| {
                if sample.body_reference == sample.reference {
                    text.to_string()
                } else {
                    ocr_metrics::body_only_with_reference(&sample.body_reference, text)
                }
            };
            ocr_metrics::print_metrics(
                "ocr-mentah",
                &sample.reference,
                &raw_text,
                &sample.body_reference,
            );

            let refiner = CropRefiner {
                tesseract: Tesseract::new(tesseract_path(), "ind+eng".to_string()),
                images: HashMap::from([(1, oriented)]),
                temp: temp.path().to_path_buf(),
                refine_calls: AtomicUsize::new(0),
                refine_millis: AtomicU64::new(0),
            };
            let (final_words, corrections, stages) =
                postprocess_stages(words, &config, Some(&refiner));
            let mut previous_body = ocr_metrics::cer(&sample.body_reference, &body_text(&raw_text));
            for stage in &stages {
                let text = words_to_text(&stage.words);
                ocr_metrics::print_metrics(
                    stage.name,
                    &sample.reference,
                    &text,
                    &sample.body_reference,
                );
                let body = ocr_metrics::cer(&sample.body_reference, &body_text(&text));
                assert!(
                    body <= previous_body + 0.005,
                    "{} / {}: menaikkan CER body: {:.4} → {:.4}",
                    sample.name,
                    stage.name,
                    previous_body,
                    body
                );
                previous_body = body;
            }
            let final_text = words_to_text(&final_words);
            assert!(
                !final_text.contains('\u{00ff}'),
                "{}: text layer mengandung U+00FF",
                sample.name
            );
            println!("{} koreksi: {}", sample.name, corrections.len());
            if !filter.is_empty() {
                for correction in &corrections {
                    println!(
                        "  halaman {} [{}] {:?} → {:?}",
                        correction.page, correction.rule, correction.before, correction.after
                    );
                }
            }
        }
    }

    /// Tes runtime sample nyata. Dijalankan manual karena butuh engine eksternal
    /// dan PDF 300 DPI yang berat:
    /// `cargo test --lib -- --ignored sample_image_only_pdf`
    #[test]
    #[ignore = "butuh Tesseract + PDF sample nyata"]
    fn sample_image_only_pdf_gets_correct_invisible_text_layer() {
        let pdfium = pdfium_for_test();
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("sample.pdf");
        fs::copy(sample_pdf_path(), &target).expect("copy sample");
        let cancel = AtomicBool::new(false);
        let rewrite = rewrite_searchable_layer(
            &pdfium,
            &target,
            Tesseract::new(tesseract_path(), "ind+eng".to_string()),
            &PostprocessConfig::default(),
            &cancel,
            temp.path(),
        )
        .expect("rewrite layer");
        assert_layer_quality(&pdfium, &target, &rewrite);
    }

    /// Halaman yang diputar 90° harus mendapat text layer pada koordinat
    /// halaman asli, bukan koordinat gambar hasil rotasi.
    /// `cargo test --lib -- --ignored rotated_pdf_text_layer`
    #[test]
    #[ignore = "butuh Tesseract + PDF rot90 nyata"]
    fn rotated_pdf_text_layer_uses_original_bbox() {
        let pdfium = pdfium_for_test();
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("rotated.pdf");
        let source = r"C:\Data\01_Development\Projek\documorph-test-docs\generated\id-rot90.pdf";
        fs::copy(source, &target).expect("copy rot90");
        let cancel = AtomicBool::new(false);
        let rewrite = rewrite_searchable_layer(
            &pdfium,
            &target,
            Tesseract::new(tesseract_path(), "ind+eng".to_string()),
            &PostprocessConfig::default(),
            &cancel,
            temp.path(),
        )
        .expect("rewrite layer");

        assert!(
            rewrite
                .corrections
                .iter()
                .any(|entry| entry.rule == "page-rotation" && entry.after == "90°"),
            "OSD tidak mendeteksi rotasi: {:?}",
            rewrite.corrections
        );

        let python = std::env::var("DOCUMORPH_PYTHON")
            .unwrap_or_else(|_| r"C:\Data\01_Development\python3.10.11\python.exe".to_string());
        let path_arg = target.to_string_lossy().into_owned();
        let result = command_text(
            &python,
            &[
                "-c",
                "import pymupdf,sys; p=pymupdf.open(sys.argv[1])[0]; w={x[4]:x for x in p.get_text('words')}; a=w['Panduan']; b=w['Antrian']; sys.stdout.buffer.write(f'{a[0]:.1f} {a[1]:.1f} {b[0]:.1f} {b[1]:.1f}'.encode())",
                &path_arg,
            ],
        );
        let values: Vec<f32> = result
            .split_whitespace()
            .map(|value| value.parse().expect("koordinat PyMuPDF"))
            .collect();
        assert_eq!(values.len(), 4, "{result}");
        // Judul di scan asli adalah kolom vertikal: x sama, y berbeda.
        assert!((values[0] - values[2]).abs() < 1.5, "{result}");
        assert!((values[1] - values[3]).abs() > 100.0, "{result}");
    }

    /// Benchmark scan asli (bukan fixture sintetis). Dijalankan manual:
    /// `cargo test --lib -- --ignored --nocapture real_scan_benchmark`
    /// `DOCUMORPH_REAL_SCANS` bisa diisi daftar path dipisah `;`.
    #[test]
    #[ignore = "butuh PDF scan asli + Tesseract"]
    fn real_scan_benchmark_reports_seconds_per_page() {
        let pdfium = pdfium_for_test();
        let default = [
            r"C:\Data\01_Development\Projek\documorph-test-docs\generated\image-only-no-text.pdf",
            r"C:\Data\01_Development\Projek\documorph-test-docs\ocrmypdf\skew.pdf",
            r"C:\Data\01_Development\Projek\documorph-test-docs\ocrmypdf\rotated_skew.pdf",
            r"C:\Data\01_Development\Projek\documorph-test-docs\ocrmypdf\graph.pdf",
            r"C:\Data\01_Development\Projek\documorph-test-docs\ocrmypdf\francais.pdf",
        ];
        let configured = std::env::var("DOCUMORPH_REAL_SCANS").unwrap_or_default();
        let paths: Vec<String> = if configured.trim().is_empty() {
            default.iter().map(|path| path.to_string()).collect()
        } else {
            configured
                .split(';')
                .filter(|path| !path.trim().is_empty())
                .map(|path| path.trim().to_string())
                .collect()
        };
        assert!(!paths.is_empty(), "DOCUMORPH_REAL_SCANS kosong");

        let temp = tempfile::tempdir().expect("tempdir");
        let config = PostprocessConfig::default();
        let cancel = AtomicBool::new(false);
        println!(
            "{:<28} {:>5} {:>9} {:>11} {:>7} {:>9}",
            "file", "hal", "total(s)", "detik/hal", "kata", "koreksi"
        );
        for (index, source) in paths.iter().enumerate() {
            let target = temp.path().join(format!("real-{index}.pdf"));
            fs::copy(source, &target).unwrap_or_else(|error| panic!("copy {source}: {error}"));
            let started = std::time::Instant::now();
            let rewrite = rewrite_searchable_layer(
                &pdfium,
                &target,
                Tesseract::new(tesseract_path(), "ind+eng".to_string()),
                &config,
                &cancel,
                temp.path(),
            )
            .unwrap_or_else(|error| panic!("rewrite {source}: {error}"));
            let elapsed = started.elapsed().as_secs_f32();
            let name = Path::new(source)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            println!(
                "{:<28} {:>5} {:>9.2} {:>11.2} {:>7} {:>9}",
                name,
                rewrite.page_count,
                elapsed,
                elapsed / rewrite.page_count as f32,
                rewrite.word_count,
                rewrite.corrections.len()
            );
            let rotation = rewrite
                .corrections
                .iter()
                .find(|entry| entry.rule == "page-rotation")
                .map(|entry| entry.after.as_str())
                .unwrap_or("-");
            let document = pdfium
                .load_pdf_from_file(&target, None)
                .unwrap_or_else(|error| panic!("buka hasil {source}: {error}"));
            let page = document.pages().get(0).expect("halaman hasil");
            let snippet: String = page
                .text()
                .expect("teks hasil")
                .all()
                .split_whitespace()
                .take(12)
                .collect::<Vec<_>>()
                .join(" ");
            println!("    rotasi={rotation} teks={snippet}");
            if std::env::var("DOCUMORPH_BENCH_OCRMYPDF").is_ok() {
                let ocr = std::env::var("OCRMYPDF_PATH").unwrap_or_else(|_| {
                    r"C:\Data\01_Development\python3.10.11\Scripts\ocrmypdf.exe".to_string()
                });
                let tesseract = tesseract_path();
                let ghostscript = std::env::var("GHOSTSCRIPT_PATH")
                    .unwrap_or_else(|_| r"C:\Program Files\gs\gs10.08.0\bin\gswin64c.exe".into());
                let engine_path = std::env::join_paths([
                    tesseract.parent().expect("dir Tesseract"),
                    Path::new(&ghostscript).parent().expect("dir Ghostscript"),
                    Path::new(r"C:\Windows\System32"),
                ])
                .expect("PATH engine");
                let final_pdf = temp.path().join(format!("real-final-{index}.pdf"));
                let ocr_started = Instant::now();
                let mut command = Command::new(&ocr);
                hide_console(&mut command);
                let status = command
                    .env("PATH", engine_path)
                    .args(["--skip-text", "-l", "ind+eng"])
                    .arg(&target)
                    .arg(&final_pdf)
                    .status()
                    .expect("jalankan OCRmyPDF");
                assert!(status.success(), "OCRmyPDF gagal: {status}");
                let ocr_elapsed = ocr_started.elapsed().as_secs_f32();
                let pipeline = elapsed + ocr_elapsed;
                println!(
                    "    ocrmypdf={ocr_elapsed:.2}s pipeline={pipeline:.2}s ({:.2}s/hal)",
                    pipeline / rewrite.page_count as f32
                );
            }
        }
    }

    /// Diagnosa trade-off skala render: page box 96 DPI (82 MP), piksel native
    /// (8,4 MP), dan varian antara. Hasil empiris menolak "pakai piksel native"
    /// sebagai guard: CER body naik dan waktunya tidak lebih cepat.
    /// `DOCUMORPH_RENDER_CANDIDATES` berisi path PDF dipisah `;`.
    #[test]
    #[ignore = "butuh Tesseract + varian PDF linn"]
    fn render_scale_tradeoff_diagnostic() {
        let pdfium = pdfium_for_test();
        let temp = tempfile::tempdir().expect("tempdir");
        let native = std::env::var("DOCUMORPH_NATIVE_PIXEL_PDF").unwrap_or_else(|_| {
            r"C:\Users\Evraldi\AppData\Local\Temp\opencode\linn-correct-dpi.pdf".to_string()
        });
        let configured = std::env::var("DOCUMORPH_RENDER_CANDIDATES").unwrap_or_default();
        let candidates: Vec<(String, String)> = if configured.trim().is_empty() {
            vec![
                ("page-box-96dpi".to_string(), sample_pdf_path()),
                ("native-pixels".to_string(), native),
            ]
        } else {
            configured
                .split(';')
                .filter(|path| !path.trim().is_empty())
                .map(|path| {
                    let path = path.trim().to_string();
                    let label = Path::new(&path)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    (label, path)
                })
                .collect()
        };
        let reference = ocr_metrics::full_reference();
        let body = ocr_metrics::body_reference();
        for (label, source) in candidates {
            let target = temp.path().join(format!("{label}.pdf"));
            fs::copy(&source, &target).unwrap_or_else(|error| panic!("copy {source}: {error}"));
            let rewrite = rewrite_searchable_layer(
                &pdfium,
                &target,
                Tesseract::new(tesseract_path(), "ind+eng".to_string()),
                &PostprocessConfig::default(),
                &AtomicBool::new(false),
                temp.path(),
            )
            .unwrap_or_else(|error| panic!("rewrite {label}: {error}"));
            let document = pdfium
                .load_pdf_from_file(&target, None)
                .unwrap_or_else(|error| panic!("buka {label}: {error}"));
            let text = document
                .pages()
                .get(0)
                .expect("halaman")
                .text()
                .expect("teks")
                .all();
            ocr_metrics::print_metrics(&label, &reference, &text, &body);
            println!(
                "    kata={} koreksi={}",
                rewrite.word_count,
                rewrite.corrections.len()
            );
        }
    }

    /// Tes rute lengkap: Tesseract → koreksi → text layer → OCRmyPDF --skip-text.
    /// Dijalankan manual:
    /// `cargo test --lib -- --ignored full_searchable_pdf_route`
    #[test]
    #[ignore = "butuh OCRmyPDF + Tesseract + PDF sample nyata"]
    fn full_searchable_pdf_route_preserves_pixels_and_rewrites_layer() {
        let pdfium = pdfium_for_test();
        let temp = tempfile::tempdir().expect("tempdir");
        let input = temp.path().join("input.pdf");
        let output = temp.path().join("output.pdf");
        fs::copy(sample_pdf_path(), &input).expect("copy sample");
        let original = render_bytes(&pdfium, &input, 796, 1030);

        let tesseract = tesseract_path();
        let cancel = AtomicBool::new(false);
        let rewrite = rewrite_searchable_layer(
            &pdfium,
            &input,
            Tesseract::new(tesseract.clone(), "ind+eng".to_string()),
            &PostprocessConfig::default(),
            &cancel,
            temp.path(),
        )
        .expect("rewrite layer");
        assert_eq!(original, render_bytes(&pdfium, &input, 796, 1030));

        let ocr = std::env::var("OCRMYPDF_PATH").unwrap_or_else(|_| {
            r"C:\Data\01_Development\python3.10.11\Scripts\ocrmypdf.exe".to_string()
        });
        let ghostscript = std::env::var("GHOSTSCRIPT_PATH")
            .unwrap_or_else(|_| r"C:\Program Files\gs\gs10.08.0\bin\gswin64c.exe".to_string());
        let engine_path = std::env::join_paths([
            tesseract.parent().expect("dir Tesseract"),
            Path::new(&ghostscript).parent().expect("dir Ghostscript"),
            Path::new(r"C:\Windows\System32"),
        ])
        .expect("PATH engine");
        let mut command = Command::new(&ocr);
        hide_console(&mut command);
        let status = command
            .env("PATH", engine_path)
            .arg("--skip-text")
            .args(["-l", "ind+eng"])
            .arg(&input)
            .arg(&output)
            .status()
            .expect("jalankan OCRmyPDF");
        assert!(status.success(), "OCRmyPDF gagal: {status}");

        assert_eq!(original, render_bytes(&pdfium, &output, 796, 1030));
        assert_layer_quality(&pdfium, &output, &rewrite);
    }

    #[test]
    fn page_image_crop_keeps_rgb_and_upscales_nearest_neighbor() {
        let image = PageImage {
            width: 2,
            height: 1,
            rgb: vec![10, 20, 30, 40, 50, 60],
        };
        let crop = image
            .crop_upscaled(
                &BBox {
                    left: 1,
                    top: 0,
                    width: 1,
                    height: 1,
                },
                2,
            )
            .expect("crop");
        // Padding dipotong ke tepi gambar: kedua piksel ikut, bukan hanya bbox.
        assert_eq!(crop.width, 4);
        assert_eq!(crop.height, 2);
        assert_eq!(
            crop.rgb,
            vec![
                10, 20, 30, 10, 20, 30, 40, 50, 60, 40, 50, 60, 10, 20, 30, 10, 20, 30, 40, 50, 60,
                40, 50, 60,
            ]
        );
    }
}
