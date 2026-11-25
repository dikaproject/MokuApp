//! Feature `privacy` — Task 3 (track B).
//!
//! Camera / microphone "in use" dots (green = camera, orange = microphone, per the mockup). The
//! source is the Windows privacy consent store,
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\{webcam,microphone}`
//! (plus the `NonPackaged` subkeys for desktop apps): an app holds the device while its
//! `LastUsedTimeStart != 0` and `LastUsedTimeStop == 0`.
//!
//! One service thread watches both keys (subtree) with `RegNotifyChangeKeyValue`, so it is idle
//! (zero CPU) until Windows writes a change. Only while something holds a device does it wake
//! every few seconds to drop apps that died without writing `LastUsedTimeStop` (the store keeps
//! such stale entries forever; an old Discord build was seen holding the mic for months).
//!
//! The service also feeds meeting-mode detection (`meeting.rs`): for a browser that holds the
//! microphone it looks at the titles of that browser's windows ("Meet"). Titles are only compared,
//! never stored or logged.

use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_ITEMS, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, RRF_RT_REG_QWORD, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegNotifyChangeKeyValue,
    RegOpenKeyExW,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, WaitForMultipleObjects, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible};
use windows::core::{BOOL, HSTRING, PWSTR};

use super::meeting;
use crate::activity::{Activity, priority};
use crate::app::{App, Ui};
use crate::i18n::tr;
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::*;

pub fn register(r: &mut Registry) {
    r.add_state(Privacy::default());
    r.on_action("privacy", on_action);
    r.on_config(sync_dots);
    r.add_service(ServiceSpec { name: "privacy", enabled: |c| c.system.privacy_dots || c.system.meeting_mode, run: service });
}

// ---- model (pure, tested) -----------------------------------------------------------------------

/// One subkey of `ConsentStore\{webcam,microphone}` as read from the registry.
#[derive(Clone, Debug, PartialEq)]
pub struct RawEntry {
    /// Subkey name: `MSTeams_8wekyb3d8bbwe`, or `C:#Program Files#Zoom#bin#Zoom.exe` below `NonPackaged`.
    pub name: String,
    pub packaged: bool,
    pub start: u64,
    pub stop: u64,
}

/// An app that currently holds the camera or the microphone.
#[derive(Clone, Debug, PartialEq)]
pub struct Holder {
    /// Friendly name ("Zoom", "Teams", "Chrome").
    pub name: String,
    /// Executable file name (`Zoom.exe`); empty when unknown (packaged apps we don't know).
    pub exe: String,
}

/// Who holds what right now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Consent {
    pub camera: Vec<Holder>,
    pub mic: Vec<Holder>,
    /// The meeting app among the microphone holders ("Zoom", "Teams", "Meet"), if any.
    pub meeting: Option<&'static str>,
}

impl Consent {
    pub fn any(&self) -> bool {
        !self.camera.is_empty() || !self.mic.is_empty()
    }
}

/// An app holds the device from `LastUsedTimeStart` until it writes `LastUsedTimeStop`.
pub fn consent_in_use(start: u64, stop: u64) -> bool {
    start != 0 && stop == 0
}

fn friendly_exe_name(exe: &str) -> String {
    let stem = exe.rsplit_once('.').map(|(s, _)| s).unwrap_or(exe);
    match stem.to_ascii_lowercase().as_str() {
        "ms-teams" | "teams" => "Teams".into(),
        "msedge" => "Edge".into(),
        "obs64" | "obs32" | "obs" => "OBS".into(),
        _ => {
            let mut c = stem.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    }
}

/// Friendly name + executable for a packaged app's family name.
fn packaged_holder(key: &str) -> Holder {
    let family = key.split('_').next().unwrap_or(key);
    match family {
        "MSTeams" => Holder { name: "Teams".into(), exe: "ms-teams.exe".into() },
        "Microsoft.WindowsCamera" => Holder { name: tr("Kamera", "Camera").into(), exe: String::new() },
        "Microsoft.WindowsSoundRecorder" => Holder { name: tr("Perekam Suara", "Voice Recorder").into(), exe: String::new() },
        other => Holder { name: other.strip_prefix("Microsoft.").unwrap_or(other).to_string(), exe: String::new() },
    }
}

/// The apps in use among `entries`. `is_running(full_path)` tells whether a desktop app's
/// process is alive: an entry whose app is gone is stale (it never wrote `LastUsedTimeStop`)
/// and is dropped. Packaged apps can't be checked that way and are trusted.
pub fn holders_from_entries(entries: &[RawEntry], is_running: &dyn Fn(&str) -> bool) -> Vec<Holder> {
    let mut out: Vec<Holder> = Vec::new();
    for e in entries {
        if e.name == "NonPackaged" || !consent_in_use(e.start, e.stop) {
            continue;
        }
        let h = if e.packaged {
            packaged_holder(&e.name)
        } else {
            let path = e.name.replace('#', "\\");
            if !is_running(&path) {
                continue;
            }
            let exe = path.rsplit('\\').next().unwrap_or(&path).to_string();
            Holder { name: friendly_exe_name(&exe), exe }
        };
        let dup = |o: &Holder| if h.exe.is_empty() { o.exe.is_empty() && o.name == h.name } else { o.exe.eq_ignore_ascii_case(&h.exe) };
        if !out.iter().any(dup) {
            out.push(h);
        }
    }
    out
}

// ---- state + UI ---------------------------------------------------------------------------------

#[derive(Default)]
struct Privacy {
    consent: Consent,
}

/// The last snapshot (what `meeting` re-evaluates after a config change).
pub fn current(app: &App) -> Consent {
    app.state::<Privacy>().consent.clone()
}

/// Apply a fresh snapshot (UI thread): remember it, update the dots pill, tell meeting mode.
fn apply(app: &mut App, c: Consent) {
    if app.state::<Privacy>().consent == c {
        return;
    }
    app.state_mut::<Privacy>().consent = c.clone();
    meeting::on_consent(app, &c); // updates the meeting pill, then calls sync_dots
    sync_dots(app);
}

fn dot(c: Rgba) -> Node {
    empty().size(9.0, 9.0).bg(c).capsule()
}

/// The dots as the pill and bubble draw them: camera first (green), then microphone (orange).
pub fn dots(camera: bool, mic: bool, mic_color: Rgba) -> Node {
    let mut v = Vec::new();
    if camera {
        v.push(dot(color::GREEN));
    }
    if mic {
        v.push(dot(mic_color));
    }
    row(v).gap(4.0)
}

/// Show or clear the "camera / microphone in use" pill. Hidden while the meeting pill is
/// showing (it carries the same dots) and when the setting is off.
pub fn sync_dots(app: &mut App) {
    let c = app.state::<Privacy>().consent.clone();
    let covered = app.has_activity("meeting.pill");
    if !app.config().system.privacy_dots || !c.any() || covered {
        app.clear_activity("privacy.dots");
        return;
    }
    app.set_activity(
        Activity::persistent("privacy.dots", priority::MEETING, |app| {
            let c = &app.state::<Privacy>().consent;
            let what = match (c.camera.is_empty(), c.mic.is_empty()) {
                (false, false) => tr("Kamera & mikrofon aktif", "Camera & microphone in use"),
                (false, true) => tr("Kamera aktif", "Camera in use"),
                _ => tr("Mikrofon aktif", "Microphone in use"),
            };
            let mut names: Vec<&str> = Vec::new();
            for h in c.camera.iter().chain(c.mic.iter()) {
                if !names.contains(&h.name.as_str()) {
                    names.push(&h.name);
                }
            }
            names.truncate(2);
            let mut items = vec![dots(!c.camera.is_empty(), !c.mic.is_empty(), color::ORANGE), text(what).semibold()];
            if !names.is_empty() {
                items.push(text(names.join(", ")).secondary().max_w(160.0));
            }
            pill(items)
        })
        .tab("control")
        .bubble(|app| {
            let c = &app.state::<Privacy>().consent;
            stack([dots(!c.camera.is_empty(), !c.mic.is_empty(), color::ORANGE)])
        }),
    );
}

/// Debug path: `mokuctl debug click privacy.sample:<cam|mic|both|off>` feeds a sample snapshot
/// through the same reducer the service uses (no hardware needed).
fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.name() != "sample" || !matches!(ev.kind, ActionKind::Click) {
        return;
    }
    let h = |name: &str, exe: &str| Holder { name: name.into(), exe: exe.into() };
    let c = match ev.arg() {
        Some("cam") => Consent { camera: vec![h("Chrome", "chrome.exe")], ..Consent::default() },
        Some("mic") => Consent { mic: vec![h("Chrome", "chrome.exe")], ..Consent::default() },
        Some("both") => Consent { camera: vec![h("Chrome", "chrome.exe")], mic: vec![h("Chrome", "chrome.exe")], meeting: None },
        _ => Consent::default(),
    };
    apply(app, c);
}

// ---- service: registry watch --------------------------------------------------------------------

const REAL_ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";
/// Re-check interval while something holds a device (stale-entry sweep), and while a browser
/// holds the microphone (its window title can change after the call started).
const SWEEP: Duration = Duration::from_secs(5);

/// The consent store root below `HKCU`. `MOKU_CONSENT_ROOT` points the watcher at a scratch key
/// (used by the manual end-to-end test in the track report so the real store is never written)
/// — honoured only with `MOKU_DEBUG=1` / a debug build, so a stray environment variable cannot
/// blind the camera / microphone indicator of a normal island.
fn consent_root() -> String {
    consent_root_from(std::env::var("MOKU_CONSENT_ROOT").ok(), crate::app::debug_enabled())
}

fn consent_root_from(over: Option<String>, debug: bool) -> String {
    over.filter(|s| debug && !s.is_empty()).unwrap_or_else(|| REAL_ROOT.to_string())
}

struct Key(HKEY);

impl Key {
    fn open(path: &str) -> Option<Key> {
        let mut h = HKEY::default();
        let err = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, KEY_READ, &mut h) };
        if err.0 == 0 { Some(Key(h)) } else { None }
    }

    fn qword(&self, sub: &HSTRING, value: &HSTRING) -> u64 {
        let mut v = 0u64;
        let mut size = size_of::<u64>() as u32;
        let err = unsafe { RegGetValueW(self.0, sub, value, RRF_RT_REG_QWORD, None, Some(&mut v as *mut u64 as *mut _), Some(&mut size)) };
        if err.0 == 0 { v } else { 0 }
    }

    fn subkeys(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut buf = [0u16; 1024];
        for i in 0.. {
            let mut len = buf.len() as u32;
            let err = unsafe { RegEnumKeyExW(self.0, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) };
            if err == ERROR_NO_MORE_ITEMS || err.0 != 0 {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        out
    }

    /// Arm a one-shot change notification (whole subtree) that signals `event`.
    fn watch(&self, event: HANDLE) {
        unsafe {
            let _ = RegNotifyChangeKeyValue(self.0, true, REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET, Some(event), true);
        }
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// Every app subkey of a device key (`webcam` / `microphone`), including the `NonPackaged` ones.
fn read_entries(dev: &Key) -> Vec<RawEntry> {
    let (start, stop) = (HSTRING::from("LastUsedTimeStart"), HSTRING::from("LastUsedTimeStop"));
    let mut out = Vec::new();
    for name in dev.subkeys() {
        if name == "NonPackaged" {
            let Some(np) = Key::open_sub(dev, "NonPackaged") else { continue };
            for n in np.subkeys() {
                let sub = HSTRING::from(n.as_str());
                out.push(RawEntry { packaged: false, start: np.qword(&sub, &start), stop: np.qword(&sub, &stop), name: n });
            }
        } else {
            let sub = HSTRING::from(name.as_str());
            out.push(RawEntry { packaged: true, start: dev.qword(&sub, &start), stop: dev.qword(&sub, &stop), name });
        }
    }
    out
}

impl Key {
    fn open_sub(parent: &Key, sub: &str) -> Option<Key> {
        let mut h = HKEY::default();
        let err = unsafe { RegOpenKeyExW(parent.0, &HSTRING::from(sub), None, KEY_READ, &mut h) };
        if err.0 == 0 { Some(Key(h)) } else { None }
    }
}

/// Running processes by executable name (lower case), taken once per read and only when an entry
/// claims to be in use.
struct Procs {
    list: Vec<(u32, String)>,
}

impl Procs {
    fn snapshot() -> Procs {
        let mut list = Vec::new();
        unsafe {
            let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return Procs { list } };
            let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut ok = Process32FirstW(snap, &mut e).is_ok();
            while ok {
                let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                list.push((e.th32ProcessID, String::from_utf16_lossy(&e.szExeFile[..n]).to_lowercase()));
                ok = Process32NextW(snap, &mut e).is_ok();
            }
            let _ = CloseHandle(snap);
        }
        Procs { list }
    }

    fn name_of(&self, pid: u32) -> Option<&str> {
        self.list.iter().find(|(p, _)| *p == pid).map(|(_, n)| n.as_str())
    }

    /// Is a process running from exactly this image path? A process whose path can't be read
    /// (protected / elevated) counts as running: better a dot too many than a missed camera.
    fn path_running(&self, full: &str) -> bool {
        let file = full.rsplit('\\').next().unwrap_or(full).to_lowercase();
        self.list.iter().filter(|(_, n)| *n == file).any(|(pid, _)| match image_path(*pid) {
            Some(p) => p.eq_ignore_ascii_case(full),
            None => true,
        })
    }
}

fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// `(pid, title)` of every visible top-level window with a title. Only compared, never kept.
fn window_titles() -> Vec<(u32, String)> {
    unsafe extern "system" fn each(hwnd: windows::Win32::Foundation::HWND, lp: windows::Win32::Foundation::LPARAM) -> BOOL {
        unsafe {
            let out = &mut *(lp.0 as *mut Vec<(u32, String)>);
            if IsWindowVisible(hwnd).as_bool() {
                let mut buf = [0u16; 256];
                let n = GetWindowTextW(hwnd, &mut buf);
                if n > 0 {
                    let mut pid = 0u32;
                    GetWindowThreadProcessId(hwnd, Some(&mut pid));
                    out.push((pid, String::from_utf16_lossy(&buf[..n as usize])));
                }
            }
        }
        BOOL(1)
    }
    let mut out: Vec<(u32, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each), windows::Win32::Foundation::LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// The meeting app among the microphone holders: Zoom / Teams by process, or a browser that
/// has a window (`titles` = `(process exe name, window title)`) titled "Meet".
pub fn detect_meeting(mic: &[Holder], titles: &[(String, String)]) -> Option<&'static str> {
    if let Some(m) = mic.iter().find_map(|h| meeting::meeting_app(&h.exe, "")) {
        return Some(m);
    }
    mic.iter().filter(|h| meeting::is_browser(&h.exe)).find_map(|h| {
        titles.iter().filter(|(proc_name, _)| proc_name.eq_ignore_ascii_case(&h.exe)).find_map(|(_, title)| meeting::meeting_app(&h.exe, title))
    })
}

/// Read both device keys and decide which meeting app (if any) holds the microphone.
fn read_consent(cam: Option<&Key>, mic: Option<&Key>) -> Consent {
    let cam_e = cam.map(read_entries).unwrap_or_default();
    let mic_e = mic.map(read_entries).unwrap_or_default();
    let claims = |e: &RawEntry| consent_in_use(e.start, e.stop);
    let procs = if cam_e.iter().chain(mic_e.iter()).any(claims) { Some(Procs::snapshot()) } else { None };
    let running = |p: &str| procs.as_ref().is_none_or(|ps| ps.path_running(p));
    let camera = holders_from_entries(&cam_e, &running);
    let mic = holders_from_entries(&mic_e, &running);

    // Window titles are only needed when a browser holds the microphone.
    let titles: Vec<(String, String)> = match &procs {
        Some(ps) if mic.iter().any(|h| meeting::is_browser(&h.exe)) => window_titles().into_iter().filter_map(|(pid, t)| ps.name_of(pid).map(|n| (n.to_string(), t))).collect(),
        _ => Vec::new(),
    };
    let meeting = detect_meeting(&mic, &titles);
    Consent { camera, mic, meeting }
}

struct AutoEvent(HANDLE);

impl AutoEvent {
    fn new() -> Option<AutoEvent> {
        unsafe { CreateEventW(None, false, false, None).ok().map(AutoEvent) }
    }
}

impl Drop for AutoEvent {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn service(ctx: ServiceCtx) {
    let root = consent_root();
    let (Some(ev_cam), Some(ev_mic)) = (AutoEvent::new(), AutoEvent::new()) else {
        log_error!("privacy: cannot create events");
        return;
    };
    let mut last: Option<Consent> = None;
    while !ctx.should_stop() {
        let cam = Key::open(&format!(r"{root}\webcam"));
        let mic = Key::open(&format!(r"{root}\microphone"));
        if cam.is_none() && mic.is_none() {
            // Never used a camera or microphone yet (keys appear later): look again in a while.
            if !ctx.sleep(Duration::from_secs(30)) {
                return;
            }
            continue;
        }
        if let Some(k) = &cam {
            k.watch(ev_cam.0);
        }
        if let Some(k) = &mic {
            k.watch(ev_mic.0);
        }
        loop {
            let snap = read_consent(cam.as_ref(), mic.as_ref());
            if last.as_ref() != Some(&snap) {
                let s = snap.clone();
                Ui::post(move |app| apply(app, s));
                last = Some(snap.clone());
            }
            // Wake on a registry change or stop; sweep periodically only while a device is held.
            let timeout = if snap.any() { SWEEP.as_millis() as u32 } else { INFINITE };
            let handles = [ctx.stop_event(), ev_cam.0, ev_mic.0];
            let r = unsafe { WaitForMultipleObjects(&handles, false, timeout) };
            let which = r.0.wrapping_sub(WAIT_OBJECT_0.0);
            match which {
                0 => return,
                1 | 2 => {
                    // Re-arm the one that fired; coalesce the Start/Stop write burst.
                    let (k, ev) = if which == 1 { (&cam, &ev_cam) } else { (&mic, &ev_mic) };
                    if let Some(k) = k {
                        k.watch(ev.0);
                    }
                    if unsafe { WaitForSingleObject(ctx.stop_event(), 120) } == WAIT_OBJECT_0 {
                        return;
                    }
                }
                _ => {} // timeout: sweep
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_root_override_needs_debug() {
        assert_eq!(consent_root_from(Some(r"Software\MokuTest".into()), false), REAL_ROOT);
        assert_eq!(consent_root_from(Some(r"Software\MokuTest".into()), true), r"Software\MokuTest");
        assert_eq!(consent_root_from(Some(String::new()), true), REAL_ROOT);
        assert_eq!(consent_root_from(None, true), REAL_ROOT);
    }

    #[test]
    fn consent_store_in_use_parsing() {
        // "in use" = started and not stopped (LastUsedTimeStop == 0).
        assert!(consent_in_use(134_370_000_000_000_000, 0));
        assert!(!consent_in_use(0, 0), "never used");
        assert!(!consent_in_use(134_370_000_000_000_000, 134_370_000_500_000_000), "stopped");
        assert!(!consent_in_use(0, 134_370_000_500_000_000));
    }

    fn e(name: &str, packaged: bool, start: u64, stop: u64) -> RawEntry {
        RawEntry { name: name.into(), packaged, start, stop }
    }

    #[test]
    fn consent_store_entries_to_holders() {
        let entries = vec![
            e("C:#Program Files#Zoom#bin#Zoom.exe", false, 10, 0),
            e("MSTeams_8wekyb3d8bbwe", true, 10, 0),
            e("Microsoft.WindowsCamera_8wekyb3d8bbwe", true, 10, 99), // stopped
            e("C:#Program Files#Google#Chrome#Application#chrome.exe", false, 0, 0), // never used
            e("NonPackaged", true, 0, 0),                              // the container key itself
        ];
        let h = holders_from_entries(&entries, &|_| true);
        assert_eq!(h.len(), 2, "{h:?}");
        assert_eq!(h[0], Holder { name: "Zoom".into(), exe: "Zoom.exe".into() });
        assert_eq!(h[1], Holder { name: "Teams".into(), exe: "ms-teams.exe".into() });
    }

    #[test]
    fn consent_store_stale_entries_are_dropped() {
        // An old Discord build whose process is gone still has Stop == 0 in the registry.
        let entries = vec![
            e("C:#Users#me#AppData#Local#Discord#app-1.0.9241#Discord.exe", false, 10, 0),
            e("C:#Users#me#AppData#Local#Discord#app-1.0.9264#Discord.exe", false, 10, 0),
        ];
        let running = |p: &str| p == r"C:\Users\me\AppData\Local\Discord\app-1.0.9264\Discord.exe";
        let h = holders_from_entries(&entries, &running);
        assert_eq!(h, vec![Holder { name: "Discord".into(), exe: "Discord.exe".into() }]);
    }

    #[test]
    fn consent_store_dedupes_same_app() {
        let entries = vec![e("C:#a#chrome.exe", false, 10, 0), e("C:#b#chrome.exe", false, 10, 0)];
        let h = holders_from_entries(&entries, &|_| true);
        assert_eq!(h, vec![Holder { name: "Chrome".into(), exe: "chrome.exe".into() }]);
    }

    #[test]
    fn consent_store_packaged_apps_are_trusted_without_a_process_check() {
        let entries = vec![e("Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe", true, 10, 0)];
        let h = holders_from_entries(&entries, &|_| false);
        assert_eq!(h.len(), 1);
        assert!(h[0].exe.is_empty());
    }

    /// Real Win32: an off-screen, non-activating window of this very process is found by title,
    /// and the process snapshot knows this process by name and by full image path.
    #[test]
    fn window_titles_and_process_snapshot_see_this_process() {
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE};
        use windows::core::w;
        let me = std::process::id();
        let exe = std::env::current_exe().expect("current exe");
        let name = exe.file_name().and_then(|n| n.to_str()).expect("exe name").to_lowercase();

        let hwnd = unsafe {
            CreateWindowExW(WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0), w!("STATIC"), w!("Meet - moku-test-window"), WS_POPUP | WS_VISIBLE, -32000, -32000, 10, 10, None, None, None, None)
        }
        .expect("create test window");
        let titles = window_titles();
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        assert!(titles.iter().any(|(pid, t)| *pid == me && t == "Meet - moku-test-window"), "window of this process not listed");

        let procs = Procs::snapshot();
        assert_eq!(procs.name_of(me), Some(name.as_str()));
        assert!(procs.path_running(exe.to_str().expect("utf-8 path")));
        assert!(!procs.path_running(r"C:\no-such-dir\moku-ghost.exe"));
    }

    fn holder(exe: &str) -> Holder {
        Holder { name: exe.into(), exe: exe.into() }
    }
    fn win(exe: &str, title: &str) -> (String, String) {
        (exe.to_lowercase(), title.to_string())
    }

    #[test]
    fn detect_meeting_from_mic_holders_and_window_titles() {
        // Zoom / Teams are recognised by process alone.
        assert_eq!(detect_meeting(&[holder("Zoom.exe")], &[]), Some("Zoom"));
        assert_eq!(detect_meeting(&[holder("obs64.exe"), holder("ms-teams.exe")], &[]), Some("Teams"));
        // A browser needs a "Meet" window of its own.
        let chrome = [holder("chrome.exe")];
        assert_eq!(detect_meeting(&chrome, &[win("chrome.exe", "Meet - abc-defg-hij - Google Chrome")]), Some("Meet"));
        assert_eq!(detect_meeting(&chrome, &[win("chrome.exe", "YouTube")]), None);
        assert_eq!(detect_meeting(&chrome, &[]), None);
        // A "Meet" window of another browser / app does not make this browser's mic use a meeting.
        assert_eq!(detect_meeting(&chrome, &[win("msedge.exe", "Google Meet")]), None);
        assert_eq!(detect_meeting(&[holder("obs64.exe")], &[win("obs64.exe", "Meet")]), None);
        assert_eq!(detect_meeting(&[], &[win("chrome.exe", "Meet")]), None);
    }
}
