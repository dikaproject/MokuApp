//! Feature `downloads` (v0.2, track J): a progress pill for browser downloads.
//!
//! A service thread watches the Downloads folder (`FOLDERID_Downloads`) with
//! `ReadDirectoryChangesW` ([`watch`]) and follows the temporary files browsers write while a
//! download runs (`.crdownload`, `.part`, `.partial`; [`tracker`]). While one grows the pill
//! shows its name, size and speed with an indeterminate ring (the percentage is unknown); when
//! the temp file is renamed to its final name the pill offers "Buka", "Folder" and "Shelf". A
//! cancelled download (the temp file is deleted) just clears the pill.
//!
//! * **Privacy**: only file names and sizes are looked at, never file contents, and names are
//!   never logged. During a meeting (when the privacy setting hides notification bodies) the
//!   pill does not show the name either.
//! * **Safety**: "Buka" is not offered for programs and scripts (`.exe`, `.bat`, `.ps1` …); the
//!   island never launches such a file, it can only show it in its folder.
//! * **Testing**: `MOKU_DOWNLOADS_DIR` points the watcher at a scratch folder; it is honoured
//!   only with `MOKU_DEBUG=1` / a debug build, so the real Downloads folder is never touched by
//!   a test and a stray environment variable cannot redirect a normal island.

mod tracker;
mod watch;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use self::tracker::{Ev, Tracker, View};
use self::watch::Fs;
use crate::activity::{Activity, priority};
use crate::app::{App, Redraw};
use crate::i18n::{self, tr};
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::*;

/// How long the "finished" pill stays (long enough to reach its buttons).
const DONE_SECS: f32 = 8.0;
/// Just above the progress priority, so a finished download replaces its own progress pill but
/// never a meeting, warning or notification.
const DONE_PRIORITY: u8 = priority::PROGRESS + 1;
/// How often sizes are looked at while a download runs.
const POLL: Duration = Duration::from_secs(1);

/// Programs and scripts are never opened from the island (the list is shared with the launcher).
use crate::safety::is_risky;

#[derive(Clone, Debug, PartialEq)]
struct Done {
    name: String,
    path: PathBuf,
    risky: bool,
}

#[derive(Default)]
struct Downloads {
    /// Downloads in progress, as the service last reported them.
    running: Vec<View>,
    /// The last finished one (its buttons act on this).
    done: Option<Done>,
}

pub fn register(r: &mut Registry) {
    r.add_state(Downloads::default());
    r.on_action("downloads", on_action);
    r.on_config(on_config);
    r.on_event(|app, ev| {
        if *ev == AppEvent::DataCleared {
            clear(app);
        }
    });
    r.add_service(ServiceSpec { name: "downloads", enabled: |c| c.downloads.enabled, run: service });
}

fn clear(app: &mut App) {
    let st = app.state_mut::<Downloads>();
    st.running.clear();
    st.done = None;
    app.clear_activity("downloads.progress");
    app.clear_activity("downloads.done");
}

fn on_config(app: &mut App) {
    if !app.config().downloads.enabled {
        clear(app);
    }
}

// ---- folder ---------------------------------------------------------------------------------------

/// The folder to watch: the test override (debug only), else the user's Downloads folder.
fn pick_dir(over: Option<OsString>, debug: bool, known: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    over.filter(|o| debug && !o.is_empty()).map(PathBuf::from).or_else(known)
}

fn known_downloads() -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty()).map(PathBuf::from)
    }
}

fn downloads_dir() -> Option<PathBuf> {
    pick_dir(std::env::var_os("MOKU_DOWNLOADS_DIR"), crate::app::debug_enabled(), known_downloads)
}

// ---- service --------------------------------------------------------------------------------------

/// What one look at the folder produced.
#[derive(Debug, Default)]
struct Step {
    /// The downloads to show, when they changed since the last step.
    running: Option<Vec<View>>,
    done: Vec<Done>,
    /// How long to wait before looking again (`None` = until the folder changes).
    wait: Option<Duration>,
}

/// The watcher's side: file events in, "what to show" out. Free of the UI so it can be tested
/// against a scratch folder.
struct Glue {
    dir: PathBuf,
    tracker: Tracker,
    shown: Vec<View>,
}

impl Glue {
    fn new(dir: PathBuf) -> Glue {
        Glue { dir, tracker: Tracker::default(), shown: vec![] }
    }

    fn size_of(&self, name: &str) -> Option<u64> {
        std::fs::metadata(self.dir.join(name)).ok().map(|m| m.len())
    }

    fn step(&mut self, batch: Vec<Fs>, now: Instant) -> Step {
        let mut finished = vec![];
        for fs in batch {
            let ev = match fs {
                Fs::Added(n) | Fs::Modified(n) if tracker::is_temp(&n) => match self.size_of(&n) {
                    Some(size) => Ev::Temp { name: n, size },
                    None => continue,
                },
                Fs::Added(n) | Fs::Modified(n) => Ev::File { name: n },
                Fs::Removed(n) => Ev::Gone { name: n },
                Fs::Renamed { from, to } => Ev::Renamed { from, to },
                // Look at everything we follow again (below).
                Fs::Rescan => continue,
            };
            finished.extend(self.tracker.event(ev, now));
        }
        // Sizes (and files that vanished without us hearing of it).
        for name in self.tracker.polls() {
            let ev = match std::fs::metadata(self.dir.join(&name)) {
                Ok(m) => Ev::Temp { name, size: m.len() },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ev::Gone { name },
                Err(_) => continue,
            };
            finished.extend(self.tracker.event(ev, now));
        }
        let done = finished
            .into_iter()
            .map(|f| {
                let path = self.dir.join(&f.name);
                Done { risky: is_risky(&f.name), path, name: f.name }
            })
            .collect();
        let views = self.tracker.views(now);
        let running = (views != self.shown).then(|| {
            self.shown = views.clone();
            views
        });
        Step { running, done, wait: (!self.tracker.is_empty()).then_some(POLL) }
    }
}

fn service(ctx: ServiceCtx) {
    let Some(dir) = downloads_dir() else {
        log_warn!("downloads: cannot find the Downloads folder");
        return;
    };
    log_info!("downloads: watching the downloads folder");
    let mut glue = Glue::new(dir.clone());
    loop {
        let result = watch::watch(&ctx, &dir, &mut |batch| {
            let s = glue.step(batch, Instant::now());
            if s.running.is_some() || !s.done.is_empty() {
                ctx.post(move |app| apply(app, s.running, s.done));
            }
            s.wait
        });
        match result {
            Ok(()) => return,
            Err(e) => log_warn!("downloads: {e}; trying again in a minute"),
        }
        // Nothing is being watched: no pill may hang on.
        glue = Glue::new(dir.clone());
        ctx.post(|app| apply(app, Some(vec![]), vec![]));
        if !ctx.sleep(Duration::from_secs(60)) {
            return;
        }
    }
}

fn apply(app: &mut App, running: Option<Vec<View>>, done: Vec<Done>) {
    if !app.config().downloads.enabled {
        return;
    }
    if let Some(v) = running {
        let empty = v.is_empty();
        app.state_mut::<Downloads>().running = v;
        if empty {
            app.clear_activity("downloads.progress");
        } else {
            app.set_activity(Activity::persistent("downloads.progress", priority::PROGRESS, progress_pill));
        }
        app.request_redraw(Redraw::Pill);
    }
    if let Some(d) = done.into_iter().last() {
        app.state_mut::<Downloads>().done = Some(d);
        app.clear_activity("downloads.progress");
        app.set_activity(Activity::transient("downloads.done", DONE_PRIORITY, DONE_SECS, done_pill));
        app.request_redraw(Redraw::Pill);
    }
}

// ---- pills ----------------------------------------------------------------------------------------

/// The file name, unless the privacy setting hides content during a meeting.
fn shown_name(app: &App, name: Option<&str>) -> String {
    let hide = app.meeting().active && app.config().privacy.hide_notification_body_in_meetings;
    match name {
        Some(n) if !hide => n.to_string(),
        _ => tr("Unduhan", "Download").to_string(),
    }
}

fn progress_pill(app: &App) -> Node {
    let st = app.state::<Downloads>();
    let Some(first) = st.running.first() else { return empty() };
    let comma = !i18n::is_english();
    let detail = match first.speed {
        Some(s) => format!("{} · {}", tracker::fmt_size(first.size, comma), crate::features::monitor::fmt_rate_with(s, comma)),
        None => tracker::fmt_size(first.size, comma),
    };
    // Two lines: the name, and under it what the download is doing now (size and speed), in a
    // readable size and colour (not the 46 % caption).
    let mut kids = vec![
        stack([spinner().color(app.base_accent()), icon(icons::DOWNLOAD).font_size(9.0)]).size(20.0, 20.0),
        col([text(shown_name(app, first.name.as_deref())).semibold(), text(detail).font_size(12.0).secondary()]).gap(1.0).grow(1.0),
    ];
    if st.running.len() > 1 {
        kids.push(text(format!("+{}", st.running.len() - 1)).font_size(11.0).semibold().color(color::TEXT2).pad_xy(7.0, 2.0).bg(color::FILL2).capsule());
    }
    pill(kids).max_w(PILL_MAX_W)
}

/// The widest a downloads pill gets (DIP): a long file name ellipsises instead of stretching
/// the island across the screen.
const PILL_MAX_W: f32 = 430.0;

/// The primary button of the finished pill: a white capsule with a glyph and a label.
fn open_button(label: &str, action: &str) -> Node {
    row([icon(OPEN_GLYPH).font_size(12.0).color(color::INK), text(label.to_string()).font_size(12.0).semibold().color(color::INK)])
        .gap(6.0)
        .pad_xy(12.0, 0.0)
        .h(26.0)
        .bg(Rgba::hex(0xffffff, 0.92))
        .capsule()
        .action(action.to_string())
}

/// "Open in new window" (Segoe MDL2 E8A7): opens the file.
const OPEN_GLYPH: &str = "\u{E8A7}";

fn done_pill(app: &App) -> Node {
    let st = app.state::<Downloads>();
    let Some(d) = &st.done else { return empty() };
    // The title over the name (which ellipsises: the pill is capped at PILL_MAX_W), then the
    // buttons: "Buka" and "Folder" with their glyphs, the Shelf as a round icon button.
    let mut kids = vec![
        stack([icon(icons::CHECK).font_size(10.0).color(color::INK)]).size(20.0, 20.0).bg(color::GREEN).capsule(),
        col([text(tr("Unduhan selesai", "Download complete")).semibold(), text(shown_name(app, Some(&d.name))).font_size(12.0).secondary()]).gap(1.0).grow(1.0),
    ];
    if !d.risky {
        kids.push(open_button(tr("Buka", "Open"), "downloads.open").name(tr("Buka file", "Open the file")));
    }
    kids.push(capsule(icons::FOLDER, tr("Folder", "Folder"), "downloads.reveal").name(tr("Tampilkan di folder", "Show in folder")));
    if app.config().shelf.enabled {
        kids.push(circle_button(icons::SHELF, 26.0, "downloads.shelf").name(tr("Simpan ke Shelf", "Save to Shelf")));
    }
    pill(kids).max_w(PILL_MAX_W)
}

// ---- actions --------------------------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    let Some(d) = app.state::<Downloads>().done.clone() else { return };
    match ev.name() {
        // Programs and scripts are only ever shown, never started from here.
        "open" if !d.risky => {
            if !shell_open(&d.path) {
                log_warn!("downloads: could not open the finished file");
            }
        }
        "reveal" => {
            if !reveal(&d.path) {
                log_warn!("downloads: could not show the file in its folder");
            }
        }
        "shelf" => {
            super::shelf::save_path(app, d.path.to_string_lossy().into_owned());
        }
        _ => return,
    }
    app.clear_activity("downloads.done");
    app.request_redraw(Redraw::Pill);
}

fn shell_open(path: &Path) -> bool {
    let h = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    h.0 as isize > 32
}

/// Explorer with the file selected.
fn reveal(path: &Path) -> bool {
    let arg = format!("/select,\"{}\"", path.display());
    let h = unsafe { ShellExecuteW(None, w!("open"), w!("explorer.exe"), &HSTRING::from(arg), PCWSTR::null(), SW_SHOWNORMAL) };
    h.0 as isize > 32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-dl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn at(base: Instant, s: f64) -> Instant {
        base + Duration::from_secs_f64(s)
    }

    #[test]
    fn the_test_folder_override_needs_debug_mode() {
        let known = || Some(PathBuf::from(r"C:\Users\x\Downloads"));
        assert_eq!(pick_dir(Some("D:\\scratch".into()), true, known), Some(PathBuf::from("D:\\scratch")));
        assert_eq!(pick_dir(Some("D:\\scratch".into()), false, known), known(), "ignored in a normal island");
        assert_eq!(pick_dir(Some("".into()), true, known), known());
        assert_eq!(pick_dir(None, true, known), known());
    }

    #[test]
    fn the_real_downloads_folder_is_found() {
        let d = known_downloads().expect("FOLDERID_Downloads");
        assert!(d.is_absolute());
    }

    #[test]
    fn a_growing_crdownload_then_rename_then_delete_in_a_scratch_folder() {
        let dir = scratch("glue");
        let mut g = Glue::new(dir.clone());
        let t0 = Instant::now();
        let temp = dir.join("Unconfirmed 99.crdownload");
        std::fs::write(&temp, vec![0u8; 1000]).unwrap();
        // Created: tracked, but too young for a pill.
        let s = g.step(vec![Fs::Added("Unconfirmed 99.crdownload".into())], t0);
        assert!(s.running.is_none() && s.done.is_empty());
        assert_eq!(s.wait, Some(POLL), "polling sizes while a download runs");
        // The browser names it and it grows: a pill with the name, size and a speed.
        std::fs::rename(&temp, dir.join("big.mkv.crdownload")).unwrap();
        let s = g.step(vec![Fs::Renamed { from: "Unconfirmed 99.crdownload".into(), to: "big.mkv.crdownload".into() }], at(t0, 0.5));
        assert!(s.running.is_none(), "still young");
        for (i, len) in [(1.0, 500_000usize), (2.0, 1_500_000), (3.0, 2_500_000)] {
            std::fs::write(dir.join("big.mkv.crdownload"), vec![0u8; len]).unwrap();
            let s = g.step(vec![], at(t0, i));
            if i == 3.0 {
                let v = s.running.expect("changed");
                assert_eq!(v.len(), 1);
                assert_eq!((v[0].name.as_deref(), v[0].size), (Some("big.mkv"), 2_500_000));
                assert!(v[0].speed.unwrap() > 400_000.0, "{:?}", v[0].speed);
            }
        }
        // Done: renamed to the final name.
        std::fs::rename(dir.join("big.mkv.crdownload"), dir.join("big.mkv")).unwrap();
        let s = g.step(vec![Fs::Renamed { from: "big.mkv.crdownload".into(), to: "big.mkv".into() }, Fs::Modified("big.mkv".into())], at(t0, 3.5));
        assert_eq!(s.done.len(), 1);
        assert_eq!((s.done[0].name.as_str(), s.done[0].risky), ("big.mkv", false));
        assert_eq!(s.done[0].path, dir.join("big.mkv"));
        assert_eq!(s.running, Some(vec![]), "the progress pill is cleared");
        assert_eq!(s.wait, None, "idle again: wait for events only");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_download_clears_the_pill_even_without_a_delete_event() {
        let dir = scratch("cancel");
        let mut g = Glue::new(dir.clone());
        let t0 = Instant::now();
        std::fs::write(dir.join("c.zip.part"), vec![0u8; 100_000]).unwrap();
        g.step(vec![Fs::Added("c.zip.part".into())], t0);
        std::fs::write(dir.join("c.zip.part"), vec![0u8; 900_000]).unwrap();
        let s = g.step(vec![], at(t0, 1.5));
        assert_eq!(s.running.as_ref().map(|v| v.len()), Some(1));
        // The file is deleted and the event is lost (buffer overflow): the poll notices.
        std::fs::remove_file(dir.join("c.zip.part")).unwrap();
        let s = g.step(vec![Fs::Rescan], at(t0, 2.5));
        assert_eq!(s.running, Some(vec![]));
        assert!(s.done.is_empty(), "a cancelled download is not a finished one");
        assert_eq!(s.wait, None);
        // The same with the delete event.
        std::fs::write(dir.join("d.zip.part"), vec![0u8; 10]).unwrap();
        g.step(vec![Fs::Added("d.zip.part".into())], at(t0, 3.0));
        std::fs::write(dir.join("d.zip.part"), vec![0u8; 5_000_000]).unwrap();
        assert!(g.step(vec![], at(t0, 4.5)).running.is_some());
        std::fs::remove_file(dir.join("d.zip.part")).unwrap();
        let s = g.step(vec![Fs::Removed("d.zip.part".into())], at(t0, 4.6));
        assert_eq!((s.running, s.done.len()), (Some(vec![]), 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_program_download_is_marked_risky_and_ordinary_files_are_ignored() {
        let dir = scratch("risky");
        let mut g = Glue::new(dir.clone());
        let t0 = Instant::now();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let s = g.step(vec![Fs::Added("notes.txt".into()), Fs::Modified("notes.txt".into()), Fs::Renamed { from: "a.txt".into(), to: "b.txt".into() }], t0);
        assert!(s.running.is_none() && s.done.is_empty());
        assert_eq!(s.wait, None);
        std::fs::write(dir.join("setup.exe"), b"MZ").unwrap();
        let s = g.step(vec![Fs::Renamed { from: "setup.exe.crdownload".into(), to: "setup.exe".into() }], at(t0, 1.0));
        assert_eq!(s.done.len(), 1);
        assert!(s.done[0].risky);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
