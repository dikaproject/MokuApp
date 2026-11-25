//! One-time-code detection in a notification's text (pure, no dependencies).
//!
//! A code is 4-8 digits, written in one piece (`482913`) or as two equal groups of 3-4 digits
//! (`123 456`, `123-456`, `1234 5678`; `G-554321` has its prefix ignored), in a text that also
//! says what it is: a keyword such as *kode*, *code*, *OTP*, *verifikasi*, *verification*.
//! Without a keyword nothing is a code, so an amount (`Rp 150000`), an order number or a year
//! is never offered for copying. A 4-digit year (1900-2099) counts only right behind a keyword.

/// Words that make a text one that carries a code (matched in lower case, as a part of a word:
/// `verifikasi` also catches `verifikasinya`).
const KEYWORDS: &[&str] = &[
    "kode", "code", "otp", "verifikasi", "verification", "verify", "verifying", "passcode", "pin", "sandi", "token", "sekali pakai", "one-time", "one time", "jangan berikan", "jangan bagikan", "jangan beritahukan",
    "do not share", "don't share", "dont share", "authentication", "autentikasi", "login code", "security code",
];

/// Digit runs of `s`: (start byte, end byte). `s` is searched on its characters, so a multi-byte
/// letter next to a number never splits it.
fn digit_runs(s: &str) -> Vec<(usize, usize)> {
    let mut runs = vec![];
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        match (c.is_ascii_digit(), start) {
            (true, None) => start = Some(i),
            (false, Some(b)) => {
                runs.push((b, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(b) = start {
        runs.push((b, s.len()));
    }
    runs
}

/// The character before byte `i` / at byte `i`.
fn before(s: &str, i: usize) -> Option<char> {
    s[..i].chars().next_back()
}

fn at(s: &str, i: usize) -> Option<char> {
    s[i..].chars().next()
}

/// Is the number that ends at byte `b` (digits end there) glued to what follows: a decimal or
/// thousands part (`3.14159`), a time (`12:3456`), a path, a percent sign, a currency or mail
/// sign. A full stop or comma that ends a sentence (`code is 52814. Do not ...`) is not glue.
fn glued_after(s: &str, b: usize) -> bool {
    match at(s, b) {
        Some('.' | ',' | ':' | '/' | '\\') => s[b..].chars().nth(1).is_some_and(|c| c.is_ascii_digit()),
        Some('%' | '+' | '$' | '@' | '_' | '#') => true,
        _ => false,
    }
}

/// The same for what is in front of the number that starts at byte `a`: `3.14159`, `12:3456`,
/// `+62812...`, `#12345`, `$5000`.
fn glued_before(s: &str, a: usize) -> bool {
    match before(s, a) {
        Some('.' | ',' | ':' | '/' | '\\') => s[..a].chars().rev().nth(1).is_some_and(|c| c.is_ascii_digit()),
        Some('%' | '+' | '$' | '@' | '_' | '#') => true,
        _ => false,
    }
}

/// One candidate: its digits and where it starts and ends (bytes; a two-group code spans both groups).
struct Candidate {
    digits: String,
    start: usize,
    end: usize,
}

/// Are runs `x` and the next one (`y`) neighbours in one digit sequence (`2026-11-25`,
/// `021 555 1234`): exactly one space or hyphen between them?
fn attached(s: &str, x: (usize, usize), y: (usize, usize)) -> bool {
    y.0 == x.1 + 1 && matches!(at(s, x.1), Some(' ' | '-'))
}

fn candidates(s: &str) -> Vec<Candidate> {
    let runs = digit_runs(s);
    let mut out = vec![];
    let mut i = 0;
    while i < runs.len() {
        let (a, b) = runs[i];
        let prev_attached = i > 0 && attached(s, runs[i - 1], runs[i]);
        let next_attached = runs.get(i + 1).is_some_and(|&n| attached(s, runs[i], n));
        // Two equal groups of 3-4 digits joined by one space or hyphen: `123 456`, `1234-5678`.
        // A third group makes it a date or a phone number, so it is not a code.
        if let Some(&(c, d)) = runs.get(i + 1)
            && next_attached
            && !prev_attached
            && b - a == d - c
            && (3..=4).contains(&(b - a))
            && !glued_before(s, a)
            && !glued_after(s, d)
            && !runs.get(i + 2).is_some_and(|&n| attached(s, (c, d), n))
        {
            out.push(Candidate { digits: format!("{}{}", &s[a..b], &s[c..d]), start: a, end: d });
            i += 2;
            continue;
        }
        // One piece of 4-8 digits, not part of a longer sequence and not glued to anything.
        if (4..=8).contains(&(b - a)) && !prev_attached && !next_attached && !glued_before(s, a) && !glued_after(s, b) {
            out.push(Candidate { digits: s[a..b].to_string(), start: a, end: b });
        }
        i += 1;
    }
    out
}

fn is_year(d: &str) -> bool {
    d.len() == 4 && d.parse::<u32>().is_ok_and(|y| (1900..=2099).contains(&y))
}

/// Byte positions where a keyword of [`KEYWORDS`] starts and ends in `lower` (the text with ASCII
/// letters lower-cased: same byte offsets as the original).
fn keyword_spans(lower: &str) -> Vec<(usize, usize)> {
    let mut spans = vec![];
    for k in KEYWORDS {
        let mut from = 0;
        while let Some(p) = lower[from..].find(k) {
            let s = from + p;
            // A keyword starts a word: "pin" is not the inside of "spinach".
            if before(lower, s).is_none_or(|c| !c.is_alphanumeric()) {
                spans.push((s, s + k.len()));
            }
            from = s + k.len();
        }
    }
    spans.sort_unstable();
    spans
}

/// The one-time code in `text` (a notification's title and body joined), digits only.
pub fn find_code(text: &str) -> Option<String> {
    find(text).map(|f| f.digits)
}

/// A code found in a text: its digits and where it stands (byte range of the original text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub digits: String,
    pub start: usize,
    pub end: usize,
}

/// `text` with every standalone occurrence of the code `digits` (one piece, or two groups) replaced
/// by dots, one per digit, the way the clipboard shows a code: `Kode: 123-456` becomes `Kode: ......`.
/// A text without the code comes back unchanged.
pub fn mask(text: &str, digits: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    for c in candidates(text) {
        if c.digits == digits && c.start >= from {
            out.push_str(&text[from..c.start]);
            out.extend(std::iter::repeat_n('\u{2022}', digits.chars().count()));
            from = c.end;
        }
    }
    out.push_str(&text[from..]);
    out
}

/// [`find_code`] with the position of the code.
pub fn find(text: &str) -> Option<Found> {
    let lower = text.to_ascii_lowercase();
    let spans = keyword_spans(&lower);
    if spans.is_empty() {
        return None;
    }
    // A year-like number counts only when it follows a keyword directly (`kode: 2025`).
    let usable = |c: &Candidate| {
        !is_year(&c.digits) || spans.iter().any(|&(_, ke)| c.start >= ke && c.start - ke <= 4 && text[ke..c.start].chars().all(|ch| matches!(ch, ' ' | ':' | '-' | '.' | '=')))
    };
    let cands: Vec<Candidate> = candidates(text).into_iter().filter(usable).collect();
    // The first candidate behind a keyword, else the first one in front of it.
    let first_kw = spans[0].0;
    cands.iter().find(|c| c.start >= first_kw).or_else(|| cands.first()).map(|c| Found { digits: c.digits.clone(), start: c.start, end: c.end })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(s: &str) -> Option<String> {
        find_code(s)
    }

    #[test]
    fn real_world_messages_have_their_code_found() {
        // Indonesian.
        assert_eq!(code("Kode verifikasi WhatsApp Anda: 123-456").as_deref(), Some("123456"));
        assert_eq!(code("<#> 482913 adalah kode OTP Anda. Jangan berikan kepada siapa pun.").as_deref(), Some("482913"));
        assert_eq!(code("JANGAN BAGIKAN kode ini. Kode login Tokopedia: 8841").as_deref(), Some("8841"));
        assert_eq!(code("Masukkan kode 5529 7731 untuk masuk").as_deref(), Some("55297731"));
        // English.
        assert_eq!(code("Your Telegram code is 52814. Do not share it.").as_deref(), Some("52814"));
        assert_eq!(code("Use 691 204 to verify your Microsoft account").as_deref(), Some("691204"));
        assert_eq!(code("G-554321 is your Google verification code").as_deref(), Some("554321"));
        assert_eq!(code("Discord verification code: 90210341").as_deref(), Some("90210341"));
        assert_eq!(code("Your one-time passcode is 4821").as_deref(), Some("4821"));
        // Title and body joined, code in either.
        assert_eq!(code("Telegram\nLogin code: 31415. Don't share").as_deref(), Some("31415"));
        assert_eq!(code("652 109\nis your Signal verification code").as_deref(), Some("652109"));
    }

    #[test]
    fn a_code_is_masked_with_dots_like_the_clipboard_shows_it() {
        let dots = |n: usize| "\u{2022}".repeat(n);
        assert_eq!(mask("Kode verifikasi WhatsApp Anda: 123-456", "123456"), format!("Kode verifikasi WhatsApp Anda: {}", dots(6)));
        assert_eq!(mask("Your Telegram code is 52814. Do not share it.", "52814"), format!("Your Telegram code is {}. Do not share it.", dots(5)));
        assert_eq!(mask("<#> 482913 adalah kode OTP. Kode 482913", "482913"), format!("<#> {} adalah kode OTP. Kode {}", dots(6), dots(6)), "every occurrence");
        assert_eq!(mask("Masukkan kode 5529 7731 untuk masuk", "55297731"), format!("Masukkan kode {} untuk masuk", dots(8)));
        // Other numbers stay; a text without the code is unchanged; non-ASCII text around it is intact.
        assert_eq!(mask("Order 12345678 dan kode 482913", "482913"), format!("Order 12345678 dan kode {}", dots(6)));
        assert_eq!(mask("Halo, apa kabar?", "482913"), "Halo, apa kabar?");
        assert_eq!(mask("K\u{f4}de \u{2014} \u{201c}482913\u{201d} \u{e9}\u{e9}", "482913"), format!("K\u{f4}de \u{2014} \u{201c}{}\u{201d} \u{e9}\u{e9}", dots(6)));
        // `find` says where it is.
        let t = "Kode: 482913.";
        let f = find(t).unwrap();
        assert_eq!((&t[f.start..f.end], f.digits.as_str()), ("482913", "482913"));
        let t = "Kode 123 456 ok";
        let f = find(t).unwrap();
        assert_eq!((&t[f.start..f.end], f.digits.as_str()), ("123 456", "123456"));
    }

    #[test]
    fn numbers_without_a_keyword_are_never_codes() {
        for t in [
            "Pembayaran Rp 150000 berhasil",
            "Order 12345678 sudah dikirim",
            "Nadia: Meeting-nya jadi jam 3 ya? Nomor 081234567",
            "Saldo Anda 2500000",
            "Tiba pukul 14:30 di stasiun",
            "",
            "   ",
            "Halo!",
        ] {
            assert_eq!(code(t), None, "{t:?}");
        }
    }

    #[test]
    fn a_keyword_alone_or_the_wrong_digits_are_not_a_code() {
        // Keyword, but no number.
        assert_eq!(code("Kode verifikasi Anda sudah dikirim ke email"), None);
        // Too short / too long.
        assert_eq!(code("Kode: 123"), None);
        assert_eq!(code("Kode order 1234567890 dan 123456789"), None);
        // Times, decimals, amounts, phone numbers, dates are not codes even next to a keyword.
        assert_eq!(code("Kode booking: jam 12:3456"), None);
        assert_eq!(code("Kode promo: diskon 3.14159 persen"), None);
        assert_eq!(code("Kode: Rp 1.234.567"), None);
        assert_eq!(code("Kode OTP dikirim ke +628123456789"), None);
        assert_eq!(code("Verifikasi tanggal 2026-11-25 selesai"), None);
        assert_eq!(code("Kode pesanan 021 555 1234"), None);
        assert_eq!(code("Kode: 50%"), None);
        // A year is only a code right behind the keyword.
        assert_eq!(code("Kode booking liburan 2025 sudah aktif"), None);
        assert_eq!(code("Kode: 2025").as_deref(), Some("2025"));
        // Keywords are words: "spinach" does not contain "pin".
        assert_eq!(code("Spinach recipe 482913"), None);
        assert_eq!(code("pin 482913").as_deref(), Some("482913"));
    }

    #[test]
    fn the_first_code_after_the_keyword_wins_and_letters_do_not_confuse() {
        assert_eq!(code("Kode 112233, bukan 445566").as_deref(), Some("112233"));
        // A number before the keyword is used only when there is none after it.
        assert_eq!(code("778899 adalah kode Anda").as_deref(), Some("778899"));
        assert_eq!(code("Order 100200 selesai. Kode verifikasi 334455").as_deref(), Some("334455"));
        // Non-ASCII text around it never splits a number or shifts the search.
        assert_eq!(code("Kôde verifikasi — ünïcode “482913” éé").as_deref(), Some("482913"));
        assert_eq!(code("日本語 code 7788 です").as_deref(), Some("7788"));
        // Full-width / other-script digits are not codes (only ASCII digits are).
        assert_eq!(code("code ４８２９１３"), None);
    }
}
