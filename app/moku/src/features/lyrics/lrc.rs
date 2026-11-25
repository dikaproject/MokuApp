//! LRC parsing (pure, no I/O): `[mm:ss.xx]text` lines, several timestamps per line,
//! `[offset:±ms]`, metadata tags ignored, enhanced word stamps (`<mm:ss.xx>`) stripped.

/// One synced lyric line.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Start time in ms (offset applied).
    pub t: u64,
    pub text: String,
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

/// Remove enhanced-LRC word timestamps (`<00:12.34>`).
fn strip_word_stamps(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        match rest[i + 1..].find('>') {
            Some(j) if parse_stamp(&rest[i + 1..i + 1 + j]).is_some() => {
                out.push_str(&rest[..i]);
                rest = &rest[i + 2 + j..];
            }
            _ => {
                out.push_str(&rest[..=i]);
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    // Collapse the double spaces word stamps leave behind.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse LRC text. Lines without a timestamp and metadata tags are skipped. Empty lyric lines
/// (instrumental breaks) are kept with empty text.
pub fn parse(src: &str) -> Lyrics {
    let mut offset: i64 = 0;
    let mut raw: Vec<(u64, String)> = Vec::new();
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
        if stamps.is_empty() {
            continue;
        }
        let text = strip_word_stamps(rest.trim());
        for t in stamps {
            raw.push((t, text.clone()));
        }
    }
    // A positive offset makes the lyrics appear sooner.
    let mut lines: Vec<Line> = raw.into_iter().map(|(t, text)| Line { t: (t as i64 - offset).max(0) as u64, text }).collect();
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(l: &Lyrics) -> Vec<(u64, &str)> {
        l.lines.iter().map(|l| (l.t, l.text.as_str())).collect()
    }

    #[test]
    fn lrc_parse_basic() {
        let l = parse("[ar:Kaia]\n[ti:Midnight Drive]\n[00:01.50]lampu kota\n[00:04.25] dan kota pun \n\nno stamp here\n[01:02]di bawah lampu\n[01:05.123]\n");
        assert_eq!(texts(&l), vec![(1500, "lampu kota"), (4250, "dan kota pun"), (62_000, "di bawah lampu"), (65_123, "")]);
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
}
