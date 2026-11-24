//! Cancel per-job: satu konversi dibatalkan tidak boleh membatalkan job lain.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

static FLAGS: Mutex<Option<HashMap<String, Arc<AtomicBool>>>> = Mutex::new(None);

fn flags() -> MutexGuard<'static, Option<HashMap<String, Arc<AtomicBool>>>> {
    let mut guard = FLAGS.lock().unwrap_or_else(|error| error.into_inner());
    if guard.is_none() {
        *guard = Some(HashMap::new());
    }
    guard
}

/// Daftarkan job. Guard menghapus flag otomatis saat job selesai/drop.
pub fn begin(id: &str) -> (Arc<AtomicBool>, JobGuard) {
    let flag = Arc::new(AtomicBool::new(false));
    let mut registry = flags();
    let map = registry.as_mut().expect("registry");
    map.insert(id.to_owned(), Arc::clone(&flag));
    (flag, JobGuard(id.to_owned()))
}

pub struct JobGuard(String);

impl Drop for JobGuard {
    fn drop(&mut self) {
        if let Some(map) = flags().as_mut() {
            map.remove(&self.0);
        }
    }
}

/// `Some(id)` membatalkan satu job; `None` membatalkan semua job aktif.
pub fn cancel(id: Option<&str>) {
    let registry = flags();
    let Some(map) = registry.as_ref() else {
        return;
    };
    match id {
        Some(id) => {
            if let Some(flag) = map.get(id) {
                flag.store(true, Ordering::SeqCst);
            }
        }
        None => {
            for flag in map.values() {
                flag.store(true, Ordering::SeqCst);
            }
        }
    }
}

pub fn cancelled(flag: &AtomicBool) -> bool {
    flag.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_only_targets_the_requested_job() {
        let (first, _first_guard) = begin("first");
        let (second, _second_guard) = begin("second");
        cancel(Some("first"));
        assert!(cancelled(&first));
        assert!(!cancelled(&second));
    }

    #[test]
    fn cancel_without_id_stops_every_job() {
        let (first, _first_guard) = begin("all-first");
        let (second, _second_guard) = begin("all-second");
        cancel(None);
        assert!(cancelled(&first));
        assert!(cancelled(&second));
    }

    #[test]
    fn guard_unregisters_finished_job() {
        let (flag, guard) = begin("finished");
        drop(guard);
        cancel(Some("finished"));
        assert!(
            !cancelled(&flag),
            "job yang sudah selesai tidak boleh ikut dibatalkan"
        );
    }
}
