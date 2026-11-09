use pdfium_render::prelude::*;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use wait_timeout::ChildExt;

static CANCEL_CONVERSION: AtomicBool = AtomicBool::new(false);
static ANYMD_EXECUTABLE: OnceLock<Option<PathBuf>> = OnceLock::new();
static PDF2DOCX_EXECUTABLE: OnceLock<Option<PathBuf>> = OnceLock::new();
static PDFIUM_INSTANCE: OnceLock<Result<Pdfium, String>> = OnceLock::new();

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
#[derive(Serialize, Deserialize)]
struct HistoryItem {
    name: String,
    output: String,
    engine: String,
    tier: String,
    at: i64,
}

#[derive(Serialize)]
struct ConversionReport {
    output: String,
    engine: String,
    tier: String,
    duration_ms: u128,
    warning: Option<String>,
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

fn locate(program: &str, candidates: &[PathBuf]) -> Option<PathBuf> {
    which::which(program)
        .ok()
        .or_else(|| candidates.iter().find(|p| p.is_file()).cloned())
}
fn is_anymd_cli(path: &Path) -> bool {
    let mut command = Command::new(path);
    command
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let Ok(Some(status)) = child.wait_timeout(Duration::from_secs(3)) else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };
    if !status.success() {
        return false;
    }
    let mut version = String::new();
    let _ = child
        .stdout
        .take()
        .is_some_and(|mut stdout| stdout.read_to_string(&mut version).is_ok());
    version.trim_start().starts_with("anymd ")
}

fn anymd_path() -> Option<PathBuf> {
    ANYMD_EXECUTABLE.get_or_init(discover_anymd_path).clone()
}

fn discover_anymd_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("ANYMD_PATH") {
        candidates.push(PathBuf::from(configured));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join("Programs/anymd-venv/Scripts/anymd.exe"));
    }
    candidates.push(PathBuf::from(r"C:\Data\anymd-venv\Scripts\anymd.exe"));
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join("anymd.exe"));
        }
    }
    candidates
        .into_iter()
        .filter(|path| path.is_file())
        .find(|path| is_anymd_cli(path))
}
fn pdf2docx_path() -> Option<PathBuf> {
    PDF2DOCX_EXECUTABLE
        .get_or_init(discover_pdf2docx_path)
        .clone()
}

fn discover_pdf2docx_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("PDF2DOCX_PATH") {
        candidates.push(PathBuf::from(configured));
    }
    if let Some(scripts) = std::env::var_os("ANYMD_PATH")
        .and_then(|anymd| PathBuf::from(anymd).parent().map(Path::to_path_buf))
    {
        candidates.push(scripts.join("pdf2docx.exe"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join("Programs/anymd-venv/Scripts/pdf2docx.exe"));
    }
    candidates.push(PathBuf::from(r"C:\Data\anymd-venv\Scripts\pdf2docx.exe"));
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("pdf2docx.exe")));
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn pandoc_path() -> Option<PathBuf> {
    let mut paths = vec![
        PathBuf::from(r"C:\Program Files\Pandoc\pandoc.exe"),
        PathBuf::from(r"C:\Tools\Pandoc\pandoc.exe"),
    ];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        paths.push(PathBuf::from(local).join("Pandoc/pandoc.exe"));
    }
    locate("pandoc", &paths)
}
fn libreoffice_path() -> Option<PathBuf> {
    locate(
        "soffice",
        &[
            PathBuf::from(r"C:\Program Files\LibreOffice\program\soffice.exe"),
            PathBuf::from(r"C:\Program Files (x86)\LibreOffice\program\soffice.exe"),
        ],
    )
}
fn typst_path() -> Option<PathBuf> {
    locate("typst", &[])
}
fn ocrmypdf_path() -> Option<PathBuf> {
    locate("ocrmypdf", &[])
}
fn tesseract_path() -> Option<PathBuf> {
    locate(
        "tesseract",
        &[PathBuf::from(
            r"C:\Program Files\Tesseract-OCR\tesseract.exe",
        )],
    )
}
fn ghostscript_path() -> Option<PathBuf> {
    locate(
        "gswin64c",
        &[PathBuf::from(
            r"C:\Program Files\gs\gs10.08.0\bin\gswin64c.exe",
        )],
    )
}

fn history_connection(app: &AppHandle) -> Result<Connection, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Tidak dapat menemukan folder data aplikasi: {e}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Tidak dapat membuat folder data aplikasi: {e}"))?;
    let conn = Connection::open(dir.join("history.sqlite3"))
        .map_err(|e| format!("Tidak dapat membuka database riwayat: {e}"))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| format!("Tidak dapat mengatur timeout database riwayat: {e}"))?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS history (id INTEGER PRIMARY KEY, name TEXT NOT NULL, output TEXT NOT NULL, engine TEXT NOT NULL, tier TEXT NOT NULL, at INTEGER NOT NULL);")
        .map_err(|e| format!("Tidak dapat menyiapkan database riwayat: {e}"))?;
    Ok(conn)
}

#[tauri::command]
fn reveal_output(path: String) -> Result<(), String> {
    let output = PathBuf::from(&path);
    if !output.is_file() {
        return Err(format!("Output file does not exist: {}", output.display()));
    }
    tauri_plugin_opener::reveal_item_in_dir(output)
        .map_err(|e| format!("Could not reveal output: {e}"))
}

#[tauri::command]
fn open_output(path: String) -> Result<(), String> {
    let output = PathBuf::from(&path);
    if !output.is_file() {
        return Err(format!("Output file does not exist: {}", output.display()));
    }
    tauri_plugin_opener::open_path(output.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| format!("Could not open output: {e}"))
}

#[tauri::command]
fn history_list(app: AppHandle) -> Result<Vec<HistoryItem>, String> {
    let conn = history_connection(&app)?;
    let mut stmt = conn.prepare("SELECT name, output, engine, tier, at FROM history ORDER BY at DESC, id DESC LIMIT 100")
        .map_err(|e| e.to_string())?;
    stmt.query_map([], |row| {
        Ok(HistoryItem {
            name: row.get(0)?,
            output: row.get(1)?,
            engine: row.get(2)?,
            tier: row.get(3)?,
            at: row.get(4)?,
        })
    })
    .map_err(|e| e.to_string())?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn history_add(app: AppHandle, item: HistoryItem) -> Result<(), String> {
    let conn = history_connection(&app)?;
    conn.execute(
        "INSERT INTO history (name, output, engine, tier, at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![item.name, item.output, item.engine, item.tier, item.at],
    )
    .map_err(|e| format!("Gagal menyimpan riwayat: {e}"))?;
    conn.execute("DELETE FROM history WHERE id NOT IN (SELECT id FROM history ORDER BY at DESC, id DESC LIMIT 100)", []).map_err(|e| format!("Gagal membatasi riwayat: {e}"))?;
    Ok(())
}

#[tauri::command]
fn history_clear(app: AppHandle) -> Result<(), String> {
    history_connection(&app)?
        .execute("DELETE FROM history", [])
        .map_err(|e| format!("Gagal menghapus riwayat: {e}"))?;
    Ok(())
}

#[tauri::command]
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

fn run_engine(command: &mut Command, name: &str) -> Result<(), String> {
    hide_console(command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Gagal menjalankan {name}: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Tidak dapat membaca output engine.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Tidak dapat membaca error engine.")?;
    let out_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = std::io::BufReader::new(stdout).read_to_end(&mut bytes);
        bytes
    });
    let err_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = std::io::BufReader::new(stderr).read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        if CANCEL_CONVERSION.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Err("Konversi dibatalkan.".into());
        }
        if started.elapsed() >= Duration::from_secs(180) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Err(format!("{name} melewati batas waktu 180 detik."));
        }
        if let Some(status) = child
            .wait_timeout(Duration::from_millis(250))
            .map_err(|e| format!("Gagal menunggu {name}: {e}"))?
        {
            break status;
        }
    };
    let _ = out_reader.join();
    let stderr = err_reader.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        let text = String::from_utf8_lossy(&stderr);
        Err(format!(
            "{name} gagal: {}",
            if text.trim().is_empty() {
                "proses keluar dengan status gagal"
            } else {
                text.trim()
            }
        ))
    }
}

fn run_engine_for_output(command: &mut Command, name: &str, output: &Path) -> Result<(), String> {
    if let Err(error) = run_engine(command, name) {
        let _ = std::fs::remove_file(output);
        return Err(error);
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
    let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bin/pdfium.dll");
    if bundled.is_file() {
        return Some(bundled);
    }
    app.path()
        .resolve("bin/pdfium.dll", tauri::path::BaseDirectory::Resource)
        .ok()
}

fn inspect_pdf(app: &AppHandle, path: &Path) -> Result<Vec<String>, String> {
    // pdfium-render bindings are process-global. Initialize one shared instance and reuse it.
    let pdfium = PDFIUM_INSTANCE.get_or_init(|| {
        let library =
            pdfium_library_path(app).ok_or_else(|| "PDFium DLL tidak ditemukan.".to_string())?;
        let bindings =
            Pdfium::bind_to_library(library).map_err(|e| format!("Gagal memuat PDFium: {e}"))?;
        Ok(Pdfium::new(bindings))
    });
    let pdfium = pdfium.as_ref().map_err(Clone::clone)?;
    let document = pdfium.load_pdf_from_file(path, None).map_err(|e| {
        format!("PDF tidak dapat dibuka (mungkin rusak atau dilindungi password): {e}")
    })?;
    let page_count = document.pages().len() as usize;
    if page_count == 0 {
        return Err("PDF tidak memiliki halaman.".into());
    }
    let mut pages_with_text = 0usize;
    for page in document.pages().iter() {
        let text = page
            .text()
            .map_err(|e| format!("Gagal membaca teks PDF: {e}"))?
            .all();
        if !text.trim().is_empty() {
            pages_with_text += 1;
        }
    }
    let mut warnings = Vec::new();
    if pages_with_text == 0 {
        warnings.push("PDF tidak memiliki teks yang dapat diekstrak; kemungkinan besar hasil scan. OCR disarankan.".into());
    } else if pages_with_text < page_count {
        warnings.push(format!("PDF campuran: hanya {pages_with_text} dari {page_count} halaman memiliki teks yang dapat diekstrak; halaman lain mungkin scan."));
    }
    Ok(warnings)
}

#[tauri::command]
fn cancel_conversion() {
    CANCEL_CONVERSION.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn preflight_file(app: AppHandle, input: String) -> Result<PreflightReport, String> {
    let path = Path::new(&input);
    let metadata = std::fs::metadata(path).map_err(|e| format!("Tidak dapat membaca file: {e}"))?;
    if !metadata.is_file() {
        return Err("Path bukan file biasa.".into());
    }
    let mut warnings = Vec::new();
    if metadata.len() == 0 {
        return Err("File kosong.".into());
    }
    if metadata.len() > 500 * 1024 * 1024 {
        warnings.push(
            "File lebih besar dari 500 MB; konversi bisa memerlukan waktu dan memori besar.".into(),
        );
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext == "pdf" {
        warnings.extend(inspect_pdf(&app, path)?);
    } else if ["docx", "odt"].contains(&ext.as_str()) {
        warnings.extend(inspect_office_archive(path, &ext)?);
    } else if ext == "rtf" {
        inspect_rtf(path)?;
    }
    Ok(PreflightReport {
        size_bytes: metadata.len(),
        warnings,
    })
}

fn inspect_office_archive(path: &Path, ext: &str) -> Result<Vec<String>, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("Tidak dapat membuka dokumen: {e}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| format!("Dokumen {ext} rusak atau bukan arsip Office valid: {e}"))?;
    if archive.len() > 10_000 {
        return Err(format!(
            "Arsip {ext} memiliki terlalu banyak entri untuk diperiksa dengan aman."
        ));
    }
    let xml_names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            archive
                .by_index(i)
                .ok()
                .map(|entry| entry.name().to_owned())
        })
        .filter(|name| {
            name.ends_with(".xml")
                && (name == "word/document.xml"
                    || name.starts_with("word/")
                    || name == "content.xml"
                    || name == "styles.xml")
        })
        .collect();
    if xml_names.is_empty() {
        return Err(format!("Arsip {ext} tidak memiliki dokumen XML utama."));
    }
    let mut referenced = std::collections::HashSet::new();
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
        let mut xml = String::with_capacity(entry.size() as usize);
        entry
            .take(MAX_XML_BYTES + 1)
            .read_to_string(&mut xml)
            .map_err(|e| format!("XML dokumen tidak valid ({name}): {e}"))?;
        if xml.len() as u64 > MAX_XML_BYTES {
            return Err(format!("XML dokumen melewati batas ukuran aman ({name})."));
        }
        for marker in ["w:rFonts", "typeface="] {
            let mut remaining = xml.as_str();
            while let Some(pos) = remaining.find(marker) {
                remaining = &remaining[pos + marker.len()..];
                if let Some(start) = remaining.find(['\"', '\'']) {
                    let quote = remaining.as_bytes()[start] as char;
                    let value = &remaining[start + 1..];
                    if let Some(end) = value.find(quote) {
                        let font = value[..end].trim();
                        if !font.is_empty() && !font.starts_with('+') {
                            referenced.insert(font.to_lowercase());
                        }
                    }
                }
            }
        }
    }
    let installed = installed_font_names();
    let missing: Vec<_> = referenced
        .into_iter()
        .filter(|font| !installed.contains(font))
        .collect();
    let warnings = if missing.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "Font mungkin tidak terpasang: {}. Nama font diekstrak dari metadata dokumen; hasil dapat berbeda jika font ter-embed atau alias digunakan.",
            missing.join(", ")
        )]
    };
    Ok(warnings)
}

fn installed_font_names() -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(r"C:\Windows\Fonts") {
        for entry in entries.flatten() {
            if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
                names.insert(stem.to_lowercase());
            }
        }
    }
    for name in [
        "arial",
        "calibri",
        "cambria",
        "times new roman",
        "courier new",
        "carlito",
        "caladea",
        "liberation sans",
        "liberation serif",
        "liberation mono",
    ] {
        if names
            .iter()
            .any(|installed| installed == name || installed.starts_with(name))
        {
            names.insert(name.into());
        }
    }
    names
}

fn inspect_rtf(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Tidak dapat membaca RTF: {e}"))?;
    let sample = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    if !sample.trim_start().starts_with("{\\rtf") {
        return Err("File berekstensi RTF tetapi header RTF tidak valid.".into());
    }
    Ok(())
}

#[tauri::command]
fn convert_file(input: String, target: String) -> Result<ConversionReport, String> {
    CANCEL_CONVERSION.store(false, Ordering::SeqCst);
    let started = Instant::now();
    let source = Path::new(&input);
    if !source.is_file() {
        return Err("File sumber tidak ditemukan.".into());
    }
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
        .and_then(|e| e.to_str())
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
    let output_stem = source.file_stem().ok_or("Nama file tidak valid.")?;
    let mut output = source.parent().unwrap_or(Path::new(".")).join(output_stem);
    if target_key == "searchable pdf" {
        output.set_file_name(format!("{}_searchable.pdf", output_stem.to_string_lossy()));
    } else {
        output.set_extension(target_ext);
    }
    if output.exists() {
        return Err(format!(
            "File tujuan sudah ada; tidak ditimpa: {}",
            output.display()
        ));
    }

    if ocr_target {
        let ocr = ocrmypdf_path().ok_or("OCRmyPDF tidak ditemukan.")?;
        let tesseract = tesseract_path().ok_or("Tesseract tidak ditemukan.")?;
        let gs = ghostscript_path().ok_or("Ghostscript tidak ditemukan.")?;
        let tess_dir = tesseract.parent().ok_or("Path Tesseract tidak valid.")?;
        let gs_dir = gs.parent().ok_or("Path Ghostscript tidak valid.")?;
        let mut search_path = vec![tess_dir.to_path_buf(), gs_dir.to_path_buf()];
        if let Some(path) = std::env::var_os("PATH") {
            search_path.extend(std::env::split_paths(&path));
        }
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
            (temp_pdf, Some((temp, output.clone())))
        };
        let mut cmd = Command::new(ocr);
        cmd.env("PATH", joined)
            .args(["--deskew", "--rotate-pages", "-l", "ind+eng"]);
        if target_key == "searchable pdf" {
            cmd.arg("--skip-text");
        } else {
            cmd.arg("--force-ocr");
        }
        if let Some((_, ref sidecar_path)) = sidecar {
            cmd.arg("--sidecar").arg(sidecar_path);
        }
        cmd.arg(source).arg(&ocr_output);
        let expected_output = sidecar
            .as_ref()
            .map(|(_, path)| path.as_path())
            .unwrap_or(&output);
        run_engine_for_output(&mut cmd, "OCRmyPDF / Tesseract", expected_output)?;
        if let Some((temp, sidecar_path)) = sidecar {
            if !sidecar_path.is_file() {
                drop(temp);
                return Err("OCR selesai tanpa menghasilkan file teks sidecar.".into());
            }
            drop(temp);
        }
        if !output.is_file() {
            return Err("OCR selesai tanpa menghasilkan output.".into());
        }
        return Ok(ConversionReport {
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
        });
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
        let mut cmd = Command::new(converter);
        cmd.args(["convert"]).arg(source).arg("-d").arg(&output);
        run_engine_for_output(&mut cmd, "pdf2docx", &output)?;
        ("pdf2docx", "B", Some("PDF direkonstruksi menjadi elemen DOCX yang dapat diedit. Kemiripan bergantung pada struktur PDF; scan, layout kompleks, dan font khusus dapat memerlukan koreksi.".into()))
    } else if target_ext == "md" {
        let anymd = anymd_path().ok_or("AnyMD tidak ditemukan.")?;
        let mut cmd = Command::new(anymd);
        cmd.args(["convert"]).arg(source).arg("-o").arg(&output);
        run_engine_for_output(&mut cmd, "AnyMD", &output)?;
        ("AnyMD", "B", Some("Markdown berfokus pada struktur dan teks; tata letak visual, sebagian tabel, dan elemen non-teks dapat berubah atau tidak ikut.".into()))
    } else if pdf_input && target_ext == "docx" && anymd_path().is_some() && pandoc_path().is_some()
    {
        let anymd = anymd_path().ok_or("AnyMD tidak ditemukan.")?;
        let pandoc = pandoc_path().ok_or("Pandoc tidak ditemukan.")?;
        let temp = tempfile::Builder::new()
            .prefix("documorph-anymd-")
            .tempdir()
            .map_err(|e| format!("Gagal membuat file sementara: {e}"))?;
        let markdown = temp.path().join("extracted.md");
        let mut extract = Command::new(anymd);
        extract
            .args(["convert"])
            .arg(source)
            .arg("-o")
            .arg(&markdown);
        run_engine_for_output(&mut extract, "AnyMD (PDF to Markdown)", &markdown)?;
        let mut build_docx = Command::new(pandoc);
        build_docx.arg(&markdown).arg("-o").arg(&output);
        run_engine_for_output(&mut build_docx, "Pandoc (Markdown to DOCX)", &output)?;
        ("AnyMD + Pandoc", "B", Some("PDF diekstrak ke Markdown lalu direkonstruksi sebagai DOCX. Layout, tabel kompleks, gambar, dan scan mungkin tidak terjaga; OCR AnyMD opsional mungkin diperlukan untuk PDF scan.".into()))
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
        let mut cmd = Command::new(lo);
        cmd.arg(format!("-env:UserInstallation={profile_url}"))
            .args(["--headless", "--convert-to", "pdf", "--outdir"])
            .arg(output.parent().unwrap_or(Path::new(".")))
            .arg(source);
        run_engine_for_output(&mut cmd, "LibreOffice", &output)?;
        (
            "LibreOffice",
            "A",
            Some(
                "Hasil bergantung pada font dan fitur dokumen yang didukung LibreOffice."
                    .to_string(),
            ),
        )
    } else if target_ext == "pdf" && pandoc_input {
        let pandoc = pandoc_path().ok_or("Pandoc tidak ditemukan.")?;
        if typst_path().is_none() {
            return Err("Typst tidak ditemukan; diperlukan untuk membuat PDF dari Pandoc.".into());
        }
        let mut cmd = Command::new(pandoc);
        cmd.arg(source)
            .arg("-o")
            .arg(&output)
            .args(["--pdf-engine=typst"]);
        run_engine_for_output(&mut cmd, "Pandoc + Typst", &output)?;
        (
            "Pandoc + Typst",
            "B",
            Some(
                "Layout dibangun ulang; hasil visual mungkin berbeda dari dokumen Office asal."
                    .to_string(),
            ),
        )
    } else if target_ext != "pdf" && pandoc_input && !pdf_input && pandoc_output {
        let pandoc = pandoc_path().ok_or("Pandoc tidak ditemukan.")?;
        let mut cmd = Command::new(pandoc);
        cmd.arg(source).arg("-o").arg(&output);
        run_engine_for_output(&mut cmd, "Pandoc", &output)?;
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
        let mut cmd = Command::new(lo);
        cmd.arg(format!("-env:UserInstallation={profile_url}"))
            .args(["--headless", "--convert-to", target_ext, "--outdir"])
            .arg(output.parent().unwrap_or(Path::new(".")))
            .arg(source);
        run_engine_for_output(&mut cmd, "LibreOffice (PDF import)", &output)?;
        ("LibreOffice (PDF import)", "C", Some("PDF adalah format visual; teks dan layout direkonstruksi, hasil bisa berubah signifikan. Scan memerlukan OCR.".to_string()))
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
        let mut cmd = Command::new(lo);
        cmd.arg(format!("-env:UserInstallation={profile_url}"))
            .args(["--headless", "--convert-to", target_ext, "--outdir"])
            .arg(output.parent().unwrap_or(Path::new(".")))
            .arg(source);
        run_engine_for_output(&mut cmd, "LibreOffice", &output)?;
        ("LibreOffice", "B", None)
    } else {
        return Err(format!(
            "Konversi dari .{ext} ke PDF belum didukung oleh engine yang terpasang."
        ));
    };
    if !output.is_file() {
        return Err("Engine selesai tanpa menghasilkan file keluaran.".into());
    }
    Ok(ConversionReport {
        output: output.to_string_lossy().into_owned(),
        engine: engine.into(),
        tier: tier.into(),
        duration_ms: started.elapsed().as_millis(),
        warning,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            engine_status,
            history_list,
            open_output,
            reveal_output,
            history_add,
            history_clear,
            preflight_file,
            convert_file,
            cancel_conversion
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
