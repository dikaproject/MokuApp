//! The "Kirim ke HP" switch of the Fitur page (v0.3, track R): `[phone]` of config.toml.
//!
//! It lives in its own Slint global (`PhoneCfg`, `ui/pages-r.slint`) instead of the shared `Form`, and
//! is read and saved here with the same read-modify-write the rest of the app uses
//! (`Config::update_file`): the switch writes only its own key and never clobbers what the island or
//! another page saved.

use std::path::{Path, PathBuf};

use moku_core::Config;
use slint::ComponentHandle;

use crate::{PhoneCfg, SettingsWindow};

pub fn read(c: &Config) -> bool {
    c.phone.enabled
}

pub fn apply(c: &mut Config, enabled: bool) {
    c.phone.enabled = enabled;
}

/// Save the switch into the config file at `path` (a missing or broken file falls back to the defaults).
pub fn save(path: &Path, enabled: bool) -> std::io::Result<()> {
    Config::update_file(path, &Config::default(), |c| apply(c, enabled)).map(|_| ())
}

/// Hook the `PhoneCfg` callbacks up to the config file at `path`.
pub fn wire(ui: &SettingsWindow, path: PathBuf) {
    let p = ui.global::<PhoneCfg>();
    p.on_reload({
        let (ui, path) = (ui.as_weak(), path.clone());
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let (c, _) = Config::load_from(&path);
            ui.global::<PhoneCfg>().set_enabled(read(&c));
        }
    });
    p.on_commit({
        let ui = ui.as_weak();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            if let Err(e) = save(&path, ui.global::<PhoneCfg>().get_enabled()) {
                eprintln!("phone: cannot save config.toml: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-settings-phone-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("config.toml")
    }

    #[test]
    fn the_switch_round_trips_through_the_config() {
        let mut c = Config::default();
        assert!(read(&c), "on by default: nothing listens until a transfer is started");
        apply(&mut c, false);
        assert!(!read(&c));
    }

    #[test]
    fn saving_touches_only_the_phone_key() {
        let path = temp("only-phone");
        let mut other = Config::default();
        other.island.scale = 1.25;
        other.save_to(&path).unwrap();
        save(&path, false).unwrap();
        let (back, warnings) = Config::load_from(&path);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back.island.scale, 1.25, "the other writer's edit survives");
        assert!(!back.phone.enabled);
    }

    #[test]
    fn saving_without_a_file_creates_one_with_the_defaults_and_the_change() {
        let path = temp("fresh");
        assert!(!path.exists());
        save(&path, false).unwrap();
        let (back, _) = Config::load_from(&path);
        assert!(!back.phone.enabled);
        assert_eq!(back.island, Config::default().island);
    }
}
