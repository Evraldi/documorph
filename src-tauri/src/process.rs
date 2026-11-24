//! Eksekusi engine eksternal tanpa shell, dengan reader terikat dan
//! pembunuhan pohon proses supaya cancel/timeout tidak menggantung.

use std::{
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};
use wait_timeout::ChildExt;

use crate::limits::truncate_chars;

const ENGINE_TIMEOUT_SECS: u64 = 300;
const MAX_CAPTURE_BYTES: usize = 64 * 1024;
const ERROR_CHARS: usize = 500;

#[cfg(windows)]
pub(crate) fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
}

#[cfg(not(windows))]
pub(crate) fn hide_console(_command: &mut Command) {}

/// Baca maksimal `limit` byte dari reader.
pub fn read_bounded<R: Read>(reader: R, limit: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = reader.take(limit as u64).read_to_end(&mut bytes);
    bytes
}

/// Argumen `taskkill` untuk membunuh proses beserta anak-anaknya.
pub fn taskkill_args(pid: u32) -> Vec<String> {
    vec!["/PID".into(), pid.to_string(), "/T".into(), "/F".into()]
}

#[cfg(windows)]
fn system32_dir() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
}

/// Matiin parent + anak (soffice.bin, Tesseract, Ghostscript, dll).
pub fn kill_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let mut killer = Command::new(system32_dir().join("taskkill.exe"));
        killer.args(taskkill_args(child.id()));
        hide_console(&mut killer);
        if let Ok(mut process) = killer.spawn() {
            let _ = process.wait_timeout(Duration::from_secs(5));
            return;
        }
    }
    let _ = child.kill();
}

fn drain(receiver: &Receiver<Vec<u8>>) {
    // Jangan pernah join() reader: pipe bisa tetap terbuka di proses anak.
    let _ = receiver.recv_timeout(Duration::from_millis(500));
}

fn spawn_reader<R: Read + Send + 'static>(reader: R) -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(read_bounded(reader, MAX_CAPTURE_BYTES));
    });
    receiver
}

/// Jalankan engine, hentikan pohon proses saat cancel/timeout, dan kembalikan
/// error yang sudah disanitasi.
pub fn run_engine(command: &mut Command, name: &str, cancel: &AtomicBool) -> Result<(), String> {
    hide_console(command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    let stdout_rx = spawn_reader(stdout);
    let stderr_rx = spawn_reader(stderr);

    let started = Instant::now();
    let status = loop {
        if cancel.load(Ordering::SeqCst) {
            kill_process_tree(&mut child);
            let _ = child.wait();
            drain(&stdout_rx);
            drain(&stderr_rx);
            return Err("Konversi dibatalkan.".into());
        }
        if started.elapsed() >= Duration::from_secs(ENGINE_TIMEOUT_SECS) {
            kill_process_tree(&mut child);
            let _ = child.wait();
            drain(&stdout_rx);
            drain(&stderr_rx);
            return Err(format!(
                "{name} melewati batas waktu {ENGINE_TIMEOUT_SECS} detik."
            ));
        }
        if let Some(status) = child
            .wait_timeout(Duration::from_millis(200))
            .map_err(|e| format!("Gagal menunggu {name}: {e}"))?
        {
            break status;
        }
    };

    // Process sudah mati; pipe harusnya segera tutup. Tetap pakai timeout agar
    // tidak pernah mem-blok antrean kalau ada handle yang tertinggal.
    let stderr = stderr_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        Err(sanitize_error(name, &stderr))
    }
}

/// Ringkas stderr mentah: satu baris, terpotong, tanpa path yang berlebihan.
pub fn sanitize_error(name: &str, stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let (trimmed, truncated) = truncate_chars(text.trim(), ERROR_CHARS);
    let flat = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return format!("{name} gagal: proses keluar dengan status gagal.");
    }
    format!("{name} gagal: {flat}{}", if truncated { " …" } else { "" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn taskkill_kills_the_whole_tree() {
        assert_eq!(taskkill_args(4242), vec!["/PID", "4242", "/T", "/F"]);
    }

    #[test]
    fn read_bounded_stops_at_limit() {
        let data = vec![0u8, 1, 2, 3, 4, 5];
        assert_eq!(read_bounded(Cursor::new(&data), 3), vec![0, 1, 2]);
    }

    #[test]
    fn sanitize_error_flatten_and_truncates() {
        let message = sanitize_error("Pandoc", b"baris satu\nbaris dua\nb     ");
        assert_eq!(message, "Pandoc gagal: baris satu baris dua b");
        let long = "x".repeat(ERROR_CHARS + 20);
        let message = sanitize_error("Pandoc", long.as_bytes());
        assert!(message.ends_with(" …"));
        assert!(message.len() < ERROR_CHARS + 100);
    }
}
