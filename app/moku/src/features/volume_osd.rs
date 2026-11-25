//! Feature `volume_osd` — Task 2 (track A).
//!
//! Core Audio on a service thread (`audio`): `IAudioEndpointVolume` callbacks on the default
//! render device, re-bound when the default device changes (`IMMNotificationClient`). Every
//! change from outside Moku (keys, the Windows flyout, apps, the music pill's scroll) shows
//! the volume pill (priority OSD, transient 1.5 s). The shared [`Audio`] state (level, mute,
//! output device name) is also read by the Musik and Kontrol panels; they change the volume
//! through [`set_volume`] / [`nudge`] / [`toggle_mute`].

use std::sync::LazyLock;

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use std::ffi::c_void;

use windows::Win32::Foundation::{E_NOINTERFACE, E_POINTER, PROPERTYKEY, S_OK};
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Vtbl};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc, PropVariantToUInt32};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, STGM_READ};
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR};

use crate::activity::{OSD_SECS, priority};
use crate::app::{App, Redraw, Ui};
use crate::features::media::inbox::Inbox;
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::*;

pub const OSD_ID: &str = "volume.osd";

/// Event context of Moku's own quiet changes (panel sliders): no OSD for those.
const CTX_QUIET: GUID = GUID::from_u128(0x6d6f6b75_766f_6c00_9a1e_0000_51_1e_4e_74);

/// Audio endpoint state, shared with `media` and `control`.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Audio {
    /// A default render device is bound.
    pub ready: bool,
    pub volume: f32,
    pub muted: bool,
    /// Friendly name of the default output ("Speakers (Realtek(R) Audio)").
    pub device: String,
    /// Headphones / headset form factor.
    pub headphones: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cmd {
    /// Bind to the (new) default device.
    Rebind,
    /// Set the level; `true` = quiet (no OSD).
    Set(f32, bool),
    Nudge(f32),
    ToggleMute,
}

static INBOX: LazyLock<Inbox<Cmd>> = LazyLock::new(Inbox::new);

pub fn register(r: &mut Registry) {
    r.add_state(Audio::default());
    r.on_action("volume", on_action);
    // The audio thread also serves the Kontrol slider and the Musik panel's volume button.
    r.add_service(ServiceSpec { name: "audio", enabled: |c| c.volume_osd.enabled || c.control.enabled || c.media.enabled, run: service });
}

// ---- API for the other track-A features ---------------------------------------------------------

/// Set the master volume (0–1) quietly (no OSD) — for sliders the user is looking at.
pub fn set_volume(v: f32) {
    INBOX.push(Cmd::Set(v.clamp(0.0, 1.0), true));
}

/// Change the volume by `delta` (shows the OSD).
pub fn nudge(delta: f32) {
    if delta != 0.0 {
        INBOX.push(Cmd::Nudge(delta));
    }
}

pub fn toggle_mute() {
    INBOX.push(Cmd::ToggleMute);
}

// ---- UI ------------------------------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    match (ev.name(), ev.kind) {
        ("osd", ActionKind::Scroll(n)) => nudge(n * 0.02),
        ("osd", ActionKind::Click) => app.open_panel(Some("control")),
        // Debug: show the pill with the current (real) level, without changing anything.
        ("test", ActionKind::Click) => show_osd(app),
        // Debug (the name starts with "debug_"): show another output device name (an invented one for
        // screenshots) on the Kontrol tab. The next real device change puts the real name back.
        ("debug_device", ActionKind::Click) => {
            if let Some(name) = ev.arg() {
                let a = app.state_mut::<Audio>();
                a.device = name.to_string();
                a.headphones = false;
                app.request_redraw(Redraw::All);
            }
        }
        _ => {}
    }
}

fn glyph(a: &Audio) -> &'static str {
    if a.muted || a.volume <= 0.005 {
        icons::MUTE
    } else if a.headphones {
        icons::HEADPHONE
    } else {
        icons::VOLUME
    }
}

fn osd_pill(app: &App) -> Node {
    let a = app.state::<Audio>();
    let v = if a.muted { 0.0 } else { a.volume };
    pill([
        icon(glyph(a)).font_size(14.0),
        progress(v).w(112.0).thickness(4.0).track(Rgba::hex(0xffffff, 0.22)),
        text(format!("{}", (v * 100.0).round() as i32)).font_size(12.0).tertiary().w(22.0).text_right(),
    ])
    .action("volume.osd")
}

fn show_osd(app: &mut App) {
    if app.config().volume_osd.enabled && app.state::<Audio>().ready {
        app.alert(OSD_ID, priority::OSD, OSD_SECS, osd_pill);
    }
}

/// Level / mute from the endpoint callback.
fn on_level(app: &mut App, volume: f32, muted: bool, show: bool) {
    let a = app.state_mut::<Audio>();
    let changed = (a.volume - volume).abs() > 0.0005 || a.muted != muted;
    a.volume = volume;
    a.muted = muted;
    if changed && show {
        show_osd(app);
    }
    app.request_redraw(Redraw::All);
}

fn on_device(app: &mut App, a: Audio) {
    *app.state_mut::<Audio>() = a;
    app.request_redraw(Redraw::All);
}

// ---- Core Audio thread --------------------------------------------------------------------------

// The two callbacks are stateless, so each is one static COM object with a static vtable and
// a no-op reference count (it is never freed). Hand-written because `#[implement]` expands to
// `::windows_core`, which this crate does not depend on directly.

#[repr(C)]
struct StaticCom<V: 'static> {
    vtbl: &'static V,
}
// SAFETY: immutable, only read by COM through the vtable pointer.
unsafe impl<V> Sync for StaticCom<V> {}

unsafe extern "system" fn add_ref(_this: *mut c_void) -> u32 {
    1
}
unsafe extern "system" fn release(_this: *mut c_void) -> u32 {
    1
}

unsafe fn query<I: Interface>(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() || iid.is_null() {
        return E_POINTER;
    }
    let iid = unsafe { *iid };
    if iid == I::IID || iid == IUnknown::IID {
        unsafe { *out = this };
        S_OK
    } else {
        unsafe { *out = std::ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "system" fn vol_qi(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    unsafe { query::<IAudioEndpointVolumeCallback>(this, iid, out) }
}

unsafe extern "system" fn vol_notify(_this: *mut c_void, p: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> HRESULT {
    if let Some(d) = unsafe { p.as_ref() } {
        let (v, m, quiet) = (d.fMasterVolume, d.bMuted.as_bool(), d.guidEventContext == CTX_QUIET);
        Ui::post(move |app| on_level(app, v, m, !quiet));
    }
    S_OK
}

static VOL_VTBL: IAudioEndpointVolumeCallback_Vtbl = IAudioEndpointVolumeCallback_Vtbl {
    base__: IUnknown_Vtbl { QueryInterface: vol_qi, AddRef: add_ref, Release: release },
    OnNotify: vol_notify,
};
static VOL_CB: StaticCom<IAudioEndpointVolumeCallback_Vtbl> = StaticCom { vtbl: &VOL_VTBL };

unsafe extern "system" fn dev_qi(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    unsafe { query::<IMMNotificationClient>(this, iid, out) }
}
unsafe extern "system" fn dev_state(_this: *mut c_void, _id: PCWSTR, _s: DEVICE_STATE) -> HRESULT {
    S_OK
}
unsafe extern "system" fn dev_added(_this: *mut c_void, _id: PCWSTR) -> HRESULT {
    S_OK
}
unsafe extern "system" fn dev_default(_this: *mut c_void, flow: EDataFlow, role: ERole, _id: PCWSTR) -> HRESULT {
    // Fires once per role; follow the console role (what the volume keys change).
    if flow == eRender && role == eConsole {
        INBOX.push(Cmd::Rebind);
    }
    S_OK
}
unsafe extern "system" fn dev_prop(_this: *mut c_void, _id: PCWSTR, _key: PROPERTYKEY) -> HRESULT {
    S_OK
}

static DEV_VTBL: IMMNotificationClient_Vtbl = IMMNotificationClient_Vtbl {
    base__: IUnknown_Vtbl { QueryInterface: dev_qi, AddRef: add_ref, Release: release },
    OnDeviceStateChanged: dev_state,
    OnDeviceAdded: dev_added,
    OnDeviceRemoved: dev_added,
    OnDefaultDeviceChanged: dev_default,
    OnPropertyValueChanged: dev_prop,
};
static DEV_CB: StaticCom<IMMNotificationClient_Vtbl> = StaticCom { vtbl: &DEV_VTBL };

fn com_ref<I: Interface, V>(obj: &'static StaticCom<V>) -> I {
    // SAFETY: `obj` starts with a vtable pointer of interface `I`; its refcount is a no-op.
    unsafe { I::from_raw(obj as *const StaticCom<V> as *mut c_void) }
}

fn prop_string(store: &windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
    unsafe {
        let mut pv = store.GetValue(key).ok()?;
        let s = PropVariantToStringAlloc(&pv).ok().map(|p| {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as _));
            s
        });
        let _ = PropVariantClear(&mut pv);
        s
    }
}

fn prop_u32(store: &windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore, key: &PROPERTYKEY) -> Option<u32> {
    unsafe {
        let mut pv = store.GetValue(key).ok()?;
        let v = PropVariantToUInt32(&pv).ok();
        let _ = PropVariantClear(&mut pv);
        v
    }
}

/// Bind to the default render device: register the volume callback, post its state.
fn bind(en: &IMMDeviceEnumerator, cb: &IAudioEndpointVolumeCallback) -> Option<IAudioEndpointVolume> {
    unsafe {
        let dev = match en.GetDefaultAudioEndpoint(eRender, eConsole) {
            Ok(d) => d,
            Err(_) => {
                Ui::post(|app| on_device(app, Audio::default()));
                return None;
            }
        };
        let (name, headphones) = match dev.OpenPropertyStore(STGM_READ) {
            Ok(store) => {
                // EndpointFormFactor: 3 = headphones, 5 = headset.
                let ff = prop_u32(&store, &PKEY_AudioEndpoint_FormFactor).unwrap_or(0);
                (prop_string(&store, &PKEY_Device_FriendlyName).unwrap_or_default(), ff == 3 || ff == 5)
            }
            Err(_) => (String::new(), false),
        };
        let ep: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None).ok()?;
        if let Err(e) = ep.RegisterControlChangeNotify(cb) {
            log_warn!("audio: volume notifications unavailable: {e}");
        }
        let a = Audio {
            ready: true,
            volume: ep.GetMasterVolumeLevelScalar().unwrap_or(0.0),
            muted: ep.GetMute().map(|b| b.as_bool()).unwrap_or(false),
            device: short_name(&name),
            headphones,
        };
        Ui::post(move |app| on_device(app, a));
        Some(ep)
    }
}

/// "Headphones (Buds Pro)" → "Buds Pro"; "Speakers (Realtek(R) Audio)" stays as is but
/// without the generic prefix when a model is in parentheses.
pub fn short_name(s: &str) -> String {
    let s = s.trim();
    if let (Some(i), true) = (s.find(" ("), s.ends_with(')')) {
        let (head, inner) = (&s[..i], &s[i + 2..s.len() - 1]);
        let generic = ["speakers", "headphones", "headset", "speaker", "headset earphone", "earphones"];
        if generic.contains(&head.to_lowercase().as_str()) && !inner.to_lowercase().contains("audio") {
            return inner.to_string();
        }
    }
    s.to_string()
}

fn service(ctx: ServiceCtx) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    serve(&ctx);
    unsafe { CoUninitialize() };
}

fn serve(ctx: &ServiceCtx) {
    INBOX.clear();
    let en: IMMDeviceEnumerator = match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
        Ok(e) => e,
        Err(e) => {
            log_warn!("audio: no device enumerator: {e}");
            return;
        }
    };
    let dev_cb: IMMNotificationClient = com_ref(&DEV_CB);
    let vol_cb: IAudioEndpointVolumeCallback = com_ref(&VOL_CB);
    unsafe {
        let _ = en.RegisterEndpointNotificationCallback(&dev_cb);
    }
    let mut ep: Option<IAudioEndpointVolume> = None;
    INBOX.push(Cmd::Rebind);
    while let Some(batch) = INBOX.wait(ctx) {
        if batch.contains(&Cmd::Rebind) {
            if let Some(old) = ep.take() {
                unsafe {
                    let _ = old.UnregisterControlChangeNotify(&vol_cb);
                }
            }
            ep = bind(&en, &vol_cb);
        }
        let Some(e) = &ep else { continue };
        unsafe {
            // Slider drags: only the last value of a batch matters.
            if let Some(Cmd::Set(v, quiet)) = batch.iter().rev().find(|c| matches!(c, Cmd::Set(..))) {
                let ctx_guid = if *quiet { &CTX_QUIET as *const GUID } else { std::ptr::null() };
                if *v > 0.0 && e.GetMute().is_ok_and(|m| m.as_bool()) {
                    let _ = e.SetMute(false, ctx_guid);
                }
                let _ = e.SetMasterVolumeLevelScalar(*v, ctx_guid);
            }
            let delta: f32 = batch.iter().map(|c| if let Cmd::Nudge(d) = c { *d } else { 0.0 }).sum();
            if delta != 0.0 {
                let v = (e.GetMasterVolumeLevelScalar().unwrap_or(0.0) + delta).clamp(0.0, 1.0);
                if delta > 0.0 && e.GetMute().is_ok_and(|m| m.as_bool()) {
                    let _ = e.SetMute(false, std::ptr::null());
                }
                let _ = e.SetMasterVolumeLevelScalar(v, std::ptr::null());
            }
            if batch.iter().filter(|c| **c == Cmd::ToggleMute).count() % 2 == 1 {
                let m = e.GetMute().map(|m| m.as_bool()).unwrap_or(false);
                let _ = e.SetMute(!m, std::ptr::null());
            }
        }
    }
    unsafe {
        if let Some(e) = ep.take() {
            let _ = e.UnregisterControlChangeNotify(&vol_cb);
        }
        let _ = en.UnregisterEndpointNotificationCallback(&dev_cb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_short_names() {
        assert_eq!(short_name("Headphones (Buds Pro)"), "Buds Pro");
        assert_eq!(short_name("Speakers (Realtek(R) Audio)"), "Speakers (Realtek(R) Audio)");
        assert_eq!(short_name("LG TV"), "LG TV");
    }
}
