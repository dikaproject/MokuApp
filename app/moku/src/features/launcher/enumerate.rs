//! What the helper process (`moku.exe --launcher-index`) does: resolve the Start Menu shortcuts,
//! enumerate the packaged apps and extract their icons. This code only runs in the helper; the
//! island reads the file it writes (`launcher/index.rs`), so none of the shell libraries these
//! calls load ever enter the island's address space.

use std::collections::HashSet;
use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;
use windows::core::{GUID, HSTRING, Interface, PWSTR, w};

use super::matcher::fold;
use crate::helper::icon::{Com, shell_icon};
use crate::helper::pack::{Icon, Kind, Rec};

/// Program shortcut names that are noise in a launcher.
pub fn is_noise(name: &str) -> bool {
    let n = name.to_lowercase();
    ["uninstall", "hapus instalasi", "copot", "readme", "read me", "release notes", "license", "lisensi"].iter().any(|w| n.contains(w))
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s.map(PathBuf::from)
    }
}

fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() && depth < 4 => walk(&p, depth + 1, out),
            Ok(t) if t.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")) => out.push(p),
            _ => {}
        }
        if out.len() > 3000 {
            return;
        }
    }
}

/// The program a shortcut points at (`IShellLinkW::GetPath`, no resolving / searching).
fn link_target(link: &Path) -> Option<String> {
    unsafe {
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let pf: IPersistFile = sl.cast().ok()?;
        pf.Load(&HSTRING::from(link.as_os_str()), STGM_READ).ok()?;
        let mut buf = [0u16; 520];
        let mut fd = WIN32_FIND_DATAW::default();
        sl.GetPath(&mut buf, &mut fd, SLGP_RAWPATH.0 as u32).ok()?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        (n > 0).then(|| String::from_utf16_lossy(&buf[..n]))
    }
}

fn file_name(p: &str) -> String {
    p.rsplit(['\\', '/']).next().unwrap_or(p).to_string()
}

/// Start Menu shortcuts (user's first, then all users'), then packaged apps not already there,
/// each with its icon. Duplicates by name are dropped. Blocking: the helper's main thread.
pub fn build(store_apps: bool) -> Vec<Rec<Icon>> {
    let _com = Com::init();
    let mut out: Vec<Rec<Icon>> = vec![];
    let mut seen: HashSet<String> = HashSet::new();
    let mut links = vec![];
    for id in [&FOLDERID_Programs, &FOLDERID_CommonPrograms] {
        if let Some(d) = known_folder(id) {
            walk(&d, 0, &mut links);
        }
    }
    for link in links {
        let Some(name) = link.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        if is_noise(&name) || !seen.insert(fold(&name)) {
            continue;
        }
        let target = link_target(&link);
        if target.as_deref().is_some_and(|t| is_noise(&file_name(t)) || file_name(t).to_lowercase().starts_with("unins")) {
            continue;
        }
        let path = link.to_string_lossy().into_owned();
        out.push(Rec { kind: Kind::Link, icon: shell_icon(&path), name, detail: target.map(|t| file_name(&t)).unwrap_or_default(), target: path });
    }
    if store_apps {
        for (name, aumid) in packaged_apps() {
            if is_noise(&name) || !seen.insert(fold(&name)) {
                continue;
            }
            out.push(Rec { kind: Kind::App, icon: shell_icon(&format!("shell:AppsFolder\\{aumid}")), name, detail: String::new(), target: aumid });
        }
    }
    out
}

/// `shell:AppsFolder` items whose parsing name is an AUMID (`Package_hash!App`).
fn packaged_apps() -> Vec<(String, String)> {
    let mut out = vec![];
    unsafe {
        let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(w!("shell:AppsFolder"), None) else { return out };
        let Ok(en) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else { return out };
        loop {
            let mut items = [None];
            let mut got = 0u32;
            if en.Next(&mut items, Some(&mut got)).is_err() || got == 0 {
                break;
            }
            let Some(item) = items[0].take() else { break };
            let (Some(name), Some(parse)) = (display_name(&item, SIGDN_NORMALDISPLAY), display_name(&item, SIGDN_PARENTRELATIVEPARSING)) else { continue };
            if parse.contains('!') && !parse.contains('\\') && !name.is_empty() {
                out.push((name, parse));
            }
            if out.len() > 2000 {
                break;
            }
        }
    }
    out
}

fn display_name(item: &IShellItem, sigdn: SIGDN) -> Option<String> {
    unsafe {
        let p: PWSTR = item.GetDisplayName(sigdn).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_dropped() {
        assert!(is_noise("Uninstall Foo"));
        assert!(is_noise("Foo ReadMe"));
        assert!(!is_noise("Visual Studio Code"));
    }

    /// Real Start Menu enumeration with icons (reads only; launches nothing).
    #[test]
    #[ignore = "reads this PC's Start Menu"]
    fn enumerate_this_pc() {
        let t = std::time::Instant::now();
        let v = build(true);
        eprintln!("{} entries with {} icons in {:?}", v.len(), v.iter().filter(|r| r.icon.is_some()).count(), t.elapsed());
        assert!(!v.is_empty());
    }
}
