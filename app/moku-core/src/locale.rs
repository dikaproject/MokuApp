//! The user's Windows locale, for defaults that depend on the country (the Indonesian holiday
//! calendar and BMKG alerts are on by default for `id-ID` users only).
//!
//! `MOKU_LOCALE` (e.g. `id-ID`) overrides the system value; it exists for tests and screenshots.

/// Is `name` an Indonesian locale name (`id`, `id-ID`, `ID_id`)? Pure; no OS.
pub fn is_indonesian(name: &str) -> bool {
    let n = name.trim();
    let lang = n.split(['-', '_']).next().unwrap_or("");
    lang.eq_ignore_ascii_case("id") || lang.eq_ignore_ascii_case("in")
}

/// The locale name Windows gives this user (`id-ID`), or `MOKU_LOCALE`.
pub fn user_locale_name() -> Option<String> {
    if let Ok(v) = std::env::var("MOKU_LOCALE") {
        if !v.trim().is_empty() {
            return Some(v.trim().to_string());
        }
    }
    system_locale_name()
}

#[cfg(windows)]
fn system_locale_name() -> Option<String> {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..(n as usize - 1).min(buf.len())]))
}

#[cfg(not(windows))]
fn system_locale_name() -> Option<String> {
    None
}

/// The default of the "Indonesian users" switches: on when the user's locale is Indonesian.
pub fn default_on_for_indonesia() -> bool {
    user_locale_name().is_some_and(|n| is_indonesian(&n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indonesian_locale_names() {
        for yes in ["id-ID", "id", "ID-id", "id_ID", " id-ID ", "in-ID"] {
            assert!(is_indonesian(yes), "{yes}");
        }
        for no in ["en-US", "", "idx-XX", "ms-MY", "jv-ID", "de-DE", "-id"] {
            assert!(!is_indonesian(no), "{no}");
        }
    }
}
