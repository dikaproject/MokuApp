//! Win32 windows and system integration (grown from the prototype's `main.rs`).
//!
//! Windows:
//! - the *visual* window (big enough for the panel + shadow) hosts the composition tree. It is
//!   WS_EX_LAYERED | WS_EX_TRANSPARENT, so it never receives mouse input. It is a real
//!   top-level window, so it also receives broadcasts (forwarded to features, see
//!   `Registry::on_message`).
//! - the *hit* window is an invisible top-most window moved/reshaped (SetWindowRgn) to the
//!   island's target shape (plus the split bubble) on each change, and up to the top edge of the
//!   monitor over the width of each shape (`hit.rs`: the pointer stops *at* the edge, not at the
//!   floating pill). It is the only thing that takes mouse input; everything else on the top of
//!   the screen stays clickable.
//! - the *tray* window is a hidden window owning the notification-area icon and its menu.

pub mod hit;
pub mod keyboard;
pub mod monitors;
pub mod tray;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, Result, w};

use crate::app::{App, Mode, RunMode, WM_APP_APPBAR, WM_APP_FLUSH, WM_APP_OPEN, WM_APP_RUN, WM_APP_TRAY, with_app, with_app_or_defer};
use crate::scene::HitShape;
use moku_core::Config;

const WM_MOUSEHOVER: u32 = 0x02A1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const SPI_SETWORKAREA: usize = 0x002F;
pub const HOTKEY_QUIT: i32 = 1;
pub const HOTKEY_TOGGLE: i32 = 2;

pub const CLASS_VISUAL: PCWSTR = w!("MokuIsland");
pub const CLASS_HIT: PCWSTR = w!("MokuIslandHit");
pub const CLASS_TRAY: PCWSTR = w!("MokuTray");

/// Primary monitor rectangle (physical px) and its DPI scale: the fallback when no monitor can
/// be enumerated.
fn primary_monitor() -> (RECT, f32) {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        let (mut dx, mut dy) = (96, 96);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        (info.rcMonitor, dx as f32 / 96.0)
    }
}

/// Where the island lives: its monitor and where along the top edge (`island.monitor`,
/// `island.anchor`, `island.offset_x`). Resolved again on every display change, config change and
/// (follow the cursor) monitor switch.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    /// Device name (`\\.\DISPLAY1`; empty when the monitors could not be listed).
    pub name: String,
    /// The monitor, physical px.
    pub mon: monitors::IRect,
    /// Effective DPI / 96.
    pub dpi: f32,
    pub anchor: moku_core::config::Anchor,
    pub offset: i32,
}

impl Target {
    /// Choose the monitor from the config. `follow` = the monitor "follow the cursor" settled on.
    /// `mons` = the monitors to choose from (see [`monitors::enumerate`]).
    pub fn resolve(cfg: &Config, mons: &[monitors::Mon], follow: Option<&str>) -> Target {
        let i = &cfg.island;
        match monitors::choose(&i.monitor_choice(), mons, monitors::cursor_pos(), follow).and_then(|k| mons.get(k)) {
            Some(m) => Target { name: m.name.clone(), mon: m.rect, dpi: m.dpi, anchor: i.anchor, offset: i.offset_x },
            None => {
                let (r, dpi) = primary_monitor();
                Target { name: String::new(), mon: monitors::IRect::from_rect(r), dpi, anchor: i.anchor, offset: i.offset_x }
            }
        }
    }

    /// The visual window's rectangle at scale `s` (monitor DPI × `island.scale`).
    pub fn visual_rect(&self, s: f32) -> monitors::IRect {
        monitors::visual_rect(self.mon, s, monitors::shift_px(self.mon.w(), s, self.anchor, self.offset))
    }
}

/// Should the island hide until the thing in front is gone? A full-screen app does (if
/// `island.hide_fullscreen`), and an app the user listed (`island.hide_apps`) always does.
fn hides_unless_forced(hide_fullscreen: bool, fullscreen: bool, app_hidden: bool) -> bool {
    (hide_fullscreen && fullscreen) || app_hidden
}

/// Is a settings window open? (Its single-instance mutex, or — for runs with `MOKU_DATA_DIR`,
/// which skip the mutex — its window title.)
pub fn settings_running() -> bool {
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    use windows::core::HSTRING;
    unsafe {
        if let Ok(h) = OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Local\\MokuSettings.SingleInstance")) {
            let _ = CloseHandle(h);
            return true;
        }
        FindWindowW(PCWSTR::null(), &HSTRING::from("Moku — Pengaturan")).is_ok_and(|h| !h.is_invalid())
    }
}

/// Start `moku-settings.exe` from the same folder as `moku.exe` (tray → Pengaturan, the
/// `island.settings` action, and the first-run onboarding all use this).
pub fn launch_settings() {
    let Ok(exe) = std::env::current_exe() else { return };
    let path = exe.with_file_name("moku-settings.exe");
    match std::process::Command::new(&path).spawn() {
        Ok(_) => log_info!("launched {}", path.display()),
        Err(e) => log_error!("cannot launch {}: {e}", path.display()),
    }
}

fn create_window(class: PCWSTR, ex: WINDOW_EX_STYLE, r: RECT, dblclk: bool) -> Result<HWND> {
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            style: if dblclk { CS_DBLCLKS } else { Default::default() },
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        CreateWindowExW(ex, class, w!("Moku"), WS_POPUP, r.left, r.top, r.right - r.left, r.bottom - r.top, None, None, Some(inst.into()), None)
    }
}

pub struct Shell {
    pub visual: HWND,
    pub hit: Option<HWND>,
    pub tray_hwnd: Option<HWND>,
    pub tray: Option<tray::Tray>,
    pub tracking: bool,
    /// Open-on-hover timer armed for the current pointer visit (see `pointer_is_passive`).
    pub hover_armed: bool,
    /// Cursor position when the hit window last moved / reshaped.
    pub moved_under: Option<(i32, i32)>,
    pub paused: bool,
    pub fullscreen: bool,
    /// Features hiding the island briefly (screen capture), counted.
    pub suppressed: u32,
    /// The shell says a full-screen app is on this monitor (appbar ABN_FULLSCREENAPP).
    pub fs_appbar: bool,
    /// Features keeping the island visible over a full-screen app (a pending AI permission
    /// card: the user is waiting on it), counted. Never overrides `paused` / `suppressed`.
    pub forced: u32,
    /// Times the appbar was (re)registered (start + every `TaskbarCreated`), for `debug state`.
    pub appbar_adds: u32,
    /// Hotkeys of the shell (panel, quit) that could not be registered, not announced yet.
    pub hotkey_notices: Vec<(moku_core::hotkey::Slot, String, crate::hotkeys::Outcome)>,
    /// Were the hotkeys let go at the last visibility check ([`Shell::hotkeys_released`])?
    pub hk_released: bool,
    /// The foreground app is one of `island.hide_apps` (`App::set_app_hidden`): hides like a
    /// full-screen app, unless a feature forces the island visible.
    pub app_hidden: bool,
    /// Clicks go through the island: it stepped aside or waits for the top edge
    /// (`App::set_away`), so the input window is hidden.
    pub passthrough: bool,
    /// The monitor the island is on and where along its top edge.
    pub target: Target,
    /// The connected monitors, listed again on every display change ([`Shell::refresh_monitors`]).
    pub monitors: Vec<monitors::Mon>,
    /// Monitors invented by `placement.debug_monitors` (tests only; never set in normal runs).
    pub fake_monitors: Option<Vec<monitors::Mon>>,
    /// "Follow the cursor": the monitor the island follows now (a device name).
    pub follow: Option<String>,
    visible: bool,
    visual_shown: bool,
    hit_shown: bool,
    hook: Option<HWINEVENTHOOK>,
    /// The island's resting rectangle on the virtual desktop (shape + bubble, x / y / w / h).
    hit_rect: (i32, i32, i32, i32),
    /// How far the input window reaches above `hit_rect` (physical px): up to the monitor's top
    /// edge, see [`hit`]. The window's client origin is this far above the island's shapes.
    hit_reach: i32,
}

impl Shell {
    pub fn create(run_mode: RunMode, scale: f32, target: Target, mons: Vec<monitors::Mon>) -> Result<Shell> {
        unsafe {
            let vr = target.visual_rect(scale).to_rect();
            // Layered + transparent: never hit-tested, so clicks fall through the whole window.
            let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP | WS_EX_LAYERED | WS_EX_TRANSPARENT;
            let visual = create_window(CLASS_VISUAL, ex, vr, false)?;
            SetLayeredWindowAttributes(visual, COLORREF(0), 255, LWA_ALPHA)?;
            if std::env::args().any(|a| a == "--blur") {
                let on = windows::core::BOOL(1);
                let _ = DwmSetWindowAttribute(visual, DWMWA_USE_HOSTBACKDROPBRUSH, &on as *const _ as _, 4);
            }
            let corner = DWMWCP_DONOTROUND;
            let _ = DwmSetWindowAttribute(visual, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
            let none: u32 = DWMWA_COLOR_NONE;
            let _ = DwmSetWindowAttribute(visual, DWMWA_BORDER_COLOR, &none as *const _ as _, 4);

            let (hit, tray_hwnd) = if run_mode == RunMode::Interactive {
                let hex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP;
                let h = create_window(CLASS_HIT, hex, RECT { left: 0, top: 0, right: 1, bottom: 1 }, true)?;
                let t = create_window(CLASS_TRAY, WS_EX_TOOLWINDOW, RECT { left: 0, top: 0, right: 1, bottom: 1 }, false)?;
                (Some(h), Some(t))
            } else {
                (None, None)
            };
            ROLES.with(|r| r.set((visual.0 as isize, hit.map_or(0, |h| h.0 as isize), tray_hwnd.map_or(0, |h| h.0 as isize))));
            Ok(Shell {
                visual,
                hit,
                tray_hwnd,
                tray: None,
                tracking: false,
                hover_armed: false,
                moved_under: None,
                paused: false,
                fullscreen: false,
                suppressed: 0,
                fs_appbar: false,
                forced: 0,
                hotkey_notices: vec![],
                hk_released: false,
                appbar_adds: 0,
                app_hidden: false,
                passthrough: false,
                follow: None,
                target,
                monitors: mons,
                fake_monitors: None,
                visible: false,
                visual_shown: false,
                hit_shown: false,
                hook: None,
                hit_rect: (0, 0, 0, 0),
                hit_reach: 0,
            })
        }
    }

    /// Show the windows, add the tray icon, hotkeys and the fullscreen hook (interactive mode).
    pub fn start(&mut self, cfg: &Config, run_mode: RunMode) {
        unsafe {
            let _ = ShowWindow(self.visual, SW_SHOWNOACTIVATE);
            if let Some(h) = self.hit {
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            }
            self.visible = true;
            self.visual_shown = true;
            self.hit_shown = self.hit.is_some();
            if run_mode != RunMode::Fixed {
                self.hook = Some(SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(on_foreground), 0, 0, WINEVENT_OUTOFCONTEXT));
                self.fullscreen = fullscreen_busy();
                self.update_visibility(cfg);
            }
        }
        if let Some(t) = self.tray_hwnd {
            self.register_appbar();
            self.tray = Some(tray::Tray::new(t));
            if let Some(tr) = self.tray.as_mut() {
                tr.add();
            }
        }
        if run_mode == RunMode::Interactive {
            self.register_hotkeys(cfg);
        } else {
            // The quit hotkey works in every mode (fails harmlessly if another instance holds it).
            let _ = crate::hotkeys::register(self.visual, HOTKEY_QUIT, moku_core::hotkey::Slot::Quit, &cfg.general.hotkey_quit);
        }
    }

    /// A (zero-size) appbar only to be told when a full-screen app takes this monitor (browser
    /// F11 / video full screen don't change the foreground window). Called at start and again
    /// on `TaskbarCreated`: a restarted Explorer has forgotten the old registration.
    pub fn register_appbar(&mut self) {
        let Some(t) = self.tray_hwnd else { return };
        let mut abd = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, hWnd: t, uCallbackMessage: WM_APP_APPBAR, ..Default::default() };
        unsafe {
            // Harmless when Explorer no longer knows us; makes ABM_NEW succeed when it still does.
            SHAppBarMessage(ABM_REMOVE, &mut abd);
            if SHAppBarMessage(ABM_NEW, &mut abd) != 0 {
                self.appbar_adds += 1;
            } else {
                log_warn!("appbar registration failed");
            }
        }
    }

    pub fn teardown(&mut self) {
        if let Some(t) = self.tray.as_mut() {
            t.remove();
        }
        if let Some(t) = self.tray_hwnd {
            let mut abd = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, hWnd: t, ..Default::default() };
            unsafe {
                SHAppBarMessage(ABM_REMOVE, &mut abd);
            }
        }
        if let Some(h) = self.hook.take() {
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
    }

    /// (Re)register the global hotkeys from the config. Failures (key taken) are logged.
    /// Failures are kept in [`Shell::hotkey_notices`] for the app to announce with the same pill as
    /// every other hotkey ([`crate::hotkeys::announce`]).
    pub fn register_hotkeys(&mut self, cfg: &Config) {
        use moku_core::hotkey::Slot;
        // Quit first: when the two are the same key, the panel one is the one skipped.
        for (id, slot) in [(HOTKEY_QUIT, Slot::Quit), (HOTKEY_TOGGLE, Slot::Panel)] {
            crate::hotkeys::unregister(self.visual, id, slot);
            let combo = slot.combo(cfg).to_string();
            let outcome = crate::hotkeys::register(self.visual, id, slot, &combo);
            if outcome != crate::hotkeys::Outcome::Registered {
                self.hotkey_notices.push((slot, combo, outcome));
            }
        }
    }

    /// The global hotkeys are let go while the island is paused from the tray or a full-screen app
    /// hides it (`island.hide_fullscreen`, unless a feature forces it visible): they belong to the
    /// program in front then. **Not** for `island.hide_apps` (the user listed that app to keep the
    /// island away, not the launcher, OCR or Tanya AI: a hotkey that opens a sheet then shows the
    /// island for it, see `App::open_sheet`) and not for a short capture.
    pub fn hotkeys_released(&self, cfg: &Config) -> bool {
        released(self.paused, cfg.island.hide_fullscreen, self.fullscreen || self.fs_appbar, self.forced)
    }

    pub fn hidden(&self) -> bool {
        !self.visible
    }

    /// Hidden while paused from the tray, while a full-screen app / presentation is in front (if
    /// `island.hide_fullscreen`), or while an app of `island.hide_apps` is.
    pub fn update_visibility(&mut self, cfg: &Config) {
        let hides = hides_unless_forced(cfg.island.hide_fullscreen, self.fullscreen || self.fs_appbar, self.app_hidden);
        self.visible = want_visible(self.paused, self.suppressed, hides, self.forced);
        self.sync_windows();
    }

    /// Show or hide the two windows to match `visible` / `passthrough`: the visual window follows
    /// `visible`; the input window also goes while clicks pass through the island.
    fn sync_windows(&mut self) {
        let show = |h: HWND, on: bool| unsafe {
            let _ = ShowWindow(h, if on { SW_SHOWNOACTIVATE } else { SW_HIDE });
        };
        if self.visible != self.visual_shown {
            show(self.visual, self.visible);
            self.visual_shown = self.visible;
        }
        let hit_on = self.visible && !self.passthrough;
        if let Some(h) = self.hit
            && hit_on != self.hit_shown
        {
            show(h, hit_on);
            self.hit_shown = hit_on;
        }
    }

    /// Let clicks through the island (its input window is hidden) or take them again.
    pub fn set_passthrough(&mut self, on: bool) {
        if self.passthrough != on {
            self.passthrough = on;
            self.sync_windows();
        }
    }

    /// List the monitors again and choose the island's (`Target::resolve`). Returns whether the
    /// target changed.
    pub fn refresh_monitors(&mut self, cfg: &Config) -> bool {
        self.monitors = self.fake_monitors.clone().unwrap_or_else(monitors::enumerate);
        let t = Target::resolve(cfg, &self.monitors, self.follow.as_deref());
        let changed = t != self.target;
        self.target = t;
        changed
    }

    pub fn place_visual(&self, s: f32) {
        let r = self.target.visual_rect(s);
        unsafe {
            let _ = SetWindowPos(self.visual, Some(HWND_TOPMOST), r.l, r.t, r.w(), r.h(), SWP_NOACTIVATE);
        }
    }

    /// The island's resting rectangle on the virtual desktop (physical px, shape + bubble): where
    /// the input window is. `None` before the first placement or without an input window.
    pub fn home_rect(&self) -> Option<monitors::IRect> {
        let (x, y, w, h) = self.hit_rect;
        (self.hit.is_some() && w > 0 && h > 0).then(|| monitors::IRect::new(x, y, x + w, y + h))
    }

    /// How far the input window reaches above the island's shapes (px): the pointer's position in
    /// the window, less this, is its position relative to the shapes ([`hit::shape_dip`]).
    pub fn hit_reach(&self) -> i32 {
        self.hit_reach
    }

    /// Move the invisible hit-test window onto the island's target shape (+ bubble), reaching up
    /// to the top edge of the monitor over the width of each shape ([`hit::layout`]).
    pub fn place_hit(&mut self, hs: &HitShape) {
        let Some(hit) = self.hit else { return };
        let mut vr = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.visual, &mut vr);
        }
        let lay = hit::layout((vr.left, vr.top), self.target.mon.t, hs);
        let (w, h) = (lay.window.w(), lay.window.h());
        unsafe {
            let mut p = POINT::default();
            if GetCursorPos(&mut p).is_ok() {
                self.moved_under = Some((p.x, p.y));
            }
            let mut rgn = HRGN::default();
            for (i, part) in lay.parts.iter().enumerate() {
                let piece = match *part {
                    hit::Part::Round { l, t, r, b, radius } => CreateRoundRectRgn(l, t, r, b, radius * 2, radius * 2),
                    hit::Part::Ellipse { l, t, r, b } => CreateEllipticRgn(l, t, r, b),
                    hit::Part::Rect { l, t, r, b } => CreateRectRgn(l, t, r, b),
                };
                if i == 0 {
                    rgn = piece;
                } else {
                    CombineRgn(Some(rgn), Some(rgn), Some(piece), RGN_OR);
                    let _ = DeleteObject(piece.into());
                }
            }
            // Compared with where the window really is, not with what was set last: a window the
            // system moved (a display change, a resume) is put back too.
            self.hit_rect = (lay.home.l, lay.home.t, lay.home.w(), lay.home.h());
            self.hit_reach = lay.reach;
            let mut now = RECT::default();
            let placed = GetWindowRect(hit, &mut now).is_ok() && monitors::IRect::from_rect(now) == lay.window;
            if !placed {
                let _ = SetWindowPos(hit, Some(HWND_TOPMOST), lay.window.l, lay.window.t, w, h, SWP_NOACTIVATE);
            }
            SetWindowRgn(hit, Some(rgn), true);
        }
    }
}

/// Are the global hotkeys let go? Paused, or a full-screen app hides the island and nothing forces it
/// visible. (`hide_apps` and a capture do not release them.)
pub fn released(paused: bool, hide_fullscreen: bool, fullscreen: bool, forced: u32) -> bool {
    paused || (hide_fullscreen && fullscreen && forced == 0)
}

/// Should the island be on screen? Paused (tray) and suppressed (a screen capture) always
/// hide it; a full-screen app hides it unless a feature forces it visible (`forced` > 0).
fn want_visible(paused: bool, suppressed: u32, fullscreen_hides: bool, forced: u32) -> bool {
    !paused && suppressed == 0 && (!fullscreen_hides || forced > 0)
}

pub(crate) fn fullscreen_busy() -> bool {
    matches!(unsafe { SHQueryUserNotificationState() }, Ok(QUNS_BUSY) | Ok(QUNS_RUNNING_D3D_FULL_SCREEN) | Ok(QUNS_PRESENTATION_MODE))
}

extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    // Deferred, not dropped, when the app is busy (a modal loop / synchronous COM call):
    // a lost foreground change would leave the island hidden (or shown) until the next one.
    with_app_or_defer(|a| {
        let busy = fullscreen_busy();
        if busy != a.shell.fullscreen {
            a.shell.fullscreen = busy;
            a.refresh_visibility(true);
        }
        // Another window came to the front while an input had the keyboard (also catches the
        // case where activating the island failed and so no WM_ACTIVATE will ever come).
        a.check_input_foreground();
        // `placement`: the per-app hide rules.
        a.on_foreground_changed();
    });
}

fn lparam_xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32)
}

/// Pointer position over the hit window, in DIP relative to the island's shapes (the window
/// reaches above them, see [`hit`]).
fn dip(a: &App, lp: LPARAM) -> (f32, f32) {
    hit::shape_dip(lparam_xy(lp), a.shell.hit_reach(), a.scale())
}

/// Windows "Animation effects" (Settings → Accessibility → Visual effects). Read on every
/// `App::scene_opts`; `true` when the call fails.
pub(crate) fn client_area_animation() -> bool {
    let mut on = BOOL(1);
    unsafe {
        if SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut BOOL as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).is_err() {
            return true;
        }
    }
    on.as_bool()
}

impl Shell {
    /// The pointer rests on the island but we are not tracking it (no mouse move arrived, e.g.
    /// right after an OLE drop): start tracking it like a hover, so the island stays open until it
    /// leaves. Returns whether it is over the island.
    pub fn adopt_pointer(&mut self) -> bool {
        let Some(hit) = self.hit else { return false };
        if self.tracking {
            return true;
        }
        if self.hidden() || cursor_outside(hit) {
            return false;
        }
        self.tracking = true;
        track(hit, None);
        true
    }
}

fn cursor_outside(hwnd: HWND) -> bool {
    unsafe {
        let mut p = POINT::default();
        let mut r = RECT::default();
        let _ = GetCursorPos(&mut p);
        let _ = GetWindowRect(hwnd, &mut r);
        p.x < r.left || p.x >= r.right || p.y < r.top || p.y >= r.bottom
    }
}

/// A pointer at `cursor` that has not moved since the hit window last moved or reshaped
/// (`moved_under`) did not enter the island by itself: the island came to it.
fn pointer_is_passive(cursor: (i32, i32), moved_under: Option<(i32, i32)>) -> bool {
    moved_under == Some(cursor)
}

fn track(hwnd: HWND, hover_ms: Option<u32>) {
    let mut tme = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE | if hover_ms.is_some() { TME_HOVER } else { Default::default() },
        hwndTrack: hwnd,
        dwHoverTime: hover_ms.unwrap_or(0).max(1),
    };
    unsafe {
        let _ = TrackMouseEvent(&mut tme);
    }
}

fn hit_msg(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if let Some(r) = keyboard::on_message(hwnd, msg, wp, lp) {
        return Some(r);
    }
    match msg {
        WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
        WM_SETCURSOR => {
            unsafe {
                if let Ok(c) = LoadCursorW(None, IDC_ARROW) {
                    SetCursor(Some(c));
                }
            }
            Some(LRESULT(1))
        }
        WM_MOUSEMOVE => {
            let mut cursor = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut cursor);
            }
            with_app(|a| {
                let cfg = a.config();
                let hover_delay = (cfg.island.open_on == moku_core::config::OpenOn::Hover && a.mode != Mode::Panel).then_some(cfg.island.hover_delay_ms);
                // Hover-to-open needs a pointer that moved onto the island. When the island
                // appears or grows under a resting pointer (start-up, a wider music pill),
                // Windows sends a synthetic move at the same spot: show the hover state, but do
                // not arm the open-on-hover timer until the pointer really moves.
                let passive = pointer_is_passive((cursor.x, cursor.y), a.shell.moved_under);
                if !a.shell.tracking {
                    a.shell.tracking = true;
                    a.shell.hover_armed = hover_delay.is_some() && !passive;
                    track(hwnd, hover_delay.filter(|_| !passive));
                    a.on_mouse_enter();
                } else if !a.shell.hover_armed && hover_delay.is_some() && !passive {
                    a.shell.hover_armed = true;
                    track(hwnd, hover_delay);
                }
                let (x, y) = dip(a, lp);
                a.on_mouse_move(x, y);
            });
            Some(LRESULT(0))
        }
        WM_MOUSEHOVER => {
            with_app(|a| a.on_hover_dwell());
            Some(LRESULT(0))
        }
        WM_MOUSELEAVE => {
            with_app_or_defer(|a| {
                a.shell.tracking = false;
                a.shell.hover_armed = false;
                a.on_mouse_leave();
            });
            Some(LRESULT(0))
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            unsafe {
                SetCapture(hwnd);
            }
            let double = msg == WM_LBUTTONDBLCLK;
            with_app(|a| {
                let (x, y) = dip(a, lp);
                a.on_button_down(x, y, double);
            });
            Some(LRESULT(0))
        }
        WM_LBUTTONUP => {
            let outside = cursor_outside(hwnd);
            // Handle the press before releasing capture: ReleaseCapture sends
            // WM_CAPTURECHANGED, which cancels a press that is still open.
            with_app(|a| {
                if outside {
                    a.shell.tracking = false;
                }
                let (x, y) = dip(a, lp);
                a.on_button_up(x, y);
            });
            unsafe {
                let _ = ReleaseCapture();
            }
            Some(LRESULT(0))
        }
        WM_CAPTURECHANGED => {
            let outside = cursor_outside(hwnd);
            with_app_or_defer(move |a| a.cancel_press(outside));
            Some(LRESULT(0))
        }        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
            let (sx, sy) = lparam_xy(lp);
            let mut p = POINT { x: sx, y: sy };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut p);
            }
            with_app(|a| {
                let (x, y) = hit::shape_dip((p.x, p.y), a.shell.hit_reach(), a.scale());
                a.on_wheel(x, y, delta);
            });
            Some(LRESULT(0))
        }
        WM_RBUTTONUP => {
            let owner = with_app(|a| (a.shell.tray_hwnd, a.shell.paused)).unwrap_or((None, false));
            if let (Some(t), paused) = owner {
                tray::show_menu(t, paused);
            }
            Some(LRESULT(0))
        }
        _ => None,
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let role = role_of(hwnd);
        // Hit window input.
        if role == Role::Hit && let Some(r) = hit_msg(hwnd, msg, wp, lp) {
            return r;
        }
        // Tray window (only it acts on TaskbarCreated; every top-level window receives it).
        if role == Role::Tray && (msg == WM_APP_TRAY || msg == tray::taskbar_created())
            && let Some(r) = tray::on_message(hwnd, msg, wp, lp)
        {
            return r;
        }
        if role == Role::Tray && msg == WM_APP_APPBAR {
            if wp.0 as u32 == ABN_FULLSCREENAPP {
                let on = lp.0 != 0;
                with_app_or_defer(move |a| a.on_fullscreen_hint(on));
            }
            return LRESULT(0);
        }
        match msg {
            WM_APP_RUN => {
                if with_app(|a| a.run_jobs()).is_none() {
                    App::retry_jobs_later(hwnd);
                }
                LRESULT(0)
            }
            WM_APP_FLUSH => {
                with_app_or_defer(|a| a.flush());
                LRESULT(0)
            }
            WM_APP_OPEN => {
                log_info!("second launch: opening the panel");
                with_app_or_defer(|a| {
                    if !a.is_panel_open() {
                        a.toggle_panel()
                    }
                });
                LRESULT(0)
            }
            WM_TIMER => {
                let id = wp.0;
                with_app_or_defer(move |a| a.on_timer(id));
                LRESULT(0)
            }
            WM_HOTKEY => {
                match wp.0 as i32 {
                    HOTKEY_QUIT => PostQuitMessage(0),
                    HOTKEY_TOGGLE => with_app_or_defer(|a| a.toggle_panel()),
                    _ => with_app_or_defer(move |a| a.on_window_message(WM_HOTKEY, wp, lp)),
                }
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ if role == Role::Visual => {
                if HOOKED.with(|h| h.borrow().contains(&msg)) {
                    with_app_or_defer(move |a| a.on_window_message(msg, wp, lp));
                }
                match msg {
                    // Resolution / monitor layout / primary monitor / DPI / work area changed.
                    WM_DISPLAYCHANGE | WM_DPICHANGED => {
                        with_app_or_defer(|a| a.reposition());
                        return LRESULT(0);
                    }
                    WM_SETTINGCHANGE if wp.0 == SPI_SETWORKAREA => with_app_or_defer(|a| a.reposition()),
                    // Settings → Accessibility → Visual effects → "Animation effects" toggled.
                    WM_SETTINGCHANGE if wp.0 == SPI_SETCLIENTAREAANIMATION.0 as usize => with_app_or_defer(|a| a.reposition()),
                    // Back from sleep / hibernate: the minute timer is stale (it was armed for a
                    // minute boundary before the sleep).
                    WM_POWERBROADCAST if wp.0 == PBT_APMRESUMEAUTOMATIC as usize => with_app_or_defer(|a| a.on_resume()),
                    WM_QUERYENDSESSION => return LRESULT(1),
                    // Logoff / shutdown: the process may be killed right after we return.
                    WM_ENDSESSION if wp.0 != 0 => {
                        crate::app::shutdown();
                        return LRESULT(0);
                    }
                    WM_DESTROY => {
                        PostQuitMessage(0);
                        return LRESULT(0);
                    }
                    _ => {}
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Visual,
    Hit,
    Tray,
    Other,
}

thread_local! {
    /// (visual, hit, tray) handles, readable while `App` is borrowed (re-entrant messages).
    static ROLES: std::cell::Cell<(isize, isize, isize)> = const { std::cell::Cell::new((0, 0, 0)) };
    /// Window messages some feature registered with `Registry::on_message`.
    pub(crate) static HOOKED: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn role_of(h: HWND) -> Role {
    let (v, hit, t) = ROLES.with(|r| r.get());
    let h = h.0 as isize;
    if h == v {
        Role::Visual
    } else if h == hit {
        Role::Hit
    } else if h == t {
        Role::Tray
    } else {
        Role::Other
    }
}
#[cfg(test)]
mod tests {
    use super::{hides_unless_forced, pointer_is_passive, want_visible};

    #[test]
    fn visibility_rules() {
        assert!(want_visible(false, 0, false, 0));
        // Full screen hides, unless a feature forces the island (pending permission card).
        assert!(!want_visible(false, 0, true, 0));
        assert!(want_visible(false, 0, true, 1));
        // Paused (tray) and a screen capture always win over forcing.
        assert!(!want_visible(true, 0, false, 1));
        assert!(!want_visible(false, 1, true, 2));
    }

    #[test]
    fn a_listed_app_hides_the_island_like_a_full_screen_app() {
        // Full screen only counts when `island.hide_fullscreen` is on; a listed app always does.
        assert!(!hides_unless_forced(true, false, false));
        assert!(hides_unless_forced(true, true, false));
        assert!(!hides_unless_forced(false, true, false));
        assert!(hides_unless_forced(false, false, true));
        assert!(hides_unless_forced(false, true, true));
        // The same forcing applies: a pending permission card still shows over a listed app, but
        // never over a pause or a capture.
        assert!(want_visible(false, 0, hides_unless_forced(true, false, true), 1));
        assert!(!want_visible(false, 0, hides_unless_forced(true, false, true), 0));
        assert!(!want_visible(true, 0, hides_unless_forced(true, false, true), 1));
    }

    #[test]
    fn island_appearing_under_a_resting_pointer_is_passive() {
        // The hit window moved / grew while the pointer rested at (500, 20): the synthetic move
        // Windows sends at the same spot must not arm hover-to-open.
        assert!(pointer_is_passive((500, 20), Some((500, 20))));
        // A pointer that moved onto the island (or no move of ours recorded yet) is real.
        assert!(!pointer_is_passive((501, 20), Some((500, 20))));
        assert!(!pointer_is_passive((500, 20), None));
    }
}
