//! Batas-batas aman untuk inspeksi file. Tanpa ini, file besar atau ZIP
//! buatan bisa menghabiskan memori WebView/proses sebelum konversi dimulai.

use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

/// Mulai peringatkan pengguna di atas 100 MB.
pub const WARN_INPUT_BYTES: u64 = 100 * 1024 * 1024;
/// Tolak input di atas 500 MB (batas lama yang hanya jadi warning).
pub const MAX_INPUT_BYTES: u64 = 500 * 1024 * 1024;
/// Cukup untuk memeriksa header RTF tanpa membaca seluruh file.
pub const RTF_PREFIX_BYTES: usize = 4096;
/// Batasi halaman yang di-inspeksi saat preflight PDF.
pub const MAX_PDF_PAGES: usize = 200;
/// Batasi total karakter teks PDF yang diekstrak saat preflight.
pub const MAX_PDF_TEXT_CHARS: usize = 200_000;
/// Total anggaran XML lintas entri dalam satu arsip Office.
pub const MAX_XML_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Pastikan path adalah file biasa, tidak kosong, dan tidak melewati batas aman.
pub fn check_input_size(path: &Path) -> Result<u64, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("Tidak dapat membaca file: {e}"))?;
    if !metadata.is_file() {
        return Err("Path bukan file biasa.".into());
    }
    if metadata.len() == 0 {
        return Err("File kosong.".into());
    }
    if metadata.len() > MAX_INPUT_BYTES {
        return Err(format!(
            "File melewati batas aman {} MB ({:.1} MB).",
            MAX_INPUT_BYTES / (1024 * 1024),
            metadata.len() as f64 / (1024.0 * 1024.0)
        ));
    }
    Ok(metadata.len())
}

pub fn size_warning(len: u64) -> Option<String> {
    if len > WARN_INPUT_BYTES {
        Some(format!(
            "File lebih besar dari {} MB; konversi bisa memerlukan waktu dan memori besar.",
            WARN_INPUT_BYTES / (1024 * 1024)
        ))
    } else {
        None
    }
}

/// Baca hanya `max` byte pertama dari file.
pub fn read_prefix(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| format!("Tidak dapat membaca file: {e}"))?;
    let mut bytes = Vec::with_capacity(max.min(RTF_PREFIX_BYTES));
    BufReader::new(file)
        .take(max as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Tidak dapat membaca file: {e}"))?;
    Ok(bytes)
}

/// Potong teks pada batas karakter tanpa memecah UTF-8.
/// Mengembalikan `(teks, terpotong)`.
pub fn truncate_chars(text: &str, max: usize) -> (String, bool) {
    let mut out = String::with_capacity(max.min(text.len()));
    for (index, ch) in text.chars().enumerate() {
        if index == max {
            return (out, true);
        }
        out.push(ch);
    }
    (out, false)
}

/// Berapa halaman yang boleh di-inspeksi, dan apakah sisanya dilewati.
pub fn page_scan_limit(page_count: usize) -> (usize, bool) {
    if page_count <= MAX_PDF_PAGES {
        (page_count, false)
    } else {
        (MAX_PDF_PAGES, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let suffix = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "documorph-limits-test-{}-{suffix}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write temp file");
        path
    }

    #[test]
    fn check_input_size_rejects_empty_and_directory() {
        let empty = temp_file("empty.bin", &[]);
        assert!(check_input_size(&empty).is_err());
        assert!(check_input_size(empty.parent().expect("parent")).is_err());
        let _ = std::fs::remove_dir_all(empty.parent().expect("parent"));
    }

    #[test]
    fn check_input_size_returns_size_for_valid_file() {
        let path = temp_file("valid.bin", b"documorph");
        assert_eq!(check_input_size(&path).expect("size"), 9);
        assert_eq!(size_warning(9), None);
        assert!(size_warning(WARN_INPUT_BYTES + 1).is_some());
        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn read_prefix_does_not_read_whole_file() {
        let path = temp_file("long.txt", b"abcdefghijklmnopqrstuvwxyz");
        assert_eq!(read_prefix(&path, 5).expect("prefix"), b"abcde");
        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn truncate_chars_respects_utf8_boundaries() {
        let (kept, cut) = truncate_chars("aé中z", 2);
        assert_eq!(kept, "aé");
        assert!(cut);
        let (kept, cut) = truncate_chars("abc", 10);
        assert_eq!(kept, "abc");
        assert!(!cut);
    }

    #[test]
    fn page_scan_limit_caps_large_documents() {
        assert_eq!(page_scan_limit(10), (10, false));
        assert_eq!(page_scan_limit(MAX_PDF_PAGES + 5), (MAX_PDF_PAGES, true));
    }
}
