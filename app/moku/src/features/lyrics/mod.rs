//! Feature `lyrics` — Task 2 (track A).
//!
//! Synced lyrics from LRCLIB for the track the `media` feature shows: fetched on a service
//! thread (only while `media.lyrics`), cached in memory (≤ 20 tracks), current line from the
//! extrapolated timeline. A one-shot timer fires at the next line change, only while
//! something showing lyrics is visible (the Musik tab, or the pill with `lyrics_in_pill`).
//!
//! A lookup keeps the few entries that fit the track ([`pick`]), shows the best one that is
//! certainly the right song, and lets the user correct it: "Bukan lagu ini" moves on to the next
//! candidate, the timing can be nudged, and both are remembered per track on this PC only
//! ([`choices`], `lyrics-choices.json`). [`view`] holds that state; [`LyricsState::handle`] is the
//! pure core of the buttons.

pub mod choices;
pub mod links;
pub mod lrc;
pub mod lrclib;
pub mod pick;
pub mod title;
pub mod view;
pub mod words;
#[cfg(test)]
mod fixture_tests;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use choices::{Entry, OFFSET_STEP_MS, Store};
use lrclib::{Found, Lookup, Query};
use pick::Candidate;
use view::{Button, Next, View};

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
/// Key of the debounced save of the choices file.
const SAVE_KEY: &str = "lyrics.choices";

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Lyrics off, or no track.
    Off,
    Loading,
    Ready(Found),
    /// Network trouble.
    Failed,
}

/// A track's view with the keys its choices are remembered under.
#[derive(Clone, Debug, PartialEq)]
struct Slot {
    view: View,
    keys: Vec<String>,
}

pub struct LyricsState {
    key: Option<String>,
    pub status: Status,
    cache: VecDeque<(String, Slot)>,
    /// The current track's candidates and the user's corrections.
    slot: Option<Slot>,
    timer: Option<TimerId>,
    debounce: Option<TimerId>,
    /// The lookup for `key` was sent to the service.
    requested: bool,
    /// What the user decided about tracks, loaded on first use.
    store: Option<Store>,
    /// The searches for "Bukan lagu ini" were sent and are not answered yet.
    searching: bool,
}

impl Default for LyricsState {
    fn default() -> Self {
        LyricsState { key: None, status: Status::Off, cache: VecDeque::new(), slot: None, timer: None, debounce: None, requested: false, store: None, searching: false }
    }
}

/// What a press of one of the lyric buttons asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// "Bukan lagu ini" / "Coba kecocokan lain" / "Coba lagi dari awal" (whichever the view offers).
    Next,
    /// Move the lyrics by this many ms (positive = later).
    Shift(i32),
    Web(links::Site),
}

/// The action → command mapping (`media.lyrics_next`, `media.lyrics_earlier`, `media.lyrics_later`,
/// `media.lyrics_web:genius`).
pub fn command_of(name: &str, arg: Option<&str>) -> Option<Cmd> {
    match name {
        "lyrics_next" => Some(Cmd::Next),
        "lyrics_earlier" => Some(Cmd::Shift(-OFFSET_STEP_MS)),
        "lyrics_later" => Some(Cmd::Shift(OFFSET_STEP_MS)),
        "lyrics_web" => arg.and_then(links::Site::of).map(Cmd::Web),
        _ => None,
    }
}

/// What the caller has to do after [`LyricsState::handle`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Effects {
    pub redraw: bool,
    /// The choices changed: write the file (debounced).
    pub save: bool,
    /// Ask the service for the entries of the searches ("Bukan lagu ini" ran out of them).
    pub search: bool,
    /// Open this address in the user's browser.
    pub open: Option<String>,
}

/// What the Musik panel shows under the lyrics.
#[derive(Clone, Debug, PartialEq)]
pub struct Footer {
    /// "Artist – Title" of the shown entry as LRCLIB has it; `None` when no entry is shown.
    pub caption: Option<String>,
    /// The shown entry is the user's own choice ("pilihanmu").
    pub user: bool,
    pub offset_ms: i32,
    /// Synced lyrics are shown, so their timing can be nudged.
    pub can_shift: bool,
    pub button: Button,
    /// The track can be searched for on the web (it has a title).
    pub web: bool,
}

impl LyricsState {
    fn cached(&mut self, key: &str) -> Option<Slot> {
        let i = self.cache.iter().position(|(k, _)| k == key)?;
        let e = self.cache.remove(i)?;
        let s = e.1.clone();
        self.cache.push_back(e);
        Some(s)
    }

    fn remember(&mut self, key: String, s: Slot) {
        self.cache.retain(|(k, _)| *k != key);
        self.cache.push_back((key, s));
        while self.cache.len() > CACHE_MAX {
            self.cache.pop_front();
        }
    }

    /// Synced lyrics of the current track, if any (the user's offset already applied).
    pub fn synced(&self) -> Option<&Arc<lrc::Lyrics>> {
        match &self.status {
            Status::Ready(Found::Synced(l)) => Some(l),
            _ => None,
        }
    }

    /// Make `slot` the current track's lookup result.
    fn install(&mut self, slot: Slot) {
        self.status = Status::Ready(slot.view.found().clone());
        self.slot = Some(slot);
        self.searching = false;
    }

    /// What is shown under the lyric block (nothing but the web search while there is no lookup
    /// result yet: loading, failed).
    pub fn footer(&self, has_title: bool) -> Footer {
        match self.slot.as_ref().map(|s| &s.view) {
            Some(v) => Footer { caption: v.caption(), user: v.is_user_choice(), offset_ms: v.offset_ms(), can_shift: v.lyrics().is_some() && !self.searching, button: if self.searching { Button::None } else { v.button() }, web: has_title },
            None => Footer { caption: None, user: false, offset_ms: 0, can_shift: false, button: Button::None, web: has_title },
        }
    }

    /// Load the choices file once (call before anything reads or writes the store).
    fn ensure_store(&mut self) {
        if self.store.is_none() {
            self.store = Some(Store::load(&choices_path()));
        }
    }

    /// Write the current view's choices into the store (or forget them when they are back to what
    /// Moku would pick by itself).
    fn persist(&mut self) {
        let (Some(slot), Some(store)) = (self.slot.as_ref(), self.store.as_mut()) else { return };
        let Some(primary) = slot.keys.first() else { return };
        for k in &slot.keys {
            store.remove(k);
        }
        if let Some(e) = slot.view.entry() {
            store.put(primary, e);
        }
    }

    /// The pure core of the lyric buttons: change the view, the store and the status, and say what
    /// the caller has to do. `artist` / `title` are the player's, for the web search.
    pub fn handle(&mut self, cmd: Cmd, artist: &str, title: &str) -> Effects {
        let mut fx = Effects::default();
        if let Cmd::Web(site) = cmd {
            fx.open = links::url(site, artist, title);
            return fx;
        }
        if self.searching {
            return fx;
        }
        let Some(slot) = self.slot.as_mut() else { return fx };
        let changed = match cmd {
            Cmd::Shift(d) => slot.view.shift(d),
            Cmd::Next => {
                let next = match slot.view.button() {
                    Button::WrongSong | Button::TryOther => slot.view.next(),
                    Button::Restart => slot.view.restart(),
                    Button::None => return fx,
                };
                fx.search = next == Next::NeedSearch;
                true
            }
            Cmd::Web(_) => false,
        };
        if !changed {
            return fx;
        }
        fx.redraw = true;
        fx.save = true;
        self.searching = fx.search;
        self.status = if fx.search { Status::Loading } else { Status::Ready(self.slot.as_ref().map(|s| s.view.found().clone()).unwrap_or(Found::Missing)) };
        self.persist();
        if let (Some(k), Some(s)) = (self.key.clone(), self.slot.clone()) {
            self.remember(k, s);
        }
        fx
    }

    /// The searches asked for by [`Effects::search`] came back.
    fn alternatives_arrived(&mut self, cands: Vec<Candidate>) {
        self.searching = false;
        if let Some(slot) = self.slot.as_mut() {
            slot.view.add(cands);
            self.status = Status::Ready(slot.view.found().clone());
        }
        self.persist();
        if let (Some(k), Some(s)) = (self.key.clone(), self.slot.clone()) {
            self.remember(k, s);
        }
    }

    /// "Hapus semua data": nothing the user decided is kept, in memory or on disk.
    fn forget_choices(&mut self) {
        self.store = Some(Store::default());
        self.cache.clear();
        self.slot = None;
        self.searching = false;
    }
}

fn choices_path() -> PathBuf {
    moku_core::paths::data_file("lyrics-choices.json")
}

/// A request to the service thread.
enum Job {
    Lookup { q: Query, keys: Vec<String>, remembered: Option<u64>, rejected: Vec<u64> },
    /// The searches, for "Bukan lagu ini" after a lookup that was not a search.
    Alternatives { q: Query, rejected: Vec<u64> },
}

static INBOX: LazyLock<Inbox<Job>> = LazyLock::new(Inbox::new);

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
        if matches!(e, AppEvent::DataCleared) {
            app.cancel_save(SAVE_KEY);
            app.state_mut::<LyricsState>().forget_choices();
            track_changed(app);
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
    if st.status == Status::Loading && !st.requested && !st.searching && st.debounce.is_none() && wanted(app) {
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
    st.searching = false;
    st.slot = None;
    st.status = match (&key, enabled) {
        (Some(k), true) => match st.cached(k) {
            Some(slot) => {
                st.install(slot);
                st.status.clone()
            }
            None => Status::Loading,
        },
        _ => Status::Off,
    };
    ensure_requested(app);
    reschedule(app);
}

/// The lookup of the current track (what the player reports of it).
fn query_of(app: &App) -> Option<Query> {
    let m = &app.state::<MediaState>().m;
    let t = m.track.as_ref()?;
    Some(Query { key: t.key(), title: t.title.clone(), artist: t.artist.clone(), album: t.album.clone(), duration_s: m.timeline.dur_ms / 1000 })
}

/// Debounce fired: send the lookup to the service (or, for a track the user said has no lyrics,
/// answer at once).
fn request(app: &mut App) {
    let st = app.state_mut::<LyricsState>();
    st.debounce = None;
    st.requested = true;
    let Some(q) = query_of(app) else { return };
    let st = app.state_mut::<LyricsState>();
    if st.key.as_deref() != Some(q.key.as_str()) {
        return;
    }
    st.ensure_store();
    let keys = choices::keys(&q.artist, &q.title, q.duration_s);
    let memory = st.store.as_ref().and_then(|s| s.get(&keys)).cloned();
    match memory {
        // "None of them": nothing is asked of the network.
        Some(Entry { id: None, .. }) => {
            let slot = Slot { view: View::new(Lookup { cands: vec![], searched: false }, memory.as_ref()), keys };
            st.remember(q.key.clone(), slot.clone());
            st.install(slot);
            app.request_redraw(Redraw::All);
            reschedule(app);
        }
        m => {
            let (remembered, rejected) = m.map(|e| (e.id, e.rejected)).unwrap_or_default();
            INBOX.push(Job::Lookup { q, keys, remembered, rejected });
        }
    }
}

/// Result of a lookup from the service.
fn on_result(app: &mut App, key: String, keys: Vec<String>, r: Result<Lookup, String>) {
    let st = app.state_mut::<LyricsState>();
    let current = st.key.as_deref() == Some(key.as_str());
    match r {
        Ok(lookup) => {
            st.ensure_store();
            let memory = st.store.as_ref().and_then(|s| s.get(&keys)).cloned();
            let slot = Slot { view: View::new(lookup, memory.as_ref()), keys };
            st.remember(key, slot.clone());
            if current {
                st.install(slot);
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
    }
}

/// Result of the searches asked for by "Bukan lagu ini".
fn on_alternatives(app: &mut App, key: String, r: Result<Vec<Candidate>, String>) {
    let st = app.state_mut::<LyricsState>();
    if st.key.as_deref() != Some(key.as_str()) {
        return;
    }
    match r {
        Ok(c) => st.alternatives_arrived(c),
        Err(e) => {
            log_warn!("lyrics: search failed: {e}");
            // Nothing new: the view stays as it was, with nothing shown, and the button asks again.
            st.alternatives_arrived(vec![]);
        }
    }
    app.save_later(SAVE_KEY, snapshot);
    app.request_redraw(Redraw::All);
    reschedule(app);
}

fn snapshot(app: &App) -> Option<(PathBuf, Vec<u8>)> {
    app.state::<LyricsState>().store.as_ref().map(|s| (choices_path(), s.to_json()))
}

/// A lyric button of the Musik panel (`media.lyrics_next`, ...): the UI thread side of
/// [`LyricsState::handle`].
pub fn on_action(app: &mut App, name: &str, arg: Option<&str>) {
    let Some(cmd) = command_of(name, arg) else { return };
    let (artist, title) = app.state::<MediaState>().m.track.as_ref().map(|t| (t.artist.clone(), t.title.clone())).unwrap_or_default();
    app.state_mut::<LyricsState>().ensure_store();
    let fx = app.state_mut::<LyricsState>().handle(cmd, &artist, &title);
    if fx.search {
        let rejected = app.state::<LyricsState>().slot.as_ref().map(|s| s.view.rejected().to_vec()).unwrap_or_default();
        if let Some(q) = query_of(app) {
            INBOX.push(Job::Alternatives { q, rejected });
        }
    }
    if fx.save {
        app.save_later(SAVE_KEY, snapshot);
    }
    if let Some(u) = fx.open {
        if !links::open(&u) {
            log_warn!("lyrics: could not open the search page");
        }
    }
    if fx.redraw {
        app.request_redraw(Redraw::All);
        reschedule(app);
    }
}

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

/// Identity of a sung line for the highlight (track + shown entry + offset + line): a sweep keeps
/// running while it is the same, and starts afresh when the lyrics or their timing changed.
pub fn line_id(app: &App, idx: usize) -> String {
    let st = app.state::<LyricsState>();
    let (id, off) = st.slot.as_ref().map(|s| (s.view.shown_id(), s.view.offset_ms())).unwrap_or((0, 0));
    format!("{}#{id}#{off}#{idx}", st.key.as_deref().unwrap_or(""))
}

/// How the demo's lyrics look in `--state lyrics-panel` (`MOKU_DEMO_LYRICS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemoMode {
    /// Moku's own pick.
    Auto,
    /// The user's own pick, nudged by +1,0 s.
    Chosen,
    /// Everything rejected.
    None,
}

impl DemoMode {
    pub fn of(s: &str) -> DemoMode {
        match s {
            "chosen" | "user" => DemoMode::Chosen,
            "none" => DemoMode::None,
            _ => DemoMode::Auto,
        }
    }
}

/// A fake LRCLIB entry for the demos.
fn demo_hit(id: u64, track: &str, artist: &str, album: &str, dur: f64, l: &Arc<lrc::Lyrics>) -> pick::Hit {
    pick::Hit { id, track: track.into(), artist: artist.into(), album: album.into(), duration_s: dur, found: Found::Synced(l.clone()), span_ms: l.lines.last().map(|x| x.t).unwrap_or(0) }
}

/// Sample lyrics for the `media.sample` demo path and `--state lyrics` (feeds the same state, with
/// a few made-up candidates so the caption and the buttons have something to show).
pub fn inject_sample(app: &mut App, key: String, src: &str, mode: DemoMode) {
    let l = Arc::new(lrc::parse(src));
    let mk = |id, track: &str, artist: &str, dur: f64, strict: bool, score: i32| Candidate { hit: demo_hit(id, track, artist, "Neon Atlas", dur, &l), strict, score };
    // Different times per candidate, so they are not "the same lyrics" for the list.
    let cands = vec![mk(101, "Midnight Drive", "Kaia", 227.0, true, 800), mk(102, "Midnight Drive (Live)", "Kaia", 241.0, false, 600), mk(103, "Midnight Drive", "Kaia & The Co", 227.5, false, 560)];
    let memory = match mode {
        DemoMode::Auto => None,
        DemoMode::Chosen => Some(Entry { id: Some(102), rejected: vec![101], offset_ms: 1000, user: true }),
        DemoMode::None => Some(Entry { id: None, rejected: vec![101, 102, 103], offset_ms: 0, user: true }),
    };
    let slot = Slot { view: View::new(Lookup { cands, searched: true }, memory.as_ref()), keys: choices::keys("Kaia", "Midnight Drive", 227) };
    let st = app.state_mut::<LyricsState>();
    st.store.get_or_insert_with(Store::default);
    st.remember(key.clone(), slot.clone());
    st.key = Some(key);
    st.install(slot);
    reschedule(app);
}

fn service(ctx: ServiceCtx) {
    INBOX.clear();
    while let Some(mut batch) = INBOX.wait(&ctx) {
        // Only the newest request matters.
        let Some(job) = batch.pop() else { continue };
        match job {
            Job::Lookup { q, keys, remembered, rejected } => {
                let r = lrclib::fetch(&q, remembered, &rejected);
                if ctx.should_stop() {
                    return;
                }
                let key = q.key;
                Ui::post(move |app| on_result(app, key, keys, r));
            }
            Job::Alternatives { q, rejected } => {
                let r = lrclib::fetch_alternatives(&q, &rejected);
                if ctx.should_stop() {
                    return;
                }
                let key = q.key;
                Ui::post(move |app| on_alternatives(app, key, r));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hit(id: u64, track: &str, artist: &str) -> pick::Hit {
        pick::Hit {
            id,
            track: track.into(),
            artist: artist.into(),
            album: "x".into(),
            duration_s: 200.0,
            found: Found::Synced(Arc::new(lrc::parse(&format!("[00:10.00]line {id}
[00:20.00]more")))),
            span_ms: 20_000,
        }
    }

    fn slot(strict_ids: &[u64], loose_ids: &[u64]) -> Slot {
        use pick::{Hit, Want, rank};
        let mut hits: Vec<Hit> = strict_ids.iter().map(|i| test_hit(*i, "Song", "Artist")).collect();
        hits.extend(loose_ids.iter().map(|i| test_hit(*i, "Song", "Cover Band")));
        let lookup = Lookup { cands: rank(&hits, &Want::new("Song", "Artist", 200)), searched: true };
        Slot { view: View::new(lookup, None), keys: choices::keys("Artist", "Song", 200) }
    }

    /// A state like the one a lookup leaves behind, with an empty store (never the real file).
    fn state(s: Slot) -> LyricsState {
        let mut st = LyricsState::default();
        st.store = Some(Store::default());
        st.key = Some("track".into());
        st.install(s);
        st
    }

    fn stored(st: &LyricsState) -> Option<Entry> {
        st.store.as_ref().and_then(|s| s.get(&st.slot.as_ref().unwrap().keys)).cloned()
    }

    fn first_line(st: &LyricsState) -> Option<u64> {
        st.synced().map(|l| l.lines[0].t)
    }

    #[test]
    fn cache_is_lru_and_bounded() {
        let mut st = LyricsState::default();
        for i in 0..25 {
            st.remember(format!("k{i}"), slot(&[1], &[]));
        }
        assert_eq!(st.cache.len(), CACHE_MAX);
        assert!(st.cached("k0").is_none());
        assert!(st.cached("k5").is_some());
        // k5 is now the most recent; adding one more drops k6, not k5.
        st.remember("new".into(), slot(&[2], &[]));
        assert!(st.cached("k5").is_some());
        assert!(st.cached("k6").is_none());
    }

    #[test]
    fn actions_map_to_commands() {
        assert_eq!(command_of("lyrics_next", None), Some(Cmd::Next));
        assert_eq!(command_of("lyrics_earlier", None), Some(Cmd::Shift(-500)));
        assert_eq!(command_of("lyrics_later", None), Some(Cmd::Shift(500)));
        assert_eq!(command_of("lyrics_web", Some("genius")), Some(Cmd::Web(links::Site::Genius)));
        assert_eq!(command_of("lyrics_web", Some("musixmatch")), Some(Cmd::Web(links::Site::Musixmatch)));
        for (n, a) in [("lyrics_web", None), ("lyrics_web", Some("evil")), ("lyrics", None), ("seek_ms", Some("5")), ("", None)] {
            assert_eq!(command_of(n, a), None, "{n} {a:?}");
        }
    }

    #[test]
    fn wrong_song_moves_on_remembers_and_runs_out() {
        let mut st = state(slot(&[1, 2], &[3]));
        assert_eq!(st.footer(true).button, Button::WrongSong);
        assert_eq!(first_line(&st), Some(10_000));
        // First press: candidate 2, remembered as the user's choice with 1 rejected.
        let fx = st.handle(Cmd::Next, "Artist", "Song");
        assert_eq!(fx, Effects { redraw: true, save: true, search: false, open: None });
        assert_eq!(stored(&st), Some(Entry { id: Some(2), rejected: vec![1], offset_ms: 0, user: true }));
        let f = st.footer(true);
        assert!(f.user && f.caption.as_deref() == Some("Artist \u{2013} Song"));
        // Second: the loose one (another artist), third: nothing is left.
        st.handle(Cmd::Next, "Artist", "Song");
        assert_eq!(st.footer(true).caption.as_deref(), Some("Cover Band \u{2013} Song"));
        st.handle(Cmd::Next, "Artist", "Song");
        assert_eq!(st.status, Status::Ready(Found::Missing));
        let f = st.footer(true);
        assert_eq!((f.caption, f.button, f.can_shift), (None, Button::Restart, false));
        assert_eq!(stored(&st), Some(Entry { id: None, rejected: vec![1, 2, 3], offset_ms: 0, user: true }));
        // "Coba lagi dari awal" forgets it all: Moku's own pick, nothing remembered.
        let fx = st.handle(Cmd::Next, "Artist", "Song");
        assert!(fx.redraw && fx.save && !fx.search);
        assert_eq!(first_line(&st), Some(10_000));
        assert_eq!(stored(&st), None);
        assert!(!st.footer(true).user);
        // The cache holds the corrected state, so coming back to the track keeps it.
        st.handle(Cmd::Next, "Artist", "Song");
        assert_eq!(st.cached("track").unwrap().view.entry().unwrap().id, Some(2));
    }

    #[test]
    fn the_offset_buttons_shift_the_lyrics_and_are_remembered() {
        let mut st = state(slot(&[1], &[]));
        let fx = st.handle(Cmd::Shift(500), "A", "T");
        assert!(fx.redraw && fx.save);
        // The status carries the shifted lyrics, so the highlight and the timer follow.
        assert_eq!(first_line(&st), Some(10_500));
        st.handle(Cmd::Shift(-1500), "A", "T");
        assert_eq!(first_line(&st), Some(9_000));
        assert_eq!(stored(&st), Some(Entry { id: Some(1), rejected: vec![], offset_ms: -1000, user: false }));
        let f = st.footer(true);
        assert_eq!((f.offset_ms, f.can_shift, f.user), (-1000, true, false));
        // At the limit nothing happens (no redraw, no save).
        for _ in 0..12 {
            st.handle(Cmd::Shift(-500), "A", "T");
        }
        assert_eq!(first_line(&st), Some(5_000));
        assert_eq!(st.handle(Cmd::Shift(-500), "A", "T"), Effects::default());
        // Back to zero: nothing to remember.
        for _ in 0..10 {
            st.handle(Cmd::Shift(500), "A", "T");
        }
        assert_eq!(stored(&st), None);
        assert_eq!(first_line(&st), Some(10_000));
    }

    #[test]
    fn nothing_changes_when_no_lyrics_are_shown() {
        let mut st = LyricsState::default();
        st.store = Some(Store::default());
        assert_eq!(st.handle(Cmd::Next, "A", "T"), Effects::default());
        assert_eq!(st.handle(Cmd::Shift(500), "A", "T"), Effects::default());
        let f = st.footer(true);
        assert_eq!((f.caption, f.button, f.can_shift, f.web), (None, Button::None, false, true));
        // The web search needs no lyrics, only a title.
        let fx = st.handle(Cmd::Web(links::Site::Genius), "Crayon Case", "because,");
        assert_eq!(fx.open.as_deref(), Some("https://genius.com/search?q=Crayon%20Case%20because"));
        assert!(!fx.redraw && !fx.save);
        assert_eq!(st.handle(Cmd::Web(links::Site::Musixmatch), "A", "").open, None);
    }

    #[test]
    fn a_choice_fetched_by_id_asks_for_the_searches_and_shows_the_answer() {
        // Remembered entry 2, fetched alone: its slot knows nothing else yet.
        let chosen = Candidate::chosen(slot(&[2], &[]).view.shown().unwrap().hit.clone());
        let memory = Entry { id: Some(2), rejected: vec![1], offset_ms: 500, user: true };
        let view = View::new(Lookup { cands: vec![chosen], searched: false }, Some(&memory));
        let mut st = state(Slot { view, keys: choices::keys("Artist", "Song", 200) });
        assert_eq!(first_line(&st), Some(10_500), "the remembered offset is applied");
        let fx = st.handle(Cmd::Next, "Artist", "Song");
        assert!(fx.search && fx.redraw && fx.save);
        assert_eq!(st.status, Status::Loading);
        assert_eq!(st.footer(true).button, Button::None, "no second press while the searches run");
        assert_eq!(st.handle(Cmd::Next, "Artist", "Song"), Effects::default());
        assert_eq!(st.handle(Cmd::Shift(500), "Artist", "Song"), Effects::default());
        // The searches found 1 (rejected), 2 (rejected) and 3: 3 is shown.
        let cands: Vec<Candidate> = [1u64, 2, 3].iter().map(|i| Candidate { hit: test_hit(*i, "Song", "Artist"), strict: true, score: 500 }).collect();
        st.alternatives_arrived(cands);
        assert_eq!(first_line(&st), Some(10_500), "3 is shown, with the offset remembered for the track");
        assert_eq!(stored(&st).unwrap().rejected, vec![1, 2]);
        assert!(!st.searching);
    }

    #[test]
    fn data_cleared_forgets_every_choice() {
        let mut st = state(slot(&[1, 2], &[]));
        st.handle(Cmd::Next, "A", "T");
        st.remember("other".into(), slot(&[9], &[]));
        assert!(!st.store.as_ref().unwrap().is_empty() && st.cached("other").is_some());
        st.forget_choices();
        assert!(st.store.as_ref().unwrap().is_empty());
        assert!(st.cache.is_empty() && st.slot.is_none());
    }

    #[test]
    fn the_footer_has_only_the_web_search_until_a_lookup_has_answered() {
        let mut st = LyricsState::default();
        let f = st.footer(true);
        assert_eq!((f.caption, f.button, f.can_shift, f.web), (None, Button::None, false, true));
        st.install(slot(&[1], &[]));
        let f = st.footer(false);
        assert!(!f.web, "no title, no web search");
        assert_eq!(f.button, Button::WrongSong);
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
