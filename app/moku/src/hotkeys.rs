//! Global hotkeys, in one place: who holds which combination, and one notice for every failure.
//!
//! Moku has five (panel, quit, launcher, OCR, Tanya AI: [`Slot`]). Each registers its own with
//! `RegisterHotKey` through [`register`], which
//!
//! * refuses a combination that another Moku feature already holds ([`Outcome::Clash`]): the later
//!   one is skipped, never fighting for the key (the settings app refuses the same assignment
//!   when it is typed, `moku_core::hotkey::clash`);
//! * reports a combination that another program holds ([`Outcome::Taken`]);
//! * and [`announce`] raises the same pill for both, once per feature and combination: "Pintasan
//!   Ctrl+Alt+T dipakai aplikasi lain".
//!
//! The table of who holds what is pure ([`Claims`], tested); only [`register`] and [`unregister`]
//! touch Windows.

use std::sync::Mutex;

use moku_core::hotkey::{Hotkey, Slot};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};

use crate::activity::priority;
use crate::app::App;
use crate::i18n::tr_s;
use crate::ui::*;

/// Which feature holds which combination (registered with Windows).
#[derive(Default)]
pub struct Claims {
    held: Vec<(Slot, Hotkey)>,
}

impl Claims {
    /// `slot` takes `hk`, or learns which other slot already holds it. A slot holds one combination
    /// at a time: claiming again replaces the earlier one.
    pub fn claim(&mut self, slot: Slot, hk: Hotkey) -> Result<(), Slot> {
        if let Some((other, _)) = self.held.iter().find(|(s, h)| *s != slot && *h == hk) {
            return Err(*other);
        }
        self.held.retain(|(s, _)| *s != slot);
        self.held.push((slot, hk));
        Ok(())
    }

    pub fn release(&mut self, slot: Slot) {
        self.held.retain(|(s, _)| *s != slot);
    }

    pub fn holder(&self, hk: Hotkey) -> Option<Slot> {
        self.held.iter().find(|(_, h)| *h == hk).map(|(s, _)| *s)
    }
}

static CLAIMS: Mutex<Claims> = Mutex::new(Claims { held: Vec::new() });
/// (slot, lower-case combination) a notice was already shown for.
static WARNED: Mutex<Vec<(Slot, String)>> = Mutex::new(Vec::new());

/// What came of one registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Registered,
    /// Another program holds the combination (`RegisterHotKey` failed).
    Taken,
    /// Another Moku feature holds it: this one is skipped.
    Clash(Slot),
    /// Not a hotkey (the config loader replaces those by the default, so this is rare).
    Invalid,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Register `combo` for `slot` on `hwnd` under `id`. Never panics, never blocks.
pub fn register(hwnd: HWND, id: i32, slot: Slot, combo: &str) -> Outcome {
    let Ok(hk) = Hotkey::parse(combo) else { return Outcome::Invalid };
    let mut claims = lock(&CLAIMS);
    if let Err(other) = claims.claim(slot, hk) {
        return Outcome::Clash(other);
    }
    let ok = unsafe { RegisterHotKey(Some(hwnd), id, HOT_KEY_MODIFIERS(hk.modifiers) | MOD_NOREPEAT, hk.vk).is_ok() };
    if ok {
        Outcome::Registered
    } else {
        claims.release(slot);
        Outcome::Taken
    }
}

/// Let go of `slot`'s hotkey (a no-op when it holds none).
pub fn unregister(hwnd: HWND, id: i32, slot: Slot) {
    lock(&CLAIMS).release(slot);
    unsafe {
        let _ = UnregisterHotKey(Some(hwnd), id);
    }
}

/// Is a notice for this (slot, combination) still to be shown? Marks it as shown.
fn first_time(slot: Slot, combo: &str) -> bool {
    let key = (slot, combo.trim().to_ascii_lowercase());
    let mut w = lock(&WARNED);
    if w.contains(&key) {
        return false;
    }
    w.push(key);
    true
}

fn pill_id(slot: Slot) -> &'static str {
    match slot {
        Slot::Panel => "hotkey.panel",
        Slot::Quit => "hotkey.quit",
        Slot::Launcher => "hotkey.launcher",
        Slot::Ocr => "hotkey.ocr",
        Slot::Ai => "hotkey.ai",
    }
}

/// The text of the pill for a failed registration (`None` for a success or a non-hotkey).
pub fn notice_text(slot: Slot, combo: &str, outcome: Outcome) -> Option<String> {
    match outcome {
        Outcome::Taken => Some(tr_s(format!("Pintasan {combo} dipakai aplikasi lain"), format!("Shortcut {combo} is used by another app"))),
        Outcome::Clash(other) => Some(tr_s(
            format!("Pintasan {combo} sudah dipakai {}: {} dilewati", other.label(), slot.label()),
            format!("Shortcut {combo} is already used by {}: {} is skipped", other.label_en(), slot.label_en()),
        )),
        Outcome::Registered | Outcome::Invalid => None,
    }
}

/// Raise the pill for a hotkey that could not be registered (once per feature and combination).
pub fn announce(app: &mut App, slot: Slot, combo: &str, outcome: Outcome) {
    let Some(text_) = notice_text(slot, combo, outcome) else { return };
    if !first_time(slot, combo) {
        return;
    }
    crate::log_warn!("hotkey {combo} for {slot:?}: {outcome:?}");
    app.alert(pill_id(slot), priority::WARNING, 6.0, move |_| pill([icon(icons::WARNING).color(color::ORANGE), text(text_.clone()).semibold()]));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hk(s: &str) -> Hotkey {
        Hotkey::parse(s).unwrap()
    }

    #[test]
    fn a_combination_belongs_to_one_feature_at_a_time() {
        let mut c = Claims::default();
        assert_eq!(c.claim(Slot::Panel, hk("Alt+M")), Ok(()));
        assert_eq!(c.claim(Slot::Launcher, hk("alt+m")), Err(Slot::Panel), "the later one is skipped, whatever the spelling");
        assert_eq!(c.claim(Slot::Launcher, hk("Alt+Space")), Ok(()));
        // The same slot may claim again (a re-registration after a config reload), or change its key.
        assert_eq!(c.claim(Slot::Launcher, hk("Alt+Space")), Ok(()));
        assert_eq!(c.claim(Slot::Launcher, hk("Alt+L")), Ok(()));
        assert_eq!(c.holder(hk("Alt+Space")), None, "its old key is free again");
        assert_eq!(c.holder(hk("Alt+L")), Some(Slot::Launcher));
        // Letting go frees the key for somebody else.
        c.release(Slot::Panel);
        assert_eq!(c.claim(Slot::Ocr, hk("Alt+M")), Ok(()));
        c.release(Slot::Ocr);
        c.release(Slot::Ocr);
        assert_eq!(c.holder(hk("Alt+M")), None);
    }

    #[test]
    fn every_failure_has_the_same_pill_text() {
        for slot in Slot::ALL {
            let t = notice_text(slot, "Ctrl+Alt+T", Outcome::Taken).unwrap();
            assert!(t.contains("Pintasan Ctrl+Alt+T dipakai aplikasi lain") || t.contains("Shortcut Ctrl+Alt+T is used by another app"), "{t}");
        }
        let c = notice_text(Slot::Ai, "Alt+Space", Outcome::Clash(Slot::Launcher)).unwrap();
        assert!(c.contains("Alt+Space") && (c.contains("peluncur") || c.contains("launcher")), "{c}");
        assert_eq!(notice_text(Slot::Ai, "Alt+A", Outcome::Registered), None);
        assert_eq!(notice_text(Slot::Ai, "nonsense", Outcome::Invalid), None);
    }

    #[test]
    fn a_notice_is_shown_once_per_feature_and_combination() {
        assert!(first_time(Slot::Ocr, "Ctrl+Alt+Home"));
        assert!(!first_time(Slot::Ocr, " ctrl+alt+home "));
        assert!(first_time(Slot::Ai, "Ctrl+Alt+Home"), "another feature has its own notice");
        assert!(first_time(Slot::Ocr, "Ctrl+Alt+End"), "another combination too");
    }

    #[test]
    fn windows_refuses_a_second_registration_and_the_claim_is_given_back() {
        // A real registration on the thread's own (null) window with an unlikely key; nothing is sent
        // anywhere and the key is released at the end.
        let hwnd = HWND::default();
        let first = register(hwnd, 41_001, Slot::Ocr, "Ctrl+Alt+Shift+F23");
        if first != Outcome::Registered {
            // Another program holds it on this PC: the claim must not stay behind.
            assert_eq!(first, Outcome::Taken);
            assert_eq!(lock(&CLAIMS).holder(hk("Ctrl+Alt+Shift+F23")), None);
            return;
        }
        // Another feature wanting the same key is told about the first, without asking Windows.
        assert_eq!(register(hwnd, 41_002, Slot::Ai, "ctrl+alt+shift+f23"), Outcome::Clash(Slot::Ocr));
        unregister(hwnd, 41_001, Slot::Ocr);
        assert_eq!(lock(&CLAIMS).holder(hk("Ctrl+Alt+Shift+F23")), None);
    }
}
