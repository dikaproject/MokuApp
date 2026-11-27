//! LRC parsing (pure, no I/O): `[mm:ss.xx]text` lines, several timestamps per line,
//! `[offset:±ms]`, metadata tags ignored. Enhanced-LRC word stamps (`<mm:ss.xx>`) are kept as
//! [`Line::words`] when the file has them (the plain `text` never contains them).
//!
//! The parser is total: whatever the input, it returns (possibly empty) lyrics and never panics.

/// One word (or syllable) of an enhanced-LRC line.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    /// When it starts, in ms (offset applied; never before the line's start, never before the
    /// previous word).
    pub t_ms: u64,
    /// Its text without surrounding whitespace; the words joined in order (a space wherever the
    /// source had one) are the line's `text`.
    pub text: String,
}

/// One synced lyric line.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Start time in ms (offset applied).
    pub t: u64,
    pub text: String,
    /// Real per-word times (enhanced LRC); `None` for the usual line-level lyrics.
    pub words: Option<Vec<Word>>,
    /// The time of the closing word stamp (`... last<00:15.20>`), when the file has one.
    pub end_ms: Option<u64>,
}

impl Line {
    /// A plain line without word data.
    pub fn plain(t: u64, text: impl Into<String>) -> Line {
        Line { t, text: text.into(), words: None, end_ms: None }
    }

    /// Byte ranges of `words` inside `text` (in order), or `None` when there are no words or
    /// they cannot be found in the text (then the line is treated as a plain one).
    pub fn word_ranges(&self) -> Option<Vec<(usize, usize)>> {
        let words = self.words.as_ref()?;
        let mut at = 0usize;
        let mut out = Vec::with_capacity(words.len());
        for w in words {
            let i = self.text.get(at..)?.find(w.text.as_str())? + at;
            let j = i + w.text.len();
            out.push((i, j));
            at = j;
        }
        Some(out)
    }
}

/// Parsed synced lyrics, sorted by time.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Lyrics {
    pub lines: Vec<Line>,
}

/// `mm:ss`, `mm:ss.x`, `mm:ss.xx`, `mm:ss.xxx` or `mm:ss:xx` → ms.
fn parse_stamp(s: &str) -> Option<u64> {
    let (min, rest) = s.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    // No song is longer than this; it also keeps the arithmetic below from overflowing.
    if min > 99_999 {
        return None;
    }
    let (sec, frac) = match rest.find(['.', ':']) {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    if sec.is_empty() || !sec.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let sec: u64 = sec.parse().ok()?;
    if sec >= 60 {
        return None;
    }
    let ms = match frac {
        None => 0,
        Some(f) if !f.is_empty() && f.len() <= 3 && f.bytes().all(|b| b.is_ascii_digit()) => {
            let v: u64 = f.parse().ok()?;
            match f.len() {
                1 => v * 100,
                2 => v * 10,
                _ => v,
            }
        }
        Some(_) => return None,
    };
    Some(min * 60_000 + sec * 1000 + ms)
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text of one line body cut at its valid `<mm:ss.xx>` stamps.
struct Body {
    /// The plain text (stamps removed, whitespace collapsed).
    text: String,
    /// `(stamp, text after it)`; the text before the first stamp comes first with no stamp.
    /// Empty when the line has no stamps at all.
    parts: Vec<(Option<u64>, String)>,
}

fn split_body(s: &str) -> Body {
    let mut parts: Vec<(Option<u64>, String)> = vec![(None, String::new())];
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        let Some(j) = rest[i + 1..].find('>') else { break };
        match parse_stamp(&rest[i + 1..i + 1 + j]) {
            Some(t) => {
                if let Some(last) = parts.last_mut() {
                    last.1.push_str(&rest[..i]);
                }
                parts.push((Some(t), String::new()));
                rest = &rest[i + 2 + j..];
            }
            None => {
                if let Some(last) = parts.last_mut() {
                    last.1.push_str(&rest[..=i]);
                }
                rest = &rest[i + 1..];
            }
        }
    }
    if let Some(last) = parts.last_mut() {
        last.1.push_str(rest);
    }
    let raw: String = parts.iter().map(|p| p.1.as_str()).collect();
    let stamped = parts.len() > 1;
    Body { text: collapse(&raw), parts: if stamped { parts } else { Vec::new() } }
}

/// The words and the closing stamp of a stamped body; `time` maps a file stamp to the line's
/// time base (offset and copy shift applied). Times never go backwards and never start before
/// `line_t`.
fn words_of(parts: &[(Option<u64>, String)], line_t: u64, time: &dyn Fn(u64) -> u64) -> (Vec<Word>, Option<u64>) {
    let mut words: Vec<Word> = Vec::new();
    let mut end = None;
    let mut prev = line_t;
    let last = parts.len().saturating_sub(1);
    for (i, (stamp, text)) in parts.iter().enumerate() {
        let text = collapse(text);
        let t = match stamp {
            Some(t) => time(*t).max(prev),
            None => line_t,
        };
        if text.is_empty() {
            // A stamp with nothing after it ends the last word when it closes the line.
            if i == last && stamp.is_some() {
                end = Some(t);
            }
            continue;
        }
        prev = t;
        words.push(Word { t_ms: t, text });
    }
    (words, end)
}

/// Parse LRC text. Lines without a timestamp and metadata tags are skipped. Empty lyric lines
/// (instrumental breaks) are kept with empty text.
pub fn parse(src: &str) -> Lyrics {
    let mut offset: i64 = 0;
    // (timestamp, shift of the word stamps for this copy of the line, body)
    let mut raw: Vec<(u64, i64, Body)> = Vec::new();
    for line in src.lines() {
        let mut rest = line.trim_start_matches('\u{feff}').trim();
        let mut stamps = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            let tag = &rest[1..end];
            if let Some(t) = parse_stamp(tag) {
                stamps.push(t);
            } else if let Some((k, v)) = tag.split_once(':') {
                if k.trim().eq_ignore_ascii_case("offset") {
                    offset = v.trim().trim_start_matches('+').parse().unwrap_or(0);
                }
            }
            rest = rest[end + 1..].trim_start();
        }
        let Some(&first) = stamps.first() else { continue };
        let body = split_body(rest.trim());
        for &t in &stamps {
            // Word stamps are absolute; a copy of the line at another timestamp moves them along.
            let delta = t as i64 - first as i64;
            raw.push((t, delta, Body { text: body.text.clone(), parts: body.parts.clone() }));
        }
    }
    // A positive offset makes the lyrics appear sooner.
    let at = |t: i64| t.saturating_sub(offset).max(0) as u64;
    let mut lines: Vec<Line> = Vec::with_capacity(raw.len());
    for (t, delta, b) in raw {
        let line_t = at(t as i64);
        let (words, end_ms) = if b.parts.is_empty() {
            (None, None)
        } else {
            let (w, end) = words_of(&b.parts, line_t, &|x| at((x as i64).saturating_add(delta)));
            if w.is_empty() { (None, None) } else { (Some(w), end) }
        };
        let mut line = Line { t: line_t, text: b.text, words, end_ms };
        // Words that cannot be found in the text again make it a plain line.
        if line.words.is_some() && line.word_ranges().is_none() {
            line.words = None;
            line.end_ms = None;
        }
        lines.push(line);
    }
    lines.sort_by_key(|l| l.t);
    Lyrics { lines }
}

impl Lyrics {
    /// Index of the line being sung at `pos_ms` (the last line that started), if any.
    pub fn line_at(&self, pos_ms: u64) -> Option<usize> {
        self.lines.partition_point(|l| l.t <= pos_ms).checked_sub(1)
    }

    /// Start time of the first line after `pos_ms`.
    pub fn next_change(&self, pos_ms: u64) -> Option<u64> {
        let i = self.lines.partition_point(|l| l.t <= pos_ms);
        self.lines.get(i).map(|l| l.t)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.text.is_empty())
    }

    /// The same lyrics with every time moved by `delta_ms` (positive = later; a line cannot start
    /// before 0). Word times and the closing stamp move with their line, so the highlight follows.
    pub fn shifted(&self, delta_ms: i64) -> Lyrics {
        let at = |t: u64| (t as i64).saturating_add(delta_ms).max(0) as u64;
        let lines = self
            .lines
            .iter()
            .map(|l| Line {
                t: at(l.t),
                text: l.text.clone(),
                words: l.words.as_ref().map(|w| w.iter().map(|w| Word { t_ms: at(w.t_ms), text: w.text.clone() }).collect()),
                end_ms: l.end_ms.map(at),
            })
            .collect();
        Lyrics { lines }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(l: &Lyrics) -> Vec<(u64, &str)> {
        l.lines.iter().map(|l| (l.t, l.text.as_str())).collect()
    }

    fn words(l: &Line) -> Vec<(u64, &str)> {
        l.words.as_ref().map(|w| w.iter().map(|w| (w.t_ms, w.text.as_str())).collect()).unwrap_or_default()
    }

    #[test]
    fn lrc_parse_basic() {
        let l = parse("[ar:Kaia]\n[ti:Midnight Drive]\n[00:01.50]lampu kota\n[00:04.25] dan kota pun \n\nno stamp here\n[01:02]di bawah lampu\n[01:05.123]\n");
        assert_eq!(texts(&l), vec![(1500, "lampu kota"), (4250, "dan kota pun"), (62_000, "di bawah lampu"), (65_123, "")]);
        // Line-level lyrics carry no word data.
        assert!(l.lines.iter().all(|x| x.words.is_none() && x.end_ms.is_none()));
    }

    #[test]
    fn lrc_parse_offset_and_multi_timestamps() {
        let l = parse("[offset:+500]\n[00:10.00][00:30.00]chorus\n[00:20.00]verse <00:20.50>two <00:21.00>words\n[00:00.20]start");
        // +500 ms offset → everything 0.5 s earlier (clamped at 0); the chorus appears twice.
        assert_eq!(texts(&l), vec![(0, "start"), (9500, "chorus"), (19_500, "verse two words"), (29_500, "chorus")]);
        let l = parse("[offset:-250]\n[00:01.00]a");
        assert_eq!(texts(&l), vec![(1250, "a")]);
        // Colon-separated hundredths and three-digit ms.
        let l = parse("[00:02:50]b\n[00:01.005]a");
        assert_eq!(texts(&l), vec![(1005, "a"), (2500, "b")]);
    }

    #[test]
    fn shifting_moves_lines_words_and_ends_and_stops_at_zero() {
        let l = parse("[00:01.00]early
[00:10.00]<00:10.00>Hello <00:10.50>world<00:11.00>");
        let later = l.shifted(500);
        assert_eq!(texts(&later), vec![(1500, "early"), (10_500, "Hello world")]);
        assert_eq!(words(&later.lines[1]), vec![(10_500, "Hello"), (11_000, "world")]);
        assert_eq!(later.lines[1].end_ms, Some(11_500));
        let sooner = l.shifted(-5000);
        assert_eq!(texts(&sooner), vec![(0, "early"), (5000, "Hello world")], "never before 0");
        assert_eq!(l.shifted(0), l);
        assert_eq!(later.shifted(-500), l);
        assert_eq!(later.line_at(10_400), Some(0), "the line has not started yet");
        assert_eq!(later.line_at(10_500), Some(1));
        // Lyrics shifted by the user come on top of the file's own [offset].
        let o = parse("[offset:+500]
[00:10.00]a");
        assert_eq!(texts(&o.shifted(500)), vec![(10_000, "a")]);
    }

    #[test]
    fn lyric_line_at_position() {
        let l = parse("[00:01.00]one\n[00:03.00]two\n[00:05.00]three");
        assert_eq!(l.line_at(0), None);
        assert_eq!(l.line_at(999), None);
        assert_eq!(l.line_at(1000), Some(0));
        assert_eq!(l.line_at(2999), Some(0));
        assert_eq!(l.line_at(3000), Some(1));
        assert_eq!(l.line_at(60_000), Some(2));
        assert_eq!(l.next_change(0), Some(1000));
        assert_eq!(l.next_change(3000), Some(5000));
        assert_eq!(l.next_change(5000), None);
        assert_eq!(Lyrics::default().line_at(100), None);
    }

    #[test]
    fn bad_stamps_are_not_lines() {
        let l = parse("[xx:yy]nope\n[00:99.00]nope\n[length: 03:47]\n[00:01.00]ok");
        assert_eq!(texts(&l), vec![(1000, "ok")]);
    }

    #[test]
    fn enhanced_lrc_keeps_word_stamps() {
        let l = parse("[00:12.00]<00:12.00>Hello <00:12.50>big <00:13.20>world<00:14.00>\n[00:15.00]plain line");
        let a = &l.lines[0];
        assert_eq!(a.text, "Hello big world", "the plain text has no stamps");
        assert_eq!(words(a), vec![(12_000, "Hello"), (12_500, "big"), (13_200, "world")]);
        assert_eq!(a.end_ms, Some(14_000), "the closing stamp ends the last word");
        assert_eq!(a.word_ranges(), Some(vec![(0, 5), (6, 9), (10, 15)]));
        assert!(l.lines[1].words.is_none());
    }

    #[test]
    fn enhanced_lrc_text_before_the_first_stamp_starts_with_the_line() {
        let l = parse("[00:10.00]Intro <00:10.80>word");
        assert_eq!(words(&l.lines[0]), vec![(10_000, "Intro"), (10_800, "word")]);
        assert_eq!(l.lines[0].end_ms, None);
    }

    #[test]
    fn enhanced_lrc_syllables_rebuild_the_text() {
        let l = parse("[00:01.00]<00:01.00>Mi<00:01.30>dnight <00:02.00>Dri<00:02.40>ve");
        let a = &l.lines[0];
        assert_eq!(a.text, "Midnight Drive");
        assert_eq!(words(a), vec![(1000, "Mi"), (1300, "dnight"), (2000, "Dri"), (2400, "ve")]);
        assert_eq!(a.word_ranges(), Some(vec![(0, 2), (2, 8), (9, 12), (12, 14)]));
    }

    #[test]
    fn enhanced_lrc_follows_offset_and_copies_and_never_goes_backwards() {
        let l = parse("[offset:+500]\n[00:10.00][00:30.00]<00:10.00>a <00:09.00>b <00:11.00>c");
        let first = &l.lines[0];
        // 0.5 s earlier; the out-of-order stamp is held at the previous word's time.
        assert_eq!(words(first), vec![(9500, "a"), (9500, "b"), (10_500, "c")]);
        // The chorus copy at 0:30 moves its word stamps along by 20 s.
        let copy = &l.lines[1];
        assert_eq!(copy.t, 29_500);
        assert_eq!(words(copy), vec![(29_500, "a"), (29_500, "b"), (30_500, "c")]);
        // A word stamp before the line's own stamp is clamped to the line start.
        let l = parse("[00:10.00]<00:09.00>x <00:10.50>y");
        assert_eq!(words(&l.lines[0]), vec![(10_000, "x"), (10_500, "y")]);
    }

    #[test]
    fn enhanced_lrc_odd_stamps_do_not_make_words() {
        // Stamps with nothing between them, a stamp-only line and a malformed stamp.
        let l = parse("[00:01.00]<00:01.00><00:01.50>solo\n[00:02.00]<00:02.00>\n[00:03.00]a <xx:yy> b <00:03.5");
        assert_eq!(l.lines[0].text, "solo");
        assert_eq!(words(&l.lines[0]), vec![(1500, "solo")]);
        assert_eq!(l.lines[1].text, "");
        assert!(l.lines[1].words.is_none());
        assert_eq!(l.lines[2].text, "a <xx:yy> b <00:03.5");
        assert!(l.lines[2].words.is_none());
    }

    #[test]
    fn unicode_words_survive() {
        let l = parse("[00:05.00]<00:05.00>夜に<00:05.40>駆ける <00:06.00>🎵 <00:06.40>ok");
        let a = &l.lines[0];
        assert_eq!(a.text, "夜に駆ける 🎵 ok");
        let r = a.word_ranges().expect("ranges");
        assert_eq!(&a.text[r[0].0..r[0].1], "夜に");
        assert_eq!(&a.text[r[1].0..r[1].1], "駆ける");
        assert_eq!(&a.text[r[2].0..r[2].1], "🎵");
        assert_eq!(&a.text[r[3].0..r[3].1], "ok");
    }

    /// Whatever the input, the parser neither panics nor returns lines out of order.
    #[test]
    fn parser_is_total_on_garbage() {
        let alphabet: Vec<&str> = vec![
            "[", "]", "<", ">", ":", ".", "0", "1", "5", "9", "99", "-", "+", "offset", "offset:", " ", "\n", "\r\n", "\u{feff}", "a", "é", "字", "🎵", "\u{0}", "00:01.00", "<00:02.00>", "[00:03.5]", "[offset:+9999999999999999999]",
            "[offset:-9223372036854775808]", "[99999999999999999999:00]", "[99999:59.999]", "<99999:59.999>",
        ];
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..4000 {
            let n = (next() % 40) as usize;
            let s: String = (0..n).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
            let l = parse(&s);
            assert!(l.lines.windows(2).all(|w| w[0].t <= w[1].t), "{s:?}");
            for line in &l.lines {
                if let Some(ws) = &line.words {
                    assert!(!ws.is_empty() && line.word_ranges().is_some(), "{s:?}");
                    assert!(ws.windows(2).all(|w| w[0].t_ms <= w[1].t_ms), "{s:?}");
                    assert!(ws.iter().all(|w| w.t_ms >= line.t && !w.text.is_empty()), "{s:?}");
                }
            }
        }
        // A few hand-picked nasties.
        for s in ["[", "]", "<", "[00:01.00]<", "[00:01.00]>", "[00:01.00]<>", "[00:01.00]<<00:01.00>", "[offset", "[offset:]", "[:]", "[00:01.00][", "[00:01.00]][", "\u{feff}[00:01.00]x"] {
            let _ = parse(s);
        }
    }
}
