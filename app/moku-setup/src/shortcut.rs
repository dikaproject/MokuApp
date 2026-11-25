//! The Start Menu shortcut: `IShellLinkW` saved through `IPersistFile`.

use std::path::Path;

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{HSTRING, Interface};

/// COM for the current thread, released on drop when this call initialised it.
struct Com(bool);

impl Com {
    fn init() -> Com {
        // S_OK / S_FALSE: ours to release. RPC_E_CHANGED_MODE (an MTA thread): usable, not ours.
        Com(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// Write `lnk` pointing at `target` (started in `workdir`, icon from `target`).
pub fn create(lnk: &Path, target: &Path, workdir: &Path, description: &str) -> Result<(), String> {
    if let Some(dir) = lnk.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }
    let _com = Com::init();
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("ShellLink: {e}"))?;
        let t = HSTRING::from(target.as_os_str());
        link.SetPath(&t).map_err(|e| format!("SetPath: {e}"))?;
        link.SetWorkingDirectory(&HSTRING::from(workdir.as_os_str())).map_err(|e| format!("SetWorkingDirectory: {e}"))?;
        link.SetDescription(&HSTRING::from(description)).map_err(|e| format!("SetDescription: {e}"))?;
        link.SetIconLocation(&t, 0).map_err(|e| format!("SetIconLocation: {e}"))?;
        let file: IPersistFile = link.cast().map_err(|e| format!("IPersistFile: {e}"))?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true).map_err(|e| format!("save {}: {e}", lnk.display()))?;
    }
    Ok(())
}

/// The target path stored in `lnk` (tests).
#[cfg(test)]
pub fn target(lnk: &Path) -> Result<std::path::PathBuf, String> {
    let _com = Com::init();
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| format!("ShellLink: {e}"))?;
        let file: IPersistFile = link.cast().map_err(|e| format!("IPersistFile: {e}"))?;
        file.Load(&HSTRING::from(lnk.as_os_str()), windows::Win32::System::Com::STGM_READ).map_err(|e| format!("load {}: {e}", lnk.display()))?;
        let mut buf = vec![0u16; 1024];
        // SLGP_RAWPATH = 4: the stored path, without resolving environment variables.
        link.GetPath(&mut buf, std::ptr::null_mut(), 4).map_err(|e| format!("GetPath: {e}"))?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Ok(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])))
    }
}
