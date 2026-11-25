//! Helper-process modes of `moku.exe`. The island never loads the shell's heavy libraries
//! (Start Menu shortcut resolution, `shell:AppsFolder`, icon extraction pull in windows.storage,
//! the image factory, thumbnail handlers ...) itself: it starts itself again with one of these
//! flags, reads the small file the helper wrote, and the helper's memory goes away with it.
//!
//! * `moku.exe --launcher-index <out> [--no-store-apps] [--sample]`: resolves the Start Menu,
//!   enumerates packaged apps, extracts 48 px icons and writes the launcher's cache file
//!   ([`pack`]); `--sample` (the island passes it only with `MOKU_DEBUG=1` and
//!   `MOKU_LAUNCHER_SAMPLE=1`) writes a fixed list of invented apps instead.
//! * `moku.exe --icons <list> <out>`: the shell icons of the executables / files named in `<list>`
//!   (UTF-8, one path per line) for the mixer and the launcher's Shelf files.
//!
//! The helper is started below normal priority without a window, with a time limit
//! ([`run_index`], [`run_icons`] kill it when it hangs on a broken shell extension), and exits
//! when done. It shows nothing and logs nothing; the island logs the result.

pub mod icon;
pub mod pack;
pub mod sample;

use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use self::pack::{IconSet, Index};

pub const INDEX_FLAG: &str = "--launcher-index";
pub const ICONS_FLAG: &str = "--icons";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
/// The most paths one `--icons` call looks at.
pub const MAX_ICON_PATHS: usize = 64;

/// Called first thing in `main`: run as a helper when the command line says so. `Some(exit code)`
/// means "this process was a helper, exit with that".
pub fn run_if_requested() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some(INDEX_FLAG) => Some(index_main(&args[1..])),
        Some(ICONS_FLAG) => Some(icons_main(&args[1..])),
        _ => None,
    }
}

fn index_main(args: &[String]) -> i32 {
    let Some(out) = args.first().filter(|a| !a.starts_with("--")) else { return 2 };
    let store_apps = !args.iter().any(|a| a == "--no-store-apps");
    let recs = if args.iter().any(|a| a == "--sample") { sample::recs() } else { crate::features::launcher::build_index(store_apps) };
    match pack::write_atomic(Path::new(out), &pack::encode_index(&recs)) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn icons_main(args: &[String]) -> i32 {
    let (Some(list), Some(out)) = (args.first(), args.get(1)) else { return 2 };
    let Ok(text) = std::fs::read_to_string(list) else { return 1 };
    let _com = icon::Com::init();
    let items: Vec<(String, Option<pack::Icon>)> = text.lines().map(str::trim).filter(|l| !l.is_empty()).take(MAX_ICON_PATHS).map(|p| (p.to_string(), icon::shell_icon(p))).collect();
    match pack::write_atomic(Path::new(out), &pack::encode_icons(&items)) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

// ---- the island's side ---------------------------------------------------------------------------

fn spawn(args: &[OsString]) -> std::io::Result<Child> {
    Command::new(std::env::current_exe()?).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS).spawn()
}

/// Wait for the helper, at most `limit`; a helper that takes longer is killed. True = it exited
/// with code 0.
fn wait_bounded(child: &mut Child, limit: Duration) -> bool {
    let end = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(40)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Start the indexing helper and wait for it (a worker thread's job). `Err` says why not.
pub fn run_index(out: &Path, store_apps: bool, sample: bool, limit: Duration) -> Result<(), String> {
    let mut args: Vec<OsString> = vec![INDEX_FLAG.into(), out.as_os_str().to_owned()];
    if !store_apps {
        args.push("--no-store-apps".into());
    }
    if sample {
        args.push("--sample".into());
    }
    let mut child = spawn(&args).map_err(|e| format!("cannot start the helper: {e}"))?;
    if wait_bounded(&mut child, limit) { Ok(()) } else { Err("the helper failed or took too long".into()) }
}

/// Read a cache file written by [`run_index`]; `None` when it is missing or damaged.
pub fn read_index(path: &Path) -> Option<Index> {
    Index::decode(std::fs::read(path).ok()?)
}

static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Shell icons for `paths` through a helper process (a worker thread's job). Paths the shell
/// has no icon for come back with `None`; the whole call is `None` when the helper fails.
pub fn run_icons(paths: &[String], limit: Duration) -> Option<IconSet> {
    if paths.is_empty() {
        return None;
    }
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let base: PathBuf = std::env::temp_dir().join(format!("moku-icons-{}-{n}", std::process::id()));
    let (list, out) = (base.with_extension("lst"), base.with_extension("bin"));
    let text: String = paths.iter().take(MAX_ICON_PATHS).filter(|p| !p.contains(['\n', '\r'])).map(|p| format!("{p}\n")).collect();
    let result = (|| {
        std::fs::write(&list, text).ok()?;
        let mut child = spawn(&[ICONS_FLAG.into(), list.as_os_str().to_owned(), out.as_os_str().to_owned()]).ok()?;
        if !wait_bounded(&mut child, limit) {
            return None;
        }
        IconSet::decode(std::fs::read(&out).ok()?)
    })();
    let _ = std::fs::remove_file(&list);
    let _ = std::fs::remove_file(&out);
    result
}
