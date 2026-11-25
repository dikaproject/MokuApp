//! Reading the battery level of a Bluetooth audio device.
//!
//! What was researched (2026-11-24), and what this module does:
//!
//! 1. **Classic (BR/EDR) headsets and earbuds**: Windows has no documented API for them. A UWP
//!    app cannot read it (Microsoft Q&A, "Is there any way can get the bluetooth battery level
//!    information?": "UWP does not provide such APIs", https://learn.microsoft.com/en-us/answers/a/597419),
//!    and the GATT Battery Service only exists on LE links. What Windows itself does (its Settings
//!    page shows the level) is to take the level from the Hands-Free Profile (battery indicator and
//!    the Apple / XEVENT AT extensions) and store it as a **device-node property** on the
//!    "... Hands-Free AG" device node of the headset: key `{104EA319-6EE2-4701-BD47-8DDBF425BBE5}`
//!    pid 2, type `DEVPROP_TYPE_BYTE`, 0-100 percent (pid 7 next to it is a FILETIME: when it was
//!    updated). The call that reads it is documented (`CM_Get_DevNode_PropertyW`,
//!    https://learn.microsoft.com/en-us/windows/win32/api/cfgmgr32/nf-cfgmgr32-cm_get_devnode_propertyw);
//!    the key itself is **not** in the public SDK headers and not documented by Microsoft, only by the
//!    community (e.g. https://github.com/nikvoronin/xm4battery reads exactly this key from the
//!    headset's "Hands-Free AG" entity). Checked on the development machine: the HFP node of a paired
//!    headset carries Byte 60 under this key (kept as the last known level while it is disconnected,
//!    so a level is only ever shown for a *connected* device).
//! 2. **LE devices** have the documented GATT Battery Service (0x180F, Battery Level 0x2A19,
//!    https://www.bluetooth.com/specifications/specs/battery-service/), exposed to apps as
//!    `GattServiceUuids::Battery` / `GattCharacteristicUuids::BatteryLevel`
//!    (https://learn.microsoft.com/en-us/uwp/api/windows.devices.bluetooth.genericattributeprofile.gattserviceuuids.battery).
//!    It is not used here: only classic / dual-mode audio devices (class of device "Audio/Video")
//!    are tracked, and for those the property above is where Windows keeps the level.
//!
//! The device node is found through the container: the "Hands-Free AG" node has no address in
//! its instance id, but shares the `DEVPKEY_Device_ContainerId` of the `BTHENUM\DEV_<address>`
//! node of the device. A device that does not report a level simply has no such property, and
//! the caller shows no percentage.

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GETIDLIST_FILTER_ENUMERATOR, CM_Get_DevNode_PropertyW, CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW, CM_LOCATE_DEVNODE_PHANTOM, CM_Locate_DevNodeW, CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows::Win32::Devices::Properties::{DEVPKEY_Device_ContainerId, DEVPROPTYPE};
use windows::Win32::Foundation::DEVPROPKEY;
use windows::core::{GUID, PCWSTR};

/// `{104EA319-6EE2-4701-BD47-8DDBF425BBE5}` pid 2: battery level, percent.
pub const BATTERY_LEVEL: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5), pid: 2 };
/// pid 7 next to it: when the level was last updated (FILETIME).
pub const BATTERY_UPDATED: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5), pid: 7 };

const DEVPROP_TYPE_BYTE: u32 = 0x03;
const DEVPROP_TYPE_UINT32: u32 = 0x07;
const DEVPROP_TYPE_GUID: u32 = 0x0D;

/// The Bluetooth device-node enumerators (classic, LE device, LE service nodes).
const ENUMERATORS: [&str; 3] = ["BTHENUM", "BTHLE", "BTHLEDEVICE"];

/// Interpret the bytes of the battery property: a percentage 0-100 as `DEVPROP_TYPE_BYTE`
/// (what Windows writes; a `UINT32` is accepted too). Anything else, and 255 / values above 100
/// ("unknown" in the Bluetooth specs), is no reading.
pub fn parse_battery(prop_type: u32, data: &[u8]) -> Option<u8> {
    let v = match (prop_type, data) {
        (DEVPROP_TYPE_BYTE, [b, ..]) => u32::from(*b),
        (DEVPROP_TYPE_UINT32, [a, b, c, d, ..]) => u32::from_le_bytes([*a, *b, *c, *d]),
        _ => return None,
    };
    (v <= 100).then_some(v as u8)
}

/// Does this device-node instance id belong to the device with Bluetooth address `addr`?
/// Classic nodes are `BTHENUM\DEV_B102D25DAB98\...`, LE nodes `BTHLE\DEV_c91164229e1f\...`.
pub fn instance_matches_address(instance_id: &str, addr: u64) -> bool {
    let token = format!("\\DEV_{:012X}\\", addr & 0xFFFF_FFFF_FFFF);
    instance_id.to_ascii_uppercase().contains(&token)
}

/// Class of device (CoD) of an audio device: major class 4 "Audio/Video" with a minor class that
/// is something you wear or listen with (wearable headset, hands-free, loudspeaker, headphones,
/// portable audio). Cars, set-top boxes, video displays and the like are left out.
pub fn is_audio_cod(cod: u32) -> bool {
    let major = (cod >> 8) & 0x1F;
    let minor = (cod >> 2) & 0x3F;
    major == 4 && matches!(minor, 1 | 2 | 5 | 6 | 7)
}

fn guid_from_bytes(b: &[u8]) -> Option<GUID> {
    let b: &[u8; 16] = b.try_into().ok()?;
    Some(GUID { data1: u32::from_le_bytes([b[0], b[1], b[2], b[3]]), data2: u16::from_le_bytes([b[4], b[5]]), data3: u16::from_le_bytes([b[6], b[7]]), data4: [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]] })
}

/// A device node by instance id (present or not: the level of a connected device lives on a
/// present node, but a phantom one is still found).
fn locate(id: &str) -> Option<u32> {
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    let mut inst = 0u32;
    // SAFETY: `wide` is NUL-terminated and outlives the call; `inst` is a valid out pointer.
    (unsafe { CM_Locate_DevNodeW(&mut inst, PCWSTR(wide.as_ptr()), CM_LOCATE_DEVNODE_PHANTOM) } == CR_SUCCESS).then_some(inst)
}

/// A device-node property: `(type, bytes)`.
fn property(inst: u32, key: &DEVPROPKEY) -> Option<(u32, Vec<u8>)> {
    let (mut ty, mut size) = (DEVPROPTYPE(0), 0u32);
    // SAFETY: the size query passes no buffer; the second call passes `size` bytes.
    unsafe {
        let r = CM_Get_DevNode_PropertyW(inst, key, &mut ty, None, &mut size, 0);
        if r != CR_BUFFER_SMALL || size == 0 || size > 4096 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        (CM_Get_DevNode_PropertyW(inst, key, &mut ty, Some(buf.as_mut_ptr()), &mut size, 0) == CR_SUCCESS).then(|| {
            buf.truncate(size as usize);
            (ty.0, buf)
        })
    }
}

fn container(inst: u32) -> Option<GUID> {
    match property(inst, &DEVPKEY_Device_ContainerId)? {
        (DEVPROP_TYPE_GUID, b) => guid_from_bytes(&b),
        _ => None,
    }
}

/// Instance ids of every device node of one enumerator (all, not only present ones).
fn instance_ids(enumerator: &str) -> Vec<String> {
    let filter: Vec<u16> = enumerator.encode_utf16().chain(std::iter::once(0)).collect();
    let mut len = 0u32;
    // SAFETY: `filter` is NUL-terminated; the list is read into a buffer of the size asked for.
    unsafe {
        if CM_Get_Device_ID_List_SizeW(&mut len, PCWSTR(filter.as_ptr()), CM_GETIDLIST_FILTER_ENUMERATOR) != CR_SUCCESS || len < 2 {
            return vec![];
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_ID_ListW(PCWSTR(filter.as_ptr()), &mut buf, CM_GETIDLIST_FILTER_ENUMERATOR) != CR_SUCCESS {
            return vec![];
        }
        buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

/// The battery level (percent) Windows knows for the Bluetooth device with address `addr`, or
/// `None` when the device does not report one. Costs a few dozen property reads: call it when
/// something changed or every minute, not in a loop.
pub fn read(addr: u64) -> Option<u8> {
    let ids: Vec<String> = ENUMERATORS.iter().flat_map(|e| instance_ids(e)).collect();
    let own = ids.iter().find(|id| instance_matches_address(id, addr)).and_then(|id| locate(id)).and_then(container)?;
    let mut best: Option<(u64, u8)> = None;
    for id in &ids {
        let Some(inst) = locate(id) else { continue };
        if container(inst) != Some(own) {
            continue;
        }
        let Some((ty, data)) = property(inst, &BATTERY_LEVEL) else { continue };
        let Some(level) = parse_battery(ty, &data) else { continue };
        // Several nodes of one device: the most recently updated reading wins.
        let updated = property(inst, &BATTERY_UPDATED).and_then(|(_, d)| <[u8; 8]>::try_from(d.get(..8)?).ok()).map_or(0, u64::from_le_bytes);
        if best.is_none_or(|(t, _)| updated >= t) {
            best = Some((updated, level));
        }
    }
    best.map(|(_, l)| l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_property_is_a_byte_percentage() {
        assert_eq!(parse_battery(DEVPROP_TYPE_BYTE, &[60]), Some(60));
        assert_eq!(parse_battery(DEVPROP_TYPE_BYTE, &[0]), Some(0));
        assert_eq!(parse_battery(DEVPROP_TYPE_BYTE, &[100]), Some(100));
        // 255 and anything over 100 mean "unknown", not a level.
        assert_eq!(parse_battery(DEVPROP_TYPE_BYTE, &[101]), None);
        assert_eq!(parse_battery(DEVPROP_TYPE_BYTE, &[255]), None);
        assert_eq!(parse_battery(DEVPROP_TYPE_BYTE, &[]), None);
    }

    #[test]
    fn battery_property_other_types() {
        // A 32-bit value is accepted when it fits.
        assert_eq!(parse_battery(DEVPROP_TYPE_UINT32, &[75, 0, 0, 0]), Some(75));
        assert_eq!(parse_battery(DEVPROP_TYPE_UINT32, &[200, 0, 0, 0]), None);
        assert_eq!(parse_battery(DEVPROP_TYPE_UINT32, &[1, 1, 0, 0]), None, "257 is not a percentage");
        assert_eq!(parse_battery(DEVPROP_TYPE_UINT32, &[75, 0]), None, "truncated");
        // Strings, GUIDs and anything else are not readings.
        assert_eq!(parse_battery(0x12, b"5\0"), None);
        assert_eq!(parse_battery(DEVPROP_TYPE_GUID, &[0; 16]), None);
    }

    #[test]
    fn instance_ids_match_the_device_address() {
        let classic = r"BTHENUM\DEV_B102D25DAB98\7&AF82EBE&0&BLUETOOTHDEVICE_B102D25DAB98";
        let le = r"BTHLE\DEV_c91164229e1f\7&6F195F&0&C91164229E1F";
        assert!(instance_matches_address(classic, 0xB102D25DAB98));
        assert!(instance_matches_address(le, 0xC91164229E1F), "case does not matter");
        assert!(!instance_matches_address(classic, 0x251030B6779E));
        // The address has to be a whole path part, not a substring of another id.
        assert!(!instance_matches_address(r"BTHENUM\DEV_B102D25DAB98FF\1", 0xB102D25DAB98));
        // A node without the address in its id (the HFP node) is found through the container instead.
        assert!(!instance_matches_address(r"BTHENUM\{0000111e-0000-1000-8000-00805f9b34fb}_HCIBYPASS_LOCALMFG&0002\7&AF82EBE&0&0", 0xB102D25DAB98));
    }

    #[test]
    fn audio_class_of_device() {
        assert!(is_audio_cod(0x240404), "wearable headset (the dev machine's headset)");
        assert!(is_audio_cod(0x240408), "hands-free");
        assert!(is_audio_cod(0x240418), "headphones");
        assert!(is_audio_cod(0x240414), "loudspeaker");
        assert!(is_audio_cod(0x24041C), "portable audio");
        assert!(!is_audio_cod(0x240420), "car audio");
        assert!(!is_audio_cod(0x5A020C), "phone");
        assert!(!is_audio_cod(0x002540), "keyboard");
        assert!(!is_audio_cod(0), "unknown");
    }

    #[test]
    fn guid_bytes_are_little_endian_fields() {
        let b = [0x70, 0x59, 0x5E, 0x22, 0xB9, 0x56, 0x23, 0x56, 0x89, 0x08, 0xBD, 0x95, 0x6B, 0x81, 0xE7, 0xBB];
        assert_eq!(guid_from_bytes(&b), Some(GUID::from_u128(0x225E5970_56B9_5623_8908_BD956B81E7BB)));
        assert_eq!(guid_from_bytes(&b[..15]), None);
    }

    /// Reads the real device-node store of this machine (read-only): must never panic or hang,
    /// and an address nobody has gives no level.
    #[test]
    fn reading_the_real_store_is_safe() {
        assert_eq!(read(0x0000_DEAD_BEEF), None);
        let ids = instance_ids("BTHENUM");
        assert!(ids.iter().all(|i| i.to_ascii_uppercase().starts_with("BTHENUM\\")), "{ids:?}");
    }
}
