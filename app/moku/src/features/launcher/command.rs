//! What a launcher query asks for: a search, a sum, `todo <text>`, `timer <duration>`, or
//! `> <settings page>`. Pure, unit-tested.

use super::calc::{self, CalcError};

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Nothing typed: recent apps and Shelf files.
    Empty,
    Search(String),
    Calc(Result<f64, CalcError>),
    /// `todo <text>` (empty text: show the hint).
    Todo(String),
    /// `timer <duration>`: seconds, `None` while it cannot be read yet.
    Timer(Option<u64>),
    /// `ocr` (the whole query): copy the text of a region of the screen.
    Ocr,
    /// `> <words>`: settings pages ("pengaturan" / "settings" in front is ignored).
    Settings(String),
    /// `? <question>`: ask the AI (Tanya AI); empty = show the hint.
    Ask(String),
}

/// Longest timer the launcher starts (the timer feature's own cap is the same order).
pub const MAX_TIMER_SECS: u64 = 24 * 3600;

pub fn parse(query: &str) -> Command {
    let q = query.trim();
    if q.is_empty() {
        return Command::Empty;
    }
    if let Some(rest) = q.strip_prefix('>') {
        let rest = rest.trim();
        let lower = rest.to_lowercase();
        let rest = ["pengaturan", "settings", "setelan"].iter().find_map(|w| lower.strip_prefix(w).filter(|r| r.is_empty() || r.starts_with(' ')).map(|r| r.trim().to_string()));
        return Command::Settings(rest.unwrap_or(lower));
    }
    if let Some(rest) = q.strip_prefix('?') {
        return Command::Ask(rest.trim().to_string());
    }
    let (head, rest) = match q.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r.trim()),
        None => (q, ""),
    };
    match head.to_lowercase().as_str() {
        "todo" | "tugas" => return Command::Todo(rest.to_string()),
        "timer" => return Command::Timer(if rest.is_empty() { None } else { parse_duration(rest) }),
        // Only the word on its own: "ocr reader" or "ocr layar" is a search ("OCR Reader" is an app).
        "ocr" if rest.is_empty() => return Command::Ocr,
        _ => {}
    }
    if calc::looks_like_calc(q) {
        return Command::Calc(calc::eval(q));
    }
    Command::Search(q.to_string())
}

/// `10m`, `90s`, `1h30m`, `1j 15mnt`, `25 menit`, `1,5m`, a bare number = minutes. 1 s … 24 h.
pub fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase().replace(',', ".");
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut total = 0.0f64;
    let mut parts = 0;
    while i < cs.len() {
        while i < cs.len() && cs[i].is_whitespace() {
            i += 1;
        }
        if i >= cs.len() {
            break;
        }
        let start = i;
        while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
            i += 1;
        }
        let num: f64 = cs[start..i].iter().collect::<String>().parse().ok()?;
        while i < cs.len() && cs[i].is_whitespace() {
            i += 1;
        }
        let ustart = i;
        while i < cs.len() && cs[i].is_alphabetic() {
            i += 1;
        }
        let unit: String = cs[ustart..i].iter().collect();
        let k = match unit.as_str() {
            "" | "m" | "min" | "mins" | "mnt" | "menit" | "minute" | "minutes" => 60.0,
            "s" | "d" | "det" | "dtk" | "detik" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "h" | "j" | "jam" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
            _ => return None,
        };
        total += num * k;
        parts += 1;
    }
    let secs = total.round();
    (parts > 0 && secs >= 1.0 && secs <= MAX_TIMER_SECS as f64).then_some(secs as u64)
}

/// A settings page of `moku-settings` (`--section <key>`).
pub struct Page {
    pub key: &'static str,
    pub title: (&'static str, &'static str),
}

/// The settings app's pages, in its sidebar order.
pub const PAGES: [Page; 12] = [
    Page { key: "umum", title: ("Umum", "General") },
    Page { key: "island", title: ("Island", "Island") },
    Page { key: "tampilan", title: ("Tampilan", "Appearance") },
    Page { key: "fitur", title: ("Fitur", "Features") },
    Page { key: "notifikasi", title: ("Notifikasi", "Notifications") },
    Page { key: "mokulink", title: ("Moku Link", "Moku Link") },
    Page { key: "pintasan", title: ("Pintasan", "Shortcuts") },
    Page { key: "plugin", title: ("Plugin", "Plugins") },
    Page { key: "privasi", title: ("Privasi", "Privacy") },
    Page { key: "ai", title: ("Tanya AI", "Ask AI") },
    Page { key: "memori", title: ("Memori & Privasi", "Memory & Privacy") },
    Page { key: "tentang", title: ("Tentang", "About") },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands() {
        assert_eq!(parse("  "), Command::Empty);
        assert_eq!(parse("vsc"), Command::Search("vsc".into()));
        assert_eq!(parse("todo beli kopi"), Command::Todo("beli kopi".into()));
        assert_eq!(parse("TODO   Beli Kopi "), Command::Todo("Beli Kopi".into()));
        assert_eq!(parse("todo"), Command::Todo(String::new()));
        assert_eq!(parse("todoist"), Command::Search("todoist".into()), "a word that only starts with todo");
        assert_eq!(parse("timer 10m"), Command::Timer(Some(600)));
        assert_eq!(parse("timer"), Command::Timer(None));
        assert_eq!(parse("timer abc"), Command::Timer(None));
        assert_eq!(parse("2+2"), Command::Calc(Ok(4.0)));
        assert_eq!(parse("=1/0"), Command::Calc(Err(CalcError::DivByZero)));
        assert_eq!(parse("> pengaturan island"), Command::Settings("island".into()));
        assert_eq!(parse(">tampilan"), Command::Settings("tampilan".into()));
        assert_eq!(parse("> settings"), Command::Settings(String::new()));
        assert_eq!(parse("> pengaturanx"), Command::Settings("pengaturanx".into()));
        assert_eq!(parse("2024"), Command::Search("2024".into()));
        assert_eq!(parse("? apa itu rust"), Command::Ask("apa itu rust".into()));
        assert_eq!(parse("?apa itu rust  "), Command::Ask("apa itu rust".into()));
        assert_eq!(parse("?"), Command::Ask(String::new()));
        assert_eq!(parse("what?"), Command::Search("what?".into()), "only a leading question mark");
    }

    /// The settings app's `SECTIONS` (its sidebar order) and these pages must stay the same list: the launcher
    /// opens a page by key, and `moku-settings --section <key>` finds it in its own list.
    #[test]
    fn the_pages_are_in_the_sidebar_order_of_the_settings_app() {
        let keys: Vec<&str> = PAGES.iter().map(|p| p.key).collect();
        assert_eq!(keys, ["umum", "island", "tampilan", "fitur", "notifikasi", "mokulink", "pintasan", "plugin", "privasi", "ai", "memori", "tentang"]);
        // `moku-settings/src/main.rs` carries the same list (SECTIONS); read it so the two can not drift apart.
        let main = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../moku-settings/src/main.rs")).expect("the settings app's main.rs");
        let line = main.lines().find(|l| l.starts_with("const SECTIONS")).expect("SECTIONS");
        let listed: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
        assert_eq!(listed, keys, "moku-settings SECTIONS and the launcher PAGES differ");
        // Tanya AI and Memori & Privasi sit after Privasi, Tentang is last.
        assert_eq!(keys.iter().position(|k| *k == "ai"), keys.iter().position(|k| *k == "privasi").map(|i| i + 1));
        assert_eq!(keys.last(), Some(&"tentang"));
    }

    #[test]
    fn ocr_command() {
        for q in ["ocr", "OCR", "  ocr  ", "Ocr\t"] {
            assert_eq!(parse(q), Command::Ocr, "{q}");
        }
        // Only the whole query: an app called "ocrmypdf" or "OCR Reader" is a search.
        assert_eq!(parse("ocrmypdf"), Command::Search("ocrmypdf".into()));
        assert_eq!(parse("docr"), Command::Search("docr".into()));
        assert_eq!(parse("ocr reader"), Command::Search("ocr reader".into()));
        assert_eq!(parse("OCR Reader Pro"), Command::Search("OCR Reader Pro".into()));
        assert_eq!(parse("ocr layar"), Command::Search("ocr layar".into()));
        assert_eq!(parse("ocr screen"), Command::Search("ocr screen".into()));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("10m"), Some(600));
        assert_eq!(parse_duration("10 m"), Some(600));
        assert_eq!(parse_duration("10"), Some(600), "bare number = minutes");
        assert_eq!(parse_duration("90s"), Some(90));
        assert_eq!(parse_duration("45 detik"), Some(45));
        assert_eq!(parse_duration("1h30m"), Some(5400));
        assert_eq!(parse_duration("1j 15mnt"), Some(4500));
        assert_eq!(parse_duration("1,5m"), Some(90));
        assert_eq!(parse_duration("25 menit"), Some(1500));
        assert_eq!(parse_duration("0s"), None);
        assert_eq!(parse_duration("25h"), None);
        assert_eq!(parse_duration("10 parsec"), None);
        assert_eq!(parse_duration("m"), None);
    }
}
