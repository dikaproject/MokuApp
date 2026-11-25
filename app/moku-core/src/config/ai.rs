fn hotkey(s: &String) -> Result<(), String> {
    crate::hotkey::Hotkey::parse(s).map(|_| ())
}

fn base_url(s: &String) -> Result<(), String> {
    crate::ai::parse_base_url(s).map(|_| ())
}

fn model(s: &String) -> Result<(), String> {
    if s.trim().is_empty() || s.len() > 200 || s.chars().any(|c| c.is_control()) { Err("is not a model name".into()) } else { Ok(()) }
}

fn retention(d: &u32) -> Result<(), String> {
    if matches!(*d, 7 | 30) { Ok(()) } else { Err(format!("{d} is not 7 or 30")) }
}

section! {
    /// `[ai]` "Tanya AI": ask an OpenAI-compatible provider about the selected text. Off until the
    /// user turns it on; the API key is in Windows Credential Manager (`secrets`), never here.
    pub struct Ai in "ai" {
        enabled: bool = false,
        /// Asks about the selected text. Syntax as `general.hotkey_toggle`.
        hotkey: String = "Ctrl+Alt+A".into() => hotkey,
        /// A preset id (`ai::PRESETS`) or `custom`; only the settings page uses it.
        preset: String = "openai".into(),
        /// The provider's base URL; `/chat/completions` is added. HTTPS, or http for localhost only.
        base_url: String = "https://api.openai.com/v1".into() => base_url,
        model: String = "gpt-4o-mini".into() => model,
        /// When the selected text cannot be read with UI Automation, copy it with Ctrl+C
        /// (only on the user's own hotkey press; the clipboard is saved and put back).
        copy_fallback: bool = true,
        /// Opt-in context: the titles of the open to-dos.
        ctx_todos: bool = false,
        /// Opt-in context: today's calendar titles and times.
        ctx_calendar: bool = false,
        /// Opt-in context: the timer / focus state.
        ctx_timer: bool = false,
        /// Opt-in context: the weather summary.
        ctx_weather: bool = false,
        /// For a localhost provider: send every opt-in source without asking per source.
        send_all_local: bool = false,
        /// Hide OTPs, 16-digit numbers and token-like strings before anything is sent.
        redact: bool = true,
        /// "Jeda memori": the memory is neither read into prompts nor written to.
        memory_paused: bool = false,
        /// Keep the questions and answers (encrypted, on this PC) for `retention_days`.
        history: bool = false,
        /// 7 or 30.
        retention_days: u32 = 7 => retention,
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    #[test]
    fn defaults_are_private_and_off() {
        let c = Config::default().ai;
        assert!(!c.enabled, "off until the user turns it on");
        assert!(!c.ctx_todos && !c.ctx_calendar && !c.ctx_timer && !c.ctx_weather && !c.send_all_local && !c.history);
        assert!(c.redact && c.copy_fallback && !c.memory_paused);
        assert_eq!((c.hotkey.as_str(), c.retention_days), ("Ctrl+Alt+A", 7));
        assert_eq!(c.base_url, crate::ai::PRESETS[0].base_url);
        assert_eq!(c.model, crate::ai::PRESETS[0].model);
    }

    #[test]
    fn bad_values_fall_back_to_defaults_with_a_warning() {
        let (c, w) = Config::parse("[ai]\nbase_url = \"http://api.openai.com/v1\"\nhotkey = \"Space\"\nmodel = \"\"\nretention_days = 14\nenabled = true\n");
        assert_eq!(w.len(), 4, "{w:?}");
        assert_eq!(c.ai.base_url, "https://api.openai.com/v1", "a key must not go over plain http to a remote host");
        assert_eq!((c.ai.hotkey.as_str(), c.ai.model.as_str(), c.ai.retention_days), ("Ctrl+Alt+A", "gpt-4o-mini", 7));
        assert!(c.ai.enabled, "valid neighbours are kept");
        let (c, w) = Config::parse("[ai]\nbase_url = \"http://localhost:11434/v1\"\nmodel = \"llama3.2\"\nretention_days = 30\nhistory = true\n");
        assert!(w.is_empty(), "{w:?}");
        assert_eq!((c.ai.base_url.as_str(), c.ai.retention_days, c.ai.history), ("http://localhost:11434/v1", 30, true));
    }

    #[test]
    fn no_secret_lives_in_the_config() {
        let t = Config::default().to_toml();
        let ai = t.split("[ai]").nth(1).unwrap_or("").split("\n[").next().unwrap_or("");
        for line in ai.lines().filter(|l| l.contains('=')) {
            let name = line.split('=').next().unwrap_or("").trim().to_lowercase();
            assert!(name != "key" && !["api_key", "apikey", "token", "secret", "password"].iter().any(|b| name.contains(b)), "[ai] has a secret-looking field: {name}");
        }
    }
}
