//! Feature `launcher` (v0.2 track K): a hotkey (default Alt+Space) turns the island into a
//! search field with up to 7 results — apps from the Start Menu and `shell:AppsFolder`, a
//! calculator, `todo <text>`, `timer 10m`, `> <settings page>` and recent Shelf files.
//!
//! * `matcher.rs` — fuzzy / acronym scoring; `history.rs` — usage boost (`launcher.json`)
//! * `calc.rs`, `command.rs` — what a query means (pure, tested)
//! * `enumerate.rs` — Start Menu / `shell:AppsFolder` / icon extraction, which only ever runs in
//!   the helper process (`moku.exe --launcher-index <file>`, see `crate::helper`)
//! * `index.rs` — the island's side: reads the helper's cache file, launching
//!
//! Memory: the island never loads the shell's heavy libraries for the index. When the launcher
//! first opens it shows the previous cache file if there is one (else a "Mengindeks…" row) and
//! starts the helper, which writes a fresh cache and exits; at most every 10 minutes while the
//! launcher is in use. The parsed cache and the icon bitmaps are dropped 2 minutes after the
//! launcher was last closed. Nothing trims the working set: what the island holds is only what
//! it uses.
//!
//! The UI is a core *sheet* (`App::open_sheet`) with a `text_input` that takes the keyboard
//! (`App::focus_input`); closing gives the foreground back to the window that had it.

pub(crate) mod calc;
mod command;
mod enumerate;
pub mod history;
mod index;
mod matcher;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY;

use self::command::{Command, PAGES};
use self::history::History;
use self::index::{Apps, Entry, Target};
use self::matcher::{Prepared, fold, score};
use crate::activity::{OSD_SECS, priority};
use crate::app::{App, Redraw, TimerId, Ui};
use crate::i18n::{self, tr, tr_s};
use crate::registry::*;
use crate::ui::text_edit::{Key, KeyPress};
use crate::ui::*;
use crate::hotkeys::Outcome;
use moku_core::hotkey::Slot;

/// The search field's input id (its edits arrive as actions `launcher.q`).
const INPUT: &str = "launcher.q";
const SHEET: &str = "launcher";
const HOTKEY_ID: i32 = 120;
const MAX_RESULTS: usize = 7;
/// Icons are drawn this many DIP wide; the 48 px cache icons are shrunk to the physical size.
const ICON_DIP: f32 = 24.0;
/// Run the helper at most this often while the launcher is used.
const REFRESH: Duration = Duration::from_secs(10 * 60);
/// A helper that takes longer than this is killed.
const HELPER_LIMIT: Duration = Duration::from_secs(90);
/// Drop the index and icons this long after the launcher was closed.
const IDLE_RELEASE: Duration = Duration::from_secs(2 * 60);

const GLYPH_APP: &str = "\u{ECAA}";
const GLYPH_CALC: &str = "\u{E8EF}";

#[derive(Clone, Debug, PartialEq)]
enum Act {
    Launch(Target),
    Copy(String),
    Todo(String),
    Timer(u64),
    /// Start the OCR region capture (`ocr.start`).
    Ocr,
    Settings(&'static str),
    /// Ask the AI (Tanya AI) this question.
    Ask(String),
    None,
}

#[derive(Clone, Debug, PartialEq)]
enum Lead {
    /// Icon from the cache by entry id (a glyph tile until it is loaded).
    Icon(String),
    Glyph(&'static str, Rgba),
}

#[derive(Clone, Debug)]
struct Item {
    /// Usage-history id (empty: not recorded).
    id: String,
    title: String,
    subtitle: String,
    hint: String,
    lead: Lead,
    act: Act,
}

#[derive(Default)]
struct Launcher {
    open: bool,
    results: Vec<Item>,
    sel: usize,
    /// The cache file's apps. `None` until the first open, and again after the idle release.
    index: Option<Apps>,
    /// When the helper last finished (successfully or not); a new run is due after `REFRESH`.
    indexed_at: Option<Instant>,
    /// A worker is reading the cache and / or the helper is running.
    indexing: bool,
    /// The helper failed and there is no cache to show.
    index_failed: bool,
    icons: HashMap<String, Image>,
    /// Shelf files whose icons were asked of the helper (once each).
    icon_wanted: HashSet<String>,
    icons_busy: bool,
    /// An app is being started: the sheet stays until that is done (see `run`), keys are ignored.
    launching: bool,
    history: Option<History>,
    idle: Option<TimerId>,
    /// Hotkey string registered now (`None` = not registered).
    hotkey: Option<String>,
}

/// The helper process's work (`moku.exe --launcher-index`): every launchable app with its icon.
pub(crate) fn build_index(store_apps: bool) -> Vec<crate::helper::pack::Rec<crate::helper::pack::Icon>> {
    enumerate::build(store_apps)
}

pub fn register(r: &mut Registry) {
    r.add_state(Launcher::default());
    r.on_action("launcher", on_action);
    r.on_config(on_config);
    r.on_exit(on_exit);
    r.on_event(on_event);
    r.on_message(WM_HOTKEY, on_hotkey);
}

// ---- hotkey ----------------------------------------------------------------------------------

fn on_config(app: &mut App) {
    sync_hotkey(app, true);
}

/// The hotkey the launcher wants registered: only while it is enabled AND the hotkeys are not let go
/// (`released`: paused from the tray, or a full-screen app or a presentation in front, which must not
/// lose Alt+Space to the launcher). An app of `island.hide_apps` in front does **not** release it: the
/// launcher still opens, and shows the island for itself (`App::open_sheet`).
fn wanted_hotkey(enabled: bool, released: bool, hotkey: &str) -> Option<String> {
    (enabled && !released).then(|| hotkey.to_string())
}

/// Register / unregister the global hotkey to match [`wanted_hotkey`]. `announce`: log a
/// successful registration (not on every hide / show).
fn sync_hotkey(app: &mut App, announce: bool) {
    let cfg = app.config().launcher.clone();
    // No windows to activate in --state / --demo.
    if app.hit_hwnd().is_none() {
        return;
    }
    let want = wanted_hotkey(cfg.enabled, app.hotkeys_released(), &cfg.hotkey);
    if app.state::<Launcher>().hotkey == want {
        return;
    }
    unregister(app);
    if !cfg.enabled {
        if app.state::<Launcher>().open {
            close(app, true);
        }
        return;
    }
    if want.is_none() {
        // Hidden: stay unregistered until the island is shown again.
        return;
    }
    match crate::hotkeys::register(app.main_hwnd(), HOTKEY_ID, Slot::Launcher, &cfg.hotkey) {
        Outcome::Registered => {
            app.state_mut::<Launcher>().hotkey = Some(cfg.hotkey.clone());
            if announce {
                log_info!("launcher: hotkey {} registered", cfg.hotkey);
            }
        }
        // Keep the setting; tell the user once per value (taken by another app, or by another Moku feature).
        failed => crate::hotkeys::announce(app, Slot::Launcher, &cfg.hotkey, failed),
    }
}

fn unregister(app: &mut App) {
    if app.state_mut::<Launcher>().hotkey.take().is_some() {
        crate::hotkeys::unregister(app.main_hwnd(), HOTKEY_ID, Slot::Launcher);
    }
}

fn on_exit(app: &mut App) {
    unregister(app);
    if app.state::<Launcher>().open {
        close(app, true);
    }
}

fn on_hotkey(app: &mut App, wp: WPARAM, _: LPARAM) {
    if wp.0 as i32 == HOTKEY_ID {
        toggle(app);
    }
}

/// The hotkey (and `mokuctl debug click launcher.debug_hotkey`): open ↔ close.
fn toggle(app: &mut App) {
    if app.state::<Launcher>().open {
        close(app, true);
    } else {
        open(app);
    }
}

fn on_event(app: &mut App, ev: &AppEvent) {
    match ev {
        // Paused or a full-screen app: let go of Alt+Space; back again: take it back.
        AppEvent::Visibility(_) | AppEvent::HotkeysChanged => sync_hotkey(app, false),
        // Closed from outside (panel hotkey, a feature opening the panel, pause, full screen).
        AppEvent::SheetClosed(SHEET) if app.state::<Launcher>().open => {
            app.state_mut::<Launcher>().open = false;
            app.blur_input();
            ended(app);
        }
        // A new scale means the icons were shrunk to the wrong size: shrink them again.
        AppEvent::DisplayChanged => {
            let st = app.state_mut::<Launcher>();
            st.icons.clear();
            st.icon_wanted.clear();
            if st.open {
                want_icons(app);
                app.request_redraw(Redraw::All);
            }
        }
        AppEvent::DataCleared => {
            app.cancel_save("launcher");
            app.state_mut::<Launcher>().history = Some(History::default());
        }
        _ => {}
    }
}

// ---- open / close ------------------------------------------------------------------------------

fn open(app: &mut App) {
    if !app.config().launcher.enabled || app.hotkeys_released() {
        return;
    }
    let st = app.state_mut::<Launcher>();
    st.open = true;
    if let Some(t) = st.idle.take() {
        app.cancel_timer(t);
    }
    ensure_history(app);
    ensure_index(app);
    app.input_mut(INPUT).clear();
    search(app);
    app.open_sheet(SHEET, sheet);
    let focused = app.focus_input(INPUT);
    if !focused {
        log_info!("launcher: opened without the foreground (a click on the island takes it)");
    }
}

/// Close. `restore` = give the foreground back to the window that had it (Esc, Enter, hotkey);
/// not after a focus loss (the user already went elsewhere).
fn close(app: &mut App, restore: bool) {
    app.state_mut::<Launcher>().open = false;
    if restore {
        app.blur_input();
    }
    app.close_sheet();
    ended(app);
}

/// After closing: forget the query, release memory after a while.
fn ended(app: &mut App) {
    app.input_mut(INPUT).clear();
    let st = app.state_mut::<Launcher>();
    st.results.clear();
    st.sel = 0;
    if let Some(t) = st.idle.take() {
        app.cancel_timer(t);
    }
    let t = app.after(IDLE_RELEASE, release);
    app.state_mut::<Launcher>().idle = Some(t);
}

/// Idle for 2 minutes: drop the index, icons and history from memory.
fn release(app: &mut App) {
    let st = app.state_mut::<Launcher>();
    st.idle = None;
    if st.open {
        return;
    }
    let n = st.index.as_ref().map_or(0, |a| a.entries.len());
    // The cache buffer (one block, with all icons) goes back to the system with the drop.
    st.index = None;
    st.icons = HashMap::new();
    st.icon_wanted = HashSet::new();
    st.results = Vec::new();
    app.cancel_save("launcher");
    app.state_mut::<Launcher>().history = None;
    log_info!("launcher: idle, released the index ({n} apps) and icons");
}

// ---- data -------------------------------------------------------------------------------------

fn history_path() -> std::path::PathBuf {
    moku_core::paths::data_file("launcher.json")
}

fn ensure_history(app: &mut App) {
    if app.state::<Launcher>().history.is_none() {
        // A few KB, read once per session of use (not on the UI thread's hot path).
        let h = std::fs::read(history_path()).map(|b| History::parse(&b)).unwrap_or_default();
        app.state_mut::<Launcher>().history = Some(h);
    }
}

fn save_history(app: &mut App) {
    app.save_later("launcher", |app| app.state::<Launcher>().history.as_ref().map(|h| (history_path(), h.to_json())));
}

/// Show the previous cache file at once and start the helper when a new run is due (first open,
/// then every `REFRESH` while the launcher is used). All on one worker thread; the island only
/// ever touches the cache file the helper wrote.
fn ensure_index(app: &mut App) {
    let st = app.state::<Launcher>();
    let due = st.indexed_at.is_none_or(|t| t.elapsed() > REFRESH);
    let need_load = st.index.is_none();
    if st.indexing || !(due || need_load) {
        return;
    }
    app.state_mut::<Launcher>().indexing = true;
    let store = app.config().launcher.store_apps;
    // Screenshots / tests: invented apps instead of the real Start Menu (debug only).
    let sample = crate::app::debug_enabled() && std::env::var_os("MOKU_LAUNCHER_SAMPLE").is_some();
    let spawned = std::thread::Builder::new().name("launcher-index".into()).spawn(move || {
        let path = index::cache_path();
        if need_load && !sample {
            // The previous run's cache: usable while the helper works on a fresh one.
            if let Some(apps) = index::load(&path) {
                Ui::post(move |app| {
                    let st = app.state_mut::<Launcher>();
                    if st.index.is_none() {
                        st.index = Some(apps);
                        if st.open {
                            search(app);
                        }
                    }
                });
            }
        }
        if !due && !sample {
            Ui::post(|app| app.state_mut::<Launcher>().indexing = false);
            return;
        }
        let t = Instant::now();
        let ran = crate::helper::run_index(&path, store, sample, HELPER_LIMIT);
        let apps = ran.as_ref().ok().and_then(|_| index::load(&path));
        let ms = t.elapsed().as_millis();
        Ui::post(move |app| {
            let st = app.state_mut::<Launcher>();
            st.indexing = false;
            st.indexed_at = Some(Instant::now());
            match (apps, ran) {
                (Some(a), _) => {
                    log_info!("launcher: the helper indexed {} apps in {ms} ms", a.entries.len());
                    st.index = Some(a);
                    st.index_failed = false;
                }
                (None, ran) => {
                    log_warn!("launcher: indexing failed: {}", ran.err().unwrap_or_else(|| "the cache file is unreadable".into()));
                    st.index_failed = st.index.is_none();
                }
            }
            if st.open {
                search(app);
            } else if st.idle.is_none() {
                // Closed meanwhile: still release it later.
                let t = app.after(IDLE_RELEASE, release);
                app.state_mut::<Launcher>().idle = Some(t);
            }
        });
    });
    if spawned.is_err() {
        app.state_mut::<Launcher>().indexing = false;
    }
}

/// Icons of the shown results. Apps come from the cache buffer (shrunk to the drawn size, on
/// the spot: a few KB); Shelf files are not in the cache, so the helper extracts theirs
/// (`--icons`) once per file on a worker thread, and the row shows a glyph tile until then.
fn want_icons(app: &mut App) {
    let px = ((ICON_DIP * app.scale()).round() as u32).clamp(16, 96);
    let Launcher { results, icons, index, icon_wanted, icons_busy, .. } = app.state_mut::<Launcher>();
    let mut files: Vec<(String, String)> = vec![];
    for it in results.iter() {
        let Lead::Icon(id) = &it.lead else { continue };
        if icons.contains_key(id) {
            continue;
        }
        match &it.act {
            Act::Launch(Target::File(p)) => {
                if icon_wanted.insert(id.clone()) {
                    files.push((id.clone(), p.clone()));
                }
            }
            _ => {
                if let Some(img) = index.as_ref().and_then(|a| a.entries.iter().find(|e| &e.id == id).and_then(|e| a.image(e, px))) {
                    icons.insert(id.clone(), img);
                }
            }
        }
    }
    if files.is_empty() || *icons_busy {
        // Files asked for while a helper runs are asked for again once it is done.
        if !files.is_empty() {
            for (id, _) in &files {
                icon_wanted.remove(id);
            }
        }
        return;
    }
    *icons_busy = true;
    let spawned = std::thread::Builder::new().name("launcher-icons".into()).spawn(move || {
        let paths: Vec<String> = files.iter().map(|(_, p)| p.clone()).collect();
        let set = crate::helper::run_icons(&paths, Duration::from_secs(20));
        let got: Vec<(String, Image)> = set
            .map(|set| {
                // The helper echoes each path with its icon: match by path.
                let id_of: HashMap<&str, &str> = files.iter().map(|(id, p)| (p.as_str(), id.as_str())).collect();
                set.items
                    .iter()
                    .filter_map(|(path, r)| {
                        let (id, r) = (id_of.get(path.as_str())?, r.as_ref()?);
                        let (w, h, bgra) = crate::helper::pack::downscale(set.bgra(r), r.w as u32, r.h as u32, px, px);
                        Some((id.to_string(), Image::new(w, h, bgra)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ui::post(move |app| {
            let st = app.state_mut::<Launcher>();
            st.icons_busy = false;
            if st.index.is_some() || st.open {
                st.icons.extend(got);
            }
            app.request_redraw(Redraw::All);
            want_icons(app);
        });
    });
    if spawned.is_err() {
        app.state_mut::<Launcher>().icons_busy = false;
    }
}

// ---- results ----------------------------------------------------------------------------------

fn english() -> bool {
    i18n::is_english()
}

fn search(app: &mut App) {
    let t = Instant::now();
    let q = app.input(INPUT).text().to_string();
    let items = build_items(app, &q);
    let n = app.state::<Launcher>().index.as_ref().map_or(0, |a| a.entries.len());
    let st = app.state_mut::<Launcher>();
    st.results = items;
    st.sel = 0;
    want_icons(app);
    app.request_redraw(Redraw::All);
    if crate::app::debug_enabled() {
        log_info!("launcher: search over {n} apps took {} µs", t.elapsed().as_micros());
    }
}

fn app_item(e: &Entry) -> Item {
    let kind = tr("Aplikasi", "App");
    Item {
        id: e.id.clone(),
        title: e.name.clone(),
        subtitle: match &e.detail {
            Some(d) => format!("{kind} · {d}"),
            None => kind.to_string(),
        },
        hint: tr("Buka", "Open").into(),
        lead: Lead::Icon(e.id.clone()),
        act: Act::Launch(e.target.clone()),
    }
}

fn page_item(p: &command::Page) -> Item {
    Item {
        id: format!("page:{}", p.key),
        title: tr_s(format!("Pengaturan · {}", p.title.0), format!("Settings · {}", p.title.1)),
        subtitle: tr("Pengaturan Moku", "Moku settings").into(),
        hint: tr("Buka", "Open").into(),
        lead: Lead::Glyph(icons::SETTINGS, Rgba::hex(0x8E8E93, 1.0)),
        act: Act::Settings(p.key),
    }
}

fn file_item(path: &str) -> Item {
    let name = path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(path).to_string();
    let id = format!("file:{}", path.to_lowercase());
    // Programs and scripts are only shown in Explorer, never started (see `index::plan`).
    let (subtitle, hint) = if index::plan(&Target::File(path.to_string())) == index::Plan::Reveal {
        (tr("Program di Shelf · ditampilkan di folder", "Program on the Shelf · shown in its folder"), tr("Tampilkan", "Show"))
    } else {
        (tr("File di Shelf", "File on the Shelf"), tr("Buka", "Open"))
    };
    Item { id: id.clone(), title: name, subtitle: subtitle.into(), hint: hint.into(), lead: Lead::Icon(id), act: Act::Launch(Target::File(path.to_string())) }
}

/// The rows for an empty query. Apps and pages the user picked before come first; Shelf files
/// only after them and never as the first row, so Enter on an empty query can not open a file
/// (without any pick, the hint row heads the list and Enter does nothing).
fn empty_rows(picks: Vec<Item>, files: Vec<Item>, placeholder: Vec<Item>) -> Vec<Item> {
    let mut out = if picks.is_empty() { placeholder } else { picks };
    let room = MAX_RESULTS.saturating_sub(out.len()).min(3);
    out.extend(files.into_iter().take(room));
    out.truncate(MAX_RESULTS);
    out
}

fn info_item(title: String, subtitle: String, glyph: &'static str, c: Rgba) -> Item {
    Item { id: String::new(), title, subtitle, hint: String::new(), lead: Lead::Glyph(glyph, c), act: Act::None }
}

fn build_items(app: &App, q: &str) -> Vec<Item> {
    let st = app.state::<Launcher>();
    let now = chrono::Utc::now().timestamp();
    let empty = History::default();
    let hist = st.history.as_ref().unwrap_or(&empty);
    let entries: &[Entry] = st.index.as_ref().map_or(&[], |a| a.entries.as_slice());
    let shelf: Vec<String> = if app.config().launcher.shelf_files && app.config().shelf.enabled { super::shelf::recent_files(app, 20) } else { vec![] };
    // No app list yet: say so (first run: the helper is still reading the Start Menu).
    let loading = || {
        if st.index_failed {
            info_item(tr("Aplikasi tidak bisa diindeks", "Could not index the apps").into(), tr("Coba buka lagi sebentar lagi", "Try again in a moment").into(), GLYPH_APP, color::ORANGE)
        } else {
            info_item(tr("Mengindeks…", "Indexing…").into(), tr("Pertama kali bisa butuh beberapa detik", "The first time can take a few seconds").into(), GLYPH_APP, color::FILL3)
        }
    };
    let comma = !english();
    // `ocr` with OCR switched off is no command: it is searched like any word (no row that does nothing).
    let command = match command::parse(q) {
        Command::Ocr if !app.config().ocr.enabled => Command::Search(q.trim().to_string()),
        c => c,
    };
    match command {
        Command::Empty => {
            let mut picks: Vec<Item> = vec![];
            for id in hist.top(MAX_RESULTS, now) {
                if let Some(e) = entries.iter().find(|e| e.id == id) {
                    picks.push(app_item(e));
                } else if let Some(p) = PAGES.iter().find(|p| format!("page:{}", p.key) == id) {
                    picks.push(page_item(p));
                }
            }
            let mut placeholder = vec![];
            if st.index.is_none() {
                placeholder.push(loading());
            }
            placeholder.push(info_item(tr("Ketik untuk mencari aplikasi", "Type to search apps").into(), tr("atau 12*3, todo …, timer 10m, > island", "or 12*3, todo …, timer 10m, > island").into(), icons::SEARCH, color::FILL3));
            empty_rows(picks, shelf.iter().map(|f| file_item(f)).collect(), placeholder)
        }
        Command::Search(s) => {
            let fq = fold(&s);
            let mut scored: Vec<(i32, Item)> = vec![];
            for e in entries {
                if let Some((_, sc)) = score(&fq, &e.prep) {
                    scored.push((sc + hist.boost(&e.id, &fq, now), app_item(e)));
                }
            }
            for p in &PAGES {
                let best = [p.title.0, p.title.1, p.key].iter().filter_map(|t| score(&fq, &Prepared::new(t))).map(|(_, s)| s).max();
                if let Some(sc) = best {
                    let id = format!("page:{}", p.key);
                    // Settings pages rank below apps of the same tier.
                    scored.push((sc - 60 + hist.boost(&id, &fq, now), page_item(p)));
                }
            }
            for f in &shelf {
                let it = file_item(f);
                if let Some((_, sc)) = score(&fq, &Prepared::new(&it.title)) {
                    scored.push((sc - 40 + hist.boost(&it.id, &fq, now), it));
                }
            }
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
            let mut out: Vec<Item> = scored.into_iter().take(MAX_RESULTS).map(|(_, i)| i).collect();
            if out.is_empty() {
                out.push(if st.index.is_none() {
                    loading()
                } else {
                    info_item(tr("Tidak ada hasil", "No results").into(), tr("Coba nama lain, atau 12*3, todo …, timer 10m", "Try another name, or 12*3, todo …, timer 10m").into(), icons::SEARCH, color::FILL3)
                });
            }
            out
        }
        Command::Calc(Ok(v)) => {
            let shown = calc::format(v, comma, true);
            let plain = calc::format(v, comma, false);
            vec![Item {
                id: String::new(),
                title: shown,
                subtitle: tr_s(format!("{} · Enter untuk menyalin", q.trim()), format!("{} · Enter to copy", q.trim())),
                hint: tr("Salin", "Copy").into(),
                lead: Lead::Glyph(GLYPH_CALC, color::ORANGE),
                act: Act::Copy(plain),
            }]
        }
        Command::Calc(Err(e)) => {
            let why = match e {
                calc::CalcError::DivByZero => tr("Tidak bisa dibagi nol", "Cannot divide by zero"),
                calc::CalcError::Range => tr("Hasilnya terlalu besar", "The result is too large"),
                calc::CalcError::Syntax => tr("Rumus belum lengkap", "Incomplete expression"),
            };
            vec![info_item(why.into(), q.trim().to_string(), GLYPH_CALC, color::ORANGE)]
        }
        Command::Todo(t) if t.is_empty() => vec![info_item(tr("Tambah to-do", "Add a to-do").into(), tr("Ketik: todo beli kopi", "Type: todo buy coffee").into(), icons::CHECK, color::GREEN)],
        Command::Todo(t) => {
            // The same words as the Hari ini field: a time in them ("rapat jam 3") is a reminder.
            let (shown, subtitle) = match super::today::preview(&t) {
                Some((title, when)) => (title, tr_s(format!("To-do · pengingat {when}"), format!("To-do · reminder {when}"))),
                None => (t.clone(), tr("Hari ini · To-do", "Today · To-do").to_string()),
            };
            vec![Item {
                id: String::new(),
                title: tr_s(format!("Tambah to-do: {shown}"), format!("Add to-do: {shown}")),
                subtitle,
                hint: tr("Tambah", "Add").into(),
                lead: Lead::Glyph(icons::CHECK, color::GREEN),
                act: if app.config().today.enabled { Act::Todo(t) } else { Act::None },
            }]
        }
        Command::Timer(Some(secs)) => vec![Item {
            id: String::new(),
            title: tr_s(format!("Mulai timer {}", super::timer::fmt_clock(secs)), format!("Start a {} timer", super::timer::fmt_clock(secs))),
            subtitle: tr("Timer Moku", "Moku timer").into(),
            hint: tr("Mulai", "Start").into(),
            lead: Lead::Glyph(icons::TIMER, color::RED),
            act: if app.config().timer.enabled { Act::Timer(secs) } else { Act::None },
        }],
        Command::Timer(None) => vec![info_item(tr("Mulai timer", "Start a timer").into(), tr("Ketik: timer 10m, timer 90s, timer 1h30m", "Type: timer 10m, timer 90s, timer 1h30m").into(), icons::TIMER, color::RED)],
        Command::Ocr => vec![Item {
            id: String::new(),
            title: tr("Salin teks dari layar", "Copy text from the screen").into(),
            subtitle: tr("OCR: seret untuk memilih area", "OCR: drag to select an area").into(),
            hint: tr("Mulai", "Start").into(),
            lead: Lead::Glyph(icons::DOCUMENT, color::TEAL),
            act: Act::Ocr,
        }],
        Command::Ask(q) if q.is_empty() => vec![info_item(tr("Tanya AI", "Ask AI").into(), tr("Ketik: ? apa itu Rust", "Type: ? what is Rust").into(), icons::MESSAGE, color::BLUE)],
        Command::Ask(q) => {
            let on = app.config().ai.enabled;
            let host = moku_core::ai::host_label(&app.config().ai.base_url).unwrap_or_default();
            vec![Item {
                id: String::new(),
                title: tr_s(format!("Tanya AI: {q}"), format!("Ask AI: {q}")),
                subtitle: if on { tr_s(format!("Dikirim ke {host}"), format!("Sent to {host}")) } else { tr("Nyalakan Tanya AI di Pengaturan", "Turn on Ask AI in Settings").to_string() },
                hint: tr("Tanya", "Ask").into(),
                lead: Lead::Glyph(icons::MESSAGE, color::BLUE),
                act: if on { Act::Ask(q) } else { Act::None },
            }]
        }
        Command::Settings(s) => {
            let fq = fold(&s);
            let mut v: Vec<(i32, &command::Page)> = PAGES
                .iter()
                .enumerate()
                .filter_map(|(i, p)| {
                    if fq.is_empty() {
                        return Some((-(i as i32), p));
                    }
                    [p.title.0, p.title.1, p.key].iter().filter_map(|t| score(&fq, &Prepared::new(t))).map(|(_, s)| s).max().map(|s| (s, p))
                })
                .collect();
            v.sort_by(|a, b| b.0.cmp(&a.0));
            // An empty `>` lists every page (12): more rows than fit, so the sheet scrolls (`window_start`).
            let out: Vec<Item> = v.into_iter().map(|(_, p)| page_item(p)).collect();
            if out.is_empty() { vec![info_item(tr("Halaman tidak ditemukan", "No such page").into(), String::new(), icons::SETTINGS, color::FILL3)] } else { out }
        }
    }
}

// ---- actions ----------------------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    match (ev.name(), ev.kind) {
        ("q", ActionKind::Edited) => search(app),
        ("q", ActionKind::Key(k)) => on_key(app, k),
        // The user went to another window / clicked outside: close, focus is already elsewhere.
        ("q", ActionKind::Blur) => {
            if app.state::<Launcher>().open {
                close(app, false);
            }
        }
        ("run", ActionKind::Click) => {
            if let Some(i) = ev.arg().and_then(|a| a.parse::<usize>().ok()) {
                run(app, i);
            }
        }
        // The wheel over the rows walks the selection (the list scrolls when it is longer than the sheet).
        ("run", ActionKind::Scroll(notches)) if notches != 0.0 => {
            let n = app.state::<Launcher>().results.len();
            move_sel(app, if notches > 0.0 { -1 } else { 1 }, n);
        }
        ("debug_hotkey", ActionKind::Click) => toggle(app),
        _ => {}
    }
}

fn on_key(app: &mut App, k: KeyPress) {
    // An app is being started: nothing else to do until the sheet closes (Esc still works).
    if app.state::<Launcher>().launching && k.key != Key::Escape {
        return;
    }
    let n = app.state::<Launcher>().results.len();
    match k.key {
        Key::Escape => close(app, true),
        Key::Enter => {
            let i = app.state::<Launcher>().sel;
            run(app, i);
        }
        Key::Down | Key::Tab if !k.shift => move_sel(app, 1, n),
        Key::Up | Key::Tab => move_sel(app, -1, n),
        Key::PageDown => move_sel(app, n as isize, n),
        Key::PageUp => move_sel(app, -(n as isize), n),
        _ => {
            // Alt+1..7 are the rows on screen: relative to the window when the list scrolls.
            if let Some(i) = alt_digit(&k) {
                let st = app.state::<Launcher>();
                let abs = window_start(st.sel, st.results.len(), MAX_RESULTS) + i;
                run(app, abs);
            }
        }
    }
}

/// `Alt+1`..`Alt+7` pick that row directly. Only with Ctrl NOT down: AltGr is reported as
/// Ctrl+Alt and types a character (AltGr+3 is "#" on some layouts), it is no shortcut.
fn alt_digit(k: &KeyPress) -> Option<usize> {
    match k.key {
        Key::Char(c @ '1'..='7') if k.alt && !k.ctrl => Some(c as usize - '1' as usize),
        _ => None,
    }
}

fn move_sel(app: &mut App, d: isize, n: usize) {
    if n == 0 {
        return;
    }
    let st = app.state_mut::<Launcher>();
    st.sel = (st.sel as isize + d).clamp(0, n as isize - 1) as usize;
    app.request_redraw(Redraw::All);
}

fn run(app: &mut App, i: usize) {
    let Some(item) = app.state::<Launcher>().results.get(i).cloned() else { return };
    if item.act == Act::None {
        return;
    }
    if !item.id.is_empty() {
        let q = fold(app.input(INPUT).text().trim());
        let now = chrono::Utc::now().timestamp();
        if let Some(h) = app.state_mut::<Launcher>().history.as_mut() {
            h.record(&item.id, &q, now);
        }
        save_history(app);
    }
    match item.act {
        Act::Launch(target) => {
            // Start it FIRST: only then is its process id known, and only that process may be
            // allowed to take the foreground (never "any process"). We still hold the foreground
            // now; the sheet closes, and the foreground goes back to where it was, right after.
            app.state_mut::<Launcher>().launching = true;
            let spawned = std::thread::Builder::new().name("launcher-run".into()).spawn(move || {
                let r = index::launch(&target);
                if !r.ok {
                    log_warn!("launcher: could not open the selected item");
                }
                Ui::post(move |app| {
                    if let Some(pid) = r.pid {
                        app.allow_foreground_for(pid);
                    }
                    app.state_mut::<Launcher>().launching = false;
                    if app.state::<Launcher>().open {
                        close(app, true);
                    }
                });
            });
            if let Err(e) = spawned {
                log_warn!("launcher: {e}");
                app.state_mut::<Launcher>().launching = false;
                close(app, true);
            }
        }
        Act::Copy(s) => {
            let ok = app.set_clipboard_text(&s);
            close(app, true);
            let msg = if ok { tr_s(format!("Disalin · {s}"), format!("Copied · {s}")) } else { tr("Papan klip sedang dipakai", "The clipboard is busy").into() };
            app.alert("launcher.done", priority::OSD, OSD_SECS, move |_| pill([icon(icons::COPY).color(color::ORANGE), text(msg.clone()).semibold()]));
        }
        Act::Todo(s) => {
            let r = super::today::add(app, &s);
            close(app, true);
            let msg = match r {
                Ok(()) => tr("To-do ditambahkan", "To-do added").to_string(),
                Err(e) => e.to_string(),
            };
            app.alert("launcher.done", priority::OSD, OSD_SECS, move |_| pill([icon(icons::CHECK).color(color::GREEN), text(msg.clone()).semibold()]));
        }
        Act::Timer(secs) => {
            close(app, true);
            app.dispatch_action(&format!("timer.start:{secs}"), ActionKind::Click);
        }
        Act::Ocr => {
            close(app, true);
            app.dispatch_action("ocr.start", ActionKind::Click);
        }
        Act::Settings(key) => {
            if let Some(pid) = open_settings(key) {
                app.allow_foreground_for(pid);
            }
            close(app, true);
        }
        Act::Ask(q) => {
            // The card takes over the keyboard: this sheet closes WITHOUT giving the foreground
            // back (the card opens right away and keeps what the launcher had taken).
            app.state_mut::<Launcher>().open = false;
            app.close_sheet();
            ended(app);
            super::ai::ask_from_launcher(app, &q);
        }
        Act::None => {}
    }
}

/// Start the settings app on a page; returns its process id.
fn open_settings(section: &str) -> Option<u32> {
    let Ok(exe) = std::env::current_exe() else { return None };
    let path = exe.with_file_name("moku-settings.exe");
    match std::process::Command::new(&path).args(["--section", section]).spawn() {
        Ok(child) => Some(child.id()),
        Err(e) => {
            log_error!("launcher: cannot start {}: {e}", path.display());
            None
        }
    }
}

// ---- UI ---------------------------------------------------------------------------------------

/// A small key-cap chip ("Esc", "Enter", "Alt+1").
fn keycap(s: impl Into<String>, strong: bool) -> Node {
    let t = text(s).font_size(10.5).semibold();
    let (t, bg) = if strong { (t.color(color::INK), Rgba::hex(0xffffff, 0.9)) } else { (t.tertiary(), color::FILL2) };
    row([t]).pad_xy(8.0, 0.0).h(20.0).bg(bg).capsule()
}

fn lead(app: &App, l: &Lead) -> Node {
    match l {
        Lead::Icon(id) => match app.state::<Launcher>().icons.get(id) {
            Some(img) => stack([image(img, 4.0).size(24.0, 24.0)]).size(30.0, 30.0),
            None => tile(GLYPH_APP, color::FILL2).size(30.0, 30.0),
        },
        Lead::Glyph(g, c) => tile(g, *c).size(30.0, 30.0),
    }
}

/// The first row shown when `n` rows do not all fit in `max`: the window follows the selection (the
/// selected row is the last visible one while going down).
fn window_start(sel: usize, n: usize, max: usize) -> usize {
    if n <= max { 0 } else { (sel + 1).saturating_sub(max).min(n - max) }
}

fn sheet(app: &App) -> Node {
    let st = app.state::<Launcher>();
    let field = row([
        icon(icons::SEARCH).font_size(16.0).color(color::TEXT2),
        text_input(INPUT, app.input(INPUT)).placeholder(tr("Cari aplikasi, hitung, todo, timer…", "Search apps, calculate, todo, timer…")).font_size(15.0).accent(app.base_accent()).grow(1.0),
        keycap("Esc", false),
    ])
    .gap(12.0)
    .pad_xy(16.0, 0.0)
    .h(44.0)
    .fill_w()
    .bg(color::FILL)
    .capsule();
    let mut children = vec![field];
    // While an IME composes ("にほ" is not committed yet) the field's text is still empty, so
    // the empty-state hint ("Ketik untuk mencari…") would sit under what is being typed.
    let composing = app.input(INPUT).composition().is_some_and(|c| !c.is_empty());
    let start = window_start(st.sel, st.results.len(), MAX_RESULTS);
    for (i, it) in st.results.iter().enumerate().skip(start).take(MAX_RESULTS) {
        if hide_info_row(it, composing) {
            continue;
        }
        let selected = i == st.sel && it.act != Act::None;
        let mut texts = col([text(it.title.clone()).semibold()]).gap(1.0).grow(1.0);
        if !it.subtitle.is_empty() {
            texts = texts.child(caption(it.subtitle.clone()));
        }
        let chip = if it.act == Act::None {
            empty()
        } else if selected {
            row([text(it.hint.clone()).font_size(11.0).tertiary(), keycap("Enter", true)]).gap(8.0)
        } else {
            keycap(format!("Alt+{}", i - start + 1), false)
        };
        let mut r = row([lead(app, &it.lead), texts, chip]).gap(12.0).pad_xy(10.0, 0.0).h(44.0).fill_w().radius(16.0);
        if selected {
            r = r.bg(color::FILL2);
        }
        if it.act != Act::None {
            r = r.action(format!("launcher.run:{i}"));
        }
        children.push(r);
    }
    children.push(footer(st.results.len().checked_sub(MAX_RESULTS).filter(|more| *more > 0).map(|_| (start, st.results.len()))));
    col(children).gap(4.0).fill_w()
}

/// The hint rows (no action: "Ketik untuk mencari…", "Mengindeks…") are hidden while an IME
/// composition is under way.
fn hide_info_row(it: &Item, composing: bool) -> bool {
    composing && it.act == Act::None
}

/// The keys of the launcher as key-cap chips with a readable label each (not one 46 % line).
/// `more` = (first visible row, all rows) when the list is longer than the sheet shows ("1–7 dari 12").
fn footer(more: Option<(usize, usize)>) -> Node {
    let hint = |key: &str, what: &str| row([keycap(key.to_string(), false), text(what.to_string()).font_size(11.5).secondary()]).gap(6.0);
    let mut kids = vec![
        hint("↑↓", tr("pilih", "select")),
        hint("Enter", tr("jalankan", "run")),
        hint("Alt+1–7", tr("langsung", "direct")),
        hint("Esc", tr("tutup", "close")),
    ];
    if let Some((start, all)) = more {
        kids.push(spacer());
        kids.push(caption(tr_s(format!("{}–{} dari {all}", start + 1, (start + MAX_RESULTS).min(all)), format!("{}–{} of {all}", start + 1, (start + MAX_RESULTS).min(all)))));
    }
    row(kids).gap(16.0).pad_xy(8.0, 0.0).h(24.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_list_scrolls_with_the_selection() {
        // 12 settings pages, 7 rows on the sheet.
        let n = 12;
        assert_eq!(window_start(0, n, MAX_RESULTS), 0);
        assert_eq!(window_start(6, n, MAX_RESULTS), 0, "the 7th row is still on the first screen");
        assert_eq!(window_start(7, n, MAX_RESULTS), 1, "going down: the selection is the last visible row");
        assert_eq!(window_start(11, n, MAX_RESULTS), 5, "the last page");
        // Always shows the selected row and never leaves the list.
        for sel in 0..n {
            let s = window_start(sel, n, MAX_RESULTS);
            assert!((s..s + MAX_RESULTS).contains(&sel) && s + MAX_RESULTS <= n, "sel {sel} -> start {s}");
        }
        // Going up from the bottom keeps the window until the selection leaves it.
        assert_eq!(window_start(5, n, MAX_RESULTS), 0);
        // A list that fits never scrolls.
        for (sel, len) in [(0, 0), (0, 1), (3, 7), (6, 7)] {
            assert_eq!(window_start(sel, len, MAX_RESULTS), 0);
        }
        // Every page of the settings app is reachable by scrolling: 12 pages, and the empty `>` lists them all.
        assert_eq!(PAGES.len(), 12);
        assert!(PAGES.len() > MAX_RESULTS);
    }

    fn entry(name: &str) -> Entry {
        Entry { id: format!("lnk:{name}"), name: name.into(), detail: None, target: Target::Link(name.into()), prep: Prepared::new(name), icon: None }
    }

    #[test]
    fn the_hotkey_is_registered_only_while_the_island_is_on_screen() {
        assert_eq!(wanted_hotkey(true, false, "Alt+Space"), Some("Alt+Space".to_string()));
        assert_eq!(wanted_hotkey(true, true, "Alt+Space"), None, "hidden: Alt+Space stays with the program in front");
        assert_eq!(wanted_hotkey(false, false, "Alt+Space"), None, "disabled in the settings");
        assert_eq!(wanted_hotkey(false, true, "Alt+Space"), None);
    }

    #[test]
    fn the_empty_state_hint_is_hidden_while_an_ime_composes() {
        let hint = info_item("Ketik".into(), String::new(), icons::SEARCH, color::FILL3);
        assert!(hide_info_row(&hint, true));
        assert!(!hide_info_row(&hint, false), "shown when nothing is being composed");
        assert!(!hide_info_row(&pick("Visual Studio Code"), true), "real results stay");
    }

    #[test]
    fn alt_digits_are_shortcuts_but_altgr_digits_are_not() {
        let key = |c: char, ctrl: bool, alt: bool| KeyPress { key: Key::Char(c), ctrl, shift: false, alt };
        assert_eq!(alt_digit(&key('1', false, true)), Some(0));
        assert_eq!(alt_digit(&key('7', false, true)), Some(6));
        assert_eq!(alt_digit(&key('8', false, true)), None, "only the 7 visible rows");
        assert_eq!(alt_digit(&key('3', false, false)), None, "a plain digit is typed");
        assert_eq!(alt_digit(&key('3', true, true)), None, "AltGr (Ctrl+Alt) + 3 is a character");
        assert_eq!(alt_digit(&key('3', true, false)), None);
    }

    fn pick(name: &str) -> Item {
        app_item(&entry(name))
    }

    #[test]
    fn an_empty_query_never_puts_a_shelf_file_first() {
        let files = || vec![file_item(r"C:\Users\x\Desktop\setup.exe"), file_item(r"C:\docs\report.pdf")];
        let hint = || vec![info_item("Ketik".into(), String::new(), icons::SEARCH, color::FILL3)];
        // No picks yet: the hint row (Enter does nothing) heads the list, the files follow.
        let rows = empty_rows(vec![], files(), hint());
        assert_eq!(rows[0].act, Act::None);
        assert!(matches!(rows[1].act, Act::Launch(Target::File(_))));
        // Picks (apps, recent) first.
        let rows = empty_rows(vec![pick("Visual Studio Code"), pick("Snipping Tool")], files(), hint());
        assert!(matches!(rows[0].act, Act::Launch(Target::Link(_))));
        assert!(matches!(rows[2].act, Act::Launch(Target::File(_))));
        assert!(rows.iter().all(|r| r.title != "Ketik"));
        // Never more than MAX_RESULTS rows, whatever there is.
        let many: Vec<Item> = (0..9).map(|i| pick(&format!("App {i}"))).collect();
        assert_eq!(empty_rows(many, files(), hint()).len(), MAX_RESULTS);
        // A program is shown, not opened: its row says so.
        assert_eq!(file_item(r"C:\Users\x\Desktop\setup.exe").hint, "Tampilkan");
        assert_eq!(file_item(r"C:\docs\report.pdf").hint, "Buka");
    }

    /// The ranking the search uses, on entries without a window.
    #[test]
    fn search_ranks_with_history() {
        let entries = [entry("Visual Studio Code"), entry("Visio"), entry("Snipping Tool"), entry("VLC media player")];
        let mut h = History::default();
        let rank = |h: &History, q: &str| {
            let fq = fold(q);
            let mut v: Vec<(i32, &str)> = entries.iter().filter_map(|e| score(&fq, &e.prep).map(|(_, s)| (s + h.boost(&e.id, &fq, 0), e.name.as_str()))).collect();
            v.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
            v.into_iter().map(|(_, n)| n).collect::<Vec<_>>()
        };
        assert_eq!(rank(&h, "vsc")[0], "Visual Studio Code");
        assert_eq!(rank(&h, "vis")[0], "Visio", "shorter prefix match first");
        // Picking VS Code for "vis" once makes it the first result for "vis".
        h.record("lnk:Visual Studio Code", "vis", 0);
        assert_eq!(rank(&h, "vis")[0], "Visual Studio Code");
    }
}
