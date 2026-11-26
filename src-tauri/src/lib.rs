use pdfium_render::prelude::*;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager, State};

mod db;
mod engines;
mod jobs;
mod limits;
mod ocr_layer;
#[cfg(test)]
mod ocr_metrics;
mod ocr_postprocess;
mod output_guard;
mod pdfium_guard;
mod process;

static PDFIUM_INSTANCE: Mutex<Option<Result<Arc<Pdfium>, String>>> = Mutex::new(None);

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

fn allowed_output_keys(conn: &Connection) -> Result<Vec<String>, String> {
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
    let conn = history_connection(app)?;
    let allowed = allowed_output_keys(&conn)?;
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

/// Hapus semua keluaran yang masih tercatat, lalu kosongkan riwayat. Path tetap
/// divalidasi lewat output_guard: hanya file hasil konversi kita yang disentuh.
fn clear_history(conn: &Connection) -> Result<usize, String> {
    let allowed = allowed_output_keys(conn)?;
    let mut statement = conn
        .prepare("SELECT output FROM history")
        .map_err(|e| e.to_string())?;
    let outputs: Vec<String> = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    drop(statement);

    let mut deleted = 0usize;
    for raw in outputs {
        let path = PathBuf::from(raw);
        if output_guard::validate(&path, &allowed).is_err() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => deleted += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Gagal menghapus keluaran {}: {error}",
                    path.display()
                ));
            }
        }
    }
    conn.execute("DELETE FROM history", [])
        .map_err(|e| format!("Gagal menghapus riwayat: {e}"))?;
    Ok(deleted)
}

#[tauri::command]
fn history_clear(app: AppHandle) -> Result<usize, String> {
    clear_history(&history_connection(&app)?)
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

/// Instance PDFium dipakai bersama preflight dan penulisan text layer.
/// `Arc` membuat guard mutex tidak perlu ditahan saat OCR berjalan.
fn pdfium_instance(app: &AppHandle) -> Result<Arc<Pdfium>, String> {
    let mut instance = PDFIUM_INSTANCE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if instance.is_none() {
        let library = pdfium_library_path(app)
            .ok_or_else(|| "PDFium DLL tidak ditemukan atau tidak valid.".to_string())?;
        let bindings =
            Pdfium::bind_to_library(library).map_err(|e| format!("Gagal memuat PDFium: {e}"))?;
        *instance = Some(Ok(Arc::new(Pdfium::new(bindings))));
    }
    match instance.as_ref().expect("instance") {
        Ok(pdfium) => Ok(Arc::clone(pdfium)),
        Err(error) => Err(error.clone()),
    }
}

fn inspect_pdf(app: &AppHandle, path: &Path) -> Result<Vec<String>, String> {
    let pdfium = pdfium_instance(app)?;
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

/// pdf2docx punya subcommand `convert`; output adalah argumen posisional kedua.
/// Memakai `-d` membuat CLI mengira output adalah opsi yang tidak dikenal.
fn pdf2docx_convert_args(source: &Path, output: &Path) -> [OsString; 3] {
    [
        OsString::from("convert"),
        source.as_os_str().to_os_string(),
        output.as_os_str().to_os_string(),
    ]
}

/// Format yang hanya bisa dicapai dari PDF lewat LibreOffice Draw -> Writer.
fn pdf_writer_route(target_ext: &str) -> bool {
    ["docx", "txt", "odt", "rtf"].contains(&target_ext)
}

/// Profile unik mencegah dua job LibreOffice berebut instance yang sama.
fn libreoffice_profile_url(profile: &Path) -> String {
    format!(
        "file:///{}",
        profile
            .to_string_lossy()
            .replace('\\', "/")
            .replace(' ', "%20")
    )
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

fn flag_disabled(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("0" | "false" | "no" | "off"))
}

fn env_flag_disabled(name: &str) -> bool {
    flag_disabled(std::env::var(name).ok().as_deref())
}

/// Rute OCR untuk PDF→DOCX bisa dimatikan tanpa menghapus engine.
fn pdf_docx_ocr_enabled() -> bool {
    !env_flag_disabled("DOCUMORPH_PDF_DOCX_OCR")
}

/// Koreksi teks sidecar bisa dimatikan tanpa mematikan seluruh rute OCR.
fn pdf_docx_sidecar_postprocess_enabled() -> bool {
    !env_flag_disabled("DOCUMORPH_PDF_DOCX_SIDECAR_POSTPROCESS")
}

fn vertically_overlaps(a: &ocr_postprocess::OcrWord, b: &ocr_postprocess::OcrWord) -> bool {
    let top = a.bbox.top.max(b.bbox.top);
    let bottom = (a.bbox.top + a.bbox.height).min(b.bbox.top + b.bbox.height);
    bottom - top >= a.bbox.height.min(b.bbox.height) / 2
}

/// Susun teks Markdown untuk Pandoc. Baris yang berdekatan digabung dengan
/// spasi, celah vertikal besar jadi `\n\n`, dan setiap marker bullet di awal
/// baris OCR memulai paragraf sendiri. Aturannya generik untuk dokumen scan,
/// bukan kata tertentu. Token kata tidak diubah, jadi metrik CER/WER tetap sama.
fn words_to_docx_text(words: &[ocr_postprocess::OcrWord]) -> String {
    let mut heights: Vec<f32> = words.iter().map(|word| word.bbox.height as f32).collect();
    heights.sort_by(f32::total_cmp);
    let median_height = heights
        .get(heights.len() / 2)
        .copied()
        .unwrap_or(10.0)
        .max(1.0);

    let mut output = String::new();
    let mut previous: Option<&ocr_postprocess::OcrWord> = None;
    for word in words {
        if let Some(prev) = previous {
            if (word.text == "•" && !vertically_overlaps(prev, word)) || word.page != prev.page {
                output.push_str("\n\n");
            } else if vertically_overlaps(prev, word) {
                output.push(' ');
            } else if (word.bbox.top - prev.bbox.top - prev.bbox.height) as f32
                >= median_height * 1.2
            {
                output.push_str("\n\n");
            } else {
                output.push('\n');
            }
        }
        output.push_str(&word.text);
        previous = Some(word);
    }
    output.push('\n');
    output
}

/// Sidecar OCRmyPDF hanya berisi teks tanpa confidence dan bbox per kata.
/// Word disintesis dengan confidence 95 supaya kamus hanya menjalankan koreksi
/// eksplisit yang konservatif. Token `0`/`2` dikecualikan dari normalisasi
/// bullet karena tanpa bbox asli token angka di awal baris tabel bisa salah
/// dianggap marker bullet.
fn postprocess_sidecar_text(text: &str, config: &ocr_postprocess::PostprocessConfig) -> String {
    if !pdf_docx_sidecar_postprocess_enabled() {
        return text.to_string();
    }
    let mut output = String::new();
    let mut first_page = true;
    for (page_index, page_text) in text.split('\u{000C}').enumerate() {
        if page_text.trim().is_empty() {
            continue;
        }
        if !first_page {
            output.push_str("\n\n");
        }
        first_page = false;
        let mut first_line = true;
        let mut previous_blank = false;
        for (line_index, line) in page_text.lines().enumerate() {
            let tokens: Vec<String> = line.split_whitespace().map(str::to_string).collect();
            if tokens.is_empty() {
                if !first_line {
                    output.push('\n');
                }
                first_line = false;
                previous_blank = true;
                continue;
            }
            let words: Vec<ocr_postprocess::OcrWord> = tokens
                .iter()
                .enumerate()
                .map(|(word_index, token)| {
                    let protected = if token == "0" {
                        Some(("\u{E000}", "0"))
                    } else if token == "2" {
                        Some(("\u{E001}", "2"))
                    } else {
                        None
                    };
                    ocr_postprocess::OcrWord {
                        page: page_index + 1,
                        block: 0,
                        paragraph: 0,
                        line: line_index + 1,
                        word: word_index,
                        bbox: ocr_postprocess::BBox {
                            left: (word_index * 100) as i32,
                            top: (line_index * 20) as i32,
                            width: 90,
                            height: 18,
                        },
                        text: protected
                            .map(|(sentinel, _)| sentinel.to_string())
                            .unwrap_or_else(|| token.clone()),
                        confidence: 95.0,
                    }
                })
                .collect();
            let (processed, corrections) = ocr_postprocess::postprocess(words, config, None);
            for correction in corrections {
                eprintln!(
                    "[PDF→DOCX OCR] halaman {}: {} → {} ({})",
                    correction.page, correction.before, correction.after, correction.rule
                );
            }
            let line_text = processed
                .iter()
                .map(|word| match word.text.as_str() {
                    "\u{E000}" => "0",
                    "\u{E001}" => "2",
                    other => other,
                })
                .collect::<Vec<_>>()
                .join(" ");
            if !first_line {
                if processed.first().is_some_and(|word| word.text == "•") && !previous_blank {
                    output.push_str("\n\n");
                } else {
                    output.push('\n');
                }
            }
            first_line = false;
            previous_blank = false;
            output.push_str(&line_text);
        }
    }
    output
}

/// `canonicalize()` di Windows menghasilkan prefiks `\\?\`. Path itu aman
/// sebagai argv, tapi tidak diterima Tesseract saat mencari `tessdata`.
fn plain_windows_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

fn pdf_docx_ocr_engines() -> Option<(PathBuf, PathBuf, PathBuf, PathBuf)> {
    Some((
        tesseract_path()?,
        ocrmypdf_path()?,
        ghostscript_path()?,
        pandoc_path()?,
    ))
}

/// PDF digital tetap memakai rute lama. Halaman dengan text layer (termasuk
/// invisible text) dianggap sudah punya teks.
fn pdf_has_extractable_text(app: &AppHandle, source: &Path) -> Result<bool, String> {
    let pdfium = pdfium_instance(app)?;
    let document = pdfium
        .load_pdf_from_file(source, None)
        .map_err(|e| format!("PDF tidak dapat dibuka: {e}"))?;
    for page in document.pages().iter().take(8) {
        let text = page
            .text()
            .map_err(|e| format!("Gagal membaca teks PDF: {e}"))?
            .all();
        if !text.trim().is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Dua kandidat OCR dijalankan untuk PDF scan. Confidence Tesseract memutuskan
/// mana yang dipakai: render langsung kita atau sidecar OCRmyPDF yang lebih
/// tahan terhadap tabel dan halaman terputar.
fn choose_docx_ocr_text(
    pdfium: &Pdfium,
    source: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    dir: &Path,
) -> Result<(&'static str, String), String> {
    let (tesseract, ocrmypdf, ghostscript, _) = pdf_docx_ocr_engines().ok_or_else(|| {
        format!(
            "Engine OCR/Pandoc tidak lengkap untuk PDF→DOCX scan (tesseract={}, ocrmypdf={}, ghostscript={}, pandoc={}).",
            tesseract_path().is_some(),
            ocrmypdf_path().is_some(),
            ghostscript_path().is_some(),
            pandoc_path().is_some()
        )
    })?;
    let config = ocr_postprocess::PostprocessConfig::from_env();
    let document = pdfium
        .load_pdf_from_file(source, None)
        .map_err(|e| format!("PDF tidak dapat dibuka: {e}"))?;

    let mut direct_text = String::new();
    let mut confidence_total = 0.0f32;
    let mut confidence_words = 0usize;

    for page_index in 0..document.pages().len() {
        let page = document
            .pages()
            .get(page_index)
            .map_err(|e| format!("Halaman PDF gagal dibaca: {e}"))?;
        let width = ((page.width().value / 72.0) * config.render_dpi as f32)
            .round()
            .max(1.0) as usize;
        let height = ((page.height().value / 72.0) * config.render_dpi as f32)
            .round()
            .max(1.0) as usize;
        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new()
                    .set_target_size(width as i32, height as i32)
                    .set_format(PdfBitmapFormat::BGR)
                    .set_reverse_byte_order(true),
            )
            .map_err(|e| format!("Gagal render halaman {}: {e}", page_index + 1))?;
        let raw = bitmap.as_raw_bytes();
        let stride = raw.len() / height.max(1);
        let row_bytes = width * 3;
        let mut rgb = Vec::with_capacity(row_bytes * height);
        for row in raw.chunks_exact(stride) {
            rgb.extend_from_slice(&row[..row_bytes]);
        }

        let mut ppm = Vec::with_capacity(32 + rgb.len());
        ppm.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
        ppm.extend_from_slice(&rgb);
        let ppm_path = dir.join(format!("direct-{page_index}.ppm"));
        std::fs::write(&ppm_path, ppm).map_err(|e| format!("Gagal menulis gambar OCR: {e}"))?;

        let stem = dir.join(format!("direct-{page_index}"));
        let tsv = stem.with_extension("tsv");
        let mut command = Command::new(&tesseract);
        command
            .arg(&ppm_path)
            .arg(&stem)
            .args(["-l", "ind+eng", "--psm", "3", "tsv"]);
        run_engine_for_output(&mut command, "Tesseract", &tsv, cancel, None)?;
        let tsv_text = std::fs::read_to_string(&tsv)
            .map_err(|e| format!("Gagal membaca hasil Tesseract: {e}"))?;
        let words = ocr_layer::parse_tsv(&tsv_text, (page_index + 1) as usize);
        let confidences: Vec<f32> = words
            .iter()
            .filter(|word| word.confidence >= 0.0)
            .map(|word| word.confidence)
            .collect();
        let mean = confidences.iter().sum::<f32>() / confidences.len().max(1) as f32;
        confidence_total += mean * confidences.len().max(1) as f32;
        confidence_words += confidences.len();
        let (processed, corrections) = ocr_postprocess::postprocess(words, &config, None);
        for correction in corrections {
            eprintln!(
                "[PDF→DOCX OCR] halaman {}: {} → {} ({})",
                correction.page, correction.before, correction.after, correction.rule
            );
        }
        if !direct_text.is_empty() {
            direct_text.push_str("\n\n");
        }
        direct_text.push_str(&words_to_docx_text(&processed));
    }

    let sidecar = dir.join("ocrmypdf.txt");
    let ocr_pdf = dir.join("ocrmypdf.pdf");
    let tesseract_dir = plain_windows_path(tesseract.parent().unwrap_or_else(|| Path::new("")));
    let ghostscript_dir = plain_windows_path(ghostscript.parent().unwrap_or_else(|| Path::new("")));
    let search_path = std::env::join_paths(vec![
        tesseract_dir.clone(),
        ghostscript_dir,
        engines::system32_dir(),
    ])
    .map_err(|e| format!("PATH engine tidak valid: {e}"))?;
    let tessdata = tesseract_dir.join("tessdata");
    let mut command = Command::new(&ocrmypdf);
    command
        .env("PATH", search_path)
        .env("TESSDATA_PREFIX", &tessdata)
        .args(["-l", "ind+eng", "--deskew", "--rotate-pages", "--force-ocr"])
        .arg("--sidecar")
        .arg(&sidecar)
        .arg(source)
        .arg(&ocr_pdf);
    run_engine_for_output(&mut command, "OCRmyPDF", &ocr_pdf, cancel, None)?;
    let ocrmypdf_text = std::fs::read_to_string(&sidecar)
        .map_err(|e| format!("Gagal membaca sidecar OCRmyPDF: {e}"))?;
    let ocrmypdf_text = postprocess_sidecar_text(&ocrmypdf_text, &config);

    let confidence = confidence_total / confidence_words.max(1) as f32;
    eprintln!("[PDF→DOCX OCR] confidence={confidence:.2}");
    if confidence >= 85.0 {
        Ok(("Tesseract + Pandoc", direct_text))
    } else {
        Ok(("OCRmyPDF + Pandoc", ocrmypdf_text))
    }
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
    let mut warnings = inspect_input(&app, &input)
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
        let searchable = target_key == "searchable pdf";
        let config = if searchable {
            Some(ocr_postprocess::PostprocessConfig::from_env())
        } else {
            None
        };
        let mut layer_note = String::new();

        // Searchable PDF: Tesseract kita yang menjalankan OCR sekali, lalu
        // OCRmyPDF --skip-text hanya mengubah struktur/PDF-A. Urutan lama
        // (OCRmyPDF OCR → buang layer → OCR Tesseract lagi) membayar dua kali
        // OCR full-page untuk setiap halaman.
        let ocr_input = if searchable {
            match config.as_ref() {
                Some(config) if config.enabled => {
                    let layer_dir = staged_dir.join("layer-input");
                    std::fs::create_dir_all(&layer_dir)
                        .map_err(|e| format!("Gagal membuat direktori layer: {e}"))?;
                    let with_layer = layer_dir.join("with-layer.pdf");
                    std::fs::copy(source, &with_layer)
                        .map_err(|e| format!("Gagal menyiapkan input layer: {e}"))?;
                    let pdfium = pdfium_instance(&app)?;
                    let engine =
                        ocr_layer::Tesseract::new(tesseract.clone(), "ind+eng".to_string());
                    let rewrite = ocr_layer::rewrite_searchable_layer(
                        &pdfium,
                        &with_layer,
                        engine,
                        config,
                        &cancel,
                        &layer_dir,
                    )?;
                    layer_note = format!(
                        "Lapisan teks ditulis ulang: {} kata pada {} halaman (sebelum OCRmyPDF).",
                        rewrite.word_count, rewrite.page_count
                    );
                    if !rewrite.corrections.is_empty() {
                        warnings.push(format!(
                            "Koreksi kata-level: {} perubahan sebelum text layer ditulis.",
                            rewrite.corrections.len()
                        ));
                        for correction in rewrite.corrections.iter().take(10) {
                            warnings.push(format!(
                                "Halaman {}: {} → {} ({})",
                                correction.page,
                                correction.before,
                                correction.after,
                                correction.rule
                            ));
                        }
                        if rewrite.corrections.len() > 10 {
                            warnings.push(format!(
                                "… dan {} koreksi lainnya.",
                                rewrite.corrections.len() - 10
                            ));
                        }
                    }
                    with_layer
                }
                _ => {
                    warnings.push(
                        "Post-processing OCR dimatikan; text layer OCRmyPDF dipertahankan apa adanya."
                            .into(),
                    );
                    source.to_path_buf()
                }
            }
        } else {
            source.to_path_buf()
        };

        let (ocr_output, sidecar) = if searchable {
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
        command.env("PATH", joined).args(["-l", "ind+eng"]);
        if searchable {
            // Deskew/rotate mengubah piksel asli; untuk searchable PDF lapse
            // teks sudah ditulis Tesseract di atas gambar yang tidak diubah.
            command.arg("--skip-text");
        } else {
            command.args(["--deskew", "--rotate-pages", "--force-ocr"]);
        }
        if target_key == "ocr text" {
            let sidecar_path = output.with_extension("txt");
            command.arg("--sidecar").arg(&sidecar_path);
        }
        command.arg(&ocr_input).arg(&ocr_output);
        let expected_output = if searchable {
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
                format!(
                    "Akurasi OCR bergantung pada kualitas scan; gambar asli dipertahankan dengan --skip-text. {layer_note}"
                )
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
    let scan_docx_ocr = pdf_input
        && target_ext == "docx"
        && pdf_docx_ocr_enabled()
        && pdf_docx_ocr_engines().is_some()
        && !pdf_has_extractable_text(&app, source)?;
    let pdf_docx_route =
        pdf_input && target_ext == "docx" && (scan_docx_ocr || pdf2docx_path().is_some());
    let route_supported = if pdf_docx_route {
        true
    } else if target_ext == "md" {
        anymd_path().is_some()
    } else if pdf_input && target_ext == "docx" && anymd_path().is_some() && pandoc_path().is_some()
    {
        true
    } else if pdf_input && pdf_writer_route(target_ext) {
        // LibreOffice tidak bisa mengekspor PDF langsung ke DOCX/ODT/TXT/RTF.
        // Rutenya: PDF -> HTML (Draw) -> format Writer dengan filter eksplisit.
        libreoffice_path().is_some()
    } else if pdf_input && target_ext == "html" {
        libreoffice_path().is_some()
    } else if pdf_input {
        false
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

    let (engine, tier, warning) = if scan_docx_ocr {
        let work = tempfile::Builder::new()
            .prefix("documorph-docx-ocr-")
            .tempdir_in(staged_dir.as_path())
            .map_err(|e| format!("Gagal membuat folder OCR DOCX: {e}"))?;
        let pdfium = pdfium_instance(&app)?;
        let (chosen_engine, text) = choose_docx_ocr_text(&pdfium, source, &cancel, work.path())?;
        let markdown = work.path().join("ocr.md");
        std::fs::write(&markdown, text).map_err(|e| format!("Gagal menulis teks OCR: {e}"))?;
        let pandoc = pandoc_path().ok_or("Pandoc tidak ditemukan.")?;
        let mut command = Command::new(pandoc);
        command.arg(&markdown).arg("-o").arg(&output);
        run_engine_for_output(
            &mut command,
            "Pandoc",
            &output,
            &cancel,
            Some((staged_dir.as_path(), target_ext)),
        )?;
        (
            chosen_engine,
            "B",
            Some("PDF scan tanpa teks; DOCX disusun dari teks OCR. Tabel, angka, dan tata letak bisa perlu koreksi.".into()),
        )
    } else if pdf_input && target_ext == "docx" && pdf2docx_path().is_some() {
        let converter = pdf2docx_path().ok_or("pdf2docx tidak ditemukan.")?;
        let mut command = Command::new(&converter);
        command.args(pdf2docx_convert_args(source, &output));
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
        let profile_url = libreoffice_profile_url(profile.path());
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
    } else if pdf_input && target_ext == "html" {
        let lo = libreoffice_path().ok_or("LibreOffice tidak ditemukan.")?;
        let profile = tempfile::Builder::new()
            .prefix("documorph-lo-")
            .tempdir()
            .map_err(|e| format!("Gagal membuat profile LibreOffice: {e}"))?;
        let profile_url = libreoffice_profile_url(profile.path());
        let mut command = Command::new(lo);
        command
            .arg(format!("-env:UserInstallation={profile_url}"))
            .args(["--headless", "--convert-to", "html", "--outdir"])
            .arg(output.parent().unwrap_or(Path::new(".")))
            .arg(source);
        run_engine_for_output(
            &mut command,
            "LibreOffice (PDF import)",
            &output,
            &cancel,
            Some((staged_dir.as_path(), "html")),
        )?;
        (
            "LibreOffice (PDF import)",
            "C",
            Some("PDF adalah format visual; teks dan layout direkonstruksi, hasil bisa berubah signifikan. Scan memerlukan OCR.".into()),
        )
    } else if pdf_input && pdf_writer_route(target_ext) {
        let lo = libreoffice_path().ok_or("LibreOffice tidak ditemukan.")?;
        let profile = tempfile::Builder::new()
            .prefix("documorph-lo-")
            .tempdir()
            .map_err(|e| format!("Gagal membuat profile LibreOffice: {e}"))?;
        let profile_url = libreoffice_profile_url(profile.path());
        let html_dir = tempfile::Builder::new()
            .prefix("lo-html-")
            .tempdir_in(staged_dir.as_path())
            .map_err(|e| format!("Gagal membuat folder antara LibreOffice: {e}"))?;
        let stem = safe_stem(
            source
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("output"),
        );
        let html = html_dir.path().join(format!("{stem}.html"));

        let mut draw = Command::new(&lo);
        draw.arg(format!("-env:UserInstallation={profile_url}"))
            .args(["--headless", "--convert-to", "html", "--outdir"])
            .arg(html_dir.path())
            .arg(source);
        run_engine_for_output(
            &mut draw,
            "LibreOffice (PDF import)",
            &html,
            &cancel,
            Some((html_dir.path(), "html")),
        )?;

        let mut writer = Command::new(&lo);
        writer
            .arg(format!("-env:UserInstallation={profile_url}"))
            .args(["--headless", "--infilter=HTML (StarWriter)", "--convert-to"])
            .arg(target_ext)
            .arg("--outdir")
            .arg(output.parent().unwrap_or(Path::new(".")))
            .arg(&html);
        run_engine_for_output(
            &mut writer,
            "LibreOffice (PDF import → Writer)",
            &output,
            &cancel,
            Some((staged_dir.as_path(), target_ext)),
        )?;
        (
            "LibreOffice (PDF import → Writer)",
            "C",
            Some("PDF diimpor sebagai HTML lalu disusun ulang dengan Writer. Teks, tabel, dan layout bisa berubah signifikan; scan memerlukan OCR.".into()),
        )
    } else if target_ext != "pdf" && office_input && office_output {
        let lo = libreoffice_path().ok_or("LibreOffice tidak ditemukan.")?;
        let profile = tempfile::Builder::new()
            .prefix("documorph-lo-")
            .tempdir()
            .map_err(|e| format!("Gagal membuat profile LibreOffice: {e}"))?;
        let profile_url = libreoffice_profile_url(profile.path());
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
    fn pdf2docx_uses_positional_output_argument() {
        let source = Path::new("laporan.pdf");
        let output = Path::new("laporan.docx");
        let args = pdf2docx_convert_args(source, output);

        assert_eq!(args.len(), 3);
        assert_eq!(args[0], "convert");
        assert_eq!(args[1], source.as_os_str());
        assert_eq!(args[2], output.as_os_str());
        assert!(
            args.iter().all(|arg| arg != "-d"),
            "pdf2docx tidak mengenal opsi -d"
        );
    }

    #[test]
    fn pdf_writer_route_covers_only_formats_libreoffice_can_export_from_html() {
        for ext in ["docx", "txt", "odt", "rtf"] {
            assert!(pdf_writer_route(ext), ".{ext} butuh rantai Draw -> Writer");
        }
        for ext in ["pdf", "html", "md", "csv"] {
            assert!(!pdf_writer_route(ext), ".{ext} tidak memakai rantai Writer");
        }
    }

    #[test]
    fn libreoffice_profile_url_is_safe_for_windows_paths() {
        let profile = Path::new(r"C:\Users\Evraldi\AppData\Local\Temp\documorph lo");
        assert_eq!(
            libreoffice_profile_url(profile),
            "file:///C:/Users/Evraldi/AppData/Local/Temp/documorph%20lo"
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
    fn clearing_history_deletes_recorded_outputs_only() {
        let conn = Connection::open_in_memory().expect("in-memory db");
        db::migrate(&conn).expect("migrate");
        let dir = tempfile::tempdir().expect("tempdir");
        let docx = dir.path().join("laporan.docx");
        let txt = dir.path().join("catatan.txt");
        let payload = dir.path().join("payload.exe");
        for path in [&docx, &txt, &payload] {
            std::fs::write(path, b"keluaran").expect("tulis keluaran");
        }
        for (name, path) in [
            ("laporan.docx", &docx),
            ("catatan.txt", &txt),
            ("payload.exe", &payload),
        ] {
            insert_history_record(
                &conn,
                &HistoryItem {
                    name: name.into(),
                    output: path.to_string_lossy().into_owned(),
                    engine: "Pandoc".into(),
                    tier: "B".into(),
                    at: 1234,
                    warnings: Vec::new(),
                },
            )
            .expect("insert");
        }

        assert_eq!(clear_history(&conn).expect("clear"), 2);
        assert!(!docx.exists());
        assert!(!txt.exists());
        assert!(payload.exists());
        assert!(list_history(&conn).expect("list").is_empty());
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

    fn diag_engine_path(env: &str, discovered: Option<PathBuf>, fallback: &str) -> PathBuf {
        std::env::var_os(env)
            .map(PathBuf::from)
            .or(discovered)
            .unwrap_or_else(|| PathBuf::from(fallback))
    }

    /// Path engine untuk benchmark. Bisa dioverride lewat env supaya mesin
    /// dengan install portable tetap bisa menjalankan tes ignored ini.
    #[allow(clippy::type_complexity)]
    fn diag_engines() -> (
        PathBuf,
        PathBuf,
        PathBuf,
        PathBuf,
        PathBuf,
        PathBuf,
        PathBuf,
    ) {
        let paths = engines::engine_paths();
        (
            diag_engine_path(
                "PDF2DOCX_PATH",
                paths.pdf2docx,
                r"C:\Users\Evraldi\AppData\Local\Temp\opencode\pdfdocx-venv\Scripts\pdf2docx.exe",
            ),
            diag_engine_path(
                "ANYMD_PATH",
                paths.anymd,
                r"C:\Users\Evraldi\AppData\Roaming\npm\anymd.cmd",
            ),
            diag_engine_path(
                "PANDOC_PATH",
                paths.pandoc,
                r"C:\Users\Evraldi\AppData\Local\Pandoc\pandoc.exe",
            ),
            diag_engine_path(
                "LIBREOFFICE_PATH",
                paths.libreoffice,
                r"C:\Program Files\LibreOffice\program\soffice.exe",
            ),
            diag_engine_path(
                "OCRMYPDF_PATH",
                paths.ocrmypdf,
                r"C:\Data\01_Development\python3.10.11\Scripts\ocrmypdf.exe",
            ),
            diag_engine_path(
                "TESSERACT_PATH",
                paths.tesseract,
                r"C:\Program Files\Tesseract-OCR\tesseract.exe",
            ),
            diag_engine_path(
                "GHOSTSCRIPT_PATH",
                paths.ghostscript,
                r"C:\Program Files\gs\gs10.08.0\bin\gswin64c.exe",
            ),
        )
    }

    fn diag_run(label: &str, command: &mut Command, expected: &Path) -> f32 {
        crate::process::hide_console(command);
        let started = Instant::now();
        let result = command
            .output()
            .unwrap_or_else(|error| panic!("{label}: {error}"));
        let seconds = started.elapsed().as_secs_f32();
        assert!(
            result.status.success(),
            "{label}: {}",
            String::from_utf8_lossy(&result.stderr)
                .chars()
                .take(400)
                .collect::<String>()
        );
        assert!(expected.is_file(), "{label}: output {expected:?} tidak ada");
        seconds
    }

    fn diag_unescape(value: &str) -> String {
        value
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&")
    }

    fn diag_count_tag(xml: &str, name: &str) -> usize {
        let open = format!("<{name}");
        xml.match_indices(&open)
            .filter(|(index, _)| {
                matches!(
                    xml.as_bytes().get(index + open.len()),
                    Some(b'>' | b' ' | b'/')
                )
            })
            .count()
    }

    /// Ambang teks + bentuk dari DOCX hasil konversi. Cukup untuk membandingkan
    /// rute secara relatif tanpa bergantung pada library DOCX tambahan.
    fn diag_docx_summary(path: &Path) -> (String, usize, usize, usize, usize) {
        use std::io::Read;

        let file = std::fs::File::open(path).expect("buka docx");
        let mut archive = zip::ZipArchive::new(file).expect("zip docx");
        let mut xml = String::new();
        archive
            .by_name("word/document.xml")
            .expect("document.xml")
            .read_to_string(&mut xml)
            .expect("baca document.xml");

        let bytes = xml.as_bytes();
        let mut text = String::new();
        let mut index = 0usize;
        while index < bytes.len() {
            if bytes[index..].starts_with(b"<w:t") {
                let after = index + 4;
                if matches!(bytes.get(after), Some(b'>' | b' '))
                    && let Some(gap) = bytes[after..].iter().position(|byte| *byte == b'>')
                {
                    let start = after + gap + 1;
                    if let Some(end) = bytes[start..]
                        .windows(6)
                        .position(|window| window == b"</w:t>")
                    {
                        text.push_str(&diag_unescape(&xml[start..start + end]));
                        index = start + end + 6;
                        continue;
                    }
                }
            }
            if bytes[index..].starts_with(b"</w:p>") {
                text.push('\n');
                index += 6;
                continue;
            }
            index += 1;
        }

        (
            text,
            diag_count_tag(&xml, "w:tbl"),
            diag_count_tag(&xml, "w:tr"),
            diag_count_tag(&xml, "w:tc"),
            diag_count_tag(&xml, "w:drawing") + diag_count_tag(&xml, "w:pict"),
        )
    }

    fn diag_pdfium() -> Pdfium {
        let dll = Path::new(env!("CARGO_MANIFEST_DIR")).join("bin/pdfium.dll");
        Pdfium::new(Pdfium::bind_to_library(&dll).expect("PDFium DLL"))
    }

    /// OCR langsung memakai pipeline Searchable PDF kita: render BGR 300 DPI,
    /// Tesseract sekali jalan, lalu koreksi kata sebelum jadi teks.
    fn diag_direct_text(
        pdfium: &Pdfium,
        tesseract: &Path,
        source: &Path,
        dir: &Path,
    ) -> (String, f32, f32) {
        let pdf = pdfium
            .load_pdf_from_file(source, None)
            .expect("buka PDF untuk OCR");
        let config = ocr_postprocess::PostprocessConfig::from_env();
        let started = Instant::now();
        let mut combined = String::new();
        let mut confidence_total = 0.0f32;
        let mut confidence_words = 0usize;

        for page_index in 0..pdf.pages().len() {
            let page = pdf.pages().get(page_index).expect("halaman");
            let width = ((page.width().value / 72.0) * config.render_dpi as f32)
                .round()
                .max(1.0) as usize;
            let height = ((page.height().value / 72.0) * config.render_dpi as f32)
                .round()
                .max(1.0) as usize;
            let bitmap = page
                .render_with_config(
                    &PdfRenderConfig::new()
                        .set_target_size(width as i32, height as i32)
                        .set_format(PdfBitmapFormat::BGR)
                        .set_reverse_byte_order(true),
                )
                .expect("render halaman");
            let raw = bitmap.as_raw_bytes();
            let stride = raw.len() / height.max(1);
            let row_bytes = width * 3;
            let mut rgb = Vec::with_capacity(row_bytes * height);
            for row in raw.chunks_exact(stride) {
                rgb.extend_from_slice(&row[..row_bytes]);
            }
            let mut ppm = Vec::with_capacity(32 + rgb.len());
            ppm.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
            ppm.extend_from_slice(&rgb);
            let ppm_path = dir.join(format!("direct-page-{page_index}.ppm"));
            std::fs::write(&ppm_path, ppm).expect("tulis PPM");

            let mut command = Command::new(tesseract);
            command
                .arg(&ppm_path)
                .arg("stdout")
                .args(["-l", "ind+eng", "--psm", "3", "tsv"]);
            crate::process::hide_console(&mut command);
            let result = command.output().expect("jalankan Tesseract");
            assert!(
                result.status.success(),
                "Tesseract: {}",
                String::from_utf8_lossy(&result.stderr)
                    .chars()
                    .take(400)
                    .collect::<String>()
            );
            let tsv = String::from_utf8_lossy(&result.stdout).into_owned();
            let words = ocr_layer::parse_tsv(&tsv, (page_index + 1) as usize);
            let confidences: Vec<f32> = words
                .iter()
                .filter(|word| word.confidence >= 0.0)
                .map(|word| word.confidence)
                .collect();
            let mean_confidence = confidences.iter().sum::<f32>() / confidences.len().max(1) as f32;
            confidence_total += mean_confidence * confidences.len().max(1) as f32;
            confidence_words += confidences.len();
            let (processed, _) = ocr_postprocess::postprocess(words, &config, None);
            if !combined.is_empty() {
                combined.push_str("\n\n");
            }
            combined.push_str(&words_to_docx_text(&processed));
        }

        let mean = confidence_total / confidence_words.max(1) as f32;
        (combined, started.elapsed().as_secs_f32(), mean)
    }

    /// Sidecar OCRmyPDF seperti rute OCR Text produksi. Dipakai sebagai
    /// kandidat teks alternatif sebelum dirakit jadi DOCX dengan Pandoc.
    fn diag_ocrmypdf_text(
        ocrmypdf: &Path,
        tesseract: &Path,
        ghostscript: &Path,
        source: &Path,
        dir: &Path,
    ) -> (String, f32) {
        let sidecar = dir.join("ocrmypdf.txt");
        let output = dir.join("ocrmypdf.pdf");
        let path = std::env::join_paths([
            tesseract.parent().expect("dir tesseract"),
            ghostscript.parent().expect("dir ghostscript"),
            Path::new(r"C:\Windows\System32"),
        ])
        .expect("PATH engine");
        let mut command = Command::new(ocrmypdf);
        command
            .env("PATH", path)
            .args(["-l", "ind+eng", "--deskew", "--rotate-pages", "--force-ocr"])
            .arg("--sidecar")
            .arg(&sidecar)
            .arg(source)
            .arg(&output);
        let seconds = diag_run("ocrmypdf sidecar", &mut command, &output);
        let text = std::fs::read_to_string(&sidecar).expect("baca sidecar OCRmyPDF");
        (text, seconds)
    }

    /// Buat PDF dengan invisible text layer memakai OCRmyPDF, sebagai input
    /// untuk pdf2docx --ocr=2 (mode "baca teks tersembunyi saja").
    fn diag_searchable_pdf(
        ocrmypdf: &Path,
        tesseract: &Path,
        ghostscript: &Path,
        source: &Path,
        dir: &Path,
    ) -> (PathBuf, f32) {
        let output = dir.join("searchable.pdf");
        let path = std::env::join_paths([
            tesseract.parent().expect("dir tesseract"),
            ghostscript.parent().expect("dir ghostscript"),
            Path::new(r"C:\Windows\System32"),
        ])
        .expect("PATH engine");
        let mut command = Command::new(ocrmypdf);
        command
            .env("PATH", path)
            .args(["-l", "ind+eng", "--skip-text", "--sidecar"])
            .arg(dir.join("sidecar.txt"))
            .arg(source)
            .arg(&output);
        let seconds = diag_run("ocrmypdf skip-text", &mut command, &output);
        (output, seconds)
    }

    /// Benchmark rute PDF -> DOCX. Default empat sample representatif; set
    /// DOCUMORPH_PDFDOCX_DOCS=all untuk menjalankan sebelas sample.
    #[test]
    #[ignore = "butuh pdf2docx, AnyMD, Pandoc, LibreOffice, OCRmyPDF + PDF sample"]
    fn pdf_to_docx_route_diagnostic() {
        let (pdf2docx, anymd, pandoc, lo, ocrmypdf, tesseract, ghostscript) = diag_engines();
        let selected = std::env::var("DOCUMORPH_PDFDOCX_DOCS")
            .unwrap_or_else(|_| "image-only-no-text,id-clean,id-table,id-two-column".into());
        let names: Vec<String> = selected
            .split(',')
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();
        let documents = ocr_metrics::cross_documents();
        let pdfium = diag_pdfium();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut anymd_path_parts = vec![
            tesseract.parent().expect("dir tesseract").to_path_buf(),
            PathBuf::from(r"C:\Windows\System32"),
        ];
        if let Some(existing) = std::env::var_os("PATH") {
            anymd_path_parts.extend(std::env::split_paths(&existing));
        }
        let anymd_path = std::env::join_paths(anymd_path_parts).expect("PATH untuk AnyMD");

        for name in names {
            let document = documents
                .iter()
                .find(|item| item.name == name.as_str())
                .unwrap_or_else(|| panic!("sample PDF tidak dikenal: {name}"));
            let dir = temp.path().join(&name);
            std::fs::create_dir_all(&dir).expect("dir sample");
            println!("\n== {name} ==");

            let report = |label: &str, seconds: f32, docx: &Path| {
                let (text, tables, rows, cells, images) = diag_docx_summary(docx);
                println!(
                    "{label:<17} {seconds:6.2}s CER={:.4} WER={:.4} words={:<5} tbl={tables}/{rows}/{cells} img={images}",
                    ocr_metrics::cer(&document.reference, &text),
                    ocr_metrics::wer(&document.reference, &text),
                    ocr_metrics::normalize_for_metrics(&text)
                        .split_whitespace()
                        .count(),
                );
            };

            // Rute produksi saat ini when pdf2docx tersedia.
            let out = dir.join("pdf2docx.docx");
            let mut command = Command::new(&pdf2docx);
            command.args(pdf2docx_convert_args(&document.pdf, &out));
            let seconds = diag_run("pdf2docx", &mut command, &out);
            report("pdf2docx", seconds, &out);

            // Mode pdf2docx yang hanya membaca hidden text layer.
            let out = dir.join("pdf2docx-ocr2.docx");
            let mut command = Command::new(&pdf2docx);
            command
                .args(pdf2docx_convert_args(&document.pdf, &out))
                .arg("--ocr=2");
            let seconds = diag_run("pdf2docx ocr=2", &mut command, &out);
            report("pdf2docx-ocr2", seconds, &out);

            // Fallback produksi: AnyMD -> Pandoc.
            let markdown = dir.join("anymd.md");
            let out = dir.join("anymd.docx");
            let mut command = Command::new(&anymd);
            command
                .env("PATH", &anymd_path)
                .arg(&document.pdf)
                .arg("-o")
                .arg(&markdown);
            let extract_seconds = diag_run("anymd", &mut command, &markdown);
            let mut command = Command::new(&pandoc);
            command.arg(&markdown).arg("-o").arg(&out);
            let build_seconds = diag_run("pandoc", &mut command, &out);
            report("anymd-pandoc", extract_seconds + build_seconds, &out);

            // AnyMD dengan OCR Tesseract eksplisit -> Pandoc.
            let markdown = dir.join("anymd-tesseract.md");
            let out = dir.join("anymd-tesseract.docx");
            let mut command = Command::new(&anymd);
            command
                .env("PATH", &anymd_path)
                .arg(&document.pdf)
                .arg("-o")
                .arg(&markdown)
                .args(["--ocr", "tesseract"]);
            let extract_seconds = diag_run("anymd tesseract", &mut command, &markdown);
            let mut command = Command::new(&pandoc);
            command.arg(&markdown).arg("-o").arg(&out);
            let build_seconds = diag_run("pandoc", &mut command, &out);
            report("anymd-ocr", extract_seconds + build_seconds, &out);

            // OCR langsung pipeline kita -> Markdown -> Pandoc -> DOCX.
            let direct_dir = dir.join("direct");
            std::fs::create_dir_all(&direct_dir).expect("dir direct");
            let (direct_text, ocr_seconds, _) =
                diag_direct_text(&pdfium, &tesseract, &document.pdf, &direct_dir);
            let markdown = direct_dir.join("direct.md");
            std::fs::write(&markdown, direct_text).expect("tulis markdown OCR");
            let out = direct_dir.join("direct.docx");
            let mut command = Command::new(&pandoc);
            command.arg(&markdown).arg("-o").arg(&out);
            let build_seconds = diag_run("pandoc direct", &mut command, &out);
            report("direct-pandoc", ocr_seconds + build_seconds, &out);

            // Sidecar OCRmyPDF -> Markdown -> Pandoc -> DOCX.
            let ocrmypdf_dir = dir.join("ocrmypdf");
            std::fs::create_dir_all(&ocrmypdf_dir).expect("dir ocrmypdf");
            let (ocr_text, ocr_seconds) = diag_ocrmypdf_text(
                &ocrmypdf,
                &tesseract,
                &ghostscript,
                &document.pdf,
                &ocrmypdf_dir,
            );
            let markdown = ocrmypdf_dir.join("ocrmypdf.md");
            std::fs::write(&markdown, ocr_text).expect("tulis markdown OCRmyPDF");
            let out = ocrmypdf_dir.join("ocrmypdf.docx");
            let mut command = Command::new(&pandoc);
            command.arg(&markdown).arg("-o").arg(&out);
            let build_seconds = diag_run("pandoc ocrmypdf", &mut command, &out);
            report("ocrmypdf-pandoc", ocr_seconds + build_seconds, &out);

            // Rute cadangan LibreOffice Draw -> Writer.
            let profile = tempfile::Builder::new()
                .prefix("documorph-lo-")
                .tempdir()
                .expect("profile LO");
            let profile_url = libreoffice_profile_url(profile.path());
            let html_dir = dir.join("lo-html");
            std::fs::create_dir_all(&html_dir).expect("dir html");
            let stem = safe_stem(
                document
                    .pdf
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("output"),
            );
            let html = html_dir.join(format!("{stem}.html"));
            let mut command = Command::new(&lo);
            command
                .arg(format!("-env:UserInstallation={profile_url}"))
                .args(["--headless", "--convert-to", "html", "--outdir"])
                .arg(&html_dir)
                .arg(&document.pdf);
            let draw_seconds = diag_run("lo draw", &mut command, &html);
            let out_dir = dir.join("lo-out");
            std::fs::create_dir_all(&out_dir).expect("dir out LO");
            let out = out_dir.join(format!("{stem}.docx"));
            let mut command = Command::new(&lo);
            command
                .arg(format!("-env:UserInstallation={profile_url}"))
                .args([
                    "--headless",
                    "--infilter=HTML (StarWriter)",
                    "--convert-to",
                    "docx",
                    "--outdir",
                ])
                .arg(&out_dir)
                .arg(&html);
            let writer_seconds = diag_run("lo writer", &mut command, &out);
            report("lo-writer", draw_seconds + writer_seconds, &out);

            // OCR dulu jadi PDF searchable, lalu baca hidden text-nya saja.
            let pre_dir = dir.join("preocr");
            std::fs::create_dir_all(&pre_dir).expect("dir preocr");
            let (searchable, ocr_seconds) =
                diag_searchable_pdf(&ocrmypdf, &tesseract, &ghostscript, &document.pdf, &pre_dir);
            let out = pre_dir.join("p2d.docx");
            let mut command = Command::new(&pdf2docx);
            command
                .args(pdf2docx_convert_args(&searchable, &out))
                .arg("--ocr=2");
            let convert_seconds = diag_run("preocr p2d", &mut command, &out);
            report("preocr-p2d", ocr_seconds + convert_seconds, &out);
        }
    }

    /// Cari sinyal pemilih direct vs OCRmyPDF tanpa melihat ground truth.
    #[test]
    #[ignore = "butuh Tesseract + OCRmyPDF + 11 PDF sample"]
    fn pdf_to_docx_selector_diagnostic() {
        let (_, _, _, _, ocrmypdf, tesseract, ghostscript) = diag_engines();
        let pdfium = diag_pdfium();
        let temp = tempfile::tempdir().expect("tempdir");
        let mut correct = 0usize;

        for document in ocr_metrics::cross_documents() {
            let dir = temp.path().join(document.name);
            std::fs::create_dir_all(&dir).expect("dir sample");
            let (direct_text, _, confidence) =
                diag_direct_text(&pdfium, &tesseract, &document.pdf, &dir);
            let (ocr_text, _) =
                diag_ocrmypdf_text(&ocrmypdf, &tesseract, &ghostscript, &document.pdf, &dir);
            let direct_cer = ocr_metrics::cer(&document.reference, &direct_text);
            let ocr_cer = ocr_metrics::cer(&document.reference, &ocr_text);
            let chosen = if confidence >= 85.0 { "direct" } else { "ocr" };
            let chosen_cer = if confidence >= 85.0 {
                direct_cer
            } else {
                ocr_cer
            };
            let best = direct_cer.min(ocr_cer);
            if chosen_cer <= best + 1e-6 {
                correct += 1;
            }
            println!(
                "{:<20} conf={confidence:6.2} direct={direct_cer:.4}/{:<5} ocr={ocr_cer:.4}/{:<5} pilih={chosen:<6} CER={chosen_cer:.4}",
                document.name,
                ocr_metrics::normalize_for_metrics(&direct_text)
                    .split_whitespace()
                    .count(),
                ocr_metrics::normalize_for_metrics(&ocr_text)
                    .split_whitespace()
                    .count(),
            );
        }
        println!("SELECTOR BENAR: {correct}/11");
    }

    #[test]
    fn docx_text_marks_paragraph_gaps_without_touching_words() {
        let word = |top: i32, text: &str| ocr_postprocess::OcrWord {
            page: 1,
            block: 0,
            paragraph: 0,
            line: top as usize,
            word: 0,
            bbox: ocr_postprocess::BBox {
                left: 0,
                top,
                width: 100,
                height: 20,
            },
            text: text.to_string(),
            confidence: 95.0,
        };
        let words = vec![
            word(0, "Satu"),
            word(0, "baris"),
            word(40, "lanjutan"),
            word(100, "Paragraf"),
            word(100, "baru"),
        ];
        assert_eq!(
            words_to_docx_text(&words),
            "Satu baris\nlanjutan\n\nParagraf baru\n"
        );
    }

    #[test]
    fn docx_text_starts_each_bullet_item_in_own_paragraph() {
        let word = |top: i32, text: &str| ocr_postprocess::OcrWord {
            page: 1,
            block: 0,
            paragraph: 0,
            line: top as usize,
            word: 0,
            bbox: ocr_postprocess::BBox {
                left: 0,
                top,
                width: 100,
                height: 20,
            },
            text: text.to_string(),
            confidence: 95.0,
        };
        let words = vec![
            word(0, "•"),
            word(0, "Satu"),
            word(0, "item"),
            word(40, "•"),
            word(40, "Dua"),
            word(40, "item"),
        ];
        assert_eq!(words_to_docx_text(&words), "• Satu item\n\n• Dua item\n");
    }

    #[test]
    fn sidecar_postprocess_keeps_numeric_table_cells() {
        let config = ocr_postprocess::PostprocessConfig::from_env();
        assert_eq!(
            postprocess_sidecar_text("2 Foo\n2 Bar", &config),
            "2 Foo\n2 Bar"
        );
    }

    #[test]
    fn sidecar_postprocess_normalizes_noise_and_fractions() {
        let config = ocr_postprocess::PostprocessConfig::from_env();
        assert_eq!(
            postprocess_sidecar_text("© Disk drive", &config),
            "• Disk drive"
        );
        assert_eq!(
            postprocess_sidecar_text("disk drive 312”", &config),
            "disk drive 3½″"
        );
    }

    #[test]
    fn sidecar_postprocess_starts_each_bullet_line_in_own_paragraph() {
        let config = ocr_postprocess::PostprocessConfig::from_env();
        assert_eq!(
            postprocess_sidecar_text("© Satu\n© Dua\nlanjutan", &config),
            "• Satu\n\n• Dua\nlanjutan"
        );
    }

    #[test]
    fn sidecar_postprocess_preserves_blank_lines_between_paragraphs() {
        let config = ocr_postprocess::PostprocessConfig::from_env();
        assert_eq!(
            postprocess_sidecar_text("Satu\n\nDua", &config),
            "Satu\n\nDua"
        );
    }

    #[test]
    fn sidecar_postprocess_handles_form_feed_and_hides_sentinels() {
        let config = ocr_postprocess::PostprocessConfig::from_env();
        let output = postprocess_sidecar_text("© Disk drive\u{000C}© Disk drive", &config);
        assert_eq!(output, "• Disk drive\n\n• Disk drive");
        assert!(!output.contains('\u{E000}'));
        assert!(!output.contains('\u{E001}'));
    }

    #[test]
    fn rollback_flags_accept_the_same_disabled_values() {
        for value in [Some("0"), Some(" false "), Some("no"), Some("off")] {
            assert!(flag_disabled(value), "{value:?} harus dimatikan");
        }
        for value in [
            None,
            Some(""),
            Some("1"),
            Some("true"),
            Some("OFF"),
            Some("aktif"),
        ] {
            assert!(!flag_disabled(value), "{value:?} tidak boleh dimatikan");
        }
    }

    #[test]
    fn canonical_windows_prefix_is_removed_for_engine_paths() {
        assert_eq!(
            plain_windows_path(Path::new(r"\\?\C:\Program Files\Tesseract-OCR")),
            Path::new(r"C:\Program Files\Tesseract-OCR")
        );
        assert_eq!(
            plain_windows_path(Path::new(r"\\?\UNC\server\share\tessdata")),
            Path::new(r"\\server\share\tessdata")
        );
        assert_eq!(
            plain_windows_path(Path::new(r"C:\already\plain")),
            Path::new(r"C:\already\plain")
        );
    }

    /// Tes runtime untuk fungsi produksi yang dipakai `convert_file`, bukan
    /// helper diagnosa. Jalankan dengan environment OCR default.
    #[test]
    #[ignore = "butuh Tesseract + OCRmyPDF + 11 PDF sample nyata"]
    fn pdf_to_docx_scan_route_matches_ground_truth() {
        let (_, _, pandoc, _, _, _, _) = diag_engines();
        let pdfium = diag_pdfium();
        let temp = tempfile::tempdir().expect("tempdir");
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let limits: &[(&str, f64, f64)] = &[
            ("image-only-no-text", 0.01, 0.01),
            ("id-clean", 0.0, 0.0),
            ("id-noisy", 0.0, 0.0),
            ("id-skew", 0.0, 0.0),
            ("id-table", 0.16, 0.18),
            ("id-lowres", 0.0, 0.0),
            ("id-blur", 0.0, 0.0),
            ("id-rot90", 0.0, 0.0),
            ("id-table-skew", 0.22, 0.28),
            ("id-two-column", 0.0, 0.0),
            ("id-linn-noise", 0.02, 0.02),
        ];

        for document in ocr_metrics::cross_documents() {
            let (max_cer, max_cer_ref) = limits
                .iter()
                .find(|(name, _, _)| *name == document.name)
                .map(|(_, cer, cer_ref)| (*cer, *cer_ref))
                .expect("batas CER untuk sample");
            let dir = temp.path().join(document.name);
            std::fs::create_dir_all(&dir).expect("dir sample");
            let (engine, text) = choose_docx_ocr_text(&pdfium, &document.pdf, &cancel, &dir)
                .expect("rute OCR PDF→DOCX");
            let cer = ocr_metrics::cer(&document.reference, &text);
            let cer_ref = ocr_metrics::cer_reference(&document.reference, &text);
            let markdown = dir.join("route.md");
            let docx = dir.join("route.docx");
            std::fs::write(&markdown, &text).expect("tulis markdown produksi");
            let mut command = Command::new(&pandoc);
            command.arg(&markdown).arg("-o").arg(&docx);
            diag_run("Pandoc", &mut command, &docx);
            let (docx_text, tables, rows, cells, images) = diag_docx_summary(&docx);
            let docx_cer = ocr_metrics::cer(&document.reference, &docx_text);
            let docx_cer_ref = ocr_metrics::cer_reference(&document.reference, &docx_text);
            let paragraphs = docx_text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count();
            println!(
                "{:<20} engine={engine:<18} CER={cer:.4}/{cer_ref:.4} DOCX={docx_cer:.4}/{docx_cer_ref:.4} batas={max_cer:.4}/{max_cer_ref:.4} par={paragraphs} tbl={tables}/{rows}/{cells} img={images}",
                document.name
            );
            assert!(
                !text.contains('\u{00FF}') && !docx_text.contains('\u{00FF}'),
                "{}: U+00FF masih ada di teks atau DOCX",
                document.name
            );
            assert!(
                !docx_text.contains('\u{E000}') && !docx_text.contains('\u{E001}'),
                "{}: sentinel koreksi sidecar bocor ke DOCX",
                document.name
            );
            assert!(
                cer <= max_cer && docx_cer <= max_cer + 1e-9,
                "{}: CER {cer:.4}/DOCX {docx_cer:.4} melebihi batas {max_cer:.4}",
                document.name
            );
            assert!(
                cer_ref <= max_cer_ref && docx_cer_ref <= max_cer_ref + 1e-9,
                "{}: CER-ref {cer_ref:.4}/DOCX {docx_cer_ref:.4} melebihi batas {max_cer_ref:.4}",
                document.name
            );
            if document.name == "image-only-no-text" {
                assert!(
                    paragraphs >= 20,
                    "{}: DOCX hanya punya {paragraphs} paragraf",
                    document.name
                );
                for token in ["Sequencer", "function", "is", "3½"] {
                    assert!(
                        docx_text.contains(token),
                        "{}: DOCX tidak bisa dicari dengan '{token}'",
                        document.name
                    );
                }
            }
            if document.name == "id-linn-noise" {
                for token in ["Sequencer", "function", "is"] {
                    assert!(
                        docx_text.contains(token),
                        "{}: DOCX tidak bisa dicari dengan '{token}'",
                        document.name
                    );
                }
            }
        }
    }
}
