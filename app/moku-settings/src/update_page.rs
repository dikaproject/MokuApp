//! Settings → Tentang → "Periksa pembaruan otomatis" (`update.check`), track I.
//!
//! Its own Slint global (`UpdateCfg`, `ui/pages-i.slint`) instead of a `Form` field, so the
//! shared form stays untouched. The toggle writes through the core's read-modify-write
//! (`Config::update_file`), so it never overwrites what the form saved, and the form's own
//! saves keep `update.check` because they also re-read the file first.

use moku_core::Config;
use moku_core::release::{REPO, UPDATE_PUBLIC_KEY, updater_built_in};
use slint::ComponentHandle;

use crate::{SettingsWindow, UpdateCfg};

pub fn wire(ui: &SettingsWindow, config: &Config) {
    let g = ui.global::<UpdateCfg>();
    g.set_check(config.update.check);
    g.set_available(available());
    g.set_repo(REPO.into());
    let fallback = config.clone();
    g.on_set_check(move |on| {
        if let Err(e) = set_check(&moku_core::paths::config_file(), &fallback, on) {
            eprintln!("cannot save update.check: {e}");
        }
    });
}

/// Does this build have an updater (a real repository AND the key releases are signed with)?
/// When not, the Tentang switch is greyed out ("Belum aktif di build ini").
fn available() -> bool {
    updater_built_in(REPO, UPDATE_PUBLIC_KEY)
}

fn set_check(path: &std::path::Path, fallback: &Config, on: bool) -> std::io::Result<Config> {
    Config::update_file(path, fallback, |c| c.update.check = on)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_is_available_only_when_the_build_can_verify_releases() {
        // Same rule as the island's updater: repository set AND public key set.
        assert_eq!(available(), !moku_core::release::is_placeholder(REPO) && UPDATE_PUBLIC_KEY.is_some());
        assert!(!updater_built_in(moku_core::release::PLACEHOLDER_REPO, Some(&"ab".repeat(64))));
        assert!(!updater_built_in("moku-app/moku", None));
    }

    #[test]
    fn toggle_writes_only_update_check() {
        let dir = std::env::temp_dir().join(format!("moku-settings-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        let mut other = Config::default();
        other.island.scale = 1.25;
        other.save_to(&p).unwrap();
        let now = set_check(&p, &Config::default(), false).unwrap();
        assert!(!now.update.check);
        let (back, _) = Config::load_from(&p);
        assert_eq!(back.island.scale, 1.25, "other settings survive");
        assert!(!back.update.check);
        set_check(&p, &Config::default(), true).unwrap();
        assert!(Config::load_from(&p).0.update.check);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
