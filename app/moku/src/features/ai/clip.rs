//! The clipboard, for the two moments "Tanya AI" has to borrow it: the Ctrl+C fallback of the
//! selection ("copy it, read it, put the old content back") and "Ganti teks" ("put the answer
//! on it, paste, put the old content back").
//!
//! The rule is that the user's clipboard comes back **exactly**: every format that lives in
//! global memory is saved and restored, and when that cannot be done faithfully (a format we
//! cannot read, an enormous clipboard, the clipboard locked) the caller is told and does not
//! touch it. What we write is marked so Windows' clipboard history, cloud sync and Moku's own
//! history leave it alone. Nothing here logs content.
//!
//! [`Clipboard`] is a trait so the logic above it (`selection.rs`) is tested without the real
//! clipboard; [`Win`] is the real thing (its tests are `#[ignore]`d: they use the real
//! clipboard, saved and restored).

use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// The most we save of the old clipboard (more is "not faithfully": the fallback is skipped).
pub const MAX_SAVE: usize = 32 * 1024 * 1024;
/// The longest text read back from the clipboard, in UTF-16 units.
pub const MAX_TEXT_UNITS: usize = 400_000;

/// The old clipboard, as it was.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Saved {
    pub formats: Vec<(u32, Vec<u8>)>,
}

pub trait Clipboard {
    /// `GetClipboardSequenceNumber`: changes whenever the content changes.
    fn sequence(&self) -> u32;
    /// The whole content, or `None` when it cannot be saved faithfully.
    fn save(&self) -> Option<Saved>;
    /// The Unicode text on it, if any.
    fn read_text(&self) -> Option<String>;
    /// Replace the content with `text`, marked private (no history, no sync, not recorded by Moku).
    fn set_private_text(&self, text: &str) -> bool;
    /// Put `saved` back (an empty one empties the clipboard). Marked private.
    fn restore(&self, saved: &Saved) -> bool;
}

/// The extra formats that keep a write out of Windows' history / cloud sync and out of Moku's own
/// clipboard history: the clipboard feature's own private formats and its "this write is Moku's
/// own" marker (`clipboard::sys`), so there is one definition of both.
fn private_formats() -> Vec<(u32, Vec<u8>)> {
    use crate::features::clipboard::sys;
    let mut v = sys::private_formats();
    v.push((sys::fmt_marker(), vec![1]));
    v
}

/// The real clipboard, with `owner` (a window of this process) as the owner of what we write.
pub struct Win {
    pub owner: isize,
}

struct Open;

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

fn open(owner: isize) -> Option<Open> {
    let hwnd = if owner == 0 { None } else { Some(HWND(owner as *mut _)) };
    for i in 0..12u64 {
        if unsafe { OpenClipboard(hwnd) }.is_ok() {
            return Some(Open);
        }
        std::thread::sleep(Duration::from_millis(15 + i * 5));
    }
    None
}

/// Formats that Windows makes up from others (or that are display-only): not saved, and they
/// come back by themselves when the real ones are restored.
fn is_synthesised(fmt: u32) -> bool {
    // CF_BITMAP, CF_METAFILEPICT, CF_PALETTE, CF_ENHMETAFILE, CF_OEMTEXT, CF_TEXT (made from
    // Unicode text), the display formats.
    matches!(fmt, 1 | 2 | 3 | 7 | 9 | 14 | 0x80..=0x8F)
}

fn alloc_global(data: &[u8]) -> Option<HGLOBAL> {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1)).ok()?;
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(h));
            return None;
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        let _ = GlobalUnlock(h);
        Some(h)
    }
}

fn utf16z(text: &str) -> Vec<u8> {
    let mut v: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    v.extend([0, 0]);
    v
}

/// Replace the clipboard with `list`. The clipboard must NOT be open by the caller.
fn set_formats(owner: isize, list: &[(u32, Vec<u8>)]) -> bool {
    // With no owner window SetClipboardData fails after EmptyClipboard: we would lose the
    // clipboard. Never start without one.
    if owner == 0 {
        return false;
    }
    let Some(_o) = open(owner) else { return false };
    unsafe {
        if EmptyClipboard().is_err() {
            return false;
        }
        let mut ok = true;
        for (fmt, data) in list {
            match alloc_global(data) {
                Some(h) => {
                    if SetClipboardData(*fmt, Some(HANDLE(h.0))).is_err() {
                        let _ = GlobalFree(Some(h));
                        ok = false;
                    }
                }
                None => ok = false,
            }
        }
        ok
    }
}

impl Clipboard for Win {
    fn sequence(&self) -> u32 {
        unsafe { GetClipboardSequenceNumber() }
    }

    fn save(&self) -> Option<Saved> {
        let _o = open(self.owner)?;
        let mut saved = Saved::default();
        let mut total = 0usize;
        unsafe {
            let mut fmt = EnumClipboardFormats(0);
            while fmt != 0 {
                if (0x300..=0x3FF).contains(&fmt) {
                    return None; // GDI objects: cannot be copied as bytes
                }
                if !is_synthesised(fmt) {
                    let h = GetClipboardData(fmt).ok()?;
                    if h.is_invalid() {
                        return None;
                    }
                    let hg = HGLOBAL(h.0);
                    let size = GlobalSize(hg);
                    if size == 0 {
                        // An empty block is fine; a handle that is not global memory is not.
                        let p = GlobalLock(hg);
                        if p.is_null() {
                            return None;
                        }
                        let _ = GlobalUnlock(hg);
                        saved.formats.push((fmt, vec![]));
                    } else {
                        total += size;
                        if total > MAX_SAVE {
                            return None;
                        }
                        let p = GlobalLock(hg) as *const u8;
                        if p.is_null() {
                            return None;
                        }
                        saved.formats.push((fmt, std::slice::from_raw_parts(p, size).to_vec()));
                        let _ = GlobalUnlock(hg);
                    }
                }
                fmt = EnumClipboardFormats(fmt);
            }
        }
        Some(saved)
    }

    fn read_text(&self) -> Option<String> {
        let _o = open(self.owner)?;
        unsafe {
            let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
            if h.is_invalid() {
                return None;
            }
            let hg = HGLOBAL(h.0);
            let p = GlobalLock(hg) as *const u16;
            if p.is_null() {
                return None;
            }
            let n = (GlobalSize(hg) / 2).min(MAX_TEXT_UNITS);
            let units = std::slice::from_raw_parts(p, n);
            let end = units.iter().position(|&u| u == 0).unwrap_or(n);
            let s = String::from_utf16_lossy(&units[..end]);
            let _ = GlobalUnlock(hg);
            Some(s)
        }
    }

    fn set_private_text(&self, text: &str) -> bool {
        let mut v = vec![(CF_UNICODETEXT.0 as u32, utf16z(text))];
        v.extend(private_formats());
        set_formats(self.owner, &v)
    }

    fn restore(&self, saved: &Saved) -> bool {
        if self.owner == 0 {
            return false;
        }
        if saved.formats.is_empty() {
            let Some(_o) = open(self.owner) else { return false };
            return unsafe { EmptyClipboard() }.is_ok();
        }
        let mut v = saved.formats.clone();
        v.extend(private_formats());
        set_formats(self.owner, &v)
    }
}

#[cfg(test)]
pub mod fake {
    //! An in-memory clipboard that behaves like the real one where it matters: the sequence
    //! number moves on every write, and a "program" can be scripted to copy.

    use super::*;
    use std::cell::{Cell, RefCell};

    pub struct Fake {
        pub content: RefCell<Saved>,
        pub seq: Cell<u32>,
        /// `save()` fails (a format that cannot be read).
        pub unsavable: Cell<bool>,
        pub writes: RefCell<Vec<String>>,
    }

    impl Fake {
        pub fn with_text(t: &str) -> Fake {
            let f = Fake { content: RefCell::new(Saved::default()), seq: Cell::new(1), unsavable: Cell::new(false), writes: RefCell::new(vec![]) };
            f.content.borrow_mut().formats.push((CF_UNICODETEXT.0 as u32, utf16z(t)));
            f
        }

        pub fn empty() -> Fake {
            let f = Fake::with_text("");
            f.content.borrow_mut().formats.clear();
            f
        }

        /// What another program does: put new text on the clipboard.
        pub fn external_copy(&self, t: &str) {
            *self.content.borrow_mut() = Saved { formats: vec![(CF_UNICODETEXT.0 as u32, utf16z(t))] };
            self.seq.set(self.seq.get() + 1);
        }

        pub fn text(&self) -> Option<String> {
            self.read_text()
        }
    }

    impl Clipboard for Fake {
        fn sequence(&self) -> u32 {
            self.seq.get()
        }
        fn save(&self) -> Option<Saved> {
            (!self.unsavable.get()).then(|| self.content.borrow().clone())
        }
        fn read_text(&self) -> Option<String> {
            let c = self.content.borrow();
            let (_, d) = c.formats.iter().find(|(f, _)| *f == CF_UNICODETEXT.0 as u32)?;
            let units: Vec<u16> = d.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).take_while(|u| *u != 0).collect();
            Some(String::from_utf16_lossy(&units))
        }
        fn set_private_text(&self, text: &str) -> bool {
            self.writes.borrow_mut().push(format!("set:{text}"));
            *self.content.borrow_mut() = Saved { formats: vec![(CF_UNICODETEXT.0 as u32, utf16z(text))] };
            self.seq.set(self.seq.get() + 1);
            true
        }
        fn restore(&self, saved: &Saved) -> bool {
            self.writes.borrow_mut().push("restore".into());
            *self.content.borrow_mut() = saved.clone();
            self.seq.set(self.seq.get() + 1);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::w;

    #[test]
    fn synthesised_formats_are_not_saved_and_real_ones_are() {
        for f in [1u32, 2, 3, 7, 9, 14, 0x80, 0x8F] {
            assert!(is_synthesised(f), "{f}");
        }
        // Unicode text, DIB, DIBV5, HDROP, PNG / HTML (registered formats) are real data.
        for f in [13u32, 8, 17, 15, 0xC0A0, 0xC123] {
            assert!(!is_synthesised(f), "{f}");
        }
    }

    #[test]
    fn what_we_write_is_marked_private() {
        let p = private_formats();
        assert_eq!(p.len(), 3);
        assert!(p.iter().any(|(_, d)| d == &vec![1, 0, 0, 0]), "exclude-from-monitoring");
        assert!(p.iter().any(|(_, d)| d == &0u32.to_le_bytes().to_vec()), "CanIncludeInClipboardHistory = 0");
        assert!(p.iter().all(|(f, _)| *f >= 0xC000), "registered formats");
    }

    /// The REAL clipboard (saved and restored): text round trip through our own functions.
    /// Run: `cargo test -p moku clip::tests::real -- --ignored --test-threads=1`
    #[test]
    #[ignore = "uses the real clipboard (saved and restored); run with --ignored --test-threads=1"]
    fn real_clipboard_roundtrip_with_save_and_restore() {
        // A message-only window of the built-in STATIC class owns what we write.
        let owner = unsafe {
            windows::Win32::UI::WindowsAndMessaging::CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("moku-ai-clip-test"),
                windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(windows::Win32::UI::WindowsAndMessaging::HWND_MESSAGE),
                None,
                None,
                None,
            )
        }
        .expect("a message-only window");
        let win = Win { owner: owner.0 as isize };
        assert!(!Win { owner: 0 }.restore(&Saved::default()), "without an owner window nothing is touched");
        let before = win.save().expect("the clipboard can be saved");
        assert!(win.set_private_text("moku ai test \u{1F511} \u{65E5}\u{672C}"));
        assert_eq!(win.read_text().as_deref(), Some("moku ai test \u{1F511} \u{65E5}\u{672C}"));
        let mid = win.save().expect("saved");
        assert!(mid.formats.iter().any(|(f, _)| *f == CF_UNICODETEXT.0 as u32));
        assert!(win.restore(&before));
        let after = win.save().expect("saved again");
        // The user's content is back, format by format (plus nothing lost).
        let strip = |s: &Saved| s.formats.iter().filter(|(f, _)| !private_formats().iter().any(|(p, _)| p == f)).cloned().collect::<Vec<_>>();
        assert_eq!(strip(&after), strip(&before));
    }
}
