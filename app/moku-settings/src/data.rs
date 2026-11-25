//! "Hapus semua data Moku": delete Moku's own data in the data folder (never other files that
//! happen to be there), except `config.toml`.
//!
//! The caller passes `moku_core::paths::data_dir()` (see `app::Env::real`), so `MOKU_DATA_DIR` is
//! respected. [`delete_data_in`] takes the folder as a parameter so tests never touch the real one, and it
//! refuses folders that are obviously not a Moku data folder (a drive root, the user profile,
//! `%APPDATA%` itself ...) in case `MOKU_DATA_DIR` was set to something silly.

use std::io;
use std::path::Path;

/// The settings file, which survives the wipe.
pub const KEEP: &str = "config.toml";
/// Also kept: the island's "we muted the microphone" marker. It is not user data but a repair
/// note; without it a crash would leave the microphone muted for good.
pub const KEEP_MUTE_FLAG: &str = "meeting-mute.flag";

/// What the running island said to "drop your data" (`Request::ClearData`) before the wipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IslandNotice {
    /// The island dropped its in-memory data and finished its writes.
    Cleared,
    /// No island running (or IPC turned off): nothing to tell.
    NotRunning,
    /// It did not confirm (older island, busy …): wipe anyway; it may write some data back.
    Failed(String),
}

/// Tell a running island that its data is about to be deleted, so it does not write its
/// in-memory clipboard / shelf / to-do state back later. `send` is the IPC call (injected for tests).
pub fn notify_island(send: impl FnOnce(&moku_core::ipc::Request) -> Result<moku_core::ipc::Response, moku_core::ipc::IpcError>) -> IslandNotice {
    use moku_core::ipc::{IpcError, Response};
    match send(&moku_core::ipc::Request::ClearData) {
        Ok(Response::Ok) => IslandNotice::Cleared,
        Err(IpcError::NotRunning) => IslandNotice::NotRunning,
        Ok(Response::Error { message }) if message.contains("disabled") => IslandNotice::NotRunning,
        Ok(other) => IslandNotice::Failed(format!("{other:?}")),
        Err(e) => IslandNotice::Failed(e.to_string()),
    }
}

/// What a wipe did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Deleted {
    /// Files and folders removed.
    pub removed: usize,
    /// Entries that could not be removed (usually a file the island has open).
    pub failed: usize,
}

/// Delete Moku's own data directly inside `dir` (`moku_core::paths::is_moku_entry`: the to-do
/// list, water counter, launcher history and cache, update state, the logs / clipboard / shelf
/// folders ...) except `config.toml`, the microphone repair note and the data-folder marker.
/// Anything else in the folder is not Moku's and stays. A missing folder is fine.
pub fn delete_data_in(dir: &Path) -> io::Result<Deleted> {
    check_safe(dir)?;
    let w = moku_core::paths::wipe_known_entries(dir, &[KEEP, KEEP_MUTE_FLAG, moku_core::paths::DATA_MARKER])?;
    Ok(Deleted { removed: w.removed, failed: w.failed })
}

/// Refuse folders that cannot be a Moku data folder (a drive root, the user profile, `%APPDATA%`
/// itself ...).
fn check_safe(dir: &Path) -> io::Result<()> {
    moku_core::paths::check_data_dir_safe(dir).map_err(|m| io::Error::new(io::ErrorKind::InvalidInput, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("moku-settings-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn delete_data_keeps_config_toml() {
        let d = temp_dir("delete");
        std::fs::write(d.join("config.toml"), "[island]\nscale = 1.25\n").unwrap();
        std::fs::write(d.join("todo.json"), "[]").unwrap();
        std::fs::create_dir_all(d.join("clipboard").join("thumbs")).unwrap();
        std::fs::write(d.join("clipboard").join("index.json"), "{}").unwrap();
        std::fs::write(d.join("clipboard").join("thumbs").join("1.png"), [0u8; 8]).unwrap();
        std::fs::create_dir_all(d.join("logs")).unwrap();
        std::fs::write(d.join("logs").join("moku.log"), "x").unwrap();

        let r = delete_data_in(&d).unwrap();
        assert_eq!(r, Deleted { removed: 3, failed: 0 });
        // config.toml is byte-for-byte untouched; nothing else is left.
        assert_eq!(std::fs::read_to_string(d.join("config.toml")).unwrap(), "[island]\nscale = 1.25\n");
        let left: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, vec!["config.toml".to_string()]);

        // Running it again (nothing to delete) and on a missing folder is fine.
        assert_eq!(delete_data_in(&d).unwrap(), Deleted::default());
        assert_eq!(delete_data_in(&d.join("missing")).unwrap(), Deleted::default());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn delete_data_keeps_the_mute_repair_marker() {
        let d = temp_dir("muteflag");
        std::fs::write(d.join("meeting-mute.flag"), "moku-mute v2\n{id}\n").unwrap();
        std::fs::write(d.join("water.json"), "{}").unwrap();
        assert_eq!(delete_data_in(&d).unwrap().removed, 1);
        assert!(d.join("meeting-mute.flag").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The island is told before the wipe; no island is fine; anything else still wipes.
    #[test]
    fn island_is_told_before_the_wipe() {
        use moku_core::ipc::{IpcError, Request, Response};
        let mut seen = None;
        let r = notify_island(|req| {
            seen = Some(req.clone());
            Ok(Response::Ok)
        });
        assert_eq!(r, IslandNotice::Cleared);
        assert_eq!(seen, Some(Request::ClearData));
        assert_eq!(notify_island(|_| Err(IpcError::NotRunning)), IslandNotice::NotRunning);
        assert_eq!(notify_island(|_| Ok(Response::Error { message: "IPC is disabled in settings".into() })), IslandNotice::NotRunning);
        assert!(matches!(notify_island(|_| Err(IpcError::Timeout)), IslandNotice::Failed(_)));
        assert!(matches!(notify_island(|_| Ok(Response::Error { message: "malformed message".into() })), IslandNotice::Failed(_)));
    }

    #[test]
    fn delete_data_without_config_empties_moku_files_but_not_foreign_ones() {
        let d = temp_dir("noconfig");
        std::fs::write(d.join("todo.json"), "[]").unwrap();
        std::fs::write(d.join("a.txt"), "a").unwrap();
        std::fs::create_dir_all(d.join("my-folder")).unwrap();
        std::fs::write(d.join("my-folder").join("b.txt"), "b").unwrap();
        assert_eq!(delete_data_in(&d).unwrap().removed, 1);
        let mut left: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec!["a.txt".to_string(), "my-folder".to_string()], "what is not Moku's stays");
        assert!(d.join("my-folder").join("b.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn delete_data_keeps_the_folder_marker() {
        let d = temp_dir("marker");
        moku_core::paths::ensure_marked_dir(&d).unwrap();
        std::fs::write(d.join("water.json"), "{}").unwrap();
        assert_eq!(delete_data_in(&d).unwrap().removed, 1);
        assert!(d.join(moku_core::paths::DATA_MARKER).exists(), "still a marked Moku folder");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn delete_data_refuses_dangerous_folders() {
        assert!(delete_data_in(Path::new(r"C:\")).is_err());
        assert!(delete_data_in(Path::new(r"C:\Users")).is_err());
        assert!(delete_data_in(Path::new("relative\\folder\\deep")).is_err());
        if let Some(home) = std::env::var_os("USERPROFILE") {
            assert!(delete_data_in(Path::new(&home)).is_err());
        }
        if let Some(roaming) = std::env::var_os("APPDATA") {
            assert!(delete_data_in(Path::new(&roaming)).is_err());
            // The real Moku folder inside it is fine to *accept* (path check only; not run here).
            assert!(check_safe(&Path::new(&roaming).join("Moku")).is_ok());
        }
    }
}
