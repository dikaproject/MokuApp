//! Small HKCU registry helpers (the installer only ever writes under HKCU).

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_OPEN_CREATE_OPTIONS, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE, RegCloseKey,
    RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::HSTRING;

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn err(what: &str, key: &str, r: WIN32_ERROR) -> String {
    format!("{what} HKCU\\{key} (error {})", r.0)
}

fn create(key: &str) -> Result<Key, String> {
    let mut h = HKEY::default();
    let r = unsafe { RegCreateKeyExW(HKEY_CURRENT_USER, &HSTRING::from(key), None, None, REG_OPEN_CREATE_OPTIONS(0), KEY_SET_VALUE, None, &mut h, None) };
    if r.0 != 0 {
        return Err(err("cannot create", key, r));
    }
    Ok(Key(h))
}

fn open(key: &str, access: REG_SAM_FLAGS) -> Result<Option<Key>, String> {
    let mut h = HKEY::default();
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(key), None, access, &mut h) };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if r.0 != 0 {
        return Err(err("cannot open", key, r));
    }
    Ok(Some(Key(h)))
}

fn wide_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(std::iter::once(0)).flat_map(|c| c.to_le_bytes()).collect()
}

/// Values to write into one key.
pub enum Value<'a> {
    Str(&'a str),
    Dword(u32),
}

/// Create `HKCU\<key>` if needed and write `values` into it.
pub fn write(key: &str, values: &[(&str, Value)]) -> Result<(), String> {
    let k = create(key)?;
    for (name, v) in values {
        let r = match v {
            Value::Str(s) => unsafe { RegSetValueExW(k.0, &HSTRING::from(*name), None, REG_SZ, Some(&wide_bytes(s))) },
            Value::Dword(d) => unsafe { RegSetValueExW(k.0, &HSTRING::from(*name), None, REG_DWORD, Some(&d.to_le_bytes())) },
        };
        if r.0 != 0 {
            return Err(format!("cannot write {name} in HKCU\\{key} (error {})", r.0));
        }
    }
    Ok(())
}

/// A string value, if the key and value exist.
pub fn get_string(key: &str, name: &str) -> Option<String> {
    let k = open(key, KEY_QUERY_VALUE).ok()??;
    let name = HSTRING::from(name);
    let mut ty = REG_VALUE_TYPE(0);
    let mut len = 0u32;
    unsafe {
        if RegQueryValueExW(k.0, &name, None, Some(&mut ty), None, Some(&mut len)).0 != 0 || ty != REG_SZ {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        if RegQueryValueExW(k.0, &name, None, Some(&mut ty), Some(buf.as_mut_ptr()), Some(&mut len)).0 != 0 {
            return None;
        }
        let wide: Vec<u16> = buf[..len as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(String::from_utf16_lossy(&wide).trim_end_matches('\0').to_string())
    }
}

/// A DWORD value, if present (tests).
#[cfg(test)]
pub fn get_dword(key: &str, name: &str) -> Option<u32> {
    let k = open(key, KEY_QUERY_VALUE).ok()??;
    let mut ty = REG_VALUE_TYPE(0);
    let mut v = [0u8; 4];
    let mut len = 4u32;
    let r = unsafe { RegQueryValueExW(k.0, &HSTRING::from(name), None, Some(&mut ty), Some(v.as_mut_ptr()), Some(&mut len)) };
    (r.0 == 0 && ty == REG_DWORD).then(|| u32::from_le_bytes(v))
}

/// Does `HKCU\<key>` exist? (tests)
#[cfg(test)]
pub fn key_exists(key: &str) -> bool {
    matches!(open(key, KEY_QUERY_VALUE), Ok(Some(_)))
}

/// Delete one value; a missing key or value is fine.
pub fn delete_value(key: &str, name: &str) -> Result<(), String> {
    let Some(k) = open(key, KEY_SET_VALUE)? else { return Ok(()) };
    let r = unsafe { RegDeleteValueW(k.0, &HSTRING::from(name)) };
    if r.0 != 0 && r != ERROR_FILE_NOT_FOUND {
        return Err(format!("cannot delete {name} in HKCU\\{key} (error {})", r.0));
    }
    Ok(())
}

/// Delete `HKCU\<key>` with everything under it; a missing key is fine.
pub fn delete_tree(key: &str) -> Result<(), String> {
    let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
    if r.0 != 0 && r != ERROR_FILE_NOT_FOUND {
        return Err(err("cannot delete", key, r));
    }
    Ok(())
}
