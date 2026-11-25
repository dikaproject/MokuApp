//! `RRULE` for ICS events: the subset people actually use.
//!
//! * `FREQ` = DAILY, WEEKLY (with `BYDAY`, `WKST`), MONTHLY (`BYMONTHDAY`, or `BYDAY` with an
//!   ordinal such as `2TU` / `-1FR`), YEARLY (`BYMONTH`), plus `INTERVAL`, `COUNT`, `UNTIL`.
//! * Anything else (`BYSETPOS`, `BYWEEKNO`, `BYYEARDAY`, `HOURLY` …) makes [`parse`] return `None`:
//!   the event then shows only its first instance, which is better than showing wrong dates.
//!
//! The generator works on wall-clock times ([`each`]); the caller maps them to UTC with the zone of
//! the event, so a weekly 09:00 meeting stays at 09:00 across a daylight saving change.

use chrono::{Datelike, Days, NaiveDate, NaiveDateTime, Weekday};

use super::tz::{last_day_of_month, nth_weekday, parse_byday};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Until {
    /// `UNTIL=20261231`: that whole day.
    Date(NaiveDate),
    /// `UNTIL=20261231T235959Z`.
    Utc(NaiveDateTime),
    /// `UNTIL=20261231T235959`: wall-clock time of the event.
    Floating(NaiveDateTime),
}

impl Until {
    pub fn year(&self) -> i32 {
        match self {
            Until::Date(d) => d.year(),
            Until::Utc(t) | Until::Floating(t) => t.year(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RRule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<Until>,
    /// `(ordinal, weekday)`; ordinal 0 = every such weekday.
    pub by_day: Vec<(i8, Weekday)>,
    pub by_monthday: Vec<i8>,
    pub by_month: Vec<u32>,
    pub wkst: Weekday,
}

/// Most periods (days, weeks, months, years) one expansion walks through: an old daily event with
/// no end still finishes in a moment.
pub const MAX_PERIODS: u32 = 60_000;

fn weekday_of(code: &str) -> Option<Weekday> {
    parse_byday(code).filter(|(n, _)| *n == 0).map(|(_, w)| w)
}

/// Parse the value of an `RRULE` line (`FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261231T000000Z`).
pub fn parse(value: &str) -> Option<RRule> {
    let mut freq = None;
    let mut r = RRule { freq: Freq::Daily, interval: 1, count: None, until: None, by_day: vec![], by_monthday: vec![], by_month: vec![], wkst: Weekday::Mon };
    for part in value.split(';') {
        let Some((k, v)) = part.split_once('=') else { continue };
        let v = v.trim();
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Some(Freq::Daily),
                    "WEEKLY" => Some(Freq::Weekly),
                    "MONTHLY" => Some(Freq::Monthly),
                    "YEARLY" => Some(Freq::Yearly),
                    _ => return None,
                }
            }
            "INTERVAL" => r.interval = v.parse::<u32>().ok().filter(|n| (1..=1000).contains(n)).unwrap_or(1),
            "COUNT" => r.count = v.parse::<u32>().ok().filter(|n| *n > 0).map(|n| n.min(100_000)),
            "UNTIL" => r.until = parse_until(v),
            "BYDAY" => r.by_day = v.split(',').filter_map(parse_byday).collect(),
            "BYMONTHDAY" => r.by_monthday = v.split(',').filter_map(|d| d.trim().parse::<i8>().ok()).filter(|d| *d != 0 && d.abs() <= 31).collect(),
            "BYMONTH" => r.by_month = v.split(',').filter_map(|m| m.trim().parse::<u32>().ok()).filter(|m| (1..=12).contains(m)).collect(),
            "WKST" => r.wkst = weekday_of(v).unwrap_or(Weekday::Mon),
            // Modifiers this generator cannot honour.
            "BYSETPOS" | "BYWEEKNO" | "BYYEARDAY" | "BYHOUR" | "BYMINUTE" | "BYSECOND" => return None,
            _ => {}
        }
    }
    r.freq = freq?;
    r.by_month.sort_unstable();
    r.by_month.dedup();
    Some(r)
}

fn parse_until(v: &str) -> Option<Until> {
    let v = v.trim();
    let (date, time) = match v.split_once(['T', 't']) {
        Some((d, t)) => (d, Some(t)),
        None => (v, None),
    };
    let d = super::ics::parse_date(date)?;
    match time {
        None => Some(Until::Date(d)),
        Some(t) => {
            let utc = t.ends_with(['Z', 'z']);
            let naive = super::ics::parse_naive(&format!("{date}T{}", t.trim_end_matches(['Z', 'z'])))?;
            Some(if utc { Until::Utc(naive) } else { Until::Floating(naive) })
        }
    }
}

/// The days of `year`-`month` the rule selects, ascending (see the module comment).
fn month_days(r: &RRule, year: i32, month: u32, default_day: u32) -> Vec<NaiveDate> {
    let Some(last) = last_day_of_month(year, month).map(|d| d.day()) else { return vec![] };
    let mut days: Vec<u32> = vec![];
    let date = |d: u32| NaiveDate::from_ymd_opt(year, month, d);
    let monthday_ok = |d: u32| r.by_monthday.iter().any(|&m| if m > 0 { d == m as u32 } else { d as i32 == last as i32 + 1 + m as i32 });
    if !r.by_day.is_empty() {
        for &(n, wd) in &r.by_day {
            if n == 0 {
                days.extend((1..=last).filter(|&d| date(d).is_some_and(|x| x.weekday() == wd)));
            } else if let Some(d) = nth_weekday(year, month, n, wd) {
                days.push(d.day());
            }
        }
        if !r.by_monthday.is_empty() {
            days.retain(|&d| monthday_ok(d));
        }
    } else if !r.by_monthday.is_empty() {
        days.extend((1..=last).filter(|&d| monthday_ok(d)));
    } else if default_day <= last {
        days.push(default_day);
    }
    days.sort_unstable();
    days.dedup();
    days.into_iter().filter_map(date).collect()
}

/// Whether a day passes the `BYxxx` limits of a DAILY rule.
fn daily_ok(r: &RRule, d: NaiveDate) -> bool {
    if !r.by_month.is_empty() && !r.by_month.contains(&d.month()) {
        return false;
    }
    if !r.by_day.is_empty() && !r.by_day.iter().any(|&(_, wd)| wd == d.weekday()) {
        return false;
    }
    if !r.by_monthday.is_empty() {
        let last = last_day_of_month(d.year(), d.month()).map_or(31, |l| l.day()) as i32;
        let day = d.day() as i32;
        if !r.by_monthday.iter().any(|&m| if m > 0 { day == m as i32 } else { day == last + 1 + m as i32 }) {
            return false;
        }
    }
    true
}

/// Call `f` with the wall-clock start of every occurrence, in order, beginning at `dtstart`.
/// Stops when `f` returns `false`, after `COUNT` occurrences, once an occurrence is later than
/// `stop_after`, or after [`MAX_PERIODS`] periods. `UNTIL` is for the caller to check in `f`
/// (it needs the event's zone).
pub fn each(rule: &RRule, dtstart: NaiveDateTime, stop_after: NaiveDateTime, f: &mut dyn FnMut(NaiveDateTime) -> bool) {
    let time = dtstart.time();
    let d0 = dtstart.date();
    let limit = stop_after.date();
    let mut n = 0u32;
    // Returns false when the expansion is over.
    let mut emit = |d: NaiveDate| -> bool {
        let t = d.and_time(time);
        if t > stop_after || rule.count.is_some_and(|c| n >= c) {
            return false;
        }
        n += 1;
        f(t)
    };
    let step = rule.interval as u64;
    match rule.freq {
        Freq::Daily => {
            for k in 0..MAX_PERIODS as u64 {
                let Some(d) = d0.checked_add_days(Days::new(k * step)) else { return };
                if d > limit {
                    return;
                }
                if daily_ok(rule, d) && !emit(d) {
                    return;
                }
            }
        }
        Freq::Weekly => {
            let back = (d0.weekday().num_days_from_monday() + 7 - rule.wkst.num_days_from_monday()) % 7;
            let Some(week0) = d0.checked_sub_days(Days::new(back as u64)) else { return };
            let mut offsets: Vec<u32> = if rule.by_day.is_empty() { vec![back] } else { rule.by_day.iter().map(|&(_, wd)| (wd.num_days_from_monday() + 7 - rule.wkst.num_days_from_monday()) % 7).collect() };
            offsets.sort_unstable();
            offsets.dedup();
            for k in 0..MAX_PERIODS as u64 {
                let Some(ws) = week0.checked_add_days(Days::new(k * step * 7)) else { return };
                if ws > limit {
                    return;
                }
                for &o in &offsets {
                    let Some(d) = ws.checked_add_days(Days::new(o as u64)) else { return };
                    if d < d0 || (!rule.by_month.is_empty() && !rule.by_month.contains(&d.month())) {
                        continue;
                    }
                    if !emit(d) {
                        return;
                    }
                }
            }
        }
        Freq::Monthly => {
            for k in 0..MAX_PERIODS as i64 {
                let months = d0.year() as i64 * 12 + d0.month0() as i64 + k * step as i64;
                let (y, m) = ((months.div_euclid(12)) as i32, months.rem_euclid(12) as u32 + 1);
                let Some(first) = NaiveDate::from_ymd_opt(y, m, 1) else { return };
                if first > limit {
                    return;
                }
                if !rule.by_month.is_empty() && !rule.by_month.contains(&m) {
                    continue;
                }
                for d in month_days(rule, y, m, d0.day()) {
                    if d >= d0 && !emit(d) {
                        return;
                    }
                }
            }
        }
        Freq::Yearly => {
            let months: Vec<u32> = if rule.by_month.is_empty() { vec![d0.month()] } else { rule.by_month.clone() };
            for k in 0..MAX_PERIODS as i64 {
                let y = d0.year() as i64 + k * step as i64;
                if y > limit.year() as i64 {
                    return;
                }
                for &m in &months {
                    for d in month_days(rule, y as i32, m, d0.day()) {
                        if d >= d0 && !emit(d) {
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i32, m: u32, d: u32, h: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, 0, 0).unwrap()
    }

    /// Expand `rule` from `start` up to `until`, as `YYYY-MM-DD` strings.
    fn days(rule: &str, start: NaiveDateTime, until: NaiveDateTime) -> Vec<String> {
        let r = parse(rule).unwrap_or_else(|| panic!("rule {rule}"));
        let mut out = vec![];
        each(&r, start, until, &mut |t| {
            out.push(t.format("%Y-%m-%d").to_string());
            out.len() < 500
        });
        out
    }

    #[test]
    fn parses_the_listed_subset_and_refuses_the_rest() {
        let r = parse("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;COUNT=6;WKST=SU").unwrap();
        assert_eq!((r.freq, r.interval, r.count, r.wkst), (Freq::Weekly, 2, Some(6), Weekday::Sun));
        assert_eq!(r.by_day, vec![(0, Weekday::Mon), (0, Weekday::Wed)]);
        assert_eq!(parse("freq=monthly;bymonthday=1,-1").unwrap().by_monthday, vec![1, -1]);
        assert_eq!(parse("FREQ=MONTHLY;BYDAY=2TU").unwrap().by_day, vec![(2, Weekday::Tue)]);
        assert!(matches!(parse("FREQ=DAILY;UNTIL=20261231").unwrap().until, Some(Until::Date(_))));
        assert!(matches!(parse("FREQ=DAILY;UNTIL=20261231T235959Z").unwrap().until, Some(Until::Utc(_))));
        assert!(matches!(parse("FREQ=DAILY;UNTIL=20261231T235959").unwrap().until, Some(Until::Floating(_))));
        assert!(parse("FREQ=HOURLY").is_none());
        assert!(parse("FREQ=MONTHLY;BYDAY=MO;BYSETPOS=-1").is_none());
        assert!(parse("BYDAY=MO").is_none(), "FREQ is required");
        assert!(parse("").is_none());
        assert!(parse("garbage").is_none());
        // Bad numbers fall back instead of failing the whole rule.
        let r = parse("FREQ=DAILY;INTERVAL=0;COUNT=x;BYDAY=XX,FR").unwrap();
        assert_eq!((r.interval, r.count, r.by_day.len()), (1, None, 1));
    }

    #[test]
    fn daily_with_interval_and_count() {
        let d = days("FREQ=DAILY;INTERVAL=2;COUNT=4", dt(2026, 11, 24, 9), dt(2027, 1, 1, 0));
        assert_eq!(d, ["2026-11-24", "2026-11-26", "2026-11-28", "2026-11-30"]);
    }

    #[test]
    fn weekly_by_day_follows_the_week_start() {
        // Mon/Wed every week.
        let d = days("FREQ=WEEKLY;BYDAY=MO,WE", dt(2026, 11, 2, 9), dt(2026, 11, 17, 0));
        assert_eq!(d, ["2026-11-02", "2026-11-04", "2026-11-09", "2026-11-11", "2026-11-16"]);
        // No BYDAY: the weekday of DTSTART.
        let d = days("FREQ=WEEKLY", dt(2026, 11, 4, 9), dt(2026, 11, 20, 0));
        assert_eq!(d, ["2026-11-04", "2026-11-11", "2026-11-18"]);
        // Every other week; Sunday-based weeks put Sunday 8 Nov with Monday 9 Nov.
        let d = days("FREQ=WEEKLY;INTERVAL=2;BYDAY=SU,MO;WKST=SU", dt(2026, 11, 1, 9), dt(2026, 12, 1, 0));
        assert_eq!(d, ["2026-11-01", "2026-11-02", "2026-11-15", "2026-11-16", "2026-11-29", "2026-11-30"]);
        // A DTSTART in the middle of the week skips the earlier days of that week.
        let d = days("FREQ=WEEKLY;BYDAY=MO,FR", dt(2026, 11, 4, 9), dt(2026, 11, 14, 0));
        assert_eq!(d, ["2026-11-06", "2026-11-09", "2026-11-13"]);
    }

    #[test]
    fn monthly_by_day_of_month_skips_months_without_it() {
        let d = days("FREQ=MONTHLY", dt(2026, 1, 31, 9), dt(2026, 6, 1, 0));
        assert_eq!(d, ["2026-01-31", "2026-03-31", "2026-05-31"]);
        let d = days("FREQ=MONTHLY;BYMONTHDAY=15,-1", dt(2026, 1, 15, 9), dt(2026, 4, 1, 0));
        assert_eq!(d, ["2026-01-15", "2026-01-31", "2026-02-15", "2026-02-28", "2026-03-15", "2026-03-31"]);
        let d = days("FREQ=MONTHLY;INTERVAL=3;BYMONTHDAY=1", dt(2026, 1, 1, 9), dt(2026, 12, 31, 0));
        assert_eq!(d, ["2026-01-01", "2026-04-01", "2026-07-01", "2026-10-01"]);
    }

    #[test]
    fn monthly_by_ordinal_weekday() {
        let d = days("FREQ=MONTHLY;BYDAY=2TU", dt(2026, 1, 13, 9), dt(2026, 4, 1, 0));
        assert_eq!(d, ["2026-01-13", "2026-02-10", "2026-03-10"]);
        let d = days("FREQ=MONTHLY;BYDAY=-1FR", dt(2026, 1, 30, 9), dt(2026, 4, 1, 0));
        assert_eq!(d, ["2026-01-30", "2026-02-27", "2026-03-27"]);
        let d = days("FREQ=MONTHLY;BYDAY=1MO", dt(2026, 2, 2, 9), dt(2026, 3, 31, 0));
        assert_eq!(d, ["2026-02-02", "2026-03-02"]);
    }

    #[test]
    fn yearly_defaults_to_the_start_date() {
        let d = days("FREQ=YEARLY", dt(2024, 2, 29, 9), dt(2033, 1, 1, 0));
        assert_eq!(d, ["2024-02-29", "2028-02-29", "2032-02-29"], "leap day only in leap years");
        let d = days("FREQ=YEARLY;BYMONTH=3;BYDAY=2SU", dt(2026, 3, 8, 2), dt(2028, 12, 31, 0));
        assert_eq!(d, ["2026-03-08", "2027-03-14", "2028-03-12"]);
    }

    #[test]
    fn count_includes_the_start_and_bounds_the_run() {
        let d = days("FREQ=DAILY;COUNT=3", dt(2026, 11, 24, 9), dt(2030, 1, 1, 0));
        assert_eq!(d.len(), 3);
        // The window end stops an endless rule.
        let d = days("FREQ=DAILY", dt(2026, 11, 24, 9), dt(2026, 12, 8, 9));
        assert_eq!(d.len(), 15, "24 Nov .. 8 Dec inclusive");
    }

    #[test]
    fn an_old_endless_rule_finishes_quickly() {
        let started = std::time::Instant::now();
        let d = days("FREQ=DAILY", dt(1990, 1, 1, 9), dt(1990, 1, 3, 9));
        assert_eq!(d.len(), 3);
        // A rule that can never match ends at the period cap, not in an endless loop.
        let r = parse("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30").unwrap();
        let mut hits = 0;
        each(&r, dt(2026, 2, 1, 9), dt(2200, 1, 1, 0), &mut |_| {
            hits += 1;
            true
        });
        assert_eq!(hits, 0);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn nonsense_never_panics() {
        for rule in ["FREQ=DAILY;BYMONTHDAY=99", "FREQ=WEEKLY;BYDAY=", "FREQ=MONTHLY;BYMONTH=13", "FREQ=YEARLY;INTERVAL=999999999999", "FREQ=DAILY;UNTIL=zzzz", "=;=;="] {
            if let Some(r) = parse(rule) {
                each(&r, dt(2026, 1, 1, 9), dt(2026, 12, 31, 0), &mut |_| true);
            }
        }
    }
}
