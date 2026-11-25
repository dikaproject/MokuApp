//! Track O (v0.3): the "island yang sopan" controls of the Island page: monitor, left / centre /
//! right + a nudge, move aside, hiding the island (never / when idle / always until the top edge),
//! and the per-app lists.
//!
//! Like the calendar controls they live outside `Form`: this module reads and writes `[island]`
//! of `config.toml` itself with the same read-modify-write as every other save
//! (`Config::update_file`: a hand edit or a save from another part of the window is kept). All the
//! decisions are in [`Model`] (no window needed), so they are unit-tested; `wire` only connects it
//! to `ui/pages-o.slint`.
//!
//! "Aplikasi aktif": the settings window is itself in front when the button is pressed, so the
//! button starts a 10 s wait for the first *other* app to come to the front and adds that one.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use moku_core::apps;
use moku_core::config::{Anchor, HideMode, Island, MonitorChoice, MoveAside};
use moku_core::Config;
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::core::BOOL;

use crate::{PlacementCfg, SettingsWindow};

const ANCHORS: [Anchor; 3] = [Anchor::Left, Anchor::Center, Anchor::Right];
const ASIDE: [MoveAside; 3] = [MoveAside::Off, MoveAside::Fade, MoveAside::Slide];
/// "Sembunyikan island": Mati, Saat idle, Selalu sampai tepi atas.
const HIDE: [HideMode; 3] = [HideMode::Off, HideMode::WhenIdle, HideMode::Always];
/// How long "Aplikasi aktif" waits for another app to come to the front.
pub const CAPTURE_SECS: u64 = 10;

/// The two lists: 1 = hide in front of the app, 2 = ignore its media.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Hide,
    Media,
}

impl List {
    pub fn from_id(i: i32) -> Option<List> {
        match i {
            1 => Some(List::Hide),
            2 => Some(List::Media),
            _ => None,
        }
    }
}

/// A monitor as the picker shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct MonEntry {
    /// Device name, what `island.monitor` stores.
    pub name: String,
    pub label: String,
}

/// "Monitor 2 · 2560×1440" for `\\.\DISPLAY2`; "(utama)" for the primary one.
pub fn label(name: &str, w: i32, h: i32, primary: bool) -> String {
    let n: String = name.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
    let n = if n.is_empty() { "?".to_string() } else { n };
    format!("Monitor {n} · {w}×{h}{}", if primary { " (utama)" } else { "" })
}

/// The connected monitors, primary first.
pub fn monitors() -> Vec<MonEntry> {
    unsafe extern "system" fn each(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let out = unsafe { &mut *(lp.0 as *mut Vec<(MonEntry, bool)>) };
        let mut info = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, ..Default::default() };
        if unsafe { GetMonitorInfoW(h, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool() {
            let n = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            let name = String::from_utf16_lossy(&info.szDevice[..n]);
            let r = info.monitorInfo.rcMonitor;
            let primary = info.monitorInfo.dwFlags & 1 != 0;
            out.push((MonEntry { label: label(&name, r.right - r.left, r.bottom - r.top, primary), name }, primary));
        }
        BOOL(1)
    }
    let mut v: Vec<(MonEntry, bool)> = vec![];
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut v as *mut Vec<(MonEntry, bool)> as isize));
    }
    v.sort_by_key(|(_, primary)| !primary);
    v.into_iter().map(|(m, _)| m).collect()
}

/// 0 utama, 1 ikuti kursor, 2 monitor tertentu.
/// The hide choice shown (both flags on, from a hand edit, reads as "Selalu").
pub fn hide_mode_index(i: &Island) -> i32 {
    HIDE.iter().position(|m| *m == i.hide_mode()).unwrap_or(0) as i32
}

/// Pick a hide choice: sets the two flags so that only that one is on. Another index changes nothing.
pub fn set_hide_mode(i: &mut Island, index: i32) {
    if let Some(m) = usize::try_from(index).ok().and_then(|n| HIDE.get(n)).copied() {
        i.set_hide_mode(m);
    }
}

pub fn monitor_mode(i: &Island) -> i32 {
    match i.monitor_choice() {
        MonitorChoice::Primary | MonitorChoice::All => 0,
        MonitorChoice::Cursor => 1,
        MonitorChoice::Named(_) => 2,
    }
}

/// Index of the chosen monitor among `mons`; -1 when it is not connected (nothing is selected).
pub fn monitor_index(i: &Island, mons: &[MonEntry]) -> i32 {
    match i.monitor_choice() {
        MonitorChoice::Named(n) => mons.iter().position(|m| m.name.eq_ignore_ascii_case(&n)).map_or(-1, |p| p as i32),
        _ => 0,
    }
}

/// Switch the monitor mode. "Tertentu" keeps a monitor already chosen; else it takes the first
/// one that is not the primary (the usual reason to pick one), else the first.
pub fn set_monitor_mode(i: &mut Island, mode: i32, mons: &[MonEntry]) {
    i.monitor = match mode {
        1 => "cursor".into(),
        2 => match i.monitor_choice() {
            MonitorChoice::Named(n) => n,
            _ => mons.get(1).or(mons.first()).map_or("primary".to_string(), |m| m.name.clone()),
        },
        _ => "primary".into(),
    };
}

/// What `capture` should do after `elapsed_ms`, given the app in front (`None`: nothing yet, or
/// it is this window).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capture {
    Waiting { secs_left: u64 },
    Got(String),
    Timeout,
}

pub fn capture_step(elapsed_ms: u64, front: Option<String>) -> Capture {
    if let Some(exe) = front.filter(|e| !e.trim().is_empty()) {
        return Capture::Got(exe);
    }
    if elapsed_ms >= CAPTURE_SECS * 1000 {
        return Capture::Timeout;
    }
    Capture::Waiting { secs_left: (CAPTURE_SECS * 1000 - elapsed_ms).div_ceil(1000) }
}

/// The settings behind the controls, without any window.
pub struct Model {
    path: PathBuf,
    /// The config as we last read or wrote it (the fallback when the file is unreadable for a moment).
    last: Config,
    pub mons: Vec<MonEntry>,
}

impl Model {
    pub fn open(path: PathBuf, mons: Vec<MonEntry>) -> Model {
        let last = Config::load_from(&path).0;
        Model { path, last, mons }
    }

    pub fn reload(&mut self) {
        self.last = Config::load_from(&self.path).0;
    }

    pub fn island(&self) -> &Island {
        &self.last.island
    }

    fn edit(&mut self, change: impl FnOnce(&mut Config)) -> Result<(), String> {
        let cfg = Config::update_file(&self.path, &self.last, change).map_err(|e| format!("Gagal menyimpan: {e}"))?;
        self.last = cfg;
        Ok(())
    }

    pub fn set_monitor_mode(&mut self, mode: i32) -> Result<(), String> {
        let mons = self.mons.clone();
        self.edit(|c| set_monitor_mode(&mut c.island, mode, &mons))
    }

    pub fn set_monitor(&mut self, index: i32) -> Result<(), String> {
        let Some(m) = usize::try_from(index).ok().and_then(|i| self.mons.get(i)).map(|m| m.name.clone()) else { return Ok(()) };
        self.edit(|c| c.island.monitor = m)
    }

    pub fn set_anchor(&mut self, index: i32) -> Result<(), String> {
        let Some(a) = usize::try_from(index).ok().and_then(|i| ANCHORS.get(i)).copied() else { return Ok(()) };
        self.edit(|c| c.island.anchor = a)
    }

    pub fn set_offset(&mut self, px: i32) -> Result<(), String> {
        self.edit(|c| c.island.offset_x = px.clamp(-2000, 2000))
    }

    pub fn set_move_aside(&mut self, index: i32) -> Result<(), String> {
        let Some(m) = usize::try_from(index).ok().and_then(|i| ASIDE.get(i)).copied() else { return Ok(()) };
        self.edit(|c| c.island.move_aside = m)
    }

    pub fn set_hide_mode(&mut self, index: i32) -> Result<(), String> {
        self.edit(|c| set_hide_mode(&mut c.island, index))
    }

    fn list_mut(c: &mut Config, l: List) -> &mut Vec<String> {
        match l {
            List::Hide => &mut c.island.hide_apps,
            List::Media => &mut c.island.media_ignore_apps,
        }
    }

    pub fn apps(&self, l: List) -> &[String] {
        match l {
            List::Hide => &self.last.island.hide_apps,
            List::Media => &self.last.island.media_ignore_apps,
        }
    }

    /// Add `text` (a program name or a path; only its file name is kept).
    pub fn add_app(&mut self, l: List, text: &str) -> Result<(), String> {
        let name = text.trim().rsplit(['\\', '/']).next().unwrap_or("").trim().to_string();
        if name.is_empty() {
            return Err("Ketik nama programnya dulu, misalnya chrome.exe".into());
        }
        if name.len() > 120 || name.chars().any(char::is_control) {
            return Err("Nama programnya tidak valid".into());
        }
        if apps::exe_matches(&name, self.apps(l)) {
            return Err("Sudah ada di daftar".into());
        }
        self.edit(|c| Self::list_mut(c, l).push(name))
    }

    pub fn remove_app(&mut self, l: List, index: usize) -> Result<(), String> {
        self.edit(|c| {
            let v = Self::list_mut(c, l);
            if index < v.len() {
                v.remove(index);
            }
        })
    }
}

fn model_of(v: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(v.iter().map(|s| SharedString::from(s.as_str())).collect::<Vec<_>>()))
}

/// Put the model's state into the window.
fn show(ui: &SettingsWindow, m: &Model) {
    let c = ui.global::<PlacementCfg>();
    let i = m.island();
    c.set_monitor_mode(monitor_mode(i));
    c.set_monitor_names(model_of(&m.mons.iter().map(|m| m.label.clone()).collect::<Vec<_>>()));
    c.set_monitor_index(monitor_index(i, &m.mons));
    c.set_anchor(ANCHORS.iter().position(|a| *a == i.anchor).unwrap_or(1) as i32);
    c.set_offset(i.offset_x.clamp(-800, 800));
    c.set_move_aside(ASIDE.iter().position(|a| *a == i.move_aside).unwrap_or(0) as i32);
    c.set_hide_mode(hide_mode_index(i));
    c.set_hide_apps(model_of(&i.hide_apps));
    c.set_media_apps(model_of(&i.media_ignore_apps));
}

/// Connect the controls to `config.toml` (the real one, or `$MOKU_DATA_DIR`'s).
pub fn wire(ui: &SettingsWindow, path: PathBuf) {
    let model = Rc::new(RefCell::new(Model::open(path, monitors())));
    let c = ui.global::<PlacementCfg>();
    show(ui, &model.borrow());

    // Run an edit, then refresh the window; a failure shows under the list's field.
    let apply = |model: &Rc<RefCell<Model>>, ui: &slint::Weak<SettingsWindow>, f: &dyn Fn(&mut Model) -> Result<(), String>| -> bool {
        let r = f(&mut model.borrow_mut());
        let Some(ui) = ui.upgrade() else { return r.is_ok() };
        match &r {
            Ok(()) => {
                ui.global::<PlacementCfg>().set_problem("".into());
                show(&ui, &model.borrow());
            }
            Err(e) => ui.global::<PlacementCfg>().set_problem(e.as_str().into()),
        }
        r.is_ok()
    };

    c.on_reload({
        let (model, ui) = (model.clone(), ui.as_weak());
        move || {
            let mut m = model.borrow_mut();
            m.reload();
            m.mons = monitors();
            if let Some(ui) = ui.upgrade() {
                ui.global::<PlacementCfg>().set_problem("".into());
                show(&ui, &m);
            }
        }
    });
    c.on_set_monitor_mode({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |v| {
            apply(&model, &ui, &|m| m.set_monitor_mode(v));
        }
    });
    c.on_set_monitor({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |v| {
            apply(&model, &ui, &|m| m.set_monitor(v));
        }
    });
    c.on_set_anchor({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |v| {
            apply(&model, &ui, &|m| m.set_anchor(v));
        }
    });
    c.on_set_offset({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |v| {
            apply(&model, &ui, &|m| m.set_offset(v));
        }
    });
    c.on_set_move_aside({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |v| {
            apply(&model, &ui, &|m| m.set_move_aside(v));
        }
    });
    c.on_set_hide_mode({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |v| {
            apply(&model, &ui, &|m| m.set_hide_mode(v));
        }
    });
    c.on_add_app({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |list, text| match List::from_id(list) {
            Some(l) => apply(&model, &ui, &|m| m.add_app(l, text.as_str())),
            None => false,
        }
    });
    c.on_remove_app({
        let (model, ui) = (model.clone(), ui.as_weak());
        move |list, i| {
            if let Some(l) = List::from_id(list) {
                apply(&model, &ui, &|m| m.remove_app(l, usize::try_from(i).unwrap_or(usize::MAX)));
            }
        }
    });

    // "Aplikasi aktif": poll the app in front until another app than this window comes up.
    let timer = Rc::new(Timer::default());
    c.on_capture_app({
        let (model, ui, timer) = (model.clone(), ui.as_weak(), timer.clone());
        move |list| {
            let Some(w) = ui.upgrade() else { return };
            let g = w.global::<PlacementCfg>();
            let Some(l) = List::from_id(list) else {
                timer.stop();
                g.set_capturing(0);
                g.set_capture_status("".into());
                return;
            };
            g.set_problem("".into());
            g.set_capturing(list);
            g.set_capture_status(format!("Pindah ke aplikasi yang mau ditambahkan… {CAPTURE_SECS} dtk").into());
            let started = Instant::now();
            let (model, ui) = (model.clone(), ui.clone());
            timer.start(TimerMode::Repeated, Duration::from_millis(250), {
                let timer = timer.clone();
                move || {
                    let Some(w) = ui.upgrade() else { return timer.stop() };
                    let g = w.global::<PlacementCfg>();
                    match capture_step(started.elapsed().as_millis() as u64, apps::foreground_exe()) {
                        Capture::Waiting { secs_left } => g.set_capture_status(format!("Pindah ke aplikasi yang mau ditambahkan… {secs_left} dtk").into()),
                        done => {
                            timer.stop();
                            g.set_capturing(0);
                            g.set_capture_status("".into());
                            match done {
                                Capture::Got(exe) => {
                                    apply(&model, &ui, &|m| m.add_app(l, &exe));
                                }
                                _ => g.set_problem("Tidak ada aplikasi lain yang aktif dalam 10 detik".into()),
                            }
                        }
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-settings-placement-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("config.toml")
    }

    fn mons() -> Vec<MonEntry> {
        vec![
            MonEntry { name: r"\\.\DISPLAY1".into(), label: label(r"\\.\DISPLAY1", 1920, 1080, true) },
            MonEntry { name: r"\\.\DISPLAY2".into(), label: label(r"\\.\DISPLAY2", 2560, 1440, false) },
        ]
    }

    #[test]
    fn monitor_labels() {
        assert_eq!(label(r"\\.\DISPLAY1", 1920, 1080, true), "Monitor 1 · 1920×1080 (utama)");
        assert_eq!(label(r"\\.\DISPLAY12", 2560, 1440, false), "Monitor 12 · 2560×1440");
        assert_eq!(label("weird", 10, 10, false), "Monitor ? · 10×10");
    }

    #[test]
    fn monitor_mode_round_trips_through_the_config_string() {
        let mut i = Island::default();
        assert_eq!((monitor_mode(&i), monitor_index(&i, &mons())), (0, 0));
        set_monitor_mode(&mut i, 1, &mons());
        assert_eq!((i.monitor.as_str(), monitor_mode(&i)), ("cursor", 1));
        // "Tertentu" without a choice yet: the first monitor that is not the primary.
        set_monitor_mode(&mut i, 2, &mons());
        assert_eq!((i.monitor.as_str(), monitor_mode(&i), monitor_index(&i, &mons())), (r"\\.\DISPLAY2", 2, 1));
        // ... and it keeps a choice already made.
        i.monitor = r"\\.\DISPLAY1".into();
        set_monitor_mode(&mut i, 2, &mons());
        assert_eq!(i.monitor, r"\\.\DISPLAY1");
        set_monitor_mode(&mut i, 0, &mons());
        assert_eq!(i.monitor, "primary");
        // A single monitor: "Tertentu" takes it. No monitors at all: primary.
        let mut i = Island::default();
        set_monitor_mode(&mut i, 2, &mons()[..1]);
        assert_eq!(i.monitor, r"\\.\DISPLAY1");
        let mut i = Island::default();
        set_monitor_mode(&mut i, 2, &[]);
        assert_eq!(i.monitor, "primary");
        // A monitor that is not connected: nothing selected, the mode still reads "tertentu".
        let i = Island { monitor: r"\\.\DISPLAY9".into(), ..Island::default() };
        assert_eq!((monitor_mode(&i), monitor_index(&i, &mons())), (2, -1));
        // "all" is not offered: it shows as the primary monitor.
        let i = Island { monitor: "all".into(), ..Island::default() };
        assert_eq!(monitor_mode(&i), 0);
    }

    #[test]
    fn every_control_saves_into_the_island_section_and_keeps_the_rest() {
        let p = temp("controls");
        // Something else the window saved before.
        let mut other = Config::default();
        other.island.scale = 1.2;
        other.timer.focus_minutes = 50;
        other.save_to(&p).unwrap();

        let mut m = Model::open(p.clone(), mons());
        m.set_monitor_mode(2).unwrap();
        m.set_anchor(2).unwrap();
        m.set_offset(-40).unwrap();
        m.set_move_aside(2).unwrap();
        m.set_hide_mode(2).unwrap();
        let (c, w) = Config::load_from(&p);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.island.monitor, r"\\.\DISPLAY2");
        assert_eq!((c.island.anchor, c.island.offset_x, c.island.move_aside, c.island.hide_until_edge), (Anchor::Right, -40, MoveAside::Slide, true));
        assert_eq!((c.island.scale, c.timer.focus_minutes), (1.2, 50), "other settings are untouched");
        // Picking a monitor by index; bad indexes change nothing.
        m.set_monitor(0).unwrap();
        m.set_monitor(7).unwrap();
        m.set_monitor(-1).unwrap();
        m.set_anchor(9).unwrap();
        m.set_move_aside(-3).unwrap();
        let c = Config::load_from(&p).0;
        assert_eq!((c.island.monitor.as_str(), c.island.anchor, c.island.move_aside), (r"\\.\DISPLAY1", Anchor::Right, MoveAside::Slide));
        // The offset is clamped to what the island accepts.
        m.set_offset(99_999).unwrap();
        assert_eq!(Config::load_from(&p).0.island.offset_x, 2000);
    }

    #[test]
    fn a_hand_edit_made_meanwhile_survives_a_save() {
        let p = temp("handedit");
        let mut m = Model::open(p.clone(), mons());
        // The user edits the file by hand after the window loaded it.
        let mut c = Config::default();
        c.island.hide_apps = vec!["code.exe".into()];
        c.save_to(&p).unwrap();
        m.set_hide_mode(2).unwrap();
        let c = Config::load_from(&p).0;
        assert_eq!(c.island.hide_apps, vec!["code.exe"]);
        assert!(c.island.hide_until_edge);
    }

    #[test]
    fn the_hide_choice_maps_onto_the_two_config_flags() {
        // 0 = Mati, 1 = Saat idle, 2 = Selalu sampai tepi atas.
        let mk = |edge, idle| Island { hide_until_edge: edge, hide_when_idle: idle, ..Island::default() };
        assert_eq!(hide_mode_index(&mk(false, false)), 0);
        assert_eq!(hide_mode_index(&mk(false, true)), 1);
        assert_eq!(hide_mode_index(&mk(true, false)), 2);
        assert_eq!(hide_mode_index(&mk(true, true)), 2, "both on in a hand-edited file: always wins");
        // Choosing sets exactly one flag.
        let mut i = mk(true, true);
        set_hide_mode(&mut i, 1);
        assert_eq!((i.hide_until_edge, i.hide_when_idle), (false, true));
        set_hide_mode(&mut i, 2);
        assert_eq!((i.hide_until_edge, i.hide_when_idle), (true, false));
        set_hide_mode(&mut i, 0);
        assert_eq!((i.hide_until_edge, i.hide_when_idle), (false, false));
        // An index outside the choices changes nothing.
        set_hide_mode(&mut i, 1);
        set_hide_mode(&mut i, 7);
        set_hide_mode(&mut i, -1);
        assert_eq!((i.hide_until_edge, i.hide_when_idle), (false, true));
    }

    #[test]
    fn the_hide_choice_is_saved_to_the_file_and_keeps_the_rest() {
        let p = temp("hidechoice");
        // An existing file that only has the old key: it reads as "Selalu" and the window changes it.
        std::fs::write(&p, "[island]\nhide_until_edge = true\nanchor = \"left\"\n[timer]\nfocus_minutes = 40\n").unwrap();
        let mut m = Model::open(p.clone(), mons());
        assert_eq!(hide_mode_index(m.island()), 2);
        m.set_hide_mode(1).unwrap();
        let (c, w) = Config::load_from(&p);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!((c.island.hide_until_edge, c.island.hide_when_idle, c.island.anchor, c.timer.focus_minutes), (false, true, Anchor::Left, 40));
        assert_eq!(hide_mode_index(m.island()), 1);
        m.set_hide_mode(0).unwrap();
        let c = Config::load_from(&p).0;
        assert_eq!((c.island.hide_until_edge, c.island.hide_when_idle), (false, false));
        // A bad index saves nothing and fails nothing.
        m.set_hide_mode(9).unwrap();
        assert_eq!(hide_mode_index(m.island()), 0);
    }

    #[test]
    fn the_app_lists_add_dedupe_and_remove() {
        let p = temp("lists");
        let mut m = Model::open(p.clone(), mons());
        m.add_app(List::Hide, "chrome.exe").unwrap();
        m.add_app(List::Hide, r" C:\Program Files\Code\Code.exe ").unwrap();
        m.add_app(List::Media, "Spotify.exe").unwrap();
        assert_eq!(m.apps(List::Hide), ["chrome.exe", "Code.exe"]);
        assert_eq!(m.apps(List::Media), ["Spotify.exe"]);
        // Duplicates (any case, with or without .exe), empty and junk are refused with a reason.
        assert!(m.add_app(List::Hide, "CHROME").unwrap_err().contains("Sudah"));
        assert!(m.add_app(List::Hide, "   ").unwrap_err().contains("Ketik"));
        assert!(m.add_app(List::Hide, "a\u{0}b").is_err());
        assert!(m.add_app(List::Hide, &"x".repeat(200)).is_err());
        // The same app may be in both lists.
        m.add_app(List::Media, "chrome.exe").unwrap();
        // Removing: by position; out of range does nothing.
        m.remove_app(List::Hide, 0).unwrap();
        m.remove_app(List::Hide, 5).unwrap();
        let c = Config::load_from(&p).0;
        assert_eq!(c.island.hide_apps, vec!["Code.exe"]);
        assert_eq!(c.island.media_ignore_apps, vec!["Spotify.exe", "chrome.exe"]);
        assert_eq!(List::from_id(1), Some(List::Hide));
        assert_eq!(List::from_id(2), Some(List::Media));
        assert_eq!(List::from_id(0), None);
    }

    #[test]
    fn waiting_for_the_app_in_front() {
        // Nothing but this window in front: keep waiting, counting down.
        assert_eq!(capture_step(0, None), Capture::Waiting { secs_left: 10 });
        assert_eq!(capture_step(250, None), Capture::Waiting { secs_left: 10 });
        assert_eq!(capture_step(1001, None), Capture::Waiting { secs_left: 9 });
        assert_eq!(capture_step(9999, None), Capture::Waiting { secs_left: 1 });
        // Time is up.
        assert_eq!(capture_step(10_000, None), Capture::Timeout);
        assert_eq!(capture_step(25_000, Some("".into())), Capture::Timeout);
        // Another app came up: it is the one (also at the very end).
        assert_eq!(capture_step(3000, Some("chrome.exe".into())), Capture::Got("chrome.exe".into()));
        assert_eq!(capture_step(10_000, Some("chrome.exe".into())), Capture::Got("chrome.exe".into()));
    }
}
