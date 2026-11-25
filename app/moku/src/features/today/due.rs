//! The due-time parser: "rapat jam 3", "kirim laporan besok 09:00", "nanti malam", "in 2 hours",
//! "tomorrow 9am" -> the to-do's text and the moment to remind. Indonesian and English. Pure: the
//! clock is an argument (`now`, local wall-clock time), nothing is read from the system, nothing
//! panics (all date arithmetic is checked), and a phrase it does not understand is left in the
//! text. Used by the to-do field of the Hari ini tab, `mokuctl todo add` and the launcher's
//! `todo` command, so all three read the same words.
//!
//! # Rules (the table of examples is `due_tests.rs`)
//!
//! * **No date, no time words** -> no due time; the text is unchanged.
//! * **Only a date** (`besok`, `lusa`, a weekday, `tanggal 28`, `28 desember`, `next friday`) -> that
//!   day at **09:00**. `hari ini` / `today` alone -> today 17:00 (nothing, while that has passed).
//! * **Only a part of the day** (`nanti malam`, `malam ini`, `tonight`, `this evening`) -> its default
//!   time: pagi 08:00, siang 12:00, sore 16:00 (afternoon 15:00, evening 18:00), malam 20:00. When that
//!   has passed but the part of the day has not, a little later instead (now + 30 min, up to
//!   the next quarter hour); when the part of the day is over, tomorrow's. A bare `pagi` / `malam` with no
//!   `nanti`, `ini`, `besok` ... next to it is NOT a time ("makan malam sama ibu" stays a to-do).
//! * **Weekdays**: `senin` = the next Monday after today (a plain weekday that is today's weekday means
//!   next week, unless a time for later today is given). `senin depan` / `next monday` = that day in
//!   the **next calendar week** (Monday to Sunday); `minggu depan` / `next week` = next Monday.
//!   `minggu` alone is Sunday.
//! * **Day of the month**: `tanggal 28` / `tgl 28` = the next 28th that is after now (months without
//!   a 28th are skipped); `28 desember` / `dec 28` = the next such date (next year when it has passed).
//! * **Clock times**: `jam 15.30`, `pukul 9`, `pk 17.00`, `at 3pm`, `15:30`, `09:00`, `3:30pm`,
//!   `jam setengah 4` (= 3:30). A time with an AM/PM or a part of the day (`jam 8 malam`, `jam 3 sore`,
//!   `jam 3 pagi`) is exact. With no date, a time that has passed today means tomorrow's.
//! * **The ambiguous hour**: a bare `jam 3` / `at 9` / `3:30` (an hour 1-12 without a leading zero,
//!   no AM/PM, no part of the day) is the **next time that clock shows** (the next 03:00 or 15:00,
//!   the next 09:00 or 21:00), except that 00:00-05:59 is skipped unless it is night now
//!   (nobody means 03:00 by "rapat jam 3", but someone typing at 02:00 may). With a date named
//!   (`besok jam 3`) it is the likelier hour: 7-12 -> morning (12 = noon), 1-6 -> afternoon (13:00-18:00).
//!   A time with a leading zero (`03:00`), an hour above 12, or an hour of 10+ with minutes
//!   (`10.30`) is read as a 24-hour clock exactly.
//! * **Midnight**: `jam 12 malam` is 00:00, and a night hour that is 0-5 (`jam 12 malam`, `jam 2 malam`)
//!   belongs to the **next** day when a day is named: `malam ini jam 12` = tonight's midnight = 00:00
//!   tomorrow, `besok jam 12 malam` = 00:00 the day after tomorrow. `24:00` is 00:00 of the next day
//!   (`besok 24:00` = 00:00 the day after tomorrow). With no day named it is simply the next time the
//!   clock shows.
//! * **Time zones**: `WIB`, `WITA` and `WIT` after a clock time (`jam 3 WITA`) are UTC+7, +8 and +9, **converted
//!   to the local zone** (in WIB: `jam 3 WITA` = 14:00). The phrase is read as that zone's clock, so
//!   "the next 15:00" and "today" are the zone's.
//! * **Relative**: `30 menit lagi`, `dalam 2 jam`, `sejam lagi`, `setengah jam lagi`, `3 hari lagi`,
//!   `in 2 hours`, `in an hour`, `in half an hour`, `in 1.5 hours`, `in 2 days`, `in 1 week`
//!   (1 minute to a year). It wins over any other date or time in the text.
//! * Dates that are explicit but already past today (`hari ini 08:30` at 20:00) are kept as given
//!   (an overdue reminder), not moved.
//! * The text is what is left after the time words are cut out (case kept), minus a connector
//!   like "pada" / "at" / "on" left dangling next to them. If nothing is left the text stays as typed.
//!
//! A phrase is only a time when it has a time word: `jam tangan`, `3 jam`, `versi 1.20`, `ratio 3:1`
//! and `Review PR #214` are plain text.

use chrono::{Datelike, Days, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

#[cfg(test)]
#[path = "due_tests.rs"]
mod tests;

/// Time for a to-do that only names a day.
pub const DEFAULT_HOUR: u32 = 9;
/// "hari ini" with no time.
const END_OF_DAY_HOUR: u32 = 17;
/// Only the first words of a very long text are looked at.
const MAX_TOKENS: usize = 120;
/// "in N minutes" is at most a year.
const MAX_REL_MINUTES: f64 = 366.0 * 24.0 * 60.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub title: String,
    pub due: Option<NaiveDateTime>,
}

// ---- vocabulary -------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Part {
    Morning,
    Noon,
    Sore,
    Afternoon,
    Evening,
    Night,
}

impl Part {
    fn default_hour(self) -> u32 {
        match self {
            Part::Morning => 8,
            Part::Noon => 12,
            Part::Sore => 16,
            Part::Afternoon => 15,
            Part::Evening => 18,
            Part::Night => 20,
        }
    }

    /// Hours (`start..end`) that count as being "in" this part of the day.
    fn window(self) -> (u32, u32) {
        match self {
            Part::Morning => (5, 12),
            Part::Noon => (11, 14),
            Part::Sore => (14, 18),
            Part::Afternoon => (12, 18),
            Part::Evening => (17, 22),
            Part::Night => (18, 24),
        }
    }

    /// The 24-hour hour for a clock `h` (1-12) said with this part of the day.
    fn hour24(self, h: u32) -> u32 {
        match self {
            Part::Morning => h,
            Part::Noon => match h {
                10..=12 => h,
                _ => h + 12,
            },
            Part::Sore | Part::Afternoon | Part::Evening => {
                if h < 12 {
                    h + 12
                } else {
                    h
                }
            }
            Part::Night => match h {
                12 => 0,
                6..=11 => h + 12,
                _ => h,
            },
        }
    }
}

fn part_word(w: &str) -> Option<Part> {
    Some(match w {
        "pagi" | "morning" => Part::Morning,
        "siang" => Part::Noon,
        "sore" => Part::Sore,
        "afternoon" => Part::Afternoon,
        "evening" => Part::Evening,
        "malam" | "night" | "tonight" => Part::Night,
        _ => return None,
    })
}

fn weekday_word(w: &str) -> Option<Weekday> {
    Some(match w {
        "senin" | "monday" => Weekday::Mon,
        "selasa" | "tuesday" => Weekday::Tue,
        "rabu" | "wednesday" => Weekday::Wed,
        "kamis" | "thursday" => Weekday::Thu,
        "jumat" | "friday" => Weekday::Fri,
        "sabtu" | "saturday" => Weekday::Sat,
        "minggu" | "ahad" | "sunday" => Weekday::Sun,
        _ => return None,
    })
}

fn month_word(w: &str) -> Option<u32> {
    Some(match w {
        "januari" | "january" | "jan" => 1,
        "februari" | "february" | "feb" => 2,
        "maret" | "march" | "mar" => 3,
        "april" | "apr" => 4,
        "mei" | "may" => 5,
        "juni" | "june" | "jun" => 6,
        "juli" | "july" | "jul" => 7,
        "agustus" | "august" | "agu" | "ags" | "agt" | "aug" => 8,
        "september" | "sep" | "sept" => 9,
        "oktober" | "october" | "okt" | "oct" => 10,
        "november" | "nov" => 11,
        "desember" | "december" | "des" | "dec" => 12,
        _ => return None,
    })
}

/// Minutes in one unit of a duration word.
fn unit_minutes(w: &str) -> Option<f64> {
    Some(match w {
        "menit" | "mnt" | "min" | "mins" | "minute" | "minutes" | "m" => 1.0,
        "jam" | "hour" | "hours" | "hr" | "hrs" | "h" => 60.0,
        "hari" | "day" | "days" | "d" => 1440.0,
        "minggu" | "week" | "weeks" | "w" => 10_080.0,
        _ => return None,
    })
}

fn is_marker(w: &str) -> bool {
    matches!(w, "jam" | "pukul" | "pk" | "at" | "@")
}

fn is_zone(w: &str) -> bool {
    zone_hours(w).is_some()
}

/// UTC offset (hours) of an Indonesian time zone: WIB +7, WITA +8, WIT +9.
fn zone_hours(w: &str) -> Option<i32> {
    match w {
        "wib" => Some(7),
        "wita" => Some(8),
        "wit" => Some(9),
        _ => None,
    }
}

/// Words left dangling next to a cut-out time ("rapat pada" + jam 3).
fn is_connector(w: &str) -> bool {
    matches!(
        w,
        "pada" | "sekitar" | "sampai" | "hingga" | "jam" | "pukul" | "pk" | "tanggal" | "tgl" | "hari" | "nanti" | "dalam" | "at" | "on" | "in" | "by" | "around" | "until" | "till" | "this" | "next"
    )
}

// ---- tokens -----------------------------------------------------------------------------------

struct Tok<'a> {
    raw: &'a str,
    /// Lower case, without apostrophes, brackets and trailing punctuation.
    low: String,
}

fn norm(raw: &str) -> String {
    let mut s: String = raw.chars().filter(|c| *c != '\'' && *c != '\u{2019}').flat_map(char::to_lowercase).collect();
    while s.ends_with([',', ';', '!', '?', ')', '.']) {
        s.pop();
    }
    while s.starts_with(['(', '"']) {
        s.remove(0);
    }
    s
}

fn int(s: &str) -> Option<u32> {
    (!s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// `30`, `1.5`, `1,5`.
fn amount(s: &str) -> Option<f64> {
    let s = s.replace(',', ".");
    let ok = !s.is_empty() && s.len() <= 12 && s.bytes().all(|b| b.is_ascii_digit() || b == b'.') && s.bytes().filter(|b| *b == b'.').count() <= 1 && s.bytes().any(|b| b.is_ascii_digit());
    if ok { s.parse().ok() } else { None }
}

// ---- what a phrase can mean -------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum DateSpec {
    Today,
    Days(u64),
    Weekday { wd: Weekday, next: bool },
    NextWeek,
    DayOfMonth(u32),
    MonthDay(u32, u32),
}

/// A clock time. `ambiguous`: `h` is 1-12 and may be morning or afternoon; otherwise `h` is 0-23.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Clock {
    h: u32,
    m: u32,
    ambiguous: bool,
    /// The clock is 00:00-05:59 of the night that *ends* the named day (`jam 12 malam`, `24:00`): with a
    /// day named it falls on the day after.
    next_day: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Found {
    Rel(i64),
    Date(DateSpec),
    Clock(Clock),
    /// A part of the day; the flag says it named today ("malam ini", "tonight").
    Part(Part, bool),
}

#[derive(Clone, Copy, Debug)]
struct Piece {
    start: usize,
    len: usize,
    found: Found,
}

/// A number token as a clock: `3`, `3:30`, `15.30`, `9am`, `3.30pm`.
struct RawClock {
    h: u32,
    m: u32,
    minutes: bool,
    two_digit_hour: bool,
    leading_zero: bool,
    pm: Option<bool>,
}

fn raw_clock(tok: &str, allow_bare: bool, allow_dot: bool) -> Option<RawClock> {
    let (body, pm) = if let Some(b) = tok.strip_suffix("am").or_else(|| tok.strip_suffix("a.m")) {
        (b, Some(false))
    } else if let Some(b) = tok.strip_suffix("pm").or_else(|| tok.strip_suffix("p.m")) {
        (b, Some(true))
    } else {
        (tok, None)
    };
    let (hs, ms) = match body.find([':', '.']) {
        Some(i) => {
            if body.as_bytes()[i] == b'.' && !allow_dot && pm.is_none() {
                return None;
            }
            (&body[..i], Some(&body[i + 1..]))
        }
        None => (body, None),
    };
    if hs.is_empty() || hs.len() > 2 || !hs.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: u32 = hs.parse().ok()?;
    let m: u32 = match ms {
        Some(ms) => {
            if ms.len() != 2 || !ms.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            ms.parse().ok()?
        }
        None => 0,
    };
    if ms.is_none() && pm.is_none() && !allow_bare {
        return None;
    }
    Some(RawClock { h, m, minutes: ms.is_some(), two_digit_hour: hs.len() == 2, leading_zero: hs.len() == 2 && hs.starts_with('0'), pm })
}

impl RawClock {
    fn into_clock(self, part: Option<Part>) -> Option<Clock> {
        if self.m > 59 {
            return None;
        }
        if let Some(pm) = self.pm {
            if !(1..=12).contains(&self.h) {
                return None;
            }
            let h = match (self.h, pm) {
                (12, false) => 0,
                (12, true) => 12,
                (h, true) => h + 12,
                (h, false) => h,
            };
            return Some(Clock { h, m: self.m, ambiguous: false, next_day: false });
        }
        // 24:00 is the end of the day: 00:00 of the next one.
        if self.h == 24 && self.m == 0 && part.is_none() {
            return Some(Clock { h: 0, m: 0, ambiguous: false, next_day: true });
        }
        if self.h > 23 {
            return None;
        }
        if let Some(p) = part {
            if !(1..=12).contains(&self.h) {
                // "jam 15 malam": the part of the day adds nothing; take the 24-hour time.
                return Some(Clock { h: self.h, m: self.m, ambiguous: false, next_day: false });
            }
            let h = p.hour24(self.h);
            return Some(Clock { h, m: self.m, ambiguous: false, next_day: p == Part::Night && h <= 5 });
        }
        let exact = self.h == 0 || self.h >= 13 || self.leading_zero || (self.two_digit_hour && self.minutes);
        Some(Clock { h: self.h, m: self.m, ambiguous: !exact, next_day: false })
    }
}

// ---- scanning ---------------------------------------------------------------------------------

fn is_date_word(w: &str) -> bool {
    matches!(w, "besok" | "esok" | "lusa" | "tomorrow" | "today") || weekday_word(w).is_some()
}

/// The clock at token `i`, with how many tokens it takes.
fn match_clock(t: &[Tok], i: usize) -> Option<(usize, Clock)> {
    let w = t[i].low.as_str();
    let at = |k: usize| t.get(k).map(|x| x.low.as_str());
    // "tengah malam" / "tengah hari", "noon", "midnight".
    let fixed = |h: u32| Some(Clock { h, m: 0, ambiguous: false, next_day: false });
    match w {
        "noon" => return fixed(12).map(|c| (1, c)),
        "midnight" => return fixed(0).map(|c| (1, c)),
        "tengah" if at(i + 1) == Some("malam") => return fixed(0).map(|c| (2, c)),
        "tengah" if at(i + 1) == Some("hari") => return fixed(12).map(|c| (2, c)),
        _ => {}
    }
    let marker = is_marker(w);
    // After a marker the number is at i + 1; without one it is this token and must say it is a time.
    let n = if marker { i + 1 } else { i };
    if marker {
        // "at noon", "jam tengah malam" and so on: the marker is only a word then.
        if let Some(next) = at(n) {
            if matches!(next, "noon" | "midnight" | "tengah") {
                return None;
            }
        }
    }
    let (num_at, setengah) = match at(n) {
        Some("setengah") => (n + 1, true),
        _ => (n, false),
    };
    let tok = at(num_at)?;
    let next = at(num_at + 1);
    let next_is_ampm = matches!(next, Some("am" | "pm" | "a.m" | "p.m"));
    let prev_is_date = i > 0 && is_date_word(&t[i - 1].low);
    let mut raw = if setengah {
        // "setengah 4" = 3:30 (the half hour BEFORE 4).
        let h = int(tok)?;
        if !(1..=12).contains(&h) {
            return None;
        }
        RawClock { h: if h == 1 { 12 } else { h - 1 }, m: 30, minutes: true, two_digit_hour: false, leading_zero: false, pm: None }
    } else if marker {
        raw_clock(tok, true, true)?
    } else {
        // Without a marker: a colon time, a time with am/pm, or "9.30"/"9" after a date word.
        let r = raw_clock(tok, next_is_ampm, prev_is_date)?;
        if !(tok.contains(':') || r.pm.is_some() || next_is_ampm || (prev_is_date && tok.contains('.'))) {
            return None;
        }
        r
    };
    let mut len = num_at + 1 - i;
    // am / pm in the next token, or a part of the day.
    let mut part = None;
    if raw.pm.is_none() {
        match next {
            Some("am" | "a.m") => {
                raw.pm = Some(false);
                len += 1;
            }
            Some("pm" | "p.m") => {
                raw.pm = Some(true);
                len += 1;
            }
            Some(p) if part_word(p).is_some() && p != "tonight" && !matches!(p, "morning" | "afternoon" | "evening" | "night") => {
                part = part_word(p);
                len += 1;
            }
            _ => {}
        }
    }
    if setengah && raw.pm.is_none() && part.is_none() {
        // "jam setengah 4": ambiguous 3:30 unless the hour says otherwise (12:30 for 1).
        raw.leading_zero = false;
    }
    if let Some(z) = t.get(i + len) {
        if is_zone(&z.low) {
            len += 1;
        }
    }
    let clock = raw.into_clock(part)?;
    Some((len, clock))
}

/// "in 2 hours", "2 jam lagi", "dalam 2 jam", "sejam lagi", "setengah jam lagi", "in an hour".
fn match_rel(t: &[Tok], i: usize) -> Option<(usize, i64)> {
    let w = t[i].low.as_str();
    let at = |k: usize| t.get(k).map(|x| x.low.as_str());
    let lagi = |k: usize| at(k) == Some("lagi");
    let minutes = |m: f64, len: usize| -> Option<(usize, i64)> {
        let r = m.round();
        (r >= 1.0 && r <= MAX_REL_MINUTES).then_some((len, r as i64))
    };
    // A number and its unit: "2 jam" or "2jam" at k. Returns (tokens, minutes).
    let number_unit = |k: usize| -> Option<(usize, f64)> {
        let tok = at(k)?;
        let split = tok.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','));
        match split {
            Some(s) if s > 0 => Some((1, amount(&tok[..s])? * unit_minutes(&tok[s..])?)),
            Some(_) => None,
            None => {
                let n = amount(tok)?;
                Some((2, n * unit_minutes(at(k + 1)?)?))
            }
        }
    };
    // A unit with an article: "sejam", "semenit", "an hour", "a minute", "half an hour", "setengah jam".
    let article_unit = |k: usize| -> Option<(usize, f64)> {
        match at(k)? {
            "sejam" => Some((1, 60.0)),
            "semenit" => Some((1, 1.0)),
            "sehari" => Some((1, 1440.0)),
            "seminggu" => Some((1, 10_080.0)),
            "setengah" if at(k + 1) == Some("jam") => Some((2, 30.0)),
            "an" | "a" => Some((2, unit_minutes(at(k + 1)?)?)),
            "half" if at(k + 1) == Some("an") && at(k + 2) == Some("hour") => Some((3, 30.0)),
            _ => None,
        }
    };
    match w {
        "dalam" | "in" => {
            let (len, m) = number_unit(i + 1).or_else(|| article_unit(i + 1))?;
            let extra = usize::from(w == "dalam" && lagi(i + 1 + len));
            minutes(m, 1 + len + extra)
        }
        _ => {
            // Without "dalam"/"in" the Indonesian form needs "lagi" after it.
            let (len, m) = number_unit(i).or_else(|| article_unit(i).filter(|_| at(i) != Some("an") && at(i) != Some("a") && at(i) != Some("half")))?;
            if !lagi(i + len) {
                return None;
            }
            minutes(m, len + 1)
        }
    }
}

/// A date at token `i`.
fn match_date(t: &[Tok], i: usize) -> Option<(usize, DateSpec)> {
    let w = t[i].low.as_str();
    let at = |k: usize| t.get(k).map(|x| x.low.as_str());
    let day = |s: &str| int(s).filter(|d| (1..=31).contains(d));
    // "5th", "21st".
    let ordinal_day = |s: &str| {
        let digits = s.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let suffix = &s[digits.len()..];
        if suffix.is_empty() || matches!(suffix, "st" | "nd" | "rd" | "th") { day(digits) } else { None }
    };
    match w {
        "besok" | "esok" | "tomorrow" => return Some((1, DateSpec::Days(1))),
        "lusa" => return Some((1, DateSpec::Days(2))),
        "today" => return Some((1, DateSpec::Today)),
        "day" if at(i + 1) == Some("after") && at(i + 2) == Some("tomorrow") => return Some((3, DateSpec::Days(2))),
        "hari" if at(i + 1) == Some("ini") => return Some((2, DateSpec::Today)),
        "hari" => {
            if let Some(wd) = at(i + 1).and_then(weekday_word) {
                let next = at(i + 2) == Some("depan");
                return Some((2 + usize::from(next), DateSpec::Weekday { wd, next }));
            }
        }
        "next" | "this" => {
            if w == "next" && at(i + 1) == Some("week") {
                return Some((2, DateSpec::NextWeek));
            }
            if let Some(wd) = at(i + 1).and_then(weekday_word) {
                return Some((2, DateSpec::Weekday { wd, next: w == "next" }));
            }
        }
        "minggu" if at(i + 1) == Some("depan") => return Some((2, DateSpec::NextWeek)),
        "minggu" if matches!(at(i + 1), Some("ini" | "lalu" | "kemarin")) => return None,
        "tanggal" | "tgl" => {
            let d = at(i + 1).and_then(day)?;
            if let Some(mo) = at(i + 2).and_then(month_word) {
                return valid_md(mo, d).then_some((3, DateSpec::MonthDay(mo, d)));
            }
            return Some((2, DateSpec::DayOfMonth(d)));
        }
        _ => {}
    }
    if let Some(wd) = weekday_word(w) {
        let next = at(i + 1) == Some("depan");
        return Some((1 + usize::from(next), DateSpec::Weekday { wd, next }));
    }
    // "28 desember", "28 dec".
    if let (Some(d), Some(mo)) = (day(w), at(i + 1).and_then(month_word)) {
        return valid_md(mo, d).then_some((2, DateSpec::MonthDay(mo, d)));
    }
    // "dec 28", "december 5th".
    if let (Some(mo), Some(d)) = (month_word(w), at(i + 1).and_then(ordinal_day)) {
        return valid_md(mo, d).then_some((2, DateSpec::MonthDay(mo, d)));
    }
    None
}

/// Does a month have this day (29 February counts: a leap year is looked for).
fn valid_md(mo: u32, d: u32) -> bool {
    NaiveDate::from_ymd_opt(2024, mo, d).is_some()
}

/// A part of the day at token `i`: `(first token, tokens, part, names today)`.
fn match_part(t: &[Tok], i: usize, last_end: usize) -> Option<(usize, usize, Part, bool)> {
    let w = t[i].low.as_str();
    let p = part_word(w)?;
    let prev = (i > 0).then(|| t[i - 1].low.as_str());
    let next = t.get(i + 1).map(|x| x.low.as_str());
    if w == "tonight" {
        return Some((i, 1, Part::Night, true));
    }
    // Context: "nanti malam", "this evening", "malam ini", or right after a date ("besok pagi").
    let mut start = i;
    let mut len = 1;
    let mut today = false;
    let mut ctx = false;
    if prev == Some("nanti") && last_end <= i - 1 {
        start = i - 1;
        len += 1;
        ctx = true;
    } else if prev == Some("this") && last_end <= i - 1 {
        start = i - 1;
        len += 1;
        ctx = true;
        today = true;
    }
    if matches!(next, Some("ini")) && matches!(w, "pagi" | "siang" | "sore" | "malam") {
        len += 1;
        ctx = true;
        today = true;
    }
    if !ctx && i > 0 && last_end == i {
        ctx = true;
    }
    ctx.then_some((start, len, p, today))
}

fn scan(t: &[Tok]) -> Vec<Piece> {
    let mut out: Vec<Piece> = vec![];
    let (mut have_rel, mut have_date, mut have_clock, mut have_part) = (false, false, false, false);
    let mut last_end = 0usize;
    let mut i = 0;
    while i < t.len() {
        let mut took = 0;
        if !have_rel {
            if let Some((len, m)) = match_rel(t, i) {
                out.push(Piece { start: i, len, found: Found::Rel(m) });
                have_rel = true;
                took = len;
            }
        }
        if took == 0 && !have_clock {
            if let Some((len, c)) = match_clock(t, i) {
                out.push(Piece { start: i, len, found: Found::Clock(c) });
                have_clock = true;
                took = len;
            }
        }
        if took == 0 && !have_date {
            if let Some((len, d)) = match_date(t, i) {
                out.push(Piece { start: i, len, found: Found::Date(d) });
                have_date = true;
                took = len;
            }
        }
        if took == 0 && !have_part {
            if let Some((start, len, p, today)) = match_part(t, i, last_end) {
                out.push(Piece { start, len, found: Found::Part(p, today) });
                have_part = true;
                took = (start + len).saturating_sub(i);
            }
        }
        if took > 0 {
            i += took;
            last_end = i;
        } else {
            i += 1;
        }
    }
    out
}

// ---- meaning ----------------------------------------------------------------------------------

fn minute_floor(d: NaiveDateTime) -> NaiveDateTime {
    d.with_second(0).and_then(|d| d.with_nanosecond(0)).unwrap_or(d)
}

fn at_time(d: NaiveDate, h: u32, m: u32) -> Option<NaiveDateTime> {
    Some(d.and_time(NaiveTime::from_hms_opt(h, m, 0)?))
}

/// The next multiple of 15 minutes at or after `d`.
fn round_up_quarter(d: NaiveDateTime) -> Option<NaiveDateTime> {
    let extra = (15 - d.minute() % 15) % 15;
    minute_floor(d).checked_add_signed(Duration::minutes(i64::from(extra)))
}

fn monday_of(d: NaiveDate) -> Option<NaiveDate> {
    d.checked_sub_days(Days::new(u64::from(d.weekday().num_days_from_monday())))
}

/// The hour to take for a bare 1-12 when the day is named.
fn plausible(h: u32) -> u32 {
    if (1..=6).contains(&h) { h + 12 } else { h }
}

/// The date named by `spec` at `h:m`, in the future where the phrase implies it.
fn place(spec: DateSpec, h: u32, m: u32, now: NaiveDateTime) -> Option<NaiveDateTime> {
    let today = now.date();
    match spec {
        DateSpec::Today => at_time(today, h, m),
        DateSpec::Days(n) => at_time(today.checked_add_days(Days::new(n))?, h, m),
        DateSpec::Weekday { wd, next: false } => {
            let delta = (7 + wd.num_days_from_monday() - today.weekday().num_days_from_monday()) % 7;
            let first = at_time(today.checked_add_days(Days::new(u64::from(delta)))?, h, m)?;
            if first > now { Some(first) } else { at_time(today.checked_add_days(Days::new(u64::from(delta) + 7))?, h, m) }
        }
        DateSpec::Weekday { wd, next: true } => at_time(monday_of(today)?.checked_add_days(Days::new(7 + u64::from(wd.num_days_from_monday())))?, h, m),
        DateSpec::NextWeek => at_time(monday_of(today)?.checked_add_days(Days::new(7))?, h, m),
        DateSpec::DayOfMonth(d) => {
            let (mut y, mut mo) = (today.year(), today.month());
            for _ in 0..24 {
                if let Some(dt) = NaiveDate::from_ymd_opt(y, mo, d).and_then(|date| at_time(date, h, m)) {
                    if dt > now {
                        return Some(dt);
                    }
                }
                if mo == 12 {
                    y = y.checked_add(1)?;
                    mo = 1;
                } else {
                    mo += 1;
                }
            }
            None
        }
        DateSpec::MonthDay(mo, d) => {
            for y in today.year()..=today.year().checked_add(4)? {
                if let Some(dt) = NaiveDate::from_ymd_opt(y, mo, d).and_then(|date| at_time(date, h, m)) {
                    if dt > now {
                        return Some(dt);
                    }
                }
            }
            None
        }
    }
}

/// No date given: the next time `candidates` (hour, minute) shows after `now`, today or tomorrow.
fn next_of(candidates: &[(u32, u32)], now: NaiveDateTime) -> Option<NaiveDateTime> {
    let today = now.date();
    let tomorrow = today.checked_add_days(Days::new(1))?;
    [today, tomorrow].into_iter().flat_map(|d| candidates.iter().filter_map(move |(h, m)| at_time(d, *h, *m))).filter(|dt| *dt > now).min()
}

fn combine(now: NaiveDateTime, date: Option<DateSpec>, clock: Option<Clock>, part: Option<(Part, bool)>) -> Option<NaiveDateTime> {
    // A part of the day that names today ("malam ini") is a date too, unless one is given.
    let date = date.or(match part {
        Some((_, true)) => Some(DateSpec::Today),
        _ => None,
    });
    let today = now.date();
    match (clock, part) {
        (Some(c), p) => {
            // The hour as far as it is known.
            let c = match (c.ambiguous, p) {
                (true, Some((p, _))) => {
                    let h = p.hour24(c.h);
                    Clock { h, m: c.m, ambiguous: false, next_day: p == Part::Night && h <= 5 }
                }
                _ => c,
            };
            let placed = match (date, c.ambiguous) {
                (None, false) => next_of(&[(c.h, c.m)], now),
                (None, true) | (Some(DateSpec::Today), true) => {
                    // 1-12: the morning one, the afternoon one (12 is noon); the small hours are
                    // not meant unless it is the small hours now.
                    let mut cands: Vec<(u32, u32)> = if c.h == 12 { vec![(12, c.m)] } else { vec![(c.h, c.m), (c.h + 12, c.m)] };
                    if now.hour() >= 6 {
                        cands.retain(|(h, _)| *h >= 6);
                    }
                    if date == Some(DateSpec::Today) {
                        let day: Vec<NaiveDateTime> = cands.iter().filter_map(|(h, m)| at_time(today, *h, *m)).collect();
                        return day.iter().copied().filter(|d| *d > now).min().or_else(|| day.iter().copied().max());
                    }
                    next_of(&cands, now)
                }
                (Some(d), false) => place(d, c.h, c.m, now),
                (Some(d), true) => place(d, plausible(c.h), c.m, now),
            };
            // The night that ends the named day: 00:00-05:59 is already the next calendar day. (Someone
            // typing "malam ini jam 2" at 01:00 means the 02:00 that is coming, not tomorrow's.)
            let coming_small_hours = date == Some(DateSpec::Today) && now.hour() < 6 && !(c.h == 0 && c.m == 0) && placed.is_some_and(|d| d > now);
            match placed {
                Some(d) if c.next_day && date.is_some() && !coming_small_hours => d.checked_add_days(Days::new(1)),
                other => other,
            }
        }
        (None, Some((p, _))) => {
            let (dh, _) = (p.default_hour(), 0);
            match date {
                Some(DateSpec::Today) | None => {
                    let default = at_time(today, dh, 0)?;
                    if default > now {
                        return Some(default);
                    }
                    let (from, to) = p.window();
                    if (from..to).contains(&now.hour()) {
                        return round_up_quarter(now.checked_add_signed(Duration::minutes(30))?);
                    }
                    if date.is_none() { at_time(today.checked_add_days(Days::new(1))?, dh, 0) } else { Some(default) }
                }
                Some(d) => place(d, dh, 0, now),
            }
        }
        (None, None) => match date? {
            DateSpec::Today => (now.hour() < END_OF_DAY_HOUR).then(|| at_time(today, END_OF_DAY_HOUR, 0)).flatten(),
            d => place(d, DEFAULT_HOUR, 0, now),
        },
    }
}

// ---- entry point ------------------------------------------------------------------------------

/// Read the due time out of `input` (see the module docs for the rules). `now` is local
/// wall-clock time; a `WIB` / `WITA` / `WIT` after a clock time is converted to this PC's time zone.
pub fn parse(input: &str, now: NaiveDateTime) -> Parsed {
    parse_zoned(input, now, local_offset_secs())
}

/// This PC's UTC offset now, in seconds (east is positive).
fn local_offset_secs() -> i32 {
    use chrono::Offset;
    chrono::Local::now().offset().fix().local_minus_utc()
}

/// [`parse`] with the local zone given (seconds east of UTC): pure, for tests.
pub fn parse_zoned(input: &str, now: NaiveDateTime, local_secs: i32) -> Parsed {
    let words: Vec<&str> = input.split_whitespace().collect();
    let plain = || Parsed { title: words.join(" "), due: None };
    let toks: Vec<Tok> = words.iter().take(MAX_TOKENS).map(|w| Tok { raw: w, low: norm(w) }).collect();
    let now = minute_floor(now);
    let pieces = scan(&toks);
    if pieces.is_empty() {
        return plain();
    }
    let first = |f: fn(&Found) -> bool| pieces.iter().find(|p| f(&p.found));
    let (due, used): (Option<NaiveDateTime>, Vec<Piece>) = if let Some(p) = first(|f| matches!(f, Found::Rel(_))) {
        let Found::Rel(m) = p.found else { return plain() };
        (now.checked_add_signed(Duration::minutes(m)), vec![*p])
    } else {
        let date = first(|f| matches!(f, Found::Date(_)));
        let clock = first(|f| matches!(f, Found::Clock(_)));
        // A zone word right after the clock (`jam 3 WITA`): read the phrase as that zone's clock, then
        // move the answer back to the local zone.
        let zone_shift = clock.and_then(|p| toks.get(p.start + p.len - 1)).and_then(|t| zone_hours(&t.low)).map_or(0, |h| i64::from(h) * 3600 - i64::from(local_secs));
        let zone_now = now.checked_add_signed(Duration::seconds(zone_shift)).unwrap_or(now);
        let part = first(|f| matches!(f, Found::Part(..)));
        let d = date.map(|p| if let Found::Date(d) = p.found { d } else { DateSpec::Today });
        let c = clock.map(|p| if let Found::Clock(c) = p.found { c } else { Clock { h: 0, m: 0, ambiguous: false, next_day: false } });
        let pt = part.map(|p| if let Found::Part(x, t) = p.found { (x, t) } else { (Part::Morning, false) });
        let due = combine(zone_now, d, c, pt).and_then(|z| z.checked_sub_signed(Duration::seconds(zone_shift)));
        (due, [date, clock, part].into_iter().flatten().copied().collect())
    };
    let Some(due) = due else { return plain() };

    // The text: the words not used by the time, minus connectors left next to it.
    let mut gone = vec![false; toks.len()];
    for p in &used {
        for g in gone.iter_mut().skip(p.start).take(p.len) {
            *g = true;
        }
    }
    let mut k = toks.len();
    while k > 0 {
        let i = k - 1;
        if gone[i] {
            k -= 1;
            continue;
        }
        if is_connector(&toks[i].low) && i + 1 < toks.len() && gone[i + 1] {
            gone[i] = true;
            k -= 1;
        } else {
            break;
        }
    }
    // A leading "pada"/"at"/"on"/"by" next to a cut-out time.
    let mut lead = 0;
    while lead < toks.len() {
        if gone[lead] {
            lead += 1;
        } else if matches!(toks[lead].low.as_str(), "pada" | "at" | "on" | "by") && lead > 0 && gone[lead - 1] {
            gone[lead] = true;
            lead += 1;
        } else {
            break;
        }
    }
    let mut kept: Vec<&str> = toks.iter().zip(&gone).filter(|(_, g)| !**g).map(|(t, _)| t.raw).collect();
    kept.extend(words.iter().skip(toks.len()));
    let title = kept.join(" ");
    let title = title.trim_matches(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '-' | '\u{2013}' | '\u{2014}')).to_string();
    let title = if title.is_empty() { words.join(" ") } else { title };
    Parsed { title, due: Some(due) }
}

// ---- display ----------------------------------------------------------------------------------

const DAYS_ID: [&str; 7] = ["Sen", "Sel", "Rab", "Kam", "Jum", "Sab", "Min"];
const DAYS_EN: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS_ID: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "Mei", "Jun", "Jul", "Agu", "Sep", "Okt", "Nov", "Des"];
const MONTHS_EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// A short label for the list: `Hari ini 15.00`, `Besok 09.00`, `Sen 09.00`, `5 Des 14.30`.
pub fn label(due: NaiveDateTime, now: NaiveDateTime, english: bool) -> String {
    let sep = if english { ':' } else { '.' };
    let clock = format!("{:02}{sep}{:02}", due.hour(), due.minute());
    let days = due.date().signed_duration_since(now.date()).num_days();
    let (d, mo) = (due.day(), due.month0() as usize);
    let tr = |id: &'static str, en: &'static str| if english { en } else { id };
    match days {
        0 => format!("{} {clock}", tr("Hari ini", "Today")),
        1 => format!("{} {clock}", tr("Besok", "Tomorrow")),
        -1 => format!("{} {clock}", tr("Kemarin", "Yesterday")),
        2..=6 => {
            let names = if english { &DAYS_EN } else { &DAYS_ID };
            format!("{} {clock}", names[due.weekday().num_days_from_monday() as usize])
        }
        _ => {
            let months = if english { &MONTHS_EN } else { &MONTHS_ID };
            if due.year() != now.year() { format!("{d} {} {} {clock}", months[mo], due.year()) } else { format!("{d} {} {clock}", months[mo]) }
        }
    }
}
