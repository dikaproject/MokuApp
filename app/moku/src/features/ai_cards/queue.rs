//! Pending AI permission requests (pure logic, no UI): first come, first shown; each holds the
//! `Reply` of the waiting `mokuctl ask`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use moku_core::ipc::Response;

use crate::ipc_server::Reply;

/// More pending asks than this are refused (each one holds a pipe thread).
pub const MAX_PENDING: usize = 16;
pub const DEFAULT_TIMEOUT_SECS: u32 = 120;
pub const MAX_TIMEOUT_SECS: u32 = 3600;

/// Answers for the default button pair.
pub const APPROVE: &str = "approve";
pub const DENY: &str = "deny";
pub const TIMEOUT: &str = "timeout";

pub struct Ask {
    pub id: u64,
    pub title: String,
    pub detail: Option<String>,
    pub command: Option<String>,
    /// Button labels from the client; empty = the default pair (Tolak, Setujui).
    pub options: Vec<String>,
    /// The agent that asks (`claude`, `codex`, `gemini`), as the client said.
    pub agent: Option<String>,
    /// The words of the rule "Selalu izinkan perintah ini" would create for this command
    /// (`npm test`); `None` = no such button (not a plain shell command, feature off ...).
    pub always: Option<String>,
    pub deadline: Instant,
    reply: Option<Reply>,
}

impl Ask {
    /// The answer for button `index`: the label for custom options, else `deny` / `approve`.
    pub fn choice(&self, index: usize) -> Option<String> {
        if self.options.is_empty() {
            return match index {
                0 => Some(DENY.into()),
                1 => Some(APPROVE.into()),
                _ => None,
            };
        }
        self.options.get(index).cloned()
    }

    /// Index of the approving button: the last one.
    pub fn approve_index(&self) -> usize {
        self.options.len().max(2) - 1
    }

    pub fn is_closed(&self) -> bool {
        self.reply.as_ref().is_none_or(|r| r.is_closed())
    }

    fn answer(&mut self, choice: &str) {
        if let Some(r) = self.reply.take() {
            r.send(Response::Answer { choice: choice.to_string() });
        }
    }
}

/// What the client sent.
pub struct NewAsk {
    pub title: String,
    pub detail: Option<String>,
    pub command: Option<String>,
    pub options: Vec<String>,
    pub timeout_secs: Option<u32>,
    pub agent: Option<String>,
    pub always: Option<String>,
}

#[derive(Default)]
pub struct AskQueue {
    items: VecDeque<Ask>,
    next_id: u64,
}

impl AskQueue {
    /// Queue an ask (shown after the ones already pending). Full → the reply is answered with
    /// an error right away and `None` is returned.
    pub fn push(&mut self, n: NewAsk, reply: Reply, now: Instant) -> Option<u64> {
        if self.items.len() >= MAX_PENDING {
            reply.error("too many pending asks");
            return None;
        }
        self.next_id += 1;
        let secs = n.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS).clamp(1, MAX_TIMEOUT_SECS);
        let options: Vec<String> = n.options.into_iter().map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).take(4).collect();
        // The rule is offered for the default Tolak / Setujui pair only: a client with its own
        // buttons decides what they mean.
        let always = n.always.filter(|_| options.is_empty());
        self.items.push_back(Ask {
            id: self.next_id,
            title: n.title,
            detail: n.detail.filter(|d| !d.trim().is_empty()),
            command: n.command.filter(|c| !c.trim().is_empty()),
            options,
            agent: n.agent,
            always,
            deadline: now + Duration::from_secs(secs as u64),
            reply: Some(reply),
        });
        Some(self.next_id)
    }

    /// The card on screen.
    pub fn front(&self) -> Option<&Ask> {
        self.items.front()
    }

    pub fn get(&self, id: u64) -> Option<&Ask> {
        self.items.iter().find(|a| a.id == id)
    }

    /// Answer ask `id` with button `index`. Returns the choice sent, `None` if there is no
    /// such ask / button (a stale click).
    pub fn answer(&mut self, id: u64, index: usize) -> Option<String> {
        let pos = self.items.iter().position(|a| a.id == id)?;
        let choice = self.items[pos].choice(index)?;
        let mut a = self.items.remove(pos)?;
        a.answer(&choice);
        Some(choice)
    }

    /// Answer timed-out asks with `timeout` and drop asks whose client went away. Returns how
    /// many were removed.
    pub fn sweep(&mut self, now: Instant) -> usize {
        let n = self.items.len();
        self.items.retain_mut(|a| {
            if a.is_closed() {
                return false;
            }
            if now >= a.deadline {
                a.answer(TIMEOUT);
                return false;
            }
            true
        });
        n - self.items.len()
    }

    /// Answer every pending ask with `choice` (island quitting, AI cards switched off).
    pub fn answer_all(&mut self, choice: &str) {
        for mut a in self.items.drain(..) {
            a.answer(choice);
        }
    }

    /// Answer every pending ask with an error (island paused): the hook then lets Claude Code
    /// show its own prompt at once instead of waiting for a card nobody can see.
    pub fn fail_all(&mut self, message: &str) {
        for mut a in self.items.drain(..) {
            if let Some(r) = a.reply.take() {
                r.error(message);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Position (1-based) of ask `id` in the queue.
    pub fn position(&self, id: u64) -> Option<usize> {
        self.items.iter().position(|a| a.id == id).map(|p| p + 1)
    }
}

// ---- showing the whole command -------------------------------------------------------------
//
// Rule: the user can only approve a command they could see in full. The command is hard-wrapped
// here into lines of at most N monospace columns (each line is drawn as its own one-line text,
// so nothing is left to the text engine's wrapping or ellipsis). A command that fits the card
// is approved from the card; a longer one is approved only from the review view in the panel,
// after it was scrolled to the end; one too long even for that is never offered for approval.

/// Monospace columns per line on the card (380 DIP card, 12 px Cascadia Mono ≈ 7.2 DIP/col → 50).
pub const CARD_COLS: usize = 46;
/// Lines of command the card shows.
pub const CARD_LINES: usize = 3;
/// Columns per line in the review view (≈ 566 DIP of text → 78 cols).
pub const REVIEW_COLS: usize = 72;
/// Lines visible at once in the review view (scrolled with the wheel or ▲ ▼).
pub const REVIEW_ROWS: usize = 11;
/// Longest command (in review lines) that can be approved in Moku at all; longer ones get
/// `timeout` at once so the hook falls back to Claude Code's own prompt.
pub const MAX_REVIEW_LINES: usize = 80;

/// Where a command can be approved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Fully visible on the card (or no command at all).
    Card,
    /// Needs the review view (Setujui on the card opens it).
    Review,
    /// Too long to review in the island: not offered.
    TooLong,
}

pub fn fit(cmd: Option<&str>) -> Fit {
    let Some(cmd) = cmd else { return Fit::Card };
    if wrap_command(cmd, CARD_COLS).len() <= CARD_LINES {
        Fit::Card
    } else if wrap_command(cmd, REVIEW_COLS).len() <= MAX_REVIEW_LINES {
        Fit::Review
    } else {
        Fit::TooLong
    }
}

/// What a click on answer button `index` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    /// Send the answer.
    Answer,
    /// Open (or stay in) the review view instead: the command was not seen in full yet.
    Review,
}

/// Button 0 (Tolak / the first option) always answers. Every other button approves something,
/// so for a command that does not fit the card it answers only after the review view was
/// scrolled to the end (`reviewed`).
pub fn click(fit: Fit, index: usize, reviewed: bool) -> Click {
    match fit {
        _ if index == 0 => Click::Answer,
        Fit::Card => Click::Answer,
        Fit::Review if reviewed => Click::Answer,
        Fit::Review | Fit::TooLong => Click::Review,
    }
}

/// Columns a character takes in a monospace font (East Asian wide / emoji = 2).
fn cols_of(c: char) -> usize {
    let u = c as u32;
    let wide = matches!(u, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF | 0x20000..=0x3FFFD);
    if wide { 2 } else { 1 }
}

/// Characters that could hide or reorder what is shown (controls, bidi overrides, zero-width)
/// are drawn as a visible `�` instead.
fn visible(c: char) -> char {
    let hidden = c.is_control() || matches!(c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2069 | 0xFEFF);
    if hidden { '\u{FFFD}' } else { c }
}

/// The whole command as lines of at most `cols` monospace columns: one or more lines per line
/// of the command (`\r\n` / `\n`), tabs as 4 spaces, nothing collapsed or left out. Long lines
/// break after a space when one is near the end, else exactly at the limit.
pub fn wrap_command(cmd: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(8);
    let mut out = vec![];
    for raw in cmd.trim_matches(|c: char| c.is_whitespace()).split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let chars: Vec<char> = raw.chars().flat_map(|c| if c == '\t' { vec![' '; 4] } else { vec![visible(c)] }).collect();
        if chars.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut start = 0;
        while start < chars.len() {
            // Take as many characters as fit.
            let (mut end, mut w) = (start, 0);
            while end < chars.len() && w + cols_of(chars[end]) <= cols {
                w += cols_of(chars[end]);
                end += 1;
            }
            if end == start {
                end = start + 1; // a character wider than a line (cannot happen with cols ≥ 8)
            }
            // Prefer breaking after a space in the last quarter of the line.
            if end < chars.len()
                && let Some(sp) = (start + 1..end).rev().take(cols / 4).find(|&i| chars[i - 1] == ' ')
            {
                end = sp;
            }
            out.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(title: &str, timeout: Option<u32>) -> NewAsk {
        NewAsk { title: title.into(), detail: None, command: Some("npm test".into()), options: vec![], timeout_secs: timeout, agent: None, always: None }
    }

    #[test]
    fn ask_queue_order() {
        let t0 = Instant::now();
        let mut q = AskQueue::default();
        let (r1, rx1) = Reply::channel();
        let (r2, rx2) = Reply::channel();
        let (r3, rx3) = Reply::channel();
        let a = q.push(ask("satu", None), r1, t0).unwrap();
        let b = q.push(ask("dua", None), r2, t0).unwrap();
        let c = q.push(ask("tiga", Some(5)), r3, t0).unwrap();
        // First in, first shown.
        assert_eq!(q.front().unwrap().title, "satu");
        assert_eq!((q.position(a), q.position(b), q.position(c)), (Some(1), Some(2), Some(3)));
        // Approve the first → the second is shown next.
        assert_eq!(q.answer(a, 1).as_deref(), Some(APPROVE));
        assert_eq!(rx1.recv().unwrap(), Response::Answer { choice: APPROVE.into() });
        assert_eq!(q.front().unwrap().title, "dua");
        // A stale click on an answered ask, or a button that does not exist, does nothing.
        assert_eq!(q.answer(a, 1), None);
        assert_eq!(q.answer(b, 9), None);
        assert_eq!(q.len(), 2);
        // Answering a later one out of order is allowed (panel buttons carry the id).
        assert_eq!(q.answer(c, 0).as_deref(), Some(DENY));
        assert_eq!(rx3.recv().unwrap(), Response::Answer { choice: DENY.into() });
        assert_eq!(q.front().unwrap().title, "dua");
        // Timeout on the island side.
        assert_eq!(q.sweep(t0 + Duration::from_secs(60)), 0);
        assert_eq!(q.sweep(t0 + Duration::from_secs(121)), 1);
        assert_eq!(rx2.recv().unwrap(), Response::Answer { choice: TIMEOUT.into() });
        assert!(q.is_empty());
    }

    #[test]
    fn ask_disconnected_client_is_dropped_and_queue_is_bounded() {
        let t0 = Instant::now();
        let mut q = AskQueue::default();
        let (r1, _rx1) = Reply::channel();
        let flag = r1.closed_flag();
        let (r2, _rx2) = Reply::channel();
        q.push(ask("pergi", None), r1, t0);
        q.push(ask("tetap", None), r2, t0);
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(q.sweep(t0), 1);
        assert_eq!(q.front().unwrap().title, "tetap");
        // Bounded: the 17th is refused with an error.
        for _ in 1..MAX_PENDING {
            q.push(ask("x", None), Reply::channel().0, t0);
        }
        assert_eq!(q.len(), MAX_PENDING);
        let (r, rx) = Reply::channel();
        assert!(q.push(ask("satu lagi", None), r, t0).is_none());
        assert!(matches!(rx.recv().unwrap(), Response::Error { .. }));
        // Timeouts are clamped: 0 → 1 s, huge → 1 h.
        let mut q = AskQueue::default();
        let a = q.push(ask("nol", Some(0)), Reply::channel().0, t0).unwrap();
        assert_eq!(q.get(a).unwrap().deadline, t0 + Duration::from_secs(1));
        let b = q.push(ask("lama", Some(u32::MAX)), Reply::channel().0, t0).unwrap();
        assert_eq!(q.get(b).unwrap().deadline, t0 + Duration::from_secs(3600));
        q.answer_all(DENY);
        assert!(q.is_empty());
    }

    #[test]
    fn ask_custom_options() {
        let mut q = AskQueue::default();
        let (r, rx) = Reply::channel();
        let n = NewAsk { options: vec!["Lewati".into(), " ".into(), "Jalankan".into()], ..ask("x", None) };
        let id = q.push(n, r, Instant::now()).unwrap();
        let a = q.get(id).unwrap();
        assert_eq!(a.options, vec!["Lewati".to_string(), "Jalankan".to_string()]);
        assert_eq!(a.approve_index(), 1);
        assert_eq!(q.answer(id, 1).as_deref(), Some("Jalankan"));
        assert_eq!(rx.recv().unwrap(), Response::Answer { choice: "Jalankan".into() });
    }

    #[test]
    fn the_always_allow_offer_needs_the_default_buttons() {
        let mut q = AskQueue::default();
        let offered = NewAsk { agent: Some("codex".into()), always: Some("npm test".into()), ..ask("x", None) };
        let id = q.push(offered, Reply::channel().0, Instant::now()).unwrap();
        let a = q.get(id).unwrap();
        assert_eq!((a.agent.as_deref(), a.always.as_deref()), (Some("codex"), Some("npm test")));
        // A client with its own buttons decides what they mean: no rule is offered.
        let custom = NewAsk { options: vec!["Lewati".into(), "Jalankan".into()], agent: Some("codex".into()), always: Some("npm test".into()), ..ask("x", None) };
        let id = q.push(custom, Reply::channel().0, Instant::now()).unwrap();
        assert_eq!(q.get(id).unwrap().always, None);
        // Blank options do not count as custom ones.
        let blank = NewAsk { options: vec![" ".into()], always: Some("npm test".into()), ..ask("x", None) };
        let id = q.push(blank, Reply::channel().0, Instant::now()).unwrap();
        assert_eq!(q.get(id).unwrap().always.as_deref(), Some("npm test"));
    }

    /// Wrapping never drops or collapses anything: joined back, the lines are the command.
    #[test]
    fn wrap_keeps_the_whole_command() {
        assert_eq!(wrap_command("  npm install express && npm run dev ", 46), vec!["npm install express && npm run dev"]);
        assert_eq!(wrap_command("cd app\r\n\tcargo   build", 46), vec!["cd app", "    cargo   build"]);
        let long = format!("git commit -am wip {} && rm -rf ./build", "x".repeat(300));
        let lines = wrap_command(&long, 46);
        assert!(lines.iter().all(|l| l.chars().count() <= 46), "{lines:?}");
        assert_eq!(lines.concat(), long);
        assert!(lines.last().unwrap().ends_with("&& rm -rf ./build"));
        // Breaks after a space near the end of a line rather than mid-word.
        let words = "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj kkkk";
        let w = wrap_command(words, 20);
        assert_eq!(w.concat(), words);
        assert!(w.iter().all(|l| l.chars().count() <= 20));
        assert!(w[0].ends_with(' '), "{w:?}");
        // Wide characters take two columns.
        assert_eq!(wrap_command(&"字".repeat(30), 46).len(), 2);
        // Bidi overrides / zero-width / controls are made visible, never hidden.
        let sneaky = "echo safe\u{202E}fr- mr\u{200B}\u{7}";
        let shown = wrap_command(sneaky, 46).concat();
        assert_eq!(shown, "echo safe\u{FFFD}fr- mr\u{FFFD}\u{FFFD}");
    }

    #[test]
    fn approve_needs_the_full_command_seen() {
        // Fits the card (≤ 3 lines of 46): approve from the card.
        assert_eq!(fit(None), Fit::Card);
        assert_eq!(fit(Some("npm test")), Fit::Card);
        assert_eq!(fit(Some(&"x".repeat(CARD_COLS * CARD_LINES))), Fit::Card);
        // One column more, or 4 short lines: review view first.
        assert_eq!(fit(Some(&"x".repeat(CARD_COLS * CARD_LINES + 1))), Fit::Review);
        assert_eq!(fit(Some("a\nb\nc\nd")), Fit::Review);
        // The hook's 4000-char cap still fits the review view (56 lines of 72).
        assert_eq!(fit(Some(&"y".repeat(4000))), Fit::Review);
        // Longer than 80 review lines: not offered at all.
        assert_eq!(fit(Some(&"z".repeat(REVIEW_COLS * MAX_REVIEW_LINES + 1))), Fit::TooLong);
        assert_eq!(fit(Some(&"q\n".repeat(MAX_REVIEW_LINES + 1))), Fit::TooLong);

        // Deny (button 0) always answers.
        for f in [Fit::Card, Fit::Review, Fit::TooLong] {
            assert_eq!(click(f, 0, false), Click::Answer);
        }
        // Approve (and any other option) on a long command opens the review until it was seen.
        assert_eq!(click(Fit::Card, 1, false), Click::Answer);
        assert_eq!(click(Fit::Review, 1, false), Click::Review);
        assert_eq!(click(Fit::Review, 2, false), Click::Review);
        assert_eq!(click(Fit::Review, 1, true), Click::Answer);
        assert_eq!(click(Fit::TooLong, 1, true), Click::Review);
    }

    #[test]
    fn fail_all_sends_errors() {
        let mut q = AskQueue::default();
        let (r, rx) = Reply::channel();
        q.push(ask("x", None), r, Instant::now());
        q.fail_all("paused");
        assert!(q.is_empty());
        assert_eq!(rx.recv().unwrap(), Response::Error { message: "paused".into() });
    }
}
