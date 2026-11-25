//! Feature `prayer` — Task 4 (track C).
//!
//! Prayer times from the standard astronomical algorithm (sun declination, equation of time,
//! hour angles), with the Kemenag RI parameters: Fajr 20°, Isha 18°, Asr shadow factor 1
//! (Shafi'i), horizon −0.833° for sunrise / Maghrib, and 2 minutes of ihtiyat. The module is off
//! by default (`prayer.enabled`).
//!
//! * A heads-up pill `minutes_before` a prayer (priority HEADS_UP, transient) and a pill at the
//!   prayer time (priority DUE). Both come from one UI-timer chain that sleeps until the next
//!   event, so nothing ticks in between.
//! * The schedule card of the "Sehat & ibadah" tab is built here and shown by `reminders.rs`.
//! * Location: `prayer.latitude/longitude`; when those are still the Jakarta defaults but the
//!   configured city is another one that the Today feature has geocoded, that position is used.
//!
//! Methods: Kemenag RI (Fajr 20°) and, since v0.3, Malaysia JAKIM (Fajr 18°). Imsak is Fajr minus
//! `ibadah.imsak_minutes` and is shown by the Ramadan pack (`ramadan.rs`), which also hooks in
//! at Maghrib ("Selamat berbuka") and at every recompute; the adzan sound (`adzan/`) starts from
//! [`fire`] at the prayer time.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, NaiveDate, Timelike};
use moku_core::config::{Prayer as PrayerCfg, PrayerMethod};

use crate::activity::{Activity, HEADS_UP_SECS, priority};
use crate::app::{App, Redraw, TimerId};
use crate::i18n::{self, tr, tr_s};
use crate::registry::*;
use crate::ui::*;

/// How long the "it is time" pill stays.
const NOW_SECS: f32 = 12.0;
/// Test-hook pills stay long enough to screenshot.
const SAMPLE_SECS: f32 = 45.0;
/// An event this late (machine was asleep) is no longer announced.
const STALE_SECS: f64 = 90.0;
/// The config default location (Jakarta); see [`location`].
const DEFAULT_LAT: f64 = -6.2088;
const DEFAULT_LON: f64 = 106.8456;

// ---- the algorithm ----------------------------------------------------------------------------

/// Calculation method parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// Sun depression for Fajr, degrees.
    pub fajr_angle: f64,
    /// Sun depression for Isha, degrees.
    pub isha_angle: f64,
    /// Asr shadow length factor (1 = Shafi'i, 2 = Hanafi).
    pub asr_factor: f64,
    /// Safety margin added to every prayer (and taken off sunrise), minutes.
    pub ihtiyat_min: f64,
}

/// Kemenag RI.
pub const KEMENAG: Params = Params { fajr_angle: 20.0, isha_angle: 18.0, asr_factor: 1.0, ihtiyat_min: 2.0 };

/// Malaysia (JAKIM): Fajr 18 deg, Isha 18 deg, Asr shadow 1 (Shafi'i), 2 minutes of ihtiyat.
/// Checked against JAKIM's own table (e-solat zone WLY01, Kuala Lumpur), see the tests.
pub const JAKIM: Params = Params { fajr_angle: 18.0, isha_angle: 18.0, asr_factor: 1.0, ihtiyat_min: 2.0 };

/// The parameters of a configured method.
pub fn params_of(method: PrayerMethod) -> Params {
    match method {
        PrayerMethod::Kemenag => KEMENAG,
        PrayerMethod::Jakim => JAKIM,
    }
}

/// Sun depression at sunrise / sunset: refraction + the sun's radius.
const HORIZON_DEG: f64 = 0.833;

/// Times of one day, in minutes after local midnight. `None` = the sun never reaches that angle
/// (polar latitudes): the caller just skips it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Times {
    pub fajr: Option<f64>,
    pub sunrise: Option<f64>,
    pub dhuhr: Option<f64>,
    pub asr: Option<f64>,
    pub maghrib: Option<f64>,
    pub isha: Option<f64>,
}

fn sin(d: f64) -> f64 {
    d.to_radians().sin()
}
fn cos(d: f64) -> f64 {
    d.to_radians().cos()
}
fn tan(d: f64) -> f64 {
    d.to_radians().tan()
}
fn fix_angle(a: f64) -> f64 {
    a - 360.0 * (a / 360.0).floor()
}
fn fix_hour(h: f64) -> f64 {
    h - 24.0 * (h / 24.0).floor()
}

/// Julian date at 0h UT of a Gregorian date.
fn julian(y: i32, m: u32, d: u32) -> f64 {
    let (mut y, mut m) = (y as f64, m as f64);
    if m <= 2.0 {
        y -= 1.0;
        m += 12.0;
    }
    let a = (y / 100.0).floor();
    let b = 2.0 - a + (a / 4.0).floor();
    (365.25 * (y + 4716.0)).floor() + (30.6001 * (m + 1.0)).floor() + d as f64 + b - 1524.5
}

/// Sun declination (degrees) and equation of time (hours) at Julian date `jd`.
fn sun_position(jd: f64) -> (f64, f64) {
    let d = jd - 2_451_545.0;
    let g = fix_angle(357.529 + 0.985_600_28 * d);
    let q = fix_angle(280.459 + 0.985_647_36 * d);
    let l = fix_angle(q + 1.915 * sin(g) + 0.020 * sin(2.0 * g));
    let e = 23.439 - 0.000_000_36 * d;
    let ra = fix_hour((cos(e) * sin(l)).atan2(cos(l)).to_degrees() / 15.0);
    let eqt = q / 15.0 - ra;
    let decl = (sin(e) * sin(l)).asin().to_degrees();
    (decl, eqt)
}

/// Hour (solar frame, before the time-zone shift) at which the sun is `angle` degrees below the
/// horizon, before (`ccw`) or after solar noon. NaN when it never gets there.
fn sun_angle_time(angle: f64, t_hours: f64, jd: f64, lat: f64, ccw: bool) -> f64 {
    let (decl, eqt) = sun_position(jd + t_hours / 24.0);
    let noon = fix_hour(12.0 - eqt);
    let x = (-sin(angle) - sin(decl) * sin(lat)) / (cos(decl) * cos(lat));
    if !x.is_finite() || x.abs() > 1.0 {
        return f64::NAN;
    }
    let z = x.acos().to_degrees() / 15.0;
    if ccw { noon - z } else { noon + z }
}

/// Afternoon time when a shadow is `factor` times the object plus its noon shadow.
fn asr_time(factor: f64, t_hours: f64, jd: f64, lat: f64) -> f64 {
    let (decl, _) = sun_position(jd + t_hours / 24.0);
    let angle = -(1.0 / (factor + tan((lat - decl).abs()))).atan().to_degrees();
    sun_angle_time(angle, t_hours, jd, lat, false)
}

/// Prayer times for a date and place. `tz_hours` is the UTC offset of the local clock
/// (Jakarta: 7). Latitude/longitude in degrees, east positive.
pub fn compute_with(p: &Params, y: i32, m: u32, d: u32, lat: f64, lon: f64, tz_hours: f64) -> Times {
    if !(lat.is_finite() && lon.is_finite() && tz_hours.is_finite()) {
        return Times::default();
    }
    let jd = julian(y, m, d) - lon / (15.0 * 24.0);
    // Rough estimates first; the second pass evaluates the sun at the first pass's answers.
    let (mut fajr, mut sunrise, mut dhuhr, mut asr, mut sunset, mut isha) = (5.0, 6.0, 12.0, 13.0, 18.0, 18.0);
    for _ in 0..2 {
        let pick = |new: f64, old: f64| if new.is_finite() { new } else { old };
        let f = sun_angle_time(p.fajr_angle, fajr, jd, lat, true);
        let sr = sun_angle_time(HORIZON_DEG, sunrise, jd, lat, true);
        let dh = fix_hour(12.0 - sun_position(jd + dhuhr / 24.0).1);
        let a = asr_time(p.asr_factor, asr, jd, lat);
        let ss = sun_angle_time(HORIZON_DEG, sunset, jd, lat, false);
        let i = sun_angle_time(p.isha_angle, isha, jd, lat, false);
        (fajr, sunrise, dhuhr, asr, sunset, isha) = (pick(f, fajr), pick(sr, sunrise), dh, pick(a, asr), pick(ss, sunset), pick(i, isha));
        if ![f, sr, a, ss, i].iter().all(|v| v.is_finite()) {
            break;
        }
    }
    // Final values (NaN stays NaN), shifted to the local clock, in minutes.
    let shift = tz_hours - lon / 15.0;
    let ih = p.ihtiyat_min;
    let out = |hours: f64, margin: f64| -> Option<f64> {
        let v = (hours + shift) * 60.0 + margin;
        v.is_finite().then_some(v)
    };
    Times {
        fajr: out(sun_angle_time(p.fajr_angle, fajr, jd, lat, true), ih),
        sunrise: out(sun_angle_time(HORIZON_DEG, sunrise, jd, lat, true), -ih),
        dhuhr: out(fix_hour(12.0 - sun_position(jd + dhuhr / 24.0).1), ih),
        asr: out(asr_time(p.asr_factor, asr, jd, lat), ih),
        maghrib: out(sun_angle_time(HORIZON_DEG, sunset, jd, lat, false), ih),
        isha: out(sun_angle_time(p.isha_angle, isha, jd, lat, false), ih),
    }
}

/// [`compute_with`] using the Kemenag parameters.
pub fn compute(y: i32, m: u32, d: u32, lat: f64, lon: f64, tz_hours: f64) -> Times {
    compute_with(&KEMENAG, y, m, d, lat, lon, tz_hours)
}

// ---- schedule and events ----------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prayer {
    Fajr,
    Dhuhr,
    Asr,
    Maghrib,
    Isha,
}

impl Prayer {
    pub fn name(self) -> &'static str {
        match self {
            Prayer::Fajr => tr("Subuh", "Fajr"),
            Prayer::Dhuhr => tr("Dzuhur", "Dhuhr"),
            Prayer::Asr => tr("Ashar", "Asr"),
            Prayer::Maghrib => "Maghrib",
            Prayer::Isha => tr("Isya", "Isha"),
        }
    }
}

/// A minute of the day as shown (`15.07`, or `15:07` in English).
pub fn fmt_hm(minute: i32) -> String {
    let m = minute.rem_euclid(24 * 60);
    let sep = if i18n::is_english() { ':' } else { '.' };
    format!("{:02}{sep}{:02}", m / 60, m % 60)
}

/// Whole minute as displayed. Rounded up, as Kemenag's tables do: a prayer time is never shown
/// earlier than it is.
pub fn whole_minute(x: f64) -> i32 {
    (x - 1e-9).ceil() as i32
}

/// The day's five prayers that exist, in order, as whole minutes after midnight.
pub fn schedule_of(t: &Times) -> Vec<(Prayer, i32)> {
    [(Prayer::Fajr, t.fajr), (Prayer::Dhuhr, t.dhuhr), (Prayer::Asr, t.asr), (Prayer::Maghrib, t.maghrib), (Prayer::Isha, t.isha)]
        .into_iter()
        .filter_map(|(p, v)| v.map(|v| (p, whole_minute(v))))
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ev {
    /// `n` minutes before the prayer.
    HeadsUp(Prayer, u32),
    /// The prayer time itself.
    Now(Prayer),
}

/// All announcements of a day as `(seconds after midnight, event)`, sorted. `before` = 0 means
/// no heads-up.
pub fn events(sched: &[(Prayer, i32)], before: u32) -> Vec<(f64, Ev)> {
    let mut v = vec![];
    for &(p, m) in sched {
        if before > 0 && m - before as i32 > 0 {
            v.push(((m - before as i32) as f64 * 60.0, Ev::HeadsUp(p, before)));
        }
        v.push((m as f64 * 60.0, Ev::Now(p)));
    }
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

/// The first prayer that has not started at `now_min` (minutes after midnight), with the minutes
/// left. `None` once Isha is over.
pub fn next_prayer(sched: &[(Prayer, i32)], now_min: f64) -> Option<(Prayer, i32, i32)> {
    sched.iter().find(|(_, m)| *m as f64 > now_min).map(|&(p, m)| (p, m, (m as f64 - now_min).ceil() as i32))
}

/// `12 mnt lagi` / `1 j 5 mnt lagi`.
fn left_text(mins: i32) -> String {
    let (h, m) = (mins / 60, mins % 60);
    let (id, en) = if h > 0 { (format!("{h} j {m} mnt lagi"), format!("in {h} h {m} min")) } else { (format!("{m} mnt lagi"), format!("in {m} min")) };
    tr_s(id, en)
}

/// Short countdown for the schedule card's highlighted row (fixed-width column): `12 mnt`,
/// `2j 20m`.
fn left_short(mins: i32) -> String {
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 { tr_s(format!("{h}j {m}m"), format!("{h}h {m}m")) } else { tr_s(format!("{m} mnt"), format!("{m} min")) }
}

// ---- feature state ----------------------------------------------------------------------------

#[derive(Default)]
pub struct PrayerState {
    /// Local date the times were computed for.
    day: Option<(i32, u32, u32)>,
    pub times: Option<Times>,
    /// Name of the place for the card header.
    pub place: String,
    /// What the times were computed from, to skip no-op refreshes: lat, lon, minutes before, method.
    key: Option<(f64, f64, u32, PrayerMethod)>,
    before: u32,
    /// Seconds after midnight up to which events have been announced (or deliberately skipped).
    last_fired: f64,
    timer: Option<TimerId>,
}

pub fn register(r: &mut Registry) {
    r.add_state(PrayerState::default());
    r.on_config(on_config);
    r.on_action("prayer", on_action);
}

/// Test hooks: `mokuctl debug click prayer.sample_next` / `prayer.sample_now` show the pills for
/// the next prayer of the schedule at once (nothing happens while the module is off).
fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind == ActionKind::Click {
        // Debug hook (the core only lets `debug_*` / `sample*` ids through with MOKU_DEBUG=1):
        //   prayer.debug_at:2027-02-20T16:40   live in that moment (advances in real time); `off` ends it
        // (`adzan.debug_play`, `adzan.debug_media` and `ramadan.sample_sunnah` are in their own modules.)
        if ev.name() == "debug_at" {
            return debug_at(app, ev.arg().unwrap_or(""));
        }
    }
    if ev.kind != ActionKind::Click || !app.config().prayer.enabled {
        return;
    }
    let now = local_now();
    let Some(times) = app.state::<PrayerState>().times else { return };
    let before = app.config().prayer.minutes_before.max(1);
    let sched = schedule_of(&times);
    let Some((p, m, _)) = next_prayer(&sched, now.secs / 60.0).or_else(|| sched.first().map(|&(p, m)| (p, m, 0))) else { return };
    match ev.name() {
        "sample_next" => announce(app, Ev::HeadsUp(p, before), (m as f64 - before as f64) * 60.0, SAMPLE_SECS),
        "sample_now" => announce(app, Ev::Now(p), m as f64 * 60.0, SAMPLE_SECS),
        _ => {}
    }
}

/// The local clock as this module sees it: date, seconds after midnight, UTC offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalNow {
    pub date: (i32, u32, u32),
    pub secs: f64,
    pub tz_hours: f64,
}

impl LocalNow {
    pub fn naive_date(&self) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(self.date.0, self.date.1, self.date.2)
    }

    /// The clock `secs` later (rolls over midnight).
    pub fn plus(mut self, secs: f64) -> LocalNow {
        self.secs += secs;
        while self.secs >= 86_400.0 {
            self.secs -= 86_400.0;
            if let Some(d) = self.naive_date().and_then(|d| d.succ_opt()) {
                self.date = (d.year(), d.month(), d.day());
            }
        }
        self
    }
}

/// Debug-only simulated clock (`prayer.debug_at:<date>T<time>`): the time it was set to, and when.
/// Ramadan is months away, so a test island needs to be able to live in it. Never set unless
/// `MOKU_DEBUG=1` (the action is gated, and [`set_sim`] checks again).
static SIM: Mutex<Option<(LocalNow, Instant)>> = Mutex::new(None);

pub fn local_now() -> LocalNow {
    if let Ok(g) = SIM.lock()
        && let Some((base, at)) = *g
    {
        return base.plus(at.elapsed().as_secs_f64());
    }
    let n = Local::now();
    LocalNow {
        date: (n.year(), n.month(), n.day()),
        secs: n.num_seconds_from_midnight() as f64 + n.nanosecond() as f64 / 1e9,
        tz_hours: n.offset().local_minus_utc() as f64 / 3600.0,
    }
}

/// Is the simulated clock on? (Debug runs only.)
pub fn sim_active() -> bool {
    SIM.lock().is_ok_and(|g| g.is_some())
}

/// Start (`Some`) or stop (`None`) the simulated clock. Refused outside debug runs.
fn set_sim(at: Option<(NaiveDate, f64)>) -> bool {
    if !crate::app::debug_enabled() {
        return false;
    }
    let tz_hours = Local::now().offset().local_minus_utc() as f64 / 3600.0;
    if let Ok(mut g) = SIM.lock() {
        *g = at.map(|(d, secs)| (LocalNow { date: (d.year(), d.month(), d.day()), secs, tz_hours }, Instant::now()));
        return true;
    }
    false
}

/// `2027-02-20T16:40` / `2027-02-20 16:40:30` / `off`: the argument of `prayer.debug_at`.
/// `Some(None)` = switch the simulation off.
pub fn parse_sim(arg: &str) -> Option<Option<(NaiveDate, f64)>> {
    let arg = arg.trim();
    if arg.eq_ignore_ascii_case("off") {
        return Some(None);
    }
    let (date, time) = arg.split_once(['T', ' '])?;
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let mut parts = time.split(':');
    let h: u32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let s: u32 = parts.next().map_or(Some(0), |v| v.parse().ok())?;
    if h > 23 || m > 59 || s > 59 || parts.next().is_some() {
        return None;
    }
    Some(Some((date, (h * 3600 + m * 60 + s) as f64)))
}

/// Today's Maghrib in seconds after midnight, as displayed (whole minute), while the module is
/// on and has times.
pub fn maghrib_secs(app: &App) -> Option<f64> {
    if !app.config().prayer.enabled {
        return None;
    }
    let t = app.state::<PrayerState>().times?;
    t.maghrib.map(|m| whole_minute(m) as f64 * 60.0)
}

/// Today's Fajr in whole minutes after midnight, as displayed.
pub fn fajr_minute(app: &App) -> Option<i32> {
    if !app.config().prayer.enabled {
        return None;
    }
    let t = app.state::<PrayerState>().times?;
    t.fajr.map(whole_minute)
}

/// `prayer.debug_at`: live in another moment; everything that depends on the clock is recomputed.
fn debug_at(app: &mut App, arg: &str) {
    match parse_sim(arg) {
        Some(at) if set_sim(at) => {
            log_info!("prayer: simulated clock {}", if at.is_some() { "on" } else { "off" });
            app.state_mut::<PrayerState>().day = None;
            refresh(app);
            super::ramadan::resync(app);
        }
        _ => log_warn!("prayer.debug_at: expected <yyyy-mm-dd>T<hh:mm> or off"),
    }
}

/// Where to compute for. See the module docs.
fn location(app: &App, cfg: &PrayerCfg) -> (f64, f64, String) {
    let default_pos = (cfg.latitude - DEFAULT_LAT).abs() < 1e-9 && (cfg.longitude - DEFAULT_LON).abs() < 1e-9;
    if default_pos
        && !cfg.city.is_empty()
        && !cfg.city.eq_ignore_ascii_case("jakarta")
        && let Some((city, lat, lon)) = super::today::geocoded(app)
        && city.eq_ignore_ascii_case(&cfg.city)
    {
        return (lat, lon, cfg.city.clone());
    }
    (cfg.latitude, cfg.longitude, cfg.city.clone())
}

fn on_config(app: &mut App) {
    let cfg = app.config().prayer.clone();
    if !cfg.enabled {
        stop(app);
        return;
    }
    let (lat, lon, _) = location(app, &cfg);
    let key = (lat, lon, cfg.minutes_before, cfg.method);
    let today = local_now().date;
    let st = app.state::<PrayerState>();
    if st.key == Some(key) && st.day == Some(today) && st.timer.is_some() {
        return;
    }
    refresh(app);
}

/// Feature off: no times, no timer, no pills.
fn stop(app: &mut App) {
    let st = app.state_mut::<PrayerState>();
    let timer = st.timer.take();
    st.times = None;
    st.day = None;
    st.key = None;
    if let Some(t) = timer {
        app.cancel_timer(t);
    }
    app.clear_activity("prayer.next");
    app.clear_activity("prayer.now");
    super::ramadan::refresh(app);
    app.request_redraw(Redraw::Tab("health"));
}

/// Recompute today's times from the config and re-arm the timer. Also called by `today` once it
/// has geocoded the city.
pub fn refresh(app: &mut App) {
    let cfg = app.config().prayer.clone();
    if !cfg.enabled {
        stop(app);
        return;
    }
    let (lat, lon, place) = location(app, &cfg);
    let now = local_now();
    let (y, m, d) = now.date;
    let times = compute_with(&params_of(cfg.method), y, m, d, lat, lon, now.tz_hours);
    let old = app.state_mut::<PrayerState>();
    let timer = old.timer.take();
    if old.day != Some(now.date) {
        old.last_fired = now.secs;
    }
    old.day = Some(now.date);
    old.times = Some(times);
    old.place = place;
    old.key = Some((lat, lon, cfg.minutes_before, cfg.method));
    old.before = cfg.minutes_before;
    if let Some(t) = timer {
        app.cancel_timer(t);
    }
    arm(app);
    // The Ramadan pack (imsak, buka puasa) is built on these times.
    super::ramadan::refresh(app);
    app.request_redraw(Redraw::Tab("health"));
}

/// Sleep until the next unannounced event (or until just after midnight).
fn arm(app: &mut App) {
    let now = local_now();
    let st = app.state::<PrayerState>();
    let Some(times) = st.times else { return };
    let evs = events(&schedule_of(&times), st.before);
    let next = evs.iter().map(|e| e.0).find(|t| *t > st.last_fired);
    let secs = match next {
        Some(t) => (t - now.secs).max(0.0) + 0.3,
        None => (86_400.0 - now.secs).max(0.0) + 2.0,
    };
    let id = app.after(Duration::from_secs_f64(secs.min(86_400.0)), fire);
    app.state_mut::<PrayerState>().timer = Some(id);
}

fn fire(app: &mut App) {
    app.state_mut::<PrayerState>().timer = None;
    let now = local_now();
    let st = app.state::<PrayerState>();
    if st.day != Some(now.date) {
        refresh(app);
        return;
    }
    let evs = st.times.map(|t| events(&schedule_of(&t), st.before)).unwrap_or_default();
    let last = st.last_fired;
    let due: Vec<(f64, Ev)> = evs.into_iter().filter(|(t, _)| *t > last && *t <= now.secs + 0.5).collect();
    app.state_mut::<PrayerState>().last_fired = now.secs;
    // Several at once (heads-up and prayer time coincide): the prayer time wins, then the latest.
    let pick = due.iter().rev().find(|(_, e)| matches!(e, Ev::Now(_))).or(due.last());
    if let Some(&(t, ev)) = pick
        && now.secs - t <= STALE_SECS
    {
        announce(app, ev, t, if matches!(ev, Ev::Now(_)) { NOW_SECS } else { HEADS_UP_SECS });
    }
    // The sound only at the prayer time itself (not for a heads-up, not for a sample pill) and
    // never for a time that passed while the PC slept (checked by the adzan itself).
    if let Some(&(t, Ev::Now(p))) = due.iter().rev().find(|(_, e)| matches!(e, Ev::Now(_))) {
        super::adzan::on_prayer_time(app, p, now.secs - t);
    }
    arm(app);
}

fn announce(app: &mut App, ev: Ev, at_secs: f64, secs: f32) {
    let (time, moon) = (fmt_hm((at_secs / 60.0).round() as i32), icons::MOON);
    match ev {
        Ev::HeadsUp(p, before) => {
            // The announcement is `before` minutes ahead; show the prayer's own time.
            let time = fmt_hm((at_secs / 60.0).round() as i32 + before as i32);
            let a = Activity::transient("prayer.next", priority::HEADS_UP, secs, move |_| {
                pill([icon(moon).color(color::PURPLE), text(p.name()).semibold(), text(tr_s(format!("{before} menit lagi"), format!("in {before} min"))).secondary(), caption(time.clone())])
            })
            .tab("health");
            app.set_activity(a);
        }
        Ev::Now(p) => {
            // Maghrib on a day of Ramadan is the moment of buka puasa.
            let iftar = p == Prayer::Maghrib && super::ramadan::is_fasting_day(app);
            let a = Activity::transient("prayer.now", priority::DUE, secs, move |_| {
                if iftar {
                    pill([icon(moon).color(color::YELLOW), text(tr("Selamat berbuka puasa", "Time to break your fast")).semibold(), caption(tr_s(format!("Maghrib {time}"), format!("Maghrib {time}")))])
                } else {
                    pill([icon(moon).color(color::YELLOW), text(tr_s(format!("Waktu {}", p.name()), format!("Time for {}", p.name()))).semibold(), caption(time.clone())])
                }
            })
            .tab("health");
            app.set_activity(a);
        }
    }
    app.request_redraw(Redraw::Tab("health"));
}

// ---- schedule card ----------------------------------------------------------------------------

/// The "Jadwal sholat" card of the Sehat & ibadah tab; `None` while the module is off or has no
/// times (nothing to show, no error).
pub fn card_node(app: &App) -> Option<Node> {
    if !app.config().prayer.enabled {
        return None;
    }
    let st = app.state::<PrayerState>();
    let sched = schedule_of(&st.times?);
    if sched.is_empty() {
        return None;
    }
    let now = local_now();
    let next = next_prayer(&sched, now.secs / 60.0);
    let mut kids = vec![row([icon(icons::MOON).color(color::YELLOW), text(tr("Jadwal sholat", "Prayer times")).semibold(), spacer(), caption(st.place.clone()).max_w(90.0)]).gap(8.0).fill_w()];
    // On a day of Ramadan, Imsak leads the list.
    if let Some(im) = super::ramadan::imsak_minute(app) {
        kids.push(row([text("Imsak").color(color::TEXT2), spacer(), text(fmt_hm(im)).color(color::TEXT2).semibold().text_right().w(40.0)]).gap(6.0).fill_w().h(24.0).pad_xy(8.0, 0.0));
    }
    for (p, m) in sched {
        let is_next = next.is_some_and(|n| n.0 == p);
        let name = text(p.name()).color(if is_next { color::WHITE } else { color::TEXT2 });
        let name = if is_next { name.semibold() } else { name };
        // Fixed columns for the countdown and the time, so neither is ever cut off.
        let mut r = vec![name, spacer()];
        if let Some((_, _, left)) = next.filter(|_| is_next) {
            r.push(text(left_short(left)).font_size(11.0).color(color::YELLOW).text_right().w(46.0));
        }
        r.push(text(fmt_hm(m)).color(if is_next { color::WHITE } else { color::TEXT2 }).semibold().text_right().w(40.0));
        let mut line = row(r).gap(6.0).fill_w().h(24.0).pad_xy(8.0, 0.0).radius(10.0);
        if is_next {
            line = line.bg(color::YELLOW.alpha(0.20));
        }
        kids.push(line);
    }
    Some(col(kids).gap(2.0).grow(1.15).bg(color::FILL).radius(typo::RADIUS_CARD).pad(12.0).align(Align::Stretch))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hm(h: u32, m: u32) -> f64 {
        (h * 60 + m) as f64
    }

    /// Reference: Kemenag RI schedule for KOTA JAKARTA (DKI JAKARTA), Tuesday 24 November 2026,
    /// as republished by the public MyQuran API (it serves the Kemenag / Bimas Islam data set):
    /// <https://api.myquran.com/v2/sholat/jadwal/1301/2026/11/24>, fetched 2026-11-24:
    /// imsak 03:55, subuh 04:05, terbit 05:23, dzuhur 11:43, ashar 15:06, maghrib 17:55, isya 19:09.
    /// (bimasislam.kemenag.go.id is now a script-driven page without a plain table, so it was
    /// not used directly.) Cross-check, not Kemenag but the same angles, for 6°10' S 106°49' E:
    /// <https://jadwalsholat.org/jadwal-sholat/monthly.php?id=308> shows 04:05, 05:24, 11:41,
    /// 15:05, 17:54, 19:08.
    #[test]
    fn prayer_times_jakarta_2026_11_24() {
        let t = compute(2026, 11, 24, -6.2088, 106.8456, 7.0);
        let expect = [
            ("fajr", t.fajr, hm(4, 5)),
            ("sunrise", t.sunrise, hm(5, 23)),
            ("dhuhr", t.dhuhr, hm(11, 43)),
            ("asr", t.asr, hm(15, 6)),
            ("maghrib", t.maghrib, hm(17, 55)),
            ("isha", t.isha, hm(19, 9)),
        ];
        for (name, got, want) in expect {
            let got = got.unwrap_or_else(|| panic!("{name} missing"));
            assert!((got - want).abs() <= 2.0, "{name}: got {got:.1} min, Kemenag {want} min");
        }
    }

    /// The minutes the island shows (rounded up) land within one minute of Kemenag's table.
    #[test]
    fn displayed_minutes_match_kemenag_within_a_minute() {
        let s = schedule_of(&compute(2026, 11, 24, -6.2088, 106.8456, 7.0));
        let want = [(Prayer::Fajr, hm(4, 5)), (Prayer::Dhuhr, hm(11, 43)), (Prayer::Asr, hm(15, 6)), (Prayer::Maghrib, hm(17, 55)), (Prayer::Isha, hm(19, 9))];
        assert_eq!(s.len(), 5);
        for ((p, got), (wp, w)) in s.iter().zip(want) {
            assert_eq!(*p, wp);
            assert!((*got as f64 - w).abs() <= 1.0, "{p:?}: shows {} but Kemenag has {}", fmt_hm(*got), fmt_hm(w as i32));
        }
        assert_eq!(whole_minute(244.8), 245);
        assert_eq!(whole_minute(245.0), 245, "an exact minute stays");
    }

    /// JAKIM's own table: e-solat takwim for zone WLY01 (Kuala Lumpur, Putrajaya),
    /// <https://www.e-solat.gov.my/index.php?r=esolatApi/takwimsolat&period=month&zone=WLY01>,
    /// fetched 2026-11-25 (fajr, syuruk, dhuhr, asr, maghrib, isha). Reference point 3.139 N
    /// 101.687 E, UTC+8.
    const JAKIM_KL: [((i32, u32, u32), [(u32, u32); 6]); 3] = [
        ((2026, 11, 1), [(5, 48), (6, 56), (13, 0), (16, 20), (18, 58), (20, 9)]),
        ((2026, 11, 15), [(5, 49), (6, 58), (13, 1), (16, 22), (18, 59), (20, 11)]),
        ((2026, 11, 24), [(5, 50), (7, 0), (13, 3), (16, 25), (19, 0), (20, 14)]),
    ];

    #[test]
    fn jakim_angles_and_times_match_the_official_table() {
        assert_eq!((JAKIM.fajr_angle, JAKIM.isha_angle, JAKIM.asr_factor), (18.0, 18.0, 1.0));
        assert_eq!(params_of(PrayerMethod::Jakim), JAKIM);
        assert_eq!(params_of(PrayerMethod::Kemenag), KEMENAG);
        for ((y, m, d), want) in JAKIM_KL {
            let t = compute_with(&JAKIM, y, m, d, 3.139, 101.687, 8.0);
            let got = [t.fajr, t.sunrise, t.dhuhr, t.asr, t.maghrib, t.isha];
            for (i, (g, (h, mi))) in got.into_iter().zip(want).enumerate() {
                let g = g.unwrap_or_else(|| panic!("{y}-{m}-{d} #{i} missing"));
                assert!((g - hm(h, mi)).abs() <= 2.0, "{y}-{m}-{d} #{i}: got {g:.1} min, JAKIM {}", hm(h, mi));
            }
            // The 18 degrees are what matter: Kemenag's 20 degrees would put Fajr ~9 minutes earlier.
            let k = compute_with(&KEMENAG, y, m, d, 3.139, 101.687, 8.0);
            let jf = t.fajr.unwrap();
            assert!(jf - k.fajr.unwrap() > 6.0, "{y}-{m}-{d}");
            // Isha uses 18 degrees in both.
            assert!((t.isha.unwrap() - k.isha.unwrap()).abs() < 1e-9);
        }
    }

    #[test]
    fn the_method_changes_only_what_it_should() {
        let kem = compute_with(&KEMENAG, 2026, 11, 24, -6.2088, 106.8456, 7.0);
        let jak = compute_with(&JAKIM, 2026, 11, 24, -6.2088, 106.8456, 7.0);
        assert!(jak.fajr.unwrap() > kem.fajr.unwrap());
        assert_eq!((jak.dhuhr, jak.asr, jak.maghrib, jak.isha), (kem.dhuhr, kem.asr, kem.maghrib, kem.isha));
    }

    #[test]
    fn debug_clock_argument() {
        let d = NaiveDate::from_ymd_opt(2027, 2, 20).unwrap();
        assert_eq!(parse_sim("2027-02-20T16:40"), Some(Some((d, 16.0 * 3600.0 + 40.0 * 60.0))));
        assert_eq!(parse_sim(" 2027-02-20 16:40:30 "), Some(Some((d, 16.0 * 3600.0 + 40.0 * 60.0 + 30.0))));
        assert_eq!(parse_sim("OFF"), Some(None));
        for bad in ["", "2027-02-20", "2027-02-20T25:00", "2027-02-20T10:61", "2027-02-30T10:00", "x", "2027-02-20T10:00:00:00"] {
            assert_eq!(parse_sim(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn local_clock_rolls_over_midnight() {
        let base = LocalNow { date: (2026, 12, 31), secs: 86_340.0, tz_hours: 7.0 };
        let later = base.plus(120.0);
        assert_eq!(later.date, (2027, 1, 1));
        assert!((later.secs - 60.0).abs() < 1e-9);
        assert_eq!(base.plus(10.0).date, (2026, 12, 31));
    }

    #[test]
    fn julian_date_of_j2000() {
        assert_eq!(julian(2000, 1, 1), 2_451_544.5);
        assert_eq!(julian(2026, 11, 24), 2_461_368.5);
    }

    #[test]
    fn polar_night_has_no_times_but_does_not_panic() {
        // Svalbard at midsummer: the sun never goes down, so no Maghrib / Isha / Fajr.
        let t = compute(2026, 6, 21, 78.2, 15.6, 2.0);
        assert_eq!(t.maghrib, None);
        assert_eq!(t.isha, None);
        assert!(schedule_of(&t).len() < 5);
        // Nonsense input stays harmless too.
        let t = compute(2026, 6, 21, f64::NAN, 15.6, 2.0);
        assert!(schedule_of(&t).is_empty());
    }

    #[test]
    fn schedule_is_in_order_and_whole_minutes() {
        let t = compute(2026, 11, 24, -6.2088, 106.8456, 7.0);
        let s = schedule_of(&t);
        assert_eq!(s.len(), 5);
        assert!(s.windows(2).all(|w| w[0].1 < w[1].1), "{s:?}");
        assert_eq!(s[0].0, Prayer::Fajr);
        assert_eq!(s[4].0, Prayer::Isha);
        assert_eq!(fmt_hm(15 * 60 + 7), "15.07");
    }

    #[test]
    fn events_heads_up_and_next_prayer() {
        let sched = vec![(Prayer::Fajr, hm(4, 5) as i32), (Prayer::Dhuhr, hm(11, 43) as i32), (Prayer::Asr, hm(15, 6) as i32)];
        let ev = events(&sched, 10);
        assert_eq!(ev.len(), 6);
        assert_eq!(ev[0], ((4 * 60 + 5 - 10) as f64 * 60.0, Ev::HeadsUp(Prayer::Fajr, 10)));
        assert_eq!(ev[1], ((4 * 60 + 5) as f64 * 60.0, Ev::Now(Prayer::Fajr)));
        assert!(ev.windows(2).all(|w| w[0].0 <= w[1].0));
        // No heads-up when minutes_before is 0.
        assert!(events(&sched, 0).iter().all(|e| matches!(e.1, Ev::Now(_))));
        // A heads-up that would fall before midnight is dropped.
        assert_eq!(events(&[(Prayer::Fajr, 5)], 10).len(), 1);

        let n = next_prayer(&sched, hm(11, 43) - 12.0).unwrap();
        assert_eq!((n.0, n.2), (Prayer::Dhuhr, 12));
        assert_eq!(next_prayer(&sched, hm(11, 43)).unwrap().0, Prayer::Asr, "at the minute itself the next one is next");
        assert_eq!(next_prayer(&sched, hm(23, 0)), None);
    }

    #[test]
    fn left_text_formats() {
        assert_eq!(left_text(12), "12 mnt lagi");
        assert_eq!(left_text(65), "1 j 5 mnt lagi");
        assert_eq!(left_short(140), "2j 20m");
        assert_eq!(left_short(9), "9 mnt");
    }
}
