//! SHA-256 with Windows CNG (`BCryptCreateHash` on the built-in `BCRYPT_SHA256_ALG_HANDLE`), so
//! no hashing crate is needed. Streaming: feed the download chunk by chunk.

use std::io::Read;
use std::path::Path;

use windows::Win32::Security::Cryptography::{
    BCRYPT_HASH_HANDLE, BCRYPT_SHA256_ALG_HANDLE, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptHashData,
};

pub struct Sha256(BCRYPT_HASH_HANDLE);

impl Sha256 {
    pub fn new() -> Result<Sha256, String> {
        let mut h = BCRYPT_HASH_HANDLE::default();
        let st = unsafe { BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut h, None, None, 0) };
        if st.is_err() || h.0.is_null() {
            return Err(format!("BCryptCreateHash failed (0x{:08X})", st.0));
        }
        Ok(Sha256(h))
    }

    pub fn update(&mut self, data: &[u8]) -> Result<(), String> {
        // BCryptHashData takes a u32 length: feed large buffers in pieces.
        for chunk in data.chunks(1 << 30) {
            let st = unsafe { BCryptHashData(self.0, chunk, 0) };
            if st.is_err() {
                return Err(format!("BCryptHashData failed (0x{:08X})", st.0));
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<[u8; 32], String> {
        let mut out = [0u8; 32];
        let st = unsafe { BCryptFinishHash(self.0, &mut out, 0) };
        if st.is_err() {
            return Err(format!("BCryptFinishHash failed (0x{:08X})", st.0));
        }
        Ok(out)
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        unsafe {
            let _ = BCryptDestroyHash(self.0);
        }
    }
}

/// SHA-256 of a byte slice.
pub fn digest(data: &[u8]) -> Result<[u8; 32], String> {
    let mut h = Sha256::new()?;
    h.update(data)?;
    h.finish()
}

/// SHA-256 of everything `r` yields.
pub fn digest_reader(mut r: impl Read) -> Result<[u8; 32], String> {
    let mut h = Sha256::new()?;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = r.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n])?;
    }
    h.finish()
}

/// Does the file at `path` hash to `expected`? Any read error is a failure.
pub fn verify_file(path: &Path, expected: &[u8; 32]) -> Result<(), String> {
    let f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let got = digest_reader(std::io::BufReader::new(f))?;
    check(&got, expected)
}

/// Compare two digests; the error names both (hex) for the log.
pub fn check(got: &[u8; 32], expected: &[u8; 32]) -> Result<(), String> {
    if got == expected {
        Ok(())
    } else {
        Err(format!("SHA-256 mismatch: expected {}, got {}", moku_core::release::to_hex(expected), moku_core::release::to_hex(got)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moku_core::release::{from_hex, to_hex};

    #[test]
    fn known_vectors() {
        // FIPS 180-2 test vectors.
        assert_eq!(to_hex(&digest(b"").unwrap()), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(to_hex(&digest(b"abc").unwrap()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        // Streaming in odd pieces gives the same digest as one call.
        let data: Vec<u8> = (0..1_000_003u32).map(|i| (i * 31 % 251) as u8).collect();
        let mut h = Sha256::new().unwrap();
        for c in data.chunks(4097) {
            h.update(c).unwrap();
        }
        assert_eq!(h.finish().unwrap(), digest(&data).unwrap());
        assert_eq!(digest_reader(&data[..]).unwrap(), digest(&data).unwrap());
    }

    #[test]
    fn hash_verification_passes_and_fails() {
        let dir = std::env::temp_dir().join(format!("moku-update-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("moku-setup.exe");
        std::fs::write(&f, b"pretend installer").unwrap();
        let good = digest(b"pretend installer").unwrap();
        assert!(verify_file(&f, &good).is_ok());

        // One flipped byte in the file, or the wrong expected digest, fails.
        std::fs::write(&f, b"pretend installeR").unwrap();
        let e = verify_file(&f, &good).unwrap_err();
        assert!(e.contains("mismatch"), "{e}");
        let other = from_hex("0000000000000000000000000000000000000000000000000000000000000000").unwrap();
        assert!(check(&good, &other).is_err());
        // A missing file is a failure, not a pass.
        assert!(verify_file(&dir.join("missing.exe"), &good).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
