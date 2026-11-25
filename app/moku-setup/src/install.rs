//! Install and uninstall, over a [`Layout`] (so tests use a temp folder and a test registry key).
//!
//! Install is atomic-ish:
//! 1. every file is written (decompressed) into a staging folder next to the program folder,
//! 2. the Moku processes running *from the program folder* are stopped,
//! 3. the old folder is renamed aside, the staging folder renamed into place (rolled back if that
//!    fails), the old folder deleted,
//! 4. shortcut, uninstall entry and the autostart value are written.
//!
//! A failure before step 3 leaves the previous installation untouched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use moku_core::install::{ISLAND_EXE, Layout, PUBLISHER, SETUP_EXE, path_is_within};

use crate::payload::PayloadFile;
use crate::reg::{self, Value};
use crate::{procs, shortcut};

/// How the autostart value is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Autostart {
    On,
    Off,
    /// Leave it as it is (`/update`).
    Keep,
}

pub struct InstallOptions {
    pub autostart: Autostart,
    /// Copied into the folder as `moku-setup.exe` (the uninstaller). `None` skips it (tests).
    pub setup_exe: Option<PathBuf>,
    /// How long running Moku processes get to quit before they are terminated.
    pub stop_timeout: Duration,
}

pub struct UninstallOptions {
    /// Also delete the user data folder (`Layout::data_dir`). Asked in the window; `/deletedata`
    /// when silent.
    pub delete_data: bool,
    pub stop_timeout: Duration,
}

/// Progress: fraction 0–1 and a short Indonesian status line.
pub type Progress<'a> = &'a dyn Fn(f32, &str);

/// Is there an installation at `layout` (the program folder has `moku.exe`)?
pub fn installed_version(layout: &Layout) -> Option<String> {
    if !layout.exe(ISLAND_EXE).is_file() {
        return None;
    }
    Some(reg::get_string(&layout.uninstall_key, "DisplayVersion").unwrap_or_default())
}

fn staging_dirs(layout: &Layout) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let parent = layout.install_dir.parent().ok_or("the install folder has no parent")?.to_path_buf();
    let pid = std::process::id();
    Ok((parent.join(format!("Moku.new-{pid}")), parent.join(format!("Moku.old-{pid}")), parent))
}

/// The process id in a folder name `Moku.new-<pid>` / `Moku.old-<pid>` (what an install creates
/// next to the program folder); `None` for any other name.
fn leftover_pid(name: &str) -> Option<u32> {
    let pid = name.strip_prefix("Moku.new-").or_else(|| name.strip_prefix("Moku.old-"))?;
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

/// Leftovers of an interrupted run (`Moku.new-<pid>`, `Moku.old-<pid>`) next to the program
/// folder. A folder whose process is still running belongs to ANOTHER setup that is installing
/// right now (two setups started at once): it is left alone. So is anything not named exactly
/// like that.
fn remove_leftovers(parent: &Path) {
    let Ok(rd) = std::fs::read_dir(parent) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(pid) = leftover_pid(&name) else { continue };
        if e.path().is_dir() && !procs::is_alive(pid) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Is `dir` an existing Moku installation, or at least free to be taken over? The program folder
/// is renamed aside and deleted when an install replaces it, so it must be Moku's: it has
/// `moku.exe` (or the uninstaller), or our uninstall entry names it, or it is empty.
fn is_ours_or_free(layout: &Layout) -> bool {
    let dir = &layout.install_dir;
    if !dir.exists() {
        return true;
    }
    if layout.exe(ISLAND_EXE).is_file() || layout.exe(SETUP_EXE).is_file() {
        return true;
    }
    let keyed = reg::get_string(&layout.uninstall_key, "InstallLocation").is_some_and(|p| {
        let p = Path::new(p.trim().trim_matches('"'));
        path_is_within(p, dir) && path_is_within(dir, p)
    });
    keyed || std::fs::read_dir(dir).is_ok_and(|mut r| r.next().is_none())
}

fn not_ours_message(dir: &Path) -> String {
    format!("Folder {} sudah ada, tetapi bukan instalasi Moku (tidak ada moku.exe di dalamnya). Pemasang tidak menimpanya. Pindahkan atau hapus folder itu, lalu coba lagi.", dir.display())
}

/// `std::fs::rename`, retried for a moment (a just-stopped process can hold a handle briefly;
/// antivirus scanners open new exes).
fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last = None;
    for i in 0..20 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(50 * (i + 1).min(6)));
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("rename failed")))
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() { dir_size(&p) } else { e.metadata().map(|m| m.len()).unwrap_or(0) }
        })
        .sum()
}

/// Install `files` into `layout`.
pub fn install(layout: &Layout, files: &[PayloadFile], opts: &InstallOptions, progress: Progress) -> Result<(), String> {
    if !files.iter().any(|f| f.path.eq_ignore_ascii_case(ISLAND_EXE)) {
        return Err("Pemasang ini tidak berisi Moku (dibuat tanpa payload). Buat dengan tools/release.ps1.".into());
    }
    // A folder that is not Moku's is never replaced (the swap below deletes the old one).
    if !is_ours_or_free(layout) {
        return Err(not_ours_message(&layout.install_dir));
    }
    let (staging, old, parent) = staging_dirs(layout)?;
    std::fs::create_dir_all(&parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    remove_leftovers(&parent);

    // 1. Stage.
    progress(0.02, "Menyiapkan berkas…");
    let staged = (|| -> Result<(), String> {
        std::fs::create_dir_all(&staging).map_err(|e| format!("create {}: {e}", staging.display()))?;
        let total: u64 = files.iter().map(|f| f.size).sum::<u64>().max(1);
        let mut done = 0u64;
        for f in files {
            let rel = Path::new(f.path);
            if rel.is_absolute() || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
                return Err(format!("bad payload path {}", f.path));
            }
            let dest = staging.join(rel);
            if let Some(d) = dest.parent() {
                std::fs::create_dir_all(d).map_err(|e| format!("create {}: {e}", d.display()))?;
            }
            let bytes = f.bytes()?;
            std::fs::write(&dest, &bytes).map_err(|e| format!("write {}: {e}", dest.display()))?;
            done += f.size;
            progress(0.05 + 0.65 * done as f32 / total as f32, &format!("Menyalin {}…", f.path));
        }
        if let Some(exe) = &opts.setup_exe {
            std::fs::copy(exe, staging.join(SETUP_EXE)).map_err(|e| format!("copy the uninstaller: {e}"))?;
        }
        Ok(())
    })();
    if let Err(e) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    // 2. Stop Moku running from the folder.
    progress(0.74, "Menutup Moku yang sedang berjalan…");
    if let Err(e) = procs::stop_within(&layout.install_dir, opts.stop_timeout) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    // 3. Swap.
    progress(0.8, "Memasang…");
    let had_old = layout.install_dir.exists();
    if had_old {
        if let Err(e) = rename_retry(&layout.install_dir, &old) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!("Folder {} sedang dipakai ({e}). Tutup Moku lalu coba lagi.", layout.install_dir.display()));
        }
    }
    if let Err(e) = rename_retry(&staging, &layout.install_dir) {
        if had_old {
            let _ = rename_retry(&old, &layout.install_dir);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("cannot move the new files into place: {e}"));
    }
    if had_old && std::fs::remove_dir_all(&old).is_err() {
        crate::log(&format!("left {} behind (removed on the next run)", old.display()));
    }

    // 4. Shortcut, uninstall entry, autostart.
    progress(0.9, "Membuat pintasan…");
    let island = layout.exe(ISLAND_EXE);
    shortcut::create(&layout.shortcut, &island, &layout.install_dir, "Moku: dynamic island untuk Windows")?;
    write_uninstall_entry(layout)?;
    match opts.autostart {
        Autostart::On => reg::write(&layout.run_key, &[(moku_core::install::APP_KEY, Value::Str(&layout.run_command()))])?,
        Autostart::Off => remove_run_value(layout)?,
        Autostart::Keep => {}
    }
    progress(1.0, "Selesai");
    Ok(())
}

fn write_uninstall_entry(layout: &Layout) -> Result<(), String> {
    let dir = layout.install_dir.display().to_string();
    let icon = format!("\"{}\",0", layout.exe(ISLAND_EXE).display());
    let kb = (dir_size(&layout.install_dir) / 1024).min(u32::MAX as u64) as u32;
    let date = chrono_free_date();
    let about = moku_core::release::repo_url(moku_core::release::REPO);
    let mut values = vec![
        ("DisplayName", Value::Str("Moku")),
        ("DisplayVersion", Value::Str(moku_core::VERSION)),
        ("Publisher", Value::Str(PUBLISHER)),
        ("DisplayIcon", Value::Str(&icon)),
        ("InstallLocation", Value::Str(&dir)),
        ("InstallDate", Value::Str(&date)),
        ("EstimatedSize", Value::Dword(kb)),
        ("NoModify", Value::Dword(1)),
        ("NoRepair", Value::Dword(1)),
    ];
    let (u, q) = (layout.uninstall_command(false), layout.uninstall_command(true));
    values.push(("UninstallString", Value::Str(&u)));
    values.push(("QuietUninstallString", Value::Str(&q)));
    if !moku_core::release::is_placeholder(moku_core::release::REPO) {
        values.push(("URLInfoAbout", Value::Str(&about)));
    }
    reg::write(&layout.uninstall_key, &values)
}

/// `YYYYMMDD` (UTC) without a date crate.
fn chrono_free_date() -> String {
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}")
}

/// Remove the `Moku` Run value, but only when it starts this installation (a value pointing at
/// another copy belongs to that copy).
fn remove_run_value(layout: &Layout) -> Result<(), String> {
    if let Some(v) = reg::get_string(&layout.run_key, moku_core::install::APP_KEY) {
        let path = PathBuf::from(v.trim().trim_matches('"'));
        if path_is_within(&path, &layout.install_dir) {
            reg::delete_value(&layout.run_key, moku_core::install::APP_KEY)?;
        }
    }
    Ok(())
}

/// Delete the user data (`--deletedata`, "also delete my data" in the window). Only Moku's own
/// files and subfolders go (`moku_core::paths::is_moku_entry`: config.toml, todo.json,
/// water.json, launcher.json, the launcher cache, update.json, capture.json, the logs /
/// clipboard / shelf folders ...); anything else in the folder stays, so a `MOKU_DATA_DIR`
/// that points at a folder with other things in it loses nothing but Moku's files. The folder
/// itself (and its marker) is removed only when nothing else is left in it. A folder without the
/// `.moku-data` marker is refused: nothing in it is touched.
pub fn delete_data(dir: &Path) -> Result<(), String> {
    use moku_core::paths::{DATA_MARKER, check_data_dir_safe, wipe_known_entries};
    if !dir.exists() {
        return Ok(());
    }
    check_data_dir_safe(dir).map_err(|e| format!("Data tidak dihapus: {e}"))?;
    if !dir.join(DATA_MARKER).is_file() {
        return Err(format!("Data tidak dihapus: {} tidak bertanda sebagai folder data Moku (berkas {DATA_MARKER} tidak ada).", dir.display()));
    }
    let w = wipe_known_entries(dir, &[DATA_MARKER]).map_err(|e| format!("delete {}: {e}", dir.display()))?;
    if w.failed > 0 {
        return Err(format!("{} berkas data masih dipakai dan tidak bisa dihapus ({}).", w.failed, dir.display()));
    }
    if w.foreign > 0 {
        crate::log(&format!("left {} entries in the data folder that are not Moku's", w.foreign));
        return Ok(());
    }
    // Nothing foreign: the marker goes, and the folder when it is empty (remove_dir refuses
    // a folder that still has something in it).
    let _ = std::fs::remove_file(dir.join(DATA_MARKER));
    let _ = std::fs::remove_dir(dir);
    Ok(())
}

/// Remove Moku: processes, shortcut, autostart value, uninstall entry, program folder, and the
/// data folder when asked.
pub fn uninstall(layout: &Layout, opts: &UninstallOptions, progress: Progress) -> Result<(), String> {
    // Never delete a folder that is not Moku's, whatever got us here (a stray --uninstall, an
    // install folder that was replaced by something else).
    if !is_ours_or_free(layout) {
        return Err(not_ours_message(&layout.install_dir));
    }
    progress(0.05, "Menutup Moku…");
    procs::stop_within(&layout.install_dir, opts.stop_timeout)?;

    progress(0.3, "Menghapus pintasan dan entri…");
    if layout.shortcut.exists() {
        std::fs::remove_file(&layout.shortcut).map_err(|e| format!("delete {}: {e}", layout.shortcut.display()))?;
    }
    remove_run_value(layout)?;
    reg::delete_tree(&layout.uninstall_key)?;

    progress(0.5, "Menghapus berkas…");
    remove_program_folder(&layout.install_dir)?;

    if opts.delete_data {
        progress(0.85, "Menghapus data…");
        delete_data(&layout.data_dir)?;
    }
    progress(1.0, "Selesai");
    Ok(())
}

/// Delete the program folder. The running uninstaller may be inside it (Apps & Features starts
/// `<folder>\moku-setup.exe --uninstall`): a running exe cannot be deleted, but it can be moved
/// on the same volume, so it goes to `%TEMP%` and is deleted from there after this process
/// exits. When `%TEMP%` is on another volume, everything else is deleted now and the exe plus the
/// then empty folder right after exit.
fn remove_program_folder(dir: &Path) -> Result<(), String> {
    if !dir.exists() {
        return Ok(());
    }
    let err = |e: std::io::Error| format!("delete {}: {e}", dir.display());
    let Some(me) = std::env::current_exe().ok().filter(|m| path_is_within(m, dir)) else {
        return std::fs::remove_dir_all(dir).map_err(err);
    };
    let tmp = std::env::temp_dir().join(format!("moku-setup-uninstall-{}.exe", std::process::id()));
    if std::fs::rename(&me, &tmp).is_ok() {
        let r = std::fs::remove_dir_all(dir).map_err(err);
        delete_after_exit(&tmp, None);
        return r;
    }
    let same = |p: &Path| p.to_string_lossy().eq_ignore_ascii_case(&me.to_string_lossy());
    for e in std::fs::read_dir(dir).map_err(err)?.flatten() {
        let p = e.path();
        if same(&p) {
            continue;
        }
        (if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) }).map_err(|e| format!("delete {}: {e}", p.display()))?;
    }
    delete_after_exit(&me, Some(dir));
    Ok(())
}

/// Files left for after exit: (file, folder to remove once empty).
static AFTER_EXIT: std::sync::Mutex<Vec<(PathBuf, Option<PathBuf>)>> = std::sync::Mutex::new(Vec::new());

/// Delete `file` (and then the empty folder `dir`) once this process has exited; see
/// [`run_after_exit`].
fn delete_after_exit(file: &Path, dir: Option<&Path>) {
    if let Ok(mut v) = AFTER_EXIT.lock() {
        v.push((file.to_path_buf(), dir.map(Path::to_path_buf)));
    }
}

/// Environment variables that carry the paths to the cleanup command (see [`cleanup_script`]).
const CLEANUP_FILE_VAR: &str = "MOKU_CLEANUP_FILE";
const CLEANUP_DIR_VAR: &str = "MOKU_CLEANUP_DIR";

/// `%SystemRoot%\System32` (what `windir` / `SystemRoot` say, else `C:\Windows`).
fn system32() -> PathBuf {
    let root = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("windir")).filter(|r| !r.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32")
}

/// What the delayed cleanup runs (`cmd /d /c <this>`): wait, delete the file, then remove the
/// folder if it is empty (`rmdir` without `/s` refuses anything else).
///
/// The paths are NOT in this text. cmd expands `%NAME%` once and never looks at the result again,
/// so the file and folder come from two environment variables: a path with `%`, `&`, `^` or
/// parentheses in it (a user or folder named `100%`, `a&b`) cannot change what the command does
/// or delete something else.
fn cleanup_script(with_dir: bool, pings: u32) -> String {
    let mut s = format!("ping -n {pings} 127.0.0.1 >nul & del /f /q \"%{CLEANUP_FILE_VAR}%\"");
    if with_dir {
        s.push_str(&format!(" & rmdir \"%{CLEANUP_DIR_VAR}%\""));
    }
    s
}

/// Start the cleanup for one file: the system's own `cmd.exe` by full path (never one found on
/// `PATH` or next to us), run from System32.
fn spawn_cleanup(file: &Path, dir: Option<&Path>, pings: u32) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let sys = system32();
    let mut cmd = std::process::Command::new(sys.join("cmd.exe"));
    cmd.current_dir(&sys).env(CLEANUP_FILE_VAR, file).raw_arg("/d /c").raw_arg(cleanup_script(dir.is_some(), pings)).creation_flags(CREATE_NO_WINDOW);
    if let Some(d) = dir {
        cmd.env(CLEANUP_DIR_VAR, d);
    }
    cmd.spawn()
}

/// Call right before the process exits: asks `cmd` to delete what [`delete_after_exit`] queued a
/// few seconds later (`rmdir` without `/s` only removes an empty folder).
pub fn run_after_exit() {
    let Ok(v) = AFTER_EXIT.lock() else { return };
    for (file, dir) in v.iter() {
        let _ = spawn_cleanup(file, dir.as_deref(), 4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const TEST_REG: &str = r"Software\MokuSetupTest";

    /// A temp root and a test registry root, both removed on drop. Never the real locations.
    struct Sandbox {
        root: PathBuf,
        reg_root: String,
        layout: Layout,
    }

    impl Sandbox {
        fn new(name: &str) -> Sandbox {
            let root = std::env::temp_dir().join(format!("moku-setup-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            // The long form: %TEMP% can be an 8.3 short path (C:\Users\RUNNER~1\... on GitHub's
            // runners) while a shortcut's target reads back as the long one.
            let long = std::fs::canonicalize(&root).unwrap();
            let root = PathBuf::from(long.to_string_lossy().strip_prefix(r"\\?\").unwrap_or(&long.to_string_lossy()).to_string());
            let reg_root = format!(r"{TEST_REG}\{name}-{}", std::process::id());
            let env = |k: &str| match k {
                moku_core::install::INSTALL_ROOT_ENV => Some(root.display().to_string()),
                moku_core::install::REG_ROOT_ENV => Some(reg_root.clone()),
                _ => None,
            };
            let layout = Layout::resolve(env, root.join("AppData").join("Moku"));
            assert!(layout.overridden);
            assert!(layout.install_dir.starts_with(&root));
            assert!(layout.shortcut.starts_with(&root));
            assert!(layout.uninstall_key.starts_with(TEST_REG) && layout.run_key.starts_with(TEST_REG));
            Sandbox { root, reg_root, layout }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            assert!(self.reg_root.starts_with(TEST_REG));
            let _ = reg::delete_tree(&self.reg_root);
            // The shared parent goes once it is empty (RegDeleteKeyW refuses keys with subkeys,
            // so a test still running in parallel keeps it).
            unsafe {
                let _ = windows::Win32::System::Registry::RegDeleteKeyW(windows::Win32::System::Registry::HKEY_CURRENT_USER, &windows::core::HSTRING::from(TEST_REG));
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn payload() -> Vec<PayloadFile> {
        let leak = |v: Vec<u8>| -> &'static [u8] { Box::leak(v.into_boxed_slice()) };
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let packed = crate::payload::compress(&big);
        vec![
            PayloadFile { path: "moku.exe", size: 200_000, compressed: true, data: leak(packed) },
            PayloadFile { path: "moku-settings.exe", size: 8, compressed: false, data: b"settings" },
            PayloadFile { path: "mokuctl.exe", size: 3, compressed: false, data: b"ctl" },
            PayloadFile { path: r"docs\claude-code-hook.md", size: 6, compressed: false, data: b"# hook" },
        ]
    }

    fn opts(autostart: Autostart, setup: Option<PathBuf>) -> InstallOptions {
        InstallOptions { autostart, setup_exe: setup, stop_timeout: Duration::from_secs(2) }
    }

    fn quiet(_: f32, _: &str) {}

    #[test]
    fn install_and_uninstall_in_a_sandbox() {
        let sb = Sandbox::new("full");
        let l = &sb.layout;
        let fake_setup = sb.root.join("downloaded-setup.exe");
        std::fs::write(&fake_setup, b"setup").unwrap();
        assert_eq!(installed_version(l), None);

        let steps = RefCell::new(vec![]);
        install(l, &payload(), &opts(Autostart::On, Some(fake_setup.clone())), &|f, s| steps.borrow_mut().push((f, s.to_string()))).unwrap();
        let steps = steps.into_inner();
        assert!(steps.windows(2).all(|w| w[0].0 <= w[1].0), "progress only goes up");
        assert_eq!(steps.last().map(|s| s.0), Some(1.0));

        // Every file, with the right content.
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(std::fs::read(l.exe("moku.exe")).unwrap(), big, "decompressed");
        assert_eq!(std::fs::read(l.exe("moku-settings.exe")).unwrap(), b"settings");
        assert_eq!(std::fs::read(l.exe("mokuctl.exe")).unwrap(), b"ctl");
        assert_eq!(std::fs::read(l.install_dir.join("docs").join("claude-code-hook.md")).unwrap(), b"# hook");
        assert_eq!(std::fs::read(l.exe("moku-setup.exe")).unwrap(), b"setup", "uninstaller copied in");
        // No staging leftovers.
        let parent = l.install_dir.parent().unwrap();
        let names: Vec<String> = std::fs::read_dir(parent).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(names, vec!["Moku".to_string()]);

        // Shortcut to moku.exe.
        assert!(l.shortcut.is_file());
        assert_eq!(shortcut::target(&l.shortcut).unwrap(), l.exe("moku.exe"));

        // Uninstall entry.
        let k = &l.uninstall_key;
        assert_eq!(reg::get_string(k, "DisplayName").as_deref(), Some("Moku"));
        assert_eq!(reg::get_string(k, "DisplayVersion").as_deref(), Some(moku_core::VERSION));
        assert_eq!(reg::get_string(k, "Publisher").as_deref(), Some("Moku contributors"));
        assert_eq!(reg::get_string(k, "DisplayIcon"), Some(format!("\"{}\",0", l.exe("moku.exe").display())));
        assert_eq!(reg::get_string(k, "InstallLocation"), Some(l.install_dir.display().to_string()));
        assert_eq!(reg::get_string(k, "UninstallString"), Some(format!("\"{}\" --uninstall", l.exe("moku-setup.exe").display())));
        assert_eq!(reg::get_string(k, "QuietUninstallString"), Some(format!("\"{}\" --uninstall /S", l.exe("moku-setup.exe").display())));
        assert_eq!(reg::get_dword(k, "NoModify"), Some(1));
        assert_eq!(reg::get_dword(k, "NoRepair"), Some(1));
        assert_eq!(reg::get_dword(k, "EstimatedSize"), Some(((200_000 + 8 + 3 + 6 + 5) / 1024) as u32));
        assert_eq!(reg::get_string(k, "InstallDate").map(|d| d.len()), Some(8));
        assert_eq!(installed_version(l).as_deref(), Some(moku_core::VERSION));

        // Autostart value.
        assert_eq!(reg::get_string(&l.run_key, "Moku"), Some(l.run_command()));

        // Data folder (kept by default).
        moku_core::paths::ensure_marked_dir(&l.data_dir).unwrap();
        std::fs::write(l.data_dir.join("config.toml"), "x").unwrap();
        uninstall(l, &UninstallOptions { delete_data: false, stop_timeout: Duration::from_secs(2) }, &quiet).unwrap();
        assert!(!l.install_dir.exists(), "program folder gone");
        assert!(!l.shortcut.exists(), "shortcut gone");
        assert!(!reg::key_exists(&l.uninstall_key), "uninstall key gone");
        assert_eq!(reg::get_string(&l.run_key, "Moku"), None, "Run value gone");
        assert!(l.data_dir.join("config.toml").exists(), "user data kept unless asked");

        // Uninstalling again is harmless; with delete_data the data folder goes too.
        uninstall(l, &UninstallOptions { delete_data: true, stop_timeout: Duration::from_secs(2) }, &quiet).unwrap();
        assert!(!l.data_dir.exists());
    }

    #[test]
    fn reinstall_replaces_files_and_keeps_autostart_on_update() {
        let sb = Sandbox::new("update");
        let l = &sb.layout;
        install(l, &payload(), &opts(Autostart::Off, None), &quiet).unwrap();
        assert_eq!(reg::get_string(&l.run_key, "Moku"), None);
        // A stray file from the old version disappears (the folder is swapped as a whole).
        std::fs::write(l.install_dir.join("old-only.dll"), b"x").unwrap();
        // The user turned autostart on in the settings app; an update keeps it.
        reg::write(&l.run_key, &[("Moku", Value::Str(&l.run_command()))]).unwrap();
        let mut p = payload();
        p[1] = PayloadFile { path: "moku-settings.exe", size: 2, compressed: false, data: b"v2" };
        install(l, &p, &opts(Autostart::Keep, None), &quiet).unwrap();
        assert_eq!(std::fs::read(l.exe("moku-settings.exe")).unwrap(), b"v2");
        assert!(!l.install_dir.join("old-only.dll").exists());
        assert_eq!(reg::get_string(&l.run_key, "Moku"), Some(l.run_command()));
        // Turning it off only removes a value that points at this installation.
        reg::write(&l.run_key, &[("Moku", Value::Str(r#""D:\elsewhere\moku.exe""#))]).unwrap();
        install(l, &payload(), &opts(Autostart::Off, None), &quiet).unwrap();
        assert_eq!(reg::get_string(&l.run_key, "Moku").as_deref(), Some(r#""D:\elsewhere\moku.exe""#));
        uninstall(l, &UninstallOptions { delete_data: false, stop_timeout: Duration::from_secs(2) }, &quiet).unwrap();
        assert_eq!(reg::get_string(&l.run_key, "Moku").as_deref(), Some(r#""D:\elsewhere\moku.exe""#), "another copy's value is left alone");
    }

    #[test]
    fn empty_or_bad_payload_changes_nothing() {
        let sb = Sandbox::new("empty");
        let l = &sb.layout;
        assert!(install(l, &[], &opts(Autostart::On, None), &quiet).is_err());
        let evil = [
            PayloadFile { path: "moku.exe", size: 1, compressed: false, data: b"m" },
            PayloadFile { path: r"..\..\escape.txt", size: 1, compressed: false, data: b"x" },
        ];
        assert!(install(l, &evil, &opts(Autostart::On, None), &quiet).is_err());
        assert!(!l.install_dir.exists());
        assert!(!l.shortcut.exists());
        assert!(!reg::key_exists(&l.uninstall_key));
        assert!(!sb.root.join("escape.txt").exists());
        // No staging folder left behind.
        let parent = l.install_dir.parent().unwrap();
        assert!(std::fs::read_dir(parent).map(|r| r.count()).unwrap_or(0) == 0);
    }

    #[test]
    fn stops_only_processes_inside_the_folder() {
        let sb = Sandbox::new("procs");
        let l = &sb.layout;
        std::fs::create_dir_all(&l.install_dir).unwrap();
        let sys = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("System32").join("PING.EXE");
        // Same exe inside and outside the folder; only the inside one may be stopped.
        let inside = l.install_dir.join("moku.exe");
        let outside_dir = sb.root.join("elsewhere");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let outside = outside_dir.join("moku.exe");
        std::fs::copy(&sys, &inside).unwrap();
        std::fs::copy(&sys, &outside).unwrap();
        let spawn = |exe: &Path| {
            std::process::Command::new(exe)
                .args(["-n", "60", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut a = spawn(&inside);
        let mut b = spawn(&outside);
        std::thread::sleep(Duration::from_millis(300));
        let found: Vec<u32> = procs::find_within(&l.install_dir).iter().map(|f| f.pid).collect();
        assert_eq!(found, vec![a.id()]);
        assert_eq!(procs::stop_within(&l.install_dir, Duration::from_millis(500)).unwrap(), 1);
        assert!(a.try_wait().unwrap().is_some(), "inside: stopped");
        assert!(b.try_wait().unwrap().is_none(), "outside: still running");
        let _ = b.kill();
        let _ = b.wait();
    }

    #[test]
    fn a_folder_that_is_not_moku_is_never_replaced_or_uninstalled() {
        let sb = Sandbox::new("notours");
        let l = &sb.layout;
        std::fs::create_dir_all(&l.install_dir).unwrap();
        std::fs::write(l.install_dir.join("precious.txt"), "somebody else's").unwrap();
        // Install refuses before anything is touched.
        let e = install(l, &payload(), &opts(Autostart::On, None), &quiet).unwrap_err();
        assert!(e.contains("bukan instalasi Moku"), "{e}");
        assert_eq!(std::fs::read_to_string(l.install_dir.join("precious.txt")).unwrap(), "somebody else's");
        assert!(!l.shortcut.exists() && !reg::key_exists(&l.uninstall_key), "nothing was written");
        assert_eq!(std::fs::read_dir(l.install_dir.parent().unwrap()).unwrap().count(), 1, "no staging folder either");
        // Uninstall does not delete it either (a stray --uninstall).
        let e = uninstall(l, &UninstallOptions { delete_data: false, stop_timeout: Duration::from_secs(2) }, &quiet).unwrap_err();
        assert!(e.contains("bukan instalasi Moku"), "{e}");
        assert!(l.install_dir.join("precious.txt").exists());
    }

    #[test]
    fn an_empty_folder_or_one_our_uninstall_entry_names_may_be_taken_over() {
        // Empty: nobody's.
        let sb = Sandbox::new("emptydir");
        std::fs::create_dir_all(&sb.layout.install_dir).unwrap();
        install(&sb.layout, &payload(), &opts(Autostart::Off, None), &quiet).unwrap();
        assert!(sb.layout.exe("moku.exe").is_file());
        // A damaged installation (moku.exe gone) that our own uninstall entry still names.
        let sb = Sandbox::new("keyed");
        let l = &sb.layout;
        std::fs::create_dir_all(&l.install_dir).unwrap();
        std::fs::write(l.install_dir.join("leftover.dll"), "x").unwrap();
        assert!(install(l, &payload(), &opts(Autostart::Off, None), &quiet).is_err(), "unknown folder: refused");
        reg::write(&l.uninstall_key, &[("InstallLocation", Value::Str(&l.install_dir.display().to_string()))]).unwrap();
        install(l, &payload(), &opts(Autostart::Off, None), &quiet).unwrap();
        assert!(l.exe("moku.exe").is_file() && !l.install_dir.join("leftover.dll").exists());
        // An entry that names ANOTHER folder does not make this one ours.
        let sb = Sandbox::new("otherkey");
        let l = &sb.layout;
        std::fs::create_dir_all(&l.install_dir).unwrap();
        std::fs::write(l.install_dir.join("x.txt"), "x").unwrap();
        reg::write(&l.uninstall_key, &[("InstallLocation", Value::Str(r"D:\somewhere\else"))]).unwrap();
        assert!(install(l, &payload(), &opts(Autostart::Off, None), &quiet).is_err());
    }

    #[test]
    fn another_running_setups_staging_folder_is_left_alone() {
        let sb = Sandbox::new("leftovers");
        let l = &sb.layout;
        let parent = l.install_dir.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&parent).unwrap();
        let sys = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let ping = Path::new(&sys).join("System32").join("PING.EXE");
        // A "setup" that is running now, and one that is gone.
        let mut live = std::process::Command::new(&ping).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let mut dead = std::process::Command::new(&ping).args(["-n", "1", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let dead_pid = dead.id();
        dead.wait().unwrap();
        let mk = |name: String| {
            let d = parent.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("f"), "x").unwrap();
            d
        };
        let live_new = mk(format!("Moku.new-{}", live.id()));
        let live_old = mk(format!("Moku.old-{}", live.id()));
        let dead_new = mk(format!("Moku.new-{dead_pid}"));
        let dead_old = mk(format!("Moku.old-{dead_pid}"));
        let foreign = [mk("Moku.new-backup".into()), mk("Moku.new-".into()), mk("Moku.newer-5".into()), mk("Moku-docs".into())];
        install(l, &payload(), &opts(Autostart::Off, None), &quiet).unwrap();
        assert!(live_new.exists() && live_old.exists(), "a running setup's folders survive");
        assert!(!dead_new.exists() && !dead_old.exists(), "an interrupted run's folders are cleaned up");
        for f in &foreign {
            assert!(f.exists(), "{} is not ours", f.display());
        }
        let _ = live.kill();
        let _ = live.wait();
        // The id parser itself.
        assert_eq!(leftover_pid("Moku.new-123"), Some(123));
        assert_eq!(leftover_pid("Moku.old-7"), Some(7));
        for bad in ["Moku.new-", "Moku.new-12a", "Moku.new--1", "moku.new-5", "Moku", "Moku.old-99999999999"] {
            assert_eq!(leftover_pid(bad), None, "{bad}");
        }
        assert!(procs::is_alive(std::process::id()));
        assert!(!procs::is_alive(0));
    }

    /// The delayed cleanup is `%SystemRoot%\System32\cmd.exe` and the paths travel in the
    /// environment, so `%`, `&`, `^` and parentheses in a folder name change nothing: the exact
    /// file goes, the folder goes when empty, and a path that LOOKS like a variable is not expanded.
    #[test]
    fn the_cleanup_command_survives_hostile_paths() {
        assert!(system32().join("cmd.exe").is_file(), "the system's cmd.exe by full path");
        let script = cleanup_script(true, 4);
        assert!(script.contains("%MOKU_CLEANUP_FILE%") && script.contains("%MOKU_CLEANUP_DIR%"), "{script}");
        assert!(!script.contains(':') && !script.contains('\\'), "no path is spelled out in the command: {script}");

        let root = std::env::temp_dir().join(format!("moku-setup-cleanup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // A folder named like a variable, plus metacharacters; and a decoy where an expanded
        // %TEMP% would point if the path were spliced into the command text.
        let dir = root.join("a%TEMP%&b^c (1)");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("moku-setup-copy.exe");
        std::fs::write(&file, b"x").unwrap();
        let decoy_dir = root.join(format!("a{}&b^c (1)", std::env::temp_dir().display()).replace([':', '\\'], "_"));
        let _ = std::fs::create_dir_all(&decoy_dir);
        let decoy = decoy_dir.join("moku-setup-copy.exe");
        std::fs::write(&decoy, b"decoy").unwrap();
        let mut child = spawn_cleanup(&file, Some(&dir), 1).expect("cmd.exe starts");
        assert!(child.wait().unwrap().success() || !file.exists());
        assert!(!file.exists(), "the file was deleted");
        assert!(!dir.exists(), "and the now empty folder");
        assert!(decoy.exists(), "nothing else was touched");
        // A folder that is not empty stays (rmdir without /s).
        let keep = root.join("keep (2)");
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::write(keep.join("moku-setup-copy.exe"), b"x").unwrap();
        std::fs::write(keep.join("other.txt"), b"x").unwrap();
        spawn_cleanup(&keep.join("moku-setup-copy.exe"), Some(&keep), 1).unwrap().wait().unwrap();
        assert!(keep.join("other.txt").exists() && !keep.join("moku-setup-copy.exe").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A data folder as the island leaves it: its own files, folders and marker.
    fn moku_data(dir: &Path) {
        moku_core::paths::ensure_marked_dir(dir).unwrap();
        for f in ["config.toml", "todo.json", "water.json", "launcher.json", "launcher-index.bin", "update.json", "meeting-mute.flag"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::write(dir.join("logs").join("moku.log"), "x").unwrap();
        std::fs::create_dir_all(dir.join("clipboard")).unwrap();
        std::fs::write(dir.join("clipboard").join("index.json"), "{}").unwrap();
        std::fs::create_dir_all(dir.join("shelf")).unwrap();
        std::fs::write(dir.join("shelf").join("shelf.json"), "{}").unwrap();
    }

    #[test]
    fn deletedata_only_removes_moku_files_and_keeps_a_foreign_folder() {
        let sb = Sandbox::new("foreign");
        let l = &sb.layout;
        moku_data(&l.data_dir);
        // Somebody else's things in the same folder (MOKU_DATA_DIR pointed at a shared folder).
        std::fs::write(l.data_dir.join("my-notes.txt"), "keep me").unwrap();
        std::fs::create_dir_all(l.data_dir.join("holiday-photos")).unwrap();
        std::fs::write(l.data_dir.join("holiday-photos").join("a.jpg"), "keep me too").unwrap();
        install(l, &payload(), &opts(Autostart::Off, None), &quiet).unwrap();
        uninstall(l, &UninstallOptions { delete_data: true, stop_timeout: Duration::from_secs(2) }, &quiet).unwrap();
        // Everything of Moku's is gone ...
        for gone in ["config.toml", "todo.json", "water.json", "launcher.json", "launcher-index.bin", "update.json", "meeting-mute.flag", "logs", "clipboard", "shelf"] {
            assert!(!l.data_dir.join(gone).exists(), "{gone} should be deleted");
        }
        // ... the foreign file and folder survive, and so does the folder that holds them.
        assert_eq!(std::fs::read_to_string(l.data_dir.join("my-notes.txt")).unwrap(), "keep me");
        assert_eq!(std::fs::read_to_string(l.data_dir.join("holiday-photos").join("a.jpg")).unwrap(), "keep me too");
    }

    #[test]
    fn deletedata_removes_the_folder_when_only_moku_files_were_in_it() {
        let sb = Sandbox::new("clean");
        let l = &sb.layout;
        moku_data(&l.data_dir);
        delete_data(&l.data_dir).unwrap();
        assert!(!l.data_dir.exists(), "the empty folder (and its marker) is removed");
        // Nothing there at all is fine.
        delete_data(&l.data_dir).unwrap();
    }

    #[test]
    fn deletedata_refuses_a_folder_without_the_marker_and_touches_nothing() {
        let sb = Sandbox::new("nomarker");
        let l = &sb.layout;
        std::fs::create_dir_all(l.data_dir.join("logs")).unwrap();
        std::fs::write(l.data_dir.join("logs").join("x.log"), "x").unwrap();
        std::fs::write(l.data_dir.join("config.toml"), "x").unwrap();
        std::fs::write(l.data_dir.join("other.txt"), "x").unwrap();
        let err = delete_data(&l.data_dir).unwrap_err();
        assert!(err.contains(".moku-data"), "{err}");
        assert!(l.data_dir.join("logs").join("x.log").exists() && l.data_dir.join("config.toml").exists() && l.data_dir.join("other.txt").exists());
        // The dangerous folders are refused whatever they contain.
        for bad in [PathBuf::from(r"C:\"), PathBuf::from(r"C:\Users"), PathBuf::from("relative")] {
            // Only the check is run here (never delete_data on a real folder).
            let e = moku_core::paths::check_data_dir_safe(&bad).unwrap_err();
            assert!(e.contains("refusing") || e.contains("not an absolute"), "{e}");
        }
    }

    #[test]
    fn date_format() {
        let d = chrono_free_date();
        assert_eq!(d.len(), 8);
        assert!(d.starts_with("20"));
    }
}
