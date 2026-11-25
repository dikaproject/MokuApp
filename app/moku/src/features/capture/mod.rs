//! Feature `capture` (Task 5, track D): screenshots.
//!
//! * **Area** — hands over to Windows' own snipping overlay (`ms-screenclip:`).
//! * **Window** — the window the user was in (the island never takes the foreground), by DWM
//!   frame bounds.
//! * **Full screen** — the monitor the island is on.
//!
//! Window / full screen: the island hides for a few frames, one GDI `BitBlt` on a worker thread
//! (the island reappears as soon as the pixels are copied), PNG saved to `capture.save_dir`
//! (default `Pictures\Moku`) and copied to the clipboard. The "last capture" card offers Salin
//! and Buka folder.

pub(crate) mod grab;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::{ILFree, SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use super::clipboard::{imgutil, sys};
use crate::activity::{Activity, priority};
use crate::app::{App, Redraw, Ui};
use crate::i18n::{self, tr, tr_s};
use crate::imaging;
use crate::registry::*;
use crate::ui::*;

/// What "Area" launches: Windows' screen-clip overlay.
pub const SCREENCLIP_URI: &str = "ms-screenclip:";

/// How long the island stays hidden around a snip started from the panel (the overlay freezes
/// the screen almost at once; this keeps the pill out of the picture).
const AREA_HIDE_MS: u64 = 1500;
/// DWM needs a couple of frames to drop the island from the screen before we copy pixels.
const HIDE_SETTLE_MS: u64 = 140;
/// Longest side of the stored "last capture" thumbnail.
const THUMB_PX: u32 = 192;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Window,
    Screen,
}

/// The last capture (persisted in `capture.json`).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
struct Last {
    path: String,
    w: u32,
    h: u32,
    /// Unix seconds.
    ts: i64,
    kind: Kind,
    /// It was also put on the clipboard.
    copied: bool,
}

#[derive(Default)]
struct Capture {
    last: Option<Last>,
    /// Thumbnail of the last capture, loaded lazily while the tab is visible.
    thumb: Option<Image>,
    busy: bool,
    /// Short feedback line ("Salin" worked, no active window …).
    note: Option<(String, bool)>,
    note_token: u64,
}

pub fn register(r: &mut Registry) {
    r.add_state(Capture::default());
    r.add_tab(TabSpec {
        id: "capture",
        order: order::CAPTURE,
        icon: icons::CAMERA,
        label: ("Tangkap", "Capture"),
        enabled: |c| c.capture.enabled,
        build: panel,
        tick: None,
    });
    r.on_action("capture", on_action);
    r.on_start(on_start);
    r.on_event(on_event);
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn meta_path() -> PathBuf {
    moku_core::paths::data_file("capture.json")
}

fn thumb_path() -> PathBuf {
    moku_core::paths::data_file("capture-thumb.png")
}

fn on_start(app: &mut App) {
    let last: Option<Last> = std::fs::read(meta_path()).ok().and_then(|b| serde_json::from_slice(imgutil::strip_utf8_bom(&b)).ok());
    app.state_mut::<Capture>().last = last;
    app.request_redraw(Redraw::Tab("capture"));
}

fn on_event(app: &mut App, ev: &AppEvent) {
    match ev {
        AppEvent::TabShown("capture") => load_thumb(app),
        AppEvent::TabHidden("capture") => {
            let st = app.state_mut::<Capture>();
            st.thumb = None;
            st.note = None;
        }
        // "Hapus semua data": forget the "last capture" card (the screenshots in Pictures\Moku
        // are the user's files and stay).
        AppEvent::DataCleared => {
            let st = app.state_mut::<Capture>();
            st.last = None;
            st.thumb = None;
            st.note = None;
            app.request_redraw(Redraw::Tab("capture"));
        }
        _ => {}
    }
}

/// The small thumbnail file (≈ 20 KB) is decoded on the UI thread, only while the tab is shown.
fn load_thumb(app: &mut App) {
    if app.state::<Capture>().last.is_none() || !app.is_tab_visible("capture") {
        return;
    }
    if let Ok(bytes) = std::fs::read(thumb_path())
        && let Ok(img) = imaging::decode(&bytes, THUMB_PX)
    {
        app.state_mut::<Capture>().thumb = Some(imgutil::crop_to_aspect(&img, 3, 2));
    }
}

fn save_meta(last: &Option<Last>) {
    match last {
        Some(l) => {
            if let Ok(json) = serde_json::to_vec(l) {
                let _ = std::fs::create_dir_all(moku_core::paths::data_dir());
                if let Err(e) = std::fs::write(meta_path(), json) {
                    log_warn!("capture.json not saved: {e}");
                }
            }
        }
        None => {
            let _ = std::fs::remove_file(meta_path());
            let _ = std::fs::remove_file(thumb_path());
        }
    }
}

// ---- actions ----------------------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    match ev.name() {
        "area" => start_area(app),
        "window" => start_window(app),
        "screen" => start_screen(app),
        // Text from a region of the screen (feature `ocr`).
        "ocr" => app.dispatch_action("ocr.start", ActionKind::Click),
        "copy" => copy_last(app),
        "folder" => open_folder(app),
        _ => {}
    }
}

fn set_note(app: &mut App, text: String, error: bool, ms: u64) {
    let st = app.state_mut::<Capture>();
    st.note_token += 1;
    let token = st.note_token;
    st.note = Some((text, error));
    app.request_redraw(Redraw::Tab("capture"));
    app.after(Duration::from_millis(ms), move |app| {
        let st = app.state_mut::<Capture>();
        if st.note_token == token {
            st.note = None;
            app.request_redraw(Redraw::Tab("capture"));
        }
    });
}

/// Area: close the panel, keep the island out of the picture, and open Windows' snipping overlay
/// (it takes over, then puts the result on the clipboard, where the clipboard history sees it).
fn start_area(app: &mut App) {
    app.close_panel();
    app.hide_island(true);
    let launched = launch_uri(SCREENCLIP_URI);
    if !launched {
        log_warn!("capture: could not launch {SCREENCLIP_URI}");
    }
    app.after(Duration::from_millis(if launched { AREA_HIDE_MS } else { 50 }), |app| app.hide_island(false));
    if !launched {
        set_note(app, tr("Tidak bisa membuka alat tangkap Windows.", "Could not open the Windows snipping tool.").to_string(), true, 3000);
    }
}

/// `ShellExecute` on a URI (the shell routes `ms-screenclip:` to the snipping overlay).
fn launch_uri(uri: &str) -> bool {
    let h = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(uri), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    h.0 as isize > 32
}

fn start_window(app: &mut App) {
    match grab::foreground_window_rect() {
        Some(r) => begin(app, r, Kind::Window),
        None => set_note(app, tr("Tidak ada jendela aktif untuk ditangkap.", "No active window to capture.").to_string(), true, 3000),
    }
}

fn start_screen(app: &mut App) {
    match grab::monitor_rect_of(app.main_hwnd()) {
        Some(r) => begin(app, r, Kind::Screen),
        None => set_note(app, tr("Layar tidak ditemukan.", "Screen not found.").to_string(), true, 3000),
    }
}

/// Hide the island, wait for DWM, grab on a worker thread.
fn begin(app: &mut App, rect: RECT, kind: Kind) {
    if app.state::<Capture>().busy {
        return;
    }
    app.state_mut::<Capture>().busy = true;
    app.close_panel();
    app.hide_island(true);
    let (setting, copy) = (app.config().capture.save_dir.clone(), app.config().capture.copy_to_clipboard);
    let owner = app.main_hwnd().0 as isize;
    app.after(Duration::from_millis(HIDE_SETTLE_MS), move |app| {
        let spawned = std::thread::Builder::new().name("capture".into()).spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let g = grab::grab_rect(rect);
            // The pixels are in memory: the island may come back while the PNG is encoded.
            Ui::post(|app| app.hide_island(false));
            let result = match g {
                Some(g) => finish(g, &grab::save_dir(&setting), copy, HWND(owner as _), kind, &thumb_path()),
                None => Err("grab failed".to_string()),
            };
            Ui::post(move |app| done(app, result));
            unsafe {
                CoUninitialize();
            }
        });
        if spawned.is_err() {
            app.hide_island(false);
            app.state_mut::<Capture>().busy = false;
        }
    });
}

/// Encode, save, copy, write the thumbnail (worker thread).
fn finish(g: grab::Grab, dir: &Path, copy: bool, owner: HWND, kind: Kind, thumb_out: &Path) -> Result<Last, String> {
    let (w, h) = (g.w, g.h);
    let img = Image::new(w, h, g.bgra);
    let png = imaging::encode_png(&img).map_err(|e| format!("png: {e:?}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("folder: {e}"))?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H.%M.%S").to_string();
    let name = grab::unique_file_name(&stamp, |n| dir.join(n).exists());
    let path = dir.join(&name);
    std::fs::write(&path, &png).map_err(|e| format!("save: {e}"))?;
    let copied = copy && sys::put_image(owner, &png, Some(&img), false);
    if let Ok(t) = imaging::from_bgra(w, h, w * 4, &img.0.bgra, true, THUMB_PX)
        && let Ok(tp) = imaging::encode_png(&t)
    {
        let _ = std::fs::write(thumb_out, tp);
    }
    Ok(Last { path: path.to_string_lossy().to_string(), w, h, ts: now(), kind, copied })
}

/// Back on the UI thread.
fn done(app: &mut App, result: Result<Last, String>) {
    app.state_mut::<Capture>().busy = false;
    match result {
        Ok(last) => {
            log_info!("capture saved ({:?}, {}x{})", last.kind, last.w, last.h);
            save_meta(&Some(last.clone()));
            let st = app.state_mut::<Capture>();
            st.thumb = None;
            let (w, h) = (last.w, last.h);
            st.last = Some(last);
            app.set_activity(
                Activity::transient("capture.done", priority::OSD, 2.5, move |_| {
                    pill([icon(icons::CAMERA).color(color::TEAL), text(tr("Tangkapan tersimpan", "Capture saved")).semibold(), caption(format!("{w} × {h}"))])
                })
                .tab("capture"),
            );
            load_thumb(app);
        }
        Err(e) => {
            log_warn!("capture failed: {e}");
            set_note(app, tr("Tangkapan gagal.", "Capture failed.").to_string(), true, 3500);
        }
    }
    app.request_redraw(Redraw::All);
}

/// "Salin": put the saved PNG on the clipboard again.
fn copy_last(app: &mut App) {
    let Some(path) = app.state::<Capture>().last.as_ref().map(|l| l.path.clone()) else { return };
    let owner = app.main_hwnd().0 as isize;
    set_note(app, tr("Disalin ke clipboard", "Copied to clipboard").to_string(), false, 1800);
    let spawned = std::thread::Builder::new().name("capture-copy".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let ok = std::fs::read(&path).ok().is_some_and(|png| sys::put_image(HWND(owner as _), &png, None, true));
        unsafe {
            CoUninitialize();
        }
        if !ok {
            Ui::post(|app| set_note(app, tr("Berkas tangkapan tidak ditemukan.", "The capture file is gone.").to_string(), true, 3000));
        }
    });
    if spawned.is_err() {
        set_note(app, tr("Tidak bisa menyalin.", "Could not copy.").to_string(), true, 3000);
    }
}

/// "Buka folder": Explorer with the file selected (`SHOpenFolderAndSelectItems`; no other app).
fn open_folder(app: &mut App) {
    let Some(path) = app.state::<Capture>().last.as_ref().map(|l| l.path.clone()) else { return };
    if !Path::new(&path).exists() {
        let st = app.state_mut::<Capture>();
        st.last = None;
        st.thumb = None;
        save_meta(&None);
        set_note(app, tr("Berkas tangkapan sudah tidak ada.", "The capture file no longer exists.").to_string(), true, 3000);
        return;
    }
    // Talks to Explorer and may pump messages: run it once the app borrow has ended.
    app.run_outside(move || {
        if !select_in_explorer(&path) {
            Ui::post(|app| set_note(app, tr("Tidak bisa membuka folder.", "Could not open the folder.").to_string(), true, 3000));
        }
    });
}

fn select_in_explorer(path: &str) -> bool {
    unsafe {
        let mut pidl = std::ptr::null_mut();
        if SHParseDisplayName(&HSTRING::from(path), None::<&windows::Win32::System::Com::IBindCtx>, &mut pidl, 0, None).is_err() || pidl.is_null() {
            return false;
        }
        // No children: `pidl` itself is the item to select inside its folder.
        let ok = SHOpenFolderAndSelectItems(pidl, None, 0).is_ok();
        ILFree(Some(pidl as *const _));
        ok
    }
}

// ---- panel ------------------------------------------------------------------------------------

fn mode_tile(glyph: &str, title: &str, sub: &str, action: Option<&str>, tint: Option<Rgba>) -> Node {
    let mut n = col([icon(glyph).font_size(22.0).color(tint.unwrap_or(color::WHITE)), text(title).semibold(), caption(sub).font_size(10.5)])
        .align(Align::Center)
        .justify(Justify::Center)
        .gap(3.0)
        .h(84.0)
        .grow(1.0)
        .radius(18.0)
        .bg(tint.map_or(color::FILL, |t| t.alpha(0.12)));
    if let Some(t) = tint {
        n = n.border(t.alpha(0.35));
    }
    match action {
        Some(a) => n.action(a),
        None => n.opacity(0.5),
    }
}

fn panel(app: &App) -> Node {
    let st = app.state::<Capture>();
    let tiles = row([
        mode_tile(icons::CROP, tr("Area", "Area"), "Win + Shift + S", Some("capture.area"), None),
        mode_tile(icons::WINDOW, tr("Jendela", "Window"), tr("Jendela aktif", "Active window"), Some("capture.window"), None),
        mode_tile(icons::DESKTOP, tr("Layar penuh", "Full screen"), tr("Seluruh layar", "Whole screen"), Some("capture.screen"), None),
        mode_tile(icons::DOCUMENT, tr("Teks (OCR)", "Text (OCR)"), &super::ocr::active_hotkey(app).replace('+', " + "), app.config().ocr.enabled.then_some("capture.ocr"), None),
        mode_tile(icons::RECORD, tr("Rekam", "Record"), tr("Segera hadir", "Coming soon"), None, Some(color::RED)),
    ])
    .gap(10.0)
    .fill_w()
    .align(Align::Stretch);

    let mut kids = vec![tiles];
    if st.busy {
        kids.push(caption(tr("Mengambil tangkapan…", "Capturing…")));
    } else if let Some((msg, err)) = &st.note
        && st.last.is_none()
    {
        kids.push(caption(msg.clone()).color(if *err { color::RED } else { app.base_accent() }));
    }
    match &st.last {
        Some(l) => kids.push(last_card(app, st, l)),
        None if !st.busy && st.note.is_none() => kids.push(caption(tr("Belum ada tangkapan. Pilih salah satu di atas.", "No captures yet. Pick one above."))),
        None => {}
    }
    col(kids).gap(10.0).fill_w()
}

fn last_card(app: &App, st: &Capture, l: &Last) -> Node {
    let thumb = match &st.thumb {
        Some(img) => image(img, 12.0).size(96.0, 64.0),
        None => stack([icon(icons::PHOTO).font_size(20.0)]).size(96.0, 64.0).bg(color::BLUE).radius(12.0),
    };
    let kind = match l.kind {
        Kind::Window => tr("Jendela", "Window"),
        Kind::Screen => tr("Layar penuh", "Full screen"),
    };
    let when = chrono::DateTime::from_timestamp(l.ts, 0).map(|t| i18n::clock(&t.with_timezone(&chrono::Local))).unwrap_or_default();
    let saved = if l.copied { tr("tersimpan ke clipboard", "saved to clipboard") } else { tr("tersimpan", "saved") };
    let detail = tr_s(format!("{kind} {} × {} · {when} · {saved}", l.w, l.h), format!("{kind} {} × {} · {when} · {saved}", l.w, l.h));
    let mut info = vec![text(tr("Tangkapan terakhir", "Last capture")).semibold(), caption(detail)];
    if let Some((msg, err)) = &st.note {
        info.push(caption(msg.clone()).color(if *err { color::RED } else { app.base_accent() }));
    } else {
        info.push(row([capsule(icons::COPY, tr("Salin", "Copy"), "capture.copy"), capsule(icons::FOLDER, tr("Buka folder", "Show in folder"), "capture.folder")]).gap(8.0));
    }
    row([thumb, col(info).gap(5.0).grow(1.0)]).gap(14.0).fill_w().pad(12.0).bg(color::FILL).radius(typo::RADIUS_CARD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_launches_the_windows_screen_clip_uri() {
        assert_eq!(SCREENCLIP_URI, "ms-screenclip:");
    }

    /// The save path end to end (without a real screen): PNG on disk with the right size, a
    /// second capture in the same second never overwrites the first, the thumbnail is written,
    /// and `copy = false` leaves the clipboard alone.
    #[test]
    fn capture_saves_a_png_and_never_overwrites() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let dir = std::env::temp_dir().join(format!("moku-capture-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let thumb = dir.join("thumb.png");
        let grab = |w: u32, h: u32| {
            let mut bgra = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    bgra.extend([(x % 256) as u8, (y % 256) as u8, 128, 255]);
                }
            }
            grab::Grab { w, h, bgra }
        };
        let a = finish(grab(320, 200), &dir.join("shots"), false, HWND::default(), Kind::Screen, &thumb).unwrap();
        let b = finish(grab(64, 48), &dir.join("shots"), false, HWND::default(), Kind::Window, &thumb).unwrap();
        assert_ne!(a.path, b.path);
        assert!(!a.copied && !b.copied);
        let bytes = std::fs::read(&a.path).unwrap();
        assert_eq!(&bytes[1..4], b"PNG");
        let img = imaging::decode(&bytes, 4096).unwrap();
        assert_eq!((img.0.width, img.0.height), (320, 200));
        assert_eq!((a.w, a.h, a.kind), (320, 200, Kind::Screen));
        assert!(Path::new(&a.path).starts_with(dir.join("shots")));
        // The thumbnail is the last capture's, at most 192 px on its long side.
        let t = imaging::decode(&std::fs::read(&thumb).unwrap(), 4096).unwrap();
        assert_eq!((t.0.width, t.0.height), (64, 48));
        // A folder that cannot be created (its parent is a file) is an error, not a panic.
        let blocked = thumb.join("sub");
        assert!(finish(grab(8, 8), &blocked, false, HWND::default(), Kind::Screen, &thumb).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn last_capture_survives_a_json_roundtrip_and_rejects_junk() {
        let l = Last { path: r"C:\Users\ana\Pictures\Moku\Moku 2026-11-24 10.15.30.png".into(), w: 1920, h: 1080, ts: 1_800_000_000, kind: Kind::Screen, copied: true };
        let json = serde_json::to_vec(&l).unwrap();
        assert_eq!(serde_json::from_slice::<Last>(&json).unwrap(), l);
        assert!(serde_json::from_slice::<Last>(b"{").is_err());
        assert!(serde_json::from_slice::<Last>(br#"{"path":1}"#).is_err());
    }
}
