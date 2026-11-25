//! The context that goes with a question: what is collected, per toggle, and how it is shown
//! on the card ("chips") and in the "Lihat yang dikirim" preview. Pure: the island collects the
//! raw [`Inputs`] and this builds the [`Item`]s; the same items are what is sent.
//!
//! * Always (unless removed for this request): the selected text, the date and time, the time
//!   zone, the app language, and the city **name**.
//! * Opt-in per source (settings, "Memori & Privasi"): the titles of the open to-dos, today's
//!   calendar titles and times, the timer / focus state, the weather summary. For a provider on
//!   this PC (`localhost`) "kirim semua konteks" turns all of them on at once.
//! * Memory: the facts the user approved, unless the memory is paused.
//! * **Never sent, whatever the settings**: clipboard history, notification bodies, OCR / screen
//!   content, OTPs, coding-agent sessions, a precise location. There is no input for them here;
//!   [`NEVER_SENT`] names them and the tests check that no item or key can carry them.
//! * Everything is redacted (`redact`) before it becomes an item, when `ai.redact` is on.

use super::redact;

/// The sources that are never sent (documentation and test list).
pub const NEVER_SENT: [&str; 6] = ["clipboard", "notifications", "ocr", "otp", "agents", "location"];

/// Every chip key there is (what "remove for this request" may name).
pub const KEYS: [&str; 10] = ["selection", "time", "zone", "lang", "city", "todos", "calendar", "timer", "weather", "memory"];

/// Largest selected text sent, in characters.
pub const MAX_SELECTION: usize = 12_000;
const MAX_TODOS: usize = 15;
const MAX_EVENTS: usize = 12;
const MAX_TITLE: usize = 80;
const MAX_FACTS: usize = 40;
const MAX_FACT_CHARS: usize = 2000;

/// What the island collected (raw, before redaction and before the toggles are applied).
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// Local time, as parts: year, month 1-12, day, weekday 0 = Monday, hour, minute.
    pub when: (i32, u32, u32, u32, u32, u32),
    /// `UTC+07:00`.
    pub zone: String,
    pub english: bool,
    /// The configured city NAME (never coordinates).
    pub city: Option<String>,
    pub selection: Option<String>,
    pub todos: Vec<String>,
    pub events: Vec<Event>,
    pub timer: Option<String>,
    pub weather: Option<String>,
    pub facts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub all_day: bool,
    /// `09:00`
    pub start: String,
    pub end: String,
    pub title: String,
}

/// Which opt-in sources are on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Toggles {
    pub todos: bool,
    pub calendar: bool,
    pub timer: bool,
    pub weather: bool,
    pub memory: bool,
    pub redact: bool,
}

impl Toggles {
    /// From the settings. `base_url` decides whether "kirim semua konteks" applies (only for a
    /// provider on this PC).
    pub fn from_config(c: &moku_core::config::Ai) -> Toggles {
        let all = c.send_all_local && moku_core::ai::is_local(&c.base_url);
        Toggles { todos: c.ctx_todos || all, calendar: c.ctx_calendar || all, timer: c.ctx_timer || all, weather: c.ctx_weather || all, memory: !c.memory_paused, redact: c.redact }
    }
}

/// One piece of context: a chip on the card, one entry in the preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Stable key (`selection`, `time`, `zone`, `lang`, `city`, `todos`, `calendar`, `timer`,
    /// `weather`, `memory`): what "remove for this request" refers to.
    pub key: &'static str,
    /// The chip text (Indonesian / English by `Inputs::english`).
    pub label: String,
    /// What is sent (already redacted). For `selection` this is the text itself; for the others a
    /// one-line statement for the system prompt.
    pub text: String,
    /// The facts, for `memory` (the prompt lists them one by one).
    pub list: Vec<String>,
    /// How many pieces redaction hid in this item.
    pub hidden: usize,
    /// The selected text was cut at [`MAX_SELECTION`].
    pub cut: bool,
}

const DAYS_ID: [&str; 7] = ["Senin", "Selasa", "Rabu", "Kamis", "Jumat", "Sabtu", "Minggu"];
const DAYS_EN: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS_ID: [&str; 12] = ["Januari", "Februari", "Maret", "April", "Mei", "Juni", "Juli", "Agustus", "September", "Oktober", "November", "Desember"];
const MONTHS_EN: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

fn pick(en: bool, id: &str, e: &str) -> String {
    if en { e.to_string() } else { id.to_string() }
}

fn one_line(s: &str, max: usize) -> String {
    let c = moku_core::aimem::clean(s);
    if c.chars().count() > max { format!("{}…", c.chars().take(max).collect::<String>()) } else { c }
}

/// Build the items for these inputs and toggles, in a fixed order. Items whose source is off or
/// empty are absent.
pub fn build(i: &Inputs, t: &Toggles) -> Vec<Item> {
    let en = i.english;
    let mut out: Vec<Item> = vec![];
    // Redact a piece of text (when on) and remember how much was hidden.
    let hide = |s: &str| -> (String, usize) {
        if t.redact {
            let r = redact::redact(s);
            (r.text, r.count)
        } else {
            (s.to_string(), 0)
        }
    };
    fn item(key: &'static str, label: String, text: String, hidden: usize) -> Item {
        Item { key, label, text, list: vec![], hidden, cut: false }
    }

    // The selected text.
    if let Some(sel) = i.selection.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let n = sel.chars().count();
        let cut = n > MAX_SELECTION;
        let kept: String = sel.chars().take(MAX_SELECTION).collect();
        let (text, hidden) = hide(&kept);
        let label = if en { format!("Selected text · {n} chars") } else { format!("Teks terpilih · {n} huruf") };
        out.push(Item { key: "selection", label, text, list: vec![], hidden, cut });
    }

    // Time, zone, language, city.
    let (y, mo, d, wd, h, mi) = i.when;
    let (days, months) = if en { (&DAYS_EN, &MONTHS_EN) } else { (&DAYS_ID, &MONTHS_ID) };
    let day = days[(wd as usize).min(6)];
    let month = months[(mo as usize).clamp(1, 12) - 1];
    out.push(item("time", pick(en, "Waktu", "Time"), format!("{}: {day}, {d} {month} {y}, {h:02}:{mi:02}", pick(en, "Sekarang", "Now")), 0));
    if !i.zone.is_empty() {
        out.push(item("zone", pick(en, "Zona waktu", "Time zone"), format!("{}: {}", pick(en, "Zona waktu", "Time zone"), i.zone), 0));
    }
    out.push(item("lang", pick(en, "Bahasa", "Language"), format!("{}: {}", pick(en, "Bahasa aplikasi", "App language"), pick(en, "Indonesia", "English")), 0));
    if let Some(city) = i.city.as_deref().map(|c| one_line(c, 60)).filter(|c| !c.is_empty()) {
        let (text, hidden) = hide(&city);
        out.push(item("city", format!("{} · {text}", pick(en, "Kota", "City")), format!("{}: {text}", pick(en, "Kota", "City")), hidden));
    }

    // Opt-in sources.
    if t.todos && !i.todos.is_empty() {
        let shown: Vec<String> = i.todos.iter().take(MAX_TODOS).map(|x| one_line(x, MAX_TITLE)).filter(|x| !x.is_empty()).collect();
        if !shown.is_empty() {
            let (joined, hidden) = hide(&shown.join("; "));
            out.push(item("todos", format!("{} · {}", pick(en, "To-do", "To-dos"), shown.len()), format!("{}: {joined}", pick(en, "To-do yang belum selesai", "Open to-dos")), hidden));
        }
    }
    if t.calendar && !i.events.is_empty() {
        let parts: Vec<String> = i
            .events
            .iter()
            .take(MAX_EVENTS)
            .map(|e| {
                let when = if e.all_day { pick(en, "sepanjang hari", "all day") } else if e.end.is_empty() || e.end == e.start { e.start.clone() } else { format!("{}–{}", e.start, e.end) };
                format!("{when} {}", one_line(&e.title, MAX_TITLE))
            })
            .collect();
        let (joined, hidden) = hide(&parts.join("; "));
        out.push(item("calendar", format!("{} · {}", pick(en, "Agenda", "Agenda"), parts.len()), format!("{}: {joined}", pick(en, "Agenda hari ini", "Today's agenda")), hidden));
    }
    if t.timer {
        if let Some(s) = i.timer.as_deref().filter(|s| !s.is_empty()) {
            out.push(item("timer", pick(en, "Timer", "Timer"), format!("Timer: {}", one_line(s, 120)), 0));
        }
    }
    if t.weather {
        if let Some(s) = i.weather.as_deref().filter(|s| !s.is_empty()) {
            out.push(item("weather", pick(en, "Cuaca", "Weather"), format!("{}: {}", pick(en, "Cuaca", "Weather"), one_line(s, 120)), 0));
        }
    }

    // Memory: the approved facts.
    if t.memory && !i.facts.is_empty() {
        let mut used = 0usize;
        let mut list: Vec<String> = vec![];
        let mut hidden = 0usize;
        for f in i.facts.iter().take(MAX_FACTS) {
            let n = f.chars().count();
            if used + n > MAX_FACT_CHARS {
                break;
            }
            used += n;
            let (text, h) = hide(f);
            hidden += h;
            list.push(text);
        }
        if !list.is_empty() {
            out.push(Item { key: "memory", label: format!("{} · {}", pick(en, "Memori", "Memory"), list.len()), text: String::new(), list, hidden, cut: false });
        }
    }

    // Belt and braces: nothing that names a forbidden source can pass.
    out.retain(|it| !NEVER_SENT.contains(&it.key));
    out
}

/// The lines of the system prompt's context: every item except the selected text and the
/// memory (which has its own list), minus the keys the user removed for this request.
pub fn system_lines(items: &[Item], removed: &[&str]) -> Vec<String> {
    items.iter().filter(|it| it.key != "selection" && it.key != "memory" && !removed.contains(&it.key)).map(|it| it.text.clone()).collect()
}

/// The facts, unless removed for this request.
pub fn fact_lines(items: &[Item], removed: &[&str]) -> Vec<String> {
    items.iter().filter(|it| it.key == "memory" && !removed.contains(&it.key)).flat_map(|it| it.list.clone()).collect()
}

/// The selected text, unless removed for this request.
pub fn selection<'a>(items: &'a [Item], removed: &[&str]) -> Option<&'a Item> {
    items.iter().find(|it| it.key == "selection" && !removed.contains(&it.key))
}

/// How many pieces redaction hid in the items that are sent.
pub fn hidden_total(items: &[Item], removed: &[&str]) -> usize {
    items.iter().filter(|it| !removed.contains(&it.key)).map(|it| it.hidden).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> Inputs {
        Inputs {
            when: (2026, 11, 25, 2, 14, 5),
            zone: "UTC+07:00".into(),
            english: false,
            city: Some("Jakarta".into()),
            selection: Some("Tolong terjemahkan kalimat ini".into()),
            todos: vec!["Kirim laporan".into(), "Beli kopi".into()],
            events: vec![Event { all_day: false, start: "09:00".into(), end: "10:00".into(), title: "Rapat tim".into() }, Event { all_day: true, start: String::new(), end: String::new(), title: "Libur".into() }],
            timer: Some("Fokus berjalan, sisa 12:30".into()),
            weather: Some("27°, Hujan ringan".into()),
            facts: vec!["Panggil saya Rani".into(), "Saya tinggal di Surabaya".into()],
        }
    }

    fn keys(items: &[Item]) -> Vec<&'static str> {
        items.iter().map(|i| i.key).collect()
    }

    fn on() -> Toggles {
        Toggles { memory: true, redact: true, ..Default::default() }
    }

    #[test]
    fn the_default_is_text_time_zone_language_city_and_memory_only() {
        let items = build(&inputs(), &on());
        assert_eq!(keys(&items), ["selection", "time", "zone", "lang", "city", "memory"]);
        let time = items.iter().find(|i| i.key == "time").unwrap();
        assert_eq!(time.text, "Sekarang: Rabu, 25 November 2026, 14:05");
        assert_eq!(items.iter().find(|i| i.key == "zone").unwrap().text, "Zona waktu: UTC+07:00");
        assert_eq!(items.iter().find(|i| i.key == "city").unwrap().text, "Kota: Jakarta");
        assert_eq!(items.iter().find(|i| i.key == "selection").unwrap().label, "Teks terpilih · 30 huruf");
        // No to-dos, calendar, timer or weather without their toggle.
        let all = items.iter().map(|i| i.text.as_str()).collect::<Vec<_>>().join("|");
        for secret in ["Kirim laporan", "Rapat tim", "Fokus", "Hujan"] {
            assert!(!all.contains(secret), "{secret} must not be there by default");
        }
    }

    #[test]
    fn each_opt_in_toggle_adds_only_its_own_source() {
        let i = inputs();
        for (t, key, expect) in [
            (Toggles { todos: true, ..on() }, "todos", "To-do yang belum selesai: Kirim laporan; Beli kopi"),
            (Toggles { calendar: true, ..on() }, "calendar", "Agenda hari ini: 09:00–10:00 Rapat tim; sepanjang hari Libur"),
            (Toggles { timer: true, ..on() }, "timer", "Timer: Fokus berjalan, sisa 12:30"),
            (Toggles { weather: true, ..on() }, "weather", "Cuaca: 27°, Hujan ringan"),
        ] {
            let items = build(&i, &t);
            let extra: Vec<&str> = keys(&items).into_iter().filter(|k| !["selection", "time", "zone", "lang", "city", "memory"].contains(k)).collect();
            assert_eq!(extra, [key], "{key}");
            assert_eq!(items.iter().find(|x| x.key == key).unwrap().text, expect);
        }
        let everything = build(&i, &Toggles { todos: true, calendar: true, timer: true, weather: true, ..on() });
        assert_eq!(keys(&everything), ["selection", "time", "zone", "lang", "city", "todos", "calendar", "timer", "weather", "memory"]);
    }

    #[test]
    fn localhost_can_send_everything_but_a_remote_provider_cannot() {
        let mut c = moku_core::config::Config::default().ai;
        c.send_all_local = true;
        c.base_url = "http://localhost:11434/v1".into();
        let t = Toggles::from_config(&c);
        assert!(t.todos && t.calendar && t.timer && t.weather, "localhost + send all");
        // The same setting means nothing for a remote provider.
        c.base_url = "https://api.openai.com/v1".into();
        let t = Toggles::from_config(&c);
        assert!(!t.todos && !t.calendar && !t.timer && !t.weather);
        // And the per-source toggles still work for a remote provider.
        c.ctx_todos = true;
        assert!(Toggles::from_config(&c).todos);
        // A trick: a host that merely contains localhost is remote.
        c.send_all_local = true;
        c.ctx_todos = false;
        c.base_url = "https://localhost.evil.example/v1".into();
        assert!(!Toggles::from_config(&c).todos);
        // Pausing the memory switches the source off.
        c.memory_paused = true;
        assert!(!Toggles::from_config(&c).memory);
    }

    #[test]
    fn a_paused_memory_and_empty_sources_add_nothing() {
        let i = inputs();
        let items = build(&i, &Toggles { memory: false, ..on() });
        assert!(!keys(&items).contains(&"memory"));
        let mut empty = inputs();
        empty.todos.clear();
        empty.events.clear();
        empty.timer = None;
        empty.weather = None;
        empty.facts.clear();
        empty.city = None;
        empty.selection = None;
        let items = build(&empty, &Toggles { todos: true, calendar: true, timer: true, weather: true, ..on() });
        assert_eq!(keys(&items), ["time", "zone", "lang"]);
    }

    #[test]
    fn nothing_that_must_never_be_sent_can_appear() {
        let everything = build(&inputs(), &Toggles { todos: true, calendar: true, timer: true, weather: true, ..on() });
        for it in &everything {
            assert!(!NEVER_SENT.contains(&it.key), "{}", it.key);
        }
        // Inputs has no field for clipboard, notifications, OCR, agents or coordinates, and the
        // text of the items never carries a coordinate-like pair.
        let text = everything.iter().map(|i| format!("{} {}", i.text, i.list.join(" "))).collect::<Vec<_>>().join("\n");
        assert!(!text.contains("latitude") && !text.contains("longitude"));
        // Even a forged item with a forbidden key is dropped by build's own filter.
        let mut forged = everything.clone();
        forged.push(Item { key: "clipboard", label: "x".into(), text: "secret".into(), list: vec![], hidden: 0, cut: false });
        forged.retain(|it| !NEVER_SENT.contains(&it.key));
        assert!(!forged.iter().any(|i| i.key == "clipboard"));
        assert_eq!(NEVER_SENT, ["clipboard", "notifications", "ocr", "otp", "agents", "location"]);
    }

    #[test]
    fn everything_is_redacted_when_on_and_untouched_when_off() {
        let mut i = inputs();
        i.selection = Some("Kode OTP 482913 dan kartu 4111 1111 1111 1111, kunci sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789".into());
        i.todos = vec!["Transfer ke 4111111111111111".into()];
        i.facts = vec!["PIN bank saya 654321".into()];
        i.city = Some("Jakarta".into());
        let items = build(&i, &Toggles { todos: true, ..on() });
        let sel = items.iter().find(|x| x.key == "selection").unwrap();
        assert!(sel.text.contains("[disamarkan: kode OTP]") && sel.text.contains("[disamarkan: nomor kartu]") && sel.text.contains("[disamarkan: token]"), "{}", sel.text);
        assert!(!sel.text.contains("482913") && !sel.text.contains("4111") && !sel.text.contains("AbCdEf"));
        assert_eq!(sel.hidden, 3);
        assert!(items.iter().find(|x| x.key == "todos").unwrap().text.contains("[disamarkan: nomor kartu]"));
        let mem = items.iter().find(|x| x.key == "memory").unwrap();
        assert_eq!(mem.list, ["PIN bank saya [disamarkan: kode OTP]"]);
        assert_eq!(hidden_total(&items, &[]), 3 + 1 + 1);
        assert_eq!(hidden_total(&items, &["selection"]), 2, "a removed chip is not counted");
        // Off: sent as is (the user's choice).
        let raw = build(&i, &Toggles { redact: false, ..on() });
        assert!(raw.iter().find(|x| x.key == "selection").unwrap().text.contains("482913"));
        assert_eq!(hidden_total(&raw, &[]), 0);
    }

    #[test]
    fn the_selection_is_cut_at_the_limit_and_long_lists_are_capped() {
        let mut i = inputs();
        i.selection = Some("kata ".repeat((MAX_SELECTION + 500) / 5));
        i.todos = (0..40).map(|n| format!("tugas nomor {n}")).collect();
        i.facts = (0..100).map(|n| format!("fakta {n} {}", "x".repeat(100))).collect();
        let items = build(&i, &Toggles { todos: true, ..on() });
        let sel = items.iter().find(|x| x.key == "selection").unwrap();
        assert!(sel.cut);
        assert_eq!(sel.text.chars().count(), MAX_SELECTION);
        assert!(sel.label.contains(&format!("{}", ((MAX_SELECTION + 500) / 5) * 5 - 1)), "the chip shows the real size: {}", sel.label);
        assert_eq!(items.iter().find(|x| x.key == "todos").unwrap().label, "To-do · 15");
        let mem = items.iter().find(|x| x.key == "memory").unwrap();
        assert!(mem.list.iter().map(|f| f.chars().count()).sum::<usize>() <= MAX_FACT_CHARS);
        assert!(mem.list.len() < 100);
        // Long titles are cut on one line.
        let mut j = inputs();
        j.todos = vec![format!("judul\npanjang {}", "z".repeat(300))];
        let t = build(&j, &Toggles { todos: true, ..on() });
        let todo = t.iter().find(|x| x.key == "todos").unwrap();
        assert!(!todo.text.contains('\n') && todo.text.contains('…'));
    }

    #[test]
    fn removing_chips_for_one_request() {
        let items = build(&inputs(), &Toggles { todos: true, ..on() });
        let lines = system_lines(&items, &[]);
        assert_eq!(lines.len(), 5, "time, zone, lang, city, todos: {lines:?}");
        let lines = system_lines(&items, &["todos", "city"]);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| !l.contains("Jakarta") && !l.contains("laporan")));
        assert_eq!(fact_lines(&items, &[]), ["Panggil saya Rani", "Saya tinggal di Surabaya"]);
        assert!(fact_lines(&items, &["memory"]).is_empty());
        assert!(selection(&items, &[]).is_some());
        assert!(selection(&items, &["selection"]).is_none());
        assert!(!system_lines(&items, &[]).iter().any(|l| l.contains("terjemahkan")), "the selected text is not part of the system prompt");
    }

    #[test]
    fn english_labels_and_names() {
        let mut i = inputs();
        i.english = true;
        let items = build(&i, &Toggles { todos: true, calendar: true, ..on() });
        let by = |k: &str| items.iter().find(|x| x.key == k).unwrap();
        assert_eq!(by("time").text, "Now: Wednesday, 25 November 2026, 14:05");
        assert_eq!(by("lang").text, "App language: English");
        assert_eq!(by("selection").label, "Selected text · 30 chars");
        assert_eq!(by("todos").text, "Open to-dos: Kirim laporan; Beli kopi");
        assert!(by("calendar").text.contains("all day Libur"));
    }
}
