//! Choosing the right LRCLIB entry for a track (pure, no I/O).
//!
//! LRCLIB stores one entry per upload of a song: the same recording can be there a dozen times
//! (other albums, other lengths, "because," with a trailing comma, a 3 s entry that is not a song at
//! all) and a search by title alone returns other artists' songs that merely contain the word. So a
//! hit is never taken because it is "close": it has to **be** the wanted song.
//!
//! # The rules ([`judge`])
//!
//! * **Noise** is dropped everywhere: an entry shorter than [`MIN_DURATION_S`], or whose lyrics run
//!   more than [`SPAN_SLACK_S`] past its own length (a 4 s entry carrying a whole song).
//! * **Title**: the cleaned titles are equal ([`TitleMatch::Exact`], punctuation, case, accents and
//!   spacing ignored), or equal after dropping the bracket / " - " part ([`TitleMatch::Core`]; "Song
//!   (Live)" for "Song"). A title that only *contains* the wanted one ("Because Because Because") is
//!   not that title. Two different qualifiers ("(Live)" vs "(Remix)") are different versions.
//! * **Artist** (when the track has one, in every kind of request): equal after normalising
//!   ([`ArtistMatch::Exact`]), or one is inside the other / they share the same words / they share
//!   one of the artists of a duet ([`ArtistMatch::Alike`]). Anything else is rejected, even when the
//!   title and the length match.
//! * **Length** (when both are known): within ±[`DUR_TOLERANCE_S`] s, ±[`DUR_TOLERANCE_EXACT_S`] s when
//!   the artist and the title are exact. Without an artist the length has to vouch for the match.
//!
//! A hit that passes all of that is **strict**: Moku shows it by itself. For the "Bukan lagu ini"
//! button there is a looser second class (any synced hit by the same artist OR with the same title,
//! within ±[`DUR_TOLERANCE_MANUAL_S`] s), which Moku only shows when the user asks.
//!
//! # The order ([`rank`])
//!
//! One score per hit: title (exact 300, core 200 minus 60 for a bracket on one side) + artist (exact
//! 200, alike 140, unknown 100) + length (300 minus 50 per second off, 100 when unknown) + synced /
//! instrumental (instrumental minus 150) + 15 for a real album. Hits are sorted strict first, then by
//! score, and the lowest LRCLIB id breaks a tie (the oldest entry is the most stable choice). That is
//! the order of preference "synced over instrumental, exact length, has an album, lowest id". Exact
//! copies of the same lyrics are listed once; the list keeps [`MAX_CANDIDATES`].

use std::sync::Arc;

use serde_json::Value;

use super::lrc::{self, Lyrics};
use super::lrclib::Found;
use super::title;

/// A hit must be within this many seconds of the track (when both lengths are known).
pub const DUR_TOLERANCE_S: f64 = 3.0;
/// ... and within this many when the artist and the title are exact.
pub const DUR_TOLERANCE_EXACT_S: f64 = 5.0;
/// The looser limit of a hit the user asks for.
pub const DUR_TOLERANCE_MANUAL_S: f64 = 10.0;
/// Candidates kept for "Bukan lagu ini".
pub const MAX_CANDIDATES: usize = 6;
/// An entry shorter than this is not a song.
const MIN_DURATION_S: f64 = 20.0;
/// Lyrics that start later than the entry's length plus this are not this entry's lyrics.
const SPAN_SLACK_S: f64 = 20.0;

/// What a track is asked for (cleaned with [`title::clean`]).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Want {
    /// Seconds (0 = unknown).
    pub duration_s: u64,
    /// The title without upload noise; brackets that say something ("(Live)") stay.
    pub title: String,
    /// The bare title: no brackets, nothing after " - ".
    pub core: String,
    /// The artist, cleaned ("X - Topic", "XVEVO" gone, guests moved out); empty = unknown.
    pub artist: String,
}

impl Want {
    pub fn new(title: &str, artist: &str, duration_s: u64) -> Want {
        let c = title::clean(title, artist);
        Want { duration_s, title: c.title, core: c.core, artist: c.artist }
    }
}

/// One LRCLIB entry that has synced lyrics (or is an instrumental).
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub id: u64,
    pub track: String,
    pub artist: String,
    pub album: String,
    /// Seconds (0 = unknown).
    pub duration_s: f64,
    /// `Synced` or `Instrumental`, never `Missing`.
    pub found: Found,
    /// Start of the last lyric line, ms (0 for an instrumental).
    pub span_ms: u64,
}

/// The entry of a `/api/search` item or a `/api/get` object. `None` when it has no synced lyrics
/// and is not an instrumental (plain-text lyrics cannot follow the music).
pub fn hit_of(v: &Value) -> Option<Hit> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let (found, span_ms) = if v.get("instrumental").and_then(Value::as_bool) == Some(true) {
        (Found::Instrumental, 0)
    } else {
        let l: Lyrics = lrc::parse(v.get("syncedLyrics").and_then(Value::as_str)?);
        if l.is_empty() {
            return None;
        }
        let span = l.lines.last().map(|x| x.t).unwrap_or(0);
        (Found::Synced(Arc::new(l)), span)
    };
    let track = Some(s("trackName")).filter(|t| !t.is_empty()).unwrap_or_else(|| s("name"));
    Some(Hit { id: v.get("id").and_then(Value::as_u64).unwrap_or(0), track, artist: s("artistName"), album: s("albumName"), duration_s: v.get("duration").and_then(Value::as_f64).unwrap_or(0.0), found, span_ms })
}

/// A hit that was kept, with how well it fits.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub hit: Hit,
    /// Passed every rule: Moku may show it without being asked.
    pub strict: bool,
    pub score: i32,
}

impl Candidate {
    pub fn id(&self) -> u64 {
        self.hit.id
    }

    /// A hit the user picked (or remembered): it needs no judging.
    pub fn chosen(hit: Hit) -> Candidate {
        Candidate { hit, strict: true, score: 0 }
    }
}

// ---- names --------------------------------------------------------------------------------------

/// Lower-case letter without its accent ("é" → "e"); `None` for a letter that has none or is not one
/// of the Latin ones handled here. Transliteration (Cyrillic, Korean ...) is out of scope.
fn unaccent(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'ç' | 'ć' | 'č' | 'ĉ' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'ğ' | 'ĝ' => "g",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => "i",
        'ł' | 'ľ' | 'ĺ' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'ŕ' | 'ř' => "r",
        'ś' | 'š' | 'ş' | 'ș' => "s",
        'ť' | 'ţ' | 'ț' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => "u",
        'ý' | 'ÿ' => "y",
        'ž' | 'ź' | 'ż' => "z",
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        _ => return None,
    })
}

/// The words of `s`: lower-case, accents gone, letters and digits only (anything else separates).
fn words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            match unaccent(c) {
                Some(a) => cur.push_str(a),
                None => cur.push(c),
            }
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Comparable form: the words run together ("Taylor Swift" and "TaylorSwift" are the same).
pub fn squash(s: &str) -> String {
    words(s).concat()
}

/// Words that join names without being part of one.
const JOINERS: &[&str] = &["and", "dan", "feat", "ft", "featuring", "with", "vs", "the", "x"];

/// "A & B feat. C" → ["a", "b", "c"] (each as written, lower-case).
fn pieces(artist: &str) -> Vec<String> {
    let spaced: String = artist.to_lowercase().chars().flat_map(|c| if matches!(c, ',' | ';' | '&' | '+' | '/') { vec![' ', '|', ' '] } else { vec![c] }).collect();
    let ws: Vec<&str> = spaced.split_whitespace().collect();
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for (i, w) in ws.iter().enumerate() {
        let w = w.trim_matches('.');
        let joiner = w == "|" || (JOINERS.contains(&w) && w != "the" && !(w == "x" && (i == 0 || i + 1 == ws.len())));
        if joiner {
            if !cur.is_empty() {
                out.push(cur.join(" "));
                cur.clear();
            }
        } else {
            cur.push(w);
        }
    }
    if !cur.is_empty() {
        out.push(cur.join(" "));
    }
    out.into_iter().filter(|p| !squash(p).is_empty()).collect()
}

/// How the artist of a hit relates to the wanted one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtistMatch {
    /// The same name after normalising.
    Exact,
    /// One is inside the other, the same words, or a shared artist of a duet.
    Alike,
    /// The track has no artist: nothing to compare.
    Unknown,
    No,
}

/// Is `a` a run of consecutive words of `b` (or the other way round)?
fn inside(a: &[String], b: &[String]) -> bool {
    let (s, l) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    !s.is_empty() && l.windows(s.len()).any(|w| w == s)
}

/// Compare the wanted artist (as [`Want::artist`]) with a hit's.
pub fn artist_match(want: &str, hit: &str) -> ArtistMatch {
    let w = title::clean_artist(want);
    if squash(&w).is_empty() {
        return ArtistMatch::Unknown;
    }
    let h = title::clean_artist(hit);
    let (ws, hs) = (squash(&w), squash(&h));
    if hs.is_empty() {
        return ArtistMatch::No;
    }
    // "Simon & Garfunkel" and "Simon and Garfunkel", "The Weeknd" and "Weeknd": the same name.
    let bare = |s: &str| -> String { words(s).into_iter().filter(|x| !JOINERS.contains(&x.as_str())).collect() };
    if ws == hs || (!bare(&w).is_empty() && bare(&w) == bare(&h)) {
        return ArtistMatch::Exact;
    }
    let (wt, ht): (Vec<String>, Vec<String>) = (words(&w).into_iter().filter(|x| !JOINERS.contains(&x.as_str())).collect(), words(&h).into_iter().filter(|x| !JOINERS.contains(&x.as_str())).collect());
    if inside(&wt, &ht) {
        return ArtistMatch::Alike;
    }
    // The same words in another order ("Hi Lee" / "Lee Hi"): token-set similarity.
    let common = wt.iter().filter(|x| ht.contains(x)).count();
    let union = wt.len() + ht.len() - common;
    if union > 0 && common as f32 / union as f32 >= 0.8 {
        return ArtistMatch::Alike;
    }
    // A duet shares one of its artists with the hit.
    let hp: Vec<String> = pieces(&h).iter().map(|p| squash(p)).collect();
    if pieces(&w).iter().any(|p| {
        let p = squash(p);
        p.chars().count() >= 2 && hp.contains(&p)
    }) {
        return ArtistMatch::Alike;
    }
    ArtistMatch::No
}

/// How the title of a hit relates to the wanted one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleMatch {
    /// The same title after cleaning and normalising.
    Exact,
    /// The same bare title; one side has a bracket / " - " part the other lacks.
    Core,
}

/// Compare the wanted title with a hit's (`hit_artist` helps to tell "Artist - Title" from "Title - Version").
pub fn title_match(want: &Want, hit_title: &str, hit_artist: &str) -> Option<TitleMatch> {
    let c = title::clean(hit_title, hit_artist);
    let (wf, wc, hf, hc) = (squash(&want.title), squash(&want.core), squash(&c.title), squash(&c.core));
    if wf.is_empty() || hf.is_empty() {
        return None;
    }
    if wf == hf {
        return Some(TitleMatch::Exact);
    }
    // Equal bare titles, and only one side says more ("(Live)", "[Remastered]"): the same song.
    // Two different qualifiers are two versions.
    if !wc.is_empty() && wc == hc && (wf == wc || hf == hc) {
        return Some(TitleMatch::Core);
    }
    None
}

/// "", "null", "<Unknown>" ...: the album field of an entry that has none.
pub fn has_album(album: &str) -> bool {
    let a = album.trim().to_lowercase();
    !(a.is_empty() || matches!(a.as_str(), "null" | "none" | "unknown" | "<unknown>" | "undefined" | "n/a" | "-"))
}

// ---- judging and ranking --------------------------------------------------------------------------

/// Verdict on one hit: dropped (`None`), a loose candidate, or a strict one.
fn judge(h: &Hit, w: &Want) -> Option<(bool, i32)> {
    // Noise: not a song, or lyrics that cannot be this entry's.
    if h.duration_s > 0.0 && h.duration_s < MIN_DURATION_S {
        return None;
    }
    if h.duration_s > 0.0 && h.span_ms as f64 / 1000.0 > h.duration_s + SPAN_SLACK_S {
        return None;
    }
    let t = title_match(w, &h.track, &h.artist);
    let a = artist_match(&w.artist, &h.artist);
    let known = w.duration_s > 0 && h.duration_s > 0.0;
    let diff = if known { (h.duration_s - w.duration_s as f64).abs() } else { 0.0 };
    // Loose gate: the same title or the same artist, and not far off in length.
    if t.is_none() && !matches!(a, ArtistMatch::Exact | ArtistMatch::Alike) {
        return None;
    }
    if known && diff > DUR_TOLERANCE_MANUAL_S {
        return None;
    }
    let exact_pair = t == Some(TitleMatch::Exact) && a == ArtistMatch::Exact;
    let tol = if exact_pair { DUR_TOLERANCE_EXACT_S } else { DUR_TOLERANCE_S };
    let dur_ok = !known || diff <= tol;
    let artist_ok = match a {
        ArtistMatch::Exact | ArtistMatch::Alike => true,
        // No artist to compare: only a length that matches can vouch for the hit.
        ArtistMatch::Unknown => known && dur_ok,
        ArtistMatch::No => false,
    };
    let strict = t.is_some() && artist_ok && dur_ok;
    // A loose candidate must be something to sing along to.
    if !strict && h.found == Found::Instrumental {
        return None;
    }
    let mut score = 0i32;
    score += match t {
        Some(TitleMatch::Exact) => 300,
        Some(TitleMatch::Core) => 140,
        None => 0,
    };
    score += match a {
        ArtistMatch::Exact => 200,
        ArtistMatch::Alike => 140,
        ArtistMatch::Unknown => 100,
        ArtistMatch::No => 0,
    };
    score += if known { 300 - 50 * diff.round() as i32 } else { 100 };
    if h.found == Found::Instrumental {
        score -= 150;
    }
    if has_album(&h.album) {
        score += 15;
    }
    Some((strict, score))
}

/// Identity of a hit's lyrics, to list exact copies once.
fn lyrics_key(f: &Found) -> String {
    match f {
        Found::Synced(l) => l.lines.iter().map(|x| format!("{}:{}", x.t, x.text)).collect::<Vec<_>>().join("\n"),
        _ => "instrumental".into(),
    }
}

/// The hits that fit `want`, best first: strict ones, then loose ones, each by score and then by the
/// lowest id; exact copies of the same lyrics once; at most [`MAX_CANDIDATES`].
pub fn rank(hits: &[Hit], want: &Want) -> Vec<Candidate> {
    let mut v: Vec<Candidate> = hits.iter().filter_map(|h| judge(h, want).map(|(strict, score)| Candidate { hit: h.clone(), strict, score })).collect();
    v.sort_by(|a, b| b.strict.cmp(&a.strict).then(b.score.cmp(&a.score)).then(a.hit.id.cmp(&b.hit.id)));
    let mut seen: Vec<String> = Vec::new();
    v.retain(|c| {
        let k = lyrics_key(&c.hit.found);
        if seen.contains(&k) {
            return false;
        }
        seen.push(k);
        true
    });
    v.truncate(MAX_CANDIDATES);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: u64, track: &str, artist: &str, album: &str, dur: f64) -> Hit {
        let l = lrc::parse(&format!("[00:01.00]line {id}\n[00:30.00]more"));
        Hit { id, track: track.into(), artist: artist.into(), album: album.into(), duration_s: dur, found: Found::Synced(Arc::new(l)), span_ms: 30_000 }
    }

    #[test]
    fn names_are_compared_without_case_accents_spaces_and_punctuation() {
        assert_eq!(squash("Beyoncé"), "beyonce");
        assert_eq!(squash("because,"), "because");
        assert_eq!(squash("Taylor Swift"), squash("TaylorSwift"));
        assert_eq!(squash("Don’t Start Now"), squash("Don't Start Now"));
        assert_eq!(squash("Hati‐Hati di Jalan"), squash("Hati-Hati di Jalan"));
        assert_eq!(squash("Mötley Crüe"), "motleycrue");
        assert_eq!(squash("!!!"), "");
        assert_eq!(pieces("A & B feat. C"), ["a", "b", "c"]);
        assert_eq!(pieces("Simon and Garfunkel"), ["simon", "garfunkel"]);
        assert_eq!(pieces("Lil Nas X"), ["lil nas x"], "a trailing X is part of the name");
        assert_eq!(pieces("A x B"), ["a", "b"]);
    }

    #[test]
    fn artists_must_be_the_same_artist() {
        use ArtistMatch::*;
        for (want, hit, expect) in [
            ("Crayon Case", "Crayon Case", Exact),
            ("crayon case", "CRAYON CASE", Exact),
            ("Taylor Swift", "TaylorSwiftVEVO", Exact),
            ("Taylor Swift", "Taylor Swift - Topic", Exact),
            ("Beyoncé", "Beyonce", Exact),
            ("Simon & Garfunkel", "Simon and Garfunkel", Exact),
            ("The Weeknd", "Weeknd", Exact),
            ("Dua Lipa", "Dua Lipa - Topic", Exact),
            ("Dua Lipa", "Dua Lipa feat. DaBaby", Alike),
            ("Dua Lipa", "Dua Lipa, DaBaby", Alike),
            ("Binker and Moses", "Binker & Moses feat. Max Luthert", Alike),
            ("Lee Hi", "Hi Lee", Alike),
            ("Mark Ronson & Bruno Mars", "Mark Ronson", Alike),
            ("", "Anyone", Unknown),
            ("Coldplay", "Kidz Bop Kids", No),
            ("Leonard Cohen", "Jeff Buckley", No),
            ("Mika", "Mikaela", No),
            ("Tulus", "Tulusan", No),
            ("Adele", "Adele Bowie Quartet Orchestra", Alike),
            ("Crayon Case", "", No),
            ("Crayon Case", "Case", Alike),
        ] {
            assert_eq!(artist_match(want, hit), expect, "{want:?} vs {hit:?}");
        }
    }

    #[test]
    fn titles_are_equal_or_equal_without_a_bracket_part_and_never_merely_containing() {
        use TitleMatch::*;
        let w = |t: &str, a: &str| Want::new(t, a, 0);
        for (want, hit, expect) in [
            ("because", "because,", Some(Exact)),
            ("Because", "because", Some(Exact)),
            ("Don't Start Now", "Don’t Start Now", Some(Exact)),
            ("Levitating (feat. DaBaby)", "Levitating", Some(Exact)),
            ("Levitating", "Levitating (feat. DaBaby)", Some(Exact)),
            ("Hati-Hati di Jalan [Official Lyric Video]", "Hati-Hati di Jalan", Some(Exact)),
            ("Song", "Song (Live)", Some(Core)),
            ("Song (Acoustic)", "Song", Some(Core)),
            ("Don't Start Now", "Don't Start Now [Don't Start Now, 2019]", Some(Core)),
            ("Song (Live)", "Song (Acoustic)", None),
            ("Don't Start Now (Purple Disco Machine Remix)", "Don't Start Now - Purple Disco Machine Remix", Some(Exact)),
            ("because", "Because Because Because", None),
            ("because", "Because Because", None),
            ("because", "Because of Because", None),
            ("Because", "Because I Love You", None),
            ("Hallelujah", "Hallelujah, Hallelujah", None),
            ("Yellow", "Yellow Yellow Happy", None),
            ("Yellow", "Mellow Yellow", None),
            ("Imagine", "Imagine Dragons", None),
        ] {
            assert_eq!(title_match(&w(want, "X"), hit, "Someone"), expect, "{want:?} vs {hit:?}");
        }
        // "Artist - Title" uploads: the artist part is not part of the title.
        assert_eq!(title_match(&w("because", "Crayon Case"), "Crayon Case - because", "Crayon Case"), Some(Exact));
    }

    #[test]
    fn a_hit_is_strict_only_when_artist_title_and_length_all_fit() {
        let w = Want::new("because", "Crayon Case", 232);
        let ok = |h: Hit| judge(&h, &w).map(|(s, _)| s);
        assert_eq!(ok(hit(1, "because,", "Crayon Case", "x", 232.0)), Some(true));
        assert_eq!(ok(hit(1, "because,", "Crayon Case", "x", 235.0)), Some(true), "3 s off");
        assert_eq!(ok(hit(1, "because,", "Crayon Case", "x", 237.0)), Some(true), "5 s off is fine when artist and title are exact");
        assert_eq!(ok(hit(1, "because,", "Crayon Case", "x", 238.0)), Some(false), "6 s off: only for the user to pick");
        assert_eq!(ok(hit(1, "because,", "Crayon Case", "x", 243.0)), None, "11 s off: never");
        // Same title and length, another artist: not this song.
        assert_eq!(ok(hit(2, "Because", "The Beatles", "Abbey Road", 232.0)), Some(false));
        // A longer title is not the title (and the artist is wrong too).
        assert_eq!(ok(hit(3, "Because Because Because", "Mercury Rev", "x", 232.0)), None);
        // Another title by the same artist is a loose candidate at most.
        assert_eq!(ok(hit(4, "Other Song", "Crayon Case", "x", 232.0)), Some(false));
        // Noise: a 3 s entry, and a 4 s entry with a whole song of lyrics.
        assert_eq!(ok(hit(5, "because,", "Crayon Case", "x", 3.0)), None);
        let mut long = hit(6, "because,", "Crayon Case", "x", 80.0);
        long.span_ms = 226_970;
        assert_eq!(ok(long), None);
        // Without a length on either side the artist and the title decide.
        let w0 = Want::new("because", "Crayon Case", 0);
        assert_eq!(judge(&hit(7, "because,", "Crayon Case", "x", 233.0), &w0).map(|x| x.0), Some(true));
        assert_eq!(judge(&hit(8, "because,", "Crayon Case", "x", 0.0), &w).map(|x| x.0), Some(true));
        // No artist: the length has to vouch for it.
        let na = Want::new("because", "", 232);
        assert_eq!(judge(&hit(9, "because,", "Anyone", "x", 232.0), &na).map(|x| x.0), Some(true));
        assert_eq!(judge(&hit(9, "because,", "Anyone", "x", 238.0), &na).map(|x| x.0), Some(false));
        let nn = Want::new("because", "", 0);
        assert_eq!(judge(&hit(9, "because,", "Anyone", "x", 232.0), &nn).map(|x| x.0), Some(false), "nothing to go by");
    }

    #[test]
    fn instrumentals_are_only_ever_strict() {
        let w = Want::new("Song", "A", 100);
        let mut h = hit(1, "Song", "A", "x", 100.0);
        h.found = Found::Instrumental;
        h.span_ms = 0;
        assert_eq!(judge(&h, &w).map(|x| x.0), Some(true));
        let mut other = hit(2, "Other", "A", "x", 100.0);
        other.found = Found::Instrumental;
        assert_eq!(judge(&other, &w), None);
        // Synced beats an instrumental of the same song and length.
        let r = rank(&[h.clone(), hit(3, "Song", "A", "x", 100.0)], &w);
        assert_eq!(r.iter().map(Candidate::id).collect::<Vec<_>>(), [3, 1]);
    }

    #[test]
    fn ranking_prefers_exact_length_then_an_album_then_the_lowest_id() {
        let w = Want::new("S", "A", 227);
        let hits = [
            hit(50, "S", "A", "Album", 229.0),
            hit(40, "S", "A", "Album", 227.0),
            hit(30, "S", "A", "null", 227.0),
            hit(20, "S", "A", "Album", 227.0),
            hit(10, "S", "A", "Album", 230.0),
        ];
        // 20 and 40 tie (exact length, album): the lower id first; 30 has no album; then 1-2 s off.
        let ids: Vec<u64> = rank(&hits, &w).iter().map(Candidate::id).collect();
        assert_eq!(ids, [20, 40, 30, 50, 10]);
        // Unknown length: the album, then the id.
        let w0 = Want::new("S", "A", 0);
        let ids: Vec<u64> = rank(&hits, &w0).iter().map(Candidate::id).collect();
        assert_eq!(ids, [10, 20, 40, 50, 30]);
    }

    #[test]
    fn strict_candidates_come_before_loose_ones_whatever_their_score() {
        let w = Want::new("Song", "A", 200);
        let r = rank(&[hit(1, "Song", "Other Artist", "x", 200.0), hit(2, "Song", "A", "x", 204.0), hit(3, "Different", "A", "x", 200.0)], &w);
        assert_eq!(r.iter().map(|c| (c.id(), c.strict)).collect::<Vec<_>>(), [(2, true), (1, false), (3, false)]);
    }

    #[test]
    fn copies_of_the_same_lyrics_are_listed_once_and_the_list_is_short() {
        let w = Want::new("Song", "A", 0);
        let mut hits: Vec<Hit> = (1..=20).map(|i| hit(i, "Song", "A", "x", 200.0)).collect();
        // All 20 have "line <id>" in their lyrics: all different. Make 2 and 3 copies of 1.
        let one = hits[0].found.clone();
        hits[1].found = one.clone();
        hits[2].found = one;
        let r = rank(&hits, &w);
        assert_eq!(r.len(), MAX_CANDIDATES);
        assert_eq!(r.iter().map(Candidate::id).collect::<Vec<_>>(), [1, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn hits_from_json() {
        let v: Value = serde_json::from_str(r#"{"id":7,"trackName":"because,","artistName":"Crayon Case","albumName":"x","duration":232.0,"instrumental":false,"plainLyrics":"p","syncedLyrics":"[00:44.27]a\n[03:46.97]b"}"#).unwrap();
        let h = hit_of(&v).unwrap();
        assert_eq!((h.id, h.track.as_str(), h.artist.as_str(), h.span_ms), (7, "because,", "Crayon Case", 226_970));
        // No synced lyrics, and no instrumental flag: not a hit.
        let v: Value = serde_json::from_str(r#"{"id":8,"trackName":"x","duration":1,"plainLyrics":"p","syncedLyrics":null}"#).unwrap();
        assert!(hit_of(&v).is_none());
        let v: Value = serde_json::from_str(r#"{"id":9,"name":"x","artistName":"y","instrumental":true,"syncedLyrics":null}"#).unwrap();
        assert_eq!(hit_of(&v).map(|h| (h.found, h.track)), Some((Found::Instrumental, "x".into())));
    }

    #[test]
    fn what_counts_as_an_album() {
        for a in ["", "null", "<Unknown>", "Unknown", " "] {
            assert!(!has_album(a), "{a:?}");
        }
        assert!(has_album("because,") && has_album("Catch Twenty-three!!"));
    }
}
