//! "Start with Windows": a value named `Moku` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
//! whose data is the quoted path of `moku.exe` (the island, which sits next to this exe).
//!
//! The key path is a parameter so the tests (and screenshot runs) use a throw-away key such as
//! `HKCU\Software\MokuSettingsTest\Run`; the real key is only used when `MOKU_AUTOSTART_KEY` is not set.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPEN_CREATE_OPTIONS, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW,
    RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::HSTRING;

/// The real Run key (under HKCU).
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Value name Moku uses inside the Run key.
pub const VALUE_NAME: &str = "Moku";
/// Environment variable that redirects every autostart call to another key (tests, screenshots).
pub const KEY_ENV: &str = "MOKU_AUTOSTART_KEY";

/// The key all UI actions use: `$MOKU_AUTOSTART_KEY` if set, else the real Run key.
pub fn run_key() -> String {
    std::env::var(KEY_ENV).ok().filter(|k| !k.trim().is_empty()).unwrap_or_else(|| RUN_KEY.to_string())
}

/// `moku.exe` next to this executable.
pub fn moku_exe() -> PathBuf {
    std::env::current_exe().map(|p| p.with_file_name("moku.exe")).unwrap_or_else(|_| PathBuf::from("moku.exe"))
}

/// The registry data for `exe`: its path in quotes (paths with spaces must be quoted in Run).
pub fn command_for(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open(key: &str, access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<Option<Key>, String> {
    let mut h = HKEY::default();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(key), None, access, &mut h) };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if r.0 != 0 {
        return Err(format!("cannot open HKCU\\{key} (error {})", r.0));
    }
    Ok(Some(Key(h)))
}

/// The tests of this crate (all in one process, in parallel) share the parent key
/// `Software\MokuSettingsTest`: one test's cleanup of the parent must not land in the middle of
/// another test's `RegCreateKeyExW` (which then fails with "key was deleted"). Creating a key and
/// deleting the parent both hold this lock.
#[cfg(test)]
static TEST_KEY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Write `Moku = "<exe>"` under `HKCU\<key>` (the key is created if needed).
pub fn enable(key: &str, exe: &Path) -> Result<(), String> {
    #[cfg(test)]
    let _serialised = TEST_KEY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut h = HKEY::default();
    let r = unsafe {
        RegCreateKeyExW(HKEY_CURRENT_USER, &HSTRING::from(key), None, None, REG_OPEN_CREATE_OPTIONS(0), KEY_SET_VALUE, None, &mut h, None)
    };
    if r.0 != 0 {
        return Err(format!("cannot create HKCU\\{key} (error {})", r.0));
    }
    let k = Key(h);
    let data: Vec<u8> = command_for(exe).encode_utf16().chain(std::iter::once(0)).flat_map(|c| c.to_le_bytes()).collect();
    let r = unsafe { RegSetValueExW(k.0, &HSTRING::from(VALUE_NAME), None, REG_SZ, Some(&data)) };
    if r.0 != 0 {
        return Err(format!("cannot write {VALUE_NAME} (error {})", r.0));
    }
    Ok(())
}

/// Remove the `Moku` value. Nothing to remove is not an error.
pub fn disable(key: &str) -> Result<(), String> {
    let Some(k) = open(key, KEY_SET_VALUE)? else { return Ok(()) };
    let r = unsafe { RegDeleteValueW(k.0, &HSTRING::from(VALUE_NAME)) };
    if r.0 != 0 && r != ERROR_FILE_NOT_FOUND {
        return Err(format!("cannot delete {VALUE_NAME} (error {})", r.0));
    }
    Ok(())
}

/// The data of the `Moku` value, if present.
pub fn current(key: &str) -> Option<String> {
    let k = open(key, KEY_QUERY_VALUE).ok()??;
    let name = HSTRING::from(VALUE_NAME);
    let mut ty = REG_VALUE_TYPE(0);
    let mut len = 0u32;
    unsafe {
        if RegQueryValueExW(k.0, &name, None, Some(&mut ty), None, Some(&mut len)).0 != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        if RegQueryValueExW(k.0, &name, None, Some(&mut ty), Some(buf.as_mut_ptr()), Some(&mut len)).0 != 0 {
            return None;
        }
        let wide: Vec<u16> = buf[..len as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let s = String::from_utf16_lossy(&wide);
        Some(s.trim_end_matches('\0').to_string())
    }
}

pub fn is_enabled(key: &str) -> bool {
    current(key).is_some()
}

/// Delete a test key again (only keys under `Software\MokuSettingsTest`).
#[cfg(test)]
pub fn delete_test_key(key: &str) {
    assert!(key.starts_with(r"Software\MokuSettingsTest"), "refusing to delete {key}");
    let _serialised = TEST_KEY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(key));
        let _ = windows::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(r"Software\MokuSettingsTest"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(name: &str) -> String {
        format!(r"Software\MokuSettingsTest\Run-{name}-{}", std::process::id())
    }

    #[test]
    fn autostart_registry_write_and_remove() {
        let key = test_key("rw");
        assert_ne!(key, RUN_KEY);
        let exe = PathBuf::from(r"C:\Program Files\Moku\moku.exe");

        assert!(!is_enabled(&key), "fresh key has no value");
        enable(&key, &exe).unwrap();
        assert!(is_enabled(&key));
        assert_eq!(current(&key).as_deref(), Some(r#""C:\Program Files\Moku\moku.exe""#));

        // Writing again replaces the value.
        enable(&key, Path::new(r"D:\Moku\moku.exe")).unwrap();
        assert_eq!(current(&key).as_deref(), Some(r#""D:\Moku\moku.exe""#));

        disable(&key).unwrap();
        assert!(!is_enabled(&key));
        // Removing twice, and removing from a key that does not exist, are fine.
        disable(&key).unwrap();
        disable(&test_key("never-created")).unwrap();

        delete_test_key(&key);
    }

    #[test]
    fn autostart_quotes_paths_with_spaces() {
        assert_eq!(command_for(Path::new(r"C:\My Apps\moku.exe")), r#""C:\My Apps\moku.exe""#);
    }
}
