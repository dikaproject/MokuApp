//! Where Moku keeps its data: `%APPDATA%\Moku\`. Nothing is ever written next to the exe.
//!
//! Set the environment variable `MOKU_DATA_DIR` to use another folder (tests, portable runs).

use std::path::{Path, PathBuf};

/// `%APPDATA%\Moku` (or `$MOKU_DATA_DIR`). Not created; see [`ensure_data_dir`].
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("MOKU_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Roaming")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("Moku")
}

/// Create the data folder if it does not exist, put the marker file (`DATA_MARKER`) in it once,
/// and return it.
pub fn ensure_data_dir() -> std::io::Result<PathBuf> {
    let d = data_dir();
    ensure_marked_dir(&d)?;
    Ok(d)
}

/// [`ensure_data_dir`] for any folder (the marker is only written when it is missing).
pub fn ensure_marked_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let marker = dir.join(DATA_MARKER);
    if !marker.exists() {
        // Best effort: a folder we cannot write the marker into just stays unmarked.
        let _ = std::fs::write(&marker, MARKER_TEXT);
    }
    Ok(())
}

/// `config.toml` — written by the settings app, watched by the island.
pub fn config_file() -> PathBuf {
    data_dir().join("config.toml")
}

/// Local to-do list (Today panel).
pub fn todo_file() -> PathBuf {
    data_dir().join("todo.json")
}

/// Clipboard history folder (index + image thumbnails).
pub fn clipboard_dir() -> PathBuf {
    data_dir().join("clipboard")
}

/// Shelf folder (index of the files dropped on the island).
pub fn shelf_dir() -> PathBuf {
    data_dir().join("shelf")
}

/// Island log file `logs\moku.log` (rotated at 1 MB to `moku.log.1`). In a subfolder so log
/// writes don't wake the config watcher, which watches the data folder itself.
pub fn log_file() -> PathBuf {
    data_dir().join("logs").join("moku.log")
}

/// Any other per-feature file: `data_dir()/<name>`.
pub fn data_file(name: &str) -> PathBuf {
    data_dir().join(name)
}

// ---- what is Moku's, and what is not -----------------------------------------------------------
//
// `MOKU_DATA_DIR` (or a mistake) can point Moku at a folder that also holds other things, so
// wiping the data (`moku-setup --deletedata`, "Hapus semua data" in the settings) never deletes
// "everything in the folder": it deletes the entries listed here and nothing else. Add a new
// data file of a feature to [`DATA_FILES`] (a test in the island fails when a `data_file("...")`
// name is missing).

/// Written into the data folder when Moku creates it ([`ensure_data_dir`]). The uninstaller
/// refuses to delete a data folder without it.
pub const DATA_MARKER: &str = ".moku-data";
const MARKER_TEXT: &str = "Moku data folder. moku-setup --deletedata and the settings app only remove Moku's own files here, never anything else.\r\n";

/// Files Moku keeps directly in the data folder (a `.tmp` next to one is a half-written save).
pub const DATA_FILES: &[&str] = &[
    "config.toml",
    "todo.json",
    "todo.corrupt.json",
    "water.json",
    "launcher.json",
    "launcher-index.bin",
    "capture.json",
    "capture-thumb.png",
    "update.json",
    "meeting-mute.flag",
    "agent-rules.json",
    "agent-rules.trusted.json",
    "holiday-eve.txt",
    "bmkg-seen.json",
    "ai-memory.bin",
    "ai-memory.lock",
    "phone-debug.txt",
    "lyrics-choices.json",
    DATA_MARKER,
];

/// Subfolders Moku keeps in the data folder (all of what is inside belongs to Moku).
pub const DATA_DIRS: &[&str] = &["logs", "clipboard", "shelf"];

/// Is the entry `name` of the data folder one of Moku's? Files and folders are told apart:
/// a folder called `config.toml` or a file called `logs` is not ours.
pub fn is_moku_entry(name: &str, is_dir: bool) -> bool {
    let n = name.to_lowercase();
    if is_dir {
        return DATA_DIRS.contains(&n.as_str());
    }
    let base = n.strip_suffix(".tmp").unwrap_or(&n);
    DATA_FILES.contains(&base)
}

/// What [`wipe_known_entries`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Wipe {
    /// Moku's files and folders removed.
    pub removed: usize,
    /// Moku's entries that could not be removed (usually a file the island has open).
    pub failed: usize,
    /// Entries that are not Moku's and were left alone (names in `keep` do not count).
    pub foreign: usize,
}

/// Delete Moku's own entries directly inside `dir` ([`is_moku_entry`]), except the names in
/// `keep` (compared without case). Everything else stays; a junction or symlink is removed
/// itself, never followed. A missing folder is fine.
pub fn wipe_known_entries(dir: &Path, keep: &[&str]) -> std::io::Result<Wipe> {
    let mut out = Wipe::default();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if keep.iter().any(|k| k.eq_ignore_ascii_case(&name)) {
            continue;
        }
        let path = entry.path();
        let is_dir = std::fs::symlink_metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
        if !is_moku_entry(&name, is_dir) {
            out.foreign += 1;
            continue;
        }
        match if is_dir { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) } {
            Ok(()) => out.removed += 1,
            Err(_) => out.failed += 1,
        }
    }
    Ok(out)
}

/// Refuse folders that cannot be a Moku data folder: relative paths, drive roots and their
/// children, and any folder that contains (or is) the user profile, `%APPDATA%`, Program Files
/// or the Windows folder.
pub fn check_data_dir_safe(dir: &Path) -> Result<(), String> {
    if !dir.is_absolute() {
        return Err(format!("data folder {} is not an absolute path", dir.display()));
    }
    let depth = dir.components().filter(|c| matches!(c, std::path::Component::Normal(_))).count();
    if depth < 3 {
        return Err(format!("refusing to touch {}: too close to the drive root", dir.display()));
    }
    for var in ["USERPROFILE", "APPDATA", "LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)", "SystemRoot", "windir"] {
        if let Some(p) = std::env::var_os(var) {
            let p = Path::new(&p);
            // `dir` is that folder or one of its ancestors.
            if !p.as_os_str().is_empty() && p.starts_with(dir) {
                return Err(format!("refusing to touch {}: it contains %{var}%", dir.display()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-core-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn known_entries_are_told_from_foreign_ones() {
        for f in ["config.toml", "TODO.JSON", "water.json", "launcher.json", "launcher-index.bin", "launcher-index.bin.tmp", "config.toml.tmp", "update.json.tmp", "meeting-mute.flag", ".moku-data"] {
            assert!(is_moku_entry(f, false), "{f}");
        }
        for d in ["logs", "Clipboard", "shelf"] {
            assert!(is_moku_entry(d, true), "{d}");
        }
        for f in ["notes.txt", "my-photos", "config.toml.bak", "todo.json.old", "logs", "clipboard", "data.json"] {
            assert!(!is_moku_entry(f, false), "{f} is not one of our files");
        }
        // Files and folders are not interchangeable.
        assert!(!is_moku_entry("config.toml", true), "a folder called config.toml");
        assert!(!is_moku_entry("logs", false), "a file called logs");
        assert!(!is_moku_entry("Documents", true) && !is_moku_entry("backup", true));
    }

    #[test]
    fn a_wipe_removes_what_is_ours_and_leaves_the_rest() {
        let d = scratch("wipe");
        for f in ["config.toml", "todo.json", "launcher-index.bin", "update.json.tmp", "notes.txt", "meeting-mute.flag"] {
            std::fs::write(d.join(f), "x").unwrap();
        }
        std::fs::create_dir_all(d.join("clipboard").join("thumbs")).unwrap();
        std::fs::write(d.join("clipboard").join("index.json"), "{}").unwrap();
        std::fs::create_dir_all(d.join("logs")).unwrap();
        std::fs::write(d.join("logs").join("moku.log"), "x").unwrap();
        std::fs::create_dir_all(d.join("holiday-photos")).unwrap();
        std::fs::write(d.join("holiday-photos").join("a.jpg"), "x").unwrap();
        let w = wipe_known_entries(&d, &["config.toml", "meeting-mute.flag"]).unwrap();
        assert_eq!(w, Wipe { removed: 5, failed: 0, foreign: 2 });
        let mut left: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec!["config.toml", "holiday-photos", "meeting-mute.flag", "notes.txt"]);
        assert!(d.join("holiday-photos").join("a.jpg").exists());
        // A missing folder is fine.
        assert_eq!(wipe_known_entries(&d.join("missing"), &[]).unwrap(), Wipe::default());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_data_folder_gets_its_marker_once() {
        let root = scratch("marker");
        let d = root.join("sub").join("data");
        ensure_marked_dir(&d).unwrap();
        assert!(d.join(DATA_MARKER).is_file(), "written together with the folder");
        // An existing marker (or any content in it) is left alone.
        std::fs::write(d.join(DATA_MARKER), "mine").unwrap();
        ensure_marked_dir(&d).unwrap();
        assert_eq!(std::fs::read_to_string(d.join(DATA_MARKER)).unwrap(), "mine");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dangerous_folders_are_refused() {
        assert!(check_data_dir_safe(Path::new(r"C:\")).is_err());
        assert!(check_data_dir_safe(Path::new(r"C:\Users")).is_err());
        assert!(check_data_dir_safe(Path::new("relative\\folder\\deep")).is_err());
        if let Some(home) = std::env::var_os("USERPROFILE") {
            assert!(check_data_dir_safe(Path::new(&home)).is_err());
        }
        if let Some(roaming) = std::env::var_os("APPDATA") {
            assert!(check_data_dir_safe(Path::new(&roaming)).is_err());
            assert!(check_data_dir_safe(&Path::new(&roaming).join("Moku")).is_ok());
        }
    }
}
