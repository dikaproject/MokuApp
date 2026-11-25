fn hotkey(s: &String) -> Result<(), String> {
    crate::hotkey::Hotkey::parse(s).map(|_| ())
}

section! {
    /// `[launcher]`: search apps, calculate, add to-dos, start timers from the island.
    pub struct Launcher in "launcher" {
        enabled: bool = true,
        /// Opens the launcher. Syntax as `general.hotkey_toggle` (see `hotkey.rs`).
        hotkey: String = "Alt+Space".into() => hotkey,
        /// Include packaged (Store) apps from `shell:AppsFolder`.
        store_apps: bool = true,
        /// Include recent Shelf files.
        shelf_files: bool = true,
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::hotkey::{Hotkey, MOD_ALT, MOD_CONTROL};

    #[test]
    fn launcher_hotkey_config_parse() {
        let c = Config::default();
        assert!(c.launcher.enabled);
        assert_eq!(Hotkey::parse(&c.launcher.hotkey), Ok(Hotkey { modifiers: MOD_ALT, vk: 0x20 }));
        let (c, w) = Config::parse("[launcher]\nhotkey = \"Ctrl+Alt+K\"\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(Hotkey::parse(&c.launcher.hotkey), Ok(Hotkey { modifiers: MOD_CONTROL | MOD_ALT, vk: b'K' as u32 }));
        // An invalid hotkey falls back to the default with a warning; the rest is kept.
        let (c, w) = Config::parse("[launcher]\nhotkey = \"Space\"\nstore_apps = false\n");
        assert_eq!(w.len(), 1, "{w:?}");
        assert_eq!((c.launcher.hotkey.as_str(), c.launcher.store_apps), ("Alt+Space", false));
        let (c, w) = Config::parse("[launcher]\nhotkey = \"Alt+Nope\"\n");
        assert_eq!((c.launcher.hotkey.as_str(), w.len()), ("Alt+Space", 1));
    }
}
