//! Smart clipboard actions: what a piece of copied text *is*, and so what Moku can offer for it.
//!
//! | `Act` | Looks like | Offered |
//! |---|---|---|
//! | `Color` | `#0A84FF`, `rgb(…)`, `hsl(…)` | the swatch, copy as the other two notations |
//! | `Url` | `https://…` | QR code, open |
//! | `Math` | `12*(3+4)` | the result, copy it |
//! | `Json` | a minified or ragged object / array | pretty-print to the clipboard |
//! | `Otp` | a one-time code | marked, kept in memory only, gone after 60 s |
//! | `Phone` | `+62 812-3456-7890`, `0812…` | WhatsApp |
//!
//! Detection is pure and conservative: a false positive costs a button nobody wants, so only text
//! that is *entirely* the thing (or, for OTP, an unmistakable message) matches. `Detected::strong`
//! decides whether the short pill is shown on arrival; weaker matches only get buttons on their
//! row. Nothing here logs or stores text.

pub mod color;
pub mod json;
pub mod otp;

use serde::{Deserialize, Serialize};

use crate::features::launcher::calc;

/// The kinds of action a clip can have. Saved in the history index except [`Act::Otp`], which is
/// never saved anywhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Act {
    Color,
    Url,
    Math,
    Json,
    Otp,
    Phone,
}

/// A match: the kind and whether it is certain enough for the pill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Detected {
    pub act: Act,
    pub strong: bool,
}

/// Longest text the detectors look at (everything but JSON is far shorter).
const MAX_SCAN: usize = 2048;

/// What `text` is, if it is something with actions. `owner_exe` is the file name of the app that
/// put it on the clipboard (only used to recognise a bare one-time code from a messaging app).
/// The order matters: an OTP is never also a number sum, a phone number never also a sum.
pub fn detect(text: &str, owner_exe: Option<&str>) -> Option<Detected> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if t.len() > json::MAX_BYTES {
        return None;
    }
    if otp::detect(t, owner_exe).is_some() {
        return Some(Detected { act: Act::Otp, strong: true });
    }
    if json::needs_pretty(t) {
        // A one-member object is hardly worth a pill; the row still offers it.
        let members = t.matches(',').count() + 1;
        return Some(Detected { act: Act::Json, strong: members >= 2 && t.len() >= 24 });
    }
    if t.len() > MAX_SCAN {
        return None;
    }
    if let Some((_, fmt)) = color::parse(t) {
        // `#abc` is as likely a hashtag as a colour.
        let three_digit_hex = fmt == color::Fmt::Hex && t.trim_end_matches(';').trim().len() == 4;
        return Some(Detected { act: Act::Color, strong: !three_digit_hex });
    }
    if url(t).is_some() {
        return Some(Detected { act: Act::Url, strong: true });
    }
    if phone_wa(t).is_some() {
        return Some(Detected { act: Act::Phone, strong: true });
    }
    if math(t).is_some() {
        return Some(Detected { act: Act::Math, strong: true });
    }
    None
}

// ---- URL --------------------------------------------------------------------------------------

/// The URL when the whole text is one `http://` or `https://` address (no spaces, a host).
pub fn url(text: &str) -> Option<String> {
    let t = text.trim();
    if t.len() < 8 || t.len() > MAX_SCAN || t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    let host_name = host.split(':').next().unwrap_or("");
    let ok_host = !host_name.is_empty() && host_name.chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '[') && (host_name.contains('.') || host_name == "localhost" || host_name.starts_with('['));
    ok_host.then(|| t.to_string())
}

/// The host of a URL for display (`github.com`), without scheme, credentials and port.
pub fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = match authority.strip_prefix('[') {
        // An IPv6 literal keeps its brackets and loses the port after them.
        Some(r) => &authority[..r.find(']').map_or(authority.len(), |i| i + 2)],
        None => authority.split(':').next().unwrap_or(authority),
    };
    host.to_string()
}

/// Can this URL be shown as a QR code (byte mode, version 10)?
pub fn qr_fits(text: &str) -> bool {
    text.trim().len() <= crate::ui::qr::MAX_BYTES
}

// ---- phone numbers ----------------------------------------------------------------------------

/// The WhatsApp number (`628123456789`, digits only) when the whole text is an Indonesian mobile
/// number: `+62 812-3456-7890`, `62812…`, `0812 3456 7890`, `(0812) 3456-7890`. Landlines
/// (`021 …`) and foreign numbers do not match; WhatsApp is for mobiles.
pub fn phone_wa(text: &str) -> Option<String> {
    let t = text.trim();
    if t.len() < 10 || t.len() > 24 {
        return None;
    }
    let plus = t.starts_with('+');
    let mut digits = String::new();
    for (i, c) in t.chars().enumerate() {
        match c {
            '0'..='9' => digits.push(c),
            '+' if i == 0 => {}
            ' ' | '-' | '.' | '(' | ')' => {}
            _ => return None,
        }
    }
    let national = if let Some(r) = digits.strip_prefix("62") {
        // `+62 0812…` (people keep the trunk zero) is read as `+62 812…`.
        let r = r.strip_prefix('0').unwrap_or(r);
        // Without a plus, `62` must be followed by the mobile prefix (a plain number like
        // 6281234567890 is fine; 62-anything-else is not a phone number).
        if !plus && !r.starts_with('8') {
            return None;
        }
        r.to_string()
    } else if !plus && let Some(r) = digits.strip_prefix('0') {
        r.to_string()
    } else {
        return None;
    };
    // Mobile numbers: 8xx followed by 7-10 more digits (9-12 digits in all).
    let ok = national.starts_with('8') && (9..=12).contains(&national.len());
    ok.then(|| format!("62{national}"))
}

/// `https://wa.me/<number>` for [`phone_wa`].
pub fn wa_link(wa: &str) -> String {
    format!("https://wa.me/{wa}")
}

/// The number as people write it: `+62 812-3456-7890` (for the row's subtitle).
pub fn phone_display(wa: &str) -> String {
    let n = wa.strip_prefix("62").unwrap_or(wa);
    let (a, rest) = n.split_at(3.min(n.len()));
    let (b, c) = rest.split_at(4.min(rest.len()));
    let mut out = format!("+62 {a}");
    if !b.is_empty() {
        out.push('-');
        out.push_str(b);
    }
    if !c.is_empty() {
        out.push('-');
        out.push_str(c);
    }
    out
}

// ---- arithmetic -------------------------------------------------------------------------------

/// The value when the whole text is a sum with a clear operator (`12*(3+4)`, `10 - 3`, `2^10`,
/// `1,5 x 4`), reusing the launcher's calculator. A lone number, a date (`25/11/2024`), a time,
/// a phone number or an id with leading zeros is not one; neither is `5-3` or `3/4` without
/// spaces (a range or a fraction).
pub fn math(text: &str) -> Option<f64> {
    let t = text.trim().trim_start_matches('=').trim();
    if t.len() < 3 || t.len() > 100 || t.contains('\n') {
        return None;
    }
    let cs: Vec<char> = t.chars().collect();
    if !cs.iter().all(|c| c.is_ascii_digit() || c.is_whitespace() || "+-*/×÷^%().,xX·−–".contains(*c)) {
        return None;
    }
    // A number with a leading zero and more digits is an id or a phone number ("0812").
    let mut run = String::new();
    for c in cs.iter().chain(std::iter::once(&' ')) {
        if c.is_ascii_digit() {
            run.push(*c);
        } else {
            if run.len() > 1 && run.starts_with('0') {
                return None;
            }
            run.clear();
        }
    }
    // `1,000` / `1.234`: a thousands separator or a decimal? The calculator reads a decimal, which
    // would be a wrong answer for someone who meant a thousand, so leave those alone.
    for (i, c) in cs.iter().enumerate() {
        if (*c == '.' || *c == ',') && cs[i + 1..].iter().take_while(|d| d.is_ascii_digit()).count() == 3 {
            return None;
        }
    }
    // A clear binary operator: + * × ÷ ^ x between operands, a parenthesis, or a spaced - or /.
    let mut clear = false;
    for (i, c) in cs.iter().enumerate() {
        let before = cs[..i].iter().rev().find(|c| !c.is_whitespace());
        let after = cs[i + 1..].iter().find(|c| !c.is_whitespace());
        let operand_before = before.is_some_and(|b| b.is_ascii_digit() || *b == ')' || *b == '%');
        let operand_after = after.is_some_and(|a| a.is_ascii_digit() || *a == '(' || *a == '.' || *a == ',' || *a == '-' || *a == '−');
        let spaced = cs.get(i.wrapping_sub(1)).is_some_and(|p| p.is_whitespace()) && cs.get(i + 1).is_some_and(|n| n.is_whitespace());
        match c {
            '+' | '*' | '×' | '÷' | '^' | '·' if operand_before && operand_after => clear = true,
            'x' | 'X' if operand_before && operand_after => clear = true,
            '-' | '−' | '–' | '/' if operand_before && operand_after && spaced => clear = true,
            '(' if i > 0 && operand_before => clear = true,
            _ => {}
        }
    }
    if !clear {
        return None;
    }
    calc::eval(t).ok()
}

/// The result as text, with a decimal comma and `.` for thousands in Indonesian, the other way
/// round in English; nothing for `plain` (what goes on the clipboard has no grouping).
pub fn math_text(v: f64, comma: bool, group: bool) -> String {
    calc::format(v, comma, group)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(s: &str) -> Option<Act> {
        detect(s, None).map(|d| d.act)
    }

    #[test]
    fn urls() {
        for ok in ["https://github.com/moku-app/moku", "http://localhost:3000/path?x=1#y", "HTTPS://Example.COM", "  https://a.co/x  ", "https://user:pw@host.example:8443/a", "http://192.168.1.10:8080", "https://[::1]:8080/x"] {
            assert!(url(ok).is_some(), "{ok}");
            assert_eq!(act(ok), Some(Act::Url), "{ok}");
        }
        for bad in ["see https://x.y for more", "https://", "https://nodot", "http://", "ftp://files.example.com", "example.com", "javascript:alert(1)", "https:// spaced.com", "mailto:a@b.co", "https://.com"] {
            assert!(url(bad).is_none(), "{bad}");
        }
        assert_eq!(host_of("https://user:pw@GitHub.com:8443/a/b?c"), "GitHub.com");
        assert_eq!(host_of("http://[::1]:8080/x"), "[::1]");
        assert!(qr_fits("https://example.com") && !qr_fits(&format!("https://x.co/{}", "a".repeat(300))));
    }

    #[test]
    fn indonesian_phone_numbers() {
        for (t, want) in [
            ("+62 812-3456-7890", "6281234567890"),
            ("+6281234567890", "6281234567890"),
            ("6281234567890", "6281234567890"),
            ("62 812 3456 7890", "6281234567890"),
            ("0812 3456 7890", "6281234567890"),
            ("0812-3456-7890", "6281234567890"),
            ("081234567890", "6281234567890"),
            ("(0812) 3456-7890", "6281234567890"),
            ("08123456789", "628123456789"),
            ("0896.1234.5678", "6289612345678"),
            ("+62 0812 3456 7890", "6281234567890"),
            ("  +62 811 234 567  ", "62811234567"),
        ] {
            assert_eq!(phone_wa(t).as_deref(), Some(want), "{t}");
            assert_eq!(act(t), Some(Act::Phone), "{t}");
        }
        for bad in [
            "021 5551234",
            "(021) 555-1234",
            "+1 202 555 0100",
            "+44 20 7946 0958",
            "0812345",
            "08123456789012345",
            "62 21 555 1234",
            "08.12.2024",
            "25-11-2024",
            "12345678901",
            "+62 21 5551234",
            "0812 3456 78x0",
            "Telp 0812 3456 7890",
            "",
        ] {
            assert_eq!(phone_wa(bad), None, "{bad}");
        }
        assert_eq!(wa_link("6281234567890"), "https://wa.me/6281234567890");
        assert_eq!(phone_display("6281234567890"), "+62 812-3456-7890");
        assert_eq!(phone_display("62811234567"), "+62 811-2345-67");
    }

    #[test]
    fn arithmetic() {
        for (t, want) in [
            ("2+2", 4.0),
            ("12*(3+4)", 84.0),
            ("10 - 3", 7.0),
            ("100 / 4", 25.0),
            ("2^10", 1024.0),
            ("1,5 x 4", 6.0),
            ("3×4", 12.0),
            ("=7*6", 42.0),
            ("(1+2)*3", 9.0),
            ("200 * 10%", 20.0),
            ("1.5+1.5", 3.0),
            ("  8 ÷ 2  ", 4.0),
        ] {
            assert_eq!(math(t), Some(want), "{t}");
        }
        for bad in [
            "2024", "5-3", "3/4", "25/11/2024", "2024-11-25", "10:30", "0812+3456", "007+1", "1 2", "1+", "(1+2", "abc", "1+a", "50%", "-5", "12", "x264", "1,000+2", "+62 812 3456", "3.14.15", "", "1/0", "a=b+c",
        ] {
            assert_eq!(math(bad), None, "{bad}");
        }
        assert_eq!(math_text(1234.5, true, true), "1.234,5");
        assert_eq!(math_text(1234.5, false, false), "1234.5");
    }

    #[test]
    fn detection_order_and_strength() {
        assert_eq!(detect("#0A84FF", None), Some(Detected { act: Act::Color, strong: true }));
        assert_eq!(detect("#fff", None), Some(Detected { act: Act::Color, strong: false }), "a hashtag could look like this");
        assert_eq!(detect("rgb(10, 132, 255)", None), Some(Detected { act: Act::Color, strong: true }));
        assert_eq!(detect("hsl(210, 100%, 52%)", None).map(|d| d.act), Some(Act::Color));
        assert_eq!(detect("https://moku.example", None), Some(Detected { act: Act::Url, strong: true }));
        assert_eq!(detect("12*(3+4)", None), Some(Detected { act: Act::Math, strong: true }));
        assert_eq!(detect("0812 3456 7890", None), Some(Detected { act: Act::Phone, strong: true }));
        // OTP first: a code message, and a bare code from a messaging app.
        assert_eq!(detect("Kode OTP Anda 482913", None), Some(Detected { act: Act::Otp, strong: true }));
        assert_eq!(detect("482913", Some("WhatsApp.exe")), Some(Detected { act: Act::Otp, strong: true }));
        assert_eq!(detect("482913", Some("chrome.exe")), Some(Detected { act: Act::Otp, strong: true }), "digits only: a code from whatever app");
        assert_eq!(detect("12345", None).map(|d| d.act), Some(Act::Otp));
        // JSON: strong when it has several members and is long enough; a small one is weak; a
        // pretty one needs nothing.
        let blob = r#"{"name":"moku","version":"0.3.0","features":["ocr","clipboard"]}"#;
        assert_eq!(detect(blob, None), Some(Detected { act: Act::Json, strong: true }));
        assert_eq!(detect(r#"{"a":1}"#, None), Some(Detected { act: Act::Json, strong: false }));
        assert_eq!(detect("{}", None), None);
        assert_eq!(detect("{\n  \"a\": 1\n}", None), None, "already pretty");
        // Plain text, numbers and prose are nothing.
        for s in ["", "   ", "halo dunia", "Rapat jam 10.00", "123", "123456789", "see https://x.y later", "Rp 15.000", "a, b, c"] {
            assert_eq!(act(s), None, "{s:?}");
        }
        // Whitespace around is fine.
        assert_eq!(act("\n  https://moku.example/a \r\n"), Some(Act::Url));
    }
}
