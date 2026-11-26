//! LRCLIB client: `GET /api/get` (exact match), then up to three `/api/search` requests with the
//! title and artist cleaned of upload noise ([`title`]): title + artist, title alone, free text.
//! At most [`MAX_REQUESTS`] per track, always to the same host with the same data (artist,
//! title, album, length). The fetch is blocking (lyrics service thread only); everything but
//! the network call is pure and tested.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::lrc::{self, Lyrics};
use super::title;
use crate::net;

const BASE: &str = "https://lrclib.net/api";
const TIMEOUT: Duration = Duration::from_secs(10);
/// A search hit must be within this many seconds of the track (when the duration is known).
const DUR_TOLERANCE_S: f64 = 3.0;

/// Outcome for one track.
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    Synced(Arc<Lyrics>),
    Instrumental,
    /// No synced lyrics for this track.
    Missing,
}

/// What to look up.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub key: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Seconds (0 = unknown).
    pub duration_s: u64,
}

pub fn get_url(q: &Query) -> String {
    format!(
        "{BASE}/get?artist_name={}&track_name={}&album_name={}&duration={}",
        net::encode(&q.artist),
        net::encode(&q.title),
        net::encode(&q.album),
        q.duration_s
    )
}

/// `/api/search` by title and artist (the artist is left out when unknown).
pub fn search_url(title: &str, artist: &str) -> String {
    if artist.is_empty() {
        return search_title_url(title);
    }
    format!("{BASE}/search?track_name={}&artist_name={}", net::encode(title), net::encode(artist))
}

/// `/api/search` by title only.
pub fn search_title_url(title: &str) -> String {
    format!("{BASE}/search?track_name={}", net::encode(title))
}

/// `/api/search` with a free-text query ("artist title").
pub fn search_q_url(q: &str) -> String {
    format!("{BASE}/search?q={}", net::encode(q))
}

fn synced(v: &Value) -> Option<Found> {
    if v.get("instrumental").and_then(Value::as_bool) == Some(true) {
        return Some(Found::Instrumental);
    }
    let text = v.get("syncedLyrics").and_then(Value::as_str)?;
    let l = lrc::parse(text);
    if l.is_empty() { None } else { Some(Found::Synced(Arc::new(l))) }
}

/// Result of `/api/get` (an object): `Some` if it has synced lyrics or is instrumental.
pub fn from_get(v: &Value) -> Option<Found> {
    synced(v)
}

/// What a search result has to be like.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Want {
    /// Seconds (0 = unknown).
    pub duration_s: u64,
    /// The bare title and the first artist, cleaned ([`title::clean`]); empty = unknown.
    pub title: String,
    pub artist: String,
    /// The search was by title only / free text: its hits must be the right song by name (and by
    /// artist when the length cannot tell).
    pub loose: bool,
}

/// Best hit of `/api/search` (an array): synced lyrics with the closest duration.
pub fn pick_search(v: &Value, duration_s: u64) -> Option<Found> {
    pick_best(v, &Want { duration_s, ..Want::default() })
}

/// Best hit of `/api/search` for `want`: within ±[`DUR_TOLERANCE_S`] of the duration (when both
/// are known), the closest wins; the right artist and title break ties.
pub fn pick_best(v: &Value, want: &Want) -> Option<Found> {
    let items = v.as_array()?;
    let mut best: Option<(f64, Found)> = None;
    for it in items {
        let Some(found) = synced(it) else { continue };
        let d = it.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
        let known = want.duration_s > 0 && d > 0.0;
        let diff = if known { (d - want.duration_s as f64).abs() } else { 0.0 };
        if known && diff > DUR_TOLERANCE_S {
            continue;
        }
        let name = it.get("trackName").and_then(Value::as_str).unwrap_or("");
        let by = it.get("artistName").and_then(Value::as_str).unwrap_or("");
        // The title is the title: the hit's bare title is the one asked for (or contains it).
        let title_ok = want.title.is_empty() || name.is_empty() || title::similar(&title::clean_title(name), &want.title);
        let artist_ok = want.artist.is_empty() || by.is_empty() || title::similar(by, &want.artist);
        if want.loose && !title_ok {
            continue;
        }
        // Without a length to go by, a loose search must at least be by the right artist.
        if want.loose && !known && !artist_ok {
            continue;
        }
        // Prefer synced over instrumental on ties.
        let score = diff + if found == Found::Instrumental { 0.5 } else { 0.0 } + if title_ok { 0.0 } else { 3.0 } + if artist_ok { 0.0 } else { 1.5 };
        if best.as_ref().is_none_or(|(s, _)| score < *s) {
            best = Some((score, found));
        }
    }
    best.map(|(_, f)| f)
}

/// One request of a lookup.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// `/api/get`: the exact track (needs album and length).
    Get(String),
    /// `/api/search`; `loose`: by title only / free text.
    Search { url: String, loose: bool },
}

/// The most requests one track may cost.
pub const MAX_REQUESTS: usize = 4;

/// The requests for a track, in the order they are tried until one finds lyrics: the exact
/// `get` (when album and length are known), then the cleaned title with the cleaned artist, then
/// the bare title alone, then `q=<artist title>`. Repeats are left out; at most [`MAX_REQUESTS`].
pub fn plan(q: &Query) -> Vec<Step> {
    let c = title::clean(&q.title, &q.artist);
    let artist = title::first_artist(&c.artist);
    let mut steps: Vec<Step> = Vec::with_capacity(MAX_REQUESTS);
    if !q.album.is_empty() && q.duration_s > 0 {
        steps.push(Step::Get(get_url(q)));
    }
    let free = if artist.is_empty() { c.core.clone() } else { format!("{artist} {}", c.core) };
    for (url, loose) in [(search_url(&c.title, &artist), false), (search_title_url(&c.core), true), (search_q_url(&free), true)] {
        if !c.core.is_empty() && !steps.iter().any(|s| matches!(s, Step::Search { url: u, .. } if *u == url)) {
            steps.push(Step::Search { url, loose });
        }
    }
    steps.truncate(MAX_REQUESTS);
    steps
}

fn fetch_json(url: &str) -> Result<Option<Value>, String> {
    let (status, body) = net::get(url, TIMEOUT)?;
    match status {
        200..=299 => serde_json::from_slice(&body).map(Some).map_err(|e| format!("bad json: {e}")),
        404 => Ok(None),
        s => Err(format!("HTTP {s}")),
    }
}

/// Look the track up. `Err` = network trouble (don't cache; try again next time).
pub fn fetch(q: &Query) -> Result<Found, String> {
    let c = title::clean(&q.title, &q.artist);
    let want = Want { duration_s: q.duration_s, title: c.core.clone(), artist: title::first_artist(&c.artist), loose: false };
    for step in plan(q) {
        match step {
            Step::Get(url) => {
                if let Some(f) = fetch_json(&url)?.as_ref().and_then(from_get) {
                    return Ok(f);
                }
            }
            Step::Search { url, loose } => {
                if let Some(f) = fetch_json(&url)?.and_then(|v| pick_best(&v, &Want { loose, ..want.clone() })) {
                    return Ok(f);
                }
            }
        }
    }
    Ok(Found::Missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrclib_get_json() {
        let v: Value = serde_json::from_str(r#"{"id":1,"trackName":"x","duration":227,"instrumental":false,"plainLyrics":"a","syncedLyrics":"[00:01.00]a\n[00:02.00]b"}"#).unwrap();
        let Some(Found::Synced(l)) = from_get(&v) else { panic!() };
        assert_eq!(l.lines.len(), 2);
        let v: Value = serde_json::from_str(r#"{"instrumental":true,"syncedLyrics":null}"#).unwrap();
        assert_eq!(from_get(&v), Some(Found::Instrumental));
        let v: Value = serde_json::from_str(r#"{"instrumental":false,"plainLyrics":"only plain","syncedLyrics":null}"#).unwrap();
        assert_eq!(from_get(&v), None);
    }

    #[test]
    fn lrclib_search_picks_closest_duration() {
        let v: Value = serde_json::from_str(
            r#"[{"duration":300,"syncedLyrics":"[00:01.00]far"},
                {"duration":226.5,"syncedLyrics":"[00:01.00]close"},
                {"duration":227,"syncedLyrics":null,"plainLyrics":"plain"},
                {"duration":229,"syncedLyrics":"[00:01.00]ok"}]"#,
        )
        .unwrap();
        let Some(Found::Synced(l)) = pick_search(&v, 227) else { panic!() };
        assert_eq!(l.lines[0].text, "close");
        assert_eq!(pick_search(&v, 100), None, "nothing within tolerance");
        assert!(pick_search(&serde_json::json!([]), 0).is_none());
    }

    #[test]
    fn urls_and_cleanup() {
        let q = Query { key: String::new(), title: "Midnight Drive".into(), artist: "Kaia & Co".into(), album: "Neon Atlas".into(), duration_s: 227 };
        assert_eq!(get_url(&q), "https://lrclib.net/api/get?artist_name=Kaia%20%26%20Co&track_name=Midnight%20Drive&album_name=Neon%20Atlas&duration=227");
        assert!(get_url(&q).starts_with("https://lrclib.net/"));
        assert_eq!(title::clean_title("Song (feat. X) [Live] - Remastered 2011"), "Song");
        assert_eq!(title::first_artist("A, B & C"), "A");
        assert_eq!(title::first_artist("Artist feat. Other"), "Artist");
        assert_eq!(search_url("Song", ""), "https://lrclib.net/api/search?track_name=Song");
        assert_eq!(search_url("Song", "A B"), "https://lrclib.net/api/search?track_name=Song&artist_name=A%20B");
        assert_eq!(search_q_url("A Song"), "https://lrclib.net/api/search?q=A%20Song");
    }

    fn q(title: &str, artist: &str, album: &str, dur: u64) -> Query {
        Query { key: String::new(), title: title.into(), artist: artist.into(), album: album.into(), duration_s: dur }
    }

    fn urls(steps: &[Step]) -> Vec<String> {
        steps.iter().map(|s| match s { Step::Get(u) | Step::Search { url: u, .. } => u.clone() }).collect()
    }

    #[test]
    fn plan_tries_get_then_cleaned_title_and_artist_then_title_then_free_text() {
        let steps = plan(&q("Blinding Lights (Official Music Video)", "The Weeknd - Topic", "After Hours", 200));
        assert_eq!(steps.len(), 4);
        assert!(matches!(&steps[0], Step::Get(u) if u.contains("album_name=After%20Hours") && u.ends_with("duration=200")));
        assert_eq!(
            urls(&steps)[1..],
            [
                "https://lrclib.net/api/search?track_name=Blinding%20Lights&artist_name=The%20Weeknd",
                "https://lrclib.net/api/search?track_name=Blinding%20Lights",
                "https://lrclib.net/api/search?q=The%20Weeknd%20Blinding%20Lights",
            ]
        );
        assert!(matches!(&steps[1], Step::Search { loose: false, .. }));
        assert!(matches!(&steps[2], Step::Search { loose: true, .. }) && matches!(&steps[3], Step::Search { loose: true, .. }));
    }

    #[test]
    fn plan_without_album_or_length_skips_get_and_never_repeats_a_request() {
        let steps = plan(&q("Yellow", "Coldplay", "", 0));
        assert!(steps.iter().all(|s| matches!(s, Step::Search { .. })));
        assert_eq!(steps.len(), 3);
        // No artist: "title + artist" and "title alone" are the same request, asked once.
        let steps = plan(&q("Coldplay - Yellow (Lyrics)", "", "", 0));
        let u = urls(&steps);
        assert_eq!(u[0], "https://lrclib.net/api/search?track_name=Yellow&artist_name=Coldplay");
        let steps = plan(&q("Yellow", "", "", 0));
        let u = urls(&steps);
        assert_eq!(u, ["https://lrclib.net/api/search?track_name=Yellow", "https://lrclib.net/api/search?q=Yellow"]);
        // Nothing to search for: nothing is asked.
        assert!(plan(&q("", "", "", 0)).is_empty());
    }

    #[test]
    fn plan_stays_within_the_request_budget_for_any_title() {
        let long = "long ".repeat(200);
        for t in ["a", "Artist - Title (feat. X) [HD]", "((((", "日本語のタイトル", long.as_str()] {
            for (album, dur) in [("", 0u64), ("Album", 180)] {
                let steps = plan(&q(t, "Some Artist", album, dur));
                assert!(steps.len() <= MAX_REQUESTS, "{t:?}: {}", steps.len());
                let u = urls(&steps);
                let mut sorted = u.clone();
                sorted.sort();
                sorted.dedup();
                assert_eq!(sorted.len(), u.len(), "{t:?}: a repeated request");
                assert!(u.iter().all(|x| x.starts_with("https://lrclib.net/api/")), "same host only");
            }
        }
    }

    #[test]
    fn search_hits_must_be_the_right_song_when_the_search_is_loose() {
        let v: Value = serde_json::from_str(
            r#"[{"trackName":"Home","artistName":"Someone Else","duration":200,"syncedLyrics":"[00:01.00]wrong"},
                {"trackName":"Home (Live)","artistName":"Edward Sharpe","duration":201,"syncedLyrics":"[00:01.00]live"},
                {"trackName":"Different Song","artistName":"Edward Sharpe","duration":200,"syncedLyrics":"[00:01.00]other"}]"#,
        )
        .unwrap();
        let loose = Want { duration_s: 200, title: "Home".into(), artist: "Edward Sharpe".into(), loose: true };
        // The right artist beats a hit that is only the same title; a different title never wins.
        let Some(Found::Synced(l)) = pick_best(&v, &loose) else { panic!() };
        assert_eq!(l.lines[0].text, "live");
        // Without a length, a loose search accepts only the right artist.
        let no_len = Want { duration_s: 0, ..loose.clone() };
        let Some(Found::Synced(l)) = pick_best(&v, &no_len) else { panic!() };
        assert_eq!(l.lines[0].text, "live");
        let stranger = Want { artist: "Nobody".into(), ..no_len };
        assert_eq!(pick_best(&v, &stranger), None);
        // With a length, the artist may be wrong (a channel name): the title and length decide.
        let channel = Want { artist: "Random Channel".into(), ..loose };
        assert!(pick_best(&v, &channel).is_some());
    }

    #[test]
    fn closest_duration_within_three_seconds_wins() {
        let v: Value = serde_json::from_str(
            r#"[{"trackName":"S","artistName":"A","duration":232,"syncedLyrics":"[00:01.00]far"},
                {"trackName":"S","artistName":"A","duration":229.6,"syncedLyrics":"[00:01.00]near"},
                {"trackName":"S","artistName":"A","duration":227.2,"syncedLyrics":"[00:01.00]best"},
                {"trackName":"S","artistName":"A","duration":226.9,"syncedLyrics":null}]"#,
        )
        .unwrap();
        let w = Want { duration_s: 227, title: "S".into(), artist: "A".into(), loose: false };
        let Some(Found::Synced(l)) = pick_best(&v, &w) else { panic!() };
        assert_eq!(l.lines[0].text, "best");
        // 3 s is the limit: 230 s is out for a 226 s track, 229 is in.
        let w = Want { duration_s: 226, ..w };
        assert!(matches!(pick_best(&v, &w), Some(Found::Synced(l)) if l.lines[0].text == "best"));
        let w = Want { duration_s: 224, ..w };
        assert_eq!(pick_best(&v, &w), None, "nothing within 3 s of 224");
        // Instrumental ties lose to synced lyrics of the same length.
        let v: Value = serde_json::from_str(r#"[{"duration":200,"instrumental":true},{"duration":200,"syncedLyrics":"[00:01.00]sung"}]"#).unwrap();
        assert!(matches!(pick_search(&v, 200), Some(Found::Synced(_))));
    }
}
