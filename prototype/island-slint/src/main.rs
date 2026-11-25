//! Moku island - Slint spike. UI + motion live in ui/island.slint; this file is the state machine,
//! the CLI flags and the Win32 glue (tool window, top-centre placement, click-through).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use slint::winit_030::winit::dpi::PhysicalPosition;
use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Timer, TimerMode};

slint::include_modules!();

/// Resting top gap of the shape inside the window (DIP); must match `top-gap` in the .slint.
const TOP_GAP: f32 = 8.0;
/// Window width in DIP; must match `width` in the .slint (used to derive the live scale factor).
const WIN_W: f32 = 720.0;
const DWELL: Duration = Duration::from_millis(400);
const DEMO_STEP: Duration = Duration::from_millis(1500);

static LOG: AtomicBool = AtomicBool::new(false);
macro_rules! log {
    ($($a:tt)*) => { if LOG.load(Ordering::Relaxed) { eprintln!($($a)*); } };
}

/// Frame statistics for `--console` (FemtoVG only): count, summed and worst frame interval (us).
static FRAMES: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

fn frame_report() -> String {
    let [n, sum, max] = [0, 1, 2].map(|i| FRAMES[i].swap(0, Ordering::Relaxed));
    format!("{n} frames, avg {:.1} ms, worst {:.1} ms", sum as f64 / n.max(1) as f64 / 1000.0, max as f64 / 1000.0)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Idle,
    Hover,
    Music,
    Panel,
}

impl Mode {
    fn parse(s: &str) -> Option<Mode> {
        Some(match s {
            "idle" => Mode::Idle,
            "hover" => Mode::Hover,
            "music" => Mode::Music,
            "music-panel" => Mode::Panel,
            _ => return None,
        })
    }
}

/// State machine. Rust only flips `mode` (+ which motion to use); Slint animates everything.
struct Ctl {
    ui: slint::Weak<Island>,
    cur: Cell<Mode>,
    resting: Cell<Mode>,
    dwell: Timer,
    t0: Instant,
}

impl Ctl {
    fn go(&self, to: Mode) {
        let from = self.cur.get();
        if from == to {
            return;
        }
        // 0 hover ease-out, 1 open panel, 2 close panel, 3 pill <-> pill
        let kind = match (from, to) {
            (_, Mode::Panel) => 1,
            (Mode::Panel, _) => 2,
            (Mode::Idle, Mode::Hover) | (Mode::Hover, Mode::Idle) => 0,
            _ => 3,
        };
        log!("[{:>6} ms] {:?} -> {:?} (anim {}) | previous period: {}", self.t0.elapsed().as_millis(), from, to, kind, frame_report());
        self.cur.set(to);
        if let Some(ui) = self.ui.upgrade() {
            ui.set_anim_kind(kind);
            ui.set_mode(to as i32);
        }
    }

    fn pointer_entered(self: &Rc<Self>) {
        let cur = self.cur.get();
        if cur == Mode::Idle || cur == Mode::Music {
            self.resting.set(cur);
            self.go(Mode::Hover);
            let me = self.clone();
            self.dwell.start(TimerMode::SingleShot, DWELL, move || me.go(Mode::Panel));
        }
    }

    fn pointer_left(&self) {
        self.dwell.stop();
        if matches!(self.cur.get(), Mode::Hover | Mode::Panel) {
            self.go(self.resting.get());
        }
    }

    fn clicked(&self) {
        self.dwell.stop();
        if self.cur.get() != Mode::Panel {
            self.go(Mode::Panel);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1));
    let demo = flag("--demo");
    win::ACRYLIC.store(flag("--acrylic"), Ordering::Relaxed);
    win::NO_REGION.store(flag("--no-region"), Ordering::Relaxed); // experiment: no click-through region
    let fixed = value("--state").and_then(|s| Mode::parse(s));
    if flag("--console") {
        win::attach_console();
        LOG.store(true, Ordering::Relaxed);
    }
    if let Some(s) = value("--state") {
        if fixed.is_none() {
            log!("unknown --state {s} (idle | hover | music | music-panel)");
        }
    }

    let renderer = if cfg!(feature = "sw") { "software" } else { "femtovg" };
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(renderer.into())
        .with_winit_window_attributes_hook(|a| a.with_skip_taskbar(true).with_active(false))
        .select()
        .expect("backend");

    let ui = Island::new().unwrap();
    let start = fixed.unwrap_or(Mode::Idle);
    ui.set_mode(start as i32);
    ui.set_real_glass(flag("--acrylic"));
    ui.set_no_shadow(flag("--no-shadow"));
    ui.set_no_clip(flag("--no-clip"));
    let ctl = Rc::new(Ctl {
        ui: ui.as_weak(),
        cur: Cell::new(start),
        resting: Cell::new(start),
        dwell: Timer::default(),
        t0: Instant::now(),
    });

    #[cfg(feature = "femtovg")]
    if LOG.load(Ordering::Relaxed) {
        let last = Cell::new(Instant::now());
        ui.window()
            .set_rendering_notifier(move |state, _| {
                if let slint::RenderingState::AfterRendering = state {
                    let now = Instant::now();
                    let us = now.duration_since(last.replace(now)).as_micros() as u64;
                    if us < 100_000 {
                        FRAMES[0].fetch_add(1, Ordering::Relaxed);
                        FRAMES[1].fetch_add(us, Ordering::Relaxed);
                        FRAMES[2].fetch_max(us, Ordering::Relaxed);
                    }
                }
            })
            .ok();
    }

    // Slint reports the animated shape every frame it changes; it becomes the window region
    // (everything outside it is click-through).
    win::set_geometry(ui.get_geom());
    ui.on_geometry(win::set_geometry);
    ui.on_quit(|| {
        let _ = slint::quit_event_loop();
    });

    // `--state` and `--demo` ignore the pointer (screenshots / measurements must stay put).
    if fixed.is_none() && !demo {
        let c = ctl.clone();
        ui.on_pointer_entered(move || c.pointer_entered());
        let c = ctl.clone();
        ui.on_pointer_left(move || c.pointer_left());
        let c = ctl.clone();
        ui.on_clicked(move || c.clicked());
    }

    // idle -> hover -> music -> music-panel -> music -> (idle) ... 1.5 s per step
    let demo_timer = Timer::default();
    if demo {
        let seq = [Mode::Idle, Mode::Hover, Mode::Music, Mode::Panel, Mode::Music];
        let step = Cell::new(0usize);
        let c = ctl.clone();
        demo_timer.start(TimerMode::Repeated, DEMO_STEP, move || {
            step.set((step.get() + 1) % seq.len());
            c.go(seq[step.get()]);
        });
    }

    // Once the winit window exists: tool window, top-centre placement, click-through hook.
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let ui = weak.unwrap();
        if let Ok(w) = ui.window().winit_window().await {
            win::setup(&w);
            if LOG.load(Ordering::Relaxed) {
                use slint::winit_030::winit::event::WindowEvent as E;
                ui.window().on_winit_window_event(|_, e| {
                    match e {
                        E::CursorEntered { .. } | E::CursorLeft { .. } => log!("   winit: {e:?}"),
                        E::CursorMoved { position, .. } => log!("   winit: CursorMoved {:.0},{:.0}", position.x, position.y),
                        _ => {}
                    }
                    slint::winit_030::EventResult::Propagate
                });
            }
            log!("window ready: {:?} scale {}", w.outer_size(), w.scale_factor());
        }
    })
    .unwrap();

    ui.show().unwrap();
    slint::run_event_loop_until_quit().unwrap();
    drop(demo_timer);
}

/// All the raw Win32 bits.
mod win {
    use super::*;
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::winit::window::Window;
    use std::ptr::null_mut;
    use std::sync::atomic::AtomicIsize;
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
    use windows_sys::Win32::System::Console::{AllocConsole, AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    static HWND_PTR: AtomicIsize = AtomicIsize::new(0);
    /// `--acrylic` experiment: ask DWM for an acrylic system backdrop behind the window.
    pub static ACRYLIC: AtomicBool = AtomicBool::new(false);
    pub static NO_REGION: AtomicBool = AtomicBool::new(false);
    /// Latest shape (w, h, radius, shadow blur, shadow dy) in DIP as f32 bits.
    static GEOM: [AtomicU32; 5] = [const { AtomicU32::new(0) }; 5];

    /// Called by Slint every frame the shape changes: remember it and reshape the window region.
    pub fn set_geometry(g: Geom) {
        for (slot, v) in GEOM.iter().zip([g.w, g.h, g.r, g.blur, g.dy]) {
            slot.store(v.to_bits(), Ordering::Relaxed);
        }
        apply_region();
    }

    /// Click-through: Windows only delivers mouse input inside the window region, so the region is the
    /// shape grown by its shadow extent (HTTRANSPARENT does not pass clicks to other processes' windows).
    fn apply_region() {
        let hwnd = HWND_PTR.load(Ordering::Relaxed) as HWND;
        if hwnd.is_null() || NO_REGION.load(Ordering::Relaxed) {
            return;
        }
        let [w, h, r, mut blur, mut dy] = [0, 1, 2, 3, 4].map(|i| f32::from_bits(GEOM[i].load(Ordering::Relaxed)));
        if ACRYLIC.load(Ordering::Relaxed) {
            (blur, dy) = (0.0, 0.0); // the backdrop fills the whole region, so it has to be the exact shape
        }
        unsafe {
            let mut rc: RECT = std::mem::zeroed();
            GetWindowRect(hwnd, &mut rc);
            let s = (rc.right - rc.left) as f32 / WIN_W; // physical px per DIP, whatever Slint currently uses
            let (left, right) = ((WIN_W - w) / 2.0 - blur, (WIN_W + w) / 2.0 + blur);
            let (top, bottom) = (TOP_GAP - (blur - dy).max(0.0), TOP_GAP + h + blur + dy);
            let px = |v: f32| (v * s).round() as i32;
            let rgn = CreateRoundRectRgn(px(left.max(0.0)), px(top.max(0.0)), px(right) + 1, px(bottom) + 1, px(2.0 * (r + blur)), px(2.0 * (r + blur)));
            SetWindowRgn(hwnd, rgn, 0); // the system owns `rgn` from here on
            if (w - 120.0).abs() < 0.01 || (w - 640.0).abs() < 0.01 {
                log!("region: shape {w}x{h} r{r} blur{blur} dy{dy} -> ({left},{top})-({right},{bottom}) DIP, scale {s}");
            }
        }
    }

    /// Experiment: legacy (undocumented) SetWindowCompositionAttribute accent = acrylic blur-behind.
    unsafe fn enable_acrylic(hwnd: HWND) {
        #[repr(C)]
        struct Accent { state: i32, flags: i32, gradient: u32, animation: i32 }
        #[repr(C)]
        struct Data { attrib: i32, data: *mut Accent, size: usize }
        type SetWca = unsafe extern "system" fn(HWND, *mut Data) -> i32;
        use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
        let user32 = LoadLibraryA(b"user32.dll ".as_ptr());
        let Some(f) = GetProcAddress(user32, b"SetWindowCompositionAttribute ".as_ptr()) else { return };
        let f: SetWca = std::mem::transmute(f);
        // ACCENT_ENABLE_ACRYLICBLURBEHIND = 4, tint 0x01RRGGBB-ish (ABGR); alpha 1 so the blur is not tinted twice.
        let mut accent = Accent { state: 4, flags: 2, gradient: 0x01_12_0A_0A, animation: 0 };
        let mut d = Data { attrib: 19, data: &mut accent, size: std::mem::size_of::<Accent>() };
        let ok = f(hwnd, &mut d);
        log!("SetWindowCompositionAttribute(acrylic) -> {ok}");
    }

    /// `--console`: log to the parent console (or a new one). If stderr is already redirected, keep it.
    pub fn attach_console() {
        unsafe {
            let h = GetStdHandle(STD_ERROR_HANDLE);
            if (h.is_null() || h as isize == -1) && AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                AllocConsole();
            }
        }
    }

    pub fn setup(w: &Window) {
        // Top centre of the primary monitor, 0 px from the top (the shape has its own 8 DIP gap).
        if let Some(m) = w.primary_monitor() {
            let x = m.position().x + (m.size().width as i32 - w.outer_size().width as i32) / 2;
            w.set_outer_position(PhysicalPosition::new(x, m.position().y));
        }
        let Ok(handle) = w.window_handle() else { return };
        let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
        let hwnd = h.hwnd.get() as HWND;
        unsafe {
            // Tool window: never in Alt-Tab (skip_taskbar already removed the taskbar button).
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex | WS_EX_TOOLWINDOW as isize) & !(WS_EX_APPWINDOW as isize));
            SetWindowPos(hwnd, null_mut(), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        }
        if ACRYLIC.load(Ordering::Relaxed) {
            unsafe { enable_acrylic(hwnd) }
        }
        HWND_PTR.store(hwnd as isize, Ordering::Relaxed);
        apply_region();
    }
}
