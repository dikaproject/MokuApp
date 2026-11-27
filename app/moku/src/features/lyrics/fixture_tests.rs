//! `plan` + `lookup` against answers recorded from the real LRCLIB (`fixtures/`), offline: no test
//! here touches the network. The recorded entries keep LRCLIB's fields (id, names, album, length,
//! flags) and the timestamps of their synced lyrics, but the lyric *words* are replaced by "la" (the
//! first line says "#<id>"): the repository does not carry other people's lyrics.
//!
//! Each case is what a player reports (title, artist, album, length) and which recorded answer each
//! request of the plan gets (`artist_track`, `title_only`, `q`, `get`; a request without an answer
//! gets "nothing found"). A few cases drop the answer of the first search on purpose ("the artist
//! search found nothing"): that is when the loose fallbacks used to pick another artist's song.

use serde_json::Value;

use super::lrclib::{Found, Lookup, Query, Step, lookup, plan};
use super::pick;

fn fx(name: &str) -> Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/features/lyrics/fixtures").join(name);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

fn kind(url: &str) -> &'static str {
    if url.contains("/get?") {
        "get"
    } else if url.contains("?q=") {
        "q"
    } else if url.contains("artist_name=") {
        "artist_track"
    } else {
        "title_only"
    }
}

fn q(title: &str, artist: &str, album: &str, dur: u64) -> Query {
    Query { key: String::new(), title: title.into(), artist: artist.into(), album: album.into(), duration_s: dur }
}

/// The lookup of `query` when each request is answered from `files` (kind → fixture file).
fn run(query: &Query, files: &[(&str, &str)]) -> (Lookup, Vec<&'static str>) {
    let mut asked = vec![];
    let r = lookup(query, None, &[], &mut |url| {
        let k = kind(url);
        asked.push(k);
        Ok(files.iter().find(|(f, _)| *f == k).map(|(_, file)| fx(file)))
    })
    .unwrap();
    (r, asked)
}

/// The id Moku shows by itself (`None` = "Lirik tidak tersedia").
fn auto(query: &Query, files: &[(&str, &str)]) -> Option<u64> {
    let (r, _) = run(query, files);
    r.cands.iter().find(|c| c.strict).map(|c| c.id())
}

const CRAYON: &[(&str, &str)] = &[("artist_track", "crayon_search_artist_track.json"), ("title_only", "crayon_search_title_only.json"), ("q", "crayon_search_q.json")];
const YELLOW: &[(&str, &str)] = &[("artist_track", "yellow_search_artist_track.json"), ("title_only", "yellow_search_title_only.json")];
const HALLELUJAH: &[(&str, &str)] = &[("artist_track", "hallelujah_search_artist_track.json"), ("title_only", "hallelujah_search_title_only.json")];
const REMIX: &[(&str, &str)] = &[("artist_track", "remix_search_artist_track.json"), ("title_only", "remix_search_title_only.json")];
const PLAIN: &[(&str, &str)] = &[("artist_track", "remix_plain_search_artist_track.json"), ("title_only", "remix_search_title_only.json")];
const LEVITATING: &[(&str, &str)] = &[("artist_track", "levitating_search_artist_track.json"), ("title_only", "levitating_search_title_only.json")];
const TULUS: &[(&str, &str)] = &[("artist_track", "tulus_search_artist_track.json"), ("title_only", "tulus_search_title_only.json")];
/// "The artist search found nothing": only the title-only answer is there.
const CRAYON_NO_ARTIST_HITS: &[(&str, &str)] = &[("title_only", "crayon_search_title_only.json")];
const HALLELUJAH_NO_ARTIST_HITS: &[(&str, &str)] = &[("title_only", "hallelujah_search_title_only.json")];
const LEVITATING_NO_ARTIST_HITS: &[(&str, &str)] = &[("title_only", "levitating_search_title_only.json")];

#[test]
fn the_recorded_answers_are_what_the_cases_below_assume() {
    // Guards the fixtures: 20 entries each (LRCLIB's search limit), and the facts the cases rely on.
    for f in ["crayon_search_artist_track.json", "crayon_search_title_only.json", "yellow_search_artist_track.json", "hallelujah_search_artist_track.json", "remix_search_artist_track.json", "levitating_search_artist_track.json", "tulus_search_artist_track.json"] {
        assert_eq!(fx(f).as_array().unwrap().len(), 20, "{f}");
    }
    let crayon = fx("crayon_search_artist_track.json");
    let ids: Vec<u64> = crayon.as_array().unwrap().iter().filter(|e| e["syncedLyrics"].is_string()).map(|e| e["id"].as_u64().unwrap()).collect();
    assert_eq!(ids, [35870680, 25993377], "the right song is there twice, as 'because,'");
    assert!(crayon.as_array().unwrap().iter().all(|e| e["trackName"] == "because,"));
}

#[test]
fn crayon_case_because_gets_the_right_entry_from_the_noisy_duplicates() {
    // Browser-like (no album): 232 s is the entry of that length, not the 233 s one.
    assert_eq!(auto(&q("because", "Crayon Case", "", 232), CRAYON), Some(25993377));
    // 233 s picks the other one; both are the same song.
    assert_eq!(auto(&q("because", "Crayon Case", "", 233), CRAYON), Some(35870680));
    // A trailing comma in the player's title changes nothing.
    assert_eq!(auto(&q("because,", "Crayon Case", "", 232), CRAYON), Some(25993377));
    // No length from the player: still synced lyrics of the right song; the oldest entry with a real
    // album ("null" is not an album).
    assert_eq!(auto(&q("because", "Crayon Case", "", 0), CRAYON), Some(25993377));
    // A YouTube-style title with the artist in it and a channel suffix.
    assert_eq!(auto(&q("Crayon Case - because (Official Audio)", "Crayon Case - Topic", "", 232), CRAYON), Some(25993377));
    // Spotify-like (album and length known): the exact `get` answers, and no search is made.
    let (r, asked) = run(&q("because", "Crayon Case", "because,", 232), &[("get", "crayon_get_25993377.json"), ("artist_track", "crayon_search_artist_track.json")]);
    assert_eq!((asked, r.cands[0].id(), r.searched), (vec!["get"], 25993377, false));
    // The 3 s / 4 s / 80 s junk entries and the unsynced ones never show up, even when asked for
    // their length: nothing within reach is the song, so nothing is shown.
    for junk in [3, 4, 80, 138] {
        let (r, _) = run(&q("because", "Crayon Case", "", junk), CRAYON);
        assert!(r.cands.iter().all(|c| !c.strict), "{junk}");
        assert!(r.cands.iter().all(|c| c.hit.duration_s >= 20.0), "{junk}: no entry shorter than 20 s");
    }
}

#[test]
fn another_artists_song_with_the_same_word_or_length_is_never_shown() {
    // The artist search found nothing: the title-only answer has the Beatles' "Because" (166 s),
    // Lillix (146 s), "Because Because Because" (Mercury Rev, an instrumental of 205 s) ...
    for dur in [166, 146, 142, 205, 206, 150] {
        assert_eq!(auto(&q("because", "Crayon Case", "", dur), CRAYON_NO_ARTIST_HITS), None, "{dur} s");
    }
    // They are still there for the user to try with "Bukan lagu ini": by title with the right
    // length (within 10 s), but never the longer titles.
    let (r, _) = run(&q("because", "Crayon Case", "", 166), CRAYON_NO_ARTIST_HITS);
    let names: Vec<&str> = r.cands.iter().map(|c| c.hit.track.as_str()).collect();
    assert!(names.contains(&"Because") && names.iter().all(|n| !n.contains("Because Because") && *n != "Because of Because"), "{names:?}");
    assert!(r.cands.iter().all(|c| !c.strict));
}

#[test]
fn a_common_word_title_stays_with_its_artist() {
    assert_eq!(auto(&q("Yellow", "Coldplay", "", 267), YELLOW), Some(4671993), "267 s, the oldest entry with an album");
    assert_eq!(auto(&q("Yellow", "Coldplay", "", 270), YELLOW), Some(3667822));
    // No length: the right artist and title are enough, and the oldest entry wins.
    assert_eq!(auto(&q("Yellow", "Coldplay", "", 0), YELLOW), Some(3667822));
    // Another artist's "Yellow" never matches Coldplay's title: "Yellow Yellow" is another song.
    let (r, _) = run(&q("Yellow", "Someone Unknown", "", 182), &[("title_only", "yellow_search_title_only.json")]);
    assert!(r.cands.iter().all(|c| !c.strict), "{:?}", r.cands.iter().map(|c| &c.hit.track).collect::<Vec<_>>());
    assert!(r.cands.iter().all(|c| c.hit.track.to_lowercase() != "yellow yellow"));
}

#[test]
fn a_cover_is_not_taken_for_the_original() {
    // Jeff Buckley's own entries, 413 s from the album: any of the right ones, never Cohen's.
    let id = auto(&q("Hallelujah", "Jeff Buckley", "Grace", 413), HALLELUJAH).unwrap();
    let (r, _) = run(&q("Hallelujah", "Jeff Buckley", "Grace", 413), HALLELUJAH);
    assert!(r.cands.iter().filter(|c| c.strict).all(|c| c.hit.artist == "Jeff Buckley" && (c.hit.duration_s - 413.0).abs() <= 5.0));
    assert_eq!(id, 2788228, "the oldest of the 413 s entries that has an album");
    // Cohen's "Hallelujah", artist search empty, a length a cover happens to share: not Buckley's.
    assert_eq!(auto(&q("Hallelujah", "Leonard Cohen", "", 409), HALLELUJAH_NO_ARTIST_HITS), None);
    // Kidz Bop's "Levitating" is not Dua Lipa's, whatever the length.
    assert_eq!(auto(&q("Levitating", "Dua Lipa", "", 185), LEVITATING_NO_ARTIST_HITS), None);
    assert_eq!(auto(&q("Levitating", "KIDZ BOP Kids", "", 185), LEVITATING_NO_ARTIST_HITS), Some(7694820));
}

#[test]
fn a_remix_is_not_the_original_and_the_original_is_not_the_remix() {
    // The remix (217 s): its own entries, the oldest with an album.
    assert_eq!(auto(&q("Don't Start Now (Purple Disco Machine Remix)", "Dua Lipa", "", 217), REMIX), Some(1207995));
    // The plain title at the remix's length: the original's entries are 183 s, so nothing is shown.
    assert_eq!(auto(&q("Don't Start Now", "Dua Lipa", "", 217), PLAIN), None);
    // The original (183 s): a real "Don't Start Now" with an album, not the odd
    // "Don't Start Now [Don't Start Now, 2019]" entry that sat first in the answer.
    let id = auto(&q("Don't Start Now", "Dua Lipa", "", 183), PLAIN).unwrap();
    let hit = fx("remix_plain_search_artist_track.json").as_array().unwrap().iter().find(|e| e["id"] == id).unwrap().clone();
    assert_eq!(hit["trackName"].as_str().map(|t| t.contains('[')), Some(false), "{hit}");
    assert_eq!(id, 3431093);
    // Curly apostrophes in LRCLIB's title are the same title.
    assert_eq!(auto(&q("Don\u{2019}t Start Now", "Dua Lipa", "", 180), PLAIN), Some(5683441));
}

#[test]
fn featuring_artists_in_the_title_or_the_hit_do_not_matter() {
    // The title says "(feat. DaBaby)", LRCLIB mostly does not.
    assert_eq!(auto(&q("Levitating (feat. DaBaby)", "Dua Lipa", "", 204), LEVITATING), Some(3820124));
    // ... and the hit's artist may carry the guest or a channel suffix ("Dua Lipa feat. DaBaby",
    // "Dua Lipa - Topic" are in the answer): all of them are Dua Lipa's, and Kidz Bop / The Boyz are not.
    let (r, _) = run(&q("Levitating", "Dua Lipa", "", 203), &[("title_only", "levitating_search_title_only.json")]);
    assert!(r.cands.iter().filter(|c| c.strict).all(|c| c.hit.artist.starts_with("Dua Lipa")));
    assert!(r.cands.iter().all(|c| !c.hit.artist.to_lowercase().contains("kidz") && c.hit.artist != "THE BOYZ"));
    assert_eq!(pick::artist_match("Dua Lipa", "Dua Lipa feat. DaBaby"), pick::ArtistMatch::Alike);
    assert_eq!(pick::artist_match("Dua Lipa", "Dua Lipa - Topic"), pick::ArtistMatch::Exact);
}

#[test]
fn an_indonesian_song_with_a_bracketed_tag_and_hyphens() {
    // "[Official Lyric Video]" and "- Topic" are noise; the three spellings of the hyphen are one title.
    let spotify = q("Hati-Hati di Jalan [Official Lyric Video]", "Tulus - Topic", "Manusia", 242);
    assert_eq!(auto(&spotify, TULUS), Some(2998875));
    let (r, _) = run(&q("Hati-Hati di Jalan", "Tulus", "", 262), TULUS);
    let top = r.cands.iter().find(|c| c.strict).unwrap();
    assert_eq!((top.hit.duration_s, top.hit.track.as_str()), (262.0, "Hati\u{2010}Hati di Jalan"));
    // No length: the junk entries (4, 50, 80, 140 s, each with a whole song of lyrics) are noise.
    let (r, _) = run(&q("Hati-Hati di Jalan [Official Lyric Video]", "Tulus - Topic", "", 0), TULUS);
    assert_eq!(r.cands.iter().find(|c| c.strict).map(|c| c.id()), Some(2998875));
    assert!(r.cands.iter().all(|c| c.hit.duration_s >= 200.0), "{:?}", r.cands.iter().map(|c| (c.id(), c.hit.duration_s)).collect::<Vec<_>>());
    // Another artist's "Hati Hati Di Jalan" (an instrumental by "Coffee Shop") is not Tulus's song.
    assert!(r.cands.iter().all(|c| c.hit.artist != "Coffee Shop"));
}

#[test]
fn every_request_goes_to_lrclib_and_the_budget_is_kept() {
    // The steps the cases above take are the plan's, at most four, all to the same host.
    for query in [q("because", "Crayon Case", "because,", 232), q("Yellow", "Coldplay", "Parachutes", 267)] {
        let steps = plan(&query);
        assert!(steps.len() <= 4);
        for s in steps {
            let (Step::Get(u) | Step::Search { url: u }) = s;
            assert!(u.starts_with("https://lrclib.net/api/"), "{u}");
        }
    }
    // A lookup that finds nothing strict asks for everything in the plan and then gives up with the
    // loose candidates only; it never panics on a fixture-shaped answer of the wrong kind.
    let (r, asked) = run(&q("Totally Unknown Song", "Nobody", "", 123), CRAYON);
    assert_eq!(asked.len(), plan(&q("Totally Unknown Song", "Nobody", "", 123)).len());
    assert!(r.cands.is_empty() && r.searched);
    let _ = Found::Missing;
}
