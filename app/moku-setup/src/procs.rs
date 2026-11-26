//! Stop the Moku processes that run **from the program folder**, and nothing else: a process is
//! matched by the full path of its image, never by name, so a development build or a portable
//! copy elsewhere keeps running. This process is never stopped.
//!
//! Gentle first: `WM_CLOSE` to the island's own window (class `MokuIsland`; its window procedure
//! then quits cleanly and flushes pending saves) and to the visible top-level windows of the
//! other exes (the settings window). Whatever is still running after the timeout is terminated.
//! Then the folder is looked at once more after a short wait, and whatever started or was still
//! shutting down in the meantime (the island that launched an `/update` quits itself right after
//! launching it, and may have started helpers of its own) gets the same treatment.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use moku_core::install::path_is_within;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW,
    TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE};
use windows::core::{BOOL, PWSTR};

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A running process whose image lies inside the folder.
pub struct Found {
    pub pid: u32,
    pub path: PathBuf,
    handle: Handle,
}

fn image_path(h: HANDLE) -> Option<PathBuf> {
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// Is a process with this id running right now? (A process we may not open, another user's or an
/// elevated one, still counts as running.)
pub fn is_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, WAIT_TIMEOUT};
    if pid == 0 {
        return false;
    }
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(h) => {
            let h = Handle(h);
            // Still running: the process object is not signalled yet.
            unsafe { WaitForSingleObject(h.0, 0) == WAIT_TIMEOUT }
        }
        Err(e) => e.code() == ERROR_ACCESS_DENIED.to_hresult(),
    }
}

/// Every process (other than this one) whose exe is inside `dir`.
pub fn find_within(dir: &Path) -> Vec<Found> {
    let me = std::process::id();
    let mut out = vec![];
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else { return out };
    let snap = Handle(snap);
    let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut ok = unsafe { Process32FirstW(snap.0, &mut e) }.is_ok();
    while ok {
        let pid = e.th32ProcessID;
        if pid != me && pid != 0 {
            if let Ok(h) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) } {
                let h = Handle(h);
                // An exited process stays listed while somebody still holds a handle to it.
                let exited = unsafe { WaitForSingleObject(h.0, 0) } == WAIT_OBJECT_0;
                if let Some(path) = image_path(h.0).filter(|_| !exited) {
                    if path_is_within(&path, dir) {
                        out.push(Found { pid, path, handle: h });
                    }
                }
            }
        }
        ok = unsafe { Process32NextW(snap.0, &mut e) }.is_ok();
    }
    out
}

struct CloseCtx {
    pids: Vec<u32>,
}

unsafe extern "system" fn close_windows(hwnd: HWND, lp: LPARAM) -> BOOL {
    let ctx = unsafe { &*(lp.0 as *const CloseCtx) };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if ctx.pids.contains(&pid) {
        let mut cls = [0u16; 64];
        let n = unsafe { GetClassNameW(hwnd, &mut cls) }.max(0) as usize;
        let cls = String::from_utf16_lossy(&cls[..n]);
        // The island: only its main window (its hit / tray windows go with it). Others: the
        // visible top-level windows.
        let post = if cls.starts_with("MokuIsland") || cls == "MokuTray" { cls == "MokuIsland" } else { unsafe { IsWindowVisible(hwnd) }.as_bool() };
        if post {
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }
    BOOL(1)
}

/// How long to wait before looking at the folder again after a round of stopping.
const SETTLE: Duration = Duration::from_millis(300);
/// Rounds of stop-wait-look-again before it is left to the swap (a program that restarts itself).
const MAX_ROUNDS: u32 = 4;

/// Stop every process running from `dir`. Returns how many were running (in all rounds).
pub fn stop_within(dir: &Path, timeout: Duration) -> Result<usize, String> {
    let mut found = find_within(dir);
    if found.is_empty() {
        return Ok(0);
    }
    let mut total = 0;
    for round in 1..=MAX_ROUNDS {
        total += stop_found(&found, timeout)?;
        std::thread::sleep(SETTLE);
        found = find_within(dir);
        if found.is_empty() {
            return Ok(total);
        }
        let list: Vec<String> = found.iter().map(|f| format!("pid {} {}", f.pid, f.path.display())).collect();
        crate::log(&format!("round {round}: still or again running from the folder: {}", list.join(", ")));
    }
    crate::log("something keeps running from the folder; leaving it to the swap");
    Ok(total)
}

/// Close, wait for and if need be terminate these processes. Returns how many there were.
fn stop_found(found: &[Found], timeout: Duration) -> Result<usize, String> {
    let ctx = CloseCtx { pids: found.iter().map(|f| f.pid).collect() };
    unsafe {
        let _ = EnumWindows(Some(close_windows), LPARAM(&ctx as *const CloseCtx as isize));
    }
    let deadline = Instant::now() + timeout;
    let mut stubborn = vec![];
    for f in found {
        let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
        if unsafe { WaitForSingleObject(f.handle.0, left) } != WAIT_OBJECT_0 {
            stubborn.push(f);
        }
    }
    for f in stubborn {
        crate::log(&format!("terminating pid {} ({})", f.pid, f.path.display()));
        unsafe {
            let _ = TerminateProcess(f.handle.0, 1);
            if WaitForSingleObject(f.handle.0, 5000) != WAIT_OBJECT_0 {
                return Err(format!("{} (pid {}) did not stop", f.path.display(), f.pid));
            }
        }
    }
    Ok(found.len())
}
