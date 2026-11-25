//! Feature `placement` (v0.3 track O): the polite island.
//!
//! * which monitor (`island.monitor`: primary, a device name, or follow the cursor with a
//!   300 ms debounce) and where along its top edge (`anchor` + `offset_x`) are the core's
//!   (`shell::monitors`, `Shell::target`); this feature only moves the island between monitors;
//! * **per-app rules**: the island hides in front of the apps in `island.hide_apps`
//!   (`App::set_app_hidden`, like a full-screen app), and the media feature ignores sessions of
//!   `island.media_ignore_apps` (`moku_core::apps`, shared with the settings app);
//! * **move aside** (`island.move_aside`): the island fades / shifts away from a pointer that
//!   heads for the area it covers without aiming at it ([`aside`]);
//! * **hide until the top edge** (`island.hide_until_edge`): it appears when the pointer rests on
//!   the top edge or something new arrives ([`edge`]);
//! * both away moves use `App::set_away`: compositor animations that follow Moku's reduce-motion
//!   setting (`scene::reduce`), with the input window hidden so clicks reach what is underneath.
//!
//! The pointer is read by polling `GetCursorPos` (one `after` timer chain), and only while a
//! setting needs it: every 100-200 ms in general, about every 30 ms while the pointer is near the
//! island, and every 400 ms once it has rested (also while the island is away or hidden until the
//! edge: nothing is deciding). **While the island is not on screen at all (paused, a full-screen
//! app, an app of `hide_apps` in front) the timer is not armed**: it is re-armed by
//! `Visibility(true)`. No hooks, nothing is ever sent to another window. `mokuctl debug cursor x y` feeds
//! fake positions (with a virtual clock) into the same [`sample`] path, so the behaviour is
//! tested without moving the real cursor.

pub mod aside;
pub mod edge;

use std::time::{Duration, Instant};

use moku_core::apps;
use moku_core::config::{MonitorChoice, MoveAside};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};

use self::aside::{Aside, Sample, Side, State};
use self::edge::Edge;
use crate::activity::priority;
use crate::app::{App, Mode, RunMode, TimerId};
use crate::registry::*;
use crate::scene::WIN_W;
use crate::scene::reduce::Away;
use crate::shell::monitors::{self, Follow, IRect, Mon};

/// Poll the pointer this often while move aside is on and nothing is near the island (ms): fast
/// enough to see a pointer arrive in the zone around it.
pub const POLL_FAR_MS: u64 = 100;
/// ... when only "follow the cursor" / "hide until the edge" need the pointer (ms): a 300 ms
/// debounce and a 120 ms dwell do not need more.
pub const POLL_SLOW_MS: u64 = 200;
/// ... near the island (move aside is deciding), ms.
pub const POLL_NEAR_MS: u64 = 24;
/// ... near the top edge while the island waits for it, ms.
pub const POLL_EDGE_MS: u64 = 40;
/// While the pointer is faked or the island is hidden: just check back, ms.
pub const POLL_IDLE_MS: u64 = 500;
/// A pointer that has not moved for this long (ms) is resting ...
pub const REST_AFTER_MS: u64 = 1500;
/// ... and is looked at this seldom while it is far from everything (ms). The first move is seen
/// up to this late; the next look is already at the fast rate.
pub const POLL_REST_MS: u64 = 400;

/// A fake pointer (`mokuctl debug cursor`) with its virtual clock.
struct Fake {
    x: i32,
    y: i32,
    t: u64,
}

pub struct Placement {
    aside: Aside,
    edge: Edge,
    follow: Follow,
    /// The edge machine has been started for the current setting.
    edge_on: bool,
    ticker: Option<TimerId>,
    /// Time zero of the real pointer's samples.
    t0: Instant,
    fake: Option<Fake>,
    /// Where the real pointer was at the last tick and since when (ms), for the rest back-off.
    still: Option<(i32, i32, u64)>,
    news_seen: u64,
    /// `media_ignore_apps` as of the last config, to re-pick the media session on a change.
    media_ignore: Vec<String>,
    /// `placement.debug_fg:<exe>`: a pretend foreground app (tests).
    fake_fg: Option<String>,
    last: Option<Sample>,
}

impl Default for Placement {
    fn default() -> Self {
        Placement {
            aside: Aside::default(),
            edge: Edge::default(),
            follow: Follow::default(),
            edge_on: false,
            ticker: None,
            t0: Instant::now(),
            fake: None,
            still: None,
            news_seen: 0,
            media_ignore: vec![],
            fake_fg: None,
            last: None,
        }
    }
}

pub fn register(r: &mut Registry) {
    r.add_state(Placement::default());
    r.on_config(on_config);
    r.on_action("placement", on_action);
    r.on_event(on_event);
}

// ---- pure decisions ----------------------------------------------------------------------------

/// Horizontal shift (DIP, positive = right) of a sliding island: clear of its own width, as far
/// as the visual window has room, away from where the pointer came from.
pub fn slide_dx(side: Side, home: IRect, scale: f32) -> f32 {
    let w = home.w() as f32 / scale.max(0.1);
    let room = ((WIN_W - w) / 2.0 - 6.0).max(0.0);
    let dx = (w + 24.0).min(room);
    match side {
        Side::Right => dx,
        Side::Left => -dx,
    }
}

/// Where the island should be, from the two machines: hidden until the edge wins; else, with
/// move aside on and the pointer in the way, a fade or a slide.
pub fn compose(edge_shown: bool, aside: State, mode: MoveAside, home: IRect, scale: f32) -> Away {
    if !edge_shown {
        return Away::Hide;
    }
    match (aside, mode) {
        (State::Away(_), MoveAside::Fade) => Away::Fade,
        (State::Away(side), MoveAside::Slide) => Away::Slide { dx: slide_dx(side, home, scale) },
        _ => Away::Home,
    }
}

/// When to read the pointer next (ms): often while it is near the island (or the island is away,
/// watching for the way back) and move aside may decide something, a bit often near the top edge
/// while the island waits for it, else seldom, and seldomer still once the pointer has rested
/// (also while away: a pointer that sits still decides nothing, the first move is seen at most
/// [`POLL_REST_MS`] late and the next look is fast again).
pub fn poll_ms(aside_on: bool, edge_on: bool, s: Sample, home: Option<IRect>, mon: IRect, scale: f32, away: bool, resting: bool) -> u64 {
    if aside_on {
        let reach = ((aside::NEAR + aside::ZONE) * scale).round() as i32;
        if away {
            return if resting { POLL_REST_MS } else { POLL_NEAR_MS };
        }
        if home.is_some_and(|h| h.inflate(reach, reach).contains(s.x, s.y)) {
            return POLL_NEAR_MS;
        }
    }
    if edge_on && s.y <= mon.t + (80.0 * scale) as i32 {
        return POLL_EDGE_MS;
    }
    let base = if aside_on { POLL_FAR_MS } else { POLL_SLOW_MS };
    if resting { base.max(POLL_REST_MS) } else { base }
}

/// How long the pointer has been at the same place: `prev` = (x, y, since) of the last sample.
/// Returns the new (x, y, since) and the milliseconds it has not moved.
pub fn track_still(prev: Option<(i32, i32, u64)>, s: Sample) -> ((i32, i32, u64), u64) {
    match prev {
        Some((x, y, since)) if (x, y) == (s.x, s.y) => ((x, y, since), s.t.saturating_sub(since)),
        _ => ((s.x, s.y, s.t), 0),
    }
}

/// Invented monitors for tests (`placement.debug_monitors:1920x1080@0,0;2560x1440@1920,0*1.25`):
/// `WxH@X,Y` with an optional `*dpi`; named `\\.\DISPLAY1`..; the first is the primary one.
pub fn parse_fake_monitors(spec: &str) -> Vec<Mon> {
    let mut out = vec![];
    for (i, part) in spec.split(';').map(str::trim).filter(|p| !p.is_empty()).enumerate() {
        let (geo, dpi) = part.split_once('*').map_or((part, 1.0), |(g, d)| (g, d.trim().parse::<f32>().ok().filter(|d| (0.5..=4.0).contains(d)).unwrap_or(1.0)));
        let parsed = geo.split_once('@').and_then(|(size, at)| {
            let (w, h) = size.split_once('x')?;
            let (x, y) = at.split_once(',')?;
            Some((w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?, x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?))
        });
        if let Some((w, h, x, y)) = parsed.filter(|(w, h, ..)| *w > 0 && *h > 0) {
            out.push(Mon { name: format!(r"\\.\DISPLAY{}", i + 1), rect: IRect::new(x, y, x + w, y + h), dpi, primary: i == 0 });
        }
    }
    out
}

// ---- the pointer -------------------------------------------------------------------------------

fn real_sample(t0: Instant) -> Option<Sample> {
    let (x, y) = monitors::cursor_pos()?;
    // Only "is a button held", to tell a click from a rest (nothing is recorded or sent).
    let down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000 != 0;
    Some(Sample { x, y, t: t0.elapsed().as_millis() as u64, down })
}

/// The latest pointer sample: the fake one while `debug cursor` drives, else the real pointer.
fn current_sample(app: &App) -> Option<Sample> {
    let st = app.state::<Placement>();
    match &st.fake {
        Some(f) => Some(Sample { x: f.x, y: f.y, t: f.t, down: st.last.is_some_and(|l| l.down) }),
        None => real_sample(st.t0),
    }
}

/// The panel or a sheet is open, or something urgent is on: the island must stay put.
fn busy(app: &App) -> bool {
    matches!(app.mode, Mode::Panel | Mode::Sheet) || app.top_priority() >= priority::DUE
}

/// The panel or a sheet is open, or a text field has the keyboard: the island must not change
/// monitors under the user's hands.
fn ui_open(app: &App) -> bool {
    matches!(app.mode, Mode::Panel | Mode::Sheet) || app.focused_input().is_some()
}

/// "Hide until the top edge" must not hide what the user may need to answer: the panel or a sheet is
/// open, something with priority 80 (a notification) or more is on, or the pill has a button / an
/// action of its own ([`App::pill_is_actionable`]).
fn edge_busy(app: &App) -> bool {
    pure_edge_busy(matches!(app.mode, Mode::Panel | Mode::Sheet), app.top_priority(), || app.pill_is_actionable())
}

/// [`edge_busy`] without the app: `ui` = panel or sheet open, `top` = priority of the pill's
/// activity (0 = idle), `actionable` is asked only when the rest did not decide.
pub fn pure_edge_busy(ui: bool, top: u8, actionable: impl FnOnce() -> bool) -> bool {
    ui || top >= priority::NOTIFICATION || (top > 0 && actionable())
}

/// One pointer sample through everything: follow the cursor, hide until the edge, move aside.
/// The timer calls it with the real pointer, `mokuctl debug cursor` with a fake one.
fn sample(app: &mut App, s: Sample) {
    let cfg = app.config_arc();
    let isl = &cfg.island;
    app.state_mut::<Placement>().last = Some(s);

    // Follow the cursor: the monitor under it, after the debounce. Not while the panel or a sheet
    // is open or a text field has the keyboard: the island does not jump away from what the user
    // is using (the debounce starts over once they are done).
    if isl.monitor_choice() == MonitorChoice::Cursor {
        if ui_open(app) {
            app.state_mut::<Placement>().follow.forget_candidate();
        } else {
            let under = monitors::monitor_at(app.monitors(), s.x, s.y).map(|i| app.monitors()[i].name.clone());
            if let Some(name) = under
                && let Some(go) = app.state_mut::<Placement>().follow.observe(s.t, &name)
            {
                app.state_mut::<Placement>().aside.reset();
                app.set_followed_monitor(Some(&go));
            }
        }
    }

    // Nothing is drawn while the island is hidden: nothing to decide.
    if app.is_hidden() {
        return;
    }
    let Some(home) = app.home_rect() else { return };
    let (mon, scale, busy) = (app.monitor_rect(), app.scale(), busy(app));
    let edge_busy = isl.hide_until_edge && edge_busy(app);
    // A monitor above the island's: the pointer crosses the top edge at once, so the reveal band is wider.
    let above = isl.hide_until_edge && {
        let rects: Vec<IRect> = app.monitors().iter().map(|m| m.rect).collect();
        edge::monitor_above(&rects, mon, edge::zone_x(home, scale))
    };
    let news_now = app.activity_news();
    let eligible = !busy && app.focused_input().is_none();
    let st = app.state_mut::<Placement>();
    let news = news_now > st.news_seen;
    st.news_seen = news_now;
    let edge_shown = if isl.hide_until_edge { st.edge.step(edge::In { s, home, mon, scale, news, busy: edge_busy, above }) } else { true };
    let aside_state = if isl.move_aside != MoveAside::Off { st.aside.step(s, home, scale, eligible) } else { State::Home };
    app.set_away(compose(edge_shown, aside_state, isl.move_aside, home, scale));
}

/// Take a sample now (the panel opened or closed, the island was shown again ...), so a state
/// change is seen at once, not at the next poll.
fn refresh_now(app: &mut App) {
    if app.run_mode != RunMode::Interactive || !app.config().island.watches_pointer() {
        return;
    }
    if let Some(s) = current_sample(app) {
        sample(app, s);
    }
}

fn tick(app: &mut App) {
    app.state_mut::<Placement>().ticker = None;
    if !app.config().island.watches_pointer() {
        return;
    }
    // Nothing is on screen (paused, a full-screen app, an app of `hide_apps` in front, a capture):
    // there is nothing to decide, so the timer is not re-armed at all. `Visibility(true)` starts it again.
    if app.is_hidden() {
        return;
    }
    let ms = if app.state::<Placement>().fake.is_some() {
        POLL_IDLE_MS
    } else if let Some(s) = real_sample(app.state::<Placement>().t0) {
        sample(app, s);
        let st = app.state_mut::<Placement>();
        let (still, for_ms) = track_still(st.still, s);
        st.still = Some(still);
        // "Away" here is move aside's: an island that edge-hiding keeps out of sight has nothing to
        // watch for near itself (the edge zone has its own rate in `poll_ms`).
        let aside_away = matches!(st.aside.state(), State::Away(_));
        let isl = &app.config().island;
        poll_ms(isl.move_aside != MoveAside::Off, isl.hide_until_edge, s, app.home_rect(), app.monitor_rect(), app.scale(), aside_away, for_ms >= REST_AFTER_MS)
    } else {
        POLL_FAR_MS
    };
    arm(app, ms);
}

/// The island is on screen again: start the pointer timer if a setting needs it, and look at once.
fn wake(app: &mut App) {
    if app.run_mode == RunMode::Interactive && app.config().island.watches_pointer() && app.state::<Placement>().ticker.is_none() && !app.is_hidden() {
        arm(app, POLL_FAR_MS);
    }
    refresh_now(app);
}

/// The island went off screen: stop the pointer timer (it is armed again by [`wake`]).
fn sleep(app: &mut App) {
    if let Some(t) = app.state_mut::<Placement>().ticker.take() {
        app.cancel_timer(t);
    }
    app.state_mut::<Placement>().still = None;
}

fn arm(app: &mut App, ms: u64) {
    let id = app.after(Duration::from_millis(ms), tick);
    app.state_mut::<Placement>().ticker = Some(id);
}

/// Start, stop and reset the polling and the two machines to match the settings.
fn sync(app: &mut App) {
    let cfg = app.config_arc();
    let isl = &cfg.island;
    let watch = isl.watches_pointer() && app.run_mode == RunMode::Interactive;
    let news = app.activity_news();
    let target_name = app.shell.target.name.clone();
    let st = app.state_mut::<Placement>();
    if isl.move_aside == MoveAside::Off {
        st.aside.reset();
    }
    match (isl.hide_until_edge, st.edge_on) {
        (true, false) => {
            st.edge = Edge::default();
            st.edge_on = true;
            st.news_seen = news;
        }
        (false, true) => {
            st.edge.disable();
            st.edge_on = false;
        }
        _ => {}
    }
    let following = isl.monitor_choice() == MonitorChoice::Cursor;
    if following {
        if st.follow.settled().is_none() && !target_name.is_empty() {
            st.follow = Follow::new(Some(&target_name));
        }
    } else {
        st.follow = Follow::default();
    }
    let ticking = st.ticker.is_some();
    if !following {
        // Not following any more: forget the monitor it had settled on.
        app.set_followed_monitor(None);
    }
    if watch {
        // A hidden island is not watched: `Visibility(true)` arms the timer.
        if !ticking && !app.is_hidden() {
            arm(app, POLL_FAR_MS);
        }
        // Apply the new settings to what is on screen right away.
        refresh_now(app);
    } else {
        if let Some(t) = app.state_mut::<Placement>().ticker.take() {
            app.cancel_timer(t);
        }
        if !isl.watches_pointer() {
            app.set_away(Away::Home);
        }
    }
}

fn on_config(app: &mut App) {
    let list = app.config().island.media_ignore_apps.clone();
    let st = app.state_mut::<Placement>();
    if st.media_ignore != list {
        st.media_ignore = list;
        crate::features::media::repick();
    }
    update_rule(app);
    sync(app);
}

fn on_event(app: &mut App, ev: &AppEvent) {
    match ev {
        AppEvent::ForegroundChanged => update_rule(app),
        AppEvent::Visibility(true) => wake(app),
        AppEvent::Visibility(false) => sleep(app),
        AppEvent::PanelOpened | AppEvent::PanelClosed | AppEvent::SheetOpened(_) | AppEvent::SheetClosed(_) => refresh_now(app),
        AppEvent::DisplayChanged => {
            // The monitors changed (or the island moved): the old samples and rectangles are stale.
            let name = app.shell.target.name.clone();
            let st = app.state_mut::<Placement>();
            st.aside.reset();
            if app.config().island.monitor_choice() == MonitorChoice::Cursor && !name.is_empty() {
                app.state_mut::<Placement>().follow.settle(&name);
            }
        }
        _ => {}
    }
}

// ---- per-app hide rules ------------------------------------------------------------------------

/// Hide the island while the foreground app is one of `island.hide_apps`.
fn update_rule(app: &mut App) {
    let list = &app.config().island.hide_apps;
    if list.is_empty() {
        return app.set_app_hidden(false);
    }
    // Moku's own windows (settings) and "no foreground window" keep what we had.
    let fg = app.state::<Placement>().fake_fg.clone().or_else(apps::foreground_exe);
    if let Some(exe) = fg {
        let hide = apps::exe_matches(&exe, &app.config().island.hide_apps);
        app.set_app_hidden(hide);
    }
}

// ---- debug / sample hooks ----------------------------------------------------------------------

/// `placement.debug_fg:<exe>` (a pretend foreground app, `-` = the real one) and
/// `placement.debug_monitors:<spec>` (invented monitors, empty = the real ones). Gated by
/// `MOKU_DEBUG` like every `debug_` action.
fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    match ev.name() {
        "debug_fg" => {
            let arg = ev.arg().unwrap_or("-").trim().to_string();
            app.state_mut::<Placement>().fake_fg = (arg != "-" && !arg.is_empty()).then_some(arg);
            update_rule(app);
        }
        "debug_monitors" => {
            let fake = parse_fake_monitors(ev.arg().unwrap_or(""));
            app.shell.fake_monitors = (!fake.is_empty()).then_some(fake);
            app.reposition();
        }
        _ => {}
    }
}

/// The machines keep times: the real pointer's samples count milliseconds since the island started,
/// a fake pointer's since its first sample. Switching between the two starts them afresh, or old
/// times from the other clock would swallow the new samples.
fn reset_clock(app: &mut App) {
    let on = app.config().island.hide_until_edge;
    let st = app.state_mut::<Placement>();
    st.aside.reset();
    st.follow.forget_candidate();
    if on {
        st.edge = Edge::default();
    } else {
        st.edge.disable();
    }
}

/// `mokuctl debug cursor`: a fake pointer (or `None`: the real one again). Returns the state.
pub fn debug_cursor(app: &mut App, pos: Option<(i32, i32)>, dt: Option<u32>, down: bool) -> Result<serde_json::Value, String> {
    match pos {
        None => {
            app.state_mut::<Placement>().fake = None;
            reset_clock(app);
        }
        Some((x, y)) => {
            if app.state::<Placement>().fake.is_none() {
                reset_clock(app);
            }
            let st = app.state_mut::<Placement>();
            let t = st.fake.as_ref().map_or(0, |f| f.t + dt.unwrap_or(16) as u64);
            st.fake = Some(Fake { x, y, t });
            sample(app, Sample { x, y, t, down });
        }
    }
    Ok(debug_json(app))
}

/// What `debug state` shows about this feature.
pub fn debug_json(app: &App) -> serde_json::Value {
    let st = app.state::<Placement>();
    let isl = &app.config().island;
    serde_json::json!({
        "pointer_watch": isl.watches_pointer(),
        "polling": st.ticker.is_some(),
        "fake_pointer": st.fake.as_ref().map(|f| serde_json::json!({ "x": f.x, "y": f.y, "t": f.t })),
        "aside": match st.aside.state() { State::Home => "home".to_string(), State::Away(Side::Left) => "away-left".into(), State::Away(Side::Right) => "away-right".into() },
        "verdict": format!("{:?}", st.aside.last).to_lowercase(),
        "edge_shown": st.edge.shown() || !isl.hide_until_edge,
        "followed": st.follow.settled(),
        "fake_foreground": st.fake_fg.is_some(),
        "monitors": app.monitors().iter().map(|m| serde_json::json!({ "name": m.name, "rect": [m.rect.l, m.rect.t, m.rect.r, m.rect.b], "dpi": m.dpi, "primary": m.primary })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: IRect = IRect::new(900, 8, 1020, 44);
    const MON: IRect = IRect::new(0, 0, 1920, 1080);
    fn s(x: i32, y: i32) -> Sample {
        Sample { x, y, t: 0, down: false }
    }

    #[test]
    fn the_two_machines_compose_into_one_away_state() {
        let away = State::Away(Side::Right);
        // Hidden until the edge wins over everything.
        assert_eq!(compose(false, State::Home, MoveAside::Off, HOME, 1.0), Away::Hide);
        assert_eq!(compose(false, away, MoveAside::Fade, HOME, 1.0), Away::Hide);
        // Shown: move aside decides.
        assert_eq!(compose(true, State::Home, MoveAside::Slide, HOME, 1.0), Away::Home);
        assert_eq!(compose(true, away, MoveAside::Fade, HOME, 1.0), Away::Fade);
        assert_eq!(compose(true, away, MoveAside::Off, HOME, 1.0), Away::Home, "switched off meanwhile");
        assert!(matches!(compose(true, away, MoveAside::Slide, HOME, 1.0), Away::Slide { dx } if dx > 0.0));
        assert!(matches!(compose(true, State::Away(Side::Left), MoveAside::Slide, HOME, 1.0), Away::Slide { dx } if dx < 0.0));
    }

    #[test]
    fn a_slide_clears_the_island_and_stays_in_the_window() {
        // A 120 DIP pill: its width + 24 DIP, the window has 332 DIP of room each side.
        assert_eq!(slide_dx(Side::Right, HOME, 1.0), 144.0);
        assert_eq!(slide_dx(Side::Left, HOME, 1.0), -144.0);
        // At 200 % the same pill is 240 px wide: the same 144 DIP.
        assert_eq!(slide_dx(Side::Right, IRect::new(1800, 16, 2040, 88), 2.0), 144.0);
        // A wide pill (600 DIP): limited by the room in the 784 DIP window (92 - 6).
        assert_eq!(slide_dx(Side::Right, IRect::new(660, 8, 1260, 44), 1.0), 86.0);
        // As wide as the window: no room, no slide (it still fades).
        assert_eq!(slide_dx(Side::Right, IRect::new(568, 8, 1352, 44), 1.0), 0.0);
    }

    #[test]
    fn polling_is_fast_only_where_a_decision_may_come() {
        // Nothing near, nothing hidden: slow (move aside keeps an eye on the zone around the island).
        assert_eq!(poll_ms(true, false, s(300, 600), Some(HOME), MON, 1.0, false, false), POLL_FAR_MS);
        assert_eq!(poll_ms(false, false, s(300, 600), Some(HOME), MON, 1.0, false, false), POLL_SLOW_MS, "only following the cursor");
        // Move aside on and the pointer within ~300 DIP of the island: fast.
        assert_eq!(poll_ms(true, false, s(700, 100), Some(HOME), MON, 1.0, false, false), POLL_NEAR_MS);
        assert_eq!(poll_ms(true, false, s(300, 600), Some(HOME), MON, 1.0, true, false), POLL_NEAR_MS, "away: watching for the way back");
        // Move aside off: the island's surroundings do not matter.
        assert_eq!(poll_ms(false, false, s(960, 20), Some(HOME), MON, 1.0, false, false), POLL_SLOW_MS);
        // Waiting for the top edge: a bit faster near the top.
        assert_eq!(poll_ms(false, true, s(960, 30), Some(HOME), MON, 1.0, false, false), POLL_EDGE_MS);
        assert_eq!(poll_ms(false, true, s(960, 500), Some(HOME), MON, 1.0, false, false), POLL_SLOW_MS);
        // A pointer that has been resting far from everything is looked at seldom; near the island
        // or the top edge, or while away, the fast rate stays.
        assert_eq!(poll_ms(true, false, s(300, 600), Some(HOME), MON, 1.0, false, true), POLL_REST_MS);
        assert_eq!(poll_ms(false, false, s(300, 600), Some(HOME), MON, 1.0, false, true), POLL_REST_MS);
        assert_eq!(poll_ms(true, false, s(700, 100), Some(HOME), MON, 1.0, false, true), POLL_NEAR_MS);
        assert_eq!(poll_ms(false, true, s(960, 20), Some(HOME), MON, 1.0, false, true), POLL_EDGE_MS);
        // Away and the pointer at rest (island moved aside, or hidden until the edge, pointer parked): seldom,
        // the first move is seen at most POLL_REST_MS late; moving, or waiting for the edge, stays fast.
        assert_eq!(poll_ms(true, false, s(300, 600), Some(HOME), MON, 1.0, true, true), POLL_REST_MS);
        assert_eq!(poll_ms(true, false, s(300, 600), Some(HOME), MON, 1.0, true, false), POLL_NEAR_MS);
        assert_eq!(poll_ms(true, true, s(960, 500), Some(HOME), MON, 1.0, true, true), POLL_REST_MS);
        assert_eq!(poll_ms(true, true, s(200, 20), Some(HOME), MON, 1.0, false, true), POLL_EDGE_MS);
        // The whole polite set (aside + edge + follow) with the pointer parked far away: the slowest rate.
        let idle = poll_ms(true, true, s(300, 600), Some(HOME), MON, 1.0, false, true);
        assert_eq!(idle, POLL_REST_MS);
        assert!(idle >= 4 * POLL_NEAR_MS);
        // The zone scales with the display.
        assert_eq!(poll_ms(true, false, s(700, 100), Some(IRect::new(1800, 16, 2040, 88)), IRect::new(0, 0, 3840, 2160), 2.0, false, false), POLL_FAR_MS);
    }

    #[test]
    fn a_pointer_that_does_not_move_is_resting_after_a_while() {
        let at = |x, y, t| Sample { x, y, t, down: false };
        let (st, ms) = track_still(None, at(5, 5, 100));
        assert_eq!((st, ms), ((5, 5, 100), 0));
        let (st, ms) = track_still(Some(st), at(5, 5, 400));
        assert_eq!((st, ms), ((5, 5, 100), 300), "still since t = 100");
        let (st, ms) = track_still(Some(st), at(5, 5, 1700));
        assert_eq!(ms, 1600);
        assert!(ms >= REST_AFTER_MS);
        // One pixel of movement starts it over.
        let (st, ms) = track_still(Some(st), at(6, 5, 1800));
        assert_eq!((st, ms), ((6, 5, 1800), 0));
    }

    #[test]
    fn hide_until_edge_keeps_what_can_be_answered() {
        // The panel or a sheet, a notification (80) or louder, or a pill with a button of its own.
        assert!(pure_edge_busy(true, 0, || false));
        assert!(pure_edge_busy(false, 80, || false));
        assert!(pure_edge_busy(false, 90, || false));
        assert!(pure_edge_busy(false, 100, || false));
        assert!(pure_edge_busy(false, 50, || true), "a timer pill with its buttons");
        // Music (40) with no button of its own, the idle clock, a heads-up: free to hide.
        assert!(!pure_edge_busy(false, 40, || false));
        assert!(!pure_edge_busy(false, 30, || false));
        assert!(!pure_edge_busy(false, 79, || false));
        // The idle pill (0) is never asked.
        assert!(!pure_edge_busy(false, 0, || panic!("an idle pill has nothing to ask")));
    }

    #[test]
    fn a_pill_with_a_button_is_actionable_but_a_root_action_is_not() {
        use crate::ui::*;
        assert!(!pill([text("Lagu")]).has_inner_action());
        assert!(!pill([text("Lagu")]).action("media.pill").has_inner_action(), "the music pill's own scroll / double-click");
        assert!(pill([text("Selesai"), button("Tunda", "timer.snooze")]).has_inner_action());
        assert!(pill([text("Selesai"), row([text("x"), icon_button(icons::CLOSE, "x.close")])]).has_inner_action(), "nested");
        assert!(!empty().has_inner_action());
    }

    #[test]
    fn invented_monitors_parse() {
        let m = parse_fake_monitors("1920x1080@0,0;2560x1440@1920,0*1.25 ; 1280x1024@-1280,-120");
        assert_eq!(m.len(), 3);
        assert_eq!((m[0].name.as_str(), m[0].rect, m[0].primary, m[0].dpi), (r"\\.\DISPLAY1", IRect::new(0, 0, 1920, 1080), true, 1.0));
        assert_eq!((m[1].name.as_str(), m[1].rect, m[1].primary, m[1].dpi), (r"\\.\DISPLAY2", IRect::new(1920, 0, 4480, 1440), false, 1.25));
        assert_eq!(m[2].rect, IRect::new(-1280, -120, 0, 904));
        // Junk is skipped; nothing usable -> empty (= the real monitors).
        assert_eq!(parse_fake_monitors("junk;0x0@0,0;1920x1080").len(), 0);
        assert_eq!(parse_fake_monitors("").len(), 0);
        assert_eq!(parse_fake_monitors("100x100@0,0*99")[0].dpi, 1.0, "an absurd dpi is ignored");
    }
}
