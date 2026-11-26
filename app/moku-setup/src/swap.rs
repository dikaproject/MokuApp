//! The last step of an install: put the staged files in place of the program folder.
//!
//! 1. **The folder swap** (fast, atomic): the program folder is renamed aside, the staging folder
//!    renamed into place. A process whose current directory is inside the folder, an open handle
//!    on a folder or a scanner looking at an exe all make a folder rename fail, for a while or for
//!    as long as that process lives.
//! 2. **The file swap** (fallback): the files are replaced one by one in the same folder. A
//!    directory handle blocks renaming the folder but not the files in it, and a running (mapped)
//!    exe can be *renamed* even though it cannot be overwritten or deleted: every file that is
//!    replaced is first renamed to `<name>.old-<n>`, the new one moved in, and the `.old-<n>` files
//!    deleted afterwards (the ones still mapped are deleted by the next setup run, see
//!    [`sweep_old`]). All or nothing: when any step fails everything done so far is undone.
//!
//! Both are retried with a back-off first ([`Pace`]); the delays are injectable so tests run
//! instantly.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::install::Progress;

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// How long and how often an operation is retried: waits of `first`, `first + step`, ... up to
/// `max`, until they add up to `total`.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub first: Duration,
    pub step: Duration,
    pub max: Duration,
    pub total: Duration,
}

impl Backoff {
    /// Renaming the program folder: Moku may still be letting go of it (a process tearing down, a
    /// scanner on the new exes), so up to 30 s, 50 ms growing to 500 ms.
    pub const DIR: Backoff = Backoff { first: ms(50), step: ms(50), max: ms(500), total: Duration::from_secs(30) };
    /// One file inside the folder: a short wait is enough, the fallback is about to give up anyway.
    pub const FILE: Backoff = Backoff { first: ms(20), step: ms(20), max: ms(250), total: Duration::from_secs(3) };

    /// The waits between attempts (one attempt more than waits), adding up to `total`.
    pub fn delays(&self) -> Vec<Duration> {
        let mut out = vec![];
        let mut sum = Duration::ZERO;
        let mut next = self.first.max(ms(1));
        while sum < self.total {
            let d = next.min(self.total - sum);
            out.push(d);
            sum += d;
            next = (next + self.step).min(self.max.max(ms(1)));
        }
        out
    }
}

/// The retry policy of a swap plus the way to wait (a plain `fn`, so tests pass one that does not).
#[derive(Debug, Clone, Copy)]
pub struct Pace {
    pub dir: Backoff,
    pub file: Backoff,
    pub sleep: fn(Duration),
}

impl Pace {
    pub fn real() -> Pace {
        Pace { dir: Backoff::DIR, file: Backoff::FILE, sleep: std::thread::sleep }
    }

    /// The real policy, without the waiting (tests).
    #[cfg(test)]
    pub fn instant() -> Pace {
        Pace { sleep: |_| {}, ..Pace::real() }
    }
}

/// Run `op` until it succeeds or `backoff` is used up; `on_wait(waited so far)` is called before
/// each wait. A missing source (`NotFound`) is not retried: waiting will not bring it back.
pub fn retry<T>(backoff: &Backoff, sleep: fn(Duration), on_wait: &mut dyn FnMut(Duration), mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut waited = Duration::ZERO;
    let mut delays = backoff.delays().into_iter();
    loop {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(e),
            Err(e) => match delays.next() {
                Some(d) => {
                    on_wait(waited);
                    sleep(d);
                    waited += d;
                }
                None => return Err(e),
            },
        }
    }
}

impl Pace {
    fn file<T>(&self, op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
        retry(&self.file, self.sleep, &mut |_| {}, op)
    }
}

/// A step of the file swap that failed: what, on which path, and the OS error.
#[derive(Debug)]
pub struct SwapError {
    /// Indonesian verb phrase, for the message ("mengganti nama", "memindahkan", ...).
    pub what: &'static str,
    pub path: PathBuf,
    pub error: io::Error,
}

impl SwapError {
    fn new(what: &'static str, path: &Path, error: io::Error) -> SwapError {
        SwapError { what, path: path.to_path_buf(), error }
    }
}

/// What a successful file swap leaves to clean up.
#[derive(Debug, Default)]
pub struct Swapped {
    /// The `<name>.old-<n>` files (or folders) that were moved aside.
    pub old: Vec<PathBuf>,
    /// Folders of the old installation that the new one does not have (to remove once empty).
    pub stale_dirs: Vec<PathBuf>,
}

/// `name.old-<digits>`: what the file swap moves a replaced file to.
pub fn is_old_name(name: &str) -> bool {
    let Some(i) = name.rfind(".old-") else { return false };
    let digits = &name[i + 5..];
    i > 0 && !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Every file (relative path, sorted) and folder below `root`; links are entries, not followed.
/// With `skip_old`, `.old-<n>` leftovers are not part of the listing.
fn walk(root: &Path, skip_old: bool) -> io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    fn go(root: &Path, rel: &Path, skip_old: bool, files: &mut Vec<PathBuf>, dirs: &mut Vec<PathBuf>) -> io::Result<()> {
        for e in std::fs::read_dir(root.join(rel))? {
            let e = e?;
            let name = e.file_name();
            let r = rel.join(&name);
            if skip_old && is_old_name(&name.to_string_lossy()) {
                continue;
            }
            if e.file_type()?.is_dir() {
                dirs.push(r.clone());
                go(root, &r, skip_old, files, dirs)?;
            } else {
                files.push(r);
            }
        }
        Ok(())
    }
    let (mut files, mut dirs) = (vec![], vec![]);
    go(root, Path::new(""), skip_old, &mut files, &mut dirs)?;
    let key = |p: &PathBuf| p.to_string_lossy().to_lowercase();
    files.sort_by_key(key);
    dirs.sort_by_key(key);
    Ok((files, dirs))
}

fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// `<path>.old-<n>`, the first `n` that is free.
fn aside_name(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    (1u32..)
        .map(|n| path.with_file_name(format!("{name}.old-{n}")))
        .find(|c| !exists(c))
        .unwrap_or_else(|| path.with_file_name(format!("{name}.old-0")))
}

/// What has been done so far, to undo in reverse.
enum Step {
    /// `from` was renamed to `to` (a replaced file moved aside).
    Renamed { from: PathBuf, to: PathBuf },
    /// A new file was moved from the staging folder to `dst`.
    Placed { src: PathBuf, dst: PathBuf },
    /// This folder was created.
    MadeDir(PathBuf),
}

/// Undo `steps`, newest first. Returns the steps that could not be undone (logged).
fn undo(steps: &[Step], pace: &Pace) -> usize {
    let mut failed = 0;
    for s in steps.iter().rev() {
        let r = match s {
            // Back to the staging folder (so the swap can be tried again), else just gone.
            Step::Placed { src, dst } => pace.file(|| std::fs::rename(dst, src)).or_else(|_| std::fs::remove_file(dst)),
            Step::Renamed { from, to } => pace.file(|| std::fs::rename(to, from)),
            Step::MadeDir(d) => std::fs::remove_dir(d),
        };
        if let Err(e) = r {
            failed += 1;
            let what = match s {
                Step::Placed { dst, .. } => format!("remove {}", dst.display()),
                Step::Renamed { from, to } => format!("restore {} to {}", to.display(), from.display()),
                Step::MadeDir(d) => format!("remove folder {}", d.display()),
            };
            crate::log(&format!("rollback: cannot {what}: {e}"));
        }
    }
    failed
}

/// Create `dir` and the missing folders above it.
fn make_dirs(dir: &Path, steps: &mut Vec<Step>) -> Result<(), SwapError> {
    let mut missing = vec![];
    let mut p = dir;
    while !exists(p) {
        missing.push(p);
        match p.parent() {
            Some(q) => p = q,
            None => break,
        }
    }
    for d in missing.into_iter().rev() {
        std::fs::create_dir(d).map_err(|e| SwapError::new("membuat folder", d, e))?;
        steps.push(Step::MadeDir(d.to_path_buf()));
    }
    Ok(())
}

/// Move a staged file into place: rename (same volume), else copy and delete.
fn move_file(src: &Path, dst: &Path, pace: &Pace) -> io::Result<()> {
    match pace.file(|| std::fs::rename(src, dst)) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            crate::log(&format!("rename {} -> {} failed ({rename_err}); copying", src.display(), dst.display()));
            match std::fs::copy(src, dst) {
                Ok(_) => {
                    let _ = std::fs::remove_file(src);
                    Ok(())
                }
                Err(e) => {
                    let _ = std::fs::remove_file(dst);
                    Err(e)
                }
            }
        }
    }
}

/// Replace the files of `dest` with the ones in `staging`, in place (see the module docs).
/// `dest` need not exist. On success the staged files have been moved out of `staging` (the
/// caller deletes the rest); on failure everything is back as it was and `staging` holds every
/// file again.
pub fn swap_files(staging: &Path, dest: &Path, pace: &Pace) -> Result<Swapped, SwapError> {
    let (new_files, new_dirs) = walk(staging, false).map_err(|e| SwapError::new("membaca", staging, e))?;
    let (old_files, old_dirs) = if dest.is_dir() { walk(dest, true).map_err(|e| SwapError::new("membaca", dest, e))? } else { (vec![], vec![]) };
    let key = |p: &Path| p.to_string_lossy().to_lowercase();
    let keep_files: HashSet<String> = new_files.iter().map(|p| key(p)).collect();
    let keep_dirs: HashSet<String> = new_dirs.iter().map(|p| key(p)).collect();

    let mut steps: Vec<Step> = vec![];
    let r = (|| -> Result<(), SwapError> {
        make_dirs(dest, &mut steps)?;
        for rel in &new_files {
            let (src, dst) = (staging.join(rel), dest.join(rel));
            if let Some(d) = dst.parent() {
                make_dirs(d, &mut steps)?;
            }
            if exists(&dst) {
                let aside = aside_name(&dst);
                pace.file(|| std::fs::rename(&dst, &aside)).map_err(|e| SwapError::new("mengganti nama", &dst, e))?;
                steps.push(Step::Renamed { from: dst.clone(), to: aside });
            }
            move_file(&src, &dst, pace).map_err(|e| SwapError::new("memindahkan berkas ke", &dst, e))?;
            steps.push(Step::Placed { src, dst });
        }
        // What the old installation had and the new one does not: moved aside too, so that the
        // result is what the folder swap gives.
        for rel in old_files.iter().filter(|r| !keep_files.contains(&key(r))) {
            let p = dest.join(rel);
            if !exists(&p) {
                continue; // went with a folder that was moved aside above
            }
            let aside = aside_name(&p);
            pace.file(|| std::fs::rename(&p, &aside)).map_err(|e| SwapError::new("mengganti nama", &p, e))?;
            steps.push(Step::Renamed { from: p, to: aside });
        }
        Ok(())
    })();
    match r {
        Ok(()) => {
            let old = steps.iter().filter_map(|s| if let Step::Renamed { to, .. } = s { Some(to.clone()) } else { None }).collect();
            let stale_dirs = old_dirs.iter().filter(|d| !keep_dirs.contains(&key(d))).map(|d| dest.join(d)).collect();
            Ok(Swapped { old, stale_dirs })
        }
        Err(e) => {
            let left = undo(&steps, pace);
            if left > 0 {
                crate::log(&format!("rollback incomplete: {left} step(s) could not be undone"));
            }
            Err(e)
        }
    }
}

fn remove_any(p: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(p)?.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) }
}

/// After a successful [`swap_files`]: delete the `.old-<n>` files (best effort; a running exe
/// cannot be deleted, those stay for [`sweep_old`]) and the folders the new version does not have.
/// Returns what is still there.
pub fn finish(swapped: &Swapped) -> Vec<PathBuf> {
    let mut left = vec![];
    for p in &swapped.old {
        if let Err(e) = remove_any(p) {
            crate::log(&format!("left {} behind ({e}); removed on the next run", p.display()));
            left.push(p.clone());
        }
    }
    // Deepest first; `remove_dir` only takes empty folders.
    for d in swapped.stale_dirs.iter().rev() {
        let _ = std::fs::remove_dir(d);
    }
    left
}

/// Delete every `.old-<n>` file or folder below `dir` (what an earlier swap could not delete
/// because a program still ran from it). Best effort; returns how many are still there.
pub fn sweep_old(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut left = 0;
    for e in rd.flatten() {
        let (p, name) = (e.path(), e.file_name().to_string_lossy().to_string());
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        if is_old_name(&name) {
            if remove_any(&p).is_err() {
                left += 1;
            }
        } else if is_dir {
            left += sweep_old(&p);
        }
    }
    left
}

/// Put the staged files in place of `dir` (see the module docs). `old` is where the folder swap
/// parks the previous folder. `progress` hears about waiting and about the fallback.
pub fn swap_in(dir: &Path, staging: &Path, old: &Path, pace: &Pace, progress: Progress) -> Result<(), String> {
    use std::cell::Cell;
    let had_old = dir.exists();

    // 1. The folder swap, with patience: the user is told while we wait.
    let last_note = Cell::new(None::<u64>);
    let mut waiting = |waited: Duration| {
        let secs = waited.as_secs();
        if last_note.get().is_none_or(|n| secs >= n + 5) {
            let text = if last_note.get().is_none() { "Menunggu Moku selesai ditutup…".to_string() } else { format!("Menunggu Moku selesai ditutup… ({secs} dtk)") };
            progress(0.8 + 0.08 * (waited.as_secs_f32() / pace.dir.total.as_secs_f32().max(1.0)).min(1.0), &text);
            last_note.set(Some(secs));
        }
    };
    let folder = (|| -> Result<(), (&'static str, PathBuf, io::Error)> {
        if had_old {
            retry(&pace.dir, pace.sleep, &mut waiting, || std::fs::rename(dir, old)).map_err(|e| ("rename the program folder aside", dir.to_path_buf(), e))?;
        }
        if let Err(e) = retry(&pace.dir, pace.sleep, &mut waiting, || std::fs::rename(staging, dir)) {
            // Put the old folder back before anything else happens.
            if had_old {
                if let Err(re) = pace.file(|| std::fs::rename(old, dir)) {
                    crate::log(&format!("cannot move {} back to {}: {re}", old.display(), dir.display()));
                }
            }
            return Err(("rename the new folder into place", staging.to_path_buf(), e));
        }
        Ok(())
    })();
    match folder {
        Ok(()) => {
            if had_old && std::fs::remove_dir_all(old).is_err() {
                crate::log(&format!("left {} behind (removed on the next run)", old.display()));
            }
            return Ok(());
        }
        Err((step, path, e)) => {
            crate::diag::log_failure(dir, step, &path, &e);
            crate::log("swap: replacing the files one by one instead");
        }
    }

    // 2. The file swap.
    progress(0.88, "Mengganti berkas satu per satu…");
    match swap_files(staging, dir, pace) {
        Ok(swapped) => {
            let left = finish(&swapped);
            if !left.is_empty() {
                crate::log(&format!("{} old file(s) still in use, removed on the next run", left.len()));
            }
            let _ = std::fs::remove_dir_all(staging);
            // The folder swap may have got as far as parking the old folder.
            if exists(old) {
                let _ = std::fs::remove_dir_all(old);
            }
            Ok(())
        }
        Err(e) => {
            crate::diag::log_failure(dir, e.what, &e.path, &e.error);
            Err(format!("Folder {} sedang dipakai: tidak bisa {} {} ({}). Tutup Moku lalu coba lagi.", dir.display(), e.what, e.path.display(), e.error))
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::os::windows::fs::OpenOptionsExt;

    /// A scratch folder under %TEMP% (long form, see `install::tests::Sandbox`), removed on drop.
    pub struct Scratch(pub PathBuf);

    impl Scratch {
        pub fn new(name: &str) -> Scratch {
            let root = std::env::temp_dir().join(format!("moku-setup-swap-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let long = std::fs::canonicalize(&root).unwrap();
            Scratch(PathBuf::from(long.to_string_lossy().strip_prefix(r"\\?\").unwrap_or(&long.to_string_lossy()).to_string()))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Kills (and reaps) a child when dropped, so a failing assertion leaves nothing running.
    pub struct Kill(pub std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Start a harmless child (`ping -n 30`) whose current directory is `dir`, and return once it
    /// really runs: it opens that directory while it starts up, after `spawn` has returned, and
    /// only its first line of output says that has happened (a fixed sleep would be a guess, and
    /// probing the folder with a rename would disturb the child's start).
    pub fn spawn_ping_in(dir: &Path) -> Kill {
        use std::io::BufRead;
        let mut child = std::process::Command::new(ping()).args(["-n", "30", "127.0.0.1"]).current_dir(dir).stdout(std::process::Stdio::piped()).spawn().unwrap();
        let out = child.stdout.take().unwrap();
        let guard = Kill(child);
        let (tx, rx) = std::sync::mpsc::channel();
        // Keeps reading, so the child never gets a broken pipe; ends when the child is killed.
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines() {
                let _ = tx.send(line.is_ok());
            }
        });
        rx.recv_timeout(Duration::from_secs(30)).expect("the child printed nothing");
        guard
    }

    pub fn ping() -> PathBuf {
        PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("System32").join("PING.EXE")
    }

    fn write(root: &Path, files: &[(&str, &str)]) {
        for (rel, text) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
    }

    /// Every file below `root` with its content, sorted.
    fn snapshot(root: &Path) -> Vec<(String, String)> {
        let (files, _) = walk(root, false).unwrap();
        files.into_iter().map(|f| (f.to_string_lossy().to_string(), String::from_utf8_lossy(&std::fs::read(root.join(&f)).unwrap()).to_string())).collect()
    }

    fn dirs(root: &Path) -> Vec<String> {
        walk(root, false).unwrap().1.into_iter().map(|d| d.to_string_lossy().to_string()).collect()
    }

    #[test]
    fn the_back_off_adds_up_to_the_wait_and_grows_gently() {
        let d = Backoff::DIR.delays();
        assert_eq!(d.iter().sum::<Duration>(), Duration::from_secs(30), "about 30 s in total");
        assert_eq!(d[0], ms(50));
        assert!(d[..d.len() - 1].windows(2).all(|w| w[0] <= w[1]), "never shorter than the one before (the last one is cut to fit)");
        assert_eq!(*d.iter().max().unwrap(), ms(500), "capped at 500 ms");
        assert!(d.len() > 50 && d.len() < 80, "{} attempts", d.len());
        let f = Backoff::FILE.delays();
        assert_eq!(f.iter().sum::<Duration>(), Duration::from_secs(3));
        // A degenerate policy terminates.
        let z = Backoff { first: Duration::ZERO, step: Duration::ZERO, max: Duration::ZERO, total: ms(5) };
        assert_eq!(z.delays().iter().sum::<Duration>(), ms(5));
    }

    #[test]
    fn retry_succeeds_when_the_blocker_clears_and_fails_when_the_policy_runs_out() {
        thread_local!(static SLEPT: Cell<u32> = const { Cell::new(0) });
        fn count(_: Duration) {
            SLEPT.with(|s| s.set(s.get() + 1));
        }
        let policy = Backoff { first: ms(10), step: ms(10), max: ms(40), total: ms(200) };
        let attempts = policy.delays().len() as u32 + 1;

        // The blocker clears after 3 failures: 4 attempts, 3 waits, 3 notes.
        let calls = Cell::new(0u32);
        let notes = RefCell::new(vec![]);
        SLEPT.with(|s| s.set(0));
        let r = retry(&policy, count, &mut |w| notes.borrow_mut().push(w), || {
            calls.set(calls.get() + 1);
            if calls.get() <= 3 { Err(io::Error::from_raw_os_error(32)) } else { Ok(calls.get()) }
        });
        assert_eq!(r.unwrap(), 4);
        assert_eq!(SLEPT.with(Cell::get), 3);
        let notes = notes.into_inner();
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0], Duration::ZERO);
        assert!(notes.windows(2).all(|w| w[0] < w[1]), "the time waited so far only grows");

        // It never clears: every attempt is made, then the LAST error comes back.
        calls.set(0);
        SLEPT.with(|s| s.set(0));
        let r: io::Result<()> = retry(&policy, count, &mut |_| {}, || {
            calls.set(calls.get() + 1);
            Err(io::Error::from_raw_os_error(if calls.get() == attempts { 5 } else { 32 }))
        });
        assert_eq!(r.unwrap_err().raw_os_error(), Some(5));
        assert_eq!(calls.get(), attempts);
        assert_eq!(SLEPT.with(Cell::get), attempts - 1);

        // A missing source is not worth waiting for.
        calls.set(0);
        let r: io::Result<()> = retry(&policy, count, &mut |_| {}, || {
            calls.set(calls.get() + 1);
            Err(io::ErrorKind::NotFound.into())
        });
        assert!(r.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn old_names() {
        for ok in ["moku.exe.old-1", "a.old-27", "x.y.old-3"] {
            assert!(is_old_name(ok), "{ok}");
        }
        for bad in ["moku.exe", ".old-1", "a.old-", "a.old-x", "a.old-1x", "Moku.new-5", "a.old1"] {
            assert!(!is_old_name(bad), "{bad}");
        }
        let s = Scratch::new("aside");
        write(&s.0, &[("a.txt", "1"), ("a.txt.old-1", "2"), ("a.txt.old-2", "3")]);
        assert_eq!(aside_name(&s.0.join("a.txt")), s.0.join("a.txt.old-3"));
    }

    #[test]
    fn the_file_swap_gives_the_same_result_as_the_folder_swap() {
        let s = Scratch::new("equal");
        let (staging, dest) = (s.0.join("new"), s.0.join("Moku"));
        write(&staging, &[("moku.exe", "new exe"), ("mokuctl.exe", "new ctl"), ("docs\\hook.md", "new doc"), ("fresh\\deep\\x.txt", "x")]);
        write(&dest, &[("moku.exe", "old exe"), ("mokuctl.exe", "old ctl"), ("old-only.dll", "gone"), ("olddir\\sub\\y.txt", "gone too"), ("docs\\hook.md", "old doc")]);
        let sw = swap_files(&staging, &dest, &Pace::instant()).unwrap();
        assert_eq!(sw.old.len(), 5, "{:?}", sw.old);
        assert!(finish(&sw).is_empty());
        assert_eq!(snapshot(&dest), snapshot_of(&[("docs\\hook.md", "new doc"), ("fresh\\deep\\x.txt", "x"), ("moku.exe", "new exe"), ("mokuctl.exe", "new ctl")]));
        assert_eq!(dirs(&dest), vec!["docs", "fresh", "fresh\\deep"], "folders the new version lacks are gone");
    }

    fn snapshot_of(v: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut v: Vec<_> = v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        v.sort_by_key(|(a, _)| a.to_lowercase());
        v
    }

    #[test]
    fn the_file_swap_creates_a_missing_folder() {
        let s = Scratch::new("fresh");
        let (staging, dest) = (s.0.join("new"), s.0.join("a").join("b").join("Moku"));
        write(&staging, &[("moku.exe", "n"), ("docs\\d.md", "d")]);
        let sw = swap_files(&staging, &dest, &Pace::instant()).unwrap();
        assert!(sw.old.is_empty());
        assert_eq!(snapshot(&dest), snapshot_of(&[("docs\\d.md", "d"), ("moku.exe", "n")]));
    }

    /// An exclusive handle on a file: it cannot be renamed or deleted until the guard is dropped.
    fn lock(p: &Path) -> std::fs::File {
        std::fs::OpenOptions::new().read(true).share_mode(0).open(p).unwrap()
    }

    #[test]
    fn a_failure_halfway_puts_everything_back() {
        let s = Scratch::new("rollback");
        let (staging, dest) = (s.0.join("new"), s.0.join("Moku"));
        let new = [("a.txt", "new a"), ("b.txt", "new b"), ("c.txt", "new c"), ("sub\\d.txt", "new d"), ("brand-new\\e.txt", "new e")];
        let old = [("a.txt", "old a"), ("b.txt", "old b"), ("c.txt", "old c"), ("sub\\d.txt", "old d"), ("z-old-only.txt", "old z")];
        write(&staging, &new);
        write(&dest, &old);
        let before = snapshot(&dest);
        let dirs_before = dirs(&dest);

        // 1. b.txt cannot be replaced: a.txt was already done and must be undone.
        {
            let _held = lock(&dest.join("b.txt"));
            let e = swap_files(&staging, &dest, &Pace::instant()).unwrap_err();
            assert_eq!(e.path, dest.join("b.txt"), "the message names the exact path");
            assert_eq!(e.what, "mengganti nama");
            assert_eq!(e.error.raw_os_error(), Some(32), "sharing violation: {}", e.error);
        }
        assert_eq!(snapshot(&dest), before, "every file is back exactly as before, no .old-* and nothing new");
        assert_eq!(dirs(&dest), dirs_before);
        assert_eq!(snapshot(&staging), snapshot_of(&new), "the staged files are all still there");

        // 2. Everything is replaced, then the old-only file cannot be moved aside: all of it is undone.
        {
            let _held = lock(&dest.join("z-old-only.txt"));
            let e = swap_files(&staging, &dest, &Pace::instant()).unwrap_err();
            assert_eq!(e.path, dest.join("z-old-only.txt"));
        }
        assert_eq!(snapshot(&dest), before);
        assert_eq!(dirs(&dest), dirs_before, "the folder that was created for the new files is removed again");

        // Once nothing is in the way the same swap goes through.
        let sw = swap_files(&staging, &dest, &Pace::instant()).unwrap();
        finish(&sw);
        assert_eq!(snapshot(&dest), snapshot_of(&new));
    }

    #[test]
    fn a_running_exe_can_be_swapped_and_its_old_copy_is_swept_once_it_stops() {
        let s = Scratch::new("mapped");
        let (staging, dest) = (s.0.join("new"), s.0.join("Moku"));
        write(&staging, &[("moku.exe", "the new moku"), ("docs\\d.md", "d")]);
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::copy(ping(), dest.join("moku.exe")).unwrap();
        let mut child = Kill(std::process::Command::new(dest.join("moku.exe")).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap());

        // Renaming a running exe works; deleting it does not.
        let sw = swap_files(&staging, &dest, &Pace::instant()).unwrap();
        assert_eq!(std::fs::read(dest.join("moku.exe")).unwrap(), b"the new moku", "the new exe is in place");
        assert_eq!(sw.old, vec![dest.join("moku.exe.old-1")]);
        let left = finish(&sw);
        assert_eq!(left, vec![dest.join("moku.exe.old-1")], "the running copy cannot be deleted yet");
        assert!(child.0.try_wait().unwrap().is_none(), "and it still runs");
        assert_eq!(sweep_old(&dest), 1, "a sweep while it runs leaves it");

        // Once it is gone the next run's sweep deletes it (poll: the image is released a moment
        // after the process object signals).
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while sweep_old(&dest) > 0 {
            assert!(std::time::Instant::now() < deadline, "moku.exe.old-1 is still there");
            std::thread::sleep(ms(50));
        }
        assert_eq!(snapshot(&dest), snapshot_of(&[("docs\\d.md", "d"), ("moku.exe", "the new moku")]));
    }

    #[test]
    fn a_process_inside_the_folder_blocks_the_folder_swap_but_not_the_file_swap() {
        let s = Scratch::new("cwd");
        let (staging, dest, old) = (s.0.join("Moku.new"), s.0.join("Moku"), s.0.join("Moku.old"));
        write(&staging, &[("moku.exe", "new exe"), ("docs\\d.md", "new doc")]);
        write(&dest, &[("moku.exe", "old exe"), ("gone.txt", "old"), ("sub\\keep-me-company.txt", "x")]);
        // A harmless child whose current directory is inside the folder.
        let _child = spawn_ping_in(&dest.join("sub"));

        // The directory rename fails.
        let err = std::fs::rename(&dest, &old).unwrap_err();
        assert!(matches!(err.raw_os_error(), Some(5 | 32)), "{err}");

        // swap_in: the folder swap exhausts its (instant) patience, tells the user, then the
        // file swap succeeds.
        let notes = RefCell::new(vec![]);
        swap_in(&dest, &staging, &old, &Pace::instant(), &|f, t| notes.borrow_mut().push((f, t.to_string()))).unwrap();
        let notes = notes.into_inner();
        assert!(notes.iter().any(|(_, t)| t.starts_with("Menunggu Moku selesai ditutup")), "{notes:?}");
        assert!(notes.iter().any(|(_, t)| t.contains("satu per satu")), "{notes:?}");
        assert!(notes.windows(2).all(|w| w[0].0 <= w[1].0) && notes.iter().all(|n| n.0 <= 0.9), "progress only goes up and stays below the next step");
        assert_eq!(snapshot(&dest), snapshot_of(&[("docs\\d.md", "new doc"), ("moku.exe", "new exe")]), "new content, no .old-* left");
        assert!(!staging.exists() && !old.exists(), "no staging or parked folder left");
        // sub is the child's cwd: it can be emptied but not removed while the child runs; that is fine.
    }

    #[test]
    fn the_folder_swap_is_used_when_nothing_is_in_the_way() {
        let s = Scratch::new("plain");
        let (staging, dest, old) = (s.0.join("Moku.new"), s.0.join("Moku"), s.0.join("Moku.old"));
        write(&staging, &[("moku.exe", "new")]);
        write(&dest, &[("moku.exe", "old"), ("gone.txt", "x")]);
        swap_in(&dest, &staging, &old, &Pace::instant(), &|_, _| {}).unwrap();
        assert_eq!(snapshot(&dest), snapshot_of(&[("moku.exe", "new")]));
        assert!(!staging.exists() && !old.exists());
        // No old folder at all.
        let dest2 = s.0.join("Fresh");
        write(&s.0.join("n2"), &[("moku.exe", "n2")]);
        swap_in(&dest2, &s.0.join("n2"), &s.0.join("Fresh.old"), &Pace::instant(), &|_, _| {}).unwrap();
        assert_eq!(snapshot(&dest2), snapshot_of(&[("moku.exe", "n2")]));
    }

    #[test]
    fn the_install_log_helpers_never_fail() {
        let s = Scratch::new("diag");
        write(&s.0, &[("a.txt", "a")]);
        // Held open by this process, with sharing: the Restart Manager may or may not list us;
        // either way the call returns.
        let _f = std::fs::File::open(s.0.join("a.txt")).unwrap();
        let _ = crate::diag::holders(&s.0);
        crate::diag::log_failure(&s.0, "test", &s.0.join("a.txt"), &io::Error::from_raw_os_error(32));
        assert!(crate::diag::holders(&s.0.join("does-not-exist")).is_empty());
    }
}
