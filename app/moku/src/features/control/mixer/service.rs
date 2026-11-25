//! The mixer service loop, written against a [`Backend`] so it can be tested with a fake one.
//!
//! The service is purely command driven: it sleeps on its inbox and wakes only for a command,
//! so with the Kontrol tab hidden (nothing sends commands) it does no work at all. Core Audio
//! is opened on the first command that needs it and let go of on `Release`.

use super::model::RawSession;
use crate::features::media::inbox::Inbox;
use crate::services::ServiceCtx;

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    /// Read the sessions and post them.
    Refresh,
    /// Set the level of every session of the app `key` (0-1).
    SetVolume(String, f32),
    SetMute(String, bool),
    /// The tab has been hidden for a while: let go of Core Audio.
    Release,
}

/// What the loop needs from Core Audio.
pub trait Backend {
    fn sessions(&mut self) -> Vec<RawSession>;
    fn set_volume(&mut self, key: &str, volume: f32);
    fn set_mute(&mut self, key: &str, muted: bool);
}

/// The tab ticked (once when shown, then every 2 s while visible): refresh the mixer, but only
/// while its view is open instead of the tiles.
pub fn tick(mixer_open: bool, inbox: &Inbox<Cmd>) {
    if mixer_open {
        inbox.push(Cmd::Refresh);
    }
}

/// Replace the value for `key` or add it: only the last value of a burst matters.
fn upsert<T>(list: &mut Vec<(String, T)>, key: &str, v: T) {
    match list.iter_mut().find(|(k, _)| k == key) {
        Some(e) => e.1 = v,
        None => list.push((key.to_string(), v)),
    }
}

/// Run until stopped. `make` opens the backend (returns `None` when Core Audio is not
/// available: the command is dropped), `post` hands a snapshot to the UI.
pub fn serve<B: Backend>(ctx: &ServiceCtx, inbox: &Inbox<Cmd>, make: impl FnMut() -> Option<B>, post: impl Fn(Vec<RawSession>)) {
    inbox.clear(); // commands sent while the service was off are stale
    run_loop(ctx, inbox, make, post);
}

fn run_loop<B: Backend>(ctx: &ServiceCtx, inbox: &Inbox<Cmd>, mut make: impl FnMut() -> Option<B>, post: impl Fn(Vec<RawSession>)) {
    let mut backend: Option<B> = None;
    while let Some(batch) = inbox.wait(ctx) {
        // Released unless a command in the same batch comes after it.
        if batch.last() == Some(&Cmd::Release) {
            backend = None;
            continue;
        }
        if backend.is_none() {
            backend = make();
        }
        let Some(b) = backend.as_mut() else { continue };
        let mut vols: Vec<(String, f32)> = vec![];
        let mut mutes: Vec<(String, bool)> = vec![];
        for c in &batch {
            match c {
                Cmd::SetVolume(k, v) => upsert(&mut vols, k, *v),
                Cmd::SetMute(k, m) => upsert(&mut mutes, k, *m),
                Cmd::Refresh | Cmd::Release => {}
            }
        }
        for (k, v) in vols {
            b.set_volume(&k, v);
        }
        for (k, m) in mutes {
            b.set_mute(&k, m);
        }
        if batch.contains(&Cmd::Refresh) {
            post(b.sessions());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::super::model::RawSession;
    use crate::features::media::inbox::Inbox;
    use crate::services::ServiceCtx;

    #[derive(Default)]
    struct Counts {
        made: AtomicUsize,
        read: AtomicUsize,
        released: AtomicUsize,
        sets: Mutex<Vec<String>>,
    }

    struct Fake(Arc<Counts>);

    impl Backend for Fake {
        fn sessions(&mut self) -> Vec<RawSession> {
            self.0.read.fetch_add(1, Ordering::SeqCst);
            vec![RawSession { key: "a.exe".into(), name: "A".into(), system: false, volume: 0.5, muted: false, active: true, expired: false, icon: None }]
        }
        fn set_volume(&mut self, key: &str, v: f32) {
            self.0.sets.lock().unwrap().push(format!("vol {key} {v}"));
        }
        fn set_mute(&mut self, key: &str, muted: bool) {
            self.0.sets.lock().unwrap().push(format!("mute {key} {muted}"));
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.released.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct Rig {
        counts: Arc<Counts>,
        inbox: Arc<Inbox<Cmd>>,
        ctx: ServiceCtx,
        posted: Arc<Mutex<Vec<usize>>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    fn start() -> Rig {
        start_with(&[])
    }

    /// `pre` is queued before the service thread starts, so a burst arrives as one batch.
    fn start_with(pre: &[Cmd]) -> Rig {
        let counts = Arc::new(Counts::default());
        let inbox: Arc<Inbox<Cmd>> = Arc::new(Inbox::new());
        let ctx = ServiceCtx::detached("mixer-test");
        let posted = Arc::new(Mutex::new(Vec::new()));
        for c in pre {
            inbox.push(c.clone());
        }
        let (c, i, x, p) = (counts.clone(), inbox.clone(), ctx.clone(), posted.clone());
        let thread = std::thread::spawn(move || {
            let made = c.clone();
            run_loop(
                &x,
                &i,
                move || {
                    made.made.fetch_add(1, Ordering::SeqCst);
                    Some(Fake(made.clone()))
                },
                move |s| p.lock().unwrap().push(s.len()),
            );
        });
        Rig { counts, inbox, ctx, posted, thread: Some(thread) }
    }

    impl Rig {
        fn settle(&self) {
            std::thread::sleep(Duration::from_millis(150));
        }
        fn stop(mut self) {
            self.ctx.request_stop();
            self.thread.take().unwrap().join().unwrap();
        }
    }

    #[test]
    fn a_hidden_mixer_does_no_work_at_all() {
        // The tab is hidden: nothing sends commands. The service must not touch Core Audio
        // (the backend is not even created), not poll, not post.
        let rig = start();
        rig.settle();
        assert_eq!(rig.counts.made.load(Ordering::SeqCst), 0, "Core Audio is only opened on the first refresh");
        assert_eq!(rig.counts.read.load(Ordering::SeqCst), 0);
        assert!(rig.posted.lock().unwrap().is_empty());
        rig.stop();
    }

    #[test]
    fn a_refresh_reads_the_sessions_once_and_posts_them() {
        let rig = start();
        rig.inbox.push(Cmd::Refresh);
        rig.settle();
        assert_eq!(rig.counts.made.load(Ordering::SeqCst), 1);
        assert_eq!(rig.counts.read.load(Ordering::SeqCst), 1);
        assert_eq!(*rig.posted.lock().unwrap(), vec![1]);
        // A second refresh reuses the backend.
        rig.inbox.push(Cmd::Refresh);
        rig.settle();
        assert_eq!(rig.counts.made.load(Ordering::SeqCst), 1);
        assert_eq!(rig.counts.read.load(Ordering::SeqCst), 2);
        rig.stop();
    }

    #[test]
    fn nothing_happens_between_refreshes() {
        let rig = start();
        rig.inbox.push(Cmd::Refresh);
        rig.settle();
        let reads = rig.counts.read.load(Ordering::SeqCst);
        rig.settle();
        rig.settle();
        assert_eq!(rig.counts.read.load(Ordering::SeqCst), reads, "no polling of its own");
        rig.stop();
    }

    #[test]
    fn volume_and_mute_commands_reach_the_backend_without_a_read() {
        // A drag sends many values in a burst: only the last one of a batch is applied.
        let rig = start_with(&[Cmd::SetVolume("a.exe".into(), 0.1), Cmd::SetVolume("a.exe".into(), 0.2), Cmd::SetVolume("a.exe".into(), 0.3), Cmd::SetMute("a.exe".into(), true)]);
        rig.settle();
        let sets = rig.counts.sets.lock().unwrap().clone();
        assert_eq!(sets, vec!["vol a.exe 0.3".to_string(), "mute a.exe true".to_string()]);
        assert_eq!(rig.counts.read.load(Ordering::SeqCst), 0, "setting a level does not enumerate the sessions");
        rig.stop();
    }

    #[test]
    fn release_drops_core_audio_and_the_next_refresh_opens_it_again() {
        let rig = start();
        rig.inbox.push(Cmd::Refresh);
        rig.settle();
        rig.inbox.push(Cmd::Release);
        rig.settle();
        assert_eq!(rig.counts.released.load(Ordering::SeqCst), 1, "the backend was dropped");
        rig.inbox.push(Cmd::Refresh);
        rig.settle();
        assert_eq!(rig.counts.made.load(Ordering::SeqCst), 2);
        let counts = rig.counts.clone();
        rig.stop();
        assert_eq!(counts.released.load(Ordering::SeqCst), 2, "dropped again when the service stops");
    }

    #[test]
    fn the_tab_tick_only_asks_for_a_refresh_while_the_mixer_view_is_open() {
        let inbox: Inbox<Cmd> = Inbox::new();
        let ctx = ServiceCtx::detached("tick-test");
        tick(false, &inbox);
        assert_eq!(inbox.wait_timeout(&ctx, Some(Duration::from_millis(30))), Some(vec![]), "tiles showing: no mixer work");
        tick(true, &inbox);
        assert_eq!(inbox.wait_timeout(&ctx, Some(Duration::from_millis(30))), Some(vec![Cmd::Refresh]));
    }
}
