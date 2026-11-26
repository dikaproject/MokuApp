//! Feature `lyrics` — Task 2 (track A).
//!
//! Synced lyrics from LRCLIB for the track the `media` feature shows: fetched on a service
//! thread (only while `media.lyrics`), cached in memory (≤ 20 tracks), current line from the
//! extrapolated timeline. A one-shot timer fires at the next line change, only while
//! something showing lyrics is visible (the Musik tab, or the pill with `lyrics_in_pill`).

pub mod lrc;
pub mod lrclib;
pub mod title;
pub mod words;

use std::collections::VecDeque;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use lrclib::{Found, Query};

use crate::app::{App, Redraw, TimerId, Ui};
use crate::features::media::inbox::Inbox;
use crate::features::media::{self, MediaState};
use crate::registry::*;
use crate::services::ServiceCtx;

/// Tracks kept in the memory cache.
pub const CACHE_MAX: usize = 20;
/// Wait this long after a track change before asking LRCLIB (skipping through tracks
/// shouldn't fire a request per track; the duration usually arrives meanwhile).
const DEBOUNCE: Duration = Duration::from_millis(700);

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Lyrics off, or no track.
    Off,
    Loading,
    Ready(Found),
    /// Network trouble.
    Failed,
}

pub struct LyricsState {
    key: Option<String>,
    pub status: Status,
    cache: VecDeque<(String, Found)>,
    timer: Option<TimerId>,
    debounce: Option<TimerId>,
    /// The lookup for `key` was sent to the service.
    requested: bool,
}

impl Default for LyricsState {
    fn default() -> Self {
        LyricsState { key: None, status: Status::Off, cache: VecDeque::new(), timer: None, debounce: None, requested: false }
    }
}

impl LyricsState {
    fn cached(&mut self, key: &str) -> Option<Found> {
        let i = self.cache.iter().position(|(k, _)| k == key)?;
        let e = self.cache.remove(i)?;
        let f = e.1.clone();
        self.cache.push_back(e);
        Some(f)
    }

    fn remember(&mut self, key: String, f: Found) {
        self.cache.retain(|(k, _)| *k != key);
        self.cache.push_back((key, f));
        while self.cache.len() > CACHE_MAX {
            self.cache.pop_front();
        }
    }

    /// Synced lyrics of the current track, if any.
    pub fn synced(&self) -> Option<&Arc<lrc::Lyrics>> {
        match &self.status {
            Status::Ready(Found::Synced(l)) => Some(l),
            _ => None,
        }
    }
}

static INBOX: LazyLock<Inbox<Query>> = LazyLock::new(Inbox::new);

pub fn register(r: &mut Registry) {
    r.add_state(LyricsState::default());
    r.add_service(ServiceSpec { name: "lyrics", enabled: |c| c.media.enabled && c.media.lyrics, run: service });
    r.on_config(|app| {
        // Turning lyrics on/off takes effect for the current track right away.
        let on = app.config().media.lyrics;
        let off = app.state::<LyricsState>().status == Status::Off;
        if on == off {
            track_changed(app);
        }
        ensure_requested(app);
        reschedule(app);
    });
    r.on_event(|app, e| {
        if matches!(e, AppEvent::TabShown(_) | AppEvent::TabHidden(_) | AppEvent::PanelClosed | AppEvent::Visibility(_)) {
            ensure_requested(app);
            reschedule(app);
        }
    });
}

/// Something shows lyrics right now: the Musik tab, or the pill in lyric mode.
fn wanted(app: &App) -> bool {
    app.is_tab_visible("media") || (app.config().media.lyrics_in_pill && app.has_activity(media::PILL_ID))
}

/// Ask LRCLIB only once lyrics are wanted: no network (and no TLS stack in memory) for
/// tracks nobody looks at the lyrics of.
fn ensure_requested(app: &mut App) {
    let st = app.state::<LyricsState>();
    if st.status == Status::Loading && !st.requested && st.debounce.is_none() && wanted(app) {
        let id = app.after(DEBOUNCE, request);
        app.state_mut::<LyricsState>().debounce = Some(id);
    }
}

/// Called by `media` when the track identity changed (or the session went away).
pub fn track_changed(app: &mut App) {
    let key = app.state::<MediaState>().m.track.as_ref().filter(|t| !t.title.is_empty()).map(|t| t.key());
    let enabled = app.config().media.enabled && app.config().media.lyrics;
    let st = app.state_mut::<LyricsState>();
    if let Some(d) = st.debounce.take() {
        app.cancel_timer(d);
    }
    let st = app.state_mut::<LyricsState>();
    st.key = key.clone();
    st.requested = false;
    st.status = match (&key, enabled) {
        (Some(k), true) => match st.cached(k) {
            Some(f) => Status::Ready(f),
            None => Status::Loading,
        },
        _ => Status::Off,
    };
    ensure_requested(app);
    reschedule(app);
}

/// Debounce fired: send the lookup to the service.
fn request(app: &mut App) {
    let st = app.state_mut::<LyricsState>();
    st.debounce = None;
    st.requested = true;
    let m = &app.state::<MediaState>().m;
    let Some(t) = m.track.as_ref() else { return };
    let q = Query { key: t.key(), title: t.title.clone(), artist: t.artist.clone(), album: t.album.clone(), duration_s: m.timeline.dur_ms / 1000 };
    if app.state::<LyricsState>().key.as_deref() == Some(q.key.as_str()) {
        INBOX.push(q);
    }
}

/// Result from the service.
fn on_result(app: &mut App, key: String, r: Result<Found, String>) {
    let st = app.state_mut::<LyricsState>();
    let current = st.key.as_deref() == Some(key.as_str());
    match r {
        Ok(f) => {
            st.remember(key, f.clone());
            if current {
                st.status = Status::Ready(f);
            }
        }
        Err(e) => {
            log_warn!("lyrics: lookup failed: {e}");
            if current {
                st.status = Status::Failed;
            }
        }
    }
    if current {
        app.request_redraw(Redraw::All);
        reschedule(app);
    }}

/// (Re)arm the one-shot timer for the next line change — only while lyrics are visible and
/// the music plays.
pub fn reschedule(app: &mut App) {
    if let Some(t) = app.state_mut::<LyricsState>().timer.take() {
        app.cancel_timer(t);
    }
    // The pill case counts while the music pill exists, even if an OSD covers it for a moment
    // (nothing would re-arm the timer when the OSD expires otherwise).
    // Nothing is drawn while the island is hidden: no timer either (re-armed on `Visibility`).
    let visible = !app.is_hidden() && (app.is_tab_visible("media") || (app.config().media.lyrics_in_pill && app.has_activity(media::PILL_ID)));
    let m = &app.state::<MediaState>().m;
    if !visible || !m.playing {
        return;
    }
    let pos = m.position_ms(Instant::now());
    let Some(next) = app.state::<LyricsState>().synced().and_then(|l| l.next_change(pos)) else { return };
    let wait = Duration::from_millis(next.saturating_sub(pos) + 30);
    let id = app.after(wait, |app| {
        app.state_mut::<LyricsState>().timer = None;
        app.request_redraw(Redraw::All);
        reschedule(app);
    });
    app.state_mut::<LyricsState>().timer = Some(id);
}

/// Lines around `pos` for display: (previous, current, next) as (start ms, text).
pub type Around = (Option<(u64, String)>, Option<(u64, String)>, Option<(u64, String)>);

pub fn around(l: &lrc::Lyrics, pos_ms: u64) -> Around {
    let get = |i: usize| l.lines.get(i).map(|x| (x.t, x.text.clone()));
    match l.line_at(pos_ms) {
        Some(i) => (i.checked_sub(1).and_then(get), get(i), get(i + 1)),
        None => (None, None, get(0)),
    }
}

/// The line being sung now, with the times of its words.
#[derive(Clone, Debug, PartialEq)]
pub struct Current {
    pub idx: usize,
    pub t: u64,
    pub text: String,
    /// Empty when there is nothing to light up (an instrumental "♪" line, right-to-left text).
    pub words: Vec<crate::ui::WordSpan>,
}

/// The line sung at `pos_ms`.
pub fn current(l: &lrc::Lyrics, pos_ms: u64) -> Option<Current> {
    let idx = l.line_at(pos_ms)?;
    let line = &l.lines[idx];
    let next = l.lines.get(idx + 1).map(|n| n.t);
    let words = if line.text.is_empty() || words::is_rtl(&line.text) { Vec::new() } else { words::spans(line, next) };
    Some(Current { idx, t: line.t, text: line.text.clone(), words })
}

/// For the pill (lyric mode): the sung line and the next one, if there is a sung line now.
pub fn pill_lines(app: &App) -> Option<(Current, String)> {
    if !app.config().media.lyrics_in_pill {
        return None;
    }
    let l = app.state::<LyricsState>().synced()?;
    let pos = app.state::<MediaState>().m.position_ms(Instant::now());
    let cur = current(l, pos).filter(|c| !c.text.is_empty())?;
    let next = l.lines.get(cur.idx + 1).map(|n| n.text.clone()).unwrap_or_default();
    Some((cur, next))
}

/// Identity of a sung line for the highlight (track + line): a sweep keeps running while it is
/// the same.
pub fn line_id(app: &App, idx: usize) -> String {
    format!("{}#{idx}", app.state::<LyricsState>().key.as_deref().unwrap_or(""))
}

/// Sample lyrics for the `media.sample` demo path (feeds the same state).
pub fn inject_sample(app: &mut App, key: String, src: &str) {
    let f = Found::Synced(Arc::new(lrc::parse(src)));
    let st = app.state_mut::<LyricsState>();
    st.remember(key.clone(), f.clone());
    st.key = Some(key);
    st.status = Status::Ready(f);
    reschedule(app);
}

fn service(ctx: ServiceCtx) {
    INBOX.clear();
    while let Some(mut batch) = INBOX.wait(&ctx) {
        // Only the newest request matters.
        let Some(q) = batch.pop() else { continue };
        let r = lrclib::fetch(&q);
        if ctx.should_stop() {
            return;
        }
        let key = q.key;
        Ui::post(move |app| on_result(app, key, r));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_is_lru_and_bounded() {
        let mut st = LyricsState::default();
        for i in 0..25 {
            st.remember(format!("k{i}"), Found::Missing);
        }
        assert_eq!(st.cache.len(), CACHE_MAX);
        assert!(st.cached("k0").is_none());
        assert!(st.cached("k5").is_some());
        // k5 is now the most recent; adding one more drops k6, not k5.
        st.remember("new".into(), Found::Instrumental);
        assert!(st.cached("k5").is_some());
        assert!(st.cached("k6").is_none());
    }

    #[test]
    fn the_sung_line_knows_its_words() {
        let l = lrc::parse("[00:01.00]lampu kota memudar
[00:05.00]
[00:07.00]oh yeah
[00:12.00]نص عربي");
        assert!(current(&l, 500).is_none(), "nothing is sung before the first line");
        let c = current(&l, 2000).unwrap();
        assert_eq!((c.idx, c.t, c.text.as_str()), (0, 1000, "lampu kota memudar"));
        let w: Vec<&str> = c.words.iter().map(|w| &c.text[w.from..w.to]).collect();
        assert_eq!(w, ["lampu", "kota", "memudar"]);
        // The estimate fills the 4 s slot to the next line, minus the tail.
        assert!(c.words.last().unwrap().end_ms <= 5000 - words::TAIL_MS);
        assert_eq!(c.words[0].start_ms, 1000);
        // A break has nothing to light up; a right-to-left line is not swept either.
        assert!(current(&l, 5500).unwrap().words.is_empty());
        assert!(current(&l, 13_000).unwrap().words.is_empty());
        // The last line gets the default slot.
        let c = current(&l, 8000).unwrap();
        assert_eq!(c.idx, 2);
        assert!(c.words.last().unwrap().end_ms <= 7000 + 5000);
    }

    #[test]
    fn real_word_times_win_over_the_estimate() {
        let l = lrc::parse("[00:10.00]<00:10.00>one <00:11.00>two <00:11.50>three<00:12.00>
[00:20.00]x");
        let c = current(&l, 10_500).unwrap();
        let t: Vec<(u64, u64)> = c.words.iter().map(|w| (w.start_ms, w.end_ms)).collect();
        assert_eq!(t, [(10_000, 11_000), (11_000, 11_500), (11_500, 12_000)]);
    }

    #[test]
    fn around_lines() {
        let l = lrc::parse("[00:01.00]a\n[00:02.00]b\n[00:03.00]c");
        assert_eq!(around(&l, 0), (None, None, Some((1000, "a".into()))));
        assert_eq!(around(&l, 2500), (Some((1000, "a".into())), Some((2000, "b".into())), Some((3000, "c".into()))));
        assert_eq!(around(&l, 9000).2, None);
    }
}
