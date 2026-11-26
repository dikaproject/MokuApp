//! Settings → Tentang → "Pembaruan" (v0.2 track I, and "Periksa sekarang" / "Perbarui sekarang").
//!
//! Two parts:
//!
//! * the switch "Periksa pembaruan otomatis" (`update.check`), written through the core's
//!   read-modify-write (`Config::update_file`), so it never overwrites what the form saved;
//! * the status line and the two buttons, which talk to the **running island** over its pipe
//!   (`Request::Update`: `status`, `check`, `install`; the island does the work, this app never
//!   touches GitHub). Everything that decides what is shown or done is plain functions
//!   ([`describe`], [`link_from`], [`next_step`], [`Poller`]) tested without a window or a pipe.
//!
//! The status is read once when the page opens, after each button, and then once a second only
//! while the page is visible AND a check or a download is under way (no idle polling: when
//! nothing is happening nothing runs). Without a reachable island the page says so in one line
//! and both buttons are off.
//!
//! Its own Slint global (`UpdateCfg`, `ui/pages-i.slint`) instead of a `Form` field, so the
//! shared form stays untouched.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use moku_core::Config;
use moku_core::ipc::{IpcError, Request, Response, UpdateAction, UpdateState, UpdateStatus};
use moku_core::release::{REPO, UPDATE_PUBLIC_KEY, updater_built_in};
use slint::{ComponentHandle, Weak};

use crate::{Cfg, SettingsWindow, UpdateCfg};

/// How often the status is read while something is in progress.
const POLL: Duration = Duration::from_secs(1);

pub fn wire(ui: &SettingsWindow, config: &Config, sample: Option<&str>) {
    let g = ui.global::<UpdateCfg>();
    g.set_check(config.update.check);
    g.set_available(available());
    g.set_repo(REPO.into());
    let fallback = config.clone();
    g.on_set_check(move |on| {
        if let Err(e) = set_check(&moku_core::paths::config_file(), &fallback, on) {
            eprintln!("cannot save update.check: {e}");
        }
    });

    if let Some(name) = sample {
        // Screenshots only (`--update-sample <state>`): a made-up answer, the pipe is never touched.
        if let Some(link) = sample_link(name) {
            apply(ui, &link, SAMPLE_NOW);
        }
        return;
    }
    if !available() {
        return;
    }
    let poller = Arc::new(Poller::default());
    {
        let (w, p) = (ui.as_weak(), poller.clone());
        g.on_shown(move || request(w.clone(), p.clone(), UpdateAction::Status));
    }
    {
        let (w, p) = (ui.as_weak(), poller.clone());
        g.on_check_now(move || request(w.clone(), p.clone(), UpdateAction::Check));
    }
    {
        let (w, p) = (ui.as_weak(), poller);
        g.on_install_now(move || request(w.clone(), p.clone(), UpdateAction::Install));
    }
}

/// Does this build have an updater (a real repository AND the key releases are signed with)?
/// When not, the Tentang switch is greyed out ("Belum aktif di build ini") and there are no buttons.
fn available() -> bool {
    updater_built_in(REPO, UPDATE_PUBLIC_KEY)
}

fn set_check(path: &std::path::Path, fallback: &Config, on: bool) -> std::io::Result<Config> {
    Config::update_file(path, fallback, |c| c.update.check = on)
}

// ------------------------------------------------------------------------------ the pipe

/// What this app knows about the island's updater.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// Not asked yet.
    Unknown,
    /// The island answered.
    Up(UpdateStatus),
    /// The island did not (the line says why).
    Down(Down),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Down {
    /// No island on the pipe.
    NotRunning,
    /// It is there but says no (IPC switched off in the island's config).
    Refused,
    /// Timed out, an unexpected answer, a pipe owned by someone else, ...
    NoAnswer,
}

/// Turn the pipe's result into what the page knows. (`send` is injected: tests use fakes.)
pub fn link_from(r: Result<Response, IpcError>) -> Link {
    match r {
        Ok(Response::Update { status }) => Link::Up(status),
        // "IPC is disabled in settings", "this build has no updater", ...: the island said no.
        Ok(Response::Error { .. }) => Link::Down(Down::Refused),
        Ok(_) => Link::Down(Down::NoAnswer),
        Err(IpcError::NotRunning) => Link::Down(Down::NotRunning),
        Err(_) => Link::Down(Down::NoAnswer),
    }
}

/// Ask the island something.
pub fn ask(action: UpdateAction, send: &dyn Fn(&Request) -> Result<Response, IpcError>) -> Link {
    link_from(send(&Request::Update { action }))
}

// ------------------------------------------------------------------------- what the page shows

/// Everything the page needs to draw, computed from the [`Link`].
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    /// The status line under "Moku <version>".
    pub status: String,
    /// The line is about something that went wrong (drawn in the error colour).
    pub problem: bool,
    pub can_check: bool,
    pub can_install: bool,
    /// The download fraction 0..1, or -1 when there is no progress bar (also for an unknown size:
    /// the status line then has no percentage and the bar stays hidden).
    pub progress: f32,
}

/// "baru saja", "3 menit lalu", "2 jam lalu", "4 hari lalu".
pub fn ago(secs: u64) -> String {
    match secs {
        0..=59 => "baru saja".into(),
        60..=3599 => format!("{} menit lalu", secs / 60),
        3600..=86399 => format!("{} jam lalu", secs / 3600),
        _ => format!("{} hari lalu", secs / 86400),
    }
}

fn checked(st: &UpdateStatus, now: u64) -> String {
    match st.checked_at {
        Some(at) => format!("dicek {}", ago(now.saturating_sub(at))),
        None => String::new(),
    }
}

/// The page for `link` at time `now` (Unix seconds). `built_in`: the build has an updater at all.
pub fn describe(link: &Link, built_in: bool, now: u64) -> View {
    let none = View { status: String::new(), problem: false, can_check: false, can_install: false, progress: -1.0 };
    if !built_in {
        return none;
    }
    let with = |s: &str| View { status: s.to_string(), ..none.clone() };
    match link {
        Link::Unknown => with("Menghubungi island…"),
        Link::Down(Down::NotRunning) => View { problem: true, ..with("Island Moku tidak berjalan") },
        Link::Down(Down::Refused) => View { problem: true, ..with("Island menolak permintaan (IPC dimatikan?)") },
        Link::Down(Down::NoAnswer) => View { problem: true, ..with("Island tidak menjawab") },
        Link::Up(st) => {
            let v = st.version.clone().unwrap_or_default();
            match st.state {
                UpdateState::Idle => View { can_check: true, ..with(&if st.checked_at.is_some() { format!("Terakhir {}", checked(st, now)) } else { "Belum diperiksa sejak Moku dibuka".to_string() }) },
                UpdateState::Checking => with("Memeriksa…"),
                UpdateState::UpToDate => View { can_check: true, ..with(&match st.checked_at {
                    Some(_) => format!("Sudah terbaru · {}", checked(st, now)),
                    None => "Sudah terbaru".to_string(),
                }) },
                UpdateState::Available => View { can_check: true, can_install: true, ..with(&format!("Versi {v} tersedia")) },
                UpdateState::Downloading => {
                    let progress = st.progress.map_or(-1.0, |p| p.clamp(0.0, 1.0));
                    let pct = if progress >= 0.0 { format!(" · {}%", (progress * 100.0).round() as u32) } else { String::new() };
                    View { progress, ..with(&format!("Mengunduh {v}{pct}")) }
                }
                UpdateState::Installing => with("Memasang pembaruan… Moku akan dimulai ulang"),
                UpdateState::Failed => {
                    let why = st.error.map_or("kesalahan tidak dikenal", |e| e.text(false));
                    View { problem: true, can_check: true, can_install: false, ..with(&format!("Gagal memeriksa: {why}")) }
                }
            }
        }
    }
}

/// Is something in progress that makes the page ask again soon?
pub fn in_progress(link: &Link) -> bool {
    matches!(link, Link::Up(st) if matches!(st.state, UpdateState::Checking | UpdateState::Downloading))
}

/// What to do after an answer arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    PollAfter(Duration),
    Stop,
}

/// Poll again only while the page is visible AND a check / download is under way.
pub fn next_step(link: &Link, page_visible: bool) -> Next {
    if page_visible && in_progress(link) { Next::PollAfter(POLL) } else { Next::Stop }
}

/// Keeps overlapping requests in order: an answer older than one already shown is dropped (a
/// `status` that left before a `check` must not undo the `checking` it would show), and at most
/// one poll is waiting at a time.
#[derive(Default)]
pub struct Poller {
    issued: AtomicU64,
    shown: AtomicU64,
    scheduled: AtomicBool,
}

impl Poller {
    /// A request is about to leave: its number.
    pub fn issue(&self) -> u64 {
        self.issued.fetch_add(1, Ordering::SeqCst) + 1
    }
    /// Its answer arrived: show it? (Not when a newer request's answer was shown already.)
    pub fn accept(&self, seq: u64) -> bool {
        self.shown.fetch_max(seq, Ordering::SeqCst) <= seq
    }
    /// A poll wants to wait: true for the first caller only.
    pub fn schedule(&self) -> bool {
        !self.scheduled.swap(true, Ordering::SeqCst)
    }
    /// The waiting poll fires.
    pub fn fired(&self) {
        self.scheduled.store(false, Ordering::SeqCst);
    }
}

// ------------------------------------------------------------------------------- the window

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The page is on screen: Tentang is the current page and the onboarding is not covering it.
fn page_visible(ui: &SettingsWindow) -> bool {
    let c = ui.global::<Cfg>();
    c.get_page() == crate::ABOUT_PAGE && !c.get_onboarding()
}

fn apply(ui: &SettingsWindow, link: &Link, now: u64) {
    let v = describe(link, available(), now);
    let g = ui.global::<UpdateCfg>();
    g.set_status(v.status.into());
    g.set_problem(v.problem);
    g.set_can_check(v.can_check);
    g.set_can_install(v.can_install);
    g.set_progress(v.progress);
}

/// Send one request off the UI thread; show the answer; poll on while it is in progress.
fn request(w: Weak<SettingsWindow>, poller: Arc<Poller>, action: UpdateAction) {
    let seq = poller.issue();
    let spawned = std::thread::Builder::new().name("update-ask".into()).spawn({
        let w = w.clone();
        move || {
            let link = ask(action, &|r| moku_core::ipc::send(r, Some(Duration::from_secs(5))));
            let _ = w.upgrade_in_event_loop(move |ui| {
                if !poller.accept(seq) {
                    return;
                }
                apply(&ui, &link, unix_now());
                if let Next::PollAfter(d) = next_step(&link, page_visible(&ui)) {
                    if poller.schedule() {
                        let (w, p) = (ui.as_weak(), poller.clone());
                        slint::Timer::single_shot(d, move || {
                            p.fired();
                            if w.upgrade().is_some_and(|ui| page_visible(&ui)) {
                                request(w, p, UpdateAction::Status);
                            }
                        });
                    }
                }
            });
        }
    });
    if spawned.is_err() {
        if let Some(ui) = w.upgrade() {
            apply(&ui, &Link::Down(Down::NoAnswer), unix_now());
        }
    }
}

// -------------------------------------------------------------------------- screenshot samples

/// A fixed "now" for the samples, so "dicek 2 menit lalu" does not depend on the clock.
const SAMPLE_NOW: u64 = 1_800_000_000;

/// `--update-sample <name>`: what the page looks like in each state (screenshots only).
fn sample_link(name: &str) -> Option<Link> {
    let st = |state, version: Option<&str>, error, progress, checked_at| Link::Up(UpdateStatus { state, current: moku_core::VERSION.into(), version: version.map(Into::into), error, progress, checked_at });
    let ago2 = Some(SAMPLE_NOW - 130);
    Some(match name {
        "unknown" => Link::Unknown,
        "idle" => st(UpdateState::Idle, None, None, None, None),
        "checking" => st(UpdateState::Checking, None, None, None, ago2),
        "uptodate" => st(UpdateState::UpToDate, None, None, None, ago2),
        "available" => st(UpdateState::Available, Some("0.3.2"), None, None, ago2),
        "downloading" => st(UpdateState::Downloading, Some("0.3.2"), None, Some(0.41), ago2),
        "installing" => st(UpdateState::Installing, Some("0.3.2"), None, None, ago2),
        "failed" => st(UpdateState::Failed, None, Some(moku_core::ipc::UpdateFailure::Network), None, ago2),
        "down" => Link::Down(Down::NotRunning),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use moku_core::ipc::UpdateFailure;

    fn status(state: UpdateState) -> UpdateStatus {
        UpdateStatus { state, current: "0.3.1".into(), version: None, error: None, progress: None, checked_at: None }
    }
    fn up(state: UpdateState, f: impl FnOnce(&mut UpdateStatus)) -> Link {
        let mut s = status(state);
        f(&mut s);
        Link::Up(s)
    }
    const NOW: u64 = 1_800_000_000;

    #[test]
    fn the_switch_is_available_only_when_the_build_can_verify_releases() {
        // Same rule as the island's updater: repository set AND public key set.
        assert_eq!(available(), !moku_core::release::is_placeholder(REPO) && UPDATE_PUBLIC_KEY.is_some());
        assert!(!updater_built_in(moku_core::release::PLACEHOLDER_REPO, Some(&"ab".repeat(64))));
        assert!(!updater_built_in("moku-app/moku", None));
    }

    #[test]
    fn toggle_writes_only_update_check() {
        let dir = std::env::temp_dir().join(format!("moku-settings-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        let mut other = Config::default();
        other.island.scale = 1.25;
        other.save_to(&p).unwrap();
        let now = set_check(&p, &Config::default(), false).unwrap();
        assert!(!now.update.check);
        let (back, _) = Config::load_from(&p);
        assert_eq!(back.island.scale, 1.25, "other settings survive");
        assert!(!back.update.check);
        set_check(&p, &Config::default(), true).unwrap();
        assert!(Config::load_from(&p).0.update.check);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn times_are_said_roughly() {
        assert_eq!(ago(0), "baru saja");
        assert_eq!(ago(59), "baru saja");
        assert_eq!(ago(60), "1 menit lalu");
        assert_eq!(ago(130), "2 menit lalu");
        assert_eq!(ago(3599), "59 menit lalu");
        assert_eq!(ago(3600), "1 jam lalu");
        assert_eq!(ago(86399), "23 jam lalu");
        assert_eq!(ago(3 * 86400 + 5), "3 hari lalu");
    }

    #[test]
    fn the_status_line_for_every_state() {
        let d = |l: &Link| describe(l, true, NOW);
        let v = d(&up(UpdateState::UpToDate, |s| s.checked_at = Some(NOW - 130)));
        assert_eq!((v.status.as_str(), v.problem, v.can_check, v.can_install), ("Sudah terbaru · dicek 2 menit lalu", false, true, false));
        // A clock set back (checked "in the future") reads as just now, never as a huge number.
        assert_eq!(d(&up(UpdateState::UpToDate, |s| s.checked_at = Some(NOW + 500))).status, "Sudah terbaru · dicek baru saja");
        assert_eq!(d(&up(UpdateState::UpToDate, |_| {})).status, "Sudah terbaru");

        let v = d(&up(UpdateState::Available, |s| s.version = Some("0.3.2".into())));
        assert_eq!((v.status.as_str(), v.can_check, v.can_install, v.progress), ("Versi 0.3.2 tersedia", true, true, -1.0));

        let v = d(&up(UpdateState::Checking, |_| {}));
        assert_eq!((v.status.as_str(), v.can_check, v.can_install), ("Memeriksa…", false, false));

        let v = d(&up(UpdateState::Downloading, |s| {
            s.version = Some("0.3.2".into());
            s.progress = Some(0.414);
        }));
        assert_eq!((v.status.as_str(), v.progress, v.can_check, v.can_install), ("Mengunduh 0.3.2 · 41%", 0.414, false, false));
        let v = d(&up(UpdateState::Downloading, |s| s.version = Some("0.3.2".into())));
        assert_eq!((v.status.as_str(), v.progress), ("Mengunduh 0.3.2", -1.0), "no size known: no bar, no percentage");
        assert_eq!(d(&up(UpdateState::Downloading, |s| s.progress = Some(7.0))).progress, 1.0, "clamped");

        let v = d(&up(UpdateState::Installing, |_| {}));
        assert!(v.status.starts_with("Memasang pembaruan") && !v.can_check && !v.can_install);

        let v = d(&up(UpdateState::Failed, |s| s.error = Some(UpdateFailure::Network)));
        assert_eq!((v.status.as_str(), v.problem, v.can_check, v.can_install), ("Gagal memeriksa: tidak bisa terhubung ke GitHub", true, true, false));
        assert_eq!(d(&up(UpdateState::Failed, |_| {})).status, "Gagal memeriksa: kesalahan tidak dikenal");

        assert_eq!(d(&up(UpdateState::Idle, |_| {})).status, "Belum diperiksa sejak Moku dibuka");
        assert_eq!(d(&up(UpdateState::Idle, |s| s.checked_at = Some(NOW - 7200))).status, "Terakhir dicek 2 jam lalu");
        assert!(d(&up(UpdateState::Idle, |_| {})).can_check);

        let v = d(&Link::Unknown);
        assert_eq!((v.status.as_str(), v.can_check, v.can_install), ("Menghubungi island…", false, false), "no buttons before the first answer");
    }

    #[test]
    fn without_a_reachable_island_the_page_says_so_and_the_buttons_are_off() {
        for (down, line) in [
            (Down::NotRunning, "Island Moku tidak berjalan"),
            (Down::Refused, "Island menolak permintaan (IPC dimatikan?)"),
            (Down::NoAnswer, "Island tidak menjawab"),
        ] {
            let v = describe(&Link::Down(down), true, NOW);
            assert_eq!((v.status.as_str(), v.problem, v.can_check, v.can_install), (line, true, false, false));
            assert!(!line.contains('\n'), "one short line");
        }
    }

    #[test]
    fn a_build_without_an_updater_has_no_status_and_no_buttons() {
        for link in [Link::Unknown, up(UpdateState::Available, |s| s.version = Some("9.9.9".into())), Link::Down(Down::NotRunning)] {
            let v = describe(&link, false, NOW);
            assert_eq!(v, View { status: String::new(), problem: false, can_check: false, can_install: false, progress: -1.0 });
        }
    }

    #[test]
    fn the_pipe_result_becomes_what_the_page_knows() {
        let ok = Response::Update { status: status(UpdateState::UpToDate) };
        assert_eq!(link_from(Ok(ok)), Link::Up(status(UpdateState::UpToDate)));
        assert_eq!(link_from(Err(IpcError::NotRunning)), Link::Down(Down::NotRunning));
        assert_eq!(link_from(Err(IpcError::Timeout)), Link::Down(Down::NoAnswer));
        assert_eq!(link_from(Err(IpcError::Untrusted("another user".into()))), Link::Down(Down::NoAnswer));
        assert_eq!(link_from(Ok(Response::Error { message: "IPC is disabled in settings".into() })), Link::Down(Down::Refused));
        assert_eq!(link_from(Ok(Response::Ok)), Link::Down(Down::NoAnswer), "an island too old to know `update` answers something else");
    }

    #[test]
    fn the_request_that_leaves_is_the_one_asked_for() {
        use std::cell::RefCell;
        let sent = RefCell::new(Vec::new());
        let fake = |r: &Request| {
            sent.borrow_mut().push(r.clone());
            Ok(Response::Update { status: status(UpdateState::Checking) })
        };
        let link = ask(UpdateAction::Check, &fake);
        ask(UpdateAction::Status, &fake);
        ask(UpdateAction::Install, &fake);
        assert_eq!(link, Link::Up(status(UpdateState::Checking)));
        assert_eq!(
            *sent.borrow(),
            vec![
                Request::Update { action: UpdateAction::Check },
                Request::Update { action: UpdateAction::Status },
                Request::Update { action: UpdateAction::Install },
            ]
        );
        // A dead island: no panic, the page gets its "not running" line.
        assert_eq!(ask(UpdateAction::Status, &|_| Err(IpcError::NotRunning)), Link::Down(Down::NotRunning));
    }

    #[test]
    fn it_polls_only_while_visible_and_something_is_in_progress() {
        let one = Next::PollAfter(Duration::from_secs(1));
        assert_eq!(next_step(&up(UpdateState::Checking, |_| {}), true), one);
        assert_eq!(next_step(&up(UpdateState::Downloading, |_| {}), true), one);
        assert_eq!(next_step(&up(UpdateState::Checking, |_| {}), false), Next::Stop, "another page is showing");
        for idle in [
            up(UpdateState::Idle, |_| {}),
            up(UpdateState::UpToDate, |_| {}),
            up(UpdateState::Available, |_| {}),
            up(UpdateState::Failed, |_| {}),
            up(UpdateState::Installing, |_| {}), // the installer closes the island: nothing to wait for
            Link::Unknown,
            Link::Down(Down::NotRunning),
        ] {
            assert_eq!(next_step(&idle, true), Next::Stop, "{idle:?}");
        }
    }

    #[test]
    fn an_old_answer_never_replaces_a_newer_one_and_only_one_poll_waits() {
        let p = Poller::default();
        let (a, b) = (p.issue(), p.issue()); // `status` leaves, then the user presses "Periksa sekarang"
        assert!(p.accept(b), "the newer answer shows");
        assert!(!p.accept(a), "the older one arrives late and is dropped");
        let c = p.issue();
        assert!(p.accept(c));
        assert!(p.schedule(), "the first poll may wait");
        assert!(!p.schedule(), "a second one does not pile up");
        p.fired();
        assert!(p.schedule(), "after it fired the next one may wait");
    }

    #[test]
    fn every_sample_state_is_known() {
        for n in ["unknown", "idle", "checking", "uptodate", "available", "downloading", "installing", "failed", "down"] {
            assert!(sample_link(n).is_some(), "{n}");
        }
        assert!(sample_link("nope").is_none());
    }
}
