//! Pure clipboard-history model (no Windows API): items, dedupe, size caps, expiry, the
//! skip rules and the JSON index. Everything here is unit-tested; `sys.rs` feeds it.
//!
//! Storage layout (`<data dir>\clipboard\`), all names derived from the item's `key` (a content
//! hash), so no user text ever becomes part of a path and identical content is stored once:
//!
//! | file | what |
//! |---|---|
//! | `index.json` | the list of [`ClipItem`]s (previews only: bounded memory) |
//! | `t<key>.txt` | full text of a text / link / colour item, UTF-8, ≤ 100 KB |
//! | `i<key>.png` | 128 px thumbnail of an image item |
//! | `o<key>.png` | the original image, only for pinned items and the newest image |

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::actions::{self, Act};

/// How long a one-time code stays in the history (it lives in memory only).
pub const OTP_TTL_SECS: i64 = 60;

/// Longest text kept per item. Longer text is truncated (and flagged).
pub const MAX_TEXT_BYTES: usize = 100 * 1024;
/// Largest image blob read from the clipboard at all (PNG bytes, or the raw DIB).
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// Largest image (in pixels) we will decode: 16 Mpx covers 4K (8.3 Mpx) and 5K (14.7 Mpx).
pub const MAX_IMAGE_PIXELS: u64 = 16_000_000;
/// Longest side of the stored thumbnail.
pub const THUMB_PX: u32 = 128;
/// Characters of text kept in memory / the index per item.
pub const PREVIEW_CHARS: usize = 160;
/// Never load more entries than this from a (possibly hand-edited) index.
const MAX_INDEX_ITEMS: usize = 1000;

// ---- skip rules -------------------------------------------------------------------------------

/// What we learned about the clipboard *before* reading any of its data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    /// The registered format `ExcludeClipboardContentFromMonitorProcessing` is present.
    pub has_exclude_format: bool,
    /// Value of the `CanIncludeInClipboardHistory` DWORD, when the format is present.
    pub can_include: Option<u32>,
    /// File name of the process that owns the clipboard (`KeePassXC.exe`), if it could be resolved.
    pub owner_exe: Option<String>,
    /// The clipboard carries Moku's own marker format: we put this content there ourselves.
    pub self_marker: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    ExcludedFormat,
    NotInHistory,
    IgnoredApp,
    SelfWrite,
}

/// Should this clipboard content be left out of the history? Password managers set the
/// exclusion formats; `ignore_apps` is the user's own list.
pub fn skip_reason(p: &Probe, ignore_apps: &[String]) -> Option<Skip> {
    if p.has_exclude_format {
        return Some(Skip::ExcludedFormat);
    }
    if p.can_include == Some(0) {
        return Some(Skip::NotInHistory);
    }
    if let Some(exe) = &p.owner_exe
        && is_ignored_app(exe, ignore_apps)
    {
        return Some(Skip::IgnoredApp);
    }
    if p.self_marker {
        return Some(Skip::SelfWrite);
    }
    None
}

fn exe_stem(s: &str) -> String {
    let name = s.trim().rsplit(['\\', '/']).next().unwrap_or("").trim();
    let lower = name.to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// Case-insensitive match of a process file name against `ignore_apps` (`.exe` optional,
/// a full path is reduced to its file name).
pub fn is_ignored_app(owner_exe: &str, ignore_apps: &[String]) -> bool {
    let me = exe_stem(owner_exe);
    !me.is_empty() && ignore_apps.iter().map(|a| exe_stem(a)).any(|a| !a.is_empty() && a == me)
}

// ---- size caps --------------------------------------------------------------------------------

/// Convert UTF-16 clipboard text (up to the first NUL) into at most `max_bytes` of UTF-8, cut
/// on a character boundary. Returns the text and whether something was cut off. Only the first
/// `max_bytes` worth of input is ever touched, so a gigabyte clipboard costs the same as 100 KB.
pub fn text_from_utf16(units: &[u16], max_bytes: usize) -> (String, bool) {
    let mut out = String::new();
    for r in char::decode_utf16(units.iter().copied().take_while(|&u| u != 0)) {
        let c = r.unwrap_or(char::REPLACEMENT_CHARACTER);
        if out.len() + c.len_utf8() > max_bytes {
            return (out, true);
        }
        out.push(c);
    }
    (out, false)
}

/// Cut `s` to at most `max_bytes` on a char boundary.
pub fn truncate_text(s: &str, max_bytes: usize) -> (String, bool) {
    if s.len() <= max_bytes {
        return (s.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// Is an image blob of `len` bytes small enough to read (cap 8 MB)?
pub fn image_len_ok(len: usize) -> bool {
    len > 0 && len <= MAX_IMAGE_BYTES
}

/// Is an image of this size small enough to decode (guards against tiny files that claim a
/// huge canvas)?
pub fn image_dims_ok(w: u32, h: u32) -> bool {
    w > 0 && h > 0 && (w as u64) * (h as u64) <= MAX_IMAGE_PIXELS
}

/// Can this clipboard DIB be read? Uncompressed bitmaps are big (a 4K PrintScreen is 33 MB), so
/// they are gated on their *pixel count* (≤ [`MAX_IMAGE_PIXELS`]) rather than on
/// [`MAX_IMAGE_BYTES`]; the byte length only has to be plausible for that many pixels
/// (≤ 4 bytes each, plus header and palette).
pub fn dib_ok(dib: &[u8]) -> bool {
    match dib_info(dib) {
        Some(i) => image_dims_ok(i.width, i.height) && (dib.len() as u64) <= MAX_IMAGE_PIXELS * 4 + 64 * 1024,
        None => false,
    }
}

// ---- text helpers -----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Link,
    Color,
    Image,
}

/// `#RGB` / `#RRGGBB` → `0xRRGGBB`.
pub fn parse_hex_color(s: &str) -> Option<u32> {
    let h = s.trim().strip_prefix('#')?;
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match h.len() {
        3 => {
            let v = u32::from_str_radix(h, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            Some(((r * 17) << 16) | ((g * 17) << 8) | (b * 17))
        }
        6 => u32::from_str_radix(h, 16).ok(),
        _ => None,
    }
}

pub fn classify_text(s: &str) -> Kind {
    let t = s.trim();
    if parse_hex_color(t).is_some() || actions::color::parse(t).is_some() {
        return Kind::Color;
    }
    let lower = t.get(..8.min(t.len())).unwrap_or("").to_ascii_lowercase();
    if (lower.starts_with("http://") || lower.starts_with("https://")) && !t.chars().any(char::is_whitespace) && t.len() < 2048 {
        return Kind::Link;
    }
    Kind::Text
}

/// First non-empty line, whitespace collapsed, at most [`PREVIEW_CHARS`] characters.
pub fn preview_of(s: &str) -> String {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut out = String::new();
    let mut last_space = false;
    for c in line.chars() {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
        if out.chars().count() >= PREVIEW_CHARS {
            break;
        }
    }
    out
}

fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
    let mut h = seed;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Content hash used as the item key (text: of the kept UTF-8; image: of the clipboard blob).
pub fn content_key(is_image: bool, bytes: &[u8]) -> u64 {
    fnv1a(if is_image { 0x9e3779b97f4a7c15 } else { 0xcbf29ce484222325 }, bytes)
}

// ---- age --------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Age {
    Now,
    Minutes(i64),
    Hours(i64),
    Yesterday,
    Days(i64),
}

pub fn age(now: i64, ts: i64) -> Age {
    let d = (now - ts).max(0);
    match d {
        0..=59 => Age::Now,
        60..=3599 => Age::Minutes(d / 60),
        3600..=86_399 => Age::Hours(d / 3600),
        86_400..=172_799 => Age::Yesterday,
        _ => Age::Days(d / 86_400),
    }
}

// ---- items and history ------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipItem {
    /// Content hash; identity of the item and base of its file names.
    pub key: u64,
    pub kind: Kind,
    /// Unix seconds of the last time this content was copied.
    pub ts: i64,
    #[serde(default)]
    pub pinned: bool,
    /// What the row shows (text: first line; image: empty).
    #[serde(default)]
    pub preview: String,
    /// Text: bytes kept. Image: size of the original blob.
    #[serde(default)]
    pub bytes: u64,
    /// The text was cut at 100 KB.
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub w: u32,
    #[serde(default)]
    pub h: u32,
    /// Image: the original is stored (`o<key>.png`).
    #[serde(default)]
    pub full: bool,
    /// What smart actions the text offers (see [`actions`]). Absent for plain text, for images
    /// and for items saved before the actions existed. [`Act::Otp`] is only ever held in memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act: Option<Act>,
}

impl ClipItem {
    /// `None` for whitespace-only text.
    pub fn try_text(text: &str, truncated: bool, ts: i64) -> Option<ClipItem> {
        if text.trim().is_empty() {
            return None;
        }
        Some(ClipItem {
            key: content_key(false, text.as_bytes()),
            kind: classify_text(text),
            ts,
            pinned: false,
            preview: preview_of(text),
            bytes: text.len() as u64,
            truncated,
            w: 0,
            h: 0,
            full: false,
            act: None,
        })
    }

    /// A one-time code, held in memory only: the preview is dots (one per digit), the item owns
    /// no file and is never saved. The text itself is kept by the feature's state, not here.
    pub fn otp(code: &str, ts: i64) -> ClipItem {
        ClipItem {
            key: content_key(false, code.as_bytes()),
            kind: Kind::Text,
            ts,
            pinned: false,
            preview: "\u{2022}".repeat(code.chars().filter(|c| c.is_ascii_digit()).count().max(1)),
            bytes: code.len() as u64,
            truncated: false,
            w: 0,
            h: 0,
            full: false,
            act: Some(Act::Otp),
        }
    }

    #[cfg(test)]
    pub fn text(text: &str, truncated: bool, ts: i64) -> ClipItem {
        ClipItem::try_text(text, truncated, ts).expect("non-empty text")
    }

    pub fn image(key: u64, w: u32, h: u32, bytes: u64, full: bool, ts: i64) -> ClipItem {
        ClipItem { key, kind: Kind::Image, ts, pinned: false, preview: String::new(), bytes, truncated: false, w, h, full, act: None }
    }

    pub fn is_image(&self) -> bool {
        self.kind == Kind::Image
    }

    /// A one-time code: memory only, never saved, gone after [`OTP_TTL_SECS`].
    pub fn is_otp(&self) -> bool {
        self.act == Some(Act::Otp)
    }

    /// Every file this item owns in the clipboard folder (none for a one-time code).
    pub fn files(&self) -> Vec<String> {
        if self.is_otp() {
            return vec![];
        }
        if self.is_image() {
            let mut v = vec![thumb_file(self.key)];
            if self.full {
                v.push(orig_file(self.key));
            }
            v
        } else {
            vec![text_file(self.key)]
        }
    }
}

pub fn text_file(key: u64) -> String {
    format!("t{key:016x}.txt")
}
pub fn thumb_file(key: u64) -> String {
    format!("i{key:016x}.png")
}
pub fn orig_file(key: u64) -> String {
    format!("o{key:016x}.png")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct History {
    /// Newest first.
    #[serde(default)]
    pub items: Vec<ClipItem>,
}

impl History {
    /// Read the index. A missing, unreadable or corrupt file gives an empty history.
    pub fn load(path: &Path) -> History {
        let Ok(bytes) = std::fs::read(path) else { return History::default() };
        let mut h: History = serde_json::from_slice(super::imgutil::strip_utf8_bom(&bytes)).unwrap_or_default();
        // Sanity: unique keys, bounded size, newest first.
        let mut seen = HashSet::new();
        h.items.retain(|i| seen.insert(i.key));
        // A one-time code is never saved, so an index that has one was edited by hand: drop it.
        h.items.retain(|i| !i.is_otp());
        h.items.sort_by_key(|i| std::cmp::Reverse(i.ts));
        h.items.truncate(MAX_INDEX_ITEMS);
        h
    }

    /// The index file's bytes (`App::save_later` writes them off the UI thread).
    pub fn to_json(&self) -> Vec<u8> {
        // One-time codes live in memory only: they are never part of the saved index.
        let saved: Vec<&ClipItem> = self.items.iter().filter(|i| !i.is_otp()).collect();
        serde_json::to_vec(&serde_json::json!({ "version": 1, "items": saved })).unwrap_or_default()
    }

    /// Write the index atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::persist::write_atomic(path, &self.to_json())
    }

    pub fn get(&self, key: u64) -> Option<&ClipItem> {
        self.items.iter().find(|i| i.key == key)
    }

    /// Pinned first, then newest first (the order the panel shows).
    pub fn ordered(&self) -> Vec<&ClipItem> {
        let mut v: Vec<&ClipItem> = self.items.iter().collect();
        v.sort_by_key(|i| (!i.pinned, std::cmp::Reverse(i.ts)));
        v
    }

    /// Add an item (or, when the same content is already there, bump it to the top with the new
    /// time and keep its pin). Returns the files that are no longer needed.
    pub fn insert(&mut self, item: ClipItem, max_items: usize) -> Vec<String> {
        if item.is_otp() {
            let mut gone = self.insert_secret(item);
            gone.extend(self.evict_over(max_items));
            return gone;
        }
        let mut gone = vec![];
        if let Some(pos) = self.items.iter().position(|i| i.key == item.key) {
            let mut old = self.items.remove(pos);
            old.ts = item.ts;
            old.full |= item.full;
            if old.is_otp() {
                // The same digits again, from an app that is not a source: they stay a code (in
                // memory only), and the file just written for this copy is not needed.
                gone.extend(item.files());
            } else {
                old.act = item.act;
            }
            self.items.insert(0, old);
        } else {
            self.items.insert(0, item);
        }
        gone.extend(self.enforce_originals());
        gone.extend(self.evict_over(max_items));
        gone
    }

    /// Add a one-time code (memory only). When a plain copy of the same digits is already there
    /// it is replaced, and its file is returned for deletion. The caller trims to `max_items`.
    pub fn insert_secret(&mut self, item: ClipItem) -> Vec<String> {
        let mut gone = vec![];
        if let Some(pos) = self.items.iter().position(|i| i.key == item.key) {
            gone.extend(self.items.remove(pos).files());
        }
        self.items.insert(0, item);
        gone
    }

    /// Remove the one-time codes that are `ttl` seconds old or more. Returns their keys (so the
    /// feature can forget the text it held for them).
    pub fn expire_otps(&mut self, now: i64, ttl: i64) -> Vec<u64> {
        let mut gone = vec![];
        self.items.retain(|i| {
            let expired = i.is_otp() && now - i.ts >= ttl;
            if expired {
                gone.push(i.key);
            }
            !expired
        });
        gone
    }

    /// Originals are kept for pinned images and the newest image only.
    fn enforce_originals(&mut self) -> Vec<String> {
        let newest_image = self.items.iter().find(|i| i.is_image()).map(|i| i.key);
        let mut gone = vec![];
        for i in self.items.iter_mut().filter(|i| i.is_image() && i.full) {
            if !i.pinned && Some(i.key) != newest_image {
                i.full = false;
                gone.push(orig_file(i.key));
            }
        }
        gone
    }

    /// Drop the oldest unpinned items beyond `max_items` (pinned items are never evicted).
    fn evict_over(&mut self, max_items: usize) -> Vec<String> {
        let mut gone = vec![];
        while self.items.len() > max_items {
            let Some(pos) = self.items.iter().rposition(|i| !i.pinned) else { break };
            gone.extend(self.items.remove(pos).files());
        }
        gone
    }

    /// Apply a smaller `max_items` after a config change.
    pub fn enforce_max(&mut self, max_items: usize) -> Vec<String> {
        self.evict_over(max_items)
    }

    /// Remove unpinned items older than `keep_days`.
    pub fn expire(&mut self, now: i64, keep_days: u32) -> Vec<String> {
        let limit = keep_days as i64 * 86_400;
        let mut gone = vec![];
        self.items.retain(|i| {
            let keep = i.pinned || now - i.ts <= limit;
            if !keep {
                gone.extend(i.files());
            }
            keep
        });
        gone
    }

    /// Pin / unpin. Unpinning an image that is not the newest drops its stored original.
    pub fn set_pinned(&mut self, key: u64, on: bool) -> Vec<String> {
        // A one-time code cannot be kept: it is gone after a minute whatever.
        if let Some(i) = self.items.iter_mut().find(|i| i.key == key && !i.is_otp()) {
            i.pinned = on;
        }
        self.enforce_originals()
    }

    pub fn remove(&mut self, key: u64) -> Vec<String> {
        let mut gone = vec![];
        self.items.retain(|i| {
            if i.key == key {
                gone.extend(i.files());
            }
            i.key != key
        });
        gone
    }

    /// "Kosongkan": everything except pinned items.
    pub fn clear_unpinned(&mut self) -> Vec<String> {
        let mut gone = vec![];
        self.items.retain(|i| {
            if !i.pinned {
                gone.extend(i.files());
            }
            i.pinned
        });
        gone
    }

    /// File names the index refers to (anything else in the folder is an orphan).
    pub fn referenced_files(&self) -> HashSet<String> {
        self.items.iter().flat_map(|i| i.files()).collect()
    }
}

// ---- image container parsing ------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DibInfo {
    pub width: u32,
    /// Absolute height (a negative `biHeight` means top-down).
    pub height: u32,
    pub bit_count: u16,
    /// Offset of the first pixel from the start of the DIB.
    pub pixel_offset: usize,
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Parse a `CF_DIB` / `CF_DIBV5` block (BITMAPINFOHEADER or later, no file header).
pub fn dib_info(dib: &[u8]) -> Option<DibInfo> {
    let size = le_u32(dib, 0)? as usize;
    if size < 40 || size > 256 || dib.len() < size {
        return None;
    }
    let width = le_u32(dib, 4)? as i32;
    let height = le_u32(dib, 8)? as i32;
    let bit_count = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = le_u32(dib, 16)?;
    let clr_used = le_u32(dib, 32)? as usize;
    if width <= 0 || height == 0 || bit_count == 0 {
        return None;
    }
    let masks = if size == 40 {
        match compression {
            3 => 12, // BI_BITFIELDS
            6 => 16, // BI_ALPHABITFIELDS
            _ => 0,
        }
    } else {
        0
    };
    let palette = if bit_count <= 8 {
        (if clr_used != 0 { clr_used } else { 1usize << bit_count }) * 4
    } else {
        clr_used * 4
    };
    let pixel_offset = size + masks + palette;
    if pixel_offset > dib.len() {
        return None;
    }
    Some(DibInfo { width: width as u32, height: height.unsigned_abs(), bit_count, pixel_offset })
}

/// Wrap a DIB in a `BITMAPFILEHEADER` so WIC can decode it like a `.bmp` file.
pub fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    let info = dib_info(dib)?;
    let total = 14 + dib.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"BM");
    out.extend((total as u32).to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(((14 + info.pixel_offset) as u32).to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

/// Width and height from a PNG's IHDR chunk.
pub fn png_dims(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[0..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    Some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn text_item(s: &str, ts: i64) -> ClipItem {
        ClipItem::text(s, false, ts)
    }

    fn image_item(seed: u64, ts: i64, full: bool) -> ClipItem {
        ClipItem::image(seed, 1280, 720, 4096, full, ts)
    }

    fn ignore() -> Vec<String> {
        vec!["KeePassXC.exe".into(), "1Password.exe".into()]
    }

    #[test]
    fn clipboard_skips_excluded_formats() {
        let ok = Probe::default();
        assert_eq!(skip_reason(&ok, &ignore()), None);

        // Password managers: ExcludeClipboardContentFromMonitorProcessing present.
        let p = Probe { has_exclude_format: true, ..Probe::default() };
        assert_eq!(skip_reason(&p, &ignore()), Some(Skip::ExcludedFormat));

        // CanIncludeInClipboardHistory = 0 skips, = 1 does not.
        let p = Probe { can_include: Some(0), ..Probe::default() };
        assert_eq!(skip_reason(&p, &ignore()), Some(Skip::NotInHistory));
        let p = Probe { can_include: Some(1), ..Probe::default() };
        assert_eq!(skip_reason(&p, &ignore()), None);

        // Owner process in ignore_apps (case-insensitive, with or without ".exe", or a full path).
        for exe in ["keepassxc.exe", "KeePassXC", r"C:\Program Files\KeePassXC\KeePassXC.exe", "1PASSWORD.EXE"] {
            let p = Probe { owner_exe: Some(exe.into()), ..Probe::default() };
            assert_eq!(skip_reason(&p, &ignore()), Some(Skip::IgnoredApp), "{exe}");
        }
        let p = Probe { owner_exe: Some("notepad.exe".into()), ..Probe::default() };
        assert_eq!(skip_reason(&p, &ignore()), None);
        // An empty entry in the list must not match everything.
        let p = Probe { owner_exe: Some("notepad.exe".into()), ..Probe::default() };
        assert_eq!(skip_reason(&p, &["".to_string(), "  ".to_string()]), None);

        // Excluded format wins over everything else.
        let p = Probe { has_exclude_format: true, can_include: Some(0), owner_exe: Some("KeePassXC.exe".into()), self_marker: true };
        assert_eq!(skip_reason(&p, &ignore()), Some(Skip::ExcludedFormat));
    }

    #[test]
    fn clipboard_self_write_is_not_recorded() {
        let p = Probe { self_marker: true, ..Probe::default() };
        assert_eq!(skip_reason(&p, &[]), Some(Skip::SelfWrite));
    }

    #[test]
    fn clipboard_caps_text_and_images() {
        // 150 KB of ASCII → exactly the cap, flagged as truncated.
        let big: Vec<u16> = std::iter::repeat_n('a' as u16, 150 * 1024).collect();
        let (s, cut) = text_from_utf16(&big, MAX_TEXT_BYTES);
        assert_eq!(s.len(), MAX_TEXT_BYTES);
        assert!(cut);

        // Multi-byte text is cut on a char boundary, never beyond the cap, and stays valid UTF-8.
        let big: Vec<u16> = std::iter::repeat_n('é' as u16, 100 * 1024).collect(); // 2 bytes each
        let (s, cut) = text_from_utf16(&big, MAX_TEXT_BYTES);
        assert!(cut && s.len() <= MAX_TEXT_BYTES && s.len() % 2 == 0);
        let emoji: Vec<u16> = "😀".repeat(60_000).encode_utf16().collect(); // 4 bytes each
        let (s, cut) = text_from_utf16(&emoji, MAX_TEXT_BYTES);
        assert!(cut && s.len() <= MAX_TEXT_BYTES && s.chars().all(|c| c == '😀'));

        // Small text passes untouched; the NUL terminator and anything after it is ignored.
        let mut small: Vec<u16> = "halo dunia".encode_utf16().collect();
        small.extend([0, 'x' as u16]);
        assert_eq!(text_from_utf16(&small, MAX_TEXT_BYTES), ("halo dunia".to_string(), false));
        // Lone surrogates do not panic.
        let (s, _) = text_from_utf16(&[0xD800, 'a' as u16], MAX_TEXT_BYTES);
        assert!(s.ends_with('a'));

        // Images: 8 MB is the limit for what is read from the clipboard at all.
        assert!(image_len_ok(MAX_IMAGE_BYTES));
        assert!(!image_len_ok(MAX_IMAGE_BYTES + 1));
        assert!(!image_len_ok(0));
        // A tiny PNG that claims to be 60000 × 60000 is refused (decompression bomb).
        assert!(image_dims_ok(1920, 1080));
        assert!(image_dims_ok(3840, 2160));
        assert!(!image_dims_ok(60_000, 60_000));
        assert!(!image_dims_ok(0, 100));
    }

    #[test]
    fn clipboard_expiry() {
        let mut h = History::default();
        h.insert(text_item("fresh", NOW - DAY), 50);
        h.insert(text_item("old", NOW - 8 * DAY), 50);
        let mut pinned = text_item("pinned old", NOW - 30 * DAY);
        pinned.pinned = true;
        h.insert(pinned, 50);
        h.insert(image_item(7, NOW - 9 * DAY, false), 50);

        let gone = h.expire(NOW, 7);
        let left: Vec<&str> = h.items.iter().map(|i| i.preview.as_str()).collect();
        assert!(left.contains(&"fresh") && left.contains(&"pinned old"), "{left:?}");
        assert!(!left.contains(&"old"));
        assert_eq!(h.items.len(), 2, "the 9-day-old image expired too");
        // The files of expired items are reported so the caller can delete them.
        assert!(gone.contains(&text_file(text_item("old", 0).key)));
        assert!(gone.contains(&thumb_file(7)));
        // Nothing else expires on a second pass.
        assert!(h.expire(NOW, 7).is_empty());
        // keep_days = 1 removes "fresh" (1 day old counts as expired once it is older than 1 day).
        h.expire(NOW + 1, 1);
        assert_eq!(h.items.len(), 1);
        assert!(h.items[0].pinned);
    }

    #[test]
    fn clipboard_dedupes_bumps_and_evicts() {
        let mut h = History::default();
        h.insert(text_item("a", 100), 3);
        h.insert(text_item("b", 200), 3);
        h.insert(text_item("c", 300), 3);
        // Same text again: no duplicate, the entry moves to the top with the new time.
        h.insert(text_item("a", 400), 3);
        assert_eq!(h.items.len(), 3);
        assert_eq!(h.items[0].preview, "a");
        assert_eq!(h.items[0].ts, 400);
        // Pin "b", then overflow: the oldest *unpinned* item goes, the pinned one stays.
        let b = text_item("b", 0).key;
        h.set_pinned(b, true);
        let gone = h.insert(text_item("d", 500), 3);
        assert_eq!(h.items.len(), 3);
        assert!(h.items.iter().any(|i| i.key == b), "pinned items are never evicted");
        assert!(!h.items.iter().any(|i| i.preview == "c"));
        assert_eq!(gone, vec![text_file(text_item("c", 0).key)]);
        // Display order: pinned first, then newest first.
        let order: Vec<&str> = h.ordered().iter().map(|i| i.preview.as_str()).collect();
        assert_eq!(order, ["b", "d", "a"]);
    }

    #[test]
    fn clipboard_clear_keeps_pinned() {
        let mut h = History::default();
        h.insert(text_item("x", 1), 50);
        h.insert(text_item("y", 2), 50);
        h.set_pinned(text_item("y", 0).key, true);
        let gone = h.clear_unpinned();
        assert_eq!(h.items.len(), 1);
        assert_eq!(h.items[0].preview, "y");
        assert_eq!(gone, vec![text_file(text_item("x", 0).key)]);
    }

    #[test]
    fn clipboard_originals_only_for_pinned_or_latest_image() {
        let mut h = History::default();
        h.insert(image_item(1, 100, true), 50);
        assert!(h.get(1).unwrap().full, "the newest image keeps its original so it can be pinned");
        let gone = h.insert(image_item(2, 200, true), 50);
        assert!(!h.get(1).unwrap().full, "an older unpinned image loses its original");
        assert!(h.get(2).unwrap().full);
        assert_eq!(gone, vec![orig_file(1)]);
        // Pinned images keep theirs.
        h.set_pinned(2, true);
        let gone = h.insert(image_item(3, 300, true), 50);
        assert!(h.get(2).unwrap().full && h.get(3).unwrap().full);
        assert!(gone.is_empty());
        // Unpinning an old image drops its original.
        let gone = h.set_pinned(2, false);
        assert!(!h.get(2).unwrap().full);
        assert_eq!(gone, vec![orig_file(2)]);
        // Copying the same image again keeps the original it already had.
        h.insert(image_item(3, 400, false), 50);
        assert!(h.get(3).unwrap().full);
    }

    #[test]
    fn clipboard_corrupt_index_is_empty_not_a_crash() {
        let dir = std::env::temp_dir().join(format!("moku-clip-test-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let idx = dir.join("index.json");

        assert!(History::load(&idx).items.is_empty(), "missing file");
        for junk in ["", "{", "not json", "[1,2,3]", "{\"items\": 5}", "\u{0}\u{0}\u{0}", "{\"items\":[{\"key\":\"x\"}]}"] {
            std::fs::write(&idx, junk).unwrap();
            assert!(History::load(&idx).items.is_empty(), "{junk:?}");
        }
        std::fs::write(&idx, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        assert!(History::load(&idx).items.is_empty(), "binary garbage");

        // Round trip, including pinned / image items.
        let mut h = History::default();
        h.insert(text_item("hello", 10), 50);
        h.insert(image_item(9, 20, true), 50);
        h.set_pinned(9, true);
        h.save(&idx).unwrap();
        let back = History::load(&idx);
        assert_eq!(back.items, h.items);
        // An unknown field or a future version does not break loading.
        std::fs::write(&idx, r#"{"version":9,"items":[],"extra":true}"#).unwrap();
        assert!(History::load(&idx).items.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clipboard_text_classification_and_preview() {
        assert_eq!(classify_text("https://github.com/moku-app/moku"), Kind::Link);
        assert_eq!(classify_text("  http://a.b/c?d=1  "), Kind::Link);
        assert_eq!(classify_text("see https://x.y for more"), Kind::Text);
        assert_eq!(classify_text("#0A84FF"), Kind::Color);
        assert_eq!(classify_text("#fff"), Kind::Color);
        assert_eq!(classify_text("#12345"), Kind::Text);
        assert_eq!(classify_text("#GGGGGG"), Kind::Text);
        assert_eq!(parse_hex_color("#0A84FF"), Some(0x0A84FF));
        assert_eq!(parse_hex_color("#fa0"), Some(0xFFAA00));
        assert_eq!(parse_hex_color("0A84FF"), None);

        assert_eq!(preview_of("\n\n  hello   \t world \nsecond line"), "hello world");
        assert_eq!(preview_of("   \n  "), "");
        assert_eq!(preview_of(&"x".repeat(1000)).chars().count(), PREVIEW_CHARS);
        // Whitespace-only text is not worth recording.
        assert!(ClipItem::try_text("  \r\n\t ", false, 0).is_none());
        assert!(ClipItem::try_text("a", false, 0).is_some());
    }

    #[test]
    fn clipboard_age_buckets() {
        assert_eq!(age(NOW, NOW - 5), Age::Now);
        assert_eq!(age(NOW, NOW + 30), Age::Now, "clock skew never goes negative");
        assert_eq!(age(NOW, NOW - 120), Age::Minutes(2));
        assert_eq!(age(NOW, NOW - 3 * 3600 - 5), Age::Hours(3));
        assert_eq!(age(NOW, NOW - DAY - 60), Age::Yesterday);
        assert_eq!(age(NOW, NOW - 5 * DAY), Age::Days(5));
    }

    /// A 4K PrintScreen (33 MB of 32-bpp pixels) is recorded: DIBs are gated on pixels, not bytes.
    #[test]
    fn clipboard_dib_gated_on_pixels() {
        fn dib(w: i32, h: i32, pixel_bytes: usize) -> Vec<u8> {
            let mut d = Vec::with_capacity(40 + pixel_bytes);
            d.extend(40u32.to_le_bytes());
            d.extend(w.to_le_bytes());
            d.extend(h.to_le_bytes());
            d.extend(1u16.to_le_bytes());
            d.extend(32u16.to_le_bytes());
            d.extend([0u8; 24]);
            d.resize(40 + pixel_bytes, 0);
            d
        }
        let uhd = dib(3840, 2160, 3840 * 2160 * 4);
        assert!(uhd.len() > MAX_IMAGE_BYTES, "bigger than the PNG byte cap");
        assert!(dib_ok(&uhd));
        assert!(dib_ok(&dib(5120, -2880, 5120 * 2880 * 4)), "5K top-down");
        // Too many pixels, a header that lies about a tiny block, or garbage: refused.
        assert!(!dib_ok(&dib(8000, 4000, 16)));
        assert!(!dib_ok(&dib(100, 100, (MAX_IMAGE_PIXELS * 4) as usize + 128 * 1024)));
        assert!(!dib_ok(b"not a bitmap"));
    }

    #[test]
    fn clipboard_dib_header_parsing() {
        // 2×2, 32 bpp, BI_RGB, bottom-up.
        let mut dib = Vec::new();
        dib.extend(40u32.to_le_bytes());
        dib.extend(2i32.to_le_bytes());
        dib.extend(2i32.to_le_bytes());
        dib.extend(1u16.to_le_bytes());
        dib.extend(32u16.to_le_bytes());
        dib.extend([0u8; 24]); // compression, size, ppm x/y, clr used/important = 0
        dib.extend([7u8; 16]);
        let info = dib_info(&dib).unwrap();
        assert_eq!((info.width, info.height, info.bit_count, info.pixel_offset), (2, 2, 32, 40));
        let bmp = dib_to_bmp(&dib).unwrap();
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize, bmp.len());
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 14 + 40);
        assert_eq!(&bmp[14..], &dib[..]);

        // Top-down DIBs have a negative height.
        let mut td = dib.clone();
        td[8..12].copy_from_slice(&(-2i32).to_le_bytes());
        assert_eq!(dib_info(&td).unwrap().height, 2);

        // BI_BITFIELDS with a 40-byte header has three mask DWORDs before the pixels.
        let mut bf = dib.clone();
        bf[16..20].copy_from_slice(&3u32.to_le_bytes());
        bf.splice(40..40, [0u8; 12]);
        assert_eq!(dib_info(&bf).unwrap().pixel_offset, 52);
        // 8 bpp with no explicit palette size has 256 palette entries.
        let mut p8 = dib.clone();
        p8[14..16].copy_from_slice(&8u16.to_le_bytes());
        assert!(dib_info(&p8).is_none(), "a palette that runs past the end of the data is rejected");
        p8.splice(40..40, [0u8; 1024]);
        assert_eq!(dib_info(&p8).unwrap().pixel_offset, 40 + 1024);

        // Garbage and truncated input.
        assert!(dib_info(&[]).is_none());
        assert!(dib_info(&[1, 2, 3]).is_none());
        assert!(dib_info(&dib[..30]).is_none());
        let mut bad = dib.clone();
        bad[0..4].copy_from_slice(&7u32.to_le_bytes());
        assert!(dib_info(&bad).is_none());
        assert!(dib_to_bmp(&[0u8; 10]).is_none());
    }

    #[test]
    fn clipboard_png_dimensions() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        png.extend(1280u32.to_be_bytes());
        png.extend(720u32.to_be_bytes());
        assert_eq!(png_dims(&png), Some((1280, 720)));
        assert_eq!(png_dims(&png[..20]), None);
        assert_eq!(png_dims(b"GIF89a..................."), None);
    }

    #[test]
    fn clipboard_file_names_are_derived_from_the_key_only() {
        // No user content ever ends up in a path (no traversal possible).
        let t = text_item("../../evil", 0);
        assert_eq!(text_file(t.key).len(), "t".len() + 16 + ".txt".len());
        assert!(text_file(t.key).chars().all(|c| c.is_ascii_alphanumeric() || c == '.'));
        assert_eq!(text_item("same", 1).key, text_item("same", 99).key);
        assert_ne!(text_item("same", 1).key, text_item("other", 1).key);
    }

    // ---- smart actions: tags, colours and one-time codes -------------------------------------

    #[test]
    fn clipboard_functional_colours_are_colour_items() {
        for s in ["rgb(10, 132, 255)", "rgba(10,132,255,0.5)", "hsl(210, 100%, 52%)", "HSL(0 100% 50%)"] {
            assert_eq!(classify_text(s), Kind::Color, "{s}");
        }
        assert_eq!(classify_text("rgb(300, 0, 0)"), Kind::Text);
        assert_eq!(classify_text("rgb(1, 2)"), Kind::Text);
        assert_eq!(classify_text("color: rgb(1, 2, 3)"), Kind::Text);
    }

    #[test]
    fn clipboard_action_tag_is_saved_and_old_indexes_still_load() {
        let mut it = text_item("https://moku.example/a", 5);
        it.act = Some(Act::Url);
        let mut h = History::default();
        h.insert(it.clone(), 50);
        let json = String::from_utf8(h.to_json()).unwrap();
        assert!(json.contains(r#""act":"url""#), "{json}");
        // An index written before the tag existed has no `act` and loads as None.
        let old = r#"{"version":1,"items":[{"key":7,"kind":"text","ts":3,"preview":"hi","bytes":2}]}"#;
        let dir = std::env::temp_dir().join(format!("moku-clip-test-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.json"), old).unwrap();
        let back = History::load(&dir.join("index.json"));
        assert_eq!((back.items.len(), back.items[0].act), (1, None));
        // A plain item does not even write the field.
        assert!(!String::from_utf8(History { items: vec![text_item("x", 1)] }.to_json()).unwrap().contains("act"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn otp_item(code: &str, ts: i64) -> ClipItem {
        ClipItem::otp(code, ts)
    }

    #[test]
    fn clipboard_otp_is_marked_masked_and_has_no_files() {
        let o = otp_item("482913", 100);
        assert!(o.is_otp());
        assert_eq!(o.act, Some(Act::Otp));
        assert!(!o.preview.contains("482913") && !o.preview.is_empty(), "the preview never shows the code: {:?}", o.preview);
        assert_eq!(o.preview.chars().count(), 6, "one dot per digit");
        assert!(o.files().is_empty(), "an OTP never owns a file");
        // Same code, same key as the plain text item (so a repeat copy is recognised).
        assert_eq!(o.key, text_item("482913", 1).key);
    }

    /// The core promise: a one-time code is never written to the index.
    #[test]
    fn clipboard_otp_is_never_serialised_or_loaded() {
        let mut h = History::default();
        h.insert(text_item("hello", 10), 50);
        h.insert_secret(otp_item("482913", 20));
        assert_eq!(h.items.len(), 2);
        let json = String::from_utf8(h.to_json()).unwrap();
        assert!(!json.contains("482913") && !json.contains("otp"), "{json}");
        assert!(!json.contains(&otp_item("482913", 0).key.to_string()), "not even its key: {json}");
        // A hand-edited index that claims an OTP item is cleaned on load.
        let dir = std::env::temp_dir().join(format!("moku-clip-test-otp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sneaky = r#"{"version":1,"items":[{"key":9,"kind":"text","ts":3,"preview":"x","bytes":6,"act":"otp"},{"key":8,"kind":"text","ts":4,"preview":"y","bytes":1}]}"#;
        std::fs::write(dir.join("index.json"), sneaky).unwrap();
        let back = History::load(&dir.join("index.json"));
        assert_eq!(back.items.len(), 1);
        assert_eq!(back.items[0].key, 8);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clipboard_otp_expires_after_a_minute() {
        let mut h = History::default();
        h.insert(text_item("keep me", 1_000), 50);
        h.insert_secret(otp_item("111111", 1_000));
        h.insert_secret(otp_item("222222", 1_030));
        // 59 s after the first one: nothing yet.
        assert!(h.expire_otps(1_059, OTP_TTL_SECS).is_empty());
        // 60 s: the first goes, the second (30 s old) stays.
        let gone = h.expire_otps(1_060, OTP_TTL_SECS);
        assert_eq!(gone, vec![otp_item("111111", 0).key]);
        assert_eq!(h.items.len(), 2);
        // 90 s: the second goes; ordinary items are never touched.
        assert_eq!(h.expire_otps(1_090, OTP_TTL_SECS), vec![otp_item("222222", 0).key]);
        assert_eq!(h.items.len(), 1);
        assert_eq!(h.items[0].preview, "keep me");
        assert!(h.expire_otps(9_999_999, OTP_TTL_SECS).is_empty());
        // The regular day-based expiry does not matter for an OTP and does not break on one.
        h.insert_secret(otp_item("333333", 1_000));
        assert!(h.expire(1_000 + 10, 1).is_empty());
        assert_eq!(h.items.len(), 2);
    }

    #[test]
    fn clipboard_copying_an_otp_again_restarts_its_minute() {
        let mut h = History::default();
        h.insert_secret(otp_item("555555", 1_000));
        h.insert_secret(otp_item("555555", 1_050));
        assert_eq!(h.items.len(), 1, "same code, one row");
        assert!(h.expire_otps(1_070, OTP_TTL_SECS).is_empty(), "20 s since the second copy");
        assert_eq!(h.expire_otps(1_110, OTP_TTL_SECS).len(), 1);
    }

    /// A plain copy of the same digits written earlier (it had no source app) is on disk. When the
    /// same digits later arrive as a code, that file is deleted and the item becomes the in-memory one.
    #[test]
    fn clipboard_a_code_replaces_an_earlier_plain_copy_and_returns_its_file() {
        let mut h = History::default();
        let plain = text_item("482913", 10);
        h.insert(plain.clone(), 50);
        let gone = h.insert_secret(otp_item("482913", 20));
        assert_eq!(gone, vec![text_file(plain.key)]);
        assert_eq!(h.items.len(), 1);
        assert!(h.items[0].is_otp());
    }

    /// The other way round: the code is already held, then the plain-text listener stores the same
    /// digits (the second copy came from an app that is not a source). The new file is thrown away.
    #[test]
    fn clipboard_a_plain_copy_of_a_held_code_stays_a_code() {
        let mut h = History::default();
        h.insert_secret(otp_item("482913", 20));
        let dup = text_item("482913", 30);
        let gone = h.insert(dup.clone(), 50);
        assert_eq!(gone, vec![text_file(dup.key)], "the file just written is deleted");
        assert_eq!(h.items.len(), 1);
        assert!(h.items[0].is_otp());
        assert_eq!(h.items[0].ts, 30, "the minute restarts");
    }

    #[test]
    fn clipboard_otp_cannot_be_pinned() {
        let mut h = History::default();
        h.insert_secret(otp_item("482913", 20));
        let k = h.items[0].key;
        h.set_pinned(k, true);
        assert!(!h.items[0].pinned);
        // And "Kosongkan" removes it like any unpinned item.
        h.clear_unpinned();
        assert!(h.items.is_empty());
    }
}
