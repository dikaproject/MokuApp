//! Loading and saving `config.toml` for the settings window.
//!
//! Every save re-reads the file, applies the form on top and writes the result, so keys the UI
//! does not manage (geocoded coordinates, onboarding flag, anything a newer Moku added) are kept
//! and a hand edit made while the window was open is not lost. The write itself is the core's
//! atomic temp-file + rename, which the island's watcher turns into a hot reload. A save that
//! would not change the file is skipped, so a plain "open and close" never touches it.

use std::io;
use std::path::PathBuf;

use moku_core::{Config, Warning};

use crate::{Form, Problems};

pub struct Store {
    path: PathBuf,
    /// The last config we read or wrote; used when the file on disk is unreadable mid-edit.
    last: Config,
}

/// Result of [`Store::save`].
#[derive(Debug)]
pub struct Saved {
    /// False when the file already had exactly these values.
    pub wrote: bool,
    /// Form values that were not usable (the file keeps the old value for those).
    pub problems: Problems,
}

impl Store {
    /// Open `path` (a missing file means defaults). Returns the config and any load warnings.
    pub fn open(path: PathBuf) -> (Store, Config, Vec<Warning>) {
        let (cfg, warnings) = Config::load_from(&path);
        (Store { path, last: cfg.clone() }, cfg, warnings)
    }

    /// The real `%APPDATA%\Moku\config.toml` (or `$MOKU_DATA_DIR`).
    pub fn open_default() -> (Store, Config, Vec<Warning>) {
        Store::open(moku_core::paths::config_file())
    }

    /// The last config we read or wrote.
    pub fn current(&self) -> &Config {
        &self.last
    }

    /// Save the form values.
    pub fn save(&mut self, form: &Form) -> io::Result<Saved> {
        self.save_with(form, |_| {})
    }

    /// Save the form values plus a change to a field the form does not hold
    /// (e.g. `onboarding.completed`).
    pub fn save_with(&mut self, form: &Form, extra: impl FnOnce(&mut Config)) -> io::Result<Saved> {
        let disk = match Config::try_load_from(&self.path) {
            Ok((c, _)) => c,
            // Half-written or locked right now: build on what we last knew.
            Err(_) => self.last.clone(),
        };
        let mut cfg = disk.clone();
        let problems = form.apply_to(&mut cfg);
        extra(&mut cfg);
        // A missing file reads as the defaults, so "nothing changed" never creates one.
        let wrote = cfg != disk;
        if wrote {
            cfg.save_to(&self.path)?;
        }
        self.last = cfg;
        Ok(Saved { wrote, problems })
    }

    /// Change fields outside the form (read-modify-write like [`Store::save`]). Skips the write
    /// when nothing changes. Returns the config now on disk.
    pub fn update(&mut self, change: impl FnOnce(&mut Config)) -> io::Result<Config> {
        let cfg = Config::update_file(&self.path, &self.last, change)?;
        self.last = cfg.clone();
        Ok(cfg)
    }

    /// Whether `config.toml` exists (a missing file means "all defaults").
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Replace the whole file with the defaults, keeping the onboarding flag.
    pub fn reset(&mut self) -> io::Result<Config> {
        let mut cfg = Config::default();
        cfg.onboarding.completed = self.last.onboarding.completed;
        cfg.save_to(&self.path)?;
        self.last = cfg.clone();
        Ok(cfg)
    }

    /// Mark onboarding done and save it together with the form.
    pub fn finish_onboarding(&mut self, form: &Form) -> io::Result<Saved> {
        self.save_with(form, |c| c.onboarding.completed = true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-settings-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn settings_binding_roundtrip() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("config.toml");

        // load (no file yet: defaults)
        let (mut store, cfg, warnings) = Store::open(path.clone());
        assert!(warnings.is_empty());
        assert_eq!(cfg, Config::default());

        // change through the form
        let mut form = Form::from_config(&cfg);
        form.island_style = 1;
        form.island_scale = 90;
        form.appearance_accent = "#30D158".into();
        form.media_lyrics = false;
        form.today_city = "Bandung".into();
        form.clipboard_ignore = "KeePassXC.exe, Foo.exe".into();
        form.reminders_water = true;
        form.prayer_lat = "-7.25".into();

        // save
        let saved = store.save(&form).unwrap();
        assert!(saved.wrote && saved.problems.is_clear());
        assert!(path.exists());

        // reload with a fresh store, as the island or the next settings run would
        let (_, back, warnings) = Store::open(path.clone());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back.island.style, moku_core::config::IslandStyle::Notch);
        assert_eq!(back.island.scale, 0.9);
        assert_eq!(back.appearance.accent, "#30D158");
        assert!(!back.media.lyrics);
        assert_eq!(back.today.city, "Bandung");
        assert_eq!(back.clipboard.ignore_apps, vec!["KeePassXC.exe", "Foo.exe"]);
        assert!(back.reminders.water);
        assert_eq!(back.prayer.latitude, -7.25);
        // untouched sections keep their defaults
        assert_eq!(back.general, Config::default().general);
        assert_eq!(back.timer, Config::default().timer);

        // and the form built from the reloaded config shows what the user saved
        let form2 = Form::from_config(&back);
        assert_eq!(form2.island_scale, 90);
        assert_eq!(form2.today_city, "Bandung");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_keeps_keys_the_form_does_not_manage() {
        let dir = temp_dir("keep");
        let path = dir.join("config.toml");
        let mut c = Config::default();
        c.today.latitude = Some(-6.9);
        c.today.longitude = Some(107.6);
        c.today.city = "Bandung".into();
        c.onboarding.completed = true;
        c.save_to(&path).unwrap();

        let (mut store, cfg, _) = Store::open(path.clone());
        let mut form = Form::from_config(&cfg);
        form.system_battery = false;
        store.save(&form).unwrap();

        let (back, _) = Config::load_from(&path);
        assert!(!back.system.battery);
        assert_eq!((back.today.latitude, back.today.longitude), (Some(-6.9), Some(107.6)));
        assert!(back.onboarding.completed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_builds_on_a_hand_edit_made_while_the_window_was_open() {
        let dir = temp_dir("handedit");
        let path = dir.join("config.toml");
        let (mut store, cfg, _) = Store::open(path.clone());
        let mut form = Form::from_config(&cfg);

        // Someone sets an unmanaged key in the file...
        std::fs::write(&path, "[today]\nlatitude = -1.5\nlongitude = 100.25\n[onboarding]\ncompleted = true\n").unwrap();
        // ...then the user flips a switch in the window.
        form.control_enabled = false;
        store.save(&form).unwrap();

        let (back, _) = Config::load_from(&path);
        assert!(!back.control.enabled);
        assert_eq!(back.today.latitude, Some(-1.5));
        assert!(back.onboarding.completed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unchanged_form_does_not_touch_the_file() {
        let dir = temp_dir("nochange");
        let path = dir.join("config.toml");
        let (mut store, cfg, _) = Store::open(path.clone());
        let form = Form::from_config(&cfg);

        // No file and nothing changed: still no file.
        let saved = store.save(&form).unwrap();
        assert!(!saved.wrote);
        assert!(!path.exists());
        // An existing file with the same values is left alone, mtime included.
        Config::default().save_to(&path).unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let saved = store.save(&form).unwrap();
        assert!(!saved.wrote);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_file_does_not_lose_the_last_good_settings() {
        let dir = temp_dir("broken");
        let path = dir.join("config.toml");
        let mut c = Config::default();
        c.island.scale = 1.2;
        c.save_to(&path).unwrap();
        let (mut store, cfg, _) = Store::open(path.clone());

        std::fs::write(&path, "[island\nscale = ").unwrap();
        let mut form = Form::from_config(&cfg);
        form.shelf_enabled = false;
        store.save(&form).unwrap();

        let (back, w) = Config::load_from(&path);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back.island.scale, 1.2);
        assert!(!back.shelf.enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_text_is_reported_and_not_written() {
        let dir = temp_dir("problems");
        let path = dir.join("config.toml");
        let (mut store, cfg, _) = Store::open(path.clone());
        let mut form = Form::from_config(&cfg);
        form.general_hotkey_toggle = "Alt+".into();
        form.monitor_enabled = true;
        let saved = store.save(&form).unwrap();
        assert!(!saved.problems.hotkey_toggle.is_empty());
        let (back, w) = Config::load_from(&path);
        assert!(w.is_empty());
        assert_eq!(back.general.hotkey_toggle, "Alt+M");
        assert!(back.monitor.enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finishing_onboarding_saves_choices_and_the_flag() {
        let dir = temp_dir("onboarding");
        let path = dir.join("config.toml");
        let (mut store, cfg, _) = Store::open(path.clone());
        assert!(!cfg.onboarding.completed);
        let mut form = Form::from_config(&cfg);
        form.island_style = 1;
        form.general_autostart = false;
        store.finish_onboarding(&form).unwrap();
        let (back, _) = Config::load_from(&path);
        assert!(back.onboarding.completed);
        assert_eq!(back.island.style, moku_core::config::IslandStyle::Notch);
        assert!(!back.general.autostart);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reset_restores_defaults_but_keeps_onboarding_done() {
        let dir = temp_dir("reset");
        let path = dir.join("config.toml");
        let mut c = Config::default();
        c.island.scale = 1.3;
        c.onboarding.completed = true;
        c.save_to(&path).unwrap();
        let (mut store, _, _) = Store::open(path.clone());
        let cfg = store.reset().unwrap();
        assert_eq!(cfg.island.scale, 1.0);
        assert!(cfg.onboarding.completed);
        assert_eq!(Config::load_from(&path).0, cfg);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
