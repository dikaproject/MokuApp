//! The files embedded by `build.rs` (empty unless `MOKU_SETUP_PAYLOAD` was set at build time).

use windows::Win32::Storage::Compression::{CloseDecompressor, COMPRESS_ALGORITHM_LZMS, CreateDecompressor, DECOMPRESSOR_HANDLE, Decompress};

/// One file to install.
pub struct PayloadFile {
    /// Path inside the program folder, `\`-separated (`moku.exe`, `docs\claude-code-hook.md`).
    pub path: &'static str,
    /// Size after decompression.
    pub size: u64,
    /// `data` is LZMS-compressed (Windows Compression API, buffer mode).
    pub compressed: bool,
    pub data: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/payload.rs"));

/// Does this build carry the binaries?
pub fn present() -> bool {
    FILES.iter().any(|f| f.path.eq_ignore_ascii_case(moku_core::install::ISLAND_EXE))
}

impl PayloadFile {
    /// The file's bytes.
    pub fn bytes(&self) -> Result<Vec<u8>, String> {
        if !self.compressed {
            return Ok(self.data.to_vec());
        }
        decompress(self.data, self.size as usize)
    }
}

pub fn decompress(data: &[u8], size: usize) -> Result<Vec<u8>, String> {
    unsafe {
        let mut h = DECOMPRESSOR_HANDLE::default();
        CreateDecompressor(COMPRESS_ALGORITHM_LZMS, None, &mut h).map_err(|e| format!("CreateDecompressor: {e}"))?;
        let mut out = vec![0u8; size];
        let mut n = 0usize;
        let r = Decompress(h, Some(data.as_ptr() as _), data.len(), Some(out.as_mut_ptr() as _), out.len(), Some(&mut n));
        let _ = CloseDecompressor(h);
        r.map_err(|e| format!("Decompress: {e}"))?;
        if n != size {
            return Err(format!("payload is damaged ({n} of {size} bytes)"));
        }
        Ok(out)
    }
}

/// The same compression the build script uses (tests).
#[cfg(test)]
pub fn compress(data: &[u8]) -> Vec<u8> {
    use windows::Win32::Storage::Compression::{CloseCompressor, COMPRESSOR_HANDLE, Compress, CreateCompressor};
    unsafe {
        let mut h = COMPRESSOR_HANDLE::default();
        CreateCompressor(COMPRESS_ALGORITHM_LZMS, None, &mut h).unwrap();
        let mut out = vec![0u8; data.len() + data.len() / 8 + 64 * 1024];
        let mut n = 0usize;
        Compress(h, Some(data.as_ptr() as _), data.len(), Some(out.as_mut_ptr() as _), out.len(), &mut n).unwrap();
        let _ = CloseCompressor(h);
        out.truncate(n);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lzms_roundtrip_and_damage() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 97) as u8 ^ (i / 1000) as u8).collect();
        let packed = compress(&data);
        assert!(packed.len() < data.len() / 4, "compresses: {}", packed.len());
        assert_eq!(decompress(&packed, data.len()).unwrap(), data);
        let mut bad = packed.clone();
        let mid = bad.len() / 2;
        bad.truncate(mid);
        assert!(decompress(&bad, data.len()).is_err(), "truncated payload is an error");
    }

    #[test]
    fn stub_build_has_no_payload_unless_packaged() {
        // `cargo test` runs without MOKU_SETUP_PAYLOAD: nothing embedded, installs refused.
        if std::option_env!("MOKU_SETUP_PAYLOAD").is_none() {
            assert!(FILES.is_empty());
            assert!(!present());
        }
    }
}
