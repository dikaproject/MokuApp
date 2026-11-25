//! Hotkey strings from the config (`"Alt+M"`, `"Ctrl+Alt+Shift+Q"`) → modifiers + virtual key,
//! in the form `RegisterHotKey` wants. Pure logic so it can be validated in the config loader.

/// `MOD_*` values of `RegisterHotKey`.
pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    /// Bitwise OR of `MOD_*`.
    pub modifiers: u32,
    /// Windows virtual-key code.
    pub vk: u32,
}

impl Hotkey {
    /// Case-insensitive, `+`-separated, modifiers in any order, exactly one key last.
    /// Keys: A–Z, 0–9, F1–F24, Space, Enter, Tab, Esc, Up/Down/Left/Right, Home, End, PgUp,
    /// PgDn, Insert, Delete, PrintScreen. Needs at least one modifier unless the key is F1–F24.
    pub fn parse(s: &str) -> Result<Hotkey, String> {
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err(format!("{s:?} is not a hotkey"));
        }
        let (key, mods) = parts.split_last().ok_or_else(|| format!("{s:?} is not a hotkey"))?;
        let mut modifiers = 0;
        for m in mods {
            modifiers |= match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => MOD_CONTROL,
                "alt" => MOD_ALT,
                "shift" => MOD_SHIFT,
                "win" | "super" | "meta" => MOD_WIN,
                _ => return Err(format!("unknown modifier {m:?} in {s:?}")),
            };
        }
        let k = key.to_ascii_uppercase();
        let vk = match k.as_str() {
            _ if k.len() == 1 && k.as_bytes()[0].is_ascii_alphanumeric() => k.as_bytes()[0] as u32,
            "SPACE" => 0x20,
            "ENTER" | "RETURN" => 0x0D,
            "TAB" => 0x09,
            "ESC" | "ESCAPE" => 0x1B,
            "UP" => 0x26,
            "DOWN" => 0x28,
            "LEFT" => 0x25,
            "RIGHT" => 0x27,
            "HOME" => 0x24,
            "END" => 0x23,
            "PGUP" | "PAGEUP" => 0x21,
            "PGDN" | "PAGEDOWN" => 0x22,
            "INSERT" | "INS" => 0x2D,
            "DELETE" | "DEL" => 0x2E,
            "PRINTSCREEN" | "PRTSC" => 0x2C,
            _ if k.starts_with('F') => match k[1..].parse::<u32>() {
                Ok(n @ 1..=24) => 0x70 + n - 1,
                _ => return Err(format!("unknown key {key:?} in {s:?}")),
            },
            _ => return Err(format!("unknown key {key:?} in {s:?}")),
        };
        let is_fkey = (0x70..=0x87).contains(&vk);
        if modifiers == 0 && !is_fkey {
            return Err(format!("{s:?} needs a modifier (Ctrl, Alt, Shift or Win)"));
        }
        Ok(Hotkey { modifiers, vk })
    }
}

/// The five global hotkeys of Moku. One combination can only belong to one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    /// `general.hotkey_toggle`: open / close the panel.
    Panel,
    /// `general.hotkey_quit`.
    Quit,
    /// `launcher.hotkey`.
    Launcher,
    /// `ocr.hotkey`.
    Ocr,
    /// `ai.hotkey` (Tanya AI).
    Ai,
}

impl Slot {
    pub const ALL: [Slot; 5] = [Slot::Panel, Slot::Quit, Slot::Launcher, Slot::Ocr, Slot::Ai];

    /// What the user calls it (Indonesian, as in the settings).
    pub fn label(self) -> &'static str {
        match self {
            Slot::Panel => "pintasan buka/tutup island",
            Slot::Quit => "pintasan keluar",
            Slot::Launcher => "peluncur",
            Slot::Ocr => "OCR",
            Slot::Ai => "Tanya AI",
        }
    }

    /// The same in English (for the island's pill).
    pub fn label_en(self) -> &'static str {
        match self {
            Slot::Panel => "the panel shortcut",
            Slot::Quit => "the quit shortcut",
            Slot::Launcher => "the launcher",
            Slot::Ocr => "OCR",
            Slot::Ai => "Ask AI",
        }
    }

    /// The combination `cfg` gives this slot, as written.
    pub fn combo(self, cfg: &crate::config::Config) -> &str {
        match self {
            Slot::Panel => &cfg.general.hotkey_toggle,
            Slot::Quit => &cfg.general.hotkey_quit,
            Slot::Launcher => &cfg.launcher.hotkey,
            Slot::Ocr => &cfg.ocr.hotkey,
            Slot::Ai => &cfg.ai.hotkey,
        }
    }

    /// Does this slot register a hotkey at all with this config? A switched-off feature does not
    /// (its combination is free for another one).
    pub fn active(self, cfg: &crate::config::Config) -> bool {
        match self {
            Slot::Panel | Slot::Quit => true,
            Slot::Launcher => cfg.launcher.enabled,
            Slot::Ocr => cfg.ocr.enabled,
            Slot::Ai => cfg.ai.enabled,
        }
    }
}

/// Another active slot that already uses `combo` (compared as keys, not as text: `alt+m` is
/// `Alt+M`). `None` when `combo` is free or is not a hotkey.
pub fn clash(cfg: &crate::config::Config, slot: Slot, combo: &str) -> Option<Slot> {
    let h = Hotkey::parse(combo).ok()?;
    Slot::ALL.into_iter().filter(|s| *s != slot && s.active(cfg)).find(|s| Hotkey::parse(s.combo(cfg)).is_ok_and(|o| o == h))
}

/// The text of the validation error in the settings ("Sudah dipakai peluncur").
pub fn clash_message(other: Slot) -> String {
    format!("Sudah dipakai {}", other.label())
}

/// Every pair of active slots with the same combination, in [`Slot::ALL`] order: the first is the
/// one that keeps it, the second is the one the island skips.
pub fn clashes(cfg: &crate::config::Config) -> Vec<(Slot, Slot)> {
    let mut out = vec![];
    for (i, a) in Slot::ALL.iter().enumerate() {
        for b in &Slot::ALL[i + 1..] {
            if a.active(cfg) && b.active(cfg) && matches!((Hotkey::parse(a.combo(cfg)), Hotkey::parse(b.combo(cfg))), (Ok(x), Ok(y)) if x == y) {
                out.push((*a, *b));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn the_default_hotkeys_are_all_different() {
        let c = Config::default();
        assert!(clashes(&c).is_empty(), "{:?}", clashes(&c));
        let all: Vec<String> = Slot::ALL.iter().map(|s| s.combo(&c).to_string()).collect();
        assert_eq!(all, ["Alt+M", "Ctrl+Alt+Shift+Q", "Alt+Space", "Ctrl+Alt+T", "Ctrl+Alt+A"]);
    }

    #[test]
    fn one_combination_can_belong_to_one_feature_only() {
        let mut c = Config::default();
        c.ai.enabled = true;
        // Compared as keys, in any spelling.
        assert_eq!(clash(&c, Slot::Ai, "ctrl+alt+t"), Some(Slot::Ocr));
        assert_eq!(clash(&c, Slot::Ocr, " Alt + Space "), Some(Slot::Launcher), "spaces around the parts do not matter");
        assert_eq!(clash(&c, Slot::Ocr, "Alt+Space"), Some(Slot::Launcher));
        assert_eq!(clash(&c, Slot::Launcher, "alt+m"), Some(Slot::Panel));
        assert_eq!(clash(&c, Slot::Panel, "Ctrl+Shift+Alt+Q"), Some(Slot::Quit));
        assert_eq!(clash(&c, Slot::Ocr, "Ctrl+Alt+A"), Some(Slot::Ai));
        // Its own current value is no clash; a free combination and junk are none either.
        assert_eq!(clash(&c, Slot::Ocr, "Ctrl+Alt+T"), None);
        assert_eq!(clash(&c, Slot::Ocr, "Ctrl+Alt+K"), None);
        assert_eq!(clash(&c, Slot::Ocr, "nonsense"), None);
        // A switched-off feature holds nothing.
        c.ai.enabled = false;
        assert_eq!(clash(&c, Slot::Ocr, "Ctrl+Alt+A"), None);
        // Two active slots with the same key are reported once, the earlier one keeps it.
        c.ai.enabled = true;
        c.ai.hotkey = "Alt+Space".into();
        assert_eq!(clashes(&c), [(Slot::Launcher, Slot::Ai)]);
        c.ocr.hotkey = "alt+space".into();
        assert_eq!(clashes(&c), [(Slot::Launcher, Slot::Ocr), (Slot::Launcher, Slot::Ai), (Slot::Ocr, Slot::Ai)]);
        assert_eq!(clash_message(Slot::Launcher), "Sudah dipakai peluncur");
    }

    #[test]
    fn hotkey_parse() {
        assert_eq!(Hotkey::parse("Alt+M"), Ok(Hotkey { modifiers: MOD_ALT, vk: b'M' as u32 }));
        assert_eq!(
            Hotkey::parse("ctrl+alt+shift+q"),
            Ok(Hotkey { modifiers: MOD_CONTROL | MOD_ALT | MOD_SHIFT, vk: b'Q' as u32 })
        );
        assert_eq!(Hotkey::parse("Win+F5").unwrap().vk, 0x74);
        assert_eq!(Hotkey::parse("F12").unwrap().modifiers, 0);
        assert!(Hotkey::parse("M").is_err());
        assert!(Hotkey::parse("Alt+").is_err());
        assert!(Hotkey::parse("Hyper+M").is_err());
        assert!(Hotkey::parse("Alt+Nope").is_err());
        assert!(Hotkey::parse("Alt+F25").is_err());
    }
}
