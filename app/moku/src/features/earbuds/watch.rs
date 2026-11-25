//! The `earbuds` service. A `DeviceWatcher` on the *connected* Bluetooth devices
//! (`BluetoothDevice::GetDeviceSelectorFromConnectionStatus(Connected)`, documented) wakes it
//! when a device connects or disconnects; it then lists the connected devices, keeps those with an
//! audio class of device and reads their battery level (`battery.rs`). The level arrives late
//! after a connect, so it is read again 2, 6 and 15 s later, and once a minute while a device stays
//! connected. With nothing connected it sleeps on its event: no polling, no CPU.
//!
//! (Paired devices that are not connected cannot be opened as `BluetoothDevice`: Windows answers
//! "The device is not ready for use". That is why only the connected ones are listed, and a device
//! that disappears from the list is a disconnect.)

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice};
use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationUpdate, DeviceWatcher};
use windows::Foundation::TypedEventHandler;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use super::battery;
use super::model::Device;
use crate::app::Ui;
use crate::features::media::inbox::Inbox;
use crate::services::ServiceCtx;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cmd {
    /// A device connected or disconnected, or a timer fired: list and read everything again.
    Refresh,
}

static INBOX: LazyLock<Inbox<Cmd>> = LazyLock::new(Inbox::new);

/// While a device is connected the level is re-read this often.
const POLL: Duration = Duration::from_secs(60);
/// After a connect the level shows up late: look again after these delays.
const AFTER_CONNECT: [Duration; 3] = [Duration::from_secs(2), Duration::from_secs(6), Duration::from_secs(15)];
/// Without a working watcher the connected devices are looked at this often instead.
const FALLBACK_POLL: Duration = Duration::from_secs(15);

pub fn run(ctx: ServiceCtx) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    serve(&ctx);
    unsafe { CoUninitialize() };
}

/// "Bluetooth#Bluetoothf4:7b:09:8b:09:60-b1:02:d2:5d:ab:98" (the id of a Bluetooth device as
/// Windows enumerates it: local radio, then the device) to the device's 48-bit address.
pub fn address_from_aep_id(id: &str) -> Option<u64> {
    let tail = id.rsplit('-').next()?;
    let hex: String = tail.chars().filter(|c| *c != ':').collect();
    if hex.len() != 12 {
        return None;
    }
    u64::from_str_radix(&hex, 16).ok()
}

/// The connected Bluetooth devices that are audio devices, with their level.
fn connected_audio() -> Vec<Device> {
    let mut out = vec![];
    let Ok(selector) = BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected) else { return out };
    let Ok(all) = DeviceInformation::FindAllAsyncAqsFilter(&selector).and_then(|op| op.join()) else {
        log_warn!("earbuds: cannot list connected Bluetooth devices");
        return out;
    };
    for info in all {
        let Ok(id) = info.Id() else { continue };
        // A device that is connected but not open-able right now is skipped; the next refresh retries.
        let Ok(dev) = BluetoothDevice::FromIdAsync(&id).and_then(|op| op.join()) else { continue };
        if !dev.ClassOfDevice().and_then(|c| c.RawValue()).is_ok_and(battery::is_audio_cod) {
            continue;
        }
        let Ok(addr) = dev.BluetoothAddress() else { continue };
        let name = dev.Name().map(|n| n.to_string()).unwrap_or_default();
        out.push(Device { id: format!("{:012X}", addr & 0xFFFF_FFFF_FFFF), name, connected: true, battery: battery::read(addr) });
    }
    out
}

/// A watcher on the connected devices: connecting makes a device match the filter ("added"),
/// disconnecting makes it stop matching ("removed").
fn connection_watcher() -> Option<(DeviceWatcher, i64, i64)> {
    let selector = BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected).ok()?;
    let watcher = DeviceInformation::CreateWatcherAqsFilter(&selector).ok()?;
    let added = watcher
        .Added(&TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(|_, _| {
            INBOX.push(Cmd::Refresh);
            Ok(())
        }))
        .ok()?;
    let removed = watcher
        .Removed(&TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(|_, _| {
            INBOX.push(Cmd::Refresh);
            Ok(())
        }))
        .ok()?;
    watcher.Start().ok()?;
    Some((watcher, added, removed))
}

fn post(devices: Vec<Device>) {
    Ui::post(move |app| super::on_snapshot(app, devices));
}

fn serve(ctx: &ServiceCtx) {
    INBOX.clear();
    let watcher = connection_watcher();
    log_info!("earbuds service: connection watcher {}", if watcher.is_some() { "started" } else { "FAILED (polling every 15 s)" });
    let mut last = connected_audio();
    post(last.clone());
    // Moments at which the level is read again after a connect.
    let mut retries: Vec<Instant> = vec![];
    let mut next_poll = poll_after(&last, watcher.is_some());
    loop {
        let timeout = retries.iter().copied().chain(next_poll).min().map(|d| d.saturating_duration_since(Instant::now()));
        let Some(batch) = INBOX.wait_timeout(ctx, timeout) else { break };
        let now = Instant::now();
        let timer_due = retries.iter().any(|t| *t <= now) || next_poll.is_some_and(|t| t <= now);
        if batch.is_empty() && !timer_due {
            continue; // woken early with nothing to do
        }
        retries.retain(|t| *t > now);
        let cur = connected_audio();
        // A device that just connected: look at its level again soon (it arrives late).
        if cur.iter().any(|d| !last.iter().any(|l| l.id == d.id)) {
            retries = AFTER_CONNECT.iter().map(|d| now + *d).collect();
        }
        next_poll = poll_after(&cur, watcher.is_some());
        if cur != last {
            post(cur.clone());
            last = cur;
        }
    }
    if let Some((w, added, removed)) = watcher {
        let _ = w.RemoveAdded(added);
        let _ = w.RemoveRemoved(removed);
        let _ = w.Stop();
    }
}

/// When to look again on a schedule: every minute while a device is connected (its level
/// changes), every 15 s when there is no watcher to say that something connected.
fn poll_after(devices: &[Device], watching: bool) -> Option<Instant> {
    match (devices.is_empty(), watching) {
        (false, _) => Some(Instant::now() + POLL),
        (true, false) => Some(Instant::now() + FALLBACK_POLL),
        (true, true) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_from_a_device_id() {
        assert_eq!(address_from_aep_id("Bluetooth#Bluetoothf4:7b:09:8b:09:60-b1:02:d2:5d:ab:98"), Some(0xB102D25DAB98));
        assert_eq!(address_from_aep_id("BluetoothLE#BluetoothLEf4:7b:09:8b:09:60-c9:11:64:22:9e:1f"), Some(0xC91164229E1F));
        assert_eq!(address_from_aep_id("Bluetooth#Bluetoothf4:7b:09:8b:09:60-b1:02"), None);
        assert_eq!(address_from_aep_id("nonsense"), None);
        assert_eq!(address_from_aep_id(""), None);
    }

    #[test]
    fn polling_only_when_it_is_needed() {
        let d = Device { id: "A".into(), name: "x".into(), connected: true, battery: None };
        assert!(poll_after(&[d], true).is_some(), "a connected device's level is re-read");
        assert!(poll_after(&[], true).is_none(), "nothing connected and a watcher: sleep on the event");
        assert!(poll_after(&[], false).is_some(), "no watcher: look every 15 s");
    }

    /// Live probe of this machine's Bluetooth state (read-only, changes nothing). Not run by
    /// default: `cargo test -p moku live_probe -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads the real Bluetooth state of this PC"]
    fn live_probe() {
        let _com = crate::features::meeting::ComGuard::init();
        let connected = connected_audio();
        println!("connected audio devices: {}", connected.len());
        for d in &connected {
            println!("  {} battery={:?}", d.name, d.battery);
        }
        // The paired devices (they cannot be opened while disconnected): what is in the store?
        let sel = BluetoothDevice::GetDeviceSelectorFromPairingState(true).unwrap();
        let all = DeviceInformation::FindAllAsyncAqsFilter(&sel).and_then(|op| op.join()).unwrap();
        println!("paired devices: {}", all.Size().unwrap_or(0));
        for info in all {
            let addr = address_from_aep_id(&info.Id().unwrap().to_string());
            println!("  {} addr_ok={} last_known_level_in_store={:?}", info.Name().unwrap(), addr.is_some(), addr.and_then(battery::read));
        }
        let t = std::time::Instant::now();
        let _ = battery::read(0xB102D25DAB98);
        println!("one battery::read takes {:?}", t.elapsed());
    }
}
