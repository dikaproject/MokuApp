//! Feature `timer` — Task 4 (track C).
//!
//! Countdown presets (5/15/25/45 min or any length via `timer.start:<secs>`), a focus cycle
//! (focus → break, Pomodoro style) and an independent stopwatch.
//!
//! * The running countdown is a persistent `timer.run` activity (priority TIMER): a ring + time
//!   for a plain countdown, the "Fokus ─── 18:20" pill from the mockup for focus / break.
//! * Finishing raises a `timer.done` alert (priority DUE).
//! * While a focus segment runs (and `timer.hold_notifications_in_focus`), the island holds
//!   notifications through `app.set_focus_hold(true)`; the `notify` feature reads it.
//! * No busy ticking: one UI timer chain exists only while something runs, and it fires at the
//!   moment the displayed second changes (≤ 1 Hz). Time is measured against `Instant`s, so a
//!   late tick never loses time.
//!
//! Drive it from a shell: `mokuctl debug click timer.start:90`, `timer.focus`, `timer.toggle`,
//! `timer.reset`, `timer.sw_toggle`.

use std::time::{Duration, Instant};

use crate::activity::{Activity, priority};
use crate::app::{App, Redraw, TimerId};
use crate::i18n::{self, tr, tr_s};
use crate::registry::*;
use crate::ui::*;

/// Preset lengths of the panel chips, minutes.
const PRESETS_MIN: [u64; 4] = [5, 15, 25, 45];
/// Longest accepted countdown (`timer.start:<secs>`).
const MAX_SECS: u64 = 24 * 3600;
/// "sesi 2/4": a Pomodoro round has four focus sessions.
const SESSIONS_PER_ROUND: u32 = 4;
/// How long the "timer done" pill stays.
const DONE_SECS: f32 = 15.0;

// ---- pure model -------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A plain countdown.
    Countdown,
    /// Focus work, then a break.
    Focus,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Work,
    Break,
}

/// What a [`Countdown::tick`] ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// A plain countdown reached zero.
    Finished,
    /// A focus segment ended; the break has started.
    FocusDone,
    /// The break ended; the cycle is over (idle).
    BreakDone,
}

/// The timer / focus state machine. All methods take `now`, so it is deterministic in tests.
#[derive(Clone, Debug)]
pub struct Countdown {
    pub active: bool,
    pub kind: Kind,
    pub stage: Stage,
    /// Length of the current segment.
    pub total: Duration,
    /// Length of the break that follows a focus segment.
    pub break_len: Duration,
    /// Focus segments finished since the last reset.
    pub completed: u32,
    /// Time spent in the current segment before the current run.
    banked: Duration,
    /// Start of the current run; `None` while paused or idle.
    since: Option<Instant>,
}

impl Countdown {
    pub fn idle() -> Countdown {
        Countdown {
            active: false,
            kind: Kind::Countdown,
            stage: Stage::Work,
            total: Duration::ZERO,
            break_len: Duration::ZERO,
            completed: 0,
            banked: Duration::ZERO,
            since: None,
        }
    }

    pub fn start_countdown(&mut self, now: Instant, total: Duration) {
        self.active = true;
        self.kind = Kind::Countdown;
        self.stage = Stage::Work;
        self.total = total.max(Duration::from_secs(1));
        self.banked = Duration::ZERO;
        self.since = Some(now);
    }

    pub fn start_focus(&mut self, now: Instant, work: Duration, brk: Duration) {
        self.start_countdown(now, work);
        self.kind = Kind::Focus;
        self.break_len = brk.max(Duration::from_secs(1));
    }

    pub fn running(&self) -> bool {
        self.active && self.since.is_some()
    }

    pub fn paused(&self) -> bool {
        self.active && self.since.is_none()
    }

    pub fn pause(&mut self, now: Instant) {
        if let Some(s) = self.since.take() {
            self.banked += now.saturating_duration_since(s);
        }
    }

    pub fn resume(&mut self, now: Instant) {
        if self.paused() {
            self.since = Some(now);
        }
    }

    /// Back to the initial state (also forgets the finished sessions).
    pub fn reset(&mut self) {
        *self = Countdown::idle();
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        self.banked + self.since.map_or(Duration::ZERO, |s| now.saturating_duration_since(s))
    }

    pub fn remaining(&self, now: Instant) -> Duration {
        self.total.saturating_sub(self.elapsed(now))
    }

    /// Seconds shown on screen (rounded up, like every countdown).
    pub fn display_secs(&self, now: Instant) -> u64 {
        ceil_secs(self.remaining(now))
    }

    /// Fraction of the segment still to go, 0–1 (the ring drains).
    pub fn remaining_frac(&self, now: Instant) -> f32 {
        if self.total.is_zero() { 0.0 } else { (self.remaining(now).as_secs_f32() / self.total.as_secs_f32()).clamp(0.0, 1.0) }
    }

    /// Whether this should hold notifications: focus work is running and the user wants it.
    pub fn holds(&self, wanted: bool) -> bool {
        wanted && self.running() && self.kind == Kind::Focus && self.stage == Stage::Work
    }

    /// Advance: ends the segment when its time is up. A finished focus segment rolls straight
    /// into the break; a finished break or plain countdown leaves the timer idle.
    pub fn tick(&mut self, now: Instant) -> Option<Event> {
        if !self.running() || self.elapsed(now) < self.total {
            return None;
        }
        match (self.kind, self.stage) {
            (Kind::Countdown, _) => {
                self.active = false;
                self.since = None;
                Some(Event::Finished)
            }
            (Kind::Focus, Stage::Work) => {
                self.completed += 1;
                self.stage = Stage::Break;
                self.total = self.break_len;
                self.banked = Duration::ZERO;
                self.since = Some(now);
                Some(Event::FocusDone)
            }
            (Kind::Focus, Stage::Break) => {
                self.active = false;
                self.since = None;
                Some(Event::BreakDone)
            }
        }
    }

    /// Time until the displayed second changes (or the segment ends); `None` unless running.
    pub fn next_delay(&self, now: Instant) -> Option<Duration> {
        if !self.running() {
            return None;
        }
        let r = self.remaining(now).as_millis() as u64;
        if r == 0 {
            return Some(Duration::from_millis(1));
        }
        let shown = r.div_ceil(1000);
        Some(Duration::from_millis(r - (shown - 1) * 1000 + 5))
    }

    /// "sesi 2/4" while working; the session that just ended while on a break.
    pub fn session_no(&self) -> u32 {
        let n = if self.stage == Stage::Break { self.completed.saturating_sub(1) } else { self.completed };
        n % SESSIONS_PER_ROUND + 1
    }
}

fn ceil_secs(d: Duration) -> u64 {
    (d.as_millis() as u64).div_ceil(1000)
}

/// A stopwatch counting up.
#[derive(Clone, Debug, Default)]
pub struct Stopwatch {
    banked: Duration,
    since: Option<Instant>,
}

impl Stopwatch {
    pub fn running(&self) -> bool {
        self.since.is_some()
    }

    /// Started and not reset (running or paused with time on the clock).
    pub fn active(&self) -> bool {
        self.running() || !self.banked.is_zero()
    }

    pub fn start(&mut self, now: Instant) {
        if self.since.is_none() {
            self.since = Some(now);
        }
    }

    pub fn pause(&mut self, now: Instant) {
        if let Some(s) = self.since.take() {
            self.banked += now.saturating_duration_since(s);
        }
    }

    pub fn reset(&mut self) {
        *self = Stopwatch::default();
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        self.banked + self.since.map_or(Duration::ZERO, |s| now.saturating_duration_since(s))
    }

    pub fn display_secs(&self, now: Instant) -> u64 {
        self.elapsed(now).as_secs()
    }

    pub fn next_delay(&self, now: Instant) -> Option<Duration> {
        self.since?;
        let e = self.elapsed(now).as_millis() as u64;
        Some(Duration::from_millis(1000 - e % 1000 + 5))
    }
}

/// `24:59`, or `1:05:00` from one hour on.
pub fn fmt_clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

// ---- feature state ----------------------------------------------------------------------------

struct TimerState {
    cd: Countdown,
    sw: Stopwatch,
    /// The pending UI timer of the tick chain.
    tick: Option<TimerId>,
    /// "Tahan notifikasi" as toggled in the panel, until the config file catches up.
    hold_override: Option<bool>,
}

impl Default for TimerState {
    fn default() -> Self {
        TimerState { cd: Countdown::idle(), sw: Stopwatch::default(), tick: None, hold_override: None }
    }
}

/// What "Tanya AI" (`features/ai`) may know about the timer, when the user turned that source on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Brief {
    /// A focus (pomodoro) timer, not a plain countdown.
    pub focus: bool,
    pub on_break: bool,
    pub paused: bool,
    /// Seconds left in the running segment.
    pub remaining: u64,
    /// The stopwatch's seconds, when it is in use.
    pub stopwatch: Option<u64>,
}

/// The running timer / stopwatch, or `None` when neither is in use.
pub fn brief(app: &App) -> Option<Brief> {
    let st = app.state::<TimerState>();
    let now = Instant::now();
    let cd = st.cd.active.then(|| (st.cd.kind == Kind::Focus, st.cd.stage == Stage::Break, st.cd.paused(), st.cd.display_secs(now)));
    let sw = st.sw.active().then(|| st.sw.display_secs(now));
    if cd.is_none() && sw.is_none() {
        return None;
    }
    let (focus, on_break, paused, remaining) = cd.unwrap_or((false, false, false, 0));
    Some(Brief { focus, on_break, paused, remaining, stopwatch: sw })
}
pub fn register(r: &mut Registry) {
    r.add_state(TimerState::default());
    r.add_tab(TabSpec {
        id: "timer",
        order: order::TIMER,
        icon: icons::TIMER,
        label: ("Timer", "Timer"),
        enabled: |c| c.timer.enabled,
        build: panel,
        tick: None,
    });
    r.on_action("timer", on_action);
    r.on_config(on_config);
}

fn on_config(app: &mut App) {
    app.state_mut::<TimerState>().hold_override = None;
    if !app.config().timer.enabled {
        stop_all(app);
        return;
    }
    sync_hold(app);
}

/// Feature switched off: drop everything.
fn stop_all(app: &mut App) {
    let st = app.state_mut::<TimerState>();
    st.cd.reset();
    st.sw.reset();
    let tick = st.tick.take();
    if let Some(t) = tick {
        app.cancel_timer(t);
    }
    publish(app);
    app.clear_activity("timer.done");
    app.set_focus_hold(false);
}

fn hold_wanted(app: &App) -> bool {
    app.state::<TimerState>().hold_override.unwrap_or(app.config().timer.hold_notifications_in_focus)
}

fn sync_hold(app: &mut App) {
    let on = app.state::<TimerState>().cd.holds(hold_wanted(app));
    app.set_focus_hold(on);
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click || !app.config().timer.enabled {
        return;
    }
    let now = Instant::now();
    match ev.name() {
        "start" => {
            if let Some(secs) = ev.arg().and_then(|a| a.parse::<u64>().ok()) {
                app.clear_activity("timer.done");
                app.state_mut::<TimerState>().cd.start_countdown(now, Duration::from_secs(secs.clamp(1, MAX_SECS)));
                changed(app);
            }
        }
        "focus" => {
            let t = &app.config().timer;
            let (work, brk) = (Duration::from_secs(t.focus_minutes as u64 * 60), Duration::from_secs(t.break_minutes as u64 * 60));
            app.clear_activity("timer.done");
            app.state_mut::<TimerState>().cd.start_focus(now, work, brk);
            changed(app);
        }
        "toggle" => {
            let cd = &mut app.state_mut::<TimerState>().cd;
            if cd.running() {
                cd.pause(now);
            } else {
                cd.resume(now);
            }
            changed(app);
        }
        "reset" => {
            app.state_mut::<TimerState>().cd.reset();
            changed(app);
        }
        "hold" => set_hold_pref(app, !hold_wanted(app)),
        "sw_toggle" => {
            let sw = &mut app.state_mut::<TimerState>().sw;
            if sw.running() {
                sw.pause(now);
            } else {
                sw.start(now);
            }
            changed(app);
        }
        "sw_reset" => {
            app.state_mut::<TimerState>().sw.reset();
            changed(app);
        }
        "dismiss" => app.clear_activity("timer.done"),
        _ => {}
    }
}

/// The panel toggle: applies at once and is saved to `config.toml` (the settings app edits the
/// same key), so it survives a restart.
fn set_hold_pref(app: &mut App, on: bool) {
    app.state_mut::<TimerState>().hold_override = Some(on);
    if let Err(e) = app.update_config(|c| c.timer.hold_notifications_in_focus = on) {
        log_warn!("timer: cannot save config.toml: {e}");
    }
    sync_hold(app);
    app.request_redraw(Redraw::Tab("timer"));
}

/// After any state change: activities, notification hold, tick chain, redraw.
fn changed(app: &mut App) {
    publish(app);
    sync_hold(app);
    ensure_tick(app);
    app.request_redraw(Redraw::All);
}

/// Which persistent pills the state wants: (`timer.run`, `timer.watch`), both at [`priority::TIMER`].
/// A countdown, a focus session *and its break*, and a stopwatch have one while they run **or are paused**
/// (the pill shows the pause); only a finished or reset timer has none. "Hide when idle" counts on it.
fn pills_wanted(cd: &Countdown, sw: &Stopwatch) -> (bool, bool) {
    (cd.active, sw.active())
}

/// Show / remove the persistent pills to match the state.
fn publish(app: &mut App) {
    let st = app.state::<TimerState>();
    let (cd, sw) = pills_wanted(&st.cd, &st.sw);
    if cd {
        app.set_activity(Activity::persistent("timer.run", priority::TIMER, run_pill).tab("timer").bubble(run_bubble));
    } else {
        app.clear_activity("timer.run");
    }
    if sw {
        app.set_activity(Activity::persistent("timer.watch", priority::TIMER, watch_pill).tab("timer").bubble(|_| stack([icon(icons::STOPWATCH).color(color::TEAL)]).size(24.0, 24.0)));
    } else {
        app.clear_activity("timer.watch");
    }
}

/// (Re)arm the tick chain for the next second boundary, or stop it when nothing runs.
fn ensure_tick(app: &mut App) {
    let now = Instant::now();
    let st = app.state_mut::<TimerState>();
    let old = st.tick.take();
    let delay = st.cd.next_delay(now).or_else(|| st.sw.next_delay(now));
    if let Some(t) = old {
        app.cancel_timer(t);
    }
    if let Some(d) = delay {
        let id = app.after(d, on_tick);
        app.state_mut::<TimerState>().tick = Some(id);
    }
}

fn on_tick(app: &mut App) {
    app.state_mut::<TimerState>().tick = None;
    let ev = app.state_mut::<TimerState>().cd.tick(Instant::now());
    match ev {
        Some(Event::Finished) => {
            let secs = app.state::<TimerState>().cd.total.as_secs();
            let sub = tr_s(format!("{} berlalu", human_len(secs)), format!("{} elapsed", human_len(secs)));
            done_alert(app, tr("Timer selesai", "Timer done").to_string(), sub, false);
            changed(app);
        }
        Some(Event::FocusDone) => {
            let mins = app.state::<TimerState>().cd.total.as_secs() / 60;
            let sub = tr_s(format!("Istirahat {mins} mnt dimulai"), format!("{mins} min break starts"));
            done_alert(app, tr("Fokus selesai", "Focus done").to_string(), sub, false);
            changed(app);
        }
        Some(Event::BreakDone) => {
            done_alert(app, tr("Istirahat selesai", "Break over").to_string(), tr("Siap fokus lagi?", "Ready to focus again?").to_string(), true);
            changed(app);
        }
        None => {
            ensure_tick(app);
            app.request_redraw(Redraw::All);
        }
    }
}

/// `5 mnt` / `1 j 30 mnt`.
fn human_len(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    let mut parts = vec![];
    if h > 0 {
        parts.push(format!("{h} {}", tr("j", "h")));
    }
    if m > 0 {
        parts.push(format!("{m} {}", tr("mnt", "min")));
    }
    if s > 0 && h == 0 && m == 0 || parts.is_empty() {
        parts.push(format!("{s} {}", tr("dtk", "s")));
    }
    parts.join(" ")
}

/// The "done" pill (priority DUE). `start_next` offers to begin the next focus session.
fn done_alert(app: &mut App, title: String, sub: String, start_next: bool) {
    let a = Activity::transient("timer.done", priority::DUE, DONE_SECS, move |_| {
        let check = stack([icon(icons::CHECK).font_size(12.0).color(color::INK)]).size(22.0, 22.0).bg(color::GREEN).capsule();
        let action = if start_next {
            button_primary(tr("Mulai fokus", "Start focus"), "timer.focus").h(26.0)
        } else {
            button(i18n::pair(("Tutup", "Close")), "timer.dismiss").h(26.0)
        };
        pill([check, text(title.clone()).semibold(), caption(sub.clone()), action]).pad4(10.0, 0.0, 6.0, 0.0)
    })
    .tab("timer");
    app.set_activity(a);
}

// ---- pills ------------------------------------------------------------------------------------

fn time_box(s: String, c: Rgba) -> Node {
    let w = if s.len() > 5 { 58.0 } else { 40.0 };
    text(s).semibold().color(c).w(w).text_center()
}

fn run_pill(app: &App) -> Node {
    let cd = &app.state::<TimerState>().cd;
    let now = Instant::now();
    let t = fmt_clock(cd.display_secs(now));
    let paused = cd.paused();
    let tc = if paused { color::TEXT2 } else { color::WHITE };
    match (cd.kind, cd.stage) {
        (Kind::Countdown, _) => {
            let c = if paused { color::TEXT3 } else { color::ORANGE };
            let mut kids = vec![ring(cd.remaining_frac(now)).color(c)];
            if paused {
                kids.push(icon(icons::PAUSE).font_size(11.0).color(color::TEXT2));
            }
            kids.push(time_box(t, if paused { tc } else { color::ORANGE }));
            pill(kids)
        }
        (Kind::Focus, stage) => {
            let (c, label) = if stage == Stage::Work { (color::PURPLE, tr("Fokus", "Focus")) } else { (color::GREEN, tr("Istirahat", "Break")) };
            let c = if paused { color::TEXT3 } else { c };
            let mut kids = vec![icon(icons::TIMER).color(c), text(label).semibold()];
            kids.push(progress(1.0 - cd.remaining_frac(now)).w(56.0).color(c).thickness(3.0));
            if paused {
                kids.push(icon(icons::PAUSE).font_size(11.0).color(color::TEXT2));
            }
            kids.push(time_box(t, tc));
            pill(kids)
        }
    }
}

fn run_bubble(app: &App) -> Node {
    let cd = &app.state::<TimerState>().cd;
    let c = match (cd.kind, cd.stage) {
        (Kind::Countdown, _) => color::ORANGE,
        (Kind::Focus, Stage::Work) => color::PURPLE,
        (Kind::Focus, Stage::Break) => color::GREEN,
    };
    stack([ring(cd.remaining_frac(Instant::now())).color(c).size(22.0, 22.0)]).size(24.0, 24.0)
}

fn watch_pill(app: &App) -> Node {
    let sw = &app.state::<TimerState>().sw;
    let t = fmt_clock(sw.display_secs(Instant::now()));
    let mut kids = vec![icon(icons::STOPWATCH).color(color::TEAL), text(tr("Stopwatch", "Stopwatch")).semibold()];
    if !sw.running() {
        kids.push(icon(icons::PAUSE).font_size(11.0).color(color::TEXT2));
    }
    kids.push(time_box(t, color::WHITE));
    pill(kids)
}

// ---- panel ------------------------------------------------------------------------------------

fn chip(label_text: String, selected: bool, action: String) -> Node {
    let (bg, fg) = if selected { (color::WHITE, color::INK) } else { (color::FILL2, color::WHITE) };
    row([text(label_text).font_size(12.0).semibold().color(fg)]).pad_xy(12.0, 0.0).h(28.0).bg(bg).capsule().action(action)
}

fn panel(app: &App) -> Node {
    let st = app.state::<TimerState>();
    let cfg = &app.config().timer;
    let now = Instant::now();
    let cd = &st.cd;

    // Left: the big ring.
    let (big, sub, ring_color, frac) = if cd.active {
        let sub = match (cd.kind, cd.stage) {
            (Kind::Countdown, _) => format!("Timer · {}", human_len(cd.total.as_secs())),
            (Kind::Focus, Stage::Work) => tr_s(format!("Fokus · sesi {}/{}", cd.session_no(), SESSIONS_PER_ROUND), format!("Focus · session {}/{}", cd.session_no(), SESSIONS_PER_ROUND)),
            (Kind::Focus, Stage::Break) => tr_s(format!("Istirahat · sesi {}/{}", cd.session_no(), SESSIONS_PER_ROUND), format!("Break · session {}/{}", cd.session_no(), SESSIONS_PER_ROUND)),
        };
        let c = match (cd.kind, cd.stage) {
            (Kind::Countdown, _) => color::ORANGE,
            (Kind::Focus, Stage::Work) => color::PURPLE,
            (Kind::Focus, Stage::Break) => color::GREEN,
        };
        (fmt_clock(cd.display_secs(now)), sub, if cd.paused() { color::TEXT3 } else { c }, cd.remaining_frac(now))
    } else {
        (fmt_clock(cfg.focus_minutes as u64 * 60), tr("Pilih durasi", "Pick a length").to_string(), color::ORANGE, 0.0)
    };
    let big_color = if cd.active { color::WHITE } else { color::TEXT3 };
    let dial = stack([
        ring(frac).size(124.0, 124.0).thickness(9.0).color(ring_color),
        col([text(big).font_size(27.0).bold().display().color(big_color), caption(sub).font_size(11.0)]).align(Align::Center).gap(1.0),
    ])
    .size(124.0, 124.0);

    // Right: presets, controls, options card.
    let active_secs = (cd.active && cd.kind == Kind::Countdown).then_some(cd.total.as_secs());
    let mut presets: Vec<Node> = PRESETS_MIN
        .iter()
        .map(|m| chip(tr_s(format!("{m} mnt"), format!("{m} min")), active_secs == Some(m * 60), format!("timer.start:{}", m * 60)))
        .collect();
    presets.push(chip(tr_s(format!("Fokus {} mnt", cfg.focus_minutes), format!("Focus {} min", cfg.focus_minutes)), cd.active && cd.kind == Kind::Focus, "timer.focus".into()));
    let presets = row(presets).gap(8.0);

    let controls = if cd.active {
        let (glyph, name) = if cd.running() { (icons::PAUSE, i18n::pair(i18n::PAUSE)) } else { (icons::PLAY, i18n::pair(i18n::RESUME)) };
        row([
            row([icon(glyph).font_size(12.0).color(color::INK), text(name).semibold().color(color::INK)]).gap(6.0).pad_xy(14.0, 0.0).h(30.0).bg(color::WHITE).capsule().action("timer.toggle"),
            button("Reset", "timer.reset"),
        ])
        .gap(8.0)
    } else {
        row([button_primary(tr("Mulai fokus", "Start focus"), "timer.focus"), caption(tr("atau pilih durasi di atas", "or pick a length above"))]).gap(10.0)
    };

    let hold_on = st.hold_override.unwrap_or(cfg.hold_notifications_in_focus);
    let sw = &st.sw;
    let sw_btn = if sw.running() {
        button(i18n::pair(i18n::PAUSE), "timer.sw_toggle")
    } else {
        button(if sw.active() { i18n::pair(i18n::RESUME) } else { tr("Mulai", "Start") }, "timer.sw_toggle")
    };
    let mut sw_row = vec![
        icon(icons::STOPWATCH).color(color::TEXT2),
        text("Stopwatch").grow(1.0),
        text(fmt_clock(sw.display_secs(now))).tertiary().mono(),
        sw_btn.h(26.0),
    ];
    if sw.active() {
        sw_row.push(button("Reset", "timer.sw_reset").h(26.0));
    }
    let options = col([
        row([icon(icons::BELL).color(color::TEXT2), text(tr("Tahan notifikasi selama fokus", "Hold notifications during focus")).grow(1.0), toggle(hold_on)])
            .gap(10.0)
            .fill_w()
            .h(30.0)
            .action("timer.hold"),
        row(sw_row).gap(10.0).fill_w().h(30.0),
    ])
    .gap(4.0)
    .fill_w()
    .bg(color::FILL)
    .radius(18.0)
    .pad_xy(14.0, 8.0);

    let right = col([presets, controls, options]).gap(10.0).grow(1.0).align(Align::Start);
    col([row([dial, right]).gap(18.0).fill_w().align(Align::Center)])
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    /// The island in "hide when idle" is up while a timer pill is: the same priority the activity list uses.
    fn island_up(cd: &Countdown, sw: &Stopwatch) -> bool {
        let (run, watch) = pills_wanted(cd, sw);
        let top = if run || watch { priority::TIMER } else { priority::IDLE };
        crate::features::placement::pure_idle_busy(false, top)
    }

    #[test]
    fn the_focus_timer_keeps_the_idle_island_up_through_focus_pause_and_break() {
        let t0 = Instant::now();
        let sw = Stopwatch::default();
        let mut c = Countdown::idle();
        assert!(!island_up(&c, &sw), "nothing running: idle, the island may hide");
        c.start_focus(t0, 25 * MIN, 5 * MIN);
        assert!(island_up(&c, &sw), "focus session running");
        c.pause(t0 + 10 * MIN);
        assert!(island_up(&c, &sw), "a paused session still has its pill");
        c.resume(t0 + 20 * MIN);
        // Focus ends (t0 + 35): straight into the break, there is no moment without the pill.
        assert_eq!(c.tick(t0 + 35 * MIN), Some(Event::FocusDone));
        assert!(island_up(&c, &sw), "the break");
        c.pause(t0 + 36 * MIN);
        assert!(island_up(&c, &sw), "a paused break");
        c.resume(t0 + 37 * MIN);
        // The break ends: the timer is idle; the "break over / start focus" alert is a DUE activity of its own.
        assert_eq!(c.tick(t0 + 41 * MIN), Some(Event::BreakDone));
        assert!(!island_up(&c, &sw));
        // Reset in the middle of a session: idle again.
        c.start_focus(t0 + 50 * MIN, 25 * MIN, 5 * MIN);
        assert!(island_up(&c, &sw));
        c.reset();
        assert!(!island_up(&c, &sw));
    }

    #[test]
    fn a_plain_timer_and_the_stopwatch_keep_the_idle_island_up_too() {
        let t0 = Instant::now();
        let mut c = Countdown::idle();
        let mut sw = Stopwatch::default();
        c.start_countdown(t0, 5 * MIN);
        assert_eq!(pills_wanted(&c, &sw), (true, false));
        assert!(island_up(&c, &sw));
        c.pause(t0 + MIN);
        assert!(island_up(&c, &sw), "paused countdown");
        // It runs out: idle.
        c.resume(t0 + 2 * MIN);
        assert_eq!(c.tick(t0 + 7 * MIN), Some(Event::Finished));
        assert!(!island_up(&c, &sw));
        // The stopwatch: running, paused with time on the clock, and gone after a reset.
        sw.start(t0);
        assert_eq!(pills_wanted(&c, &sw), (false, true));
        assert!(island_up(&c, &sw));
        sw.pause(t0 + MIN);
        assert!(island_up(&c, &sw), "paused stopwatch");
        sw.reset();
        assert!(!island_up(&c, &sw));
        // Both at once.
        c.start_countdown(t0, MIN);
        sw.start(t0);
        assert_eq!(pills_wanted(&c, &sw), (true, true));
    }

    #[test]
    fn timer_cycle_focus_break() {
        let t0 = Instant::now();
        let mut c = Countdown::idle();
        assert!(!c.active);
        c.start_focus(t0, 25 * MIN, 5 * MIN);
        assert!(c.running() && c.kind == Kind::Focus && c.stage == Stage::Work);
        assert!(c.holds(true), "focus work holds notifications");
        assert!(!c.holds(false), "...unless the user turned it off");
        assert_eq!(c.tick(t0 + 24 * MIN + Duration::from_secs(59)), None);
        assert_eq!(c.display_secs(t0 + 10 * MIN), 15 * 60);

        // Pausing stops the clock and releases the hold.
        c.pause(t0 + 10 * MIN);
        assert!(c.paused() && !c.holds(true));
        assert_eq!(c.tick(t0 + 99 * MIN), None, "a paused timer never finishes");
        assert_eq!(c.remaining(t0 + 99 * MIN), 15 * MIN);
        c.resume(t0 + 20 * MIN);
        assert!(c.holds(true));
        assert_eq!(c.remaining(t0 + 25 * MIN), 10 * MIN);

        // Work ends at t0 + 35: the break starts by itself, without the hold.
        assert_eq!(c.tick(t0 + 35 * MIN), Some(Event::FocusDone));
        assert_eq!((c.stage, c.completed, c.total), (Stage::Break, 1, 5 * MIN));
        assert!(c.running() && !c.holds(true));
        assert_eq!(c.session_no(), 1, "the break belongs to the session that just ended");

        // Break ends: idle, the finished session stays counted.
        assert_eq!(c.tick(t0 + 39 * MIN), None);
        assert_eq!(c.tick(t0 + 40 * MIN), Some(Event::BreakDone));
        assert!(!c.active && !c.running());
        assert_eq!(c.completed, 1);
        assert_eq!(c.tick(t0 + 99 * MIN), None);
    }

    #[test]
    fn countdown_finishes_and_resets() {
        let t0 = Instant::now();
        let mut c = Countdown::idle();
        c.start_countdown(t0, 5 * MIN);
        assert_eq!(c.kind, Kind::Countdown);
        assert!((c.remaining_frac(t0 + Duration::from_secs(150)) - 0.5).abs() < 1e-3);
        assert!(!c.holds(true), "a plain timer never holds notifications");
        assert_eq!(c.tick(t0 + 5 * MIN), Some(Event::Finished));
        assert!(!c.active);
        c.start_countdown(t0, MIN);
        c.reset();
        assert!(!c.active && c.completed == 0);
    }

    #[test]
    fn late_tick_does_not_lose_time() {
        // The machine slept for an hour in a 25 minute timer: it finishes on the first tick.
        let t0 = Instant::now();
        let mut c = Countdown::idle();
        c.start_countdown(t0, 25 * MIN);
        assert_eq!(c.tick(t0 + 60 * MIN), Some(Event::Finished));
    }

    #[test]
    fn tick_delay_lands_on_the_next_displayed_second() {
        let t0 = Instant::now();
        let mut c = Countdown::idle();
        assert_eq!(c.next_delay(t0), None, "idle: no timer at all");
        c.start_countdown(t0, Duration::from_millis(60_000));
        assert_eq!(c.next_delay(t0), Some(Duration::from_millis(1005)));
        // 0.4 s later 59.6 s remain, shown as 60 → the display changes in 0.6 s.
        let d = c.next_delay(t0 + Duration::from_millis(400)).unwrap();
        assert_eq!(d, Duration::from_millis(605));
        c.pause(t0);
        assert_eq!(c.next_delay(t0), None, "paused: no timer");
        let mut s = Stopwatch::default();
        assert_eq!(s.next_delay(t0), None);
        s.start(t0);
        assert_eq!(s.next_delay(t0 + Duration::from_millis(2300)), Some(Duration::from_millis(705)));
    }

    #[test]
    fn stopwatch_counts_up_and_pauses() {
        let t0 = Instant::now();
        let mut s = Stopwatch::default();
        assert!(!s.active());
        s.start(t0);
        s.start(t0 + Duration::from_secs(5)); // a second start must not rewind
        assert_eq!(s.display_secs(t0 + Duration::from_secs(12)), 12);
        s.pause(t0 + Duration::from_secs(12));
        assert!(s.active() && !s.running());
        assert_eq!(s.display_secs(t0 + Duration::from_secs(100)), 12);
        s.start(t0 + Duration::from_secs(100));
        assert_eq!(s.display_secs(t0 + Duration::from_secs(103)), 15);
        s.reset();
        assert!(!s.active());
    }

    #[test]
    fn clock_and_length_formats() {
        assert_eq!(fmt_clock(0), "00:00");
        assert_eq!(fmt_clock(24 * 60 + 59), "24:59");
        assert_eq!(fmt_clock(3600 + 5 * 60), "1:05:00");
        assert_eq!(human_len(25 * 60), "25 mnt");
        assert_eq!(human_len(90 * 60), "1 j 30 mnt");
        assert_eq!(human_len(45), "45 dtk");
    }
}
