//! Feature `ocr` (v0.3, track L): copy the text of a region of the screen.
//!
//! The hotkey (default `Ctrl+Alt+T`; when another program or Moku feature holds it, the usual
//! "Pintasan ... dipakai aplikasi lain" pill says so), the
//! launcher command `ocr` and the Tangkap tab's button start it:
//!
//! 1. the island hides for a few frames and one GDI `BitBlt` freezes the monitor under the cursor
//!    (`capture::grab`), in memory only;
//! 2. [`overlay`]: our own full-screen window shows that picture dimmed; the user drags a rectangle
//!    (Esc / right click cancels);
//! 3. the chosen region goes to [`engine`] (`Windows.Media.Ocr`) on a worker thread. Nothing of the
//!    screen is ever written to disk, and the frozen picture is freed as soon as the overlay closes;
//! 4. the text is copied and a pill says "Teks disalin · 42 kata" with "Lihat", which opens a card
//!    with the lines (each a `text_input`: select, edit, Ctrl+C) and "Salin semua".
//!
//! With no OCR language installed the pill explains how to add one (Moku never installs anything).
//! The recognised text is never logged and never saved; the card's copy of it is dropped a few
//! minutes after it was last used.
//!
//! Debug (`MOKU_DEBUG=1` only): `ocr.debug_start` shows the overlay over a *synthetic* picture of
//! rendered text instead of the real screen, and `ocr.debug_region:x,y,w,h` chooses a rectangle
//! of it as if it had been dragged. That run does not write the real clipboard.

pub mod engine;
mod overlay;
mod region;
mod sample;

use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SW_SHOWNORMAL, WM_HOTKEY};
use windows::core::{HSTRING, PCWSTR, w};

use self::engine::OcrError;
use self::overlay::{Frozen, Labels, Outcome};
use self::region::Rect;
use super::capture::grab;
use crate::activity::{Activity, priority};
use crate::app::{App, Redraw, Ui};
use crate::i18n::{tr, tr_s};
use crate::registry::*;
use crate::ui::text_edit::{Key, KeyPress};
use crate::ui::*;
use crate::hotkeys::Outcome as HotkeyOutcome;
use moku_core::hotkey::Slot;

const HOTKEY_ID: i32 = 121;
const SHEET: &str = "ocr";
/// Lines of the card shown at once (the wheel / arrow keys scroll through the rest).
const VISIBLE: usize = 6;
/// DWM needs a couple of frames to drop the island from the screen before we copy pixels.
const HIDE_SETTLE_MS: u64 = 140;
/// The card's text is dropped from memory this long after it was last used.
const KEEP_SECS: u64 = 300;
/// Windows Settings, Language & region (to add an OCR language).
const SETTINGS_URI: &str = "ms-settings:regionlanguage";
/// The synthetic picture of a debug run.
const DEBUG_SIZE: (i32, i32) = (960, 420);
const DEBUG_AT: (i32, i32) = (60, 60);

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum Busy {
    #[default]
    Idle,
    /// Freezing the screen / the overlay is up.
    Selecting,
    /// The engine is reading.
    Reading,
}

/// The result of one capture, for the card.
struct Card {
    lines: Vec<String>,
    words: usize,
    language: String,
    /// First visible line.
    offset: usize,
    /// It was copied to the clipboard.
    copied: bool,
    /// A short note ("Disalin") under the lines.
    note: Option<String>,
}

#[derive(Default)]
struct Ocr {
    busy: Busy,
    /// The hotkey registered now (`None` = not registered) and the configured string it came from.
    hotkey: Option<String>,
    configured: String,
    card: Option<Card>,
    card_token: u64,
    note_token: u64,
    /// The island was hidden for the capture and must be shown again.
    hid_island: bool,
    /// A debug run: synthetic picture, no real clipboard.
    debug: bool,
}

pub fn register(r: &mut Registry) {
    r.add_state(Ocr::default());
    r.on_action("ocr", on_action);
    r.on_config(on_config);
    r.on_exit(on_exit);
    r.on_event(on_event);
    r.on_message(WM_HOTKEY, on_hotkey);
}

// ---- hotkey -------------------------------------------------------------------------------------

fn on_config(app: &mut App) {
    sync_hotkey(app);
}

fn unregister(app: &mut App) {
    if app.state_mut::<Ocr>().hotkey.take().is_some() {
        crate::hotkeys::unregister(app.main_hwnd(), HOTKEY_ID, Slot::Ocr);
    }
}

/// Register the hotkey while the feature is on and the hotkeys are not let go (paused, or a full-screen
/// app in front, as the launcher's; an app of `island.hide_apps` does not release it). A hotkey that
/// another program (or another Moku feature) holds gets the same pill as every other hotkey.
fn sync_hotkey(app: &mut App) {
    // No windows to activate in --state / --demo.
    if app.hit_hwnd().is_none() {
        return;
    }
    let cfg = app.config().ocr.clone();
    let want = cfg.enabled && !app.hotkeys_released();
    {
        let st = app.state::<Ocr>();
        if want && st.hotkey.is_some() && st.configured == cfg.hotkey {
            return;
        }
        if !want && st.hotkey.is_none() {
            return;
        }
    }
    unregister(app);
    if !cfg.enabled {
        if overlay::is_open() {
            overlay::close();
        }
        return;
    }
    if !want {
        return;
    }
    app.state_mut::<Ocr>().configured = cfg.hotkey.clone();
    match crate::hotkeys::register(app.main_hwnd(), HOTKEY_ID, Slot::Ocr, &cfg.hotkey) {
        HotkeyOutcome::Registered => {
            app.state_mut::<Ocr>().hotkey = Some(cfg.hotkey.clone());
            log_info!("ocr: hotkey {} registered", cfg.hotkey);
        }
        failed => crate::hotkeys::announce(app, Slot::Ocr, &cfg.hotkey, failed),
    }
}

/// The hotkey that starts OCR now: the one registered (the fallback when the configured one is
/// taken), else the configured one (not registered yet, or the island is hidden).
pub fn active_hotkey(app: &App) -> String {
    app.state::<Ocr>().hotkey.clone().unwrap_or_else(|| app.config().ocr.hotkey.clone())
}

fn on_hotkey(app: &mut App, wp: WPARAM, _: LPARAM) {
    if wp.0 as i32 == HOTKEY_ID {
        start(app);
    }
}

fn on_exit(app: &mut App) {
    unregister(app);
    if overlay::is_open() {
        overlay::close();
    }
}

fn on_event(app: &mut App, ev: &AppEvent) {
    match ev {
        // Paused or a full-screen app: let go of the hotkey; back again: take it back.
        AppEvent::Visibility(_) | AppEvent::HotkeysChanged => sync_hotkey(app),
        AppEvent::SheetClosed(SHEET) => {
            app.blur_input();
            clear_line_inputs(app);
            schedule_drop(app);
        }
        // "Hapus semua data": nothing of this feature is on disk, but forget what is in memory.
        AppEvent::DataCleared => {
            app.state_mut::<Ocr>().card = None;
            clear_line_inputs(app);
            app.clear_activity("ocr.done");
        }
        _ => {}
    }
}

// ---- capture ------------------------------------------------------------------------------------

fn physical_cursor() -> Option<POINT> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok().map(|_| p) }
}

/// The monitor rectangle the cursor is on (the primary one when it cannot be told).
fn monitor_under_cursor() -> Option<RECT> {
    unsafe {
        let mon = MonitorFromPoint(physical_cursor().unwrap_or_default(), MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        GetMonitorInfoW(mon, &mut info).as_bool().then_some(info.rcMonitor)
    }
}

/// Start a capture: hide the island, freeze the monitor, show the overlay.
fn start(app: &mut App) {
    if !app.config().ocr.enabled || app.state::<Ocr>().busy != Busy::Idle || overlay::is_open() {
        return;
    }
    let Some(rect) = monitor_under_cursor() else {
        return notice(app, tr("Layar tidak ditemukan.", "Screen not found."));
    };
    app.close_panel();
    app.close_sheet();
    app.clear_activity("ocr.done");
    let st = app.state_mut::<Ocr>();
    st.busy = Busy::Selecting;
    st.debug = false;
    st.hid_island = true;
    app.hide_island(true);
    app.after(Duration::from_millis(HIDE_SETTLE_MS), move |app| {
        let spawned = std::thread::Builder::new().name("ocr-grab".into()).spawn(move || {
            let g = grab::grab_rect(rect);
            Ui::post(move |app| match g {
                Some(g) => show_overlay(app, rect, Frozen { w: g.w as i32, h: g.h as i32, bgra: g.bgra }),
                None => {
                    abort(app);
                    notice(app, tr("Tidak bisa mengambil gambar layar.", "Could not capture the screen."));
                }
            });
        });
        if spawned.is_err() {
            abort(app);
        }
    });
}

/// Debug (`MOKU_DEBUG`): the overlay over a synthetic picture, nothing of the screen involved.
fn debug_start(app: &mut App) {
    if app.state::<Ocr>().busy != Busy::Idle || overlay::is_open() {
        return;
    }
    let (w, h) = DEBUG_SIZE;
    let rect = RECT { left: DEBUG_AT.0, top: DEBUG_AT.1, right: DEBUG_AT.0 + w, bottom: DEBUG_AT.1 + h };
    app.close_panel();
    app.close_sheet();
    let st = app.state_mut::<Ocr>();
    st.busy = Busy::Selecting;
    st.debug = true;
    st.hid_island = false;
    show_overlay(app, rect, Frozen { w, h, bgra: sample::sample_screen(w as u32, h as u32) });
}

fn show_overlay(app: &mut App, rect: RECT, frozen: Frozen) {
    let labels = Labels { hint: tr("Seret untuk memilih teks  \u{b7}  Esc untuk batal", "Drag to select text  \u{b7}  Esc to cancel").to_string() };
    // The callback runs from the window procedure (or from a debug action that still holds the
    // app): `with_app_or_defer` waits for that borrow to end.
    let shown = overlay::show(rect, frozen, labels, Box::new(|outcome| crate::app::with_app_or_defer(move |app| on_outcome(app, outcome))));
    if !shown {
        abort(app);
        notice(app, tr("Tidak bisa membuka pemilih area.", "Could not open the area selector."));
    }
}

/// Give the island back and go idle (capture failed or was cancelled).
fn abort(app: &mut App) {
    let st = app.state_mut::<Ocr>();
    st.busy = Busy::Idle;
    if std::mem::take(&mut st.hid_island) {
        app.hide_island(false);
    }
}

fn on_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::Cancelled => abort(app),
        Outcome::Region(r, px) => {
            // The island comes back now; the engine works on the pixels meanwhile.
            let st = app.state_mut::<Ocr>();
            if std::mem::take(&mut st.hid_island) {
                app.hide_island(false);
            }
            read(app, r, px);
        }
    }
}

/// Send the region to the recogniser on a worker thread.
fn read(app: &mut App, r: Rect, px: Vec<u8>) {
    let lang = app.config().ocr.language.clone();
    app.state_mut::<Ocr>().busy = Busy::Reading;
    app.set_activity(Activity::persistent("ocr.busy", priority::OSD, |_| pill([spinner(), text(tr("Membaca teks\u{2026}", "Reading text\u{2026}")).semibold()])));
    let (w, h) = (r.w as u32, r.h as u32);
    let spawned = std::thread::Builder::new().name("ocr-read".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let result = engine::recognise(px, w, h, &lang);
        unsafe {
            CoUninitialize();
        }
        Ui::post(move |app| on_result(app, result));
    });
    if spawned.is_err() {
        on_result(app, Err(OcrError::Failed(0)));
    }
}

fn on_result(app: &mut App, result: Result<engine::Recognized, OcrError>) {
    app.clear_activity("ocr.busy");
    app.state_mut::<Ocr>().busy = Busy::Idle;
    match result {
        Ok(rec) => {
            let text = rec.text();
            if text.is_empty() {
                return notice(app, tr("Tidak ada teks yang terbaca di area itu.", "No text could be read in that area."));
            }
            let debug = app.state::<Ocr>().debug;
            // A debug run never touches the real clipboard.
            let copied = !debug && app.set_clipboard_text(&text);
            let words = region::word_count(&text);
            let st = app.state_mut::<Ocr>();
            st.card_token += 1;
            st.card = Some(Card { lines: rec.lines, words, language: rec.language, offset: 0, copied: copied || debug, note: None });
            schedule_drop(app);
            app.set_activity(Activity::transient("ocr.done", priority::OSD, 7.0, done_pill).tab("capture"));
        }
        Err(OcrError::NoLanguage) => {
            app.set_activity(Activity::transient("ocr.nolang", priority::OSD, 12.0, no_language_pill).tab("capture"));
        }
        Err(OcrError::TooBig(max)) => {
            let msg = tr_s(format!("Area terlalu besar (maksimal {max} piksel)."), format!("The area is too large (at most {max} pixels)."));
            notice(app, &msg);
        }
        Err(OcrError::Failed(hr)) => {
            log_warn!("ocr: recognition failed (0x{:08x})", hr as u32);
            notice(app, tr("Gagal membaca teks.", "Could not read the text."));
        }
    }
    app.request_redraw(Redraw::All);
}

/// A short message pill.
fn notice(app: &mut App, msg: &str) {
    let msg = msg.to_string();
    app.alert("ocr.notice", priority::OSD, 3.5, move |_| pill([icon(icons::WARNING).color(color::ORANGE), text(msg.clone()).semibold()]));
}

// ---- pills --------------------------------------------------------------------------------------

fn done_pill(app: &App) -> Node {
    let Some(c) = &app.state::<Ocr>().card else { return pill([text("\u{2026}")]) };
    let msg = if c.copied {
        tr_s(format!("Teks disalin \u{b7} {} kata", c.words), format!("Text copied \u{b7} {} words", c.words))
    } else {
        tr_s(format!("Teks terbaca \u{b7} {} kata (tidak tersalin)", c.words), format!("Text read \u{b7} {} words (not copied)", c.words))
    };
    pill([icon(icons::DOCUMENT).color(app.base_accent()), text(msg).semibold(), capsule(icons::EYE, tr("Lihat", "View"), "ocr.view")])
}

/// The "install an OCR language" explanation: a taller pill with the way to the setting.
fn no_language_pill(_: &App) -> Node {
    row([
        icon(icons::WARNING).font_size(18.0).color(color::ORANGE),
        col([
            text(tr("Bahasa OCR belum terpasang", "No OCR language installed")).semibold(),
            text(tr(
                "Pengaturan Windows \u{203a} Waktu & bahasa \u{203a} Bahasa & wilayah \u{203a} pilih bahasa \u{203a} Opsi bahasa \u{203a} Pengenalan teks (OCR)",
                "Windows Settings \u{203a} Time & language \u{203a} Language & region \u{203a} a language \u{203a} Language options \u{203a} Optical character recognition",
            ))
            .font_size(typo::CAPTION)
            .tertiary()
            .lines(2)
            .max_w(380.0),
        ])
        .gap(2.0),
        button(tr("Buka Pengaturan", "Open Settings"), "ocr.settings"),
    ])
    .gap(12.0)
    .pad_xy(16.0, 10.0)
}

// ---- the card -----------------------------------------------------------------------------------

fn input_id(i: usize) -> String {
    format!("ocr.l{i}")
}

/// `l3` -> 3 (a visible line slot).
fn slot(name: &str) -> Option<usize> {
    name.strip_prefix('l')?.parse::<usize>().ok().filter(|i| *i < VISIBLE)
}

/// Put the visible lines into the `text_input`s.
fn sync_inputs(app: &mut App) {
    let shown: Vec<String> = match &app.state::<Ocr>().card {
        Some(c) => (0..VISIBLE).map(|i| c.lines.get(c.offset + i).cloned().unwrap_or_default()).collect(),
        None => vec![],
    };
    for i in 0..VISIBLE {
        let line = shown.get(i).map(String::as_str).unwrap_or("");
        app.input_mut(&input_id(i)).set_text(line);
    }
}

fn clear_line_inputs(app: &mut App) {
    for i in 0..VISIBLE {
        app.input_mut(&input_id(i)).clear();
    }
}

/// Drop the card's text a few minutes after it was last used (not while the card is open).
fn schedule_drop(app: &mut App) {
    let token = app.state::<Ocr>().card_token;
    app.after(Duration::from_secs(KEEP_SECS), move |app| {
        if app.state::<Ocr>().card_token == token && app.sheet() != Some(SHEET) {
            app.state_mut::<Ocr>().card = None;
            app.clear_activity("ocr.done");
        }
    });
}

fn view(app: &mut App) {
    if app.state::<Ocr>().card.is_none() {
        return;
    }
    app.state_mut::<Ocr>().card_token += 1;
    app.clear_activity("ocr.done");
    sync_inputs(app);
    app.open_sheet(SHEET, sheet);
    app.focus_input(&input_id(0));
    app.request_redraw(Redraw::All);
}

fn close_card(app: &mut App) {
    if app.sheet() == Some(SHEET) {
        app.close_sheet();
    }
}

fn set_note(app: &mut App, msg: String) {
    let st = app.state_mut::<Ocr>();
    st.note_token += 1;
    let token = st.note_token;
    if let Some(c) = st.card.as_mut() {
        c.note = Some(msg);
    }
    app.request_redraw(Redraw::All);
    app.after(Duration::from_millis(1800), move |app| {
        let st = app.state_mut::<Ocr>();
        if st.note_token == token {
            if let Some(c) = st.card.as_mut() {
                c.note = None;
            }
            app.request_redraw(Redraw::All);
        }
    });
}

/// "Salin semua": every line, with the user's edits.
fn copy_all(app: &mut App) {
    let Some(text) = app.state::<Ocr>().card.as_ref().map(|c| region::join_lines(&c.lines)) else { return };
    let ok = app.set_clipboard_text(&text);
    set_note(app, if ok { tr("Semua baris disalin", "All lines copied") } else { tr("Papan klip sedang dipakai", "The clipboard is busy") }.to_string());
}

/// Ctrl+C in a line: the selected part, or the whole line when nothing is selected.
fn copy_selection(app: &mut App, i: usize) {
    let ed = app.input(&input_id(i));
    let text = match ed.selection() {
        Some((a, b)) => ed.text().get(a..b).unwrap_or("").to_string(),
        None => ed.text().to_string(),
    };
    if text.is_empty() {
        return;
    }
    let ok = app.set_clipboard_text(&text);
    set_note(app, if ok { tr("Disalin", "Copied") } else { tr("Papan klip sedang dipakai", "The clipboard is busy") }.to_string());
}

fn max_offset(c: &Card) -> usize {
    c.lines.len().saturating_sub(VISIBLE)
}

fn scroll_by(app: &mut App, delta: isize) {
    let Some(c) = app.state_mut::<Ocr>().card.as_mut() else { return };
    let new = (c.offset as isize + delta).clamp(0, max_offset(c) as isize) as usize;
    if new == c.offset {
        return;
    }
    c.offset = new;
    sync_inputs(app);
    app.request_redraw(Redraw::All);
}

fn focus_line(app: &mut App, i: usize) {
    let id = input_id(i);
    app.focus_input(&id);
    app.input_mut(&id).select_all();
    app.request_redraw(Redraw::All);
}

/// How many of the visible slots hold a line.
fn shown_lines(c: &Card) -> usize {
    c.lines.len().saturating_sub(c.offset).min(VISIBLE)
}

fn on_line_key(app: &mut App, i: usize, k: KeyPress) {
    let shown = app.state::<Ocr>().card.as_ref().map_or(0, shown_lines);
    match k.key {
        Key::Escape => close_card(app),
        Key::Char('c') if k.ctrl && !k.alt => copy_selection(app, i),
        Key::Up => {
            if i > 0 {
                focus_line(app, i - 1);
            } else {
                scroll_by(app, -1);
            }
        }
        Key::Down => {
            if i + 1 < shown {
                focus_line(app, i + 1);
            } else {
                scroll_by(app, 1);
            }
        }
        Key::Tab => {
            let next = if k.shift { i.saturating_sub(1) } else { (i + 1).min(shown.saturating_sub(1)) };
            focus_line(app, next);
        }
        Key::PageDown => scroll_by(app, VISIBLE as isize),
        Key::PageUp => scroll_by(app, -(VISIBLE as isize)),
        _ => {}
    }
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    let name = ev.name();
    if let Some(i) = slot(name) {
        return match ev.kind {
            ActionKind::Click => focus_line(app, i),
            // The user corrected a line: keep it, so "Salin semua" copies the correction.
            ActionKind::Edited => {
                let t = app.input(&input_id(i)).text().to_string();
                let st = app.state_mut::<Ocr>();
                if let Some(c) = st.card.as_mut()
                    && let Some(l) = c.lines.get_mut(c.offset + i)
                {
                    *l = t;
                }
            }
            ActionKind::Key(k) => on_line_key(app, i, k),
            // The user went to another window: the card closes, its text is on the clipboard.
            ActionKind::Blur => close_card(app),
            ActionKind::Scroll(n) => scroll_by(app, if n > 0.0 { -1 } else { 1 }),
            _ => {}
        };
    }
    match (name, ev.kind) {
        ("start", ActionKind::Click) => start(app),
        ("view", ActionKind::Click) => view(app),
        ("close", ActionKind::Click) => close_card(app),
        ("copy_all", ActionKind::Click) => copy_all(app),
        ("scroll", ActionKind::Scroll(n)) => scroll_by(app, if n > 0.0 { -1 } else { 1 }),
        ("settings", ActionKind::Click) => {
            open_uri(SETTINGS_URI);
            app.clear_activity("ocr.nolang");
        }
        // Debug helpers (`mokuctl debug click ocr.debug_start`, then `ocr.debug_region:x,y,w,h`).
        ("debug_start", ActionKind::Click) => debug_start(app),
        // The "no OCR language" explanation (for a screenshot; the real one needs no language).
        ("debug_nolang", ActionKind::Click) => on_result(app, Err(OcrError::NoLanguage)),
        ("debug_region", ActionKind::Click) => {
            let nums: Vec<i32> = ev.arg().unwrap_or("").split(',').filter_map(|p| p.trim().parse().ok()).collect();
            if let [x, y, w, h] = nums[..] {
                overlay::debug_select(Rect { x, y, w, h });
            }
        }
        _ => {}
    }
}

/// `ShellExecute` on a Windows Settings URI (the user pressed the button).
fn open_uri(uri: &str) -> bool {
    let h = unsafe { ShellExecuteW(None, w!("open"), &HSTRING::from(uri), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    h.0 as isize > 32
}

fn line_row(app: &App, slot: usize, n: usize) -> Node {
    let id = input_id(slot);
    row([caption((n + 1).to_string()).w(22.0), text_input(&id, app.input(&id)).font_size(14.0).accent(app.base_accent()).grow(1.0)]).gap(8.0).fill_w().pad_xy(12.0, 9.0).bg(color::FILL).radius(12.0)
}

fn sheet(app: &App) -> Node {
    let Some(c) = &app.state::<Ocr>().card else { return col([caption(tr("Tidak ada teks.", "No text."))]) };
    let header = row([
        icon(icons::DOCUMENT).font_size(16.0).color(app.base_accent()),
        text(tr("Teks dari layar", "Text from the screen")).semibold().font_size(14.0),
        caption(tr_s(format!("{} kata \u{b7} {}", c.words, c.language), format!("{} words \u{b7} {}", c.words, c.language))),
        spacer(),
        capsule(icons::COPY, tr("Salin semua", "Copy all"), "ocr.copy_all"),
        icon_button(icons::CLOSE, "ocr.close").name(tr("Tutup", "Close")),
    ])
    .gap(10.0)
    .fill_w()
    .h(32.0);
    let rows: Vec<Node> = (0..shown_lines(c)).map(|i| line_row(app, i, c.offset + i)).collect();
    let total = c.lines.len();
    let footer = match &c.note {
        Some(n) => caption(n.clone()).color(app.base_accent()),
        None if total > VISIBLE => caption(tr_s(
            format!("Baris {}\u{2013}{} dari {total} \u{b7} roda mouse atau \u{2191}\u{2193} untuk menggulir", c.offset + 1, c.offset + shown_lines(c)),
            format!("Lines {}\u{2013}{} of {total} \u{b7} mouse wheel or \u{2191}\u{2193} to scroll", c.offset + 1, c.offset + shown_lines(c)),
        )),
        None => caption(tr("Klik baris lalu Ctrl+C untuk menyalin sebagian; Esc untuk menutup", "Click a line, then Ctrl+C to copy part of it; Esc closes")),
    };
    col([header, col(rows).gap(4.0).fill_w().action("ocr.scroll"), footer]).gap(8.0).fill_w()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_hotkey_parses_and_is_the_documented_one() {
        let c = moku_core::config::Config::default();
        assert_eq!(c.ocr.hotkey, "Ctrl+Alt+T");
        assert!(moku_core::hotkey::Hotkey::parse(&c.ocr.hotkey).is_ok());
    }

    #[test]
    fn line_slots() {
        assert_eq!(slot("l0"), Some(0));
        assert_eq!(slot("l5"), Some(5));
        assert_eq!(slot("l6"), None);
        assert_eq!(slot("copy_all"), None);
        assert_eq!(slot("l"), None);
        assert_eq!(slot("lx"), None);
        assert_eq!(input_id(3), "ocr.l3");
    }

    fn card(n: usize, offset: usize) -> Card {
        Card { lines: (0..n).map(|i| format!("line {i}")).collect(), words: n * 2, language: "en-US".into(), offset, copied: true, note: None }
    }

    #[test]
    fn card_paging() {
        assert_eq!((max_offset(&card(3, 0)), shown_lines(&card(3, 0))), (0, 3));
        assert_eq!((max_offset(&card(6, 0)), shown_lines(&card(6, 0))), (0, 6));
        assert_eq!((max_offset(&card(14, 0)), shown_lines(&card(14, 0))), (8, 6));
        assert_eq!(shown_lines(&card(14, 8)), 6);
        assert_eq!(shown_lines(&card(14, 12)), 2, "a stale offset never shows more than exists");
        assert_eq!(shown_lines(&card(0, 0)), 0);
    }
}
