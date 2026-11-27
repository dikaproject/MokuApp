//! What the user decided about a track's lyrics, remembered on this PC only (`lyrics-choices.json`
//! in the data folder, deleted by "Hapus semua data"): which LRCLIB entry is the right one (or that
//! none is), which ones were rejected, and how far the timing was nudged. Pure: parsing, writing,
//! the key of a track and the offset arithmetic; the file itself is read and written by the callers
//! (lazily, through [`crate::persist`]).
//!
//! A track's key is a hash of the normalised artist and title plus a bucket of its length, so the
//! same song played from another player (a slightly different title, a length a second off) finds
//! the same entry. Nothing in the file is readable as a title or an artist.

use std::path::Path;

use serde_json::{Value, json};

use super::pick::squash;
use super::title;

/// Entries kept; the oldest change is dropped first.
pub const MAX_ENTRIES: usize = 500;
/// Rejected ids kept per track.
const MAX_REJECTED: usize = 40;
/// The user's nudge of the lyrics stays within this many ms either way.
pub const MAX_OFFSET_MS: i32 = 5000;
/// One press of the offset buttons.
pub const OFFSET_STEP_MS: i32 = 500;
/// Seconds per length bucket of a key.
const BUCKET_S: u64 = 6;

/// What is remembered about one track.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Entry {
    /// The LRCLIB entry to show; `None` = the user said no entry fits.
    pub id: Option<u64>,
    pub rejected: Vec<u64>,
    /// Added to every lyric time: positive = the lyrics come later. On top of the file's own `[offset]`.
    pub offset_ms: i32,
    /// The match is the user's own choice (not what Moku picked by itself).
    pub user: bool,
}

/// All remembered tracks, oldest change first.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Store {
    entries: Vec<(String, Entry)>,
}

/// FNV-1a, 64 bit: stable across runs and Rust versions (unlike `DefaultHasher`).
fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The keys under which a track may be remembered: the first is the one to write; the second, when
/// there is one, is the neighbouring length bucket (a player that reports a second more or less).
/// `artist` / `title` are as the player gave them; they are cleaned and normalised here.
pub fn keys(artist: &str, title_: &str, duration_s: u64) -> Vec<String> {
    let c = title::clean(title_, artist);
    let a = squash(&title::first_artist(&c.artist));
    let t = squash(&c.core);
    let make = |bucket: &str| format!("{:016x}", fnv(&format!("{a}\u{1f}{t}\u{1f}{bucket}")));
    if duration_s == 0 {
        return vec![make("?")];
    }
    let (b, r) = (duration_s / BUCKET_S, duration_s % BUCKET_S);
    let mut v = vec![make(&b.to_string())];
    if r < 2 && b > 0 {
        v.push(make(&(b - 1).to_string()));
    } else if r + 2 >= BUCKET_S {
        v.push(make(&(b + 1).to_string()));
    }
    v
}

/// `offset_ms` after one press (`delta_ms`), kept within ±[`MAX_OFFSET_MS`].
pub fn nudge(offset_ms: i32, delta_ms: i32) -> i32 {
    offset_ms.saturating_add(delta_ms).clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS)
}

/// "+0,5 s" / "−1,0 s" / "0,0 s" (Indonesian decimal comma, a real minus sign).
pub fn fmt_offset(ms: i32) -> String {
    let sign = match ms {
        0 => "",
        m if m > 0 => "+",
        _ => "\u{2212}",
    };
    let a = ms.unsigned_abs();
    format!("{sign}{},{} s", a / 1000, (a % 1000) / 100)
}

impl Store {
    /// The entry for any of `keys` (the first one that is there).
    pub fn get(&self, keys: &[String]) -> Option<&Entry> {
        keys.iter().find_map(|k| self.entries.iter().find(|(x, _)| x == k).map(|(_, e)| e))
    }

    /// Remember `e` under `key` (the newest change goes last; the oldest are dropped past the cap).
    pub fn put(&mut self, key: &str, e: Entry) {
        self.entries.retain(|(k, _)| k != key);
        self.entries.push((key.to_string(), e));
        let extra = self.entries.len().saturating_sub(MAX_ENTRIES);
        self.entries.drain(..extra);
    }

    pub fn remove(&mut self, key: &str) {
        self.entries.retain(|(k, _)| k != key);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The file's text. Unreadable things (a wrong version, a bad entry) are skipped, never an error.
    pub fn parse(text: &str) -> Store {
        let Ok(v) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else { return Store::default() };
        let mut store = Store::default();
        for e in v.get("entries").and_then(Value::as_array).into_iter().flatten() {
            let Some(k) = e.get("k").and_then(Value::as_str).filter(|k| !k.is_empty() && k.len() <= 64) else { continue };
            let id = match e.get("id") {
                Some(Value::Number(n)) => match n.as_u64() {
                    Some(n) => Some(n),
                    None => continue,
                },
                Some(Value::String(s)) if s == "none" => None,
                _ => continue,
            };
            let rejected: Vec<u64> = e.get("rejected").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).take(MAX_REJECTED).collect()).unwrap_or_default();
            let offset_ms = e.get("offset_ms").and_then(Value::as_i64).unwrap_or(0).clamp(-(MAX_OFFSET_MS as i64), MAX_OFFSET_MS as i64) as i32;
            let user = e.get("user").and_then(Value::as_bool).unwrap_or(false);
            store.put(k, Entry { id, rejected, offset_ms, user });
        }
        store
    }

    pub fn to_json(&self) -> Vec<u8> {
        let entries: Vec<Value> = self
            .entries
            .iter()
            .map(|(k, e)| json!({ "k": k, "id": e.id.map(Value::from).unwrap_or_else(|| Value::from("none")), "rejected": e.rejected, "offset_ms": e.offset_ms, "user": e.user }))
            .collect();
        serde_json::to_vec(&json!({ "v": 1, "entries": entries })).unwrap_or_default()
    }

    /// Read the file (a missing or unreadable file is an empty store).
    pub fn load(path: &Path) -> Store {
        std::fs::read(path).map(|b| Store::parse(&String::from_utf8_lossy(&b))).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: Option<u64>) -> Entry {
        Entry { id, rejected: vec![], offset_ms: 0, user: true }
    }

    #[test]
    fn the_same_song_from_another_player_has_the_same_key() {
        let k = |a: &str, t: &str, d: u64| keys(a, t, d)[0].clone();
        let base = k("Crayon Case", "because", 232);
        assert_eq!(base.len(), 16);
        // Case, trailing punctuation, upload noise and channel suffixes do not matter.
        assert_eq!(k("crayon case", "because,", 232), base);
        assert_eq!(k("Crayon Case - Topic", "because (Official Audio)", 232), base);
        assert_eq!(k("CrayonCaseVEVO", "BECAUSE", 232), base);
        assert_eq!(k("", "Crayon Case - because", 232), base);
        // A guest in the title or the artist field is not part of the identity.
        assert_eq!(k("Dua Lipa feat. DaBaby", "Levitating", 204), k("Dua Lipa", "Levitating (feat. DaBaby)", 204));
        // Another song, another artist, another length: another key.
        assert_ne!(k("Crayon Case", "because", 100), base);
        assert_ne!(k("Someone", "because", 232), base);
        assert_ne!(k("Crayon Case", "because of you", 232), base);
        assert_ne!(k("Crayon Case", "because", 0), base);
    }

    #[test]
    fn a_length_a_second_off_finds_the_same_entry() {
        // Bucket edges: 6 s buckets; the writer uses keys[0], a reader tries all of its own.
        for d in 100..400u64 {
            let write = keys("A", "T", d)[0].clone();
            for off in [-2i64, -1, 0, 1, 2] {
                let read = keys("A", "T", (d as i64 + off) as u64);
                assert!(read.contains(&write), "{d} written, {} read", d as i64 + off);
            }
        }
        assert_eq!(keys("A", "T", 0).len(), 1);
        assert_eq!(keys("A", "T", 3).len(), 1, "bucket 0 has no lower neighbour");
    }

    #[test]
    fn nothing_readable_is_in_a_key() {
        let k = &keys("Crayon Case", "because", 232)[0];
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()), "{k}");
    }

    #[test]
    fn offsets_step_and_stay_within_five_seconds() {
        assert_eq!(nudge(0, OFFSET_STEP_MS), 500);
        assert_eq!(nudge(500, -OFFSET_STEP_MS * 3), -1000);
        assert_eq!(nudge(4800, OFFSET_STEP_MS), MAX_OFFSET_MS);
        assert_eq!(nudge(-4800, -OFFSET_STEP_MS), -MAX_OFFSET_MS);
        assert_eq!(nudge(MAX_OFFSET_MS, i32::MAX), MAX_OFFSET_MS);
        assert_eq!(nudge(i32::MIN, i32::MIN), -MAX_OFFSET_MS);
        for (ms, s) in [(0, "0,0 s"), (500, "+0,5 s"), (-500, "\u{2212}0,5 s"), (1000, "+1,0 s"), (-5000, "\u{2212}5,0 s"), (2500, "+2,5 s")] {
            assert_eq!(fmt_offset(ms), s);
        }
    }

    #[test]
    fn the_file_round_trips_and_the_oldest_entries_fall_off() {
        let mut s = Store::default();
        s.put("a", Entry { id: Some(25993377), rejected: vec![1, 2], offset_ms: -500, user: true });
        s.put("b", entry(None));
        let back = Store::parse(std::str::from_utf8(&s.to_json()).unwrap());
        assert_eq!(back, s);
        assert_eq!(back.get(&["b".into()]).unwrap().id, None);
        assert!(std::str::from_utf8(&s.to_json()).unwrap().contains("\"id\":\"none\""));
        // The newest change is the last; changing an entry again moves it to the end.
        s.put("a", entry(Some(7)));
        assert_eq!(s.entries.last().unwrap().0, "a");
        assert_eq!(s.len(), 2);
        let mut big = Store::default();
        for i in 0..(MAX_ENTRIES + 25) {
            big.put(&format!("k{i}"), entry(Some(i as u64)));
        }
        assert_eq!(big.len(), MAX_ENTRIES);
        assert!(big.get(&["k0".into()]).is_none() && big.get(&["k24".into()]).is_none());
        assert!(big.get(&["k25".into()]).is_some() && big.get(&[format!("k{}", MAX_ENTRIES + 24)]).is_some());
        s.remove("a");
        assert!(s.get(&["a".into()]).is_none());
    }

    #[test]
    fn the_first_known_key_of_a_track_wins_and_bad_files_are_empty_stores() {
        let mut s = Store::default();
        s.put("old", entry(Some(1)));
        assert_eq!(s.get(&["new".into(), "old".into()]).unwrap().id, Some(1));
        assert!(s.get(&["nothing".into()]).is_none());
        for bad in ["", "not json", "[]", "{}", "{\"entries\":3}", "\u{feff}{\"v\":1}"] {
            assert!(Store::parse(bad).is_empty(), "{bad:?}");
        }
        // A bad entry is skipped, the others stay; an offset out of range is brought back.
        let t = r#"{"v":1,"entries":[{"k":"a","id":5,"rejected":[1,"x",2],"offset_ms":99999,"user":true},{"k":"","id":1},{"id":2,"k":"b","offset_ms":-3},{"k":"c","id":"maybe"},{"k":"d","id":"none","rejected":[9]}]}"#;
        let s = Store::parse(t);
        assert_eq!(s.len(), 3, "a, b and d; the entry without a key and the one with a bad id are skipped");
        assert_eq!(s.get(&["a".into()]), Some(&Entry { id: Some(5), rejected: vec![1, 2], offset_ms: MAX_OFFSET_MS, user: true }));
        assert_eq!(s.get(&["d".into()]), Some(&Entry { id: None, rejected: vec![9], offset_ms: 0, user: false }));
    }

    #[test]
    fn the_file_is_written_atomically_and_read_back() {
        // The temp folder of a runner can be an 8.3 path (RUNNER~1): compare real paths only.
        let dir = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("moku-lyrics-choices-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = dir.join("lyrics-choices.json");
        assert!(Store::load(&f).is_empty(), "a missing file is an empty store");
        let mut s = Store::default();
        s.put(&keys("Crayon Case", "because", 232)[0], Entry { id: Some(25993377), rejected: vec![35870680], offset_ms: 1500, user: true });
        crate::persist::write_atomic(&f, &s.to_json()).unwrap();
        assert!(!dir.join("lyrics-choices.json.tmp").exists());
        assert_eq!(Store::load(&f), s);
        std::fs::write(&f, b"{broken").unwrap();
        assert!(Store::load(&f).is_empty(), "a damaged file is an empty store, not a crash");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
