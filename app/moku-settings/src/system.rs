//! Small Windows helpers: single instance, open a folder / document, font detection.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsIconic, SW_RESTORE, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow};
use windows::core::{HSTRING, PCWSTR};

/// The window title, also what a second launch looks for.
pub const WINDOW_TITLE: &str = "Moku — Pengaturan";

/// The named mutex that makes the settings window a singleton.
const MUTEX_NAME: &str = r"Local\MokuSettings.SingleInstance";

/// Keeps the single-instance mutex alive; dropping it releases the name again.
pub struct InstanceGuard(HANDLE);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Take the named mutex `name`; `None` when another process (or guard) already holds it.
pub fn try_acquire(name: &str) -> Option<InstanceGuard> {
    let h = unsafe { CreateMutexW(None, true, &HSTRING::from(name)) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(h);
        }
        return None;
    }
    Some(InstanceGuard(h))
}

/// Try to become the only settings window. `Err(())` means another one is running; it has been
/// brought to the front and this process should exit.
///
/// Runs with `MOKU_DATA_DIR` set (tests, portable copies) never conflict with each other or
/// with the real window, so this check is skipped for them.
pub fn single_instance() -> Result<Option<InstanceGuard>, ()> {
    if std::env::var_os("MOKU_DATA_DIR").is_some_and(|d| !d.is_empty()) {
        return Ok(None);
    }
    match try_acquire(MUTEX_NAME) {
        Some(g) => Ok(Some(g)),
        None => {
            bring_existing_to_front();
            Err(())
        }
    }
}

fn bring_existing_to_front() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(PCWSTR::null(), &HSTRING::from(WINDOW_TITLE)) {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

/// Open a file or folder with its default handler (Explorer for folders).
pub fn open_path(path: &Path) -> bool {
    let r = unsafe { ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(path.as_os_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    // ShellExecute returns a value > 32 on success.
    r.0 as usize > 32
}

/// (UI font, icon font): Segoe UI Variable and Segoe Fluent Icons when installed, else the
/// Windows 10 fonts.
pub fn fonts() -> (&'static str, &'static str) {
    let dir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows")).join("Fonts");
    let has = |f: &str| dir.join(f).exists();
    let ui = if has("SegUIVar.ttf") { "Segoe UI Variable Text" } else { "Segoe UI" };
    let icons = if has("SegoeIcons.ttf") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
    (ui, icons)
}

/// Where `claude-code-hook.md` lives relative to this exe: next to it, in a `docs` folder, or
/// (in a source checkout, with the exe in `target\release`) in the repository's `app\docs`.
pub const DOC_NAME: &str = "claude-code-hook.md";

pub fn find_doc() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    for _ in 0..8 {
        for rel in ["docs", "app/docs", ""] {
            let p = if rel.is_empty() { dir.join(DOC_NAME) } else { dir.join(rel).join(DOC_NAME) };
            if p.is_file() {
                return Some(p);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_instance_is_refused_until_the_first_exits() {
        let name = format!(r"Local\MokuSettings.Test.{}", std::process::id());
        let first = try_acquire(&name).expect("first instance gets the mutex");
        assert!(try_acquire(&name).is_none(), "a second one is refused while the first lives");
        drop(first);
        assert!(try_acquire(&name).is_some(), "the name is free again after the first exits");
    }

    #[test]
    fn fonts_fall_back_to_the_windows_10_set() {
        let (ui, icons) = fonts();
        assert!(ui.starts_with("Segoe UI"));
        assert!(icons.starts_with("Segoe "));
    }

    #[test]
    fn doc_lookup_does_not_panic_without_the_file() {
        // In a build folder the guide usually is not next to the exe; either answer is fine.
        if let Some(p) = find_doc() {
            assert!(p.is_file());
        }
    }
}