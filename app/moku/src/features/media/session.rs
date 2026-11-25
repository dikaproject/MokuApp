//! The `media` service: Windows media sessions (GSMTC) on a background MTA thread.
//!
//! WinRT event handlers only queue a command; this thread reads the session (blocking waits are
//! fine here, never on the UI thread — each bounded by `OP_TIMEOUT`, `wait_op!`) and posts
//! [`Update`]s to the UI thread. The session manager is requested again with backoff while it is
//! unavailable (logon). The album art is re-read on every properties event and ~1.2 s after a
//! track change, and decoded again only when its bytes changed (`ArtSig`).
//! Transport commands from the panel come through the same inbox.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Media::Control::*;
use windows::Storage::Streams::{DataReader, IInputStream, IRandomAccessStreamReference};
use windows::Win32::Foundation::ERROR_TIMEOUT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::core::Interface;

use super::inbox::Inbox;
use super::model::{Controls, Timeline, Track, Update};
use crate::app::Ui;
use crate::services::ServiceCtx;

/// Longest wait for one WinRT async call: a hung player or broker must not hang the service
/// (and with it the exit path, which joins the service).
const OP_TIMEOUT: Duration = Duration::from_secs(5);
/// Re-read the album art this long after a track change: many players publish the title first
/// and the thumbnail a moment later.
const ART_RECHECK: Duration = Duration::from_millis(1200);

/// Wait for a WinRT async operation (any `IAsyncOperation<T>` or operation class) on this MTA
/// thread, at most [`OP_TIMEOUT`]; then it is cancelled and `ERROR_TIMEOUT` returned. A macro,
/// because the async interfaces live in `windows-future`, which is not a direct dependency.
/// Polls `Status()` with a short, growing sleep (1 → 16 ms): these calls usually finish in a few
/// milliseconds, and this thread has nothing else to do meanwhile.
macro_rules! wait_op {
    ($op:expr) => {{
        let op = $op;
        let start = Instant::now();
        let mut nap = 1u64;
        loop {
            // AsyncStatus: 0 = Started, 1 = Completed, 2 = Canceled, 3 = Error.
            if op.Status()?.0 != 0 {
                break op.GetResults();
            }
            if start.elapsed() > OP_TIMEOUT {
                let _ = op.Cancel();
                break Err(windows::core::Error::from(ERROR_TIMEOUT.to_hresult()));
            }
            std::thread::sleep(Duration::from_millis(nap));
            nap = (nap * 2).min(16);
        }
    }};
}

/// What identifies the art we decoded last: the track and the thumbnail's bytes (size + FNV-1a).
/// Players often change the thumbnail without changing the title (art arriving late, a new
/// cover for the same track), so the bytes are compared on every properties event and only a
/// different picture is decoded again.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ArtSig {
    key: String,
    len: u64,
    hash: u64,
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Decode (and post) the art again? Only for a different picture — and a missing / unreadable
/// thumbnail never replaces the art already shown for the same track (a slow read is not "no art").
fn art_changed(shown: Option<&ArtSig>, new: &ArtSig) -> bool {
    match shown {
        Some(s) if s == new => false,
        Some(s) if s.key == new.key && new.len == 0 => false,
        _ => true,
    }
}

impl ArtSig {
    fn of(key: &str, bytes: &[u8]) -> ArtSig {
        ArtSig { key: key.to_string(), len: bytes.len() as u64, hash: fnv1a(bytes) }
    }
    /// The track has no (readable) thumbnail.
    fn none(key: &str) -> ArtSig {
        ArtSig { key: key.to_string(), len: 0, hash: 0 }
    }
}

/// Largest album-art edge kept in memory (px).
pub const ART_MAX_PX: u32 = 256;
/// Thumbnails bigger than this are not read.
const ART_MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    /// Re-pick the current session (session list / current session changed).
    Session,
    /// Forget the bound session and pick again from scratch (debug `media.sample_off`).
    Reset,
    Props,
    Playback,
    Timeline,
    Toggle,
    Next,
    Prev,
    /// Seek to this position (ms).
    Seek(u64),
}

pub static INBOX: LazyLock<Inbox<Cmd>> = LazyLock::new(Inbox::new);

type Session = GlobalSystemMediaTransportControlsSession;

/// A session we listen to; handlers are removed on drop.
struct Bound {
    s: Session,
    app: String,
    tokens: [i64; 3],
}

impl Drop for Bound {
    fn drop(&mut self) {
        let _ = self.s.RemoveMediaPropertiesChanged(self.tokens[0]);
        let _ = self.s.RemovePlaybackInfoChanged(self.tokens[1]);
        let _ = self.s.RemoveTimelinePropertiesChanged(self.tokens[2]);
    }
}

fn bind(s: Session) -> windows::core::Result<Bound> {
    let app = s.SourceAppUserModelId().map(|h| h.to_string_lossy()).unwrap_or_default();
    let t0 = s.MediaPropertiesChanged(&TypedEventHandler::<Session, MediaPropertiesChangedEventArgs>::new(|_, _| {
        INBOX.push(Cmd::Props);
        Ok(())
    }))?;
    let t1 = s.PlaybackInfoChanged(&TypedEventHandler::<Session, PlaybackInfoChangedEventArgs>::new(|_, _| {
        INBOX.push(Cmd::Playback);
        Ok(())
    }))?;
    let t2 = s.TimelinePropertiesChanged(&TypedEventHandler::<Session, TimelinePropertiesChangedEventArgs>::new(|_, _| {
        INBOX.push(Cmd::Timeline);
        Ok(())
    }))?;
    Ok(Bound { s, app, tokens: [t0, t1, t2] })
}

/// A media session as far as choosing one cares.
struct Cand {
    /// `SourceAppUserModelId`.
    app: String,
    playing: bool,
    /// The system's current session.
    current: bool,
}

/// Which candidate to show, skipping the apps the user ignores (`island.media_ignore_apps`): the
/// system's current one, else a playing one, else the first.
fn pick_index(c: &[Cand], ignore: &[String]) -> Option<usize> {
    let ok = |i: &usize| !moku_core::apps::media_ignored(&c[*i].app, ignore);
    (0..c.len()).filter(ok).find(|&i| c[i].current).or_else(|| (0..c.len()).filter(ok).find(|&i| c[i].playing)).or_else(|| (0..c.len()).find(|i| ok(i)))
}

fn pick(mgr: &GlobalSystemMediaTransportControlsSessionManager) -> Option<Session> {
    let ignore = crate::app::current_config().island.media_ignore_apps.clone();
    let cur = mgr.GetCurrentSession().ok();
    let mut list: Vec<Session> = match mgr.GetSessions() {
        Ok(all) => (0..all.Size().unwrap_or(0)).filter_map(|i| all.GetAt(i).ok()).collect(),
        Err(_) => vec![],
    };
    if let Some(c) = &cur
        && !list.iter().any(|s| s == c)
    {
        list.insert(0, c.clone());
    }
    let cands: Vec<Cand> = list
        .iter()
        .map(|s| Cand { app: s.SourceAppUserModelId().map(|h| h.to_string_lossy()).unwrap_or_default(), playing: is_playing(s), current: cur.as_ref() == Some(s) })
        .collect();
    pick_index(&cands, &ignore).map(|i| list.swap_remove(i))
}

fn is_playing(s: &Session) -> bool {
    s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).is_ok_and(|st| st == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
}

fn read_track(s: &Session) -> windows::core::Result<(Track, Option<IRandomAccessStreamReference>)> {
    let p = wait_op!(s.TryGetMediaPropertiesAsync()?)?;
    let title = p.Title()?.to_string_lossy();
    let mut artist = p.Artist().map(|h| h.to_string_lossy()).unwrap_or_default();
    if artist.trim().is_empty() {
        artist = p.AlbumArtist().map(|h| h.to_string_lossy()).unwrap_or_default();
    }
    let album = p.AlbumTitle().map(|h| h.to_string_lossy()).unwrap_or_default();
    let thumb = p.Thumbnail().ok();
    Ok((Track { title: title.trim().into(), artist: artist.trim().into(), album: album.trim().into() }, thumb))
}

/// The thumbnail's encoded bytes (decoded only when they changed, see [`ArtSig`]).
fn read_art_bytes(r: &IRandomAccessStreamReference) -> windows::core::Result<Vec<u8>> {
    let st = wait_op!(r.OpenReadAsync()?)?;
    let size = st.Size()?;
    if size == 0 || size > ART_MAX_BYTES {
        return Err(windows::core::Error::empty());
    }
    let dr = DataReader::CreateDataReader(&st.cast::<IInputStream>()?)?;
    let n = wait_op!(dr.LoadAsync(size as u32)?)?;
    let mut buf = vec![0u8; n as usize];
    dr.ReadBytes(&mut buf)?;
    Ok(buf)
}

fn read_playback(s: &Session) -> windows::core::Result<(bool, Controls)> {
    let info = s.GetPlaybackInfo()?;
    let playing = info.PlaybackStatus()? == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
    let c = info.Controls()?;
    let controls = Controls {
        prev: c.IsPreviousEnabled().unwrap_or(false),
        next: c.IsNextEnabled().unwrap_or(false),
        play_pause: c.IsPlayPauseToggleEnabled().unwrap_or(false) || c.IsPlayEnabled().unwrap_or(false) || c.IsPauseEnabled().unwrap_or(false),
        seek: c.IsPlaybackPositionEnabled().unwrap_or(false),
    };
    Ok((playing, controls))
}

/// System time in 100 ns ticks since 1601 (same epoch as WinRT `DateTime`).
fn now_ticks() -> i64 {
    let ft = unsafe { GetSystemTimeAsFileTime() };
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

fn read_timeline(s: &Session, playing: bool) -> windows::core::Result<Timeline> {
    let t = s.GetTimelineProperties()?;
    let start = t.StartTime()?.Duration;
    let end = t.EndTime()?.Duration;
    let pos = t.Position()?.Duration;
    let dur_ms = ((end - start).max(0) / 10_000) as u64;
    let mut pos_ms = ((pos - start).max(0) / 10_000) as u64;
    // The position is as of LastUpdatedTime; move it to "now" while playing.
    if playing {
        let last = t.LastUpdatedTime().map(|d| d.UniversalTime).unwrap_or(0);
        let elapsed = now_ticks() - last;
        if last > 0 && (0..24 * 3600 * 10_000_000i64).contains(&elapsed) {
            pos_ms += (elapsed / 10_000) as u64;
        }
    }
    if dur_ms > 0 {
        pos_ms = pos_ms.min(dur_ms);
    }
    Ok(Timeline { pos_ms, dur_ms, at: Some(std::time::Instant::now()) })
}

/// Retry delay after `d`: doubled, at most 60 s.
fn next_backoff(d: Duration) -> Duration {
    (d * 2).min(Duration::from_secs(60))
}

fn post(u: Update) {
    Ui::post(move |app| super::apply(app, u));
}

fn control(s: &Session, c: Cmd) {
    let r = match c {
        Cmd::Toggle => (|| wait_op!(s.TryTogglePlayPauseAsync()?))(),
        Cmd::Next => (|| wait_op!(s.TrySkipNextAsync()?))(),
        Cmd::Prev => (|| wait_op!(s.TrySkipPreviousAsync()?))(),
        Cmd::Seek(ms) => (|| wait_op!(s.TryChangePlaybackPositionAsync(ms as i64 * 10_000)?))(),
        _ => return,
    };
    if let Err(e) = r {
        log_warn!("media: {c:?} failed: {e}");
    }
}

pub fn run(ctx: ServiceCtx) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    serve(&ctx);
    unsafe { CoUninitialize() };
}

fn serve(ctx: &ServiceCtx) {
    INBOX.clear();
    // At logon the media broker may not be up yet: retry with backoff (2 s … 60 s) instead of
    // giving up for the whole session.
    let mut delay = Duration::from_secs(2);
    let mgr = loop {
        match (|| wait_op!(GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?))() {
            Ok(m) => break m,
            Err(e) => {
                log_warn!("media: session manager unavailable ({e}); retrying in {} s", delay.as_secs());
                if !ctx.sleep(delay) {
                    return;
                }
                delay = next_backoff(delay);
            }
        }
    };
    let on_change = || {
        INBOX.push(Cmd::Session);
        Ok(())
    };
    let tok_cur = mgr.CurrentSessionChanged(&TypedEventHandler::<GlobalSystemMediaTransportControlsSessionManager, CurrentSessionChangedEventArgs>::new(move |_, _| on_change()));
    let tok_all = mgr.SessionsChanged(&TypedEventHandler::<GlobalSystemMediaTransportControlsSessionManager, SessionsChangedEventArgs>::new(move |_, _| on_change()));

    let mut cur: Option<Bound> = None;
    // The art on screen (so repeated property events don't re-decode the same picture).
    let mut art: Option<ArtSig> = None;
    let mut track_key: Option<String> = None;
    let mut recheck: Option<Instant> = None;
    let mut playing = false;
    INBOX.push(Cmd::Session);
    while let Some(batch) = INBOX.wait_timeout(ctx, recheck.map(|t| t.saturating_duration_since(Instant::now()))) {
        let mut session = false;
        let (mut props, mut playback, mut timeline) = (false, false, false);
        if batch.is_empty() {
            // The delayed second look at the art after a track change.
            recheck = None;
            props = true;
        }
        for c in &batch {
            match c {
                Cmd::Session => session = true,
                Cmd::Reset => {
                    cur = None;
                    session = true;
                }
                Cmd::Props => props = true,
                Cmd::Playback => playback = true,
                Cmd::Timeline => timeline = true,
                _ => {}
            }
        }
        if session {
            let next = pick(&mgr);
            let same = matches!((&cur, &next), (Some(b), Some(n)) if b.s == *n);
            if !same {
                cur = None;
                art = None;
                track_key = None;
                match next.map(bind) {
                    Some(Ok(b)) => {
                        post(Update::Session { app: b.app.clone() });
                        cur = Some(b);
                        (props, playback, timeline) = (true, true, true);
                    }
                    Some(Err(e)) => {
                        log_warn!("media: cannot listen to session: {e}");
                        post(Update::SessionGone);
                    }
                    None => post(Update::SessionGone),
                }
            }
        }
        let Some(b) = &cur else { continue };
        for c in batch.iter().filter(|c| matches!(c, Cmd::Toggle | Cmd::Next | Cmd::Prev | Cmd::Seek(_))) {
            control(&b.s, *c);
        }
        if props {
            match read_track(&b.s) {
                Ok((track, thumb)) => {
                    let key = track.key();
                    if track_key.as_deref() != Some(key.as_str()) {
                        track_key = Some(key.clone());
                        recheck = Some(Instant::now() + ART_RECHECK);
                    }
                    post(Update::Track(track));
                    // Read the thumbnail on every properties event; decode only a new picture.
                    let bytes = thumb.as_ref().and_then(|t| read_art_bytes(t).ok());
                    let sig = bytes.as_deref().map_or_else(|| ArtSig::none(&key), |b| ArtSig::of(&key, b));
                    if art_changed(art.as_ref(), &sig) {
                        let img = bytes.and_then(|b| crate::imaging::decode(&b, ART_MAX_PX).ok());
                        art = Some(sig);
                        post(Update::Art { key, art: img });
                    }
                }
                Err(e) => log_warn!("media: properties unavailable: {e}"),
            }
        }
        if playback {
            if let Ok((p, controls)) = read_playback(&b.s) {
                playing = p;
                post(Update::Playback { playing: p, controls });
            }
            timeline = true;
        }
        if timeline {
            if let Ok(t) = read_timeline(&b.s, playing) {
                post(Update::Timeline(t));
            }
        }
    }
    drop(cur);
    if let Ok(t) = tok_cur {
        let _ = mgr.RemoveCurrentSessionChanged(t);
    }
    if let Ok(t) = tok_all {
        let _ = mgr.RemoveSessionsChanged(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_is_decoded_again_only_when_the_picture_changes() {
        let a = ArtSig::of("song|artist", b"png-bytes-1");
        // Same track, same bytes: nothing to do.
        assert!(!art_changed(Some(&a), &ArtSig::of("song|artist", b"png-bytes-1")));
        // Same track, new picture (late art / new cover): decode.
        assert!(art_changed(Some(&a), &ArtSig::of("song|artist", b"png-bytes-2")));
        assert_ne!(fnv1a(b"ab"), fnv1a(b"ba"));
        // Same track, thumbnail unreadable right now: keep what is shown.
        assert!(!art_changed(Some(&a), &ArtSig::none("song|artist")));
        // First art for a track that had none: decode.
        assert!(art_changed(Some(&ArtSig::none("song|artist")), &a));
        // A new track without art: clear.
        assert!(art_changed(Some(&a), &ArtSig::none("other|artist")));
        assert!(art_changed(None, &ArtSig::none("x")));
    }

    fn cand(app: &str, playing: bool, current: bool) -> Cand {
        Cand { app: app.into(), playing, current }
    }

    #[test]
    fn the_session_to_show_skips_ignored_apps() {
        let none: Vec<String> = vec![];
        let sessions = [cand("chrome.exe", false, true), cand("Spotify.exe", true, false), cand("vlc.exe", false, false)];
        // Nothing ignored: the system's current session, as before.
        assert_eq!(pick_index(&sessions, &none), Some(0));
        // The current one is ignored: the playing one.
        assert_eq!(pick_index(&sessions, &["chrome.exe".to_string()]), Some(1));
        // The current and the playing one are ignored: the first that is left.
        assert_eq!(pick_index(&sessions, &["chrome".to_string(), "spotify".to_string()]), Some(2));
        // Nobody playing and no current one: the first.
        assert_eq!(pick_index(&[cand("a.exe", false, false), cand("b.exe", false, false)], &none), Some(0));
        // Everything ignored (or nothing there): no session.
        assert_eq!(pick_index(&sessions, &["chrome".to_string(), "spotify".to_string(), "vlc.exe".to_string()]), None);
        assert_eq!(pick_index(&[], &none), None);
    }

    #[test]
    fn session_manager_retry_backs_off() {
        let mut d = Duration::from_secs(2);
        let mut seen = vec![];
        for _ in 0..7 {
            seen.push(d.as_secs());
            d = next_backoff(d);
        }
        assert_eq!(seen, vec![2, 4, 8, 16, 32, 60, 60]);
    }
}
