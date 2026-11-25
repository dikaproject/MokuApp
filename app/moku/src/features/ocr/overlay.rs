//! The region selector: a full-screen window of our own, over a frozen picture of the monitor the
//! user was on. A dimmed copy of the picture is drawn everywhere except inside the rectangle the
//! user drags, which shows the picture at full brightness with a thin accent border and its size.
//! Esc, a right click, a click elsewhere (the window loses the foreground) or a minute without
//! action cancel it.
//!
//! The window is ours and ordinary: it gets its mouse and keyboard messages like any window, and
//! nothing is ever sent to another one. The frozen picture lives in two GDI bitmaps (bright, dim)
//! for as long as the overlay is open and is freed when it closes; the only thing that leaves is
//! the cropped region the user chose, handed to the callback.
//!
//! One overlay at a time, on the UI thread (the window procedure runs there).

use std::cell::RefCell;
use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::region::{self, Rect};

const CLASS: PCWSTR = w!("MokuOcrOverlay");
const TIMER_IDLE: usize = 1;
/// Retries of `SetForegroundWindow` while the overlay is not the foreground window.
const TIMER_FOCUS: usize = 2;
/// Esc is looked at while the overlay has no keyboard focus.
const TIMER_ESC: usize = 3;
/// The overlay closes itself after this long without a selection.
const IDLE_MS: u32 = 60_000;
/// Every retry to become the foreground window, ms, and how many (about half a second in all).
const FOCUS_RETRY_MS: u32 = 50;
const FOCUS_TRIES: u32 = 10;
/// How often Esc is looked at, ms, and the grace after the overlay opened (a key still held from the
/// hotkey must not close it at once).
const ESC_POLL_MS: u32 = 40;
const ESC_GRACE_MS: u32 = 250;
/// The picture outside the selection is drawn at this share of its brightness.
const DIM_PERCENT: u32 = 42;
/// Selection border, `0xRRGGBB` (the island's blue).
const ACCENT: u32 = 0x0A84FF;

/// A frozen picture: top-down BGRA, `w` x `h`.
pub struct Frozen {
    pub w: i32,
    pub h: i32,
    pub bgra: Vec<u8>,
}

/// How the selection ended.
pub enum Outcome {
    /// The chosen rectangle (picture coordinates) and its pixels (top-down BGRA, `rect.w` x `rect.h`).
    Region(Rect, Vec<u8>),
    Cancelled,
}

/// Called once, on the UI thread, when the overlay closes.
pub type Done = Box<dyn FnOnce(Outcome)>;

/// The texts the overlay draws (the feature provides them in the right language).
pub struct Labels {
    pub hint: String,
}

struct Overlay {
    hwnd: HWND,
    /// Dots per inch of the monitor (96 = 100 %): sizes of the border, pills and font.
    dpi: i32,
    w: i32,
    h: i32,
    dc_bright: HDC,
    dc_dim: HDC,
    bmp_bright: HBITMAP,
    bmp_dim: HBITMAP,
    old_bright: HGDIOBJ,
    old_dim: HGDIOBJ,
    /// The pixels of the bright bitmap (valid while it lives): what a region is cropped from.
    bits: *const u8,
    font: HFONT,
    hint: Vec<u16>,
    hint_rc: RECT,
    /// Drag start and current point, client coordinates.
    drag: Option<((i32, i32), (i32, i32))>,
    dragging: bool,
    done: Option<Done>,
}

thread_local! {
    static STATE: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

/// Is an overlay on screen?
pub fn is_open() -> bool {
    STATE.with(|s| s.borrow().is_some())
}

fn dim_copy(src: &[u8]) -> Vec<u8> {
    src.iter().enumerate().map(|(i, &c)| if i % 4 == 3 { 255 } else { (c as u32 * DIM_PERCENT / 100) as u8 }).collect()
}

/// A 32-bit top-down DIB section holding `bgra`; returns the bitmap and the pointer to its pixels.
unsafe fn make_dib(dc: HDC, w: i32, h: i32, bgra: &[u8]) -> Option<(HBITMAP, *mut u8)> {
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(bmp.into());
            return None;
        }
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u8, bgra.len().min((w * h * 4) as usize));
        Some((bmp, bits as *mut u8))
    }
}

/// Both values, or none: when only one exists it is handed to `free` (a half-made pair must not leak the
/// half that was made).
fn both<T>(a: Option<T>, b: Option<T>, mut free: impl FnMut(T)) -> Option<(T, T)> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a, b)),
        (a, b) => {
            a.into_iter().chain(b).for_each(&mut free);
            None
        }
    }
}

fn colorref(rgb: u32) -> COLORREF {
    COLORREF(((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff))
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Show the overlay over the screen rectangle `at` (physical pixels; the monitor the picture was
/// taken from) with the frozen picture `frozen` (which must be `at`'s size). `done` is called
/// when it closes. False (and `done` not called) when it could not be created or one is open.
pub fn show(at: RECT, frozen: Frozen, labels: Labels, done: Done) -> bool {
    if is_open() || frozen.w <= 0 || frozen.h <= 0 || frozen.bgra.len() < (frozen.w * frozen.h * 4) as usize {
        return false;
    }
    unsafe {
        let Ok(inst) = GetModuleHandleW(None) else { return false };
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: CLASS, ..Default::default() };
        RegisterClassW(&wc); // fails harmlessly when already registered
        let Ok(hwnd) = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, CLASS, w!(""), WS_POPUP, at.left, at.top, frozen.w, frozen.h, None, None, Some(inst.into()), None) else { return false };
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let Some(ov) = Overlay::build(hwnd, &frozen, &labels.hint, dpi, Some(done)) else {
            let _ = DestroyWindow(hwnd);
            return false;
        };
        STATE.with(|s| *s.borrow_mut() = Some(ov));
        // The frozen picture of the Windows desktop stays in memory only while this window lives.
        drop(frozen);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        // Windows may refuse the foreground (another process holds the foreground lock). Then the overlay
        // would get no key, and Esc could not close it: try again for a moment, and meanwhile read the Esc
        // key's state (nothing else, no hook) so Esc closes it whoever has the focus.
        FOCUS_LEFT.set(FOCUS_TRIES);
        OPENED_AT.set(Some(std::time::Instant::now()));
        SetTimer(Some(hwnd), TIMER_FOCUS, FOCUS_RETRY_MS, None);
        SetTimer(Some(hwnd), TIMER_ESC, ESC_POLL_MS, None);
        SetTimer(Some(hwnd), TIMER_IDLE, IDLE_MS, None);
        let _ = InvalidateRect(Some(hwnd), None, false);
        true
    }
}

/// A pill of `text` centred on `cx` with its top at `top`: its rectangle.
unsafe fn measure_pill(dc: HDC, font: HFONT, text: &[u16], cx: i32, top: i32, dpi: i32) -> RECT {
    unsafe {
        let old = SelectObject(dc, font.into());
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(dc, text, &mut size);
        SelectObject(dc, old);
        let (padx, pady) = (14 * dpi / 96, 7 * dpi / 96);
        RECT { left: cx - size.cx / 2 - padx, top, right: cx + size.cx / 2 + padx, bottom: top + size.cy + 2 * pady }
    }
}

impl Overlay {
    /// The GDI side: both bitmaps (bright, and a dimmed copy), the font and the hint pill's place.
    /// `hwnd` is null in offscreen tests. `None` when a bitmap could not be made.
    unsafe fn build(hwnd: HWND, frozen: &Frozen, hint: &str, dpi: i32, done: Option<Done>) -> Option<Overlay> {
        unsafe {
            let screen = GetDC(None);
            let (dc_bright, dc_dim) = (CreateCompatibleDC(Some(screen)), CreateCompatibleDC(Some(screen)));
            let dim = dim_copy(&frozen.bgra);
            let (b, d) = (make_dib(screen, frozen.w, frozen.h, &frozen.bgra), make_dib(screen, frozen.w, frozen.h, &dim));
            drop(dim);
            ReleaseDC(None, screen);
            // Both bitmaps or neither: when only one could be made, that one is deleted too (it used to leak).
            let Some(((bmp_bright, bits), (bmp_dim, _))) = both(b, d, |(bmp, _)| {
                let _ = DeleteObject(bmp.into());
            }) else {
                let _ = DeleteDC(dc_bright);
                let _ = DeleteDC(dc_dim);
                return None;
            };
            let (old_bright, old_dim) = (SelectObject(dc_bright, bmp_bright.into()), SelectObject(dc_dim, bmp_dim.into()));
            let font = CreateFontW(-(15 * dpi / 96), 0, 0, 0, FW_SEMIBOLD.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32, w!("Segoe UI"));
            let hint = utf16(hint);
            let hint_rc = measure_pill(dc_bright, font, &hint, frozen.w / 2, 24 * dpi / 96, dpi);
            Some(Overlay { hwnd, dpi, w: frozen.w, h: frozen.h, dc_bright, dc_dim, bmp_bright, bmp_dim, old_bright, old_dim, bits, font, hint, hint_rc, drag: None, dragging: false, done })
        }
    }

    fn selection(&self) -> Option<Rect> {
        let (a, b) = self.drag?;
        let r = Rect { x: a.0.min(b.0).clamp(0, self.w), y: a.1.min(b.1).clamp(0, self.h), w: 0, h: 0 };
        let (x1, y1) = (a.0.max(b.0).clamp(0, self.w), a.1.max(b.1).clamp(0, self.h));
        let r = Rect { w: x1 - r.x, h: y1 - r.y, ..r };
        (r.w > 0 && r.h > 0).then_some(r)
    }

    /// Paint `dirty` (client coordinates) through a back buffer so dragging does not flicker.
    unsafe fn draw(&self, hdc: HDC, dirty: RECT) {
        unsafe {
            let (dw, dh) = (dirty.right - dirty.left, dirty.bottom - dirty.top);
            if dw <= 0 || dh <= 0 {
                return;
            }
            let back = CreateCompatibleDC(Some(hdc));
            let bmp = CreateCompatibleBitmap(hdc, dw, dh);
            let old = SelectObject(back, bmp.into());
            self.paint_into(back, dirty);
            let _ = BitBlt(hdc, dirty.left, dirty.top, dw, dh, Some(back), 0, 0, SRCCOPY);
            SelectObject(back, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(back);
        }
    }

    /// Compose `dirty` (client coordinates) into `back`, a bitmap of that size: the dimmed picture,
    /// the bright selection with its border and size, the hint.
    unsafe fn paint_into(&self, back: HDC, dirty: RECT) {
        unsafe {
            let (dw, dh) = (dirty.right - dirty.left, dirty.bottom - dirty.top);
            let _ = BitBlt(back, 0, 0, dw, dh, Some(self.dc_dim), dirty.left, dirty.top, SRCCOPY);
            let dpi = self.dpi;
            if let Some(sel) = self.selection() {
                let (l, t, r, b) = (sel.x.max(dirty.left), sel.y.max(dirty.top), (sel.x + sel.w).min(dirty.right), (sel.y + sel.h).min(dirty.bottom));
                if r > l && b > t {
                    let _ = BitBlt(back, l - dirty.left, t - dirty.top, r - l, b - t, Some(self.dc_bright), l, t, SRCCOPY);
                }
                // A border outside the selection, 2 px (scaled), so it never covers the text inside.
                let brush = CreateSolidBrush(colorref(ACCENT));
                let bw = (2 * dpi / 96).max(2);
                for i in 1..=bw {
                    let rc = RECT { left: sel.x - i - dirty.left, top: sel.y - i - dirty.top, right: sel.x + sel.w + i - dirty.left, bottom: sel.y + sel.h + i - dirty.top };
                    FrameRect(back, &rc, brush);
                }
                let _ = DeleteObject(brush.into());
                let label = utf16(&format!("{} \u{d7} {}", sel.w, sel.h));
                self.pill(back, dirty, &label, sel.x, if sel.y > 40 * dpi / 96 { sel.y - 34 * dpi / 96 } else { sel.y + sel.h + 8 * dpi / 96 }, false);
            }
            // The hint goes away once a drag begins, so it never covers the text being selected.
            if self.drag.is_none() {
                let hc = (self.hint_rc.left + self.hint_rc.right) / 2;
                self.pill(back, dirty, &self.hint, hc, self.hint_rc.top, true);
            }
        }
    }

    /// A dark rounded pill with white text, in client coordinates, drawn into `back` (which shows
    /// `dirty`). `centre`: `x` is the middle of the pill, else its left edge.
    unsafe fn pill(&self, back: HDC, dirty: RECT, text: &[u16], x: i32, top: i32, centre: bool) {
        unsafe {
            let dpi = self.dpi;
            let old_font = SelectObject(back, self.font.into());
            let mut size = SIZE::default();
            let _ = GetTextExtentPoint32W(back, text, &mut size);
            let (padx, pady) = (14 * dpi / 96, 7 * dpi / 96);
            let width = size.cx + 2 * padx;
            let left = if centre { x - width / 2 } else { x };
            let rc = RECT { left, top, right: left + width, bottom: top + size.cy + 2 * pady };
            if rc.right > dirty.left && rc.left < dirty.right && rc.bottom > dirty.top && rc.top < dirty.bottom {
                let brush = CreateSolidBrush(colorref(0x1C1C22));
                let pen = CreatePen(PS_SOLID, 1, colorref(0x44444C));
                let (ob, op) = (SelectObject(back, brush.into()), SelectObject(back, pen.into()));
                let _ = RoundRect(back, rc.left - dirty.left, rc.top - dirty.top, rc.right - dirty.left, rc.bottom - dirty.top, 2 * pady + size.cy, 2 * pady + size.cy);
                SelectObject(back, ob);
                SelectObject(back, op);
                let _ = DeleteObject(brush.into());
                let _ = DeleteObject(pen.into());
                SetBkMode(back, TRANSPARENT);
                SetTextColor(back, colorref(0xFFFFFF));
                let _ = TextOutW(back, rc.left + padx - dirty.left, rc.top + pady - dirty.top, text);
            }
            SelectObject(back, old_font);
        }
    }

    /// What must be repainted when the selection goes from `old` to `new` (labels included).
    fn damage(&self, old: Option<Rect>, new: Option<Rect>) -> RECT {
        let mut r = RECT { left: i32::MAX, top: i32::MAX, right: i32::MIN, bottom: i32::MIN };
        for s in [old, new].into_iter().flatten() {
            r.left = r.left.min(s.x - 12);
            r.top = r.top.min(s.y - 60);
            r.right = r.right.max(s.x + s.w + 12);
            r.bottom = r.bottom.max(s.y + s.h + 60);
        }
        if r.left == i32::MAX {
            return RECT { left: 0, top: 0, right: self.w, bottom: self.h };
        }
        RECT { left: r.left.max(0), top: r.top.max(0), right: r.right.min(self.w), bottom: r.bottom.min(self.h) }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc_bright, self.old_bright);
            SelectObject(self.dc_dim, self.old_dim);
            let _ = DeleteObject(self.bmp_bright.into());
            let _ = DeleteObject(self.bmp_dim.into());
            let _ = DeleteDC(self.dc_bright);
            let _ = DeleteDC(self.dc_dim);
            let _ = DeleteObject(self.font.into());
        }
    }
}

/// Close the overlay and report how it ended. The state is taken out first, so the messages the
/// window sends while it is destroyed find nothing to touch.
fn finish(outcome: impl FnOnce(&Overlay) -> Outcome) {
    let Some(mut ov) = STATE.with(|s| s.borrow_mut().take()) else { return };
    let outcome = outcome(&ov);
    let done = ov.done.take();
    unsafe {
        let _ = KillTimer(Some(ov.hwnd), TIMER_IDLE);
        let _ = KillTimer(Some(ov.hwnd), TIMER_FOCUS);
        let _ = KillTimer(Some(ov.hwnd), TIMER_ESC);
        let _ = ReleaseCapture();
        let _ = DestroyWindow(ov.hwnd);
    }
    // The frozen picture is freed here, before the callback runs.
    drop(ov);
    if let Some(done) = done {
        done(outcome);
    }
}

fn cancel() {
    finish(|_| Outcome::Cancelled);
}

/// Close the overlay as if the user pressed Esc (app exit, the feature switched off).
pub fn close() {
    cancel();
}

/// End the drag at the current rectangle: the region, or (too small) keep waiting.
fn accept() {
    let sel = STATE.with(|s| s.borrow().as_ref().and_then(|o| o.drag.map(|(a, b)| (a, b, o.w, o.h))));
    let Some((a, b, w, h)) = sel else { return };
    match region::from_drag(a, b, w, h) {
        Some(r) => finish(|o| {
            // The pixels come from the bright bitmap, which is still alive here.
            let all = unsafe { std::slice::from_raw_parts(o.bits, (o.w * o.h * 4) as usize) };
            match region::crop_bgra(all, o.w as usize, o.h as usize, r) {
                Some(px) => Outcome::Region(r, px),
                None => Outcome::Cancelled,
            }
        }),
        None => {
            // A click or a twitch: forget it and let the user drag again.
            let hwnd = STATE.with(|s| {
                s.borrow_mut().as_mut().map(|o| {
                    o.drag = None;
                    o.dragging = false;
                    o.hwnd
                })
            });
            if let Some(h) = hwnd {
                unsafe {
                    let _ = ReleaseCapture();
                    let _ = InvalidateRect(Some(h), None, false);
                }
            }
        }
    }
}

/// Debug (`MOKU_DEBUG` only): choose `r` (picture coordinates) as if the user had dragged it.
/// False when no overlay is open.
pub fn debug_select(r: Rect) -> bool {
    let ok = STATE.with(|s| {
        s.borrow_mut().as_mut().map(|o| {
            o.drag = Some(((r.x, r.y), (r.x + r.w, r.y + r.h)));
        })
    });
    if ok.is_none() {
        return false;
    }
    accept();
    true
}

/// The selection rectangle now (for tests / debug state).
pub fn debug_selection() -> Option<Rect> {
    STATE.with(|s| s.borrow().as_ref().and_then(|o| o.selection()))
}

thread_local! {
    /// Retries of `SetForegroundWindow` left for the overlay that is open.
    static FOCUS_LEFT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// When the open overlay was shown.
    static OPENED_AT: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

fn point(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                STATE.with(|s| {
                    if let Some(o) = s.borrow().as_ref() {
                        o.draw(hdc, ps.rcPaint);
                    }
                });
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_SETCURSOR => {
                if let Ok(c) = LoadCursorW(None, IDC_CROSS) {
                    SetCursor(Some(c));
                }
                LRESULT(1)
            }
            WM_LBUTTONDOWN => {
                let p = point(lp);
                STATE.with(|s| {
                    if let Some(o) = s.borrow_mut().as_mut() {
                        o.drag = Some((p, p));
                        o.dragging = true;
                    }
                });
                // Wipe the hint (it is not drawn once a drag has begun).
                let hint = STATE.with(|s| s.borrow().as_ref().map(|o| o.hint_rc));
                if let Some(rc) = hint {
                    let _ = InvalidateRect(Some(hwnd), Some(&rc), false);
                }
                SetCapture(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let p = point(lp);
                let damage = STATE.with(|s| {
                    let mut s = s.borrow_mut();
                    let o = s.as_mut()?;
                    if !o.dragging {
                        return None;
                    }
                    let old = o.selection();
                    if let Some((a, _)) = o.drag {
                        o.drag = Some((a, p));
                    }
                    Some(o.damage(old, o.selection()))
                });
                if let Some(rc) = damage {
                    let _ = InvalidateRect(Some(hwnd), Some(&rc), false);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let was = STATE.with(|s| s.borrow_mut().as_mut().map(|o| std::mem::replace(&mut o.dragging, false)));
                if was == Some(true) {
                    let p = point(lp);
                    STATE.with(|s| {
                        if let Some(o) = s.borrow_mut().as_mut()
                            && let Some((a, _)) = o.drag
                        {
                            o.drag = Some((a, p));
                        }
                    });
                    accept();
                }
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                cancel();
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 as u32 == VK_ESCAPE.0 as u32 => {
                cancel();
                LRESULT(0)
            }
            // The user went elsewhere (another window took the foreground).
            WM_ACTIVATE if (wp.0 & 0xFFFF) == 0 => {
                cancel();
                LRESULT(0)
            }
            // Not the foreground window yet: ask again, a few times.
            WM_TIMER if wp.0 == TIMER_FOCUS => {
                let left = FOCUS_LEFT.get();
                if GetForegroundWindow() == hwnd || left == 0 {
                    let _ = KillTimer(Some(hwnd), TIMER_FOCUS);
                } else {
                    FOCUS_LEFT.set(left - 1);
                    let _ = SetForegroundWindow(hwnd);
                    let _ = SetFocus(Some(hwnd));
                }
                LRESULT(0)
            }
            // Esc, read as a key state: works when the overlay never got the keyboard.
            WM_TIMER if wp.0 == TIMER_ESC => {
                let grace_over = OPENED_AT.get().is_none_or(|t| t.elapsed().as_millis() as u32 >= ESC_GRACE_MS);
                if grace_over && GetForegroundWindow() != hwnd && GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8000 != 0 {
                    cancel();
                }
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TIMER_IDLE => {
                cancel();
                LRESULT(0)
            }
            WM_CLOSE => {
                cancel();
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::ocr::sample;

    #[test]
    fn a_half_made_pair_frees_the_half_that_exists() {
        let mut freed = vec![];
        assert_eq!(both(Some(1), Some(2), |x| freed.push(x)), Some((1, 2)));
        assert!(freed.is_empty(), "nothing is freed when both exist");
        assert_eq!(both(Some(1), None, |x| freed.push(x)), None);
        assert_eq!(both(None, Some(2), |x| freed.push(x)), None);
        assert_eq!(both::<i32>(None, None, |x| freed.push(x)), None);
        assert_eq!(freed, [1, 2], "the lone bitmap is deleted, whichever one it was");
    }

    #[test]
    fn building_and_dropping_the_overlay_leaks_no_gdi_objects() {
        use windows::Win32::System::Threading::{GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources};
        let frozen = Frozen { w: 64, h: 48, bgra: vec![200; 64 * 48 * 4] };
        let count = || unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        // Warm up (the first font / DC creation may cache something), then measure a few rounds.
        drop(unsafe { Overlay::build(HWND::default(), &frozen, "x", 96, None) });
        let before = count();
        for _ in 0..5 {
            drop(unsafe { Overlay::build(HWND::default(), &frozen, "x", 96, None) });
        }
        assert!(count() <= before + 1, "GDI objects before {before}, after {}", count());
    }

    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    /// Compose the overlay for a drag into an off-screen bitmap and return its BGRA pixels. Nothing
    /// is shown on any screen and nothing of the real desktop is involved.
    fn compose(frozen: &Frozen, drag: Option<((i32, i32), (i32, i32))>, dpi: i32) -> Vec<u8> {
        unsafe {
            let mut ov = Overlay::build(HWND::default(), frozen, "Seret untuk memilih teks  \u{b7}  Esc untuk batal", dpi, None).expect("overlay bitmaps");
            ov.drag = drag;
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let (bmp, bits) = make_dib(screen, frozen.w, frozen.h, &vec![0; (frozen.w * frozen.h * 4) as usize]).expect("target");
            ReleaseDC(None, screen);
            let old = SelectObject(dc, bmp.into());
            ov.paint_into(dc, RECT { left: 0, top: 0, right: frozen.w, bottom: frozen.h });
            let out = std::slice::from_raw_parts(bits, (frozen.w * frozen.h * 4) as usize).to_vec();
            SelectObject(dc, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(dc);
            out
        }
    }

    fn px(img: &[u8], w: i32, x: i32, y: i32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2]]
    }

    #[test]
    fn the_overlay_dims_everything_except_the_selection() {
        let (w, h) = (960, 420);
        let frozen = Frozen { w, h, bgra: sample::sample_screen(w as u32, h as u32) };
        let out = compose(&frozen, Some(((40, 30), (640, 130))), 96);
        // Background is #F4F6FA (BGRA F4 F6 FA reversed: B=FA, G=F6, R=F4).
        let page = [0xFA, 0xF6, 0xF4];
        assert_eq!(px(&frozen.bgra, w, 900, 380), page, "the sample page is the expected colour");
        let dim = px(&out, w, 900, 380);
        assert!(dim.iter().zip(page).all(|(d, p)| (*d as u32) * 100 / 42 + 3 >= p as u32 && (*d as u32) < p as u32 / 2), "dimmed to about 42 %: {dim:?}");
        // Inside the selection the picture is untouched, down to the pixel.
        assert_eq!(px(&out, w, 45, 35), page);
        assert_eq!(px(&out, w, 600, 120), page);
        // The border is the accent colour (#0A84FF) just outside the rectangle.
        assert_eq!(px(&out, w, 39, 80), [0xFF, 0x84, 0x0A]);
        assert_eq!(px(&out, w, 300, 29), [0xFF, 0x84, 0x0A]);
        // While a drag exists the hint is gone (it would cover the text being selected): where it
        // would be, inside the selection, the page shows.
        assert_eq!(px(&out, w, w / 2 - 150, 33), page);
        // Before the drag it sits at the top, in its dark fill.
        let idle = compose(&frozen, None, 96);
        let hint = px(&idle, w, w / 2 - 150, 24 + 14);
        assert!(hint.iter().all(|c| *c < 0x60), "a dark pill under the hint: {hint:?}");
    }

    #[test]
    fn without_a_selection_the_whole_picture_is_dim() {
        let (w, h) = (400, 200);
        let frozen = Frozen { w, h, bgra: sample::text_image(w as u32, h as u32, 0xFFFFFF, 0, 20, &[]) };
        let out = compose(&frozen, None, 96);
        let c = px(&out, w, 200, 150);
        assert!(c.iter().all(|v| (100..=112).contains(v)), "white at 42 % is about 107: {c:?}");
    }

    /// The pixels the callback gets are exactly the chosen rectangle of the frozen picture.
    #[test]
    fn the_bright_bitmap_hands_back_the_exact_pixels() {
        let (w, h) = (960, 420);
        let frozen = Frozen { w, h, bgra: sample::sample_screen(w as u32, h as u32) };
        let r = Rect { x: 37, y: 41, w: 201, h: 63 };
        let want = region::crop_bgra(&frozen.bgra, w as usize, h as usize, r).unwrap();
        unsafe {
            let ov = Overlay::build(HWND::default(), &frozen, "x", 96, None).unwrap();
            let all = std::slice::from_raw_parts(ov.bits, (w * h * 4) as usize);
            let mut got = region::crop_bgra(all, w as usize, h as usize, r).unwrap();
            // The DIB's alpha byte is whatever GDI left; the colour bytes must match.
            let mut want = want;
            for (g, wnt) in got.chunks_exact_mut(4).zip(want.chunks_exact_mut(4)) {
                g[3] = 0;
                wnt[3] = 0;
            }
            assert_eq!(got, want);
        }
    }

    #[test]
    fn selections_normalise_and_clamp() {
        let (w, h) = (100, 80);
        let frozen = Frozen { w, h, bgra: vec![0; (w * h * 4) as usize] };
        unsafe {
            let mut ov = Overlay::build(HWND::default(), &frozen, "x", 96, None).unwrap();
            assert_eq!(ov.selection(), None);
            ov.drag = Some(((60, 50), (10, 20)));
            assert_eq!(ov.selection(), Some(Rect { x: 10, y: 20, w: 50, h: 30 }));
            ov.drag = Some(((-30, -30), (500, 500)));
            assert_eq!(ov.selection(), Some(Rect { x: 0, y: 0, w: 100, h: 80 }));
            ov.drag = Some(((5, 5), (5, 40)));
            assert_eq!(ov.selection(), None, "no width");
            // The damage rectangle stays inside the window and covers old and new.
            let d = ov.damage(Some(Rect { x: 10, y: 10, w: 20, h: 20 }), Some(Rect { x: 40, y: 30, w: 30, h: 30 }));
            assert!(d.left >= 0 && d.top >= 0 && d.right <= w && d.bottom <= h);
            assert!(d.left <= 10 && d.top <= 10 && d.right >= 70 && d.bottom >= 60);
        }
    }

    /// Regenerates the overlay picture for the results folder:
    /// `$env:MOKU_SHOT_DIR = "<repo>\prototype\results\v03"; cargo test -p moku overlay_picture -- --ignored`
    /// (a synthetic page, composed off-screen).
    #[test]
    #[ignore = "writes a picture to MOKU_SHOT_DIR"]
    fn overlay_picture() {
        let Some(dir) = std::env::var_os("MOKU_SHOT_DIR") else { return };
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let (w, h) = (960, 420);
        let frozen = Frozen { w, h, bgra: sample::sample_screen(w as u32, h as u32) };
        let mut out = compose(&frozen, Some(((30, 28), (700, 140))), 96);
        for p in out.chunks_exact_mut(4) {
            p[3] = 255;
        }
        let img = crate::ui::Image::new(w as u32, h as u32, out);
        let path = std::path::Path::new(&dir).join("l-ocr-overlay.png");
        crate::imaging::save_png(&img, &path).expect("save");
    }
}
