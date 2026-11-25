//! Redaction: before anything is sent to a provider (and in the "Lihat yang dikirim" preview),
//! OTPs, 16-digit numbers (card numbers, NIK) and token-like strings are replaced by a marker
//! such as `[disamarkan: kode OTP]` (Indonesian, and what the provider gets too: a model reads it as "hidden"). It is on by default (`ai.redact`).
//!
//! These are heuristics for the common cases, written without a regex crate. They lean toward
//! hiding too much rather than too little, but ordinary numbers (years, prices, phone numbers,
//! postal codes, ids) and ordinary long words, URLs and identifiers are left alone; the tests
//! list both sides. The text is never logged here.
//!
//! What is hidden:
//! * **Tokens**: API keys with a known prefix (`sk-…`, `ghp_…`, `AKIA…`, `xox…`, `AIza…`, JWTs …),
//!   `Bearer …` values, `password: …` / `api_key=…` style values, private-key blocks, and long
//!   random-looking strings (mixed case and digits that change character class often, or pure
//!   hex of 32+ characters).
//! * **16-digit numbers**: any 16 digits in a row, or 14–19 digits (also in groups separated by
//!   a space or dash) that pass the Luhn check, or grouped numbers shaped like a NIK. A valid
//!   16-digit NIK is marked `nik`, the rest `card`.
//! * **OTPs**: a 4–8 digit code (`123456`, `123 456`, `G-123456`) next to a word like OTP, kode,
//!   code, PIN, verifikasi, verification, passcode (not "kode pos", "area code", "status code" …).

/// What was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Token,
    Nik,
    Card,
    Otp,
}

impl Kind {
    pub fn marker(self) -> &'static str {
        match self {
            Kind::Token => "[disamarkan: token]",
            Kind::Nik => "[disamarkan: NIK]",
            Kind::Card => "[disamarkan: nomor kartu]",
            Kind::Otp => "[disamarkan: kode OTP]",
        }
    }

    /// Indonesian name for the card's note ("2 hal disembunyikan: OTP, token").
    pub fn label(self) -> &'static str {
        match self {
            Kind::Token => "token",
            Kind::Nik => "NIK",
            Kind::Card => "nomor kartu",
            Kind::Otp => "OTP",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    /// How many pieces were replaced.
    pub count: usize,
    /// Which kinds, each once, in a fixed order.
    pub kinds: Vec<Kind>,
}

#[derive(Debug, Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
    kind: Kind,
}

/// Replace what must not be sent.
pub fn redact(text: &str) -> Redacted {
    let mut found: Vec<Span> = vec![];
    token_spans(text, &mut found);
    digit_spans(text, &mut found);
    // Higher-priority kinds win an overlap (Token, then Nik, Card, Otp); then left to right.
    found.sort_by_key(|s| (s.kind, s.start));
    let mut kept: Vec<Span> = vec![];
    for s in found {
        if s.end > s.start && !kept.iter().any(|k| s.start < k.end && k.start < s.end) {
            kept.push(s);
        }
    }
    kept.sort_by_key(|s| s.start);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for s in &kept {
        out.push_str(&text[at..s.start]);
        out.push_str(s.kind.marker());
        at = s.end;
    }
    out.push_str(&text[at..]);
    let mut kinds: Vec<Kind> = kept.iter().map(|s| s.kind).collect();
    kinds.sort();
    kinds.dedup();
    Redacted { text: out, count: kept.len(), kinds }
}

/// "2 hal disembunyikan: OTP, token", or `None` when nothing was hidden.
pub fn summary(r: &Redacted) -> Option<String> {
    if r.count == 0 {
        return None;
    }
    let names: Vec<&str> = r.kinds.iter().map(|k| k.label()).collect();
    Some(format!("{} hal disembunyikan: {}", r.count, names.join(", ")))
}

// ---- tokens ---------------------------------------------------------------------------------

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '=')
}

/// Exclusive end of the run of word characters that starts at byte `from`.
fn word_end(text: &str, from: usize) -> usize {
    text[from..].char_indices().find(|(_, c)| !is_word_char(*c)).map_or(text.len(), |(i, _)| from + i)
}

/// A string that was generated, not written: long and with many changes between lower case,
/// upper case, digits and symbols (camelCase names, words and URLs change far less often).
fn looks_random(w: &str) -> bool {
    let w = w.trim_matches(|c: char| matches!(c, '.' | '=' | '/' | '-' | '_' | '+') && !c.is_ascii_alphanumeric());
    if w.len() < 32 || !w.is_ascii() {
        return false;
    }
    if w.contains('.') {
        return false; // file names, domains, versions (JWTs are matched by their own rule)
    }
    let hex = w.bytes().all(|b| b.is_ascii_hexdigit());
    if hex {
        return true;
    }
    // A UUID is an id, not a secret.
    if w.len() == 36 && w.bytes().enumerate().all(|(i, b)| if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() }) {
        return false;
    }
    let class = |b: u8| match b {
        b'a'..=b'z' => 0,
        b'A'..=b'Z' => 1,
        b'0'..=b'9' => 2,
        _ => 3,
    };
    let bytes = w.as_bytes();
    let mut seen = [false; 4];
    let mut changes = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        seen[class(b)] = true;
        if i > 0 && class(bytes[i - 1]) != class(b) {
            changes += 1;
        }
    }
    let classes = seen.iter().filter(|s| **s).count();
    classes >= 3 && seen[2] && changes * 100 >= bytes.len() * 45
}

/// A key with a well-known prefix.
fn known_prefix(w: &str) -> bool {
    let tail = |p: &str, n: usize| w.strip_prefix(p).is_some_and(|r| r.len() >= n);
    if tail("sk-", 17) || tail("sk_live_", 16) || tail("sk_test_", 16) || tail("pk_live_", 16) || tail("rk_live_", 16) {
        return true;
    }
    if ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"].iter().any(|p| tail(p, 20)) || tail("github_pat_", 30) || tail("glpat-", 18) {
        return true;
    }
    if tail("hf_", 30) || tail("npm_", 30) || tail("AIza", 30) {
        return true;
    }
    if ["AKIA", "ASIA"].iter().any(|p| w.strip_prefix(p).is_some_and(|r| r.len() == 16 && r.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))) {
        return true;
    }
    // Slack: xoxb-…, xoxp-…, xoxa-…, xoxr-…, xoxs-…
    if w.len() >= 15 && w.starts_with("xox") && matches!(w.as_bytes()[3], b'a' | b'b' | b'p' | b'r' | b's') && w.as_bytes()[4] == b'-' {
        return true;
    }
    // JWT: three dot-separated base64url parts, the first one starting with eyJ.
    if w.starts_with("eyJ") {
        let parts: Vec<&str> = w.split('.').collect();
        if parts.len() == 3 && parts.iter().all(|p| p.len() >= 5 && p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'='))) {
            return true;
        }
    }
    false
}

const SECRET_NAMES: [&str; 14] = [
    "password", "passwd", "pwd", "secret", "api_key", "api-key", "apikey", "access_token", "access-token", "auth_token", "client_secret", "private_key", "kata sandi", "token",
];

fn token_spans(text: &str, out: &mut Vec<Span>) {
    // Private key blocks, up to their END line (or the end of the text).
    let mut from = 0;
    while let Some(i) = text[from..].find("-----BEGIN ") {
        let start = from + i;
        let head_end = text[start..].find("-----\n").or_else(|| text[start + 11..].find("-----").map(|p| p + 11 - 0)).map_or(text.len(), |p| start + p + 5);
        let is_private = text[start..head_end.min(text.len())].contains("PRIVATE KEY");
        if is_private {
            let end = match text[start..].find("-----END ") {
                Some(e) => {
                    let e = start + e;
                    text[e + 9..].find("-----").map_or(text.len(), |p| e + 9 + p + 5)
                }
                None => text.len(),
            };
            out.push(Span { start, end, kind: Kind::Token });
            from = end.max(start + 11);
        } else {
            from = start + 11;
        }
        if from >= text.len() {
            break;
        }
    }

    // Word by word.
    let lower = text.to_ascii_lowercase();
    let mut i = 0;
    while i < text.len() {
        let Some(c) = text[i..].chars().next() else { break };
        if !is_word_char(c) {
            i += c.len_utf8();
            continue;
        }
        let end = word_end(text, i);
        let w = &text[i..end];
        let w_trim = w.trim_end_matches(['.', '=', '/']);
        if known_prefix(w_trim) || looks_random(w_trim) {
            out.push(Span { start: i, end: i + w_trim.len(), kind: Kind::Token });
        } else if matches!(lower[i..end].trim_end_matches(['.', ':']), "bearer" | "basic") {
            // "Bearer <value>" / "Authorization: Basic <value>"
            let mut j = end;
            while j < text.len() && text.as_bytes()[j] == b' ' {
                j += 1;
            }
            let vend = word_end(text, j);
            if vend - j >= 16 {
                out.push(Span { start: j, end: vend, kind: Kind::Token });
            }
        }
        i = end.max(i + 1);
    }

    // password: value / api_key=value / "token": "value"
    for name in SECRET_NAMES {
        let mut from = 0;
        while let Some(p) = lower[from..].find(name) {
            let at = from + p;
            let after = at + name.len();
            from = after;
            // Not the middle of a longer word ("tokenizer", "mypassword" is fine: it still counts).
            if lower[after..].chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                continue;
            }
            let b = text.as_bytes();
            let mut j = after;
            // Optional closing quote of a JSON key, spaces, then : or =
            if j < b.len() && matches!(b[j], b'"' | b'\'') {
                j += 1;
            }
            while j < b.len() && b[j] == b' ' {
                j += 1;
            }
            if j >= b.len() || !matches!(b[j], b':' | b'=') {
                continue;
            }
            j += 1;
            while j < b.len() && matches!(b[j], b' ' | b'"' | b'\'') {
                j += 1;
            }
            let mut e = j;
            while e < b.len() && !matches!(b[e], b' ' | b'\n' | b'\r' | b'\t' | b'"' | b'\'' | b',' | b';' | b')' | b'}') {
                e += 1;
            }
            // Land on a character boundary (the loop above only stops on ASCII bytes or the end).
            while e < text.len() && !text.is_char_boundary(e) {
                e += 1;
            }
            if e - j >= 6 {
                out.push(Span { start: j, end: e, kind: Kind::Token });
            }
        }
    }
}

// ---- digits ---------------------------------------------------------------------------------

/// A run of digits, with optional single space / dash separators between digit groups.
struct Run {
    start: usize,
    end: usize,
    digits: String,
    /// Separators were used (`1234 5678 …`).
    grouped: bool,
}

fn runs(text: &str) -> Vec<Run> {
    let b = text.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // Part of a word like "abc123…" or "v2024…": not a number of its own.
        let prev_alnum = i > 0 && text[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric());
        let start = i;
        let mut j = i;
        let mut digits = String::new();
        let mut grouped = false;
        loop {
            while j < b.len() && b[j].is_ascii_digit() {
                digits.push(b[j] as char);
                j += 1;
            }
            if j + 1 < b.len() && matches!(b[j], b' ' | b'-') && b[j + 1].is_ascii_digit() {
                grouped = true;
                j += 1;
                continue;
            }
            break;
        }
        let next_alnum = text[j..].chars().next().is_some_and(|c| c.is_alphanumeric());
        if !prev_alnum && !next_alnum {
            out.push(Run { start, end: j, digits, grouped });
        }
        i = j.max(i + 1);
    }
    out
}

fn luhn(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.bytes().rev().enumerate() {
        let mut d = (c - b'0') as u32;
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum % 10 == 0
}

/// 16 digits shaped like an Indonesian NIK: a province code that exists (11-19, 21, 31-36, 51-53,
/// 61-65, 71-76, 81-82, 91-97), a birth day 01-31 (+40 for women) and a month 01-12.
fn nik_shaped(d: &str) -> bool {
    if d.len() != 16 || !d.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let n = |a: usize, b: usize| d[a..b].parse::<u32>().unwrap_or(0);
    let province = matches!(n(0, 2), 11..=19 | 21 | 31..=36 | 51..=53 | 61..=65 | 71..=76 | 81 | 82 | 91..=97);
    province && (matches!(n(6, 8), 1..=31) || matches!(n(6, 8), 41..=71)) && matches!(n(8, 10), 1..=12)
}

/// Words that make a number next to them a code that is sent to you once.
const OTP_WORDS: [&str; 17] = [
    "otp", "kode", "code", "pin", "verifikasi", "verification", "verify", "passcode", "one-time", "one time", "sekali pakai", "2fa", "mfa", "kata sandi sekali", "security code", "kode keamanan", "login code",
];

/// A word between the keyword and the number that says it is another kind of code.
const NOT_OTP: [&str; 20] = [
    "pos", "postal", "zip", "area", "negara", "country", "produk", "product", "barang", "wilayah", "promo", "error", "status", "http", "response", "exit", "source", "sumber", "kesalahan", "return",
];

fn has_word(hay: &str, w: &str) -> bool {
    let mut from = 0;
    while let Some(p) = hay[from..].find(w) {
        let s = from + p;
        let e = s + w.len();
        let before_ok = s == 0 || !hay[..s].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric());
        let after_ok = e == hay.len() || !hay[e..].chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = s + 1;
        if from >= hay.len() {
            break;
        }
    }
    false
}

fn snap(text: &str, mut i: usize, down: bool) -> usize {
    i = i.min(text.len());
    while i > 0 && i < text.len() && !text.is_char_boundary(i) {
        if down { i -= 1 } else { i += 1 }
    }
    i
}

/// Is the digit run at `[s, e)` a one-time code, going by the words around it?
fn otp_context(lower: &str, s: usize, e: usize) -> bool {
    // Google style "G-123456".
    if s >= 2 && lower.get(s - 2..s) == Some("g-") && (s == 2 || !lower[..s - 2].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric())) {
        return true;
    }
    let from = snap(lower, s.saturating_sub(48), true);
    let mut before = &lower[from..s];
    // Only the same sentence counts (a line break does not end it: "Kode OTP:\n123456").
    if let Some(p) = before.rfind(['.', '!', '?']) {
        before = &before[p + 1..];
    }
    let to = snap(lower, e + 30, false);
    let mut after = &lower[e..to];
    if let Some(p) = after.find(['.', '!', '?']) {
        after = &after[..p];
    }
    for w in OTP_WORDS {
        // The keyword before the number: "kode otp anda: 123456", "your code is 123456".
        if let Some(p) = before.rfind(w) {
            let between = &before[p + w.len()..];
            // "postal code 90210", "area code 4155", "kode pos 40115": another kind of code.
            let lead = &before[snap(before, p.saturating_sub(16), true)..p];
            if has_word(before, w) && !NOT_OTP.iter().any(|n| has_word(between, n) || has_word(lead, n)) {
                // Right after the keyword there must be no other digits.
                if !between.bytes().any(|b| b.is_ascii_digit()) {
                    return true;
                }
            }
        }
        // The keyword after the number: "123456 adalah kode rahasia anda".
        if has_word(after, w) && !NOT_OTP.iter().any(|n| has_word(after, n)) {
            return true;
        }
    }
    false
}

fn digit_spans(text: &str, out: &mut Vec<Span>) {
    let lower = text.to_ascii_lowercase();
    for r in runs(text) {
        let n = r.digits.len();
        let kind = if n == 16 && !r.grouped {
            Some(if nik_shaped(&r.digits) { Kind::Nik } else { Kind::Card })
        } else if r.grouped && nik_shaped(&r.digits) {
            Some(Kind::Nik)
        } else if (14..=19).contains(&n) && luhn(&r.digits) {
            Some(Kind::Card)
        } else {
            None
        };
        if let Some(k) = kind {
            out.push(Span { start: r.start, end: r.end, kind: k });
            continue;
        }
        // A one-time code: 4–8 digits, as one number or two groups ("123 456").
        if (4..=8).contains(&n) && (!r.grouped || (n >= 6 && r.digits.len() == n)) && otp_context(&lower, r.start, r.end) {
            out.push(Span { start: r.start, end: r.end, kind: Kind::Otp });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> String {
        redact(s).text
    }

    fn untouched(s: &str) {
        let x = redact(s);
        assert_eq!(x.text, s, "must be left alone: {s:?}");
        assert_eq!(x.count, 0);
    }

    #[test]
    fn otp_messages_are_hidden() {
        assert_eq!(r("Kode OTP Anda: 482913. Jangan berikan kode ini ke siapa pun."), "Kode OTP Anda: [disamarkan: kode OTP]. Jangan berikan kode ini ke siapa pun.");
        assert_eq!(r("Your verification code is 4829."), "Your verification code is [disamarkan: kode OTP].");
        assert_eq!(r("<#> 123456 adalah kode verifikasi Anda"), "<#> [disamarkan: kode OTP] adalah kode verifikasi Anda");
        assert_eq!(r("G-482913 is your Google verification code"), "[disamarkan: kode OTP] is your Google verification code".replace("[disamarkan: kode OTP]", "G-[disamarkan: kode OTP]").as_str());
        assert_eq!(r("PIN: 123456"), "PIN: [disamarkan: kode OTP]");
        assert_eq!(r("OTP 12345678"), "OTP [disamarkan: kode OTP]");
        assert_eq!(r("kode 482 913"), "kode [disamarkan: kode OTP]");
        assert_eq!(r("Use code 7731 to sign in"), "Use code [disamarkan: kode OTP] to sign in");
        assert_eq!(r("Masukkan kode OTP\n654321\nberlaku 5 menit"), "Masukkan kode OTP\n654321\nberlaku 5 menit".replace("654321", "[disamarkan: kode OTP]").as_str());
    }

    #[test]
    fn ordinary_numbers_are_left_alone() {
        for s in [
            "Saya lahir tahun 1995 dan pindah di 2024.",
            "Harga Rp 50.000 atau Rp 1.500.000, total 4500 rupiah",
            "Telepon 0812-3456-7890 atau +62 812 3456 7890",
            "Kode pos 40115, kode area 022",
            "Postal code 90210 and area code 4155",
            "HTTP status code 40400 for the error code 12345",
            "Pada tanggal 25 11 2026 jam 14.30",
            "Versi 2.0.1234 dirilis; build 20261125",
            "Order #123456 dikirim, tracking 8801234567",
            "Terjadi 1234 kali selama 5678 hari",
            "ISBN 9786020318998 dan tanggal 1700000000000",
            "call 2024 2025 2026",
        ] {
            untouched(s);
        }
        // The code word is far from the number, in another sentence.
        untouched("Kode ini penting. Saya membeli 4500 butir telur kemarin.");
    }

    #[test]
    fn sixteen_digit_numbers_and_cards_are_hidden() {
        assert_eq!(r("kartu 4111111111111111 saya"), "kartu [disamarkan: nomor kartu] saya");
        assert_eq!(r("kartu 4111 1111 1111 1111 saya"), "kartu [disamarkan: nomor kartu] saya");
        assert_eq!(r("4111-1111-1111-1111"), "[disamarkan: nomor kartu]");
        // Any 16 digits in a row, even when the Luhn check fails.
        assert_eq!(r("id 1234567890123456 ok"), "id [disamarkan: nomor kartu] ok");
        // Amex (15) and a 19-digit card pass Luhn; a random 15 digit number does not.
        assert_eq!(r("amex 378282246310005"), "amex [disamarkan: nomor kartu]");
        assert_eq!(r("4111111111111111111").contains("disamarkan"), luhn("4111111111111111111"));
        untouched("nomor 123456789012345");
        // Inside a word it is an identifier, not a number.
        untouched("ref abc1234567890123456xyz");
    }

    #[test]
    fn nik_is_recognised_and_marked() {
        // Jakarta (31), born on the 15th of March (day 15, month 03, year 90).
        assert_eq!(r("NIK saya 3174031503900001."), "NIK saya [disamarkan: NIK].");
        // A woman: the day is +40.
        assert_eq!(r("3374095506920003"), "[disamarkan: NIK]");
        assert_eq!(r("3174 0315 0390 0001"), "[disamarkan: NIK]");
        // Not NIK-shaped: province 99, month 13: still hidden as a 16-digit number.
        assert_eq!(r("9999991399000001"), "[disamarkan: nomor kartu]");
        assert!(nik_shaped("3174031503900001") && !nik_shaped("3174031503900"));
    }

    #[test]
    fn tokens_are_hidden() {
        for (input, hidden) in [
            ("key sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789 ok", "sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"),
            ("sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv", "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv"),
            ("token ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789 repo", "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789"),
            ("github_pat_11ABCDEFG0abcdefghijkl_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", "github_pat_11ABCDEFG0abcdefghijkl_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"),
            ("aws AKIAIOSFODNN7EXAMPLE secret", "AKIAIOSFODNN7EXAMPLE"),
            // Split with concat! so secret scanners (GitHub push protection) do not take the fake
            // sample for a real Slack token; the compiled string is the same.
            (concat!("slack xox", "b-123456789012-abcdefghijklmnop"), concat!("xox", "b-123456789012-abcdefghijklmnop")),
            ("gcp AIzaSyA1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6Q", "AIzaSyA1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6Q"),
            ("jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c here", "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"),
            ("random q8Zx3LmN0pRtV7yBcD2eFgH5jKw9sA1u end", "q8Zx3LmN0pRtV7yBcD2eFgH5jKw9sA1u"),
            ("hex 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08 sha", "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"),
        ] {
            let x = redact(input);
            assert!(!x.text.contains(hidden), "{hidden} was not hidden in {input:?} -> {:?}", x.text);
            assert!(x.text.contains("[disamarkan: token]"), "{input:?} -> {:?}", x.text);
            assert!(x.kinds.contains(&Kind::Token));
        }
        // The words around it stay.
        assert_eq!(r("key sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789 ok"), "key [disamarkan: token] ok");
        assert_eq!(r("Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345"), "Authorization: Bearer [disamarkan: token]");
        assert_eq!(r("curl -H 'Authorization: Basic dXNlcjpwYXNzd29yZDEyMzQ1'"), "curl -H 'Authorization: Basic [disamarkan: token]'");
    }

    #[test]
    fn secret_assignments_hide_the_value_only() {
        assert_eq!(r("password: hunter2xyz"), "password: [disamarkan: token]");
        assert_eq!(r("PASSWORD=Sup3rS3cret!"), "PASSWORD=[disamarkan: token]");
        assert_eq!(r("api_key = \"abc123def456\""), "api_key = \"[disamarkan: token]\"");
        assert_eq!(r("{\"token\": \"abcdef123456\", \"user\": \"rani\"}"), "{\"token\": \"[disamarkan: token]\", \"user\": \"rani\"}");
        assert_eq!(r("kata sandi: rahasia123"), "kata sandi: [disamarkan: token]");
        assert_eq!(r("client_secret=Zm9vYmFyYmF6"), "client_secret=[disamarkan: token]");
        // A word that only starts with the name, a short value, and no assignment are not secrets.
        untouched("tokenizer: spaCy");
        untouched("password: abc");
        untouched("Lupa password? Klik reset.");
        untouched("the secret to good soup is salt");
    }

    #[test]
    fn private_key_blocks_are_hidden_whole() {
        let pem = "intro\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA7\nabcdef\n-----END RSA PRIVATE KEY-----\noutro";
        assert_eq!(r(pem), "intro\n[disamarkan: token]\noutro");
        // A certificate is public: left alone.
        untouched("-----BEGIN CERTIFICATE-----\nMIIDdzCCAl+gAwIBAgIE\n-----END CERTIFICATE-----");
        // An unfinished block hides everything after it.
        assert_eq!(r("a\n-----BEGIN PRIVATE KEY-----\nMIIE"), "a\n[disamarkan: token]");
    }

    #[test]
    fn ordinary_text_code_urls_and_ids_are_not_tokens() {
        for s in [
            "pemberdayaanmasyarakatdesaterpencilberkelanjutan adalah kata yang panjang sekali",
            "https://example.com/blog/how-to-write-a-really-long-article-title-for-seo-2026",
            "someVeryLongFunctionNameThatDoesSomethingUseful123(argumentNumberOne)",
            "C:\\Users\\Rani\\Documents\\Projects\\moku\\app\\moku\\src\\features\\ai\\redact.rs",
            "550e8400-e29b-41d4-a716-446655440000",
            "commit a1b2c3d fix: typo",
            "Kontak: rani@example.com, https://github.com/rani/moku",
            "The quick brown fox jumps over the lazy dog 0123456789 times",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMNOPQRSTUVWXYZ",
            "e5-large-v2/bge-base-en-v1.5-quantized-model-final",
            "Selamat pagi, apa kabar? Saya mau tanya tentang jadwal besok.",
            "",
        ] {
            untouched(s);
        }
    }

    #[test]
    fn mixed_text_counts_kinds_and_is_idempotent() {
        let text = "OTP 123456, kartu 4111 1111 1111 1111, NIK 3174031503900001, key sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        let x = redact(text);
        assert_eq!(x.count, 4);
        assert_eq!(x.kinds, [Kind::Token, Kind::Nik, Kind::Card, Kind::Otp]);
        assert_eq!(x.text, "OTP [disamarkan: kode OTP], kartu [disamarkan: nomor kartu], NIK [disamarkan: NIK], key [disamarkan: token]");
        assert_eq!(summary(&x).as_deref(), Some("4 hal disembunyikan: token, NIK, nomor kartu, OTP"));
        // Again: nothing more to hide.
        let again = redact(&x.text);
        assert_eq!((again.text.as_str(), again.count), (x.text.as_str(), 0));
        assert_eq!(summary(&redact("halo")), None);
    }

    #[test]
    fn unicode_and_odd_input_never_panic() {
        for s in ["日本語のテキスト 123456 kode", "kode OTP: ٤٥٦٧٨٩", "🔑 token=🔑🔑🔑🔑🔑🔑🔑", "password:", "password: ", "kode\u{0}123456", "Bearer", "Bearer ", "-----BEGIN", "-----BEGIN PRIVATE KEY-----", "é".repeat(500).as_str(), "1".repeat(5000).as_str(), "a-".repeat(400).as_str()] {
            let _ = redact(s);
        }
        // Multi-byte characters around a code are handled on character boundaries.
        assert_eq!(r("ééééééééééééééééééééééééééééé kode 123456 éééé"), "ééééééééééééééééééééééééééééé kode [disamarkan: kode OTP] éééé");
    }
}
