//! LRCLIB client: `GET /api/get` (exact match), then up to three `/api/search` requests with the
//! title and artist cleaned of upload noise ([`title`]): title + artist, title alone, free text.
//! At most [`MAX_REQUESTS`] per track, always to the same host with the same data (artist,
//! title, album, length); a choice the user made earlier is fetched by id instead
//! (`/api/get/<id>`, same host). Every hit is judged by [`pick`]: the right artist, the right title
//! and the right length, or it is not shown. The fetch is blocking (lyrics service thread only);
//! everything but the network call is pure and tested offline (`fixtures/`).

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::lrc::Lyrics;
use super::pick::{self, Candidate, Hit, Want};
use super::title;
use crate::net;

const BASE: &str = "https://lrclib.net/api";
const TIMEOUT: Duration = Duration::from_secs(10);

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

/// `/api/get/<id>`: one entry by its LRCLIB id (a choice the user made earlier).
pub fn get_id_url(id: u64) -> String {
    format!("{BASE}/get/{id}")
}

/// One request of a lookup.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// `/api/get`: the exact track (needs album and length).
    Get(String),
    /// `/api/search`.
    Search { url: String },
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
    for url in [search_url(&c.title, &artist), search_title_url(&c.core), search_q_url(&free)] {
        if !c.core.is_empty() && !steps.iter().any(|s| matches!(s, Step::Search { url: u } if *u == url)) {
            steps.push(Step::Search { url });
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

/// What one lookup found.
#[derive(Clone, Debug, PartialEq)]
pub struct Lookup {
    /// The entries that fit the track, best first (strict ones before the loose ones, see [`pick`]).
    pub cands: Vec<Candidate>,
    /// The searches were made: there are no more entries to be had by asking again.
    pub searched: bool,
}

/// Hits of one answer: an array of entries (`/api/search`) or one entry (`/api/get`).
fn hits_of(v: &Value) -> Vec<Hit> {
    match v {
        Value::Array(items) => items.iter().filter_map(pick::hit_of).collect(),
        one => pick::hit_of(one).into_iter().collect(),
    }
}

fn add(all: &mut Vec<Hit>, new: Vec<Hit>) {
    for h in new {
        if !all.iter().any(|x| x.id == h.id && h.id != 0) {
            all.push(h);
        }
    }
}

fn ranked(hits: &[Hit], want: &Want, rejected: &[u64]) -> Vec<Candidate> {
    let mut c = pick::rank(hits, want);
    c.retain(|x| !rejected.contains(&x.id()));
    c
}

/// One request: the answer's JSON, `Ok(None)` for a 404.
pub type Getter<'a> = &'a mut dyn FnMut(&str) -> Result<Option<Value>, String>;

/// Look the track up with `get` doing the requests. With a `remembered` id, that entry is fetched
/// alone (`searched` stays false: nothing else was looked at); when it is gone, or there is no id,
/// the plan is followed until a **strict** candidate (see [`pick`]) turns up, and the entries in
/// `rejected` are left out.
pub fn lookup(q: &Query, remembered: Option<u64>, rejected: &[u64], get: Getter) -> Result<Lookup, String> {
    if let Some(id) = remembered {
        if let Some(h) = get(&get_id_url(id))?.as_ref().and_then(pick::hit_of) {
            return Ok(Lookup { cands: vec![Candidate::chosen(Hit { id, ..h })], searched: false });
        }
    }
    let want = Want::new(&q.title, &q.artist, q.duration_s);
    let mut hits: Vec<Hit> = Vec::new();
    for step in plan(q) {
        let (url, by_get) = match &step {
            Step::Get(u) => (u, true),
            Step::Search { url } => (url, false),
        };
        if let Some(v) = get(url)? {
            add(&mut hits, hits_of(&v));
        }
        let cands = ranked(&hits, &want, rejected);
        if cands.iter().any(|c| c.strict) {
            // An exact `get` hit leaves the searches undone: they only run if the user wants another.
            return Ok(Lookup { cands, searched: !by_get });
        }
    }
    Ok(Lookup { cands: ranked(&hits, &want, rejected), searched: true })
}

/// Every search of the plan (no early stop, no `get`): the entries for "Bukan lagu ini" once the
/// first lookup had nothing more to offer.
pub fn alternatives(q: &Query, rejected: &[u64], get: Getter) -> Result<Vec<Candidate>, String> {
    let want = Want::new(&q.title, &q.artist, q.duration_s);
    let mut hits: Vec<Hit> = Vec::new();
    for step in plan(q) {
        if let Step::Search { url } = step {
            if let Some(v) = get(&url)? {
                add(&mut hits, hits_of(&v));
            }
        }
    }
    Ok(ranked(&hits, &want, rejected))
}

/// [`lookup`] over the network. `Err` = network trouble (don't cache; try again next time).
pub fn fetch(q: &Query, remembered: Option<u64>, rejected: &[u64]) -> Result<Lookup, String> {
    lookup(q, remembered, rejected, &mut fetch_json)
}

/// [`alternatives`] over the network.
pub fn fetch_alternatives(q: &Query, rejected: &[u64]) -> Result<Vec<Candidate>, String> {
    alternatives(q, rejected, &mut fetch_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(title: &str, artist: &str, album: &str, dur: u64) -> Query {
        Query { key: String::new(), title: title.into(), artist: artist.into(), album: album.into(), duration_s: dur }
    }

    fn urls(steps: &[Step]) -> Vec<String> {
        steps.iter().map(|s| match s { Step::Get(u) | Step::Search { url: u } => u.clone() }).collect()
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
        assert!(steps[1..].iter().all(|s| matches!(s, Step::Search { .. })));
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
    fn the_id_url_stays_on_the_same_host() {
        assert_eq!(get_id_url(25993377), "https://lrclib.net/api/get/25993377");
    }

    #[test]
    fn a_remembered_choice_is_fetched_by_id_alone() {
        let one = serde_json::json!({"id": 5, "trackName": "Whatever", "artistName": "Anyone", "duration": 10, "syncedLyrics": "[00:01.00]a"});
        let mut asked: Vec<String> = vec![];
        let r = lookup(&q("Song", "Artist", "", 200), Some(5), &[], &mut |u| {
            asked.push(u.to_string());
            Ok(Some(one.clone()))
        })
        .unwrap();
        assert_eq!(asked, ["https://lrclib.net/api/get/5"]);
        assert_eq!((r.cands.len(), r.searched, r.cands[0].id()), (1, false, 5), "taken as it is: the user chose it, whatever its name");
        // Gone from LRCLIB: the normal lookup runs instead.
        let mut asked: Vec<String> = vec![];
        let r = lookup(&q("Song", "Artist", "", 200), Some(5), &[], &mut |u| {
            asked.push(u.to_string());
            Ok(None)
        })
        .unwrap();
        assert!(asked[0].ends_with("/get/5") && asked.len() > 1 && asked[1..].iter().all(|u| u.contains("/search?")));
        assert!(r.cands.is_empty() && r.searched);
        // Network trouble is an error, not "no lyrics".
        assert!(lookup(&q("Song", "Artist", "", 200), None, &[], &mut |_| Err("timeout".into())).is_err());
    }

    #[test]
    fn the_searches_stop_at_the_first_strict_candidate_and_skip_rejected_ones() {
        let hit = |id: u64, artist: &str| serde_json::json!({"id": id, "trackName": "Song", "artistName": artist, "albumName": "x", "duration": 200, "syncedLyrics": format!("[00:01.00]#{id}")});
        let mut n = 0;
        let r = lookup(&q("Song", "Artist", "", 200), None, &[], &mut |_| {
            n += 1;
            Ok(Some(serde_json::json!([hit(1, "Someone Else"), hit(2, "Artist")])))
        })
        .unwrap();
        assert_eq!(n, 1, "the first search had a strict hit");
        assert_eq!(r.cands.iter().map(|c| (c.id(), c.strict)).collect::<Vec<_>>(), [(2, true), (1, false)]);
        assert!(r.searched);
        // The user rejected 2: nothing strict is left, so every request of the plan runs.
        let mut n = 0;
        let r = lookup(&q("Song", "Artist", "", 200), None, &[2], &mut |_| {
            n += 1;
            Ok(Some(serde_json::json!([hit(1, "Someone Else"), hit(2, "Artist")])))
        })
        .unwrap();
        assert_eq!(n, plan(&q("Song", "Artist", "", 200)).len());
        assert_eq!(r.cands.iter().map(|c| c.id()).collect::<Vec<_>>(), [1]);
    }

    #[test]
    fn an_exact_get_hit_leaves_the_searches_for_later() {
        let exact = serde_json::json!({"id": 9, "trackName": "Song", "artistName": "Artist", "albumName": "Alb", "duration": 200, "syncedLyrics": "[00:01.00]a"});
        let mut asked = 0;
        let r = lookup(&q("Song", "Artist", "Alb", 200), None, &[], &mut |u| {
            asked += 1;
            assert!(u.contains("/get?"), "only the exact request: {u}");
            Ok(Some(exact.clone()))
        })
        .unwrap();
        assert_eq!((asked, r.searched, r.cands.len()), (1, false, 1));
        // `alternatives` then runs the searches (never `get`).
        let mut seen: Vec<String> = vec![];
        let alt = alternatives(&q("Song", "Artist", "Alb", 200), &[9], &mut |u| {
            seen.push(u.to_string());
            Ok(Some(serde_json::json!([exact.clone()])))
        })
        .unwrap();
        assert!(seen.iter().all(|u| u.contains("/search?")) && !seen.is_empty());
        assert!(alt.is_empty(), "9 was rejected");
    }
}
