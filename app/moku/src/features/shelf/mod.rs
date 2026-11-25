//! Feature `shelf` (Task 5, track D): drop files on the island, keep them there, drag them back
//! out. The shelf stores **paths only**; it never copies, moves or deletes a user's file.
//!
//! * `dnd.rs` — the OLE drop target on the island's hit window and `DoDragDrop` for dragging out.
//! * `model.rs` — pure list model (dedupe, caps, persistence) with the unit tests.
//! * this file — state, drop state of the pill, panel, actions.

pub mod convert;
mod dnd;
mod model;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use self::convert::Target;
use self::model::*;
pub use self::model::fmt_size;
use super::clipboard::imgutil::crop_to_aspect;
use moku_core::config::Animations;

use crate::activity::priority;
use crate::app::{App, Redraw, TimerId, Ui, with_app_or_defer};
use crate::i18n::{self, tr, tr_s};
use crate::imaging;
use crate::registry::*;
use crate::ui::*;

/// Tiles per row and rows: 8 tiles at most (7 files + the "drop here" tile, or 8 files).
const COLS: usize = 4;
const MAX_TILES: usize = 8;
const TILE_W: f32 = 145.0;
const TILE_H: f32 = 84.0;
/// Largest image file decoded for a tile preview.
const MAX_THUMB_FILE: u64 = 24 * 1024 * 1024;

/// Motion timings (ms). The animations themselves run in the compositor.
/// A drag that leaves without dropping restores the panel after this long (a quick wobble back
/// in keeps it open).
const LEAVE_RESTORE_MS: u64 = 250;
/// After a drop the panel stays this long to show the new tiles (longer while the pointer rests
/// on it).
const DROP_HOLD_MS: u64 = 1400;
/// New tiles pop in one after another.
const ENTER_STAGGER: f32 = 40.0;
/// "Kosongkan": tiles fly into the bin one after another, then the bin wiggles.
const FLY_STAGGER: f32 = 30.0;
const FLY_MS: f32 = 340.0;
const WIGGLE_MS: u64 = 420;
/// Key of the trash button (the target the tiles fly into).
const TRASH: &str = "shelf.trash";

/// What the panel showed when a drag opened it (restored when the drag leaves without a drop).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Before {
    Closed,
    Tab(&'static str),
}

#[derive(Default)]
struct Shelf {
    list: ShelfList,
    /// A drag carrying files hovers over the island: show the drop state.
    dragging: bool,
    registered: bool,
    /// Tile previews of image files, loaded lazily while the tab is visible.
    thumbs: HashMap<String, Image>,
    thumbs_pending: HashSet<String>,
    /// Set while a drag holds the panel open (from DragEnter until the drop / the restore).
    before: Option<Before>,
    /// The drag left: restore `before` when this fires (cancelled if it comes back in time).
    leave_timer: Option<TimerId>,
    /// Keys of the files the last drop added, in order (their entrance is staggered).
    fresh: Vec<String>,
    /// Pulse generation per key: a file dropped again that was on the shelf already.
    pulses: HashMap<String, u32>,
    /// Keys flying into the bin ("Kosongkan"); forgotten when the animation is over.
    clearing: Option<Vec<String>>,
    /// Bumped to wiggle the bin after the tiles landed in it.
    trash_pulse: u32,
    /// The tile the user clicked (its path key): the bar under the grid acts on it.
    selected: Option<String>,
    /// "Ubah ke..." is open: the bar shows the formats instead.
    convert_menu: bool,
    /// Checked when the menu opens (the extensions can be installed while Moku runs).
    webp_encoder: bool,
    heif_missing: bool,
    /// The one conversion in progress (a progress ring shows on its tile).
    converting: Option<Converting>,
    convert_seq: u64,
    /// The last conversion's output (the done pill's "Folder" shows it in Explorer).
    last_result: Option<PathBuf>,
}

struct Converting {
    id: u64,
    key: String,
    target: Target,
    /// 0-1.
    progress: f32,
    cancel: Arc<AtomicBool>,
}

pub fn register(r: &mut Registry) {
    r.add_state(Shelf::default());
    r.add_tab(TabSpec {
        id: "shelf",
        order: order::SHELF,
        icon: icons::SHELF,
        label: ("Shelf", "Shelf"),
        enabled: |c| c.shelf.enabled,
        build: panel,
        tick: None,
    });
    r.on_action("shelf", on_action);
    r.on_start(on_start);
    r.on_exit(on_exit);
    r.on_event(on_event);
}

/// The `n` files most recently put on the Shelf, newest first (paths; for the launcher).
pub fn recent_files(app: &App, n: usize) -> Vec<String> {
    let mut v: Vec<&ShelfItem> = app.state::<Shelf>().list.items.iter().collect();
    v.sort_by(|a, b| b.added.cmp(&a.added));
    v.into_iter().take(n).map(|i| i.path.clone()).collect()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn index_path() -> std::path::PathBuf {
    moku_core::paths::shelf_dir().join("shelf.json")
}

/// Save the list soon (debounced, written off the UI thread; flushed on exit).
fn save(app: &mut App) {
    app.save_later("shelf", |app| Some((index_path(), app.state::<Shelf>().list.to_json())));
}

// ---- lifecycle --------------------------------------------------------------------------------

fn on_start(app: &mut App) {
    app.state_mut::<Shelf>().list = ShelfList::load(&index_path());
    // Register once: `hit_hwnd` keeps its handle while its shape changes (None in --state modes).
    if let Some(hwnd) = app.hit_hwnd() {
        let ok = dnd::register(hwnd, || crate::app::current_config().shelf.enabled, on_dnd);
        app.state_mut::<Shelf>().registered = ok;
    }
    app.request_redraw(Redraw::Tab("shelf"));
}

/// Drop-target events arrive on the UI thread from inside OLE's own message handling: run them
/// now, or right after the borrow of the app that is in progress.
fn on_dnd(ev: dnd::DndEvent) {
    with_app_or_defer(move |app| match ev {
        dnd::DndEvent::Enter => drag_enter(app),
        dnd::DndEvent::Leave => drag_leave(app, false),
        dnd::DndEvent::Drop(paths) => dropped(app, paths),
    });
}

fn on_exit(app: &mut App) {
    if let Some(h) = app.hit_hwnd() {
        dnd::revoke(h);
    }
    app.state_mut::<Shelf>().registered = false;
    save(app);
}

fn on_event(app: &mut App, ev: &AppEvent) {
    match ev {
        AppEvent::TabShown("shelf") => {
            refresh(app);
            ensure_thumbs(app);
        }
        AppEvent::TabHidden("shelf") => {
            let st = app.state_mut::<Shelf>();
            st.thumbs = HashMap::new();
            st.thumbs_pending.clear();
            st.fresh.clear();
            st.selected = None;
            st.convert_menu = false;
        }
        // The settings app deletes shelf.json right after (the parked files themselves are the
        // user's and stay where they are).
        AppEvent::DataCleared => {
            app.cancel_save("shelf");
            let st = app.state_mut::<Shelf>();
            st.list = ShelfList::default();
            st.thumbs = HashMap::new();
            st.thumbs_pending.clear();
            st.clearing = None;
            st.fresh.clear();
            st.pulses.clear();
            st.selected = None;
            st.convert_menu = false;
            st.last_result = None;
            app.request_redraw(Redraw::Tab("shelf"));
        }
        _ => {}
    }
}

// ---- drag & drop state --------------------------------------------------------------------------

/// A file drag entered the island: morph straight into the panel on the Shelf tab (switching
/// tabs if it was open on another one), with the drop zone highlighted. Our own drag out of the
/// shelf never gets here (`dnd` refuses it).
fn drag_enter(app: &mut App) {
    if app.state::<Shelf>().dragging {
        return;
    }
    match app.state_mut::<Shelf>().leave_timer.take() {
        // Back in before the restore: keep the hold and what to go back to.
        Some(t) => app.cancel_timer(t),
        None => {
            let before = match app.current_tab() {
                Some(t) => Before::Tab(t),
                None => Before::Closed,
            };
            app.state_mut::<Shelf>().before = Some(before);
            app.hold_panel(true);
        }
    }
    app.state_mut::<Shelf>().dragging = true;
    // A feature's open_panel: not the user's tab choice, so reopening later still goes to the
    // tab they had chosen.
    app.open_panel(Some("shelf"));
    app.request_redraw(Redraw::All);
}

/// The drag left without a drop (`dropped` = false: go back to what was shown before after
/// [`LEAVE_RESTORE_MS`]) or ended in a drop (the panel stays a moment to show the new files).
fn drag_leave(app: &mut App, dropped: bool) {
    if !app.state::<Shelf>().dragging {
        return;
    }
    app.state_mut::<Shelf>().dragging = false;
    if dropped {
        app.state_mut::<Shelf>().before = None;
        app.after(Duration::from_millis(DROP_HOLD_MS), |app| app.hold_panel(false));
    } else {
        let t = app.after(Duration::from_millis(LEAVE_RESTORE_MS), restore_after_leave);
        app.state_mut::<Shelf>().leave_timer = Some(t);
    }
    app.request_redraw(Redraw::All);
}

/// The drag is really gone: collapse if it opened the panel, else back to the user's tab.
fn restore_after_leave(app: &mut App) {
    let st = app.state_mut::<Shelf>();
    st.leave_timer = None;
    let before = st.before.take();
    match before {
        Some(Before::Tab(t)) => {
            if app.is_panel_open() {
                app.open_panel(Some(t));
            }
            app.hold_panel(false);
        }
        _ => {
            app.hold_panel(false);
            if app.is_panel_open() {
                app.close_panel();
            }
        }
    }
}

/// Files were dropped on the island (UI thread; also the debug path).
fn dropped(app: &mut App, paths: Vec<String>) {
    let st = app.state_mut::<Shelf>();
    let d = st.list.drop_paths(paths, now());
    let added = d.added.len();
    // New tiles pop in staggered; files that were here already pulse instead.
    st.fresh = d.added;
    for k in d.already {
        *st.pulses.entry(k).or_default() += 1;
    }
    drag_leave(app, true);
    if added > 0 {
        save(app);
        refresh(app);
        ensure_thumbs(app);
        app.alert("shelf.added", priority::OSD, 2.2, move |_| {
            pill([
                icon(icons::SHELF).color(color::TEAL),
                text(tr_s(format!("Disimpan ke Shelf · {added} file"), format!("Saved to Shelf · {added} file{}", if added == 1 { "" } else { "s" }))).semibold(),
            ])
        });
    }
    app.request_redraw(Redraw::Tab("shelf"));
}

/// Put one file on the shelf: the "Simpan ke Shelf" shortcut of a finished download. The shelf
/// keeps the path only; the file stays where it is. Does nothing while the shelf is off.
pub fn save_path(app: &mut App, path: String) {
    if app.config().shelf.enabled {
        dropped(app, vec![path]);
    }
}

/// A file that arrived from the phone, or that a conversion made: put it on the shelf without the
/// "Disimpan ke Shelf" pill (the caller has its own). The file stays where it is.
pub fn add_received(app: &mut App, path: String) {
    if !app.config().shelf.enabled {
        return;
    }
    let st = app.state_mut::<Shelf>();
    let d = st.list.drop_paths([path], now());
    let added = !d.added.is_empty();
    // New files go to the front: the grid shows the first eight, and what just arrived is what the
    // user looks for.
    st.list.items.rotate_right(d.added.len());
    st.fresh.extend(d.added);
    for k in d.already {
        *st.pulses.entry(k).or_default() += 1;
    }
    if added {
        save(app);
        refresh(app);
        ensure_thumbs(app);
    }
    app.request_redraw(Redraw::Tab("shelf"));
}

// ---- "Kosongkan" ------------------------------------------------------------------------------

/// The trash button: the tiles fly into it (staggered), it wiggles, and only then the shelf is
/// emptied (paths only: the files stay where they are) and the empty state fades in.
fn start_clear(app: &mut App) {
    let st = app.state::<Shelf>();
    if st.clearing.is_some() || st.list.items.is_empty() {
        return;
    }
    let keys: Vec<String> = st.list.items.iter().map(|i| path_key(&i.path)).collect();
    if app.animations() == Animations::Off || !app.is_tab_visible("shelf") {
        return finish_clear(app, keys);
    }
    let shown = keys.len().min(MAX_TILES);
    app.state_mut::<Shelf>().clearing = Some(keys);
    app.request_redraw(Redraw::Tab("shelf"));
    let fly = (FLY_STAGGER * (shown.saturating_sub(1)) as f32 + FLY_MS) as u64;
    app.after(Duration::from_millis(fly), |app| {
        if app.state::<Shelf>().clearing.is_none() {
            return;
        }
        app.state_mut::<Shelf>().trash_pulse += 1;
        app.request_redraw(Redraw::Tab("shelf"));
        app.after(Duration::from_millis(WIGGLE_MS), |app| {
            if let Some(keys) = app.state_mut::<Shelf>().clearing.take() {
                finish_clear(app, keys);
            }
        });
    });
}

fn finish_clear(app: &mut App, keys: Vec<String>) {
    let st = app.state_mut::<Shelf>();
    st.list.remove_keys(&keys);
    st.thumbs.retain(|k, _| !keys.contains(k));
    st.pulses.retain(|k, _| !keys.contains(k));
    st.clearing = None;
    save(app);
    app.request_redraw(Redraw::Tab("shelf"));
}

// ---- background work ----------------------------------------------------------------------------

/// `stat` every path off the UI thread: fills sizes, drops files that no longer exist.
fn refresh(app: &mut App) {
    let paths: Vec<String> = app.state::<Shelf>().list.items.iter().map(|i| i.path.clone()).collect();
    if paths.is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new().name("shelf-stat".into()).spawn(move || {
        let stats: Vec<(String, Stat)> = paths.iter().map(|p| (path_key(p), stat_path(p))).collect();
        Ui::post(move |app| {
            let removed = app.state_mut::<Shelf>().list.apply_stats(&stats);
            if removed > 0 {
                save(app);
            }
            ensure_thumbs(app);
            app.request_redraw(Redraw::Tab("shelf"));
        });
    });
    if spawned.is_err() {
        log_warn!("shelf: could not start the stat thread");
    }
}

/// Decode previews of the image files that have a tile (one worker for all of them).
fn ensure_thumbs(app: &mut App) {
    if !app.is_tab_visible("shelf") {
        return;
    }
    let st = app.state_mut::<Shelf>();
    let want: Vec<(String, String)> = st
        .list
        .items
        .iter()
        .take(MAX_TILES)
        .filter(|i| !i.is_dir && is_thumbnailable(&i.path))
        .map(|i| (path_key(&i.path), i.path.clone()))
        .filter(|(k, _)| !st.thumbs.contains_key(k) && !st.thumbs_pending.contains(k))
        .collect();
    if want.is_empty() {
        return;
    }
    for (k, _) in &want {
        st.thumbs_pending.insert(k.clone());
    }
    let spawned = std::thread::Builder::new().name("shelf-thumbs".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        for (key, path) in want {
            let img = load_thumb(&path);
            Ui::post(move |app| {
                let visible = app.is_tab_visible("shelf");
                let st = app.state_mut::<Shelf>();
                st.thumbs_pending.remove(&key);
                if let (Some(img), true) = (img, visible) {
                    st.thumbs.insert(key, img);
                    app.request_redraw(Redraw::Tab("shelf"));
                }
            });
        }
        unsafe {
            CoUninitialize();
        }
    });
    if spawned.is_err() {
        app.state_mut::<Shelf>().thumbs_pending.clear();
    }
}

fn load_thumb(path: &str) -> Option<Image> {
    if std::fs::metadata(path).ok()?.len() > MAX_THUMB_FILE {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let img = imaging::decode(&bytes, 192).ok()?;
    Some(crop_to_aspect(&img, TILE_W as u32, TILE_H as u32))
}

// ---- actions ----------------------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    let idx = ev.arg().and_then(|a| a.parse::<usize>().ok());
    match (ev.name(), ev.kind, idx) {
        ("clear", ActionKind::Click, _) => start_clear(app),
        // A click on a tile selects it (a press that moves is a drag, below).
        ("file", ActionKind::Click, Some(i)) => select(app, i),
        ("send", ActionKind::Click, _) => send_selected(app),
        ("convert_menu", ActionKind::Click, _) => toggle_convert_menu(app),
        ("convert", ActionKind::Click, _) => {
            if let Some(t) = ev.arg().and_then(Target::from_id) {
                start_convert(app, t);
            }
        }
        ("cancel_convert", ActionKind::Click, _) => {
            if let Some(c) = &app.state::<Shelf>().converting {
                c.cancel.store(true, Ordering::Relaxed);
            }
        }
        ("reveal", ActionKind::Click, _) => {
            if let Some(p) = app.state::<Shelf>().last_result.clone() {
                app.clear_activity("shelf.converted");
                let _ = std::thread::Builder::new().name("shelf-reveal".into()).spawn(move || {
                    if !crate::safety::reveal(&p) {
                        log_warn!("shelf: could not show the converted file in its folder");
                    }
                });
            }
        }
        ("open_store", ActionKind::Click, _) => open_store(),
        // Debug: a conversion ring on tile `i` at `p` per cent (`shelf.debug_converting:0:42`; `:off`).
        ("debug_converting", ActionKind::Click, _) => debug_converting(app, ev.arg().unwrap_or("")),
        ("remove", ActionKind::Click, Some(i)) => {
            let st = app.state_mut::<Shelf>();
            st.list.remove(i);
            save(app);
            ensure_thumbs(app);
            app.request_redraw(Redraw::Tab("shelf"));
        }
        ("file", ActionKind::DragStart, Some(i)) => {
            let Some(path) = app.state::<Shelf>().list.items.get(i).map(|f| f.path.clone()) else { return };
            // The drag is modal: start it after this borrow of the app ends, and keep the panel
            // open until it is over.
            app.hold_panel(true);
            app.run_outside(move || {
                dnd::drag_out(&path);
                Ui::post(|app| app.hold_panel(false));
            });
        }
        // Debug helpers (`mokuctl debug click "shelf.debug_drop:C:\a.txt|C:\b.png"`): the same
        // code path as a real drop, without a real drag.
        ("debug_drop", ActionKind::Click, _) => {
            if let Some(a) = ev.arg() {
                dropped(app, a.split('|').map(str::to_string).collect());
            }
        }
        ("debug_enter", ActionKind::Click, _) => drag_enter(app),
        ("debug_leave", ActionKind::Click, _) => drag_leave(app, false),
        _ => {}
    }
}

// ---- selection, "Kirim ke HP" and "Ubah ke..." ------------------------------------------------------

fn selected_item(st: &Shelf) -> Option<&ShelfItem> {
    let k = st.selected.as_ref()?;
    st.list.items.iter().find(|i| path_key(&i.path) == *k)
}

/// Click on tile `i`: select it (click again to let go); the bar under the grid acts on it.
fn select(app: &mut App, i: usize) {
    let st = app.state_mut::<Shelf>();
    let Some(key) = st.list.items.get(i).map(|f| path_key(&f.path)) else { return };
    st.selected = if st.selected.as_deref() == Some(key.as_str()) { None } else { Some(key) };
    st.convert_menu = false;
    app.request_redraw(Redraw::Tab("shelf"));
}

fn send_selected(app: &mut App) {
    let st = app.state_mut::<Shelf>();
    let Some(item) = selected_item(st).filter(|i| !i.is_dir) else { return };
    let path = item.path.clone();
    st.selected = None;
    st.convert_menu = false;
    super::phone_send::start_send(app, vec![path]);
}

fn toggle_convert_menu(app: &mut App) {
    let st = app.state_mut::<Shelf>();
    if st.converting.is_some() {
        return;
    }
    st.convert_menu = !st.convert_menu;
    if st.convert_menu {
        // Looked at every time the menu opens: the Store extensions can be installed while Moku runs.
        st.webp_encoder = convert::webp_encoder_available();
        let heic = selected_item(st).is_some_and(|i| matches!(i.path.rsplit('.').next().map(str::to_ascii_lowercase).as_deref(), Some("heic" | "heif")));
        st.heif_missing = heic && !convert::heif_decoder_available();
    }
    app.request_redraw(Redraw::Tab("shelf"));
}

/// Convert the selected file to `target` on a worker thread; the tile shows a progress ring.
fn start_convert(app: &mut App, target: Target) {
    let st = app.state_mut::<Shelf>();
    if st.converting.is_some() {
        return;
    }
    let Some(item) = selected_item(st).filter(|i| !i.is_dir) else { return };
    let (path, key) = (item.path.clone(), path_key(&item.path));
    if !convert::targets_for(&path, st.webp_encoder).contains(&target) {
        return;
    }
    st.convert_seq += 1;
    let id = st.convert_seq;
    let cancel = Arc::new(AtomicBool::new(false));
    st.converting = Some(Converting { id, key, target, progress: 0.0, cancel: cancel.clone() });
    st.convert_menu = false;
    app.request_redraw(Redraw::Tab("shelf"));
    let spawned = std::thread::Builder::new().name("shelf-convert".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        // The ring moves about 8 times a second, not on every band.
        let last = std::sync::Mutex::new((std::time::Instant::now(), 0.0f32));
        let progress: convert::Progress = Arc::new(move |p| {
            let mut l = last.lock().unwrap_or_else(|e| e.into_inner());
            if p >= 1.0 || (p - l.1 >= 0.01 && l.0.elapsed() >= Duration::from_millis(120)) {
                *l = (std::time::Instant::now(), p);
                Ui::post(move |app| {
                    if let Some(c) = app.state_mut::<Shelf>().converting.as_mut().filter(|c| c.id == id) {
                        c.progress = p;
                        app.request_redraw(Redraw::Tab("shelf"));
                    }
                });
            }
        });
        let result = convert::convert(std::path::Path::new(&path), target, &super::phone_send::receive_dir(), &progress, &cancel);
        unsafe {
            CoUninitialize();
        }
        Ui::post(move |app| convert_done(app, id, result));
    });
    if spawned.is_err() {
        log_warn!("shelf: could not start the converter thread");
        app.state_mut::<Shelf>().converting = None;
    }
}

fn convert_done(app: &mut App, id: u64, result: Result<PathBuf, convert::ConvertError>) {
    let st = app.state_mut::<Shelf>();
    if st.converting.as_ref().is_none_or(|c| c.id != id) {
        return;
    }
    st.converting = None;
    st.selected = None;
    app.request_redraw(Redraw::Tab("shelf"));
    match result {
        Ok(out) => {
            let name = out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let hide = app.meeting().active && app.config().privacy.hide_notification_body_in_meetings;
            app.state_mut::<Shelf>().last_result = Some(out.clone());
            add_received(app, out.to_string_lossy().into_owned());
            app.alert("shelf.converted", priority::OSD, 8.0, move |_| {
                let shown = if hide { tr("File baru", "New file").to_string() } else { name.clone() };
                pill([
                    stack([icon(icons::CHECK).font_size(10.0).color(color::INK)]).size(20.0, 20.0).bg(color::GREEN).capsule(),
                    col([text(tr("Selesai diubah", "Converted")).semibold(), text(shown).font_size(12.0).secondary()]).gap(1.0).grow(1.0),
                    capsule(icons::FOLDER, tr("Folder", "Folder"), "shelf.reveal").name(tr("Tampilkan di folder", "Show in folder")),
                ])
                .max_w(520.0)
            });
        }
        Err(convert::ConvertError::Cancelled) => {}
        Err(e) => {
            // Never a file name in the log: the kind of failure only.
            log_warn!("shelf: conversion failed ({})", match &e {
                convert::ConvertError::MissingCodec(w) => format!("missing codec {w}"),
                convert::ConvertError::BadSource => "bad source".into(),
                convert::ConvertError::NoEncoder(w) => format!("no encoder {w}"),
                convert::ConvertError::Failed(c) => c.clone(),
                convert::ConvertError::Cancelled => String::new(),
            });
            let (msg, store) = match &e {
                convert::ConvertError::MissingCodec(w) if *w != "codec" => (tr_s(format!("{w} butuh ekstensi gratis dari Microsoft Store"), format!("{w} needs a free extension from the Microsoft Store")), true),
                convert::ConvertError::MissingCodec(_) => (tr("Windows tidak punya codec untuk file ini", "Windows has no codec for this file").to_string(), false),
                convert::ConvertError::BadSource => (tr("File ini tidak bisa dibaca", "This file can not be read").to_string(), false),
                convert::ConvertError::NoEncoder(w) => (tr_s(format!("Windows tidak bisa membuat {w}"), format!("Windows can not create {w}")), false),
                _ => (tr("Gagal mengubah file", "Could not convert the file").to_string(), false),
            };
            app.alert("shelf.converted", priority::OSD, 9.0, move |_| {
                let mut kids = vec![icon(icons::WARNING).color(color::ORANGE), text(msg.clone()).semibold().grow(1.0)];
                if store {
                    kids.push(capsule(icons::SEARCH, tr("Buka Store", "Open Store"), "shelf.open_store"));
                }
                pill(kids).max_w(600.0)
            });
        }
    }
}

/// The Microsoft Store's search for the HEIF extension (a click on "Buka Store"; nothing is installed).
fn open_store() {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};
    unsafe {
        let _ = ShellExecuteW(None, w!("open"), &HSTRING::from("ms-windows-store://search/?query=HEIF%20Image%20Extensions"), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// `shelf.debug_converting:<tile>:<percent>` shows a conversion ring on a tile without converting
/// anything (screenshots); `:off` clears it.
fn debug_converting(app: &mut App, arg: &str) {
    let st = app.state_mut::<Shelf>();
    if arg == "off" || arg.is_empty() {
        st.converting = None;
    } else if let Some((i, p)) = arg.split_once(':')
        && let (Ok(i), Ok(p)) = (i.parse::<usize>(), p.parse::<f32>())
        && let Some(key) = st.list.items.get(i).map(|f| path_key(&f.path))
    {
        st.converting = Some(Converting { id: u64::MAX, key, target: Target::Jpg, progress: (p / 100.0).clamp(0.0, 1.0), cancel: Arc::new(AtomicBool::new(false)) });
    }
    app.request_redraw(Redraw::Tab("shelf"));
}

// ---- panel ------------------------------------------------------------------------------------

fn style(c: Category) -> (&'static str, Rgba) {
    match c {
        Category::Document => (icons::DOCUMENT, Rgba::hex(0xE5575F, 1.0)),
        Category::Image => (icons::PHOTO, Rgba::hex(0x2F9BFF, 1.0)),
        Category::Video => (icons::VIDEO, Rgba::hex(0x8B63F5, 1.0)),
        Category::Audio => (icons::MUSIC, Rgba::hex(0xF0518A, 1.0)),
        Category::Archive => (icons::DOWNLOAD, Rgba::hex(0xE8913A, 1.0)),
        Category::Code => (icons::CODE, Rgba::hex(0x2DB5A0, 1.0)),
        Category::Folder => (icons::FOLDER, Rgba::hex(0xE3A72F, 1.0)),
        Category::Other => (icons::DOCUMENT, Rgba::hex(0x6E7686, 1.0)),
    }
}

fn panel(app: &App) -> Node {
    // While a phone transfer is open its card (QR code, approval, progress) takes the place of the grid.
    if let Some(session) = super::phone_send::panel(app) {
        return col([session]).fill_w();
    }
    let st = app.state::<Shelf>();
    let accent = app.base_accent();
    let phone = super::phone_send::enabled(app);
    if st.list.items.is_empty() {
        // Keyed: fades in after "Kosongkan", fades out under the first tiles.
        let mut kids = vec![empty_drop(st.dragging, accent).key("shelf.empty").enter(Anim::fade().ms(240.0))];
        if phone {
            kids.push(row([spacer(), receive_button()]).fill_w());
        }
        return col(kids).gap(8.0).fill_w();
    }
    let english = i18n::is_english();
    let mut cells: Vec<Node> = st.list.items.iter().take(MAX_TILES).enumerate().map(|(i, f)| animated_tile(st, i, f, english, accent)).collect();
    let has_drop_tile = cells.len() < MAX_TILES;
    if has_drop_tile {
        // Keyed too, so it glides when tiles come and go.
        cells.push(add_tile(st.dragging, accent).key("shelf.add"));
    }
    let mut children: Vec<Node> = cells.chunks(COLS).map(|c| row(c.to_vec()).gap(10.0).fill_w().align(Align::Start)).collect();

    let n = st.list.items.len();
    let total = st.list.total_size();
    let mut info = tr_s(format!("{n} file"), format!("{n} file{}", if n == 1 { "" } else { "s" }));
    if total > 0 {
        info.push_str(&format!(" · {}", fmt_size(total, english)));
    }
    info.push_str(&format!(" · {}", tr("tersimpan sementara", "kept for now")));
    if n > MAX_TILES {
        info.push_str(&tr_s(format!(" · +{} lagi", n - MAX_TILES), format!(" · +{} more", n - MAX_TILES)));
    }
    // A full grid has no drop tile: while a drag hovers, the info line becomes the drop zone.
    let info = if st.dragging && !has_drop_tile {
        row([icon(icons::DOWNLOAD).font_size(13.0), caption(tr("Lepas untuk simpan", "Release to save")).color(color::WHITE)])
            .gap(6.0)
            .pad_xy(12.0, 0.0)
            .h(28.0)
            .bg(accent.alpha(0.25))
            .border(accent)
            .dashed()
            .capsule()
    } else {
        caption(info)
    };
    // The bar for the selected tile: "Kirim ke HP", "Ubah ke...", or the formats.
    if let Some(item) = selected_item(st) {
        children.push(selection_bar(st, item, phone, accent));
    }
    let trash = trash_button("shelf.clear", tr("Kosongkan Shelf", "Clear the Shelf")).key(TRASH).wiggle(st.trash_pulse);
    let mut bottom = vec![info, spacer()];
    if phone {
        bottom.push(receive_button());
    }
    bottom.push(trash);
    children.push(row(bottom).gap(8.0).fill_w().h(32.0));
    col(children).gap(8.0).fill_w()
}

/// "Terima dari HP": starts a session that receives from the phone.
fn receive_button() -> Node {
    capsule(icons::PHONE, tr("Terima dari HP", "Receive from phone"), "phone.receive")
}

/// A format chip of the "Ubah ke..." bar.
fn format_chip(label: &str, action: String, accent: Rgba) -> Node {
    row([text(label.to_string()).semibold().font_size(12.0)]).pad_xy(14.0, 0.0).h(28.0).bg(accent.alpha(0.55)).capsule().action(action)
}

/// What the selected file can do: be sent to the phone, be converted (then the formats), or, while
/// a conversion runs, show how far it is.
fn selection_bar(st: &Shelf, item: &ShelfItem, phone: bool, accent: Rgba) -> Node {
    let key = path_key(&item.path);
    if let Some(c) = st.converting.as_ref().filter(|c| c.key == key) {
        return row([
            text(tr_s(format!("Mengubah ke {}\u{2026} {}%", c.target.label(), (c.progress * 100.0).round() as u32), format!("Converting to {}\u{2026} {}%", c.target.label(), (c.progress * 100.0).round() as u32))).semibold().font_size(12.0),
            spacer(),
            capsule(icons::CLOSE, tr("Batal", "Cancel"), "shelf.cancel_convert"),
        ])
        .gap(8.0)
        .fill_w()
        .h(30.0);
    }
    let targets = if item.is_dir { vec![] } else { convert::targets_for(&item.path, st.webp_encoder) };
    if st.convert_menu {
        let mut kids = vec![caption(tr("Ubah ke", "Convert to"))];
        for t in &targets {
            kids.push(format_chip(t.label(), format!("shelf.convert:{}", t.id()), accent));
        }
        kids.push(spacer());
        kids.push(icon_button(icons::CLOSE, "shelf.convert_menu").name(tr("Tutup", "Close")));
        let mut col_kids = vec![row(kids).gap(8.0).fill_w().h(30.0)];
        if st.heif_missing {
            col_kids.push(row([icon(icons::WARNING).font_size(12.0).color(color::ORANGE), text(tr("HEIC butuh ekstensi HEIF dari Microsoft Store (gratis) supaya bisa dibaca.", "HEIC needs the HEIF extension from the Microsoft Store (free) to be read.")).font_size(12.0).color(color::ORANGE).lines(2).grow(1.0), capsule(icons::SEARCH, tr("Buka Store", "Open Store"), "shelf.open_store")]).gap(6.0).fill_w());
        }
        return col(col_kids).gap(6.0).fill_w();
    }
    let mut kids = vec![];
    if phone && !item.is_dir {
        kids.push(capsule(icons::PHONE, tr("Kirim ke HP", "Send to phone"), "shelf.send"));
    }
    if !targets.is_empty() {
        kids.push(capsule(icons::SYNC, tr("Ubah ke\u{2026}", "Convert to\u{2026}"), "shelf.convert_menu"));
    }
    if kids.is_empty() {
        kids.push(caption(tr("Seret keluar untuk memakai file ini.", "Drag it out to use this file.")));
    }
    kids.push(spacer());
    row(kids).gap(8.0).fill_w().h(30.0)
}

/// A tile with its motion: pops in (staggered after a drop), pulses when dropped again, shrinks
/// away when removed, flies into the bin while the shelf is being emptied.
fn animated_tile(st: &Shelf, idx: usize, f: &ShelfItem, english: bool, accent: Rgba) -> Node {
    let key = path_key(&f.path);
    let order = st.fresh.iter().position(|k| *k == key).unwrap_or(0);
    let mut n = tile_node(st, idx, f, english, accent)
        .key(format!("shelf.f:{key}"))
        .enter(Anim::pop().delay(ENTER_STAGGER * order as f32))
        .exit(Anim::Pop { ms: 200.0, delay: 0.0, from_scale: 0.8, dy: 0.0 })
        .pulse(st.pulses.get(&key).copied().unwrap_or(0));
    if let Some(i) = st.clearing.as_ref().and_then(|c| c.iter().position(|k| *k == key)) {
        n = n.exit(Anim::into_key(TRASH).ms(FLY_MS).delay(FLY_STAGGER * i as f32)).leaving();
    }
    n
}

fn tile_node(st: &Shelf, idx: usize, f: &ShelfItem, english: bool, accent: Rgba) -> Node {
    let (glyph, bg) = style(category(&f.path, f.is_dir));
    let key = path_key(&f.path);
    let mut visual = match st.thumbs.get(&key) {
        Some(img) => image(img, 18.0).size(TILE_W, TILE_H),
        None => stack([icon(glyph).font_size(26.0)]).size(TILE_W, TILE_H).bg(bg).radius(18.0),
    };
    // The selected tile (the bar below acts on it) has an accent border.
    if st.selected.as_deref() == Some(key.as_str()) {
        let ring = stack([]).size(TILE_W, TILE_H).radius(18.0).bg(accent.alpha(0.16)).border(accent);
        visual = stack([visual, ring]).size(TILE_W, TILE_H);
    }
    // A conversion in progress: a dimmed tile with a ring (click it to cancel).
    if let Some(c) = st.converting.as_ref().filter(|c| c.key == key) {
        let pct = (c.progress * 100.0).round() as u32;
        let overlay = col([ring(c.progress).size(30.0, 30.0).thickness(3.5).color(accent), text(format!("{pct}%")).font_size(11.0).semibold()])
            .align(Align::Center)
            .justify(Justify::Center)
            .gap(4.0)
            .size(TILE_W, TILE_H)
            .bg(Rgba::hex(0x000000, 0.58))
            .radius(18.0)
            .action("shelf.cancel_convert");
        visual = stack([visual, overlay]).size(TILE_W, TILE_H);
    }
    let size_text = match (f.is_dir, f.size) {
        (true, _) => tr("Folder", "Folder").to_string(),
        (false, Some(s)) => fmt_size(s, english),
        (false, None) => String::new(),
    };
    let remove = stack([icon(icons::CLOSE).font_size(10.0).color(color::TEXT3)]).size(24.0, 20.0).action(format!("shelf.remove:{idx}"));
    col([
        visual,
        col([text(display_name(&f.path)).semibold().font_size(12.0).max_w(TILE_W), row([caption(size_text).grow(1.0), remove]).w(TILE_W)]).gap(0.0).pad4(2.0, 6.0, 0.0, 0.0),
    ])
    .w(TILE_W)
    .action(format!("shelf.file:{idx}"))
    .draggable()
}

/// The dashed "Seret file ke sini" tile; highlighted while a drag hovers.
fn add_tile(dragging: bool, accent: Rgba) -> Node {
    let label_text = if dragging { tr("Lepas untuk simpan", "Release to save") } else { tr("Seret file ke sini", "Drop files here") };
    let mut n = col([icon(if dragging { icons::DOWNLOAD } else { icons::ADD }).font_size(18.0).color(color::TEXT2), caption(label_text).color(color::TEXT2)])
        .align(Align::Center)
        .justify(Justify::Center)
        .gap(6.0)
        .size(TILE_W, TILE_H)
        .radius(18.0)
        .border(if dragging { accent } else { Rgba::hex(0xffffff, 0.35) })
        .dashed();
    if dragging {
        n = n.bg(accent.alpha(0.20));
    }
    n
}

/// Empty shelf: one wide dashed drop area.
fn empty_drop(dragging: bool, accent: Rgba) -> Node {
    let (head, sub) = if dragging {
        (tr("Lepas untuk simpan di Shelf", "Release to save on the Shelf"), "")
    } else {
        (tr("Seret file ke sini", "Drop files here"), tr("File disimpan sementara di island. Seret keluar ke aplikasi mana pun.", "Files stay on the island for now. Drag them out into any app."))
    };
    let mut kids = vec![icon(if dragging { icons::DOWNLOAD } else { icons::SHELF }).font_size(26.0).color(color::TEXT2), text(head).font_size(typo::CARD_TITLE).semibold()];
    if !sub.is_empty() {
        kids.push(caption(sub));
    }
    let mut n = col(kids)
        .align(Align::Center)
        .justify(Justify::Center)
        .gap(8.0)
        .fill_w()
        .h(128.0)
        .radius(typo::RADIUS_CARD)
        .border(if dragging { accent } else { Rgba::hex(0xffffff, 0.35) })
        .dashed();
    if dragging {
        n = n.bg(accent.alpha(0.20));
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_styles_cover_every_category() {
        for c in [Category::Document, Category::Image, Category::Video, Category::Audio, Category::Archive, Category::Code, Category::Folder, Category::Other] {
            let (g, _) = style(c);
            assert!(!g.is_empty());
        }
    }
}
