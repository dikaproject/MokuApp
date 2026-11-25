//! Fuzzy matching for the launcher: exact > prefix > word prefix > acronym ("vsc" → Visual
//! Studio Code) > substring > subsequence, plus a usage boost (see `history.rs`) that reorders
//! entries within about one tier. Names are prepared once when the index is built, so a query
//! over ~1000 entries costs well under a millisecond.

/// Fold for matching: lower case, common Latin accents removed (é → e).
pub fn fold(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).map(unaccent).collect()
}

fn unaccent(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        _ => c,
    }
}

/// A name ready for matching.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prepared {
    /// Folded characters.
    chars: Vec<char>,
    /// Indices into `chars` where a word starts (start, after a separator, camelCase hump,
    /// letter ↔ digit change).
    starts: Vec<usize>,
    /// First letter of every word, folded.
    acronym: Vec<char>,
}

impl Prepared {
    pub fn new(name: &str) -> Prepared {
        let orig: Vec<char> = name.chars().collect();
        let chars: Vec<char> = fold(name).chars().collect();
        // `fold` maps one char to one char for everything but a few exotic lowercasings; fall
        // back to plain separators when the lengths differ.
        let same = orig.len() == chars.len();
        let mut starts = vec![];
        for i in 0..chars.len() {
            if !chars[i].is_alphanumeric() {
                continue;
            }
            let start = i == 0 || {
                let p = chars[i - 1];
                !p.is_alphanumeric() || (same && orig[i].is_uppercase() && orig[i - 1].is_lowercase()) || (p.is_alphabetic() != chars[i].is_alphabetic())
            };
            if start {
                starts.push(i);
            }
        }
        let acronym = starts.iter().map(|&i| chars[i]).collect();
        Prepared { chars, starts, acronym }
    }
}

/// How a query matched (better first).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Tier {
    Exact,
    Prefix,
    WordPrefix,
    Acronym,
    Substring,
    Subsequence,
}

fn starts_with(hay: &[char], needle: &[char]) -> bool {
    hay.len() >= needle.len() && hay[..needle.len()] == *needle
}

fn find(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// Score of `query` (already [`fold`]ed, trimmed) against `p`, or `None` if it does not match.
/// Tiers are far apart (exact 1000, prefix ≥ 900, word prefix ≥ 800, acronym ≥ 700, substring
/// ≥ 600, subsequence ≤ 560) so a usage boost of at most [`super::history::MAX_BOOST`] reorders
/// within a tier and lifts a much-used entry over a barely better tier only.
pub fn score(query: &str, p: &Prepared) -> Option<(Tier, i32)> {
    let q: Vec<char> = query.chars().collect();
    if q.is_empty() || p.chars.is_empty() {
        return None;
    }
    let n = p.chars.len() as i32;
    let ql = q.len() as i32;
    if p.chars == q {
        return Some((Tier::Exact, 1000));
    }
    if starts_with(&p.chars, &q) {
        return Some((Tier::Prefix, 900 + 40 * ql / n));
    }
    // A later word starts with the query ("code" in "Visual Studio Code").
    if let Some(&w) = p.starts.iter().skip(1).find(|&&s| starts_with(&p.chars[s..], &q)) {
        return Some((Tier::WordPrefix, 800 + 30 * ql / n - (w as i32).min(20)));
    }
    // Several query words, each the prefix of a word of the name, in order ("vis code").
    if q.contains(&' ') {
        let words: Vec<&[char]> = q.split(|c| *c == ' ').filter(|w| !w.is_empty()).collect();
        let mut from = 0;
        let ok = words.iter().all(|w| match p.starts.iter().skip(from).position(|&s| starts_with(&p.chars[s..], w)) {
            Some(k) => {
                from += k + 1;
                true
            }
            None => false,
        });
        if ok && words.len() > 1 {
            return Some((Tier::WordPrefix, 790));
        }
    }
    if ql >= 2 && q.iter().all(|c| c.is_alphanumeric()) {
        if p.acronym == q {
            return Some((Tier::Acronym, 760));
        }
        if starts_with(&p.acronym, &q) {
            return Some((Tier::Acronym, 720 + 20 * ql / p.acronym.len() as i32));
        }
    }
    if let Some(i) = find(&p.chars, &q) {
        return Some((Tier::Substring, 600 + 40 * ql / n - (i as i32).min(30)));
    }
    subsequence(&q, p).map(|s| (Tier::Subsequence, s))
}

/// Every query char in order; consecutive runs and word starts score, gaps cost.
fn subsequence(q: &[char], p: &Prepared) -> Option<i32> {
    let q: Vec<char> = q.iter().copied().filter(|c| *c != ' ').collect();
    if q.len() < 2 {
        return None;
    }
    let mut s = 300;
    let mut j = 0;
    let mut last: Option<usize> = None;
    for (i, &c) in p.chars.iter().enumerate() {
        if j < q.len() && c == q[j] {
            if last == Some(i.wrapping_sub(1)) {
                s += 15;
            }
            if p.starts.contains(&i) {
                s += 20;
            }
            if let Some(l) = last {
                s -= ((i - l - 1) as i32).min(8);
            }
            last = Some(i);
            j += 1;
        }
    }
    (j == q.len()).then_some(s.clamp(200, 560))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc(q: &str, name: &str) -> Option<(Tier, i32)> {
        score(&fold(q), &Prepared::new(name))
    }

    /// Rank names for a query the way the launcher does (score + boost, ties by name).
    fn rank<'a>(q: &str, names: &[(&'a str, i32)]) -> Vec<&'a str> {
        let mut v: Vec<(i32, &str)> = names.iter().filter_map(|(n, boost)| sc(q, n).map(|(_, s)| (s + boost, *n))).collect();
        v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        v.into_iter().map(|(_, n)| n).collect()
    }

    #[test]
    fn tiers() {
        assert_eq!(sc("notepad", "Notepad").unwrap().0, Tier::Exact);
        assert_eq!(sc("note", "Notepad").unwrap().0, Tier::Prefix);
        assert_eq!(sc("code", "Visual Studio Code").unwrap().0, Tier::WordPrefix);
        assert_eq!(sc("vsc", "Visual Studio Code").unwrap().0, Tier::Acronym);
        assert_eq!(sc("vs", "Visual Studio Code").unwrap().0, Tier::Acronym);
        assert_eq!(sc("ps", "PowerShell").unwrap().0, Tier::Acronym, "camelCase humps are words");
        assert_eq!(sc("tepa", "Notepad").unwrap().0, Tier::Substring);
        assert_eq!(sc("ntpd", "Notepad").unwrap().0, Tier::Subsequence);
        assert_eq!(sc("vis code", "Visual Studio Code").unwrap().0, Tier::WordPrefix);
        assert_eq!(sc("kalkulator", "Kalkulator").unwrap().0, Tier::Exact);
        assert_eq!(sc("cafe", "Café Menu").unwrap().0, Tier::Prefix, "accents folded");
        assert!(sc("xyz", "Notepad").is_none());
        assert!(sc("", "Notepad").is_none());
    }

    #[test]
    fn ranking_prefix_over_acronym_over_subsequence() {
        let apps = [("Visual Studio Code", 0), ("VLC media player", 0), ("Voice Recorder", 0), ("Vivaldi", 0), ("Snipping Tool", 0)];
        // "vsc": acronym of VS Code; subsequence of "VLC media player"? no; of "Voice Recorder"? v..c → no s; …
        assert_eq!(rank("vsc", &apps)[0], "Visual Studio Code");
        // "vi": prefix (Vivaldi) beats acronym-free word prefix / subsequence matches.
        assert_eq!(rank("vi", &apps)[0], "Vivaldi");
        let r = rank("vc", &[("Visual Code", 0), ("VideoCast", 0), ("Vector", 0)]);
        // Acronym (Visual Code, VideoCast camelCase) above the subsequence match (Vector).
        assert_eq!(r.last(), Some(&"Vector"));
        assert_eq!(sc("vc", "Vector").unwrap().0, Tier::Subsequence);
        let (p, a, s) = (sc("vi", "Vivaldi").unwrap().1, sc("vsc", "Visual Studio Code").unwrap().1, sc("ntpd", "Notepad").unwrap().1);
        assert!(p > a && a > s, "{p} {a} {s}");
    }

    #[test]
    fn usage_boost_reorders_within_a_tier_only() {
        use crate::features::launcher::history::MAX_BOOST;
        // Two prefix matches: the used one wins.
        assert_eq!(rank("note", &[("Notepad", 0), ("Notes", 0)])[0], "Notes", "shorter name first without usage");
        assert_eq!(rank("note", &[("Notepad", MAX_BOOST), ("Notes", 0)])[0], "Notepad");
        // A much-used acronym / subsequence match never beats an unused prefix match.
        assert_eq!(rank("cod", &[("Cod Liver", 0), ("Calendar Of Duty", MAX_BOOST)])[0], "Cod Liver");
        assert_eq!(rank("ntpd", &[("Notepad", MAX_BOOST), ("ntpdate", 0)])[0], "ntpdate");
    }

    #[test]
    fn fast_enough_for_a_keystroke() {
        let names: Vec<Prepared> = (0..1000).map(|i| Prepared::new(&format!("Application Number {i} Studio Tool"))).collect();
        let t = std::time::Instant::now();
        for q in ["a", "an", "ans", "app num 5", "xyz", "st", "tool", "anst"] {
            let q = fold(q);
            let hits = names.iter().filter_map(|p| score(&q, p)).count();
            assert!(hits <= 1000);
        }
        let per = t.elapsed().as_secs_f64() * 1000.0 / 8.0;
        assert!(per < 16.0, "{per:.2} ms per query over 1000 names");
    }
}
