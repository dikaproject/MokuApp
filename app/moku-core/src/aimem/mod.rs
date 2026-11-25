//! The local memory of "Tanya AI" (v0.3 track M): facts the user approved ("Ingat ini") and an
//! optional chat history, in `ai-memory.bin` of the data folder.
//!
//! * **Encrypted at rest.** The file holds JSON encrypted with AES-256-GCM (Windows CNG). The key
//!   is random and used for ONE save; it is wrapped with DPAPI for the current Windows user
//!   (`crypt.rs`) and stored in the file's header, so the file opens only for that user on that
//!   PC and the key itself never exists on disk in the clear.
//! * **Fails closed.** A file that cannot be read (changed bytes, truncated, another Windows
//!   user, an unknown version) makes the memory *unavailable*: nothing is shown, nothing is sent
//!   to a provider, **nothing is written over it** (so no silent data loss) until the user
//!   chooses "Hapus semua" in Settings. No error ever panics.
//! * **Shared by two programs**: the island (adds facts, history, reads for prompts) and the
//!   settings app (list, edit, delete, export, delete all). Every change is a locked
//!   read-modify-write of the whole (small) file; neither program caches it.
//!
//! The memory file is the only place this feature keeps what the user typed or what an AI
//! answered. Nothing here logs content.

pub mod crypt;

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file in the data folder.
pub const FILE_NAME: &str = "ai-memory.bin";
/// Advisory lock next to it (held while a program rewrites the file).
pub const LOCK_NAME: &str = "ai-memory.lock";

const MAGIC: &[u8; 4] = b"MKAI";
const VERSION: u8 = 1;
/// Mixed into DPAPI: another entropy (another program) cannot unwrap the key.
const ENTROPY: &[u8] = b"Moku.AiMemory.v1";
/// Header: magic(4) version(1) reserved(1) wrapped_len(2).
const HEAD: usize = 8;
const MAX_WRAPPED: usize = 2048;
/// Biggest file read (a hostile or damaged file never makes us allocate more).
pub const MAX_FILE: u64 = 8 * 1024 * 1024;

pub const MAX_FACTS: usize = 200;
pub const MAX_FACT_CHARS: usize = 500;
pub const MAX_HISTORY: usize = 500;
/// Question / answer text kept per history entry.
pub const MAX_ENTRY_CHARS: usize = 4000;

/// The default location: `<data dir>\ai-memory.bin`.
pub fn default_path() -> PathBuf {
    crate::paths::data_file(FILE_NAME)
}

// ---- the data -------------------------------------------------------------------------------

/// One thing the user asked to be remembered ("Saya tinggal di Surabaya").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub id: u64,
    pub text: String,
    /// Unix seconds when it was saved.
    pub ts: i64,
}

/// One exchange of the optional chat history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: u64,
    pub ts: i64,
    /// `translate`, `summarize`, `explain`, `fix` or `ask`.
    pub action: String,
    pub q: String,
    pub a: String,
    /// The provider host it was sent to.
    pub host: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    #[serde(default = "one")]
    pub v: u32,
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub facts: Vec<Fact>,
    #[serde(default)]
    pub history: Vec<Entry>,
}

fn one() -> u32 {
    1
}

impl Default for Memory {
    fn default() -> Self {
        Memory { v: 1, next_id: 1, facts: vec![], history: vec![] }
    }
}

/// Why a fact was not saved (Indonesian, shown to the user as is).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactError {
    Empty,
    TooLong,
    Duplicate,
    Full,
    Missing,
}

impl FactError {
    pub fn message(self) -> &'static str {
        match self {
            FactError::Empty => "Tulis dulu apa yang perlu diingat",
            FactError::TooLong => "Terlalu panjang (paling banyak 500 huruf)",
            FactError::Duplicate => "Sudah ada di memori",
            FactError::Full => "Memori penuh (200 hal); hapus yang tidak perlu",
            FactError::Missing => "Catatan itu sudah tidak ada",
        }
    }
}

/// One line of text: control characters and runs of whitespace become single spaces.
pub fn clean(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

impl Memory {
    fn take_id(&mut self) -> u64 {
        let floor = self.facts.iter().map(|f| f.id).chain(self.history.iter().map(|e| e.id)).max().unwrap_or(0) + 1;
        let id = self.next_id.max(floor);
        self.next_id = id + 1;
        id
    }

    /// Save a fact. Returns its id.
    pub fn add_fact(&mut self, text: &str, now: i64) -> Result<u64, FactError> {
        let text = clean(text);
        if text.is_empty() {
            return Err(FactError::Empty);
        }
        if text.chars().count() > MAX_FACT_CHARS {
            return Err(FactError::TooLong);
        }
        if self.facts.iter().any(|f| f.text.to_lowercase() == text.to_lowercase()) {
            return Err(FactError::Duplicate);
        }
        if self.facts.len() >= MAX_FACTS {
            return Err(FactError::Full);
        }
        let id = self.take_id();
        self.facts.push(Fact { id, text, ts: now });
        Ok(id)
    }

    /// Change the text of fact `id` (keeps its date).
    pub fn edit_fact(&mut self, id: u64, text: &str) -> Result<(), FactError> {
        let text = clean(text);
        if text.is_empty() {
            return Err(FactError::Empty);
        }
        if text.chars().count() > MAX_FACT_CHARS {
            return Err(FactError::TooLong);
        }
        if self.facts.iter().any(|f| f.id != id && f.text.to_lowercase() == text.to_lowercase()) {
            return Err(FactError::Duplicate);
        }
        match self.facts.iter_mut().find(|f| f.id == id) {
            Some(f) => {
                f.text = text;
                Ok(())
            }
            None => Err(FactError::Missing),
        }
    }

    pub fn delete_fact(&mut self, id: u64) -> bool {
        let n = self.facts.len();
        self.facts.retain(|f| f.id != id);
        self.facts.len() != n
    }

    /// Append an exchange to the history (the oldest go when there are more than [`MAX_HISTORY`]).
    pub fn add_history(&mut self, action: &str, q: &str, a: &str, host: &str, now: i64) -> u64 {
        let id = self.take_id();
        let cut = |s: &str| s.chars().take(MAX_ENTRY_CHARS).collect::<String>();
        self.history.push(Entry { id, ts: now, action: action.chars().take(20).collect(), q: cut(q), a: cut(a), host: host.chars().take(120).collect() });
        let extra = self.history.len().saturating_sub(MAX_HISTORY);
        if extra > 0 {
            self.history.drain(..extra);
        }
        id
    }

    pub fn delete_history(&mut self, id: u64) -> bool {
        let n = self.history.len();
        self.history.retain(|e| e.id != id);
        self.history.len() != n
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    /// Retention sweep: drop history older than `days` days. Facts never expire. Returns how many
    /// entries went.
    pub fn sweep(&mut self, now: i64, days: u32) -> usize {
        let cutoff = now - days as i64 * 86_400;
        let n = self.history.len();
        self.history.retain(|e| e.ts >= cutoff);
        n - self.history.len()
    }

    /// The facts for a prompt, newest last, at most `max_facts` and `max_chars` characters in all
    /// (the newest are kept when it does not all fit).
    pub fn facts_for_prompt(&self, max_facts: usize, max_chars: usize) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        let mut used = 0usize;
        for f in self.facts.iter().rev().take(max_facts) {
            let n = f.text.chars().count();
            if used + n > max_chars {
                break;
            }
            used += n;
            out.push(f.text.clone());
        }
        out.reverse();
        out
    }

    /// The export file: plain JSON (it is the user's own copy, so it is NOT encrypted).
    pub fn export_json(&self, now: i64) -> String {
        #[derive(Serialize)]
        struct Out<'a> {
            app: &'static str,
            kind: &'static str,
            exported_at: i64,
            facts: &'a [Fact],
            history: &'a [Entry],
        }
        serde_json::to_string_pretty(&Out { app: "Moku", kind: "tanya-ai-memory", exported_at: now, facts: &self.facts, history: &self.history }).unwrap_or_default()
    }
}

// ---- the file -------------------------------------------------------------------------------

/// Encrypt `mem` into the file format (a fresh key, wrapped for the current user with `entropy`).
fn encode_with(mem: &Memory, entropy: &[u8]) -> Result<Vec<u8>, String> {
    let json = crypt::Secret(serde_json::to_vec(mem).map_err(|e| format!("cannot serialise: {e}"))?);
    let mut key = crypt::Secret(vec![0u8; crypt::KEY_LEN]);
    crypt::random(&mut key.0)?;
    let wrapped = crypt::dpapi_protect(&key.0, entropy)?;
    if wrapped.is_empty() || wrapped.len() > MAX_WRAPPED {
        return Err("unexpected size of the wrapped key".into());
    }
    let mut nonce = [0u8; crypt::NONCE_LEN];
    crypt::random(&mut nonce)?;

    let mut out = Vec::with_capacity(HEAD + wrapped.len() + crypt::NONCE_LEN + 4 + json.0.len() + crypt::TAG_LEN);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.push(0);
    out.extend_from_slice(&(wrapped.len() as u16).to_le_bytes());
    out.extend_from_slice(&wrapped);
    // Everything so far is authenticated (but readable): a changed header fails the tag.
    let aad = out.clone();
    let k: [u8; crypt::KEY_LEN] = key.0.as_slice().try_into().map_err(|_| "key size".to_string())?;
    let (cipher, tag) = crypt::seal(&k, &nonce, &aad, &json.0)?;
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&(cipher.len() as u32).to_le_bytes());
    out.extend_from_slice(&cipher);
    out.extend_from_slice(&tag);
    Ok(out)
}

/// Decrypt the file format. Any problem is an `Err` with a short reason (never content).
fn decode_with(bytes: &[u8], entropy: &[u8]) -> Result<Memory, String> {
    if bytes.len() < HEAD || &bytes[..4] != MAGIC {
        return Err("not a Moku memory file".into());
    }
    if bytes[4] != VERSION {
        return Err(format!("unsupported memory file version {}", bytes[4]));
    }
    let wl = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    if wl == 0 || wl > MAX_WRAPPED {
        return Err("damaged header".into());
    }
    let n_at = HEAD + wl;
    let c_at = n_at + crypt::NONCE_LEN + 4;
    if bytes.len() < c_at + crypt::TAG_LEN {
        return Err("the file is cut short".into());
    }
    let nonce: [u8; crypt::NONCE_LEN] = bytes[n_at..n_at + crypt::NONCE_LEN].try_into().map_err(|_| "damaged nonce".to_string())?;
    let cl = u32::from_le_bytes([bytes[c_at - 4], bytes[c_at - 3], bytes[c_at - 2], bytes[c_at - 1]]) as usize;
    if cl as u64 > MAX_FILE || bytes.len() != c_at + cl + crypt::TAG_LEN {
        return Err("the file is cut short or has extra data".into());
    }
    let cipher = &bytes[c_at..c_at + cl];
    let tag: [u8; crypt::TAG_LEN] = bytes[c_at + cl..].try_into().map_err(|_| "damaged tag".to_string())?;
    let key = crypt::dpapi_unprotect(&bytes[HEAD..n_at], entropy).map_err(|_| "this Windows account cannot unlock the file".to_string())?;
    let k: [u8; crypt::KEY_LEN] = key.0.as_slice().try_into().map_err(|_| "damaged key".to_string())?;
    let plain = crypt::open(&k, &nonce, &bytes[..n_at], cipher, &tag)?;
    let mem: Memory = serde_json::from_slice(&plain.0).map_err(|_| "the decrypted data is not valid".to_string())?;
    if mem.v != 1 {
        return Err(format!("unsupported memory version {}", mem.v));
    }
    Ok(mem)
}

/// What opening the file found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// There is no file (nothing was ever remembered).
    Missing,
    /// There is a file we cannot use. It is left untouched. The text is a short technical reason.
    Unavailable(String),
}

/// Read and decrypt the memory at `path`.
pub fn load(path: &Path) -> Result<Memory, LoadError> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LoadError::Missing),
        Err(e) => return Err(LoadError::Unavailable(format!("cannot read the file ({})", e.kind()))),
    };
    if meta.len() > MAX_FILE + 4096 {
        return Err(LoadError::Unavailable("the file is too large".into()));
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LoadError::Missing),
        Err(e) => return Err(LoadError::Unavailable(format!("cannot read the file ({})", e.kind()))),
    };
    decode_with(&bytes, ENTROPY).map_err(LoadError::Unavailable)
}

/// Like [`load`], but a missing file is an empty memory. An unreadable file is an `Err` and
/// is not touched.
pub fn load_or_new(path: &Path) -> Result<Memory, String> {
    match load(path) {
        Ok(m) => Ok(m),
        Err(LoadError::Missing) => Ok(Memory::default()),
        Err(LoadError::Unavailable(why)) => Err(why),
    }
}

/// Encrypt and write `mem` atomically (temp file + rename).
pub fn save(path: &Path, mem: &Memory) -> Result<(), String> {
    save_with(path, mem, ENTROPY)
}

fn save_with(path: &Path, mem: &Memory, entropy: &[u8]) -> Result<(), String> {
    let bytes = encode_with(mem, entropy)?;
    if let Some(dir) = path.parent() {
        crate::paths::ensure_marked_dir(dir).map_err(|e| format!("cannot create the data folder ({})", e.kind()))?;
    }
    let tmp = path.with_extension("bin.tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("cannot write ({})", e.kind()))?;
        f.write_all(&bytes).map_err(|e| format!("cannot write ({})", e.kind()))?;
        f.sync_all().map_err(|e| format!("cannot write ({})", e.kind()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace the memory file ({})", e.kind())
    })
}

/// Holds the lock file open (exclusive) until dropped. Dies with the process, so it is never
/// stale.
struct Lock(#[allow(dead_code)] std::fs::File);

fn lock(path: &Path) -> Result<Lock, String> {
    use std::os::windows::fs::OpenOptionsExt;
    let lp = path.with_file_name(LOCK_NAME);
    if let Some(dir) = lp.parent() {
        let _ = crate::paths::ensure_marked_dir(dir);
    }
    for _ in 0..60 {
        match std::fs::OpenOptions::new().write(true).create(true).truncate(false).share_mode(0).open(&lp) {
            Ok(f) => return Ok(Lock(f)),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }
    Err("the memory is in use by another Moku window; try again".into())
}

/// A locked read-modify-write: load (a missing file = empty memory), run `f`, save. An unreadable
/// file is an `Err` and **nothing is written** (no overwrite of data that cannot be read).
pub fn update<R>(path: &Path, f: impl FnOnce(&mut Memory) -> R) -> Result<R, String> {
    let _lock = lock(path)?;
    let mut mem = load_or_new(path)?;
    let r = f(&mut mem);
    save(path, &mem)?;
    Ok(r)
}

/// Delete the memory file ("Hapus semua"), readable or not. `Ok(true)` when a file was removed.
pub fn delete_file(path: &Path) -> Result<bool, String> {
    let _lock = lock(path)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot delete the memory file ({})", e.kind())),
    }
}

/// What the settings page shows about the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Empty,
    Ready { facts: usize, history: usize },
    Unavailable(String),
}

pub fn status(path: &Path) -> Status {
    match load(path) {
        Ok(m) => Status::Ready { facts: m.facts.len(), history: m.history.len() },
        Err(LoadError::Missing) => Status::Empty,
        Err(LoadError::Unavailable(w)) => Status::Unavailable(w),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moku-aimem-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample() -> Memory {
        let mut m = Memory::default();
        m.add_fact("Saya tinggal di Surabaya", 1_700_000_000).unwrap();
        m.add_fact("Panggil saya Rani", 1_700_000_100).unwrap();
        m.add_history("translate", "Selamat pagi", "Good morning", "api.example.com", 1_700_000_200);
        m
    }

    #[test]
    fn roundtrip_through_the_file_and_the_file_is_not_plaintext() {
        let d = dir("rt");
        let p = d.join(FILE_NAME);
        assert_eq!(load(&p), Err(LoadError::Missing));
        assert_eq!(status(&p), Status::Empty);
        let m = sample();
        save(&p, &m).unwrap();
        assert_eq!(load(&p).unwrap(), m);
        assert_eq!(status(&p), Status::Ready { facts: 2, history: 1 });
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(&raw[..4], MAGIC);
        let text = String::from_utf8_lossy(&raw).to_string();
        for needle in ["Surabaya", "Rani", "Selamat pagi", "Good morning", "api.example.com", "facts", "history"] {
            assert!(!text.contains(needle), "{needle} must not be readable in the file");
        }
        // Two saves of the same data differ (fresh key + nonce every time).
        save(&p, &m).unwrap();
        assert_ne!(std::fs::read(&p).unwrap(), raw);
        assert_eq!(load(&p).unwrap(), m);
        assert!(!d.join("ai-memory.bin.tmp").exists(), "no temp file left");
    }

    /// Every single flipped bit of the file makes the memory unavailable: never a crash, never
    /// wrong data.
    #[test]
    fn every_flipped_bit_fails_closed() {
        let d = dir("flip");
        let p = d.join(FILE_NAME);
        save(&p, &sample()).unwrap();
        let good = std::fs::read(&p).unwrap();
        for i in 0..good.len() {
            let mut bad = good.clone();
            bad[i] ^= 1 << (i % 8);
            assert!(decode_with(&bad, ENTROPY).is_err(), "byte {i} of {} was changed and the file still opened", good.len());
        }
    }

    #[test]
    fn truncated_extended_and_hostile_files_fail_closed() {
        let d = dir("hostile");
        let p = d.join(FILE_NAME);
        save(&p, &sample()).unwrap();
        let good = std::fs::read(&p).unwrap();
        for cut in [0, 1, 4, 7, 8, 20, good.len() / 2, good.len() - 1] {
            assert!(decode_with(&good[..cut], ENTROPY).is_err(), "cut at {cut}");
        }
        let mut longer = good.clone();
        longer.push(0);
        assert!(decode_with(&longer, ENTROPY).is_err(), "extra byte");
        // Header claims absurd sizes.
        let mut huge = good.clone();
        huge[6] = 0xff;
        huge[7] = 0xff;
        assert!(decode_with(&huge, ENTROPY).is_err(), "wrapped_len 65535");
        let wl = u16::from_le_bytes([good[6], good[7]]) as usize;
        let mut huge = good.clone();
        let at = HEAD + wl + crypt::NONCE_LEN;
        huge[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_with(&huge, ENTROPY).is_err(), "ct_len 4 GB");
        // Other versions and unrelated files.
        let mut v2 = good.clone();
        v2[4] = 2;
        assert!(decode_with(&v2, ENTROPY).unwrap_err().contains("version"));
        assert!(decode_with(b"{\"facts\":[]}", ENTROPY).is_err(), "plain JSON is not accepted");
        assert!(decode_with(&[], ENTROPY).is_err());
        // The same through the file API: unavailable, and the bytes on disk are not touched.
        std::fs::write(&p, &good[..good.len() / 2]).unwrap();
        assert!(matches!(load(&p), Err(LoadError::Unavailable(_))));
        assert!(matches!(status(&p), Status::Unavailable(_)));
    }

    #[test]
    fn an_unreadable_file_is_never_overwritten() {
        let d = dir("nodataloss");
        let p = d.join(FILE_NAME);
        save(&p, &sample()).unwrap();
        let mut bad = std::fs::read(&p).unwrap();
        let n = bad.len();
        bad[n - 3] ^= 0xff;
        std::fs::write(&p, &bad).unwrap();
        // update() refuses and leaves the damaged file exactly as it was.
        let e = update(&p, |m| m.add_fact("sesuatu yang baru", 1).map(|_| ())).unwrap_err();
        assert!(!e.is_empty());
        assert_eq!(std::fs::read(&p).unwrap(), bad, "not overwritten");
        assert!(load_or_new(&p).is_err());
        // Only the explicit "Hapus semua" removes it; then a new memory starts clean.
        assert_eq!(delete_file(&p), Ok(true));
        assert_eq!(delete_file(&p), Ok(false));
        update(&p, |m| m.add_fact("mulai baru", 5)).unwrap().unwrap();
        assert_eq!(load(&p).unwrap().facts.len(), 1);
    }

    /// A file wrapped for another "scope" (different DPAPI entropy here; another Windows user
    /// behaves the same: the key cannot be unwrapped) is unavailable, not a crash.
    #[test]
    fn a_file_for_another_scope_is_unavailable() {
        let d = dir("scope");
        let p = d.join(FILE_NAME);
        save_with(&p, &sample(), b"somebody else").unwrap();
        match load(&p) {
            Err(LoadError::Unavailable(why)) => assert!(why.contains("account"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(update(&p, |_| ()).is_err());
    }

    #[test]
    fn facts_add_edit_delete() {
        let mut m = Memory::default();
        let a = m.add_fact("  Saya\ttinggal \n di   Surabaya ", 10).unwrap();
        assert_eq!(m.facts[0].text, "Saya tinggal di Surabaya", "whitespace is normalised");
        assert_eq!(m.add_fact("saya TINGGAL di surabaya", 11), Err(FactError::Duplicate));
        assert_eq!(m.add_fact("   ", 11), Err(FactError::Empty));
        assert_eq!(m.add_fact(&"x".repeat(MAX_FACT_CHARS + 1), 11), Err(FactError::TooLong));
        assert!(m.add_fact(&"x".repeat(MAX_FACT_CHARS), 11).is_ok());
        let b = m.add_fact("Panggil saya Rani", 12).unwrap();
        // Edit keeps the id and the date, refuses duplicates / empty / missing.
        m.edit_fact(a, "Saya tinggal di Surabaya").unwrap();
        assert_eq!((m.facts[0].id, m.facts[0].ts, m.facts[0].text.as_str()), (a, 10, "Saya tinggal di Surabaya"));
        assert_eq!(m.edit_fact(a, "panggil saya rani"), Err(FactError::Duplicate));
        assert_eq!(m.edit_fact(a, ""), Err(FactError::Empty));
        assert_eq!(m.edit_fact(999, "x"), Err(FactError::Missing));
        assert_eq!(m.edit_fact(b, "Panggil saya Rani"), Ok(()), "unchanged text is not a duplicate of itself");
        // Delete.
        assert!(m.delete_fact(a));
        assert!(!m.delete_fact(a));
        assert_eq!(m.facts.iter().filter(|f| f.id == a).count(), 0);
        // Ids are never reused, even after deletes.
        let c = m.add_fact("fakta baru", 13).unwrap();
        assert!(c > b && c > a);
    }

    #[test]
    fn the_fact_list_is_capped() {
        let mut m = Memory::default();
        for i in 0..MAX_FACTS {
            m.add_fact(&format!("fakta {i}"), 1).unwrap();
        }
        assert_eq!(m.add_fact("satu lagi", 1), Err(FactError::Full));
        assert_eq!(m.facts.len(), MAX_FACTS);
    }

    #[test]
    fn history_is_capped_cut_and_swept_by_retention() {
        let now = 1_700_000_000i64;
        let day = 86_400;
        let mut m = Memory::default();
        m.add_history("ask", "lama", "jawab", "h", now - 40 * day);
        m.add_history("ask", "agak lama", "jawab", "h", now - 10 * day);
        m.add_history("ask", "baru", &"z".repeat(MAX_ENTRY_CHARS + 50), "h", now - day);
        assert_eq!(m.history[2].a.chars().count(), MAX_ENTRY_CHARS, "long answers are cut");
        // Retention: 7 days drops the first two, 30 days only the oldest.
        let mut m30 = m.clone();
        assert_eq!(m30.sweep(now, 30), 1);
        assert_eq!(m30.history.len(), 2);
        assert_eq!(m.sweep(now, 7), 2);
        assert_eq!(m.history.len(), 1);
        assert_eq!(m.history[0].q, "baru");
        assert_eq!(m.sweep(now, 7), 0, "idempotent");
        // Facts never expire.
        let mut f = Memory::default();
        f.add_fact("selalu ada", now - 400 * day).unwrap();
        assert_eq!(f.sweep(now, 7), 0);
        assert_eq!(f.facts.len(), 1);
        // The history keeps the newest MAX_HISTORY entries.
        let mut big = Memory::default();
        for i in 0..MAX_HISTORY + 20 {
            big.add_history("ask", &format!("q{i}"), "a", "h", now);
        }
        assert_eq!(big.history.len(), MAX_HISTORY);
        assert_eq!(big.history[0].q, "q20");
        // Deleting one entry and clearing all.
        let id = big.history[3].id;
        assert!(big.delete_history(id));
        assert!(!big.delete_history(id));
        big.clear_history();
        assert!(big.history.is_empty());
    }

    #[test]
    fn export_is_plain_json_with_the_user_data_and_no_key_material() {
        let m = sample();
        let json = m.export_json(1_800_000_000);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["kind"], "tanya-ai-memory");
        assert_eq!(v["exported_at"], 1_800_000_000i64);
        assert_eq!(v["facts"].as_array().unwrap().len(), 2);
        assert_eq!(v["facts"][0]["text"], "Saya tinggal di Surabaya");
        assert_eq!(v["history"][0]["a"], "Good morning");
        assert!(v.get("key").is_none() && !json.contains("MKAI"));
    }

    #[test]
    fn facts_for_prompt_keeps_the_newest_within_the_limits() {
        let mut m = Memory::default();
        for t in ["satu", "dua", "tiga", "empat", "lima"] {
            m.add_fact(t, 1).unwrap();
        }
        assert_eq!(m.facts_for_prompt(10, 1000), ["satu", "dua", "tiga", "empat", "lima"]);
        assert_eq!(m.facts_for_prompt(2, 1000), ["empat", "lima"], "newest two");
        assert_eq!(m.facts_for_prompt(10, 9), ["empat", "lima"], "9 characters: 'lima' + 'empat'");
        assert!(m.facts_for_prompt(0, 1000).is_empty());
    }

    #[test]
    fn update_is_a_locked_read_modify_write_that_two_threads_can_share() {
        let d = dir("threads");
        let p = d.join(FILE_NAME);
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for i in 0..5 {
                        update(&p, |m| m.add_fact(&format!("fakta {t}-{i}"), 1).map(|_| ())).unwrap().unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(load(&p).unwrap().facts.len(), 20, "no update was lost");
    }

    /// Screenshots only: writes invented facts and history to the file named by
    /// `MOKU_AIMEM_SEED` (never the real memory). Run:
    /// `$env:MOKU_AIMEM_SEED="<scratch>\ai-memory.bin"; cargo test -p moku-core seed_sample -- --ignored`
    #[test]
    #[ignore = "writes sample data to the file named by MOKU_AIMEM_SEED"]
    fn seed_sample_memory_for_screenshots() {
        let Some(p) = std::env::var_os("MOKU_AIMEM_SEED") else { return };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let mut m = Memory::default();
        m.add_fact("Panggil saya Nama", now - 20 * 86_400).unwrap();
        m.add_fact("Saya tinggal di Surabaya", now - 12 * 86_400).unwrap();
        m.add_fact("Jawab singkat dan pakai bahasa Indonesia santai", now - 3 * 86_400).unwrap();
        m.add_history("translate", "Selamat pagi", "Good morning", "localhost:18765", now - 86_400);
        m.add_history("ask", "Apa itu Rust?", "Bahasa pemrograman sistem.", "localhost:18765", now - 3_600);
        save(Path::new(&p), &m).unwrap();
    }

    #[test]
    fn a_lost_file_and_the_default_path() {
        assert!(default_path().ends_with(FILE_NAME));
        let d = dir("lost");
        assert_eq!(load(&d.join("nope.bin")), Err(LoadError::Missing));
        // A directory where the file should be is "unavailable", not a panic.
        let p = d.join(FILE_NAME);
        std::fs::create_dir_all(&p).unwrap();
        assert!(matches!(load(&p), Err(LoadError::Unavailable(_))));
    }
}
