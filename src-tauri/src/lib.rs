use pdfium_render::prelude::*;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager, State};

mod db;
mod engines;
mod jobs;
mod limits;
mod output_guard;
mod pdfium_guard;
mod process;

static PDFIUM_INSTANCE: Mutex<Option<Result<Pdfium, String>>> = Mutex::new(None);

/// Path yang dibuka dari Explorer ditampung sampai frontend siap mengambilnya.
#[derive(Default)]
struct OpenFiles(Mutex<Vec<String>>);

const OPEN_EXTENSIONS: &[&str] = &[
    "pdf", "doc", "docx", "odt", "rtf", "md", "markdown", "html", "htm", "txt", "tex", "epub",
];
/// Batasi peringatan supaya tidak membanjiri UI dengan font tema Office.
const MAX_MISSING_FONTS: usize = 5;

#[derive(Serialize)]
struct EngineStatus {
    pandoc: bool,
    anymd: bool,
    pdf2docx: bool,
    libreoffice: bool,
    typst: bool,
    ocrmypdf: bool,
    tesseract: bool,
    ghostscript: bool,
}

#[derive(Serialize)]
struct PreflightReport {
    size_bytes: u64,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct HistoryItem {
    name: String,
    output: String,
    engine: String,
    tier: String,
    at: i64,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct ConversionReport {
    output: String,
    engine: String,
    tier: String,
    duration_ms: u128,
    warning: Option<String>,
}

fn anymd_path() -> Option<PathBuf> {
    engines::engine_paths().anymd
}
fn pdf2docx_path() -> Option<PathBuf> {
    engines::engine_paths().pdf2docx
}
fn pandoc_path() -> Option<PathBuf> {
    engines::engine_paths().pandoc
}
fn libreoffice_path() -> Option<PathBuf> {
    engines::engine_paths().libreoffice
}
fn typst_path() -> Option<PathBuf> {
    engines::engine_paths().typst
}
fn ocrmypdf_path() -> Option<PathBuf> {
    engines::engine_paths().ocrmypdf
}
fn tesseract_path() -> Option<PathBuf> {
    engines::engine_paths().tesseract
}
fn ghostscript_path() -> Option<PathBuf> {
    engines::engine_paths().ghostscript
}

fn history_connection(app: &AppHandle) -> Result<Connection, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Tidak dapat menemukan folder data aplikasi: {e}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Tidak dapat membuat folder data aplikasi: {e}"))?;
    db::open(&dir.join("history.sqlite3"))
}

fn insert_history_record(conn: &Connection, item: &HistoryItem) -> Result<(), String> {
    conn.execute(
        "INSERT INTO history (name, output, engine, tier, at, warnings) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            item.name,
            item.output,
            item.engine,
            item.tier,
            item.at,
            db::encode_warnings(&item.warnings)
        ],
    )
    .map_err(|e| format!("Gagal menyimpan riwayat: {e}"))?;
    conn.execute("DELETE FROM history WHERE id NOT IN (SELECT id FROM history ORDER BY at DESC, id DESC LIMIT 100)", [])
        .map_err(|e| format!("Gagal membatasi riwayat: {e}"))?;
    Ok(())
}

/// Riwayat hanya boleh ditulis oleh `convert_file`, bukan oleh frontend.
fn record_history(
    app: &AppHandle,
    source: &Path,
    report: &ConversionReport,
    warnings: &[String],
) -> Result<(), String> {
    let name = source
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("output")
        .to_owned();
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as i64;
    insert_history_record(
        &history_connection(app)?,
        &HistoryItem {
            name,
            output: report.output.clone(),
            engine: report.engine.clone(),
            tier: report.tier.clone(),
            at,
            warnings: warnings.to_vec(),
        },
    )
}

fn allowed_output_keys(app: &AppHandle) -> Result<Vec<String>, String> {
    let conn = history_connection(app)?;
    let mut statement = conn
        .prepare("SELECT output FROM history")
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut keys = Vec::new();
    for row in rows.flatten() {
        if let Ok(key) = output_guard::canonical_key(Path::new(&row)) {
            keys.push(key);
        }
    }
    Ok(keys)
}

fn ensure_output_path(app: &AppHandle, path: &Path) -> Result<(), String> {
    let allowed = allowed_output_keys(app)?;
    output_guard::validate(path, &allowed)
}

#[tauri::command]
fn reveal_output(app: AppHandle, path: String) -> Result<(), String> {
    let output = PathBuf::from(&path);
    ensure_output_path(&app, &output)?;
    tauri_plugin_opener::reveal_item_in_dir(output)
        .map_err(|e| format!("Tidak dapat menampilkan folder keluaran: {e}"))
}

#[tauri::command]
fn open_output(app: AppHandle, path: String) -> Result<(), String> {
    let output = PathBuf::from(&path);
    ensure_output_path(&app, &output)?;
    tauri_plugin_opener::open_path(output.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| format!("Tidak dapat membuka keluaran: {e}"))
}

#[tauri::command]
fn history_list(app: AppHandle) -> Result<Vec<HistoryItem>, String> {
    list_history(&history_connection(&app)?)
}

fn list_history(conn: &Connection) -> Result<Vec<HistoryItem>, String> {
    let mut statement = conn.prepare("SELECT name, output, engine, tier, at, warnings FROM history ORDER BY at DESC, id DESC LIMIT 100")
        .map_err(|e| e.to_string())?;
    statement
        .query_map([], |row| {
            Ok(HistoryItem {
                name: row.get(0)?,
                output: row.get(1)?,
                engine: row.get(2)?,
                tier: row.get(3)?,
                at: row.get(4)?,
                warnings: db::decode_warnings(&row.get::<_, String>(5)?),
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn history_clear(app: AppHandle) -> Result<(), String> {
    history_connection(&app)?
        .execute("DELETE FROM history", [])
        .map_err(|e| format!("Gagal menghapus riwayat: {e}"))?;
    Ok(())
}

#[tauri::command(async)]
fn engine_status() -> EngineStatus {
    EngineStatus {
        pandoc: pandoc_path().is_some(),
        anymd: anymd_path().is_some(),
        pdf2docx: pdf2docx_path().is_some(),
        libreoffice: libreoffice_path().is_some(),
        typst: typst_path().is_some(),
        ocrmypdf: ocrmypdf_path().is_some(),
        tesseract: tesseract_path().is_some(),
        ghostscript: ghostscript_path().is_some(),
    }
}

/// Tidak ada `OnceLock` yang mengunci error selamanya: engine dan PDFium
/// dicari ulang setelah pengguna memperbaiki instalasi.
#[tauri::command(async)]
fn refresh_dependencies() -> EngineStatus {
    engines::refresh();
    *PDFIUM_INSTANCE
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
    engine_status()
}

fn run_engine_for_output(
    command: &mut Command,
    name: &str,
    output: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    staging: Option<(&Path, &str)>,
) -> Result<(), String> {
    if let Err(error) = process::run_engine(command, name, cancel) {
        let _ = std::fs::remove_file(output);
        return Err(error);
    }
    if !output.is_file()
        && let Some((dir, ext)) = staging
    {
        settle_engine_output(output, dir, ext)?;
    }
    if !output.is_file() {
        return Err(format!(
            "{name} selesai tanpa menghasilkan file keluaran: {}",
            output.display()
        ));
    }
    Ok(())
}

fn pdfium_library_path(app: &AppHandle) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if cfg!(debug_assertions) {
        candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bin/pdfium.dll"));
    }
    if let Ok(resource) = app
        .path()
        .resolve("bin/pdfium.dll", tauri::path::BaseDirectory::Resource)
    {
        candidates.push(resource);
    }
    candidates
        .into_iter()
        .find(|path| path.is_file() && pdfium_guard::verify(path).is_ok())
}

fn inspect_pdf(app: &AppHandle, path: &Path) -> Result<Vec<String>, String> {
    // Guard ditahan selama inspeksi supaya refresh tidak membuang instance
    // yang sedang dipakai. Error tidak di-cache permanen.
    let mut instance = PDFIUM_INSTANCE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if instance.is_none() {
        let library = pdfium_library_path(app)
            .ok_or_else(|| "PDFium DLL tidak ditemukan atau tidak valid.".to_string())?;
        let bindings =
            Pdfium::bind_to_library(library).map_err(|e| format!("Gagal memuat PDFium: {e}"))?;
        *instance = Some(Ok(Pdfium::new(bindings)));
    }
    let pdfium = instance
        .as_ref()
        .expect("instance")
        .as_ref()
        .map_err(Clone::clone)?;
    let document = pdfium.load_pdf_from_file(path, None).map_err(|e| {
        format!("PDF tidak dapat dibuka (mungkin rusak atau dilindungi password): {e}")
    })?;
    let page_count = document.pages().len() as usize;
    if page_count == 0 {
        return Err("PDF tidak memiliki halaman.".into());
    }
    let (scan_count, truncated_pages) = limits::page_scan_limit(page_count);
    let mut pages_with_text = 0usize;
    let mut total_chars = 0usize;
    let mut truncated_text = false;
    for page in document.pages().iter().take(scan_count) {
        if total_chars >= limits::MAX_PDF_TEXT_CHARS {
            truncated_text = true;
            break;
        }
        let text = page
            .text()
            .map_err(|e| format!("Gagal membaca teks PDF: {e}"))?
            .all();
        let remaining = limits::MAX_PDF_TEXT_CHARS - total_chars;
        let (kept, cut) = limits::truncate_chars(&text, remaining);
        total_chars += kept.chars().count();
        if cut {
            truncated_text = true;
        }
        if !kept.trim().is_empty() {
            pages_with_text += 1;
        }
    }
    let mut warnings = Vec::new();
    if pages_with_text == 0 {
        warnings.push("PDF tidak memiliki teks yang dapat diekstrak; kemungkinan besar hasil scan. OCR disarankan.".into());
    } else if pages_with_text < scan_count {
        warnings.push(format!("PDF campuran: hanya {pages_with_text} dari {scan_count} halaman yang diperiksa memiliki teks yang dapat diekstrak; halaman lain mungkin scan."));
    }
    if truncated_pages {
        warnings.push(format!(
            "PDF memiliki {page_count} halaman; preflight hanya memeriksa {} halaman pertama.",
            limits::MAX_PDF_PAGES
        ));
    }
    if truncated_text {
        warnings.push(format!(
            "Teks PDF dipotong pada {} karakter saat preflight.",
            limits::MAX_PDF_TEXT_CHARS
        ));
    }
    Ok(warnings)
}

/// Tanpa `jobId` berarti batalkan semua job aktif (tombol Cancel queue).
#[tauri::command]
fn cancel_conversion(job_id: Option<String>) {
    jobs::cancel(job_id.as_deref());
}

/// Filter drag-drop di backend: hanya file biasa dengan ekstensi yang
/// memang diterima tab saat ini. Direktori dan `.exe` tidak masuk antrean.
#[tauri::command]
fn filter_inputs(paths: Vec<String>, extensions: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| {
            let candidate = Path::new(path);
            if !candidate.is_file() {
                return false;
            }
            candidate
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|ext| {
                    extensions
                        .iter()
                        .any(|allowed| allowed.eq_ignore_ascii_case(ext))
                })
        })
        .collect()
}

#[tauri::command(async)]
fn preflight_file(app: AppHandle, input: String) -> Result<PreflightReport, String> {
    inspect_input(&app, &input)
}

/// Dipakai preflight UI dan konversi supaya warning yang sama ikut tersimpan
/// di riwayat.
fn inspect_input(app: &AppHandle, input: &str) -> Result<PreflightReport, String> {
    let path = Path::new(input);
    let size = limits::check_input_size(path)?;
    let mut warnings = Vec::new();
    if let Some(warning) = limits::size_warning(size) {
        warnings.push(warning);
    }
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext == "pdf" {
        warnings.extend(inspect_pdf(app, path)?);
    } else if ["docx", "odt"].contains(&ext.as_str()) {
        warnings.extend(inspect_office_archive(path, &ext)?);
    } else if ext == "rtf" {
        inspect_rtf(path)?;
    }
    Ok(PreflightReport {
        size_bytes: size,
        warnings,
    })
}

fn inspect_office_archive(path: &Path, ext: &str) -> Result<Vec<String>, String> {
    use std::io::Read as _;

    let file =
        std::fs::File::open(path).map_err(|e| format!("Tidak dapat membuka dokumen: {e}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("Dokumen {ext} rusak atau bukan arsip Office valid: {e}"))?;
    if archive.len() > 10_000 {
        return Err(format!(
            "Arsip {ext} memiliki terlalu banyak entri untuk diperiksa dengan aman."
        ));
    }
    // `word/theme` dan `fontTable` berisi daftar font untuk semua skrip Office,
    // bukan font yang benar-benar dipakai di dokumen. Header/footer/footnote
    // tetap diperiksa karena isinya bisa memakai font berbeda.
    let xml_names: Vec<String> = (0..archive.len())
        .filter_map(|index| {
            archive
                .by_index(index)
                .ok()
                .and_then(|entry| entry.name().ok().map(|name| name.into_owned()))
        })
        .filter(|name| font_xml_name(name))
        .collect();
    if xml_names.is_empty() {
        return Err(format!("Arsip {ext} tidak memiliki dokumen XML utama."));
    }
    let mut referenced = std::collections::HashSet::new();
    let mut total_bytes = 0u64;
    for name in xml_names {
        let entry = archive
            .by_name(&name)
            .map_err(|e| format!("Dokumen Office tidak lengkap ({name}): {e}"))?;
        const MAX_XML_BYTES: u64 = 32 * 1024 * 1024;
        if entry.size() > MAX_XML_BYTES {
            return Err(format!(
                "XML dokumen terlalu besar untuk diperiksa dengan aman ({name})."
            ));
        }
        total_bytes = total_bytes.saturating_add(entry.size());
        if total_bytes > limits::MAX_XML_TOTAL_BYTES {
            return Err("Total XML dokumen melewati batas ukuran aman.".into());
        }
        let mut xml = String::new();
        entry
            .take(MAX_XML_BYTES + 1)
            .read_to_string(&mut xml)
            .map_err(|e| format!("XML dokumen tidak valid ({name}): {e}"))?;
        if xml.len() as u64 > MAX_XML_BYTES {
            return Err(format!("XML dokumen melewati batas ukuran aman ({name})."));
        }
        for marker in ["w:rFonts", "typeface="] {
            let mut remaining = xml.as_str();
            while let Some(position) = remaining.find(marker) {
                remaining = &remaining[position + marker.len()..];
                if let Some(start) = remaining.find(['"', '\'']) {
                    let quote = remaining.as_bytes()[start] as char;
                    let value = &remaining[start + 1..];
                    if let Some(end) = value.find(quote) {
                        let font = value[..end].trim();
                        if plausible_font(font) && referenced.len() < 24 {
                            referenced.insert(font.to_lowercase());
                        }
                    }
                }
            }
        }
    }
    let installed = installed_font_names();
    let mut missing: Vec<_> = referenced
        .into_iter()
        .filter(|font| !font_is_installed(font, &installed))
        .collect();
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    missing.sort();
    let shown = missing
        .iter()
        .take(MAX_MISSING_FONTS)
        .cloned()
        .collect::<Vec<_>>();
    let extra = missing.len().saturating_sub(MAX_MISSING_FONTS);
    let suffix = if extra == 0 {
        String::new()
    } else {
        format!(" (+{extra} lainnya)")
    };
    Ok(vec![format!(
        "Font mungkin tidak terpasang: {}{suffix}. Nama font diekstrak dari teks dokumen; hasil dapat berbeda jika font ter-embed atau alias digunakan.",
        shown.join(", ")
    )])
}

/// XML yang benar-benar bisa memuat referensi font yang dipakai dokumen.
fn font_xml_name(name: &str) -> bool {
    if name == "content.xml" || name == "styles.xml" {
        return true;
    }
    if !name.starts_with("word/") || !name.ends_with(".xml") {
        return false;
    }
    !matches!(
        name,
        "word/fontTable.xml" | "word/settings.xml" | "word/webSettings.xml"
    ) && !name.starts_with("word/theme/")
}

fn installed_font_names() -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(r"C:\Windows\Fonts") {
        for entry in entries.flatten() {
            if let Some(stem) = entry.path().file_stem().and_then(|value| value.to_str()) {
                names.insert(stem.to_lowercase());
            }
        }
    }
    for name in [
        "arial",
        "calibri",
        "cambria",
        "courier new",
        "carlito",
        "caladea",
        "liberation sans",
        "liberation serif",
        "liberation mono",
    ] {
        if names.iter().any(|installed| installed == name) {
            names.insert(name.into());
        }
    }
    names
}

/// Nama font Word sering berbeda dari nama file font-nya, misalnya
/// "Times New Roman" tersimpan sebagai `times.ttf`.
fn font_is_installed(font: &str, installed: &std::collections::HashSet<String>) -> bool {
    if installed.contains(font) {
        return true;
    }
    let aliases: &[&str] = match font {
        "times new roman" => &["times", "timesbd", "timesbi", "timesi"],
        "courier" | "courier new" => &["cour", "courbd", "courbi", "couri"],
        "calibri light" => &["calibril", "calibri"],
        "cambria math" => &["cambriamath", "cambria"],
        "ms gothic" | "ｍｓ ゴシック" => &["msgothic"],
        _ => &[],
    };
    aliases.iter().any(|alias| installed.contains(*alias))
}

/// `{\rtf1` plus angka versi dan pemisah, bukan sekadar `{\rtf`.
fn is_rtf_header(bytes: &[u8]) -> bool {
    let sample = String::from_utf8_lossy(bytes);
    let Some(rest) = sample.trim_start().strip_prefix("{\\rtf") else {
        return false;
    };
    let mut chars = rest.chars();
    let Some(version) = chars.next() else {
        return false;
    };
    if !version.is_ascii_digit() {
        return false;
    }
    chars
        .next()
        .is_none_or(|next| matches!(next, '\\' | ' ' | '\r' | '\n' | '{'))
}

fn inspect_rtf(path: &Path) -> Result<(), String> {
    let bytes = limits::read_prefix(path, limits::RTF_PREFIX_BYTES)?;
    if !is_rtf_header(&bytes) {
        return Err("File berekstensi RTF tetapi header RTF tidak valid.".into());
    }
    Ok(())
}

fn plausible_font(value: &str) -> bool {
    let name = value.trim();
    !name.is_empty()
        && name.len() <= 80
        && !["auto", "default", "none"].contains(&name.to_ascii_lowercase().as_str())
        && !name.contains(['/', '\\', ':', ';', '%'])
        && !name.to_ascii_lowercase().starts_with("http")
}

fn safe_stem(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || matches!(ch, '_' | '-' | ' ') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim().to_owned();
    if cleaned.is_empty() {
        "output".into()
    } else {
        cleaned
    }
}

/// AnyMD tidak punya subcommand `convert`: argumen pertama adalah file input.
/// Menaruh `convert` di sana membuatnya mencari file bernama "convert".
fn anymd_convert_args(source: &Path, output: &Path) -> [OsString; 3] {
    [
        source.as_os_str().to_os_string(),
        OsString::from("-o"),
        output.as_os_str().to_os_string(),
    ]
}

/// Keluaran dibuat di folder data aplikasi dengan direktori unik per job.
/// Tidak ada lagi penulisan di sebelah input yang bisa menimpa file milik user.
fn staged_output(
    app: &AppHandle,
    stem: &str,
    target_key: &str,
    ext: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let outbox = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Tidak dapat menemukan folder data aplikasi: {e}"))?
        .join("output");
    std::fs::create_dir_all(&outbox)
        .map_err(|e| format!("Tidak dapat membuat folder keluaran: {e}"))?;
    let dir = tempfile::Builder::new()
        .prefix("job-")
        .tempdir_in(&outbox)
        .map_err(|e| format!("Tidak dapat membuat direktori job: {e}"))?
        .keep();
    let file_name = if target_key == "searchable pdf" {
        format!("{stem}_searchable.pdf")
    } else {
        format!("{stem}.{ext}")
    };
    Ok((dir.join(file_name), dir))
}

/// LibreOffice menurunkan nama file dari input, bukan dari argumen `-o`.
/// Pindahkan hasil ke nama yang diharapkan sebelum divalidasi.
fn settle_engine_output(output: &Path, dir: &Path, ext: &str) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("Tidak dapat membaca direktori keluaran: {e}"))?;
    for entry in entries.flatten() {
        let produced = entry.path();
        if produced.is_file()
            && produced
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(ext))
        {
            if produced != output {
                std::fs::rename(&produced, output)
                    .map_err(|e| format!("Tidak dapat merapikan nama keluaran: {e}"))?;
            }
            return Ok(());
        }
    }
    Ok(())
}

#[tauri::command(async)]
fn convert_file(
    app: AppHandle,
    input: String,
    target: String,
    job_id: String,
) -> Result<ConversionReport, String> {
    let (cancel, _job_guard) = jobs::begin(&job_id);
    if jobs::cancelled(&cancel) {
        return Err("Konversi dibatalkan.".into());
    }
    let started = Instant::now();
    let source = Path::new(&input);
    if !source.is_file() {
        return Err("File sumber tidak ditemukan.".into());
    }
    limits::check_input_size(source)?;
    let warnings = inspect_input(&app, &input)
        .map(|report| report.warnings)
        .unwrap_or_default();

    let target_key = target.to_lowercase();
    let target_ext = match target_key.as_str() {
        "searchable pdf" => "pdf",
        "ocr text" => "txt",
        "pdf" => "pdf",
        "docx" => "docx",
        "markdown" | "md" => "md",
        "html" => "html",
        "txt" => "txt",
        "odt" => "odt",
        "rtf" => "rtf",
        _ => return Err("Format keluaran belum didukung.".into()),
    };
    let ext = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_lowercase();
    let ocr_target = target_key == "searchable pdf" || target_key == "ocr text";
    if ocr_target && ext != "pdf" {
        return Err("OCR hanya tersedia untuk input PDF.".into());
    }
    if !ocr_target && ext == target_ext
        || (target_ext == "md" && ext == "markdown")
        || (target_ext == "html" && ext == "htm")
    {
        return Err("Format sumber dan tujuan sama.".into());
    }
    let source_stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or("Nama file tidak valid.")?;
    let output_stem = safe_stem(source_stem);
    let (output, staged_dir) = staged_output(&app, &output_stem, &target_key, target_ext)?;

    if ocr_target {
        let ocr = ocrmypdf_path().ok_or("OCRmyPDF tidak ditemukan.")?;
        let tesseract = tesseract_path().ok_or("Tesseract tidak ditemukan.")?;
        let gs = ghostscript_path().ok_or("Ghostscript tidak ditemukan.")?;
        let tess_dir = tesseract.parent().ok_or("Path Tesseract tidak valid.")?;
        let gs_dir = gs.parent().ok_or("Path Ghostscript tidak valid.")?;
        // Hanya direktori engine terverifikasi + System32. Tidak ada PATH user.
        let search_path = vec![
            tess_dir.to_path_buf(),
            gs_dir.to_path_buf(),
            engines::system32_dir(),
        ];
        let joined = std::env::join_paths(search_path)
            .map_err(|e| format!("PATH engine tidak valid: {e}"))?;
        let (ocr_output, sidecar) = if target_key == "searchable pdf" {
            (output.clone(), None)
        } else {
            let temp = tempfile::Builder::new()
                .prefix("documorph-ocr-")
                .tempdir()
                .map_err(|e| format!("Gagal membuat direktori OCR sementara: {e}"))?;
            let temp_pdf = temp.path().join("searchable.pdf");
            (temp_pdf, Some(temp))
        };
        let mut command = Command::new(ocr);
        command
            .env("PATH", joined)
            .args(["--deskew", "--rotate-pages", "-l", "ind+eng"]);
        if target_key == "searchable pdf" {
            command.arg("--skip-text");
        } else {
            command.arg("--force-ocr");
        }
        if target_key == "ocr text" {
            let sidecar_path = output.with_extension("txt");
            command.arg("--sidecar").arg(&sidecar_path);
        }
        command.arg(source).arg(&ocr_output);
        let expected_output = if target_key == "searchable pdf" {
            output.as_path()
        } else {
            ocr_output.as_path()
        };
        let ocr_staging = sidecar
            .as_ref()
            .map(|dir| (dir.path() as &Path, "pdf"))
            .unwrap_or((staged_dir.as_path(), "pdf"));
        run_engine_for_output(
            &mut command,
            "OCRmyPDF / Tesseract",
            expected_output,
            &cancel,
            Some(ocr_staging),
        )?;
        if target_key == "ocr text" && !output.is_file() {
            drop(sidecar);
            return Err("OCR selesai tanpa menghasilkan file teks sidecar.".into());
        }
        drop(sidecar);
        let report = ConversionReport {
            output: output.to_string_lossy().into_owned(),
            engine: "OCRmyPDF + Tesseract + Ghostscript".into(),
            tier: "B/C".into(),
            duration_ms: started.elapsed().as_millis(),
            warning: Some(if target_key == "ocr text" {
                "OCR Text merasterisasi halaman sebelum OCR; akurasi tergantung kualitas scan."
                    .into()
            } else {
                "Akurasi OCR bergantung pada kualitas scan; lapisan teks yang sudah ada dipertahankan dengan --skip-text.".into()
            }),
        };
        record_history(&app, source, &report, &warnings)?;
        return Ok(report);
    }

    let office_input = ["docx", "doc", "odt", "rtf"].contains(&ext.as_str());
    let pandoc_input = [
        "md", "markdown", "html", "htm", "txt", "docx", "odt", "rtf", "epub", "tex",
    ]
    .contains(&ext.as_str());
    let pdf_input = ext == "pdf";
    let markup_input =
        ["md", "markdown", "html", "htm", "txt", "epub", "tex"].contains(&ext.as_str());
    let pandoc_output = ["docx", "md", "html", "txt", "odt", "rtf"].contains(&target_ext);
    let office_output = ["pdf", "docx", "odt", "rtf"].contains(&target_ext);
    let route_supported = if pdf_input && target_ext == "docx" && pdf2docx_path().is_some() {
        true
    } else if target_ext == "md" {
        anymd_path().is_some()
    } else if pdf_input && target_ext == "docx" && anymd_path().is_some() && pandoc_path().is_some()
    {
        true
    } else if pdf_input {
        ["docx", "txt", "html", "odt", "rtf"].contains(&target_ext) && libreoffice_path().is_some()
    } else if ext == "doc" {
        ["pdf", "docx", "odt", "rtf"].contains(&target_ext) && libreoffice_path().is_some()
    } else if office_input && target_ext == "pdf" {
        libreoffice_path().is_some()
    } else if pandoc_input && pandoc_output {
        pandoc_path().is_some()
    } else if markup_input && target_ext == "pdf" {
        pandoc_path().is_some() && typst_path().is_some()
    } else if office_input && office_output {
        libreoffice_path().is_some()
    } else {
        false
    };
    if !route_supported {
        return Err(format!(
            "Rute konversi .{ext} ke {target} tidak didukung atau engine yang diperlukan tidak tersedia."
        ));
    }

    let (engine, tier, warning) = if pdf_input && target_ext == "docx" && pdf2docx_path().is_some()
    {
        let converter = pdf2docx_path().ok_or("pdf2docx tidak ditemukan.")?;
        let mut command = Command::new(&converter);
        command.args(["convert"]).arg(source).arg("-d").arg(&output);
        run_engine_for_output(
            &mut command,
            "pdf2docx",
            &output,
            &cancel,
            Some((staged_dir.as_path(), target_ext)),
        )?;
        (
            "pdf2docx",
            "B",
            Some("PDF direkonstruksi menjadi elemen DOCX yang dapat diedit. Kemiripan bergantung pada struktur PDF; scan, layout kompleks, dan font khusus dapat memerlukan koreksi.".into()),
        )
    } else if target_ext == "md" {
        let anymd = anymd_path().ok_or("AnyMD tidak ditemukan.")?;
        let mut command = Command::new(&anymd);
        command.args(anymd_convert_args(source, &output));
        run_engine_for_output(
            &mut command,
            "AnyMD",
            &output,
            &cancel,
            Some((staged_dir.as_path(), target_ext)),
        )?;
        (
            "AnyMD",
            "B",
            Some("Markdown berfokus pada struktur dan teks; tata letak visual, sebagian tabel, dan elemen non-teks dapat berubah atau tidak ikut.".into()),
        )
    } else if pdf_input && target_ext == "docx" && anymd_path().is_some() && pandoc_path().is_some()
    {
        let anymd = anymd_path().ok_or("AnyMD tidak ditemukan.")?;
        let pandoc = pandoc_path().ok_or("Pandoc tidak ditemukan.")?;
        let temp = tempfile::Builder::new()
            .prefix("documorph-anymd-")
            .tempdir()
            .map_err(|e| format!("Gagal membuat file sementara: {e}"))?;
        let markdown = temp.path().join("extracted.md");
        let mut extract = Command::new(&anymd);
        extract.args(anymd_convert_args(source, &markdown));
        run_engine_for_output(
            &mut extract,
            "AnyMD (PDF to Markdown)",
            &markdown,
            &cancel,
            Some((temp.path(), "md")),
        )?;
        let mut build_docx = Command::new(pandoc);
        build_docx.arg(&markdown).arg("-o").arg(&output);
        run_engine_for_output(
            &mut build_docx,
            "Pandoc (Markdown to DOCX)",
            &output,
            &cancel,
            Some((staged_dir.as_path(), target_ext)),
        )?;
        (
            "AnyMD + Pandoc",
            "B",
            Some("PDF diekstrak ke Markdown lalu direkonstruksi sebagai DOCX. Layout, tabel kompleks, gambar, dan scan mungkin tidak terjaga; OCR AnyMD opsional mungkin diperlukan untuk PDF scan.".into()),
        )
    } else if target_ext == "pdf" && office_input {
        let lo = libreoffice_path().ok_or("LibreOffice tidak ditemukan.")?;
        let profile = tempfile::Builder::new()
            .prefix("documorph-lo-")
            .tempdir()
            .map_err(|e| format!("Gagal membuat profile LibreOffice: {e}"))?;
        let profile_url = format!(
            "file:///{}",
            profile
                .path()
                .to_string_lossy()
                .replace('\\', "/")
                .replace(' ', "%20")
        );
        let mut command = Command::new(lo);
        command
            .arg(format!("-env:UserInstallation={profile_url}"))
            .args(["--headless", "--convert-to", "pdf", "--outdir"])
            .arg(output.parent().unwrap_or(Path::new(".")))
            .arg(source);
        run_engine_for_output(
            &mut command,
            "LibreOffice",
            &output,
            &cancel,
            Some((staged_dir.as_path(), "pdf")),
        )?;
        (
            "LibreOffice",
            "A",
            Some("Hasil bergantung pada font dan fitur dokumen yang didukung LibreOffice.".into()),
        )
    } else if target_ext == "pdf" && pandoc_input {
        let pandoc = pandoc_path().ok_or("Pandoc tidak ditemukan.")?;
        if typst_path().is_none() {
            return Err("Typst tidak ditemukan; diperlukan untuk membuat PDF dari Pandoc.".into());
        }
        let mut command = Command::new(pandoc);
        command
            .arg(source)
            .arg("-o")
            .arg(&output)
            .args(["--pdf-engine=typst"]);
        run_engine_for_output(
            &mut command,
            "Pandoc + Typst",
            &output,
            &cancel,
            Some((staged_dir.as_path(), "pdf")),
        )?;
        (
            "Pandoc + Typst",
            "B",
            Some(
                "Layout dibangun ulang; hasil visual mungkin berbeda dari dokumen Office asal."
                    .into(),
            ),
        )
    } else if target_ext != "pdf" && pandoc_input && !pdf_input && pandoc_output {
        let pandoc = pandoc_path().ok_or("Pandoc tidak ditemukan.")?;
        let mut command = Command::new(pandoc);
        command.arg(source).arg("-o").arg(&output);
        run_engine_for_output(
            &mut command,
            "Pandoc",
            &output,
            &cancel,
            Some((staged_dir.as_path(), target_ext)),
        )?;
        ("Pandoc", "B", None)
    } else if pdf_input {
        let lo = libreoffice_path().ok_or("LibreOffice tidak ditemukan.")?;
        let profile = tempfile::Builder::new()
            .prefix("documorph-lo-")
            .tempdir()
            .map_err(|e| format!("Gagal membuat profile LibreOffice: {e}"))?;
        let profile_url = format!(
            "file:///{}",
            profile
                .path()
                .to_string_lossy()
                .replace('\\', "/")
                .replace(' ', "%20")
        );
        let mut command = Command::new(lo);
        command
            .arg(format!("-env:UserInstallation={profile_url}"))
            .args(["--headless", "--convert-to", target_ext, "--outdir"])
            .arg(output.parent().unwrap_or(Path::new(".")))
            .arg(source);
        run_engine_for_output(
            &mut command,
            "LibreOffice (PDF import)",
            &output,
            &cancel,
            Some((staged_dir.as_path(), target_ext)),
        )?;
        (
            "LibreOffice (PDF import)",
            "C",
            Some("PDF adalah format visual; teks dan layout direkonstruksi, hasil bisa berubah signifikan. Scan memerlukan OCR.".into()),
        )
    } else if target_ext != "pdf" && office_input && office_output {
        let lo = libreoffice_path().ok_or("LibreOffice tidak ditemukan.")?;
        let profile = tempfile::Builder::new()
            .prefix("documorph-lo-")
            .tempdir()
            .map_err(|e| format!("Gagal membuat profile LibreOffice: {e}"))?;
        let profile_url = format!(
            "file:///{}",
            profile
                .path()
                .to_string_lossy()
                .replace('\\', "/")
                .replace(' ', "%20")
        );
        let mut command = Command::new(lo);
        command
            .arg(format!("-env:UserInstallation={profile_url}"))
            .args(["--headless", "--convert-to", target_ext, "--outdir"])
            .arg(output.parent().unwrap_or(Path::new(".")))
            .arg(source);
        run_engine_for_output(
            &mut command,
            "LibreOffice",
            &output,
            &cancel,
            Some((staged_dir.as_path(), target_ext)),
        )?;
        ("LibreOffice", "B", None)
    } else {
        return Err(format!(
            "Konversi dari .{ext} ke PDF belum didukung oleh engine yang terpasang."
        ));
    };
    if !output.is_file() {
        return Err("Engine selesai tanpa menghasilkan file keluaran.".into());
    }
    let report = ConversionReport {
        output: output.to_string_lossy().into_owned(),
        engine: engine.into(),
        tier: tier.into(),
        duration_ms: started.elapsed().as_millis(),
        warning,
    };
    record_history(&app, source, &report, &warnings)?;
    Ok(report)
}

fn report_startup_error(error: &str) {
    eprintln!("documorph gagal start: {error}");
    if let Some(dir) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        let path = dir.join("documorph").join("startup-error.log");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let line = format!(
            "{} documorph gagal start: {error}\n",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_secs())
                .unwrap_or(0)
        );
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()));
    }
}

/// File Association / Explorer hanya boleh menyuntik file dokumen yang memang
/// didukung, bukan exe/dll atau direktori.
fn openable_path(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| OPEN_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

fn file_arguments() -> Vec<String> {
    std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .filter(|path| openable_path(path))
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

#[tauri::command]
fn take_open_files(state: State<'_, OpenFiles>) -> Vec<String> {
    state
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .drain(..)
        .collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(OpenFiles(Mutex::new(file_arguments())));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine_status,
            refresh_dependencies,
            filter_inputs,
            take_open_files,
            history_list,
            open_output,
            reveal_output,
            history_clear,
            preflight_file,
            convert_file,
            cancel_conversion
        ])
        .run(tauri::generate_context!());
    if let Err(error) = result {
        report_startup_error(&error.to_string());
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtf_header_needs_a_version_number() {
        assert!(is_rtf_header(b"{\\rtf1\\ansi hello"));
        assert!(is_rtf_header(b"  {\\rtf1 test"));
        assert!(!is_rtf_header(b"{\\rtf whatever"));
        assert!(!is_rtf_header(b"plain text"));
    }

    #[test]
    fn font_xml_scan_skips_theme_and_metadata_parts() {
        assert!(font_xml_name("word/document.xml"));
        assert!(font_xml_name("word/header1.xml"));
        assert!(font_xml_name("word/glossary/document.xml"));
        assert!(font_xml_name("content.xml"));
        assert!(!font_xml_name("word/theme/theme1.xml"));
        assert!(!font_xml_name("word/fontTable.xml"));
        assert!(!font_xml_name("word/settings.xml"));
        assert!(!font_xml_name("word/_rels/document.xml.rels"));
    }

    #[test]
    fn font_heuristic_rejects_paths_and_placeholders() {
        assert!(plausible_font("Times New Roman"));
        assert!(!plausible_font("auto"));
        assert!(!plausible_font("C:/Windows/Fonts/arial.ttf"));
        assert!(!plausible_font("http://example.com/font"));
        assert!(!plausible_font(&"x".repeat(100)));
    }

    #[test]
    fn office_font_warning_ignores_theme_fonts() {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;

        let dir = std::env::temp_dir().join(format!("documorph-font-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create font fixture dir");
        let path = dir.join("sample.docx");
        {
            let file = std::fs::File::create(&path).expect("create docx");
            let mut archive = zip::ZipWriter::new(file);
            archive
                .start_file("word/document.xml", SimpleFileOptions::default())
                .expect("start document.xml");
            archive
                .write_all(
                    br#"<w:document><w:rFonts w:ascii="Times New Roman"/><w:rFonts w:ascii="Documorph Body Font"/></w:document>"#,
                )
                .expect("write document.xml");
            archive
                .start_file("word/theme/theme1.xml", SimpleFileOptions::default())
                .expect("start theme1.xml");
            archive
                .write_all(br#"<a:theme><a:latin typeface="Theme Exotic Font"/></a:theme>"#)
                .expect("write theme1.xml");
            archive
                .start_file("word/header1.xml", SimpleFileOptions::default())
                .expect("start header1.xml");
            archive
                .write_all(br#"<w:hdr><w:rFonts w:ascii="Documorph Header Font"/></w:hdr>"#)
                .expect("write header1.xml");
            archive.finish().expect("finish docx");
        }

        let warnings = inspect_office_archive(&path, "docx").expect("inspect docx");
        assert_eq!(warnings.len(), 1);
        let text = warnings[0].to_lowercase();
        assert!(text.contains("documorph body font"));
        assert!(text.contains("documorph header font"));
        assert!(!text.contains("theme exotic font"));
        assert!(
            !text.contains("times new roman"),
            "alias font tidak boleh dilaporkan"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn office_font_warning_caps_the_list() {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;

        let dir = std::env::temp_dir().join(format!("documorph-font-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create cap fixture dir");
        let path = dir.join("many.docx");
        {
            let file = std::fs::File::create(&path).expect("create docx");
            let mut archive = zip::ZipWriter::new(file);
            archive
                .start_file("word/document.xml", SimpleFileOptions::default())
                .expect("start document.xml");
            let fonts = ["a", "b", "c", "d", "e", "f", "g"]
                .map(|name| format!("<w:rFonts w:ascii=\"Documorph Missing {name}\"/>"))
                .join("");
            archive
                .write_all(format!("<w:document>{fonts}</w:document>").as_bytes())
                .expect("write document.xml");
            archive.finish().expect("finish docx");
        }

        let warnings = inspect_office_archive(&path, "docx").expect("inspect docx");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("(+2 lainnya)"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn safe_stem_removes_windows_reserved_characters() {
        assert_eq!(safe_stem("laporan:<bulan>"), "laporan__bulan_");
        assert_eq!(safe_stem("  "), "output");
    }

    #[test]
    fn anymd_convert_args_do_not_include_convert_subcommand() {
        let source = Path::new("skripsi.docx");
        let output = Path::new("skripsi.md");
        let args = anymd_convert_args(source, output);

        assert_eq!(args.len(), 3);
        assert_eq!(args[0], source.as_os_str());
        assert_eq!(args[1], "-o");
        assert_eq!(args[2], output.as_os_str());
        assert!(
            args.iter().all(|arg| arg != "convert"),
            "AnyMD akan mengira 'convert' sebagai nama file"
        );
    }

    #[test]
    fn filter_inputs_rejects_directories_and_unknown_extensions() {
        let dir = std::env::temp_dir().join(format!("documorph-filter-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let allowed = dir.join("report.pdf");
        let blocked = dir.join("payload.exe");
        std::fs::write(&allowed, b"pdf").expect("allowed file");
        std::fs::write(&blocked, b"exe").expect("blocked file");
        let result = filter_inputs(
            vec![
                allowed.to_string_lossy().into_owned(),
                blocked.to_string_lossy().into_owned(),
                dir.to_string_lossy().into_owned(),
            ],
            vec!["pdf".into()],
        );
        assert_eq!(result, vec![allowed.to_string_lossy().into_owned()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn history_round_trips_preflight_warnings() {
        let conn = Connection::open_in_memory().expect("in-memory db");
        db::migrate(&conn).expect("migrate");
        insert_history_record(
            &conn,
            &HistoryItem {
                name: "laporan.pdf".into(),
                output: "C:\\Out\\laporan.docx".into(),
                engine: "Pandoc".into(),
                tier: "B".into(),
                at: 1234,
                warnings: vec!["PDF berisi teks kecil".into(), "Font tidak standar".into()],
            },
        )
        .expect("insert");
        let records = list_history(&conn).expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].warnings,
            vec!["PDF berisi teks kecil", "Font tidak standar"]
        );
    }

    #[test]
    fn explorer_arguments_accept_only_supported_documents() {
        let dir = std::env::temp_dir().join(format!("documorph-open-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let pdf = dir.join("laporan.pdf");
        let exe = dir.join("payload.exe");
        std::fs::write(&pdf, b"pdf").expect("pdf");
        std::fs::write(&exe, b"exe").expect("exe");
        assert!(openable_path(&pdf));
        assert!(!openable_path(&exe));
        assert!(!openable_path(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }
}
