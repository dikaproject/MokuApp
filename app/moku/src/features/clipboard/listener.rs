//! The clipboard listener: a message-only window on a service thread that receives
//! `WM_CLIPBOARDUPDATE`, reads the clipboard (bounded, skip rules first) and hands a finished
//! [`ClipItem`] to a sink. All the slow work (waiting for a delayed-render owner, WIC decode,
//! PNG encode, file writes) happens here, never on the UI thread.

use std::cell::RefCell;
use std::sync::Arc;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::model::Skip;
use super::store::{self, Ingested};
use super::sys;
use crate::services::ServiceCtx;

/// Receives every recorded clip: a stored item, or a one-time code held in memory only
/// (production: posts to the UI thread).
pub type Sink = Arc<dyn Fn(Ingested) + Send + Sync>;
/// The user's `ignore_apps` list, read fresh for every clipboard change.
pub type IgnoreFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

const CLASS: PCWSTR = w!("MokuClipboardListener");
const TIMER_DEBOUNCE: usize = 1;
/// Several apps update the clipboard in bursts (copy = clear + set + set …): act once, 80 ms after
/// the last notification.
const DEBOUNCE_MS: u32 = 80;

struct Listener {
    ignore: IgnoreFn,
    sink: Sink,
    dir: std::path::PathBuf,
}

thread_local! {
    static LISTENER: RefCell<Option<Listener>> = const { RefCell::new(None) };
}

/// Create the message-only window (also used by tests as a clipboard owner).
pub fn create_window() -> Option<HWND> {
    unsafe {
        let inst = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: CLASS, ..Default::default() };
        RegisterClassW(&wc); // fails harmlessly when already registered
        CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("MokuClip"), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst.into()), None).ok()
    }
}

/// Service body: listen until stopped. `dir` is the clipboard folder.
pub fn run(ctx: ServiceCtx, dir: std::path::PathBuf, ignore: IgnoreFn, sink: Sink) {
    unsafe {
        // MTA: WIC and the clipboard calls need COM, no apartment-bound objects are kept.
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        if let Some(hwnd) = create_window() {
            LISTENER.with(|l| *l.borrow_mut() = Some(Listener { ignore, sink, dir }));
            if AddClipboardFormatListener(hwnd).is_ok() {
                log_info!("clipboard listener on");
                ctx.run_message_loop();
                let _ = RemoveClipboardFormatListener(hwnd);
            } else {
                log_error!("AddClipboardFormatListener failed");
            }
            LISTENER.with(|l| *l.borrow_mut() = None);
            let _ = DestroyWindow(hwnd);
        } else {
            log_error!("clipboard listener window could not be created");
        }
        if com {
            CoUninitialize();
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CLIPBOARDUPDATE => {
                SetTimer(Some(hwnd), TIMER_DEBOUNCE, DEBOUNCE_MS, None);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TIMER_DEBOUNCE => {
                let _ = KillTimer(Some(hwnd), TIMER_DEBOUNCE);
                process(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Handle one (debounced) clipboard change.
fn process(hwnd: HWND) {
    let seq = sys::sequence();
    // Moku is borrowing the clipboard (Tanya AI's copy fallback, "Ganti teks"): what the program in front
    // copied for it, and the restore that follows, are not the user's copies.
    if super::hold::covers(seq) {
        log_info!("clipboard: change while Moku borrowed the clipboard, not recorded");
        return;
    }
    // Ask who owns the clipboard right away: it changes when the next app copies.
    let owner = sys::owner_exe();
    // The file name of the owner is also what tells a code from a messaging app (smart actions).
    let owner_for_detection = owner.clone();
    let Some((ignore, sink, dir)) = LISTENER.with(|l| l.borrow().as_ref().map(|l| (l.ignore.clone(), l.sink.clone(), l.dir.clone()))) else { return };
    let apps = ignore();

    let read = {
        let Some(_open) = sys::open(Some(hwnd)) else {
            log_warn!("clipboard busy, change not recorded");
            return;
        };
        sys::read_open(&apps, owner_for_detection)
    };
    // Something else copied while we were reading: that change has its own notification.
    if sys::sequence() != seq {
        return;
    }
    match read {
        sys::Read::Nothing => {}
        sys::Read::Skip(Skip::SelfWrite) => {}
        sys::Read::Skip(why) => log_info!("clipboard: left out ({why:?})"),
        sys::Read::TooBig => log_info!("clipboard: image too large, not recorded"),
        sys::Read::Got(c) => {
            // One-time codes are recognised here, before anything is written (see `store::ingest`).
            let otp_guard = crate::app::current_config().clipboard.otp_guard;
            if let Some(ingested) = store::ingest(&dir, c, owner.as_deref(), otp_guard, now()) {
                sink(ingested);
            } else {
                log_warn!("clipboard: could not store the new item");
            }
        }
    }
}

/// End-to-end test against the REAL clipboard, so it is `#[ignore]`d: it saves what is on the
/// clipboard, runs the listener on a message-only window, writes test content with the real
/// Windows API, and puts the user's content back afterwards (also when an assertion fails).
/// Run: `cargo test -p moku clipboard_live -- --ignored --test-threads=1`
#[cfg(test)]
mod live {
    use std::sync::mpsc::{Receiver, channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use windows::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};

    use super::*;
    use crate::features::clipboard::model::{ClipItem, Kind};
    use crate::features::clipboard::{imgutil, model, sys};
    use crate::ui::Image;

    struct Restore {
        backup: sys::Backup,
        owner: HWND,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            self.backup.restore(self.owner);
        }
    }

    fn utf16z(s: &str) -> Vec<u8> {
        let mut v: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        v.extend([0, 0]);
        v
    }

    fn text_fmt() -> u32 {
        CF_UNICODETEXT.0 as u32
    }

    fn expect(rx: &Receiver<ClipItem>, ms: u64) -> Option<ClipItem> {
        rx.recv_timeout(Duration::from_millis(ms)).ok()
    }

    fn gradient(w: u32, h: u32) -> Image {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                v.extend([(x * 4) as u8, (y * 4) as u8, 90, 255]);
            }
        }
        Image::new(w, h, v)
    }

    #[test]
    #[ignore = "uses the real clipboard (saved and restored); run with --ignored --test-threads=1"]
    fn clipboard_live_listener_end_to_end() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let dir = std::env::temp_dir().join(format!("moku-clip-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let owner = create_window().expect("message window");
        let _restore = Restore { backup: sys::Backup::take(owner), owner };

        let (tx, rx) = channel::<ClipItem>();
        let tx = Mutex::new(tx);
        // One-time codes arrive on their own channel (held in memory by the feature, never stored).
        let (stx, srx) = channel::<String>();
        let stx = Mutex::new(stx);
        let sink: Sink = Arc::new(move |i| match i {
            Ingested::Item { item, .. } => {
                let _ = tx.lock().unwrap().send(item);
            }
            Ingested::Secret(_, code) => {
                let _ = stx.lock().unwrap().send(code);
            }
        });
        let ignore_list = Arc::new(Mutex::new(Vec::<String>::new()));
        let il = ignore_list.clone();
        let ignore: IgnoreFn = Arc::new(move || il.lock().unwrap().clone());
        let ctx = ServiceCtx::detached("clipboard-live-test");
        let (c2, d2) = (ctx.clone(), dir.clone());
        let (idtx, idrx) = channel::<u32>();
        let th = std::thread::spawn(move || {
            let _ = idtx.send(unsafe { windows::Win32::System::Threading::GetCurrentThreadId() });
            run(c2, d2, ignore, sink)
        });
        let listener_tid = idrx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(500)); // listener registered

        // 1. Plain text is recorded.
        assert!(sys::set_formats(owner, &[(text_fmt(), utf16z("halo moku live test"))]));
        let it = expect(&rx, 2000).expect("text recorded");
        assert_eq!(it.kind, Kind::Text);
        assert_eq!(it.preview, "halo moku live test");
        assert_eq!(store::read_text(&dir, it.key).as_deref(), Some("halo moku live test"));

        // 2. A password manager's exclusion format: nothing is recorded, not even read.
        let mut f = vec![(text_fmt(), utf16z("super secret 1")), (sys::fmt_exclude(), vec![1, 0, 0, 0])];
        assert!(sys::set_formats(owner, &f));
        assert!(expect(&rx, 700).is_none(), "ExcludeClipboardContentFromMonitorProcessing must be respected");

        // 3. CanIncludeInClipboardHistory = 0 skips, = 1 records.
        f = vec![(text_fmt(), utf16z("super secret 2")), (sys::fmt_can_include(), 0u32.to_le_bytes().to_vec())];
        assert!(sys::set_formats(owner, &f));
        assert!(expect(&rx, 700).is_none(), "CanIncludeInClipboardHistory = 0 must be respected");
        f = vec![(text_fmt(), utf16z("allowed text")), (sys::fmt_can_include(), 1u32.to_le_bytes().to_vec())];
        assert!(sys::set_formats(owner, &f));
        assert_eq!(expect(&rx, 2000).expect("CanInclude=1 is recorded").preview, "allowed text");

        // 4. Our own put-back (marker format) is not recorded again.
        assert!(sys::put_text(owner, "written by moku"));
        assert!(expect(&rx, 700).is_none(), "self-writes are not re-recorded");

        // 5. The owner process is on ignore_apps (this test process).
        let exe = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().to_string();
        *ignore_list.lock().unwrap() = vec![exe.clone()];
        assert!(sys::set_formats(owner, &[(text_fmt(), utf16z("from an ignored app"))]));
        assert_eq!(sys::owner_exe().as_deref(), Some(exe.as_str()), "the owner process is resolved to its exe name");
        assert!(expect(&rx, 700).is_none(), "ignore_apps must be respected ({exe})");
        *ignore_list.lock().unwrap() = vec!["somethingelse.exe".into()];
        assert!(sys::set_formats(owner, &[(text_fmt(), utf16z("from a normal app"))]));
        assert_eq!(expect(&rx, 2000).expect("not ignored").preview, "from a normal app");

        // 6. Huge text: 3 MB arrives truncated to 100 KB; the island never copies the rest.
        let big = "x".repeat(3 * 1024 * 1024);
        assert!(sys::set_formats(owner, &[(text_fmt(), utf16z(&big))]));
        let it = expect(&rx, 3000).expect("big text recorded (truncated)");
        assert!(it.truncated);
        assert_eq!(it.bytes as usize, model::MAX_TEXT_BYTES);
        assert_eq!(std::fs::metadata(dir.join(model::text_file(it.key))).unwrap().len() as usize, model::MAX_TEXT_BYTES);

        // 7. A PNG image: 128 px thumbnail stored, the original is kept for the newest image.
        let img = gradient(64, 48);
        let png = crate::imaging::encode_png(&img).unwrap();
        assert!(sys::put_image(owner, &png, Some(&img), false));
        let it = expect(&rx, 3000).expect("image recorded");
        assert_eq!((it.kind, it.w, it.h), (Kind::Image, 64, 48));
        assert!(it.full);
        assert!(dir.join(model::thumb_file(it.key)).exists() && dir.join(model::orig_file(it.key)).exists());

        // 8. A DIB-only image (older apps) goes through the BMP path.
        let dib = imgutil::dib_from_premul_bgra(100, 50, &gradient(100, 50).0.bgra);
        assert!(sys::set_formats(owner, &[(CF_DIB.0 as u32, dib)]));
        let it = expect(&rx, 3000).expect("dib recorded");
        assert_eq!((it.kind, it.w, it.h), (Kind::Image, 100, 50));

        // 9. A tiny PNG claiming 60000 × 60000 pixels, and a 9 MB blob: both refused.
        let mut bomb = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        bomb.extend(60_000u32.to_be_bytes());
        bomb.extend(60_000u32.to_be_bytes());
        bomb.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        assert!(sys::set_formats(owner, &[(sys::fmt_png(), bomb)]));
        assert!(expect(&rx, 700).is_none(), "decompression bomb refused");
        assert!(sys::set_formats(owner, &[(sys::fmt_png(), vec![0u8; 9 * 1024 * 1024])]));
        assert!(expect(&rx, 700).is_none(), "image over 8 MB refused");

        // 10. A one-time code is recognised before anything is written: it arrives as a secret
        // (digits only), no item is stored and the clipboard folder gains no file.
        let files = |d: &std::path::Path| std::fs::read_dir(d).map(|r| r.count()).unwrap_or(0);
        let before = files(&dir);
        assert!(sys::set_formats(owner, &[(text_fmt(), utf16z("Kode OTP Anda 482913. Jangan berikan kepada siapapun."))]));
        assert_eq!(srx.recv_timeout(Duration::from_millis(2000)).as_deref(), Ok("482913"));
        assert!(expect(&rx, 500).is_none(), "a code is never an item");
        assert_eq!(files(&dir), before, "and never a file");
        // The same text with the guard off is stored like any text (the setting is read per change
        // from the app's config, which is the default here, so this is only checked in `ingest` tests).

        // Put the user's clipboard back first; only then wind the listener down.
        drop(_restore);
        ctx.request_stop();
        // A detached context has no thread id, so end the listener's message loop ourselves.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW(listener_tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        th.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Helper for the manual island check: while a real `moku.exe` is running, writes a harmless text
/// and a small image to the real clipboard (so the island's listener records them), then puts the
/// user's own clipboard back, marked private so the island does not record it.
/// Run: `cargo test -p moku clipboard_live_sampler -- --ignored --nocapture`
#[cfg(test)]
mod sampler {
    use std::time::Duration;

    use super::*;
    use crate::features::clipboard::sys;
    use crate::ui::Image;

    #[test]
    #[ignore = "writes sample items to the real clipboard (saved and restored) for a running island"]
    fn clipboard_live_sampler() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let owner = create_window().expect("message window");
        let backup = sys::Backup::take(owner);
        let text = "Rapat tim dipindah ke Kamis jam 10.00, tolong kabari yang lain";
        let mut v: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        v.extend([0, 0]);
        let ok_text = sys::set_formats(owner, &[(windows::Win32::System::Ole::CF_UNICODETEXT.0 as u32, v)]);
        std::thread::sleep(Duration::from_millis(1500));
        let mut px = Vec::new();
        for y in 0..90u32 {
            for x in 0..160u32 {
                px.extend([(60 + x) as u8, (200 - y) as u8, (120 + y) as u8, 255]);
            }
        }
        let img = Image::new(160, 90, px);
        let png = crate::imaging::encode_png(&img).unwrap();
        let ok_img = sys::put_image(owner, &png, Some(&img), false);
        std::thread::sleep(Duration::from_millis(1500));
        backup.restore(owner);
        std::thread::sleep(Duration::from_millis(300));
        assert!(ok_text && ok_img);
    }
}
