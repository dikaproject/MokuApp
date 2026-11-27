//! The lyrics of the current track as the user sees and corrects them (pure, no I/O): the
//! candidates a lookup found, which one is shown, which ones were rejected, the timing offset.
//! "Bukan lagu ini" moves to the next candidate; after the last one nothing is shown ("Lirik tidak
//! tersedia") until the user starts over. [`View::entry`] is what is remembered of all that
//! ([`super::choices`]).

use std::sync::Arc;

use super::choices::{Entry, MAX_OFFSET_MS, nudge};
use super::lrc::Lyrics;
use super::lrclib::{Found, Lookup};
use super::pick::Candidate;

/// What pressing "Bukan lagu ini" did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Another candidate is shown.
    Shown,
    /// Nothing more is known: the searches have to be made first (a remembered choice was fetched
    /// by id alone). Nothing is shown until the answer is added with [`View::add`].
    NeedSearch,
    /// Every candidate was rejected: nothing is shown.
    Exhausted,
}

/// What the one button next to the caption does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// A match is shown: "Bukan lagu ini".
    WrongSong,
    /// Nothing is shown, and there is something else to try (or to look for).
    TryOther,
    /// Everything was rejected: "Coba lagi dari awal".
    Restart,
    /// Nothing to offer.
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub struct View {
    cands: Vec<Candidate>,
    shown: Option<u64>,
    rejected: Vec<u64>,
    offset_ms: i32,
    /// The shown entry (or the "none") is the user's decision.
    user: bool,
    /// The searches have been made (see [`Lookup::searched`]).
    searched: bool,
    /// The shown lyrics with the offset applied.
    display: Found,
    /// A restart is waiting for the searches ([`Next::NeedSearch`]).
    restart_pending: bool,
}

impl View {
    /// The view of a lookup, with what is remembered of the track (if anything) applied.
    pub fn new(lookup: Lookup, memory: Option<&Entry>) -> View {
        let mut v = View { cands: lookup.cands, shown: None, rejected: vec![], offset_ms: 0, user: false, searched: lookup.searched, display: Found::Missing, restart_pending: false };
        match memory {
            Some(m) => {
                v.rejected = m.rejected.clone();
                v.offset_ms = m.offset_ms;
                match m.id {
                    // "None fits": what the user decided; nothing is shown.
                    None => v.user = true,
                    Some(id) if v.cands.iter().any(|c| c.id() == id) => {
                        v.shown = Some(id);
                        v.user = m.user;
                    }
                    // The remembered entry is gone: Moku's own pick, the offset stays.
                    Some(_) => v.shown = v.auto(),
                }
            }
            None => v.shown = v.auto(),
        }
        v.refresh();
        v
    }

    /// Moku's own pick: the best strict candidate that was not rejected.
    fn auto(&self) -> Option<u64> {
        self.cands.iter().find(|c| c.strict && !self.rejected.contains(&c.id())).map(Candidate::id)
    }

    fn refresh(&mut self) {
        let shown = self.shown.and_then(|id| self.cands.iter().find(|c| c.id() == id));
        self.display = match shown.map(|c| &c.hit.found) {
            Some(Found::Synced(l)) => Found::Synced(Arc::new(l.shifted(self.offset_ms as i64))),
            Some(Found::Instrumental) => Found::Instrumental,
            _ => Found::Missing,
        };
    }

    /// What to show now (the lyrics with the user's offset applied, or `Missing`).
    pub fn found(&self) -> &Found {
        &self.display
    }

    pub fn shown(&self) -> Option<&Candidate> {
        self.shown.and_then(|id| self.cands.iter().find(|c| c.id() == id))
    }

    /// Id of the shown entry (0 when nothing is shown).
    pub fn shown_id(&self) -> u64 {
        self.shown.unwrap_or(0)
    }

    /// Ids the user said are not the song.
    pub fn rejected(&self) -> &[u64] {
        &self.rejected
    }

    pub fn offset_ms(&self) -> i32 {
        self.offset_ms
    }

    pub fn is_user_choice(&self) -> bool {
        self.user && self.shown.is_some()
    }

    /// Is another candidate left to show (not shown now, not rejected)?
    fn remaining(&self) -> bool {
        self.cands.iter().any(|c| Some(c.id()) != self.shown && !self.rejected.contains(&c.id()))
    }

    /// The label of the button for the current state.
    pub fn button(&self) -> Button {
        if self.shown.is_some() {
            Button::WrongSong
        } else if self.rejected.is_empty() && (self.remaining() || !self.searched) {
            Button::TryOther
        } else if !self.rejected.is_empty() {
            Button::Restart
        } else {
            Button::None
        }
    }

    /// "Bukan lagu ini" (or "Coba kecocokan lain" when nothing is shown): reject what is shown and
    /// move to the next candidate.
    pub fn next(&mut self) -> Next {
        if let Some(id) = self.shown.take() {
            self.rejected.push(id);
        }
        self.user = true;
        self.restart_pending = false;
        self.select()
    }

    fn select(&mut self) -> Next {
        let pick = self.cands.iter().find(|c| !self.rejected.contains(&c.id())).map(Candidate::id);
        self.shown = pick;
        self.refresh();
        match pick {
            Some(_) => Next::Shown,
            None if !self.searched => Next::NeedSearch,
            None => Next::Exhausted,
        }
    }

    /// "Coba lagi dari awal": forget the rejections, the choice and the offset; Moku's own pick is
    /// shown again.
    pub fn restart(&mut self) -> Next {
        self.rejected.clear();
        self.offset_ms = 0;
        self.user = false;
        self.shown = self.auto();
        self.refresh();
        self.restart_pending = self.shown.is_none() && !self.searched;
        match self.shown {
            Some(_) => Next::Shown,
            None if !self.searched => Next::NeedSearch,
            None => Next::Exhausted,
        }
    }

    /// The answer to a search that was asked for ([`Next::NeedSearch`]): the new candidates join the
    /// list (those already there, and the rejected ones, are left out) and the next one is shown.
    pub fn add(&mut self, more: Vec<Candidate>) -> Next {
        for c in more {
            let same = |x: &Candidate| x.id() == c.id() || (x.hit.found == c.hit.found && c.hit.found != Found::Instrumental);
            if !self.cands.iter().any(same) {
                self.cands.push(c);
            }
        }
        // Moku's own order: strict first, then by score.
        self.cands.sort_by(|a, b| b.strict.cmp(&a.strict).then(b.score.cmp(&a.score)).then(a.id().cmp(&b.id())));
        self.searched = true;
        // A restart that was waiting for the searches shows Moku's own pick; a "next" the next one.
        if std::mem::take(&mut self.restart_pending) {
            self.shown = self.auto();
            self.refresh();
            if self.shown.is_some() { Next::Shown } else { Next::Exhausted }
        } else {
            self.select()
        }
    }

    /// Move the lyrics by `delta_ms` (positive = later), within ±5 s. Only while lyrics are shown.
    /// True if anything changed.
    pub fn shift(&mut self, delta_ms: i32) -> bool {
        if !matches!(self.display, Found::Synced(_)) {
            return false;
        }
        let o = nudge(self.offset_ms, delta_ms);
        if o == self.offset_ms {
            return false;
        }
        self.offset_ms = o;
        self.refresh();
        true
    }

    /// The synced lyrics being shown.
    pub fn lyrics(&self) -> Option<&Arc<Lyrics>> {
        match &self.display {
            Found::Synced(l) => Some(l),
            _ => None,
        }
    }

    /// "Artist – Title" of the shown entry as LRCLIB has it.
    pub fn caption(&self) -> Option<String> {
        let h = &self.shown()?.hit;
        Some(match (h.artist.trim(), h.track.trim()) {
            ("", t) => t.to_string(),
            (a, "") => a.to_string(),
            (a, t) => format!("{a} \u{2013} {t}"),
        })
    }

    /// What is worth remembering of this view; `None` when it is just what Moku would pick again.
    pub fn entry(&self) -> Option<Entry> {
        if !self.user && self.offset_ms == 0 && self.rejected.is_empty() {
            return None;
        }
        // Moku's own pick with only an offset: pin it, so the offset goes with the same lyrics.
        Some(Entry { id: self.shown, rejected: self.rejected.clone(), offset_ms: self.offset_ms.clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS), user: self.user })
    }
}

#[cfg(test)]
mod tests {
    use super::super::lrc;
    use super::super::pick::{Hit, Want, rank};
    use super::*;

    fn hit(id: u64, track: &str, artist: &str, dur: f64) -> Hit {
        let l = lrc::parse(&format!("[00:10.00]line {id}\n[00:20.00]more {id}"));
        Hit { id, track: track.into(), artist: artist.into(), album: "x".into(), duration_s: dur, found: Found::Synced(Arc::new(l)), span_ms: 20_000 }
    }

    /// A lookup of Song / Artist / 200 s: ids 1 and 2 strict, 3 and 4 loose.
    fn lookup(searched: bool) -> Lookup {
        let hits = [hit(1, "Song", "Artist", 200.0), hit(2, "Song", "Artist", 202.0), hit(3, "Other", "Artist", 200.0), hit(4, "Song", "Cover Band", 200.0)];
        Lookup { cands: rank(&hits, &Want::new("Song", "Artist", 200)), searched }
    }

    fn shown(v: &View) -> Option<u64> {
        v.shown().map(Candidate::id)
    }

    #[test]
    fn moku_shows_the_best_strict_candidate_and_never_a_loose_one_by_itself() {
        let v = View::new(lookup(true), None);
        assert_eq!(shown(&v), Some(1));
        assert_eq!((v.is_user_choice(), v.offset_ms(), v.button()), (false, 0, Button::WrongSong));
        assert_eq!(v.caption().as_deref(), Some("Artist \u{2013} Song"));
        assert_eq!(v.entry(), None, "nothing to remember about Moku's own pick");
        // Only loose candidates: nothing is shown, but the user may try them.
        let loose = Lookup { cands: lookup(true).cands.into_iter().filter(|c| !c.strict).collect(), searched: true };
        let v = View::new(loose, None);
        assert_eq!((shown(&v), v.found().clone(), v.button()), (None, Found::Missing, Button::TryOther));
        let none = View::new(Lookup { cands: vec![], searched: true }, None);
        assert_eq!((shown(&none), none.button()), (None, Button::None));
    }

    #[test]
    fn wrong_song_walks_the_candidates_then_runs_out_and_can_start_over() {
        let mut v = View::new(lookup(true), None);
        assert_eq!(v.next(), Next::Shown);
        assert_eq!((shown(&v), v.is_user_choice()), (Some(2), true));
        assert_eq!(v.next(), Next::Shown);
        assert_eq!(shown(&v), Some(4), "loose candidates follow the strict ones: the same title by another artist first");
        assert_eq!(v.next(), Next::Shown);
        assert_eq!(shown(&v), Some(3), "then another song by the same artist");
        assert_eq!(v.next(), Next::Exhausted);
        assert_eq!((shown(&v), v.found().clone(), v.button()), (None, Found::Missing, Button::Restart));
        assert_eq!(v.entry(), Some(Entry { id: None, rejected: vec![1, 2, 4, 3], offset_ms: 0, user: true }));
        // "Coba lagi dari awal": Moku's pick again, no rejections.
        assert_eq!(v.restart(), Next::Shown);
        assert_eq!((shown(&v), v.is_user_choice(), v.button()), (Some(1), false, Button::WrongSong));
        assert_eq!(v.entry(), None);
    }

    #[test]
    fn pressing_it_with_nothing_shown_tries_the_next_loose_candidate_without_rejecting_anything() {
        let loose = Lookup { cands: lookup(true).cands.into_iter().filter(|c| !c.strict).collect(), searched: true };
        let mut v = View::new(loose, None);
        assert_eq!(v.next(), Next::Shown);
        assert_eq!((shown(&v), v.is_user_choice()), (Some(4), true));
        assert_eq!(v.entry().unwrap().rejected, Vec::<u64>::new());
    }

    #[test]
    fn a_remembered_choice_is_shown_with_its_rejections_and_offset() {
        let mem = Entry { id: Some(3), rejected: vec![1, 2], offset_ms: 1500, user: true };
        let v = View::new(lookup(true), Some(&mem));
        assert_eq!((shown(&v), v.offset_ms(), v.is_user_choice()), (Some(3), 1500, true));
        assert_eq!(v.entry(), Some(mem.clone()));
        // The shown lyrics carry the offset: the first line at 10 s is at 11,5 s.
        assert_eq!(v.lyrics().unwrap().lines[0].t, 11_500);
        // The remembered entry is gone from LRCLIB: Moku's own pick, the offset stays.
        let mem = Entry { id: Some(99), rejected: vec![1], offset_ms: -500, user: true };
        let v = View::new(lookup(true), Some(&mem));
        assert_eq!((shown(&v), v.offset_ms(), v.is_user_choice()), (Some(2), -500, false));
        // "None fits" is remembered as such, and nothing is fetched to show.
        let mem = Entry { id: None, rejected: vec![1, 2, 3, 4], offset_ms: 0, user: true };
        let v = View::new(Lookup { cands: vec![], searched: false }, Some(&mem));
        assert_eq!((shown(&v), v.button(), v.is_user_choice()), (None, Button::Restart, false));
    }

    #[test]
    fn a_choice_fetched_by_id_alone_asks_for_the_searches_when_the_user_wants_another() {
        let chosen = Candidate::chosen(hit(2, "Song", "Artist", 202.0));
        let mem = Entry { id: Some(2), rejected: vec![1], offset_ms: 0, user: true };
        let mut v = View::new(Lookup { cands: vec![chosen], searched: false }, Some(&mem));
        assert_eq!(shown(&v), Some(2));
        assert_eq!(v.next(), Next::NeedSearch);
        assert_eq!(shown(&v), None);
        // The searches found 1 (rejected before), 2 (rejected now) and two more.
        let more = lookup(true).cands;
        assert_eq!(v.add(more), Next::Shown);
        assert_eq!(shown(&v), Some(4), "1 and 2 stay rejected; the best loose one is next");
        // And a restart waiting for the searches shows Moku's own pick afterwards.
        let mem = Entry { id: None, rejected: vec![1, 2], offset_ms: 0, user: true };
        let mut v = View::new(Lookup { cands: vec![], searched: false }, Some(&mem));
        assert_eq!(v.restart(), Next::NeedSearch);
        assert_eq!(v.add(lookup(true).cands), Next::Shown);
        assert_eq!((shown(&v), v.is_user_choice(), v.entry()), (Some(1), false, None));
        // Nothing new from the searches: exhausted.
        let mem = Entry { id: Some(2), rejected: vec![1], offset_ms: 0, user: true };
        let mut v = View::new(Lookup { cands: vec![Candidate::chosen(hit(2, "Song", "Artist", 202.0))], searched: false }, Some(&mem));
        assert_eq!(v.next(), Next::NeedSearch);
        assert_eq!(v.add(vec![]), Next::Exhausted);
        assert_eq!(v.button(), Button::Restart);
    }

    #[test]
    fn the_offset_moves_the_lyrics_within_five_seconds() {
        let mut v = View::new(lookup(true), None);
        assert_eq!(v.lyrics().unwrap().lines[0].t, 10_000);
        assert!(v.shift(500));
        assert_eq!((v.offset_ms(), v.lyrics().unwrap().lines[0].t), (500, 10_500));
        assert_eq!(v.entry(), Some(Entry { id: Some(1), rejected: vec![], offset_ms: 500, user: false }), "an offset pins Moku's pick but it is not 'pilihanmu'");
        assert!(v.shift(-1500));
        assert_eq!(v.lyrics().unwrap().lines[0].t, 9_000);
        for _ in 0..20 {
            v.shift(-500);
        }
        assert_eq!((v.offset_ms(), v.lyrics().unwrap().lines[0].t), (-5000, 5_000));
        assert!(!v.shift(-500), "at the limit nothing changes");
        for _ in 0..30 {
            v.shift(500);
        }
        assert_eq!(v.offset_ms(), 5000);
        // Back to zero: Moku's own pick again, nothing to remember.
        for _ in 0..10 {
            v.shift(-500);
        }
        assert_eq!((v.offset_ms(), v.entry()), (0, None));
        // Nothing shown, nothing to shift.
        let mut none = View::new(Lookup { cands: vec![], searched: true }, None);
        assert!(!none.shift(500));
    }

    #[test]
    fn instrumentals_have_no_offset_and_show_as_such() {
        let mut h = hit(5, "Song", "Artist", 200.0);
        h.found = Found::Instrumental;
        h.span_ms = 0;
        let mut v = View::new(Lookup { cands: rank(&[h], &Want::new("Song", "Artist", 200)), searched: true }, None);
        assert_eq!(v.found(), &Found::Instrumental);
        assert!(!v.shift(500) && v.lyrics().is_none());
        assert_eq!(v.button(), Button::WrongSong);
    }

    #[test]
    fn the_caption_is_artist_dash_title_as_lrclib_has_it() {
        let mut h = hit(5, "because,", "Crayon Case", 232.0);
        let v = |h: &Hit| View::new(Lookup { cands: vec![Candidate::chosen(h.clone())], searched: true }, None).caption();
        assert_eq!(v(&h).as_deref(), Some("Crayon Case \u{2013} because,"));
        h.artist = String::new();
        assert_eq!(v(&h).as_deref(), Some("because,"));
        h.artist = "A".into();
        h.track = " ".into();
        assert_eq!(v(&h).as_deref(), Some("A"));
    }
}
