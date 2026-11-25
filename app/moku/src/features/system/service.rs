//! The `system` service thread: a message-only window that receives Raw Input for Caps Lock, and
//! the `INetworkListManagerEvents` sink. One thread, one message loop, zero CPU until an event.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Networking::NetworkListManager::{INetworkListManager, INetworkListManagerEvents, INetworkListManagerEvents_Vtbl, NLM_CONNECTIVITY, NLM_CONNECTIVITY_IPV4_INTERNET, NLM_CONNECTIVITY_IPV6_INTERNET, NetworkListManager};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, IConnectionPoint, IConnectionPointContainer};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::{GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEKEYBOARD, RegisterRawInputDevices};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_MESSAGE, KillTimer, RI_KEY_BREAK, RegisterClassW, SetTimer, UnregisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WM_TIMER, WNDCLASSW,
};
use windows::Win32::Foundation::E_NOINTERFACE;
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, w};

use crate::app::Ui;
use crate::services::ServiceCtx;

pub fn run(ctx: ServiceCtx) {
    let cfg = ctx.config();
    let _com = crate::features::meeting::ComGuard::init();
    {
        // Declared in this block so the COM objects are released before CoUninitialize.
        let _net = if cfg.system.network { NetWatch::start() } else { None };
        let _caps = if cfg.system.caps_lock { CapsWindow::create() } else { None };
        log_info!("system service: network watcher {}, caps lock watcher {}", on_off(cfg.system.network, _net.is_some()), on_off(cfg.system.caps_lock, _caps.is_some()));
        if _net.is_none() && _caps.is_none() {
            return;
        }
        ctx.run_message_loop();
    }
}

fn on_off(wanted: bool, started: bool) -> &'static str {
    match (wanted, started) {
        (false, _) => "off",
        (true, true) => "started",
        (true, false) => "FAILED",
    }
}

// ---- Caps Lock ----------------------------------------------------------------------------------

const CLASS: windows::core::PCWSTR = w!("MokuSystemInput");
const TIMER_CAPS: usize = 1;

thread_local! {
    /// The key is physically down (typematic repeats are not new presses).
    static CAPS_DOWN: Cell<bool> = const { Cell::new(false) };
}

struct CapsWindow(HWND);

impl CapsWindow {
    fn create() -> Option<CapsWindow> {
        unsafe {
            let hinst = HINSTANCE(GetModuleHandleW(None).ok()?.0);
            let wc = WNDCLASSW { lpfnWndProc: Some(caps_proc), hInstance: hinst, lpszClassName: CLASS, ..Default::default() };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst), None).ok()?;
            // Keyboard (usage page 1, usage 6), delivered even when we are not the foreground window.
            let dev = [RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd }];
            if let Err(e) = RegisterRawInputDevices(&dev, size_of::<RAWINPUTDEVICE>() as u32) {
                log_warn!("system: raw input registration failed: {e}");
                let _ = DestroyWindow(hwnd);
                let _ = UnregisterClassW(CLASS, Some(hinst));
                return None;
            }
            Some(CapsWindow(hwnd))
        }
    }
}

impl Drop for CapsWindow {
    fn drop(&mut self) {
        unsafe {
            let dev = [RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_REMOVE, hwndTarget: HWND::default() }];
            let _ = RegisterRawInputDevices(&dev, size_of::<RAWINPUTDEVICE>() as u32);
            let _ = DestroyWindow(self.0);
            if let Ok(m) = GetModuleHandleW(None) {
                let _ = UnregisterClassW(CLASS, Some(HINSTANCE(m.0)));
            }
        }
    }
}

extern "system" fn caps_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_INPUT => {
                on_raw_input(hwnd, lp);
                // Sink input must still reach DefWindowProc so the system can free it.
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            // The key's toggle state flips just after the raw input arrives: look 30 ms later.
            WM_TIMER if wp.0 == TIMER_CAPS => {
                let _ = KillTimer(Some(hwnd), TIMER_CAPS);
                let on = GetKeyState(VK_CAPITAL.0 as i32) & 1 != 0;
                Ui::post(move |app| super::caps_changed(app, on));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Raw keyboard input. Only the virtual-key code is looked at, and only to compare it with
/// `VK_CAPITAL`: no other key is stored, logged or forwarded.
unsafe fn on_raw_input(hwnd: HWND, lp: LPARAM) {
    unsafe {
        let handle = HRAWINPUT(lp.0 as *mut _);
        let header = size_of::<RAWINPUTHEADER>() as u32;
        let mut buf = [0u64; 32]; // 256 bytes, 8-aligned; a keyboard RAWINPUT is ~40
        let mut size = size_of_val(&buf) as u32;
        if GetRawInputData(handle, RID_INPUT, Some(buf.as_mut_ptr() as *mut _), &mut size, header) == u32::MAX {
            return;
        }
        let ri = &*(buf.as_ptr() as *const RAWINPUT);
        if ri.header.dwType != RIM_TYPEKEYBOARD.0 {
            return;
        }
        let kb = ri.data.keyboard;
        if kb.VKey != VK_CAPITAL.0 {
            return;
        }
        if u32::from(kb.Flags) & RI_KEY_BREAK != 0 {
            CAPS_DOWN.set(false);
        } else if !CAPS_DOWN.replace(true) {
            SetTimer(Some(hwnd), TIMER_CAPS, 30, None);
        }
    }
}

// ---- Internet -----------------------------------------------------------------------------------

/// The event sink, written by hand: the `#[implement]` macro needs `windows-core` as a direct
/// dependency of this crate, and the architecture rules say not to touch Cargo.toml for that.
/// Layout: the vtable pointer first (that is what COM sees), then a reference count.
#[repr(C)]
struct NetSink {
    vtbl: *const INetworkListManagerEvents_Vtbl,
    refs: AtomicU32,
}

static SINK_VTBL: INetworkListManagerEvents_Vtbl =
    INetworkListManagerEvents_Vtbl { base__: IUnknown_Vtbl { QueryInterface: sink_query_interface, AddRef: sink_add_ref, Release: sink_release }, ConnectivityChanged: sink_connectivity_changed };

unsafe extern "system" fn sink_query_interface(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    unsafe {
        if out.is_null() || iid.is_null() {
            return E_NOINTERFACE;
        }
        if *iid == IUnknown::IID || *iid == INetworkListManagerEvents::IID {
            *out = this;
            sink_add_ref(this);
            HRESULT(0)
        } else {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn sink_add_ref(this: *mut c_void) -> u32 {
    unsafe { (*(this as *const NetSink)).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn sink_release(this: *mut c_void) -> u32 {
    unsafe {
        let left = (*(this as *const NetSink)).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        if left == 0 {
            drop(Box::from_raw(this as *mut NetSink));
        }
        left
    }
}

/// Called by the network list manager on an RPC thread: only posts to the UI thread.
unsafe extern "system" fn sink_connectivity_changed(_this: *mut c_void, c: NLM_CONNECTIVITY) -> HRESULT {
    let connected = has_internet(c);
    Ui::post(move |app| super::net_changed(app, connected));
    HRESULT(0)
}

fn new_sink() -> IUnknown {
    let raw = Box::into_raw(Box::new(NetSink { vtbl: &SINK_VTBL, refs: AtomicU32::new(1) }));
    // SAFETY: `raw` starts with a valid vtable pointer and carries the one reference IUnknown now owns.
    unsafe { IUnknown::from_raw(raw as *mut c_void) }
}

fn has_internet(c: NLM_CONNECTIVITY) -> bool {
    c.0 & (NLM_CONNECTIVITY_IPV4_INTERNET.0 | NLM_CONNECTIVITY_IPV6_INTERNET.0) != 0
}

struct NetWatch {
    point: IConnectionPoint,
    cookie: u32,
}

impl NetWatch {
    fn start() -> Option<NetWatch> {
        unsafe {
            let nlm: INetworkListManager = CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL).ok()?;
            let baseline = nlm.GetConnectivity().map(has_internet).unwrap_or(true);
            Ui::post(move |app| super::net_changed(app, baseline));
            let container: IConnectionPointContainer = nlm.cast().ok()?;
            let point = container.FindConnectionPoint(&INetworkListManagerEvents::IID).ok()?;
            let sink = new_sink();
            let cookie = point.Advise(&sink).ok()?;
            Some(NetWatch { point, cookie })
        }
    }
}

impl Drop for NetWatch {
    fn drop(&mut self) {
        unsafe {
            let _ = self.point.Unadvise(self.cookie);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn net_sink_com_vtable_works() {
        let _com = crate::features::meeting::ComGuard::init();
        let sink = new_sink();
        let events: INetworkListManagerEvents = sink.cast().expect("QueryInterface for INetworkListManagerEvents");
        unsafe { events.ConnectivityChanged(NLM_CONNECTIVITY(0)).expect("callback") };
        assert!(sink.cast::<INetworkListManager>().is_err(), "refuses interfaces it does not implement");
        // The last release frees the object (a wrong count would crash here).
        drop(events);
        drop(sink);
    }

    #[test]
    fn net_watcher_registers_with_the_network_list_manager() {
        let _com = crate::features::meeting::ComGuard::init();
        let w = NetWatch::start();
        assert!(w.is_some(), "INetworkListManager connection point + Advise");
    }

    #[test]
    fn internet_flags() {
        assert!(has_internet(NLM_CONNECTIVITY_IPV4_INTERNET));
        assert!(has_internet(NLM_CONNECTIVITY(NLM_CONNECTIVITY_IPV6_INTERNET.0 | 1)));
        assert!(!has_internet(NLM_CONNECTIVITY(0)));
        assert!(!has_internet(NLM_CONNECTIVITY(16 | 2))); // local network only (IPv4 LOCALNETWORK | IPv4 NOTRAFFIC)
    }
}
