//! v0.2 track G: the Claude Code block of the Notifikasi page (`ui/pages-g.slint`, global
//! `ClaudeCfg`) <-> the `[claude]` section of `config.toml`.
//!
//! The block reads the section each time it is shown (so "Reset ke awal" and hand edits show
//! up) and saves every change at once with the core's read-modify-write, so the main form's own
//! debounced save and this one never undo each other: each only writes the keys it owns.

use std::io;
use std::path::{Path, PathBuf};

use moku_core::Config;
use moku_core::config::Claude;
use slint::ComponentHandle;

use crate::{ClaudeCfg, SettingsWindow};

pub fn wire(ui: &SettingsWindow, path: PathBuf) {
    let g = ui.global::<ClaudeCfg>();
    g.on_load({
        let (ui, path) = (ui.as_weak(), path.clone());
        move || {
            if let Some(ui) = ui.upgrade() {
                show(&ui.global::<ClaudeCfg>(), &read(&path));
            }
        }
    });
    g.on_save({
        let ui = ui.as_weak();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            if let Err(e) = save(&path, &from_ui(&ui.global::<ClaudeCfg>())) {
                eprintln!("claude: cannot save config.toml: {e}");
            }
        }
    });
}

fn show(g: &ClaudeCfg<'_>, c: &Claude) {
    g.set_enabled(c.enabled);
    g.set_done_pill(c.done_pill);
    g.set_stale_minutes(c.stale_minutes as i32);
}

fn from_ui(g: &ClaudeCfg<'_>) -> Claude {
    Claude { enabled: g.get_enabled(), done_pill: g.get_done_pill(), stale_minutes: g.get_stale_minutes().clamp(5, 1440) as u32 }
}

/// `[claude]` as saved (defaults when missing or unreadable).
pub fn read(path: &Path) -> Claude {
    Config::load_from(path).0.claude
}

/// Write `[claude]`, keeping everything else in the file. A file that cannot be parsed right
/// now (being written by someone else) is left alone: the next change saves again.
pub fn save(path: &Path, claude: &Claude) -> io::Result<()> {
    let (disk, _) = Config::try_load_from(path).map_err(|w| io::Error::other(w.to_string()))?;
    Config::update_file(path, &disk, |c| c.claude = claude.clone()).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_section_saves_only_its_own_keys() {
        let dir = std::env::temp_dir().join(format!("moku-settings-claude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        // Missing file → defaults.
        assert_eq!(read(&p), Claude::default());
        // Another writer's values survive.
        let mut other = Config::default();
        other.island.scale = 1.25;
        other.save_to(&p).unwrap();
        let c = Claude { enabled: false, done_pill: false, stale_minutes: 90 };
        save(&p, &c).unwrap();
        assert_eq!(read(&p), c);
        assert_eq!(Config::load_from(&p).0.island.scale, 1.25);
        // A half-written file is not overwritten.
        std::fs::write(&p, "[island\nscale = ").unwrap();
        assert!(save(&p, &Claude::default()).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[island\nscale = ");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
