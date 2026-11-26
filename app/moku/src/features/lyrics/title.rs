//! Cleaning messy track metadata before it is looked up (pure, no I/O).
//!
//! Titles from browsers and YouTube carry noise ("(Official Music Video)", "[HD]", "| Lirik",
//! "- Topic", a leading "Artist - "), and the player's artist field is sometimes empty or a
//! channel name ("ArtistVEVO"). LRCLIB stores plain `title` / `artist`, so a lookup works
//! much better with the noise removed. Nothing here touches the network.

/// A track as it should be looked up.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Cleaned {
    /// Title without noise; brackets that say something ("(Live at Wembley)", "(Acoustic)") stay.
    pub title: String,
    /// The bare title: no brackets, nothing after " - ".
    pub core: String,
    /// The artist as given, cleaned ("X - Topic", "XVEVO", brackets removed; "Artist - Title"
    /// titles give their artist when the artist field is empty).
    pub artist: String,
    /// Guest artists that were written into the title / artist ("feat. X", "ft. X", "with X").
    pub featured: Vec<String>,
}

/// Words (any language the lyrics sites use) that only say what kind of upload it is.
const NOISE: &[&str] = &[
    "official", "officiel", "oficial", "ufficiale", "resmi", "music", "video", "videoclip", "videoclipe", "audio", "lyric", "lyrics", "lyrical", "lirik", "letra", "letras", "paroles", "testo", "songtext", "mv", "m/v",
    "pv", "hd", "hq", "uhd", "4k", "8k", "1080p", "720p", "480p", "60fps", "visualizer", "visualiser", "vevo", "topic", "explicit", "clean", "full", "song", "songs", "album", "version", "remaster", "remastered",
    "remasterizado", "remastering", "digitally", "radio", "edit", "single", "mono", "stereo", "clip", "klip", "lagu", "terjemahan", "terbaru", "lengkap", "sub", "indo", "indonesia", "subtitle", "subtitles", "subtitulado", "español",
    "english", "eng", "romanized", "romaji", "translation", "translated", "with", "and", "the", "new", "ost", "soundtrack", "hq", "audio", "only", "by", "channel", "release", "released", "premiere",
    "공식", "뮤직비디오", "뮤비", "가사", "음원", "官方", "完整版", "高清", "歌词", "歌詞", "公式", "ミュージックビデオ", "リリック", "リリックビデオ", "オフィシャル", "歌",
];

/// Words that are enough to make a phrase noise on their own (the others only help).
const STRONG: &[&str] = &[
    "official", "officiel", "oficial", "ufficiale", "resmi", "video", "videoclip", "lyric", "lyrics", "lirik", "letra", "letras", "paroles", "testo", "songtext", "mv", "m/v", "pv", "hd", "hq", "uhd", "4k", "8k", "1080p", "720p",
    "480p", "60fps", "visualizer", "visualiser", "vevo", "topic", "remaster", "remastered", "remasterizado", "remastering", "explicit", "audio", "clip", "klip", "공식", "뮤직비디오", "뮤비", "가사", "음원", "官方", "完整版", "高清",
    "歌词", "歌詞", "公式", "ミュージックビデオ", "リリック", "リリックビデオ", "オフィシャル", "terjemahan", "subtitulado", "translation", "radio", "single", "mono", "stereo", "digitally",
];

fn is_year(w: &str) -> bool {
    w.len() == 4 && w.bytes().all(|b| b.is_ascii_digit()) && (w.starts_with("19") || w.starts_with("20"))
}

/// The words of `s`, lower-cased, punctuation (and the odd symbol) cut away.
fn words_of(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || matches!(c, '&' | '+' | ',' | '-' | '–' | '—' | ':' | ';' | '/' | '.' | '|' | '·' | '・') && c != '/')
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '/').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// "(Official Music Video)", "[HD]", "Remastered 2011", "Lirik Lagu", "Radio Edit": only noise.
fn is_noise(s: &str) -> bool {
    let w = words_of(s);
    if w.is_empty() {
        return false;
    }
    let all = w.iter().all(|x| NOISE.contains(&x.as_str()) || is_year(x));
    let strong = w.iter().any(|x| STRONG.contains(&x.as_str()));
    all && strong
}

/// Emoji, pictographs and musical symbols ("🎵", "🔥", "♪"): never part of a song title.
fn is_symbol(c: char) -> bool {
    matches!(c as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF | 0xFE0F | 0x200D | 0x20E3 | 0x2122 | 0x00AE)
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Edges: spaces, dashes, bars, commas and quotes are not part of a title.
fn trim_edges(s: &str) -> String {
    let t = s.trim_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '–' | '—' | '|' | '~' | ',' | ';' | ':' | '"' | '\'' | '“' | '”' | '‘' | '’' | '「' | '」' | '『' | '』' | '«' | '»' | '/'));
    collapse(t)
}

fn closer_of(open: char) -> Option<char> {
    Some(match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '【' => '】',
        '（' => '）',
        '［' => '］',
        '「' => '」',
        '『' => '』',
        '《' => '》',
        '〈' => '〉',
        '<' => '>',
        _ => return None,
    })
}

/// A guest written as "feat. X": the name, if `s` starts like that.
fn feat_name(s: &str) -> Option<String> {
    let l = s.trim_start();
    for p in ["featuring ", "feat. ", "feat ", "ft. ", "ft ", "with ", "w/ ", "feat.", "ft."] {
        if l.len() >= p.len() && l.is_char_boundary(p.len()) && l[..p.len()].eq_ignore_ascii_case(p) {
            return Some(trim_edges(&l[p.len()..]));
        }
    }
    None
}

enum Group {
    Noise,
    Feat(String),
    Keep,
}

fn classify(inner: &str) -> Group {
    let t = inner.trim();
    if t.is_empty() || is_noise(t) {
        return Group::Noise;
    }
    match feat_name(t) {
        Some(n) if !n.is_empty() => Group::Feat(n),
        _ => Group::Keep,
    }
}

/// Remove the bracket groups that are noise or a guest ("feat. X"); the guests are returned.
/// With `all`, every bracket group goes (the bare title).
fn strip_groups(s: &str, all: bool) -> (String, Vec<String>) {
    let mut out = String::with_capacity(s.len());
    let mut open: Vec<(char, usize)> = Vec::new();
    let mut feats = Vec::new();
    for c in s.chars() {
        if closer_of(c).is_some() {
            open.push((c, out.len()));
            out.push(c);
            continue;
        }
        if let Some(&(o, at)) = open.last() {
            if closer_of(o) == Some(c) {
                open.pop();
                let inner = out[at + o.len_utf8()..].to_string();
                match if all { Group::Noise } else { classify(&inner) } {
                    Group::Noise => {
                        out.truncate(at);
                        out.push(' ');
                    }
                    Group::Feat(n) => {
                        out.truncate(at);
                        out.push(' ');
                        feats.push(n);
                    }
                    Group::Keep => out.push(c),
                }
                continue;
            }
        }
        out.push(c);
    }
    (collapse(&out), feats)
}

/// Split at the separators of upload titles: " - ", " – ", " — ", "|", "||", "//", " ~ ", " : "-less.
fn segments(s: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let spaced = |i: usize| i > 0 && i + 1 < chars.len() && chars[i - 1].is_whitespace() && chars[i + 1].is_whitespace();
        let cut = match c {
            '-' | '–' | '—' | '~' => spaced(i),
            '|' | '｜' => true,
            '/' => i + 1 < chars.len() && chars[i + 1] == '/',
            _ => false,
        };
        if cut {
            parts.push(std::mem::take(&mut cur));
            if c == '/' {
                i += 1;
            }
        } else {
            cur.push(c);
        }
        i += 1;
    }
    parts.push(cur);
    parts.into_iter().map(|p| trim_edges(&p)).filter(|p| !p.is_empty()).collect()
}

/// Comparable form of a name: lower-case letters and digits only.
pub fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Two names that mean the same artist / title (equal, or one contains the other).
pub fn similar(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && !b.is_empty() && (a == b || a.contains(&b) || b.contains(&a))
}

/// "Title feat. X" / "Title ft. X" at the end of a (bracket-less) title.
fn split_feat(s: &str) -> (String, Option<String>) {
    let lower = s.to_lowercase();
    for key in [" featuring ", " feat. ", " feat ", " ft. ", " ft "] {
        if let Some(i) = lower.find(key) {
            // `lower` and `s` can differ in byte length for exotic characters: only cut when both agree.
            if lower.len() == s.len() && s.is_char_boundary(i) {
                let name = trim_edges(&s[i + key.len()..]);
                return (trim_edges(&s[..i]), Some(name).filter(|n| !n.is_empty()));
            }
        }
    }
    (s.to_string(), None)
}

/// "ArtistVEVO", "Artist - Topic", "Artist Official", "TaylorSwiftVEVO" → the artist's name.
pub fn clean_artist(a: &str) -> String {
    let (no_groups, _) = strip_groups(&a.chars().filter(|c| !is_symbol(*c)).collect::<String>(), true);
    let mut s = trim_edges(no_groups.trim_start_matches('@'));
    let mut changed = true;
    while changed {
        changed = false;
        let l = s.to_lowercase();
        for suf in [" - topic", "- topic", " topic", "vevo", " official", "official", " music official", " official channel", " officialchannel"] {
            if l.ends_with(suf) && l.len() == s.len() && s.len() > suf.len() {
                let keep = s.len() - suf.len();
                if s.is_char_boundary(keep) {
                    s = trim_edges(&s[..keep]);
                    changed = true;
                    // "TaylorSwiftVEVO" → "Taylor Swift" (a run-together name only).
                    if suf == "vevo" || suf == "official" {
                        s = split_camel(&s);
                    }
                    break;
                }
            }
        }
    }
    s
}

/// "TaylorSwift" → "Taylor Swift"; anything with a space (or not CamelCase) stays.
fn split_camel(s: &str) -> String {
    if s.contains(' ') || !s.chars().next().is_some_and(|c| c.is_uppercase()) || !s.chars().all(|c| c.is_ascii_alphabetic()) {
        return s.to_string();
    }
    let mut out = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c.is_ascii_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = c.is_ascii_lowercase();
        out.push(c);
    }
    out
}

/// `Artist 'Title'`, `Artist "Title"`, `Artist「Title」`, `Artist『Title』`: the quoted part is
/// the title (only when there is more than the quote).
fn quoted(s: &str) -> Option<(String, String)> {
    for (o, c) in [('\'', '\''), ('"', '"'), ('“', '”'), ('‘', '’'), ('「', '」'), ('『', '』'), ('«', '»')] {
        let Some(i) = s.find(o) else { continue };
        let rest = &s[i + o.len_utf8()..];
        let Some(j) = rest.find(c) else { continue };
        let (before, title, after) = (trim_edges(&s[..i]), trim_edges(&rest[..j]), trim_edges(&rest[j + c.len_utf8()..]));
        // An apostrophe inside a word ("Don't") is not a quote: an ASCII quote must start a
        // word, there must be a name before it, and what follows it must be noise (or nothing).
        let starts_word = !o.is_ascii() || s[..i].ends_with(char::is_whitespace);
        if starts_word && !before.is_empty() && !title.is_empty() && (after.is_empty() || is_noise(&after)) {
            return Some((before, title));
        }
    }
    None
}

/// Clean a track's title and artist for a lookup.
pub fn clean(title: &str, artist: &str) -> Cleaned {
    let raw_title: String = title.chars().filter(|c| !is_symbol(*c)).collect();
    let (t, mut featured) = strip_groups(&raw_title, false);
    let mut artist_out = clean_artist(artist);
    // "A feat. B" in the artist field: the first one is the artist, the rest are guests.
    let (head, guest) = split_feat(&artist_out);
    if let Some(g) = guest {
        featured.push(g);
        artist_out = head;
    }

    // Separator-cut parts of the title; noise parts go.
    let mut parts: Vec<String> = segments(&t).into_iter().filter(|p| !is_noise(p)).collect();
    if parts.is_empty() {
        parts = segments(&t);
    }
    // "Artist 'Title' Official MV": a title in quotes behind a name.
    if parts.len() == 1 && artist_out.is_empty() {
        if let Some((a, tt)) = quoted(&parts[0]) {
            artist_out = clean_artist(&a);
            parts = vec![tt];
        }
    }
    // "Artist - Title" / "Title - Artist": the part that is the artist goes (or becomes the artist).
    if parts.len() >= 2 {
        if artist_out.is_empty() {
            artist_out = clean_artist(&parts.remove(0));
        } else if similar(&parts[0], &artist_out) && !similar(&parts[1], &artist_out) {
            parts.remove(0);
        } else if parts.len() == 2 && similar(&parts[1], &artist_out) && !similar(&parts[0], &artist_out) {
            parts.pop();
        } else if parts.len() > 2 && similar(&parts[0], &artist_out) {
            parts.remove(0);
        }
    }
    let mut title_out = parts.join(" - ");
    // "Title feat. X" (not in brackets).
    let (head, guest) = split_feat(&title_out);
    if let Some(g) = guest {
        featured.push(g);
        title_out = head;
    }
    let title_out = trim_edges(&title_out);
    let first = parts.first().map(String::as_str).unwrap_or("");
    let (first, guest) = split_feat(first);
    if let Some(g) = guest {
        if !featured.contains(&g) {
            featured.push(g);
        }
    }
    let (core, _) = strip_groups(&first, true);
    let mut core = trim_edges(&core);
    // Nothing left: keep what we were given rather than ask for an empty title.
    let mut title_out = title_out;
    if title_out.is_empty() {
        title_out = collapse(title.trim());
        core = title_out.clone();
    }
    if core.is_empty() {
        core = title_out.clone();
    }
    featured.dedup();
    Cleaned { title: title_out, core, artist: artist_out, featured }
}

/// First artist of "A, B & C" / "A feat. B" / "A x B".
pub fn first_artist(a: &str) -> String {
    let lower = a.to_lowercase();
    let mut end = a.len();
    if lower.len() == a.len() {
        for sep in [",", " & ", " feat", " ft.", " x ", ";", " / ", " and ", " dan ", " with ", " + "] {
            if let Some(i) = lower.find(sep) {
                if a.is_char_boundary(i) {
                    end = end.min(i);
                }
            }
        }
    }
    a[..end].trim().to_string()
}

/// The bare title of `t` ("Song (feat. X) [Live] - Remastered 2011" → "Song").
pub fn clean_title(t: &str) -> String {
    clean(t, "").core
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (title, artist) → (title, core, artist).
    fn c(title: &str, artist: &str) -> (String, String, String) {
        let r = clean(title, artist);
        (r.title, r.core, r.artist)
    }

    fn row(t: &str, a: &str, want_t: &str, want_a: &str) {
        let (title, _, artist) = c(t, a);
        assert_eq!((title.as_str(), artist.as_str()), (want_t, want_a), "input: {t:?} / {a:?}");
    }

    #[test]
    fn upload_noise_in_brackets_goes() {
        for (t, a, wt, wa) in [
            ("Blinding Lights (Official Music Video)", "The Weeknd", "Blinding Lights", "The Weeknd"),
            ("Shape of You [Official Audio]", "Ed Sheeran", "Shape of You", "Ed Sheeran"),
            ("Levitating (Lyrics)", "Dua Lipa", "Levitating", "Dua Lipa"),
            ("Perfect (Official Lyric Video) [HD]", "Ed Sheeran", "Perfect", "Ed Sheeran"),
            ("My Song [Official Video] [4K]", "Someone", "My Song", "Someone"),
            ("Hotel California (Remastered 2013)", "Eagles", "Hotel California", "Eagles"),
            ("The Chain (2004 Remaster)", "Fleetwood Mac", "The Chain", "Fleetwood Mac"),
            ("Song 【MV】", "Artist", "Song", "Artist"),
            ("Song (Explicit)", "Artist", "Song", "Artist"),
            ("Song (Video Klip Resmi)", "Artist", "Song", "Artist"),
            ("Song (Lirik Lagu)", "Artist", "Song", "Artist"),
        ] {
            row(t, a, wt, wa);
        }
    }

    #[test]
    fn upload_noise_after_separators_goes() {
        for (t, a, wt, wa) in [
            ("Bohemian Rhapsody - Remastered 2011", "Queen", "Bohemian Rhapsody", "Queen"),
            ("Imagine | Lirik Lagu", "John Lennon", "Imagine", "John Lennon"),
            ("Wake Me Up - Radio Edit", "Avicii", "Wake Me Up", "Avicii"),
            ("Beat It - 2012 Remaster", "Michael Jackson", "Beat It", "Michael Jackson"),
            ("Africa - Official Video", "Toto", "Africa", "Toto"),
            ("Song || Official Audio", "Artist", "Song", "Artist"),
            ("Song｜Lyrics", "Artist", "Song", "Artist"),
            ("Song // Official Music Video", "Artist", "Song", "Artist"),
        ] {
            row(t, a, wt, wa);
        }
    }

    #[test]
    fn meaningful_brackets_and_hyphens_stay() {
        row("Song (Live at Wembley)", "Queen", "Song (Live at Wembley)", "Queen");
        row("Song (Acoustic)", "Artist", "Song (Acoustic)", "Artist");
        row("Hati-Hati di Jalan (Official Music Video)", "Tulus", "Hati-Hati di Jalan", "Tulus");
        row("Anti-Hero", "Taylor Swift", "Anti-Hero", "Taylor Swift");
        // The core title drops them for the loose lookups.
        assert_eq!(c("Song (Live at Wembley)", "Queen").1, "Song");
        assert_eq!(c("Song (Acoustic) - Radio Edit", "A").1, "Song");
        assert_eq!(clean_title("Song (feat. X) [Live] - Remastered 2011"), "Song");
    }

    #[test]
    fn channel_names_become_artists() {
        row("Shape of You", "Ed Sheeran - Topic", "Shape of You", "Ed Sheeran");
        row("Rolling in the Deep", "AdeleVEVO", "Rolling in the Deep", "Adele");
        row("Uptown Funk", "MarkRonsonVEVO", "Uptown Funk", "Mark Ronson");
        row("Shake It Off", "Taylor Swift Vevo", "Shake It Off", "Taylor Swift");
        row("Song", "Artist Official", "Song", "Artist");
        row("Song", "@artist", "Song", "artist");
        row("Dynamite", "BTS (방탄소년단)", "Dynamite", "BTS");
        row("Song", "Lofi Girl - Topic", "Song", "Lofi Girl");
    }

    #[test]
    fn guests_are_moved_out_of_the_title() {
        let r = clean("Sorry (feat. Jhene Aiko)", "Justin Bieber");
        assert_eq!((r.title.as_str(), r.featured.as_slice()), ("Sorry", ["Jhene Aiko".to_string()].as_slice()));
        let r = clean("Despacito ft. Daddy Yankee", "Luis Fonsi");
        assert_eq!((r.title.as_str(), r.featured.as_slice()), ("Despacito", ["Daddy Yankee".to_string()].as_slice()));
        let r = clean("Title (with Someone Else)", "A");
        assert_eq!((r.title.as_str(), r.featured.as_slice()), ("Title", ["Someone Else".to_string()].as_slice()));
        let r = clean("Title [Feat. B & C]", "A");
        assert_eq!((r.title.as_str(), r.featured.as_slice()), ("Title", ["B & C".to_string()].as_slice()));
        let r = clean("Song", "Main feat. Guest");
        assert_eq!((r.artist.as_str(), r.featured.as_slice()), ("Main", ["Guest".to_string()].as_slice()));
        // A word that merely looks like a marker is not one.
        row("Feather in the Wind", "A", "Feather in the Wind", "A");
        row("Without You", "A", "Without You", "A");
    }

    #[test]
    fn artist_and_title_in_the_title_field() {
        row("Taylor Swift - Anti-Hero (Official Music Video)", "", "Anti-Hero", "Taylor Swift");
        row("Coldplay - Yellow", "", "Yellow", "Coldplay");
        row("Dewa 19 - Kangen (Official Video)", "", "Kangen", "Dewa 19");
        row("Sheila On 7 - Dan (Lirik)", "", "Dan", "Sheila On 7");
        row("Ariana Grande - 7 rings (Official Video)", "", "7 rings", "Ariana Grande");
        row("Dua Lipa - Levitating Featuring DaBaby (Official Music Video)", "", "Levitating", "Dua Lipa");
        // The artist field is there: "Artist - " in the title is dropped, in either order.
        row("Coldplay - Yellow", "Coldplay", "Yellow", "Coldplay");
        row("Yellow - Coldplay", "Coldplay", "Yellow", "Coldplay");
        row("Coldplay - Fix You (Official Video)", "Coldplay - Topic", "Fix You", "Coldplay");
        // A title with a real hyphen-less subtitle after the artist stays whole.
        row("Linkin Park - In the End (Official HD Music Video)", "Linkin Park", "In the End", "Linkin Park");
    }

    #[test]
    fn korean_and_japanese_titles() {
        row("BTS (방탄소년단) 'Dynamite' Official MV", "", "Dynamite", "BTS");
        row("YOASOBI「夜に駆ける」 Official Music Video", "", "夜に駆ける", "YOASOBI");
        row("米津玄師 - Lemon【MV】", "", "Lemon", "米津玄師");
        row("夜に駆ける (公式 ミュージックビデオ)", "YOASOBI", "夜に駆ける", "YOASOBI");
        row("사랑해 (뮤직비디오)", "아이유", "사랑해", "아이유");
        row("봄날 가사", "BTS", "봄날 가사", "BTS");
        row("너와 나 [공식 MV]", "아이유", "너와 나", "아이유");
        row("Lemon 歌詞", "米津玄師", "Lemon 歌詞", "米津玄師");
    }

    #[test]
    fn indonesian_titles() {
        row("Hati-Hati di Jalan (Official Lyric Video)", "Tulus", "Hati-Hati di Jalan", "Tulus");
        row("Pamungkas - To The Bone (Official Music Video)", "", "To The Bone", "Pamungkas");
        row("Mahalini - Sial | Lirik", "", "Sial", "Mahalini");
        row("Peterpan - Semua Tentang Kita (Official Video Klip)", "", "Semua Tentang Kita", "Peterpan");
        row("Dewa 19 - Kangen [Video Klip Resmi]", "Dewa 19", "Kangen", "Dewa 19");
    }

    #[test]
    fn emoji_and_whitespace_are_cleaned() {
        row("Song 🎵🔥 (HD)", "Artist 🎶", "Song", "Artist");
        row("  Spaced   Title  ", "  Art  ", "Spaced Title", "Art");
        row("♪ Little Song ♪", "Artist", "Little Song", "Artist");
        row("\"Quoted Title\"", "Artist", "Quoted Title", "Artist");
        row("Song ™", "Artist", "Song", "Artist");
    }

    #[test]
    fn nothing_left_keeps_what_was_given() {
        // A title that is all noise is not turned into an empty lookup.
        assert_eq!(c("(Official Video)", "A").0, "(Official Video)");
        assert_eq!(c("", "A").0, "");
        assert_eq!(c("   ", "").0, "");
        assert_eq!(c("Lyrics", "A").0, "Lyrics");
    }

    #[test]
    fn broken_brackets_and_odd_input_do_not_panic() {
        for t in ["Song (live", "Song ) x", "((((", "))))", "[(])", "Song - ", " - Song", "- - -", "|||", "'", "\"", "「", "ß İ ǅ", "\u{0}", "a\u{301}"] {
            let r = clean(t, t);
            assert!(r.title.len() <= t.len() + 2, "{t:?} -> {r:?}");
        }
        row("Song (live", "A", "Song (live", "A");
    }

    #[test]
    fn first_artist_and_similarity() {
        assert_eq!(first_artist("A, B & C"), "A");
        assert_eq!(first_artist("Artist feat. Other"), "Artist");
        assert_eq!(first_artist("Mark Ronson and Bruno Mars"), "Mark Ronson");
        assert_eq!(first_artist("Solo"), "Solo");
        assert!(similar("The Weeknd", "the weeknd"));
        assert!(similar("Coldplay", "Coldplay & Rihanna"));
        assert!(!similar("Coldplay", "Muse"));
        assert!(!similar("", "x"));
        assert_eq!(norm("Hati-Hati, di Jalan!"), "hatihatidijalan");
    }
}
