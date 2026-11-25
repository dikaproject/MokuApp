//! The Windows side of the clipboard: probing, bounded reading, writing, the owner process.
//! Thread-agnostic (all calls work on any thread; nothing here touches the UI). Never logs
//! clipboard content.

use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_UNICODETEXT};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
use windows::core::{PWSTR, w};

use super::imgutil;
use super::model::{self, MAX_TEXT_BYTES, Probe, Skip};
use super::store::Captured;
use crate::imaging;
use crate::ui::Image;

/// Registered clipboard formats we care about.
struct Formats {
    /// Set by password managers (KeePassXC, Bitwarden …): "do not process this content".
    exclude: u32,
    /// Legacy form of the same request.
    viewer_ignore: u32,
    /// DWORD; 0 = keep this out of Windows' clipboard history (1Password …).
    can_include: u32,
    /// Common PNG format (Snipping Tool, browsers, Paint).
    png: u32,
    /// Moku's own marker: "this content was put here by Moku, do not record it again".
    marker: u32,
}

fn formats() -> &'static Formats {
    static F: OnceLock<Formats> = OnceLock::new();
    F.get_or_init(|| unsafe {
        Formats {
            exclude: RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")),
            viewer_ignore: RegisterClipboardFormatW(w!("Clipboard Viewer Ignore")),
            can_include: RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
            png: RegisterClipboardFormatW(w!("PNG")),
            marker: RegisterClipboardFormatW(w!("MokuSelfWrite")),
        }
    })
}

pub fn fmt_png() -> u32 {
    formats().png
}
pub fn fmt_marker() -> u32 {
    formats().marker
}
pub fn fmt_exclude() -> u32 {
    formats().exclude
}
pub fn fmt_can_include() -> u32 {
    formats().can_include
}

/// Keeps the clipboard open; closes it when dropped.
pub struct Open;

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// `OpenClipboard` with retries (another app may hold it for a few milliseconds).
pub fn open(owner: Option<HWND>) -> Option<Open> {
    for i in 0..12u64 {
        if unsafe { OpenClipboard(owner) }.is_ok() {
            return Some(Open);
        }
        std::thread::sleep(Duration::from_millis(15 + i * 5));
    }
    None
}

pub fn sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// File name of the process that owns the clipboard (`KeePassXC.exe`), if it can be resolved.
/// Ask *before* opening the clipboard and as soon as the change notification arrives.
pub fn owner_exe() -> Option<String> {
    unsafe {
        let owner = GetClipboardOwner().ok()?;
        if owner.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(owner, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        let full = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
        Some(full.rsplit('\\').next().unwrap_or(&full).to_string())
    }
}

fn available(fmt: u32) -> bool {
    unsafe { IsClipboardFormatAvailable(fmt) }.is_ok()
}

/// Run `f` over the bytes of clipboard format `fmt` (the clipboard must be open).
fn with_data<R>(fmt: u32, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
    unsafe {
        let h = GetClipboardData(fmt).ok()?;
        if h.is_invalid() {
            return None;
        }
        let hg = HGLOBAL(h.0);
        let size = GlobalSize(hg);
        if size == 0 {
            return None;
        }
        let p = GlobalLock(hg) as *const u8;
        if p.is_null() {
            return None;
        }
        let r = f(std::slice::from_raw_parts(p, size));
        let _ = GlobalUnlock(hg);
        Some(r)
    }
}

/// Size of format `fmt` without copying it (the clipboard must be open).
fn data_len(fmt: u32) -> Option<usize> {
    unsafe {
        let h = GetClipboardData(fmt).ok()?;
        if h.is_invalid() { None } else { Some(GlobalSize(HGLOBAL(h.0))) }
    }
}

/// What the probe found out about the open clipboard.
pub fn probe(owner_exe: Option<String>) -> Probe {
    let f = formats();
    let can_include = if available(f.can_include) {
        // Present but unreadable counts as "do not include" (the safe side for privacy).
        Some(with_data(f.can_include, |b| b.get(..4).map(|d| u32::from_le_bytes([d[0], d[1], d[2], d[3]])).unwrap_or(0)).unwrap_or(0))
    } else {
        None
    };
    Probe { has_exclude_format: available(f.exclude) || available(f.viewer_ignore), can_include, owner_exe, self_marker: available(f.marker) }
}

/// Result of reading the open clipboard.
pub enum Read {
    /// Nothing we record (files, empty text, formats we do not handle).
    Nothing,
    /// Deliberately left out (password manager, ignored app, our own write).
    Skip(Skip),
    /// An image we refuse to read (too big / too many pixels): only a note is logged.
    TooBig,
    Got(Captured),
}

/// Read the open clipboard, honouring the skip rules *before* any data is touched. Text is read
/// at most [`MAX_TEXT_BYTES`] deep; PNG images up to 8 MB and 16 Mpx, bitmaps (DIB, e.g. a 4K
/// PrintScreen) up to 16 Mpx whatever their byte size.
pub fn read_open(ignore_apps: &[String], owner_exe: Option<String>) -> Read {
    let p = probe(owner_exe);
    if let Some(why) = model::skip_reason(&p, ignore_apps) {
        return Read::Skip(why);
    }
    // Text first (a spreadsheet cell has both text and a picture: the text is what people want).
    if available(CF_UNICODETEXT.0 as u32)
        && let Some((text, truncated)) = with_data(CF_UNICODETEXT.0 as u32, |b| {
            let units = unsafe { std::slice::from_raw_parts(b.as_ptr() as *const u16, b.len() / 2) };
            model::text_from_utf16(units, MAX_TEXT_BYTES)
        })
        && !text.trim().is_empty()
    {
        return Read::Got(Captured::Text { text, truncated });
    }
    read_image_open()
}

fn read_image_open() -> Read {
    let f = formats();
    // PNG: compact, keeps alpha, and is already the "original" we may keep for pinning.
    if available(f.png) {
        let len = data_len(f.png).unwrap_or(0);
        if !model::image_len_ok(len) {
            return if len > 0 { Read::TooBig } else { Read::Nothing };
        }
        if let Some(Some(c)) = with_data(f.png, |b| capture_png(b)) {
            return Read::Got(c);
        }
    }
    let dib_fmt = if available(CF_DIBV5.0 as u32) {
        CF_DIBV5.0 as u32
    } else if available(CF_DIB.0 as u32) {
        CF_DIB.0 as u32
    } else {
        return Read::Nothing;
    };
    // Gated on the pixel count from the header (reading it maps the block, copies nothing).
    match with_data(dib_fmt, model::dib_ok) {
        Some(true) => {}
        Some(false) => return Read::TooBig,
        None => return Read::Nothing,
    }
    match with_data(dib_fmt, |b| capture_dib(b)) {
        Some(Some(c)) => Read::Got(c),
        Some(None) => Read::TooBig,
        None => Read::Nothing,
    }
}

fn capture_png(png: &[u8]) -> Option<Captured> {
    let (w, h) = model::png_dims(png)?;
    if !model::image_dims_ok(w, h) {
        return None;
    }
    let thumb = imaging::decode(png, model::THUMB_PX).ok()?;
    Some(Captured::Image {
        thumb_png: imaging::encode_png(&thumb).ok()?,
        w,
        h,
        key: model::content_key(true, png),
        bytes: png.len() as u64,
        orig_png: Some(png.to_vec()),
    })
}

fn capture_dib(dib: &[u8]) -> Option<Captured> {
    let info = model::dib_info(dib)?;
    if !model::image_dims_ok(info.width, info.height) {
        return None;
    }
    let bmp = model::dib_to_bmp(dib)?;
    let full = imaging::decode(&bmp, 16_384).ok()?;
    let b = &full.0;
    let thumb = imaging::from_bgra(b.width, b.height, b.width * 4, &b.bgra, true, model::THUMB_PX).ok()?;
    // Keep the original only if its PNG fits the cap (a noisy 4K screenshot may not).
    let orig = imaging::encode_png(&full).ok().filter(|p| model::image_len_ok(p.len()));
    Some(Captured::Image {
        thumb_png: imaging::encode_png(&thumb).ok()?,
        w: b.width,
        h: b.height,
        key: model::content_key(true, dib),
        bytes: dib.len() as u64,
        orig_png: orig,
    })
}

// ---- writing ----------------------------------------------------------------------------------

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

/// Replace the clipboard content with `formats`. `owner` must be a real window of this process.
pub fn set_formats(owner: HWND, formats: &[(u32, Vec<u8>)]) -> bool {
    let Some(_open) = open(Some(owner)) else { return false };
    unsafe {
        if EmptyClipboard().is_err() {
            return false;
        }
        let mut ok = true;
        for (fmt, data) in formats {
            match alloc_global(data) {
                Some(h) => {
                    if SetClipboardData(*fmt, Some(HANDLE(h.0))).is_err() {
                        // Ownership was not taken: free it ourselves.
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

/// Extra formats that keep a *test* write out of Windows' own clipboard history and cloud sync.
pub fn private_formats() -> Vec<(u32, Vec<u8>)> {
    vec![(fmt_exclude(), vec![1, 0, 0, 0]), (fmt_can_include(), 0u32.to_le_bytes().to_vec())]
}

fn utf16z(text: &str) -> Vec<u8> {
    let mut v: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    v.extend([0, 0]);
    v
}

/// Put text back on the clipboard (marked as ours, so the listener does not record it again).
pub fn put_text(owner: HWND, text: &str) -> bool {
    set_formats(owner, &[(CF_UNICODETEXT.0 as u32, utf16z(text)), (fmt_marker(), vec![1])])
}

/// [`put_text`] for a secret (a one-time code): also keeps it out of Windows' own clipboard
/// history (Win+V) and cloud sync, like a password manager does.
pub fn put_text_private(owner: HWND, text: &str) -> bool {
    let mut f = vec![(CF_UNICODETEXT.0 as u32, utf16z(text)), (fmt_marker(), vec![1])];
    f.extend(private_formats());
    set_formats(owner, &f)
}

/// Put an image on the clipboard as PNG + DIB. `mark_self` adds Moku's marker (clipboard
/// put-back); a capture does not, so it shows up in the history like any screenshot.
pub fn put_image(owner: HWND, png: &[u8], pixels: Option<&Image>, mark_self: bool) -> bool {
    let decoded;
    let img = match pixels {
        Some(i) => i,
        None => match imaging::decode(png, 16_384) {
            Ok(i) => {
                decoded = i;
                &decoded
            }
            Err(_) => return false,
        },
    };
    let b = &img.0;
    let mut v = vec![(fmt_png(), png.to_vec()), (CF_DIB.0 as u32, imgutil::dib_from_premul_bgra(b.width, b.height, &b.bgra))];
    if mark_self {
        v.push((fmt_marker(), vec![1]));
    }
    set_formats(owner, &v)
}

// ---- test support: save / restore the user's clipboard ------------------------------------------

/// Everything on the clipboard that lives in global memory (text, DIB, PNG, HTML, file lists,
/// registered formats …), so a test can put the user's content back afterwards.
#[cfg(test)]
pub struct Backup(Vec<(u32, Vec<u8>)>);

#[cfg(test)]
impl Backup {
    pub fn take(owner: HWND) -> Backup {
        let mut all = vec![];
        if let Some(_o) = open(Some(owner)) {
            unsafe {
                let mut fmt = EnumClipboardFormats(0);
                while fmt != 0 {
                    // GDI-handle formats (CF_BITMAP, CF_PALETTE, metafiles) are synthesised from
                    // the DIB; display formats (0x80..0x8F) and private ones are not global memory.
                    let skip = matches!(fmt, 2 | 3 | 9 | 14 | 0x80..=0x8F);
                    if !skip && let Some(bytes) = with_data(fmt, |b| b.to_vec()) {
                        all.push((fmt, bytes));
                    }
                    fmt = EnumClipboardFormats(fmt);
                }
            }
        }
        Backup(all)
    }

    /// Put the saved content back. Marked private so Windows' history does not log it twice.
    pub fn restore(&self, owner: HWND) {
        let mut v = self.0.clone();
        if v.is_empty() {
            if let Some(_o) = open(Some(owner)) {
                unsafe {
                    let _ = EmptyClipboard();
                }
            }
            return;
        }
        v.extend(private_formats());
        set_formats(owner, &v);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
