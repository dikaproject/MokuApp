//! Watches `%APPDATA%\Moku\` and reloads `config.toml` when it changes (core service
//! `"config-watch"`). Event-driven (`FindFirstChangeNotification`), debounced 250 ms; a change
//! is only acted on when the file's size / write time actually changed.
//!
//! If the data folder is deleted (by hand, or a wipe that took the folder), the watch handle
//! dies with it: the folder is created again and the watch re-opened (at most once a second),
//! so later edits from the settings app are still picked up.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification};
use windows::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};
use windows::core::HSTRING;

use crate::services::ServiceCtx;

fn stamp(file: &Path) -> Option<(u64, SystemTime)> {
    let m = std::fs::metadata(file).ok()?;
    Some((m.len(), m.modified().ok()?))
}

pub fn run(ctx: ServiceCtx) {
    watch(&ctx, &moku_core::paths::data_dir(), &moku_core::paths::config_file(), &|| ctx.post(|app| app.reload_config()));
}

/// Close the notification handle on drop.
struct Watch(HANDLE);
impl Drop for Watch {
    fn drop(&mut self) {
        unsafe {
            let _ = FindCloseChangeNotification(self.0);
        }
    }
}

fn open(dir: &Path) -> Option<Watch> {
    if let Err(e) = std::fs::create_dir_all(dir) {
        log_warn!("config-watch: cannot create the data folder: {e}");
        return None;
    }
    let flags = FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_DIR_NAME;
    match unsafe { FindFirstChangeNotificationW(&HSTRING::from(dir.as_os_str()), false, flags) } {
        Ok(h) => Some(Watch(h)),
        Err(e) => {
            log_warn!("config-watch: {e:?}");
            None
        }
    }
}

/// Watch `dir` until `ctx` stops; call `changed` when `file`'s size / write time changes.
fn watch(ctx: &ServiceCtx, dir: &PathBuf, file: &Path, changed: &dyn Fn()) {
    let mut last = stamp(file);
    loop {
        let Some(w) = open(dir) else {
            if !ctx.sleep(Duration::from_secs(1)) {
                return;
            }
            continue;
        };
        loop {
            let r = unsafe { WaitForMultipleObjects(&[w.0, ctx.stop_event()], false, INFINITE) };
            if ctx.should_stop() {
                return;
            }
            if r != WAIT_OBJECT_0 {
                break; // the handle failed: re-open
            }
            if !ctx.sleep(Duration::from_millis(250)) {
                return;
            }
            let now = stamp(file);
            if now != last {
                last = now;
                changed();
            }
            // The folder itself is gone (or the handle broke): re-create it and watch again.
            if !dir.is_dir() || unsafe { FindNextChangeNotification(w.0) }.is_err() {
                log_warn!("config-watch: the data folder went away; watching it again");
                break;
            }
        }
        drop(w);
        if !ctx.sleep(Duration::from_secs(1)) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn wait_for(cond: impl Fn() -> bool) -> bool {
        let t = std::time::Instant::now();
        while t.elapsed() < Duration::from_secs(8) {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// The data folder is deleted while watched: it comes back, and edits are seen again.
    #[test]
    fn config_watch_survives_folder_removal() {
        let dir = std::env::temp_dir().join(format!("moku-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("config.toml");
        let ctx = ServiceCtx::detached("config-watch-test");
        let hits = Arc::new(AtomicUsize::new(0));
        let (c2, d2, f2, h2) = (ctx.clone(), dir.clone(), file.clone(), hits.clone());
        let t = std::thread::spawn(move || watch(&c2, &d2, &f2, &|| {
            h2.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(wait_for(|| dir.is_dir()), "folder created");
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&file, "a = 1\n").unwrap();
        assert!(wait_for(|| hits.load(Ordering::SeqCst) >= 1), "first edit seen");

        std::fs::remove_dir_all(&dir).unwrap();
        assert!(wait_for(|| dir.is_dir()), "folder re-created after removal");
        let before = hits.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(&file, "a = 22\n").unwrap();
        assert!(wait_for(|| hits.load(Ordering::SeqCst) > before), "edit after re-creation seen");

        ctx.request_stop();
        t.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
