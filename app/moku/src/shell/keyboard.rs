//! Keyboard focus for text inputs (`App::focus_input`): the island's hit window normally never
//! activates (`WS_EX_NOACTIVATE`, `MA_NOACTIVATE`). While an input has focus it is made
//! activatable and brought to the foreground â€” allowed right after our own hotkey, which hands
//! the foreground right to this process â€” and gets `WM_KEYDOWN` / `WM_CHAR` / `WM_IME_*`.
//! Releasing focus restores the style and gives the foreground back to the window that had it
//! (only if we still own the foreground: a click elsewhere already moved it on).
//!
//! Nothing here sends input anywhere; it only receives keys sent to our own window.

use std::cell::Cell;

use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Input::Ime::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::app::with_app_or_defer;
use crate::ui::Rect;
use crate::ui::text_edit::{Key, KeyPress};

thread_local! {
    /// An input has keyboard focus (readable while the app is borrowed).
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// High surrogate of a `WM_CHAR` pair waiting for its low half.
    static HIGH: Cell<u16> = const { Cell::new(0) };
}

pub fn active() -> bool {
    ACTIVE.with(|a| a.get())
}

/// Take the keyboard for `hit`. Returns the window that was in the foreground (to give it back
/// later) and whether `hit` really is the foreground window now.
pub fn take(hit: HWND) -> (isize, bool) {
    ACTIVE.with(|a| a.set(true));
    HIGH.with(|h| h.set(0));
    unsafe {
        let prev = GetForegroundWindow();
        let ex = GetWindowLongPtrW(hit, GWL_EXSTYLE);
        SetWindowLongPtrW(hit, GWL_EXSTYLE, ex & !(WS_EX_NOACTIVATE.0 as isize));
        let _ = SetForegroundWindow(hit);
        let _ = SetFocus(Some(hit));
        let ok = GetForegroundWindow() == hit;
        (if prev == hit { 0 } else { prev.0 as isize }, ok)
    }
}

/// Give the keyboard up: the hit window is never-activated again, and the previous foreground
/// window gets it back if we still hold it (`prev` = 0: nobody to give it to).
pub fn release(hit: HWND, prev: isize) {
    ACTIVE.with(|a| a.set(false));
    unsafe {
        let ex = GetWindowLongPtrW(hit, GWL_EXSTYLE);
        SetWindowLongPtrW(hit, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
        if GetForegroundWindow() == hit {
            let p = HWND(prev as _);
            if prev != 0 && IsWindow(Some(p)).as_bool() {
                let _ = SetForegroundWindow(p);
            }
        }
    }
}

/// The foreground window's handle (0 = none).
pub fn foreground() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

/// Let ONE process we started (the app the user picked) take the foreground, and only that one:
/// never "any process" (`ASFW_ANY`), which would let whatever starts next steal the focus.
/// Refuses pid 0 and the `ASFW_ANY` value itself. Returns whether Windows accepted.
pub fn allow_foreground_for(pid: u32) -> bool {
    if pid == 0 || pid == ASFW_ANY {
        return false;
    }
    unsafe { AllowSetForegroundWindow(pid).is_ok() }
}

fn down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

/// AltGr is reported as Ctrl+Alt (Windows adds a fake left Ctrl). Such a press types a character
/// (`@`, `{`, `€` ...) and is never a shortcut: Alt shortcuts count only while Ctrl is NOT down.
/// Returns the `(ctrl, alt)` an input should see.
fn shortcut_mods(ctrl: bool, alt: bool) -> (bool, bool) {
    if ctrl && alt { (false, false) } else { (ctrl, alt) }
}

/// Virtual key â†’ the input's key (None: not one an input cares about).
fn map_key(vk: u16, ctrl: bool, alt: bool) -> Option<Key> {
    Some(match VIRTUAL_KEY(vk) {
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_BACK => Key::Backspace,
        VK_DELETE => Key::Delete,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        VK_TAB => Key::Tab,
        // Alt+F4 on a focused input means "close it".
        VK_F4 if alt => Key::Escape,
        _ if (ctrl || alt) && (vk as u8).is_ascii_alphanumeric() && vk < 0x80 => Key::Char((vk as u8 as char).to_ascii_lowercase()),
        _ => return None,
    })
}

/// Keyboard and IME messages of the hit window while an input has focus.
pub fn on_message(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if !active() {
        return None;
    }
    match msg {
        // A click on the island while it lacks the foreground (activation after the hotkey
        // failed) activates it: a click is the user's own input.
        WM_MOUSEACTIVATE => Some(LRESULT(MA_ACTIVATE as isize)),
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let shift = down(VK_SHIFT);
            let (ctrl, alt) = shortcut_mods(down(VK_CONTROL), msg == WM_SYSKEYDOWN || down(VK_MENU));
            match map_key(wp.0 as u16, ctrl, alt) {
                Some(key) => {
                    let k = KeyPress { key, ctrl, shift, alt };
                    with_app_or_defer(move |a| a.on_input_key(k));
                    Some(LRESULT(0))
                }
                // Alt alone / Alt+other: swallowed, so no menu mode and no beep. (AltGr + a
                // character key is no shortcut: it falls through and arrives as WM_CHAR.)
                None if msg == WM_SYSKEYDOWN && !down(VK_CONTROL) => Some(LRESULT(0)),
                None => None,
            }
        }
        // AltGr + a character key may arrive as WM_SYSCHAR: it is the character, not a menu key.
        WM_SYSCHAR if down(VK_CONTROL) => Some(on_char(wp)),
        WM_SYSKEYUP | WM_SYSCHAR => Some(LRESULT(0)),
        WM_CHAR => Some(on_char(wp)),
        // We draw the composition inline; the IME still shows its candidate list.
        WM_IME_SETCONTEXT => {
            let lp = LPARAM(lp.0 & !(ISC_SHOWUICOMPOSITIONWINDOW as isize));
            Some(unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
        }
        WM_IME_STARTCOMPOSITION => Some(LRESULT(0)),
        WM_IME_COMPOSITION => {
            let flags = lp.0 as u32;
            unsafe {
                let himc = ImmGetContext(hwnd);
                if flags & GCS_RESULTSTR.0 != 0 {
                    let s = comp_string(himc, GCS_RESULTSTR);
                    with_app_or_defer(move |a| a.on_input_commit(&s));
                }
                if flags & GCS_COMPSTR.0 != 0 {
                    let s = comp_string(himc, GCS_COMPSTR);
                    let cur16 = ImmGetCompositionStringW(himc, GCS_CURSORPOS, None, 0).max(0) as usize;
                    let cursor = s.encode_utf16().take(cur16).map(|_| 1).count();
                    // UTF-16 units â†’ byte index.
                    let byte = String::from_utf16_lossy(&s.encode_utf16().take(cursor).collect::<Vec<_>>()).len();
                    with_app_or_defer(move |a| a.on_input_compose(&s, byte));
                }
                let _ = ImmReleaseContext(hwnd, himc);
            }
            Some(LRESULT(0))
        }
        WM_IME_ENDCOMPOSITION => {
            with_app_or_defer(|a| a.on_input_compose("", 0));
            Some(LRESULT(0))
        }
        WM_ACTIVATE => {
            if (wp.0 & 0xffff) as u32 == WA_INACTIVE {
                with_app_or_defer(|a| a.on_input_focus_lost());
            }
            None
        }
        _ => None,
    }
}

/// A typed character (a UTF-16 surrogate pair arrives as two messages and is joined here).
fn on_char(wp: WPARAM) -> LRESULT {
    let u = wp.0 as u16;
    if (0xD800..0xDC00).contains(&u) {
        HIGH.with(|h| h.set(u));
        return LRESULT(0);
    }
    let units: Vec<u16> = if (0xDC00..0xE000).contains(&u) { vec![HIGH.with(|h| h.replace(0)), u] } else { vec![u] };
    let s = String::from_utf16_lossy(&units);
    // Control characters (Ctrl+A = 0x01, Backspace, Enter, Esc) arrive as keys.
    if s.chars().all(|c| !c.is_control()) {
        with_app_or_defer(move |a| a.on_input_text(&s));
    }
    LRESULT(0)
}

unsafe fn comp_string(himc: HIMC, which: IME_COMPOSITION_STRING) -> String {
    unsafe {
        let bytes = ImmGetCompositionStringW(himc, which, None, 0);
        if bytes <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; bytes as usize / 2];
        let got = ImmGetCompositionStringW(himc, which, Some(buf.as_mut_ptr() as *mut _), bytes as u32);
        buf.truncate(got.max(0) as usize / 2);
        String::from_utf16_lossy(&buf)
    }
}

/// Put the IME's composition / candidate window at the caret (`caret` in DIP, relative to the
/// hit window's client area; `scale` = DIP â†’ px).
pub fn place_ime(hwnd: HWND, caret: Rect, scale: f32) {
    unsafe {
        let himc = ImmGetContext(hwnd);
        if himc.is_invalid() {
            return;
        }
        let (x, y) = ((caret.x * scale).round() as i32, (caret.y * scale).round() as i32);
        let bottom = ((caret.y + caret.h) * scale).round() as i32;
        let cf = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: POINT { x, y }, rcArea: RECT::default() };
        let _ = ImmSetCompositionWindow(himc, &cf);
        // Candidates under the caret, never covering it.
        let cand = CANDIDATEFORM { dwIndex: 0, dwStyle: CFS_EXCLUDE, ptCurrentPos: POINT { x, y: bottom }, rcArea: RECT { left: x, top: y, right: x + 2, bottom } };
        let _ = ImmSetCandidateWindow(himc, &cand);
        let _ = ImmReleaseContext(hwnd, himc);
    }
}

struct Open;

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn open(owner: HWND) -> Option<Open> {
    for _ in 0..5 {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            return Some(Open);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    None
}

/// Unicode text on the clipboard (Ctrl+V into an input), at most 64 KB.
pub fn clipboard_text(owner: HWND) -> Option<String> {
    let _open = open(owner)?;
    unsafe {
        let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
        let g = HGLOBAL(h.0);
        let p = GlobalLock(g) as *const u16;
        if p.is_null() {
            return None;
        }
        let n = (GlobalSize(g) / 2).min(32 * 1024);
        let units = std::slice::from_raw_parts(p, n);
        let end = units.iter().position(|&u| u == 0).unwrap_or(n);
        let s = String::from_utf16_lossy(&units[..end]);
        let _ = GlobalUnlock(g);
        Some(s)
    }
}

/// Replace the clipboard with `text` (e.g. the launcher's "Salin").
pub fn set_clipboard_text(owner: HWND, text: &str) -> bool {
    let Some(_open) = open(owner) else { return false };
    let mut data: Vec<u16> = text.encode_utf16().collect();
    data.push(0);
    unsafe {
        if EmptyClipboard().is_err() {
            return false;
        }
        let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2) else { return false };
        let p = GlobalLock(g) as *mut u16;
        if p.is_null() {
            let _ = GlobalFree(Some(g));
            return false;
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        let _ = GlobalUnlock(g);
        if SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(g.0))).is_err() {
            let _ = GlobalFree(Some(g));
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_foreground_is_only_ever_handed_to_one_named_process() {
        // "Any process" and "no process" are not pids we may grant to.
        assert!(!allow_foreground_for(0));
        assert!(!allow_foreground_for(ASFW_ANY));
        assert!(!allow_foreground_for(u32::MAX));
        // A pid that does not exist is refused by Windows too (never a blanket grant).
        assert!(!allow_foreground_for(u32::MAX - 7));
    }

    #[test]
    fn altgr_is_a_character_not_a_shortcut() {
        // AltGr arrives as Ctrl+Alt: neither counts, so "AltGr+Q" (@ on a German layout) is no
        // Alt+Q / Ctrl+Q and the letter key maps to nothing (the character comes as WM_CHAR).
        assert_eq!(shortcut_mods(true, true), (false, false));
        let (ctrl, alt) = shortcut_mods(true, true);
        assert_eq!(map_key(b'Q' as u16, ctrl, alt), None);
        assert_eq!(map_key(b'3' as u16, ctrl, alt), None, "AltGr+3 is a character too");
        // The real shortcuts are untouched: Alt alone, Ctrl alone.
        assert_eq!(shortcut_mods(false, true), (false, true));
        assert_eq!(shortcut_mods(true, false), (true, false));
        assert_eq!(shortcut_mods(false, false), (false, false));
        let (ctrl, alt) = shortcut_mods(false, true);
        assert_eq!(map_key(b'1' as u16, ctrl, alt), Some(Key::Char('1')), "Alt+1 still works");
        let (ctrl, alt) = shortcut_mods(true, false);
        assert_eq!(map_key(b'A' as u16, ctrl, alt), Some(Key::Char('a')), "Ctrl+A still works");
    }

    #[test]
    fn keys_map() {
        assert_eq!(map_key(VK_DOWN.0, false, false), Some(Key::Down));
        assert_eq!(map_key(b'A' as u16, true, false), Some(Key::Char('a')));
        assert_eq!(map_key(b'1' as u16, false, true), Some(Key::Char('1')));
        assert_eq!(map_key(b'A' as u16, false, false), None, "plain letters come as WM_CHAR");
        assert_eq!(map_key(VK_F4.0, false, true), Some(Key::Escape));
        assert_eq!(map_key(VK_F4.0, false, false), None);
    }
}
