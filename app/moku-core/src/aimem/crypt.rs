//! The crypto of the AI memory file: AES-256-GCM with Windows CNG, and DPAPI (current user) to
//! wrap the key. No crates. Every failure is an `Err(String)` (never a panic: release builds
//! abort on panic), and a failed check never yields partial plaintext.

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::*;
use windows::core::w;

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;

/// Bytes that are wiped when dropped (keys, decrypted data).
pub struct Secret(pub Vec<u8>);

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

pub fn wipe(v: &mut [u8]) {
    for b in v.iter_mut() {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

fn status(st: windows::Win32::Foundation::NTSTATUS, what: &str) -> Result<(), String> {
    if st.is_err() { Err(format!("{what} failed (0x{:08X})", st.0 as u32)) } else { Ok(()) }
}

/// Fill `buf` from the system's random number generator.
pub fn random(buf: &mut [u8]) -> Result<(), String> {
    status(unsafe { BCryptGenRandom(None, buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }, "BCryptGenRandom")
}

/// An AES provider in GCM mode, closed when dropped.
struct Alg(BCRYPT_ALG_HANDLE);

impl Alg {
    fn gcm() -> Result<Alg, String> {
        let mut h = BCRYPT_ALG_HANDLE::default();
        status(unsafe { BCryptOpenAlgorithmProvider(&mut h, BCRYPT_AES_ALGORITHM, None, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0)) }, "BCryptOpenAlgorithmProvider")?;
        let alg = Alg(h);
        // "ChainingModeGCM" as UTF-16 with its terminating NUL.
        let mode: Vec<u8> = "ChainingModeGCM\0".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        status(unsafe { BCryptSetProperty(BCRYPT_HANDLE(alg.0.0), BCRYPT_CHAINING_MODE, &mode, 0) }, "BCryptSetProperty(GCM)")?;
        let _ = BCRYPT_CHAIN_MODE_GCM; // the constant documents the value above
        Ok(alg)
    }
}

impl Drop for Alg {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptCloseAlgorithmProvider(self.0, 0);
        }
    }
}

/// A symmetric key (and the memory CNG wants for it), destroyed when dropped.
struct SymKey {
    h: BCRYPT_KEY_HANDLE,
    // Must outlive the handle.
    _obj: Vec<u8>,
}

impl Drop for SymKey {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyKey(self.h);
        }
        wipe(&mut self._obj);
    }
}

fn make_key(alg: &Alg, key: &[u8; KEY_LEN]) -> Result<SymKey, String> {
    let mut len = 0u32;
    let mut got = 0u32;
    status(
        unsafe { BCryptGetProperty(BCRYPT_HANDLE(alg.0.0), BCRYPT_OBJECT_LENGTH, Some(std::slice::from_raw_parts_mut(&mut len as *mut u32 as *mut u8, 4)), &mut got, 0) },
        "BCryptGetProperty(ObjectLength)",
    )?;
    let mut obj = vec![0u8; len as usize];
    let mut h = BCRYPT_KEY_HANDLE::default();
    status(unsafe { BCryptGenerateSymmetricKey(alg.0, &mut h, Some(&mut obj), key, 0) }, "BCryptGenerateSymmetricKey")?;
    Ok(SymKey { h, _obj: obj })
}

/// Encrypt `plain`: returns the ciphertext (same length) and the 16-byte tag. `aad` is
/// authenticated but not encrypted.
pub fn seal(key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN], aad: &[u8], plain: &[u8]) -> Result<(Vec<u8>, [u8; TAG_LEN]), String> {
    let alg = Alg::gcm()?;
    let k = make_key(&alg, key)?;
    let mut nonce = *nonce;
    let mut aad = aad.to_vec();
    let mut tag = [0u8; TAG_LEN];
    let mut info = BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
        cbSize: size_of::<BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO>() as u32,
        dwInfoVersion: BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO_VERSION,
        pbNonce: nonce.as_mut_ptr(),
        cbNonce: NONCE_LEN as u32,
        pbAuthData: if aad.is_empty() { std::ptr::null_mut() } else { aad.as_mut_ptr() },
        cbAuthData: aad.len() as u32,
        pbTag: tag.as_mut_ptr(),
        cbTag: TAG_LEN as u32,
        ..Default::default()
    };
    let mut out = vec![0u8; plain.len()];
    let mut written = 0u32;
    status(
        unsafe { BCryptEncrypt(k.h, Some(plain), Some(&mut info as *mut _ as *const core::ffi::c_void), None, Some(&mut out), &mut written, BCRYPT_FLAGS(0)) },
        "BCryptEncrypt",
    )?;
    out.truncate(written as usize);
    Ok((out, tag))
}

/// Decrypt and verify. A wrong key, nonce, tag, AAD or any changed byte is an `Err`, and nothing
/// of the plaintext is returned.
pub fn open(key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN], aad: &[u8], cipher: &[u8], tag: &[u8; TAG_LEN]) -> Result<Secret, String> {
    let alg = Alg::gcm()?;
    let k = make_key(&alg, key)?;
    let mut nonce = *nonce;
    let mut aad = aad.to_vec();
    let mut tag = *tag;
    let mut info = BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
        cbSize: size_of::<BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO>() as u32,
        dwInfoVersion: BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO_VERSION,
        pbNonce: nonce.as_mut_ptr(),
        cbNonce: NONCE_LEN as u32,
        pbAuthData: if aad.is_empty() { std::ptr::null_mut() } else { aad.as_mut_ptr() },
        cbAuthData: aad.len() as u32,
        pbTag: tag.as_mut_ptr(),
        cbTag: TAG_LEN as u32,
        ..Default::default()
    };
    let mut out = Secret(vec![0u8; cipher.len()]);
    let mut written = 0u32;
    let st = unsafe { BCryptDecrypt(k.h, Some(cipher), Some(&mut info as *mut _ as *const core::ffi::c_void), None, Some(&mut out.0), &mut written, BCRYPT_FLAGS(0)) };
    if st.is_err() {
        // `out` is wiped by its Drop: a failed authentication never leaves plaintext behind.
        return Err(if st.0 as u32 == 0xC000A002 { "authentication failed (the data was changed or the key is wrong)".into() } else { format!("BCryptDecrypt failed (0x{:08X})", st.0 as u32) });
    }
    out.0.truncate(written as usize);
    Ok(out)
}

/// DPAPI: protect `data` for the current Windows user (`entropy` is mixed in: a different
/// entropy cannot unprotect it). No UI is ever shown.
pub fn dpapi_protect(data: &[u8], entropy: &[u8]) -> Result<Vec<u8>, String> {
    #[cfg(test)]
    if fake::on() {
        return Ok(fake::protect(data, entropy));
    }
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let ent = CRYPT_INTEGER_BLOB { cbData: entropy.len() as u32, pbData: entropy.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe { CryptProtectData(&input, w!("Moku"), Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.map_err(|e| format!("CryptProtectData failed (0x{:08X})", e.code().0 as u32))?;
    Ok(take_blob(out))
}

/// Undo [`dpapi_protect`]. Fails for another Windows user, another entropy or a damaged blob.
pub fn dpapi_unprotect(blob: &[u8], entropy: &[u8]) -> Result<Secret, String> {
    #[cfg(test)]
    if fake::on() {
        return fake::unprotect(blob, entropy).map(Secret);
    }
    let input = CRYPT_INTEGER_BLOB { cbData: blob.len() as u32, pbData: blob.as_ptr() as *mut u8 };
    let ent = CRYPT_INTEGER_BLOB { cbData: entropy.len() as u32, pbData: entropy.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe { CryptUnprotectData(&input, None, Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.map_err(|e| format!("CryptUnprotectData failed (0x{:08X})", e.code().0 as u32))?;
    Ok(Secret(take_blob(out)))
}

/// Copy a DPAPI output blob and free it (zeroed first: it may hold a key).
fn take_blob(b: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if b.pbData.is_null() {
        return vec![];
    }
    unsafe {
        let v = std::slice::from_raw_parts(b.pbData, b.cbData as usize).to_vec();
        std::ptr::write_bytes(b.pbData, 0, b.cbData as usize);
        let _ = LocalFree(Some(HLOCAL(b.pbData as *mut core::ffi::c_void)));
        v
    }
}

/// Unit tests of this crate do not call the real DPAPI (a CI account or a sandbox may not have a
/// user profile to wrap keys for): they use a stand-in with the same contract (the entropy is
/// part of the key, a damaged or foreign blob is refused). `MOKU_TEST_SYSTEM=1` switches the
/// stand-in off, and the `#[ignore]`d `dpapi_roundtrip...` test then runs the real one:
/// `MOKU_TEST_SYSTEM=1 cargo test -p moku-core dpapi -- --ignored`. Never compiled outside tests.
#[cfg(test)]
mod fake {
    pub fn on() -> bool {
        std::env::var("MOKU_TEST_SYSTEM").as_deref() != Ok("1")
    }

    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3))
    }

    const MAGIC: &[u8; 4] = b"FAKE";

    pub fn protect(data: &[u8], entropy: &[u8]) -> Vec<u8> {
        let k = fnv(entropy).to_le_bytes();
        let mut out = MAGIC.to_vec();
        out.extend(k);
        out.extend(data.iter().enumerate().map(|(i, b)| b ^ k[i % 8]));
        out.extend(fnv(data).to_le_bytes());
        out
    }

    pub fn unprotect(blob: &[u8], entropy: &[u8]) -> Result<Vec<u8>, String> {
        let k = fnv(entropy).to_le_bytes();
        if blob.len() < 20 || &blob[..4] != MAGIC || blob[4..12] != k {
            return Err("fake DPAPI: not for this scope".into());
        }
        let body = &blob[12..blob.len() - 8];
        let data: Vec<u8> = body.iter().enumerate().map(|(i, b)| b ^ k[i % 8]).collect();
        if blob[blob.len() - 8..] != fnv(&data).to_le_bytes() {
            return Err("fake DPAPI: damaged".into());
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; KEY_LEN] {
        let mut k = [0u8; KEY_LEN];
        random(&mut k).unwrap();
        k
    }

    /// NIST / McGrew-Viega GCM test case 16 (AES-256, 96-bit IV, with AAD): the CNG wrapper
    /// must produce the published ciphertext and tag.
    #[test]
    fn matches_the_published_gcm_vector() {
        fn hex(s: &str) -> Vec<u8> {
            (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
        }
        let k: [u8; 32] = hex("feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308").try_into().unwrap();
        let iv: [u8; 12] = hex("cafebabefacedbaddecaf888").try_into().unwrap();
        let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let p = hex(
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
        );
        let (c, tag) = seal(&k, &iv, &aad, &p).unwrap();
        assert_eq!(
            c,
            hex("522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662")
        );
        assert_eq!(tag.to_vec(), hex("76fc6ece0f4e1768cddf8853bb2d551b"));
        assert_eq!(open(&k, &iv, &aad, &c, &tag).unwrap().0, p);
    }

    #[test]
    fn roundtrip_and_every_change_is_detected() {
        let k = key();
        let mut nonce = [0u8; NONCE_LEN];
        random(&mut nonce).unwrap();
        let plain = b"{\"facts\":[\"nama saya Rani\"]}".to_vec();
        let (c, tag) = seal(&k, &nonce, b"header", &plain).unwrap();
        assert_ne!(c, plain, "encrypted");
        assert_eq!(open(&k, &nonce, b"header", &c, &tag).unwrap().0, plain);
        // Wrong key, wrong nonce, wrong AAD, changed ciphertext, changed tag: all refused.
        assert!(open(&key(), &nonce, b"header", &c, &tag).is_err());
        let mut n2 = nonce;
        n2[0] ^= 1;
        assert!(open(&k, &n2, b"header", &c, &tag).is_err());
        assert!(open(&k, &nonce, b"headeR", &c, &tag).is_err());
        let mut c2 = c.clone();
        c2[3] ^= 0x80;
        assert!(open(&k, &nonce, b"header", &c2, &tag).is_err());
        let mut t2 = tag;
        t2[15] ^= 1;
        assert!(open(&k, &nonce, b"header", &c, &t2).is_err());
        // Truncation and extension too.
        assert!(open(&k, &nonce, b"header", &c[..c.len() - 1], &tag).is_err());
        let mut c3 = c.clone();
        c3.push(0);
        assert!(open(&k, &nonce, b"header", &c3, &tag).is_err());
    }

    #[test]
    fn empty_plaintext_and_empty_aad_work() {
        let k = key();
        let nonce = [7u8; NONCE_LEN];
        let (c, tag) = seal(&k, &nonce, &[], &[]).unwrap();
        assert!(c.is_empty());
        assert!(open(&k, &nonce, &[], &c, &tag).unwrap().0.is_empty());
    }

    #[test]
    fn random_is_not_constant() {
        let (a, b) = (key(), key());
        assert_ne!(a, b);
        assert_ne!(a, [0u8; KEY_LEN]);
    }

    /// The stand-in keeps the contract the memory file relies on (so the file tests mean something).
    #[test]
    fn the_test_stand_in_for_dpapi_keeps_the_contract() {
        let data = key();
        let blob = fake::protect(&data, b"Moku.test.a");
        assert_ne!(blob, data.to_vec());
        assert_eq!(fake::unprotect(&blob, b"Moku.test.a").unwrap(), data.to_vec());
        assert!(fake::unprotect(&blob, b"Moku.test.b").is_err());
        let mut bad = blob.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0x55;
        assert!(fake::unprotect(&bad, b"Moku.test.a").is_err());
        assert!(fake::unprotect(&blob[..blob.len() / 2], b"Moku.test.a").is_err());
        assert!(fake::unprotect(&[], b"Moku.test.a").is_err());
    }

    #[test]
    #[ignore = "uses the real Windows DPAPI: run with MOKU_TEST_SYSTEM=1 --ignored"]
    fn dpapi_roundtrip_and_the_entropy_is_part_of_the_key() {
        assert_eq!(std::env::var("MOKU_TEST_SYSTEM").as_deref(), Ok("1"), "set MOKU_TEST_SYSTEM=1 to run the test that uses the real DPAPI");
        let data = key();
        let blob = dpapi_protect(&data, b"Moku.test.a").unwrap();
        assert_ne!(blob, data.to_vec());
        assert_eq!(dpapi_unprotect(&blob, b"Moku.test.a").unwrap().0, data.to_vec());
        // Another "scope" (a different entropy; a different Windows user behaves the same way:
        // DPAPI cannot unprotect it) is refused.
        assert!(dpapi_unprotect(&blob, b"Moku.test.b").is_err());
        // A damaged blob is refused too.
        let mut bad = blob.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0x55;
        assert!(dpapi_unprotect(&bad, b"Moku.test.a").is_err());
        assert!(dpapi_unprotect(&blob[..blob.len() / 2], b"Moku.test.a").is_err());
        assert!(dpapi_unprotect(&[], b"Moku.test.a").is_err());
    }
}
