//! The island's side of the launcher's app index: the cache file the helper process wrote
//! (names, targets, 48 px icons; see `crate::helper`), the entries built from it, and launching.
//! Nothing here enumerates the Start Menu or extracts icons: that is the helper's job, so the
//! shell's heavy libraries never load in the island because of the launcher's index.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::GetProcessId;
use windows::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use super::matcher::Prepared;
use crate::helper::icon::Com;
use crate::helper::pack::{self, Blob, IconRef, Kind};
use crate::ui::Image;

/// What an entry starts.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A Start Menu shortcut (`.lnk`), opened like Explorer would.
    Link(PathBuf),
    /// A packaged app by its AppUserModelID (`shell:AppsFolder\<AUMID>`).
    App(String),
    /// A file (recent Shelf files), opened with its default app.
    File(String),
}

impl Target {
    /// Shell parsing name for launching.
    pub(super) fn parsing_name(&self) -> String {
        match self {
            Target::Link(p) => p.to_string_lossy().into_owned(),
            Target::App(a) => format!("shell:AppsFolder\\{a}"),
            Target::File(f) => f.clone(),
        }
    }
}

/// One launchable app.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Stable id for the usage history and the icon cache.
    pub id: String,
    pub name: String,
    /// Subtitle: the program's file name.
    pub detail: Option<String>,
    pub target: Target,
    pub prep: Prepared,
    /// Where the 48 px icon is in the cache buffer.
    pub(super) icon: Option<IconRef>,
}

/// The apps of one cache file. Holds the file's buffer (a few MB with icons) until dropped.
pub struct Apps {
    pub entries: Vec<Entry>,
    blob: Blob,
}

impl Apps {
    pub fn from_pack(idx: pack::Index) -> Apps {
        let entries = idx
            .recs
            .into_iter()
            .filter(|r| !r.name.is_empty() && !r.target.is_empty())
            .map(|r| {
                let (id, target) = match r.kind {
                    Kind::Link => (format!("lnk:{}", r.target.to_lowercase()), Target::Link(PathBuf::from(&r.target))),
                    Kind::App => (format!("app:{}", r.target.to_lowercase()), Target::App(r.target.clone())),
                };
                Entry { id, prep: Prepared::new(&r.name), name: r.name, detail: Some(r.detail).filter(|d| !d.is_empty()), target, icon: r.icon }
            })
            .collect();
        Apps { entries, blob: idx.blob }
    }

    /// The icon of `e` shrunk to `px` x `px` (48 px source: the box filter keeps it crisp).
    pub fn image(&self, e: &Entry, px: u32) -> Option<Image> {
        let r = e.icon.as_ref()?;
        let (w, h, bgra) = pack::downscale(self.blob.bgra(r), r.w as u32, r.h as u32, px, px);
        Some(Image::new(w, h, bgra))
    }
}

/// Read the cache file the helper wrote. `None` when it is missing, damaged or from another version.
pub fn load(path: &Path) -> Option<Apps> {
    crate::helper::read_index(path).map(Apps::from_pack)
}

/// The cache file in the data folder.
pub fn cache_path() -> PathBuf {
    moku_core::paths::data_file("launcher-index.bin")
}

/// What Enter does with a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Start it like Explorer would (shortcut, packaged app, a document).
    Open,
    /// Only show it in Explorer: a program or script that was found in the Shelf is never run
    /// from the launcher (the same rule as the downloads pill).
    Reveal,
}

pub fn plan(target: &Target) -> Plan {
    match target {
        Target::File(f) if crate::safety::is_risky(f) => Plan::Reveal,
        _ => Plan::Open,
    }
}

/// What `launch` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Launched {
    pub ok: bool,
    /// The process the shell started, when it started one (so only that process is allowed to
    /// take the foreground). `None` for a document an existing program opens, a packaged app,
    /// a reveal in Explorer.
    pub pid: Option<u32>,
}

/// Open `target` like Explorer would (shortcut, packaged app, a document), or only show a risky
/// file in its folder ([`plan`]). Blocking: worker thread.
pub fn launch(target: &Target) -> Launched {
    if plan(target) == Plan::Reveal {
        return Launched { ok: crate::safety::reveal(Path::new(&target.parsing_name())), pid: None };
    }
    let _com = Com::init();
    let file = HSTRING::from(target.parsing_name());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        let ok = ShellExecuteExW(&mut info).is_ok();
        let mut pid = None;
        if !info.hProcess.is_invalid() {
            pid = Some(GetProcessId(info.hProcess)).filter(|p| *p != 0);
            let _ = CloseHandle(info.hProcess);
        }
        Launched { ok, pid }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::pack::{Icon, Rec, encode_index};

    #[test]
    fn a_risky_shelf_file_is_revealed_never_opened() {
        for f in [r"C:\Users\x\Desktop\setup.exe", r"D:\tools\run.ps1", r"E:\a\b.LNK", r"E:\a\b.hta"] {
            assert_eq!(plan(&Target::File(f.into())), Plan::Reveal, "{f}");
        }
        for f in [r"C:\docs\report.pdf", r"C:\pics\a.png", r"C:\docs\folder"] {
            assert_eq!(plan(&Target::File(f.into())), Plan::Open, "{f}");
        }
        // Shortcuts and packaged apps are what the launcher is for.
        assert_eq!(plan(&Target::Link(r"C:\Start Menu\Tool.lnk".into())), Plan::Open);
        assert_eq!(plan(&Target::App("Pkg_x!App".into())), Plan::Open);
    }

    #[test]
    fn entries_come_from_the_cache_with_stable_ids_and_icons() {
        let recs = vec![
            Rec { kind: Kind::Link, name: "Visual Studio Code".into(), detail: "Code.exe".into(), target: r"C:\Start\Visual Studio Code.lnk".into(), icon: Some(Icon { w: 48, h: 48, bgra: vec![5; 48 * 48 * 4] }) },
            Rec { kind: Kind::App, name: "Kalkulator".into(), detail: String::new(), target: "Microsoft.Calc_x!App".into(), icon: None },
            Rec { kind: Kind::Link, name: String::new(), detail: String::new(), target: "broken.lnk".into(), icon: None },
        ];
        let apps = Apps::from_pack(pack::Index::decode(encode_index(&recs)).expect("decodes"));
        assert_eq!(apps.entries.len(), 2, "a record without a name is dropped");
        // The ids are what launcher.json already stores: keep their format.
        assert_eq!(apps.entries[0].id, r"lnk:c:\start\visual studio code.lnk");
        assert_eq!(apps.entries[1].id, "app:microsoft.calc_x!app");
        assert_eq!((apps.entries[0].detail.as_deref(), apps.entries[1].detail.as_deref()), (Some("Code.exe"), None));
        assert_eq!(apps.entries[1].target, Target::App("Microsoft.Calc_x!App".into()));
        let img = apps.image(&apps.entries[0], 24).expect("icon");
        assert_eq!((img.0.width, img.0.height), (24, 24), "shrunk to the size it is drawn at");
        assert!(img.0.bgra.iter().all(|&b| b == 5));
        assert_eq!(apps.image(&apps.entries[0], 64).unwrap().0.width, 48, "never enlarged");
        assert!(apps.image(&apps.entries[1], 24).is_none());
    }
}
