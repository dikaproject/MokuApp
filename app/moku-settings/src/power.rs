//! The "Daya & audio" switches of the Fitur page (v0.2, track H): `[power]` of config.toml.
//!
//! They live in their own Slint global (`PowerCfg`, `ui/pages-h.slint`) instead of the shared
//! `Form`, and are read and saved here with the same read-modify-write the rest of the app uses
//! (`Config::update_file`): a toggle writes only its own keys and never clobbers what the island or
//! another page saved.

use std::path::{Path, PathBuf};

use moku_core::Config;
use slint::ComponentHandle;

use crate::{PowerCfg, SettingsWindow};

/// The three switches, in the order of the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switches {
    pub charging_animation: bool,
    pub earbuds: bool,
    pub mixer: bool,
}

pub fn read(c: &Config) -> Switches {
    Switches { charging_animation: c.power.charging_animation, earbuds: c.power.earbuds, mixer: c.power.mixer }
}

pub fn apply(c: &mut Config, s: Switches) {
    c.power.charging_animation = s.charging_animation;
    c.power.earbuds = s.earbuds;
    c.power.mixer = s.mixer;
}

/// Save `s` into the config file at `path` (a missing or broken file falls back to the defaults).
pub fn save(path: &Path, s: Switches) -> std::io::Result<()> {
    Config::update_file(path, &Config::default(), |c| apply(c, s)).map(|_| ())
}

/// Hook the `PowerCfg` callbacks up to the config file at `path`.
pub fn wire(ui: &SettingsWindow, path: PathBuf) {
    let p = ui.global::<PowerCfg>();
    p.on_reload({
        let (ui, path) = (ui.as_weak(), path.clone());
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let (c, _) = Config::load_from(&path);
            let s = read(&c);
            let p = ui.global::<PowerCfg>();
            p.set_charging_animation(s.charging_animation);
            p.set_earbuds(s.earbuds);
            p.set_mixer(s.mixer);
        }
    });
    p.on_commit({
        let ui = ui.as_weak();
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let p = ui.global::<PowerCfg>();
            let s = Switches { charging_animation: p.get_charging_animation(), earbuds: p.get_earbuds(), mixer: p.get_mixer() };
            if let Err(e) = save(&path, s) {
                eprintln!("power: cannot save config.toml: {e}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-settings-power-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("config.toml")
    }

    #[test]
    fn switches_round_trip_through_the_config() {
        let mut c = Config::default();
        assert_eq!(read(&c), Switches { charging_animation: true, earbuds: true, mixer: true }, "all on by default");
        apply(&mut c, Switches { charging_animation: false, earbuds: true, mixer: false });
        assert_eq!(read(&c), Switches { charging_animation: false, earbuds: true, mixer: false });
    }

    #[test]
    fn saving_touches_only_the_power_keys() {
        let path = temp("only-power");
        // Another writer (the island, another page) saved a different scale in the meantime.
        let mut other = Config::default();
        other.island.scale = 1.25;
        other.save_to(&path).unwrap();
        save(&path, Switches { charging_animation: false, earbuds: false, mixer: true }).unwrap();
        let (back, warnings) = Config::load_from(&path);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back.island.scale, 1.25, "the other writer's edit survives");
        assert_eq!(read(&back), Switches { charging_animation: false, earbuds: false, mixer: true });
    }

    #[test]
    fn saving_without_a_file_creates_one_with_the_defaults_and_the_change() {
        let path = temp("fresh");
        assert!(!path.exists());
        save(&path, Switches { charging_animation: true, earbuds: true, mixer: false }).unwrap();
        let (back, _) = Config::load_from(&path);
        assert!(!back.power.mixer && back.power.earbuds);
        assert_eq!(back.island, Config::default().island);
    }

    #[test]
    fn saving_an_unchanged_value_does_not_touch_the_file() {
        let path = temp("noop");
        Config::default().save_to(&path).unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        save(&path, Switches { charging_animation: true, earbuds: true, mixer: true }).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);
    }
}
