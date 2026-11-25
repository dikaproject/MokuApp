//! Tiny file logger: `%APPDATA%\Moku\logs\moku.log`, rotated to `moku.log.1` whenever it
//! passes 1 MB (on start and while running), so the logs never take more than ~2 MB.
//! Use the macros `log_info!`, `log_warn!`, `log_error!` from any thread. Never log personal
//! content (clipboard text, notification bodies, keystrokes).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Size at which the log is rotated.
pub const MAX_LOG: u64 = 1_000_000;

struct Log {
    path: PathBuf,
    file: std::fs::File,
    size: u64,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

fn open(path: &Path) -> Option<Log> {
    if std::fs::metadata(path).map(|m| m.len() > MAX_LOG).unwrap_or(false) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some(Log { path: path.to_path_buf(), file, size })
}

pub fn init() {
    let path = moku_core::paths::log_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Some(l) = open(&path)
        && let Ok(mut g) = LOG.lock()
    {
        *g = Some(l);
    }
}

fn append(slot: &mut Option<Log>, line: &str) {
    let Some(l) = slot.as_mut() else { return };
    if l.size + line.len() as u64 > MAX_LOG {
        // Close before renaming (Windows), then start a fresh file.
        let path = l.path.clone();
        *slot = None;
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
        *slot = open(&path);
    }
    if let Some(l) = slot.as_mut()
        && l.file.write_all(line.as_bytes()).is_ok()
    {
        l.size += line.len() as u64;
    }
}

pub fn write(level: &str, msg: std::fmt::Arguments) {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let line = format!("{now} {level} {msg}\n");
    if cfg!(debug_assertions) {
        eprint!("{line}");
    }
    if let Ok(mut g) = LOG.lock() {
        append(&mut g, &line);
    }
}

/// The panic hook's line: like write, but never waits for the log lock (the panic may have
/// happened while this thread held it — panic = "abort" follows right after).
pub fn write_panic(msg: std::fmt::Arguments) {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let line = format!("{now} ERROR {msg}\n");
    eprint!("{line}");
    if let Ok(mut g) = LOG.try_lock() {
        append(&mut g, &line);
    }
}

/// "file:line:col: message" for a panic (the message is a &str or String payload).
pub fn panic_text(info: &std::panic::PanicHookInfo) -> String {
    let at = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_else(|| "?".into());
    let p = info.payload();
    let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "(no message)".into());
    format!("{at}: {msg}")
}

/// Log every panic (location + message) before the process aborts (release: panic = "abort").
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("?").to_string();
        write_panic(format_args!("panic in thread '{thread}' at {}", panic_text(info)));
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_text_has_location_and_message() {
        let seen = std::sync::Arc::new(Mutex::new(String::new()));
        let s2 = seen.clone();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| *s2.lock().unwrap() = panic_text(info)));
        let _ = std::panic::catch_unwind(|| panic!("boom {}", 7));
        std::panic::set_hook(prev);
        let t = seen.lock().unwrap().clone();
        assert!(t.contains("log.rs:") && t.ends_with(": boom 7"), "{t}");
    }

    #[test]
    fn log_rotates_while_running() {
        let d = std::env::temp_dir().join(format!("moku-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("moku.log");
        let mut slot = open(&p);
        let line = format!("{}\n", "x".repeat(999));
        for _ in 0..1500 {
            append(&mut slot, &line);
        }
        let cur = std::fs::metadata(&p).unwrap().len();
        let old = std::fs::metadata(p.with_extension("log.1")).unwrap().len();
        assert!(cur <= MAX_LOG && old <= MAX_LOG, "{cur} {old}");
        assert!(cur > 0 && old > MAX_LOG - 2000, "{cur} {old}");
        drop(slot);
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[macro_export]
macro_rules! log_info {
    ($($t:tt)*) => { $crate::log::write("INFO ", format_args!($($t)*)) };
}
#[macro_export]
macro_rules! log_warn {
    ($($t:tt)*) => { $crate::log::write("WARN ", format_args!($($t)*)) };
}
#[macro_export]
macro_rules! log_error {
    ($($t:tt)*) => { $crate::log::write("ERROR", format_args!($($t)*)) };
}
