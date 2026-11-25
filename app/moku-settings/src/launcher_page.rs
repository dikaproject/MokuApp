//! Track K: the launcher's settings (`[launcher]`), bound to the `LauncherCfg` global of
//! `ui/pages-k.slint`. Saved with a read-modify-write of config.toml (`Config::update_file`),
//! so it never overwrites what the main form saves, and debounced while the hotkey is typed.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use moku_core::Config;
use moku_core::hotkey::{self, Hotkey, Slot};
use slint::ComponentHandle;

use crate::{LauncherCfg, SettingsWindow};

/// The values of the page.
#[derive(Clone, Debug, PartialEq)]
pub struct Values {
    pub enabled: bool,
    pub hotkey: String,
    pub store_apps: bool,
    pub shelf_files: bool,
}

/// Why `hotkey` cannot be used (Indonesian), or `None`.
pub fn problem(hotkey: &str, cfg: &Config) -> Option<String> {
    if Hotkey::parse(hotkey).is_err() {
        return Some("Pakai Ctrl, Alt, Shift, atau Win, lalu satu tombol".into());
    }
    hotkey::clash(cfg, Slot::Launcher, hotkey).map(hotkey::clash_message)
}

/// Apply the page to a config; an unusable hotkey keeps the old one.
pub fn apply(v: &Values, cfg: &mut Config) {
    cfg.launcher.enabled = v.enabled;
    cfg.launcher.store_apps = v.store_apps;
    cfg.launcher.shelf_files = v.shelf_files;
    if problem(&v.hotkey, cfg).is_none() {
        cfg.launcher.hotkey = v.hotkey.trim().to_string();
    }
}

fn read(ui: &SettingsWindow) -> Values {
    let g = ui.global::<LauncherCfg>();
    Values { enabled: g.get_enabled(), hotkey: g.get_hotkey().to_string(), store_apps: g.get_store_apps(), shelf_files: g.get_shelf_files() }
}

/// Fill the page from `config` and save its edits.
pub fn wire(ui: &SettingsWindow, config: &Config) {
    let g = ui.global::<LauncherCfg>();
    let l = &config.launcher;
    g.set_enabled(l.enabled);
    g.set_hotkey(l.hotkey.clone().into());
    g.set_store_apps(l.store_apps);
    g.set_shelf_files(l.shelf_files);
    let timer = Rc::new(slint::Timer::default());
    let last = Rc::new(RefCell::new(config.clone()));
    let weak = ui.as_weak();
    g.on_changed(move || {
        let Some(ui) = weak.upgrade() else { return };
        let v = read(&ui);
        ui.global::<LauncherCfg>().set_problem(problem(&v.hotkey, &last.borrow()).unwrap_or_default().into());
        let (weak, last) = (weak.clone(), last.clone());
        timer.start(slint::TimerMode::SingleShot, Duration::from_millis(400), move || {
            let Some(ui) = weak.upgrade() else { return };
            let v = read(&ui);
            let fallback = last.borrow().clone();
            match Config::update_file(&moku_core::paths::config_file(), &fallback, |c| apply(&v, c)) {
                Ok(c) => *last.borrow_mut() = c,
                Err(e) => eprintln!("launcher settings: {e}"),
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_problems_and_apply() {
        let c = Config::default();
        assert_eq!(problem("Alt+Space", &c), None);
        assert!(problem("Space", &c).is_some());
        assert!(problem("Alt+M", &c).is_some(), "the island's own toggle");
        let mut c2 = c.clone();
        apply(&Values { enabled: false, hotkey: "Alt+Nope".into(), store_apps: false, shelf_files: true }, &mut c2);
        assert_eq!((c2.launcher.enabled, c2.launcher.hotkey.as_str(), c2.launcher.store_apps), (false, "Alt+Space", false));
        apply(&Values { enabled: true, hotkey: " Ctrl+Alt+K ".into(), store_apps: true, shelf_files: false }, &mut c2);
        assert_eq!((c2.launcher.hotkey.as_str(), c2.launcher.shelf_files), ("Ctrl+Alt+K", false));
    }
}
