//! Usage history (`launcher.json` in the data folder): how often and when each entry was run,
//! and which entry was picked for a query. Feeds the ranking boost. Ids only (app paths /
//! AUMIDs / settings pages, Shelf file paths) — never the calculator or to-do text.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Largest boost from use counts and recency (see `matcher::score` for the tiers).
pub const MAX_BOOST: i32 = 140;
/// Extra for the entry picked last time for exactly this query.
pub const PICK_BOOST: i32 = 300;
const MAX_ENTRIES: usize = 400;
const MAX_PICKS: usize = 200;
const DAY: i64 = 86_400;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Use {
    pub count: u32,
    /// Unix seconds.
    pub last: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub uses: HashMap<String, Use>,
    /// Folded query → id picked for it.
    #[serde(default)]
    pub picks: HashMap<String, String>,
}

impl History {
    pub fn parse(bytes: &[u8]) -> History {
        let b = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        serde_json::from_slice(b).unwrap_or_default()
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// `id` was run after typing `query` (folded; empty = picked from the list without typing).
    pub fn record(&mut self, id: &str, query: &str, now: i64) {
        let u = self.uses.entry(id.to_string()).or_default();
        u.count = u.count.saturating_add(1);
        u.last = now;
        let q = query.trim();
        if !q.is_empty() && q.chars().count() <= 24 {
            self.picks.insert(q.to_string(), id.to_string());
        }
        // Bounded: forget the least used / oldest first.
        if self.uses.len() > MAX_ENTRIES {
            let mut v: Vec<(String, Use)> = self.uses.drain().collect();
            v.sort_by(|a, b| b.1.count.cmp(&a.1.count).then(b.1.last.cmp(&a.1.last)));
            v.truncate(MAX_ENTRIES);
            self.uses = v.into_iter().collect();
        }
        if self.picks.len() > MAX_PICKS {
            let keep: Vec<(String, String)> = self.picks.drain().filter(|(_, id)| self.uses.contains_key(id)).take(MAX_PICKS).collect();
            self.picks = keep.into_iter().collect();
        }
    }

    /// Ranking boost for `id` (0 ..= `MAX_BOOST` + `PICK_BOOST`).
    pub fn boost(&self, id: &str, query: &str, now: i64) -> i32 {
        let mut b = 0;
        if let Some(u) = self.uses.get(id) {
            b += (30.0 * (1.0 + u.count as f64).ln()).min(110.0) as i32;
            let age = now - u.last;
            b += if age < DAY { 30 } else if age < 7 * DAY { 15 } else { 0 };
        }
        let b = b.min(MAX_BOOST);
        if !query.is_empty() && self.picks.get(query.trim()).is_some_and(|p| p == id) { b + PICK_BOOST } else { b }
    }

    /// Most used ids (for the list shown before anything is typed).
    pub fn top(&self, n: usize, now: i64) -> Vec<&str> {
        let mut v: Vec<(&String, i32)> = self.uses.keys().map(|k| (k, self.boost(k, "", now))).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        v.into_iter().take(n).map(|(k, _)| k.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boost_grows_with_use_and_recency_and_is_capped() {
        let mut h = History::default();
        let now = 1_800_000_000;
        assert_eq!(h.boost("a", "", now), 0);
        h.record("a", "", now - 30 * DAY);
        let once_old = h.boost("a", "", now);
        h.record("a", "", now);
        assert!(h.boost("a", "", now) > once_old);
        for _ in 0..500 {
            h.record("a", "", now);
        }
        assert_eq!(h.boost("a", "", now), MAX_BOOST);
        // The entry picked for exactly this query gets the pick boost.
        h.record("b", "co", now);
        assert_eq!(h.boost("b", "co", now) - h.boost("b", "", now), PICK_BOOST);
        assert_eq!(h.boost("a", "co", now), MAX_BOOST);
        assert_eq!(h.top(1, now), ["a"]);
    }

    #[test]
    fn roundtrip_and_bounds() {
        let mut h = History::default();
        for i in 0..(MAX_ENTRIES + 20) {
            h.record(&format!("id{i}"), &format!("q{i}"), 1000 + i as i64);
        }
        assert!(h.uses.len() <= MAX_ENTRIES && h.picks.len() <= MAX_PICKS);
        let back = History::parse(&h.to_json());
        assert_eq!(back, h);
        assert_eq!(History::parse(b"not json"), History::default());
        assert_eq!(History::parse(b"\xEF\xBB\xBF{}"), History::default());
    }
}
