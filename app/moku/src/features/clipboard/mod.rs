//! Feature `clipboard` (Task 5, track D): clipboard history of text, links, colours and images.
//!
//! * `listener.rs` — service: message-only window + `AddClipboardFormatListener`; reads the
//!   clipboard off the UI thread, honours the password-manager formats and `ignore_apps`.
//! * `sys.rs` — the Windows clipboard calls (bounded reads, writes, owner process).
//! * `model.rs` — pure history model (dedupe, caps, expiry, index) with the unit tests.
//! * `store.rs` — content-addressed files in `<data dir>\clipboard\`.
//! * this file — state, panel, actions.
//!
//! * `hold.rs` — a hold while Moku borrows the clipboard (Tanya AI's Ctrl+C fallback and "Ganti teks"):
//!   the history ignores what changes meanwhile.
//!
//! Privacy: content is never logged; password-manager content (exclusion formats,
//! `ignore_apps`) is never read at all; Moku's own put-backs carry a private marker format so
//! they are not recorded again.

pub(crate) mod actions;
pub(crate) mod hold;
pub(crate) mod imgutil;
mod listener;
mod model;
mod smart;
mod store;
pub(crate) mod sys;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use self::model::{Age, ClipItem, History, Kind, OTP_TTL_SECS};
use self::store::Ingested;
use crate::app::{App, Redraw, Ui};
use crate::i18n::{tr, tr_s};
use crate::registry::*;
use crate::services::ServiceCtx;
use crate::ui::*;
use crate::imaging;

/// Rows the panel shows at once (the wheel scrolls through the rest).
const PAGE: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flash {
    Copied,
    Failed,
    /// Any short message `(Indonesian, English)`: "Dirapikan dan disalin".
    Msg(&'static str, &'static str),
    /// "Disalin sebagai RGB".
    As(&'static str),
}

/// The QR card that replaces the list: the URL it shows (held in memory while it is on screen).
struct QrView {
    key: u64,
    text: String,
}

#[derive(Default)]
struct Clipboard {
    hist: History,
    /// One-time codes (OTP) by item key: the digits, in memory only. They are never written to
    /// disk and leave with their history item (60 s) or on clear.
    secrets: HashMap<u64, String>,
    /// The QR card, when "QR" was pressed on a link.
    qr: Option<QrView>,
    /// First visible row (scrolled with the wheel).
    offset: usize,
    /// Square thumbnails of the visible image rows. Filled lazily while the tab is visible and
    /// dropped when it is hidden, so the idle island holds no pixels.
    thumbs: HashMap<u64, Image>,
    /// Brief feedback on one row ("Disalin").
    flash: Option<(u64, Flash)>,
    flash_token: u64,
    /// Rows flying into the bin ("Kosongkan"); the history is cleared when they landed.
    clearing: Option<Vec<u64>>,
    /// Bumped to wiggle the bin after the rows landed in it.
    trash_pulse: u32,
}

/// Same motion as the Shelf's bin (ms).
const FLY_STAGGER: f32 = 30.0;
const FLY_MS: f32 = 340.0;
const WIGGLE_MS: u64 = 420;
const TRASH: &str = "clipboard.trash";

pub fn register(r: &mut Registry) {
    r.add_state(Clipboard::default());
    r.add_tab(TabSpec {
        id: "clipboard",
        order: order::CLIPBOARD,
        icon: icons::CLIPBOARD,
        label: ("Clipboard", "Clipboard"),
        enabled: |c| c.clipboard.enabled,
        build: panel,
        // Once a second while the tab is shown: the "hilang dalam N dtk" of a one-time code.
        tick: Some((Duration::from_secs(1), on_tick)),
    });
    r.on_action("clipboard", on_action);
    r.on_config(on_config);
    r.on_start(on_start);
    r.on_exit(save_index); // flushed by the core right after the exit hooks
    r.on_event(on_event);
    r.add_service(ServiceSpec { name: "clipboard", enabled: |c| c.clipboard.enabled, run: service });
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn dir() -> std::path::PathBuf {
    moku_core::paths::clipboard_dir()
}

// ---- lifecycle --------------------------------------------------------------------------------

fn service(ctx: ServiceCtx) {
    let c2 = ctx.clone();
    let sink: listener::Sink = Arc::new(move |ingested| {
        c2.post(move |app| match ingested {
            Ingested::Item { item, strong } => add_item(app, item, strong),
            Ingested::Secret(item, code) => add_secret(app, item, code),
        })
    });
    let ignore: listener::IgnoreFn = Arc::new(|| crate::app::current_config().clipboard.ignore_apps.clone());
    listener::run(ctx, dir(), ignore, sink);
}

fn on_start(app: &mut App) {
    let mut hist = History::load(&store::index_path());
    let (keep, max) = (app.config().clipboard.keep_days, app.config().clipboard.max_items as usize);
    let mut gone = hist.expire(now(), keep);
    gone.extend(hist.enforce_max(max));
    // One small directory listing at startup: files of crashed / half-finished writes.
    store::cleanup_orphans(&dir(), &hist.referenced_files());
    store::delete_files(&dir(), &gone);
    app.state_mut::<Clipboard>().hist = hist;
    save_index(app);
    app.request_redraw(Redraw::Tab("clipboard"));
}

/// Save the index soon (debounced, written off the UI thread).
fn save_index(app: &mut App) {
    app.save_later("clipboard", |app| Some((store::index_path(), app.state::<Clipboard>().hist.to_json())));
}

fn on_config(app: &mut App) {
    let (keep, max) = (app.config().clipboard.keep_days, app.config().clipboard.max_items as usize);
    let st = app.state_mut::<Clipboard>();
    let mut gone = st.hist.expire(now(), keep);
    gone.extend(st.hist.enforce_max(max));
    if !gone.is_empty() {
        store::delete_files(&dir(), &gone);
        save_index(app);
        app.request_redraw(Redraw::Tab("clipboard"));
    }
}

fn on_event(app: &mut App, ev: &AppEvent) {
    match ev {
        AppEvent::TabShown("clipboard") => {
            let (keep, max) = (app.config().clipboard.keep_days, app.config().clipboard.max_items as usize);
            let st = app.state_mut::<Clipboard>();
            let mut gone = st.hist.expire(now(), keep);
            gone.extend(st.hist.enforce_max(max));
            if !gone.is_empty() {
                store::delete_files(&dir(), &gone);
                save_index(app);
            }
            purge_otps(app);
            ensure_thumbs(app);
        }
        AppEvent::TabHidden("clipboard") => {
            let st = app.state_mut::<Clipboard>();
            st.thumbs = HashMap::new();
            st.flash = None;
            // The QR card and its bitmap go with the tab.
            st.qr = None;
            crate::ui::qr::clear_cache();
        }
        // The settings app deletes the clipboard folder right after: forget everything.
        AppEvent::DataCleared => {
            app.cancel_save("clipboard");
            let st = app.state_mut::<Clipboard>();
            st.hist = History::default();
            st.secrets.clear();
            st.qr = None;
            crate::ui::qr::clear_cache();
            st.offset = 0;
            st.thumbs = HashMap::new();
            st.flash = None;
            st.clearing = None;
            app.request_redraw(Redraw::Tab("clipboard"));
        }
        _ => {}
    }
}

/// A new item from the listener (UI thread). `strong`: its smart action is certain enough for the
/// short pill.
fn add_item(app: &mut App, item: ClipItem, strong: bool) {
    let (keep, max) = (app.config().clipboard.keep_days, app.config().clipboard.max_items as usize);
    let key = item.key;
    let st = app.state_mut::<Clipboard>();
    let mut gone = st.hist.insert(item, max);
    gone.extend(st.hist.expire(now(), keep));
    st.offset = 0;
    if st.qr.take().is_some() {
        crate::ui::qr::clear_cache();
    }
    store::delete_files(&dir(), &gone);
    save_index(app);
    purge_otps(app);
    ensure_thumbs(app);
    // The same digits can arrive as a plain copy of a code that is held: it stays a code.
    if strong && app.state::<Clipboard>().hist.get(key).is_some_and(|i| i.act.is_some() && !i.is_otp()) {
        smart::show_pill(app, key);
    }
    app.request_redraw(Redraw::Tab("clipboard"));
}

/// A one-time code from the listener (UI thread): held in memory only, gone after a minute.
fn add_secret(app: &mut App, item: ClipItem, code: String) {
    let max = app.config().clipboard.max_items as usize;
    let key = item.key;
    let st = app.state_mut::<Clipboard>();
    let mut gone = st.hist.insert_secret(item);
    // A plain copy of the same digits written earlier is deleted (it is a code now).
    gone.extend(st.hist.enforce_max(max));
    st.secrets.insert(key, code);
    st.offset = 0;
    if st.qr.take().is_some() {
        crate::ui::qr::clear_cache();
    }
    store::delete_files(&dir(), &gone);
    save_index(app);
    // Drop it when its minute is over (the check also catches a re-copy that restarted it).
    app.after(Duration::from_secs(OTP_TTL_SECS as u64 + 1), purge_otps);
    smart::show_pill(app, key);
    app.request_redraw(Redraw::Tab("clipboard"));
}

/// Forget the one-time codes that are a minute old (and the digits held for them).
fn purge_otps(app: &mut App) {
    let st = app.state_mut::<Clipboard>();
    let expired = st.hist.expire_otps(now(), OTP_TTL_SECS);
    let before = st.secrets.len();
    let live: std::collections::HashSet<u64> = st.hist.items.iter().filter(|i| i.is_otp()).map(|i| i.key).collect();
    st.secrets.retain(|k, _| live.contains(k));
    if !expired.is_empty() || st.secrets.len() != before {
        app.request_redraw(Redraw::Tab("clipboard"));
    }
}

/// Once a second while the tab is visible: expire codes, refresh their countdown.
fn on_tick(app: &mut App) {
    purge_otps(app);
    if app.state::<Clipboard>().hist.items.iter().any(|i| i.is_otp()) {
        app.request_redraw(Redraw::Tab("clipboard"));
    }
}

/// Decode the thumbnails of the rows that are on screen (small PNGs, a millisecond each).
fn ensure_thumbs(app: &mut App) {
    if !app.is_tab_visible("clipboard") {
        return;
    }
    let st = app.state_mut::<Clipboard>();
    let keys: Vec<u64> = st.hist.ordered().iter().skip(st.offset).take(PAGE).filter(|i| i.is_image()).map(|i| i.key).collect();
    st.thumbs.retain(|k, _| keys.contains(k));
    for k in keys {
        if st.thumbs.contains_key(&k) {
            continue;
        }
        if let Some(bytes) = store::read_file(&dir(), &model::thumb_file(k))
            && let Ok(img) = imaging::decode(&bytes, model::THUMB_PX)
        {
            st.thumbs.insert(k, imgutil::crop_to_aspect(&img, 1, 1));
        }
    }
}

// ---- actions ----------------------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    if let ActionKind::Scroll(n) = ev.kind {
        return scroll(app, n);
    }
    if ev.kind != ActionKind::Click {
        return;
    }
    let key = ev.arg().and_then(|a| a.parse::<u64>().ok());
    match (ev.name(), key) {
        ("copy", Some(k)) => copy_back(app, k),
        ("pin", Some(k)) => toggle_pin(app, k),
        ("clear", _) => start_clear(app),
        // A smart-action button: `clipboard.act:<key>:<verb>`.
        ("act", _) => {
            if let Some(a) = ev.arg() {
                smart::act(app, a);
            }
        }
        ("qr_back", _) => smart::close_qr(app),
        // Debug helpers (`mokuctl debug click "clipboard.debug_text:hello"`): feed the same code
        // path as a real clipboard change without touching the real clipboard.
        ("debug_text", _) => {
            if let Some(t) = ev.arg() {
                debug_add(app, store::Captured::Text { text: t.to_string(), truncated: false }, None);
            }
        }
        // `clipboard.debug_text_from:WhatsApp.exe|482913`: as if that app had copied the text
        // (a bare code from a messaging app is a one-time code).
        ("debug_text_from", _) => {
            if let Some((exe, t)) = ev.arg().and_then(|a| a.split_once('|')) {
                debug_add(app, store::Captured::Text { text: t.to_string(), truncated: false }, Some(exe));
            }
        }
        ("debug_image", _) => debug_add(app, debug_image(ev.arg().and_then(|a| a.parse().ok()).unwrap_or(1)), None),
        _ => {}
    }
}

fn scroll(app: &mut App, notches: f32) {
    let st = app.state_mut::<Clipboard>();
    let n = st.hist.items.len();
    let max_off = n.saturating_sub(PAGE);
    let step = if notches > 0.0 { -1isize } else { 1 };
    let new = (st.offset as isize + step).clamp(0, max_off as isize) as usize;
    if new != st.offset {
        st.offset = new;
        ensure_thumbs(app);
        app.request_redraw(Redraw::Tab("clipboard"));
    }
}

fn toggle_pin(app: &mut App, key: u64) {
    let st = app.state_mut::<Clipboard>();
    let Some(pinned) = st.hist.get(key).map(|i| i.pinned) else { return };
    let gone = st.hist.set_pinned(key, !pinned);
    store::delete_files(&dir(), &gone);
    save_index(app);
    app.request_redraw(Redraw::Tab("clipboard"));
}

/// The trash button: the visible unpinned rows fly into it, it wiggles, then [`clear`] runs.
fn start_clear(app: &mut App) {
    let st = app.state::<Clipboard>();
    if st.clearing.is_some() {
        return;
    }
    let ordered = st.hist.ordered();
    let offset = st.offset.min(ordered.len().saturating_sub(PAGE));
    let flying: Vec<u64> = ordered.iter().skip(offset).take(PAGE).filter(|i| !i.pinned).map(|i| i.key).collect();
    if flying.is_empty() || app.animations() == moku_core::config::Animations::Off || !app.is_tab_visible("clipboard") {
        return clear(app);
    }
    let fly = (FLY_STAGGER * (flying.len() - 1) as f32 + FLY_MS) as u64;
    app.state_mut::<Clipboard>().clearing = Some(flying);
    app.request_redraw(Redraw::Tab("clipboard"));
    app.after(Duration::from_millis(fly), |app| {
        if app.state::<Clipboard>().clearing.is_none() {
            return;
        }
        app.state_mut::<Clipboard>().trash_pulse += 1;
        app.request_redraw(Redraw::Tab("clipboard"));
        app.after(Duration::from_millis(WIGGLE_MS), |app| {
            if app.state_mut::<Clipboard>().clearing.take().is_some() {
                clear(app);
            }
        });
    });
}

/// "Kosongkan": everything except pinned items.
fn clear(app: &mut App) {
    let st = app.state_mut::<Clipboard>();
    st.clearing = None;
    let gone = st.hist.clear_unpinned();
    st.offset = 0;
    st.thumbs.clear();
    store::delete_files(&dir(), &gone);
    save_index(app);
    purge_otps(app);
    ensure_thumbs(app);
    app.request_redraw(Redraw::Tab("clipboard"));
}

/// Put an item back on the clipboard. Marked as ours, so the listener does not record it again.
/// The slow parts (file read, WIC decode, waiting for the clipboard) run on a worker thread.
fn copy_back(app: &mut App, key: u64) {
    let Some(item) = app.state::<Clipboard>().hist.get(key).cloned() else { return };
    // A one-time code is not on disk: its digits come from memory.
    let secret = app.state::<Clipboard>().secrets.get(&key).cloned();
    run_copy(app, key, Flash::Copied, move |owner| put_back(&item, secret.as_deref(), owner));
}

/// Put `text` on the clipboard (marked as ours) and flash `ok` on the row `key`. Used by the
/// smart actions (a colour in another notation, a sum's result, pretty JSON).
fn copy_string(app: &mut App, key: u64, text: String, ok: Flash) {
    run_copy(app, key, ok, move |owner| sys::put_text(owner, &text));
}

/// Run a clipboard write on a worker thread; the row flashes `ok` at once and `Failed` if it did not work.
fn run_copy(app: &mut App, key: u64, ok: Flash, put: impl FnOnce(HWND) -> bool + Send + 'static) {
    let owner = app.main_hwnd().0 as isize;
    set_flash(app, key, ok, 1600);
    let spawned = std::thread::Builder::new().name("clip-copy".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let done = put(HWND(owner as _));
        unsafe {
            CoUninitialize();
        }
        if !done {
            Ui::post(move |app| set_flash(app, key, Flash::Failed, 2500));
        }
    });
    if spawned.is_err() {
        set_flash(app, key, Flash::Failed, 2500);
    }
}

fn put_back(item: &ClipItem, secret: Option<&str>, owner: HWND) -> bool {
    let d = dir();
    if item.is_otp() {
        // Kept out of Windows' own clipboard history and cloud sync too.
        return secret.is_some_and(|code| sys::put_text_private(owner, code));
    }
    if item.is_image() {
        // The original when we kept one (pinned / newest), else the 128 px preview.
        let name = if item.full { model::orig_file(item.key) } else { model::thumb_file(item.key) };
        match store::read_file(&d, &name) {
            Some(png) => sys::put_image(owner, &png, None, true),
            None => false,
        }
    } else {
        match store::read_text(&d, item.key) {
            Some(t) => sys::put_text(owner, &t),
            None => false,
        }
    }
}

fn set_flash(app: &mut App, key: u64, f: Flash, ms: u64) {
    let st = app.state_mut::<Clipboard>();
    st.flash_token += 1;
    let token = st.flash_token;
    st.flash = Some((key, f));
    app.request_redraw(Redraw::Tab("clipboard"));
    app.after(Duration::from_millis(ms), move |app| {
        let st = app.state_mut::<Clipboard>();
        if st.flash_token == token {
            st.flash = None;
            app.request_redraw(Redraw::Tab("clipboard"));
        }
    });
}

/// The debug path goes through the same `ingest` as a real clipboard change, so the detectors,
/// the tags and the one-time-code rule apply.
fn debug_add(app: &mut App, c: store::Captured, owner_exe: Option<&str>) {
    let otp_guard = app.config().clipboard.otp_guard;
    match store::ingest(&dir(), c, owner_exe, otp_guard, now()) {
        Some(Ingested::Item { item, strong }) => add_item(app, item, strong),
        Some(Ingested::Secret(item, code)) => add_secret(app, item, code),
        None => {}
    }
}

/// A synthetic gradient picture (debug path for screenshots and tests; no clipboard involved).
fn debug_image(seed: u64) -> store::Captured {
    let (w, h) = (320u32, 200u32);
    let (r0, g0, b0) = ((seed * 67 % 200) as u32 + 40, (seed * 131 % 200) as u32 + 40, (seed * 29 % 200) as u32 + 40);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let t = (x + y) * 255 / (w + h);
            px.extend([((b0 + t) / 2) as u8, ((g0 + 255 - t) / 2) as u8, ((r0 + t / 2) / 2) as u8, 255]);
        }
    }
    let img = Image::new(w, h, px);
    let thumb = imaging::from_bgra(w, h, w * 4, &img.0.bgra, true, model::THUMB_PX).unwrap_or_else(|_| img.clone());
    let orig = imaging::encode_png(&img).ok();
    store::Captured::Image {
        thumb_png: imaging::encode_png(&thumb).unwrap_or_default(),
        w,
        h,
        key: model::content_key(true, &seed.to_le_bytes()),
        bytes: orig.as_ref().map_or(0, |o| o.len() as u64),
        orig_png: orig,
    }
}

// ---- panel ------------------------------------------------------------------------------------

fn ago_text(a: Age) -> String {
    match a {
        Age::Now => tr("baru saja", "just now").to_string(),
        Age::Minutes(n) => tr_s(format!("{n} menit lalu"), format!("{n} min ago")),
        Age::Hours(n) => tr_s(format!("{n} jam lalu"), format!("{n} h ago")),
        Age::Yesterday => tr("kemarin", "yesterday").to_string(),
        Age::Days(n) => tr_s(format!("{n} hari lalu"), format!("{n} d ago")),
    }
}

fn kind_label(k: Kind) -> &'static str {
    match k {
        Kind::Text => tr("Teks", "Text"),
        Kind::Link => tr("Tautan", "Link"),
        Kind::Color => tr("Warna", "Color"),
        Kind::Image => tr("Gambar", "Image"),
    }
}

fn panel(app: &App) -> Node {
    let st = app.state::<Clipboard>();
    if let Some(q) = &st.qr {
        return smart::qr_view(q);
    }
    let ordered = st.hist.ordered();
    if ordered.is_empty() {
        // Wrapped: a top-level column is split into blocks, which would drop the key.
        return col([placeholder(
            icons::CLIPBOARD,
            tr("Riwayat masih kosong", "History is empty"),
            tr("Salin teks atau gambar, riwayatnya muncul di sini.", "Copy some text or an image and it shows up here."),
        )
        .key("clipboard.empty")
        .enter(Anim::fade().ms(240.0))])
        .fill_w();
    }
    let n = ordered.len();
    let offset = st.offset.min(n.saturating_sub(PAGE));
    let now = now();
    let rows: Vec<Node> = ordered
        .iter()
        .skip(offset)
        .take(PAGE)
        .map(|it| {
            // Keyed: new copies slide the others down, scrolling glides, the bin eats them.
            let mut r = row_node(app, st, it, now).key(format!("clip.{}", it.key));
            if let Some(i) = st.clearing.as_ref().and_then(|c| c.iter().position(|k| *k == it.key)) {
                r = r.exit(Anim::into_key(TRASH).ms(FLY_MS).delay(FLY_STAGGER * i as f32)).leaving();
            }
            r
        })
        .collect();
    let range = if n > PAGE {
        tr_s(format!("{}–{} dari {n}", offset + 1, (offset + PAGE).min(n)), format!("{}–{} of {n}", offset + 1, (offset + PAGE).min(n)))
    } else {
        tr_s(format!("{n} item"), format!("{n} items"))
    };
    let trash = trash_button("clipboard.clear", tr("Kosongkan riwayat (yang disematkan tetap)", "Clear history (pinned items stay)")).key(TRASH).wiggle(st.trash_pulse);
    let header = row([label(tr("Riwayat", "History")), caption(range), spacer(), trash]).gap(8.0).fill_w().h(30.0);
    col([header, col(rows).gap(4.0).fill_w()]).gap(8.0).fill_w()
}

fn lead_node(st: &Clipboard, it: &ClipItem) -> Node {
    match it.kind {
        Kind::Text => tile(icons::CLIPBOARD, color::BLUE),
        Kind::Link => tile(icons::LINK, color::GREEN),
        Kind::Color => {
            // A colour item is the whole text (`#0A84FF`), so its preview is the colour: no file read
            // in a builder.
            let rgb = actions::color::parse(&it.preview).map_or(0x808080, |(c, _)| c.rgb_u32());
            empty().size(28.0, 28.0).bg(Rgba::hex(rgb, 1.0)).radius(8.0).border(Rgba::hex(0xffffff, 0.25))
        }
        Kind::Image => match st.thumbs.get(&it.key) {
            Some(img) => image(img, 8.0).size(28.0, 28.0),
            None => tile(icons::PHOTO, color::RED),
        },
    }
}

fn row_node(app: &App, st: &Clipboard, it: &ClipItem, now: i64) -> Node {
    let title = match it.kind {
        Kind::Image => tr_s(format!("Gambar {} × {}", it.w, it.h), format!("Image {} × {}", it.w, it.h)),
        _ if it.preview.is_empty() => "…".to_string(),
        _ => it.preview.clone(),
    };
    let sub = match st.flash {
        Some((k, Flash::Failed)) if k == it.key => caption(tr("Gagal menyalin", "Could not copy")).color(color::RED),
        Some((k, f)) if k == it.key => caption(smart::flash_text(f).unwrap_or_default()).color(app.base_accent()),
        _ if it.is_otp() => {
            // The countdown refreshes with the tab's 1 s tick.
            let left = (OTP_TTL_SECS - (now - it.ts)).clamp(0, OTP_TTL_SECS);
            caption(tr_s(format!("Kode OTP · hilang dalam {left} dtk"), format!("One-time code · gone in {left} s"))).color(color::ORANGE)
        }
        _ => {
            let smart_on = app.config().clipboard.smart_actions;
            let kind = smart_on.then(|| smart::caption_for(it, !crate::i18n::is_english())).flatten().unwrap_or_else(|| kind_label(it.kind).to_string());
            let mut parts = vec![kind];
            if it.truncated {
                parts.push(tr("dipotong 100 KB", "cut at 100 KB").to_string());
            }
            if it.is_image() && !it.full {
                parts.push(tr("pratinjau saja", "preview only").to_string());
            }
            parts.push(ago_text(model::age(now, it.ts)));
            caption(parts.join(" · "))
        }
    };
    let lead = if it.is_otp() { tile(icons::LOCK, color::ORANGE) } else { lead_node(st, it) };
    let mut kids = vec![lead, col([text(title).font_size(12.5), sub]).gap(1.0).grow(1.0)];
    kids.extend(smart::chip_row(app, it));
    // A one-time code cannot be pinned (it is gone after a minute).
    if !it.is_otp() {
        kids.push(
            stack([icon(icons::PIN).font_size(13.0).color(if it.pinned { color::YELLOW } else { color::TEXT3 })])
                .size(typo::MIN_TARGET, typo::MIN_TARGET)
                .action(format!("clipboard.pin:{}", it.key)),
        );
    }
    row(kids).gap(10.0).fill_w().pad4(10.0, 7.0, 6.0, 7.0).bg(color::FILL).radius(14.0).action(format!("clipboard.copy:{}", it.key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_image_is_a_well_formed_capture() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        match debug_image(3) {
            store::Captured::Image { thumb_png, w, h, orig_png, .. } => {
                assert_eq!((w, h), (320, 200));
                assert_eq!(model::png_dims(&thumb_png), Some((128, 80)));
                assert!(orig_png.is_some());
            }
            _ => panic!("expected an image"),
        }
    }

    #[test]
    fn age_text_is_localised() {
        assert!(!ago_text(Age::Minutes(2)).is_empty());
        assert_eq!(ago_text(Age::Yesterday), "kemarin");
    }
}
