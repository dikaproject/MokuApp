//! Secrets in Windows Credential Manager (v0.3, track M: the API key of "Tanya AI").
//!
//! A secret is a *generic* credential with the target name `Moku/<name>`, written with
//! `CredWriteW` and read with `CredReadW`. It never goes into `config.toml`, a log or an error
//! message. The value is stored as UTF-16 (what the Windows credential UI shows as a password),
//! persisted for this user on this PC (`CRED_PERSIST_LOCAL_MACHINE`: not roamed to a domain).
//!
//! Both the island (reads the key to call the provider) and the settings app (writes / deletes
//! it) use this module, so it lives in the core crate.

use windows::Win32::Security::Credentials::{
    CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
};
use windows::core::{HSTRING, PWSTR};

/// Every target name starts with this.
pub const PREFIX: &str = "Moku/";
/// `CRED_MAX_CREDENTIAL_BLOB_SIZE`: 5 * 512 bytes, so at most 1280 UTF-16 units.
pub const MAX_BYTES: usize = 5 * 512;
/// Longest secret in characters (UTF-16 units).
pub const MAX_UNITS: usize = MAX_BYTES / 2;
/// Windows' limit for a target name (`CRED_MAX_GENERIC_TARGET_NAME_LENGTH`).
const MAX_TARGET: usize = 32767;
const ERROR_NOT_FOUND: i32 = 0x80070490u32 as i32;

/// The target name for `name`: `Moku/<name>`.
pub fn target(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// The credential vault can be briefly busy (another program using it at the same moment): try a
/// call up to three times before giving up. "Not found" is an answer, not a failure.
fn retry<T>(mut f: impl FnMut() -> windows::core::Result<T>) -> windows::core::Result<T> {
    let mut last = f();
    for _ in 0..2 {
        match &last {
            Ok(_) => break,
            Err(e) if e.code().0 == ERROR_NOT_FOUND => break,
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(40));
                last = f();
            }
        }
    }
    last
}

fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 400 || name.chars().any(|c| c.is_control()) {
        return Err("invalid secret name".into());
    }
    if target(name).encode_utf16().count() > MAX_TARGET {
        return Err("secret name too long".into());
    }
    Ok(())
}

/// Store `secret` under `Moku/<name>` (replaces an existing one). An empty secret is refused: use
/// [`delete`].
pub fn write(name: &str, secret: &str) -> Result<(), String> {
    valid_name(name)?;
    if secret.is_empty() {
        return Err("empty secret".into());
    }
    let units: Vec<u16> = secret.encode_utf16().collect();
    if units.len() > MAX_UNITS {
        return Err(format!("secret too long (at most {MAX_UNITS} characters)"));
    }
    let mut blob: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    let mut tn: Vec<u16> = target(name).encode_utf16().collect();
    tn.push(0);
    let cred = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(tn.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };
    let r = retry(|| unsafe { CredWriteW(&cred, 0) });
    wipe(&mut blob);
    r.map_err(|e| format!("CredWriteW failed (0x{:08X})", e.code().0 as u32))
}

/// The secret stored under `Moku/<name>`; `Ok(None)` when there is none.
pub fn read(name: &str) -> Result<Option<String>, String> {
    valid_name(name)?;
    let tn = HSTRING::from(target(name));
    let mut p: *mut CREDENTIALW = std::ptr::null_mut();
    match retry(|| unsafe { CredReadW(&tn, CRED_TYPE_GENERIC, None, &mut p) }) {
        Ok(()) => {}
        Err(e) if e.code().0 == ERROR_NOT_FOUND => return Ok(None),
        Err(e) => return Err(format!("CredReadW failed (0x{:08X})", e.code().0 as u32)),
    }
    if p.is_null() {
        return Ok(None);
    }
    let text = unsafe {
        let c = &*p;
        let bytes: &[u8] = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 { &[] } else { std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize) };
        let units: Vec<u16> = bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        let s = String::from_utf16(&units);
        CredFree(p as *const _);
        s
    };
    match text {
        Ok(s) if s.is_empty() => Ok(None),
        Ok(s) => Ok(Some(s)),
        Err(_) => Err("the stored secret is not valid text".into()),
    }
}

/// Remove `Moku/<name>`. `Ok(true)` when something was removed, `Ok(false)` when there was none.
pub fn delete(name: &str) -> Result<bool, String> {
    valid_name(name)?;
    let tn = HSTRING::from(target(name));
    match retry(|| unsafe { CredDeleteW(&tn, CRED_TYPE_GENERIC, None) }) {
        Ok(()) => Ok(true),
        Err(e) if e.code().0 == ERROR_NOT_FOUND => Ok(false),
        Err(e) => Err(format!("CredDeleteW failed (0x{:08X})", e.code().0 as u32)),
    }
}

/// Is there a secret under `Moku/<name>`? (Reads it; the value is dropped at once.)
pub fn exists(name: &str) -> bool {
    matches!(read(name), Ok(Some(_)))
}

fn wipe(v: &mut [u8]) {
    for b in v.iter_mut() {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // The tests that write the real Windows Credential Manager are `#[ignore]`d: a CI runner or a
    // sandboxed account may have no usable vault, and a developer's own vault is not a test fixture.
    // Run them on purpose: `MOKU_TEST_SYSTEM=1 cargo test -p moku-core secrets -- --ignored`.
    /// Refuse to touch the real vault unless the switch is set (an `--ignored` run by accident).
    fn system_switch() {
        assert_eq!(std::env::var("MOKU_TEST_SYSTEM").as_deref(), Ok("1"), "set MOKU_TEST_SYSTEM=1 to run the tests that use the real Credential Manager");
    }

    /// A test target that is removed again when the guard drops, even when an assert fails.
    struct Guard(String);

    impl Guard {
        fn new(tag: &str) -> Guard {
            static N: AtomicU32 = AtomicU32::new(0);
            Guard(format!("test-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = delete(&self.0);
        }
    }

    #[test]
    #[ignore = "writes the real Windows Credential Manager (a test target, removed again): run with MOKU_TEST_SYSTEM=1 --ignored"]
    fn write_read_delete_roundtrip() {
        system_switch();
        let g = Guard::new("rt");
        assert_eq!(read(&g.0), Ok(None), "nothing there yet");
        assert!(!exists(&g.0));
        write(&g.0, "sk-test-0123456789").unwrap();
        assert_eq!(read(&g.0), Ok(Some("sk-test-0123456789".into())));
        assert!(exists(&g.0));
        // Overwrite keeps one credential with the new value.
        write(&g.0, "sk-test-NEW").unwrap();
        assert_eq!(read(&g.0), Ok(Some("sk-test-NEW".into())));
        assert_eq!(delete(&g.0), Ok(true));
        assert_eq!(read(&g.0), Ok(None));
        assert_eq!(delete(&g.0), Ok(false), "deleting what is not there is not an error");
    }

    #[test]
    fn the_target_has_the_moku_prefix() {
        assert_eq!(target("ai/example.com"), "Moku/ai/example.com");
        assert!(target("x").starts_with(PREFIX));
    }

    #[test]
    #[ignore = "writes the real Windows Credential Manager (a test target, removed again): run with MOKU_TEST_SYSTEM=1 --ignored"]
    fn unicode_secrets_round_trip() {
        system_switch();
        let g = Guard::new("uni");
        write(&g.0, "kunci-ünï-🔑-テスト").unwrap();
        assert_eq!(read(&g.0).unwrap().as_deref(), Some("kunci-ünï-🔑-テスト"));
    }

    #[test]
    fn bad_input_is_refused_without_touching_the_store() {
        // Every one of these is refused before the vault is asked anything.
        let name = "test-bad-input";
        assert!(write(name, "").is_err(), "empty secret");
        assert!(write(name, &"k".repeat(MAX_UNITS + 1)).is_err(), "too long");
        assert!(write("", "x").is_err(), "empty name");
        assert!(write("a\nb", "x").is_err(), "control character in the name");
    }

    #[test]
    #[ignore = "writes the real Windows Credential Manager (a test target, removed again): run with MOKU_TEST_SYSTEM=1 --ignored"]
    fn the_longest_allowed_secret_works() {
        system_switch();
        let g = Guard::new("long");
        assert_eq!(read(&g.0), Ok(None));
        write(&g.0, &"k".repeat(MAX_UNITS)).unwrap();
        assert_eq!(read(&g.0).unwrap().map(|s| s.len()), Some(MAX_UNITS));
    }

    #[test]
    fn errors_never_contain_the_secret() {
        let secret = "sk-very-secret-value";
        let e = write("", secret).unwrap_err();
        assert!(!e.contains(secret), "{e}");
        let e = write(&"x".repeat(500), secret).unwrap_err();
        assert!(!e.contains(secret), "{e}");
    }
}
