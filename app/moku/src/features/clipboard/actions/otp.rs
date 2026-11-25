//! One-time codes (OTP) in copied text.
//!
//! A code is 4-8 digits (`123456`, `123-456`, `123 456`). It counts as an OTP when
//!
//! * the text (trimmed) is **only digits, 4 to 8 of them**: `482913`. **Whatever app copied it**: a
//!   browser tab, an authenticator and a notes app copy codes too, and a code that leaked into the
//!   history is worse than a number that is only hidden (it is shown as dots, held in memory and
//!   dropped after 60 s; the cost is that a PIN, a year or a short amount is treated the same way), or
//! * the text is two groups of digits (`123-456`, `123 456`) and the app that copied it is a
//!   messaging / phone-link app (the owner of the clipboard), or
//! * the text is a message (up to 1 KB) that says so: a keyword like "OTP", "kode verifikasi" or
//!   "verification code" with a single number in it, or "code" / "kode" / "pin" / "token"
//!   directly in front of the number ("Your code is 482913").
//!
//! Ordinary text with a number in it ("Kode pos 40115", "error code 404", "pada 2024") is not an
//! OTP. Pure (no Windows API).

/// Apps whose clipboard output is treated as a code when it is only digits. Lower case, without
/// `.exe`.
pub const SOURCE_APPS: &[&str] = &["phoneexperiencehost", "yourphone", "whatsapp", "whatsapp.root", "telegram", "signal", "authy desktop", "messenger", "wechat"];

/// Phrases that make a message an OTP message by themselves.
const STRONG: &[&str] = &[
    "otp",
    "kode verifikasi",
    "verification code",
    "kode keamanan",
    "security code",
    "kode konfirmasi",
    "confirmation code",
    "kode login",
    "login code",
    "kode masuk",
    "one-time",
    "one time",
    "sekali pakai",
    "passcode",
    "kode rahasia",
    "authentication code",
    "kode otentikasi",
    "kode autentikasi",
    "do not share",
    "don't share",
    "jangan bagikan",
    "jangan berikan",
    "jangan beritahukan",
    "jangan sebarkan",
];

/// Words that only count when the number comes right after them.
const WEAK: &[&str] = &["code", "kode", "pin", "token"];

/// Words that, in front of a weak keyword, say it is not a one-time code ("error code 5000").
const NOT_A_CODE_BEFORE: &[&str] = &["error", "exit", "status", "http", "zip", "post", "postal", "pos", "area", "country", "promo", "voucher", "kupon", "referral", "invite", "source", "response", "return", "key", "product", "item", "order"];

/// Little words between a keyword and its number ("kode OTP Anda adalah 482913").
const FILLER: &[&str] = &["is", "adalah", "anda", "kamu", "mu", "your", "you", "ini", "yaitu", "no", "nomor", "number", "for", "untuk", "login", "masuk"];

/// Longest message looked at (1 KB: a long bank SMS or an e-mail's first lines).
const MAX_TEXT: usize = 1024;

pub fn is_source_app(exe: &str) -> bool {
    let name = exe.trim().rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);
    SOURCE_APPS.contains(&stem)
}

/// The code when the whole text is only digits, 4 to 8 of them (after trimming): `482913`. Any
/// source app.
pub fn digits_only(text: &str) -> Option<String> {
    let t = text.trim();
    ((4..=8).contains(&t.len()) && t.chars().all(|c| c.is_ascii_digit())).then(|| t.to_string())
}

/// The code when the whole text is one: `123456`, `123-456`, `123 456`, `1234-5678`.
pub fn bare_code(text: &str) -> Option<String> {
    let t = text.trim();
    if t.len() > 9 {
        return None;
    }
    let (digits, groups) = grouped_digits(t)?;
    ((4..=8).contains(&digits.len()) && matches!(groups.as_slice(), [_] | [3, 3] | [4, 4])).then_some(digits)
}

/// `(digits, group sizes)` of a string made only of digit groups joined by single `-` or spaces.
fn grouped_digits(t: &str) -> Option<(String, Vec<usize>)> {
    let mut digits = String::new();
    let mut groups = vec![];
    for g in t.split(['-', ' ']) {
        if g.is_empty() || !g.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        groups.push(g.len());
        digits.push_str(g);
    }
    (groups.len() <= 2).then_some((digits, groups))
}

/// The code of an OTP message, if the text is one.
pub fn message_code(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || t.len() > MAX_TEXT {
        return None;
    }
    let lower = t.to_lowercase();
    // 1. A number right after a keyword.
    for kw in STRONG.iter().chain(WEAK) {
        let weak = WEAK.contains(kw);
        for (at, _) in lower.match_indices(kw) {
            let end = at + kw.len();
            if !word_boundary(&lower, at, end) {
                continue;
            }
            if weak && preceded_by_stop_word(&lower[..at]) {
                continue;
            }
            if let Some(c) = code_after(&lower[end..]) {
                return Some(c);
            }
        }
    }
    // 2. A strong phrase somewhere and exactly one standalone number in the message.
    if STRONG.iter().any(|kw| lower.match_indices(kw).any(|(at, _)| word_boundary(&lower, at, at + kw.len()))) {
        let all = standalone_numbers(&lower);
        if let [only] = all.as_slice() {
            return Some(only.clone());
        }
    }
    None
}

/// What to do with a piece of text and its clipboard owner: the code, if it is an OTP.
pub fn detect(text: &str, owner_exe: Option<&str>) -> Option<String> {
    if let Some(c) = digits_only(text) {
        return Some(c);
    }
    if let Some(c) = bare_code(text) {
        return owner_exe.is_some_and(is_source_app).then_some(c);
    }
    message_code(text)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Is `s[start..end]` a whole word (or phrase), not part of a longer one?
fn word_boundary(s: &str, start: usize, end: usize) -> bool {
    !s[..start].chars().next_back().is_some_and(is_word_char) && !s[end..].chars().next().is_some_and(is_word_char)
}

fn preceded_by_stop_word(before: &str) -> bool {
    let w: String = before.trim_end().chars().rev().take_while(|c| c.is_alphabetic()).collect::<Vec<_>>().into_iter().rev().collect();
    NOT_A_CODE_BEFORE.contains(&w.as_str())
}

/// A code at the start of `rest`, after separators and filler words.
fn code_after(rest: &str) -> Option<String> {
    let mut s = rest;
    for _ in 0..5 {
        s = s.trim_start_matches(|c: char| c.is_whitespace() || ":=-,.#<>()\"'".contains(c));
        let word: String = s.chars().take_while(|c| c.is_alphabetic()).collect();
        if !word.is_empty() && FILLER.contains(&word.as_str()) {
            s = &s[word.len()..];
        } else {
            break;
        }
    }
    number_at(s).map(|(d, _)| d)
}

/// A code at the very start of `s`: 4-8 digits, or two groups of 3 or 4 joined by `-` / a space,
/// followed by something that is not a letter or digit.
fn number_at(s: &str) -> Option<(String, usize)> {
    let run = |from: usize| s[from..].chars().take_while(|c| c.is_ascii_digit()).count();
    let first = run(0);
    if first == 0 {
        return None;
    }
    let mut end = first;
    let mut digits = s[..first].to_string();
    // Two groups: `123-456`, `123 456`, `1234 5678`.
    if matches!(first, 3 | 4) && matches!(s[first..].chars().next(), Some('-' | ' ')) {
        let second = run(first + 1);
        if second == first {
            digits.push_str(&s[first + 1..first + 1 + second]);
            end = first + 1 + second;
        }
    }
    let after_ok = !s[end..].chars().next().is_some_and(|c| c.is_alphanumeric());
    // "5000,00" / "1.500" are amounts, not codes.
    let decimal = matches!(s[end..].chars().next(), Some('.' | ',')) && s[end + 1..].chars().next().is_some_and(|c| c.is_ascii_digit());
    ((4..=8).contains(&digits.len()) && after_ok && !decimal).then_some((digits, end))
}

/// Every standalone 4-8 digit number of `s` (not inside a longer number, a word or an amount),
/// with years like 2024 left out.
fn standalone_numbers(s: &str) -> Vec<String> {
    let cs: Vec<(usize, char)> = s.char_indices().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < cs.len() {
        if !cs[i].1.is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = cs[i].0;
        let before = s[..start].chars().next_back();
        // Skip the whole digit/separator run so a phone number or an amount is not cut up.
        let mut j = i;
        while j < cs.len() && cs[j].1.is_ascii_digit() {
            j += 1;
        }
        let tail = cs.get(j).map_or(s.len(), |c| c.0);
        let hit = number_at(&s[start..]);
        let joined_to_word = before.is_some_and(|c| c.is_alphanumeric());
        let money = {
            let b = s[..start].trim_end();
            b.ends_with("rp") || b.ends_with("rp.") || b.ends_with("idr") || b.ends_with('$') || b.ends_with("usd")
        };
        if let Some((d, used)) = hit
            && !joined_to_word
            && !money
            && !(d.len() == 4 && (1900..=2099).contains(&d.parse::<u32>().unwrap_or(0)))
            && !matches!(before, Some('.' | ','))
        {
            out.push(d);
            i = cs.iter().position(|c| c.0 >= start + used).unwrap_or(cs.len());
        } else {
            i = cs.iter().position(|c| c.0 >= tail).unwrap_or(cs.len());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_codes() {
        for (t, want) in [("482913", "482913"), ("4829", "4829"), ("48291357", "48291357"), (" 123-456 ", "123456"), ("123 456", "123456"), ("1234-5678", "12345678")] {
            assert_eq!(bare_code(t).as_deref(), Some(want), "{t:?}");
        }
        for t in ["123", "123456789", "12-3456", "12a456", "1.234", "", "123--456", "1234 56", "12 34 56"] {
            assert_eq!(bare_code(t), None, "{t:?}");
        }
    }

    #[test]
    fn digits_only_text_is_a_code_whatever_the_app() {
        for owner in [Some("WhatsApp.exe"), Some("chrome.exe"), Some("notepad.exe"), Some("Authy Desktop.exe"), Some(r"C:\Windows\explorer.exe"), None] {
            for t in ["482913", " 482913 ", "4829", "48291357", "\t482913\r\n"] {
                assert_eq!(detect(t, owner).map(|c| c.len()), Some(t.trim().len()), "{t:?} from {owner:?}");
            }
        }
        // 4 to 8 digits, nothing else.
        for t in ["123", "123456789", "12 34", "1234a", "12.34", "\u{661}\u{662}\u{663}\u{664}\u{665}"] {
            assert_eq!(digits_only(t), None, "{t:?}");
            assert_eq!(detect(t, Some("chrome.exe")), None, "{t:?}");
        }
    }

    #[test]
    fn a_grouped_code_still_needs_a_source_app() {
        assert_eq!(detect("123-456", Some("WhatsApp.exe")).as_deref(), Some("123456"));
        assert_eq!(detect("123 456", Some(r"C:\Program Files\WindowsApps\Microsoft.YourPhone\PhoneExperienceHost.exe")).as_deref(), Some("123456"));
        assert_eq!(detect("1234-5678", Some("Telegram.exe")).as_deref(), Some("12345678"));
        assert_eq!(detect("123-456", Some("chrome.exe")), None, "two groups from a browser: a phone number, a date ...");
        assert_eq!(detect("123 456", None), None);
        assert!(is_source_app("phoneexperiencehost.exe") && is_source_app("SIGNAL.EXE") && !is_source_app("whatsappfake.exe"));
    }

    #[test]
    fn an_otp_message_up_to_a_kilobyte_is_handled() {
        let msg = "Kode OTP Anda 482913. Berlaku 5 menit. Jangan berikan kepada siapapun. ";
        let padded = |n: usize| format!("{msg}{}", "x".repeat(n.saturating_sub(msg.len())));
        assert_eq!(message_code(&padded(300)).as_deref(), Some("482913"));
        assert_eq!(message_code(&padded(900)).as_deref(), Some("482913"));
        assert_eq!(message_code(&padded(1024)).as_deref(), Some("482913"), "exactly 1 KB");
        assert_eq!(detect(&padded(1000), Some("chrome.exe")).as_deref(), Some("482913"));
        assert_eq!(message_code(&padded(1025)), None, "more than 1 KB is a document");
    }

    #[test]
    fn otp_messages_in_indonesian_and_english() {
        let cases = [
            ("Kode OTP Anda 482913. Berlaku 5 menit. Jangan berikan kepada siapapun.", "482913"),
            ("<#> Kode verifikasi Tokopedia: 7421. Jangan beritahukan kode ini kepada siapa pun.", "7421"),
            ("Your Grab verification code is 8456", "8456"),
            ("G-123456 is your Google verification code.", "123456"),
            ("Use 654 321 as your login code", "654321"),
            ("Your code is 482913", "482913"),
            ("Kode: 123456", "123456"),
            ("Gojek: 4821 adalah kode OTP kamu. Rahasiakan.", "4821"),
            ("Ref 98765432 - verification code 112233 expires in 10 minutes", "112233"),
            ("Your one-time password is 90817263", "90817263"),
            ("Token 345678 jangan dibagikan", "345678"),
        ];
        for (t, want) in cases {
            assert_eq!(message_code(t).as_deref(), Some(want), "{t}");
        }
    }

    #[test]
    fn false_positives_stay_plain_text() {
        for t in [
            "Meeting at 1400 in room 12",
            "Kode pos 40115",
            "error code 404",
            "error code 5000",
            "Total Rp 150000 sudah termasuk pajak",
            "Order 123456 has shipped",
            "pada tahun 2024 kami",
            "Aktifkan OTP untuk keamanan akun Anda",
            "The code review is at 14:00",
            "Call 0812 3456 7890 for support",
            "OTP 12",
            "kode promo 1234 tidak berlaku",
            "Nomor referensi 12345678901234",
            "",
            "   ",
        ] {
            assert_eq!(message_code(t), None, "{t:?}");
            assert_eq!(detect(t, Some("chrome.exe")), None, "{t:?}");
        }
        // A long document that happens to contain the word is ignored.
        let long = format!("kode verifikasi 123456 {}", "x ".repeat(600));
        assert_eq!(message_code(&long), None);
    }

    #[test]
    fn the_one_number_rule_needs_exactly_one() {
        // A strong phrase, two plausible numbers, neither next to the keyword: ambiguous.
        assert_eq!(message_code("Jangan bagikan kepada siapa pun. Kode A 111222 dan kode B 333444"), None);
        // A year does not count as the number.
        assert_eq!(message_code("Verification code expires 2024-12-01. Code: 556677").as_deref(), Some("556677"));
    }
}
