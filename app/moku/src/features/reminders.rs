//! Feature `reminders` — Task 4 (track C).
//!
//! Drink-water and eye-break reminders (priority DUE), and the "Sehat" tab of the panel that
//! shows them together with the prayer schedule (the mockup's "Sehat & ibadah").
//!
//! * Water: a glass counter against `reminders.water_target`, persisted per day in
//!   `water.json` in the data folder (`{"date":"2026-11-24","count":4}`; a new day starts at
//!   0). A reminder is due every `water_interval_min` after the last glass, and stops for the
//!   day once the target is reached. "+1 gelas" / "Sudah" add a glass.
//! * Eye break: every `eye_interval_min` (20 min by default), "20-20-20" style.
//! * Both are single UI timers that sleep until the next reminder: nothing ticks in between.
//!   The panel redraws every 30 s while it is visible, for the countdown texts.
//!
//! Test hooks: `mokuctl debug click reminders.sample_water` / `reminders.sample_eye` show the
//! due pills at once.

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{Datelike, Local};
use serde::{Deserialize, Serialize};

use crate::activity::{Activity, HEADS_UP_SECS, priority};
use crate::app::{App, Redraw, TimerId};
use crate::i18n::{tr, tr_s};
use crate::registry::*;
use crate::ui::*;

/// Heart outline (Segoe Fluent Icons `EB51`), the tab icon.
const HEART: &str = "\u{EB51}";
/// Water drop (`EB42`).
const DROPLET: &str = "\u{EB42}";
/// How long a reminder pill stays.
const DUE_SECS: f32 = 12.0;
/// Test-hook pills stay long enough to screenshot.
const SAMPLE_SECS: f32 = 45.0;
/// Glasses are capped so a stuck button cannot overflow the counter.
const MAX_GLASSES: u32 = 99;

// ---- water log (pure) -------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WaterLog {
    /// Local date `YYYY-MM-DD`.
    pub date: String,
    pub count: u32,
}

impl WaterLog {
    /// The log as it is on `today`: a log of another day counts as 0.
    pub fn on(self, today: &str) -> WaterLog {
        if self.date == today { self } else { WaterLog { date: today.to_string(), count: 0 } }
    }

    /// Anything unreadable is an empty log.
    pub fn parse(text: &str) -> WaterLog {
        serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_default()
    }
}

fn load_water(path: &Path) -> WaterLog {
    std::fs::read(path).map(|b| WaterLog::parse(&String::from_utf8_lossy(&b))).unwrap_or_default()
}

fn save_water(path: &Path, log: &WaterLog) -> std::io::Result<()> {
    crate::persist::write_atomic(path, serde_json::to_string(log).map_err(std::io::Error::other)?.as_bytes())
}

fn water_path() -> std::path::PathBuf {
    moku_core::paths::data_file("water.json")
}

fn today_key() -> String {
    let n = Local::now();
    format!("{:04}-{:02}-{:02}", n.year(), n.month(), n.day())
}

// ---- state ------------------------------------------------------------------------------------

#[derive(Default)]
struct Health {
    water: WaterLog,
    loaded: bool,
    water_timer: Option<TimerId>,
    eye_timer: Option<TimerId>,
    /// `(enabled, interval_min, target)` the timers were set up for.
    water_key: Option<(bool, u32, u32)>,
    eye_key: Option<(bool, u32)>,
    /// When the eye-break reminder fires next, and the length of that interval.
    eye_due: Option<(Instant, Duration)>,
}

pub fn register(r: &mut Registry) {
    r.add_state(Health::default());
    r.add_tab(TabSpec {
        id: "health",
        order: 25,
        icon: HEART,
        label: ("Sehat", "Health"),
        enabled: |c| c.reminders.water || c.reminders.eye_break || c.prayer.enabled,
        build: panel,
        tick: Some((Duration::from_secs(30), |app| app.request_redraw(Redraw::Tab("health")))),
    });
    r.on_action("reminders", on_action);
    r.on_config(on_config);
    r.on_event(|app, ev| {
        // "Hapus semua data": the settings app deletes water.json right after.
        if *ev == AppEvent::DataCleared {
            app.cancel_save("reminders.water");
            let h = app.state_mut::<Health>();
            h.water = WaterLog::default();
            h.loaded = true;
            app.request_redraw(Redraw::Tab("health"));
        }
    });
}

fn ensure_loaded(app: &mut App) {
    let h = app.state_mut::<Health>();
    if !h.loaded {
        h.loaded = true;
        h.water = load_water(&water_path()).on(&today_key());
    }
}

/// Glasses drunk today.
fn glasses(app: &App) -> u32 {
    let w = &app.state::<Health>().water;
    if w.date == today_key() { w.count } else { 0 }
}

fn on_config(app: &mut App) {
    ensure_loaded(app);
    let c = app.config().reminders.clone();
    let (wkey, ekey) = ((c.water, c.water_interval_min, c.water_target), (c.eye_break, c.eye_interval_min));
    let h = app.state_mut::<Health>();
    let (wchg, echg) = (h.water_key != Some(wkey), h.eye_key != Some(ekey));
    h.water_key = Some(wkey);
    h.eye_key = Some(ekey);
    if wchg {
        schedule_water(app);
    }
    if echg {
        schedule_eye(app);
    }
    app.request_redraw(Redraw::Tab("health"));
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    match ev.name() {
        "water_add" => add_water(app),
        "eye_ok" => app.clear_activity("reminders.eye"),
        "sample_water" => show_water_due(app, SAMPLE_SECS),
        "sample_eye" => show_eye_due(app, SAMPLE_SECS),
        _ => {}
    }
}

// ---- water ------------------------------------------------------------------------------------

fn add_water(app: &mut App) {
    ensure_loaded(app);
    let today = today_key();
    let h = app.state_mut::<Health>();
    let mut log = std::mem::take(&mut h.water).on(&today);
    log.count = (log.count + 1).min(MAX_GLASSES);
    let count = log.count;
    h.water = log;
    app.save_later("reminders.water", |app| Some((water_path(), serde_json::to_vec(&app.state::<Health>().water).ok()?)));
    app.clear_activity("reminders.water");
    let target = app.config().reminders.water_target;
    if app.config().reminders.water && count == target {
        let a = Activity::transient("reminders.water_goal", priority::HEADS_UP, HEADS_UP_SECS, move |_| {
            pill([icon(DROPLET).color(color::TEAL), text(tr("Target minum tercapai", "Water goal reached")).semibold(), caption(tr_s(format!("{count} gelas hari ini"), format!("{count} glasses today")))])
        })
        .tab("health");
        app.set_activity(a);
    }
    schedule_water(app);
    app.request_redraw(Redraw::Tab("health"));
}

/// Arm (or disarm) the water reminder: due `water_interval_min` from now, unless it is off or
/// today's target is already reached.
fn schedule_water(app: &mut App) {
    let old = app.state_mut::<Health>().water_timer.take();
    if let Some(t) = old {
        app.cancel_timer(t);
    }
    let c = &app.config().reminders;
    if !c.water {
        app.clear_activity("reminders.water");
        return;
    }
    let every = Duration::from_secs(c.water_interval_min as u64 * 60);
    let id = app.after(every, water_due);
    app.state_mut::<Health>().water_timer = Some(id);
}

fn water_due(app: &mut App) {
    app.state_mut::<Health>().water_timer = None;
    let (target, on) = (app.config().reminders.water_target, app.config().reminders.water);
    if on && glasses(app) < target {
        show_water_due(app, DUE_SECS);
    }
    // Keep the chain going: it re-checks the target (and the date) every interval, which is
    // also what lets reminders resume on the next day.
    schedule_water(app);
}

fn show_water_due(app: &mut App, secs: f32) {
    let a = Activity::transient("reminders.water", priority::DUE, secs, |app| {
        let target = app.config().reminders.water_target;
        let done = row([icon(icons::CHECK).font_size(11.0), text(tr("Sudah", "Done")).semibold()]).gap(5.0).pad_xy(12.0, 0.0).h(26.0).bg(color::FILL2).capsule().action("reminders.water_add");
        pill([icon(DROPLET).color(color::TEAL), text(tr("Waktunya minum", "Time to drink")).semibold(), caption(tr_s(format!("{} dari {target} gelas", glasses(app)), format!("{} of {target} glasses", glasses(app)))), done]).pad4(14.0, 0.0, 6.0, 0.0)
    })
    .tab("health");
    app.set_activity(a);
}

// ---- eye break --------------------------------------------------------------------------------

fn schedule_eye(app: &mut App) {
    let old = app.state_mut::<Health>().eye_timer.take();
    if let Some(t) = old {
        app.cancel_timer(t);
    }
    let c = &app.config().reminders;
    if !c.eye_break {
        app.state_mut::<Health>().eye_due = None;
        app.clear_activity("reminders.eye");
        return;
    }
    let every = Duration::from_secs(c.eye_interval_min as u64 * 60);
    let id = app.after(every, eye_due);
    let h = app.state_mut::<Health>();
    h.eye_timer = Some(id);
    h.eye_due = Some((Instant::now() + every, every));
}

fn eye_due(app: &mut App) {
    app.state_mut::<Health>().eye_timer = None;
    if app.config().reminders.eye_break {
        show_eye_due(app, DUE_SECS);
    }
    schedule_eye(app);
}

fn show_eye_due(app: &mut App, secs: f32) {
    let a = Activity::transient("reminders.eye", priority::DUE, secs, |_| {
        let ok = row([icon(icons::CHECK).font_size(11.0), text("OK").semibold()]).gap(5.0).pad_xy(12.0, 0.0).h(26.0).bg(color::FILL2).capsule().action("reminders.eye_ok");
        pill([icon(icons::EYE).color(color::MINT), text(tr("Istirahat mata", "Eye break")).semibold(), caption(tr("lihat jauh 20 detik", "look far away for 20 s")), ok]).pad4(14.0, 0.0, 6.0, 0.0)
    })
    .tab("health");
    app.set_activity(a);
}

// ---- panel ------------------------------------------------------------------------------------

fn card_frame(kids: Vec<Node>) -> Node {
    col(kids).gap(8.0).grow(1.0).bg(color::FILL).radius(typo::RADIUS_CARD).pad(14.0).align(Align::Stretch)
}

fn water_card(app: &App) -> Node {
    let target = app.config().reminders.water_target;
    let n = glasses(app);
    let bar = if target <= 12 {
        row((0..target).map(|i| spacer().h(16.0).bg(if i < n { color::TEAL } else { color::FILL2 }).radius(5.0))).gap(4.0).fill_w()
    } else {
        progress(n as f32 / target as f32).color(color::TEAL).thickness(8.0)
    };
    let add = row([text(tr("+ 1 gelas", "+ 1 glass")).semibold().color(color::INK)]).center().fill_w().h(30.0).bg(color::TEAL).capsule().action("reminders.water_add");
    card_frame(vec![
        row([icon(DROPLET).color(color::TEAL), text(tr("Minum", "Water")).semibold()]).gap(8.0),
        text(tr_s(format!("{n} / {target} gelas"), format!("{n} / {target} glasses"))).font_size(typo::PANEL_TITLE).bold().display(),
        bar,
        add,
    ])
}

fn eye_card(app: &App) -> Node {
    let (left_text, frac) = match app.state::<Health>().eye_due {
        Some((at, every)) => {
            let left = at.saturating_duration_since(Instant::now());
            let mins = left.as_secs().div_ceil(60);
            let t = if mins == 0 { tr("sebentar lagi", "any moment").to_string() } else { tr_s(format!("{mins} menit lagi"), format!("in {mins} min")) };
            (t, 1.0 - left.as_secs_f32() / every.as_secs_f32().max(1.0))
        }
        None => ("—".to_string(), 0.0),
    };
    let every = app.config().reminders.eye_interval_min;
    card_frame(vec![
        row([icon(icons::EYE).color(color::MINT), text(tr("Istirahat mata", "Eye break")).semibold()]).gap(8.0),
        text(left_text).font_size(typo::PANEL_TITLE).bold().display(),
        progress(frac).color(color::MINT).thickness(6.0),
        caption(tr_s(format!("Tiap {every} menit, lihat jauh selama 20 detik"), format!("Every {every} min, look far away for 20 s"))).lines(2),
    ])
}

fn panel(app: &App) -> Node {
    let c = &app.config();
    let mut cards = vec![];
    if c.reminders.water {
        cards.push(water_card(app));
    }
    if c.reminders.eye_break {
        cards.push(eye_card(app));
    }
    if let Some(p) = super::prayer::card_node(app) {
        cards.push(p);
    }
    if cards.is_empty() {
        return placeholder(HEART, tr("Tidak ada pengingat aktif", "No reminders on"), tr("Nyalakan di Pengaturan → Fitur.", "Turn them on in Settings → Features."));
    }
    let mut body = vec![row(cards).gap(10.0).fill_w().align(Align::Stretch)];
    // The Hijri date, the Ramadan day / tomorrow's sunnah fast and the buka puasa countdown (track N).
    if let Some(strip) = super::ramadan::strip_node(app) {
        body.push(strip);
    }
    col(body).gap(10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn water_log_rolls_over_with_the_day() {
        let log = WaterLog { date: "2026-11-24".into(), count: 4 };
        assert_eq!(log.clone().on("2026-11-24").count, 4);
        let next = log.on("2026-11-25");
        assert_eq!((next.date.as_str(), next.count), ("2026-11-25", 0), "a new day starts at zero");
    }

    #[test]
    fn water_log_roundtrip_and_garbage() {
        let dir = std::env::temp_dir().join(format!("moku-water-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("water.json");
        assert_eq!(load_water(&path), WaterLog::default(), "no file = empty log");

        let log = WaterLog { date: "2026-11-24".into(), count: 5 };
        save_water(&path, &log).unwrap();
        assert_eq!(load_water(&path), log);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"date":"2026-11-24","count":5}"#);

        for junk in ["", "not json", "[1,2]", r#"{"count":"many"}"#, "\u{0}\u{1}"] {
            std::fs::write(&path, junk).unwrap();
            assert_eq!(load_water(&path).count, 0, "{junk:?}");
        }
        std::fs::write(&path, "\u{feff}{\"date\":\"2026-11-24\",\"count\":2}").unwrap();
        assert_eq!(load_water(&path).count, 2, "BOM tolerated");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn today_key_format() {
        let k = today_key();
        assert_eq!(k.len(), 10);
        assert_eq!(k.as_bytes()[4], b'-');
        assert_eq!(k.as_bytes()[7], b'-');
    }
}
