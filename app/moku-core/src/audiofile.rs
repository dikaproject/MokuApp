//! Is this file something the adzan can play? Shared by the island (falls back to the chime when
//! the answer is no) and the settings app (tells the user why).
//!
//! The check is cheap and local: the file exists and is a regular file, has a known extension, is
//! not empty and not huge, and its first bytes look like that format. The operating system's
//! decoder is still the last judge (the island also falls back when playback fails); this only
//! keeps obviously wrong files (a renamed text file, a folder, a missing file) out.

use std::io::Read;
use std::path::Path;

/// Largest file accepted (an adzan recording is a few MB).
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Smallest plausible audio file.
pub const MIN_BYTES: u64 = 64;
/// Extensions the Windows media player decodes out of the box.
pub const EXTENSIONS: [&str; 6] = ["wav", "mp3", "m4a", "aac", "wma", "flac"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// No path was given.
    NoPath,
    Missing,
    /// A folder, device or other non-file.
    NotAFile,
    /// Not one of [`EXTENSIONS`].
    Extension,
    Empty,
    TooLarge,
    Unreadable,
    /// The extension says audio but the content does not look like it.
    NotAudio,
}

impl Problem {
    /// Indonesian text for the settings page.
    pub fn text(self) -> &'static str {
        match self {
            Problem::NoPath => "Belum ada file dipilih. Dipakai lonceng bawaan.",
            Problem::Missing => "File tidak ditemukan. Dipakai lonceng bawaan.",
            Problem::NotAFile => "Itu bukan file. Dipakai lonceng bawaan.",
            Problem::Extension => "Jenis file tidak didukung (pakai wav, mp3, m4a, aac, wma, atau flac). Dipakai lonceng bawaan.",
            Problem::Empty => "File kosong. Dipakai lonceng bawaan.",
            Problem::TooLarge => "File terlalu besar (maksimal 50 MB). Dipakai lonceng bawaan.",
            Problem::Unreadable => "File tidak bisa dibaca. Dipakai lonceng bawaan.",
            Problem::NotAudio => "Isi file tidak terlihat seperti audio. Dipakai lonceng bawaan.",
        }
    }
}

/// Check `path` against [`MAX_BYTES`].
pub fn check(path: &Path) -> Result<(), Problem> {
    check_with(path, MAX_BYTES)
}

/// Check `path` with an explicit size limit (the tests use a small one).
pub fn check_with(path: &Path, max_bytes: u64) -> Result<(), Problem> {
    if path.as_os_str().is_empty() {
        return Err(Problem::NoPath);
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    // `metadata` follows links, so a link to a folder is a folder.
    let meta = std::fs::metadata(path).map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { Problem::Missing } else { Problem::Unreadable })?;
    if !meta.is_file() {
        return Err(Problem::NotAFile);
    }
    if !EXTENSIONS.contains(&ext.as_str()) {
        return Err(Problem::Extension);
    }
    if meta.len() == 0 {
        return Err(Problem::Empty);
    }
    if meta.len() > max_bytes {
        return Err(Problem::TooLarge);
    }
    let mut head = vec![0u8; 8192.min(meta.len() as usize)];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).map_err(|_| Problem::Unreadable)?;
    head.truncate(n);
    if meta.len() < MIN_BYTES || !looks_like(&ext, &head) {
        return Err(Problem::NotAudio);
    }
    Ok(())
}

/// Do these first bytes look like a file of type `ext` (lower case)?
pub fn looks_like(ext: &str, head: &[u8]) -> bool {
    let id3 = head.starts_with(b"ID3");
    match ext {
        "wav" => head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WAVE",
        "flac" => head.starts_with(b"fLaC") || id3,
        "m4a" => head.len() >= 12 && &head[4..8] == b"ftyp",
        // WMA: the ASF header GUID.
        "wma" => head.starts_with(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11]),
        // MP3 and ADTS AAC: an ID3 tag, or an MPEG audio frame sync (11 set bits) near the start.
        "mp3" | "aac" => id3 || frame_sync_in(head),
        _ => false,
    }
}

/// An MPEG / ADTS frame header: 0xFF then a byte whose top three bits are set, with a layer that is
/// not "reserved" and a version that is not "reserved" (these rule out most random bytes).
fn frame_sync_in(head: &[u8]) -> bool {
    head.windows(2).take(4096).any(|w| w[0] == 0xFF && w[1] & 0xE0 == 0xE0 && w[1] & 0x18 != 0x08 && w[1] & 0x06 != 0x00 || (w[0] == 0xFF && w[1] & 0xF6 == 0xF0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-audiofile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A minimal valid WAV: PCM 16-bit mono 8 kHz, 0.1 s of silence.
    fn wav() -> Vec<u8> {
        let data = vec![0u8; 1600];
        let mut v = b"RIFF".to_vec();
        v.extend((36 + data.len() as u32).to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend(16u32.to_le_bytes());
        v.extend([1, 0, 1, 0]);
        v.extend(8000u32.to_le_bytes());
        v.extend(16_000u32.to_le_bytes());
        v.extend([2, 0, 16, 0]);
        v.extend(b"data");
        v.extend((data.len() as u32).to_le_bytes());
        v.extend(data);
        v
    }

    #[test]
    fn a_real_wav_passes() {
        let d = dir("ok");
        let p = d.join("adzan.WAV");
        std::fs::write(&p, wav()).unwrap();
        assert_eq!(check(&p), Ok(()), "extension case does not matter");
    }

    #[test]
    fn missing_empty_and_non_files_are_refused() {
        let d = dir("bad");
        assert_eq!(check(Path::new("")), Err(Problem::NoPath));
        assert_eq!(check(&d.join("nope.mp3")), Err(Problem::Missing));
        assert_eq!(check(&d), Err(Problem::NotAFile));
        let folder = d.join("folder.mp3");
        std::fs::create_dir_all(&folder).unwrap();
        assert_eq!(check(&folder), Err(Problem::NotAFile), "a folder named like a song");
        let empty = d.join("empty.wav");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(check(&empty), Err(Problem::Empty));
    }

    #[test]
    fn wrong_extension_and_wrong_content_are_refused() {
        let d = dir("content");
        let txt = d.join("notes.txt");
        std::fs::write(&txt, wav()).unwrap();
        assert_eq!(check(&txt), Err(Problem::Extension), "audio bytes but not a playable extension");
        let fake = d.join("fake.mp3");
        std::fs::write(&fake, "this is just text, renamed to .mp3. ".repeat(10)).unwrap();
        assert_eq!(check(&fake), Err(Problem::NotAudio));
        // A WAV extension on MP3 bytes is refused too (the player would guess, we do not).
        let wrong = d.join("wrong.wav");
        let mut mp3 = b"ID3\x04\0\0\0\0\0\0".to_vec();
        mp3.extend(vec![0u8; 200]);
        std::fs::write(&wrong, &mp3).unwrap();
        assert_eq!(check(&wrong), Err(Problem::NotAudio));
        let tiny = d.join("tiny.wav");
        std::fs::write(&tiny, b"RIFFxxxxWAVE").unwrap();
        assert_eq!(check(&tiny), Err(Problem::NotAudio), "too short to be a real file");
    }

    #[test]
    fn size_limit() {
        let d = dir("size");
        let p = d.join("big.wav");
        std::fs::write(&p, wav()).unwrap();
        let len = std::fs::metadata(&p).unwrap().len();
        assert_eq!(check_with(&p, len), Ok(()));
        assert_eq!(check_with(&p, len - 1), Err(Problem::TooLarge));
    }

    #[test]
    fn sniffing_each_format() {
        assert!(looks_like("mp3", b"ID3\x03\0\0\0\0\0\0"));
        assert!(looks_like("mp3", &[0xFF, 0xFB, 0x90, 0x44, 0, 0]), "MPEG-1 layer 3 frame");
        assert!(looks_like("mp3", &[0, 0, 0, 0xFF, 0xFB, 0x90, 0x44]), "frame sync after some padding");
        assert!(!looks_like("mp3", b"hello world, plain text"));
        assert!(!looks_like("mp3", &[0xFF, 0xEB, 0x00, 0x00]), "0xEB: reserved MPEG version");
        assert!(looks_like("aac", &[0xFF, 0xF1, 0x50, 0x80]), "ADTS");
        assert!(looks_like("m4a", b"\0\0\0\x20ftypM4A \0\0\0\0"));
        assert!(!looks_like("m4a", b"RIFF....WAVE"));
        assert!(looks_like("flac", b"fLaC\0\0\0\x22"));
        assert!(looks_like("wma", &[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6]));
        assert!(!looks_like("ogg", b"OggS"), "not a supported extension");
    }

    #[test]
    fn every_problem_has_a_text() {
        for p in [Problem::NoPath, Problem::Missing, Problem::NotAFile, Problem::Extension, Problem::Empty, Problem::TooLarge, Problem::Unreadable, Problem::NotAudio] {
            assert!(p.text().contains("lonceng"), "{p:?}");
        }
    }
}
