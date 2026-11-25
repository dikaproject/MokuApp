//! Small data files (clipboard index, shelf list, to-dos …): atomic writes on a background
//! thread, and debounced saves from the UI thread (`App::save_later`).
//!
//! * [`write_atomic`] — temp file + rename in the same folder, so a crash never leaves a
//!   half-written file (any thread).
//! * [`write_async`] — queue a write on the one writer thread. Writes run in order, so an older
//!   snapshot never lands after a newer one of the same file.
//! * [`drain`] — wait (bounded) until the queue is empty: the exit path after the last saves,
//!   and "Hapus semua data" (the island answers `ClearData` only after its queue is empty).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Debounce for `App::save_later`.
pub const SAVE_DELAY: Duration = Duration::from_millis(500);

/// Write `bytes` to `path` atomically (creates the folder).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

struct Writer {
    tx: Mutex<Sender<(PathBuf, Vec<u8>)>>,
    /// Queued + in-flight writes, and a condvar to wait for 0.
    pending: Arc<(Mutex<usize>, Condvar)>,
}

fn writer() -> Option<&'static Writer> {
    static W: OnceLock<Option<Writer>> = OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = channel::<(PathBuf, Vec<u8>)>();
        let pending = Arc::new((Mutex::new(0usize), Condvar::new()));
        let p2 = pending.clone();
        let spawned = std::thread::Builder::new().name("persist".into()).spawn(move || {
            while let Ok((path, bytes)) = rx.recv() {
                if let Err(e) = write_atomic(&path, &bytes) {
                    log_warn!("cannot save {}: {e}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                }
                let (m, cv) = &*p2;
                if let Ok(mut n) = m.lock() {
                    *n = n.saturating_sub(1);
                    cv.notify_all();
                }
            }
        });
        spawned.ok().map(|_| Writer { tx: Mutex::new(tx), pending })
    })
    .as_ref()
}

/// Queue an atomic write on the writer thread (falls back to writing here if the thread could
/// not be started).
pub fn write_async(path: PathBuf, bytes: Vec<u8>) {
    let Some(w) = writer() else {
        let _ = write_atomic(&path, &bytes);
        return;
    };
    if let Ok(mut n) = w.pending.0.lock() {
        *n += 1;
    }
    let sent = w.tx.lock().map(|tx| tx.send((path, bytes)).is_ok()).unwrap_or(false);
    if !sent && let Ok(mut n) = w.pending.0.lock() {
        *n = n.saturating_sub(1);
    }
}

/// Wait until every queued write is done, at most `timeout`. True if the queue is empty.
pub fn drain(timeout: Duration) -> bool {
    let Some(w) = writer() else { return true };
    let deadline = Instant::now() + timeout;
    let (m, cv) = &*w.pending;
    let Ok(mut n) = m.lock() else { return false };
    while *n > 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        match cv.wait_timeout(n, left) {
            Ok((g, _)) => n = g,
            Err(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-persist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn writes_are_atomic_ordered_and_drained() {
        let d = temp("order");
        let f = d.join("sub").join("index.json");
        for i in 0..50 {
            write_async(f.clone(), format!("{{\"n\":{i}}}").into_bytes());
        }
        assert!(drain(Duration::from_secs(5)));
        // The last snapshot wins; no temp file is left behind.
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{\"n\":49}");
        assert!(!d.join("sub").join("index.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
