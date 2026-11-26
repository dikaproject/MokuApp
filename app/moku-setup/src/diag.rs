//! What to write to the installer log when the swap fails, so the next report says who held the
//! folder: the step and path, the OS error, the processes still running from the folder and the
//! processes the Restart Manager says hold files in it.
//!
//! All of it is best effort: nothing here returns an error or panics, and a failing lookup is only
//! a shorter log.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
use windows::Win32::System::RestartManager::{CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmGetList, RmRegisterResources, RmStartSession};
use windows::core::{PCWSTR, PWSTR};

/// Files the Restart Manager is asked about (a program folder has a handful).
const MAX_FILES: usize = 2000;

/// The OS error code of `e`, spelled the way the log reads best.
fn os_error(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(c) => format!("{e} (code {c})"),
        None => e.to_string(),
    }
}

/// Write the failed step and everything known about who may be holding `dir` to the log.
pub fn log_failure(dir: &Path, step: &str, path: &Path, error: &std::io::Error) {
    crate::log(&format!("swap failed: {step}: {}: {}", path.display(), os_error(error)));
    let running = crate::procs::find_within(dir);
    if running.is_empty() {
        crate::log("no process runs from the install folder");
    }
    for f in &running {
        crate::log(&format!("still running from the install folder: pid {} {}", f.pid, f.path.display()));
    }
    let holders = holders(dir);
    if holders.is_empty() {
        crate::log("restart manager: no process holds a file in the install folder (or it could not be asked)");
    }
    for h in holders {
        crate::log(&format!("restart manager: {h}"));
    }
}

fn files_below(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        match e.file_type() {
            Ok(t) if t.is_dir() => files_below(&e.path(), out),
            Ok(_) => out.push(e.path()),
            Err(_) => {}
        }
    }
}

fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

fn from_wide(s: &[u16]) -> String {
    String::from_utf16_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())])
}

/// The processes (`pid name`) the Restart Manager says have a file below `dir` open or loaded.
/// Empty when none do, when there are no files, or when the Restart Manager cannot be used.
pub fn holders(dir: &Path) -> Vec<String> {
    let mut files = vec![];
    files_below(dir, &mut files);
    if files.is_empty() {
        return vec![];
    }
    let names: Vec<Vec<u16>> = files.iter().map(|f| wide(f)).collect();
    let ptrs: Vec<PCWSTR> = names.iter().map(|n| PCWSTR(n.as_ptr())).collect();

    let mut session = 0u32;
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    if unsafe { RmStartSession(&mut session, None, PWSTR(key.as_mut_ptr())) } != ERROR_SUCCESS {
        return vec![];
    }
    let mut out = vec![];
    unsafe {
        if RmRegisterResources(session, Some(&ptrs), None, None) == ERROR_SUCCESS {
            let mut needed = 0u32;
            let mut have = 0u32;
            let mut reasons = 0u32;
            let mut r = RmGetList(session, &mut needed, &mut have, None, &mut reasons);
            // The list can grow between the two calls: ask again a few times.
            for _ in 0..4 {
                if r != ERROR_MORE_DATA || needed == 0 {
                    break;
                }
                let mut buf = vec![RM_PROCESS_INFO::default(); needed as usize];
                have = needed;
                r = RmGetList(session, &mut needed, &mut have, Some(buf.as_mut_ptr()), &mut reasons);
                if r == ERROR_SUCCESS {
                    for p in buf.iter().take(have as usize) {
                        let svc = from_wide(&p.strServiceShortName);
                        let svc = if svc.is_empty() { String::new() } else { format!(" (service {svc})") };
                        out.push(format!("pid {} {}{svc}", p.Process.dwProcessId, from_wide(&p.strAppName)));
                    }
                }
            }
        }
        let _ = RmEndSession(session);
    }
    out
}
