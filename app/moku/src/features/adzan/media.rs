//! Pausing the user's media around the adzan, and putting it back.
//!
//! The decision logic is pure and works on a [`Sessions`] trait, so it is tested with a fake:
//!
//! * before the sound: pause every session that is **playing** (and is not Moku's own);
//! * after it: resume only what *Moku* paused, and only if it is still paused. A session the user
//!   resumed or stopped meanwhile, or that has gone, is left alone, and so is anything that was
//!   already paused before. **By session, not by app**: an app can have several sessions (browser tabs,
//!   a player and its mini player) under one AUMID, so [`Gsmtc`] remembers the exact sessions it paused
//!   ([`Ours`]) and resumes those only; a sibling session the user had paused earlier stays paused.
//!
//! The real implementation ([`Gsmtc`]) talks to the Windows media session manager; it runs on the
//! adzan thread (an MTA thread), never on the UI thread.

use std::time::{Duration, Instant};

use windows::Media::Control::*;

/// What a session is doing, as far as this module cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Playing,
    Paused,
    /// Stopped, closed, opened, changing: not something to pause or resume.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    /// The app the session belongs to (its AUMID); never shown or logged.
    pub id: String,
    pub status: Status,
}

pub trait Sessions {
    fn list(&mut self) -> Vec<SessionInfo>;
    /// Pause the playing session(s) of `id`; `true` when something was paused.
    fn pause(&mut self, id: &str) -> bool;
    /// Resume the paused session(s) of `id`; `true` when something was resumed.
    fn play(&mut self, id: &str) -> bool;
}

/// Moku's own sessions (e.g. a test sound) are never paused.
pub fn is_own(id: &str) -> bool {
    id.to_ascii_lowercase().contains("moku")
}

/// Ids to pause: what is playing now (each once), minus our own.
pub fn plan_pause(list: &[SessionInfo]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for s in list {
        if s.status == Status::Playing && !is_own(&s.id) && !out.contains(&s.id) {
            out.push(s.id.clone());
        }
    }
    out
}

/// Ids to resume: those Moku paused that are still paused now.
pub fn plan_resume(ours: &[String], now: &[SessionInfo]) -> Vec<String> {
    ours.iter().filter(|id| now.iter().any(|s| &s.id == *id && s.status == Status::Paused)).cloned().collect()
}

/// What a cycle did (counts only are ever logged).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cycle {
    pub paused: Vec<String>,
    pub resumed: Vec<String>,
}

/// Run `body` with the media paused: pause (when `enabled`), `body`, resume what we paused.
pub fn around<S: Sessions, R>(s: &mut S, enabled: bool, body: impl FnOnce(&mut S) -> R) -> (R, Cycle) {
    let mut cycle = Cycle::default();
    if enabled {
        for id in plan_pause(&s.list()) {
            if s.pause(&id) {
                cycle.paused.push(id);
            }
        }
    }
    let r = body(s);
    if !cycle.paused.is_empty() {
        for id in plan_resume(&cycle.paused, &s.list()) {
            if s.play(&id) {
                cycle.resumed.push(id);
            }
        }
    }
    (r, cycle)
}

// ---- the real thing ---------------------------------------------------------------------------

/// Longest wait for one media call (a hung player must not hang the adzan).
const OP_TIMEOUT: Duration = Duration::from_secs(3);

/// Wait for a WinRT async operation on this MTA thread, at most [`OP_TIMEOUT`].
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
                break Err(windows::core::Error::from(windows::Win32::Foundation::ERROR_TIMEOUT.to_hresult()));
            }
            std::thread::sleep(Duration::from_millis(nap));
            nap = (nap * 2).min(16);
        }
    }};
}
pub(crate) use wait_op;

type Session = GlobalSystemMediaTransportControlsSession;

/// The exact sessions Moku paused: an app id and a handle to each session. Pure (the handle is anything),
/// so the bookkeeping is tested without Windows.
#[derive(Debug)]
pub struct Ours<T> {
    list: Vec<(String, T)>,
}

impl<T> Default for Ours<T> {
    fn default() -> Self {
        Ours { list: vec![] }
    }
}

impl<T> Ours<T> {
    /// Moku paused this session of app `id`.
    pub fn record(&mut self, id: &str, handle: T) {
        self.list.push((id.to_string(), handle));
    }

    /// The sessions of app `id` that Moku paused (and forget them): the only ones that may be resumed.
    pub fn take(&mut self, id: &str) -> Vec<T> {
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.list).into_iter().partition(|(i, _)| i == id);
        self.list = rest;
        mine.into_iter().map(|(_, h)| h).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// The system's media sessions.
pub struct Gsmtc {
    mgr: GlobalSystemMediaTransportControlsSessionManager,
    /// The sessions this object paused (see [`Ours`]).
    ours: Ours<Session>,
}

impl Gsmtc {
    pub fn open() -> windows::core::Result<Gsmtc> {
        let mgr = (|| wait_op!(GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?))()?;
        Ok(Gsmtc { mgr, ours: Ours::default() })
    }

    fn all(&self) -> Vec<(String, Status, Session)> {
        let Ok(list) = self.mgr.GetSessions() else { return vec![] };
        let n = list.Size().unwrap_or(0);
        (0..n)
            .filter_map(|i| list.GetAt(i).ok())
            .map(|s| {
                let id = s.SourceAppUserModelId().map(|h| h.to_string_lossy()).unwrap_or_default();
                let st = match s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()) {
                    Ok(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing) => Status::Playing,
                    Ok(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused) => Status::Paused,
                    _ => Status::Other,
                };
                (id, st, s)
            })
            .collect()
    }
}

impl Sessions for Gsmtc {
    fn list(&mut self) -> Vec<SessionInfo> {
        // One entry per app: playing beats paused beats the rest.
        let mut out: Vec<SessionInfo> = vec![];
        for (id, status, _) in self.all() {
            match out.iter_mut().find(|o| o.id == id) {
                Some(o) => {
                    if rank(status) > rank(o.status) {
                        o.status = status;
                    }
                }
                None => out.push(SessionInfo { id, status }),
            }
        }
        out
    }

    fn pause(&mut self, id: &str) -> bool {
        let mut any = false;
        for (sid, st, s) in self.all() {
            if sid == id && st == Status::Playing && (|| wait_op!(s.TryPauseAsync()?))().unwrap_or(false) {
                self.ours.record(&sid, s);
                any = true;
            }
        }
        any
    }

    /// Resume the sessions of `id` that Moku paused and that are still paused: never a session that was
    /// paused before, or that the user paused or started again meanwhile.
    fn play(&mut self, id: &str) -> bool {
        let mut any = false;
        for s in self.ours.take(id) {
            let still_paused = matches!(s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()), Ok(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused));
            if still_paused {
                any |= (|| wait_op!(s.TryPlayAsync()?))().unwrap_or(false);
            }
        }
        any
    }
}

fn rank(s: Status) -> u8 {
    match s {
        Status::Playing => 2,
        Status::Paused => 1,
        Status::Other => 0,
    }
}

/// No media access (the manager could not be opened): nothing to pause.
pub struct NoSessions;

impl Sessions for NoSessions {
    fn list(&mut self) -> Vec<SessionInfo> {
        vec![]
    }
    fn pause(&mut self, _: &str) -> bool {
        false
    }
    fn play(&mut self, _: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_sessions_moku_paused_come_back() {
        // One app (one AUMID), two sessions: tab 1 was playing, Moku paused it; tab 2 the user had paused.
        let mut ours: Ours<&str> = Ours::default();
        assert!(ours.is_empty());
        ours.record("chrome", "tab 1");
        ours.record("spotify", "main");
        // After the adzan: resume app "chrome": only tab 1. Tab 2 was never recorded, so it stays paused.
        assert_eq!(ours.take("chrome"), ["tab 1"]);
        assert!(ours.take("chrome").is_empty(), "a session is resumed once");
        assert_eq!(ours.take("spotify"), ["main"]);
        assert!(ours.take("never").is_empty());
        assert!(ours.is_empty());
        // Several sessions of one app that Moku paused come back together, in order.
        ours.record("chrome", "a");
        ours.record("vlc", "x");
        ours.record("chrome", "b");
        assert_eq!(ours.take("chrome"), ["a", "b"]);
        assert_eq!(ours.take("vlc"), ["x"]);
    }

    /// A fake set of sessions that behaves like the real ones: pausing a playing session makes it
    /// paused, resuming a paused one makes it playing; anything else is refused.
    #[derive(Default)]
    struct Fake {
        sessions: Vec<SessionInfo>,
        /// Every call, in order.
        log: Vec<String>,
        /// Ids whose pause fails (an app that ignores the request).
        refuse_pause: Vec<String>,
    }

    impl Fake {
        fn with(list: &[(&str, Status)]) -> Fake {
            Fake { sessions: list.iter().map(|(id, st)| SessionInfo { id: (*id).into(), status: *st }).collect(), ..Default::default() }
        }
        fn status(&self, id: &str) -> Option<Status> {
            self.sessions.iter().find(|s| s.id == id).map(|s| s.status)
        }
        fn set(&mut self, id: &str, st: Status) {
            if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                s.status = st;
            }
        }
    }

    impl Sessions for Fake {
        fn list(&mut self) -> Vec<SessionInfo> {
            self.sessions.clone()
        }
        fn pause(&mut self, id: &str) -> bool {
            self.log.push(format!("pause {id}"));
            if self.refuse_pause.iter().any(|r| r == id) || self.status(id) != Some(Status::Playing) {
                return false;
            }
            self.set(id, Status::Paused);
            true
        }
        fn play(&mut self, id: &str) -> bool {
            self.log.push(format!("play {id}"));
            if self.status(id) != Some(Status::Paused) {
                return false;
            }
            self.set(id, Status::Playing);
            true
        }
    }

    #[test]
    fn nothing_playing_nothing_touched() {
        let mut f = Fake::with(&[("spotify", Status::Paused), ("chrome", Status::Other)]);
        let (ran, cycle) = around(&mut f, true, |_| 7);
        assert_eq!(ran, 7, "the sound still plays");
        assert_eq!(cycle, Cycle::default());
        assert!(f.log.is_empty(), "{:?}", f.log);
    }

    #[test]
    fn what_we_paused_is_resumed_and_only_that() {
        // Spotify plays, Chrome was paused by the user long ago, VLC is stopped.
        let mut f = Fake::with(&[("spotify", Status::Playing), ("chrome", Status::Paused), ("vlc", Status::Other)]);
        let (_, cycle) = around(&mut f, true, |f| {
            // During the adzan: Spotify is paused, the others are as they were.
            assert_eq!(f.status("spotify"), Some(Status::Paused));
            assert_eq!(f.status("chrome"), Some(Status::Paused));
        });
        assert_eq!(cycle.paused, vec!["spotify".to_string()]);
        assert_eq!(cycle.resumed, vec!["spotify".to_string()]);
        assert_eq!(f.status("spotify"), Some(Status::Playing), "back to how it was");
        assert_eq!(f.status("chrome"), Some(Status::Paused), "the user's own pause is respected");
        assert_eq!(f.status("vlc"), Some(Status::Other));
        assert_eq!(f.log, vec!["pause spotify", "play spotify"]);
    }

    #[test]
    fn several_players_all_come_back() {
        let mut f = Fake::with(&[("a", Status::Playing), ("b", Status::Playing)]);
        let (_, cycle) = around(&mut f, true, |_| ());
        assert_eq!(cycle.paused, vec!["a", "b"]);
        assert_eq!(cycle.resumed, vec!["a", "b"]);
    }

    #[test]
    fn the_user_took_over_during_the_adzan() {
        // The user pressed play again themselves: do not press it a second time.
        let mut f = Fake::with(&[("spotify", Status::Playing)]);
        let (_, cycle) = around(&mut f, true, |f| f.set("spotify", Status::Playing));
        assert_eq!(cycle.paused, vec!["spotify"]);
        assert!(cycle.resumed.is_empty());
        assert_eq!(f.log, vec!["pause spotify"], "no play call");
        // The user stopped it: it stays stopped.
        let mut f = Fake::with(&[("spotify", Status::Playing)]);
        let (_, cycle) = around(&mut f, true, |f| f.set("spotify", Status::Other));
        assert!(cycle.resumed.is_empty());
        // The app went away (closed during the adzan).
        let mut f = Fake::with(&[("spotify", Status::Playing)]);
        let (_, cycle) = around(&mut f, true, |f| f.sessions.clear());
        assert!(cycle.resumed.is_empty());
        assert_eq!(f.log, vec!["pause spotify"]);
    }

    #[test]
    fn the_setting_off_touches_nothing() {
        let mut f = Fake::with(&[("spotify", Status::Playing)]);
        let (_, cycle) = around(&mut f, false, |_| ());
        assert_eq!(cycle, Cycle::default());
        assert!(f.log.is_empty());
        assert_eq!(f.status("spotify"), Some(Status::Playing));
    }

    #[test]
    fn a_failed_pause_is_not_resumed() {
        let mut f = Fake::with(&[("stubborn", Status::Playing), ("spotify", Status::Playing)]);
        f.refuse_pause.push("stubborn".into());
        let (_, cycle) = around(&mut f, true, |_| ());
        assert_eq!(cycle.paused, vec!["spotify"]);
        assert_eq!(cycle.resumed, vec!["spotify"]);
        assert_eq!(f.status("stubborn"), Some(Status::Playing), "untouched");
        assert!(!f.log.contains(&"play stubborn".to_string()));
    }

    #[test]
    fn plans_are_deduplicated_and_skip_moku() {
        let list = vec![
            SessionInfo { id: "chrome".into(), status: Status::Playing },
            SessionInfo { id: "chrome".into(), status: Status::Playing },
            SessionInfo { id: "moku.exe".into(), status: Status::Playing },
            SessionInfo { id: "Moku.Island".into(), status: Status::Playing },
            SessionInfo { id: "vlc".into(), status: Status::Paused },
        ];
        assert_eq!(plan_pause(&list), vec!["chrome"]);
        assert!(is_own("MOKU.exe") && !is_own("spotify.exe"));
        let ours = vec!["chrome".to_string(), "gone".to_string()];
        assert_eq!(plan_resume(&ours, &[SessionInfo { id: "chrome".into(), status: Status::Paused }]), vec!["chrome"]);
        assert!(plan_resume(&ours, &[]).is_empty());
        assert!(plan_resume(&[], &list).is_empty());
    }

    #[test]
    fn the_sound_runs_between_pause_and_resume() {
        let mut f = Fake::with(&[("spotify", Status::Playing)]);
        let mut order = vec![];
        let ((), cycle) = around(&mut f, true, |f| {
            order.push(f.log.clone());
        });
        assert_eq!(order, vec![vec!["pause spotify".to_string()]], "paused before the body ran, not yet resumed");
        assert_eq!(cycle.resumed.len(), 1);
    }
}
