//! Track L: OCR layar and the smart clipboard actions (`[ocr]` and three keys of `[clipboard]`),
//! bound to the `OcrCfg` global of `ui/pages-l.slint`. Saved with a read-modify-write of
//! config.toml (`Config::update_file`), so it never overwrites what the main form saves, and
//! debounced while the hotkey is typed.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use moku_core::Config;
use moku_core::hotkey::{self, Hotkey, Slot};
use slint::ComponentHandle;

use crate::{OcrCfg, SettingsWindow};

/// The values of the page.
#[derive(Clone, Debug, PartialEq)]
pub struct Values {
    pub enabled: bool,
    pub hotkey: String,
    pub language: String,
    pub smart_actions: bool,
    pub smart_pill: bool,
    pub otp_guard: bool,
}

/// Why `hotkey` cannot be used (Indonesian), or `None`.
pub fn problem(hotkey: &str, cfg: &Config) -> Option<String> {
    if Hotkey::parse(hotkey).is_err() {
        return Some("Pakai Ctrl, Alt, Shift, atau Win, lalu satu tombol".into());
    }
    hotkey::clash(cfg, Slot::Ocr, hotkey).map(hotkey::clash_message)
}

/// Why `language` is not a usable tag (Indonesian), or `None` (empty = automatic).
pub fn language_problem(language: &str) -> Option<String> {
    let l = language.trim();
    let ok = l.is_empty() || (l.len() <= 16 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    (!ok).then(|| "Tulis kode bahasa, mis. id-ID atau en-US".into())
}

/// Apply the page to a config; an unusable hotkey or language keeps the old one.
pub fn apply(v: &Values, cfg: &mut Config) {
    cfg.ocr.enabled = v.enabled;
    cfg.clipboard.smart_actions = v.smart_actions;
    cfg.clipboard.smart_pill = v.smart_pill;
    cfg.clipboard.otp_guard = v.otp_guard;
    if problem(&v.hotkey, cfg).is_none() {
        cfg.ocr.hotkey = v.hotkey.trim().to_string();
    }
    if language_problem(&v.language).is_none() {
        cfg.ocr.language = v.language.trim().to_string();
    }
}

fn read(ui: &SettingsWindow) -> Values {
    let g = ui.global::<OcrCfg>();
    Values {
        enabled: g.get_enabled(),
        hotkey: g.get_hotkey().to_string(),
        language: g.get_language().to_string(),
        smart_actions: g.get_smart_actions(),
        smart_pill: g.get_smart_pill(),
        otp_guard: g.get_otp_guard(),
    }
}

/// Fill the page from `config` and save its edits.
pub fn wire(ui: &SettingsWindow, config: &Config) {
    let g = ui.global::<OcrCfg>();
    g.set_enabled(config.ocr.enabled);
    g.set_hotkey(config.ocr.hotkey.clone().into());
    g.set_language(config.ocr.language.clone().into());
    g.set_smart_actions(config.clipboard.smart_actions);
    g.set_smart_pill(config.clipboard.smart_pill);
    g.set_otp_guard(config.clipboard.otp_guard);
    let timer = Rc::new(slint::Timer::default());
    let last = Rc::new(RefCell::new(config.clone()));
    let weak = ui.as_weak();
    g.on_changed(move || {
        let Some(ui) = weak.upgrade() else { return };
        let v = read(&ui);
        let g = ui.global::<OcrCfg>();
        g.set_problem(problem(&v.hotkey, &last.borrow()).unwrap_or_default().into());
        g.set_language_problem(language_problem(&v.language).unwrap_or_default().into());
        let (weak, last) = (weak.clone(), last.clone());
        timer.start(slint::TimerMode::SingleShot, Duration::from_millis(400), move || {
            let Some(ui) = weak.upgrade() else { return };
            let v = read(&ui);
            let fallback = last.borrow().clone();
            match Config::update_file(&moku_core::paths::config_file(), &fallback, |c| apply(&v, c)) {
                Ok(c) => *last.borrow_mut() = c,
                Err(e) => eprintln!("ocr settings: {e}"),
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values {
        Values { enabled: true, hotkey: "Ctrl+Alt+T".into(), language: String::new(), smart_actions: true, smart_pill: true, otp_guard: true }
    }

    #[test]
    fn hotkey_problems() {
        let c = Config::default();
        assert_eq!(problem("Win+Shift+T", &c), None);
        assert_eq!(problem("Ctrl+Alt+T", &c), None);
        assert!(problem("T", &c).is_some(), "needs a modifier");
        assert_eq!(problem("Alt+M", &c).as_deref(), Some("Sudah dipakai pintasan buka/tutup island"), "the island's own toggle");
        assert_eq!(problem("alt+space", &c).as_deref(), Some("Sudah dipakai peluncur"));
        assert!(problem("Alt+Nope", &c).is_some());
        // Tanya AI's key, once it is switched on, is taken too (while it is off it holds nothing).
        let mut c2 = Config::default();
        assert_eq!(problem("Ctrl+Alt+A", &c2), None);
        c2.ai.enabled = true;
        assert_eq!(problem("Ctrl+Alt+A", &c2).as_deref(), Some("Sudah dipakai Tanya AI"));
    }

    #[test]
    fn language_problems() {
        for ok in ["", "  ", "id-ID", "en-US", "ja", "zh-Hans-CN"] {
            assert_eq!(language_problem(ok), None, "{ok:?}");
        }
        for bad in ["not a tag!", "id_ID", "averyveryveryverylongtag-x", "é"] {
            assert!(language_problem(bad).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn apply_saves_good_values_and_keeps_old_ones_for_bad() {
        let mut c = Config::default();
        apply(&Values { enabled: false, hotkey: " Ctrl+Alt+O ".into(), language: " id-ID ".into(), smart_actions: false, smart_pill: false, otp_guard: false }, &mut c);
        assert_eq!((c.ocr.enabled, c.ocr.hotkey.as_str(), c.ocr.language.as_str()), (false, "Ctrl+Alt+O", "id-ID"));
        assert_eq!((c.clipboard.smart_actions, c.clipboard.smart_pill, c.clipboard.otp_guard), (false, false, false));
        // A bad hotkey / language leaves the previous ones; the toggles still apply.
        apply(&Values { enabled: true, hotkey: "Alt+Nope".into(), language: "bad tag".into(), ..values() }, &mut c);
        assert_eq!((c.ocr.enabled, c.ocr.hotkey.as_str(), c.ocr.language.as_str()), (true, "Ctrl+Alt+O", "id-ID"));
        assert!(c.clipboard.smart_actions && c.clipboard.otp_guard);
        // The result survives a round trip through the file format.
        let toml = c.to_toml();
        let (back, w) = Config::parse(&toml);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back.ocr, c.ocr);
        assert_eq!(back.clipboard, c.clipboard);
    }
}
