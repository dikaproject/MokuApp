//! Feature `notify` — Task 6 (track E).
//!
//! IPC notifications (`mokuctl notify` / `mokuctl progress`):
//! * a notification pill (priority 80, transient 5 s) for the newest one;
//! * progress notifications as a persistent pill (priority 55) updated in place, finished by
//!   `progress >= 1` (a short "done" pill) or by a plain notify with the same id; a progress
//!   that stops updating for [`STALE`] is dropped from the pill (kept in the history);
//! * the Notifikasi tab: history (`ipc.max_notifications`), coalesced by `id`, unread count,
//!   per-item dismiss, clear all, "Jangan ganggu" (do-not-disturb, this session only), and the
//!   pending AI permission cards on top (`ai_cards`);
//! * while `app.focus_hold()` (focus mode) or do-not-disturb is on, notifications go to the
//!   history without a pill; when the hold ends, one summary pill tells how many arrived;
//! * in a meeting with `privacy.hide_notification_body_in_meetings`, bodies are hidden.

mod history;

use std::collections::HashMap;
use std::time::Duration;

use chrono::Local;
use moku_core::ipc::Request;

use crate::activity::{Activity, NOTIFICATION_SECS, priority};
use crate::app::{App, Redraw, TimerId};
use crate::i18n::{self, tr, tr_s};
use crate::ipc_server::Reply;
use crate::registry::*;
use crate::ui::*;

use history::{History, Incoming, Item};

/// A running progress that gets no update for this long leaves the pill.
const STALE: Duration = Duration::from_secs(10 * 60);
/// Rows shown in the tab at once (scroll for more).
const ROWS: usize = 5;
/// Activity ids.
const PILL: &str = "notify.pill";
const HELD: &str = "notify.held";
const PROGRESS: &str = "notify.progress:";

#[derive(Default)]
struct Notify {
    history: History,
    /// The item the notification pill shows.
    pill_seq: Option<u64>,
    /// Running progress pills: key → (item seq, stale timer).
    running: HashMap<String, (u64, TimerId)>,
    /// Do-not-disturb (header toggle), this session only.
    dnd: bool,
    /// Notifications that arrived while held (focus / DND).
    held: u32,
    /// First row shown in the tab (scroll).
    scroll: usize,
}

pub fn register(r: &mut Registry) {
    r.add_state(Notify::default());
    r.add_ipc_handler(IpcKind::Notify, on_notify);
    r.on_action("notify", on_action);
    r.on_event(on_event);
    r.on_config(on_config);
    r.add_tab(TabSpec {
        id: "notify",
        order: order::NOTIFY,
        icon: icons::BELL,
        label: ("Notifikasi", "Notifications"),
        enabled: |c| c.ipc.enabled,
        build: panel,
        tick: Some((Duration::from_secs(30), |app| app.request_redraw(Redraw::Tab("notify")))),
    });
}

fn cap(app: &App) -> usize {
    app.config().ipc.max_notifications as usize
}

/// Is do-not-disturb on (the Kontrol tab's tile shows and toggles it, through `notify.dnd`)?
pub fn dnd_on(app: &App) -> bool {
    app.state::<Notify>().dnd
}

fn held(app: &App) -> bool {
    app.focus_hold() || app.state::<Notify>().dnd
}

fn on_config(app: &mut App) {
    let c = cap(app);
    app.state_mut::<Notify>().history.set_cap(c);
}

fn on_notify(app: &mut App, req: Request, reply: Reply) {
    let Request::Notify { title, body, icon, id, progress } = req else { return };
    if title.trim().is_empty() {
        return reply.error("title is empty");
    }
    let n = Incoming { id, title, body, icon, progress };
    let key = n.key();
    let c = cap(app);
    let (seq, _) = app.state_mut::<Notify>().history.push(n, c, Local::now());
    reply.ok();
    let Some(item) = app.state::<Notify>().history.get(seq).cloned() else { return };
    let is_held = held(app);
    if item.running() {
        // Start / update the progress pill (priority 55), restart its stale timer.
        let key = key.unwrap_or_default();
        set_running(app, &key, seq, is_held);
    } else {
        // A finished progress (or a plain notify with the id of a running one) ends it.
        if let Some(k) = &key {
            stop_running(app, k);
        }
        if is_held {
            app.state_mut::<Notify>().held += 1;
        } else {
            show_pill(app, seq);
        }
    }
    app.request_redraw(Redraw::Tab("notify"));
}

fn show_pill(app: &mut App, seq: u64) {
    app.state_mut::<Notify>().pill_seq = Some(seq);
    app.set_activity(Activity::transient(PILL, priority::NOTIFICATION, NOTIFICATION_SECS, notification_pill).tab("notify"));
}

fn set_running(app: &mut App, key: &str, seq: u64, is_held: bool) {
    if let Some((_, t)) = app.state_mut::<Notify>().running.remove(key) {
        app.cancel_timer(t);
    }
    let k = key.to_string();
    let timer = app.after(STALE, move |app| {
        app.state_mut::<Notify>().running.remove(&k);
        app.clear_activity(&format!("{PROGRESS}{k}"));
    });
    app.state_mut::<Notify>().running.insert(key.to_string(), (seq, timer));
    if !is_held {
        show_progress(app, key, seq);
    }
}

fn show_progress(app: &mut App, key: &str, seq: u64) {
    app.set_activity(Activity::persistent(format!("{PROGRESS}{key}"), priority::PROGRESS, move |app| progress_pill(app, seq)).tab("notify").bubble(move |app| progress_bubble(app, seq)));
}

fn stop_running(app: &mut App, key: &str) {
    if let Some((_, t)) = app.state_mut::<Notify>().running.remove(key) {
        app.cancel_timer(t);
    }
    app.clear_activity(&format!("{PROGRESS}{key}"));
}

fn on_event(app: &mut App, ev: &AppEvent) {
    match ev {
        AppEvent::FocusHoldChanged(false) => release_held(app),
        AppEvent::FocusHoldChanged(true) => hide_progress_pills(app),
        AppEvent::TabHidden("notify") => {
            let s = app.state_mut::<Notify>();
            s.history.mark_all_read();
            s.scroll = 0;
        }
        AppEvent::MeetingChanged => app.request_redraw(Redraw::All),
        // "Hapus semua data": the notification centre is user data too (kept in memory only).
        AppEvent::DataCleared => {
            let s = app.state_mut::<Notify>();
            s.history.clear();
            s.scroll = 0;
            app.request_redraw(Redraw::Tab("notify"));
        }
        _ => {}
    }
}

fn hide_progress_pills(app: &mut App) {
    let keys: Vec<String> = app.state::<Notify>().running.keys().cloned().collect();
    for k in keys {
        app.clear_activity(&format!("{PROGRESS}{k}"));
    }
}

/// Focus / DND ended: bring back running progress pills and say how many arrived meanwhile.
fn release_held(app: &mut App) {
    if held(app) {
        return;
    }
    let running: Vec<(String, u64)> = app.state::<Notify>().running.iter().map(|(k, (s, _))| (k.clone(), *s)).collect();
    for (k, s) in running {
        show_progress(app, &k, s);
    }
    let n = std::mem::take(&mut app.state_mut::<Notify>().held);
    if n > 0 {
        app.set_activity(Activity::transient(HELD, priority::NOTIFICATION, NOTIFICATION_SECS, move |app| held_pill(app, n)).tab("notify"));
    }
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    let seq = ev.arg().and_then(|a| a.parse::<u64>().ok());
    match (ev.name(), ev.kind) {
        ("dismiss", ActionKind::Click) => {
            if let Some(s) = seq
                && let Some(it) = app.state_mut::<Notify>().history.dismiss(s)
            {
                if let Some(k) = &it.key {
                    stop_running(app, k);
                }
                if app.state::<Notify>().pill_seq == Some(s) {
                    app.clear_activity(PILL);
                }
            }
            clamp_scroll(app);
            app.request_redraw(Redraw::All);
        }
        ("clear", ActionKind::Click) => {
            let keys: Vec<String> = app.state::<Notify>().running.keys().cloned().collect();
            for k in keys {
                stop_running(app, &k);
            }
            let s = app.state_mut::<Notify>();
            s.history.clear();
            s.scroll = 0;
            s.pill_seq = None;
            app.clear_activity(PILL);
            app.clear_activity(HELD);
            app.request_redraw(Redraw::All);
        }
        ("dnd", ActionKind::Click) => {
            let on = !app.state::<Notify>().dnd;
            app.state_mut::<Notify>().dnd = on;
            if on {
                hide_progress_pills(app);
            } else {
                release_held(app);
            }
            app.request_redraw(Redraw::All);
        }
        ("list", ActionKind::Scroll(notches)) => {
            let s = app.state_mut::<Notify>();
            s.scroll = if notches > 0.0 { s.scroll.saturating_sub(1) } else { s.scroll + 1 };
            clamp_scroll(app);
            app.request_redraw(Redraw::Tab("notify"));
        }
        ("open", ActionKind::Click) => app.open_panel(Some("notify")),
        _ => {}
    }
}

fn clamp_scroll(app: &mut App) {
    let s = app.state_mut::<Notify>();
    s.scroll = s.scroll.min(s.history.len().saturating_sub(ROWS));
}

// ---- look -----------------------------------------------------------------------------------

/// Colour of an icon tile by icon name (the mockup gives each kind its own colour).
fn tint(icon: Option<&str>, accent: Rgba) -> Rgba {
    match icon.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("download" | "upload" | "file" | "document" | "folder") => color::PURPLE,
        Some("check" | "done" | "success" | "message" | "chat" | "mic") => color::GREEN,
        Some("warning" | "warn" | "ai" | "lightning" | "code" | "terminal") => color::ORANGE,
        Some("error" | "close") => color::RED,
        Some("calendar" | "timer" | "info" | "sync") => color::BLUE,
        Some("music" | "photo" | "image" | "camera") => color::PINK,
        _ => accent,
    }
}

fn glyph(icon: Option<&str>) -> &str {
    icon.map(icons::by_name).unwrap_or(icons::BELL)
}

fn item_tile(it: &Item, accent: Rgba, size: f32) -> Node {
    stack([icon(glyph(it.icon.as_deref())).font_size((size * 0.47).round())]).size(size, size).bg(tint(it.icon.as_deref(), accent)).radius((size * 0.3).round())
}

/// The body, or "content hidden" while a meeting runs (privacy setting).
fn body_text(app: &App, it: &Item) -> Option<String> {
    let hide = app.meeting().active && app.config().privacy.hide_notification_body_in_meetings;
    match &it.body {
        Some(_) if hide => Some(tr("Isi disembunyikan selama rapat", "Hidden during the meeting").to_string()),
        b => b.clone(),
    }
}

fn when(it: &Item) -> String {
    let now = Local::now();
    let age = now.signed_duration_since(it.time);
    if age.num_seconds() < 60 {
        tr("kini", "now").to_string()
    } else if it.time.date_naive() == now.date_naive() {
        i18n::clock(&it.time)
    } else {
        i18n::date_short(&it.time)
    }
}

fn percent(p: f32) -> String {
    format!("{}%", (p * 100.0).round() as i32)
}

/// Notification pill per the mockup ("Nadia · Pesan / Meeting-nya jadi jam 3 ya?  kini").
fn notification_pill(app: &App) -> Node {
    let s = app.state::<Notify>();
    let Some(it) = s.pill_seq.and_then(|q| s.history.get(q)) else { return empty() };
    match body_text(app, it) {
        Some(body) => row([
            item_tile(it, app.base_accent(), 30.0),
            col([text(it.title.clone()).semibold(), text(body).font_size(12.5).secondary()]).gap(1.0).max_w(300.0),
            caption(tr("kini", "now")),
        ])
        .gap(10.0)
        .h(50.0)
        .pad4(10.0, 0.0, 16.0, 0.0),
        // Short form ("✓ Claude Code selesai").
        None => {
            let mut items = vec![icon(glyph(it.icon.as_deref())).color(tint(it.icon.as_deref(), app.base_accent())), text(it.title.clone()).semibold().max_w(340.0)];
            if it.progress.is_some() {
                // A finished progress ("⬇ moku-setup.exe  selesai").
                items.push(text(tr("selesai", "done")).secondary());
            }
            pill(items)
        }
    }
}

/// Progress pill per the mockup ("⬇ moku-setup.exe ━━━━── 64%").
fn progress_pill(app: &App, seq: u64) -> Node {
    let Some(it) = app.state::<Notify>().history.get(seq) else { return empty() };
    let p = it.progress.unwrap_or(0.0);
    let ic = it.icon.as_deref().or(Some("download"));
    pill([
        icon(glyph(ic)).color(color::BLUE),
        text(it.title.clone()).semibold().max_w(240.0),
        progress(p).color(color::BLUE).w(60.0),
        text(percent(p)).font_size(12.0).tertiary(),
    ])
}

fn progress_bubble(app: &App, seq: u64) -> Node {
    let p = app.state::<Notify>().history.get(seq).and_then(|i| i.progress).unwrap_or(0.0);
    stack([ring(p).size(20.0, 20.0).color(color::BLUE)])
}

fn held_pill(_app: &App, n: u32) -> Node {
    pill([
        icon(icons::BELL).color(color::ORANGE),
        text(tr_s(format!("{n} notifikasi"), format!("{n} notification{}", if n == 1 { "" } else { "s" }))).semibold(),
        text(tr("masuk saat ditahan", "arrived while held")).secondary(),
    ])
}

fn row_node(app: &App, it: &Item) -> Node {
    let mut texts = col([text(it.title.clone()).semibold()]).gap(2.0).grow(1.0);
    if let Some(p) = it.progress.filter(|_| it.running()) {
        texts = texts.child(row([progress(p).color(color::BLUE).w(160.0), caption(percent(p))]).gap(8.0));
    } else if let Some(b) = body_text(app, it) {
        texts = texts.child(text(b).font_size(12.5).secondary());
    } else if it.progress.is_some() {
        texts = texts.child(text(tr("Selesai", "Done")).font_size(12.5).secondary());
    }
    let dot = if it.unread { stack([empty()]).size(6.0, 6.0).bg(app.base_accent()).capsule() } else { empty() };
    row([
        item_tile(it, app.base_accent(), 32.0),
        texts,
        col([caption(when(it)), dot]).gap(4.0).align(Align::End),
        stack([icon(icons::CLOSE).font_size(10.0).color(color::TEXT3)]).size(24.0, 24.0).capsule().action(format!("notify.dismiss:{}", it.seq)),
    ])
    .gap(10.0)
    .fill_w()
    .pad4(10.0, 8.0, 6.0, 8.0)
    .bg(color::FILL)
    .radius(16.0)
}

fn header(app: &App) -> Node {
    let s = app.state::<Notify>();
    let unread = s.history.unread();
    let sub = if s.history.is_empty() {
        String::new()
    } else if unread == 0 {
        tr("semua sudah dibaca", "all read").to_string()
    } else {
        tr_s(format!("{unread} belum dibaca"), format!("{unread} unread"))
    };
    let dnd_on = s.dnd || app.focus_hold();
    let dnd_label = if app.focus_hold() { tr("Ditahan fokus", "Held by focus") } else { tr("Jangan ganggu", "Do not disturb") };
    let dnd = row([icon(icons::MOON).font_size(12.0), text(dnd_label).font_size(12.0).semibold()])
        .gap(6.0)
        .pad_xy(12.0, 0.0)
        .h(28.0)
        .bg(if dnd_on { app.base_accent() } else { color::FILL2 })
        .capsule()
        .action("notify.dnd");
    let mut items = vec![text(tr("Notifikasi", "Notifications")).font_size(typo::CARD_TITLE).bold(), caption(sub), spacer(), dnd];
    if !s.history.is_empty() {
        items.push(row([text(tr("Bersihkan", "Clear all")).font_size(12.0).semibold()]).pad_xy(12.0, 0.0).h(28.0).bg(color::FILL2).capsule().action("notify.clear"));
    }
    row(items).gap(8.0).fill_w().pad_xy(4.0, 0.0)
}

fn panel(app: &App) -> Node {
    let s = app.state::<Notify>();
    // The review view of a long command needs the whole body (no header, no history).
    if crate::features::ai_cards::reviewing(app) {
        return col(crate::features::ai_cards::panel_cards(app)).gap(10.0).fill_w();
    }
    let mut children = vec![header(app)];
    // Pending AI permission requests first.
    let cards = crate::features::ai_cards::panel_cards(app);
    let has_cards = !cards.is_empty();
    children.extend(cards);
    // Claude Code sessions (track G).
    let sessions = crate::features::agents::panel_section(app);
    let has_sessions = !sessions.is_empty();
    children.extend(sessions);
    // Windows notifications mirrored on the island (v0.3, track Q).
    let mirrored = crate::features::notify_mirror::panel_section(app);
    let has_mirrored = !mirrored.is_empty();
    children.extend(mirrored);
    if s.history.is_empty() {
        if !has_cards && !has_sessions && !has_mirrored {
            children.push(
                col([
                    icon(icons::BELL).font_size(26.0).color(color::TEXT3),
                    text(tr("Belum ada notifikasi", "No notifications yet")).semibold(),
                    caption(tr("Kirim dari skrip dengan: mokuctl notify --title \"Halo\"", "Send one from a script: mokuctl notify --title \"Hello\"")),
                ])
                .gap(6.0)
                .align(Align::Center)
                .fill_w()
                .pad_xy(0.0, 28.0),
            );
        }
        return col(children).gap(10.0).fill_w();
    }
    let mut rows_shown = if has_cards { 2 } else if has_sessions { 3 } else { ROWS };
    if has_mirrored {
        rows_shown = rows_shown.saturating_sub(2).max(1);
    }
    let start = s.scroll.min(s.history.len().saturating_sub(1));
    let rows: Vec<Node> = s.history.iter().skip(start).take(rows_shown).map(|it| row_node(app, it)).collect();
    let rest = s.history.len().saturating_sub(start + rows.len());
    let mut list = col(rows).gap(8.0).fill_w().action("notify.list");
    if rest > 0 || start > 0 {
        let more = match (start, rest) {
            (0, r) => tr_s(format!("{r} lagi · gulir untuk melihat"), format!("{r} more · scroll to see")),
            (_, 0) => tr_s(format!("{start} lebih baru di atas"), format!("{start} newer above")),
            (_, r) => tr_s(format!("{start} di atas · {r} di bawah"), format!("{start} above · {r} below")),
        };
        list = list.child(caption(more).pad_xy(6.0, 0.0));
    }
    children.push(list);
    col(children).gap(10.0).fill_w()
}
