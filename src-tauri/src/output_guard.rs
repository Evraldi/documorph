//! Validasi path keluaran: hanya file yang benar-benar dihasilkan aplikasi
//! dan masih tercatat di riwayat yang boleh dibuka/ditampilkan.

use std::path::Path;

/// Hanya format keluaran yang memang diproduksi engine kita.
pub const OUTPUT_EXTENSIONS: &[&str] = &[
    "pdf", "docx", "md", "markdown", "html", "htm", "txt", "tex", "odt", "rtf",
];

/// Kunci kanonik yang stabil untuk perbandingan (tanpa prefiks `\\?\` Windows).
pub fn canonical_key(path: &Path) -> Result<String, String> {
    let canonical =
        std::fs::canonicalize(path).map_err(|e| format!("Path tidak dapat diverifikasi: {e}"))?;
    let text = canonical.to_string_lossy();
    let stripped = text.strip_prefix(r"\\?\").unwrap_or(&text);
    Ok(stripped.replace('\\', "/").to_ascii_lowercase())
}

pub fn extension_key(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Pastikan path ada, berekstensi dikenal, dan kuncinya ada di daftar output
/// yang dibuat aplikasi ini.
pub fn validate(path: &Path, allowed_keys: &[String]) -> Result<(), String> {
    let key = canonical_key(path)?;
    let extension = extension_key(path);
    if !OUTPUT_EXTENSIONS.contains(&extension.as_str()) {
        return Err(format!(
            "Jenis keluaran .{extension} tidak diizinkan untuk dibuka."
        ));
    }
    if !allowed_keys.iter().any(|allowed| allowed == &key) {
        return Err("Path ini bukan hasil konversi yang tercatat di riwayat.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    fn temp_file(name: &str) -> std::path::PathBuf {
        let suffix = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "documorph-output-test-{}-{suffix}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        std::fs::write(&path, b"output").expect("write temp output");
        path
    }

    #[test]
    fn accepts_recorded_output() {
        let path = temp_file("result.pdf");
        let key = canonical_key(&path).expect("key");
        assert!(validate(&path, &[key]).is_ok());
        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn rejects_unrecorded_and_disallowed_paths() {
        let path = temp_file("cmd.exe");
        let key = canonical_key(&path).expect("key");
        assert!(validate(&path, &[key]).is_err());
        assert!(validate(&path, &[]).is_err());
        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn canonical_key_is_case_insensitive_on_windows_paths() {
        let path = temp_file("UPPER.TXT");
        let key = canonical_key(&path).expect("key");
        assert!(key.ends_with("/upper.txt"));
        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }
}
