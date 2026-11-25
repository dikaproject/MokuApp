//! Checks shared by every feature that can open a file the user did not pick by name just now
//! (finished downloads, Shelf files in the launcher): programs and scripts are never *opened*
//! from the island, only shown in Explorer.

use std::path::Path;

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{ILFree, SHOpenFolderAndSelectItems, SHParseDisplayName};
use windows::core::HSTRING;

/// Extensions of programs, scripts, installers, shortcuts and other things that run code (or
/// pull in more code) when they are opened. Lower case, no dot.
const RISKY: &[&str] = &[
    // programs and installers
    "exe", "msi", "msix", "msixbundle", "appx", "appxbundle", "appinstaller", "msp", "com", "scr", "pif", "application", "gadget", "jar", "cpl", "dll", "sys", "inf",
    // scripts and script hosts
    "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "wsc", "sct", "hta", "reg", "py", "pyw",
    // things that open straight into a program or a remote location
    "lnk", "url", "website", "msc", "chm", "diagcab", "settingcontent-ms", "library-ms", "search-ms",
    // disk images Windows mounts on a double click (their contents then run from a new drive)
    "iso", "img", "vhd", "vhdx",
];

fn ext_is_risky(part: &str) -> bool {
    // Windows ignores trailing dots and spaces in a file name ("setup.exe. " is "setup.exe").
    let part = part.trim_end_matches(['.', ' ']);
    part.rsplit_once('.').is_some_and(|(_, ext)| RISKY.iter().any(|r| r.eq_ignore_ascii_case(ext)))
}

/// Is `name` (a file name or a path) a program or script? Judged by the extension of the last
/// path component; each `:`-separated part counts (`C:setup.exe`, `a.txt:evil.exe`), so neither
/// a drive-relative path nor an alternate data stream hides the extension.
pub fn is_risky(name: &str) -> bool {
    let last = name.rsplit(['\\', '/']).next().unwrap_or(name);
    last.split(':').any(ext_is_risky)
}

/// Explorer with `path` selected (`SHOpenFolderAndSelectItems`). Never starts the file.
/// Blocking: call from a worker thread.
pub fn reveal(path: &Path) -> bool {
    unsafe {
        // Explorer's COM objects need an apartment; on a thread that already has one this is a
        // no-op (and then must not be balanced).
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        let ok = SHParseDisplayName(&HSTRING::from(path.as_os_str()), None, &mut pidl, 0, None).is_ok()
            && !pidl.is_null()
            && SHOpenFolderAndSelectItems(pidl, None, 0).is_ok();
        if !pidl.is_null() {
            ILFree(Some(pidl));
        }
        if init {
            CoUninitialize();
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_and_scripts_are_risky_documents_are_not() {
        for n in ["setup.exe", "SETUP.EXE", "a.b.msi", "run.ps1", "x.bat", "y.js", "z.lnk", "p.hta", "t.dll", r"C:\Users\x\Downloads\setup.exe", "D:/a/b/run.cmd"] {
            assert!(is_risky(n), "{n}");
        }
        // The v0.2 additions (consoles, help files, installers, scripts, images, search / library
        // connectors, shortcuts to web sites).
        for ext in ["msc", "chm", "appinstaller", "settingcontent-ms", "msp", "wsc", "sct", "py", "pyw", "diagcab", "iso", "img", "vhd", "vhdx", "application", "library-ms", "search-ms", "url", "website"] {
            assert!(is_risky(&format!("file.{ext}")), "{ext}");
            assert!(is_risky(&format!("FILE.{}", ext.to_uppercase())), "{ext} in capitals");
        }
        for n in ["report.pdf", "photo.jpg", "notes.txt", "archive.zip", "movie.mp4", "noext", "exe", ".gitignore", "a.exe.txt", r"C:\dir.exe\readme.txt", "C:\\a.exe\\"] {
            assert!(!is_risky(n), "{n}");
        }
    }

    #[test]
    fn trailing_dots_spaces_and_streams_do_not_hide_the_extension() {
        for n in ["setup.exe.", "setup.exe . ", "setup.exe...", "C:setup.exe", "notes.txt:evil.exe", r"C:\x\a.txt:evil.ps1:$DATA"] {
            assert!(is_risky(n), "{n}");
        }
    }
}
