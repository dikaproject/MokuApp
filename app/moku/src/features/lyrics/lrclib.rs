//! LRCLIB client: `GET /api/get` (exact match), falling back to `/api/search`. The fetch is
//! blocking (lyrics service thread only); the JSON → result part is pure and tested.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::lrc::{self, Lyrics};
use crate::net;

const BASE: &str = "https://lrclib.net/api";
const TIMEOUT: Duration = Duration::from_secs(10);
/// A search hit must be within this many seconds of the track (when the duration is known).
const DUR_TOLERANCE_S: f64 = 4.0;

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

pub fn search_url(title: &str, artist: &str) -> String {
    format!("{BASE}/search?track_name={}&artist_name={}", net::encode(title), net::encode(artist))
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

/// Best hit of `/api/search` (an array): synced lyrics with the closest duration.
pub fn pick_search(v: &Value, duration_s: u64) -> Option<Found> {
    let items = v.as_array()?;
    let mut best: Option<(f64, Found)> = None;
    for it in items {
        let Some(found) = synced(it) else { continue };
        let d = it.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
        let diff = if duration_s > 0 && d > 0.0 { (d - duration_s as f64).abs() } else { 0.0 };
        if duration_s > 0 && d > 0.0 && diff > DUR_TOLERANCE_S {
            continue;
        }
        // Prefer synced over instrumental on ties.
        let score = diff + if found == Found::Instrumental { 0.5 } else { 0.0 };
        if best.as_ref().is_none_or(|(s, _)| score < *s) {
            best = Some((score, found));
        }
    }
    best.map(|(_, f)| f)
}

/// Title without "(feat. …)", "[Remastered]", " - Remastered 2011", " - Radio Edit" etc.
pub fn clean_title(t: &str) -> String {
    let mut s = t.to_string();
    for (open, close) in [('(', ')'), ('[', ']')] {
        while let (Some(i), Some(j)) = (s.find(open), s.find(close)) {
            if j <= i {
                break;
            }
            s.replace_range(i..=j, "");
        }
    }
    if let Some(i) = s.find(" - ") {
        s.truncate(i);
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// First artist of "A, B & C" / "A feat. B".
pub fn first_artist(a: &str) -> String {
    let lower = a.to_lowercase();
    let mut end = a.len();
    for sep in [",", " & ", " feat", " ft.", " x ", ";", " / "] {
        if let Some(i) = lower.find(sep) {
            end = end.min(i);
        }
    }
    a[..end].trim().to_string()
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
    if !q.album.is_empty() && q.duration_s > 0 {
        if let Some(v) = fetch_json(&get_url(q))? {
            if let Some(f) = from_get(&v) {
                return Ok(f);
            }
        }
    }
    if let Some(v) = fetch_json(&search_url(&q.title, &q.artist))? {
        if let Some(f) = pick_search(&v, q.duration_s) {
            return Ok(f);
        }
    }
    let (t, a) = (clean_title(&q.title), first_artist(&q.artist));
    if (t.as_str(), a.as_str()) != (q.title.as_str(), q.artist.as_str()) && !t.is_empty() {
        if let Some(v) = fetch_json(&search_url(&t, &a))? {
            if let Some(f) = pick_search(&v, q.duration_s) {
                return Ok(f);
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
        assert_eq!(clean_title("Song (feat. X) [Live] - Remastered 2011"), "Song");
        assert_eq!(first_artist("A, B & C"), "A");
        assert_eq!(first_artist("Artist feat. Other"), "Artist");
    }
}
