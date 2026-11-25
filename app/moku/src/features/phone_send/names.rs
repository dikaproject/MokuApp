//! File names that come from outside (a phone's upload, an offered file) and the names Moku
//! gives its own outputs. Pure string logic plus helpers that create a file without ever
//! replacing an existing one.
//!
//! * [`sanitize`]: whatever a client sent becomes ONE safe Windows file name: no path (so no
//!   traversal), no reserved device names, no `:` (alternate data streams), no control or
//!   direction-override characters, no trailing dots / spaces.
//! * [`numbered`] / [`moku_variant`] / [`create_unique`]: `photo.jpg` -> `photo (2).jpg` for a
//!   received file, `photo (Moku).jpg` -> `photo (Moku 2).jpg` for a converted one. The file is
//!   created with `create_new`, so even a race with another writer never overwrites.
//! * [`content_disposition`]: the header of a download, with an ASCII fallback and an RFC 5987
//!   `filename*`.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// Used when nothing usable is left of a name.
pub const FALLBACK: &str = "file";
/// Longest name kept (UTF-16 units, like the file system counts them). A path is limited to 260
/// characters on many Windows setups, and the folder takes part of that.
pub const MAX_NAME: usize = 100;

/// Characters Windows does not allow in a file name, and the separators. `:` is in the list on
/// purpose: `a.txt:evil.exe` would be an alternate data stream.
const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

fn is_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}'   // zero-width, LRM, RLM
        | '\u{202A}'..='\u{202E}'   // embeddings and overrides (the "exe" spoofing trick)
        | '\u{2060}'..='\u{206F}'   // word joiner, isolates, deprecated format characters
        | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}'
        | '\u{E0000}'..='\u{E007F}' // tags
    )
}

/// `CON`, `nul`, `COM3`, `lpt9`, `CONIN$` ... (Windows treats them as devices whatever the extension).
pub fn is_reserved_stem(stem: &str) -> bool {
    // "con .txt" and "con..txt" are the device too: trailing spaces and dots of the stem do not count.
    let s = stem.trim_end_matches([' ', '.']).to_uppercase();
    if matches!(s.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$") {
        return true;
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() == 4 && (s.starts_with("COM") || s.starts_with("LPT")) {
        // COM0..COM9 and the superscript digits COM1 2 3 (all reserved).
        let d = chars[3];
        return d.is_ascii_digit() || matches!(d, '\u{00B9}' | '\u{00B2}' | '\u{00B3}');
    }
    false
}

/// The stem (up to the first dot) and the rest (starting at that dot, or empty). A leading dot
/// belongs to the stem (".gitignore").
fn split_stem(name: &str) -> (&str, &str) {
    let skip = name.chars().next().map_or(0, char::len_utf8);
    match name[skip..].find('.') {
        Some(i) => name.split_at(skip + i),
        None => (name, ""),
    }
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn truncate_utf16(s: &str, max: usize) -> String {
    let mut n = 0;
    let mut out = String::new();
    for c in s.chars() {
        n += c.len_utf16();
        if n > max {
            break;
        }
        out.push(c);
    }
    out
}

/// One safe file name from anything a client may send. Never empty, never a path.
pub fn sanitize(raw: &str) -> String {
    // 1. Only the last path component counts (both separators; `..\..\x.dll` -> `x.dll`).
    let last = raw.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("");
    // 2. Replace what Windows forbids and every control character; drop invisible format characters.
    let s: String = last.chars().filter(|c| !is_format_char(*c)).map(|c| if c.is_control() || FORBIDDEN.contains(&c) { '_' } else { c }).collect();
    // 3. No leading spaces, no trailing dots or spaces (Windows ignores them: "setup.exe. " is setup.exe).
    let mut s = s.trim_start().trim_end_matches(['.', ' ']).to_string();
    if s.is_empty() {
        return FALLBACK.to_string();
    }
    // 4. Device names are reserved with any extension.
    if is_reserved_stem(split_stem(&s).0) {
        s.insert(0, '_');
    }
    // 5. Length: shorten the stem, keep the extension.
    if utf16_len(&s) > MAX_NAME {
        let (stem, ext) = split_stem(&s);
        let ext = truncate_utf16(ext, 20);
        let room = MAX_NAME.saturating_sub(utf16_len(&ext)).max(1);
        let stem = truncate_utf16(stem, room);
        s = format!("{stem}{ext}").trim_end_matches(['.', ' ']).to_string();
        if s.is_empty() {
            s = FALLBACK.to_string();
        }
    }
    s
}

/// `name`, then `name (2).ext`, `name (3).ext` ... (`n` starts at 1 = the plain name).
pub fn numbered(name: &str, n: u32) -> String {
    if n <= 1 {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    format!("{stem} ({n}){ext}")
}

/// The name of a converted file: `photo.png` -> `photo (Moku).jpg` (`n` = 1), `photo (Moku 2).jpg`
/// (`n` = 2) .... `new_ext` has no dot; `None` keeps the extension.
pub fn moku_variant(name: &str, new_ext: Option<&str>, n: u32) -> String {
    let (stem, ext) = split_ext(name);
    let ext = match new_ext {
        Some(e) => format!(".{}", e.trim_start_matches('.')),
        None => ext.to_string(),
    };
    if n <= 1 { format!("{stem} (Moku){ext}") } else { format!("{stem} (Moku {n}){ext}") }
}

/// (stem, extension with its dot): the LAST dot splits (`a.tar.gz` -> `a.tar`, `.gz`); a leading
/// dot only (`.gitignore`) is no extension.
pub fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

/// Create a new file in `dir` named by `name_for(1)`, `name_for(2)` ... until one did not exist.
/// `create_new` makes "does not exist" and "create" one atomic step: an existing file is never
/// opened, truncated or replaced. Returns the open file and its path.
pub fn create_unique(dir: &Path, mut name_for: impl FnMut(u32) -> String) -> io::Result<(File, PathBuf)> {
    for n in 1..=10_000u32 {
        let path = dir.join(name_for(n));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free name"))
}

/// Rename `from` to `to` and fail if `to` exists (`MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`).
pub fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use windows::Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW};
    use windows::core::HSTRING;
    unsafe { MoveFileExW(&HSTRING::from(from.as_os_str()), &HSTRING::from(to.as_os_str()), MOVE_FILE_FLAGS(0)) }
        // The HRESULT of a Win32 error carries the code in its low word:
        // ERROR_ALREADY_EXISTS (183) / ERROR_FILE_EXISTS (80) become `AlreadyExists`.
        .map_err(|e| io::Error::from_raw_os_error((e.code().0 & 0xFFFF) as i32))
}

/// The text of the `Zone.Identifier` stream Windows reads as "this file came from the Internet".
pub const MOTW: &str = "[ZoneTransfer]
ZoneId=3
";

/// Mark `path` as downloaded from the Internet (Mark-of-the-Web): the `Zone.Identifier` alternate
/// data stream with `ZoneId=3`. A file from a phone is not a file the user made, so SmartScreen,
/// Office's Protected View and the "this file came from another computer" prompt apply to it like
/// to a browser download. Only NTFS has streams: on another file system this fails, and the
/// caller carries on (the file is already complete; it just has no mark).
pub fn mark_from_internet(path: &Path) -> io::Result<()> {
    let mut stream = path.as_os_str().to_os_string();
    stream.push(":Zone.Identifier");
    std::fs::write(PathBuf::from(stream), MOTW)
}

/// Percent-encode for `filename*=UTF-8''...`: everything but RFC 5987 attr-chars.
fn pct(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `attachment; filename="..."; filename*=UTF-8''...` for an already sanitised `name`. The ASCII
/// fallback never contains a quote, backslash, percent sign or anything non-ASCII, so it can not
/// end the header value or the quoted string early.
pub fn content_disposition(name: &str) -> String {
    let ascii: String = name.chars().map(|c| if c.is_ascii() && !c.is_ascii_control() && !matches!(c, '"' | '\\' | '%' | ';') { c } else { '_' }).collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{}", pct(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-phone-names-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_received_file_gets_the_mark_of_the_web() {
        let d = scratch("motw");
        let f = d.join("photo.jpg");
        std::fs::write(&f, b"data").unwrap();
        mark_from_internet(&f).unwrap();
        // The stream is there, says "Internet", and the file itself is untouched.
        let mut stream = f.as_os_str().to_os_string();
        stream.push(":Zone.Identifier");
        let text = std::fs::read_to_string(PathBuf::from(stream)).unwrap();
        assert_eq!(text, "[ZoneTransfer]
ZoneId=3
");
        assert_eq!(std::fs::read(&f).unwrap(), b"data");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn traversal_and_hostile_names_become_one_safe_name() {
        let table: &[(&str, &str)] = &[
            // traversal and absolute paths: only the last component stays
            ("../../Windows/System32/evil.dll", "evil.dll"),
            (r"..\..\evil.dll", "evil.dll"),
            (r"C:\Windows\system32\cmd.exe", "cmd.exe"),
            (r"\\server\share\f.txt", "f.txt"),
            ("//host/x.txt", "x.txt"),
            ("/etc/passwd", "passwd"),
            ("photos/2024/", "2024"),
            ("a/../b.txt", "b.txt"),
            // nothing left
            ("", "file"),
            (".", "file"),
            ("..", "file"),
            ("...", "file"),
            ("   ", "file"),
            ("../", "file"),
            ("/", "file"),
            // alternate data streams and forbidden characters
            ("notes.txt:evil.exe", "notes.txt_evil.exe"),
            ("a.txt::$DATA", "a.txt__$DATA"),
            ("a<b>c|d?e*f\"g.txt", "a_b_c_d_e_f_g.txt"),
            // control characters and NUL
            ("a\0b.png", "a_b.png"),
            ("a\r\nb.png", "a__b.png"),
            ("tab\there.txt", "tab_here.txt"),
            // trailing dots and spaces are ignored by Windows: they must not hide an extension
            ("setup.exe.", "setup.exe"),
            ("setup.exe . ", "setup.exe"),
            ("  lead.txt", "lead.txt"),
            // hidden-file style names are fine
            (".gitignore", ".gitignore"),
            // ordinary names, spaces, unicode, emoji
            ("laporan Q4 final.pdf", "laporan Q4 final.pdf"),
            ("Foto liburan \u{1F334}.jpg", "Foto liburan \u{1F334}.jpg"),
            ("\u{65E5}\u{672C}\u{8A9E}.txt", "\u{65E5}\u{672C}\u{8A9E}.txt"),
            ("a.tar.gz", "a.tar.gz"),
        ];
        for (raw, want) in table {
            assert_eq!(sanitize(raw), *want, "{raw:?}");
        }
    }

    #[test]
    fn reserved_device_names_are_defused_with_any_extension_and_case() {
        for raw in [
            "CON",
            "con",
            "Con.txt",
            "PRN",
            "AUX.tar.gz",
            "NUL",
            "nul.png",
            "COM1",
            "com9.txt",
            "LPT1",
            "lpt9.dat",
            "COM0",
            "CONIN$",
            "CONOUT$",
            "con .txt",
            "com\u{00B9}.txt",
            "LPT\u{00B2}",
            r"C:\x\NUL.txt",
            "../CON",
        ] {
            let s = sanitize(raw);
            assert!(s.starts_with('_'), "{raw:?} -> {s:?}");
            assert!(!is_reserved_stem(split_stem(&s).0), "{raw:?} -> {s:?}");
        }
        // Names that merely start like a device are untouched.
        for raw in ["console.txt", "COM10", "COMX", "LPT", "auxiliary.pdf", "nullable.rs", "CON-1.txt", "prn1.txt"] {
            assert_eq!(sanitize(raw), raw, "{raw}");
        }
    }

    #[test]
    fn direction_overrides_and_invisible_characters_cannot_spoof_an_extension() {
        // "invoice<RLO>fdp.exe" displays as "invoiceexe.pdf".
        assert_eq!(sanitize("invoice\u{202E}fdp.exe"), "invoicefdp.exe");
        assert_eq!(sanitize("a\u{200B}b\u{FEFF}c.txt"), "abc.txt");
        assert_eq!(sanitize("x\u{2066}y\u{2069}.txt"), "xy.txt");
        // Other C1 control characters become underscores.
        assert_eq!(sanitize("a\u{0085}b.txt"), "a_b.txt");
    }

    #[test]
    fn long_names_keep_their_extension_and_stay_within_the_limit() {
        let long = format!("{}.jpeg", "x".repeat(400));
        let s = sanitize(&long);
        assert!(utf16_len(&s) <= MAX_NAME, "{}", utf16_len(&s));
        assert!(s.ends_with(".jpeg"));
        // An absurdly long "extension" is cut too, and the result is still a name.
        let s = sanitize(&format!("a.{}", "e".repeat(500)));
        assert!(utf16_len(&s) <= MAX_NAME && !s.is_empty());
        // Surrogate pairs are never split.
        let s = sanitize(&"\u{1F334}".repeat(200));
        assert!(utf16_len(&s) <= MAX_NAME);
        assert!(s.chars().all(|c| c == '\u{1F334}'));
    }

    #[test]
    fn sanitize_is_idempotent_and_never_yields_a_path() {
        for raw in ["a.txt", "../../x", "con", "x:y", "  ..  ", "a\u{202E}b.exe", "\u{65E5}\u{672C}.txt", "\\\\?\\C:\\x"] {
            let once = sanitize(raw);
            assert_eq!(sanitize(&once), once, "{raw:?}");
            assert!(!once.contains(['/', '\\', ':', '\0']) && !once.is_empty());
        }
    }

    #[test]
    fn numbered_and_moku_names() {
        assert_eq!(numbered("photo.jpg", 1), "photo.jpg");
        assert_eq!(numbered("photo.jpg", 2), "photo (2).jpg");
        assert_eq!(numbered("a.tar.gz", 3), "a.tar (3).gz");
        assert_eq!(numbered("noext", 2), "noext (2)");
        assert_eq!(numbered(".gitignore", 2), ".gitignore (2)");
        assert_eq!(moku_variant("photo.png", Some("jpg"), 1), "photo (Moku).jpg");
        assert_eq!(moku_variant("photo.png", Some("jpg"), 2), "photo (Moku 2).jpg");
        assert_eq!(moku_variant("photo.png", Some(".jpg"), 3), "photo (Moku 3).jpg");
        assert_eq!(moku_variant("clip.mov", None, 1), "clip (Moku).mov");
        assert_eq!(moku_variant("noext", Some("mp3"), 1), "noext (Moku).mp3");
        assert_eq!(moku_variant("My Song.v2.wav", Some("m4a"), 1), "My Song.v2 (Moku).m4a");
    }

    #[test]
    fn create_unique_never_replaces_an_existing_file() {
        let dir = scratch("unique");
        let (mut f, p1) = create_unique(&dir, |n| numbered("a.txt", n)).unwrap();
        io::Write::write_all(&mut f, b"first").unwrap();
        drop(f);
        let (mut f, p2) = create_unique(&dir, |n| numbered("a.txt", n)).unwrap();
        io::Write::write_all(&mut f, b"second").unwrap();
        drop(f);
        let (_, p3) = create_unique(&dir, |n| numbered("a.txt", n)).unwrap();
        assert_eq!(p1.file_name().unwrap(), "a.txt");
        assert_eq!(p2.file_name().unwrap(), "a (2).txt");
        assert_eq!(p3.file_name().unwrap(), "a (3).txt");
        assert_eq!(std::fs::read(&p1).unwrap(), b"first", "the first file is untouched");
        assert_eq!(std::fs::read(&p2).unwrap(), b"second");
        // The converter's naming goes through the same helper.
        std::fs::write(dir.join("p (Moku).jpg"), b"mine").unwrap();
        let (_, p) = create_unique(&dir, |n| moku_variant("p.png", Some("jpg"), n)).unwrap();
        assert_eq!(p.file_name().unwrap(), "p (Moku 2).jpg");
        assert_eq!(std::fs::read(dir.join("p (Moku).jpg")).unwrap(), b"mine");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn move_no_replace_refuses_to_overwrite() {
        let dir = scratch("move");
        std::fs::write(dir.join("a.part"), b"new").unwrap();
        std::fs::write(dir.join("a.txt"), b"old").unwrap();
        let e = move_no_replace(&dir.join("a.part"), &dir.join("a.txt")).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{e}");
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"old");
        assert!(dir.join("a.part").exists());
        move_no_replace(&dir.join("a.part"), &dir.join("b.txt")).unwrap();
        assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), b"new");
        assert!(!dir.join("a.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_disposition_cannot_break_out_of_the_header() {
        let h = content_disposition("laporan Q4.pdf");
        assert_eq!(h, "attachment; filename=\"laporan Q4.pdf\"; filename*=UTF-8''laporan%20Q4.pdf");
        let h = content_disposition("\u{65E5}\u{672C}\u{8A9E} \"x\".txt");
        assert!(h.starts_with("attachment; filename=\"___ _x_.txt\"; filename*=UTF-8''"), "{h}");
        assert!(h.contains("%E6%97%A5"));
        // Even a name that was NOT sanitised (it always is) cannot inject a line.
        let h = content_disposition("a\r\nSet-Cookie: x=1");
        assert!(!h.contains('\r') && !h.contains('\n'));
        assert!(h.is_ascii());
    }
}
