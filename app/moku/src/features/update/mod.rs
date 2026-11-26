//! Feature `update` — v0.2 track I.
//!
//! Looks for a newer Moku on GitHub Releases and offers to install it:
//!
//! * **Check**: service `update`, only while `update.check = true` (Settings → Tentang) **and**
//!   this build can verify releases: `moku_core::release::REPO` is a real repository and
//!   `UPDATE_PUBLIC_KEY` is set (the placeholder repository or a missing key keeps the updater
//!   off; that is logged once at start). The first check waits a jittered 1–4 minutes after start, then
//!   at most one check a day (`update.json` in the data folder keeps the time of the last one;
//!   a failed check retries after 6 h). The only request is
//!   `GET https://api.github.com/repos/<REPO>/releases/latest`.
//! * **Pill**: a newer semver shows "Moku x.y.z tersedia" with "Perbarui" (and a close button;
//!   the next daily check offers it again).
//! * **Perbarui**: downloads `moku-setup.exe.sha256`, `moku-setup.exe.sig` and `moku-setup.exe`
//!   from the release (HTTPS, allowlisted hosts only, redirects checked: `fetch.rs`) into
//!   `%TEMP%\moku-update\`. The installer is only ever run when **both** hold: its SHA-256 (hashed
//!   while downloading and again from disk with writes denied, Windows CNG, `sha256.rs`) equals the
//!   `.sha256` file, and the `.sig` file is a valid ECDSA P-256 signature of that digest by the key
//!   built into the program (`sig.rs`: the checksum alone only proves the download is intact, the
//!   signature proves who made it). A release without a signature is never offered. The installer
//!   is started with `/S /update`; it stops this island, swaps the program folder and starts the
//!   new one. A failed check deletes the file, logs it and shows "Pembaruan gagal". A copy of Moku
//!   that does not run from the install folder (portable zip, dev build) opens the release page
//!   instead.
//!
//! **Manual check** (Settings → Tentang "Periksa sekarang", `mokuctl update check`): the same one
//! request, made now, also while "Periksa pembaruan otomatis" is off (the user asked), and only
//! when the updater is built in. A successful manual check is stamped like the daily one (the
//! next automatic check comes a day later); a failed one leaves the stamp alone. The result is a
//! short pill ("Moku sudah terbaru (x.y.z)", the usual offer pill, or "Gagal memeriksa
//! pembaruan" with a few words of why) and is kept in the status the settings app reads over the
//! pipe (`Request::Update`): `status`, `check`, `install` (the same install as the pill's
//! "Perbarui"). The settings app polls `status` only while a check or download is under way.
//!
//! Test hooks (`MOKU_DEBUG=1`): `update.sample` shows the pill for a fake 9.9.9;
//! `update.debug_feed:<path>` runs the "is it newer" step on a local release JSON file.

mod fetch;
pub mod sha256;
pub mod sig;

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use moku_core::ipc::{Request, Response, UpdateAction, UpdateFailure, UpdateState as Wire, UpdateStatus};
use moku_core::release::{self, Release};
use serde::{Deserialize, Serialize};

use crate::activity::{Activity, priority};
use crate::app::{App, Redraw, Ui};
use crate::i18n::{tr, tr_s};
use crate::ipc_server::Reply;
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::*;

const PILL_ID: &str = "update.available";
const NEWS_ID: &str = "update.news";
/// How long the offer outranks other activities when it first appears.
const NEWS_SECS: f32 = 12.0;
const DAY: u64 = 24 * 60 * 60;
/// After a failed check, try again this much later (instead of a day).
const RETRY_AFTER: u64 = 6 * 60 * 60;
/// Longest single sleep of the service (re-evaluates after sleep / hibernate and clock changes).
const MAX_SLEEP: u64 = 6 * 60 * 60;
/// Largest installer accepted.
const MAX_SETUP_BYTES: u64 = 256 * 1024 * 1024;
const FAIL_SECS: f32 = 8.0;
/// How long "Moku sudah terbaru" / "Gagal memeriksa pembaruan" stay after a manual check.
const RESULT_SECS: f32 = 5.0;

#[derive(Default)]
struct UpdateState {
    /// The newer release on offer (the pill is shown while this is `Some`).
    offer: Option<Release>,
    phase: Phase,
    /// A manual check is running.
    checking: bool,
    /// How the last check (or install) ended and when (Unix seconds), since the island started.
    last: Option<(u64, Last)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Last {
    UpToDate,
    /// Found a newer release (it may have been dismissed since).
    Offered,
    Failed(UpdateFailure),
}

#[derive(Default, Clone, PartialEq)]
enum Phase {
    #[default]
    Offer,
    /// Downloading: (received, total) bytes.
    Downloading(u64, u64),
    /// Verified and started the installer; it will close this island.
    Installing,
}

pub fn register(r: &mut Registry) {
    r.add_state(UpdateState::default());
    r.on_action("update", on_action);
    r.on_start(on_start);
    r.add_ipc_handler(IpcKind::Update, on_ipc);
    r.add_service(ServiceSpec { name: "update", enabled: |c| release::updater_available(c.update.check, release::REPO, release::UPDATE_PUBLIC_KEY), run: service });
}

fn on_start(app: &mut App) {
    if app.config().update.check {
        if release::is_placeholder(release::REPO) {
            crate::log_info!("update: repository is still the placeholder {:?}; the updater is off", release::REPO);
        } else if release::UPDATE_PUBLIC_KEY.is_none() {
            crate::log_info!("update: this build has no public key to verify releases with; the updater is off");
        }
    }
}

// ------------------------------------------------------------------------------------- pill

fn show_offer(app: &mut App, rel: Release) {
    crate::log_info!("update: Moku {} is available (running {})", rel.version, moku_core::VERSION);
    let st = app.state_mut::<UpdateState>();
    if st.phase != Phase::Offer {
        return; // a download for an earlier offer is under way
    }
    st.offer = Some(rel);
    // News first: a notification-level pill for a few seconds (above music), then it stays as a
    // low-priority persistent activity (the split bubble while something else plays).
    app.alert(NEWS_ID, priority::NOTIFICATION, NEWS_SECS, update_pill);
    app.set_activity(Activity::persistent(PILL_ID, priority::HEADS_UP, update_pill));
}

fn clear_offer(app: &mut App) {
    app.state_mut::<UpdateState>().offer = None;
    app.clear_activity(NEWS_ID);
    app.clear_activity(PILL_ID);
}

fn update_pill(app: &App) -> Node {
    let st = app.state::<UpdateState>();
    let Some(rel) = &st.offer else { return pill([]) };
    let v = rel.version.to_string();
    match st.phase {
        Phase::Offer => pill([
            icon(icons::DOWNLOAD).color(app.base_accent()),
            text(tr_s(format!("Moku {v} tersedia"), format!("Moku {v} available"))).semibold(),
            button_primary(tr("Perbarui", "Update"), "update.install"),
            icon_button(icons::CLOSE, "update.dismiss").name(tr("Nanti", "Later")),
        ]),
        Phase::Downloading(got, total) => {
            let frac = if total > 0 { (got as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
            pill([
                icon(icons::DOWNLOAD).color(app.base_accent()),
                text(tr_s(format!("Mengunduh Moku {v}…"), format!("Downloading Moku {v}…"))).semibold(),
                progress(frac).w(90.0).color(app.base_accent()),
            ])
        }
        Phase::Installing => pill([icon(icons::SYNC).color(app.base_accent()), text(tr("Memasang pembaruan…", "Installing the update…")).semibold()]),
    }
}

fn fail(app: &mut App, why: &str) {
    crate::log_error!("update: {why}");
    let st = app.state_mut::<UpdateState>();
    st.phase = Phase::Offer;
    st.last = Some((unix_now(), Last::Failed(UpdateFailure::Install)));
    clear_offer(app);
    app.alert("update.failed", priority::WARNING, FAIL_SECS, |_| {
        pill([icon(icons::WARNING).color(color::ORANGE), text(tr("Pembaruan gagal", "Update failed")).semibold(), caption(tr("lihat log", "see the log"))])
    });
}

// ------------------------------------------------------------------------------------ actions

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    match ev.name() {
        "install" => install(app),
        "dismiss" => {
            let st = app.state_mut::<UpdateState>();
            if st.phase == Phase::Offer {
                clear_offer(app);
            }
        }
        "sample" => {
            let rel = Release {
                tag: "v9.9.9".into(),
                version: release::Version::parse("9.9.9").unwrap_or_else(|| release::Version { major: 9, minor: 9, patch: 9, pre: vec![] }),
                html_url: format!("{}/releases/tag/v9.9.9", release::repo_url(release::REPO)),
                setup_url: format!("{}/releases/download/v9.9.9/{}", release::repo_url(release::REPO), release::SETUP_ASSET),
                sha256_url: format!("{}/releases/download/v9.9.9/{}", release::repo_url(release::REPO), release::SHA256_ASSET),
                sig_url: format!("{}/releases/download/v9.9.9/{}", release::repo_url(release::REPO), release::SIG_ASSET),
                setup_size: 0,
            };
            show_offer(app, rel);
        }
        "debug_feed" => {
            let Some(path) = ev.arg().map(PathBuf::from) else { return };
            std::thread::spawn(move || {
                let r = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| offer_from_json(&t, moku_core::VERSION));
                Ui::post(move |app| match r {
                    Ok(Some(rel)) => show_offer(app, rel),
                    Ok(None) => crate::log_info!("update: debug feed is not newer than {}", moku_core::VERSION),
                    Err(e) => crate::log_warn!("update: debug feed: {e}"),
                });
            });
        }
        _ => {}
    }
}

fn install(app: &mut App) {
    let st = app.state_mut::<UpdateState>();
    let Some(rel) = st.offer.clone() else { return };
    if st.phase != Phase::Offer {
        return;
    }
    if !release::updater_built_in(release::REPO, release::UPDATE_PUBLIC_KEY) {
        fail(app, "this build has no release repository or no signing key set; nothing to download");
        return;
    }
    let layout = moku_core::install::Layout::from_env();
    let installed = std::env::current_exe().is_ok_and(|exe| moku_core::install::path_is_within(&exe, &layout.install_dir));
    if !installed {
        // Portable or development copy: let the user pick it up from the release page.
        crate::log_info!("update: not running from {}; opening the release page", layout.install_dir.display());
        clear_offer(app);
        if rel.html_url.starts_with("https://github.com/") && release::is_allowed_url(&rel.html_url) {
            let url = rel.html_url.clone();
            app.run_outside(move || open_url(&url));
        }
        return;
    }
    app.state_mut::<UpdateState>().phase = Phase::Downloading(0, rel.setup_size);
    app.request_redraw(Redraw::Pill);
    std::thread::spawn(move || {
        let r = download_verify_run(&rel);
        Ui::post(move |app| match r {
            Ok(()) => {
                app.state_mut::<UpdateState>().phase = Phase::Installing;
                app.request_redraw(Redraw::Pill);
            }
            Err(e) => fail(app, &e),
        });
    });
}

// ------------------------------------------------------------------------- IPC + manual check

/// What a `check` request does.
#[derive(Debug, PartialEq, Eq)]
enum CheckPlan {
    Start,
    /// Nothing to do: a check is already running.
    AlreadyChecking,
    /// Nothing to do: an offer is being downloaded or installed.
    Busy,
    /// This build has no updater (no repository or no signing key): nothing may be fetched.
    NotBuiltIn,
}

/// Decide a manual check. It depends only on the build and the updater's own state: whether
/// "Periksa pembaruan otomatis" is on is not asked (the user pressed the button).
fn plan_check(built_in: bool, st: &UpdateState) -> CheckPlan {
    if !built_in {
        CheckPlan::NotBuiltIn
    } else if st.phase != Phase::Offer {
        CheckPlan::Busy
    } else if st.checking {
        CheckPlan::AlreadyChecking
    } else {
        CheckPlan::Start
    }
}

/// What an `install` request does.
#[derive(Debug, PartialEq, Eq)]
enum InstallPlan {
    Go,
    /// Already downloading or installing.
    Busy,
    /// No release on offer.
    NoOffer,
    NotBuiltIn,
}

fn plan_install(built_in: bool, st: &UpdateState) -> InstallPlan {
    if !built_in {
        InstallPlan::NotBuiltIn
    } else if st.phase != Phase::Offer {
        InstallPlan::Busy
    } else if st.offer.is_none() {
        InstallPlan::NoOffer
    } else {
        InstallPlan::Go
    }
}

/// The status the settings app reads. Installing and downloading come first, then a running
/// check; an offer on hand outranks the outcome of the last check (a failed re-check does not
/// take the offer away).
fn status_of(st: &UpdateState, current: &str) -> UpdateStatus {
    let (state, progress) = match st.phase {
        Phase::Installing => (Wire::Installing, None),
        Phase::Downloading(got, total) => (Wire::Downloading, (total > 0).then(|| (got as f32 / total as f32).clamp(0.0, 1.0))),
        Phase::Offer if st.checking => (Wire::Checking, None),
        Phase::Offer if st.offer.is_some() => (Wire::Available, None),
        Phase::Offer => match st.last {
            Some((_, Last::Failed(_))) => (Wire::Failed, None),
            Some((_, Last::UpToDate)) => (Wire::UpToDate, None),
            _ => (Wire::Idle, None),
        },
    };
    let error = match (state, st.last) {
        (Wire::Failed, Some((_, Last::Failed(f)))) => Some(f),
        _ => None,
    };
    UpdateStatus {
        state,
        current: current.to_string(),
        version: st.offer.as_ref().map(|r| r.version.to_string()).filter(|_| matches!(state, Wire::Available | Wire::Downloading | Wire::Installing | Wire::Checking)),
        error,
        progress,
        checked_at: st.last.map(|(at, _)| at),
    }
}

/// Should a successful manual check move the daily stamp? Like the daily check itself: a
/// success counts as today's check, a failure changes nothing (the daily retry rule stays).
fn stamp_after_manual_check(ok: bool, now: u64) -> Option<Stamp> {
    ok.then_some(Stamp { last_check: now })
}

fn on_ipc(app: &mut App, req: Request, reply: Reply) {
    let Request::Update { action } = req else { return reply.error("not an update request") };
    let built_in = release::updater_built_in(release::REPO, release::UPDATE_PUBLIC_KEY);
    match action {
        UpdateAction::Status => {}
        UpdateAction::Check => match plan_check(built_in, app.state::<UpdateState>()) {
            CheckPlan::NotBuiltIn => return reply.error("this build has no updater"),
            CheckPlan::Start => start_manual_check(app),
            CheckPlan::AlreadyChecking | CheckPlan::Busy => {}
        },
        UpdateAction::Install => match plan_install(built_in, app.state::<UpdateState>()) {
            InstallPlan::NotBuiltIn => return reply.error("this build has no updater"),
            InstallPlan::NoOffer => return reply.error("no update is on offer"),
            InstallPlan::Go => install(app),
            InstallPlan::Busy => {}
        },
    }
    reply.send(Response::Update { status: status_of(app.state::<UpdateState>(), moku_core::VERSION) });
}

fn start_manual_check(app: &mut App) {
    app.state_mut::<UpdateState>().checking = true;
    crate::log_info!("update: manual check");
    std::thread::spawn(move || {
        let r = fetch_latest(&|| false);
        if let Some(s) = stamp_after_manual_check(r.is_ok(), unix_now()) {
            save_stamp(&stamp_file(), &s);
        }
        Ui::post(move |app| finish_check(app, r, true));
    });
}

/// A check ended (the daily service, or a manual one). Keeps the result for the status; a manual
/// check also answers with a short pill.
fn finish_check(app: &mut App, r: Result<Option<Release>, String>, manual: bool) {
    let now = unix_now();
    if manual {
        app.state_mut::<UpdateState>().checking = false;
    }
    match r {
        Ok(Some(rel)) => {
            app.state_mut::<UpdateState>().last = Some((now, Last::Offered));
            show_offer(app, rel);
        }
        Ok(None) => {
            crate::log_info!("update: Moku {} is up to date", moku_core::VERSION);
            let st = app.state_mut::<UpdateState>();
            st.last = Some((now, Last::UpToDate));
            if manual {
                if st.phase == Phase::Offer && st.offer.is_some() {
                    clear_offer(app); // the release it pointed at is gone
                }
                app.alert("update.checked", priority::NOTIFICATION, RESULT_SECS, |app| {
                    let v = moku_core::VERSION;
                    pill([icon(icons::CHECK).color(app.base_accent()), text(tr_s(format!("Moku sudah terbaru ({v})"), format!("Moku is up to date ({v})"))).semibold()])
                });
            }
        }
        Err(e) => {
            crate::log_warn!("update: check failed: {e}");
            let why = UpdateFailure::classify(&e);
            app.state_mut::<UpdateState>().last = Some((now, Last::Failed(why)));
            if manual {
                app.alert("update.check_failed", priority::WARNING, RESULT_SECS, move |_| {
                    pill([
                        icon(icons::WARNING).color(color::ORANGE),
                        text(tr("Gagal memeriksa pembaruan", "Could not check for updates")).semibold(),
                        caption(tr(why.text(false), why.text(true))),
                    ])
                });
            }
        }
    }
}

fn open_url(url: &str) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(url), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

// ------------------------------------------------------------------------- download + verify

fn download_verify_run(rel: &Release) -> Result<(), String> {
    let never = || false;
    // 1. The expected digest, and the signature of that digest: checked before the big download,
    //    so an unsigned or wrongly signed release costs nothing.
    let mut sha_text = Vec::new();
    fetch::get(&rel.sha256_url, Duration::from_secs(30), 4096, &never, |c, _| {
        sha_text.extend_from_slice(c);
        Ok(())
    })?;
    let expected = release::parse_sha256_file(&String::from_utf8_lossy(&sha_text), release::SETUP_ASSET).ok_or("the .sha256 file is not valid")?;
    let mut sig_text = Vec::new();
    fetch::get(&rel.sig_url, Duration::from_secs(30), 1024, &never, |c, _| {
        sig_text.extend_from_slice(c);
        Ok(())
    })?;
    let sig_text = String::from_utf8_lossy(&sig_text).into_owned();
    let check = |digest: &[u8; 32]| sig::verify_release(release::UPDATE_PUBLIC_KEY, Some(&sig_text), digest);
    check(&expected).map_err(|e| format!("release {} refused: {e}", rel.tag))?;

    // 2. The installer, hashed while it streams to a .part file.
    let dir = std::env::temp_dir().join("moku-update");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let exe = dir.join(format!("moku-setup-{}.exe", rel.version));
    let part = exe.with_extension("exe.part");
    let result = (|| {
        let mut file = std::fs::File::create(&part).map_err(|e| format!("create {}: {e}", part.display()))?;
        let mut hash = sha256::Sha256::new()?;
        let mut last_post = 0u64;
        fetch::get(&rel.setup_url, Duration::from_secs(60), MAX_SETUP_BYTES, &never, |c, p| {
            file.write_all(c).map_err(|e| format!("write: {e}"))?;
            hash.update(c)?;
            if p.received - last_post >= 512 * 1024 {
                last_post = p.received;
                let (got, total) = (p.received, p.total.unwrap_or(rel.setup_size));
                Ui::post(move |app| {
                    let st = app.state_mut::<UpdateState>();
                    if matches!(st.phase, Phase::Downloading(..)) {
                        st.phase = Phase::Downloading(got, total);
                        app.request_redraw(Redraw::Pill);
                    }
                });
            }
            Ok(())
        })?;
        file.sync_all().map_err(|e| format!("flush: {e}"))?;
        drop(file);
        sha256::check(&hash.finish()?, &expected)?;
        let _ = std::fs::remove_file(&exe);
        std::fs::rename(&part, &exe).map_err(|e| format!("rename: {e}"))
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(format!("download of {} refused: {e}", fetch::redact(&rel.setup_url)));
    }

    // 3. Verify again from disk through a handle that denies writes, and keep it open until the
    //    installer has started, so the file that runs is the file that was checked.
    run_verified(&exe, &expected, &check, &["/S", "/update"]).inspect_err(|_| {
        let _ = std::fs::remove_file(&exe);
    })
}

/// Hash `exe` through a handle that allows other readers only, then start it with `args` while
/// that handle is still open. Nothing runs unless the digest matches `expected` AND `signed`
/// accepts that digest (the signature check: [`sig::verify_release`]).
pub fn run_verified(exe: &Path, expected: &[u8; 32], signed: &dyn Fn(&[u8; 32]) -> Result<(), String>, args: &[&str]) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    let mut f = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(exe).map_err(|e| format!("open {}: {e}", exe.display()))?;
    let got = sha256::digest_reader(std::io::BufReader::new(Read::by_ref(&mut f)))?;
    sha256::check(&got, expected)?;
    signed(&got)?;
    f.rewind().map_err(|e| e.to_string())?;
    let child = std::process::Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn().map_err(|e| format!("start installer: {e}"))?;
    crate::log_info!("update: verified installer started (pid {})", child.id());
    drop(f);
    Ok(())
}

// ------------------------------------------------------------------------------------ service

#[derive(Default, Serialize, Deserialize)]
struct Stamp {
    /// Unix seconds of the last check (successful, or set back for a retry).
    last_check: u64,
}

fn stamp_file() -> PathBuf {
    moku_core::paths::data_file("update.json")
}

fn load_stamp(path: &Path) -> Stamp {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_stamp(path: &Path, s: &Stamp) {
    let Ok(json) = serde_json::to_vec(s) else { return };
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
        crate::log_warn!("update: cannot write {}", path.display());
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Seconds until the next check is due (0 = now). A stamp in the future (clock set back) counts
/// as due, so a wrong clock cannot silence the updater for long.
fn next_check_in(last_check: u64, now: u64) -> u64 {
    if last_check > now + 60 {
        return 0;
    }
    (last_check + DAY).saturating_sub(now)
}

/// The start delay: 60–240 s, spread by `seed` so many machines do not all ask at boot.
fn jitter_secs(seed: u64) -> u64 {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    60 + x % 181
}

/// Is `json` (a `releases/latest` body) a release newer than `current`?
fn offer_from_json(json: &str, current: &str) -> Result<Option<Release>, String> {
    let rel = release::parse_latest_release(json)?;
    Ok(release::is_newer(&rel.version.to_string(), current).then_some(rel))
}

/// The one request: `GET releases/latest`, and is it newer than this island?
fn fetch_latest(stop: &dyn Fn() -> bool) -> Result<Option<Release>, String> {
    let mut body = Vec::new();
    fetch::get(&release::latest_release_api(release::REPO), Duration::from_secs(20), 1024 * 1024, stop, |c, _| {
        body.extend_from_slice(c);
        Ok(())
    })?;
    offer_from_json(&String::from_utf8_lossy(&body), moku_core::VERSION)
}

fn service(ctx: ServiceCtx) {
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0) ^ std::process::id() as u64;
    if !ctx.sleep(Duration::from_secs(jitter_secs(seed))) {
        return;
    }
    let path = stamp_file();
    loop {
        let now = unix_now();
        let wait = next_check_in(load_stamp(&path).last_check, now);
        if wait > 0 {
            if !ctx.sleep(Duration::from_secs(wait.min(MAX_SLEEP))) {
                return;
            }
            continue;
        }
        let r = fetch_latest(&|| ctx.should_stop());
        if ctx.should_stop() {
            return;
        }
        let last_check = match &r {
            Ok(_) => now,
            Err(_) => now.saturating_sub(DAY - RETRY_AFTER),
        };
        save_stamp(&path, &Stamp { last_check });
        ctx.post(move |app| finish_check(app, r, false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_at_most_once_a_day() {
        let now = 1_800_000_000;
        assert_eq!(next_check_in(0, now), 0, "never checked");
        assert_eq!(next_check_in(now - DAY, now), 0);
        assert_eq!(next_check_in(now - DAY - 5, now), 0);
        assert_eq!(next_check_in(now - 3600, now), DAY - 3600);
        assert_eq!(next_check_in(now, now), DAY);
        assert_eq!(next_check_in(now + 30, now), DAY + 30, "small clock skew keeps the stamp");
        assert_eq!(next_check_in(now + 10 * DAY, now), 0, "a stamp far in the future is ignored");
        // A failed check is stamped so the retry comes after RETRY_AFTER.
        assert_eq!(next_check_in(now - (DAY - RETRY_AFTER), now), RETRY_AFTER);
    }

    #[test]
    fn start_is_jittered() {
        let v: Vec<u64> = (0..200).map(jitter_secs).collect();
        assert!(v.iter().all(|s| (60..=240).contains(s)));
        let distinct: std::collections::HashSet<_> = v.iter().collect();
        assert!(distinct.len() > 50, "spread out: {}", distinct.len());
    }

    #[test]
    fn offer_only_for_newer_releases() {
        let json = |tag: &str| {
            format!(
                r#"{{"tag_name":"{tag}","html_url":"https://github.com/a/moku/releases/tag/{tag}","draft":false,"prerelease":false,
                "assets":[{{"name":"moku-setup.exe","size":1,"browser_download_url":"https://github.com/a/moku/releases/download/{tag}/moku-setup.exe"}},
                          {{"name":"moku-setup.exe.sha256","size":1,"browser_download_url":"https://github.com/a/moku/releases/download/{tag}/moku-setup.exe.sha256"}},
                          {{"name":"moku-setup.exe.sig","size":129,"browser_download_url":"https://github.com/a/moku/releases/download/{tag}/moku-setup.exe.sig"}}]}}"#
            )
        };
        assert_eq!(offer_from_json(&json("v0.3.0"), "0.2.0").unwrap().map(|r| r.tag), Some("v0.3.0".into()));
        assert!(offer_from_json(&json("v0.2.0"), "0.2.0").unwrap().is_none());
        assert!(offer_from_json(&json("v0.1.0"), "0.2.0").unwrap().is_none());
        assert!(offer_from_json("{}", "0.2.0").is_err());
    }

    #[test]
    fn placeholder_slug_keeps_the_service_off() {
        let mut c = moku_core::Config::default();
        assert!(c.update.check, "on by default");
        // The registered `enabled` predicate with the placeholder repository (the constant, not
        // REPO: setting the real slug later must not break this test) and a well-formed key.
        let key = Some("00".repeat(64));
        let enabled = |c: &moku_core::Config, repo: &str, key: Option<&str>| release::updater_available(c.update.check, repo, key);
        assert!(release::is_placeholder(release::PLACEHOLDER_REPO));
        assert!(!enabled(&c, release::PLACEHOLDER_REPO, key.as_deref()));
        // A real repository but no signing key: still off.
        assert!(!enabled(&c, "moku-app/moku", None));
        assert!(enabled(&c, "moku-app/moku", key.as_deref()));
        c.update.check = false;
        assert!(!enabled(&c, "moku-app/moku", key.as_deref()));
    }

    fn rel(v: &str) -> Release {
        Release {
            tag: format!("v{v}"),
            version: release::Version::parse(v).unwrap(),
            html_url: String::new(),
            setup_url: String::new(),
            sha256_url: String::new(),
            sig_url: String::new(),
            setup_size: 1000,
        }
    }

    #[test]
    fn a_manual_check_runs_whenever_the_updater_is_built_in_and_idle() {
        let mut st = UpdateState::default();
        assert_eq!(plan_check(true, &st), CheckPlan::Start);
        // The automatic check being off is not an input: the button is an explicit request.
        assert_eq!(plan_check(false, &st), CheckPlan::NotBuiltIn, "no updater in this build: nothing is fetched");
        st.checking = true;
        assert_eq!(plan_check(true, &st), CheckPlan::AlreadyChecking, "one request at a time");
        st.checking = false;
        st.offer = Some(rel("0.3.2"));
        assert_eq!(plan_check(true, &st), CheckPlan::Start, "re-checking with an offer on hand is fine");
        st.phase = Phase::Downloading(1, 2);
        assert_eq!(plan_check(true, &st), CheckPlan::Busy);
        st.phase = Phase::Installing;
        assert_eq!(plan_check(true, &st), CheckPlan::Busy);
    }

    #[test]
    fn install_needs_an_offer_and_an_updater() {
        let mut st = UpdateState::default();
        assert_eq!(plan_install(true, &st), InstallPlan::NoOffer);
        st.offer = Some(rel("0.3.2"));
        assert_eq!(plan_install(true, &st), InstallPlan::Go);
        assert_eq!(plan_install(false, &st), InstallPlan::NotBuiltIn);
        st.phase = Phase::Downloading(0, 1);
        assert_eq!(plan_install(true, &st), InstallPlan::Busy, "a second click does not start a second download");
    }

    #[test]
    fn the_status_follows_the_updater() {
        let mut st = UpdateState::default();
        let s = status_of(&st, "0.3.1");
        assert_eq!((s.state, s.current.as_str(), s.version.clone(), s.checked_at), (Wire::Idle, "0.3.1", None, None));

        st.checking = true;
        assert_eq!(status_of(&st, "0.3.1").state, Wire::Checking);
        st.checking = false;

        st.last = Some((100, Last::UpToDate));
        let s = status_of(&st, "0.3.1");
        assert_eq!((s.state, s.checked_at), (Wire::UpToDate, Some(100)));

        st.last = Some((200, Last::Failed(UpdateFailure::Network)));
        let s = status_of(&st, "0.3.1");
        assert_eq!((s.state, s.error, s.version), (Wire::Failed, Some(UpdateFailure::Network), None));

        // An offer outranks the last outcome and carries its version.
        st.offer = Some(rel("0.3.2"));
        st.last = Some((300, Last::Offered));
        let s = status_of(&st, "0.3.1");
        assert_eq!((s.state, s.version.as_deref(), s.error), (Wire::Available, Some("0.3.2"), None));
        st.last = Some((310, Last::Failed(UpdateFailure::RateLimited)));
        assert_eq!(status_of(&st, "0.3.1").state, Wire::Available, "a failed re-check does not take the offer away");

        st.phase = Phase::Downloading(250, 1000);
        let s = status_of(&st, "0.3.1");
        assert_eq!((s.state, s.progress), (Wire::Downloading, Some(0.25)));
        st.phase = Phase::Downloading(5, 0);
        assert_eq!(status_of(&st, "0.3.1").progress, None, "unknown size: no fraction");
        st.phase = Phase::Downloading(5000, 1000);
        assert_eq!(status_of(&st, "0.3.1").progress, Some(1.0), "clamped");
        st.phase = Phase::Installing;
        let s = status_of(&st, "0.3.1");
        assert_eq!((s.state, s.version.as_deref(), s.progress), (Wire::Installing, Some("0.3.2"), None));

        // Dismissed offer: back to what the last check said, not "up to date".
        st.phase = Phase::Offer;
        st.offer = None;
        st.last = Some((300, Last::Offered));
        assert_eq!(status_of(&st, "0.3.1").state, Wire::Idle);
    }

    #[test]
    fn a_manual_check_stamps_only_on_success() {
        assert_eq!(stamp_after_manual_check(true, 42).map(|s| s.last_check), Some(42));
        assert!(stamp_after_manual_check(false, 42).is_none(), "a failed check leaves the daily stamp alone");
    }

    #[test]
    fn stamp_roundtrip() {
        let dir = std::env::temp_dir().join(format!("moku-update-stamp-{}", std::process::id()));
        let p = dir.join("update.json");
        assert_eq!(load_stamp(&p).last_check, 0, "missing file");
        save_stamp(&p, &Stamp { last_check: 42 });
        assert_eq!(load_stamp(&p).last_check, 42);
        std::fs::write(&p, "garbage").unwrap();
        assert_eq!(load_stamp(&p).last_check, 0, "corrupt file = never checked");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that fails verification is never started.
    #[test]
    fn mismatching_installer_is_not_run() {
        let dir = std::env::temp_dir().join(format!("moku-update-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A copy of a harmless system exe; if it did start, it would exit at once anyway.
        let exe = dir.join("moku-setup.exe");
        let sys = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        std::fs::copy(Path::new(&sys).join("System32").join("where.exe"), &exe).unwrap();
        let accept = |_: &[u8; 32]| Ok(());
        let wrong = [7u8; 32];
        let e = run_verified(&exe, &wrong, &accept, &["/?"]).unwrap_err();
        assert!(e.contains("mismatch"), "{e}");
        // The right digest but a signature that is not good: not run either.
        let right = sha256::digest(&std::fs::read(&exe).unwrap()).unwrap();
        let reject = |_: &[u8; 32]| Err("the signature does not match".to_string());
        let e = run_verified(&exe, &right, &reject, &["/?"]).unwrap_err();
        assert!(e.contains("signature"), "{e}");
        // With the right digest AND a signed digest it starts (where.exe /? prints help and exits).
        assert!(run_verified(&exe, &right, &accept, &["/?"]).is_ok());
        std::thread::sleep(Duration::from_millis(300));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
