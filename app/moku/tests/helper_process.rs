//! The helper-process modes of the real `moku.exe`: they must write their file and exit without
//! starting an island (no window, no single-instance mutex, no pipe).

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn moku() -> Command {
    Command::new(env!("CARGO_BIN_EXE_moku"))
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("moku-helper-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run_to_end(mut cmd: Command) -> (Option<i32>, Duration) {
    let t = Instant::now();
    let mut child = cmd.spawn().expect("start moku.exe");
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            return (st.code(), t.elapsed());
        }
        if t.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("the helper did not exit within 60 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn launcher_index_sample_writes_a_cache_and_exits() {
    let dir = scratch("sample");
    let out = dir.join("sub").join("launcher-index.bin");
    let mut c = moku();
    c.args(["--launcher-index", out.to_str().unwrap(), "--sample"]);
    let (code, took) = run_to_end(c);
    assert_eq!(code, Some(0), "exit code");
    assert!(took < Duration::from_secs(10), "{took:?}");
    let bytes = std::fs::read(&out).expect("cache file");
    assert_eq!(&bytes[..4], b"MKIX");
    assert_eq!(&bytes[bytes.len() - 4..], b"END.");
    assert!(bytes.len() > 10_000, "icons are in it: {} bytes", bytes.len());
    assert!(!out.with_extension("bin.tmp").exists(), "no temporary file is left");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn launcher_index_needs_an_output_path() {
    let mut c = moku();
    c.arg("--launcher-index");
    assert_eq!(run_to_end(c).0, Some(2));
    let mut c = moku();
    c.args(["--launcher-index", "--sample"]);
    assert_eq!(run_to_end(c).0, Some(2), "a flag is not an output path");
}

#[test]
fn icons_mode_writes_the_icons_of_the_listed_files() {
    let dir = scratch("icons");
    let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let list = dir.join("paths.lst");
    std::fs::write(&list, format!("{sys}\\System32\\notepad.exe\n\n{sys}\\no-such-file-{}.exe\n", std::process::id())).unwrap();
    let out = dir.join("icons.bin");
    let mut c = moku();
    c.args(["--icons", list.to_str().unwrap(), out.to_str().unwrap()]);
    let (code, _) = run_to_end(c);
    assert_eq!(code, Some(0));
    let bytes = std::fs::read(&out).expect("icons file");
    assert_eq!(&bytes[..4], b"MKIC");
    assert_eq!(&bytes[bytes.len() - 4..], b"END.");
    // Missing list file: a failure exit code, no output file.
    let mut c = moku();
    c.args(["--icons", dir.join("nope.lst").to_str().unwrap(), dir.join("none.bin").to_str().unwrap()]);
    assert_eq!(run_to_end(c).0, Some(1));
    assert!(!dir.join("none.bin").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The real Start Menu and Store apps of this PC (reads only). Not run by default:
/// `cargo test -p moku --test helper_process real_start_menu -- --ignored --nocapture`.
#[test]
#[ignore = "enumerates this PC's Start Menu (takes seconds)"]
fn real_start_menu() {
    let dir = scratch("real");
    let out = dir.join("launcher-index.bin");
    let mut c = moku();
    c.args(["--launcher-index", out.to_str().unwrap()]);
    let (code, took) = run_to_end(c);
    println!("exit {code:?} after {took:?}, {} bytes", std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0));
    assert_eq!(code, Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}
