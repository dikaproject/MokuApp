//! Files of the clipboard history (`<data dir>\clipboard\`): content-addressed, written
//! atomically, cleaned up when items go away. No Windows API; tested against a temp folder.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::actions;
use super::model::{self, ClipItem};

/// What the clipboard reader found, before it becomes a [`ClipItem`].
pub enum Captured {
    Text { text: String, truncated: bool },
    Image {
        /// PNG of the 128 px thumbnail.
        thumb_png: Vec<u8>,
        w: u32,
        h: u32,
        /// Content hash of the clipboard blob.
        key: u64,
        /// Size of the clipboard blob.
        bytes: u64,
        /// The original as PNG (≤ 8 MB), kept for the newest image so it can be pinned.
        orig_png: Option<Vec<u8>>,
    },
}

/// Write `bytes` to `dir/name` unless it already exists (content-addressed names make an existing
/// file identical). Temp file + rename, so a crash never leaves a half-written file.
pub fn write_if_absent(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let path = dir.join(name);
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)
}

/// Persist the files of a captured item and describe it. `None` when it must not be recorded
/// (empty text) or the files could not be written.
pub fn store(dir: &Path, c: Captured, ts: i64) -> Option<ClipItem> {
    match c {
        Captured::Text { text, truncated } => {
            let item = ClipItem::try_text(&text, truncated, ts)?;
            write_if_absent(dir, &model::text_file(item.key), text.as_bytes()).ok()?;
            Some(item)
        }
        Captured::Image { thumb_png, w, h, key, bytes, orig_png } => {
            write_if_absent(dir, &model::thumb_file(key), &thumb_png).ok()?;
            let full = match &orig_png {
                Some(o) => write_if_absent(dir, &model::orig_file(key), o).is_ok(),
                None => false,
            };
            Some(ClipItem::image(key, w, h, bytes, full, ts))
        }
    }
}

/// What a captured clip became.
pub enum Ingested {
    /// Stored (its text file is on disk). `strong`: its smart action is certain enough for the pill.
    Item { item: ClipItem, strong: bool },
    /// A one-time code: nothing was written anywhere. The item is for the in-memory history and
    /// the `String` is the code (digits only), which the feature keeps in memory until it expires.
    Secret(ClipItem, String),
}

/// Decide what a captured clip is and store it. The detectors run first: a one-time code (when
/// `otp_guard` is on) is never handed to [`store`], so no file and no index entry can ever hold
/// it. Everything else is stored as before and tagged with its smart action, if it has one.
pub fn ingest(dir: &Path, c: Captured, owner_exe: Option<&str>, otp_guard: bool, ts: i64) -> Option<Ingested> {
    let Captured::Text { text, .. } = &c else {
        return store(dir, c, ts).map(|item| Ingested::Item { item, strong: false });
    };
    let found = actions::detect(text, owner_exe);
    if found.is_some_and(|d| d.act == actions::Act::Otp) {
        if otp_guard && let Some(code) = actions::otp::detect(text.trim(), owner_exe) {
            return Some(Ingested::Secret(ClipItem::otp(&code, ts), code));
        }
        // The guard is off: an ordinary item, with no action tag (an `otp` tag means "never saved").
        return store(dir, c, ts).map(|item| Ingested::Item { item, strong: false });
    }
    let mut item = store(dir, c, ts)?;
    item.act = found.map(|d| d.act);
    Some(Ingested::Item { item, strong: found.is_some_and(|d| d.strong) })
}

pub fn read_file(dir: &Path, name: &str) -> Option<Vec<u8>> {
    std::fs::read(dir.join(name)).ok()
}

pub fn read_text(dir: &Path, key: u64) -> Option<String> {
    String::from_utf8(read_file(dir, &model::text_file(key))?).ok()
}

pub fn delete_files(dir: &Path, names: &[String]) {
    for n in names {
        let _ = std::fs::remove_file(dir.join(n));
    }
}

/// Is `name` one of ours (`t|i|o` + 16 hex digits + `.txt|.png`, or a leftover `.tmp` of those)?
fn is_our_file(name: &str) -> bool {
    let base = name.strip_suffix(".tmp").unwrap_or(name);
    let Some((stem, ext)) = base.rsplit_once('.') else { return false };
    let ok_ext = matches!(ext, "txt" | "png");
    let mut chars = stem.chars();
    let ok_head = matches!(chars.next(), Some('t' | 'i' | 'o'));
    ok_ext && ok_head && stem.len() == 17 && chars.all(|c| c.is_ascii_hexdigit())
}

/// Delete files of ours that no item refers to (a crash between writing a file and saving the
/// index, an expired item whose delete failed). Anything with another name is left alone.
pub fn cleanup_orphans(dir: &Path, referenced: &HashSet<String>) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if is_our_file(&name) && !referenced.contains(&name) && std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

pub fn index_path() -> PathBuf {
    moku_core::paths::clipboard_dir().join("index.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::clipboard::model::History;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-clip-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn stores_text_once_and_reads_it_back() {
        let d = tmp("store-text");
        let a = store(&d, Captured::Text { text: "halo".into(), truncated: false }, 5).unwrap();
        let b = store(&d, Captured::Text { text: "halo".into(), truncated: false }, 9).unwrap();
        assert_eq!(a.key, b.key);
        assert_eq!(read_text(&d, a.key).as_deref(), Some("halo"));
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "identical content shares one file");
        assert!(store(&d, Captured::Text { text: "   ".into(), truncated: false }, 1).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stores_images_with_optional_original() {
        let d = tmp("store-image");
        let with = store(&d, Captured::Image { thumb_png: vec![1, 2, 3], w: 10, h: 5, key: 42, bytes: 99, orig_png: Some(vec![9; 20]) }, 1).unwrap();
        assert!(with.full);
        assert_eq!(read_file(&d, &model::orig_file(42)).unwrap().len(), 20);
        let without = store(&d, Captured::Image { thumb_png: vec![1], w: 1, h: 1, key: 43, bytes: 1, orig_png: None }, 1).unwrap();
        assert!(!without.full);
        assert!(!d.join(model::orig_file(43)).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn orphans_are_removed_but_foreign_files_are_not() {
        let d = tmp("orphans");
        let mut h = History::default();
        let keep = store(&d, Captured::Text { text: "keep".into(), truncated: false }, 1).unwrap();
        h.insert(keep.clone(), 50);
        let orphan = store(&d, Captured::Text { text: "orphan".into(), truncated: false }, 1).unwrap();
        std::fs::write(d.join(format!("{}.tmp", model::thumb_file(7))), b"partial").unwrap();
        std::fs::write(d.join("index.json"), b"{}").unwrap();
        std::fs::write(d.join("notes.txt"), b"mine").unwrap();
        std::fs::write(d.join("tZZZZZZZZZZZZZZZZ.txt"), b"not hex").unwrap();

        let n = cleanup_orphans(&d, &h.referenced_files());
        assert_eq!(n, 2, "the unreferenced text file and the stale .tmp");
        assert!(d.join(model::text_file(keep.key)).exists());
        assert!(!d.join(model::text_file(orphan.key)).exists());
        assert!(d.join("index.json").exists() && d.join("notes.txt").exists() && d.join("tZZZZZZZZZZZZZZZZ.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn text(s: &str) -> Captured {
        Captured::Text { text: s.into(), truncated: false }
    }

    fn files_in(d: &Path) -> usize {
        std::fs::read_dir(d).map(|r| r.count()).unwrap_or(0)
    }

    /// The promise of the OTP guard: a code never reaches `store`, so nothing is written, not
    /// even the folder.
    #[test]
    fn ingest_never_writes_a_one_time_code() {
        let d = tmp("ingest-otp");
        let _ = std::fs::remove_dir_all(&d);
        for (t, owner) in [
            ("Kode OTP Anda 482913. Jangan berikan kepada siapapun.", None),
            ("Your verification code is 8456", Some("chrome.exe")),
            ("482913", Some("WhatsApp.exe")),
            ("123-456", Some("PhoneExperienceHost.exe")),
            ("G-123456 is your Google verification code.", None),
            // Digits only, 4 to 8 of them: a code from whatever app copied it (v0.3 fix).
            ("482913", Some("chrome.exe")),
            ("  4829  ", Some("notepad.exe")),
            ("48291357", None),
            // An OTP message up to 1 KB.
            (&format!("Kode OTP Anda 482913. Jangan berikan kepada siapapun. {}", "x".repeat(900)), Some("chrome.exe")),
        ] {
            match ingest(&d, text(t), owner, true, 100).expect(t) {
                Ingested::Secret(item, code) => {
                    assert!(item.is_otp() && item.files().is_empty(), "{t}");
                    assert!(code.chars().all(|c| c.is_ascii_digit()) && (4..=8).contains(&code.len()), "{code}");
                    assert!(!item.preview.chars().any(|c| c.is_ascii_digit()), "the preview must not show digits: {:?}", item.preview);
                }
                Ingested::Item { .. } => panic!("{t} must be held in memory only"),
            }
        }
        assert!(!d.exists(), "no folder, no file: nothing about a code touched the disk");
        // The code is the key of an ordinary copy of the same digits (so a repeat is recognised).
        let Some(Ingested::Secret(item, _)) = ingest(&d, text("Kode OTP 482913"), None, true, 1) else { panic!() };
        assert_eq!(item.key, model::content_key(false, b"482913"));
    }

    #[test]
    fn ingest_stores_everything_else_and_tags_it() {
        let d = tmp("ingest-plain");
        let cases = [
            ("halo dunia", None, false),
            ("https://moku.example/a", Some(actions::Act::Url), true),
            ("#0A84FF", Some(actions::Act::Color), true),
            ("#fff", Some(actions::Act::Color), false),
            ("12*(3+4)", Some(actions::Act::Math), true),
            ("0812-3456-7890", Some(actions::Act::Phone), true),
            (r#"{"a":1,"b":[1,2,3],"c":"some longer text"}"#, Some(actions::Act::Json), true),
            // Digits that are not 4 to 8 long are not a code: stored like any text.
            ("482", None, false),
            ("482913577", None, false),
        ];
        for (t, act, strong) in cases {
            let Some(Ingested::Item { item, strong: s }) = ingest(&d, text(t), Some("chrome.exe"), true, 5) else { panic!("{t}") };
            assert_eq!((item.act, s), (act, strong), "{t}");
            assert_eq!(read_text(&d, item.key).as_deref(), Some(t), "{t} is on disk");
        }
        // Images are untouched by the detectors.
        let img = Captured::Image { thumb_png: vec![1], w: 2, h: 2, key: 5, bytes: 1, orig_png: None };
        assert!(matches!(ingest(&d, img, None, true, 1), Some(Ingested::Item { strong: false, .. })));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ingest_with_the_guard_off_stores_a_code_as_plain_text_without_the_otp_tag() {
        let d = tmp("ingest-guard-off");
        let Some(Ingested::Item { item, strong }) = ingest(&d, text("Kode OTP Anda 482913"), None, false, 1) else { panic!() };
        assert!(!strong && item.act.is_none() && !item.is_otp(), "{:?}", item.act);
        assert_eq!(files_in(&d), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ingest_ignores_whitespace_only_text_like_store_does() {
        let d = tmp("ingest-empty");
        assert!(ingest(&d, text("  \r\n "), None, true, 1).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn delete_files_ignores_missing_names() {
        let d = tmp("delete");
        std::fs::write(d.join("a.txt"), b"x").unwrap();
        delete_files(&d, &["a.txt".into(), "missing.png".into()]);
        assert!(!d.join("a.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
