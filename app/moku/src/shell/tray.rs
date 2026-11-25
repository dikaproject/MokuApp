//! Notification-area icon and menu: Pengaturan · Jeda/Lanjutkan · Keluar.
//! Re-added when Explorer restarts (`TaskbarCreated` broadcast).

use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};
use windows_numerics::Vector2;

use crate::app::{WM_APP_TRAY, with_app, with_app_or_defer};
use crate::i18n::{self, tr};

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// The registered `TaskbarCreated` message id.
pub fn taskbar_created() -> u32 {
    let v = TASKBAR_CREATED.load(Ordering::Relaxed);
    if v != 0 {
        return v;
    }
    let id = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    TASKBAR_CREATED.store(id, Ordering::Relaxed);
    id
}

const CMD_SETTINGS: u32 = 1;
const CMD_PAUSE: u32 = 2;
const CMD_QUIT: u32 = 3;

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    pub added: bool,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Tray {
        let _ = taskbar_created();
        let icon = resource_icon().or_else(make_icon).unwrap_or_else(|| unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() });
        Tray { hwnd, icon, added: false }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_APP_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        for (i, c) in "Moku".encode_utf16().enumerate() {
            nid.szTip[i] = c;
        }
        nid
    }

    /// Add (or re-add) the icon. Returns whether it is now in the notification area.
    pub fn add(&mut self) -> bool {
        unsafe {
            let nid = self.data();
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            self.added = Shell_NotifyIconW(NIM_ADD, &nid).as_bool();
        }
        if self.added {
            log_info!("tray icon added");
        } else {
            log_warn!("tray icon could not be added (no taskbar yet?)");
        }
        self.added
    }

    pub fn remove(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
        }
        self.added = false;
    }
}

/// Tray-window messages. Runs the menu outside the app borrow (TrackPopupMenu is modal).
pub fn on_message(hwnd: HWND, msg: u32, _wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if msg == taskbar_created() {
        with_app_or_defer(|a| a.on_taskbar_created());
        return Some(LRESULT(0));
    }
    if msg == WM_APP_TRAY {
        let ev = (lp.0 & 0xffff) as u32;
        if ev == WM_RBUTTONUP || ev == WM_LBUTTONUP || ev == WM_CONTEXTMENU {
            let paused = with_app(|a| a.shell.paused).unwrap_or(false);
            show_menu(hwnd, paused);
        }
        return Some(LRESULT(0));
    }
    None
}

/// Show the menu at the cursor and act on the choice.
pub fn show_menu(owner: HWND, paused: bool) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let items = [
            (CMD_SETTINGS, i18n::pair(i18n::SETTINGS)),
            (CMD_PAUSE, if paused { tr(i18n::RESUME.0, i18n::RESUME.1) } else { tr(i18n::PAUSE.0, i18n::PAUSE.1) }),
            (CMD_QUIT, i18n::pair(i18n::QUIT)),
        ];
        for (i, (id, label)) in items.iter().enumerate() {
            if i == 2 {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            }
            let _ = AppendMenuW(menu, MF_STRING, *id as usize, &HSTRING::from(*label));
        }
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(owner);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, p.x, p.y, Some(0), owner, None).0 as u32;
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match cmd {
            CMD_SETTINGS => super::launch_settings(),
            CMD_PAUSE => {
                with_app(|a| a.set_paused(!paused));
            }
            CMD_QUIT => PostQuitMessage(0),
            _ => {}
        }
    }
}

/// The tray icon from the exe's own resources (id 1, `assets/icon/moku.ico`): `LoadImageW` picks
/// the entry that fits the notification area (16 / 20 / 24 / 32 px for 100 / 125 / 150 / 200 %),
/// and each of those is drawn for its size rather than scaled. `None` when the exe carries no
/// resource (built with `MOKU_NO_RESOURCES`), then `make_icon` draws one.
fn resource_icon() -> Option<HICON> {
    unsafe {
        let n = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()).max(16);
        let inst = HINSTANCE(GetModuleHandleW(None).ok()?.0);
        // MAKEINTRESOURCE(1)
        let id = PCWSTR(1 as *const u16);
        let handle = LoadImageW(Some(inst), id, IMAGE_ICON, n, n, LR_DEFAULTCOLOR).ok()?;
        Some(HICON(handle.0))
    }
}

/// Fallback tray icon: the logo mark on a dark rounded square, drawn with D2D into a DIB.
fn make_icon() -> Option<HICON> {
    unsafe {
        let n = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()).max(16);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: n,
                biHeight: -n,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, dib.into());
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let rt = factory.CreateDCRenderTarget(&props).ok()?;
        rt.BindDC(dc, &RECT { left: 0, top: 0, right: n, bottom: n }).ok()?;
        rt.BeginDraw();
        rt.Clear(Some(&D2D1_COLOR_F::default()));
        let s = n as f32;
        let bg = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.043, g: 0.043, b: 0.078, a: 1.0 }, None).ok()?;
        rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: 0.0, top: 0.0, right: s, bottom: s }, radiusX: s * 0.28, radiusY: s * 0.28 }, &bg);
        // Logo path (100×100 box), drawn a bit larger than the icon and lifted to centre the ink.
        let k = s * 1.12 / 100.0;
        let (ox, oy) = (s * 0.5 - 50.0 * k, s * 0.5 - 60.5 * k);
        let p = |x: f32, y: f32| Vector2 { X: ox + x * k, Y: oy + y * k };
        let geo = factory.CreatePathGeometry().ok()?;
        let sink = geo.Open().ok()?;
        sink.BeginFigure(p(10.0, 70.0), D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddLine(p(25.0, 70.0));
        sink.AddLine(p(25.0, 51.0));
        let arc = |to| D2D1_ARC_SEGMENT { point: to, size: D2D_SIZE_F { width: 12.5 * k, height: 12.5 * k }, rotationAngle: 0.0, sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE, arcSize: D2D1_ARC_SIZE_SMALL };
        sink.AddArc(&arc(p(50.0, 51.0)));
        sink.AddLine(p(50.0, 70.0));
        sink.AddLine(p(50.0, 51.0));
        sink.AddArc(&arc(p(75.0, 51.0)));
        sink.AddLine(p(75.0, 70.0));
        sink.AddLine(p(90.0, 70.0));
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close().ok()?;
        let c = |hex: u32| D2D1_COLOR_F { r: ((hex >> 16) & 255) as f32 / 255.0, g: ((hex >> 8) & 255) as f32 / 255.0, b: (hex & 255) as f32 / 255.0, a: 1.0 };
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: c(0x4FD8FF) },
            D2D1_GRADIENT_STOP { position: 0.5, color: c(0x7C6BFF) },
            D2D1_GRADIENT_STOP { position: 1.0, color: c(0xFF5FA2) },
        ];
        let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP).ok()?;
        let grad = rt.CreateLinearGradientBrush(&D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: p(10.0, 0.0), endPoint: p(90.0, 0.0) }, None, &coll).ok()?;
        let sp = D2D1_STROKE_STYLE_PROPERTIES { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, dashCap: D2D1_CAP_STYLE_ROUND, lineJoin: D2D1_LINE_JOIN_ROUND, miterLimit: 10.0, ..Default::default() };
        let stroke = factory.CreateStrokeStyle(&sp, None).ok()?;
        rt.DrawGeometry(&geo, &grad, (11.0 * k).max(1.6), &stroke);
        rt.EndDraw(None, None).ok()?;
        SelectObject(dc, old);
        let _ = DeleteDC(dc);
        // Icons want straight alpha: un-premultiply.
        let px = std::slice::from_raw_parts_mut(bits as *mut u8, (n * n * 4) as usize);
        for p in px.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a > 0 && a < 255 {
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        let mask = CreateBitmap(n, n, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: dib };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(dib.into());
        icon
    }
}
