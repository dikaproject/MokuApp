//! Feature `agents` — v0.2 track G (Claude Code live status), generalised in v0.3 track Q to the
//! coding-agent hub: Claude Code, OpenAI Codex CLI and Gemini CLI.
//!
//! IPC `session` (from `mokuctl claude-hook` / `mokuctl agent-hook`, or `mokuctl session`) drives
//! one pill per session, keyed by the agent's session id (scoped by agent: sessions of different
//! agents never mix) and labelled with the project folder's name; a session of an agent other
//! than Claude Code also carries the agent's name as a small tag. `[claude] enabled`,
//! `[agents] codex` and `[agents] gemini` switch each agent's sessions on and off.
//! * **working** ("✦ toko-online  Bekerja… 2m 14s", priority `PROGRESS`): from
//!   `UserPromptSubmit` to `Stop`. The elapsed time is redrawn once a second, on the second, and
//!   only while that pill is the one on screen (pill mode, island visible); nothing ticks while
//!   the panel is open, the island is hidden, or no session works;
//! * **needs input** ("Butuh input", priority `DUE` 90): a Claude Code `Notification` that waits
//!   for the user (permission prompt, question); back to working on the next tool call;
//! * **done** ("✓ toko-online  Selesai · 3m 12s", transient 6 s, priority `NOTIFICATION`) and
//!   **failed** ("Berhenti", `StopFailure`), not shown while focus mode holds notifications;
//! * a pending AI permission card (priority `AI_CARD` 100) always wins over these pills;
//! * several sessions: each has its own activity, so the second one shows as the split bubble;
//!   the pill says "+N" when more sessions are busy; the Notifikasi tab lists them all
//!   ([`panel_section`]), each with a forget button;
//! * a session with no event for `[claude] stale_minutes` (default 30) is dropped (one timer,
//!   armed for the next deadline only); `SessionEnd` drops it at once.
//!
//! The state machine is pure and tested in `model.rs`. Off (`[claude] enabled = false`): the
//! request is refused, the list is emptied and no timer runs.
//!
//! Actions: `agents.forget:<n>` (list row ×), `agents.open` (pill → Notifikasi).
//!
//! The permission cards (`mokuctl ask`, "always allow" rules) are the `ai_cards` feature.

mod model;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use moku_core::ipc::Request;

use crate::activity::{Activity, HEADS_UP_SECS, priority};
use crate::app::{App, Redraw, TimerId};
use crate::i18n::{self, tr, tr_s};
use crate::ipc_server::Reply;
use crate::registry::*;
use crate::ui::*;

use model::{Agent, Event, Outcome, Phase, Session, Sessions};

/// Activity id prefixes.
const PILL: &str = "agents.s:";
const DONE: &str = "agents.done:";
/// Rows shown in the Notifikasi tab.
const ROWS: usize = 3;

#[derive(Default)]
struct Hub {
    sessions: Sessions,
    /// The 1 Hz redraw chain (only while a working pill can be on screen).
    tick: Option<TimerId>,
    /// Fires when the next session goes stale.
    stale: Option<TimerId>,
}

pub fn register(r: &mut Registry) {
    r.add_state(Hub::default());
    r.add_ipc_handler(IpcKind::Session, on_session);
    r.on_action("agents", on_action);
    r.on_event(on_event);
    r.on_config(on_config);
}

fn wall_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn stale_after(app: &App) -> Duration {
    Duration::from_secs(app.config().claude.stale_minutes as u64 * 60)
}

/// Is the status of `agent` switched on?
fn agent_enabled(app: &App, agent: Agent) -> bool {
    let c = app.config();
    match agent {
        Agent::Claude => c.claude.enabled,
        Agent::Codex => c.agents.codex,
        Agent::Gemini => c.agents.gemini,
    }
}

fn on_session(app: &mut App, req: Request, reply: Reply) {
    let Request::Session { id, project, state, detail, ts, agent } = req else { return };
    let Some(agent) = Agent::from_wire(agent.as_deref()) else {
        return reply.error("unknown agent (claude, codex or gemini)");
    };
    if !agent_enabled(app, agent) {
        return reply.error(format!("{} status is turned off in Moku settings", agent.name()));
    }
    // The key the model stores it under (trimmed, cut to its maximum length, scoped by agent).
    let key = model::scoped(agent, &id);
    if key.is_empty() {
        return reply.error("session id is empty");
    }
    reply.ok();
    let ev = Event { agent, id, project, state, detail, ts };
    let outcome = app.state_mut::<Hub>().sessions.apply(ev, Instant::now(), wall_ms());
    if outcome == Outcome::Ignored {
        return;
    }
    log_info!("agents: {} session {} ({} known, {} busy)", agent.slug(), state.as_str(), app.state::<Hub>().sessions.len(), app.state::<Hub>().sessions.busy());
    match outcome {
        Outcome::Removed => forget_activities(app, &key),
        Outcome::Finished { took, failed } => {
            app.clear_activity(&format!("{PILL}{key}"));
            finished_pill(app, &key, took, failed);
        }
        Outcome::Changed => sync_pill(app, &key),
        Outcome::Ignored => {}
    }
    after_change(app);
}

/// Bookkeeping after any change: other sessions' "+N", stale timer, tick chain, list.
fn after_change(app: &mut App) {
    arm_stale(app);
    ensure_tick(app);
    app.request_redraw(Redraw::All);
}

fn forget_activities(app: &mut App, id: &str) {
    app.clear_activity(&format!("{PILL}{id}"));
    app.clear_activity(&format!("{DONE}{id}"));
}

/// Show / update / remove the persistent pill of session `id` to match its phase.
fn sync_pill(app: &mut App, id: &str) {
    let act = format!("{PILL}{id}");
    let Some(phase) = app.state::<Hub>().sessions.get(id).map(|s| s.phase) else {
        return forget_activities(app, id);
    };
    let prio = match phase {
        Phase::Working => priority::PROGRESS,
        Phase::NeedsInput => priority::DUE,
        Phase::Idle | Phase::Done | Phase::Failed => return app.clear_activity(&act),
    };
    // A new turn replaces a "Selesai" still on screen.
    app.clear_activity(&format!("{DONE}{id}"));
    if phase == Phase::NeedsInput {
        // Waiting for the user is news (re-added, not updated in place): it pops over older
        // pills of the same priority, and a click on the pill opens the list.
        app.clear_activity(&act);
    }
    let (pid, bid) = (id.to_string(), id.to_string());
    app.set_activity(Activity::persistent(act, prio, move |app| session_pill(app, &pid)).tab("notify").bubble(move |app| session_bubble(app, &bid)));
}

fn finished_pill(app: &mut App, id: &str, took: Option<Duration>, failed: bool) {
    if !app.config().claude.done_pill || app.focus_hold() {
        return;
    }
    let Some(s) = app.state::<Hub>().sessions.get(id) else { return };
    // Captured now: the session may end (SessionEnd) while the pill is still up.
    let project = project_of(s);
    let tag = agent_tag(s.agent);
    let what = if failed { tr("Berhenti", "Stopped") } else { tr("Selesai", "Done") };
    let mut status = what.to_string();
    if let Some(t) = took {
        status = format!("{status} · {}", model::elapsed(t, i18n::is_english()));
    }
    let err = if failed { s.detail.as_deref().map(|d| localize_failure(d, i18n::is_english())) } else { None };
    app.set_activity(
        Activity::transient(format!("{DONE}{id}"), priority::NOTIFICATION, HEADS_UP_SECS, move |_| {
            let (glyph, tint) = if failed { (icons::WARNING, color::RED) } else { (icons::CHECK, color::GREEN) };
            let badge = stack([icon(glyph).font_size(11.0).color(color::INK)]).size(20.0, 20.0).bg(tint).capsule();
            let mut kids = vec![badge, text(project.clone()).semibold().max_w(220.0)];
            kids.extend(tag.clone());
            kids.push(text(status.clone()).secondary());
            if let Some(e) = &err {
                kids.push(caption(e.clone()).max_w(160.0));
            }
            pill(kids)
        })
        .tab("notify"),
    );
}

// ---- timers -----------------------------------------------------------------------------------

/// One timer for the next stale deadline (none while there are no sessions).
fn arm_stale(app: &mut App) {
    if let Some(t) = app.state_mut::<Hub>().stale.take() {
        app.cancel_timer(t);
    }
    let stale = stale_after(app);
    let Some(at) = app.state::<Hub>().sessions.next_stale(stale) else { return };
    let d = at.saturating_duration_since(Instant::now()) + Duration::from_millis(50);
    let t = app.after(d, |app| {
        app.state_mut::<Hub>().stale = None;
        let stale = stale_after(app);
        let gone = app.state_mut::<Hub>().sessions.sweep(Instant::now(), stale);
        if !gone.is_empty() {
            log_info!("agents: {} stale session(s) dropped", gone.len());
        }
        for id in &gone {
            forget_activities(app, id);
        }
        after_change(app);
    });
    app.state_mut::<Hub>().stale = Some(t);
}

/// Session id of the pill on screen, when it is a working Claude pill.
fn visible_working(app: &App) -> Option<&Session> {
    if app.is_panel_open() || app.is_hidden() {
        return None;
    }
    let id = app.top_activity()?.strip_prefix(PILL)?;
    app.state::<Hub>().sessions.get(id).filter(|s| s.phase == Phase::Working)
}

/// Run the 1 Hz chain only while a working session exists and the pill can be seen.
fn ensure_tick(app: &mut App) {
    let want = !app.is_panel_open() && !app.is_hidden() && app.state::<Hub>().sessions.iter().any(|s| s.phase == Phase::Working);
    let running = app.state::<Hub>().tick.is_some();
    if want && !running {
        arm_tick(app);
    } else if !want && let Some(t) = app.state_mut::<Hub>().tick.take() {
        app.cancel_timer(t);
    }
}

fn arm_tick(app: &mut App) {
    // Land just after the shown second changes, so the timer never skips or repeats a second.
    let ms = match visible_working(app).and_then(|s| s.turn_start) {
        Some(t) => 1000 - (t.elapsed().as_millis() % 1000) as u64 + 15,
        None => 1000,
    };
    let t = app.after(Duration::from_millis(ms), on_tick);
    app.state_mut::<Hub>().tick = Some(t);
}

fn on_tick(app: &mut App) {
    app.state_mut::<Hub>().tick = None;
    if visible_working(app).is_some() {
        app.request_redraw(Redraw::Pill);
    }
    ensure_tick(app);
}

// ---- events, config, actions ------------------------------------------------------------------

fn on_event(app: &mut App, ev: &AppEvent) {
    match ev {
        AppEvent::PanelOpened | AppEvent::PanelClosed | AppEvent::Visibility(_) => ensure_tick(app),
        AppEvent::DataCleared => clear_all(app),
        _ => {}
    }
}

fn on_config(app: &mut App) {
    // An agent switched off: its sessions go (the others stay).
    let off: Vec<String> = app.state::<Hub>().sessions.iter().filter(|s| !agent_enabled(app, s.agent)).map(|s| s.id.clone()).collect();
    for id in &off {
        app.state_mut::<Hub>().sessions.remove(id);
        forget_activities(app, id);
    }
    if !off.is_empty() {
        after_change(app);
    }
    if !app.state::<Hub>().sessions.is_empty() {
        // `stale_minutes` may have changed.
        arm_stale(app);
    }
}

fn clear_all(app: &mut App) {
    let ids: Vec<String> = app.state::<Hub>().sessions.iter().map(|s| s.id.clone()).collect();
    for id in &ids {
        forget_activities(app, id);
    }
    app.state_mut::<Hub>().sessions.clear();
    for t in [app.state_mut::<Hub>().tick.take(), app.state_mut::<Hub>().stale.take()].into_iter().flatten() {
        app.cancel_timer(t);
    }
    app.request_redraw(Redraw::All);
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    if ev.kind != ActionKind::Click {
        return;
    }
    match ev.name() {
        "forget" => {
            let n = ev.arg().and_then(|a| a.parse::<u64>().ok());
            let id = app.state::<Hub>().sessions.iter().find(|s| Some(s.order) == n).map(|s| s.id.clone());
            if let Some(id) = id {
                app.state_mut::<Hub>().sessions.remove(&id);
                forget_activities(app, &id);
                after_change(app);
            }
        }
        "open" => app.open_panel(Some("notify")),
        _ => {}
    }
}

// ---- look -------------------------------------------------------------------------------------

/// The name on the pill: the project folder, else the agent's own name.
fn project_of(s: &Session) -> String {
    if s.project.is_empty() { s.agent.name().to_string() } else { s.project.clone() }
}

/// "Codex" / "Gemini CLI" tag after the project name; nothing for Claude Code (as before).
fn agent_tag(a: Agent) -> Option<Node> {
    (a != Agent::Claude).then(|| text(a.name()).font_size(11.0).semibold().color(color::TEXT2).pad_xy(7.0, 2.0).bg(color::FILL2).capsule())
}

fn spark(c: Rgba, size: f32) -> Node {
    text("✦").font_size(size).color(c)
}

fn phase_tint(p: Phase) -> Rgba {
    match p {
        Phase::Working => color::ORANGE,
        Phase::NeedsInput => color::YELLOW,
        Phase::Done => color::GREEN,
        Phase::Failed => color::RED,
        Phase::Idle => color::TEXT3,
    }
}

/// The persistent pill of a working / waiting session.
fn session_pill(app: &App, id: &str) -> Node {
    let st = app.state::<Hub>();
    let Some(s) = st.sessions.get(id) else { return empty() };
    let mut kids = vec![spark(phase_tint(s.phase), 15.0), text(project_of(s)).semibold().max_w(220.0)];
    kids.extend(agent_tag(s.agent));
    match s.phase {
        Phase::NeedsInput => kids.push(text(tr("Butuh input", "Needs input")).color(color::YELLOW).semibold()),
        _ => {
            kids.push(text(tr("Bekerja…", "Working…")).secondary());
            if let Some(t) = s.turn_start {
                let e = model::elapsed(t.elapsed(), i18n::is_english());
                // Fixed width: the pill does not twitch as the digits change.
                let w = match e.len() {
                    0..=3 => 26.0,
                    4..=6 => 46.0,
                    _ => 58.0,
                };
                kids.push(text(e).font_size(12.5).tertiary().w(w));
            }
        }
    }
    let others = st.sessions.busy().saturating_sub(1);
    if others > 0 {
        kids.push(text(format!("+{others}")).font_size(11.0).semibold().color(color::TEXT2).pad_xy(7.0, 2.0).bg(color::FILL2).capsule());
    }
    pill(kids)
}

fn session_bubble(app: &App, id: &str) -> Node {
    let tint = app.state::<Hub>().sessions.get(id).map(|s| phase_tint(s.phase)).unwrap_or(color::ORANGE);
    stack([spark(tint, 15.0)]).size(24.0, 24.0)
}

/// "3m", "1j 5m" (minute resolution: the list redraws every 30 s with its tab).
fn minutes(d: Duration) -> String {
    let m = d.as_secs() / 60;
    if m < 60 { format!("{m}m") } else { format!("{}{} {}m", m / 60, tr("j", "h"), m % 60) }
}

/// What Claude Code says in a `Notification` (`Claude needs your permission to use Bash`) is
/// English whatever the island's language. In Indonesian the two sentences Claude Code sends are
/// translated; any other text (a question, a newer message) is shown quoted, as Claude's own
/// words, instead of passing for part of the interface. In English it is shown as it came.
fn localize_notification(msg: &str, english: bool) -> String {
    if english {
        return msg.to_string();
    }
    // Claude Code's own sentence, and the one `mokuctl agent-hook` sends for Codex.
    if let Some(tool) = msg.strip_prefix("Claude needs your permission to use ").or_else(|| msg.strip_prefix("Needs your permission to use ")) {
        return format!("Butuh izin untuk memakai {}", tool.trim());
    }
    match msg.trim().trim_end_matches('.') {
        "Claude is waiting for your input" => "Menunggu masukanmu".to_string(),
        "Claude needs your attention" => "Butuh perhatianmu".to_string(),
        other => format!("\u{201C}{other}\u{201D}"),
    }
}

/// The error of a failed turn (`StopFailure`'s `error`): Claude Code's known codes in Indonesian,
/// anything else quoted.
fn localize_failure(code: &str, english: bool) -> String {
    if english {
        return code.to_string();
    }
    match code.trim() {
        "rate_limit" => "batas pemakaian tercapai".to_string(),
        "authentication_failed" => "gagal masuk (autentikasi)".to_string(),
        "billing_error" => "masalah tagihan".to_string(),
        "invalid_request" => "permintaan tidak valid".to_string(),
        "server_error" => "kesalahan server".to_string(),
        "max_output_tokens" => "keluaran terlalu panjang".to_string(),
        // Codex CLI `Interrupt` (the user pressed Esc).
        "interrupted" => "dihentikan".to_string(),
        other => format!("\u{201C}{other}\u{201D}"),
    }
}

fn row_status(s: &Session, now: Instant) -> (String, Option<String>) {
    let en = i18n::is_english();
    match s.phase {
        Phase::Idle => (tr("Siap", "Ready").to_string(), None),
        Phase::Working => {
            let mut t = tr("Bekerja…", "Working…").to_string();
            if s.subagents > 0 {
                t = format!("{t} · {}", tr_s(format!("{} subagen selesai", s.subagents), format!("{} subagent{} done", s.subagents, if s.subagents == 1 { "" } else { "s" })));
            }
            (t, s.turn_start.map(|t| minutes(now.saturating_duration_since(t))))
        }
        Phase::NeedsInput => {
            let t = tr("Butuh input", "Needs input").to_string();
            (s.detail.as_ref().map(|d| format!("{t} · {}", localize_notification(d, en))).unwrap_or(t), None)
        }
        Phase::Done | Phase::Failed => {
            let what = if s.phase == Phase::Done { tr("Selesai", "Done") } else { tr("Berhenti", "Stopped") };
            let mut t = what.to_string();
            if let Some(d) = s.took {
                t = format!("{t} · {}", model::elapsed(d, en));
            }
            if let Some(e) = s.detail.as_ref().filter(|_| s.phase == Phase::Failed) {
                t = format!("{t} · {}", localize_failure(e, en));
            }
            (t, s.finished_at.map(|f| model::ago(now.saturating_duration_since(f), en)))
        }
    }
}

fn session_row(s: &Session, now: Instant) -> Node {
    let (status, when) = row_status(s, now);
    let tile = stack([spark(if s.phase == Phase::Idle { color::TEXT2 } else { color::INK }, 13.0)])
        .size(28.0, 28.0)
        .bg(if s.phase == Phase::Idle { color::FILL2 } else { phase_tint(s.phase) })
        .radius(8.0);
    let mut trailing = vec![];
    if let Some(w) = when {
        trailing.push(caption(w));
    }
    trailing.push(
        stack([icon(icons::CLOSE).font_size(10.0).color(color::TEXT3)])
            .size(24.0, 24.0)
            .capsule()
            .action(format!("agents.forget:{}", s.order))
            .name(tr("Lupakan sesi", "Forget session")),
    );
    let mut title = vec![text(project_of(s)).semibold()];
    title.extend(agent_tag(s.agent));
    row([tile, col([row(title).gap(6.0), text(status).font_size(12.0).secondary()]).gap(1.0).grow(1.0), row(trailing).gap(6.0)])
        .gap(10.0)
        .fill_w()
        .pad4(8.0, 6.0, 6.0, 6.0)
}

/// The coding-agent section of the Notifikasi tab (nothing when no session is known): busy
/// sessions first, then the most recent; at most [`ROWS`] rows plus "N lagi".
pub fn panel_section(app: &App) -> Vec<Node> {
    let st = app.state::<Hub>();
    if st.sessions.is_empty() {
        return vec![];
    }
    let now = Instant::now();
    let mut list: Vec<&Session> = st.sessions.iter().collect();
    list.sort_by_key(|s| (!s.phase.is_busy(), std::cmp::Reverse(s.last_event)));
    let title = if list.iter().all(|s| s.agent == Agent::Claude) { "Claude Code" } else { tr("Agen coding", "Coding agents") };
    let mut rows: Vec<Node> = vec![row([label(title), spacer(), caption(tr_s(format!("{} sesi", list.len()), format!("{} session{}", list.len(), if list.len() == 1 { "" } else { "s" })))]).fill_w().pad_xy(8.0, 2.0)];
    rows.extend(list.iter().take(ROWS).map(|s| session_row(s, now)));
    if list.len() > ROWS {
        let n = list.len() - ROWS;
        rows.push(caption(tr_s(format!("{n} sesi lain"), format!("{n} more"))).pad_xy(8.0, 0.0));
    }
    vec![col(rows).gap(2.0).fill_w().pad_xy(0.0, 6.0).bg(color::FILL).radius(16.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_codes_notification_text_is_translated_or_quoted_in_indonesian() {
        let id = |m: &str| localize_notification(m, false);
        assert_eq!(id("Claude needs your permission to use Bash"), "Butuh izin untuk memakai Bash");
        assert_eq!(id("Claude needs your permission to use mcp__fs__read "), "Butuh izin untuk memakai mcp__fs__read");
        assert_eq!(id("Claude is waiting for your input"), "Menunggu masukanmu");
        // The sentence `mokuctl agent-hook` sends for a Codex permission request.
        assert_eq!(id("Needs your permission to use Bash"), "Butuh izin untuk memakai Bash");
        // Not one of Claude Code's own sentences: shown quoted, never as if it were the island's.
        assert_eq!(id("Pilih satu cabang"), "\u{201C}Pilih satu cabang\u{201D}");
        assert!(id("Which branch should I use?").starts_with('\u{201C}'));
        // English UI: as it came.
        assert_eq!(localize_notification("Claude needs your permission to use Bash", true), "Claude needs your permission to use Bash");
    }

    #[test]
    fn failure_codes_are_translated_or_quoted() {
        assert_eq!(localize_failure("rate_limit", false), "batas pemakaian tercapai");
        assert_eq!(localize_failure("something_new", false), "\u{201C}something_new\u{201D}");
        assert_eq!(localize_failure("rate_limit", true), "rate_limit");
        assert_eq!(localize_failure("interrupted", false), "dihentikan");
    }
}
