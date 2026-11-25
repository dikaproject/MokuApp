//! Moku island spike: Win32 window + Windows.UI.Composition.
//!
//! Two windows:
//! - the *visual* window (big enough for the panel + shadow) hosts the composition tree. It is
//!   WS_EX_LAYERED | WS_EX_TRANSPARENT, so it never receives mouse input: clicks fall through
//!   everywhere, including on the transparent margins and the shadow.
//! - the *hit* window is an invisible (no redirection bitmap, nothing drawn) top-most window
//!   that is moved/reshaped (SetWindowRgn, rounded) to the island's target shape on each state
//!   change. It is the only thing that takes mouse input. Hover uses TrackMouseEvent
//!   (WM_MOUSEHOVER after 400 ms, WM_MOUSELEAVE) so there is no polling at all.
//!
//! Flags: --state <idle|hover|music|music-panel>, --demo, --blur (HostBackdropBrush glass),
//! --gpu (hardware D3D for text rasterisation), --test-bg (striped window behind the island).
//! Quit: right-click the island, or Ctrl+Alt+Shift+Q from anywhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gfx;
mod island;
mod state;

use std::cell::RefCell;

use island::{Island, TOP, WIN_H, WIN_W};
use state::State;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::*;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

const WM_MOUSEHOVER: u32 = 0x02A1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const HOTKEY_QUIT: i32 = 1;

#[derive(PartialEq)]
enum Mode {
    Interactive,
    Fixed,
    Demo,
}

struct App {
    island: Island,
    visual: HWND,
    hit: Option<HWND>,
    mode: Mode,
    resting: State,
    tracking: bool,
    demo_step: usize,
    hidden: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

const DEMO: [State; 5] = [State::Idle, State::Hover, State::Music, State::Panel, State::Music];

/// Primary monitor rectangle (physical px) and its DPI scale.
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

/// Physical-pixel rectangle of the visual window.
fn visual_rect(mon: &RECT, s: f32) -> RECT {
    let w = (WIN_W * s).round() as i32;
    let h = (WIN_H * s).round() as i32;
    let x = mon.left + (mon.right - mon.left - w) / 2;
    RECT { left: x, top: mon.top, right: x + w, bottom: mon.top + h }
}

impl App {
    fn go(&mut self, to: State) {
        if self.island.state == to {
            return;
        }
        let _ = self.island.set_state(to);
        self.place_hit();
    }

    /// Move the invisible hit-test window onto the island's target shape.
    fn place_hit(&self) {
        let Some(hit) = self.hit else { return };
        let s = self.island.scale();
        let (mon, _) = primary_monitor();
        let vr = visual_rect(&mon, s);
        let (w, h) = self.island.size(self.island.state);
        // Same rounding as the island's Offset expression.
        let x = vr.left + ((WIN_W - w) * 0.5 * s).round() as i32;
        let y = vr.top + (TOP * s).round() as i32;
        let (pw, ph) = ((w * s).round() as i32, (h * s).round() as i32);
        let r = (state::radius(h) * 2.0 * s).round() as i32;
        unsafe {
            let _ = SetWindowPos(hit, Some(HWND_TOPMOST), x, y, pw, ph, SWP_NOACTIVATE);
            let rgn = CreateRoundRectRgn(0, 0, pw + 1, ph + 1, r, r);
            SetWindowRgn(hit, Some(rgn), true);
        }
    }

    /// Re-anchor both windows to the (possibly changed) primary monitor, rebuilding the
    /// surfaces if its DPI changed. Used for WM_DISPLAYCHANGE and WM_DPICHANGED.
    fn reposition(&mut self) {
        let (mon, s) = primary_monitor();
        if (s - self.island.scale()).abs() > 0.001 {
            let _ = self.island.set_scale(s);
        }
        let r = visual_rect(&mon, s);
        unsafe {
            let _ = SetWindowPos(self.visual, Some(HWND_TOPMOST), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE);
        }
        self.place_hit();
    }

    /// Hide while a full-screen app / presentation is in front (checked on foreground change).
    fn update_fullscreen(&mut self) {
        let busy = matches!(
            unsafe { SHQueryUserNotificationState() },
            Ok(QUNS_BUSY) | Ok(QUNS_RUNNING_D3D_FULL_SCREEN) | Ok(QUNS_PRESENTATION_MODE)
        );
        if busy == self.hidden {
            return;
        }
        self.hidden = busy;
        let cmd = if busy { SW_HIDE } else { SW_SHOWNOACTIVATE };
        unsafe {
            let _ = ShowWindow(self.visual, cmd);
            if let Some(hit) = self.hit {
                let _ = ShowWindow(hit, cmd);
            }
        }
    }
}

fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|a| {
        if let Ok(mut g) = a.try_borrow_mut()
            && let Some(app) = g.as_mut()
        {
            f(app)
        }
    });
}

extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    with_app(|a| a.update_fullscreen());
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_MOUSEMOVE => {
                with_app(|a| {
                    if !a.tracking {
                        a.tracking = true;
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE | TME_HOVER,
                            hwndTrack: hwnd,
                            dwHoverTime: 400,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        if a.island.state != State::Panel {
                            a.go(State::Hover);
                        }
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEHOVER | WM_LBUTTONDOWN => {
                with_app(|a| a.go(State::Panel));
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with_app(|a| {
                    a.tracking = false;
                    let r = a.resting;
                    a.go(r);
                });
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_HOTKEY if wp.0 == HOTKEY_QUIT as usize => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_TIMER => {
                with_app(|a| {
                    if a.mode == Mode::Demo {
                        a.demo_step = (a.demo_step + 1) % DEMO.len();
                        a.go(DEMO[a.demo_step]);
                    }
                });
                LRESULT(0)
            }
            // Resolution / monitor layout / primary monitor / DPI changed: re-anchor both windows.
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                with_app(|a| {
                    if a.visual == hwnd {
                        a.reposition()
                    }
                });
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// `--test-bg`: a colourful striped window under the island, so the glass (blur or tint) can be
/// judged regardless of what is on the user's screen. Plain GDI, test aid only.
extern "system" fn testbg_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_PAINT {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let colors = [0x4FD8FF, 0xFF5FA2, 0xFFD60A, 0x30D158, 0x7C6BFF, 0xFF9F0A];
            let (mut x, mut i) = (0, 0);
            while x < rc.right {
                let c = colors[i % colors.len()];
                let br = CreateSolidBrush(COLORREF(((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff)));
                FillRect(dc, &RECT { left: x, top: 0, right: x + 40, bottom: rc.bottom }, br);
                let _ = DeleteObject(br.into());
                x += 40;
                i += 1;
            }
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

fn create_window(class: PCWSTR, proc: WNDPROC, ex: WINDOW_EX_STYLE, r: RECT) -> Result<HWND> {
    unsafe {
        let inst = GetModuleHandleW(None)?;
        let wc = WNDCLASSW {
            lpfnWndProc: proc,
            hInstance: inst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        CreateWindowExW(ex, class, w!("Moku"), WS_POPUP, r.left, r.top, r.right - r.left, r.bottom - r.top, None, None, Some(inst.into()), None)
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (mut fixed, mut demo, mut blur, mut test_bg) = (None, false, false, false);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--state" => {
                i += 1;
                fixed = args.get(i).and_then(|s| State::parse(s));
            }
            "--demo" => demo = true,
            "--blur" => blur = true,
            "--test-bg" => test_bg = true,
            _ => {}
        }
        i += 1;
    }
    let mode = match (demo, fixed.is_some()) {
        (true, _) => Mode::Demo,
        (_, true) => Mode::Fixed,
        _ => Mode::Interactive,
    };
    let start = if demo { State::Idle } else { fixed.unwrap_or(State::Idle) };

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Composition needs a DispatcherQueue on the UI thread; it is pumped by our message loop.
        let _dq = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?;

        let (mon, scale) = primary_monitor();
        let vr = visual_rect(&mon, scale);
        if test_bg {
            let r = RECT { left: vr.left - 60, right: vr.right + 60, ..vr };
            let bg = create_window(w!("MokuTestBg"), Some(testbg_wndproc), WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, r)?;
            let _ = ShowWindow(bg, SW_SHOWNOACTIVATE);
        }

        // Layered + transparent: never hit-tested, so clicks fall through the whole window.
        let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP | WS_EX_LAYERED | WS_EX_TRANSPARENT;
        let visual = create_window(w!("MokuIsland"), Some(wndproc), ex, vr)?;
        SetLayeredWindowAttributes(visual, COLORREF(0), 255, LWA_ALPHA)?;
        if blur {
            // Lets the composition HostBackdropBrush see the (DWM-blurred) desktop behind us.
            let on = windows::core::BOOL(1);
            let _ = DwmSetWindowAttribute(visual, DWMWA_USE_HOSTBACKDROPBRUSH, &on as *const _ as _, 4);
        }
        let corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(visual, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
        let none: u32 = DWMWA_COLOR_NONE;
        let _ = DwmSetWindowAttribute(visual, DWMWA_BORDER_COLOR, &none as *const _ as _, 4);
        // Global quit hotkey, in every mode (fails harmlessly if another instance holds it).
        let _ = RegisterHotKey(Some(visual), HOTKEY_QUIT, MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, u32::from(b'Q'));

        let island = Island::new(visual, scale, start, blur)?;
        let _ = ShowWindow(visual, SW_SHOWNOACTIVATE);

        let hit = if mode == Mode::Interactive {
            let hex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP;
            let h = create_window(w!("MokuIslandHit"), Some(wndproc), hex, RECT { left: 0, top: 0, right: 1, bottom: 1 })?;
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            Some(h)
        } else {
            None
        };
        if mode == Mode::Demo {
            SetTimer(Some(visual), 1, 1500, None);
        }

        let mut app = App { island, visual, hit, mode, resting: start, tracking: false, demo_step: 0, hidden: false };
        app.place_hit();
        // Full-screen hiding is event-driven (foreground changes), not polled. Not in --state
        // mode, which exists for screenshots.
        let _hook = if app.mode != Mode::Fixed {
            app.update_fullscreen();
            Some(SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(on_foreground), 0, 0, WINEVENT_OUTOFCONTEXT))
        } else {
            None
        };
        APP.with(|a| *a.borrow_mut() = Some(app));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|a| a.borrow_mut().take());
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        let text: Vec<u16> = format!("{e:?}\0").encode_utf16().collect();
        unsafe {
            MessageBoxW(None, PCWSTR(text.as_ptr()), w!("Moku island"), MB_ICONERROR);
        }
    }
}
