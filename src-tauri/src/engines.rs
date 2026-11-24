//! Penemuan engine eksternal yang eksplisit dan terverifikasi.
//! Tidak ada pencarian di PATH: siapa pun yang bisa menulis ke salah satu
//! direktori PATH tidak boleh menyuntik binary ke aplikasi ini.

#[cfg(test)]
use std::ffi::OsStr;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{OnceLock, mpsc},
    thread,
    time::Duration,
};
use wait_timeout::ChildExt;

use crate::process::{hide_console, kill_process_tree, read_bounded};

const PROBE_TIMEOUT_SECS: u64 = 3;
const MAX_PROBE_BYTES: usize = 64 * 1024;
/// Batasi traversal direktori dikenal supaya startup tidak melambat.
const MAX_SCAN_DIRS: usize = 512;

#[derive(Default, Clone)]
pub struct EnginePaths {
    pub anymd: Option<PathBuf>,
    pub pdf2docx: Option<PathBuf>,
    pub pandoc: Option<PathBuf>,
    pub libreoffice: Option<PathBuf>,
    pub typst: Option<PathBuf>,
    pub ocrmypdf: Option<PathBuf>,
    pub tesseract: Option<PathBuf>,
    pub ghostscript: Option<PathBuf>,
}

static ENGINES: OnceLock<EnginePaths> = OnceLock::new();

pub fn engine_paths() -> &'static EnginePaths {
    ENGINES.get_or_init(discover_all)
}

fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

fn program_files() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for key in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = std::env::var_os(key) {
            paths.push(PathBuf::from(root));
        }
    }
    if paths.is_empty() {
        paths.push(PathBuf::from(r"C:\Program Files"));
        paths.push(PathBuf::from(r"C:\Program Files (x86)"));
    }
    paths
}

/// Path eksplisit dari env var. Path relatif ditolak.
fn env_candidate(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).and_then(|value| usable_path(Path::new(&value)))
}

fn usable_path(path: &Path) -> Option<PathBuf> {
    if path.is_relative() || !path.is_file() {
        return None;
    }
    std::fs::canonicalize(path).ok()
}

fn env_dir_candidate(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).and_then(|value| {
        let path = PathBuf::from(value);
        if path.is_relative() || !path.is_dir() {
            return None;
        }
        std::fs::canonicalize(path).ok()
    })
}

/// Cari nama file tertentu di bawah root yang dikenal, dengan batas kedalaman.
/// Symlink dilewati supaya tidak ada loop atau escape keluar dari root.
fn named_files_under(root: &Path, names: &[&str], max_depth: usize) -> Vec<PathBuf> {
    use std::collections::VecDeque;

    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = 0usize;
    let mut found = Vec::new();
    while let Some((dir, depth)) = queue.pop_front() {
        if visited >= MAX_SCAN_DIRS {
            break;
        }
        visited += 1;
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_file() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if names
                    .iter()
                    .any(|wanted| name == wanted.to_ascii_lowercase())
                    && let Some(canonical) = usable_path(&path)
                {
                    found.push(canonical);
                }
            } else if file_type.is_dir() && depth < max_depth {
                queue.push_back((path, depth + 1));
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// WinGet menyimpan binary di `Packages/<package>/<versi>/<exe>`.
fn winget_candidates(names: &[&str]) -> Vec<PathBuf> {
    local_app_data()
        .map(|root| named_files_under(&root.join(r"Microsoft\WinGet\Packages"), names, 3))
        .unwrap_or_default()
}

/// npm global menyimpan shim `.cmd` di `%APPDATA%\npm` dan `node_modules`.
fn npm_global_candidates(names: &[&str]) -> Vec<PathBuf> {
    std::env::var_os("APPDATA")
        .map(|root| named_files_under(&PathBuf::from(root).join("npm"), names, 3))
        .unwrap_or_default()
}

/// Python Scripts standar, plus direktori eksplisit untuk install portable.
fn python_scripts_candidates(names: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some(ref local) = local_app_data() {
        found.extend(named_files_under(&local.join(r"Programs\Python"), names, 3));
    }
    if let Some(dir) = env_dir_candidate("PYTHON_SCRIPTS_PATH") {
        found.extend(names.iter().filter_map(|name| usable_path(&dir.join(name))));
    }
    found.sort();
    found.dedup();
    found
}

fn ghostscript_candidates() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in program_files() {
        let gs_root = root.join("gs");
        let Ok(entries) = std::fs::read_dir(gs_root) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join("bin").join("gswin64c.exe");
            if let Some(path) = usable_path(&candidate) {
                found.push(path);
            }
        }
    }
    found.sort();
    found.reverse();
    found
}

/// Cocokkan marker versi dengan case-insensitive.
pub fn contains_marker(output: &str, markers: &[&str]) -> bool {
    let lower = output.to_ascii_lowercase();
    markers
        .iter()
        .any(|marker| lower.contains(&marker.to_ascii_lowercase()))
}

fn probe(path: &Path, argument: &str) -> Option<String> {
    let mut command = Command::new(path);
    command
        .arg(argument)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(read_bounded(stdout, MAX_PROBE_BYTES));
    });

    let status = match child.wait_timeout(Duration::from_secs(PROBE_TIMEOUT_SECS)) {
        Ok(Some(status)) => status,
        _ => {
            kill_process_tree(&mut child);
            let _ = child.wait();
            let _ = receiver.recv_timeout(Duration::from_millis(500));
            return None;
        }
    };
    let bytes = receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_default();
    if !status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Verifikasi identitas engine sebelum dipakai. `--version` diutamakan;
/// `--help` jadi fallback untuk CLI seperti pdf2docx.
fn verify_engine(path: &Path, markers: &[&str]) -> bool {
    ["--version", "--help"].iter().any(|argument| {
        probe(path, argument)
            .map(|output| contains_marker(&output, markers))
            .unwrap_or(false)
    })
}

fn locate(candidates: Vec<PathBuf>, markers: &[&str]) -> Option<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for path in candidates {
        if seen.contains(&path) {
            continue;
        }
        seen.push(path.clone());
        if verify_engine(&path, markers) {
            return Some(path);
        }
    }
    None
}

fn discover_all() -> EnginePaths {
    let local = local_app_data();
    let anymd_candidates = {
        let mut candidates = Vec::new();
        if let Some(path) = env_candidate("ANYMD_PATH") {
            candidates.push(path);
        }
        if let Some(ref local) = local {
            candidates.push(local.join(r"Programs\anymd-venv\Scripts\anymd.exe"));
        }
        candidates.extend(winget_candidates(&["anymd.exe"]));
        candidates.extend(npm_global_candidates(&["anymd.cmd", "anymd.exe"]));
        candidates
    };
    let anymd = locate(anymd_candidates, &["anymd "]);

    let pdf2docx_candidates = {
        let mut candidates = Vec::new();
        if let Some(path) = env_candidate("PDF2DOCX_PATH") {
            candidates.push(path);
        }
        if let Some(scripts) = anymd.as_ref().and_then(|path| path.parent()) {
            candidates.push(scripts.join("pdf2docx.exe"));
        }
        if let Some(ref local) = local {
            candidates.push(local.join(r"Programs\anymd-venv\Scripts\pdf2docx.exe"));
        }
        candidates.extend(npm_global_candidates(&["pdf2docx.cmd", "pdf2docx.exe"]));
        candidates.extend(python_scripts_candidates(&["pdf2docx.exe"]));
        candidates
    };
    let pdf2docx = locate(pdf2docx_candidates, &["pdf2docx"]);

    let pandoc_candidates = {
        let mut candidates = Vec::new();
        if let Some(path) = env_candidate("PANDOC_PATH") {
            candidates.push(path);
        }
        for root in program_files() {
            candidates.push(root.join(r"Pandoc\pandoc.exe"));
        }
        if let Some(ref local) = local {
            candidates.push(local.join(r"Pandoc\pandoc.exe"));
        }
        candidates.extend(winget_candidates(&["pandoc.exe"]));
        candidates
    };

    let libreoffice_candidates = {
        let mut candidates = Vec::new();
        if let Some(path) = env_candidate("LIBREOFFICE_PATH") {
            candidates.push(path);
        }
        for root in program_files() {
            candidates.push(root.join(r"LibreOffice\program\soffice.exe"));
        }
        candidates.extend(winget_candidates(&["soffice.exe"]));
        candidates
    };

    let typst_candidates = {
        let mut candidates = Vec::new();
        if let Some(path) = env_candidate("TYPST_PATH") {
            candidates.push(path);
        }
        if let Some(ref local) = local {
            candidates.push(local.join(r"Programs\Typst\typst.exe"));
            candidates.push(local.join(r"typst\bin\typst.exe"));
        }
        candidates.extend(winget_candidates(&["typst.exe"]));
        candidates
    };

    let ocrmypdf_candidates = {
        let mut candidates = Vec::new();
        if let Some(path) = env_candidate("OCRMYPDF_PATH") {
            candidates.push(path);
        }
        if let Some(ref local) = local {
            candidates.push(local.join(r"Programs\ocrmypdf-venv\Scripts\ocrmypdf.exe"));
        }
        candidates.extend(python_scripts_candidates(&["ocrmypdf.exe"]));
        candidates.extend(winget_candidates(&["ocrmypdf.exe"]));
        candidates
    };

    let tesseract_candidates = {
        let mut candidates = Vec::new();
        if let Some(path) = env_candidate("TESSERACT_PATH") {
            candidates.push(path);
        }
        for root in program_files() {
            candidates.push(root.join(r"Tesseract-OCR\tesseract.exe"));
        }
        candidates.extend(winget_candidates(&["tesseract.exe"]));
        candidates
    };

    let ghostscript_candidates = {
        let mut candidates = Vec::new();
        if let Some(path) = env_candidate("GHOSTSCRIPT_PATH") {
            candidates.push(path);
        }
        candidates.extend(ghostscript_candidates());
        candidates.extend(winget_candidates(&["gswin64c.exe"]));
        candidates
    };

    EnginePaths {
        anymd,
        pdf2docx,
        pandoc: locate(pandoc_candidates, &["pandoc"]),
        libreoffice: locate(libreoffice_candidates, &["libreoffice", "soffice"]),
        typst: locate(typst_candidates, &["typst"]),
        ocrmypdf: locate(ocrmypdf_candidates, &["ocrmypdf"]),
        tesseract: locate(tesseract_candidates, &["tesseract"]),
        ghostscript: locate(ghostscript_candidates, &["ghostscript"]),
    }
}

/// Direktori System32 dipakai sebagai PATH minimal untuk OCRmyPDF.
pub fn system32_dir() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
}

#[cfg(test)]
fn env_candidate_from(value: &OsStr) -> Option<PathBuf> {
    usable_path(Path::new(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn marker_matching_is_case_insensitive() {
        assert!(contains_marker("LibreOffice 7.6.7.2", &["libreoffice"]));
        assert!(contains_marker("usage: pdf2docx convert", &["PDF2DOCX"]));
        assert!(!contains_marker("something else", &["pandoc"]));
    }

    #[test]
    fn npm_style_cmd_shims_are_discovered() {
        let root = std::env::temp_dir().join(format!("documorph-npm-{}", std::process::id()));
        let _ = std::fs::create_dir_all(root.join("node_modules"));
        let shim = root.join("anymd.cmd");
        std::fs::write(&shim, b"@echo off").expect("write shim");
        let found = named_files_under(&root, &["anymd.cmd"], 2);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("anymd.cmd"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn relative_candidates_are_rejected() {
        assert!(usable_path(Path::new(r"anymd.exe")).is_none());
    }

    #[test]
    fn absolute_file_candidate_is_canonicalized() {
        let dir = std::env::temp_dir().join(format!("documorph-engine-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("fake.exe");
        std::fs::write(&path, b"fake").expect("write fake engine");
        let found = usable_path(&path).expect("absolute path accepted");
        assert!(found.is_absolute());
        assert!(found.ends_with("fake.exe"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn env_value_helper_accepts_os_str() {
        let value: &OsStr = OsStr::new("");
        assert_eq!(env_candidate_from(value), None);
    }

    #[test]
    fn named_files_under_respects_depth_and_names() {
        let root = std::env::temp_dir().join(format!("documorph-scan-{}", std::process::id()));
        let nested = root.join("pkg").join("v1");
        let deeper = nested.join("extra");
        std::fs::create_dir_all(&deeper).expect("create scan tree");
        std::fs::write(nested.join("tool.exe"), b"x").expect("write tool");
        std::fs::write(nested.join("other.exe"), b"x").expect("write other");
        std::fs::write(deeper.join("tool.exe"), b"x").expect("write deep tool");
        let found = named_files_under(&root, &["tool.exe"], 2);
        assert_eq!(found.len(), 1, "hanya satu tool.exe dalam batas kedalaman");
        assert!(found[0].ends_with("tool.exe"));
        let _ = std::fs::remove_dir_all(root);
    }
}
