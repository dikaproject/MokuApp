//! The adzan sound at prayer time (v0.3, track N). Opt-in (`ibadah.adzan`), per prayer.
//!
//! * **Trigger**: `prayer.rs::fire` calls [`on_prayer_time`] at the prayer time itself (never for
//!   the heads-up, never for a time that passed while the PC slept, never while the island is
//!   paused or in a meeting, never twice at once).
//! * **Media first**: the user's playing media is paused (Windows media sessions), the sound
//!   plays, and **only what Moku paused, and only if it is still paused**, is resumed
//!   (`media.rs`, tested with a fake).
//! * **Sound**: by default a gentle chime generated in code (`chime.rs`; nothing is bundled, no
//!   recording of the adzan ships with Moku). Or the user's own file (`moku_core::audiofile`
//!   checks it; a missing or unusable file, or one the player cannot decode, falls back to the
//!   chime). Played by `Windows.Media.Playback.MediaPlayer` on its own thread (`player.rs`), loaded only
//!   when a sound plays.
//! * **UI**: a pill "Adzan <prayer>" with a "Hentikan" button while it plays.
//!
//! Test hooks (`MOKU_DEBUG=1`): `adzan.debug_play[:<fajr|dhuhr|asr|maghrib|isha>[:<volume %>]]`
//! plays the configured sound at once, quietly (at most 15 %, 2.5 s) and `adzan.debug_media` logs
//! how many media sessions play / are paused (counts only). Nothing here logs file names or app
//! names.

pub mod chime;
pub mod media;
pub mod player;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use moku_core::audiofile;
use moku_core::config::{AdzanSound, Ibadah};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use super::prayer::Prayer;
use crate::activity::Activity;
use crate::app::{App, Redraw, Ui};
use crate::i18n::{tr, tr_s};
use crate::registry::*;
use crate::ui::*;

use media::{Gsmtc, NoSessions, Sessions, around};
use player::{Outcome, Playable};

/// An adzan later than this after its time (the PC was asleep) is not played.
pub const MAX_LATE_SECS: f64 = 30.0;
/// Longest a sound plays (a long Fajr adzan is about six minutes).
const MAX_PLAY: Duration = Duration::from_secs(10 * 60);
/// The debug hook plays this long, at most this loud.
const DEBUG_PLAY: Duration = Duration::from_millis(2500);
const DEBUG_MAX_PERCENT: u32 = 15;
const DEBUG_DEFAULT_PERCENT: u32 = 6;
/// Priority of the "playing" pill: above notifications, below AI cards and "due" reminders.
const PRIO_PLAYING: u8 = 85;

// ---- decisions (pure) -------------------------------------------------------------------------

/// Everything that decides whether a sound plays at a prayer time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gate {
    /// `ibadah.adzan`.
    pub enabled: bool,
    /// The toggle of this prayer.
    pub prayer_on: bool,
    /// Seconds after the prayer time that now is.
    pub late_secs: f64,
    pub in_meeting: bool,
    pub island_paused: bool,
    pub already_playing: bool,
}

pub fn should_play(g: &Gate) -> bool {
    g.enabled && g.prayer_on && g.late_secs <= MAX_LATE_SECS && !g.in_meeting && !g.island_paused && !g.already_playing
}

pub fn prayer_on(c: &Ibadah, p: Prayer) -> bool {
    match p {
        Prayer::Fajr => c.adzan_fajr,
        Prayer::Dhuhr => c.adzan_dhuhr,
        Prayer::Asr => c.adzan_asr,
        Prayer::Maghrib => c.adzan_maghrib,
        Prayer::Isha => c.adzan_isha,
    }
}

/// `adzan_volume` percent to the player's 0..1 volume.
pub fn volume_of(percent: u32) -> f32 {
    percent.clamp(5, 100) as f32 / 100.0
}

/// The sound to play.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Chime,
    File(PathBuf),
}

/// The configured sound, or the chime (and why) when the file cannot be used. Explorer's "Copy as
/// path" puts quotes around the path: they are ignored.
pub fn resolve(sound: AdzanSound, file: &str) -> (Source, Option<audiofile::Problem>) {
    match sound {
        AdzanSound::Chime => (Source::Chime, None),
        AdzanSound::File => {
            let path = PathBuf::from(file.trim().trim_matches('"').trim());
            match audiofile::check(&path) {
                Ok(()) => (Source::File(path), None),
                Err(p) => (Source::Chime, Some(p)),
            }
        }
    }
}

// ---- state ------------------------------------------------------------------------------------

#[derive(Default)]
pub struct AdzanState {
    run: Option<Running>,
    /// Counter, so a late "finished" of an old run cannot clear a newer one.
    seq: u64,
}

struct Running {
    id: u64,
    prayer: Prayer,
    stop: Arc<AtomicBool>,
    /// Signalled when the thread is completely done (media resumed).
    done: mpsc::Receiver<()>,
}

pub fn register(r: &mut Registry) {
    r.add_state(AdzanState::default());
    r.on_action("adzan", on_action);
    // Turning the setting off silences what is playing.
    r.on_config(|app| {
        if !app.config().ibadah.adzan {
            stop(app);
        }
    });
    // Quitting while the adzan plays: give the thread a moment to hand the media back.
    r.on_exit(|app| {
        if let Some(run) = app.state_mut::<AdzanState>().run.take() {
            run.stop.store(true, Ordering::Relaxed);
            let _ = run.done.recv_timeout(Duration::from_millis(2500));
        }
    });
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    match ev.name() {
        "stop" => stop(app),
        "debug_play" => debug_play(app, ev.arg()),
        "debug_media" => debug_media(),
        _ => {}
    }
}

// ---- starting and stopping --------------------------------------------------------------------

/// What one run needs, copied out of the config.
#[derive(Clone, Debug)]
struct Job {
    prayer: Prayer,
    sound: AdzanSound,
    file: String,
    volume: f32,
    pause_media: bool,
    max: Duration,
}

/// `prayer.rs` calls this at the prayer time.
pub fn on_prayer_time(app: &mut App, p: Prayer, late_secs: f64) {
    let c = app.config().ibadah.clone();
    let gate = Gate {
        enabled: c.adzan,
        prayer_on: prayer_on(&c, p),
        late_secs,
        in_meeting: app.meeting().active,
        island_paused: app.is_paused(),
        already_playing: app.state::<AdzanState>().run.is_some(),
    };
    if !should_play(&gate) {
        return;
    }
    let job = Job { prayer: p, sound: c.adzan_sound, file: c.adzan_file.clone(), volume: volume_of(c.adzan_volume), pause_media: c.adzan_pause_media, max: MAX_PLAY };
    start(app, job);
}

/// `adzan.debug_play[:<prayer>[:<volume %>]]`: a short, quiet sound through the real code path.
fn debug_play(app: &mut App, arg: Option<&str>) {
    let mut parts = arg.unwrap_or("").split(':');
    let prayer = match parts.next().unwrap_or("") {
        "fajr" => Prayer::Fajr,
        "asr" => Prayer::Asr,
        "maghrib" => Prayer::Maghrib,
        "isha" => Prayer::Isha,
        _ => Prayer::Dhuhr,
    };
    let percent = parts.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(DEBUG_DEFAULT_PERCENT).clamp(1, DEBUG_MAX_PERCENT);
    if app.state::<AdzanState>().run.is_some() {
        return;
    }
    let c = app.config().ibadah.clone();
    start(app, Job { prayer, sound: c.adzan_sound, file: c.adzan_file.clone(), volume: percent as f32 / 100.0, pause_media: c.adzan_pause_media, max: DEBUG_PLAY });
}

fn start(app: &mut App, job: Job) {
    let st = app.state_mut::<AdzanState>();
    st.seq += 1;
    let id = st.seq;
    let stop = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let prayer = job.prayer;
    let flag = stop.clone();
    let spawned = std::thread::Builder::new().name("adzan".into()).spawn(move || {
        let report = run(&job, &flag);
        let _ = done_tx.send(());
        Ui::post(move |app| finished(app, id, report));
    });
    if let Err(e) = spawned {
        log_warn!("adzan: cannot start the sound thread: {e}");
        return;
    }
    app.state_mut::<AdzanState>().run = Some(Running { id, prayer, stop, done: done_rx });
    let a = Activity::persistent("adzan.playing", PRIO_PLAYING, move |_| {
        pill([bars(true).color(color::MINT), text(tr_s(format!("Adzan {}", prayer.name()), format!("{} call", prayer.name()))).semibold(), button(tr("Hentikan", "Stop"), "adzan.stop")]).pad4(14.0, 0.0, 6.0, 0.0)
    })
    .tab("health")
    // Under a higher pill (the prayer-time pill) it is the small bubble beside it.
    .bubble(|_| stack([icon(icons::VOLUME).font_size(14.0).color(color::MINT)]).size(24.0, 24.0));
    app.set_activity(a);
    app.request_redraw(Redraw::Pill);
}

/// Silence the adzan (the pill's "Hentikan", or the setting turned off).
pub fn stop(app: &mut App) {
    if let Some(r) = &app.state::<AdzanState>().run {
        r.stop.store(true, Ordering::Relaxed);
    }
}

fn finished(app: &mut App, id: u64, report: Report) {
    log_info!("adzan: done ({:?}; media paused {}, resumed {})", report.outcome, report.paused, report.resumed);
    if app.state::<AdzanState>().run.as_ref().is_some_and(|r| r.id == id) {
        app.state_mut::<AdzanState>().run = None;
        app.clear_activity("adzan.playing");
        app.request_redraw(Redraw::Pill);
    }
}

// ---- the sound thread -------------------------------------------------------------------------

struct Report {
    outcome: Outcome,
    paused: usize,
    resumed: usize,
}

fn run(job: &Job, stop: &AtomicBool) -> Report {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let report = match Gsmtc::open() {
        Ok(mut g) => go(&mut g, job, stop),
        Err(e) => {
            log_warn!("adzan: media sessions unavailable ({e}); not pausing anything");
            go(&mut NoSessions, job, stop)
        }
    };
    unsafe { CoUninitialize() };
    report
}

fn go<S: Sessions>(s: &mut S, job: &Job, stop: &AtomicBool) -> Report {
    let (outcome, cycle) = around(s, job.pause_media, |_| {
        if job.pause_media {
            // Let the other player settle before the sound starts.
            std::thread::sleep(Duration::from_millis(250));
        }
        play_sound(job, stop)
    });
    Report { outcome, paused: cycle.paused.len(), resumed: cycle.resumed.len() }
}

fn playable_of(source: &Source) -> Playable {
    match source {
        Source::Chime => Playable::Bytes(chime::wav_bytes(), "audio/wav"),
        Source::File(p) => Playable::File(p.clone()),
    }
}

/// The configured sound; the chime when the file is unusable or the player cannot decode it.
fn play_sound(job: &Job, stop: &AtomicBool) -> Outcome {
    let (source, problem) = resolve(job.sound, &job.file);
    if let Some(p) = problem {
        log_info!("adzan: using the chime instead of the file ({p:?})");
    }
    let outcome = player::play(&playable_of(&source), job.volume, stop, job.max);
    if outcome == Outcome::Failed && source != Source::Chime {
        log_info!("adzan: the file could not be played; using the chime");
        return player::play(&playable_of(&Source::Chime), job.volume, stop, job.max);
    }
    outcome
}

/// `adzan.debug_media`: log how many sessions play / are paused / are something else.
fn debug_media() {
    let spawned = std::thread::Builder::new().name("adzan-media".into()).spawn(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        match Gsmtc::open() {
            Ok(mut g) => {
                let list = g.list();
                let count = |s: media::Status| list.iter().filter(|x| x.status == s).count();
                log_info!("adzan: media sessions: {} playing, {} paused, {} other", count(media::Status::Playing), count(media::Status::Paused), count(media::Status::Other));
            }
            Err(e) => log_warn!("adzan: media sessions unavailable ({e})"),
        }
        unsafe { CoUninitialize() };
    });
    if spawned.is_err() {
        log_warn!("adzan: cannot start the media thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> Gate {
        Gate { enabled: true, prayer_on: true, late_secs: 0.4, in_meeting: false, island_paused: false, already_playing: false }
    }

    #[test]
    fn the_sound_plays_only_when_everything_allows_it() {
        assert!(should_play(&gate()));
        assert!(!should_play(&Gate { enabled: false, ..gate() }), "the setting is off");
        assert!(!should_play(&Gate { prayer_on: false, ..gate() }), "this prayer is switched off");
        assert!(!should_play(&Gate { in_meeting: true, ..gate() }), "never in a meeting");
        assert!(!should_play(&Gate { island_paused: true, ..gate() }), "tray pause silences it");
        assert!(!should_play(&Gate { already_playing: true, ..gate() }), "not twice at once");
        // The PC woke up long after the time: no sound for a prayer that is long past.
        assert!(should_play(&Gate { late_secs: MAX_LATE_SECS, ..gate() }));
        assert!(!should_play(&Gate { late_secs: MAX_LATE_SECS + 1.0, ..gate() }));
    }

    #[test]
    fn each_prayer_has_its_own_toggle() {
        let c = Ibadah { adzan_fajr: false, adzan_dhuhr: true, adzan_asr: false, adzan_maghrib: true, adzan_isha: false, ..Ibadah::default() };
        let got: Vec<bool> = [Prayer::Fajr, Prayer::Dhuhr, Prayer::Asr, Prayer::Maghrib, Prayer::Isha].into_iter().map(|p| prayer_on(&c, p)).collect();
        assert_eq!(got, vec![false, true, false, true, false]);
    }

    #[test]
    fn volume_is_clamped() {
        assert_eq!(volume_of(40), 0.4);
        assert_eq!(volume_of(100), 1.0);
        assert_eq!(volume_of(0), 0.05, "never silent by accident, never above full");
        assert_eq!(volume_of(500), 1.0);
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-adzan-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_missing_or_bad_file_falls_back_to_the_chime() {
        let d = temp("resolve");
        // The default is the chime and never looks at a file.
        assert_eq!(resolve(AdzanSound::Chime, "C:\\definitely\\not\\there.mp3"), (Source::Chime, None));
        // Asked for a file that is not there.
        assert_eq!(resolve(AdzanSound::File, &d.join("gone.mp3").to_string_lossy()), (Source::Chime, Some(audiofile::Problem::Missing)));
        assert_eq!(resolve(AdzanSound::File, ""), (Source::Chime, Some(audiofile::Problem::NoPath)));
        // A text file renamed to .mp3.
        let fake = d.join("fake.mp3");
        std::fs::write(&fake, "not audio at all, just words. ".repeat(10)).unwrap();
        assert_eq!(resolve(AdzanSound::File, &fake.to_string_lossy()), (Source::Chime, Some(audiofile::Problem::NotAudio)));
        // A folder.
        assert_eq!(resolve(AdzanSound::File, &d.to_string_lossy()).0, Source::Chime);
        // A good file is used, also with the quotes Explorer's "Copy as path" adds and with spaces around.
        let good = d.join("mine.wav");
        std::fs::write(&good, chime::wav_bytes()).unwrap();
        assert_eq!(resolve(AdzanSound::File, &good.to_string_lossy()), (Source::File(good.clone()), None));
        assert_eq!(resolve(AdzanSound::File, &format!("  \"{}\" ", good.display())), (Source::File(good), None));
        let _ = std::fs::remove_dir_all(d);
    }
}
