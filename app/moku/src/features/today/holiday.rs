//! Indonesian public holidays and cuti bersama (`today.holidays`).
//!
//! * **Source**: the public Google holiday calendar for Indonesia (an ICS file, no account, no
//!   key), read with the calendar feature's own ICS reader ([`crate::features::calendar::ics`]) and
//!   [`crate::net`]. It lists three kinds of days; only "Hari libur nasional" (national holidays and
//!   cuti bersama) are days off, the "Perayaan" ones (Valentine's, Malam Tahun Baru, ...) are not
//!   and are dropped. Fetched when the island starts and every 12 hours, only while the switch is
//!   on; the raw file is dropped after parsing, a few dozen small records are kept.
//! * **Hari ini**: a red badge under the date on a holiday, and a line with the next one.
//! * **"Besok libur"**: from 17:00 the evening before a holiday that is not a Sunday, once per
//!   holiday (the date is remembered in `holiday-eve.txt`, so restarting does not repeat it).
//!
//! Test hook: `mokuctl debug click today.sample_holiday[:today|:eve]` (invented dates, kept).

use std::time::Duration;

use chrono::{Datelike, Days, Local, NaiveDate, NaiveDateTime, Timelike, Weekday};

use crate::activity::Activity;
use crate::app::{App, Redraw, TimerId};
use crate::features::calendar::ics;
use crate::i18n::{self, tr, tr_s};
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::*;

/// `id.indonesian#holiday@group.v.calendar.google.com`, the calendar Google shows Indonesian users.
pub const ICS_URL: &str = "https://calendar.google.com/calendar/ical/id.indonesian%23holiday%40group.v.calendar.google.com/public/basic.ics";
const REFRESH: Duration = Duration::from_secs(12 * 3600);
const RETRY: Duration = Duration::from_secs(5 * 60);
const MAX_RETRY: Duration = Duration::from_secs(3600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// The evening starts here.
const EVE_FROM_HOUR: u32 = 17;
/// Between the volume OSD (70) and a running timer (50).
const EVE_PRIORITY: u8 = 52;
const EVE_SECS: f32 = 12.0;
/// Test-hook pills stay long enough to screenshot.
const SAMPLE_SECS: f32 = 45.0;
const MAX_HOLIDAYS: usize = 400;
const EVE_FILE: &str = "holiday-eve.txt";

// ---- model (pure) -----------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holiday {
    pub date: NaiveDate,
    pub name: String,
    /// "Cuti Bersama ...": a day off by government decree, not a holiday of its own.
    pub cuti: bool,
    /// "(belum pasti)": the date may still change.
    pub tentative: bool,
}

/// The days off in an ICS text, from `from` on, in date order. Anything that is not an all-day
/// "Hari libur nasional" event is ignored; a broken file gives an empty list.
pub fn parse(text: &str, from: NaiveDate) -> Vec<Holiday> {
    let mut out: Vec<Holiday> = vec![];
    let (mut date, mut name, mut desc, mut inside) = (None, String::new(), String::new(), false);
    for raw in ics::unfold(text) {
        let Some(line) = ics::parse_line(&raw) else { continue };
        match (line.name.as_str(), line.value.as_str()) {
            ("BEGIN", v) if v.eq_ignore_ascii_case("VEVENT") => {
                inside = true;
                (date, name, desc) = (None, String::new(), String::new());
            }
            ("END", v) if v.eq_ignore_ascii_case("VEVENT") => {
                if inside && let Some(d) = date.filter(|d| *d >= from) {
                    let national = desc.trim_start().to_lowercase().starts_with("hari libur nasional");
                    let summary = name.trim();
                    if national && !summary.is_empty() {
                        let lower = summary.to_lowercase();
                        let tentative = lower.contains("(belum pasti)") || desc.to_lowercase().contains("tentatif");
                        let shown = summary.replace("(belum pasti)", "").replace("(Belum pasti)", "");
                        out.push(Holiday { date: d, name: shown.split_whitespace().collect::<Vec<_>>().join(" "), cuti: lower.starts_with("cuti bersama"), tentative });
                    }
                }
                inside = false;
            }
            _ if !inside => {}
            ("DTSTART", v) => date = ics::parse_date(v),
            ("SUMMARY", v) => name = ics::unescape(v),
            ("DESCRIPTION", v) => desc = ics::unescape(v),
            _ => {}
        }
        if out.len() > MAX_HOLIDAYS * 4 {
            break;
        }
    }
    out.sort_by(|a, b| (a.date, a.cuti, &a.name).cmp(&(b.date, b.cuti, &b.name)));
    out.dedup_by(|a, b| a.date == b.date && a.name == b.name);
    out.truncate(MAX_HOLIDAYS);
    out
}

/// The holiday on `date` (a national holiday before a cuti bersama if both fall on it).
pub fn on(list: &[Holiday], date: NaiveDate) -> Option<&Holiday> {
    list.iter().find(|h| h.date == date)
}

/// The first holiday after `date`.
pub fn next_after(list: &[Holiday], date: NaiveDate) -> Option<&Holiday> {
    list.iter().find(|h| h.date > date)
}

/// The holiday to warn about at `now`: from the evening on, tomorrow's. A holiday on a Sunday
/// is no extra day off, so it is not announced.
pub fn eve(list: &[Holiday], now: NaiveDateTime) -> Option<&Holiday> {
    if now.hour() < EVE_FROM_HOUR {
        return None;
    }
    let tomorrow = now.date().checked_add_days(Days::new(1))?;
    if tomorrow.weekday() == Weekday::Sun {
        return None;
    }
    on(list, tomorrow)
}

/// The longest the check sleeps: the list is read again every [`REFRESH`] anyway, and a PC that slept
/// through the timer is looked at again within this.
const MAX_SLEEP_SECS: i64 = 12 * 3600;

/// Seconds until the next moment the evening check can have something to say: the start of the evening
/// (17:00:01) before the next day off that is not a Sunday. `None` when no such evening is ahead (nothing
/// is armed: the next refresh of the list looks again). Never more than [`MAX_SLEEP_SECS`].
fn secs_to_next_check(list: &[Holiday], now: NaiveDateTime) -> Option<i64> {
    let next = list
        .iter()
        .filter(|h| h.date.weekday() != Weekday::Sun)
        .filter_map(|h| h.date.checked_sub_days(Days::new(1))?.and_hms_opt(EVE_FROM_HOUR, 0, 1))
        .filter(|at| *at > now)
        .min()?;
    Some(next.signed_duration_since(now).num_seconds().clamp(5, MAX_SLEEP_SECS))
}

// ---- state ------------------------------------------------------------------------------------

#[derive(Default)]
pub struct HolidayState {
    list: Vec<Holiday>,
    loaded: bool,
    failed: bool,
    /// The holiday the evening pill was last shown for.
    eve_shown: Option<NaiveDate>,
    eve_loaded: bool,
    timer: Option<TimerId>,
    /// Invented data from the test hook: kept, and shown even when the switch is off.
    sample: bool,
}

pub fn register(r: &mut Registry) {
    r.add_state(HolidayState::default());
    r.add_service(ServiceSpec { name: "holiday", enabled: |c| c.today.holidays, run: service });
}

/// The config changed (called from the Hari ini feature's `on_config`).
pub fn on_config(app: &mut App) {
    let on = app.config().today.holidays;
    let st = app.state_mut::<HolidayState>();
    if !on && !st.sample {
        st.list.clear();
        st.loaded = false;
        st.failed = false;
        if let Some(t) = st.timer.take() {
            app.cancel_timer(t);
        }
        app.clear_activity("today.holiday");
    } else {
        check(app);
    }
    app.request_redraw(Redraw::Tab("today"));
}

/// "Hapus semua data".
pub fn on_data_cleared(app: &mut App) {
    app.cancel_save("today.eve");
    let st = app.state_mut::<HolidayState>();
    st.eve_shown = None;
    st.eve_loaded = true;
    app.request_redraw(Redraw::Tab("today"));
}

fn enabled(app: &App) -> bool {
    app.config().today.holidays || app.state::<HolidayState>().sample
}

fn today() -> NaiveDate {
    Local::now().date_naive()
}

// ---- service ----------------------------------------------------------------------------------

fn service(ctx: ServiceCtx) {
    let mut fails = 0u32;
    loop {
        let wait = match crate::net::get_text(ICS_URL, HTTP_TIMEOUT) {
            Ok(text) => {
                fails = 0;
                let from = today().checked_sub_days(Days::new(7)).unwrap_or_else(today);
                let list = parse(&text, from);
                log_info!("holiday: {} day(s) off from the public calendar", list.len());
                ctx.post(move |app| set_list(app, list));
                REFRESH
            }
            Err(e) => {
                fails += 1;
                log_warn!("holiday: {e}");
                ctx.post(|app| {
                    app.state_mut::<HolidayState>().failed = true;
                    app.request_redraw(Redraw::Tab("today"));
                });
                (RETRY * 2u32.saturating_pow(fails - 1)).min(MAX_RETRY)
            }
        };
        if !ctx.sleep(wait) {
            return;
        }
    }
}

fn set_list(app: &mut App, list: Vec<Holiday>) {
    let on = app.config().today.holidays;
    let st = app.state_mut::<HolidayState>();
    if st.sample || !on {
        return;
    }
    st.list = list;
    st.loaded = true;
    st.failed = false;
    check(app);
    app.request_redraw(Redraw::Tab("today"));
}

// ---- the evening before -----------------------------------------------------------------------

fn eve_path() -> std::path::PathBuf {
    moku_core::paths::data_file(EVE_FILE)
}

/// The holiday date the pill was last shown for, from the data folder (one small file, read once).
fn ensure_eve_loaded(app: &mut App) {
    if app.state::<HolidayState>().eve_loaded {
        return;
    }
    let shown = std::fs::read_to_string(eve_path()).ok().and_then(|t| NaiveDate::parse_from_str(t.trim(), "%Y-%m-%d").ok());
    let st = app.state_mut::<HolidayState>();
    st.eve_loaded = true;
    st.eve_shown = shown;
}

/// Raise the "Besok libur" pill when it is time (once per holiday), and arrange the next look.
fn check(app: &mut App) {
    if let Some(t) = app.state_mut::<HolidayState>().timer.take() {
        app.cancel_timer(t);
    }
    if !enabled(app) {
        return;
    }
    ensure_eve_loaded(app);
    let now = Local::now().naive_local();
    let st = app.state::<HolidayState>();
    if let Some(h) = eve(&st.list, now).filter(|h| st.eve_shown != Some(h.date)).cloned() {
        app.state_mut::<HolidayState>().eve_shown = Some(h.date);
        app.save_later("today.eve", |app| app.state::<HolidayState>().eve_shown.map(|d| (eve_path(), d.format("%Y-%m-%d").to_string().into_bytes())));
        show_eve(app, h, EVE_SECS);
    }
    // Armed for the evening before the next day off, and for nothing at all when there is none ahead.
    let Some(wait) = secs_to_next_check(&app.state::<HolidayState>().list, now) else { return };
    let t = app.after(Duration::from_secs(wait as u64), check);
    app.state_mut::<HolidayState>().timer = Some(t);
}

fn show_eve(app: &mut App, h: Holiday, secs: f32) {
    app.set_activity(
        Activity::transient("today.holiday", EVE_PRIORITY, secs, move |_| {
            let mut kids = vec![icon(icons::CALENDAR).color(color::RED), text(tr("Besok libur", "Day off tomorrow")).semibold()];
            kids.push(caption("·"));
            kids.push(text(h.name.clone()).secondary().max_w(300.0));
            pill(kids)
        })
        .tab("today")
        .bubble(|_| stack([icon(icons::CALENDAR).color(color::RED)])),
    );
}

// ---- test hook --------------------------------------------------------------------------------

/// `today.sample_holiday[:today|:eve]`: invented holidays through the real code (the badge, the
/// next-holiday line, the evening pill).
pub fn sample(app: &mut App, kind: &str) {
    let t = today();
    let day = |n: u64| t.checked_add_days(Days::new(n)).unwrap_or(t);
    let h = |n: u64, name: &str, cuti: bool| Holiday { date: day(n), name: name.to_string(), cuti, tentative: false };
    let list = if kind == "today" {
        vec![h(0, "Hari Raya Natal", false), h(7, "Hari Tahun Baru", false)]
    } else {
        vec![h(1, "Hari Raya Natal", false), h(8, "Hari Tahun Baru", false)]
    };
    let st = app.state_mut::<HolidayState>();
    st.list = list.clone();
    st.loaded = true;
    st.sample = true;
    if kind != "today" {
        show_eve(app, list[0].clone(), SAMPLE_SECS);
    }
    app.request_redraw(Redraw::Tab("today"));
}

// ---- Hari ini ---------------------------------------------------------------------------------

/// A badge under the date when today is a day off.
pub fn badge(app: &App) -> Option<Node> {
    if !enabled(app) {
        return None;
    }
    let st = app.state::<HolidayState>();
    let h = on(&st.list, today())?;
    let label = if h.cuti { tr("Cuti bersama", "Collective leave") } else { tr("Libur nasional", "Public holiday") };
    let maybe = if h.tentative { tr(" (belum pasti)", " (not certain yet)") } else { "" };
    Some(
        row([icon(icons::CALENDAR).font_size(11.0).color(color::RED), text(format!("{label} · {}{maybe}", h.name)).font_size(11.5).semibold().color(color::RED)])
            .gap(5.0)
            .pad_xy(8.0, 3.0)
            .bg(color::RED.alpha(0.16))
            .radius(8.0)
            .max_w(268.0),
    )
}

/// "Libur berikutnya · Jum, 25 Des · Hari Raya Natal".
pub fn next_line(app: &App) -> Option<Node> {
    if !enabled(app) {
        return None;
    }
    let st = app.state::<HolidayState>();
    let t = today();
    let h = next_after(&st.list, t)?;
    let days = h.date.signed_duration_since(t).num_days();
    let when = match days {
        1 => tr("besok", "tomorrow").to_string(),
        n if n < 7 => tr_s(format!("{} hari lagi", n), format!("in {n} days")),
        _ => i18n::date_short(&h.date),
    };
    let maybe = if h.tentative { tr(" (belum pasti)", " (not certain yet)") } else { "" };
    Some(caption(tr_s(format!("Libur berikutnya · {when} · {}{maybe}", h.name), format!("Next day off · {when} · {}{maybe}", h.name))))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("fixtures/holiday-id-2026-2027.ics");

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn at(y: i32, m: u32, day: u32, h: u32, mi: u32) -> NaiveDateTime {
        d(y, m, day).and_hms_opt(h, mi, 0).unwrap()
    }

    #[test]
    fn the_live_sample_keeps_only_the_days_off() {
        let all = parse(SAMPLE, d(2026, 1, 1));
        // 2026 and the start of 2027, as in the Google file; "Perayaan" are dropped.
        assert!(all.iter().all(|h| h.date.year() >= 2026));
        for kept in ["Hari Tahun Baru", "Tahun Baru Imlek", "Hari Raya Natal", "Cuti Bersama Natal (Malam Natal)", "Hari Proklamasi Kemerdekaan R.I.", "Maulid Nabi Muhammad"] {
            assert!(all.iter().any(|h| h.name == kept), "{kept} is a day off: {all:?}");
        }
        // "Perayaan" (observances), by date: 1 Ramadan, Satu Muharam (the day off is the 16th), Malam Tahun Baru.
        for (y, m, day) in [(2026, 2, 19), (2026, 6, 17), (2026, 12, 31)] {
            assert!(on(&all, d(y, m, day)).is_none(), "{y}-{m}-{day} is only a 'Perayaan', not a day off");
        }
        assert!(all.iter().any(|h| h.date == d(2026, 6, 16)), "Hari Kedua Muharram is");
        // Sorted, no duplicates.
        assert!(all.windows(2).all(|w| w[0].date <= w[1].date));
        // Idul Fitri 2026: two days of the holiday itself and cuti bersama around them.
        let fitri: Vec<&Holiday> = all.iter().filter(|h| h.date >= d(2026, 3, 20) && h.date <= d(2026, 3, 24)).collect();
        assert_eq!(fitri.len(), 5, "{fitri:?}");
        assert_eq!(fitri.iter().filter(|h| h.cuti).count(), 3);
    }

    #[test]
    fn cuti_bersama_and_tentative_dates_are_marked() {
        let all = parse(SAMPLE, d(2026, 1, 1));
        let natal_eve = all.iter().find(|h| h.date == d(2026, 12, 24)).unwrap();
        assert!(natal_eve.cuti && !natal_eve.tentative);
        let natal = all.iter().find(|h| h.date == d(2026, 12, 25)).unwrap();
        assert!(!natal.cuti);
        let isra = all.iter().find(|h| h.date == d(2027, 1, 5)).unwrap();
        assert!(isra.tentative, "(belum pasti) in the file");
        assert_eq!(isra.name, "Isra Mikraj Nabi Muhammad", "the '(belum pasti)' is cut from the name");
    }

    #[test]
    fn only_the_future_is_kept() {
        let all = parse(SAMPLE, d(2026, 12, 25));
        assert_eq!(all.first().map(|h| h.date), Some(d(2026, 12, 25)));
        assert!(parse(SAMPLE, d(2100, 1, 1)).is_empty());
    }

    #[test]
    fn broken_input_is_an_empty_list() {
        for s in ["", "garbage", "BEGIN:VEVENT\nDTSTART;VALUE=DATE:2026\nEND:VEVENT", "BEGIN:VEVENT\nSUMMARY:x\nDESCRIPTION:Hari libur nasional\nEND:VEVENT", "END:VEVENT\nEND:VEVENT"] {
            assert!(parse(s, d(2000, 1, 1)).is_empty(), "{s:?}");
        }
        // A day-off event with a time (not an all-day one) still has a date part; the file never does that, but it must not panic.
        let _ = parse("BEGIN:VEVENT\nDTSTART:20261225T000000Z\nSUMMARY:x\nDESCRIPTION:Hari libur nasional\nEND:VEVENT", d(2000, 1, 1));
    }

    #[test]
    fn lookups() {
        let all = parse(SAMPLE, d(2026, 1, 1));
        assert_eq!(on(&all, d(2026, 8, 17)).map(|h| h.name.as_str()), Some("Hari Proklamasi Kemerdekaan R.I."));
        assert!(on(&all, d(2026, 8, 18)).is_none());
        assert_eq!(next_after(&all, d(2026, 11, 25)).map(|h| h.date), Some(d(2026, 12, 24)));
        assert_eq!(next_after(&all, d(2026, 12, 24)).map(|h| h.date), Some(d(2026, 12, 25)));
    }

    #[test]
    fn the_evening_before_a_holiday() {
        let all = parse(SAMPLE, d(2026, 1, 1));
        // 17 August 2026 is a Monday: Sunday evening 16 August is the eve.
        assert_eq!(eve(&all, at(2026, 8, 16, 17, 0)).map(|h| h.date), Some(d(2026, 8, 17)));
        assert_eq!(eve(&all, at(2026, 8, 16, 23, 59)).map(|h| h.date), Some(d(2026, 8, 17)));
        assert!(eve(&all, at(2026, 8, 16, 16, 59)).is_none(), "not before 17:00");
        assert!(eve(&all, at(2026, 8, 16, 9, 0)).is_none());
        assert!(eve(&all, at(2026, 8, 15, 20, 0)).is_none(), "two days before");
        assert!(eve(&all, at(2026, 8, 17, 20, 0)).is_none(), "the day itself: tomorrow is a normal day");
        // Natal 2026 (Friday 25 December) with cuti bersama on Thursday 24: Wednesday evening and Thursday evening.
        assert_eq!(eve(&all, at(2026, 12, 23, 18, 0)).map(|h| h.name.as_str()), Some("Cuti Bersama Natal (Malam Natal)"));
        assert_eq!(eve(&all, at(2026, 12, 24, 18, 0)).map(|h| h.name.as_str()), Some("Hari Raya Natal"));
        // Paskah 2026 is on a Sunday: no extra day off, so no pill the evening before.
        assert!(eve(&all, at(2026, 4, 4, 20, 0)).is_none());
        // Month and year ends.
        let l = vec![Holiday { date: d(2027, 1, 1), name: "Hari Tahun Baru".into(), cuti: false, tentative: false }];
        assert_eq!(eve(&l, at(2026, 12, 31, 21, 0)).map(|h| h.date), Some(d(2027, 1, 1)));
        assert!(eve(&[], at(2026, 12, 31, 21, 0)).is_none());
    }

    #[test]
    fn the_next_look_is_the_evening_before_the_next_day_off_and_nothing_when_there_is_none() {
        let day_off = |y, m, dd| Holiday { date: d(y, m, dd), name: "Libur".into(), cuti: false, tentative: false };
        // Merdeka, Monday 17 Aug 2026: the evening before is Sunday 16 Aug at 17:00:01.
        let list = vec![day_off(2026, 8, 17)];
        assert_eq!(secs_to_next_check(&list, at(2026, 8, 16, 16, 59)), Some(61));
        assert_eq!(secs_to_next_check(&list, at(2026, 8, 16, 9, 0)), Some(8 * 3600 + 1), "the same day, in the morning: hours, not an hourly poll");
        // Days ahead: capped by MAX_SLEEP_SECS (the list is refreshed every 12 h anyway).
        assert_eq!(secs_to_next_check(&list, at(2026, 8, 1, 9, 0)), Some(MAX_SLEEP_SECS));
        // Once that evening has started and passed, there is nothing ahead: no timer.
        assert_eq!(secs_to_next_check(&list, at(2026, 8, 16, 17, 0).checked_add_signed(chrono::Duration::seconds(2)).unwrap()), None);
        assert_eq!(secs_to_next_check(&list, at(2026, 8, 16, 23, 59)), None);
        assert_eq!(secs_to_next_check(&[], at(2026, 8, 16, 9, 0)), None, "no holidays: nothing armed");
        // The next one after the first: Natal 2026 (Friday 25 Dec) is announced on Thursday evening.
        let two = vec![day_off(2026, 8, 17), day_off(2026, 12, 25)];
        assert_eq!(secs_to_next_check(&two, at(2026, 8, 17, 12, 0)).map(|s| s > 5), Some(true));
        assert_eq!(secs_to_next_check(&two, at(2026, 12, 24, 16, 0)), Some(3601));
        // A day off on a Sunday is no extra day off: never announced, so never waited for.
        let sunday = vec![day_off(2026, 4, 5)];
        assert_eq!(secs_to_next_check(&sunday, at(2026, 4, 1, 9, 0)), None);
        // Never less than 5 s.
        assert_eq!(secs_to_next_check(&list, at(2026, 8, 16, 17, 0)), Some(5));
    }
}
