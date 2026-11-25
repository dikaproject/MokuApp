//! Time zones for ICS files, without a time zone database.
//!
//! Three sources, in this order (see [`super::ics`]): the `VTIMEZONE` block of the file itself
//! ([`Tz::from_vtimezone`]), a small built-in table of the common IANA and Windows zone names
//! ([`builtin`]), and finally "the zone is unknown": the caller reads the time as local time.
//!
//! A zone is a list of *observances* (when the UTC offset changes, and to what). To find the
//! offset for a wall-clock time, take the latest onset at or before it. During the one repeated
//! hour of a "fall back" the first (daylight) reading wins, as RFC 5545 asks.

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

/// Which day of the month an onset falls on.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Day {
    /// The `n`th weekday (`n` > 0) or the `n`th from the end (`n` < 0).
    Nth(i8, Weekday),
    Fixed(u32),
}

#[derive(Clone, Debug, PartialEq)]
enum Onset {
    /// A single change at this wall-clock time (the clock before the change).
    Once(NaiveDateTime),
    /// Every year from `from_year` on (until `until_year`, inclusive), at `secs` after midnight.
    Yearly { month: u32, day: Day, secs: u32, from_year: i32, until_year: Option<i32> },
}

#[derive(Clone, Debug, PartialEq)]
struct Observance {
    onset: Onset,
    /// UTC offset in effect from the onset on, in seconds east of UTC.
    offset: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tz {
    obs: Vec<Observance>,
    /// Offset for times before every onset.
    base: i32,
}

/// The `n`th (`n` > 0) or `n`th last (`n` < 0) `weekday` of a month.
pub fn nth_weekday(year: i32, month: u32, n: i8, weekday: Weekday) -> Option<NaiveDate> {
    if n == 0 {
        return None;
    }
    if n > 0 {
        let first = NaiveDate::from_ymd_opt(year, month, 1)?;
        let shift = (weekday.num_days_from_monday() + 7 - first.weekday().num_days_from_monday()) % 7;
        let d = first.checked_add_days(chrono::Days::new(shift as u64 + 7 * (n as u64 - 1)))?;
        (d.month() == month).then_some(d)
    } else {
        let last = last_day_of_month(year, month)?;
        let back = (last.weekday().num_days_from_monday() + 7 - weekday.num_days_from_monday()) % 7;
        let d = last.checked_sub_days(chrono::Days::new(back as u64 + 7 * (n.unsigned_abs() as u64 - 1)))?;
        (d.month() == month).then_some(d)
    }
}

pub fn last_day_of_month(year: i32, month: u32) -> Option<NaiveDate> {
    let (y, m) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    NaiveDate::from_ymd_opt(y, m, 1)?.pred_opt()
}

impl Onset {
    /// The onset in `year`, if it has one.
    fn in_year(&self, year: i32) -> Option<NaiveDateTime> {
        match self {
            Onset::Once(t) => (t.year() == year).then_some(*t),
            Onset::Yearly { month, day, secs, from_year, until_year } => {
                if year < *from_year || until_year.is_some_and(|u| year > u) {
                    return None;
                }
                let date = match day {
                    Day::Nth(n, wd) => nth_weekday(year, *month, *n, *wd)?,
                    Day::Fixed(d) => NaiveDate::from_ymd_opt(year, *month, *d)?,
                };
                Some(date.and_time(NaiveTime::from_num_seconds_from_midnight_opt(*secs % 86_400, 0)?))
            }
        }
    }
}

impl Tz {
    /// A zone that never changes.
    pub fn fixed(offset: i32) -> Tz {
        Tz { obs: vec![], base: offset }
    }

    /// Seconds east of UTC at the wall-clock time `local`.
    pub fn offset_at_local(&self, local: NaiveDateTime) -> i32 {
        let mut best: Option<(NaiveDateTime, i32)> = None;
        let y = local.year();
        for o in &self.obs {
            // A `Once` onset can lie any number of years back; yearly ones need this year and
            // the one before.
            let cands: [Option<NaiveDateTime>; 2] = match &o.onset {
                Onset::Once(t) => [Some(*t), None],
                yearly => [yearly.in_year(y), yearly.in_year(y - 1)],
            };
            for c in cands.into_iter().flatten() {
                if c <= local && best.is_none_or(|(b, _)| c > b) {
                    best = Some((c, o.offset));
                }
            }
        }
        best.map_or(self.base, |(_, off)| off)
    }

    /// The UTC time (Unix seconds) of a wall-clock time in this zone.
    pub fn local_to_utc(&self, local: NaiveDateTime) -> i64 {
        local.and_utc().timestamp() - self.offset_at_local(local) as i64
    }

    /// Build a zone from the lines of one `VTIMEZONE` block (without its own BEGIN / END):
    /// `STANDARD` / `DAYLIGHT` sub-blocks with `DTSTART`, `TZOFFSETTO`, `RRULE` and `RDATE`.
    /// `None` when nothing in it is usable.
    pub fn from_vtimezone(lines: &[super::ics::Line]) -> Option<Tz> {
        let mut obs = vec![];
        let mut cur: Option<Vec<&super::ics::Line>> = None;
        for l in lines {
            let v = l.value.trim().to_ascii_uppercase();
            match (l.name.as_str(), v.as_str()) {
                ("BEGIN", "STANDARD" | "DAYLIGHT") => cur = Some(vec![]),
                ("END", "STANDARD" | "DAYLIGHT") => {
                    if let Some(block) = cur.take() {
                        obs.extend(observances(&block));
                    }
                }
                _ => {
                    if let Some(b) = cur.as_mut() {
                        b.push(l);
                    }
                }
            }
        }
        if obs.is_empty() {
            return None;
        }
        // Before the first onset the zone used some earlier offset we cannot know; the offset of
        // the earliest observance is the best guess available.
        let earliest = obs.iter().min_by_key(|o| match &o.onset {
            Onset::Once(t) => *t,
            Onset::Yearly { from_year, .. } => NaiveDate::from_ymd_opt(*from_year, 1, 1).unwrap_or(NaiveDate::MIN).and_time(NaiveTime::MIN),
        });
        let base = earliest.map_or(0, |o| o.offset);
        Some(Tz { obs, base })
    }
}

/// Parse `+0700`, `-0530`, `+053000` into seconds.
pub fn parse_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => (1, s),
    };
    if !(rest.len() == 4 || rest.len() == 6) || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: i32 = rest[0..2].parse().ok()?;
    let m: i32 = rest[2..4].parse().ok()?;
    let sec: i32 = if rest.len() == 6 { rest[4..6].parse().ok()? } else { 0 };
    if h > 23 || m > 59 || sec > 59 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60 + sec))
}

fn weekday_code(s: &str) -> Option<Weekday> {
    Some(match s {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// `1SU`, `-1SU`, `SU` → (ordinal, weekday); ordinal 0 when absent.
pub fn parse_byday(s: &str) -> Option<(i8, Weekday)> {
    let s = s.trim().to_ascii_uppercase();
    if s.len() < 2 || !s.is_ascii() {
        return None;
    }
    let (num, wd) = s.split_at(s.len() - 2);
    let n: i8 = if num.is_empty() { 0 } else { num.trim_start_matches('+').parse().ok()? };
    Some((n, weekday_code(wd)?))
}

fn observances(block: &[&super::ics::Line]) -> Vec<Observance> {
    let get = |name: &str| block.iter().find(|l| l.name == name);
    let Some(offset) = get("TZOFFSETTO").and_then(|l| parse_offset(&l.value)) else { return vec![] };
    let Some(start) = get("DTSTART").and_then(|l| super::ics::parse_naive(&l.value)) else { return vec![] };
    let mut out = vec![];
    match get("RRULE").and_then(|l| super::rrule::parse(&l.value)) {
        Some(r) if r.freq == super::rrule::Freq::Yearly => {
            let month = r.by_month.first().copied().unwrap_or(start.month());
            let day = match (r.by_day.first(), r.by_monthday.first()) {
                (Some(&(n, wd)), _) if n != 0 => Day::Nth(n, wd),
                (_, Some(&d)) if d > 0 => Day::Fixed(d as u32),
                _ => Day::Fixed(start.day()),
            };
            let until_year = r.until.as_ref().map(|u| u.year());
            out.push(Observance { onset: Onset::Yearly { month, day, secs: start.time().num_seconds_from_midnight(), from_year: start.year(), until_year }, offset });
        }
        _ => out.push(Observance { onset: Onset::Once(start), offset }),
    }
    // Extra single changes (`RDATE:19700101T000000`).
    for l in block.iter().filter(|l| l.name == "RDATE") {
        for v in l.value.split(',') {
            if let Some(t) = super::ics::parse_naive(v) {
                out.push(Observance { onset: Onset::Once(t), offset });
            }
        }
    }
    out
}

// ---- built-in zones -----------------------------------------------------------------------------

const fn h(hours: i32) -> i32 {
    hours * 3600
}
const fn hm(hours: i32, minutes: i32) -> i32 {
    if hours < 0 { hours * 3600 - minutes * 60 } else { hours * 3600 + minutes * 60 }
}

/// How a built-in zone changes its clocks.
#[derive(Clone, Copy)]
enum Spec {
    /// No daylight saving time.
    Fixed(i32),
    /// North America: 2nd Sunday of March to 1st Sunday of November, 02:00 local.
    Us(i32),
    /// Europe: last Sunday of March to last Sunday of October, 01:00 UTC.
    Eu(i32),
    /// Southern Australia: 1st Sunday of October to 1st Sunday of April.
    Au(i32),
    /// New Zealand: last Sunday of September to 1st Sunday of April.
    Nz(i32),
}

fn yearly(month: u32, n: i8, wd: Weekday, secs: u32, from_year: i32, offset: i32) -> Observance {
    Observance { onset: Onset::Yearly { month, day: Day::Nth(n, wd), secs, from_year, until_year: None }, offset }
}

impl Spec {
    fn build(self) -> Tz {
        // Every daylight-saving zone starts from its standard offset, so old dates are right too.
        let std_of = |std: i32| Observance { onset: Onset::Once(NaiveDate::from_ymd_opt(1900, 1, 1).unwrap_or(NaiveDate::MIN).and_time(NaiveTime::MIN)), offset: std };
        match self {
            Spec::Fixed(o) => Tz::fixed(o),
            Spec::Us(std) => Tz { obs: vec![std_of(std), yearly(3, 2, Weekday::Sun, 2 * 3600, 2007, std + 3600), yearly(11, 1, Weekday::Sun, 2 * 3600, 2007, std)], base: std },
            // The change is at 01:00 UTC: 01:00 + std on the local clock before the spring
            // change, and 01:00 + std + 1 h (the clock reads summer time) before the autumn one.
            Spec::Eu(std) => Tz {
                obs: vec![
                    std_of(std),
                    yearly(3, -1, Weekday::Sun, (3600 + std).rem_euclid(86_400) as u32, 1996, std + 3600),
                    yearly(10, -1, Weekday::Sun, (3600 + std + 3600).rem_euclid(86_400) as u32, 1996, std),
                ],
                base: std,
            },
            Spec::Au(std) => Tz { obs: vec![std_of(std), yearly(10, 1, Weekday::Sun, 2 * 3600, 2008, std + 3600), yearly(4, 1, Weekday::Sun, 3 * 3600, 2008, std)], base: std },
            Spec::Nz(std) => Tz { obs: vec![std_of(std), yearly(9, -1, Weekday::Sun, 2 * 3600, 2007, std + 3600), yearly(4, 1, Weekday::Sun, 3 * 3600, 2008, std)], base: std },
        }
    }
}

use Spec::{Au, Eu, Fixed, Nz, Us};

/// Common IANA names and Windows names, lower case.
const ZONES: &[(&str, Spec)] = &[
    // UTC
    ("utc", Fixed(0)), ("etc/utc", Fixed(0)), ("gmt", Fixed(0)), ("etc/gmt", Fixed(0)), ("z", Fixed(0)), ("zulu", Fixed(0)),
    ("etc/zulu", Fixed(0)), ("universal", Fixed(0)), ("atlantic/reykjavik", Fixed(0)), ("africa/accra", Fixed(0)),
    // North America
    ("america/new_york", Us(h(-5))), ("america/detroit", Us(h(-5))), ("america/toronto", Us(h(-5))), ("us/eastern", Us(h(-5))),
    ("canada/eastern", Us(h(-5))), ("america/indiana/indianapolis", Us(h(-5))),
    ("america/chicago", Us(h(-6))), ("america/winnipeg", Us(h(-6))), ("us/central", Us(h(-6))), ("canada/central", Us(h(-6))),
    ("america/denver", Us(h(-7))), ("america/edmonton", Us(h(-7))), ("america/boise", Us(h(-7))), ("us/mountain", Us(h(-7))),
    ("canada/mountain", Us(h(-7))),
    ("america/los_angeles", Us(h(-8))), ("america/vancouver", Us(h(-8))), ("america/tijuana", Us(h(-8))), ("us/pacific", Us(h(-8))),
    ("canada/pacific", Us(h(-8))),
    ("america/anchorage", Us(h(-9))), ("us/alaska", Us(h(-9))),
    ("america/halifax", Us(h(-4))), ("canada/atlantic", Us(h(-4))),
    ("america/st_johns", Us(hm(-3, 30))), ("canada/newfoundland", Us(hm(-3, 30))),
    ("america/phoenix", Fixed(h(-7))), ("us/arizona", Fixed(h(-7))), ("pacific/honolulu", Fixed(h(-10))), ("us/hawaii", Fixed(h(-10))),
    ("america/regina", Fixed(h(-6))), ("america/mexico_city", Fixed(h(-6))), ("america/guatemala", Fixed(h(-6))),
    ("america/costa_rica", Fixed(h(-6))), ("america/bogota", Fixed(h(-5))), ("america/lima", Fixed(h(-5))),
    ("america/panama", Fixed(h(-5))), ("america/jamaica", Fixed(h(-5))), ("america/caracas", Fixed(h(-4))),
    ("america/puerto_rico", Fixed(h(-4))), ("america/la_paz", Fixed(h(-4))), ("america/sao_paulo", Fixed(h(-3))),
    ("america/argentina/buenos_aires", Fixed(h(-3))), ("america/buenos_aires", Fixed(h(-3))), ("america/montevideo", Fixed(h(-3))),
    // Europe and Africa
    ("europe/london", Eu(0)), ("europe/dublin", Eu(0)), ("europe/lisbon", Eu(0)), ("atlantic/canary", Eu(0)), ("gb", Eu(0)),
    ("europe/paris", Eu(h(1))), ("europe/berlin", Eu(h(1))), ("europe/madrid", Eu(h(1))), ("europe/rome", Eu(h(1))),
    ("europe/amsterdam", Eu(h(1))), ("europe/brussels", Eu(h(1))), ("europe/vienna", Eu(h(1))), ("europe/zurich", Eu(h(1))),
    ("europe/stockholm", Eu(h(1))), ("europe/oslo", Eu(h(1))), ("europe/copenhagen", Eu(h(1))), ("europe/prague", Eu(h(1))),
    ("europe/warsaw", Eu(h(1))), ("europe/budapest", Eu(h(1))), ("europe/belgrade", Eu(h(1))), ("europe/zagreb", Eu(h(1))),
    ("europe/luxembourg", Eu(h(1))), ("europe/monaco", Eu(h(1))), ("europe/malta", Eu(h(1))), ("europe/bratislava", Eu(h(1))),
    ("europe/ljubljana", Eu(h(1))), ("europe/sarajevo", Eu(h(1))), ("europe/skopje", Eu(h(1))), ("europe/tirane", Eu(h(1))),
    ("europe/athens", Eu(h(2))), ("europe/helsinki", Eu(h(2))), ("europe/kiev", Eu(h(2))), ("europe/kyiv", Eu(h(2))),
    ("europe/bucharest", Eu(h(2))), ("europe/sofia", Eu(h(2))), ("europe/riga", Eu(h(2))), ("europe/tallinn", Eu(h(2))),
    ("europe/vilnius", Eu(h(2))),
    ("europe/istanbul", Fixed(h(3))), ("europe/moscow", Fixed(h(3))), ("europe/minsk", Fixed(h(3))),
    ("africa/johannesburg", Fixed(h(2))), ("africa/lagos", Fixed(h(1))), ("africa/nairobi", Fixed(h(3))),
    ("africa/addis_ababa", Fixed(h(3))),
    // Asia
    ("asia/jakarta", Fixed(h(7))), ("asia/pontianak", Fixed(h(7))), ("asia/makassar", Fixed(h(8))), ("asia/jayapura", Fixed(h(9))),
    ("asia/singapore", Fixed(h(8))), ("asia/kuala_lumpur", Fixed(h(8))), ("asia/manila", Fixed(h(8))),
    ("asia/hong_kong", Fixed(h(8))), ("asia/shanghai", Fixed(h(8))), ("asia/chongqing", Fixed(h(8))), ("asia/taipei", Fixed(h(8))),
    ("asia/tokyo", Fixed(h(9))), ("asia/seoul", Fixed(h(9))), ("asia/kolkata", Fixed(hm(5, 30))), ("asia/calcutta", Fixed(hm(5, 30))),
    ("asia/colombo", Fixed(hm(5, 30))), ("asia/kathmandu", Fixed(hm(5, 45))), ("asia/dhaka", Fixed(h(6))),
    ("asia/bangkok", Fixed(h(7))), ("asia/ho_chi_minh", Fixed(h(7))), ("asia/saigon", Fixed(h(7))), ("asia/phnom_penh", Fixed(h(7))),
    ("asia/yangon", Fixed(hm(6, 30))), ("asia/dubai", Fixed(h(4))), ("asia/muscat", Fixed(h(4))), ("asia/baku", Fixed(h(4))),
    ("asia/tbilisi", Fixed(h(4))), ("asia/yerevan", Fixed(h(4))), ("asia/karachi", Fixed(h(5))), ("asia/tashkent", Fixed(h(5))),
    ("asia/tehran", Fixed(hm(3, 30))), ("asia/riyadh", Fixed(h(3))), ("asia/baghdad", Fixed(h(3))), ("asia/kuwait", Fixed(h(3))),
    ("asia/qatar", Fixed(h(3))),
    // Australia and the Pacific
    ("australia/sydney", Au(h(10))), ("australia/melbourne", Au(h(10))), ("australia/canberra", Au(h(10))),
    ("australia/hobart", Au(h(10))), ("australia/adelaide", Au(hm(9, 30))), ("australia/brisbane", Fixed(h(10))),
    ("australia/darwin", Fixed(hm(9, 30))), ("australia/perth", Fixed(h(8))), ("pacific/auckland", Nz(h(12))),
    ("pacific/fiji", Fixed(h(12))), ("pacific/guam", Fixed(h(10))),
    // Windows names
    ("greenwich standard time", Fixed(0)), ("gmt standard time", Eu(0)), ("w. europe standard time", Eu(h(1))),
    ("central europe standard time", Eu(h(1))), ("central european standard time", Eu(h(1))), ("romance standard time", Eu(h(1))),
    ("e. europe standard time", Eu(h(2))), ("gtb standard time", Eu(h(2))), ("fle standard time", Eu(h(2))),
    ("russian standard time", Fixed(h(3))), ("turkey standard time", Fixed(h(3))), ("arab standard time", Fixed(h(3))),
    ("arabian standard time", Fixed(h(4))), ("pakistan standard time", Fixed(h(5))), ("india standard time", Fixed(hm(5, 30))),
    ("sri lanka standard time", Fixed(hm(5, 30))), ("nepal standard time", Fixed(hm(5, 45))), ("bangladesh standard time", Fixed(h(6))),
    ("myanmar standard time", Fixed(hm(6, 30))), ("se asia standard time", Fixed(h(7))), ("china standard time", Fixed(h(8))),
    ("singapore standard time", Fixed(h(8))), ("taipei standard time", Fixed(h(8))), ("w. australia standard time", Fixed(h(8))),
    ("tokyo standard time", Fixed(h(9))), ("korea standard time", Fixed(h(9))), ("cen. australia standard time", Au(hm(9, 30))),
    ("aus central standard time", Fixed(hm(9, 30))), ("aus eastern standard time", Au(h(10))),
    ("e. australia standard time", Fixed(h(10))), ("tasmania standard time", Au(h(10))), ("new zealand standard time", Nz(h(12))),
    ("hawaiian standard time", Fixed(h(-10))), ("alaskan standard time", Us(h(-9))), ("pacific standard time", Us(h(-8))),
    ("us mountain standard time", Fixed(h(-7))), ("mountain standard time", Us(h(-7))), ("central standard time", Us(h(-6))),
    ("canada central standard time", Fixed(h(-6))), ("central america standard time", Fixed(h(-6))),
    ("central standard time (mexico)", Fixed(h(-6))), ("eastern standard time", Us(h(-5))), ("us eastern standard time", Us(h(-5))),
    ("sa pacific standard time", Fixed(h(-5))), ("atlantic standard time", Us(h(-4))), ("sa western standard time", Fixed(h(-4))),
    ("newfoundland standard time", Us(hm(-3, 30))), ("e. south america standard time", Fixed(h(-3))),
    ("argentina standard time", Fixed(h(-3))), ("sa eastern standard time", Fixed(h(-3))), ("south africa standard time", Fixed(h(2))),
    ("w. central africa standard time", Fixed(h(1))), ("e. africa standard time", Fixed(h(3))), ("iran standard time", Fixed(hm(3, 30))),
];

/// The built-in zone for an IANA or Windows name (`America/New_York`, `Eastern Standard Time`).
/// Quotes, a `/mozilla.org/20050126_1/` style prefix and letter case are ignored.
pub fn builtin(name: &str) -> Option<Tz> {
    let n = name.trim().trim_matches('"').trim().to_ascii_lowercase();
    if n.is_empty() || n.len() > 80 {
        return None;
    }
    let find = |k: &str| ZONES.iter().find(|(z, _)| *z == k).map(|(_, s)| s.build());
    if let Some(t) = find(&n) {
        return Some(t);
    }
    // `/mozilla.org/20050126_1/America/New_York`: the zone is the last two (or three) parts.
    if n.contains('/') {
        let parts: Vec<&str> = n.split('/').filter(|p| !p.is_empty()).collect();
        for take in [3usize, 2] {
            if parts.len() >= take {
                if let Some(t) = find(&parts[parts.len() - take..].join("/")) {
                    return Some(t);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::calendar::ics;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap()
    }

    fn off(name: &str, t: NaiveDateTime) -> i32 {
        builtin(name).unwrap_or_else(|| panic!("zone {name}")).offset_at_local(t)
    }

    #[test]
    fn us_zones_switch_on_the_second_sunday_of_march_and_first_of_november() {
        // 2026: DST starts Sunday 8 March, ends Sunday 1 November.
        assert_eq!(off("America/New_York", at(2026, 3, 8, 1, 59)), h(-5));
        assert_eq!(off("America/New_York", at(2026, 3, 8, 3, 0)), h(-4));
        assert_eq!(off("America/New_York", at(2026, 7, 15, 9, 0)), h(-4));
        assert_eq!(off("America/New_York", at(2026, 11, 1, 1, 30)), h(-4), "the repeated hour reads as daylight time first");
        assert_eq!(off("America/New_York", at(2026, 11, 1, 2, 30)), h(-5));
        assert_eq!(off("America/Los_Angeles", at(2026, 7, 1, 12, 0)), h(-7));
        assert_eq!(off("Pacific Standard Time", at(2026, 12, 1, 12, 0)), h(-8));
        assert_eq!(off("America/Phoenix", at(2026, 7, 1, 12, 0)), h(-7), "no DST in Arizona");
        assert_eq!(off("America/St_Johns", at(2026, 7, 1, 12, 0)), hm(-2, 30));
    }

    #[test]
    fn european_zones_change_at_one_utc() {
        assert_eq!(off("Europe/Berlin", at(2026, 3, 29, 1, 59)), h(1));
        assert_eq!(off("Europe/Berlin", at(2026, 3, 29, 3, 30)), h(2));
        assert_eq!(off("Europe/Berlin", at(2026, 10, 25, 2, 30)), h(2), "ambiguous hour: first reading");
        assert_eq!(off("Europe/Berlin", at(2026, 10, 25, 3, 30)), h(1));
        assert_eq!(off("Europe/London", at(2026, 3, 29, 0, 59)), 0);
        assert_eq!(off("Europe/London", at(2026, 3, 29, 2, 0)), h(1));
        assert_eq!(off("GMT Standard Time", at(2026, 8, 1, 12, 0)), h(1));
        assert_eq!(off("W. Europe Standard Time", at(2026, 1, 1, 12, 0)), h(1));
        assert_eq!(off("Europe/Athens", at(2026, 7, 1, 12, 0)), h(3));
    }

    #[test]
    fn southern_hemisphere_and_fixed_zones() {
        // Sydney: DST from the first Sunday of October (4 Oct 2026) to the first of April (5 Apr 2026).
        assert_eq!(off("Australia/Sydney", at(2026, 1, 15, 12, 0)), h(11));
        assert_eq!(off("Australia/Sydney", at(2026, 4, 5, 3, 30)), h(10));
        assert_eq!(off("Australia/Sydney", at(2026, 10, 4, 3, 0)), h(11));
        assert_eq!(off("AUS Eastern Standard Time", at(2026, 7, 1, 12, 0)), h(10));
        assert_eq!(off("Pacific/Auckland", at(2026, 12, 1, 12, 0)), h(13));
        assert_eq!(off("Asia/Jakarta", at(2026, 6, 1, 12, 0)), h(7));
        assert_eq!(off("SE Asia Standard Time", at(2026, 6, 1, 12, 0)), h(7));
        assert_eq!(off("Asia/Kolkata", at(2026, 6, 1, 12, 0)), hm(5, 30));
        assert_eq!(off("Tokyo Standard Time", at(2026, 6, 1, 12, 0)), h(9));
        assert_eq!(off("UTC", at(2026, 6, 1, 12, 0)), 0);
        // Before the rules existed the standard offset applies.
        assert_eq!(off("America/New_York", at(2001, 7, 1, 12, 0)), h(-5));
    }

    #[test]
    fn zone_names_are_forgiving_and_unknown_ones_are_none() {
        assert!(builtin("america/new_york").is_some());
        assert!(builtin("\"America/New_York\"").is_some());
        assert!(builtin("/mozilla.org/20050126_1/America/New_York").is_some());
        assert!(builtin("/softwarestudio.org/Tzfile/Europe/Paris").is_some());
        assert!(builtin("/mozilla.org/20050126_1/America/Argentina/Buenos_Aires").is_some());
        assert!(builtin("Mars/Olympus_Mons").is_none());
        assert!(builtin("").is_none());
        assert!(builtin(&"x".repeat(500)).is_none());
    }

    #[test]
    fn local_to_utc_applies_the_offset() {
        let ny = builtin("America/New_York").unwrap();
        assert_eq!(ny.local_to_utc(at(2026, 11, 30, 9, 0)), at(2026, 11, 30, 14, 0).and_utc().timestamp());
        assert_eq!(ny.local_to_utc(at(2026, 7, 15, 9, 0)), at(2026, 7, 15, 13, 0).and_utc().timestamp());
    }

    #[test]
    fn nth_weekday_counts_from_both_ends() {
        assert_eq!(nth_weekday(2026, 3, 2, Weekday::Sun), NaiveDate::from_ymd_opt(2026, 3, 8));
        assert_eq!(nth_weekday(2026, 10, -1, Weekday::Sun), NaiveDate::from_ymd_opt(2026, 10, 25));
        assert_eq!(nth_weekday(2026, 2, 5, Weekday::Mon), None, "no fifth Monday in Feb 2026");
        assert_eq!(nth_weekday(2026, 2, 0, Weekday::Mon), None);
        assert_eq!(last_day_of_month(2026, 2), NaiveDate::from_ymd_opt(2026, 2, 28));
        assert_eq!(last_day_of_month(2024, 2), NaiveDate::from_ymd_opt(2024, 2, 29));
        assert_eq!(last_day_of_month(2026, 12), NaiveDate::from_ymd_opt(2026, 12, 31));
    }

    #[test]
    fn offsets_and_byday_parse() {
        assert_eq!(parse_offset("+0700"), Some(h(7)));
        assert_eq!(parse_offset("-0530"), Some(-hm(5, 30)));
        assert_eq!(parse_offset("+053045"), Some(5 * 3600 + 30 * 60 + 45));
        assert_eq!(parse_offset("0700"), Some(h(7)));
        assert_eq!(parse_offset("+7"), None);
        assert_eq!(parse_offset("+2560"), None);
        assert_eq!(parse_offset(""), None);
        assert_eq!(parse_byday("MO"), Some((0, Weekday::Mon)));
        assert_eq!(parse_byday("2tu"), Some((2, Weekday::Tue)));
        assert_eq!(parse_byday("-1FR"), Some((-1, Weekday::Fri)));
        assert_eq!(parse_byday("XX"), None);
        assert_eq!(parse_byday("é"), None);
    }

    #[test]
    fn vtimezone_block_with_yearly_rules() {
        let cal = ics::parse(
            "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:Custom/Eastern\r\nBEGIN:DAYLIGHT\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\nDTSTART:19700308T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\nEND:DAYLIGHT\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nDTSTART:19701101T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\nEND:VCALENDAR\r\n",
        );
        let tz = cal.zone("Custom/Eastern").expect("the block is parsed");
        assert_eq!(tz.offset_at_local(at(2026, 7, 1, 12, 0)), h(-4));
        assert_eq!(tz.offset_at_local(at(2026, 12, 1, 12, 0)), h(-5));
        assert_eq!(tz.offset_at_local(at(2026, 3, 8, 3, 0)), h(-4));
    }

    #[test]
    fn vtimezone_without_rules_is_a_fixed_offset() {
        // What Outlook writes for a zone without daylight saving time.
        let cal = ics::parse(
            "BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:Singapore Standard Time\nBEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0800\nTZOFFSETTO:+0800\nEND:STANDARD\nEND:VTIMEZONE\nEND:VCALENDAR\n",
        );
        assert_eq!(cal.zone("Singapore Standard Time").unwrap().offset_at_local(at(2026, 5, 5, 5, 5)), h(8));
    }
}
