//! Feature `ai_cards` — Task 6 (track E).
//!
//! AI permission cards for `mokuctl ask` (the Claude Code hook `app/tools/claude-hook.ps1`, and
//! `mokuctl agent-hook` for Codex CLI):
//! * a card per the mockup — title, detail ("Jalankan perintah · toko-online"), the command in
//!   a monospace box, and the buttons Tolak / Setujui (or the client's own options, the last
//!   one approving);
//! * **the user can only approve a command they could see in full.** The command is
//!   hard-wrapped (`queue::wrap_command`) — nothing is collapsed, elided or left to the text
//!   engine's ellipsis. Up to 3 card lines: approve from the card. Longer: the card shows the
//!   start and "Setujui…" opens the review view in the panel (Notifikasi tab): the whole command,
//!   11 lines at a time, scrolled with the wheel or ▲ ▼, and its own Tolak / Setujui — Setujui
//!   works only once the view was scrolled to the end. Longer than 80 review lines: answered
//!   `timeout` at once (the hook falls back to Claude Code's own prompt), never shown;
//! * priority 100 (`AI_CARD`), persistent until answered; it pops even in focus mode;
//! * the answer goes back on the stored `Reply`: `approve` / `deny` (or the option label),
//!   `timeout` after `timeout_secs` (default 120, enforced here too);
//! * the island hidden: **paused from the tray** → the ask is refused with an error right away
//!   (pending ones too, when the user pauses), so the hook exits into Claude Code's normal
//!   prompt instead of waiting for a card nobody can see. **Hidden by a full-screen app**
//!   (`island.hide_fullscreen`) → the island is forced visible while asks wait
//!   (`App::force_visible`): the user is actively waiting on Claude Code;
//! * several asks queue in order (the card shows "1 dari 3"); a client that disconnects has
//!   its ask removed (the pipe server flags its `Reply`), swept every second while asks wait;
//! * while asks wait and the panel is open, the panel shows them on the Notifikasi tab and
//!   stays open (`hold_panel`) until they are answered.
//!
//! * **always allow** (v0.3, track Q; [`always`]; off until the user turns it on): a card of a shell
//!   command from a known agent offers "Selalu izinkan perintah ini": the command (`npm test`,
//!   shown on the card) becomes a rule in `agent-rules.json`, listed and removable in Settings. A
//!   later ask for **exactly that command** (no other argument), from the same agent, is answered
//!   `approve` at once (a short pill says so). Nothing else is ever auto-approved, Moku never
//!   writes to an agent's own settings, and a rule that was added to the file without Moku (an
//!   agent edited it) is ignored until the user confirms it in Settings, with a pill to say so.
//!
//! Actions: `ai.answer:<ask id>:<button index>`, `ai.approve` / `ai.deny` (the card on
//! screen; for `mokuctl debug click`), `ai.always:<ask id>` (approve and remember the rule),
//! `ai.more` / `ai.less` (review page down / up, also the wheel on `ai.review`), `ai.back`
//! (leave the review view).

mod always;
mod queue;

use std::time::{Duration, Instant};

use moku_core::agent_rules::{self, Refusal};
use moku_core::ipc::{Request, Response};

use crate::activity::{Activity, HEADS_UP_SECS, priority};
use crate::app::{App, Redraw, TimerId};
use crate::i18n::{tr, tr_s};
use crate::ipc_server::Reply;
use crate::registry::*;
use crate::ui::*;

use queue::{Ask, AskQueue, Click, Fit, NewAsk};

const CARD: &str = "ai.card";
/// Card width in the pill.
const PILL_W: f32 = 380.0;
const PAUSED: &str = "Moku is paused (tray → Lanjutkan); answer in the terminal";

/// The review view of one long command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReviewView {
    id: u64,
    /// First line shown.
    offset: usize,
    /// Lines seen so far (max of `offset + REVIEW_ROWS`).
    seen_to: usize,
}

impl ReviewView {
    fn new(id: u64) -> ReviewView {
        ReviewView { id, offset: 0, seen_to: queue::REVIEW_ROWS }
    }

    /// Move by `delta` lines within `total`, remembering how far the user got.
    fn scroll(&mut self, delta: isize, total: usize) {
        let max = total.saturating_sub(queue::REVIEW_ROWS);
        self.offset = (self.offset as isize + delta).clamp(0, max as isize) as usize;
        self.seen_to = self.seen_to.max(self.offset + queue::REVIEW_ROWS);
    }

    fn seen_all(&self, total: usize) -> bool {
        self.seen_to >= total
    }
}

#[derive(Default)]
struct AiCards {
    queue: AskQueue,
    /// 1 s sweep (timeouts, disconnected clients) while asks wait.
    sweep: Option<TimerId>,
    /// We hold the panel open (one `hold_panel(true)` outstanding).
    holding: bool,
    /// We keep the island visible over full-screen apps (one `force_visible(true)` outstanding).
    forcing: bool,
    review: Option<ReviewView>,
    /// The "always allow" rules as the file has them (re-read only when it changed).
    rules: always::Cache,
    /// 30 s look at the rules file for edits made outside Moku (only while "always allow" is on).
    watch: Option<TimerId>,
}

pub fn register(r: &mut Registry) {
    r.add_state(AiCards::default());
    r.add_ipc_handler(IpcKind::Ask, on_ask);
    r.on_action("ai", on_action);
    r.on_event(on_event);
    r.on_config(on_config);
    r.on_exit(|app| {
        app.state_mut::<AiCards>().queue.answer_all(queue::TIMEOUT);
    });
}

fn on_ask(app: &mut App, req: Request, reply: Reply) {
    let Request::Ask { title, detail, command, options, timeout_secs, kind, agent } = req else { return };
    if !app.config().ipc.ai_cards {
        return reply.error("AI permission cards are turned off in Moku settings");
    }
    if app.is_paused() {
        log_info!("ai: ask refused (island paused)");
        return reply.error(PAUSED);
    }
    if queue::fit(command.as_deref()) == Fit::TooLong {
        // Never offered: the user could not review it here. "timeout" = the hook falls back to
        // Claude Code's own prompt.
        log_info!("ai: ask with a command too long to review; answered timeout");
        reply.send(Response::Answer { choice: queue::TIMEOUT.into() });
        app.alert("ai.too_long", priority::HEADS_UP, HEADS_UP_SECS, |_| {
            pill([text("✦").color(color::ORANGE), text(tr("Perintah terlalu panjang — jawab di terminal", "Command too long — answer in the terminal")).semibold()])
        });
        return;
    }
    // "Always allow" rules: a listed command is answered at once; otherwise the card may offer one.
    let rules_on = app.config().agents.always_allow;
    if rules_on {
        check_rules_file(app);
    }
    if options.is_empty() {
        let rules = app.state_mut::<AiCards>().rules.current(&agent_rules::path());
        if let Some(prefix) = always::auto_allow(rules_on, kind.as_deref(), agent.as_deref(), command.as_deref(), rules) {
            // Not the command and not the rule's words in the log: only that a rule acted.
            log_info!("ai: ask allowed by a rule");
            reply.send(Response::Answer { choice: queue::APPROVE.into() });
            app.alert("ai.auto", priority::HEADS_UP, 3.5, move |_| {
                pill([icon(icons::CHECK).color(color::GREEN), text(tr("Diizinkan otomatis", "Allowed automatically")).semibold(), caption(prefix.clone()).max_w(220.0)])
            });
            return;
        }
    }
    let always = if options.is_empty() { always::offer(rules_on, kind.as_deref(), agent.as_deref(), command.as_deref()) } else { None };
    let title = if title.trim().is_empty() { tr("Izin diminta", "Permission requested").to_string() } else { title };
    let n = NewAsk { title, detail, command, options, timeout_secs, agent, always };
    if app.state_mut::<AiCards>().queue.push(n, reply, Instant::now()).is_none() {
        return;
    }
    log_info!("ai: ask queued ({} pending)", app.state::<AiCards>().queue.len());
    if app.state::<AiCards>().sweep.is_none() {
        let t = app.every(Duration::from_secs(1), sweep);
        app.state_mut::<AiCards>().sweep = Some(t);
    }
    if !std::mem::replace(&mut app.state_mut::<AiCards>().forcing, true) {
        app.force_visible(true);
    }
    app.set_activity(Activity::persistent(CARD, priority::AI_CARD, pill_card).tab("notify").bubble(|_| bubble()));
    if app.is_panel_open() {
        // The pill is not visible while the panel is: show the card in the panel instead.
        app.open_panel(Some("notify"));
        hold(app);
    }
    app.request_redraw(Redraw::All);
}

/// The rules file has rules Moku did not write (an agent or a program edited it): tell the user once
/// per change. Those rules do not act until the user confirms them in Settings.
fn check_rules_file(app: &mut App) {
    if app.state_mut::<AiCards>().rules.news(&agent_rules::path()) {
        log_info!("ai: the rules file was changed outside Moku; new rules wait for confirmation");
        app.alert("ai.rules_pending", priority::WARNING, 9.0, |_| {
            pill([
                icon(icons::WARNING).color(color::ORANGE),
                text(tr("Aturan izin diubah di luar Moku", "Allow rules changed outside Moku")).semibold(),
                caption(tr("Diabaikan sampai kamu konfirmasi", "Ignored until you confirm")).max_w(200.0),
                button(tr("Tinjau", "Review"), "island.settings"),
            ])
        });
    }
}

/// Timeouts and vanished clients.
fn sweep(app: &mut App) {
    if app.state_mut::<AiCards>().queue.sweep(Instant::now()) > 0 {
        changed(app);
    }
}

/// After the queue changed: update or remove the card, stop the sweep, release the panel.
fn changed(app: &mut App) {
    let s = app.state_mut::<AiCards>();
    if s.review.is_some_and(|r| s.queue.get(r.id).is_none()) {
        s.review = None;
    }
    if app.state::<AiCards>().queue.is_empty() {
        if let Some(t) = app.state_mut::<AiCards>().sweep.take() {
            app.cancel_timer(t);
        }
        app.clear_activity(CARD);
        release(app);
        if std::mem::take(&mut app.state_mut::<AiCards>().forcing) {
            app.force_visible(false);
        }
    }
    app.request_redraw(Redraw::All);
}

fn hold(app: &mut App) {
    let s = app.state::<AiCards>();
    if !s.holding && !s.queue.is_empty() && app.is_panel_open() {
        app.state_mut::<AiCards>().holding = true;
        app.hold_panel(true);
    }
}

fn release(app: &mut App) {
    if std::mem::take(&mut app.state_mut::<AiCards>().holding) {
        app.hold_panel(false);
    }
}

fn on_event(app: &mut App, ev: &AppEvent) {
    match ev {
        AppEvent::PanelOpened => hold(app),
        // `close_panel` resets the hold count itself.
        AppEvent::PanelClosed => app.state_mut::<AiCards>().holding = false,
        // Nobody can see the cards now: let the clients fall back to their own prompt.
        AppEvent::Paused(true) if !app.state::<AiCards>().queue.is_empty() => {
            app.state_mut::<AiCards>().queue.fail_all(PAUSED);
            changed(app);
        }
        // "Hapus semua data": the settings app deletes agent-rules.json next.
        AppEvent::DataCleared => app.state_mut::<AiCards>().rules.clear(),
        _ => {}
    }
}

fn on_config(app: &mut App) {
    // "Always allow" is opt-in: watch the rules file only while it is on.
    let on = app.config().agents.always_allow;
    let armed = app.state::<AiCards>().watch.is_some();
    if on && !armed {
        check_rules_file(app);
        let t = app.every(Duration::from_secs(30), check_rules_file);
        app.state_mut::<AiCards>().watch = Some(t);
    } else if !on && armed {
        if let Some(t) = app.state_mut::<AiCards>().watch.take() {
            app.cancel_timer(t);
        }
    }
    if !app.config().ipc.ai_cards && !app.state::<AiCards>().queue.is_empty() {
        app.state_mut::<AiCards>().queue.answer_all(queue::TIMEOUT);
        changed(app);
    }
}

/// Lines of ask `id`'s command in the review view (0 = no such ask / no command).
fn review_lines(app: &App, id: u64) -> Vec<String> {
    app.state::<AiCards>().queue.get(id).and_then(|a| a.command.as_deref()).map(|c| queue::wrap_command(c, queue::REVIEW_COLS)).unwrap_or_default()
}

fn open_review(app: &mut App, id: u64) {
    let s = app.state_mut::<AiCards>();
    if s.review.is_none_or(|r| r.id != id) {
        s.review = Some(ReviewView::new(id));
    }
    app.open_panel(Some("notify"));
    hold(app);
    app.request_redraw(Redraw::All);
}

fn scroll_review(app: &mut App, delta: isize) {
    let Some(mut r) = app.state::<AiCards>().review else { return };
    let total = review_lines(app, r.id).len();
    r.scroll(delta, total);
    app.state_mut::<AiCards>().review = Some(r);
    app.request_redraw(Redraw::Tab("notify"));
}

fn on_action(app: &mut App, ev: &ActionEvent) {
    let page = queue::REVIEW_ROWS as isize - 2;
    match (ev.name(), ev.kind) {
        ("more" | "less" | "review", ActionKind::Scroll(n)) => return scroll_review(app, -(n * 3.0).round() as isize),
        ("more", ActionKind::Click) => return scroll_review(app, page),
        ("less", ActionKind::Click) => return scroll_review(app, -page),
        ("back", ActionKind::Click) => {
            app.state_mut::<AiCards>().review = None;
            return app.request_redraw(Redraw::All);
        }
        (_, ActionKind::Click) => {}
        _ => return,
    }
    let target = match ev.name() {
        // The same as Setujui (with the same review rule), and the rule is saved.
        "always" => ev.arg().and_then(|a| a.parse::<u64>().ok()).and_then(|id| app.state::<AiCards>().queue.get(id).filter(|a| a.always.is_some()).map(|a| (id, a.approve_index()))),
        "answer" => ev.arg().and_then(|a| a.split_once(':')).and_then(|(id, i)| Some((id.parse::<u64>().ok()?, i.parse::<usize>().ok()?))),
        "approve" => app.state::<AiCards>().queue.front().map(|a| (a.id, a.approve_index())),
        "deny" => app.state::<AiCards>().queue.front().map(|a| (a.id, 0)),
        _ => None,
    };
    let Some((id, index)) = target else { return };
    let Some(ask) = app.state::<AiCards>().queue.get(id) else { return };
    let fit = queue::fit(ask.command.as_deref());
    let total = review_lines(app, id).len();
    let reviewed = app.state::<AiCards>().review.is_some_and(|r| r.id == id && r.seen_all(total));
    if queue::click(fit, index, reviewed) == Click::Review {
        return open_review(app, id);
    }
    // Taken before the answer removes the ask.
    let remember = if ev.name() == "always" { ask.agent.clone().zip(ask.always.clone()) } else { None };
    if let Some(choice) = app.state_mut::<AiCards>().queue.answer(id, index) {
        log_info!("ai: ask answered ({})", if choice == queue::APPROVE { "approve" } else if choice == queue::DENY { "deny" } else { "option" });
        if let Some((agent, prefix)) = remember {
            remember_rule(agent, prefix);
        }
        changed(app);
    }
}

/// Save the rule on a short-lived thread (a file write never runs on the UI thread). The next ask
/// finds it: the cache re-reads the file when it changed. The words are not logged.
fn remember_rule(agent: String, prefix: String) {
    let _ = std::thread::Builder::new().name("agent-rule".into()).spawn(move || match agent_rules::update_file(&agent_rules::path(), |r| r.add(&agent, &prefix)) {
        Ok((_, Ok(_))) => log_info!("ai: rule saved"),
        Ok((_, Err(Refusal::Duplicate))) => {}
        Ok((_, Err(e))) => log_warn!("ai: rule not saved ({e:?})"),
        Err(e) => log_warn!("ai: cannot save the rule: {e}"),
    });
}

// ---- look -----------------------------------------------------------------------------------

fn bubble() -> Node {
    stack([text("✦").font_size(14.0).color(color::ORANGE)])
}

/// The card as the pill (priority 100).
fn pill_card(app: &App) -> Node {
    let s = app.state::<AiCards>();
    match s.queue.front() {
        Some(a) => card(a, s.queue.len(), PILL_W).pad(12.0),
        None => empty(),
    }
}

/// True while the Notifikasi tab shows the review view (it then shows nothing else).
pub fn reviewing(app: &App) -> bool {
    let s = app.state::<AiCards>();
    s.review.is_some_and(|r| s.queue.get(r.id).is_some())
}

/// Cards for the Notifikasi tab (empty when nothing waits): the review view of a long command,
/// or the first ask in full plus one line for the rest of the queue.
pub fn panel_cards(app: &App) -> Vec<Node> {
    let s = app.state::<AiCards>();
    if let Some(r) = s.review
        && let Some(a) = s.queue.get(r.id)
    {
        return vec![review_card(a, r, s.queue.len())];
    }
    let Some(a) = s.queue.front() else { return vec![] };
    let mut out = vec![card(a, s.queue.len(), 0.0).pad(12.0).bg(color::FILL).radius(typo::RADIUS_CARD).fill_w()];
    if s.queue.len() > 1 {
        let n = s.queue.len() - 1;
        out.push(caption(tr_s(format!("{n} permintaan izin lain menunggu"), format!("{n} more permission request{} waiting", if n == 1 { "" } else { "s" }))).pad_xy(6.0, 0.0));
    }
    out
}

fn button_grow(label_text: &str, primary: bool, action: Option<String>) -> Node {
    let t = text(label_text.to_string()).semibold().text_center();
    let (t, bg) = if primary { (t.color(color::INK), color::WHITE) } else { (t, color::FILL2) };
    let b = row([t]).justify(Justify::Center).h(32.0).grow(1.0).bg(bg).capsule();
    match action {
        Some(a) => b.action(a),
        // Not available yet (review not scrolled to the end): visibly inert.
        None => b.opacity(0.35),
    }
}

fn head(a: &Ask, pending: usize, extra: Option<String>) -> Node {
    let tile = stack([text("✦").font_size(15.0).bold()]).size(30.0, 30.0).bg(color::ORANGE).radius(9.0);
    let mut texts = col([text(a.title.clone()).semibold()]).gap(1.0).grow(1.0);
    if let Some(d) = &a.detail {
        texts = texts.child(text(d.clone()).font_size(12.0).secondary());
    }
    let mut head = vec![tile, texts];
    if let Some(e) = extra {
        head.push(caption(e));
    } else if pending > 1 {
        head.push(caption(tr_s(format!("1 dari {pending}"), format!("1 of {pending}"))));
    }
    row(head).gap(10.0).fill_w()
}

fn mono_lines(lines: &[String]) -> Node {
    col(lines.iter().map(|l| text(l.clone()).mono().font_size(12.0))).gap(1.0).grow(1.0)
}

/// Answer buttons; `approve_ok` false = approving buttons are inert (review not finished).
fn buttons(a: &Ask, approve_label: Option<&str>, approve_ok: bool) -> Vec<Node> {
    let act = |i: usize, ok: bool| ok.then(|| format!("ai.answer:{}:{i}", a.id));
    if a.options.is_empty() {
        vec![button_grow(tr("Tolak", "Deny"), false, act(0, true)), button_grow(approve_label.unwrap_or(tr("Setujui", "Approve")), true, act(1, approve_ok))]
    } else {
        let last = a.options.len() - 1;
        a.options.iter().enumerate().map(|(i, o)| button_grow(o, i == last, act(i, i == 0 || approve_ok))).collect()
    }
}

/// "Selalu izinkan perintah ini": the rule is the exact command (the card shows it in full).
fn always_row(a: &Ask) -> Node {
    let button = row([icon(icons::SHIELD).font_size(12.0), text(tr("Selalu izinkan perintah ini", "Always allow this command")).font_size(12.0).semibold()])
        .gap(6.0)
        .pad_xy(12.0, 0.0)
        .h(28.0)
        .bg(color::FILL2)
        .capsule()
        .action(format!("ai.always:{}", a.id));
    // The rule is the whole command the card shows: say so, and that it is no other.
    let note = tr("Berlaku hanya untuk perintah yang sama persis dari agen ini. Daftarnya ada di Pengaturan.", "Covers only this exact command from this agent. The list is in Settings.");
    col([row([button, spacer()]).fill_w(), caption(note).lines(2).fill_w()]).gap(4.0).fill_w()
}

/// Card body per the mockup. `width` 0 = fill the parent.
fn card(a: &Ask, pending: usize, width: f32) -> Node {
    let mut children = vec![head(a, pending, None)];
    let fit = queue::fit(a.command.as_deref());
    if let Some(cmd) = &a.command {
        let lines = queue::wrap_command(cmd, queue::CARD_COLS);
        let shown = if fit == Fit::Card { &lines[..] } else { &lines[..queue::CARD_LINES - 1] };
        let mut boxed = vec![mono_lines(shown)];
        if fit != Fit::Card {
            let rest = queue::wrap_command(cmd, queue::REVIEW_COLS).len();
            boxed.push(caption(tr_s(format!("{rest} baris — Setujui… menampilkan semuanya"), format!("{rest} lines — Approve… shows all of it"))));
        }
        children.push(col(boxed).gap(4.0).fill_w().pad_xy(10.0, 7.0).bg(color::FILL).radius(10.0));
    }
    // Approve from the card: directly when the command fits, else it opens the review view.
    let label = (fit != Fit::Card).then(|| tr("Setujui…", "Approve…"));
    children.push(row(buttons(a, label, true)).gap(8.0).fill_w());
    // A command that fits the card is fully seen here, so the rule can be made from the card; a
    // long one only from the review view, after it was scrolled to the end.
    if let (Some(_), Fit::Card) = (&a.always, fit) {
        children.push(always_row(a));
    }
    let c = col(children).gap(10.0);
    if width > 0.0 { c.w(width) } else { c.fill_w() }
}

/// The review view: the whole command, scrolled; approve only after the last line was shown.
fn review_card(a: &Ask, r: ReviewView, pending: usize) -> Node {
    let lines = a.command.as_deref().map(|c| queue::wrap_command(c, queue::REVIEW_COLS)).unwrap_or_default();
    let total = lines.len();
    let end = (r.offset + queue::REVIEW_ROWS).min(total);
    let pos = tr_s(format!("baris {}–{end} dari {total}", r.offset + 1), format!("lines {}–{end} of {total}", r.offset + 1));
    let seen = r.seen_all(total);
    let arrows = col([
        icon_button(icons::CHEVRON_UP, "ai.less").opacity(if r.offset > 0 { 1.0 } else { 0.35 }),
        spacer(),
        icon_button(icons::CHEVRON_DOWN, "ai.more").opacity(if end < total { 1.0 } else { 0.35 }),
    ])
    .fill_h();
    let body = row([mono_lines(&lines[r.offset.min(total)..end]), arrows]).gap(6.0).align(Align::Stretch).fill_w().pad4(10.0, 7.0, 4.0, 7.0).bg(color::FILL).radius(10.0).action("ai.review");
    let hint = if seen {
        tr("Seluruh perintah sudah ditampilkan.", "You have seen the whole command.")
    } else {
        tr("Gulir sampai baris terakhir untuk bisa menyetujui.", "Scroll to the last line to be able to approve.")
    };
    let mut btns = vec![button_grow(tr("Kembali", "Back"), false, Some("ai.back".into()))];
    btns.extend(buttons(a, None, seen));
    let mut kids = vec![head(a, pending, Some(pos)), body, caption(hint).pad_xy(4.0, 0.0), row(btns).gap(8.0).fill_w()];
    if let (Some(_), true) = (&a.always, seen) {
        kids.push(always_row(a));
    }
    col(kids).gap(10.0).pad(12.0).bg(color::FILL).radius(typo::RADIUS_CARD).fill_w()
}

#[cfg(test)]
mod tests {
    use super::ReviewView;
    use super::queue::REVIEW_ROWS;

    #[test]
    fn review_must_reach_the_last_line() {
        let total = 40;
        let mut r = ReviewView::new(7);
        assert!(!r.seen_all(total));
        r.scroll(10, total);
        assert_eq!(r.offset, 10);
        assert!(!r.seen_all(total));
        // Scrolling back up does not forget how far the user read.
        r.scroll(-100, total);
        assert_eq!(r.offset, 0);
        r.scroll(1000, total);
        assert_eq!(r.offset, total - REVIEW_ROWS);
        assert!(r.seen_all(total));
        r.scroll(-5, total);
        assert!(r.seen_all(total));
        // A command that fits one page is seen at once.
        assert!(ReviewView::new(1).seen_all(REVIEW_ROWS));
    }
}
