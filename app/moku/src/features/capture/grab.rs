//! Screen grabbing with GDI, window bounds from DWM, and the pure helpers around saving
//! (default folder, `%VAR%` expansion, file names). Runs on a worker thread; nothing here touches
//! the UI.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{FOLDERID_Pictures, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows::Win32::UI::WindowsAndMessaging::*;

/// A grabbed rectangle: top-down BGRA, alpha forced to 255 (screens have no transparency).
pub struct Grab {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

/// Refuse to allocate more than this for one grab (a 4K screen is 33 MB; 8K is 133 MB).
const MAX_GRAB_BYTES: u64 = 192 * 1024 * 1024;

/// Is a grab of `w`×`h` pixels reasonable?
pub fn size_ok(w: i64, h: i64) -> bool {
    w > 0 && h > 0 && (w as u64) * (h as u64) * 4 <= MAX_GRAB_BYTES
}

/// Copy `r` (physical pixels, virtual-desktop coordinates) from the screen. The caller hides the
/// island first and waits a few frames. `None` on any GDI failure or an unreasonable size.
pub fn grab_rect(r: RECT) -> Option<Grab> {
    let (w, h) = ((r.right - r.left) as i64, (r.bottom - r.top) as i64);
    if !size_ok(w, h) {
        return None;
    }
    let (w, h) = (w as i32, h as i32);
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let mut result = None;
        if !mem.is_invalid() && !bmp.is_invalid() {
            let old = SelectObject(mem, bmp.into());
            // CAPTUREBLT includes layered windows (many apps use them).
            if BitBlt(mem, 0, 0, w, h, Some(screen), r.left, r.top, SRCCOPY | CAPTUREBLT).is_ok() {
                let mut bi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        biHeight: -h, // top-down
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
                let got = GetDIBits(mem, bmp, 0, h as u32, Some(buf.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS);
                if got == h {
                    for px in buf.chunks_exact_mut(4) {
                        px[3] = 255;
                    }
                    result = Some(Grab { w: w as u32, h: h as u32, bgra: buf });
                }
            }
            SelectObject(mem, old);
        }
        if !bmp.is_invalid() {
            let _ = DeleteObject(bmp.into());
        }
        if !mem.is_invalid() {
            let _ = DeleteDC(mem);
        }
        ReleaseDC(None, screen);
        result
    }
}

/// The monitor rectangle of the monitor `hwnd` is on (the island's monitor).
pub fn monitor_rect_of(hwnd: HWND) -> Option<RECT> {
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        GetMonitorInfoW(mon, &mut info).as_bool().then_some(info.rcMonitor)
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

/// Is this a window that is not "a window the user was working in" (desktop, taskbar)?
fn is_shell_window(hwnd: HWND) -> bool {
    matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

/// Rectangle of the window the user was in before they used the island: the foreground window
/// (the island's windows are never activated, so it is still the foreground window), as DWM's
/// extended frame bounds, which exclude the invisible resize border and shadow. `None` when there
/// is no such window (desktop, minimised, or one of our own).
pub fn foreground_window_rect() -> Option<RECT> {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || !IsWindowVisible(fg).as_bool() || IsIconic(fg).as_bool() || is_shell_window(fg) {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return None; // never capture the island itself
        }
        let mut r = RECT::default();
        if DwmGetWindowAttribute(fg, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut c_void, size_of::<RECT>() as u32).is_err() && GetWindowRect(fg, &mut r).is_err() {
            return None;
        }
        clip_to_desktop(r)
    }
}

/// Intersect with the virtual desktop (a window dragged half off-screen).
fn clip_to_desktop(r: RECT) -> Option<RECT> {
    let (vx, vy, vw, vh) = unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN), GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)) };
    intersect(r, RECT { left: vx, top: vy, right: vx + vw, bottom: vy + vh })
}

pub fn intersect(a: RECT, b: RECT) -> Option<RECT> {
    let r = RECT { left: a.left.max(b.left), top: a.top.max(b.top), right: a.right.min(b.right), bottom: a.bottom.min(b.bottom) };
    (r.right > r.left && r.bottom > r.top).then_some(r)
}

// ---- where files go -----------------------------------------------------------------------------

/// `%USERPROFILE%`-style expansion for the `save_dir` setting. Unknown variables are left as typed.
pub fn expand_env(s: &str, get: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        match tail.find('%') {
            Some(j) if j > 0 && !tail[..j].contains(['\\', '/', ' ']) => match get(&tail[..j]) {
                Some(v) => {
                    out.push_str(&v);
                    rest = &tail[j + 1..];
                }
                None => {
                    out.push('%');
                    rest = tail;
                }
            },
            _ => {
                out.push('%');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The folder screenshots are saved to: the `save_dir` setting, or `Pictures\Moku` when empty.
pub fn resolve_save_dir(setting: &str, pictures: &Path, env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let s = setting.trim();
    if s.is_empty() { pictures.join("Moku") } else { PathBuf::from(expand_env(s, env)) }
}

/// The user's Pictures folder (`SHGetKnownFolderPath`, falling back to `%USERPROFILE%\Pictures`).
pub fn pictures_dir() -> PathBuf {
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None) {
            let s = p.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Pictures")).unwrap_or_else(std::env::temp_dir)
}

pub fn save_dir(setting: &str) -> PathBuf {
    resolve_save_dir(setting, &pictures_dir(), |k| std::env::var(k).ok())
}

/// `Moku 2026-11-24 10.15.30.png`, with ` (2)`, ` (3)` … when that name is taken.
pub fn unique_file_name(stamp: &str, taken: impl Fn(&str) -> bool) -> String {
    let first = format!("Moku {stamp}.png");
    if !taken(&first) {
        return first;
    }
    (2..1000).map(|n| format!("Moku {stamp} ({n}).png")).find(|n| !taken(n)).unwrap_or_else(|| format!("Moku {stamp} (x).png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_environment_variables_in_the_save_dir() {
        let env = |k: &str| match k {
            "USERPROFILE" => Some(r"C:\Users\ana".to_string()),
            _ => None,
        };
        assert_eq!(expand_env(r"%USERPROFILE%\Pictures\Moku", env), r"C:\Users\ana\Pictures\Moku");
        assert_eq!(expand_env(r"%NOPE%\x", env), r"%NOPE%\x");
        assert_eq!(expand_env(r"D:\100%\done", env), r"D:\100%\done", "a lone percent sign stays");
        assert_eq!(expand_env("%%", env), "%%");
        assert_eq!(expand_env("", env), "");
        assert_eq!(expand_env(r"%USERPROFILE%", env), r"C:\Users\ana");
    }

    #[test]
    fn default_save_dir_is_pictures_moku() {
        let pics = Path::new(r"C:\Users\ana\Pictures");
        let none = |_: &str| None;
        assert_eq!(resolve_save_dir("", pics, none), Path::new(r"C:\Users\ana\Pictures\Moku"));
        assert_eq!(resolve_save_dir("   ", pics, none), Path::new(r"C:\Users\ana\Pictures\Moku"));
        assert_eq!(resolve_save_dir(r"E:\shots", pics, none), Path::new(r"E:\shots"));
        // The real lookup returns an absolute folder named like the user's Pictures.
        assert!(pictures_dir().is_absolute());
    }

    #[test]
    fn file_names_never_overwrite() {
        let taken = ["Moku 2026-11-24 10.15.30.png", "Moku 2026-11-24 10.15.30 (2).png"];
        assert_eq!(unique_file_name("2026-11-24 10.15.30", |n| taken.contains(&n)), "Moku 2026-11-24 10.15.30 (3).png");
        assert_eq!(unique_file_name("2026-11-24 10.15.31", |n| taken.contains(&n)), "Moku 2026-11-24 10.15.31.png");
    }

    #[test]
    fn grab_size_guard() {
        assert!(size_ok(1920, 1080));
        assert!(size_ok(3840, 2160));
        assert!(!size_ok(0, 100));
        assert!(!size_ok(-5, 100));
        assert!(!size_ok(30_000, 30_000));
    }

    #[test]
    fn rectangles_are_clipped_to_the_desktop() {
        let a = RECT { left: -100, top: -50, right: 300, bottom: 200 };
        let b = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let r = intersect(a, b).unwrap();
        assert_eq!((r.left, r.top, r.right, r.bottom), (0, 0, 300, 200));
        assert!(intersect(RECT { left: 2000, top: 0, right: 2100, bottom: 50 }, b).is_none());
    }

    /// A real BitBlt of a tiny corner of the screen (kept in memory, never saved or shown).
    #[test]
    fn grabs_a_small_screen_region() {
        let g = grab_rect(RECT { left: 0, top: 0, right: 16, bottom: 8 }).expect("grab");
        assert_eq!((g.w, g.h), (16, 8));
        assert_eq!(g.bgra.len(), 16 * 8 * 4);
        assert!(g.bgra.chunks_exact(4).all(|p| p[3] == 255), "alpha is forced opaque");
        assert!(grab_rect(RECT { left: 0, top: 0, right: 0, bottom: 10 }).is_none());
    }

    #[test]
    fn the_island_process_never_captures_itself() {
        // The test process owns no foreground window, so this is simply "something or nothing":
        // it must never panic and never return an empty rectangle.
        if let Some(r) = foreground_window_rect() {
            assert!(r.right > r.left && r.bottom > r.top);
        }
    }
}
