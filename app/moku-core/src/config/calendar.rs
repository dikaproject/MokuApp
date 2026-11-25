/// Upper bound for the number of calendar addresses (each one is fetched every 15 minutes).
pub const MAX_URLS: usize = 8;
/// Upper bound for one address (browsers and calendar servers stop around 2 KB).
pub const MAX_URL_CHARS: usize = 2048;

/// Accept a list of at most [`MAX_URLS`] non-empty addresses, none longer than [`MAX_URL_CHARS`].
fn urls_ok(v: &Vec<String>) -> Result<(), String> {
    if v.len() > MAX_URLS {
        return Err(format!("{} calendar addresses (at most {MAX_URLS})", v.len()));
    }
    if v.iter().any(|u| u.trim().is_empty() || u.chars().count() > MAX_URL_CHARS) {
        return Err("a calendar address is empty or too long".into());
    }
    Ok(())
}

/// A local file: `file:` address or a `C:\...` / `C:/...` path.
fn is_local(u: &str) -> bool {
    let b = u.as_bytes();
    (u.len() > 5 && u.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("file:"))) || (b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
}

/// A network path: `\\server\share\...`, `//server/share`, `file://server/share/...`, `file:////server/...`.
/// Opening one makes Windows contact that host and offer the user's credentials to it, so such
/// an address is never accepted (only `https:`, `webcal:` and files on a drive letter).
/// `file://localhost/C:/...` is a local file and is fine.
pub fn is_network_path(u: &str) -> bool {
    let u = u.trim();
    if u.starts_with("\\\\") || u.starts_with("//") {
        return true;
    }
    let Some(rest) = u.get(..5).filter(|p| p.eq_ignore_ascii_case("file:")).map(|_| &u[5..]) else { return false };
    let rest = rest.replace('\\', "/");
    if let Some(after) = rest.strip_prefix("///") {
        // file:///C:/x is local; file:////server/share is UNC.
        return after.starts_with('/');
    }
    match rest.strip_prefix("//") {
        Some(authority) => {
            let host = authority.split('/').next().unwrap_or("");
            !host.eq_ignore_ascii_case("localhost")
        }
        None => false,
    }
}

/// Check an address typed in Settings. Returns it trimmed, or why it cannot be used (Indonesian,
/// shown under the field). `existing` are the addresses already saved.
///
/// Usable: `https://`, `webcal://` / `webcals://` (calendar apps hand these out), a local
/// `file:` address, or a Windows path to an exported `.ics`. Plain `http://` is refused: the
/// address is a secret and must not travel in the clear.
pub fn check_url(input: &str, existing: &[String]) -> Result<String, &'static str> {
    let u = input.trim();
    let lower = u.to_ascii_lowercase();
    if u.is_empty() {
        return Err("Tempel alamat iCal terlebih dahulu");
    }
    if u.chars().count() > MAX_URL_CHARS {
        return Err("Alamat terlalu panjang");
    }
    if u.chars().any(char::is_control) {
        return Err("Alamat berisi karakter yang tidak valid");
    }
    if is_network_path(u) {
        return Err("Alamat jaringan (\\\\server\\folder) tidak didukung: pakai berkas di drive lokal atau alamat https://");
    }
    match ["https://", "webcal://", "webcals://"].iter().find(|p| lower.starts_with(**p)) {
        Some(p) => {
            if u.len() <= p.len() || u.contains(char::is_whitespace) || u[p.len()..].starts_with('/') {
                return Err("Alamat tidak lengkap atau mengandung spasi");
            }
        }
        None if is_local(u) => {}
        None if lower.starts_with("http://") => return Err("Pakai alamat https:// (alamat ini rahasia dan tidak boleh lewat koneksi tanpa enkripsi)"),
        None => return Err("Alamat harus diawali https:// atau webcal://"),
    }
    if existing.iter().any(|e| e.trim().eq_ignore_ascii_case(u)) {
        return Err("Alamat ini sudah ada");
    }
    if existing.len() >= MAX_URLS {
        return Err("Sudah ada 8 alamat (batas maksimal)");
    }
    Ok(u.to_string())
}

/// An address for display in Settings: the scheme and host (so you can tell which calendar it
/// is) and the file name when it is harmless; the rest (account, token, query) is hidden.
/// `https://calendar.google.com/calendar/ical/me%40x.com/private-0f9a/basic.ics?k=1` →
/// `https://calendar.google.com/••••••••/basic.ics`.
pub fn mask_url(u: &str) -> String {
    const DOTS: &str = "••••••••";
    let u = u.trim();
    let local = is_local(u);
    // The last path segment, without any query or fragment.
    let path_end = u.find(['?', '#']).unwrap_or(u.len());
    let last = u[..path_end].rsplit(['/', '\\']).next().unwrap_or("");
    // A file name is shown only when it is short and does not look like a token.
    let tokenish = last.split('.').any(|p| p.len() >= 16);
    let name = (!last.is_empty() && last.len() <= 24 && u[..path_end].to_ascii_lowercase().ends_with(".ics") && !tokenish).then_some(last);
    if local {
        let scheme = if u.len() > 5 && u.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("file:")) { "file:" } else { "" };
        return match name {
            Some(n) => format!("{scheme}{DOTS}\\{n}"),
            None => format!("{scheme}{DOTS}"),
        };
    }
    let Some((scheme, rest)) = u.split_once("://") else { return DOTS.to_string() };
    // Anything before the host (user:password@) is part of the secret.
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").rsplit('@').next().unwrap_or("");
    let scheme = if scheme.eq_ignore_ascii_case("webcal") || scheme.eq_ignore_ascii_case("webcals") { "webcal" } else { "https" };
    let has_path = rest.split(['?', '#']).next().is_some_and(|r| r.contains('/'));
    match name {
        Some(n) if has_path => format!("{scheme}://{host}/{DOTS}/{n}"),
        _ if has_path => format!("{scheme}://{host}/{DOTS}"),
        _ => format!("{scheme}://{host}"),
    }
}

section! {
    /// `[calendar]` (iCal / ICS agenda; off by default, no account or OAuth)
    pub struct Calendar in "calendar" {
        enabled: bool = false,
        /// iCal (ICS) addresses: Google "Secret address in iCal format", Outlook "Publish
        /// calendar", any `.ics` link, or a local `file:` path. **These are secrets** (anyone who
        /// has the address can read the calendar): never logged, shown masked in Settings.
        urls: Vec<String> = vec![] => urls_ok,
        /// A heads-up pill appears this many minutes before a meeting starts.
        minutes_before: u32 = 5 => super::range(1, 60),
    }
}

#[cfg(test)]
mod tests {
    use super::{check_url, is_network_path, mask_url};
    use crate::Config;

    #[test]
    fn calendar_defaults_are_off_and_empty() {
        let c = Config::default();
        assert!(!c.calendar.enabled);
        assert!(c.calendar.urls.is_empty());
        assert_eq!(c.calendar.minutes_before, 5);
        assert!(c.downloads.enabled);
    }

    #[test]
    fn calendar_urls_roundtrip_as_is() {
        // The address is a secret but is stored exactly as pasted (masking is a display matter).
        let mut c = Config::default();
        c.calendar.enabled = true;
        c.calendar.urls = vec!["https://calendar.example/ical/abc%40x/private-0123456789/basic.ics".into(), "file:///C:/cal/work.ics".into()];
        c.calendar.minutes_before = 10;
        let (back, w) = Config::parse(&c.to_toml());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back, c);
    }

    #[test]
    fn calendar_bad_values_fall_back_without_echoing_the_address() {
        let (c, w) = Config::parse("[calendar]\nenabled = true\nminutes_before = 0\nurls = [\"https://a.example/x.ics\", \"\"]\n");
        assert!(c.calendar.enabled, "valid neighbours are kept");
        assert_eq!(c.calendar.minutes_before, 5);
        assert!(c.calendar.urls.is_empty(), "a list with an empty entry is replaced by the default");
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().all(|x| !x.message.contains("a.example")), "warnings must not leak addresses: {w:?}");
        let many = format!("[calendar]\nurls = [{}]\n", (0..9).map(|i| format!("\"https://h/{i}.ics\"")).collect::<Vec<_>>().join(","));
        let (c, w) = Config::parse(&many);
        assert!(c.calendar.urls.is_empty());
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn addresses_are_checked_before_they_are_saved() {
        let ok = |s: &str| check_url(s, &[]).unwrap();
        assert_eq!(ok("  https://calendar.example/ical/x/basic.ics "), "https://calendar.example/ical/x/basic.ics");
        assert_eq!(ok("webcal://p01-caldav.example/x.ics"), "webcal://p01-caldav.example/x.ics");
        assert_eq!(ok("file:///C:/Users/Me/cal.ics"), "file:///C:/Users/Me/cal.ics");
        assert_eq!(ok(r"D:\Exports\Work Calendar.ics"), r"D:\Exports\Work Calendar.ics", "spaces are fine in a file path");
        assert_eq!(ok("file://localhost/C:/cal.ics"), "file://localhost/C:/cal.ics");
        // A network path is refused (Windows would contact that host with the user's credentials).
        for unc in [r"\\nas\cal\a.ics", "//nas/cal/a.ics", "file://nas/cal/a.ics", "file:////nas/cal/a.ics", "FILE://NAS/cal/a.ics", r"file:\\nas\cal", r"\\?\UNC\nas\cal\a.ics"] {
            let e = check_url(unc, &[]).unwrap_err();
            assert!(e.contains("jaringan"), "{unc}: {e}");
            assert!(is_network_path(unc), "{unc}");
        }
        for fine in ["file:///C:/cal.ics", "file:C:/cal.ics", "file:/C:/cal.ics", r"D:\x\cal.ics", "https://h/x.ics", "file:日本語.ics"] {
            assert!(!is_network_path(fine), "{fine}");
        }
        for bad in ["", "   ", "calendar.ics", "ftp://x/y.ics", "https://", "https:///x.ics", "https://a b/x.ics", "https://a/\u{1}x", "javascript:alert(1)", "file:", "C:"] {
            assert!(check_url(bad, &[]).is_err(), "{bad:?}");
        }
        let http = check_url("http://plain.example/x.ics", &[]).unwrap_err();
        assert!(http.contains("https"), "{http}");
        assert!(check_url(&format!("https://h/{}", "a".repeat(3000)), &[]).is_err());
        let have = vec!["https://calendar.example/ical/x/basic.ics".to_string()];
        assert_eq!(check_url("HTTPS://Calendar.example/ical/x/basic.ics", &have), Err("Alamat ini sudah ada"));
        assert!(check_url("https://other.example/y.ics", &have).is_ok());
        let full: Vec<String> = (0..8).map(|i| format!("https://h/{i}.ics")).collect();
        assert!(check_url("https://h/new.ics", &full).is_err());
    }

    #[test]
    fn multibyte_input_never_panics() {
        // Byte offsets inside a character must not be sliced (the settings app takes pasted text).
        for s in ["ééééé", "日本語のカレンダー", "filé:x", "https://日本語/カレンダー.ics", "file:日本語.ics", "Cé:\\x","😀😀😀", "é"] {
            let _ = check_url(s, &[]);
            let _ = mask_url(s);
        }
    }

    #[test]
    fn masked_addresses_hide_everything_secret() {
        let cases = [
            ("https://calendar.google.com/calendar/ical/me%40gmail.com/private-0f9a7c1e2b3d4455/basic.ics", "https://calendar.google.com/••••••••/basic.ics"),
            ("https://outlook.office365.com/owa/calendar/6f1c@corp.example/8a7b6c5d4e3f2a1b/calendar.ics", "https://outlook.office365.com/••••••••/calendar.ics"),
            ("webcal://p01-caldav.icloud.example/published/2/AbCdEf123456", "webcal://p01-caldav.icloud.example/••••••••"),
            ("https://example.com/feeds/8f3a9c2b7e1d4a6f90b3.ics?token=SECRET", "https://example.com/••••••••"),
            ("https://user:pass@example.com/cal.ics", "https://example.com/••••••••/cal.ics"),
            ("https://example.com", "https://example.com"),
            ("https://example.com?key=SECRET", "https://example.com"),
            (r"D:\Exports\Work Calendar.ics", "••••••••\\Work Calendar.ics"),
            ("file:///C:/Users/Me/cal.ics", "file:••••••••\\cal.ics"),
        ];
        for (input, want) in cases {
            assert_eq!(mask_url(input), want, "{input}");
        }
        for leak in ["me%40gmail", "private-0f9a", "6f1c@corp", "8a7b6c5d", "AbCdEf", "SECRET", "pass", "8f3a9c2b"] {
            for (input, _) in cases {
                assert!(!mask_url(input).contains(leak), "{leak} leaked from {input}");
            }
        }
        assert_eq!(mask_url(""), "••••••••");
        assert_eq!(mask_url("nonsense"), "••••••••");
    }
}
