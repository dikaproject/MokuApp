//! `App`: the island's state on the UI thread, and the API features use.
//!
//! Threading: `App` lives in a thread-local on the UI thread (the thread that runs the message
//! loop and owns the compositor). Every `&mut App` method is UI-thread only. Other threads talk
//! to it only through [`Ui::post`], which queues a closure and wakes the message loop.
//!
//! Redraws are lazy: anything that may change what is shown marks the app dirty
//! (`request_redraw`, `set_activity`, ...) and one flush per message batch rebuilds the visible
//! trees, diffs them against the last ones, and only then touches the compositor.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use moku_core::Config;
use moku_core::ipc::{DebugOp, Request, Response};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer};

use crate::activity::{Activity, Stack};
use crate::ipc_server::Reply;
use crate::reentrant::Reentrant;
use crate::registry::*;
use crate::scene::reduce::{self, Away};
use crate::scene::spring::motion;
use crate::scene::{Block, Content, Island, PANEL_BODY_MAX_H, PANEL_W, SceneOpts};
use crate::services::Services;
use crate::shell::{self, Shell};
use crate::ui::layout::{self, Hit, HitKind, Laid, Live};
use crate::ui::text_edit::{Key, KeyPress, Outcome, TextEdit};
use crate::ui::{self, Kind, Node, Rect, Rgba, TabItem, icons};
use crate::{demo, i18n};

// ---- cross-thread dispatcher ------------------------------------------------------------------

type Job = Box<dyn FnOnce(&mut App) + Send>;

struct UiInner {
    tx: Mutex<Sender<Job>>,
    hwnd: isize,
}

static UI: OnceLock<UiInner> = OnceLock::new();
static CONFIG: RwLock<Option<Arc<Config>>> = RwLock::new(None);

/// Window messages private to the island.
pub const WM_APP_RUN: u32 = 0x8000 + 1;
pub const WM_APP_FLUSH: u32 = 0x8000 + 2;
pub const WM_APP_OPEN: u32 = 0x8000 + 3;
pub const WM_APP_TRAY: u32 = 0x8000 + 4;
pub const WM_APP_APPBAR: u32 = 0x8000 + 5;

/// Handle for running code on the UI thread from anywhere.
pub struct Ui;

impl Ui {
    /// Queue `f` to run on the UI thread with `&mut App`. Callable from any thread (services,
    /// WinRT event handlers, COM callbacks). Runs in order; never blocks the caller.
    pub fn post(f: impl FnOnce(&mut App) + Send + 'static) {
        if let Some(u) = UI.get() {
            if let Ok(tx) = u.tx.lock() {
                let _ = tx.send(Box::new(f));
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(u.hwnd as _)), WM_APP_RUN, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// The current config, from any thread.
pub fn current_config() -> Arc<Config> {
    CONFIG.read().ok().and_then(|g| g.clone()).unwrap_or_default()
}

fn set_current_config(c: Arc<Config>) {
    if let Ok(mut g) = CONFIG.write() {
        *g = Some(c);
    }
}

thread_local! {
    static APP: Reentrant<App> = const { Reentrant::new() };
}

/// Run `f` with the app (UI thread). Returns `None` if the app is busy (re-entrant call from a
/// modal loop / synchronous COM) or not created yet — use [`with_app_or_defer`] for work that
/// must not be lost.
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    let r = APP.with(|a| a.run(f))?;
    after_app();
    Some(r)
}

/// Run `f` with the app now, or right after the borrow that is in progress (re-entrant call).
pub fn with_app_or_defer(f: impl FnOnce(&mut App) + 'static) {
    let now = APP.with(|a| {
        let busy = a.is_busy();
        a.run_or_defer(f) && !busy
    });
    if now {
        after_app();
    }
}

/// After a borrow: post one flush if something changed, run `run_outside` closures.
fn after_app() {
    let outside = APP
        .with(|a| {
            a.run(|app| {
                if app.dirty && !app.flush_posted {
                    app.flush_posted = true;
                    unsafe {
                        let _ = PostMessageW(Some(app.shell.visual), WM_APP_FLUSH, WPARAM(0), LPARAM(0));
                    }
                }
                std::mem::take(&mut app.outside)
            })
        })
        .unwrap_or_default();
    for f in outside {
        f();
    }
}

pub(crate) fn install(app: App) {
    APP.with(|a| a.set(Some(app)));
}

/// Run the exit hooks and stop services (joined, ≤ 1 s in total). Idempotent; called on
/// normal exit and on `WM_ENDSESSION`.
pub(crate) fn shutdown() {
    with_app(|app| {
        if app.shut_down {
            return;
        }
        app.shut_down = true;
        for f in app.on_exit.clone() {
            f(app);
        }
        // Pending debounced saves now, and let the writer thread finish them.
        app.flush_saves();
        if !crate::persist::drain(Duration::from_millis(800)) {
            log_warn!("exit: some saves did not finish in time");
        }
        app.services.stop_all_and_join(Duration::from_secs(1));
        app.shell.teardown();
    });
}

pub(crate) fn uninstall() {
    shutdown();
    APP.with(|a| a.set(None));
}

// ---- debug / sample actions -----------------------------------------------------------------

/// Test hooks that are not `<feature>.sample…` / `<feature>.debug_…` by name.
const DEBUG_ACTIONS: &[&str] = &["volume.test"];

/// Sample / debug action ids (fake media sessions, fake drops, sample pills …): the part before
/// the first `:` is `<feature>.sample…`, `<feature>.debug_…` or one of [`DEBUG_ACTIONS`].
pub fn is_debug_action(id: &str) -> bool {
    let head = id.split(':').next().unwrap_or(id);
    let name = head.split_once('.').map_or("", |(_, n)| n);
    name.starts_with("sample") || name.starts_with("debug_") || DEBUG_ACTIONS.contains(&head)
}

/// Debug actions run only in debug builds or with `MOKU_DEBUG=1` (checked once).
pub fn debug_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| cfg!(debug_assertions) || std::env::var("MOKU_DEBUG").is_ok_and(|v| v == "1"))
}

/// Which `mokuctl debug` ops the pipe accepts. Every op except the read-only `state` drives the
/// UI (a `click` reaches any action, e.g. `ai.approve` on a pending permission card), so it is
/// only served when [`debug_enabled`] — never by a release island started without `MOKU_DEBUG=1`.
pub fn debug_op_allowed(op: &DebugOp, debug: bool) -> bool {
    debug || matches!(op, DebugOp::State)
}

/// The error a rejected debug op gets.
pub const DEBUG_OP_DISABLED: &str = "debug ops are disabled: start moku.exe with MOKU_DEBUG=1 (only `debug state` is always available)";

// ---- which tab a reopened panel shows ------------------------------------------------------

/// The activity the pill shows, as far as [`reopen_tab`] cares.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PillActivity<'a> {
    pub tab: Option<&'a str>,
    pub priority: u8,
    /// `Entry::changed`: when it last became news.
    pub changed: u64,
}

/// The tab a panel opened *without* a target shows (hover, a click on the pill, the hotkey, a
/// second launch). Pure, so the rule is unit-tested:
///
/// 1. a pending AI ask (`ai_ask` = its tab) always wins;
/// 2. else the pill's activity, if it became news after the panel last closed (`closed_at`),
///    ranks above music and has an enabled tab — a new notification, a timer that just finished;
///    music that keeps playing or changes track never steals the tab;
/// 3. else the tab the user last chose (`remembered`: tab bar click / scroll, a bubble click,
///    `island.open:<tab>`), if it is still enabled;
/// 4. else the pill activity's tab (nothing chosen yet), else the first enabled tab.
pub(crate) fn reopen_tab<'a>(remembered: Option<&'a str>, pill: Option<PillActivity<'a>>, ai_ask: Option<&'a str>, closed_at: u64, enabled: impl Fn(&str) -> bool, first: Option<&'a str>) -> Option<&'a str> {
    let ok = |t: Option<&'a str>| t.filter(|t| enabled(t));
    ok(ai_ask)
        .or_else(|| ok(pill.filter(|p| p.changed > closed_at && p.priority > crate::activity::priority::MUSIC).and_then(|p| p.tab)))
        .or_else(|| ok(remembered))
        .or_else(|| ok(pill.and_then(|p| p.tab)))
        .or(first)
}

// ---- shared cross-feature state ------------------------------------------------------------

/// Meeting-mode state. Owned by the `meeting` feature (Task 3): it calls
/// [`App::set_meeting`]; everyone else reads [`App::meeting`]. To *toggle* meeting mode from
/// elsewhere (the control panel), dispatch the action `"meeting.toggle"`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeetingState {
    /// Meeting mode switched on (manually or auto-detected).
    pub active: bool,
    /// App detected as in a meeting ("Zoom", "Teams", "Meet"), if any.
    pub app_name: Option<String>,
    pub mic_in_use: bool,
    pub camera_in_use: bool,
    /// Default capture device muted by meeting mode.
    pub mic_muted: bool,
}

#[derive(Default)]
struct Shared {
    focus_hold: bool,
    meeting: MeetingState,
    accent_override: Option<Rgba>,
}

/// One-shot or repeating UI-thread timer handle.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TimerId(usize);

enum TimerFn {
    Once(Box<dyn FnOnce(&mut App)>),
    Every(Rc<dyn Fn(&mut App)>),
}

const TIMER_EXPIRY: usize = 10;
const TIMER_CLOCK: usize = 11;
const TIMER_RETRY: usize = 12;
const TIMER_DEMO: usize = 13;
/// Drops faded-out layers (and trims drawing memory after the panel closed) while idle.
const TIMER_GC: usize = 14;
const TIMER_FIRST_USER: usize = 1000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Pill,
    Hover,
    Panel,
    /// A feature's own full-width surface without the tab bar (`App::open_sheet`), e.g. the
    /// launcher. Stays open when the pointer leaves; never opened by hover.
    Sheet,
}

/// Builds a sheet's body (610 DIP wide, like a tab body).
pub type SheetFn = fn(&App) -> Node;

/// Sheet body max height: a panel's body plus the room of the tab bar it does not have.
pub const SHEET_BODY_MAX_H: f32 = PANEL_BODY_MAX_H + 52.0;

/// The text input that has the keyboard (`App::focus_input`).
struct Focus {
    id: String,
    /// Foreground window before we took it (0 = none), given back on `blur_input`.
    prev: isize,
    /// The island window really is the foreground window (activation may fail without a
    /// foreground right, e.g. when driven by `mokuctl debug`).
    window: bool,
}

/// What to redraw (`App::request_redraw`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Redraw {
    /// Whatever the pill / bubble shows.
    Pill,
    /// A tab's panel body (no-op unless that tab is visible).
    Tab(&'static str),
    All,
}

/// Which window a hit belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Region {
    Main,
    Bubble,
}

struct Press {
    hit: Option<Hit>,
    region: Region,
    x: f32,
    y: f32,
    dragging: bool,
    /// Second press of a double-click.
    double: bool,
    /// Last slider value sent while pressed.
    slide: f32,
}

/// Run mode from the command line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunMode {
    Interactive,
    /// `--state <s>`: fixed state for screenshots; no tray/pipe/services/hit window.
    Fixed,
    /// `--demo`: loops through the prototype states.
    Demo,
}

pub struct App {
    config: Arc<Config>,
    store: HashMap<TypeId, Box<dyn Any>>,
    tabs: Vec<TabSpec>,
    actions: HashMap<String, ActionFn>,
    ipc: HashMap<IpcKind, IpcFn>,
    on_config: Vec<AppFn>,
    on_start: Vec<AppFn>,
    on_exit: Vec<AppFn>,
    shut_down: bool,
    last_click: Option<(String, Instant)>,
    on_event: Vec<EventFn>,
    pub(crate) messages: Vec<(u32, MessageFn)>,
    service_specs: Vec<ServiceSpec>,
    services: Services,
    activities: Stack<Activity>,
    shared: Shared,
    pub(crate) mode: Mode,
    tab: Option<&'static str>,
    /// The tab the user last chose (tab bar, bubble, `island.open:<tab>`): see [`reopen_tab`].
    chosen_tab: Option<&'static str>,
    /// `activities.clock()` when the panel last closed.
    closed_at: u64,
    /// Wheel notches over the tab bar not turned into a tab switch yet (touchpads send fractions).
    tab_scroll: f32,
    hold: u32,
    tick: Option<(&'static str, TimerId)>,
    pub(crate) shell: Shell,
    pub(crate) scene: Island,
    main_hits: Vec<Hit>,
    main_origin: (f32, f32),
    bubble_hits: Vec<Hit>,
    bubble_id: Option<String>,
    shown_key: String,
    shown_panel: bool,
    shown_mode: Mode,
    press: Option<Press>,
    timers: HashMap<usize, TimerFn>,
    next_timer: usize,
    jobs: Receiver<Job>,
    pub(crate) dirty: bool,
    flush_posted: bool,
    outside: Vec<Box<dyn FnOnce()>>,
    pub(crate) run_mode: RunMode,
    demo_step: usize,
    pub(crate) tray_adds: u32,
    /// Debounced saves waiting for their timer (`save_later`).
    saves: HashMap<&'static str, (TimerId, SnapshotFn)>,
    /// The open sheet (`Mode::Sheet`).
    sheet: Option<(&'static str, SheetFn)>,
    /// The open sheet holds one `force_visible(true)` because `hide_apps` hides the island.
    sheet_forced: bool,
    /// Text inputs by id (`input`, `focus_input`).
    inputs: HashMap<String, TextEdit>,
    focus: Option<Focus>,
    /// Caret rect last given to the IME (hit-window DIP).
    ime_caret: Option<Rect>,
    /// How long the last flush took (ms), for `debug state`.
    flush_ms: f32,
}

/// Builds what to write for a debounced save: `(path, bytes)`, or `None` to skip. Runs on the
/// UI thread when the save is due; keep it to a serialisation of the feature's state.
pub type SnapshotFn = fn(&App) -> Option<(std::path::PathBuf, Vec<u8>)>;

impl App {
    pub(crate) fn new(reg: Registry, config: Config, shell: Shell, scene: Island, run_mode: RunMode, start_mode: Mode) -> App {
        let (tx, rx) = channel();
        let _ = UI.set(UiInner { tx: Mutex::new(tx), hwnd: shell.visual.0 as isize });
        let config = Arc::new(config);
        set_current_config(config.clone());
        i18n::set_language(config.general.language);
        shell::HOOKED.with(|h| *h.borrow_mut() = reg.messages.iter().map(|(m, _)| *m).collect());
        let mut tabs = reg.tabs;
        tabs.sort_by_key(|t| t.order);
        App {
            config,
            store: reg.states,
            tabs,
            actions: reg.actions,
            ipc: reg.ipc,
            on_config: reg.config,
            on_start: reg.start,
            on_exit: reg.exit,
            shut_down: false,
            last_click: None,
            on_event: reg.events,
            messages: reg.messages,
            service_specs: reg.services,
            services: Services::default(),
            activities: Stack::default(),
            shared: Shared::default(),
            mode: start_mode,
            tab: None,
            chosen_tab: None,
            closed_at: 0,
            tab_scroll: 0.0,
            hold: 0,
            tick: None,
            shell,
            scene,
            main_hits: vec![],
            main_origin: (0.0, 0.0),
            bubble_hits: vec![],
            bubble_id: None,
            shown_key: String::new(),
            shown_panel: false,
            shown_mode: start_mode,
            press: None,
            timers: HashMap::new(),
            next_timer: TIMER_FIRST_USER,
            jobs: rx,
            dirty: true,
            flush_posted: false,
            outside: vec![],
            run_mode,
            demo_step: 0,
            tray_adds: 0,
            saves: HashMap::new(),
            sheet: None,
            sheet_forced: false,
            inputs: HashMap::new(),
            focus: None,
            ime_caret: None,
            flush_ms: 0.0,
        }
    }

    /// Called once by `main` after `install`.
    pub(crate) fn start(&mut self) {
        for f in self.on_config.clone() {
            f(self);
        }
        if self.run_mode == RunMode::Interactive {
            self.services.sync(&self.service_specs, &self.config);
        }
        for f in self.on_start.clone() {
            f(self);
        }
        // The panel / quit hotkeys the shell could not register at startup.
        self.announce_hotkey_notices();
        // First run: the settings app shows the onboarding by itself when started without args.
        if self.run_mode == RunMode::Interactive && !self.config.onboarding.completed {
            if shell::settings_running() {
                log_info!("onboarding pending; settings already open");
            } else {
                log_info!("onboarding pending; opening the settings app");
                shell::launch_settings();
            }
        }
        self.arm_clock();
        if self.run_mode == RunMode::Demo {
            self.set_raw_timer(TIMER_DEMO, 1500);
        }
        if self.mode == Mode::Panel {
            let t = self.default_tab();
            self.open_panel(t);
        }
        self.dirty = true;
        self.flush();
    }

    // =========================================================================================
    // Public feature API
    // =========================================================================================

    /// The current config (after validation; never invalid).
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn config_arc(&self) -> Arc<Config> {
        self.config.clone()
    }

    /// A feature's state, registered with `Registry::add_state`. Panics if it was not.
    pub fn state<T: 'static>(&self) -> &T {
        self.store
            .get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref())
            .unwrap_or_else(|| panic!("state {} not registered (Registry::add_state)", std::any::type_name::<T>()))
    }

    /// Mutable feature state. Does not redraw by itself: call `request_redraw` after changes
    /// that are visible.
    pub fn state_mut<T: 'static>(&mut self) -> &mut T {
        self.store
            .get_mut(&TypeId::of::<T>())
            .and_then(|b| b.downcast_mut())
            .unwrap_or_else(|| panic!("state {} not registered (Registry::add_state)", std::any::type_name::<T>()))
    }

    /// Mark something as possibly changed. Cheap; the actual rebuild + diff happens once per
    /// message batch, and only visible things are rebuilt.
    pub fn request_redraw(&mut self, target: Redraw) {
        match target {
            Redraw::Tab(id) if self.mode != Mode::Panel || self.tab != Some(id) => {}
            _ => self.dirty = true,
        }
    }

    /// Show or update an activity (same id = replace in place; transient = restart its timer).
    pub fn set_activity(&mut self, a: Activity) {
        let expires = a.duration.map(|d| Instant::now() + d);
        let id = a.id.clone();
        let prio = a.priority;
        self.activities.set(&id, prio, expires, a);
        self.arm_expiry();
        self.dirty = true;
    }

    /// Remove an activity (no-op if absent).
    pub fn clear_activity(&mut self, id: &str) {
        if self.activities.remove(id) {
            self.dirty = true;
        }
    }

    pub fn has_activity(&self, id: &str) -> bool {
        self.activities.get(id).is_some()
    }

    /// Shorthand for a transient activity: `app.alert("volume.osd", priority::OSD, OSD_SECS, |_| pill)`.
    pub fn alert(&mut self, id: &str, priority: u8, secs: f32, pill: impl Fn(&App) -> Node + 'static) {
        self.set_activity(Activity::transient(id, priority, secs, pill));
    }

    /// Id of the activity currently shown in the pill (`None` = idle clock).
    pub fn top_activity(&self) -> Option<&str> {
        self.activities.top().map(|e| e.id.as_str())
    }

    /// Open the panel on a tab. `None` = reopen: the tab the user last chose, unless something
    /// new wants attention (see [`reopen_tab`]); an open panel then stays on its tab.
    /// Opening a tab from a feature does not count as the user's choice.
    pub fn open_panel(&mut self, tab: Option<&str>) {
        // The panel replaces a sheet (its owner gets `SheetClosed`).
        if self.mode == Mode::Sheet {
            self.close_sheet();
        }
        let was_open = self.mode == Mode::Panel;
        let want = match tab {
            Some(t) => self.tab_spec(t).map(|t| t.id).or_else(|| self.default_tab()),
            None if was_open => self.tab.filter(|t| self.tab_spec(t).is_some()).or_else(|| self.default_tab()),
            None => self.default_tab(),
        };
        let Some(want) = want else { return };
        self.mode = Mode::Panel;
        self.show_tab(want);
        if !was_open {
            self.emit(AppEvent::PanelOpened);
        }
        self.dirty = true;
    }

    /// Collapse the panel (or an open sheet) back to the pill.
    pub fn close_panel(&mut self) {
        if self.mode == Mode::Sheet {
            return self.close_sheet();
        }
        if self.mode != Mode::Panel {
            return;
        }
        self.hide_tab();
        self.mode = Mode::Pill;
        self.hold = 0;
        self.closed_at = self.activities.clock();
        self.emit(AppEvent::PanelClosed);
        self.dirty = true;
    }

    /// Open `tab` as the user's own choice: reopening the panel later comes back to it.
    pub fn choose_tab(&mut self, tab: &str) {
        if let Some(t) = self.tab_spec(tab).map(|t| t.id) {
            self.chosen_tab = Some(t);
            self.open_panel(Some(t));
        }
    }

    /// The effective animation setting: `appearance.animations`, or `Off` while Windows
    /// "Animation effects" is switched off. Features that sequence work after an animation
    /// (e.g. clear the shelf after its tiles flew into the bin) skip the wait when it is `Off`.
    pub fn animations(&self) -> moku_core::config::Animations {
        self.scene.opts().anim
    }

    pub fn is_panel_open(&self) -> bool {
        self.mode == Mode::Panel
    }

    /// The visible tab, if the panel is open.
    pub fn current_tab(&self) -> Option<&'static str> {
        if self.mode == Mode::Panel { self.tab } else { None }
    }

    pub fn is_tab_visible(&self, id: &str) -> bool {
        self.current_tab() == Some(id) && !self.shell.hidden()
    }

    /// Keep the panel open even when the pointer leaves (e.g. while an OLE drag hovers the
    /// island). Balanced calls: `hold_panel(true)` … `hold_panel(false)`.
    pub fn hold_panel(&mut self, on: bool) {
        if on {
            self.hold += 1;
        } else {
            self.hold = self.hold.saturating_sub(1);
            // A pointer resting on the island (e.g. right after a drop: OLE drags send no mouse
            // moves) keeps it open until it leaves, like a hover.
            if self.hold == 0 && self.mode == Mode::Panel && !self.shell.tracking && self.run_mode == RunMode::Interactive && self.press.is_none() && !self.shell.adopt_pointer() {
                self.close_panel();
            }
        }
    }

    /// Run `f` once after `d` (UI thread).
    pub fn after(&mut self, d: Duration, f: impl FnOnce(&mut App) + 'static) -> TimerId {
        let id = self.alloc_timer(TimerFn::Once(Box::new(f)));
        self.set_raw_timer(id.0, d.as_millis().min(0x7FFF_FFFF) as u32);
        id
    }

    /// Run `f` every `d` until cancelled (UI thread). Prefer `TabSpec::tick` for anything that
    /// only matters while a panel is visible.
    pub fn every(&mut self, d: Duration, f: impl Fn(&mut App) + 'static) -> TimerId {
        let id = self.alloc_timer(TimerFn::Every(Rc::new(f)));
        self.set_raw_timer(id.0, d.as_millis().min(0x7FFF_FFFF) as u32);
        id
    }

    pub fn cancel_timer(&mut self, id: TimerId) {
        if self.timers.remove(&id.0).is_some() {
            unsafe {
                let _ = KillTimer(Some(self.shell.visual), id.0);
            }
        }
    }

    /// Deliver an action as if its widget had been used. Also the way to call into another
    /// feature (e.g. the control panel dispatches `"meeting.toggle"`).
    pub fn dispatch_action(&mut self, id: &str, kind: ActionKind) {
        if is_debug_action(id) && !debug_enabled() {
            log_warn!("debug action {} ignored (set MOKU_DEBUG=1)", id.split(':').next().unwrap_or(id));
            return;
        }
        let ev = ActionEvent { id: id.to_string(), kind };
        match ev.prefix() {
            "tab" => match ev.kind {
                // The wheel anywhere on the tab bar walks to the previous (up) / next tab.
                ActionKind::Scroll(n) => self.scroll_tabs(n),
                ActionKind::Click => {
                    let name = ev.name().to_string();
                    self.choose_tab(&name);
                }
                _ => {}
            },
            "island" => match ev.name() {
                "open" => match ev.arg() {
                    Some(t) => self.choose_tab(t),
                    None => self.open_panel(None),
                },
                "close" => self.close_panel(),
                "settings" => shell::launch_settings(),
                _ => {}
            },
            // Prototype demo widgets (--state / --demo) do nothing.
            "demo" => {}
            p => match self.actions.get(p).copied() {
                Some(f) => f(self, &ev),
                None => log_warn!("no handler for action {}", ev.id),
            },
        }
    }

    /// Run `f` after the current `&mut App` borrow ends — for modal Win32 loops
    /// (`DoDragDrop`, `TrackPopupMenu`, dialogs). Inside `f`, use `Ui::post` to get back to the app.
    pub fn run_outside(&mut self, f: impl FnOnce() + 'static) {
        self.outside.push(Box::new(f));
    }

    /// Focus mode holds IPC notifications (set by `timer`, read by `notify`).
    pub fn focus_hold(&self) -> bool {
        self.shared.focus_hold
    }

    pub fn set_focus_hold(&mut self, on: bool) {
        if self.shared.focus_hold != on {
            self.shared.focus_hold = on;
            self.emit(AppEvent::FocusHoldChanged(on));
        }
    }

    pub fn meeting(&self) -> &MeetingState {
        &self.shared.meeting
    }

    /// Owned by the `meeting` feature. Emits `AppEvent::MeetingChanged` when it changes.
    pub fn set_meeting(&mut self, m: MeetingState) {
        if self.shared.meeting != m {
            self.shared.meeting = m;
            self.emit(AppEvent::MeetingChanged);
            self.dirty = true;
        }
    }

    /// The accent colour to draw with: the album colour while `follow_album_color` and an
    /// override is set, else `appearance.accent`.
    pub fn accent(&self) -> Rgba {
        match self.shared.accent_override {
            Some(c) if self.config.appearance.follow_album_color => c,
            _ => Rgba::from_rgb(self.config.appearance.accent_rgb()),
        }
    }

    /// The configured accent (`appearance.accent`, #0A84FF by default), never the album colour:
    /// for "on" states that are not about music (Wi-Fi, Bluetooth tiles …).
    pub fn base_accent(&self) -> Rgba {
        Rgba::from_rgb(self.config.appearance.accent_rgb())
    }

    /// Set (or clear) the album-art accent (the `media` feature). Redraws everything.
    pub fn set_accent_override(&mut self, c: Option<Rgba>) {
        if self.shared.accent_override != c {
            self.shared.accent_override = c;
            self.emit(AppEvent::AccentChanged);
            self.dirty = true;
        }
    }

    /// The island's main (visual) window: a top-level, never-activated window. Use it as the
    /// owner / notification target for system registrations (see `Registry::on_message`).
    pub fn main_hwnd(&self) -> HWND {
        self.shell.visual
    }

    /// The input window over the island shape (drop target for the shelf: `RegisterDragDrop`).
    /// `None` in `--state` / `--demo` modes.
    pub fn hit_hwnd(&self) -> Option<HWND> {
        self.shell.hit
    }

    /// DIP → physical pixel factor (monitor DPI × `island.scale`).
    pub fn scale(&self) -> f32 {
        self.scene.scale()
    }

    /// Hide the island windows (e.g. while taking a screenshot), counted: every
    /// `hide_island(true)` needs a matching `hide_island(false)`. Hiding is immediate; DWM
    /// removes the window from the next composed frame, so wait ~50 ms (`app.after`) before
    /// capturing the screen.
    pub fn hide_island(&mut self, on: bool) {
        if on {
            self.shell.suppressed += 1;
        } else {
            self.shell.suppressed = self.shell.suppressed.saturating_sub(1);
        }
        self.refresh_visibility(false);
    }

    /// Save a data file soon: `snapshot` runs ~500 ms after the *last* `save_later` with this
    /// `key` (bursts of changes cost one write), and the bytes are written on the writer thread
    /// with temp file + rename (`crate::persist`). Pending saves are flushed on exit.
    pub fn save_later(&mut self, key: &'static str, snapshot: SnapshotFn) {
        if let Some((t, _)) = self.saves.remove(key) {
            self.cancel_timer(t);
        }
        let t = self.after(crate::persist::SAVE_DELAY, move |app| {
            if let Some((_, f)) = app.saves.remove(key) {
                app.save_now(f);
            }
        });
        self.saves.insert(key, (t, snapshot));
    }

    /// Drop a pending `save_later` (data cleared: nothing old may be written back).
    pub fn cancel_save(&mut self, key: &'static str) {
        if let Some((t, _)) = self.saves.remove(key) {
            self.cancel_timer(t);
        }
    }

    fn save_now(&self, f: SnapshotFn) {
        if let Some((path, bytes)) = f(self) {
            crate::persist::write_async(path, bytes);
        }
    }

    /// Run every pending debounced save now (exit).
    fn flush_saves(&mut self) {
        let pending: Vec<_> = self.saves.drain().collect();
        for (_, (t, f)) in pending {
            self.cancel_timer(t);
            self.save_now(f);
        }
    }

    // ---- sheets and text inputs -----------------------------------------------------------

    /// Show a sheet: the island becomes a panel-sized glass surface whose whole body is
    /// `build(app)` (610 DIP wide, height measured up to [`SHEET_BODY_MAX_H`]) — no tab bar. It
    /// replaces an open panel, stays open when the pointer leaves and is never opened by hover.
    /// It closes with [`App::close_sheet`], the panel hotkey, `open_panel` from anywhere, pause or
    /// a full-screen app; the owner then gets `AppEvent::SheetClosed(id)`. Rebuilt on every flush
    /// while shown (`request_redraw(Redraw::All)` after state changes).
    pub fn open_sheet(&mut self, id: &'static str, build: SheetFn) {
        // A hotkey (or a click) opened this sheet while an app of `island.hide_apps` is in front, so
        // the island is hidden: the user asked for the sheet, so show the island for as long as it is
        // open (it hides again when the sheet closes). Pause and a capture still win.
        if self.shell.app_hidden && !self.sheet_forced {
            self.sheet_forced = true;
            self.force_visible(true);
        }
        if self.mode == Mode::Panel {
            self.close_panel();
        }
        if let Some((old, _)) = self.sheet
            && old != id
        {
            self.emit(AppEvent::SheetClosed(old));
        }
        self.sheet = Some((id, build));
        self.mode = Mode::Sheet;
        self.dirty = true;
        self.emit(AppEvent::SheetOpened(id));
    }

    /// Close the open sheet (no-op without one); emits `AppEvent::SheetClosed`.
    pub fn close_sheet(&mut self) {
        let Some((id, _)) = self.sheet.take() else { return };
        if self.mode == Mode::Sheet {
            self.mode = Mode::Pill;
        }
        self.dirty = true;
        self.emit(AppEvent::SheetClosed(id));
        if std::mem::take(&mut self.sheet_forced) {
            self.force_visible(false);
        }
    }

    /// Id of the open sheet.
    pub fn sheet(&self) -> Option<&'static str> {
        self.sheet.filter(|_| self.mode == Mode::Sheet).map(|(id, _)| id)
    }

    /// The editing state of text input `id` (empty if it was never used).
    pub fn input(&self, id: &str) -> &TextEdit {
        static EMPTY: OnceLock<TextEdit> = OnceLock::new();
        self.inputs.get(id).unwrap_or_else(|| EMPTY.get_or_init(TextEdit::new))
    }

    /// Change text input `id` from code (e.g. `set_text`, `clear`). Request a redraw after.
    pub fn input_mut(&mut self, id: &str) -> &mut TextEdit {
        self.inputs.entry(id.to_string()).or_default()
    }

    /// Give text input `id` the keyboard: the island window is activated (allowed right after
    /// the user's hotkey or a click on the island), keys and IME input go to the input, and the
    /// owner of `id` gets `ActionKind::Edited` / `Key` / `Blur`. Returns whether the window really
    /// got the foreground (it may not without a foreground right; then a click on the island
    /// activates it). Call [`App::blur_input`] when done.
    pub fn focus_input(&mut self, id: &str) -> bool {
        if let Some(f) = self.focus.as_mut() {
            if f.id != id {
                let old = std::mem::replace(&mut f.id, id.to_string());
                if let Some(e) = self.inputs.get_mut(&old) {
                    e.focused = false;
                    e.set_composition("", 0);
                }
            }
        } else if let Some(hit) = self.shell.hit {
            let (prev, window) = shell::keyboard::take(hit);
            self.focus = Some(Focus { id: id.to_string(), prev, window });
        } else {
            // --state / --demo: no input window; draw the caret anyway.
            self.focus = Some(Focus { id: id.to_string(), prev: 0, window: false });
        }
        self.input_mut(id).focused = true;
        self.ime_caret = None;
        self.dirty = true;
        self.focus.as_ref().is_some_and(|f| f.window)
    }

    /// Give the keyboard back: the window that had the foreground before `focus_input` gets it
    /// again (if the island still holds it). No-op without a focused input.
    pub fn blur_input(&mut self) {
        let Some(f) = self.focus.take() else { return };
        self.unfocus(&f.id);
        if let Some(hit) = self.shell.hit {
            shell::keyboard::release(hit, f.prev);
        }
    }

    fn unfocus(&mut self, id: &str) {
        if let Some(e) = self.inputs.get_mut(id) {
            e.focused = false;
            e.set_composition("", 0);
        }
        self.ime_caret = None;
        self.dirty = true;
    }

    /// Id of the input that has the keyboard.
    pub fn focused_input(&self) -> Option<&str> {
        self.focus.as_ref().map(|f| f.id.as_str())
    }

    /// Put text on the clipboard (owned by the island's window). False if the clipboard is busy.
    pub fn set_clipboard_text(&self, text: &str) -> bool {
        shell::keyboard::set_clipboard_text(self.shell.visual, text)
    }

    /// Let the program the user picked (process `pid`, and only that one) take the foreground:
    /// it would otherwise open behind the window that gets the foreground back from
    /// `blur_input`. Call it while this process still holds the foreground, i.e. before
    /// `blur_input` / closing the sheet.
    pub fn allow_foreground_for(&self, pid: u32) -> bool {
        shell::keyboard::allow_foreground_for(pid)
    }

    // ---- keyboard input (UI thread, from the hit window or `mokuctl debug`) ----------------

    pub(crate) fn on_input_key(&mut self, k: KeyPress) {
        let Some(id) = self.focused_input().map(str::to_string) else { return };
        if k.key == Key::Char('v') && k.ctrl && !k.alt {
            let text = self.shell.hit.and_then(shell::keyboard::clipboard_text);
            if let Some(t) = text
                && self.input_mut(&id).paste(&t)
            {
                self.dirty = true;
                self.dispatch_action(&id, ActionKind::Edited);
            }
            return;
        }
        match self.input_mut(&id).apply(k) {
            Outcome::Changed => {
                self.dirty = true;
                self.dispatch_action(&id, ActionKind::Edited);
            }
            Outcome::Moved => self.dirty = true,
            Outcome::Unhandled => self.dispatch_action(&id, ActionKind::Key(k)),
        }
    }

    pub(crate) fn on_input_text(&mut self, s: &str) {
        let Some(id) = self.focused_input().map(str::to_string) else { return };
        if self.input_mut(&id).insert(s) {
            self.dirty = true;
            self.dispatch_action(&id, ActionKind::Edited);
        }
    }

    pub(crate) fn on_input_commit(&mut self, s: &str) {
        let Some(id) = self.focused_input().map(str::to_string) else { return };
        let changed = self.input_mut(&id).commit(s);
        self.dirty = true;
        if changed {
            self.dispatch_action(&id, ActionKind::Edited);
        }
    }

    pub(crate) fn on_input_compose(&mut self, s: &str, cursor: usize) {
        let Some(id) = self.focused_input().map(str::to_string) else { return };
        self.input_mut(&id).set_composition(s, cursor);
        self.dirty = true;
    }

    /// The island window was deactivated while an input had the keyboard: the user went
    /// elsewhere, so nothing is given back to the old foreground window.
    pub(crate) fn on_input_focus_lost(&mut self) {
        let Some(f) = self.focus.take() else { return };
        self.unfocus(&f.id);
        if let Some(hit) = self.shell.hit {
            shell::keyboard::release(hit, 0);
        }
        self.dispatch_action(&f.id, ActionKind::Blur);
    }

    /// The foreground window changed: if it is not the island while an input has focus, that is
    /// a focus loss (also when activating the island had failed). The island becoming the
    /// foreground (a click on it) completes a failed activation.
    pub(crate) fn check_input_foreground(&mut self) {
        let (Some(hit), Some(f)) = (self.shell.hit, self.focus.as_mut()) else { return };
        let fg = shell::keyboard::foreground();
        if fg == hit.0 as isize {
            f.window = true;
        } else if !f.window && fg == f.prev {
            // Activation failed and the window that was in front still is: a late event (e.g.
            // the foreground we gave back when the input closed last time), not a change.
        } else {
            self.on_input_focus_lost();
        }
    }

    /// Restart a service (e.g. its settings changed). No-op if it is disabled.
    pub fn restart_service(&mut self, name: &str) {
        let specs = self.service_specs.clone();
        let cfg = self.config.clone();
        self.services.restart(&specs, name, &cfg);
    }

    // =========================================================================================
    // Internals
    // =========================================================================================

    fn emit(&mut self, e: AppEvent) {
        for f in self.on_event.clone() {
            f(self, &e);
        }
    }

    fn alloc_timer(&mut self, f: TimerFn) -> TimerId {
        let id = self.next_timer;
        self.next_timer += 1;
        self.timers.insert(id, f);
        TimerId(id)
    }

    fn set_raw_timer(&self, id: usize, ms: u32) {
        unsafe {
            SetTimer(Some(self.shell.visual), id, ms.clamp(1, 0x7FFF_FFFF), None);
        }
    }

    fn kill_raw_timer(&self, id: usize) {
        unsafe {
            let _ = KillTimer(Some(self.shell.visual), id);
        }
    }

    pub(crate) fn on_timer(&mut self, id: usize) {
        match id {
            TIMER_EXPIRY => {
                self.kill_raw_timer(id);
                if self.activities.expire(Instant::now()) {
                    self.dirty = true;
                }
                self.arm_expiry();
            }
            TIMER_CLOCK => {
                self.arm_clock();
                self.dirty = true;
            }
            TIMER_RETRY => {
                self.kill_raw_timer(id);
                self.run_jobs();
            }
            TIMER_DEMO => self.demo_next(),
            TIMER_GC => {
                self.kill_raw_timer(id);
                if let Err(e) = self.scene.gc() {
                    log_warn!("scene gc failed: {e:?}");
                }
                self.arm_gc();
            }
            _ => match self.timers.get(&id) {
                Some(TimerFn::Every(f)) => {
                    let f = f.clone();
                    f(self);
                }
                Some(TimerFn::Once(_)) => {
                    self.kill_raw_timer(id);
                    if let Some(TimerFn::Once(f)) = self.timers.remove(&id) {
                        f(self);
                    }
                }
                // Not ours: a feature's own SetTimer on main_hwnd → its on_message(WM_TIMER).
                None => self.on_window_message(windows::Win32::UI::WindowsAndMessaging::WM_TIMER, WPARAM(id), LPARAM(0)),
            },
        }
    }

    fn arm_expiry(&mut self) {
        match self.activities.next_expiry() {
            Some(t) => {
                let ms = t.saturating_duration_since(Instant::now()).as_millis() as u32 + 5;
                self.set_raw_timer(TIMER_EXPIRY, ms);
            }
            None => self.kill_raw_timer(TIMER_EXPIRY),
        }
    }

    /// Resumed from sleep: re-arm the clock (and activity expiry) and redraw — the idle clock
    /// must not show the time the machine went to sleep at.
    pub(crate) fn on_resume(&mut self) {
        log_info!("resumed from sleep");
        self.arm_clock();
        self.arm_expiry();
        self.dirty = true;
    }

    /// Wake at the next minute boundary (the idle clock).
    fn arm_clock(&mut self) {
        let now = chrono::Local::now();
        let ms_into_minute = (now.timestamp_subsec_millis() as u64) + (chrono::Timelike::second(&now) as u64) * 1000;
        let wait = 60_000 - ms_into_minute.min(59_999) + 20;
        self.set_raw_timer(TIMER_CLOCK, wait as u32);
    }

    pub(crate) fn run_jobs(&mut self) {
        while let Ok(job) = self.jobs.try_recv() {
            job(self);
        }
    }

    /// Called when the UI-thread job queue could not be drained (app busy in a modal loop).
    pub(crate) fn retry_jobs_later(hwnd: HWND) {
        unsafe {
            SetTimer(Some(hwnd), TIMER_RETRY, 30, None);
        }
    }

    fn tab_spec(&self, id: &str) -> Option<&TabSpec> {
        self.tabs.iter().find(|t| t.id == id && (t.enabled)(&self.config))
    }

    fn enabled_tabs(&self) -> Vec<&TabSpec> {
        self.tabs.iter().filter(|t| (t.enabled)(&self.config)).collect()
    }

    /// The tab a panel opened without a target shows ([`reopen_tab`]).
    fn default_tab(&self) -> Option<&'static str> {
        let pill = self.activities.top().map(|e| PillActivity { tab: e.data.tab, priority: e.priority, changed: e.changed });
        let ai = self.activities.iter().filter(|e| e.priority >= crate::activity::priority::AI_CARD && e.data.tab.is_some()).max_by_key(|e| e.changed).and_then(|e| e.data.tab);
        let first = self.enabled_tabs().first().map(|t| t.id);
        reopen_tab(self.chosen_tab, pill, ai, self.closed_at, |t| self.tab_spec(t).is_some(), first)
    }

    /// Wheel over the tab bar: one tab per notch, no wrap-around. Counts as the user's choice.
    fn scroll_tabs(&mut self, notches: f32) {
        if self.mode != Mode::Panel {
            return;
        }
        self.tab_scroll += notches;
        if self.tab_scroll.abs() < 1.0 {
            return;
        }
        let step: isize = if self.tab_scroll > 0.0 { -1 } else { 1 };
        self.tab_scroll = 0.0;
        let ids: Vec<&'static str> = self.enabled_tabs().iter().map(|t| t.id).collect();
        let Some(i) = self.tab.and_then(|t| ids.iter().position(|x| *x == t)) else { return };
        let j = (i as isize + step).clamp(0, ids.len() as isize - 1) as usize;
        if j != i {
            self.choose_tab(ids[j]);
        }
    }

    fn show_tab(&mut self, id: &'static str) {
        if self.tab == Some(id) && self.tick.is_some_and(|(t, _)| t == id) {
            return;
        }
        if self.tab != Some(id) {
            self.hide_tab();
        }
        self.tab = Some(id);
        self.emit(AppEvent::TabShown(id));
        if let Some((every, f)) = self.tab_spec(id).and_then(|t| t.tick) {
            f(self);
            let tid = self.every(every, move |app| f(app));
            self.tick = Some((id, tid));
        }
        self.dirty = true;
    }

    fn hide_tab(&mut self) {
        if let Some((_, tid)) = self.tick.take() {
            self.cancel_timer(tid);
        }
        if let Some(t) = self.tab.take() {
            self.emit(AppEvent::TabHidden(t));
        }
    }

    // ---- composing what is shown ---------------------------------------------------------

    fn idle_pill(&self, hover: bool) -> Node {
        let now = chrono::Local::now();
        let mut items = vec![ui::logo(24.0), ui::text(i18n::clock(&now)).semibold()];
        if hover {
            items.push(ui::text(i18n::date_short(&now)).secondary());
            items.push(ui::icon(icons::CHEVRON_DOWN).font_size(12.0).color(ui::color::TEXT2));
        }
        ui::row(items).h(if hover { 40.0 } else { 36.0 }).pad4(14.0, 0.0, 14.0, 0.0).gap(if hover { 10.0 } else { 8.0 })
    }

    fn panel_content(&self, tab: &'static str) -> Content {
        let enabled = self.enabled_tabs();
        let labels: Vec<(String, &TabSpec)> = enabled.iter().map(|t| (i18n::pair(t.label).to_string(), *t)).collect();
        let items: Vec<TabItem> = labels.iter().map(|(l, t)| TabItem { id: t.id, glyph: t.icon, label: l, dimmed: t.id == "phone" }).collect();
        let bar = ui::tab_bar(&items, Some(tab)).w(PANEL_W - 30.0);
        let body = if demo::active() && tab == "media" {
            demo::panel(self)
        } else {
            match self.tab_spec(tab) {
                Some(t) => (t.build)(self),
                None => ui::placeholder(icons::INFO, i18n::pair(i18n::COMING_SOON), ""),
            }
        };
        let g = &self.scene.gfx;
        g.clear_measure_cache();
        let body_w = PANEL_W - 30.0;
        let mut blocks = vec![];
        let mut bar_laid = Laid::default();
        layout::place(&bar, Rect::new(15.0, 15.0, body_w, 38.0), g, &mut bar_laid, 1.0);
        blocks.push(Block { rect: Rect::new(15.0, 15.0, body_w, 38.0), laid: bar_laid });
        let bh = self.body_blocks(&body, 67.0, PANEL_BODY_MAX_H, &mut blocks);
        Content { key: format!("panel:{tab}"), size: (PANEL_W, 67.0 + bh + 15.0), blocks, panel: true }
    }

    /// Lay out a panel / sheet body at `top` (610 wide, height measured, at most `max_h`) into
    /// `blocks`; returns the body height. A top-level column becomes one block per child
    /// (staggered in when the panel opens).
    fn body_blocks(&self, body: &Node, top: f32, max_h: f32, blocks: &mut Vec<Block>) -> f32 {
        let g = &self.scene.gfx;
        let body_w = PANEL_W - 30.0;
        let (_, bh) = layout::measure(body, g, body_w);
        let bh = bh.clamp(40.0, max_h);
        let body_rect = Rect::new(15.0, top, body_w, bh);
        let first = blocks.len();
        let split = matches!(body.kind, Kind::Col(_)) && body.bg.is_none() && body.border.is_none() && body.action.is_none();
        if split {
            if let Kind::Col(children) = &body.kind {
                for (ch, r) in children.iter().zip(layout::child_rects(body, body_rect, g)) {
                    let mut l = Laid::default();
                    layout::place(ch, r, g, &mut l, body.opacity);
                    blocks.push(Block { rect: r, laid: l });
                }
            }
        } else {
            let mut l = Laid::default();
            layout::place(body, body_rect, g, &mut l, 1.0);
            blocks.push(Block { rect: body_rect, laid: l });
        }
        // Clip blocks that would run past the body max height.
        let bottom = top + bh;
        for b in &mut blocks[first..] {
            if b.rect.y + b.rect.h > bottom + 0.5 && b.rect.y >= top {
                b.rect.h = (bottom - b.rect.y).max(1.0);
            }
        }
        bh
    }

    fn sheet_content(&self, id: &'static str, build: SheetFn) -> Content {
        let body = build(self);
        self.scene.gfx.clear_measure_cache();
        let mut blocks = vec![];
        let bh = self.body_blocks(&body, 15.0, SHEET_BODY_MAX_H, &mut blocks);
        Content { key: format!("sheet:{id}"), size: (PANEL_W, bh + 30.0), blocks, panel: true }
    }

    fn pill_content(&self, key: String, node: &Node, min_h: f32) -> Content {
        let g = &self.scene.gfx;
        g.clear_measure_cache();
        let ((w, h), laid) = layout::layout(node, g, 0.0, 0.0, PANEL_W);
        let size = ((w / 2.0).ceil() * 2.0, h.max(min_h));
        let laid = if h < min_h {
            let mut l = Laid::default();
            layout::place(node, Rect::new(0.0, 0.0, size.0, size.1), g, &mut l, 1.0);
            l
        } else {
            laid
        };
        Content { key, size, blocks: vec![Block { rect: Rect::new(0.0, 0.0, size.0, size.1), laid }], panel: false }
    }

    fn bubble_content(&self, a: &Activity) -> Content {
        let node = match &a.bubble {
            Some(b) => b(self),
            None => ui::stack([ui::icon(icons::INFO).font_size(14.0).color(ui::color::TEXT2)]).size(36.0, 36.0),
        };
        let g = &self.scene.gfx;
        let mut l = Laid::default();
        layout::place(&node.size(36.0, 36.0), Rect::new(0.0, 0.0, 36.0, 36.0), g, &mut l, 1.0);
        Content { key: a.id.clone(), size: (36.0, 36.0), blocks: vec![Block { rect: Rect::new(0.0, 0.0, 36.0, 36.0), laid: l }], panel: false }
    }

    /// Rebuild what is visible and hand it to the scene (only changed blocks get redrawn).
    pub(crate) fn flush(&mut self) {
        self.flush_posted = false;
        if !self.dirty {
            return;
        }
        self.dirty = false;
        // Hidden (paused, full screen, capture): nothing is rasterised or presented. Showing
        // the island again marks it dirty (`refresh_visibility`), so the next flush catches up.
        if self.shell.hidden() {
            return;
        }
        let started = Instant::now();
        if self.activities.expire(Instant::now()) {
            self.arm_expiry();
        }
        if self.mode == Mode::Sheet && self.sheet.is_none() {
            self.mode = Mode::Pill;
        }
        let (main, shape) = if let (Mode::Sheet, Some((id, build))) = (self.mode, self.sheet) {
            let c = self.sheet_content(id, build);
            let s = c.size;
            (c, s)
        } else if self.mode == Mode::Panel {
            let tab = match self.tab {
                Some(t) if self.tab_spec(t).is_some() => t,
                _ => match self.default_tab() {
                    Some(t) => {
                        self.show_tab(t);
                        t
                    }
                    None => {
                        self.mode = Mode::Pill;
                        return self.flush_again();
                    }
                },
            };
            let c = self.panel_content(tab);
            let s = c.size;
            (c, s)
        } else {
            let hover = self.mode == Mode::Hover;
            match self.activities.top() {
                Some(e) => {
                    let node = (e.data.pill)(self);
                    let c = self.pill_content(e.id.clone(), &node, 36.0);
                    let s = if hover { (c.size.0 + 12.0, c.size.1 + 4.0) } else { c.size };
                    (c, s)
                }
                None => {
                    let node = self.idle_pill(hover);
                    let c = self.pill_content(if hover { "idle.hover".into() } else { "idle".into() }, &node, 36.0);
                    let s = c.size;
                    (c, s)
                }
            }
        };
        let bubble = if !matches!(self.mode, Mode::Panel | Mode::Sheet) {
            self.activities.split().map(|e| (e.id.clone(), self.bubble_content(&e.data)))
        } else {
            None
        };
        let m = match (self.shown_mode, self.mode) {
            (_, Mode::Panel | Mode::Sheet) => motion::OPEN,
            (Mode::Panel | Mode::Sheet, _) => motion::CLOSE,
            (_, Mode::Hover) | (Mode::Hover, _) if self.shown_key.starts_with("idle") || self.shown_key == main.key => motion::HOVER,
            _ if main.key != self.shown_key && self.activities.top().is_some_and(|e| e.priority >= crate::activity::priority::NOTIFICATION) => motion::ALERT,
            _ => motion::PILL,
        };
        self.main_origin = if main.panel { (0.0, 0.0) } else { (((shape.0 - main.size.0) / 2.0).round(), ((shape.1 - main.size.1) / 2.0).round()) };
        self.main_hits = main.blocks.iter().flat_map(|b| b.laid.hits.clone()).collect();
        self.bubble_hits = bubble.as_ref().map(|(_, b)| b.blocks.iter().flat_map(|b| b.laid.hits.clone()).collect()).unwrap_or_default();
        self.bubble_id = bubble.as_ref().map(|(id, _)| id.clone());
        self.shown_key = main.key.clone();
        self.shown_panel = main.panel;
        self.shown_mode = self.mode;
        // The IME's candidate window follows the caret of the focused input.
        let caret = main.blocks.iter().flat_map(|b| &b.laid.live).find_map(|l| match l {
            Live::Caret { r, .. } => Some(r.translate(self.main_origin.0, self.main_origin.1)),
            _ => None,
        });
        if let Err(e) = self.scene.present(main, shape, bubble.map(|(_, b)| b), m) {
            log_error!("present failed: {e:?}");
        }
        self.shell.place_hit(&self.scene.hit_shape());
        if self.focus.is_some()
            && caret.is_some()
            && caret != self.ime_caret
            && let (Some(hit), Some(r)) = (self.shell.hit, caret)
        {
            shell::keyboard::place_ime(hit, r, self.scene.scale(), self.shell.hit_reach());
            self.ime_caret = caret;
        }
        self.arm_gc();
        self.flush_ms = started.elapsed().as_secs_f32() * 1000.0;
    }

    /// Retired layers are dropped ~0.6 s after they faded out, even if nothing else redraws.
    fn arm_gc(&self) {
        if self.scene.needs_gc() {
            self.set_raw_timer(TIMER_GC, 600);
        }
    }

    fn flush_again(&mut self) {
        self.dirty = true;
        self.flush();
    }

    // ---- input from the hit window (DIP, relative to the hit window) -----------------------

    fn region_at(&self, x: f32, y: f32) -> (Region, f32, f32) {
        let hs = self.scene.hit_shape();
        let s = self.scene.scale();
        if let Some((bx, by, bw, _)) = hs.bubble {
            let lx = (bx - hs.main.0) as f32 / s;
            if x >= lx {
                return (Region::Bubble, x - lx, y - (by - hs.main.1) as f32 / s);
            }
            let _ = bw;
        }
        (Region::Main, x - self.main_origin.0, y - self.main_origin.1)
    }

    fn hit_at(&self, x: f32, y: f32) -> (Region, Option<Hit>, f32, f32) {
        let (region, cx, cy) = self.region_at(x, y);
        let hits = if region == Region::Main { &self.main_hits } else { &self.bubble_hits };
        let h = hits.iter().rev().find(|h| h.rect.contains(cx, cy)).cloned();
        (region, h, cx, cy)
    }

    pub(crate) fn on_mouse_enter(&mut self) {
        if self.mode == Mode::Pill {
            self.mode = Mode::Hover;
            self.dirty = true;
        }
    }

    pub(crate) fn on_hover_dwell(&mut self) {
        if !matches!(self.mode, Mode::Panel | Mode::Sheet) && self.config.island.open_on == moku_core::config::OpenOn::Hover {
            self.open_panel(None);
        }
    }

    pub(crate) fn on_mouse_leave(&mut self) {
        if self.press.is_some() || self.hold > 0 {
            return;
        }
        match self.mode {
            Mode::Hover => {
                self.mode = Mode::Pill;
                self.dirty = true;
            }
            Mode::Panel => self.close_panel(),
            // A sheet closes by its owner, a key or focus loss, not by the pointer.
            Mode::Pill | Mode::Sheet => {}
        }
    }

    pub(crate) fn on_mouse_move(&mut self, x: f32, y: f32) {
        let Some(p) = self.press.as_mut() else { return };
        let Some(hit) = p.hit.clone() else { return };
        match hit.kind {
            HitKind::Slider => {
                let (_, cx, _) = self.region_at(x, y);
                let v = ((cx - hit.rect.x) / hit.rect.w.max(1.0)).clamp(0.0, 1.0);
                if let Some(p) = self.press.as_mut() {
                    p.slide = v;
                }
                self.dispatch_action(&hit.action, ActionKind::Slide { value: v, done: false });
            }
            HitKind::Drag if !p.dragging && ((x - p.x).abs() > 4.0 || (y - p.y).abs() > 4.0) => {
                // The feature starts an OLE drag (its own modal loop): the button-up never
                // reaches us, so the press ends here.
                self.press = None;
                self.dispatch_action(&hit.action, ActionKind::DragStart);
            }
            _ => {}
        }
    }

    /// Button down; `double` = the second press of a double-click (`WM_LBUTTONDBLCLK`).
    pub(crate) fn on_button_down(&mut self, x: f32, y: f32, double: bool) {
        let (region, hit, cx, _) = self.hit_at(x, y);
        if let Some(h) = &hit
            && h.kind == HitKind::Slider
        {
            let v = ((cx - h.rect.x) / h.rect.w.max(1.0)).clamp(0.0, 1.0);
            self.dispatch_action(&h.action, ActionKind::Slide { value: v, done: false });
        }
        let slide = match &hit {
            Some(h) if h.kind == HitKind::Slider => ((cx - h.rect.x) / h.rect.w.max(1.0)).clamp(0.0, 1.0),
            _ => 0.0,
        };
        self.press = Some(Press { hit, region, x, y, dragging: false, double, slide });
    }

    /// Button up. A double-click delivers `Click` for each press, plus `DoubleClick` after the
    /// second one if both clicks hit the same action.
    pub(crate) fn on_button_up(&mut self, x: f32, y: f32) {
        let Some(p) = self.press.take() else { return };
        let (region, hit, cx, _) = self.hit_at(x, y);
        let mut clicked = None;
        match (&p.hit, &hit) {
            (Some(a), _) if a.kind == HitKind::Slider => {
                let v = ((cx - a.rect.x) / a.rect.w.max(1.0)).clamp(0.0, 1.0);
                self.dispatch_action(&a.action, ActionKind::Slide { value: v, done: true });
            }
            (Some(a), Some(b)) if a.action == b.action && !p.dragging => {
                self.dispatch_action(&a.action, ActionKind::Click);
                clicked = Some(a.action.clone());
            }
            (None, None) if region == p.region && !p.double => self.click_background(region),
            _ => {}
        }
        if p.double
            && let Some(a) = &clicked
            && self.last_click.as_ref().is_some_and(|(prev, t)| prev == a && t.elapsed() < Duration::from_millis(800))
        {
            self.dispatch_action(a, ActionKind::DoubleClick);
        }
        self.last_click = clicked.map(|a| (a, Instant::now()));
        if !self.shell.tracking && self.hold == 0 {
            self.on_mouse_leave();
        }
    }

    /// Capture lost without a button-up (OLE drag, another app took capture): end the press
    /// (sliders get `done`) and re-check whether the pointer is still over the island.
    pub(crate) fn cancel_press(&mut self, pointer_outside: bool) {
        let Some(p) = self.press.take() else { return };
        if let Some(h) = &p.hit
            && h.kind == HitKind::Slider
        {
            self.dispatch_action(&h.action, ActionKind::Slide { value: p.slide, done: true });
        }
        if pointer_outside {
            self.shell.tracking = false;
            if self.hold == 0 {
                self.on_mouse_leave();
            }
        }
    }

    pub(crate) fn on_wheel(&mut self, x: f32, y: f32, notches: f32) {
        if let (_, Some(h), _, _) = self.hit_at(x, y) {
            self.dispatch_action(&h.action, ActionKind::Scroll(notches));
        }
    }

    fn click_background(&mut self, region: Region) {
        if matches!(self.mode, Mode::Panel | Mode::Sheet) {
            return;
        }
        match region {
            Region::Bubble => {
                // The bubble's own tab (the user picked it: remembered); without one, the
                // chosen / first tab — never the tab of the activity in the main pill, which is
                // not what was clicked.
                let own = self.bubble_id.as_deref().and_then(|id| self.activities.get(id)).and_then(|e| e.data.tab);
                match own {
                    Some(t) => self.choose_tab(t),
                    None => {
                        let t = self.chosen_tab.filter(|t| self.tab_spec(t).is_some()).or_else(|| self.enabled_tabs().first().map(|t| t.id));
                        self.open_panel(t);
                    }
                }
            }
            // The pill itself: reopen (the remembered tab unless something new wants attention).
            Region::Main => self.open_panel(None),
        }
    }

    /// Hotkey / second launch: open ↔ close. Ignored while the island is hidden (paused or
    /// a full-screen app is in front).
    pub(crate) fn toggle_panel(&mut self) {
        if matches!(self.mode, Mode::Panel | Mode::Sheet) {
            self.close_panel()
        } else if !self.shell.hidden() {
            self.open_panel(None)
        }
    }

    // ---- config ---------------------------------------------------------------------------

    pub(crate) fn scene_opts(c: &Config) -> SceneOpts {
        SceneOpts {
            blur: std::env::args().any(|a| a == "--blur"),
            material: c.appearance.material,
            notch: c.island.style == moku_core::config::IslandStyle::Notch,
            // Moku's own setting on top of Windows "Animation effects" (SPI_GETCLIENTAREAANIMATION).
            anim: reduce::effective(c.appearance.animations, shell::client_area_animation()),
        }
    }

    /// Re-read config.toml (file watcher or `mokuctl`). Invalid values were already replaced by
    /// defaults with warnings in the log.
    pub(crate) fn reload_config(&mut self) {
        let (c, warnings) = match Config::try_load_from(&moku_core::paths::config_file()) {
            Ok(r) => r,
            Err(w) => {
                // Half-saved or locked file: keep the last good config instead of resetting.
                log_warn!("{w} — keeping the current settings");
                return;
            }
        };
        for w in &warnings {
            log_warn!("{w}");
        }
        if c != *self.config {
            log_info!("config reloaded");
            self.apply_config(c);
        }
    }

    /// Change the config from a feature (e.g. a panel toggle that is also a setting) and save it.
    ///
    /// Read-modify-write: the file is re-read first, so edits the settings app (or a hand edit)
    /// saved since the island last loaded it are kept; if the file is unreadable right now the
    /// in-memory config is used instead. The result is written atomically and applied at once
    /// (`on_config` handlers, services, redraw), exactly like a hot reload. Never call
    /// `Config::save` / `save_to` from a feature: that would write the island's possibly stale
    /// copy over the user's newer edits.
    pub fn update_config(&mut self, change: impl FnOnce(&mut Config)) -> std::io::Result<()> {
        let c = Config::update_file(&moku_core::paths::config_file(), &self.config, change)?;
        if c != *self.config {
            self.apply_config(c);
        }
        Ok(())
    }

    fn apply_config(&mut self, c: Config) {
        let old = std::mem::replace(&mut self.config, Arc::new(c));
        set_current_config(self.config.clone());
        i18n::set_language(self.config.general.language);
        for f in self.on_config.clone() {
            f(self);
        }
        if self.run_mode == RunMode::Interactive {
            let specs = self.service_specs.clone();
            let cfg = self.config.clone();
            self.services.sync(&specs, &cfg);
            self.shell.register_hotkeys(&self.config);
            self.announce_hotkey_notices();
        }
        let opts = App::scene_opts(&self.config);
        let (a, b) = (&old.island, &self.config.island);
        if opts != self.scene.opts() || a.scale != b.scale || (&a.monitor, a.anchor, a.offset_x) != (&b.monitor, b.anchor, b.offset_x) {
            self.reposition();
        }
        self.refresh_visibility(true);
        if let Some(t) = self.tab
            && self.tab_spec(t).is_none()
        {
            self.hide_tab();
        }
        self.emit(AppEvent::ConfigChanged);
        self.dirty = true;
    }

    /// Display / DPI / config scale changed: re-anchor windows and rebuild surfaces if needed.
    pub(crate) fn reposition(&mut self) {
        let cfg = self.config.clone();
        self.shell.refresh_monitors(&cfg);
        let s = self.shell.target.dpi * cfg.island.scale;
        let opts = App::scene_opts(&self.config);
        if (s - self.scene.scale()).abs() > 0.001 || opts != self.scene.opts() {
            if let Err(e) = self.scene.rebuild(s, opts) {
                log_error!("scene rebuild failed: {e:?}");
            }
        }
        self.shell.place_visual(s);
        self.shell.place_hit(&self.scene.hit_shape());
        self.emit(AppEvent::DisplayChanged);
        self.dirty = true;
    }

    /// Appbar notification: a full-screen app appeared / went away on this monitor. The
    /// foreground-based flag is re-queried too, so the two never disagree for long (the appbar
    /// says "gone" while the shell still reports a full-screen D3D app, or the reverse).
    pub(crate) fn on_fullscreen_hint(&mut self, on: bool) {
        self.shell.fs_appbar = on;
        self.shell.fullscreen = shell::fullscreen_busy();
        self.refresh_visibility(true);
    }

    /// Apply the shell's visibility rules after one of their inputs changed. Hidden → nothing is
    /// presented (see `flush`); shown again → redraw what changed meanwhile. `close_if_hidden`
    /// collapses the panel (pause, full screen) — not for a short capture hide.
    pub(crate) fn refresh_visibility(&mut self, close_if_hidden: bool) {
        let was_hidden = self.shell.hidden();
        let cfg = self.config.clone();
        self.shell.update_visibility(&cfg);
        if self.shell.hidden() {
            if close_if_hidden {
                self.close_panel();
            }
        } else if was_hidden {
            self.dirty = true;
        }
        if was_hidden != self.shell.hidden() {
            self.emit(AppEvent::Visibility(!self.shell.hidden()));
        }
        // The hotkeys follow pause and full screen only (not hide_apps): tell the features that own
        // one when that changed, even though the island's own visibility may not have.
        let released = self.shell.hotkeys_released(&cfg);
        if released != std::mem::replace(&mut self.shell.hk_released, released) {
            self.emit(AppEvent::HotkeysChanged);
        }
    }

    /// Height (DIP) of `node` laid out `w` DIP wide with the real text measurer: for a panel body that
    /// must fit the room it has (Hari ini asks, so nothing is clipped at the bottom). Cheap enough
    /// for a handful of calls per redraw of one tab; not for lists.
    pub fn measure_height(&self, node: &Node, w: f32) -> f32 {
        layout::measure(node, &self.scene.gfx, w).1
    }

    /// The island is not on screen (paused, a full-screen app, a capture in progress). Nothing is
    /// drawn meanwhile; features with their own timers (lyric line, progress) should not run them.
    pub fn is_hidden(&self) -> bool {
        self.shell.hidden()
    }

    /// Are the global hotkeys of the features let go of? True while paused from the tray or while a
    /// full-screen app hides the island; **false** while only an app of `island.hide_apps` is in
    /// front (the user did not ask for the launcher, OCR or Tanya AI to stop answering there).
    /// Features register their hotkey while this is false and re-check on `AppEvent::HotkeysChanged`.
    pub fn hotkeys_released(&self) -> bool {
        self.shell.hotkeys_released(&self.config)
    }

    /// Raise the pill for the shell's hotkeys (panel, quit) that could not be registered.
    pub(crate) fn announce_hotkey_notices(&mut self) {
        for (slot, combo, outcome) in std::mem::take(&mut self.shell.hotkey_notices) {
            crate::hotkeys::announce(self, slot, &combo, outcome);
        }
    }

    /// Explorer restarted (`TaskbarCreated`): re-add the tray icon and the full-screen appbar,
    /// and forget the old appbar's full-screen state (the new Explorer re-sends it).
    pub(crate) fn on_taskbar_created(&mut self) {
        if let Some(t) = self.shell.tray.as_mut()
            && t.add()
        {
            self.tray_adds += 1;
        }
        self.shell.register_appbar();
        self.shell.fs_appbar = false;
        self.shell.fullscreen = shell::fullscreen_busy();
        self.refresh_visibility(true);
    }

    pub(crate) fn set_paused(&mut self, paused: bool) {
        self.shell.paused = paused;
        self.refresh_visibility(true);
        self.emit(AppEvent::Paused(paused));
    }

    /// Paused from the tray (the island is hidden until resumed).
    pub fn is_paused(&self) -> bool {
        self.shell.paused
    }

    /// Keep the island visible over a full-screen app (counted, like `hide_island`): a pending
    /// AI permission card the user is waiting for. Does not override pause or capture hiding.
    pub fn force_visible(&mut self, on: bool) {
        if on {
            self.shell.forced += 1;
        } else {
            self.shell.forced = self.shell.forced.saturating_sub(1);
        }
        self.refresh_visibility(true);
    }

    // ---- placement: where the island is, and how politely (feature `placement`) ------------

    /// The foreground app changed (`EVENT_SYSTEM_FOREGROUND`): `AppEvent::ForegroundChanged`.
    pub(crate) fn on_foreground_changed(&mut self) {
        self.emit(AppEvent::ForegroundChanged);
    }

    /// The foreground app is one the user asked the island to stay away from
    /// (`island.hide_apps`): hides like a full-screen app (unless `force_visible`).
    pub fn set_app_hidden(&mut self, on: bool) {
        if self.shell.app_hidden != on {
            self.shell.app_hidden = on;
            self.refresh_visibility(true);
        }
    }

    /// Step the island out of the way ([`Away`]): the visuals fade / shift on the compositor
    /// (as far as "Kurangi" / "Mati" allow, see `scene::reduce`) and clicks go through it. A hover
    /// in progress ends. Setting the same value again does nothing.
    pub fn set_away(&mut self, away: Away) {
        if away == self.scene.away() {
            return;
        }
        if !away.is_home() {
            // The input window goes away: the pointer cannot be over the island any more.
            if self.mode == Mode::Hover {
                self.mode = Mode::Pill;
                self.dirty = true;
            }
            self.shell.tracking = false;
            self.shell.hover_armed = false;
        }
        self.shell.set_passthrough(!away.is_home());
        let anim = self.animations();
        if let Err(e) = self.scene.set_away(away, anim) {
            log_warn!("set_away failed: {e:?}");
        }
        self.dirty = true;
    }

    pub fn away(&self) -> Away {
        self.scene.away()
    }

    /// The island's resting rectangle (shape + bubble) on the virtual desktop, physical px.
    pub fn home_rect(&self) -> Option<shell::monitors::IRect> {
        self.shell.home_rect()
    }

    /// The monitor the island is on, physical px.
    pub fn monitor_rect(&self) -> shell::monitors::IRect {
        self.shell.target.mon
    }

    /// The connected monitors (listed again on every display change).
    pub fn monitors(&self) -> &[shell::monitors::Mon] {
        &self.shell.monitors
    }

    /// "Follow the cursor": live on the monitor `name` (`None`: choose again from the config).
    pub fn set_followed_monitor(&mut self, name: Option<&str>) {
        let name = name.map(str::to_string);
        if self.shell.follow != name {
            self.shell.follow = name;
            self.reposition();
        }
    }

    /// How many things became *news* so far (activities added, transients set again): a feature
    /// that reveals the island when something arrives compares it with the last value it saw.
    pub fn activity_news(&self) -> u64 {
        self.activities.clock()
    }

    /// The pill's activity has something to press: a button or another action below its root. Builds
    /// the pill once, so ask only when it matters (hide until the edge does).
    pub fn pill_is_actionable(&self) -> bool {
        self.activities.top().is_some_and(|e| (e.data.pill)(self).has_inner_action())
    }

    /// Priority of the activity in the pill (0 = idle).
    pub fn top_priority(&self) -> u8 {
        self.activities.top().map_or(0, |e| e.priority)
    }

    pub(crate) fn on_window_message(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) {
        let handlers: Vec<MessageFn> = self.messages.iter().filter(|(m, _)| *m == msg).map(|(_, f)| *f).collect();
        for f in handlers {
            f(self, wp, lp);
        }
    }

    // ---- --demo / --state -----------------------------------------------------------------

    fn demo_next(&mut self) {
        // idle → hover → music → panel → music → idle (prototype loop).
        self.demo_step = (self.demo_step + 1) % 5;
        match self.demo_step {
            0 => {
                self.close_panel();
                self.clear_activity("demo.music");
                self.mode = Mode::Pill;
            }
            1 => self.mode = Mode::Hover,
            2 => {
                self.mode = Mode::Pill;
                demo::show_music(self);
            }
            3 => self.open_panel(Some("media")),
            _ => self.close_panel(),
        }
        self.dirty = true;
    }

    // ---- IPC ------------------------------------------------------------------------------

    pub(crate) fn handle_request(&mut self, req: Request, reply: Reply) {
        match req {
            Request::Ping => reply.send(Response::Pong { version: moku_core::VERSION.into() }),
            Request::Debug(op) => self.handle_debug(op, reply),
            Request::ClearData => {
                log_info!("data cleared by the settings app");
                self.emit(AppEvent::DataCleared);
                // Whatever a feature still had scheduled would write old data back.
                for (_, (t, _)) in std::mem::take(&mut self.saves) {
                    self.cancel_timer(t);
                }
                self.dirty = true;
                // Answer once the writes already queued have landed, so the wipe comes after them.
                let spawned = std::thread::Builder::new().name("clear-data".into()).spawn(move || {
                    crate::persist::drain(Duration::from_secs(2));
                    reply.ok();
                });
                if let Err(e) = spawned {
                    log_warn!("clear-data: {e}");
                }
            }
            other => {
                let Some(kind) = IpcKind::of(&other) else { return };
                if !self.config.ipc.enabled {
                    return reply.send(Response::Error { message: "IPC is disabled in settings".into() });
                }
                match self.ipc.get(&kind).copied() {
                    Some(f) => f(self, other, reply),
                    None => reply.send(Response::Error { message: format!("{kind:?} is not available") }),
                }
            }
        }
    }

    fn handle_debug(&mut self, op: DebugOp, reply: Reply) {
        if !debug_op_allowed(&op, debug_enabled()) {
            log_warn!("pipe: debug op rejected (MOKU_DEBUG is not set)");
            return reply.error(DEBUG_OP_DISABLED);
        }
        match op {
            DebugOp::OpenTab { id } => match self.tab_spec(&id).map(|t| t.id) {
                Some(t) => {
                    // Like a click on the tab bar.
                    self.choose_tab(t);
                    reply.ok();
                }
                None => reply.send(Response::Error { message: format!("no enabled tab {id:?}") }),
            },
            DebugOp::Close => {
                self.close_panel();
                if self.mode == Mode::Hover {
                    self.mode = Mode::Pill;
                }
                self.dirty = true;
                reply.ok();
            }
            DebugOp::State => {
                let state = self.debug_state();
                reply.send(Response::State { state });
            }
            DebugOp::Click { action } => {
                self.dispatch_action(&action, ActionKind::Click);
                reply.ok();
            }
            DebugOp::ScreenshotReady => {
                self.flush_again();
                let wait = if self.scene.animating() { 900 } else { 150 };
                self.after(Duration::from_millis(wait), move |app| {
                    let more = app.scene.animating();
                    if more {
                        app.after(Duration::from_millis(500), move |_| reply.ok());
                    } else {
                        reply.ok();
                    }
                });
            }
            DebugOp::Activity { id, priority, label, secs } => {
                let l = label.clone();
                let pill = move |_: &App| ui::pill([ui::icon(icons::INFO).color(ui::color::TEAL), ui::text(l.clone()).semibold()]);
                let a = match secs {
                    Some(s) => Activity::transient(id, priority, s, pill),
                    None => Activity::persistent(id, priority, pill),
                };
                let a = a.bubble(|_| ui::stack([ui::icon(icons::INFO).font_size(14.0).color(ui::color::TEAL)]));
                self.set_activity(a);
                reply.ok();
            }
            DebugOp::ClearActivity { id } => {
                self.clear_activity(&id);
                reply.ok();
            }
            // Drive the focused input like the keyboard would (never other apps' windows).
            DebugOp::Type { text, compose } => {
                if self.focus.is_none() {
                    return reply.error("no text input has focus");
                }
                if compose {
                    self.on_input_compose(&text, text.len());
                } else {
                    self.on_input_compose("", 0);
                    self.on_input_text(&text);
                }
                reply.ok();
            }
            DebugOp::Key { key } => match (KeyPress::parse(&key), self.focus.is_some()) {
                (_, false) => reply.error("no text input has focus"),
                (None, _) => reply.error(format!("unknown key {key:?}")),
                (Some(k), true) => {
                    self.on_input_key(k);
                    reply.ok();
                }
            },
            // A fake pointer for the placement logic (never the real cursor).
            DebugOp::Cursor { x, y, dt, down } => match crate::features::placement::debug_cursor(self, x.zip(y), dt, down) {
                Ok(state) => reply.send(Response::State { state }),
                Err(m) => reply.error(m),
            },
        }
    }

    fn debug_state(&self) -> serde_json::Value {
        let hs = self.scene.hit_shape();
        serde_json::json!({
            "version": moku_core::VERSION,
            "mode": format!("{:?}", self.mode).to_lowercase(),
            "tab": self.current_tab(),
            "chosen_tab": self.chosen_tab,
            "reopen_tab": self.default_tab(),
            "animations": format!("{:?}", self.scene.opts().anim).to_lowercase(),
            "labels": self.main_hits.iter().filter_map(|h| h.name.as_ref().map(|n| serde_json::json!({ "action": h.action, "name": n }))).collect::<Vec<_>>(),
            "tabs": self.enabled_tabs().iter().map(|t| t.id).collect::<Vec<_>>(),
            "shown": self.shown_key,
            "top": self.activities.top().map(|e| e.id.clone()),
            "split": self.activities.split().map(|e| e.id.clone()),
            "activities": self.activities.iter().map(|e| serde_json::json!({
                "id": e.id, "priority": e.priority, "transient": e.expires.is_some(), "tab": e.data.tab,
            })).collect::<Vec<_>>(),
            "hit": { "main": [hs.main.0, hs.main.1, hs.main.2, hs.main.3], "bubble": hs.bubble.map(|b| [b.0, b.1, b.2, b.3]), "reach": self.shell.hit_reach() },
            "actions": self.main_hits.iter().map(|h| h.action.clone()).collect::<Vec<_>>(),
            "scale": self.scene.scale(),
            "animating": self.scene.animating(),
            "paused": self.shell.paused,
            "hidden": self.shell.hidden(),
            "focus_hold": self.shared.focus_hold,
            "meeting": self.shared.meeting.active,
            "services": self.services.names(),
            "tray_adds": self.tray_adds,
            "appbar_adds": self.shell.appbar_adds,
            "fullscreen": self.shell.fullscreen || self.shell.fs_appbar,
            "forced_visible": self.shell.forced,
            "app_hidden": self.shell.app_hidden,
            "away": self.scene.away().name(),
            "passthrough": self.shell.passthrough,
            "monitor": { "name": self.shell.target.name, "rect": [self.shell.target.mon.l, self.shell.target.mon.t, self.shell.target.mon.r, self.shell.target.mon.b], "dpi": self.shell.target.dpi },
            "home": self.shell.home_rect().map(|r| [r.l, r.t, r.r, r.b]),
            "placement": crate::features::placement::debug_json(self),
            "config": moku_core::paths::config_file(),
            "sheet": self.sheet(),
            "input": self.focus.as_ref().map(|f| {
                let e = self.input(&f.id);
                // The typed text only with MOKU_DEBUG (it may be personal).
                serde_json::json!({
                    "id": f.id, "window_focused": f.window, "caret": e.caret(), "selection": e.selection(),
                    "text": debug_enabled().then(|| e.text().to_string()), "composition": debug_enabled().then(|| e.composition().map(str::to_string)),
                })
            }),
            "flush_ms": self.flush_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{PillActivity, debug_op_allowed, is_debug_action, reopen_tab};
    use crate::activity::{Stack, priority};
    use moku_core::ipc::DebugOp;
    use std::time::{Duration, Instant};

    const ALL: &[&str] = &["media", "notify", "shelf", "timer", "clipboard"];

    fn enabled(t: &str) -> bool {
        ALL.contains(&t)
    }

    /// The panel state the app keeps, driven like the app drives it.
    struct Sim {
        acts: Stack<Option<&'static str>>,
        chosen: Option<&'static str>,
        closed_at: u64,
    }

    impl Sim {
        fn new() -> Sim {
            Sim { acts: Stack::default(), chosen: None, closed_at: 0 }
        }
        fn persistent(&mut self, id: &str, prio: u8, tab: &'static str) {
            self.acts.set(id, prio, None, Some(tab));
        }
        fn transient(&mut self, id: &str, prio: u8, tab: &'static str) {
            self.acts.set(id, prio, Some(Instant::now() + Duration::from_secs(5)), Some(tab));
        }
        fn close(&mut self) {
            self.closed_at = self.acts.clock();
        }
        fn reopen(&self) -> Option<&'static str> {
            let pill = self.acts.top().map(|e| PillActivity { tab: e.data, priority: e.priority, changed: e.changed });
            let ai = self.acts.iter().filter(|e| e.priority >= priority::AI_CARD).max_by_key(|e| e.changed).and_then(|e| e.data);
            reopen_tab(self.chosen, pill, ai, self.closed_at, enabled, Some("media"))
        }
    }

    /// The bug: music plays, the user switches to Shelf, the panel closes, hover again → Shelf.
    #[test]
    fn reopen_returns_to_the_chosen_tab_while_music_keeps_playing() {
        let mut s = Sim::new();
        s.persistent("media.music", priority::MUSIC, "media");
        assert_eq!(s.reopen(), Some("media"), "nothing chosen yet: the music tab");
        s.chosen = Some("shelf");
        s.close();
        assert_eq!(s.reopen(), Some("shelf"));
        // Track change / play-pause: the music activity is updated in place.
        s.persistent("media.music", priority::MUSIC, "media");
        assert_eq!(s.reopen(), Some("shelf"), "a track change does not steal the tab");
        // Music stopping and starting again after the close is still only music.
        s.acts.remove("media.music");
        s.persistent("media.music", priority::MUSIC, "media");
        assert_eq!(s.reopen(), Some("shelf"));
    }

    #[test]
    fn reopen_prefers_news_that_arrived_after_the_panel_closed() {
        let mut s = Sim::new();
        s.persistent("media.music", priority::MUSIC, "media");
        s.chosen = Some("shelf");
        // A notification that arrived while the panel was open is not news afterwards.
        s.transient("notify.pill", priority::NOTIFICATION, "notify");
        s.close();
        assert_eq!(s.reopen(), Some("shelf"));
        // Another notification after the close is.
        s.transient("notify.pill", priority::NOTIFICATION, "notify");
        assert_eq!(s.reopen(), Some("notify"));
        // Once seen (opened, closed again) it no longer wins.
        s.close();
        assert_eq!(s.reopen(), Some("shelf"));
        s.acts.remove("notify.pill");
        // A timer that just finished wins too.
        s.persistent("timer.done", priority::DUE, "timer");
        assert_eq!(s.reopen(), Some("timer"));
        s.close();
        // A running timer ticking (in-place updates) does not.
        s.acts.remove("timer.done");
        s.persistent("timer.run", priority::TIMER, "timer");
        s.close();
        s.persistent("timer.run", priority::TIMER, "timer");
        assert_eq!(s.reopen(), Some("shelf"));
    }

    #[test]
    fn reopen_ai_ask_always_wins() {
        let mut s = Sim::new();
        s.chosen = Some("clipboard");
        s.persistent("ai.card", priority::AI_CARD, "notify");
        s.close();
        // Even after it was seen and the panel closed again.
        assert_eq!(s.reopen(), Some("notify"));
        // Even when other news arrived after it.
        s.persistent("timer.done", priority::DUE, "timer");
        s.transient("notify.pill", priority::NOTIFICATION, "notify");
        assert_eq!(s.reopen(), Some("notify"));
        // Without the ask, the newest news wins again.
        s.acts.remove("ai.card");
        s.acts.remove("notify.pill");
        assert_eq!(s.reopen(), Some("timer"));
    }

    #[test]
    fn reopen_skips_disabled_tabs_and_falls_back() {
        let mut s = Sim::new();
        // Chosen tab switched off in the settings: the pill's tab, else the first tab.
        s.chosen = Some("health");
        assert_eq!(s.reopen(), Some("media"));
        s.persistent("timer.run", priority::TIMER, "timer");
        s.close();
        assert_eq!(s.reopen(), Some("timer"));
        // A news activity whose tab is disabled does not win.
        s.chosen = Some("shelf");
        s.persistent("phone.call", priority::DUE, "phone");
        assert_eq!(s.reopen(), Some("shelf"));
        // An activity without a tab never wins.
        let pill = PillActivity { tab: None, priority: priority::OSD, changed: 99 };
        assert_eq!(reopen_tab(Some("shelf"), Some(pill), None, 0, enabled, Some("media")), Some("shelf"));
        assert_eq!(reopen_tab(None, None, None, 0, enabled, Some("media")), Some("media"));
    }

    /// Without MOKU_DEBUG only the read-only `state` op is served: no click (e.g. `ai.approve`),
    /// no fake activity, no tab switching from another process.
    #[test]
    fn debug_ops_gated() {
        let driving = [
            DebugOp::Click { action: "ai.approve".into() },
            DebugOp::Click { action: "clipboard.clear".into() },
            DebugOp::OpenTab { id: "media".into() },
            DebugOp::Close,
            DebugOp::ScreenshotReady,
            DebugOp::Activity { id: "x".into(), priority: 100, label: "fake".into(), secs: None },
            DebugOp::ClearActivity { id: "ai.card".into() },
            DebugOp::Type { text: "x".into(), compose: false },
            DebugOp::Key { key: "enter".into() },
            DebugOp::Cursor { x: Some(960), y: Some(0), dt: None, down: false },
        ];
        for op in &driving {
            assert!(!debug_op_allowed(op, false), "{op:?}");
            assert!(debug_op_allowed(op, true), "{op:?}");
        }
        assert!(debug_op_allowed(&DebugOp::State, false));
    }

    #[test]
    fn debug_actions_are_recognised() {
        for id in [
            "media.sample",
            "media.sample_off",
            "system.sample:caps-on",
            "privacy.sample:both",
            "meeting.sample:zoom",
            "clipboard.debug_text:hello",
            "clipboard.debug_image:2",
            "shelf.debug_drop:C:\\a.txt|C:\\b.png",
            "shelf.debug_enter",
            "reminders.sample_water",
            "prayer.sample_next",
            "today.sample_rain",
            "volume.test",
        ] {
            assert!(is_debug_action(id), "{id}");
        }
        for id in ["media.pill", "clipboard.copy:17", "tab.media", "island.open:media", "timer.focus", "clipboard.copy:x.sample", "volume.osd", "shelf.open:debug_x"] {
            assert!(!is_debug_action(id), "{id}");
        }
    }
}
