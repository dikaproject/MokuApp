//! System calls behind the Kontrol tab, on the `control` service thread (MTA): radios
//! (`Windows.Devices.Radios`), screen brightness (WMI `root\wmi`), dark mode (registry +
//! `WM_SETTINGCHANGE` broadcast). Nothing is loaded until the tab is first shown.

use std::sync::LazyLock;

use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Foundation::TypedEventHandler;
use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CoUninitialize, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW};
use windows::Win32::System::Variant::{VARENUM, VARIANT, VT_I4, VT_UI1, VariantClear, VariantToUInt32};
use windows::Win32::System::Wmi::*;
use windows::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE};
use windows::core::{BSTR, PCWSTR, w};

use crate::app::Ui;
use crate::features::media::inbox::Inbox;
use crate::services::ServiceCtx;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    /// Read radios + brightness (tab shown / every 2 s while visible).
    Refresh,
    RadiosChanged,
    SetRadio(Kind, bool),
    /// Brightness 0–100.
    SetBrightness(u8),
    SetDark(bool),
    /// The tab has been hidden for a while: drop the radio objects and the WMI connection
    /// (loaded again on the next `Refresh`).
    Release,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Wifi,
    Bluetooth,
}

pub static INBOX: LazyLock<Inbox<Cmd>> = LazyLock::new(Inbox::new);

/// What the service reports back.
#[derive(Clone, Debug, PartialEq)]
pub enum Report {
    /// Radio states (`None` = no such radio).
    Radios { wifi: Option<bool>, bt: Option<bool> },
    /// Brightness 0–100, or `None` = not supported (desktop monitor).
    Brightness(Option<u8>),
    Dark(bool),
}

fn post(r: Report) {
    Ui::post(move |app| super::on_report(app, r));
}

// ---- dark mode (registry; also read on the UI thread) ------------------------------------------

const PERSONALIZE: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");

fn reg_dword(name: PCWSTR) -> Option<u32> {
    let mut v: u32 = 0;
    let mut size = 4u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, PERSONALIZE, name, RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as _), Some(&mut size)) };
    (r == ERROR_SUCCESS).then_some(v)
}

/// Apps use the dark theme (`AppsUseLightTheme = 0`).
pub fn read_dark() -> bool {
    reg_dword(w!("AppsUseLightTheme")) == Some(0)
}

fn write_dark(dark: bool) {
    let light: u32 = if dark { 0 } else { 1 };
    for name in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
        let r = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, PERSONALIZE, name, REG_DWORD.0, Some(&light as *const u32 as _), 4) };
        if r != ERROR_SUCCESS {
            log_warn!("control: cannot write theme setting: {r:?}");
        }
    }
    unsafe {
        SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, WPARAM(0), LPARAM(w!("ImmersiveColorSet").as_ptr() as isize), SMTO_ABORTIFHUNG, 1000, None);
    }
}

// ---- radios ------------------------------------------------------------------------------------

struct Radios {
    list: Vec<(Radio, i64)>,
}

impl Drop for Radios {
    fn drop(&mut self) {
        for (r, t) in &self.list {
            let _ = r.RemoveStateChanged(*t);
        }
    }
}

fn load_radios() -> Radios {
    let mut list = Vec::new();
    if let Ok(v) = Radio::GetRadiosAsync().and_then(|op| op.join()) {
        for i in 0..v.Size().unwrap_or(0) {
            let Ok(r) = v.GetAt(i) else { continue };
            let tok = r
                .StateChanged(&TypedEventHandler::<Radio, windows::core::IInspectable>::new(|_, _| {
                    INBOX.push(Cmd::RadiosChanged);
                    Ok(())
                }))
                .unwrap_or(0);
            list.push((r, tok));
        }
    }
    Radios { list }
}

fn radio_kind(k: Kind) -> RadioKind {
    match k {
        Kind::Wifi => RadioKind::WiFi,
        Kind::Bluetooth => RadioKind::Bluetooth,
    }
}

fn read_radios(r: &Radios) -> Report {
    let state = |k: RadioKind| -> Option<bool> {
        let mut found = None;
        for (radio, _) in &r.list {
            if radio.Kind().ok() == Some(k) {
                let on = radio.State().ok() == Some(RadioState::On);
                found = Some(found.unwrap_or(false) || on);
            }
        }
        found
    };
    Report::Radios { wifi: state(RadioKind::WiFi), bt: state(RadioKind::Bluetooth) }
}

fn set_radio(r: &Radios, k: Kind, on: bool) {
    let allowed = Radio::RequestAccessAsync().and_then(|op| op.join());
    if allowed != Ok(RadioAccessStatus::Allowed) {
        log_warn!("control: radio access not allowed ({allowed:?})");
        return;
    }
    let want = if on { RadioState::On } else { RadioState::Off };
    for (radio, _) in r.list.iter().filter(|(x, _)| x.Kind().ok() == Some(radio_kind(k))) {
        match radio.SetStateAsync(want).and_then(|op| op.join()) {
            Ok(RadioAccessStatus::Allowed) => {}
            other => log_warn!("control: radio switch refused ({other:?})"),
        }
    }
}

// ---- brightness (WMI) --------------------------------------------------------------------------

const FLAGS: WBEM_GENERIC_FLAG_TYPE = WBEM_GENERIC_FLAG_TYPE(WBEM_FLAG_FORWARD_ONLY.0 | WBEM_FLAG_RETURN_IMMEDIATELY.0);

fn wmi_connect() -> windows::core::Result<IWbemServices> {
    unsafe {
        let loc: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
        let svc = loc.ConnectServer(&BSTR::from("ROOT\\WMI"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)?;
        // RPC_C_AUTHN_WINNT = 10, RPC_C_AUTHZ_NONE = 0.
        CoSetProxyBlanket(&svc, 10, 0, PCWSTR::null(), RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE)?;
        Ok(svc)
    }
}

fn first(svc: &IWbemServices, query: &str) -> Option<IWbemClassObject> {
    unsafe {
        let en = svc.ExecQuery(&BSTR::from("WQL"), &BSTR::from(query), FLAGS, None).ok()?;
        let mut objs = [None];
        let mut n = 0u32;
        let _ = en.Next(3000, &mut objs, &mut n);
        if n == 0 { None } else { objs[0].take() }
    }
}

fn wmi_brightness(svc: &IWbemServices) -> Option<u8> {
    let obj = first(svc, "SELECT CurrentBrightness FROM WmiMonitorBrightness WHERE Active=TRUE")?;
    unsafe {
        let mut v = VARIANT::default();
        obj.Get(w!("CurrentBrightness"), 0, &mut v, None, None).ok()?;
        let b = VariantToUInt32(&v).ok();
        let _ = VariantClear(&mut v);
        b.map(|b| b.min(100) as u8)
    }
}

fn variant(vt: VARENUM, set: impl FnOnce(&mut windows::Win32::System::Variant::VARIANT_0_0_0)) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        inner.vt = vt;
        set(&mut inner.Anonymous);
    }
    v
}

fn wmi_set_brightness(svc: &IWbemServices, pct: u8) -> windows::core::Result<()> {
    let none = || windows::core::Error::empty();
    let obj = first(svc, "SELECT * FROM WmiMonitorBrightnessMethods WHERE Active=TRUE").ok_or_else(none)?;
    unsafe {
        let mut pv = VARIANT::default();
        obj.Get(w!("__PATH"), 0, &mut pv, None, None)?;
        let path: BSTR = (*pv.Anonymous.Anonymous.Anonymous.bstrVal).clone();
        let _ = VariantClear(&mut pv);
        let mut class = None;
        svc.GetObject(&BSTR::from("WmiMonitorBrightnessMethods"), WBEM_GENERIC_FLAG_TYPE(0), None, Some(&mut class), None)?;
        let class: IWbemClassObject = class.ok_or_else(none)?;
        let mut sig = None;
        class.GetMethod(w!("WmiSetBrightness"), 0, &mut sig, std::ptr::null_mut())?;
        let params = sig.ok_or_else(none)?.SpawnInstance(0)?;
        // WMI wants uint32 as VT_I4 and uint8 as VT_UI1.
        params.Put(w!("Timeout"), 0, &variant(VT_I4, |u| u.lVal = 0), 0)?;
        params.Put(w!("Brightness"), 0, &variant(VT_UI1, |u| u.bVal = pct.min(100)), 0)?;
        svc.ExecMethod(&path, &BSTR::from("WmiSetBrightness"), WBEM_GENERIC_FLAG_TYPE(0), None, &params, None, None)
    }
}

// ---- service -----------------------------------------------------------------------------------

pub fn run(ctx: ServiceCtx) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    serve(&ctx);
    unsafe { CoUninitialize() };
}

fn serve(ctx: &ServiceCtx) {
    INBOX.clear();
    let mut radios: Option<Radios> = None;
    // Some(None) = WMI tried and brightness not supported: don't ask again.
    let mut wmi: Option<Option<IWbemServices>> = None;
    while let Some(batch) = INBOX.wait(ctx) {
        let refresh = batch.contains(&Cmd::Refresh);
        // Released unless a refresh / command in the same batch needs them again.
        if batch.last() == Some(&Cmd::Release) {
            radios = None;
            // Keep "WMI has no brightness here" (costs nothing); drop a live connection.
            if matches!(wmi, Some(Some(_))) {
                wmi = None;
            }
            continue;
        }
        if let Some(Cmd::SetDark(d)) = batch.iter().rev().find(|c| matches!(c, Cmd::SetDark(_))) {
            write_dark(*d);
            post(Report::Dark(read_dark()));
        }
        if radios.is_none() && (refresh || batch.iter().any(|c| matches!(c, Cmd::SetRadio(..)))) {
            radios = Some(load_radios());
        }
        if let Some(r) = &radios {
            for c in &batch {
                if let Cmd::SetRadio(k, on) = c {
                    set_radio(r, *k, *on);
                }
            }
            if refresh || batch.iter().any(|c| matches!(c, Cmd::RadiosChanged | Cmd::SetRadio(..))) {
                post(read_radios(r));
            }
        }
        let set_b = batch.iter().rev().find_map(|c| if let Cmd::SetBrightness(b) = c { Some(*b) } else { None });
        if refresh || set_b.is_some() {
            let svc = wmi.get_or_insert_with(|| {
                let s = wmi_connect().ok();
                // Only keep WMI around if this machine has a brightness-capable panel.
                s.filter(|s| wmi_brightness(s).is_some())
            });
            match svc {
                Some(s) => {
                    if let Some(b) = set_b {
                        if let Err(e) = wmi_set_brightness(s, b) {
                            log_warn!("control: brightness change failed: {e}");
                        }
                    }
                    // While dragging, the slider shows its own value; report only on refresh.
                    if refresh || set_b.is_some() {
                        post(Report::Brightness(wmi_brightness(s)));
                    }
                }
                None => post(Report::Brightness(None)),
            }
        }
    }
}
