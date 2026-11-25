//! Feature `meeting` — Task 3 (track B).
//!
//! Meeting mode: detect Zoom / Teams / Meet, show the pill with a mute button, and mute the
//! default capture device (`IAudioEndpointVolume::SetMute`). Owns `App::set_meeting`; handles the
//! action `"meeting.toggle"` (the control panel dispatches it) and `"meeting.mute"` (the pill).
//!
//! Detection comes from the privacy service (`privacy.rs`): the microphone is held by `Zoom.exe`
//! or `ms-teams.exe`, or by a browser that has a window titled "Meet". Meeting mode is *active*
//! while a meeting is detected or the user switched it on by hand; toggling it off during a
//! detected meeting keeps it off until that meeting ends.
//!
//! Muting is the one thing here that changes the user's system, so it is careful: the mic is
//! only ever muted by an explicit click, a marker file records that *we* muted it — and which
//! endpoint (`meeting-mute.flag`: "moku-mute v2" + the endpoint id) — and that same device is
//! restored (`IMMDeviceEnumerator::GetDevice`, even if the default changed meanwhile) when meeting
//! mode ends, when Moku exits, and at the next start if Moku was killed while it was muted. An
//! old-format marker restores the default device. Audio calls are cross-process COM, so they run on short-lived worker
//! threads, never on the UI thread.

use std::time::Duration;

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eConsole};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::core::{GUID, HSTRING};

use super::privacy::{self, Consent};
use crate::activity::{Activity, priority};
use crate::app::{App, MeetingState, Ui};
use crate::i18n::tr;
use crate::registry::*;
use crate::ui::*;

pub fn register(r: &mut Registry) {
    r.add_state(Meeting::default());
    r.on_action("meeting", on_action);
    r.on_config(on_config);
    r.on_start(on_start);
    r.on_exit(on_exit);
}

// ---- detection (pure, tested) ------------------------------------------------------------------

const BROWSERS: [&str; 6] = ["chrome.exe", "msedge.exe", "firefox.exe", "brave.exe", "opera.exe", "vivaldi.exe"];

pub fn is_browser(process: &str) -> bool {
    BROWSERS.iter().any(|b| process.eq_ignore_ascii_case(b))
}

/// "Meet" as a word (not "Meeting", "Meetup"), or a meet.google.com title.
fn title_is_meet(title: &str) -> bool {
    if title.to_ascii_lowercase().contains("meet.google.com") {
        return true;
    }
    title.match_indices("Meet").any(|(i, m)| {
        let before = title[..i].chars().next_back();
        let after = title[i + m.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphabetic()) && !after.is_some_and(|c| c.is_alphabetic())
    })
}

/// Which meeting app a process (and, for browsers, a window title) is.
pub fn meeting_app(process: &str, title: &str) -> Option<&'static str> {
    let p = process.to_ascii_lowercase();
    match p.as_str() {
        "zoom.exe" => Some("Zoom"),
        "ms-teams.exe" | "teams.exe" => Some("Teams"),
        _ if is_browser(&p) && title_is_meet(title) => Some("Meet"),
        _ => None,
    }
}

// ---- model (pure, tested) ----------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
struct Model {
    /// Meeting app detected from the microphone holders.
    detected: Option<String>,
    mic_in_use: bool,
    camera_in_use: bool,
    /// Switched on by hand (`meeting.toggle`).
    manual: bool,
    /// Switched off by hand while a meeting is detected; lasts until that meeting ends.
    dismissed: bool,
    /// Last known mute state of the default capture device.
    muted: bool,
}

impl Model {
    fn active(&self) -> bool {
        self.manual || (self.detected.is_some() && !self.dismissed)
    }

    fn detect(&mut self, detected: Option<String>, mic: bool, camera: bool) {
        if detected.is_none() {
            self.dismissed = false;
        }
        self.detected = detected;
        self.mic_in_use = mic;
        self.camera_in_use = camera;
    }

    fn toggle(&mut self) {
        if self.active() {
            self.manual = false;
            self.dismissed = self.detected.is_some();
        } else {
            self.manual = true;
            self.dismissed = false;
        }
    }

    fn state(&self) -> MeetingState {
        MeetingState { active: self.active(), app_name: self.detected.clone(), mic_in_use: self.mic_in_use, camera_in_use: self.camera_in_use, mic_muted: self.muted }
    }
}

#[derive(Default)]
struct Meeting {
    model: Model,
    /// A mute / unmute / read worker is running.
    busy: bool,
    /// We turned the microphone off, so we owe the user a restore.
    we_muted: bool,
    /// Endpoint id of the device we muted (restored by id, not "whatever is default now").
    muted_id: Option<String>,
    /// Meeting mode was active at the last `sync` (to notice a start / an end).
    was_active: bool,
}

// ---- reducer + UI ------------------------------------------------------------------------------

/// Called by `privacy` with each new snapshot of who holds the camera / microphone.
pub fn on_consent(app: &mut App, c: &Consent) {
    let detected = if app.config().system.meeting_mode { c.meeting.map(str::to_string) } else { None };
    app.state_mut::<Meeting>().model.detect(detected, !c.mic.is_empty(), !c.camera.is_empty());
    sync(app);
}

/// Config reloaded (`meeting_mode` may have flipped): re-evaluate the last snapshot.
fn on_config(app: &mut App) {
    let c = privacy::current(app);
    on_consent(app, &c);
}

/// Publish the model: `App::set_meeting`, the pill, the auto-restore of the mic.
fn sync(app: &mut App) {
    let (state, active, was_active, we_muted, busy) = {
        let m = app.state::<Meeting>();
        (m.model.state(), m.model.active(), m.was_active, m.we_muted, m.busy)
    };
    app.set_meeting(state);
    if active {
        app.set_activity(
            Activity::persistent("meeting.pill", priority::MEETING, pill_node)
                .tab("control")
                .bubble(|app| stack([privacy::dots(app.meeting().camera_in_use, app.meeting().mic_in_use, mic_color(app.meeting().mic_muted))])),
        );
    } else {
        app.clear_activity("meeting.pill");
    }
    app.state_mut::<Meeting>().was_active = active;
    privacy::sync_dots(app);

    if active && !was_active {
        refresh_mute(app); // show the real mute state when the pill appears
    } else if !active && we_muted && !busy {
        set_mute(app, false); // meeting over: give the microphone back
    }
}

fn mic_color(muted: bool) -> Rgba {
    if muted { color::RED } else { color::ORANGE }
}

fn pill_node(app: &App) -> Node {
    let m = app.state::<Meeting>();
    let s = &m.model;
    let title = s.detected.clone().unwrap_or_else(|| tr("Mode rapat", "Meeting mode").to_string());
    let status = if s.muted {
        tr("mikrofon dibisukan", "microphone muted")
    } else if s.mic_in_use {
        tr("mikrofon aktif", "microphone active")
    } else if s.camera_in_use {
        tr("kamera aktif", "camera active")
    } else {
        tr("aktif", "on")
    };
    let lead = if s.camera_in_use || s.mic_in_use { privacy::dots(s.camera_in_use, s.mic_in_use, mic_color(s.muted)) } else { icon(icons::MIC).color(color::TEXT2) };
    let (glyph, label) = if s.muted { (icons::MIC_OFF, tr("Aktifkan", "Unmute")) } else { (icons::MIC, tr("Bisukan", "Mute")) };
    let button = capsule(glyph, label, "meeting.mute");
    pill([lead, text(title).semibold().max_w(160.0), text(status).secondary(), if m.busy { button.opacity(0.55) } else { button }])
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    if !matches!(ev.kind, ActionKind::Click) {
        return;
    }
    match ev.name() {
        "toggle" => {
            app.state_mut::<Meeting>().model.toggle();
            sync(app);
        }
        "mute" => {
            let target = !app.state::<Meeting>().model.muted;
            set_mute(app, target);
        }
        // Debug path: `mokuctl debug click meeting.sample:<zoom|teams|meet|off>`.
        "sample" => {
            let detected = match ev.arg() {
                Some("zoom") => Some("Zoom"),
                Some("teams") => Some("Teams"),
                Some("meet") => Some("Meet"),
                _ => None,
            };
            let on = detected.is_some();
            let m = &mut app.state_mut::<Meeting>().model;
            m.manual = false;
            m.detect(detected.map(str::to_string), on, on);
            sync(app);
        }
        _ => {}
    }
}

// ---- muting (worker threads) --------------------------------------------------------------------

fn flag_file() -> std::path::PathBuf {
    moku_core::paths::data_file("meeting-mute.flag")
}

/// Initialises COM (multithreaded) for the current thread until dropped.
pub(crate) struct ComGuard;

impl ComGuard {
    pub(crate) fn init() -> ComGuard {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        ComGuard
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Which capture endpoint a mute operation acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    /// The current default capture device (muting; a restore from an old-format flag file).
    Default,
    /// A specific endpoint by its `IMMDevice::GetId` (restoring the device we muted, even if the
    /// default device changed meanwhile).
    Device(String),
}

const FLAG_V2: &str = "moku-mute v2";

/// The marker file: "moku-mute v2" and the endpoint id of the device we muted.
fn flag_contents(id: &str) -> String {
    format!("{FLAG_V2}\n{id}\n")
}

/// Which device a marker file asks to restore. Old files (v0.1 before this fix) only said "the
/// default microphone": fall back to the default device for those.
fn parse_flag(s: &str) -> Target {
    let mut lines = s.lines().map(str::trim);
    match (lines.next(), lines.next()) {
        (Some(FLAG_V2), Some(id)) if !id.is_empty() => Target::Device(id.to_string()),
        _ => Target::Default,
    }
}

/// A capture device's endpoint volume and its endpoint id. The caller holds a [`ComGuard`]
/// declared before the result, so the COM objects are released before `CoUninitialize`.
fn open_capture(target: &Target) -> windows::core::Result<(IAudioEndpointVolume, String)> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = match target {
            Target::Default => enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?,
            Target::Device(id) => enumerator.GetDevice(&HSTRING::from(id.as_str()))?,
        };
        let pid = device.GetId()?;
        let id = pid.to_string().unwrap_or_default();
        CoTaskMemFree(Some(pid.0 as _));
        Ok((device.Activate(CLSCTX_ALL, None)?, id))
    }
}

fn read_mute(target: &Target) -> Result<bool, String> {
    let _com = ComGuard::init();
    let (v, _) = open_capture(target).map_err(|e| e.to_string())?;
    unsafe { v.GetMute().map(|b| b.as_bool()).map_err(|e| e.to_string()) }
}

/// Set the mute state of `target`; returns the state read back and the endpoint id. Muting
/// resolves the default device once and records *its id* in the marker file before muting it;
/// the restore then unmutes that same device even if the default changed. The marker is removed
/// once the device is no longer muted, so a crash while muted is repaired at the next start.
fn write_mute(mute: bool, target: &Target) -> Result<(bool, String), String> {
    let _com = ComGuard::init();
    let (v, id) = open_capture(target).map_err(|e| e.to_string())?;
    if mute {
        let _ = moku_core::paths::ensure_data_dir();
        if let Err(e) = std::fs::write(flag_file(), flag_contents(&id)) {
            // Without the marker a crash could leave the mic muted for good: do not mute.
            return Err(format!("cannot write the mute marker: {e}"));
        }
    }
    let set = unsafe { v.SetMute(mute, &GUID::zeroed()) };
    let now = unsafe { v.GetMute() }.ok().map(|b| b.as_bool());
    // Known to be unmuted (restored, or the mute did not take): nothing left to repair.
    if now == Some(false) {
        let _ = std::fs::remove_file(flag_file());
    }
    set.map_err(|e| e.to_string())?;
    Ok((now.unwrap_or(mute), id))
}
/// Ask a worker to mute the default microphone / unmute the one we muted, and apply the result
/// on the UI thread.
fn set_mute(app: &mut App, mute: bool) {
    if app.state::<Meeting>().busy {
        return;
    }
    let target = match (&app.state::<Meeting>().muted_id, mute) {
        (Some(id), false) => Target::Device(id.clone()),
        _ => Target::Default,
    };
    app.state_mut::<Meeting>().busy = true;
    app.request_redraw(crate::app::Redraw::Pill);
    let spawned = std::thread::Builder::new().name("meeting-mute".into()).spawn(move || {
        let r = write_mute(mute, &target);
        Ui::post(move |app| mute_done(app, mute, r));
    });
    if spawned.is_err() {
        app.state_mut::<Meeting>().busy = false;
    }
}

fn mute_done(app: &mut App, wanted: bool, result: Result<(bool, String), String>) {
    let m = app.state_mut::<Meeting>();
    m.busy = false;
    match result {
        Ok((now, id)) => {
            log_info!("meeting: microphone muted = {now}");
            m.model.muted = now;
            // We owe a restore only if we are the ones who muted it.
            if now && wanted {
                m.we_muted = true;
                m.muted_id = Some(id);
            } else if !now {
                m.we_muted = false;
                m.muted_id = None;
            }
        }
        Err(e) => log_warn!("meeting: microphone mute failed: {e}"),
    }
    sync(app);
}
/// Read the current mute state (no change to the device): the device we muted, else the default.
fn refresh_mute(app: &mut App) {
    if app.state::<Meeting>().busy {
        return;
    }
    let target = app.state::<Meeting>().muted_id.clone().map_or(Target::Default, Target::Device);
    app.state_mut::<Meeting>().busy = true;
    let spawned = std::thread::Builder::new().name("meeting-mute".into()).spawn(move || {
        let r = read_mute(&target);
        Ui::post(move |app| {
            let m = app.state_mut::<Meeting>();
            m.busy = false;
            if let Ok(v) = r {
                m.model.muted = v;
            }
            sync(app);
        });
    });
    if spawned.is_err() {
        app.state_mut::<Meeting>().busy = false;
    }
}

/// A previous Moku was killed while it had the microphone muted: give that device back (an
/// old-format marker only knows "the default microphone").
fn on_start(_app: &mut App) {
    let Ok(s) = std::fs::read_to_string(flag_file()) else { return };
    let target = parse_flag(&s);
    log_warn!("meeting: restoring a microphone left muted by a previous run ({})", if target == Target::Default { "default device" } else { "the muted device" });
    let _ = std::thread::Builder::new().name("meeting-restore".into()).spawn(move || {
        if let Err(e) = write_mute(false, &target) {
            log_warn!("meeting: restore failed ({e}); will retry at the next start");
        }
    });
}

/// Quitting while we hold the microphone muted: unmute (bounded wait, the exit path is short).
fn on_exit(app: &mut App) {
    let m = app.state::<Meeting>();
    if !m.we_muted {
        return;
    }
    let target = m.muted_id.clone().map_or(Target::Default, Target::Device);
    if let Ok(h) = std::thread::Builder::new().name("meeting-restore".into()).spawn(move || {
        let _ = write_mute(false, &target);
    }) {
        let deadline = std::time::Instant::now() + Duration::from_millis(700);
        while !h.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_app_from_process_name() {
        assert_eq!(meeting_app("Zoom.exe", ""), Some("Zoom"));
        assert_eq!(meeting_app("zoom.exe", ""), Some("Zoom"));
        assert_eq!(meeting_app("ms-teams.exe", ""), Some("Teams"));
        assert_eq!(meeting_app("Teams.exe", ""), Some("Teams"));
        assert_eq!(meeting_app("notepad.exe", "Meet"), None, "title only counts for browsers");
        assert_eq!(meeting_app("obs64.exe", ""), None);
        assert_eq!(meeting_app("", ""), None);
    }

    #[test]
    fn meeting_app_from_browser_title() {
        assert_eq!(meeting_app("chrome.exe", "Meet - abc-defg-hij - Google Chrome"), Some("Meet"));
        assert_eq!(meeting_app("msedge.exe", "Google Meet - Microsoft Edge"), Some("Meet"));
        assert_eq!(meeting_app("firefox.exe", "Rapat Meet"), Some("Meet"));
        assert_eq!(meeting_app("chrome.exe", "meet.google.com/abc-defg-hij"), Some("Meet"));
        assert_eq!(meeting_app("chrome.exe", "Team Meeting notes - Docs"), None, "Meeting is not Meet");
        assert_eq!(meeting_app("chrome.exe", "YouTube"), None);
        assert_eq!(meeting_app("chrome.exe", ""), None);
        assert_eq!(meeting_app("chrome.exe", "Meetup groups near you"), None);
        assert_eq!(meeting_app("chrome.exe", "Teammeet"), None);
    }

    #[test]
    fn model_auto_detect_activates_and_ends() {
        let mut m = Model::default();
        assert!(!m.active());
        m.detect(Some("Zoom".into()), true, true);
        assert!(m.active());
        let s = m.state();
        assert_eq!((s.active, s.app_name.as_deref(), s.mic_in_use, s.camera_in_use), (true, Some("Zoom"), true, true));
        m.detect(None, false, false);
        assert!(!m.active());
        assert_eq!(m.state().app_name, None);
    }

    #[test]
    fn model_manual_toggle() {
        let mut m = Model::default();
        m.toggle();
        assert!(m.active() && m.manual);
        m.toggle();
        assert!(!m.active());
    }

    #[test]
    fn model_toggle_off_during_detected_meeting_lasts_until_it_ends() {
        let mut m = Model::default();
        m.detect(Some("Teams".into()), true, false);
        assert!(m.active());
        m.toggle(); // user switches it off
        assert!(!m.active());
        m.detect(Some("Teams".into()), true, true); // same meeting, still detected
        assert!(!m.active(), "stays off");
        assert_eq!(m.state().app_name.as_deref(), Some("Teams"), "detection is still reported");
        m.detect(None, false, false); // meeting ends: dismissal is forgotten
        m.detect(Some("Zoom".into()), true, false);
        assert!(m.active(), "next meeting activates again");
        // Toggling on again by hand after dismissing.
        m.toggle();
        assert!(!m.active());
        m.toggle();
        assert!(m.active());
    }

    #[test]
    fn mute_flag_names_the_muted_device() {
        let id = "{0.0.1.00000000}.{6b0f6c2e-1111-2222-3333-444455556666}";
        assert_eq!(parse_flag(&flag_contents(id)), Target::Device(id.into()));
        assert_eq!(parse_flag(&flag_contents(id).replace('\n', "\r\n")), Target::Device(id.into()));
        // Old format (before the id was stored) and damaged files: the default device.
        assert_eq!(parse_flag("moku muted the default microphone\n"), Target::Default);
        assert_eq!(parse_flag("moku-mute v2\n\n"), Target::Default);
        assert_eq!(parse_flag(""), Target::Default);
    }

    /// Read-only: the id of the default capture device opens the same device again
    /// (`GetDevice`), which is what the restore relies on. Skipped without a microphone.
    #[test]
    fn capture_device_reopens_by_id() {
        let _com = ComGuard::init();
        let Ok((v, id)) = open_capture(&Target::Default) else { return };
        let by_id = open_capture(&Target::Device(id.clone())).expect("reopen by id");
        assert_eq!(by_id.1, id);
        let a = unsafe { v.GetMute() }.map(|b| b.as_bool()).ok();
        let b = unsafe { by_id.0.GetMute() }.map(|b| b.as_bool()).ok();
        assert_eq!(a, b);
    }

    #[test]
    fn model_state_carries_mute() {
        let m = Model { muted: true, ..Model::default() };
        assert!(m.state().mic_muted);
        assert!(!m.state().active, "muted alone is not a meeting");
    }
}
