//! Pin integritas DLL PDFium yang di-bundle.

use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

/// SHA-256 dari `src-tauri/bin/pdfium.dll` (PDFium 159.0.8138.0).
pub const PDFIUM_SHA256: &str = "71A3E63E6AB8F5746A0FC9DE9BC3D0C0DC07EB60990DFD37D9DB7FC891821943";

pub fn sha256_hex(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("Tidak dapat membuka PDFium: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("Tidak dapat membaca PDFium: {e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Tolak DLL yang berubah atau disuntik, termasuk dari path build-machine.
pub fn verify(path: &Path) -> Result<(), String> {
    let digest = sha256_hex(path)?;
    if digest.eq_ignore_ascii_case(PDFIUM_SHA256) {
        Ok(())
    } else {
        Err(format!(
            "SHA-256 PDFium tidak cocok: {digest} (diharapkan {PDFIUM_SHA256})."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_pdfium_matches_pinned_hash() {
        let dll = Path::new(env!("CARGO_MANIFEST_DIR")).join("bin/pdfium.dll");
        assert!(dll.is_file(), "bundled pdfium.dll harus ada");
        verify(&dll).expect("hash PDFium harus cocok");
    }

    #[test]
    fn modified_file_is_rejected() {
        let dir = std::env::temp_dir().join(format!("documorph-pdfium-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("fake.dll");
        std::fs::write(&file, b"not pdfium").expect("write fake dll");
        assert!(verify(&file).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
