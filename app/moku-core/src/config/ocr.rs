fn hotkey(s: &String) -> Result<(), String> {
    crate::hotkey::Hotkey::parse(s).map(|_| ())
}

/// Longest OCR language tag accepted (`id-ID`, `en-US`, `zh-Hans-CN`).
fn language(s: &String) -> Result<(), String> {
    let ok = s.is_empty() || (s.len() <= 16 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    if ok { Ok(()) } else { Err(format!("{s:?} is not a language tag")) }
}

section! {
    /// `[ocr]`: copy the text of a region of the screen (v0.3, track L).
    pub struct Ocr in "ocr" {
        enabled: bool = true,
        /// Starts a capture. Syntax as `general.hotkey_toggle` (see `hotkey.rs`). Windows keeps
        /// `Win+Shift+T` for itself, so the default is `Ctrl+Alt+T`; when another app owns it, the
        /// island shows the "Pintasan ... dipakai aplikasi lain" pill.
        hotkey: String = "Ctrl+Alt+T".into() => hotkey,
        /// Preferred recognition language as a BCP-47 tag (`id-ID`, `en-US`); it is used when that
        /// OCR language is installed. Empty: Indonesian, then English, then the user's profile languages.
        language: String = String::new() => language,
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::hotkey::{Hotkey, MOD_ALT, MOD_CONTROL};

    #[test]
    fn ocr_defaults_and_parsing() {
        let c = Config::default();
        assert!(c.ocr.enabled);
        assert_eq!(Hotkey::parse(&c.ocr.hotkey), Ok(Hotkey { modifiers: MOD_CONTROL | MOD_ALT, vk: b'T' as u32 }));
        assert_eq!(c.ocr.language, "");
        let (c, w) = Config::parse("[ocr]\nenabled = false\nhotkey = \"Ctrl+Alt+O\"\nlanguage = \"en-US\"\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!((c.ocr.enabled, c.ocr.hotkey.as_str(), c.ocr.language.as_str()), (false, "Ctrl+Alt+O", "en-US"));
        // Bad values fall back to their defaults, one warning each; the rest is kept.
        let (c, w) = Config::parse("[ocr]\nhotkey = \"T\"\nlanguage = \"not a tag!\"\nenabled = false\n");
        assert_eq!(w.len(), 2, "{w:?}");
        assert_eq!((c.ocr.enabled, c.ocr.hotkey.as_str(), c.ocr.language.as_str()), (false, "Ctrl+Alt+T", ""));
    }

    #[test]
    fn smart_clipboard_defaults() {
        let c = Config::default();
        assert!(c.clipboard.smart_actions && c.clipboard.smart_pill && c.clipboard.otp_guard);
        let (c, w) = Config::parse("[clipboard]\nsmart_pill = false\notp_guard = false\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!((c.clipboard.smart_actions, c.clipboard.smart_pill, c.clipboard.otp_guard), (true, false, false));
    }
}
