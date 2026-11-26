//! When each word of a lyric line is sung (pure, no I/O).
//!
//! Real word times are used as they are when the lyrics have them (enhanced LRC; rare). For the
//! usual line-level lyrics the times are **estimated**: the line's sung time is spread over its
//! words by how long they take to say (syllables / letters), with short pauses after commas and
//! full stops. The estimate is approximate by nature; what it guarantees is that the highlight
//! stays inside the line's slot, never runs ahead across a long instrumental gap, and always
//! moves forward.

use super::lrc::Line;
use crate::ui::WordSpan;

/// Singing speed the estimate starts from: ms per letter.
pub const MS_PER_CHAR: f32 = 80.0;
/// Allowance on top of it for held notes and breaths: the line may be sung this much slower than
/// `chars x MS_PER_CHAR`, as far as its slot allows.
pub const SLACK: f32 = 1.75;
/// A line is sung for at least / at most this long (when its slot allows).
pub const MIN_SUNG_MS: f32 = 1200.0;
pub const MAX_SUNG_MS: f32 = 8000.0;
/// The last word ends this long before the next line starts.
pub const TAIL_MS: u64 = 150;
/// Slot of the last line (no next line to measure against).
pub const LAST_LINE_GAP_MS: u64 = 5000;

/// Scripts written without spaces (every character is its own unit of timing): Chinese, Japanese
/// kana, Thai, Lao, Khmer, Myanmar. Hangul is spaced and counts as words.
fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF | 0xFF66..=0xFF9F
        | 0x0E00..=0x0E7F | 0x0E80..=0x0EFF | 0x1780..=0x17FF | 0x1000..=0x109F)
}

/// Marks that join the character before them into one cluster (combining marks, Thai vowel and
/// tone marks, variation selectors, skin tones, the zero-width joiner).
fn is_extend(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x20D0..=0x20FF | 0x3099..=0x309A | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F
        | 0x200D | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F
        | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x0EB1 | 0x0EB4..=0x0EBC | 0x0EC8..=0x0ECD | 0x17B7..=0x17BD | 0x17C6 | 0x17C9..=0x17D3)
}

/// Thai / Lao vowels written before their consonant.
fn is_prefix_vowel(c: char) -> bool {
    matches!(c as u32, 0x0E40..=0x0E44 | 0x0EC0..=0x0EC4)
}

fn is_rtl_char(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
}

/// Mostly right-to-left script (Hebrew, Arabic, ...). The sweep runs left to right, so the
/// highlight is left off for such lines (they are shown fully bright).
pub fn is_rtl(text: &str) -> bool {
    let (mut rtl, mut other) = (0usize, 0usize);
    for c in text.chars() {
        if is_rtl_char(c) {
            rtl += 1;
        } else if c.is_alphabetic() {
            other += 1;
        }
    }
    rtl > 0 && rtl >= other
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Space,
    /// A character of a script without spaces.
    Wide,
    /// Letters, digits.
    Alnum,
    /// Everything else: punctuation, symbols, emoji.
    Punct,
}

/// A grapheme-like cluster: one character plus the marks that belong to it.
struct Cluster {
    from: usize,
    to: usize,
    kind: Kind,
}

fn clusters(text: &str) -> Vec<Cluster> {
    let mut out: Vec<Cluster> = Vec::new();
    let mut join_next = false;
    for (i, c) in text.char_indices() {
        let end = i + c.len_utf8();
        let attach = join_next || is_extend(c);
        join_next = c == '\u{200d}';
        if attach {
            if let Some(last) = out.last_mut().filter(|l| l.kind != Kind::Space) {
                last.to = end;
                continue;
            }
        }
        let kind = if c.is_whitespace() {
            Kind::Space
        } else if is_wide(c) && c.is_alphanumeric() {
            Kind::Wide
        } else if c.is_alphanumeric() {
            Kind::Alnum
        } else {
            Kind::Punct
        };
        // A Thai / Lao vowel written before its consonant belongs with it.
        if let Some(last) = out.last_mut() {
            if last.kind == Kind::Wide && last.to == i && text[last.from..last.to].chars().all(is_prefix_vowel) && kind == Kind::Wide {
                last.to = end;
                continue;
            }
        }
        out.push(Cluster { from: i, to: end, kind });
    }
    out
}

/// Byte ranges of the words of `text`: runs of non-space characters, except that characters of
/// scripts written without spaces are one word each (a run of kana / ideographs is cut into
/// single characters, punctuation sticks to the one before it).
pub fn tokenize(text: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut open = false; // the last token can still take plain characters
    for c in clusters(text) {
        match c.kind {
            Kind::Space => open = false,
            Kind::Wide => {
                out.push((c.from, c.to));
                open = false;
            }
            Kind::Alnum | Kind::Punct => match out.last_mut() {
                // Punctuation right after a token sticks to it; letters after a wide char start anew.
                Some(last) if last.1 == c.from && (open || c.kind == Kind::Punct) => {
                    last.1 = c.to;
                    open = true;
                }
                _ => {
                    out.push((c.from, c.to));
                    open = true;
                }
            },
        }
    }
    out
}

fn is_vowel(c: char) -> bool {
    matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u' | 'y') || "àáâãäåæèéêëìíîïòóôõöøùúûüýÿ".contains(c.to_lowercase().next().unwrap_or(c))
}

/// How long a word takes to sing, in arbitrary units (a short word is about 3, a long one 8+).
fn weight(tok: &str) -> f32 {
    let letters = tok.chars().filter(|c| c.is_alphanumeric()).count();
    if letters == 0 {
        return 0.3;
    }
    let punct = tok.chars().filter(|c| !c.is_alphanumeric()).count();
    let wide = tok.chars().any(|c| is_wide(c) && c.is_alphanumeric());
    let w = if wide {
        2.0 * letters as f32
    } else if tok.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_ascii_alphabetic() || ('\u{c0}'..='\u{24f}').contains(&c)) {
        // Latin script: syllables (runs of vowels) matter most, letters a little.
        let mut syl = 0usize;
        let mut prev = false;
        for c in tok.chars() {
            let v = is_vowel(c);
            if v && !prev {
                syl += 1;
            }
            prev = v;
        }
        1.6 * syl.max(1) as f32 + 0.45 * letters as f32
    } else {
        // Hangul, Cyrillic, Greek, ...: about one syllable per letter.
        1.1 * letters as f32
    };
    w.max(1.5) + 0.12 * punct as f32
}

/// Silence after a word, in the same units, from its closing punctuation.
fn pause(tok: &str) -> f32 {
    let t = tok.trim_end_matches(['"', '\'', ')', ']', '}', '”', '’', '」', '』', '»']);
    match t.chars().last() {
        Some('.' | '!' | '?' | '…' | '。' | '！' | '？') => 2.5,
        Some(',' | ';' | ':' | '、' | '，' | '；' | '：' | '—' | '–') => 1.2,
        _ => 0.0,
    }
}

/// Estimated time to sing `text` at a natural pace, ms (before the slack).
fn natural_ms(text: &str) -> f32 {
    text.chars()
        .map(|c| {
            if is_wide(c) && c.is_alphanumeric() {
                2.5 * MS_PER_CHAR
            } else if c.is_alphanumeric() {
                MS_PER_CHAR
            } else {
                0.0
            }
        })
        .sum()
}

/// How long the line is sung for: its natural time with some slack, kept between 1.2 s and 8 s,
/// but never more than its slot (the gap to the next line minus [`TAIL_MS`]) allows. Without a
/// next line the slot is [`LAST_LINE_GAP_MS`].
pub fn sung_ms(text: &str, gap_ms: Option<u64>) -> u64 {
    let wanted = (natural_ms(text) * SLACK).clamp(MIN_SUNG_MS, MAX_SUNG_MS);
    let slot = gap_ms.unwrap_or(LAST_LINE_GAP_MS).saturating_sub(TAIL_MS) as f32;
    wanted.min(slot) as u64
}

/// Estimated word times of a line that starts at `start_ms`, the next line at `next_ms`. Empty
/// when there is nothing to light up (an empty line, "♪", "...").
pub fn estimate(text: &str, start_ms: u64, next_ms: Option<u64>) -> Vec<WordSpan> {
    let toks = tokenize(text);
    let units: Vec<(f32, f32)> = toks.iter().map(|&(a, b)| (weight(&text[a..b]), pause(&text[a..b]))).collect();
    if !text.chars().any(|c| c.is_alphanumeric()) || toks.is_empty() {
        return Vec::new();
    }
    let gap = next_ms.map(|n| n.saturating_sub(start_ms));
    let sung = sung_ms(text, gap) as f64;
    let n = toks.len();
    // Total weight: words plus the pauses between them (no pause after the last word).
    let total: f64 = units.iter().enumerate().map(|(i, &(w, p))| w as f64 + if i + 1 < n { p as f64 } else { 0.0 }).sum();
    let end = start_ms + sung as u64;
    let mut out: Vec<WordSpan> = Vec::with_capacity(n);
    let mut cum = 0.0f64;
    for (i, (&(a, b), &(w, p))) in toks.iter().zip(&units).enumerate() {
        let s = start_ms + (sung * cum / total).round() as u64;
        cum += w as f64;
        let e = if i + 1 == n { end } else { start_ms + (sung * cum / total).round() as u64 };
        if i + 1 < n {
            cum += p as f64;
        }
        let s = s.max(out.last().map_or(start_ms, |l: &WordSpan| l.end_ms)).min(end);
        out.push(WordSpan { start_ms: s, end_ms: e.max(s).min(end), from: a, to: b });
    }
    out
}

/// The real word times of an enhanced-LRC line, or `None` for a plain line. A word lasts until
/// the next one starts; the last one until the closing stamp, else for a time typical for its
/// length, in any case ending [`TAIL_MS`] before the next line.
pub fn real(line: &Line, next_ms: Option<u64>) -> Option<Vec<WordSpan>> {
    let words = line.words.as_ref()?;
    let ranges = line.word_ranges()?;
    let n = words.len();
    let limit = next_ms.map(|nx| nx.saturating_sub(TAIL_MS));
    let mut out: Vec<WordSpan> = Vec::with_capacity(n);
    for i in 0..n {
        let s = words[i].t_ms.max(out.last().map_or(0, |l: &WordSpan| l.end_ms));
        let e = if i + 1 < n {
            words[i + 1].t_ms
        } else {
            let own = (natural_ms(&words[i].text) * SLACK).clamp(400.0, 2500.0) as u64;
            line.end_ms.unwrap_or(s + own)
        };
        let e = limit.map_or(e, |l| e.min(l.max(s)));
        out.push(WordSpan { start_ms: s, end_ms: e.max(s), from: ranges[i].0, to: ranges[i].1 });
    }
    Some(out)
}

/// Word times for a line: the real ones when the lyrics have them, else the estimate.
pub fn spans(line: &Line, next_ms: Option<u64>) -> Vec<WordSpan> {
    real(line, next_ms).unwrap_or_else(|| estimate(&line.text, line.t, next_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks<'a>(s: &'a str) -> Vec<&'a str> {
        tokenize(s).into_iter().map(|(a, b)| &s[a..b]).collect()
    }

    /// The invariants of any estimate.
    fn check(text: &str, start: u64, next: Option<u64>) -> Vec<WordSpan> {
        let sp = estimate(text, start, next);
        let slot_end = next.map(|n| n.saturating_sub(TAIL_MS).max(start)).unwrap_or(start + LAST_LINE_GAP_MS);
        for (i, w) in sp.iter().enumerate() {
            assert!(w.start_ms >= start && w.start_ms <= w.end_ms, "{text:?}: {w:?}");
            assert!(w.end_ms <= slot_end.max(start), "{text:?}: ends after its slot ({} > {slot_end})", w.end_ms);
            assert!(w.from < w.to && w.to <= text.len() && text.is_char_boundary(w.from) && text.is_char_boundary(w.to), "{text:?}: {w:?}");
            if i > 0 {
                assert!(sp[i - 1].end_ms <= w.start_ms, "{text:?}: overlap {:?} {w:?}", sp[i - 1]);
                assert!(sp[i - 1].to <= w.from, "{text:?}: ranges overlap");
            }
        }
        sp
    }

    #[test]
    fn tokens_by_whitespace() {
        assert_eq!(toks("lampu kota  memudar"), ["lampu", "kota", "memudar"]);
        assert_eq!(toks("  hello, world!  "), ["hello,", "world!"]);
        assert_eq!(toks("don't stop—believing"), ["don't", "stop—believing"]);
        assert!(toks("").is_empty() && toks("   ").is_empty());
    }

    #[test]
    fn spaceless_scripts_are_cut_per_character() {
        assert_eq!(toks("夜に駆ける"), ["夜", "に", "駆", "け", "る"]);
        // Punctuation sticks to the character before it; a space starts a new group.
        assert_eq!(toks("夜に、駆ける。"), ["夜", "に、", "駆", "け", "る。"]);
        assert_eq!(toks("你好 world"), ["你", "好", "world"]);
        // Latin inside CJK stays whole.
        assert_eq!(toks("Love你"), ["Love", "你"]);
        // Thai: a vowel written before its consonant and the marks above stay with their letter.
        assert_eq!(toks("เกม"), ["เก", "ม"]);
        assert_eq!(toks("ที่"), ["ที่"]);
        // Hangul is spaced like Latin.
        assert_eq!(toks("사랑해 요"), ["사랑해", "요"]);
    }

    #[test]
    fn emoji_and_combining_marks_do_not_split() {
        assert_eq!(toks("🎵 la la 🎶"), ["🎵", "la", "la", "🎶"]);
        assert_eq!(toks("👨\u{200d}👩\u{200d}👧 ok"), ["👨\u{200d}👩\u{200d}👧", "ok"]);
        assert_eq!(toks("e\u{301}te\u{301}"), ["e\u{301}te\u{301}"]);
        assert_eq!(toks("👍🏽 go"), ["👍🏽", "go"]);
    }

    #[test]
    fn rtl_lines_are_recognised() {
        assert!(is_rtl("مرحبا بالعالم"));
        assert!(is_rtl("שלום עולם"));
        assert!(!is_rtl("hello world"));
        assert!(!is_rtl("夜に駆ける"));
        assert!(!is_rtl(""));
        // A line that is mostly Latin with one Arabic word is not RTL.
        assert!(!is_rtl("this is a song مرحبا about love"));
    }

    #[test]
    fn a_short_line_fills_its_slot_a_long_gap_is_not_stretched() {
        let text = "lampu kota memudar di kaca jendela"; // 30 letters
        // Slot 3 s: shorter than the line's own time, so the line fills it (minus the tail).
        let sp = check(text, 20_000, Some(23_000));
        assert_eq!(sp.first().unwrap().start_ms, 20_000);
        assert_eq!(sp.last().unwrap().end_ms, 23_000 - TAIL_MS);
        // Slot 5 s: sung for its own (slackened) time, 29 letters x 80 ms x 1.75, a little less than the slot.
        let sp = check(text, 20_000, Some(25_000));
        assert_eq!(sp.last().unwrap().end_ms, 20_000 + 4060);
        // A 60 s instrumental gap after the line: it is sung for its own time, not 60 s.
        let sp = check(text, 20_000, Some(80_000));
        let sung = sp.last().unwrap().end_ms - 20_000;
        assert_eq!(sung, 4060, "sung for its own time, not 60 s");
        // A one-word line in a long gap: 1.2 s, the minimum.
        let sp = check("oh", 5_000, Some(65_000));
        assert_eq!(sp.last().unwrap().end_ms, 5_000 + 1200);
    }

    #[test]
    fn the_last_line_uses_the_default_slot() {
        let sp = check("tentang esok yang belum bernama", 200_000, None);
        assert!(sp.last().unwrap().end_ms <= 200_000 + LAST_LINE_GAP_MS - TAIL_MS);
        assert!(sp.last().unwrap().end_ms > 200_000 + 1200);
    }

    #[test]
    fn longer_words_take_longer_and_pauses_leave_gaps() {
        let sp = check("a extraordinary", 0, Some(10_000));
        assert!(sp[1].end_ms - sp[1].start_ms > 3 * (sp[0].end_ms - sp[0].start_ms), "{sp:?}");
        // Words are in the order of the text; every word has a positive length here.
        assert!(sp.iter().all(|w| w.end_ms > w.start_ms));
        // A comma leaves a short silence before the next word.
        let sp = check("wait, for me", 0, Some(10_000));
        assert!(sp[1].start_ms > sp[0].end_ms, "pause after the comma: {sp:?}");
        let sp = check("wait for me", 0, Some(10_000));
        assert_eq!(sp[1].start_ms, sp[0].end_ms, "no pause without punctuation");
        // The pause after a full stop is longer than after a comma.
        let c = check("go, home now", 0, Some(10_000));
        let d = check("go. home now", 0, Some(10_000));
        assert!(d[1].start_ms - d[0].end_ms > c[1].start_ms - c[0].end_ms);
    }

    #[test]
    fn word_ranges_cover_the_text_in_order() {
        let text = "di bawah lampu jalan";
        let sp = check(text, 1000, Some(6000));
        let words: Vec<&str> = sp.iter().map(|w| &text[w.from..w.to]).collect();
        assert_eq!(words, ["di", "bawah", "lampu", "jalan"]);
    }

    #[test]
    fn one_word_empty_and_silent_lines() {
        let sp = check("hallelujah", 3000, Some(9000));
        assert_eq!(sp.len(), 1);
        assert_eq!((sp[0].start_ms, sp[0].from, sp[0].to), (3000, 0, 10));
        assert!(estimate("", 0, Some(5000)).is_empty());
        assert!(estimate("   ", 0, Some(5000)).is_empty());
        assert!(estimate("♪", 0, Some(5000)).is_empty());
        assert!(estimate("...", 0, None).is_empty());
        assert!(estimate("♪ ♫", 0, None).is_empty());
    }

    #[test]
    fn a_zero_gap_gives_empty_spans_at_the_start() {
        // Two lines with the same timestamp: nothing to sweep, but still valid and monotonic.
        let sp = check("hello world", 4000, Some(4000));
        assert!(sp.iter().all(|w| w.start_ms == 4000 && w.end_ms == 4000), "{sp:?}");
        let sp = check("hello world", 4000, Some(4100));
        assert!(sp.last().unwrap().end_ms <= 4000);
        // The next line before this one (unsorted input): same.
        check("hello world", 4000, Some(1000));
    }

    #[test]
    fn unicode_lines() {
        let sp = check("夜に駆ける", 1000, Some(6000));
        assert_eq!(sp.len(), 5);
        // Equal weight per character: five equal parts.
        let d: Vec<u64> = sp.iter().map(|w| w.end_ms - w.start_ms).collect();
        assert!(d.iter().max().unwrap() - d.iter().min().unwrap() <= 2, "{d:?}");
        // CJK takes longer per character than Latin.
        let cjk = check("夜に駆ける", 0, Some(60_000)).last().unwrap().end_ms;
        let lat = check("yoru ni", 0, Some(60_000)).last().unwrap().end_ms;
        assert!(cjk >= lat);
        check("사랑해 요 정말로", 0, Some(5000));
        check("é́ ñandú über Straße", 0, Some(5000));
        check("🎵 la la la 🎶", 0, Some(5000));
        check("ที่รัก เธอคือทุกอย่าง", 0, Some(6000));
        check("مرحبا بالعالم", 0, Some(6000));
    }

    #[test]
    fn a_very_long_line_is_capped_at_eight_seconds() {
        let text = "word ".repeat(500);
        let sp = check(text.trim(), 0, Some(120_000));
        assert_eq!(sp.len(), 500);
        assert_eq!(sp.last().unwrap().end_ms, 8000);
        // Even with a huge text the spans stay valid.
        let text = "supercalifragilistic ".repeat(2000);
        check(text.trim(), 10, None);
    }

    #[test]
    fn estimates_are_valid_for_any_slot() {
        let lines = ["la", "lampu kota memudar", "a, b. c! d? e…", "x", "  spaced   out  ", "-", "(oh) yeah", "\"quoted\", then"];
        for text in lines {
            for gap in [0u64, 1, 100, 149, 150, 151, 500, 1200, 1350, 3000, 7999, 8200, 60_000] {
                check(text, 7777, Some(7777 + gap));
            }
            check(text, 0, None);
        }
    }

    fn enhanced(src: &str) -> Line {
        super::super::lrc::parse(src).lines.remove(0)
    }

    #[test]
    fn real_stamps_are_used_as_they_are() {
        let l = enhanced("[00:12.00]<00:12.00>Hello <00:12.50>big <00:13.20>world<00:14.00>");
        let sp = real(&l, Some(20_000)).unwrap();
        let t: Vec<(u64, u64)> = sp.iter().map(|w| (w.start_ms, w.end_ms)).collect();
        assert_eq!(t, [(12_000, 12_500), (12_500, 13_200), (13_200, 14_000)]);
        assert_eq!(&l.text[sp[1].from..sp[1].to], "big");
        // spans() prefers them over an estimate.
        assert_eq!(spans(&l, Some(20_000)), sp);
        // Without a closing stamp the last word gets a length typical for it; the next line caps it.
        let l = enhanced("[00:12.00]<00:12.00>one <00:12.50>two");
        let sp = real(&l, None).unwrap();
        assert!(sp[1].end_ms > 12_500 && sp[1].end_ms <= 12_500 + 2500);
        let sp = real(&l, Some(12_900)).unwrap();
        assert_eq!(sp[1].end_ms, 12_750, "ends {TAIL_MS} ms before the next line");
    }

    #[test]
    fn plain_lines_have_no_real_spans_and_fall_back_to_the_estimate() {
        let l = Line::plain(1000, "just a line");
        assert!(real(&l, Some(5000)).is_none());
        assert_eq!(spans(&l, Some(5000)), estimate("just a line", 1000, Some(5000)));
    }
}
