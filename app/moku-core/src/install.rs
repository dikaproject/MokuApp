//! Where `moku-setup` installs Moku (per user, no admin rights), and the overrides tests use.
//!
//! | What | Real location | With overrides |
//! |---|---|---|
//! | Program folder | `%LOCALAPPDATA%\Programs\Moku` | `$MOKU_INSTALL_ROOT\Programs\Moku` |
//! | Start Menu shortcut | `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Moku.lnk` | `$MOKU_INSTALL_ROOT\Start Menu\Programs\Moku.lnk` |
//! | Uninstall entry | `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Moku` | `HKCU\$MOKU_REG_ROOT\Uninstall\Moku` |
//! | Autostart value `Moku` | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` | `HKCU\$MOKU_AUTOSTART_KEY`, else `HKCU\$MOKU_REG_ROOT\Run` |
//!
//! Pure path logic; the registry and file work is in `moku-setup`.

use std::path::{Path, PathBuf};

/// Overrides the base folder of the program folder and the shortcut (tests).
pub const INSTALL_ROOT_ENV: &str = "MOKU_INSTALL_ROOT";
/// Overrides `Software\Microsoft\Windows\CurrentVersion` under HKCU (tests).
pub const REG_ROOT_ENV: &str = "MOKU_REG_ROOT";
/// Overrides the Run key alone (shared with the settings app's autostart switch).
pub const AUTOSTART_KEY_ENV: &str = "MOKU_AUTOSTART_KEY";

/// The real registry root under HKCU.
pub const REAL_REG_ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion";
/// Name of the uninstall subkey and of the Run value.
pub const APP_KEY: &str = "Moku";
/// Shortcut file name in the Start Menu's Programs folder.
pub const SHORTCUT_NAME: &str = "Moku.lnk";
/// The installer's copy of itself inside the program folder (the uninstaller).
pub const SETUP_EXE: &str = "moku-setup.exe";
/// The executables the installer ships, in start order.
pub const ISLAND_EXE: &str = "moku.exe";
pub const SETTINGS_EXE: &str = "moku-settings.exe";
pub const CTL_EXE: &str = "mokuctl.exe";
/// Shown in Apps & Features.
pub const PUBLISHER: &str = "Moku contributors";

/// Every location the installer writes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub install_dir: PathBuf,
    /// Full path of `Moku.lnk`.
    pub shortcut: PathBuf,
    /// HKCU subkey of the uninstall entry.
    pub uninstall_key: String,
    /// HKCU subkey holding the `Moku` autostart value.
    pub run_key: String,
    /// The user data folder (`%APPDATA%\Moku`, or `$MOKU_DATA_DIR`), removed on request only.
    pub data_dir: PathBuf,
    /// True when any override is set (the installer then says so, and the tests require it).
    pub overridden: bool,
}

impl Layout {
    /// The layout for this process's environment.
    pub fn from_env() -> Layout {
        Layout::resolve(|k| std::env::var(k).ok(), crate::paths::data_dir())
    }

    /// The layout given an environment lookup (unit-testable).
    pub fn resolve(env: impl Fn(&str) -> Option<String>, data_dir: PathBuf) -> Layout {
        let var = |k: &str| env(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let root = var(INSTALL_ROOT_ENV).map(PathBuf::from);
        let reg_root = var(REG_ROOT_ENV).map(|r| r.trim_matches('\\').to_string());
        let run_override = var(AUTOSTART_KEY_ENV);
        let overridden = root.is_some() || reg_root.is_some() || run_override.is_some();

        let (install_dir, shortcut) = match &root {
            Some(r) => (r.join("Programs").join("Moku"), r.join("Start Menu").join("Programs").join(SHORTCUT_NAME)),
            None => {
                let local = var("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .or_else(|| var("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Local")))
                    .unwrap_or_else(std::env::temp_dir);
                let roaming = var("APPDATA")
                    .map(PathBuf::from)
                    .or_else(|| var("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Roaming")))
                    .unwrap_or_else(std::env::temp_dir);
                (
                    local.join("Programs").join("Moku"),
                    roaming.join("Microsoft").join("Windows").join("Start Menu").join("Programs").join(SHORTCUT_NAME),
                )
            }
        };
        let reg = reg_root.clone().unwrap_or_else(|| REAL_REG_ROOT.to_string());
        Layout {
            install_dir,
            shortcut,
            uninstall_key: format!(r"{reg}\Uninstall\{APP_KEY}"),
            run_key: run_override.unwrap_or_else(|| format!(r"{reg}\Run")),
            data_dir,
            overridden,
        }
    }

    pub fn exe(&self, name: &str) -> PathBuf {
        self.install_dir.join(name)
    }

    /// `"<dir>\moku.exe"`, the Run value's data (quoted like the settings app writes it).
    pub fn run_command(&self) -> String {
        quoted(&self.exe(ISLAND_EXE))
    }

    /// Apps & Features: `"<dir>\moku-setup.exe" --uninstall`.
    pub fn uninstall_command(&self, silent: bool) -> String {
        let base = format!("{} --uninstall", quoted(&self.exe(SETUP_EXE)));
        if silent { format!("{base} /S") } else { base }
    }
}

fn quoted(p: &Path) -> String {
    format!("\"{}\"", p.display())
}

/// Is `path` inside `dir` (or `dir` itself)? Case-insensitive, separators normalised, as Windows
/// compares paths. Used to stop only the Moku processes that run from the program folder.
pub fn path_is_within(path: &Path, dir: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('/', "\\").to_lowercase();
        let s = s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s);
        s.trim_end_matches('\\').to_string()
    };
    let (p, d) = (norm(path), norm(dir));
    !d.is_empty() && (p == d || p.starts_with(&format!("{d}\\")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn real_layout_without_overrides() {
        let l = Layout::resolve(env(&[("LOCALAPPDATA", r"C:\Users\a\AppData\Local"), ("APPDATA", r"C:\Users\a\AppData\Roaming")]), PathBuf::from("d"));
        assert!(!l.overridden);
        assert_eq!(l.install_dir, PathBuf::from(r"C:\Users\a\AppData\Local\Programs\Moku"));
        assert_eq!(l.shortcut, PathBuf::from(r"C:\Users\a\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Moku.lnk"));
        assert_eq!(l.uninstall_key, r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Moku");
        assert_eq!(l.run_key, r"Software\Microsoft\Windows\CurrentVersion\Run");
        assert_eq!(l.run_command(), r#""C:\Users\a\AppData\Local\Programs\Moku\moku.exe""#);
        assert_eq!(l.uninstall_command(true), r#""C:\Users\a\AppData\Local\Programs\Moku\moku-setup.exe" --uninstall /S"#);
    }

    #[test]
    fn overrides_move_every_location() {
        let l = Layout::resolve(
            env(&[("LOCALAPPDATA", r"C:\real"), ("APPDATA", r"C:\real"), (INSTALL_ROOT_ENV, r"E:\t\root"), (REG_ROOT_ENV, r"\Software\MokuSetupTest\1\")]),
            PathBuf::from(r"E:\t\data"),
        );
        assert!(l.overridden);
        assert_eq!(l.install_dir, PathBuf::from(r"E:\t\root\Programs\Moku"));
        assert_eq!(l.shortcut, PathBuf::from(r"E:\t\root\Start Menu\Programs\Moku.lnk"));
        assert_eq!(l.uninstall_key, r"Software\MokuSetupTest\1\Uninstall\Moku");
        assert_eq!(l.run_key, r"Software\MokuSetupTest\1\Run");
        // The settings app's own override wins for the Run key.
        let l = Layout::resolve(env(&[(REG_ROOT_ENV, r"Software\T"), (AUTOSTART_KEY_ENV, r"Software\T\MyRun")]), PathBuf::new());
        assert_eq!(l.run_key, r"Software\T\MyRun");
        // Blank values do not count.
        assert!(!Layout::resolve(env(&[(INSTALL_ROOT_ENV, "  ")]), PathBuf::new()).overridden);
    }

    #[test]
    fn within_compares_like_windows() {
        let d = Path::new(r"C:\Users\a\AppData\Local\Programs\Moku");
        assert!(path_is_within(Path::new(r"c:\users\A\appdata\local\programs\moku\moku.exe"), d));
        assert!(path_is_within(Path::new(r"\\?\C:\Users\a\AppData\Local\Programs\Moku\moku.exe"), d));
        assert!(path_is_within(Path::new(r"C:/Users/a/AppData/Local/Programs/Moku/sub/x.exe"), d));
        assert!(!path_is_within(Path::new(r"C:\Users\a\AppData\Local\Programs\Moku2\moku.exe"), d));
        assert!(!path_is_within(Path::new(r"D:\build\release\moku.exe"), d));
        assert!(!path_is_within(Path::new(r"C:\x.exe"), Path::new("")));
    }
}
