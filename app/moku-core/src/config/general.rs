use serde::{Deserialize, Serialize};

/// UI language of the island and the settings app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Id,
    En,
}

fn hotkey(s: &String) -> Result<(), String> {
    crate::hotkey::Hotkey::parse(s).map(|_| ())
}

section! {
    /// `[general]`
    pub struct General in "general" {
        language: Language = Language::Id,
        /// Start with Windows (`HKCU\...\Run`, written by the settings app).
        autostart: bool = true,
        /// Open / close the panel. Syntax: `Ctrl+Alt+Shift+Win+<key>` (see `hotkey.rs`).
        hotkey_toggle: String = "Alt+M".into() => hotkey,
        hotkey_quit: String = "Ctrl+Alt+Shift+Q".into() => hotkey,
    }
}
