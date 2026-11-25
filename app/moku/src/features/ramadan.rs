//! The Ramadan pack and the puasa sunnah reminders (v0.3, track N). Built on `prayer.rs` (the
//! day's times) and `hijri.rs` (the date).
//!
//! * **Ramadan** is detected from the Hijri date plus `ibadah.hijri_offset`, on the civil date of
//!   the daytime (`hijri::from_date`). Needs `prayer.enabled` and `ibadah.ramadan`.
//! * **Countdown pill** (`ramadan.countdown`): a ring that fills across the fasting day (Imsak to
//!   Maghrib) with the time left. `ibadah.countdown_pill` chooses all day / the last 30 minutes /
//!   off. In the last 30 minutes before Imsak and before Maghrib it is *prominent*: a higher
//!   priority (it takes the pill from music, which becomes the split bubble), a bigger ring and
//!   warmer colours. One UI timer sleeps until the next time the text changes (a clock minute) or
//!   the next boundary; nothing ticks while no pill is shown.
//! * **Selamat berbuka** at Maghrib is the "prayer.now" pill, worded for buka puasa
//!   (`prayer.rs::announce`).
//! * **Puasa sunnah**: at `ibadah.sunnah_hour` the evening before, one pill for tomorrow's
//!   fast(s) (`hijri::reminder_for`). Opt-in per kind. Works without the prayer module.
//! * The Sehat tab shows a strip under its cards: the Hijri date, the Ramadan day / tomorrow's
//!   sunnah fast, and the countdown.
//!
//! Test hook: `mokuctl debug click ramadan.sample_sunnah[:monday|thursday|bidh|arafah|tasua|asyura]`
//! shows the reminder pill at once; `prayer.debug_at:<date>T<time>` moves the clock (see
//! `prayer.rs`).

use std::time::Duration;

use chrono::NaiveDate;
use moku_core::config::CountdownPill;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, WM_POWERBROADCAST};

use super::hijri::{self, Fast, Flags};
use super::prayer;
use crate::activity::{Activity, priority};
use crate::app::{App, Redraw, TimerId};
use crate::i18n::{tr, tr_s};
use crate::registry::*;
use crate::ui::*;

/// "Prominent" window before Imsak and Maghrib.
pub const PROMINENT_SECS: f64 = 1800.0;
/// The calm pill sits under music and prayer heads-ups; the prominent one over music and timers.
const PRIO_CALM: u8 = 20;
const PRIO_PROMINENT: u8 = 52;
const SUNNAH_SECS: f32 = 10.0;
/// Test-hook pills stay long enough to screenshot.
const SAMPLE_SECS: f32 = 45.0;
/// A reminder is still given this long after its hour (the island was started late).
const SUNNAH_LATE_SECS: f64 = 3.0 * 3600.0;

// ---- pure logic -------------------------------------------------------------------------------

/// Where `now` is in a fasting day (all values in seconds after midnight).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    /// Not a day of Ramadan (or no times).
    None,
    BeforeImsak { left: f64 },
    /// Between Imsak and Maghrib; `frac` is the part of the fast already over (0..1).
    Fasting { left: f64, frac: f32 },
    AfterMaghrib,
}

pub fn phase(now: f64, imsak: f64, maghrib: f64, fasting_day: bool) -> Phase {
    if !fasting_day || !(imsak.is_finite() && maghrib.is_finite()) || maghrib <= imsak {
        return Phase::None;
    }
    if now < imsak {
        Phase::BeforeImsak { left: imsak - now }
    } else if now < maghrib {
        Phase::Fasting { left: maghrib - now, frac: ((now - imsak) / (maghrib - imsak)).clamp(0.0, 1.0) as f32 }
    } else {
        Phase::AfterMaghrib
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Imsak,
    Buka,
}

/// What the countdown pill shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PillState {
    pub kind: Kind,
    /// Seconds to Imsak / Maghrib.
    pub left: f64,
    /// Ring fill, 0..1.
    pub frac: f32,
    /// Last 30 minutes.
    pub prominent: bool,
}

pub fn pill_state(phase: Phase, mode: CountdownPill) -> Option<PillState> {
    if mode == CountdownPill::Off {
        return None;
    }
    match phase {
        Phase::BeforeImsak { left } if left <= PROMINENT_SECS => Some(PillState { kind: Kind::Imsak, left, frac: (1.0 - left / PROMINENT_SECS).clamp(0.0, 1.0) as f32, prominent: true }),
        Phase::Fasting { left, frac } => {
            let prominent = left <= PROMINENT_SECS;
            (mode == CountdownPill::Always || prominent).then_some(PillState { kind: Kind::Buka, left, frac, prominent })
        }
        _ => None,
    }
}

/// Whole minutes left as shown (rounded up: "1 mnt" until it is over).
pub fn minutes_left(left: f64) -> i32 {
    (left / 60.0 - 1e-9).ceil().max(0.0) as i32
}

/// Seconds until the text of a countdown with `left` seconds left changes (the minute turns).
fn secs_to_minute_change(left: f64) -> f64 {
    let m = minutes_left(left);
    (left - 60.0 * (m as f64 - 1.0)).max(1.0)
}

/// Seconds from `now` until the Ramadan pill needs to be looked at again: the next change of the
/// shown minutes while a pill is up, else the next boundary (window start, Imsak, Maghrib) or
/// midnight. Always in 1 s ..= 24 h.
pub fn next_wake(now: f64, imsak: f64, maghrib: f64, fasting_day: bool, mode: CountdownPill) -> f64 {
    let midnight = 86_400.0 + 2.0;
    let mut at = midnight;
    let mut consider = |t: f64| {
        if t > now + 0.5 && t < at {
            at = t;
        }
    };
    if fasting_day && mode != CountdownPill::Off && imsak.is_finite() && maghrib.is_finite() {
        consider(imsak - PROMINENT_SECS);
        consider(imsak);
        consider(maghrib - PROMINENT_SECS);
        consider(maghrib);
        if let Some(p) = pill_state(phase(now, imsak, maghrib, true), mode) {
            consider(now + secs_to_minute_change(p.left));
        }
    }
    (at - now).clamp(1.0, 86_400.0)
}

/// Seconds until the next evening reminder: the reminder hour (today's if it is still ahead, or
/// a few seconds from now when the island starts within [`SUNNAH_LATE_SECS`] after it and today's
/// has not been given), else tomorrow's.
pub fn sunnah_wait(now: f64, hour: u32, done_today: bool) -> f64 {
    let target = hour.min(23) as f64 * 3600.0;
    if now < target {
        target - now
    } else if !done_today && now - target <= SUNNAH_LATE_SECS {
        20.0
    } else {
        86_400.0 - now + target
    }
}

// ---- state ------------------------------------------------------------------------------------

#[derive(Default)]
struct RamadanState {
    timer: Option<TimerId>,
    sunnah_timer: Option<TimerId>,
    /// The civil date the evening reminder was last given for.
    sunnah_done: Option<NaiveDate>,
    /// Config the timers were set up for: sunnah flags + hour + offset, countdown inputs.
    key: Option<(bool, bool, CountdownPill, u32, i32, Flags, u32)>,
}

pub fn register(r: &mut Registry) {
    r.add_state(RamadanState::default());
    r.on_config(on_config);
    r.on_action("ramadan", on_action);
    r.on_message(WM_POWERBROADCAST, on_power);
}

/// After sleep the timers of this pack are far behind the wall clock (they only count awake
/// time): recompute today's times and everything built on them.
fn on_power(app: &mut App, wp: WPARAM, _lp: LPARAM) {
    if matches!(wp.0 as u32, PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND) {
        prayer::refresh(app);
        resync(app);
    }
}

fn flags_of(app: &App) -> Flags {
    let c = &app.config().ibadah;
    Flags { monday_thursday: c.sunnah_monday_thursday, ayyamul_bidh: c.sunnah_ayyamul_bidh, arafah: c.sunnah_arafah, asyura: c.sunnah_asyura }
}

fn on_config(app: &mut App) {
    let c = &app.config().ibadah;
    let key = (app.config().prayer.enabled, c.ramadan, c.countdown_pill, c.imsak_minutes, c.hijri_offset, flags_of(app), c.sunnah_hour);
    let st = app.state_mut::<RamadanState>();
    if st.key == Some(key) {
        return;
    }
    st.key = Some(key);
    resync(app);
}

/// Recompute everything that depends on the clock or the config (also after the debug clock moved).
pub fn resync(app: &mut App) {
    refresh(app);
    arm_sunnah(app);
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click || ev.name() != "sample_sunnah" {
        return;
    }
    let fast = match ev.arg().unwrap_or("") {
        "thursday" => Fast::Thursday,
        "bidh" => Fast::AyyamulBidh,
        "arafah" => Fast::Arafah,
        "tasua" => Fast::Tasua,
        "asyura" => Fast::Asyura,
        _ => Fast::Monday,
    };
    show_sunnah(app, &[fast], SAMPLE_SECS);
}

// ---- Ramadan: today's numbers -----------------------------------------------------------------

/// Is today a day of fasting in Ramadan (and the pack is on)?
pub fn is_fasting_day(app: &App) -> bool {
    let c = app.config();
    if !(c.prayer.enabled && c.ibadah.ramadan) {
        return false;
    }
    let Some(date) = prayer::local_now().naive_date() else { return false };
    hijri::from_date(date, c.ibadah.hijri_offset).is_ramadan()
}

/// Imsak of today in minutes after midnight (`fajr - ibadah.imsak_minutes`), for the schedule card;
/// `None` outside Ramadan or when Imsak is set to 0.
pub fn imsak_minute(app: &App) -> Option<i32> {
    let before = app.config().ibadah.imsak_minutes as i32;
    if before == 0 || !is_fasting_day(app) {
        return None;
    }
    prayer::fajr_minute(app).map(|f| f - before)
}

/// `(imsak, maghrib)` of today in seconds after midnight.
fn bounds(app: &App) -> Option<(f64, f64)> {
    let fajr = prayer::fajr_minute(app)?;
    let maghrib = prayer::maghrib_secs(app)?;
    Some(((fajr - app.config().ibadah.imsak_minutes as i32) as f64 * 60.0, maghrib))
}

fn current_phase(app: &App) -> Phase {
    let Some((imsak, maghrib)) = bounds(app) else { return Phase::None };
    phase(prayer::local_now().secs, imsak, maghrib, is_fasting_day(app))
}

fn current_pill(app: &App) -> Option<PillState> {
    pill_state(current_phase(app), app.config().ibadah.countdown_pill)
}

/// Show / update / remove the countdown pill and re-arm its timer.
pub fn refresh(app: &mut App) {
    if let Some(t) = app.state_mut::<RamadanState>().timer.take() {
        app.cancel_timer(t);
    }
    match current_pill(app) {
        Some(p) => {
            let a = Activity::persistent("ramadan.countdown", if p.prominent { PRIO_PROMINENT } else { PRIO_CALM }, countdown_pill)
                .tab("health")
                .bubble(countdown_bubble);
            app.set_activity(a);
        }
        None => app.clear_activity("ramadan.countdown"),
    }
    // Nothing to time while the pack is off.
    if app.config().prayer.enabled && app.config().ibadah.ramadan {
        let wake = match bounds(app) {
            Some((imsak, maghrib)) => next_wake(prayer::local_now().secs, imsak, maghrib, is_fasting_day(app), app.config().ibadah.countdown_pill),
            None => 3600.0,
        };
        let id = app.after(Duration::from_secs_f64(wake + 0.2), |app| {
            app.state_mut::<RamadanState>().timer = None;
            refresh(app);
        });
        app.state_mut::<RamadanState>().timer = Some(id);
    }
    app.request_redraw(Redraw::Tab("health"));
}

// ---- pill -------------------------------------------------------------------------------------

/// `4 j 12 mnt` / `17 mnt`.
fn fmt_left(mins: i32) -> String {
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 { tr_s(format!("{h} j {m} mnt"), format!("{h} h {m} min")) } else { tr_s(format!("{m} mnt"), format!("{m} min")) }
}

fn label_of(kind: Kind, app: &App) -> String {
    match kind {
        Kind::Buka => tr("Buka puasa", "Iftar").to_string(),
        Kind::Imsak => if app.config().ibadah.imsak_minutes == 0 { tr("Subuh", "Fajr") } else { "Imsak" }.to_string(),
    }
}

fn countdown_pill(app: &App) -> Node {
    let Some(p) = current_pill(app) else { return empty() };
    let mins = minutes_left(p.left);
    let label = label_of(p.kind, app);
    let ring_color = if p.kind == Kind::Imsak { color::PURPLE } else if p.prominent { color::ORANGE } else { color::TEAL };
    if p.prominent {
        // Bigger ring, bold warm time: the last 30 minutes.
        let r = stack([ring(p.frac).size(24.0, 24.0).thickness(4.0).color(ring_color)]).size(26.0, 26.0);
        pill([r, text(label).semibold(), text(tr_s(format!("{} lagi", fmt_left(mins)), format!("{} left", fmt_left(mins)))).bold().color(color::YELLOW)])
    } else {
        pill([ring(p.frac).thickness(3.0).color(ring_color), text(label).semibold(), text(fmt_left(mins)).secondary()])
    }
}

fn countdown_bubble(app: &App) -> Node {
    let (frac, c) = current_pill(app).map_or((0.0, color::TEAL), |p| (p.frac, if p.kind == Kind::Imsak { color::PURPLE } else { color::ORANGE }));
    stack([ring(frac).color(c).size(22.0, 22.0)]).size(24.0, 24.0)
}

// ---- Sehat tab strip --------------------------------------------------------------------------

/// The strip under the cards of the Sehat tab; `None` when there is nothing to say.
pub fn strip_node(app: &App) -> Option<Node> {
    // The date (while `ibadah.hijri` is on) and the countdown (while it is Ramadan) are
    // independent: either alone is enough for a strip.
    let date = hijri::shown_now(app);
    let countdown = strip_countdown(app);
    if date.is_none() && countdown.is_none() {
        return None;
    }
    let mut kids = vec![icon(icons::MOON).font_size(16.0).color(color::YELLOW)];
    if let Some(h) = date {
        let mut left = vec![text(hijri::format(h)).semibold()];
        if let Some(s) = if is_fasting_day(app) { fasting_hours(app) } else { tomorrow_note(app) } {
            left.push(caption(s));
        }
        kids.push(col(left).gap(1.0));
    }
    kids.push(spacer());
    if let Some(r) = countdown {
        kids.push(r);
    }
    Some(row(kids).gap(10.0).fill_w().bg(color::FILL).radius(typo::RADIUS_CARD).pad_xy(14.0, 10.0))
}

/// `Imsak 04.32 · Buka 18.17`: today's fast, on a day of Ramadan.
fn fasting_hours(app: &App) -> Option<String> {
    let (imsak, maghrib) = bounds(app)?;
    Some(tr_s(
        format!("{} {} · Buka {}", label_of(Kind::Imsak, app), prayer::fmt_hm((imsak / 60.0) as i32), prayer::fmt_hm((maghrib / 60.0) as i32)),
        format!("{} {} · Iftar {}", label_of(Kind::Imsak, app), prayer::fmt_hm((imsak / 60.0) as i32), prayer::fmt_hm((maghrib / 60.0) as i32)),
    ))
}

/// `Besok: Puasa Kamis` while tomorrow is a sunnah fast the user follows.
fn tomorrow_note(app: &App) -> Option<String> {
    let date = prayer::local_now().naive_date()?;
    let fasts = hijri::reminder_for(date, app.config().ibadah.hijri_offset, flags_of(app));
    let first = fasts.first()?;
    Some(tr_s(format!("Besok: {}", first.name()), format!("Tomorrow: {}", first.name())))
}

fn strip_countdown(app: &App) -> Option<Node> {
    match current_phase(app) {
        Phase::Fasting { left, frac } => {
            let mins = minutes_left(left);
            let maghrib = prayer::maghrib_secs(app).map(|m| prayer::fmt_hm((m / 60.0) as i32)).unwrap_or_default();
            Some(
                row([
                    ring(frac).size(22.0, 22.0).thickness(3.5).color(if left <= PROMINENT_SECS { color::ORANGE } else { color::TEAL }),
                    col([text(tr("Buka puasa", "Iftar")).semibold(), caption(format!("{} · {maghrib}", fmt_left(mins)))]).gap(1.0),
                ])
                .gap(8.0),
            )
        }
        Phase::BeforeImsak { left } => {
            let imsak = bounds(app).map(|b| prayer::fmt_hm((b.0 / 60.0) as i32)).unwrap_or_default();
            Some(row([icon(icons::MOON).color(color::PURPLE), col([text("Imsak").semibold(), caption(format!("{} · {imsak}", fmt_left(minutes_left(left))))]).gap(1.0)]).gap(8.0))
        }
        Phase::AfterMaghrib => Some(row([icon(icons::CHECK).color(color::GREEN), text(tr("Sudah berbuka", "Fast is over")).secondary()]).gap(6.0)),
        Phase::None => None,
    }
}

// ---- puasa sunnah reminder --------------------------------------------------------------------

fn arm_sunnah(app: &mut App) {
    if let Some(t) = app.state_mut::<RamadanState>().sunnah_timer.take() {
        app.cancel_timer(t);
    }
    if !flags_of(app).any() {
        return;
    }
    let now = prayer::local_now();
    let done_today = now.naive_date().is_some_and(|d| app.state::<RamadanState>().sunnah_done == Some(d));
    let wait = sunnah_wait(now.secs, app.config().ibadah.sunnah_hour, done_today);
    let id = app.after(Duration::from_secs_f64(wait.min(86_400.0)), sunnah_due);
    app.state_mut::<RamadanState>().sunnah_timer = Some(id);
}

fn sunnah_due(app: &mut App) {
    app.state_mut::<RamadanState>().sunnah_timer = None;
    let now = prayer::local_now();
    if let Some(date) = now.naive_date() {
        let done = app.state::<RamadanState>().sunnah_done == Some(date);
        let past_hour = now.secs + 1.0 >= app.config().ibadah.sunnah_hour as f64 * 3600.0;
        if !done && past_hour {
            app.state_mut::<RamadanState>().sunnah_done = Some(date);
            let fasts = hijri::reminder_for(date, app.config().ibadah.hijri_offset, flags_of(app));
            if !fasts.is_empty() {
                show_sunnah(app, &fasts, SUNNAH_SECS);
            }
        }
    }
    arm_sunnah(app);
}

fn show_sunnah(app: &mut App, fasts: &[Fast], secs: f32) {
    let Some(&first) = fasts.first() else { return };
    let more = fasts.len() - 1;
    let a = Activity::transient("ramadan.sunnah", priority::NOTIFICATION, secs, move |_| {
        let mut kids = vec![icon(icons::MOON).color(color::MINT), text(tr_s(format!("Besok {}", first.name()), format!("Tomorrow: {}", first.name()))).semibold()];
        if more > 0 {
            kids.push(caption(format!("+{more}")));
        }
        pill(kids)
    })
    .tab("health");
    app.set_activity(a);
    app.request_redraw(Redraw::Tab("health"));
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMSAK: f64 = (3 * 60 + 55) as f64 * 60.0; // 03.55
    const MAGHRIB: f64 = (17 * 60 + 55) as f64 * 60.0; // 17.55

    fn at(h: u32, m: u32, s: u32) -> f64 {
        (h * 3600 + m * 60 + s) as f64
    }

    #[test]
    fn the_four_states_of_a_fasting_day() {
        // Before Imsak (the night).
        match phase(at(1, 0, 0), IMSAK, MAGHRIB, true) {
            Phase::BeforeImsak { left } => assert_eq!(left, IMSAK - at(1, 0, 0)),
            p => panic!("{p:?}"),
        }
        // Between Imsak and Maghrib, with the fraction of the fast that is over.
        match phase(at(10, 55, 0), IMSAK, MAGHRIB, true) {
            Phase::Fasting { left, frac } => {
                assert_eq!(left, MAGHRIB - at(10, 55, 0));
                assert!((frac - 0.5).abs() < 1e-6, "{frac}");
            }
            p => panic!("{p:?}"),
        }
        // The last 30 minutes are still "Fasting", just closer to the end.
        assert!(matches!(phase(MAGHRIB - 1800.0, IMSAK, MAGHRIB, true), Phase::Fasting { left, .. } if left == 1800.0));
        // At Maghrib itself the fast is over.
        assert_eq!(phase(MAGHRIB, IMSAK, MAGHRIB, true), Phase::AfterMaghrib);
        assert_eq!(phase(at(23, 0, 0), IMSAK, MAGHRIB, true), Phase::AfterMaghrib);
        // At Imsak itself the fast has begun.
        assert!(matches!(phase(IMSAK, IMSAK, MAGHRIB, true), Phase::Fasting { frac, .. } if frac == 0.0));
        // Not Ramadan, or nonsense times.
        assert_eq!(phase(at(12, 0, 0), IMSAK, MAGHRIB, false), Phase::None);
        assert_eq!(phase(at(12, 0, 0), f64::NAN, MAGHRIB, true), Phase::None);
        assert_eq!(phase(at(12, 0, 0), MAGHRIB, IMSAK, true), Phase::None);
    }

    #[test]
    fn the_pill_follows_the_mode() {
        use CountdownPill::*;
        // Long before Imsak: no pill in any mode (nobody wants a pill at 01.00).
        let night = phase(at(1, 0, 0), IMSAK, MAGHRIB, true);
        assert!(pill_state(night, Always).is_none() && pill_state(night, Last30).is_none());
        // Last 30 minutes before Imsak: prominent Imsak countdown.
        let sahur = phase(IMSAK - 600.0, IMSAK, MAGHRIB, true);
        let p = pill_state(sahur, Always).unwrap();
        assert_eq!((p.kind, p.prominent, p.left), (Kind::Imsak, true, 600.0));
        assert!((p.frac - (1.0 - 600.0 / 1800.0) as f32).abs() < 1e-6);
        assert!(pill_state(sahur, Last30).is_some());
        // Mid-day: only "all day" shows it, calmly.
        let day = phase(at(12, 0, 0), IMSAK, MAGHRIB, true);
        let p = pill_state(day, Always).unwrap();
        assert_eq!((p.kind, p.prominent), (Kind::Buka, false));
        assert!(pill_state(day, Last30).is_none());
        // Last 30 minutes: prominent in both.
        let late = phase(MAGHRIB - 1200.0, IMSAK, MAGHRIB, true);
        for mode in [Always, Last30] {
            let p = pill_state(late, mode).unwrap();
            assert_eq!((p.kind, p.prominent), (Kind::Buka, true), "{mode:?}");
        }
        // Exactly 30 minutes left counts as prominent; 30 min + 1 s does not.
        assert!(pill_state(phase(MAGHRIB - 1800.0, IMSAK, MAGHRIB, true), Last30).unwrap().prominent);
        assert!(pill_state(phase(MAGHRIB - 1801.0, IMSAK, MAGHRIB, true), Last30).is_none());
        // Off, after Maghrib, not Ramadan: nothing.
        assert!(pill_state(late, Off).is_none() && pill_state(sahur, Off).is_none());
        assert!(pill_state(phase(MAGHRIB + 60.0, IMSAK, MAGHRIB, true), Always).is_none());
        assert!(pill_state(Phase::None, Always).is_none());
    }

    #[test]
    fn minutes_round_up_and_the_clock_tick_follows_the_minute() {
        assert_eq!(minutes_left(1800.0), 30);
        assert_eq!(minutes_left(1799.0), 30);
        assert_eq!(minutes_left(1740.0), 29);
        assert_eq!(minutes_left(1.0), 1);
        assert_eq!(minutes_left(0.0), 0);
        // 29 min 40 s left shows "30 mnt"; the text turns to "29 mnt" 40 s later.
        assert_eq!(secs_to_minute_change(1780.0), 40.0);
        assert_eq!(secs_to_minute_change(1800.0), 60.0);
        assert_eq!(secs_to_minute_change(30.0), 30.0);
        assert!(secs_to_minute_change(0.0) >= 1.0 && secs_to_minute_change(0.2) >= 1.0, "never a zero-length sleep");
    }

    #[test]
    fn the_timer_sleeps_until_something_changes() {
        use CountdownPill::*;
        // Mid-day with the all-day pill: the next minute turn (here on a whole minute = 60 s).
        let w = next_wake(at(12, 0, 0), IMSAK, MAGHRIB, true, Always);
        assert_eq!(w, 60.0);
        let w = next_wake(at(12, 0, 20), IMSAK, MAGHRIB, true, Always);
        assert_eq!(w, 40.0);
        // Mid-day with "last 30 minutes": sleep until the window opens.
        assert_eq!(next_wake(at(12, 0, 0), IMSAK, MAGHRIB, true, Last30), MAGHRIB - 1800.0 - at(12, 0, 0));
        // Before the sahur window: wake when it opens.
        assert_eq!(next_wake(at(1, 0, 0), IMSAK, MAGHRIB, true, Last30), IMSAK - 1800.0 - at(1, 0, 0));
        // Inside the sahur window (29 min 50 s left shows "30 mnt"): the text turns to 29 in 50 s.
        assert_eq!(next_wake(IMSAK - 1790.0, IMSAK, MAGHRIB, true, Always), 50.0);
        // The last minute: wake at Maghrib itself (the end), not later.
        assert_eq!(next_wake(MAGHRIB - 30.0, IMSAK, MAGHRIB, true, Always), 30.0);
        // After Maghrib, off, or not a Ramadan day: just past midnight.
        assert_eq!(next_wake(at(20, 0, 0), IMSAK, MAGHRIB, true, Always), 86_402.0 - at(20, 0, 0));
        assert_eq!(next_wake(at(12, 0, 0), IMSAK, MAGHRIB, true, Off), 86_402.0 - at(12, 0, 0));
        assert_eq!(next_wake(at(12, 0, 0), IMSAK, MAGHRIB, false, Always), 86_402.0 - at(12, 0, 0));
        // Always within bounds, whatever the inputs.
        for now in [0.0, 1.0, 43_200.0, 86_399.0] {
            let w = next_wake(now, IMSAK, MAGHRIB, true, Always);
            assert!((1.0..=86_400.0).contains(&w), "{now}: {w}");
        }
    }

    #[test]
    fn the_evening_reminder_schedule() {
        let h = 20;
        // Before the hour: wait for it.
        assert_eq!(sunnah_wait(at(9, 0, 0), h, false), at(11, 0, 0));
        // Started shortly after the hour and not given yet: soon.
        assert_eq!(sunnah_wait(at(20, 30, 0), h, false), 20.0);
        // Already given today: tomorrow's.
        assert_eq!(sunnah_wait(at(20, 30, 0), h, true), 86_400.0 - at(20, 30, 0) + at(20, 0, 0));
        // Started very late (after the 3 h window): tomorrow's.
        assert_eq!(sunnah_wait(at(23, 30, 0), h, false), 86_400.0 - at(23, 30, 0) + at(20, 0, 0));
        // Exactly at the hour counts as "late start" not "wait 0".
        assert_eq!(sunnah_wait(at(20, 0, 0), h, false), 20.0);
        assert_eq!(sunnah_wait(at(0, 0, 0), 12, false), at(12, 0, 0));
    }

    #[test]
    fn labels() {
        assert_eq!(fmt_left(252), "4 j 12 mnt");
        assert_eq!(fmt_left(17), "17 mnt");
        assert_eq!(fmt_left(60), "1 j 0 mnt");
    }
}
