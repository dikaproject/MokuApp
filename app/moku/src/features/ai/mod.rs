//! Feature `ai` (v0.3 track M): **Tanya AI**. Select text in any program, press the hotkey
//! (default Ctrl+Alt+A) or type `? question` in the launcher, and the island becomes a card
//! with Terjemahkan / Ringkas / Jelaskan / Perbaiki tulisan, a free question field and a
//! streaming answer, then Salin / Ganti teks / Ingat ini.
//!
//! Where the pieces are:
//! * `provider.rs` + `sse.rs`: the OpenAI-compatible streaming call (any provider, a preset or a
//!   custom base URL), through `crate::net`; the API key comes from Windows Credential Manager
//!   (`moku_core::secrets`, one key per provider host).
//! * `selection.rs` + `clip.rs`: the selected text (UI Automation, then a guarded Ctrl+C with the
//!   clipboard saved and restored) and "Ganti teks".
//! * `context.rs`, `redact.rs`, `prompts.rs`: what is sent. Every card shows **"Dikirim ke
//!   <host>"** and chips for the context (each removable for that request), "Lihat yang dikirim"
//!   shows the exact payload, OTPs / 16-digit numbers / tokens are hidden before sending, and
//!   clipboard history, notification bodies, OCR / screens, OTPs, agent sessions and a precise
//!   location are never part of it.
//! * Memory: `moku_core::aimem` (AES-256-GCM via CNG, key wrapped by DPAPI): facts the user
//!   approved ("Ingat ini") and an optional chat history. Nothing is cached: the file is read
//!   when a card opens and written on request, so the settings app and the island never disagree.
//!
//! Privacy: the only address ever contacted is the configured base URL; nothing the user typed or
//! an AI answered is logged; the feature is off until the user turns it on and gives a key (or
//! picks a provider on this PC, which needs none).
//!
//! Actions (`ask.<name>`): `act:<translate|summarize|explain|fix>`, `send`, `stop`, `copy`,
//! `replace`, `remember`, `fact_save`, `fact_cancel`, `chip:<key>`, `preview`, `close`,
//! `settings`; the inputs `ask.q` (question) and `ask.fact` (the fact being edited); test hooks
//! `ask.debug_open:<text>` and `ask.debug_ask:<question>` (they never read another program).

mod clip;
mod context;
mod prompts;
mod provider;
mod redact;
mod selection;
mod sse;

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, Timelike};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use crate::hotkeys::Outcome;
use moku_core::hotkey::Slot;
use windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY;

use self::prompts::{Action, Msg};
use crate::activity::{OSD_SECS, priority};
use crate::app::{App, Redraw, Ui};
use crate::i18n::{self, tr, tr_s};
use crate::net::Cancel;
use crate::registry::*;
use crate::ui::text_edit::{Key, KeyPress};
use crate::ui::*;
use moku_core::{aimem, secrets};

const SHEET: &str = "ask";
/// The question field (`ask.q`) and the fact editor (`ask.fact`).
const Q: &str = "ask.q";
const FACT: &str = "ask.fact";
const HOTKEY_ID: i32 = 150;
/// Lines of the answer shown on the card; more are cut with an ellipsis (Salin gives all of it).
const ANSWER_LINES: u32 = 8;
/// How often a growing answer is pushed to the screen.
const POST_EVERY: Duration = Duration::from_millis(60);
const PREVIEW_COLS: usize = 84;
const PREVIEW_ROWS: usize = 8;

#[derive(Default, Clone, PartialEq, Debug)]
enum Phase {
    #[default]
    Idle,
    Streaming,
    Done,
    Failed {
        msg: String,
        key: bool,
    },
}

/// A short message under the answer (copied, saved, a problem).
#[derive(Clone, Debug, PartialEq)]
struct Notice {
    text: String,
    error: bool,
}

#[derive(Default)]
struct AskState {
    open: bool,
    /// Counts requests; a worker's late result for an older one is ignored.
    req: u64,
    cancel: Option<Arc<Cancel>>,
    phase: Phase,
    action: Option<Action>,
    /// The selected text as read (raw, only ever shown on this card).
    selection: Option<String>,
    src: Option<selection::Source>,
    removed: Vec<&'static str>,
    items: Vec<context::Item>,
    facts: Vec<String>,
    /// Why the memory is not used right now ("Memori dijeda", "Memori tidak bisa dibuka").
    memory_note: Option<String>,
    /// Completed turns of this card (user / assistant), for follow-up questions.
    convo: Vec<Msg>,
    answer: String,
    truncated: bool,
    last_question: String,
    preview: bool,
    preview_at: usize,
    editing_fact: bool,
    notice: Option<Notice>,
    notice_seq: u64,
    hotkey: Option<String>,
    capturing: bool,
    replacing: bool,
}

pub fn register(r: &mut Registry) {
    r.add_state(AskState::default());
    r.on_action("ask", on_action);
    r.on_config(on_config);
    r.on_exit(on_exit);
    r.on_event(on_event);
    r.on_message(WM_HOTKEY, on_hotkey);
}

fn en() -> bool {
    i18n::is_english()
}

fn path() -> std::path::PathBuf {
    aimem::default_path()
}

// ---- hotkey ----------------------------------------------------------------------------------

fn on_config(app: &mut App) {
    sync_hotkey(app);
    if !app.config().ai.enabled {
        if app.state::<AskState>().open {
            close_card(app, true);
        }
        return;
    }
    if app.state::<AskState>().open {
        rebuild_items(app);
        app.request_redraw(Redraw::All);
    }
}

/// Only while the feature is on and the hotkeys are not let go (`released`: paused, or a full-screen
/// app in front, which keep the hotkey; `island.hide_apps` does not release it, the card then shows
/// the island for itself).
fn wanted_hotkey(enabled: bool, released: bool, hotkey: &str) -> Option<String> {
    (enabled && !released).then(|| hotkey.to_string())
}

fn sync_hotkey(app: &mut App) {
    if app.hit_hwnd().is_none() {
        return; // --state / --demo
    }
    let cfg = app.config().ai.clone();
    let want = wanted_hotkey(cfg.enabled, app.hotkeys_released(), &cfg.hotkey);
    if app.state::<AskState>().hotkey == want {
        return;
    }
    unregister(app);
    let Some(h) = want else { return };
    match crate::hotkeys::register(app.main_hwnd(), HOTKEY_ID, Slot::Ai, &h) {
        Outcome::Registered => app.state_mut::<AskState>().hotkey = Some(h),
        failed => crate::hotkeys::announce(app, Slot::Ai, &h, failed),
    }
}

fn unregister(app: &mut App) {
    if app.state_mut::<AskState>().hotkey.take().is_some() {
        crate::hotkeys::unregister(app.main_hwnd(), HOTKEY_ID, Slot::Ai);
    }
}

fn on_exit(app: &mut App) {
    unregister(app);
    cancel_request(app);
}

fn on_hotkey(app: &mut App, wp: WPARAM, _: LPARAM) {
    if wp.0 as i32 != HOTKEY_ID {
        return;
    }
    if !app.config().ai.enabled || app.hotkeys_released() {
        return;
    }
    // Pressed again while the card is up: close it.
    if app.state::<AskState>().open {
        close_card(app, true);
        return;
    }
    capture_and_open(app);
}

fn on_event(app: &mut App, ev: &AppEvent) {
    match ev {
        AppEvent::Visibility(_) | AppEvent::HotkeysChanged => sync_hotkey(app),
        AppEvent::SheetClosed(SHEET) if app.state::<AskState>().open => {
            // Closed from outside (the panel hotkey, pause, a full-screen app).
            reset(app);
            app.blur_input();
        }
        AppEvent::DataCleared => {
            if app.state::<AskState>().open {
                close_card(app, true);
            } else {
                reset(app);
            }
        }
        _ => {}
    }
}

// ---- opening and closing ---------------------------------------------------------------------

/// The hotkey: read the selection on a worker (UI Automation, then the guarded copy), then open
/// the card on the UI thread. Never reads or sends keys to anything but the program in front.
fn capture_and_open(app: &mut App) {
    if app.state::<AskState>().capturing {
        return;
    }
    let src = selection::foreground_source();
    let allow_copy = app.config().ai.copy_fallback;
    let owner = app.main_hwnd().0 as isize;
    app.state_mut::<AskState>().capturing = true;
    let spawned = std::thread::Builder::new().name("ai-capture".into()).spawn(move || {
        let found = src.as_ref().map(|s| selection::capture(s, allow_copy, owner));
        Ui::post(move |app| {
            app.state_mut::<AskState>().capturing = false;
            let text = found.and_then(|r| r.ok()).map(|(t, _)| t);
            // A terminal or our own window gives no text (and the card still opens for a question).
            open_card(app, text, src);
        });
    });
    if spawned.is_err() {
        app.state_mut::<AskState>().capturing = false;
    }
}

/// Reset everything the card shows (keeping the hotkey bookkeeping).
fn reset(app: &mut App) {
    cancel_request(app);
    let st = app.state_mut::<AskState>();
    let (hotkey, req) = (st.hotkey.take(), st.req);
    *st = AskState { hotkey, req: req + 1, ..Default::default() };
    app.input_mut(Q).clear();
    app.input_mut(FACT).clear();
}

fn cancel_request(app: &mut App) {
    if let Some(c) = app.state_mut::<AskState>().cancel.take() {
        c.cancel();
    }
}

fn open_card(app: &mut App, selection_text: Option<String>, src: Option<selection::Source>) {
    reset(app);
    let st = app.state_mut::<AskState>();
    st.open = true;
    st.selection = selection_text.map(|t| t.chars().take(selection::MAX_CHARS).collect());
    st.src = src;
    rebuild_items(app);
    app.open_sheet(SHEET, sheet);
    if !app.focus_input(Q) {
        log_info!("ai: card opened without the foreground (a click on the island takes it)");
    }
    refresh_memory(app);
    app.request_redraw(Redraw::All);
}

/// Close the card. `restore`: give the keyboard back to the program that had it.
fn close_card(app: &mut App, restore: bool) {
    reset(app);
    if restore {
        app.blur_input();
    }
    app.close_sheet();
}

/// Called by the launcher for `? question`: opens the card (no selection) and asks at once. The
/// launcher keeps the keyboard focus, so the card simply takes it over.
pub fn ask_from_launcher(app: &mut App, question: &str) {
    if !app.config().ai.enabled {
        return;
    }
    open_card(app, None, None);
    app.input_mut(Q).set_text(question.trim());
    start(app, Action::Ask);
}

// ---- context and memory ------------------------------------------------------------------------

fn timer_text(b: super::timer::Brief) -> String {
    let clock = super::timer::fmt_clock(b.remaining);
    let main = match (b.focus, b.on_break) {
        (true, false) => tr_s(format!("fokus berjalan, sisa {clock}"), format!("focus running, {clock} left")),
        (true, true) => tr_s(format!("istirahat fokus, sisa {clock}"), format!("focus break, {clock} left")),
        (false, _) if b.remaining > 0 => tr_s(format!("hitung mundur, sisa {clock}"), format!("countdown, {clock} left")),
        _ => String::new(),
    };
    let mut parts = vec![];
    if !main.is_empty() {
        parts.push(if b.paused { tr_s(format!("{main} (dijeda)"), format!("{main} (paused)")) } else { main });
    }
    if let Some(s) = b.stopwatch {
        parts.push(tr_s(format!("stopwatch {}", super::timer::fmt_clock(s)), format!("stopwatch {}", super::timer::fmt_clock(s))));
    }
    parts.join("; ")
}

/// Collect only what the toggles allow: a source that is off is not even read.
fn collect_inputs(app: &App, selection_text: Option<String>, facts: Vec<String>) -> context::Inputs {
    let cfg = app.config();
    let t = context::Toggles::from_config(&cfg.ai);
    let now = Local::now();
    let off = now.offset().local_minus_utc();
    let zone = format!("UTC{}{:02}:{:02}", if off < 0 { '-' } else { '+' }, off.abs() / 3600, off.abs() % 3600 / 60);
    let hm = |ts: i64| chrono::DateTime::from_timestamp(ts, 0).map(|d| d.with_timezone(&Local).format("%H:%M").to_string()).unwrap_or_default();
    let (mut todos, mut weather) = (vec![], None);
    if t.todos || t.weather {
        let (open, w) = super::today::ai_view(app);
        if t.todos {
            todos = open;
        }
        if t.weather {
            weather = w.map(|(c, code)| format!("{}, {}", super::today::fmt_temp(c, cfg.today.units), i18n::pair(super::today::describe(code).1)));
        }
    }
    let events = if t.calendar { super::calendar::ai_events(app).into_iter().map(|(all_day, s, e, title)| context::Event { all_day, start: hm(s), end: hm(e), title }).collect() } else { vec![] };
    let timer = if t.timer { super::timer::brief(app).map(timer_text).filter(|s| !s.is_empty()) } else { None };
    context::Inputs {
        when: (now.year(), now.month(), now.day(), now.weekday().num_days_from_monday(), now.hour(), now.minute()),
        zone,
        english: en(),
        city: Some(cfg.today.city.clone()).filter(|c| !c.trim().is_empty()),
        selection: selection_text,
        todos,
        events,
        timer,
        weather,
        facts,
    }
}

fn rebuild_items(app: &mut App) {
    let (sel, facts) = {
        let st = app.state::<AskState>();
        (st.selection.clone(), st.facts.clone())
    };
    let items = context::build(&collect_inputs(app, sel, facts), &context::Toggles::from_config(&app.config().ai));
    app.state_mut::<AskState>().items = items;
}

/// Read the facts for the card on a worker (a DPAPI call plus a small file), then rebuild.
fn refresh_memory(app: &mut App) {
    if app.config().ai.memory_paused {
        let st = app.state_mut::<AskState>();
        st.facts.clear();
        st.memory_note = Some(tr("Memori dijeda", "Memory paused").to_string());
        rebuild_items(app);
        return;
    }
    app.state_mut::<AskState>().memory_note = None;
    let req = app.state::<AskState>().req;
    let spawned = std::thread::Builder::new().name("ai-memory".into()).spawn(move || {
        let r = aimem::load(&path());
        Ui::post(move |app| {
            if app.state::<AskState>().req != req {
                return;
            }
            let st = app.state_mut::<AskState>();
            match r {
                Ok(m) => st.facts = m.facts_for_prompt(40, 2000),
                Err(aimem::LoadError::Missing) => st.facts = vec![],
                Err(aimem::LoadError::Unavailable(_)) => {
                    st.facts = vec![];
                    st.memory_note = Some(tr("Memori tidak bisa dibuka (lihat Pengaturan)", "Memory cannot be opened (see Settings)").to_string());
                }
            }
            rebuild_items(app);
            app.request_redraw(Redraw::All);
        });
    });
    let _ = spawned;
}

fn removed_keys(st: &AskState) -> Vec<&'static str> {
    st.removed.clone()
}

/// The messages for `action` with the question `question` (what `start` sends and the preview
/// shows: one function, so they cannot differ). `None`: there is no text to work on.
fn messages_for(app: &App, action: Action, question: &str) -> Option<(Vec<Msg>, Msg)> {
    let st = app.state::<AskState>();
    let removed = removed_keys(st);
    let cfg = &app.config().ai;
    let hide = |s: &str| if cfg.redact { redact::redact(s).text } else { s.to_string() };
    let question = hide(question.trim());
    let sel = context::selection(&st.items, &removed).map(|i| i.text.clone());
    let (text, q) = match action {
        Action::Ask => {
            if question.is_empty() {
                return None;
            }
            (sel, question)
        }
        _ => match sel.or_else(|| Some(question.clone()).filter(|q| !q.is_empty())) {
            // What was typed becomes the text itself.
            Some(t) => (Some(t), if context::selection(&st.items, &removed).is_some() { question } else { String::new() }),
            None => return None,
        },
    };
    let first_turn = action != Action::Ask || st.convo.is_empty();
    let user = Msg::user(prompts::user_message(action, text.as_deref(), &q, en(), first_turn));
    let mut msgs = vec![Msg::system(prompts::system_prompt(en(), &context::system_lines(&st.items, &removed), &context::fact_lines(&st.items, &removed)))];
    if action == Action::Ask {
        msgs.extend(st.convo.iter().cloned());
    }
    msgs.push(user.clone());
    Some((msgs, user))
}

// ---- asking ------------------------------------------------------------------------------------

fn notice(app: &mut App, text: impl Into<String>, error: bool) {
    let st = app.state_mut::<AskState>();
    st.notice_seq += 1;
    let seq = st.notice_seq;
    st.notice = Some(Notice { text: text.into(), error });
    app.after(Duration::from_secs(5), move |app| {
        let st = app.state_mut::<AskState>();
        if st.notice_seq == seq && st.notice.take().is_some() {
            app.request_redraw(Redraw::All);
        }
    });
    app.request_redraw(Redraw::All);
}

fn start(app: &mut App, action: Action) {
    if app.state::<AskState>().phase == Phase::Streaming {
        return;
    }
    let cfg = app.config().ai.clone();
    let typed = app.input(Q).text().trim().to_string();
    let Some((msgs, user)) = messages_for(app, action, &typed) else {
        let hint = if action == Action::Ask { tr("Ketik pertanyaan dulu", "Type a question first") } else { tr("Pilih teks di aplikasi lain, atau ketik teksnya di kolom", "Select text in another app, or type it in the field") };
        notice(app, hint, true);
        return;
    };
    let url = cfg.base_url.clone();
    let Some(key_name) = moku_core::ai::key_name(&url) else {
        app.state_mut::<AskState>().phase = Phase::Failed { msg: tr("Alamat penyedia tidak valid (Pengaturan > Tanya AI)", "The provider address is not valid (Settings > Ask AI)").into(), key: true };
        app.request_redraw(Redraw::All);
        return;
    };
    let local = moku_core::ai::is_local(&url);
    let host = moku_core::ai::host_label(&url).unwrap_or_default();
    let keep_history = cfg.history && !cfg.memory_paused;
    let retention = cfg.retention_days;
    let history_q = match action {
        Action::Ask => user.content.clone(),
        _ => format!("[{}] {}", action.label().0, user.content),
    };
    let english = en();

    cancel_request(app);
    let cancel = Arc::new(Cancel::new());
    let st = app.state_mut::<AskState>();
    st.req += 1;
    let req = st.req;
    st.cancel = Some(cancel.clone());
    st.phase = Phase::Streaming;
    st.action = Some(action);
    st.answer.clear();
    st.truncated = false;
    st.notice = None;
    st.preview = false;
    st.editing_fact = false;
    if action != Action::Ask {
        st.convo.clear();
    }
    st.last_question = typed.clone();
    // What was typed has been used (as the text, or as the question).
    if action != Action::Ask && !typed.is_empty() {
        app.input_mut(Q).clear();
    } else if action == Action::Ask {
        app.input_mut(Q).clear();
    }
    app.request_redraw(Redraw::All);

    let model = cfg.model.clone();
    let spawned = std::thread::Builder::new().name("ai-ask".into()).spawn(move || {
        let key = secrets::read(&key_name).ok().flatten();
        let mut outcome = provider::Outcome::default();
        if key.is_none() && !local {
            outcome.failure = Some(provider::Failure::Http { status: 401, detail: Some(tr("belum ada kunci API untuk penyedia ini", "no API key is set for this provider").to_string()) });
        } else {
            let p = provider::Provider { base_url: &url, model: &model, key: key.as_deref() };
            let (mut acc, mut last) = (String::new(), Instant::now() - POST_EVERY);
            outcome = provider::run(&p, &msgs, &cancel, &mut |piece| {
                acc.push_str(piece);
                if last.elapsed() >= POST_EVERY {
                    last = Instant::now();
                    let snapshot = acc.clone();
                    Ui::post(move |app| show_partial(app, req, snapshot));
                }
            });
        }
        let ok = outcome.failure.is_none() && !outcome.text.is_empty();
        if ok && keep_history {
            let (q, a, h) = (history_q, outcome.text.clone(), host);
            let now = chrono::Utc::now().timestamp();
            let r = aimem::update(&path(), |m| {
                m.add_history(action.id(), &q, &a, &h, now);
                m.sweep(now, retention);
            });
            if r.is_err() {
                log_warn!("ai: could not save the history");
            }
        }
        Ui::post(move |app| finished(app, req, outcome, user, english));
    });
    if spawned.is_err() {
        app.state_mut::<AskState>().phase = Phase::Failed { msg: tr("Tidak bisa memulai permintaan", "Could not start the request").into(), key: false };
    }
}

fn show_partial(app: &mut App, req: u64, text: String) {
    let st = app.state_mut::<AskState>();
    if st.req != req || st.phase != Phase::Streaming {
        return;
    }
    st.answer = text;
    app.request_redraw(Redraw::All);
}

fn finished(app: &mut App, req: u64, o: provider::Outcome, user: Msg, english: bool) {
    let st = app.state_mut::<AskState>();
    if st.req != req {
        return;
    }
    st.cancel = None;
    st.answer = o.text.clone();
    st.truncated = o.truncated;
    st.phase = match &o.failure {
        None => Phase::Done,
        Some(provider::Failure::Cancelled) => {
            if o.text.is_empty() {
                Phase::Idle
            } else {
                Phase::Done
            }
        }
        Some(f) => Phase::Failed { msg: f.message(english), key: f.is_key_problem() },
    };
    if o.failure.is_none() && !o.text.is_empty() {
        st.convo.push(user);
        st.convo.push(Msg::assistant(o.text));
        log_info!("ai: answer received");
    } else if let Some(f) = &o.failure {
        // The category only: never the text, the URL or the provider's message.
        log_info!("ai: request ended ({})", match f {
            provider::Failure::Cancelled => "cancelled".to_string(),
            provider::Failure::Http { status, .. } => format!("http {status}"),
            provider::Failure::Network(_) => "network".to_string(),
            provider::Failure::Provider(_) => "provider error".to_string(),
            provider::Failure::TooBig => "too big".to_string(),
            provider::Failure::Empty => "empty".to_string(),
            provider::Failure::Config(_) => "settings".to_string(),
        });
    }
    app.request_redraw(Redraw::All);
}

// ---- actions -----------------------------------------------------------------------------------

fn on_action(app: &mut App, ev: &ActionEvent) {
    // A card that was closed (from outside, or by the user) ignores late clicks and keys; only the
    // test hooks may open one.
    if !app.state::<AskState>().open && !ev.name().starts_with("debug_") {
        return;
    }
    match (ev.name(), ev.kind) {
        ("q", ActionKind::Key(k)) => on_q_key(app, k),
        ("q", ActionKind::Edited) => {
            if app.state_mut::<AskState>().notice.take().is_some() {
                app.request_redraw(Redraw::All);
            }
        }
        ("fact", ActionKind::Key(k)) => match k.key {
            Key::Enter => save_fact(app),
            Key::Escape => end_fact_edit(app),
            _ => {}
        },
        ("act", ActionKind::Click) => {
            if let Some(a) = ev.arg().and_then(Action::parse) {
                start(app, a);
            }
        }
        ("send", ActionKind::Click) => start(app, Action::Ask),
        ("stop", ActionKind::Click) => stop(app),
        ("copy", ActionKind::Click) => copy(app),
        ("replace", ActionKind::Click) => replace(app),
        ("remember", ActionKind::Click) => begin_fact_edit(app),
        ("fact_save", ActionKind::Click) => save_fact(app),
        ("fact_cancel", ActionKind::Click) => end_fact_edit(app),
        ("chip", ActionKind::Click) => {
            if let Some(k) = ev.arg().and_then(|k| context::KEYS.into_iter().find(|x| *x == k)) {
                let st = app.state_mut::<AskState>();
                match st.removed.iter().position(|x| *x == k) {
                    Some(i) => {
                        st.removed.remove(i);
                    }
                    None => st.removed.push(k),
                }
                app.request_redraw(Redraw::All);
            }
        }
        ("preview", ActionKind::Click) => {
            let st = app.state_mut::<AskState>();
            st.preview = !st.preview;
            st.preview_at = 0;
            app.request_redraw(Redraw::All);
        }
        ("scroll", ActionKind::Scroll(n)) => scroll_preview(app, -(n * 3.0).round() as isize),
        ("close", ActionKind::Click) => close_card(app, true),
        ("settings", ActionKind::Click) => {
            if let Some(pid) = open_settings("ai") {
                app.allow_foreground_for(pid);
            }
            close_card(app, true);
        }
        ("debug_open", ActionKind::Click) => {
            if crate::app::debug_enabled() {
                let t = ev.arg().map(str::to_string).filter(|t| !t.is_empty());
                // An invented source program (no window: nothing can be pasted into it), so the
                // card shows "Ganti teks" like it would after a real selection.
                let src = t.as_ref().map(|_| selection::Source { hwnd: 0, pid: 0, exe: "sample.exe".into(), class: "Sample".into() });
                open_card(app, t, src);
            }
        }
        ("debug_ask", ActionKind::Click) => {
            if crate::app::debug_enabled() {
                open_card(app, None, None);
                app.input_mut(Q).set_text(ev.arg().unwrap_or(""));
                start(app, Action::Ask);
            }
        }
        _ => {}
    }
}

fn on_q_key(app: &mut App, k: KeyPress) {
    match k.key {
        Key::Enter => start(app, Action::Ask),
        Key::Escape => {
            let st = app.state_mut::<AskState>();
            if st.preview {
                st.preview = false;
                app.request_redraw(Redraw::All);
            } else {
                close_card(app, true);
            }
        }
        Key::Down | Key::PageDown => scroll_preview(app, PREVIEW_ROWS as isize - 2),
        Key::Up | Key::PageUp => scroll_preview(app, -(PREVIEW_ROWS as isize - 2)),
        _ => {}
    }
}

fn stop(app: &mut App) {
    if app.state::<AskState>().phase != Phase::Streaming {
        return;
    }
    cancel_request(app);
    let st = app.state_mut::<AskState>();
    // A late result of the stopped request is ignored.
    st.req += 1;
    st.phase = if st.answer.is_empty() { Phase::Idle } else { Phase::Done };
    notice(app, tr("Dihentikan", "Stopped"), false);
}

fn copy(app: &mut App) {
    let a = app.state::<AskState>().answer.clone();
    if a.is_empty() {
        return;
    }
    let ok = app.set_clipboard_text(&a);
    notice(app, if ok { tr("Disalin", "Copied") } else { tr("Papan klip sedang dipakai", "The clipboard is busy") }, !ok);
}

/// Can the answer replace the selection in the program it came from?
fn can_replace(st: &AskState) -> bool {
    st.action.is_some_and(|a| a.rewrites()) && st.phase == Phase::Done && !st.answer.is_empty() && st.src.as_ref().is_some_and(|s| !selection::is_terminal(s)) && st.selection.is_some()
}

/// "Ganti teks": only from an explicit click. Gives the keyboard back to the program, waits for
/// it to be in front, pastes the answer with the clipboard saved and restored.
fn replace(app: &mut App) {
    let st = app.state::<AskState>();
    if !can_replace(st) || st.replacing {
        return;
    }
    let (Some(src), text) = (st.src.clone(), st.answer.clone()) else { return };
    let owner = app.main_hwnd().0 as isize;
    app.state_mut::<AskState>().replacing = true;
    // The island lets go of the keyboard first: the source program gets the foreground back.
    app.blur_input();
    let spawned = std::thread::Builder::new().name("ai-replace".into()).spawn(move || {
        let r = selection::paste_back(&src, &text, &clip::Win { owner }, &selection::RealForeground, &selection::CtrlKey { expect: src.hwnd, only_pid: None }, Duration::from_millis(350));
        Ui::post(move |app| {
            app.state_mut::<AskState>().replacing = false;
            match r {
                Ok(()) => {
                    close_card(app, false);
                    app.alert("ask.done", priority::OSD, OSD_SECS, |_| pill([icon(icons::CHECK).color(color::GREEN), text_pill(tr("Teks diganti", "Text replaced"))]));
                }
                Err(why) => {
                    let m = match why {
                        selection::PasteSkip::Terminal => tr("Tidak dipakai untuk terminal; pakai Salin", "Not used for terminals; use Copy"),
                        selection::PasteSkip::ClipboardNotSavable => tr("Isi papan klip tidak bisa disimpan dulu; pakai Salin", "The clipboard cannot be saved first; use Copy"),
                        selection::PasteSkip::ClipboardBusy => tr("Papan klip sedang dipakai", "The clipboard is busy"),
                        selection::PasteSkip::NoSourceWindow => tr("Aplikasi asal tidak di depan lagi; pakai Salin", "The original app is no longer in front; use Copy"),
                        selection::PasteSkip::NoKeys => tr("Tidak bisa menempel ke aplikasi itu; pakai Salin", "Could not paste into that app; use Copy"),
                    };
                    if app.state::<AskState>().open {
                        notice(app, m, true);
                    }
                }
            }
        });
    });
    if spawned.is_err() {
        app.state_mut::<AskState>().replacing = false;
    }
}

fn text_pill(s: &str) -> Node {
    text(s.to_string()).semibold()
}

// ---- "Ingat ini" -------------------------------------------------------------------------------

/// A starting point for the fact: what the user typed as a statement, else the start of the
/// selected text, else the first sentence of the answer. The user edits it before it is saved.
fn fact_candidate(st: &AskState) -> String {
    let q = st.last_question.trim();
    let pick = if !q.is_empty() && !q.ends_with('?') {
        q.to_string()
    } else if let Some(s) = st.selection.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        s.to_string()
    } else {
        let a = st.answer.trim();
        a.split_inclusive(['.', '!', '?', '\n']).next().unwrap_or(a).to_string()
    };
    let one = aimem::clean(&pick);
    let one = redact::redact(&one).text;
    one.chars().take(aimem::MAX_FACT_CHARS).collect()
}

fn begin_fact_edit(app: &mut App) {
    if app.config().ai.memory_paused {
        notice(app, tr("Memori sedang dijeda (Pengaturan > Memori & Privasi)", "Memory is paused (Settings > Memory & Privacy)"), true);
        return;
    }
    if app.state::<AskState>().memory_note.as_deref().is_some_and(|n| n.contains("tidak bisa") || n.contains("cannot")) {
        notice(app, tr("Memori tidak bisa dibuka (lihat Pengaturan > Memori & Privasi)", "Memory cannot be opened (see Settings > Memory & Privacy)"), true);
        return;
    }
    let cand = fact_candidate(app.state::<AskState>());
    app.state_mut::<AskState>().editing_fact = true;
    app.input_mut(FACT).set_text(&cand);
    app.focus_input(FACT);
    app.request_redraw(Redraw::All);
}

fn end_fact_edit(app: &mut App) {
    app.state_mut::<AskState>().editing_fact = false;
    app.input_mut(FACT).clear();
    app.focus_input(Q);
    app.request_redraw(Redraw::All);
}

fn save_fact(app: &mut App) {
    if !app.state::<AskState>().editing_fact {
        return;
    }
    let text = app.input(FACT).text().to_string();
    let now = chrono::Utc::now().timestamp();
    let req = app.state::<AskState>().req;
    // The file is read, changed and written on a worker; the card shows the result.
    let spawned = std::thread::Builder::new().name("ai-fact".into()).spawn(move || {
        let r = aimem::update(&path(), |m| m.add_fact(&text, now));
        Ui::post(move |app| {
            if app.state::<AskState>().req != req {
                return;
            }
            match r {
                Ok(Ok(_)) => {
                    end_fact_edit(app);
                    notice(app, tr("Diingat (tersimpan terenkripsi di PC ini)", "Remembered (stored encrypted on this PC)"), false);
                    refresh_memory(app);
                }
                Ok(Err(e)) => notice(app, e.message(), true),
                Err(_) => notice(app, tr("Memori tidak bisa dibuka atau ditulis (lihat Pengaturan > Memori & Privasi)", "Memory cannot be opened or written (see Settings > Memory & Privacy)"), true),
            }
        });
    });
    let _ = spawned;
}

// ---- preview -----------------------------------------------------------------------------------

/// Word-wrap `text` to `cols` columns (long words are cut).
fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut out = vec![];
    for raw in text.split('\n') {
        let mut line = String::new();
        for word in raw.split(' ') {
            let mut w: Vec<char> = word.chars().collect();
            loop {
                let room = cols.saturating_sub(line.chars().count() + usize::from(!line.is_empty()));
                if w.len() <= room {
                    if !line.is_empty() {
                        line.push(' ');
                    }
                    line.extend(w.iter());
                    break;
                }
                if line.is_empty() {
                    line.extend(w.drain(..cols.max(1).min(w.len())));
                }
                out.push(std::mem::take(&mut line));
                if w.is_empty() {
                    break;
                }
            }
        }
        out.push(line);
    }
    out
}

/// The preview of the next request, as lines: every message, labelled.
fn preview_lines(app: &App) -> Vec<String> {
    let st = app.state::<AskState>();
    let action = st.action.unwrap_or(if st.selection.is_some() { Action::Summarize } else { Action::Ask });
    let typed = app.input(Q).text().to_string();
    let question = if typed.trim().is_empty() && action == Action::Ask { tr("(pertanyaanmu)", "(your question)").to_string() } else { typed };
    let Some((msgs, _)) = messages_for(app, action, &question) else {
        return vec![tr("Belum ada yang akan dikirim: pilih teks atau ketik pertanyaan.", "Nothing to send yet: select text or type a question.").to_string()];
    };
    let mut out = vec![];
    // What the user wrote and the text come first; the standing instructions and context follow.
    for m in msgs.iter().rev() {
        let label = match m.role {
            "system" => tr("— Instruksi dan konteks —", "— Instructions and context —"),
            "assistant" => tr("— Jawaban sebelumnya —", "— Previous answer —"),
            _ => tr("— Pesanmu —", "— Your message —"),
        };
        out.push(label.to_string());
        out.extend(wrap(&m.content, PREVIEW_COLS));
        out.push(String::new());
    }
    out
}

fn scroll_preview(app: &mut App, delta: isize) {
    if !app.state::<AskState>().preview {
        return;
    }
    let total = preview_lines(app).len();
    let st = app.state_mut::<AskState>();
    let max = total.saturating_sub(PREVIEW_ROWS);
    st.preview_at = (st.preview_at as isize + delta).clamp(0, max as isize) as usize;
    app.request_redraw(Redraw::All);
}

// ---- settings --------------------------------------------------------------------------------

/// Start the settings app on a page; returns its process id.
fn open_settings(section: &str) -> Option<u32> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.with_file_name("moku-settings.exe");
    match std::process::Command::new(&path).args(["--section", section]).spawn() {
        Ok(child) => Some(child.id()),
        Err(e) => {
            log_error!("ai: cannot start {}: {e}", path.display());
            None
        }
    }
}

// ---- the card ----------------------------------------------------------------------------------

fn chip(label: String, removed: bool, key: &str) -> Node {
    let (glyph, op) = if removed { (icons::ADD, 0.45) } else { (icons::CLOSE, 1.0) };
    row([text(label).font_size(11.0).semibold(), icon(glyph).font_size(9.0).tertiary()]).gap(5.0).pad_xy(9.0, 0.0).h(22.0).bg(color::FILL2).capsule().opacity(op).action(format!("ask.chip:{key}"))
}

fn action_button(a: Action, active: bool, accent: Rgba) -> Node {
    let l = a.label();
    let label = tr(l.0, l.1);
    let b = row([text(label).font_size(12.0).semibold().color(if active { color::INK } else { color::WHITE })]).pad_xy(12.0, 0.0).h(28.0).capsule().action(format!("ask.act:{}", a.id()));
    if active { b.bg(color::WHITE) } else { b.bg(accent.fade(0.22)) }
}

fn header(app: &App) -> Node {
    let cfg = &app.config().ai;
    let host = moku_core::ai::host_label(&cfg.base_url);
    let sent = match &host {
        Some(h) if moku_core::ai::is_local(&cfg.base_url) => tr_s(format!("Dikirim ke {h} · di PC ini"), format!("Sent to {h} · on this PC")),
        Some(h) => tr_s(format!("Dikirim ke {h}"), format!("Sent to {h}")),
        None => tr("Alamat penyedia tidak valid", "The provider address is not valid").to_string(),
    };
    let privacy = if host.as_deref().is_some() && moku_core::ai::is_local(&cfg.base_url) {
        tr("Tidak ada data yang keluar dari PC ini", "Nothing leaves this PC")
    } else {
        tr("Setelah dikirim, privasi mengikuti penyedia AI-mu", "Once sent, privacy depends on your AI provider")
    };
    row([
        stack([text("✦").font_size(15.0).bold()]).size(30.0, 30.0).bg(app.base_accent()).radius(9.0),
        col([
            text(tr("Tanya AI", "Ask AI")).semibold(),
            row([icon(icons::LOCK).font_size(10.0).tertiary(), text(format!("{sent} · {}", cfg.model)).font_size(11.5).secondary()]).gap(4.0),
            text(privacy).font_size(10.5).tertiary(),
        ])
        .gap(1.0)
        .grow(1.0),
        icon_button(icons::CLOSE, "ask.close").name(tr("Tutup", "Close")),
    ])
    .gap(10.0)
    .fill_w()
}

fn chips(app: &App) -> Node {
    let st = app.state::<AskState>();
    let mut nodes: Vec<Node> = st.items.iter().map(|it| chip(it.label.clone(), st.removed.contains(&it.key), it.key)).collect();
    // Two rows at most: the first five chips, then the rest and the preview switch.
    let rest: Vec<Node> = if nodes.len() > 5 { nodes.split_off(5) } else { vec![] };
    let preview = row([icon(icons::EYE).font_size(11.0), text(tr("Lihat yang dikirim", "See what is sent")).font_size(11.0).semibold()]).gap(5.0).pad_xy(10.0, 0.0).h(22.0).bg(if st.preview { app.base_accent().fade(0.45) } else { color::FILL }).capsule().action("ask.preview");
    let mut r1 = nodes;
    let mut r2 = rest;
    if r2.is_empty() {
        r1.push(spacer());
        r1.push(preview);
        col([row(r1).gap(6.0).fill_w()]).gap(5.0).fill_w()
    } else {
        r2.push(spacer());
        r2.push(preview);
        col([row(r1).gap(6.0).fill_w(), row(r2).gap(6.0).fill_w()]).gap(5.0).fill_w()
    }
}

fn answer_area(app: &App) -> Node {
    let st = app.state::<AskState>();
    if st.preview {
        let lines = preview_lines(app);
        let from = st.preview_at.min(lines.len().saturating_sub(1));
        let shown: Vec<Node> = lines[from..(from + PREVIEW_ROWS).min(lines.len())].iter().map(|l| text(l.clone()).mono().font_size(11.0).secondary()).collect();
        let more = if lines.len() > PREVIEW_ROWS { caption(tr_s(format!("baris {}–{} dari {} · gulir untuk melihat sisanya", from + 1, (from + PREVIEW_ROWS).min(lines.len()), lines.len()), format!("lines {}–{} of {} · scroll for the rest", from + 1, (from + PREVIEW_ROWS).min(lines.len()), lines.len()))) } else { empty() };
        return col([col(shown).gap(1.0).fill_w(), more]).gap(5.0).fill_w().pad_xy(10.0, 8.0).bg(color::FILL).radius(12.0).action("ask.scroll");
    }
    let mut kids: Vec<Node> = vec![];
    match &st.phase {
        Phase::Streaming if st.answer.is_empty() => kids.push(row([spinner(), text(tr("Menunggu jawaban…", "Waiting for the answer…")).secondary()]).gap(10.0)),
        Phase::Idle if st.answer.is_empty() => {
            let hint = if st.selection.is_some() { tr("Pilih tindakan di atas, atau tanyakan sesuatu tentang teksnya.", "Pick an action above, or ask something about the text.") } else { tr("Pilih teks di aplikasi lain lalu tekan pintasan, atau ketik pertanyaan di bawah.", "Select text in another app and press the hotkey, or type a question below.") };
            kids.push(text(hint).secondary().lines(2));
        }
        // A failure before anything arrived has no answer to show, only the error below.
        Phase::Failed { .. } if st.answer.is_empty() => {}
        _ => kids.push(text(st.answer.clone()).lines(ANSWER_LINES)),
    }
    if st.truncated {
        kids.push(caption(tr("Jawaban dipotong karena terlalu panjang", "The answer was cut because it was too long")));
    }
    if let Phase::Failed { msg, key } = &st.phase {
        kids.push(row([icon(icons::WARNING).font_size(13.0).color(color::ORANGE), text(msg.clone()).color(color::ORANGE).lines(3).grow(1.0)]).gap(8.0).fill_w());
        if *key {
            kids.push(button(tr("Buka pengaturan Tanya AI", "Open Ask AI settings"), "ask.settings"));
        }
    }
    col(kids).gap(8.0).fill_w().pad_xy(12.0, 10.0).bg(color::FILL).radius(14.0)
}

fn result_buttons(app: &App) -> Node {
    let st = app.state::<AskState>();
    let mut btns: Vec<Node> = vec![];
    if st.phase == Phase::Done && !st.answer.is_empty() {
        btns.push(capsule(icons::COPY, tr("Salin", "Copy"), "ask.copy"));
        if can_replace(st) {
            btns.push(capsule(icons::SYNC, tr("Ganti teks", "Replace text"), "ask.replace"));
        }
        btns.push(capsule(icons::PIN, tr("Ingat ini", "Remember this"), "ask.remember"));
    }
    if let Some(n) = &st.notice {
        btns.push(spacer());
        btns.push(text(n.text.clone()).font_size(11.5).color(if n.error { color::ORANGE } else { color::GREEN }));
    } else if let Some(m) = &st.memory_note {
        btns.push(spacer());
        btns.push(caption(m.clone()));
    }
    if btns.is_empty() {
        return empty();
    }
    row(btns).gap(8.0).fill_w().h(28.0)
}

fn input_row(app: &App) -> Node {
    let st = app.state::<AskState>();
    let accent = app.base_accent();
    if st.editing_fact {
        return col([
            caption(tr("Ubah dulu kalau perlu, lalu Simpan. Disimpan terenkripsi di PC ini dan ikut dikirim ke penyedia AI saat memori dipakai.", "Edit it if needed, then Save. Stored encrypted on this PC; it is sent to your AI provider when the memory is used.")).lines(2),
            row([
                row([text_input(FACT, app.input(FACT)).placeholder(tr("Hal yang perlu diingat", "What to remember")).font_size(13.0).accent(accent).grow(1.0)]).pad_xy(14.0, 0.0).h(38.0).grow(1.0).bg(color::FILL).capsule(),
                button_primary(tr("Simpan", "Save"), "ask.fact_save"),
                button(tr("Batal", "Cancel"), "ask.fact_cancel"),
            ])
            .gap(8.0)
            .fill_w(),
        ])
        .gap(6.0)
        .fill_w();
    }
    let busy = st.phase == Phase::Streaming;
    row([
        row([text_input(Q, app.input(Q)).placeholder(tr("Tanya sesuatu…", "Ask something…")).font_size(13.0).accent(accent).grow(1.0)]).pad_xy(14.0, 0.0).h(38.0).grow(1.0).bg(color::FILL).capsule(),
        if busy { button(tr("Berhenti", "Stop"), "ask.stop") } else { button_primary(tr("Kirim", "Send"), "ask.send") },
    ])
    .gap(8.0)
    .fill_w()
}

fn sheet(app: &App) -> Node {
    let st = app.state::<AskState>();
    let accent = app.base_accent();
    let sel_excerpt = context::selection(&st.items, &removed_keys(st)).map(|i| {
        let t: String = moku_core::aimem::clean(&i.text);
        let t = if t.chars().count() > 150 { format!("{}…", t.chars().take(150).collect::<String>()) } else { t };
        row([icon(icons::DOCUMENT).font_size(12.0).tertiary(), text(t).font_size(12.0).secondary().lines(2).grow(1.0)]).gap(8.0).fill_w().pad_xy(10.0, 6.0).bg(color::FILL).radius(12.0)
    });
    let mut kids = vec![header(app), chips(app)];
    if let Some(n) = redact_note(st) {
        kids.push(row([icon(icons::SHIELD).font_size(11.0).color(color::GREEN), caption(n)]).gap(6.0));
    }
    kids.push(row(Action::ALL[..4].iter().map(|a| action_button(*a, st.action == Some(*a) && st.phase != Phase::Idle, accent))).gap(6.0));
    if let (Some(s), true) = (sel_excerpt, st.phase == Phase::Idle && st.answer.is_empty()) {
        kids.push(s);
    }
    kids.push(answer_area(app));
    // The preview is a read-only view of the payload: no answer buttons under it.
    if !st.preview {
        kids.push(result_buttons(app));
    }
    kids.push(input_row(app));
    col(kids).gap(8.0).fill_w()
}

fn redact_note(st: &AskState) -> Option<String> {
    let n = context::hidden_total(&st.items, &removed_keys(st));
    (n > 0).then(|| tr_s(format!("{n} data sensitif disembunyikan sebelum dikirim (OTP, nomor kartu, token)"), format!("{n} sensitive item(s) hidden before sending (OTP, card numbers, tokens)")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hotkey_is_registered_only_while_the_feature_is_on_and_the_island_visible() {
        assert_eq!(wanted_hotkey(true, false, "Ctrl+Alt+A"), Some("Ctrl+Alt+A".into()));
        assert_eq!(wanted_hotkey(true, true, "Ctrl+Alt+A"), None);
        assert_eq!(wanted_hotkey(false, false, "Ctrl+Alt+A"), None);
    }

    #[test]
    fn wrapping_keeps_words_and_cuts_long_ones() {
        assert_eq!(wrap("satu dua tiga empat lima", 10), ["satu dua", "tiga empat", "lima"]);
        assert_eq!(wrap("a\n\nb", 10), ["a", "", "b"]);
        assert_eq!(wrap(&"x".repeat(25), 10), ["xxxxxxxxxx", "xxxxxxxxxx", "xxxxx"]);
        assert_eq!(wrap("", 10), [""]);
        // Never an empty loop on odd widths.
        for cols in [0usize, 1, 2] {
            let w = wrap("halo dunia yang indah", cols);
            assert_eq!(w.concat().replace(' ', ""), "halodunia" .to_string() + "yangindah");
        }
        for line in wrap("kalimat yang agak panjang untuk diuji dengan lebar tertentu saja", 16) {
            assert!(line.chars().count() <= 16, "{line:?}");
        }
    }

    #[test]
    fn the_fact_candidate_is_the_statement_the_selection_or_the_first_sentence() {
        let mut st = AskState { last_question: "Saya tinggal di Surabaya".into(), ..Default::default() };
        assert_eq!(fact_candidate(&st), "Saya tinggal di Surabaya");
        // A question is not a fact: the selected text is offered instead.
        st.last_question = "Apa artinya?".into();
        st.selection = Some("  Rapat\ndengan Pak Budi  ".into());
        assert_eq!(fact_candidate(&st), "Rapat dengan Pak Budi");
        st.selection = None;
        st.answer = "Itu artinya selamat pagi. Dan seterusnya.".into();
        assert_eq!(fact_candidate(&st), "Itu artinya selamat pagi.");
        // Secrets never start out in the editor.
        st.last_question = "PIN saya 482913".into();
        assert!(!fact_candidate(&st).contains("482913"));
        // Never longer than a fact may be.
        st.last_question = "z ".repeat(600);
        assert!(fact_candidate(&st).chars().count() <= aimem::MAX_FACT_CHARS);
    }

    #[test]
    fn only_a_finished_rewrite_of_a_selection_in_an_ordinary_program_can_replace_it() {
        let src = selection::Source { hwnd: 5, pid: 77_777, exe: "notepad.exe".into(), class: "Notepad".into() };
        let mut st = AskState { action: Some(Action::Translate), phase: Phase::Done, answer: "Hello".into(), src: Some(src.clone()), selection: Some("Halo".into()), ..Default::default() };
        assert!(can_replace(&st));
        st.action = Some(Action::Summarize);
        assert!(!can_replace(&st), "a summary does not replace the text");
        st.action = Some(Action::Fix);
        st.phase = Phase::Streaming;
        assert!(!can_replace(&st), "not while it is still streaming");
        st.phase = Phase::Done;
        st.selection = None;
        assert!(!can_replace(&st), "no selection, nothing to replace (the question field is not a place in another program)");
        st.selection = Some("x".into());
        st.src = Some(selection::Source { exe: "windowsterminal.exe".into(), ..src.clone() });
        assert!(!can_replace(&st), "never into a terminal");
        st.src = None;
        assert!(!can_replace(&st), "a launcher question has no source program");
    }

    #[test]
    fn timer_text_is_short_and_has_no_secrets() {
        let b = super::super::timer::Brief { focus: true, on_break: false, paused: false, remaining: 750, stopwatch: None };
        assert_eq!(timer_text(b), "fokus berjalan, sisa 12:30");
        let b = super::super::timer::Brief { focus: false, on_break: false, paused: true, remaining: 60, stopwatch: Some(125) };
        assert_eq!(timer_text(b), "hitung mundur, sisa 01:00 (dijeda); stopwatch 02:05");
        let b = super::super::timer::Brief { focus: false, on_break: false, paused: false, remaining: 0, stopwatch: Some(5) };
        assert_eq!(timer_text(b), "stopwatch 00:05");
    }
}
