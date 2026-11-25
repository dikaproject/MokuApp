//! `ReadDirectoryChangesW` on one folder, on a service thread.
//!
//! Only file-name changes are requested (create, delete, rename): no storm of "size changed"
//! events while a big download writes. The caller looks at sizes itself, once a second, only
//! while a download is running (see [`watch`]'s `step`). Nothing here opens a file.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_NOTIFY_ENUM_DIR, ERROR_OPERATION_ABORTED, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForMultipleObjects};
use windows::core::PCWSTR;

use crate::services::ServiceCtx;

/// One record of the kernel's change list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Raw {
    Added(String),
    Removed(String),
    Modified(String),
    RenamedOld(String),
    RenamedNew(String),
}

/// A change with renames paired up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fs {
    Added(String),
    Removed(String),
    Modified(String),
    Renamed { from: String, to: String },
    /// The kernel's buffer overflowed: some changes were lost, look at the folder again.
    Rescan,
}

/// Decode a `FILE_NOTIFY_INFORMATION` chain. Stops quietly at anything malformed.
pub fn parse_notifications(buf: &[u8]) -> Vec<Raw> {
    let mut out = vec![];
    let mut off = 0usize;
    let u32_at = |o: usize| buf.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    while let (Some(next), Some(action), Some(len)) = (u32_at(off), u32_at(off + 4), u32_at(off + 8)) {
        let (start, end) = (off + 12, off + 12 + len);
        if len % 2 != 0 || end > buf.len() {
            break;
        }
        let units: Vec<u16> = buf[start..end].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let name = String::from_utf16_lossy(&units);
        match action {
            1 => out.push(Raw::Added(name)),
            2 => out.push(Raw::Removed(name)),
            3 => out.push(Raw::Modified(name)),
            4 => out.push(Raw::RenamedOld(name)),
            5 => out.push(Raw::RenamedNew(name)),
            _ => {}
        }
        if next == 0 || next < 12 {
            break;
        }
        off += next;
    }
    out
}

/// Joins `RenamedOld` + `RenamedNew` into [`Fs::Renamed`], also when the two arrive in
/// different reads.
#[derive(Default, Debug)]
pub struct Pairer {
    old: Option<String>,
}

impl Pairer {
    pub fn push(&mut self, raw: Raw) -> Vec<Fs> {
        let mut out = vec![];
        match raw {
            Raw::RenamedOld(n) => {
                if let Some(o) = self.old.replace(n) {
                    out.push(Fs::Removed(o));
                }
            }
            Raw::RenamedNew(n) => out.push(match self.old.take() {
                Some(from) => Fs::Renamed { from, to: n },
                None => Fs::Added(n),
            }),
            other => {
                out.extend(self.flush());
                out.push(match other {
                    Raw::Added(n) => Fs::Added(n),
                    Raw::Removed(n) => Fs::Removed(n),
                    Raw::Modified(n) => Fs::Modified(n),
                    Raw::RenamedOld(_) | Raw::RenamedNew(_) => return out,
                });
            }
        }
        out
    }

    /// An old name whose new name never came: it was moved out of the folder.
    pub fn flush(&mut self) -> Option<Fs> {
        self.old.take().map(Fs::Removed)
    }
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Watch `dir` until the service is asked to stop (`Ok`) or the folder cannot be watched
/// (`Err`, with a message free of file names).
///
/// `step` is called with each batch of changes (empty on a timer tick) and answers how long to
/// wait before the next tick: `None` = until something changes.
pub fn watch(ctx: &ServiceCtx, dir: &Path, step: &mut dyn FnMut(Vec<Fs>) -> Option<Duration>) -> Result<(), String> {
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let h = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        )
    }
    .map_err(|e| format!("cannot open the folder ({})", e.code().0))?;
    let dirh = Handle(h);
    let event = Handle(unsafe { CreateEventW(None, true, false, None) }.map_err(|e| format!("cannot create an event ({})", e.code().0))?);
    // u32 storage keeps the buffer aligned for the kernel.
    let mut buf = vec![0u32; 16 * 1024];
    let mut pairer = Pairer::default();
    let mut wait = step(vec![]);
    loop {
        let mut ov = OVERLAPPED { hEvent: event.0, ..Default::default() };
        unsafe { windows::Win32::System::Threading::ResetEvent(event.0) }.map_err(|e| format!("event reset failed ({})", e.code().0))?;
        unsafe {
            ReadDirectoryChangesW(dirh.0, buf.as_mut_ptr() as *mut _, (buf.len() * 4) as u32, false, FILE_NOTIFY_CHANGE_FILE_NAME, None, Some(&mut ov), None)
        }
        .map_err(|e| format!("cannot watch the folder ({})", e.code().0))?;
        // Wait for a change, the stop signal or the next tick.
        let finished = loop {
            let ms = wait.map_or(INFINITE, |d| d.as_millis().min(u32::MAX as u128 - 1) as u32);
            let r = unsafe { WaitForMultipleObjects(&[event.0, ctx.stop_event()], false, ms) };
            if r == WAIT_OBJECT_0 {
                break true;
            }
            if r == WAIT_TIMEOUT {
                let mut batch: Vec<Fs> = pairer.flush().into_iter().collect();
                batch.shrink_to_fit();
                wait = step(batch);
                continue;
            }
            // Stop requested (or the wait failed): the request must finish before `ov` and
            // `buf` go away.
            unsafe {
                let _ = CancelIoEx(dirh.0, Some(&ov));
                let mut n = 0u32;
                let _ = GetOverlappedResult(dirh.0, &ov, &mut n, true);
            }
            break false;
        };
        if !finished {
            return if ctx.should_stop() { Ok(()) } else { Err("waiting for changes failed".into()) };
        }
        let mut n = 0u32;
        match unsafe { GetOverlappedResult(dirh.0, &ov, &mut n, false) } {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_OPERATION_ABORTED.to_hresult() => return Ok(()),
            // The kernel's buffer overflowed: changes were lost.
            Err(e) if e.code() == ERROR_NOTIFY_ENUM_DIR.to_hresult() => n = 0,
            Err(e) => return Err(format!("the folder went away ({})", e.code().0)),
        }
        let batch = if n == 0 {
            vec![Fs::Rescan]
        } else {
            // SAFETY: the kernel wrote `n` bytes into the buffer we own.
            let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, (n as usize).min(buf.len() * 4)) };
            parse_notifications(bytes).into_iter().flat_map(|r| pairer.push(r)).collect()
        };
        wait = step(batch);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    /// One `FILE_NOTIFY_INFORMATION` record.
    fn rec(action: u32, name: &str, last: bool) -> Vec<u8> {
        let units: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let mut r = vec![];
        let size = (12 + units.len()).div_ceil(4) * 4;
        r.extend(if last { 0u32 } else { size as u32 }.to_le_bytes());
        r.extend(action.to_le_bytes());
        r.extend((units.len() as u32).to_le_bytes());
        r.extend(units);
        r.resize(size, 0);
        r
    }

    #[test]
    fn notification_records_are_decoded() {
        let mut buf = rec(1, "a.zip.crdownload", false);
        buf.extend(rec(4, "a.zip.crdownload", false));
        buf.extend(rec(5, "a.zip", false));
        buf.extend(rec(2, "naïve ✓.txt", false));
        buf.extend(rec(3, "m", true));
        assert_eq!(
            parse_notifications(&buf),
            [Raw::Added("a.zip.crdownload".into()), Raw::RenamedOld("a.zip.crdownload".into()), Raw::RenamedNew("a.zip".into()), Raw::Removed("naïve ✓.txt".into()), Raw::Modified("m".into())]
        );
    }

    #[test]
    fn malformed_buffers_stop_quietly() {
        assert!(parse_notifications(&[]).is_empty());
        assert!(parse_notifications(&[1, 2, 3]).is_empty());
        // A name longer than the buffer.
        let mut b = rec(1, "abcdef", true);
        b[8..12].copy_from_slice(&1000u32.to_le_bytes());
        assert!(parse_notifications(&b).is_empty());
        // An odd byte length.
        let mut b = rec(1, "abcdef", true);
        b[8..12].copy_from_slice(&3u32.to_le_bytes());
        assert!(parse_notifications(&b).is_empty());
        // A chain that points back at itself must not loop.
        let mut b = rec(1, "x", false);
        b[0..4].copy_from_slice(&4u32.to_le_bytes());
        assert!(parse_notifications(&b).len() <= 1);
        // Unknown action codes are skipped.
        assert!(parse_notifications(&rec(99, "x", true)).is_empty());
    }

    #[test]
    fn renames_are_paired_also_across_reads() {
        let mut p = Pairer::default();
        assert_eq!(p.push(Raw::RenamedOld("a.part".into())), []);
        assert_eq!(p.push(Raw::RenamedNew("a.zip".into())), [Fs::Renamed { from: "a.part".into(), to: "a.zip".into() }]);
        // An old name that is followed by something else was moved away.
        assert_eq!(p.push(Raw::RenamedOld("b.part".into())), []);
        assert_eq!(p.push(Raw::Added("c".into())), [Fs::Removed("b.part".into()), Fs::Added("c".into())]);
        // A new name with no old one is a new file.
        assert_eq!(p.push(Raw::RenamedNew("d".into())), [Fs::Added("d".into())]);
        // Two old names in a row.
        assert_eq!(p.push(Raw::RenamedOld("e".into())), []);
        assert_eq!(p.push(Raw::RenamedOld("f".into())), [Fs::Removed("e".into())]);
        assert_eq!(p.flush(), Some(Fs::Removed("f".into())));
        assert_eq!(p.flush(), None);
    }

    /// The real thing, on a scratch folder: create, rename and delete files and watch the events
    /// arrive; the service stops promptly.
    #[test]
    fn watches_a_real_folder_and_stops_when_asked() {
        let dir = std::env::temp_dir().join(format!("moku-dl-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ServiceCtx::detached("dl-watch-test");
        let (tx, rx) = mpsc::channel::<Vec<Fs>>();
        let (c2, d2) = (ctx.clone(), dir.clone());
        let th = std::thread::spawn(move || {
            watch(&c2, &d2, &mut |batch| {
                if !batch.is_empty() {
                    let _ = tx.send(batch);
                }
                None
            })
        });
        // Give the watcher a moment to arm, then act.
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(dir.join("t.zip.crdownload"), b"abc").unwrap();
        std::fs::rename(dir.join("t.zip.crdownload"), dir.join("t.zip")).unwrap();
        std::fs::remove_file(dir.join("t.zip")).unwrap();
        let mut seen: Vec<Fs> = vec![];
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !seen.iter().any(|f| matches!(f, Fs::Removed(n) if n == "t.zip")) {
            if let Ok(b) = rx.recv_timeout(Duration::from_millis(200)) {
                seen.extend(b);
            }
        }
        ctx.request_stop();
        let stopped = std::time::Instant::now();
        let r = th.join().unwrap();
        assert!(r.is_ok(), "{r:?}");
        assert!(stopped.elapsed() < Duration::from_secs(2), "stops promptly");
        assert!(seen.contains(&Fs::Added("t.zip.crdownload".into())), "{seen:?}");
        assert!(seen.contains(&Fs::Renamed { from: "t.zip.crdownload".into(), to: "t.zip".into() }), "{seen:?}");
        assert!(seen.contains(&Fs::Removed("t.zip".into())), "{seen:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_folder_is_an_error_not_a_hang() {
        let ctx = ServiceCtx::detached("dl-watch-missing");
        let r = watch(&ctx, Path::new(r"C:\definitely\not\a\folder\moku-test"), &mut |_| None);
        assert!(r.is_err());
    }
}
