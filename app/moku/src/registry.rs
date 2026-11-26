//! What a feature registers at startup. Every feature has one `pub fn register(r: &mut Registry)`
//! called once from `features::register_all`, before the island exists.
//!
//! Handlers are plain `fn` pointers that receive `&mut App`; keep feature state in the typed
//! store (`r.add_state(MyState::default())`, then `app.state::<MyState>()`).

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::time::Duration;

use moku_core::Config;
use moku_core::ipc::Request;
use windows::Win32::Foundation::{LPARAM, WPARAM};

use crate::app::App;
use crate::ipc_server::Reply;
use crate::services::ServiceCtx;
use crate::ui::Node;

/// A tab of the panel.
#[derive(Clone)]
pub struct TabSpec {
    /// Stable id: used in `tab.<id>` actions and `mokuctl debug open-tab <id>`.
    pub id: &'static str,
    /// Position in the tab bar (see [`order`]); lower = further left.
    pub order: u16,
    /// Glyph from `ui::icons`.
    pub icon: &'static str,
    /// Label `(id, en)` shown on the active tab.
    pub label: (&'static str, &'static str),
    /// Tab shown only when this returns true (e.g. `|c| c.monitor.enabled`).
    pub enabled: fn(&Config) -> bool,
    /// Builds the panel body (610 DIP wide; height is measured, max 360). A top-level `col`
    /// gets its children staggered in when the panel opens.
    pub build: fn(&App) -> Node,
    /// Called every `interval` while this tab is visible, and once right when it is shown.
    /// Stops when the panel closes or another tab is selected. Use for 1 Hz timelines and
    /// meters so nothing ticks while the panel is closed.
    pub tick: Option<(Duration, fn(&mut App))>,
}

/// Tab bar order from the plan: Musik · Hari ini · Kontrol · Shelf · Timer · Clipboard ·
/// Tangkap · Monitor · Notifikasi · HP.
pub mod order {
    pub const MEDIA: u16 = 10;
    pub const TODAY: u16 = 20;
    pub const CONTROL: u16 = 30;
    pub const SHELF: u16 = 40;
    pub const TIMER: u16 = 50;
    pub const CLIPBOARD: u16 = 60;
    pub const CAPTURE: u16 = 70;
    pub const MONITOR: u16 = 80;
    pub const NOTIFY: u16 = 90;
    pub const PHONE: u16 = 100;
}

/// How an action widget was used.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ActionKind {
    Click,
    DoubleClick,
    /// Wheel notches; positive = up / away from the user.
    Scroll(f32),
    /// Slider value 0–1 while pressed / dragged; `done` on release.
    Slide { value: f32, done: bool },
    /// Press-and-move on a `.draggable()` node (start an OLE drag here, via `app.run_outside`).
    DragStart,
    /// The text of the focused input (`ui::text_input`, action id = input id) changed: typing,
    /// deleting, paste, IME commit. Read it with `app.input(id).text()`.
    Edited,
    /// A key the focused input does not use itself (Enter, Esc, ↑/↓, Tab, PgUp/PgDn, Alt+1 …).
    Key(crate::ui::text_edit::KeyPress),
    /// The focused input lost the keyboard because the user went elsewhere (another window was
    /// activated, a click outside the island). The core already gave focus up.
    Blur,
}

/// An action delivered to a feature: `id` is the full action id (`"clipboard.copy:17"`).
#[derive(Clone, Debug)]
pub struct ActionEvent {
    pub id: String,
    pub kind: ActionKind,
}

impl ActionEvent {
    /// Part before the first `.` — the owning feature.
    pub fn prefix(&self) -> &str {
        action_prefix(&self.id)
    }
    /// Part after the prefix and before `:` — e.g. `"copy"` in `"clipboard.copy:17"`.
    pub fn name(&self) -> &str {
        let rest = self.id.split_once('.').map(|(_, r)| r).unwrap_or("");
        rest.split_once(':').map(|(n, _)| n).unwrap_or(rest)
    }
    /// Part after `:` — e.g. `"17"` in `"clipboard.copy:17"`.
    pub fn arg(&self) -> Option<&str> {
        self.id.split_once(':').map(|(_, a)| a)
    }
}

pub fn action_prefix(id: &str) -> &str {
    id.split(['.', ':']).next().unwrap_or(id)
}

/// Which IPC requests a handler takes. `Ping` and `Debug` are handled by the core.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IpcKind {
    Notify,
    Ask,
    Todo,
    Session,
    Update,
}

impl IpcKind {
    pub fn of(r: &Request) -> Option<IpcKind> {
        match r {
            Request::Notify { .. } => Some(IpcKind::Notify),
            Request::Ask { .. } => Some(IpcKind::Ask),
            Request::Todo { .. } => Some(IpcKind::Todo),
            Request::Session { .. } => Some(IpcKind::Session),
            Request::Update { .. } => Some(IpcKind::Update),
            Request::Ping | Request::ClearData | Request::Debug(_) => None,
        }
    }
}

/// Cross-feature events (see `App::set_focus_hold`, `App::set_meeting`, `App::set_accent_override`).
#[derive(Clone, Debug, PartialEq)]
pub enum AppEvent {
    /// After every config (re)load, after the `on_config` handlers ran.
    ConfigChanged,
    /// Focus mode started/stopped holding notifications.
    FocusHoldChanged(bool),
    /// `App::meeting()` changed.
    MeetingChanged,
    /// The effective accent colour changed.
    AccentChanged,
    PanelOpened,
    PanelClosed,
    /// A sheet (`App::open_sheet`) was closed — by its owner, the panel hotkey, a feature opening
    /// the panel, pause or a full-screen app. Carries the sheet id.
    SheetClosed(&'static str),
    /// A sheet (`App::open_sheet`) was opened. Carries the sheet id.
    SheetOpened(&'static str),
    TabShown(&'static str),
    TabHidden(&'static str),
    /// Display / DPI / work area changed (island already re-centred, possibly on another monitor).
    DisplayChanged,
    /// The foreground window changed (another app came to the front).
    ForegroundChanged,
    /// Paused / resumed from the tray.
    Paused(bool),
    /// The island was hidden (`false`: pause, full-screen app, capture) or shown again (`true`).
    /// Nothing is drawn while hidden; stop / re-arm drawing timers on this.
    Visibility(bool),
    /// The global hotkeys were let go (paused, a full-screen app) or may be taken again. Not for
    /// `island.hide_apps`: see `App::hotkeys_released`. Features that own a hotkey re-check on this
    /// and on `Visibility`.
    HotkeysChanged,
    /// "Hapus semua data" in the settings app (IPC `ClearData`): drop every piece of user data
    /// held in memory (history, lists, thumbnails) and cancel pending saves
    /// (`app.cancel_save(key)`); the settings app deletes the files right after the island
    /// answered. Do not write the old data back.
    DataCleared,
    /// The pill went from the idle clock to an activity (`App::set_activity` with nothing else in the
    /// list). "Hide when idle" shows the island at once on this.
    ActivityStarted,
}

/// A background thread owned by the core: started when `enabled(config)` becomes true,
/// stopped (flag + WM_QUIT to its thread) when it becomes false or the app exits.
#[derive(Clone)]
pub struct ServiceSpec {
    pub name: &'static str,
    pub enabled: fn(&Config) -> bool,
    pub run: fn(ServiceCtx),
}

pub type ActionFn = fn(&mut App, &ActionEvent);
pub type IpcFn = fn(&mut App, Request, Reply);
pub type AppFn = fn(&mut App);
pub type EventFn = fn(&mut App, &AppEvent);
pub type MessageFn = fn(&mut App, WPARAM, LPARAM);

#[derive(Default)]
pub struct Registry {
    pub(crate) tabs: Vec<TabSpec>,
    pub(crate) actions: HashMap<String, ActionFn>,
    pub(crate) ipc: HashMap<IpcKind, IpcFn>,
    pub(crate) config: Vec<AppFn>,
    pub(crate) start: Vec<AppFn>,
    pub(crate) exit: Vec<AppFn>,
    pub(crate) events: Vec<EventFn>,
    pub(crate) messages: Vec<(u32, MessageFn)>,
    pub(crate) services: Vec<ServiceSpec>,
    pub(crate) states: HashMap<TypeId, Box<dyn Any>>,
}

impl Registry {
    /// Register the feature's state (one value per type). Read it with `app.state::<T>()`.
    pub fn add_state<T: 'static>(&mut self, value: T) {
        self.states.insert(TypeId::of::<T>(), Box::new(value));
    }

    pub fn add_tab(&mut self, tab: TabSpec) {
        assert!(!self.tabs.iter().any(|t| t.id == tab.id), "tab {} registered twice", tab.id);
        self.tabs.push(tab);
    }

    /// Handle every action whose id starts with `prefix.` (or `prefix:`), e.g. `"media"`.
    pub fn on_action(&mut self, prefix: &str, f: ActionFn) {
        let old = self.actions.insert(prefix.to_string(), f);
        assert!(old.is_none(), "action prefix {prefix} registered twice");
    }

    /// Called once after startup and after every config reload (`app.config()` is current).
    pub fn on_config(&mut self, f: AppFn) {
        self.config.push(f);
    }

    /// Called once on the UI thread when the island is up (after the first `on_config`).
    pub fn on_start(&mut self, f: AppFn) {
        self.start.push(f);
    }

    /// Called once when the island quits (tray → Keluar, quit hotkey, Windows logoff /
    /// shutdown via `WM_ENDSESSION`), before services are stopped. Save state here; keep it
    /// short (services then get ≤ 1 s in total to finish).
    pub fn on_exit(&mut self, f: AppFn) {
        self.exit.push(f);
    }

    pub fn on_event(&mut self, f: EventFn) {
        self.events.push(f);
    }

    /// Receive a window message sent to the island's main (visual) window — a real top-level
    /// window, so it gets broadcasts like `WM_SETTINGCHANGE`, `WM_POWERBROADCAST`,
    /// `WM_DEVICECHANGE`, and can be the target of `RegisterPowerSettingNotification`,
    /// `AddClipboardFormatListener`, raw input (`app.main_hwnd()`).
    pub fn on_message(&mut self, msg: u32, f: MessageFn) {
        self.messages.push((msg, f));
    }

    pub fn add_service(&mut self, s: ServiceSpec) {
        self.services.push(s);
    }

    /// Handle one IPC request kind. The handler runs on the UI thread and must answer through
    /// `reply` (now, or later — e.g. `Ask` keeps the `Reply` in its state until clicked).
    pub fn add_ipc_handler(&mut self, kind: IpcKind, f: IpcFn) {
        let old = self.ipc.insert(kind, f);
        assert!(old.is_none(), "IPC handler {kind:?} registered twice");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_id_parts() {
        let e = ActionEvent { id: "clipboard.copy:17".into(), kind: ActionKind::Click };
        assert_eq!(e.prefix(), "clipboard");
        assert_eq!(e.name(), "copy");
        assert_eq!(e.arg(), Some("17"));
        let e = ActionEvent { id: "tab.media".into(), kind: ActionKind::Click };
        assert_eq!((e.prefix(), e.name(), e.arg()), ("tab", "media", None));
        assert_eq!(action_prefix("media"), "media");
    }
}
