//! A small iCalendar (RFC 5545) reader for agendas: line unfolding, escaped text, `VEVENT` with
//! `DTSTART` / `DTEND` / `DURATION`, all-day events, `TZID` with the file's `VTIMEZONE` blocks
//! or the built-in zone table, `RRULE` ([`super::rrule`]), `EXDATE`, `RECURRENCE-ID` overrides
//! and `STATUS:CANCELLED`.
//!
//! Everything here is pure (no clock, no OS): the caller passes the window to expand and a
//! function that reads a wall-clock time as local time (the fallback for floating times and for
//! zones nobody knows). Any input, even a broken or hostile file, gives a (possibly empty)
//! [`Calendar`]; nothing in here panics or logs.

use std::collections::HashMap;

use chrono::{Days, NaiveDate, NaiveDateTime, NaiveTime};

use super::links::{self, MeetingLink};
use super::rrule::{self, RRule, Until};
use super::tz::{self, Tz};

/// Only the next two weeks are expanded (plan).
pub const HORIZON_DAYS: i64 = 14;
/// Hard caps, so a huge or hostile calendar cannot use unbounded memory or time.
const MAX_EVENTS: usize = 5000;
const MAX_PER_EVENT: usize = 120;
pub const MAX_OCCURRENCES: usize = 600;
const MAX_TITLE_CHARS: usize = 160;
const MAX_TEXT_BYTES: usize = 16 * 1024;

/// One logical line: `NAME;PARAM=value:value`.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Upper case.
    pub name: String,
    /// Upper-case names, unquoted values.
    pub params: Vec<(String, String)>,
    pub value: String,
}

impl Line {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Undo line folding (a line break followed by a space or tab continues the line). Handles CRLF,
/// bare LF and bare CR, and a leading byte-order mark.
pub fn unfold(text: &str) -> Vec<String> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut out: Vec<String> = Vec::new();
    // A blank line ends a line for good: a fold right after it starts a new line.
    let mut folds_ok = false;
    for raw in text.split(['\n', '\r']) {
        if raw.is_empty() {
            folds_ok = false;
            continue;
        }
        if folds_ok && raw.starts_with([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                // A single folding space is dropped; anything beyond belongs to the text.
                last.push_str(&raw[1..]);
                continue;
            }
        }
        out.push(raw.to_string());
        folds_ok = true;
    }
    out
}

/// Split one unfolded line. `None` when it has no `:` (not a content line).
pub fn parse_line(raw: &str) -> Option<Line> {
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i] != b':' && b[i] != b';' {
        i += 1;
    }
    let name = raw[..i].trim().to_ascii_uppercase();
    if name.is_empty() || i >= b.len() {
        return None;
    }
    let mut params = vec![];
    while i < b.len() && b[i] == b';' {
        i += 1;
        let ks = i;
        while i < b.len() && b[i] != b'=' && b[i] != b';' && b[i] != b':' {
            i += 1;
        }
        let key = raw[ks..i].trim().to_ascii_uppercase();
        let mut val = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            if i < b.len() && b[i] == b'"' {
                i += 1;
                let vs = i;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                val = raw[vs..i].to_string();
                i = (i + 1).min(b.len());
            } else {
                let vs = i;
                while i < b.len() && b[i] != b';' && b[i] != b':' {
                    i += 1;
                }
                val = raw[vs..i].to_string();
            }
        }
        if !key.is_empty() {
            params.push((key, val));
        }
    }
    if i >= b.len() || b[i] != b':' {
        return None;
    }
    Some(Line { name, params, value: raw[i + 1..].to_string() })
}

/// Undo TEXT escaping: `\n` / `\N` (newline), `\,` `\;` `\\`.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

/// `20261125`.
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    NaiveDate::from_ymd_opt(s[0..4].parse().ok()?, s[4..6].parse().ok()?, s[6..8].parse().ok()?)
}

/// `20261125T090000` (a trailing `Z` is ignored; seconds are optional; a bare date is midnight).
pub fn parse_naive(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim().trim_end_matches(['Z', 'z']);
    let Some((d, t)) = s.split_once(['T', 't']) else { return parse_date(s).map(|d| d.and_time(NaiveTime::MIN)) };
    if !(t.len() == 4 || t.len() == 6) || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let sec = if t.len() == 6 { t[4..6].parse().ok()? } else { 0 };
    // Leap second 60 → 59.
    let time = NaiveTime::from_hms_opt(t[0..2].parse().ok()?, t[2..4].parse().ok()?, sec.min(59))?;
    Some(parse_date(d)?.and_time(time))
}

/// A date or date-time as written in the file.
#[derive(Clone, Debug, PartialEq)]
pub enum When {
    /// `VALUE=DATE`: an all-day event.
    Date(NaiveDate),
    /// No zone: read as local time.
    Floating(NaiveDateTime),
    /// `…Z`.
    Utc(NaiveDateTime),
    Zoned(NaiveDateTime, String),
}

impl When {
    fn naive(&self) -> NaiveDateTime {
        match self {
            When::Date(d) => d.and_time(NaiveTime::MIN),
            When::Floating(t) | When::Utc(t) | When::Zoned(t, _) => *t,
        }
    }
}

fn parse_when(value: &str, params: &[(String, String)]) -> Option<When> {
    let v = value.trim();
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    if get("VALUE").is_some_and(|x| x.eq_ignore_ascii_case("DATE")) || (v.len() == 8 && !v.contains(['T', 't'])) {
        return parse_date(v).map(When::Date);
    }
    let naive = parse_naive(v)?;
    if v.ends_with(['Z', 'z']) {
        return Some(When::Utc(naive));
    }
    match get("TZID").map(str::trim).filter(|t| !t.is_empty()) {
        Some(id) => Some(When::Zoned(naive, id.to_string())),
        None => Some(When::Floating(naive)),
    }
}

/// `PT1H30M`, `P1D`, `P1W`, `-PT15M` → seconds.
fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix('P')?;
    let (mut total, mut num, mut in_time) = (0i64, String::new(), false);
    for c in s.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            'W' | 'D' | 'H' | 'M' | 'S' => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total = total.checked_add(n.checked_mul(match (c, in_time) {
                    ('W', _) => 7 * 86_400,
                    ('D', _) => 86_400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                })?)?;
            }
            _ => return None,
        }
    }
    num.is_empty().then_some(if neg { -total } else { total })
}

#[derive(Clone, Debug)]
struct Event {
    uid: String,
    title: String,
    location: String,
    description: String,
    url: String,
    conference: String,
    start: When,
    end: Option<When>,
    duration: Option<i64>,
    rrule: Option<RRule>,
    exdates: Vec<When>,
    recurrence_id: Option<When>,
    cancelled: bool,
}

/// A parsed calendar: its events (not yet expanded) and the zones it defines.
#[derive(Default, Debug)]
pub struct Calendar {
    events: Vec<Event>,
    zones: HashMap<String, Tz>,
}

impl Calendar {
    /// A zone defined by a `VTIMEZONE` block of this file (exact id, else ignoring case).
    pub fn zone(&self, id: &str) -> Option<&Tz> {
        self.zones.get(id).or_else(|| self.zones.iter().find(|(k, _)| k.eq_ignore_ascii_case(id)).map(|(_, v)| v))
    }

}

fn clip(mut s: String, max_bytes: usize) -> String {
    if s.len() > max_bytes {
        let cut = s.floor_char_boundary(max_bytes);
        s.truncate(cut);
    }
    s
}

fn build_event(lines: &[Line]) -> Option<Event> {
    let first = |name: &str| lines.iter().find(|l| l.name == name);
    let start_line = first("DTSTART")?;
    let start = parse_when(&start_line.value, &start_line.params)?;
    let text = |name: &str, max: usize| first(name).map(|l| clip(unescape(&l.value), max)).unwrap_or_default();
    let title: String = text("SUMMARY", 1024).split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TITLE_CHARS).collect();
    let mut exdates = vec![];
    for l in lines.iter().filter(|l| l.name == "EXDATE") {
        exdates.extend(l.value.split(',').filter_map(|v| parse_when(v, &l.params)));
        exdates.truncate(2000);
    }
    Some(Event {
        uid: clip(first("UID").map(|l| l.value.trim().to_string()).unwrap_or_default(), 512),
        title,
        location: text("LOCATION", 2048),
        description: text("DESCRIPTION", MAX_TEXT_BYTES),
        url: clip(first("URL").map(|l| l.value.trim().to_string()).unwrap_or_default(), 2048),
        conference: clip(first("X-GOOGLE-CONFERENCE").map(|l| l.value.trim().to_string()).unwrap_or_default(), 2048),
        start,
        end: first("DTEND").and_then(|l| parse_when(&l.value, &l.params)),
        duration: first("DURATION").and_then(|l| parse_duration(&l.value)),
        rrule: first("RRULE").and_then(|l| rrule::parse(&l.value)),
        exdates,
        recurrence_id: first("RECURRENCE-ID").and_then(|l| parse_when(&l.value, &l.params)),
        cancelled: first("STATUS").is_some_and(|l| l.value.trim().eq_ignore_ascii_case("CANCELLED")),
    })
}

/// Parse calendar text. Unknown or broken parts are skipped; the result may be empty.
pub fn parse(text: &str) -> Calendar {
    enum Cur {
        Nothing,
        Event { lines: Vec<Line>, depth: u32 },
        Zone(Vec<Line>),
    }
    let mut cal = Calendar::default();
    let mut cur = Cur::Nothing;
    for raw in unfold(text) {
        let Some(l) = parse_line(&raw) else { continue };
        let is = |n: &str| l.value.trim().eq_ignore_ascii_case(n);
        cur = match cur {
            Cur::Nothing => match (l.name.as_str(), is("VEVENT"), is("VTIMEZONE")) {
                ("BEGIN", true, _) => Cur::Event { lines: vec![], depth: 0 },
                ("BEGIN", _, true) => Cur::Zone(vec![]),
                _ => Cur::Nothing,
            },
            Cur::Event { mut lines, depth } => match l.name.as_str() {
                // A new event while one is open: the file lost an END:VEVENT. Keep what we have.
                "BEGIN" if is("VEVENT") => {
                    if cal.events.len() < MAX_EVENTS {
                        cal.events.extend(build_event(&lines));
                    }
                    Cur::Event { lines: vec![], depth: 0 }
                }
                "BEGIN" => Cur::Event { lines, depth: depth + 1 },
                "END" if depth > 0 => Cur::Event { lines, depth: depth - 1 },
                "END" => {
                    if cal.events.len() < MAX_EVENTS {
                        cal.events.extend(build_event(&lines));
                    }
                    Cur::Nothing
                }
                _ => {
                    if depth == 0 {
                        lines.push(l);
                    }
                    Cur::Event { lines, depth }
                }
            },
            Cur::Zone(mut lines) => {
                if l.name == "END" && is("VTIMEZONE") {
                    if let Some(id) = lines.iter().find(|x| x.name == "TZID").map(|x| x.value.trim().to_string()) {
                        if cal.zones.len() < 64 {
                            if let Some(z) = Tz::from_vtimezone(&lines) {
                                cal.zones.insert(id, z);
                            }
                        }
                    }
                    Cur::Nothing
                } else {
                    if lines.len() < 400 {
                        lines.push(l);
                    }
                    Cur::Zone(lines)
                }
            }
        };
    }
    cal
}

/// One instance of an event inside the window.
#[derive(Clone, Debug, PartialEq)]
pub struct Occurrence {
    /// Stable id of this instance (hash of the event's UID and its start).
    pub key: u64,
    pub title: String,
    /// Unix seconds (UTC). For all-day events: local midnight of the first day …
    pub start: i64,
    /// … and local midnight after the last day.
    pub end: i64,
    pub all_day: bool,
    pub link: Option<MeetingLink>,
}

/// FNV-1a: no dependency, stable across runs.
fn hash(uid: &str, start: i64) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in uid.bytes().chain(start.to_le_bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// How a wall-clock time of one event becomes UTC.
enum Zone<'a> {
    Cal(&'a Tz),
    Own(Tz),
    Local,
}

struct Ctx<'a> {
    cal: &'a Calendar,
    /// Reads a wall-clock time as local time → Unix seconds.
    local: &'a dyn Fn(NaiveDateTime) -> i64,
}

impl<'a> Ctx<'a> {
    fn zone(&self, w: &When) -> Zone<'a> {
        match w {
            When::Utc(_) => Zone::Own(Tz::fixed(0)),
            When::Zoned(_, id) => match self.cal.zone(id) {
                Some(z) => Zone::Cal(z),
                None => tz::builtin(id).map_or(Zone::Local, Zone::Own),
            },
            When::Date(_) | When::Floating(_) => Zone::Local,
        }
    }

    fn utc(&self, z: &Zone, n: NaiveDateTime) -> i64 {
        match z {
            Zone::Cal(t) => t.local_to_utc(n),
            Zone::Own(t) => t.local_to_utc(n),
            Zone::Local => (self.local)(n),
        }
    }

    /// Does `w` (an `EXDATE` / `RECURRENCE-ID`) name the instance starting at `naive` / `utc`?
    fn same_instance(&self, w: &When, naive: NaiveDateTime, utc: i64) -> bool {
        match w {
            When::Date(d) => naive.date() == *d,
            When::Floating(n) => naive == *n,
            When::Utc(n) => n.and_utc().timestamp() == utc,
            When::Zoned(n, _) => self.utc(&self.zone(w), *n) == utc,
        }
    }
}

/// Every instance that overlaps `[from, to)` (Unix seconds), sorted by start. Floating times and
/// unknown zones are read through `local`.
pub fn occurrences(cal: &Calendar, from: i64, to: i64, local: &dyn Fn(NaiveDateTime) -> i64) -> Vec<Occurrence> {
    let ctx = Ctx { cal, local };
    // Rescheduled / cancelled single instances of a repeating event.
    let mut overrides: HashMap<&str, Vec<&Event>> = HashMap::new();
    for e in cal.events.iter().filter(|e| e.recurrence_id.is_some() && !e.uid.is_empty()) {
        overrides.entry(e.uid.as_str()).or_default().push(e);
    }
    let mut out: Vec<Occurrence> = vec![];
    for ev in &cal.events {
        if ev.cancelled || out.len() >= MAX_OCCURRENCES * 2 {
            continue;
        }
        expand(&ctx, ev, overrides.get(ev.uid.as_str()).map(|v| v.as_slice()).unwrap_or(&[]), from, to, &mut out);
    }
    out.sort_by(|a, b| (a.start, &a.title, a.key).cmp(&(b.start, &b.title, b.key)));
    out.dedup_by_key(|o| o.key);
    out.truncate(MAX_OCCURRENCES);
    out
}

fn expand(ctx: &Ctx, ev: &Event, overrides: &[&Event], from: i64, to: i64, out: &mut Vec<Occurrence>) {
    let all_day = matches!(ev.start, When::Date(_));
    let zone = ctx.zone(&ev.start);
    let start_naive = ev.start.naive();
    // Length of one instance.
    let days = match (&ev.start, ev.end.as_ref(), ev.duration) {
        (When::Date(s), Some(When::Date(e)), _) => (*e - *s).num_days().max(1),
        (_, _, Some(d)) if all_day => (d / 86_400).max(1),
        _ => 1,
    };
    let secs = match (ev.end.as_ref(), ev.duration) {
        _ if all_day => 0,
        (Some(end), _) => (ctx.utc(&ctx.zone(end), end.naive()) - ctx.utc(&zone, start_naive)).max(0),
        (None, Some(d)) => d.max(0),
        (None, None) => 0,
    };
    let link = links::find(&[&ev.conference, &ev.url, &ev.location, &ev.description]);
    let mut taken = 0usize;
    // Returns false when no later instance can be inside the window.
    let mut visit = |naive: NaiveDateTime| -> bool {
        let (start, end) = if all_day {
            let d = naive.date();
            let after = d.checked_add_days(Days::new(days as u64)).unwrap_or(d);
            ((ctx.local)(d.and_time(NaiveTime::MIN)), (ctx.local)(after.and_time(NaiveTime::MIN)))
        } else {
            let s = ctx.utc(&zone, naive);
            (s, s + secs)
        };
        if start >= to {
            return false;
        }
        let hidden = ev.exdates.iter().any(|x| ctx.same_instance(x, naive, start)) || overrides.iter().any(|o| o.recurrence_id.as_ref().is_some_and(|r| ctx.same_instance(r, naive, start)));
        if !hidden && end.max(start + 1) > from {
            taken += 1;
            out.push(Occurrence { key: hash(&ev.uid, start), title: ev.title.clone(), start, end, all_day, link: link.clone() });
        }
        taken < MAX_PER_EVENT
    };
    match &ev.rrule {
        // An override is a single instance of its own, even when it carries the master's rule.
        Some(rule) if ev.recurrence_id.is_none() => {
            // Wall-clock times of the zone lie within a day of UTC; two days of margin.
            let stop = chrono::DateTime::from_timestamp(to.saturating_add(2 * 86_400), 0).map_or(NaiveDateTime::MAX, |t| t.naive_utc());
            let until_ok = |naive: NaiveDateTime| match &rule.until {
                None => true,
                Some(Until::Date(d)) => naive.date() <= *d,
                Some(Until::Floating(u)) => naive <= *u,
                Some(Until::Utc(u)) => ctx.utc(&zone, naive) <= u.and_utc().timestamp(),
            };
            rrule::each(rule, start_naive, stop, &mut |naive| until_ok(naive) && visit(naive));
        }
        _ => {
            visit(start_naive);
        }
    }
}

/// Convenience for the island: `NaiveDateTime` read as local time (a DST gap reads as the
/// instant after it).
pub fn system_local(n: NaiveDateTime) -> i64 {
    use chrono::TimeZone;
    chrono::Local.from_local_datetime(&n).earliest().or_else(|| chrono::Local.from_local_datetime(&n).latest()).map_or_else(|| n.and_utc().timestamp(), |t| t.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Local time for these tests: UTC+7 (Jakarta), whatever machine runs them.
    fn local7(n: NaiveDateTime) -> i64 {
        n.and_utc().timestamp() - 7 * 3600
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap().and_utc().timestamp()
    }

    /// `ics` event(s) wrapped in a VCALENDAR, expanded over `[from, from + 14 days)`.
    fn expand_14(body: &str, from: i64) -> Vec<Occurrence> {
        let text = format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}END:VCALENDAR\r\n");
        occurrences(&parse(&text), from, from + HORIZON_DAYS * 86_400, &local7)
    }

    fn starts(o: &[Occurrence]) -> Vec<i64> {
        o.iter().map(|o| o.start).collect()
    }

    // ---- lines ----

    #[test]
    fn unfolds_folded_lines_whatever_the_line_ending() {
        let crlf = unfold("SUMMARY:Sprint\r\n  review with\r\n\tthe team\r\nLOCATION:Room 4\r\n");
        assert_eq!(crlf, ["SUMMARY:Sprint review withthe team", "LOCATION:Room 4"]);
        assert_eq!(unfold("A:1\nB:long\n er\n\n C:x"), ["A:1", "B:longer", " C:x"], "a blank line ends the folding");
        assert_eq!(unfold("\u{feff}BEGIN:VCALENDAR\rEND:VCALENDAR"), ["BEGIN:VCALENDAR", "END:VCALENDAR"], "BOM and bare CR");
        assert!(unfold("").is_empty());
        assert_eq!(unfold(" orphan continuation"), [" orphan continuation".to_string()], "a fold with nothing before it is kept as text");
    }

    #[test]
    fn splits_names_parameters_and_values() {
        let l = parse_line("DTSTART;TZID=America/New_York:20261130T090000").unwrap();
        assert_eq!((l.name.as_str(), l.param("TZID"), l.value.as_str()), ("DTSTART", Some("America/New_York"), "20261130T090000"));
        // Quoted parameter values may contain ':' ';' ','.
        let l = parse_line("ATTENDEE;CN=\"Doe, John: Jr; x\";ROLE=CHAIR:mailto:john@example.com").unwrap();
        assert_eq!(l.param("CN"), Some("Doe, John: Jr; x"));
        assert_eq!((l.param("ROLE"), l.value.as_str()), (Some("CHAIR"), "mailto:john@example.com"));
        let l = parse_line("summary:Colon: inside").unwrap();
        assert_eq!((l.name.as_str(), l.value.as_str()), ("SUMMARY", "Colon: inside"));
        assert!(parse_line("no colon here").is_none());
        assert!(parse_line(":value only").is_none());
        assert!(parse_line("X;P=\"unterminated").is_none());
        assert!(parse_line("").is_none());
    }

    #[test]
    fn unescapes_text() {
        assert_eq!(unescape(r"Rapat\, lantai 3\; \\ruang\NBaris dua"), "Rapat, lantai 3; \\ruang\nBaris dua");
        assert_eq!(unescape("trailing\\"), "trailing\\");
    }

    #[test]
    fn dates_and_durations_parse() {
        assert_eq!(parse_naive("20261125T090000"), NaiveDate::from_ymd_opt(2026, 11, 25).unwrap().and_hms_opt(9, 0, 0));
        assert_eq!(parse_naive("20261125T0930Z"), NaiveDate::from_ymd_opt(2026, 11, 25).unwrap().and_hms_opt(9, 30, 0));
        assert_eq!(parse_naive("20261125"), NaiveDate::from_ymd_opt(2026, 11, 25).unwrap().and_hms_opt(0, 0, 0));
        assert_eq!(parse_naive("20261125T235960"), NaiveDate::from_ymd_opt(2026, 11, 25).unwrap().and_hms_opt(23, 59, 59), "leap second");
        for bad in ["", "2026", "20261340T000000", "20261125T250000", "20261125T9", "abcdefgh", "20261125T09000"] {
            assert_eq!(parse_naive(bad), None, "{bad}");
        }
        assert_eq!(parse_duration("PT1H30M"), Some(5400));
        assert_eq!(parse_duration("P1D"), Some(86_400));
        assert_eq!(parse_duration("P1W"), Some(604_800));
        assert_eq!(parse_duration("-PT15M"), Some(-900));
        assert_eq!(parse_duration("P1DT2H"), Some(93_600));
        for bad in ["", "1H", "PTH", "P1X", "PT1H30", "P99999999999999999999D"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
    }

    // ---- events ----

    const SPRINT: &str = "BEGIN:VEVENT\r\nUID:sprint-1\r\nSUMMARY:Sprint review\\, Q4\r\nLOCATION:Ruang 4\r\nDTSTART:20261125T093000Z\r\nDTEND:20261125T104500Z\r\nEND:VEVENT\r\n";

    #[test]
    fn a_plain_event_with_utc_times() {
        let o = expand_14(SPRINT, utc(2026, 11, 24, 0, 0));
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].title, "Sprint review, Q4", "escaped text and folding are undone");
        assert_eq!((o[0].start, o[0].end), (utc(2026, 11, 25, 9, 30), utc(2026, 11, 25, 10, 45)));
        assert!(!o[0].all_day && o[0].link.is_none());
        // Outside the window: nothing, before or after.
        assert!(expand_14(SPRINT, utc(2026, 12, 1, 0, 0)).is_empty());
        assert!(expand_14(SPRINT, utc(2026, 11, 1, 0, 0)).is_empty());
        // An instance that started before the window but is still running is included.
        assert_eq!(expand_14(SPRINT, utc(2026, 11, 25, 10, 0)).len(), 1);
        assert!(expand_14(SPRINT, utc(2026, 11, 25, 10, 45)).is_empty(), "ended exactly then");
    }

    #[test]
    fn folded_summary_lines_in_a_real_event() {
        let body = "BEGIN:VEVENT\r\nUID:x\r\nSUMMARY:Rapat koordinasi tim produk dan\r\n  desain minggu ini\r\nDTSTART:20261125T093000Z\r\nEND:VEVENT\r\n";
        assert_eq!(expand_14(body, utc(2026, 11, 24, 0, 0))[0].title, "Rapat koordinasi tim produk dan desain minggu ini");
    }

    #[test]
    fn all_day_events_cover_whole_local_days() {
        let body = "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Libur\r\nDTSTART;VALUE=DATE:20261125\r\nDTEND;VALUE=DATE:20261126\r\nEND:VEVENT\r\n\
                    BEGIN:VEVENT\r\nUID:b\r\nSUMMARY:Konferensi\r\nDTSTART;VALUE=DATE:20261126\r\nDTEND;VALUE=DATE:20261129\r\nEND:VEVENT\r\n\
                    BEGIN:VEVENT\r\nUID:c\r\nSUMMARY:No end\r\nDTSTART:20261127\r\nEND:VEVENT\r\n";
        let o = expand_14(body, utc(2026, 11, 20, 0, 0));
        assert_eq!(o.len(), 3);
        assert!(o.iter().all(|x| x.all_day));
        // Local (UTC+7) midnight of 25 Nov is 24 Nov 17:00 UTC.
        assert_eq!((o[0].start, o[0].end), (utc(2026, 11, 24, 17, 0), utc(2026, 11, 25, 17, 0)));
        assert_eq!((o[1].start, o[1].end), (utc(2026, 11, 25, 17, 0), utc(2026, 11, 28, 17, 0)), "three days");
        assert_eq!(o[2].end - o[2].start, 86_400, "no DTEND = one day");
    }

    #[test]
    fn tzid_is_converted_with_the_zone_rules() {
        // New York is UTC-5 in November (EST) and UTC-4 in July (EDT).
        let winter = "BEGIN:VEVENT\r\nUID:w\r\nSUMMARY:Stand-up\r\nDTSTART;TZID=America/New_York:20261130T090000\r\nDTEND;TZID=America/New_York:20261130T093000\r\nEND:VEVENT\r\n";
        let o = expand_14(winter, utc(2026, 11, 29, 0, 0));
        assert_eq!((o[0].start, o[0].end), (utc(2026, 11, 30, 14, 0), utc(2026, 11, 30, 14, 30)));
        // … which is 21:00 on the Jakarta wall clock.
        assert_eq!(o[0].start + 7 * 3600, utc(2026, 11, 30, 21, 0));
        let summer = winter.replace("20261130", "20260715");
        assert_eq!(expand_14(&summer, utc(2026, 7, 14, 0, 0))[0].start, utc(2026, 7, 15, 13, 0));
    }

    #[test]
    fn windows_zone_names_work_with_and_without_a_vtimezone_block() {
        let ev = "BEGIN:VEVENT\r\nUID:w\r\nDTSTART;TZID=Eastern Standard Time:20260715T090000\r\nEND:VEVENT\r\n";
        assert_eq!(expand_14(ev, utc(2026, 7, 14, 0, 0))[0].start, utc(2026, 7, 15, 13, 0), "built-in table");
        let ev = "BEGIN:VEVENT\r\nUID:w\r\nDTSTART;TZID=SE Asia Standard Time:20260715T090000\r\nEND:VEVENT\r\n";
        assert_eq!(expand_14(ev, utc(2026, 7, 14, 0, 0))[0].start, utc(2026, 7, 15, 2, 0));
        // The file's own VTIMEZONE wins, and a zone nobody knows works when the file defines it.
        let body = "BEGIN:VTIMEZONE\r\nTZID:Custom/Kathmandu\r\nBEGIN:STANDARD\r\nDTSTART:19860101T000000\r\nTZOFFSETFROM:+0530\r\nTZOFFSETTO:+0545\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
                    BEGIN:VEVENT\r\nUID:k\r\nDTSTART;TZID=Custom/Kathmandu:20261125T100000\r\nEND:VEVENT\r\n";
        assert_eq!(expand_14(body, utc(2026, 11, 24, 0, 0))[0].start, utc(2026, 11, 25, 4, 15));
    }

    #[test]
    fn unknown_zones_and_floating_times_fall_back_to_local_time() {
        let unknown = "BEGIN:VEVENT\r\nUID:u\r\nDTSTART;TZID=Mars/Olympus_Mons:20261125T100000\r\nEND:VEVENT\r\n";
        assert_eq!(expand_14(unknown, utc(2026, 11, 24, 0, 0))[0].start, utc(2026, 11, 25, 3, 0), "10:00 on the UTC+7 wall clock");
        let floating = "BEGIN:VEVENT\r\nUID:f\r\nDTSTART:20261125T100000\r\nEND:VEVENT\r\n";
        assert_eq!(expand_14(floating, utc(2026, 11, 24, 0, 0))[0].start, utc(2026, 11, 25, 3, 0));
        let quoted = "BEGIN:VEVENT\r\nUID:q\r\nDTSTART;TZID=\"Europe/Paris\":20261125T100000\r\nEND:VEVENT\r\n";
        assert_eq!(expand_14(quoted, utc(2026, 11, 24, 0, 0))[0].start, utc(2026, 11, 25, 9, 0));
    }

    #[test]
    fn duration_is_used_when_there_is_no_end() {
        let body = "BEGIN:VEVENT\r\nUID:d\r\nDTSTART:20261125T090000Z\r\nDURATION:PT1H30M\r\nEND:VEVENT\r\n";
        let o = expand_14(body, utc(2026, 11, 24, 0, 0));
        assert_eq!(o[0].end - o[0].start, 5400);
    }

    #[test]
    fn weekly_by_day_with_exdate() {
        // Mondays and Wednesdays 09:00 New York from Mon 2 Nov 2026; Wed 4 Nov is skipped.
        // (Daylight time ended on Sunday 1 Nov, so these are all EST = UTC-5.)
        let body = "BEGIN:VEVENT\r\nUID:wk\r\nSUMMARY:Latihan\r\nDTSTART;TZID=America/New_York:20261102T090000\r\nDTEND;TZID=America/New_York:20261102T100000\r\n\
                    RRULE:FREQ=WEEKLY;BYDAY=MO,WE\r\nEXDATE;TZID=America/New_York:20261104T090000\r\nEND:VEVENT\r\n";
        let o = expand_14(body, utc(2026, 11, 2, 0, 0));
        // The window ends on Mon 16 Nov 00:00 UTC, before that Monday's 14:00.
        assert_eq!(starts(&o), [utc(2026, 11, 2, 14, 0), utc(2026, 11, 9, 14, 0), utc(2026, 11, 11, 14, 0)]);
        assert!(o.iter().all(|x| x.end - x.start == 3600));
    }

    #[test]
    fn exdate_works_in_utc_and_date_forms() {
        let body = "BEGIN:VEVENT\r\nUID:ex\r\nDTSTART:20261124T020000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\nEXDATE:20261125T020000Z\r\nEXDATE;VALUE=DATE:20261126\r\nEND:VEVENT\r\n";
        let o = expand_14(body, utc(2026, 11, 24, 0, 0));
        assert_eq!(starts(&o), [utc(2026, 11, 24, 2, 0), utc(2026, 11, 27, 2, 0), utc(2026, 11, 28, 2, 0)]);
    }

    #[test]
    fn count_and_until_end_a_recurrence() {
        let daily = |rule: &str| format!("BEGIN:VEVENT\r\nUID:r\r\nDTSTART:20261124T020000Z\r\nRRULE:{rule}\r\nEND:VEVENT\r\n");
        let from = utc(2026, 11, 24, 0, 0);
        assert_eq!(expand_14(&daily("FREQ=DAILY;COUNT=3"), from).len(), 3);
        // UNTIL in UTC is inclusive.
        assert_eq!(expand_14(&daily("FREQ=DAILY;UNTIL=20261126T020000Z"), from).len(), 3);
        assert_eq!(expand_14(&daily("FREQ=DAILY;UNTIL=20261126T015959Z"), from).len(), 2);
        // UNTIL as a date ends at the end of that day.
        assert_eq!(expand_14(&daily("FREQ=DAILY;UNTIL=20261127"), from).len(), 4);
        // COUNT counts instances before the window too: 5 in total, 3 of them already past.
        let o = expand_14(&daily("FREQ=DAILY;COUNT=5"), utc(2026, 11, 27, 0, 0));
        assert_eq!(starts(&o), [utc(2026, 11, 27, 2, 0), utc(2026, 11, 28, 2, 0)]);
    }

    #[test]
    fn expansion_stops_at_the_14_day_window() {
        let body = "BEGIN:VEVENT\r\nUID:e\r\nDTSTART:20260101T020000Z\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\n";
        let from = utc(2026, 11, 24, 0, 0);
        let o = expand_14(body, from);
        assert_eq!(o.len(), 14, "one per day for 14 days");
        assert_eq!(o.last().unwrap().start, utc(2026, 12, 7, 2, 0));
        // A single event on day 15 is not in the window.
        let far = "BEGIN:VEVENT\r\nUID:far\r\nDTSTART:20261208T020000Z\r\nEND:VEVENT\r\n";
        assert!(expand_14(far, from).is_empty());
        let edge = "BEGIN:VEVENT\r\nUID:edge\r\nDTSTART:20261207T230000Z\r\nEND:VEVENT\r\n";
        assert_eq!(expand_14(edge, from).len(), 1);
    }

    #[test]
    fn monthly_rules_by_month_day() {
        let body = "BEGIN:VEVENT\r\nUID:m\r\nDTSTART:20261101T020000Z\r\nRRULE:FREQ=MONTHLY;BYMONTHDAY=1,30\r\nEND:VEVENT\r\n";
        let o = occurrences(&parse(&format!("BEGIN:VCALENDAR\r\n{body}END:VCALENDAR\r\n")), utc(2026, 11, 25, 0, 0), utc(2027, 1, 31, 0, 0), &local7);
        assert_eq!(starts(&o), [utc(2026, 11, 30, 2, 0), utc(2026, 12, 1, 2, 0), utc(2026, 12, 30, 2, 0), utc(2027, 1, 1, 2, 0), utc(2027, 1, 30, 2, 0)]);
    }

    #[test]
    fn a_weekly_event_keeps_its_wall_clock_time_across_a_dst_change() {
        // 09:00 in New York every Friday; DST ends Sunday 1 Nov 2026.
        let body = "BEGIN:VEVENT\r\nUID:dst\r\nDTSTART;TZID=America/New_York:20261023T090000\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n";
        let o = expand_14(body, utc(2026, 10, 28, 0, 0));
        assert_eq!(starts(&o), [utc(2026, 10, 30, 13, 0), utc(2026, 11, 6, 14, 0)], "13:00 UTC in EDT, 14:00 UTC in EST");
    }

    #[test]
    fn rescheduled_and_cancelled_instances_replace_the_original() {
        let body = "BEGIN:VEVENT\r\nUID:series\r\nSUMMARY:Weekly sync\r\nDTSTART:20261123T020000Z\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n\
                    BEGIN:VEVENT\r\nUID:series\r\nSUMMARY:Weekly sync (moved)\r\nRECURRENCE-ID:20261130T020000Z\r\nDTSTART:20261201T060000Z\r\nEND:VEVENT\r\n\
                    BEGIN:VEVENT\r\nUID:series\r\nSTATUS:CANCELLED\r\nRECURRENCE-ID:20261207T020000Z\r\nDTSTART:20261207T020000Z\r\nEND:VEVENT\r\n";
        let from = utc(2026, 11, 23, 0, 0);
        let o = occurrences(&parse(&format!("BEGIN:VCALENDAR\r\n{body}END:VCALENDAR\r\n")), from, from + 20 * 86_400, &local7);
        let got: Vec<(i64, &str)> = o.iter().map(|x| (x.start, x.title.as_str())).collect();
        assert_eq!(got, [(utc(2026, 11, 23, 2, 0), "Weekly sync"), (utc(2026, 12, 1, 6, 0), "Weekly sync (moved)")]);
        // A cancelled event on its own is gone too.
        let c = "BEGIN:VEVENT\r\nUID:c\r\nSTATUS:CANCELLED\r\nDTSTART:20261125T020000Z\r\nEND:VEVENT\r\n";
        assert!(expand_14(c, utc(2026, 11, 24, 0, 0)).is_empty());
    }

    #[test]
    fn alarms_inside_an_event_are_not_the_events_properties() {
        let body = "BEGIN:VEVENT\r\nUID:al\r\nSUMMARY:Real title\r\nDTSTART:20261125T020000Z\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder text https://zoom.us/j/999\r\nSUMMARY:Alarm title\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\n";
        let o = expand_14(body, utc(2026, 11, 24, 0, 0));
        assert_eq!(o[0].title, "Real title");
        assert!(o[0].link.is_none());
    }

    #[test]
    fn finds_meeting_links_in_location_description_and_url() {
        let ev = |extra: &str| format!("BEGIN:VEVENT\r\nUID:l{}\r\nDTSTART:20261125T020000Z\r\n{extra}END:VEVENT\r\n", extra.len());
        let link = |extra: &str| expand_14(&ev(extra), utc(2026, 11, 24, 0, 0))[0].link.clone();
        assert_eq!(link("LOCATION:https://meet.google.com/abc-defg-hij\r\n").unwrap().service, links::Service::Meet);
        assert_eq!(link("DESCRIPTION:Gabung: https://us02web.zoom.us/j/81234567890?pwd=abc\\nSandi: 123\r\n").unwrap().service, links::Service::Zoom);
        assert_eq!(link("URL:https://teams.microsoft.com/l/meetup-join/19%3ameeting_x/0\r\n").unwrap().service, links::Service::Teams);
        assert_eq!(link("X-GOOGLE-CONFERENCE:https://meet.google.com/zzz-yyyy-xxx\r\n").unwrap().service, links::Service::Meet);
        assert_eq!(link("DESCRIPTION:Agenda: https://docs.example.com/agenda\r\n"), None);
        assert_eq!(link("LOCATION:Ruang rapat 4\r\n"), None);
    }

    // ---- broken input ----

    #[test]
    fn broken_files_give_an_empty_or_partial_calendar_never_a_crash() {
        let cases: Vec<String> = vec![
            String::new(),
            "not a calendar at all".into(),
            "\u{0}\u{1}\u{2}\u{ffff}".into(),
            "BEGIN:VCALENDAR".into(),
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:no end".into(),
            "END:VEVENT\r\nEND:VCALENDAR\r\nEND:VTIMEZONE\r\nEND:STANDARD".into(),
            "BEGIN:VEVENT\r\nEND:VEVENT\r\n".into(),
            "BEGIN:VEVENT\r\nDTSTART:garbage\r\nEND:VEVENT\r\n".into(),
            "BEGIN:VEVENT\r\nDTSTART:20261340T250000\r\nEND:VEVENT\r\n".into(),
            "BEGIN:VEVENT\r\nDTSTART;TZID=:20261125T100000\r\nEND:VEVENT\r\n".into(),
            "BEGIN:VEVENT\r\nDTSTART:20261125T100000Z\r\nRRULE:FREQ=\r\nEXDATE:,,,\r\nDURATION:xx\r\nEND:VEVENT\r\n".into(),
            "BEGIN:VEVENT\r\nDTSTART:20261125T100000Z\r\nBEGIN:VEVENT\r\nDTSTART:20261126T100000Z\r\nEND:VEVENT\r\n".into(),
            "BEGIN:VTIMEZONE\r\nBEGIN:STANDARD\r\nTZOFFSETTO:+9999\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nBEGIN:VTIMEZONE\r\nTZID:X\r\nEND:VTIMEZONE\r\n".into(),
            format!("BEGIN:VEVENT\r\nDTSTART:20261125T100000Z\r\nSUMMARY:{}\r\nEND:VEVENT\r\n", "x".repeat(2_000_000)),
            format!("BEGIN:VEVENT\r\nDTSTART:00010101T000000Z\r\nDTEND:99991231T235959Z\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\n{}", "BEGIN:VEVENT\r\n".repeat(1000)),
            "BEGIN:VEVENT\r\nDTSTART;VALUE=DATE:99991231\r\nRRULE:FREQ=YEARLY\r\nEND:VEVENT\r\n".into(),
        ];
        for c in &cases {
            let cal = parse(c);
            let _ = occurrences(&cal, utc(2026, 11, 24, 0, 0), utc(2026, 12, 8, 0, 0), &local7);
        }
        // A good event after garbage still comes through.
        let o = expand_14(&format!("garbage line\r\nBEGIN:VEVENT\r\nDTSTART:bad\r\nEND:VEVENT\r\n{SPRINT}"), utc(2026, 11, 24, 0, 0));
        assert_eq!(o.len(), 1);
        // A file cut off in the middle of an event keeps the finished ones.
        let cut = format!("BEGIN:VCALENDAR\r\n{SPRINT}BEGIN:VEVENT\r\nUID:cut\r\nDTSTART:20261126T0");
        assert_eq!(occurrences(&parse(&cut), utc(2026, 11, 24, 0, 0), utc(2026, 12, 8, 0, 0), &local7).len(), 1);
    }

    #[test]
    fn pseudo_random_garbage_never_panics() {
        // A deterministic mix of real fragments and noise.
        let frags = ["BEGIN:VEVENT", "END:VEVENT", "DTSTART:20261125T", "DTSTART;TZID=", ";", ":", "=", "\"", "RRULE:FREQ=", "WEEKLY", "BYDAY=", "-1", "MO", ",", "\r\n", "\n ", "Z", "20261125", "EXDATE", "BEGIN:VTIMEZONE", "TZOFFSETTO:+", "0700", "END:VTIMEZONE", "UNTIL=", "COUNT=-5", "\\", "é", "\u{0}"];
        let mut seed = 0x2545F4914F6CDD1Du64;
        for _ in 0..300 {
            let mut s = String::new();
            for _ in 0..40 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                s.push_str(frags[(seed % frags.len() as u64) as usize]);
            }
            let cal = parse(&s);
            let _ = occurrences(&cal, utc(2026, 11, 24, 0, 0), utc(2026, 12, 8, 0, 0), &local7);
        }
    }

    #[test]
    fn caps_keep_a_huge_calendar_bounded() {
        let mut body = String::new();
        for i in 0..2000 {
            body.push_str(&format!("BEGIN:VEVENT\r\nUID:{i}\r\nDTSTART:20261125T020000Z\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\n"));
        }
        let o = expand_14(&body, utc(2026, 11, 24, 0, 0));
        assert!(o.len() <= MAX_OCCURRENCES, "{}", o.len());
    }
}
