//! The pure session state machine behind the coding-agent pills (Claude Code, Codex CLI, Gemini
//! CLI): no `App`, no clock of its own
//! (every call gets `now` and the wall clock in ms), so it is fully unit-tested.
//!
//! ```text
//!            working                needs_input               done / failed
//!   start ─────────────► Working ─────────────────► NeedsInput ─────────────► Done | Failed
//!   (Idle)                 ▲  ◄──────── active ───────┘                         │
//!                          └───────────────────────── working (next prompt) ────┘
//!   end → removed · no event for `stale` → removed · subagent → counted, no change
//! ```
//!
//! Events carry the hook's own timestamp (`ts`, ms since the epoch). One older than the last
//! event applied to its session is ignored: hooks run in the background (`"async": true`) can
//! reach the island out of order, and a late `active` must not undo a `done`.

use std::time::{Duration, Instant};

use moku_core::ipc::SessionState;

/// Sessions kept at most (a misbehaving client cannot grow the list without bound).
pub const MAX_SESSIONS: usize = 32;
/// Longest stored id / project / detail, in chars (the rest is cut).
pub const MAX_ID: usize = 128;
pub const MAX_PROJECT: usize = 60;
pub const MAX_DETAIL: usize = 200;
/// A `ts` further in the past than this does not move a turn's start (clock nonsense).
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// Which coding agent a session belongs to. Sessions of different agents never mix, even when
/// their ids are equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Agent {
    /// Also what a client from before v0.3 means (no `agent` on the wire).
    #[default]
    Claude,
    Codex,
    Gemini,
}

impl Agent {
    /// The wire name (`Request::Session::agent`): absent = Claude Code; `None` = unknown agent.
    pub fn from_wire(s: Option<&str>) -> Option<Agent> {
        match s.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("claude") => Some(Agent::Claude),
            Some("codex") => Some(Agent::Codex),
            Some("gemini") => Some(Agent::Gemini),
            Some(_) => None,
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Gemini => "gemini",
        }
    }

    /// The name shown to the user.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Gemini => "Gemini CLI",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub agent: Agent,
    pub id: String,
    pub project: Option<String>,
    pub state: SessionState,
    pub detail: Option<String>,
    pub ts: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Known, no turn running (after `start`).
    Idle,
    Working,
    NeedsInput,
    /// The last turn finished.
    Done,
    /// The last turn stopped with an error.
    Failed,
}

impl Phase {
    /// Shown as a persistent pill.
    pub fn is_busy(self) -> bool {
        matches!(self, Phase::Working | Phase::NeedsInput)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub agent: Agent,
    /// The key it is stored under: the client's id for Claude Code, `<agent>:<id>` for the others.
    pub id: String,
    /// Folder name; empty when the client never sent one.
    pub project: String,
    pub phase: Phase,
    /// Start of the running (or last) turn.
    pub turn_start: Option<Instant>,
    /// Length of the last finished turn (Done / Failed), when its start was seen.
    pub took: Option<Duration>,
    /// When the last turn finished.
    pub finished_at: Option<Instant>,
    /// `needs_input` message / `failed` error.
    pub detail: Option<String>,
    /// Subagents that finished during the current turn.
    pub subagents: u32,
    pub last_event: Instant,
    /// Newest `ts` applied (0 = none seen).
    pub last_ts: u64,
    /// Creation order (stable list order).
    pub order: u64,
}

/// What [`Sessions::apply`] did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// Out of order, a duplicate, or nothing to do.
    Ignored,
    /// The session changed (or was created) without finishing a turn.
    Changed,
    /// A turn just ended: show the "Selesai" / "Berhenti" pill.
    Finished { took: Option<Duration>, failed: bool },
    /// The session is gone (`end`).
    Removed,
}

#[derive(Default, Debug)]
pub struct Sessions {
    list: Vec<Session>,
    next_order: u64,
}

fn cut(s: &str, max: usize) -> String {
    let t = s.trim();
    match t.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", t[..i].trim_end()),
        None => t.to_string(),
    }
}

fn clean(s: Option<&str>, max: usize) -> Option<String> {
    // One line: control characters (newlines in a notification message) become spaces.
    let one: String = s?.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let t = cut(&one.split_whitespace().collect::<Vec<_>>().join(" "), max);
    (!t.is_empty()).then_some(t)
}

/// The id a client's id is trimmed to: at most [`MAX_ID`] chars (+ "…").
pub fn key(id: &str) -> String {
    cut(id, MAX_ID)
}

/// The key a session of `agent` is stored under (empty when the id is empty). Claude Code keeps
/// its own id, so everything from before v0.3 keeps working; the others get their name in front.
pub fn scoped(agent: Agent, id: &str) -> String {
    let k = key(id);
    match agent {
        _ if k.is_empty() => k,
        Agent::Claude => k,
        a => format!("{}:{k}", a.slug()),
    }
}

/// When a turn started, from the event's wall-clock `ts` (falls back to `now`).
fn start_of(ts: Option<u64>, now: Instant, wall_ms: u64) -> Instant {
    let Some(ts) = ts else { return now };
    let age = Duration::from_millis(wall_ms.saturating_sub(ts));
    if age > MAX_AGE { now } else { now.checked_sub(age).unwrap_or(now) }
}

impl Sessions {
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Session> {
        self.list.iter().find(|s| s.id == id)
    }

    /// In the order they appeared.
    pub fn iter(&self) -> impl Iterator<Item = &Session> {
        self.list.iter()
    }

    /// Sessions in `Working` / `NeedsInput`.
    pub fn busy(&self) -> usize {
        self.list.iter().filter(|s| s.phase.is_busy()).count()
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let n = self.list.len();
        self.list.retain(|s| s.id != id);
        self.list.len() != n
    }

    pub fn clear(&mut self) {
        self.list.clear();
    }

    /// Apply one event. `now` = the island's monotonic clock, `wall_ms` = ms since the epoch
    /// (to read the event's `ts`).
    pub fn apply(&mut self, ev: Event, now: Instant, wall_ms: u64) -> Outcome {
        let id = scoped(ev.agent, &ev.id);
        if id.is_empty() {
            return Outcome::Ignored;
        }
        if ev.state == SessionState::End {
            // Always final, whatever its timestamp.
            return if self.remove(&id) { Outcome::Removed } else { Outcome::Ignored };
        }
        let project = clean(ev.project.as_deref(), MAX_PROJECT);
        let detail = clean(ev.detail.as_deref(), MAX_DETAIL);
        let mut created = false;
        let idx = match self.list.iter().position(|s| s.id == id) {
            Some(i) => {
                if ev.ts.is_some_and(|t| t < self.list[i].last_ts) {
                    return Outcome::Ignored;
                }
                i
            }
            None => {
                self.make_room();
                self.next_order += 1;
                created = true;
                self.list.push(Session {
                    agent: ev.agent,
                    id,
                    project: String::new(),
                    phase: Phase::Idle,
                    turn_start: None,
                    took: None,
                    finished_at: None,
                    detail: None,
                    subagents: 0,
                    last_event: now,
                    last_ts: 0,
                    order: self.next_order,
                });
                self.list.len() - 1
            }
        };
        let s = &mut self.list[idx];
        let before = s.clone();
        s.last_event = now;
        if let Some(t) = ev.ts {
            s.last_ts = s.last_ts.max(t);
        }
        if let Some(p) = project {
            s.project = p;
        }
        match ev.state {
            SessionState::Start | SessionState::End => {}
            SessionState::Working => {
                s.phase = Phase::Working;
                s.turn_start = Some(start_of(ev.ts, now, wall_ms));
                s.took = None;
                s.finished_at = None;
                s.detail = None;
                s.subagents = 0;
            }
            SessionState::Active => match s.phase {
                Phase::NeedsInput => {
                    s.phase = Phase::Working;
                    s.detail = None;
                    s.turn_start.get_or_insert(now);
                }
                // No prompt seen (Moku started while Claude worked): a turn from now.
                Phase::Idle => {
                    s.phase = Phase::Working;
                    s.turn_start = Some(now);
                }
                // Background work after the turn ended does not reopen it.
                Phase::Working | Phase::Done | Phase::Failed => {}
            },
            SessionState::NeedsInput => {
                s.phase = Phase::NeedsInput;
                s.detail = detail;
            }
            SessionState::Done | SessionState::Failed => {
                let failed = ev.state == SessionState::Failed;
                let target = if failed { Phase::Failed } else { Phase::Done };
                if s.phase == target {
                    return Outcome::Ignored;
                }
                s.took = if s.phase.is_busy() { s.turn_start.map(|t| now.saturating_duration_since(t)) } else { None };
                s.phase = target;
                s.finished_at = Some(now);
                s.detail = if failed { detail } else { None };
                return Outcome::Finished { took: s.took, failed };
            }
            SessionState::Subagent => s.subagents += 1,
        }
        // Only what is drawn counts (not the bookkeeping times).
        let same = (&s.project, s.phase, s.turn_start, &s.detail, s.subagents) == (&before.project, before.phase, before.turn_start, &before.detail, before.subagents);
        if same && !created { Outcome::Ignored } else { Outcome::Changed }
    }

    /// Drop the least recently active session when the list is full.
    fn make_room(&mut self) {
        while self.list.len() >= MAX_SESSIONS {
            let Some((i, _)) = self.list.iter().enumerate().min_by_key(|(_, s)| s.last_event) else { return };
            self.list.remove(i);
        }
    }

    /// Remove sessions with no event for `stale`. Returns their ids.
    pub fn sweep(&mut self, now: Instant, stale: Duration) -> Vec<String> {
        let mut gone = vec![];
        self.list.retain(|s| {
            let keep = now.saturating_duration_since(s.last_event) < stale;
            if !keep {
                gone.push(s.id.clone());
            }
            keep
        });
        gone
    }

    /// When the next session goes stale.
    pub fn next_stale(&self, stale: Duration) -> Option<Instant> {
        self.list.iter().map(|s| s.last_event + stale).min()
    }
}

/// "12s", "3m 12s", "1j 5m" (id) / "1h 5m" (en).
pub fn elapsed(d: Duration, en: bool) -> String {
    let t = d.as_secs();
    match t {
        0..60 => format!("{t}s"),
        60..3600 => format!("{}m {}s", t / 60, t % 60),
        _ => format!("{}{} {}m", t / 3600, if en { "h" } else { "j" }, (t % 3600) / 60),
    }
}

/// "baru saja", "5 mnt lalu", "2 jam lalu" / "just now", "5 min ago", "2 h ago".
pub fn ago(d: Duration, en: bool) -> String {
    let m = d.as_secs() / 60;
    match (m, en) {
        (0, false) => "baru saja".into(),
        (0, true) => "just now".into(),
        (1..60, false) => format!("{m} mnt lalu"),
        (1..60, true) => format!("{m} min ago"),
        (_, false) => format!("{} jam lalu", m / 60),
        (_, true) => format!("{} h ago", m / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WALL: u64 = 1_795_000_000_000;

    fn ev(id: &str, state: SessionState) -> Event {
        Event { agent: Agent::Claude, id: id.into(), project: Some("toko-online".into()), state, detail: None, ts: None }
    }

    fn at(id: &str, state: SessionState, ts: u64) -> Event {
        Event { ts: Some(ts), ..ev(id, state) }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn claude_session_lifecycle() {
        let t0 = Instant::now();
        let mut s = Sessions::default();
        // start: listed, idle, no pill.
        assert_eq!(s.apply(ev("a", SessionState::Start), t0, WALL), Outcome::Changed);
        let a = s.get("a").unwrap();
        assert_eq!((a.phase, a.project.as_str(), s.busy()), (Phase::Idle, "toko-online", 0));
        // working: a turn starts now.
        assert_eq!(s.apply(ev("a", SessionState::Working), t0 + secs(1), WALL), Outcome::Changed);
        assert_eq!(s.get("a").unwrap().phase, Phase::Working);
        assert_eq!(s.get("a").unwrap().turn_start, Some(t0 + secs(1)));
        assert_eq!(s.busy(), 1);
        // a tool ran: nothing changes, no redraw needed.
        assert_eq!(s.apply(ev("a", SessionState::Active), t0 + secs(5), WALL), Outcome::Ignored);
        // needs input, with the message (one line, trimmed).
        let n = Event { detail: Some("  Claude needs your\npermission to use Bash ".into()), ..ev("a", SessionState::NeedsInput) };
        assert_eq!(s.apply(n, t0 + secs(30), WALL), Outcome::Changed);
        let a = s.get("a").unwrap();
        assert_eq!((a.phase, a.detail.as_deref()), (Phase::NeedsInput, Some("Claude needs your permission to use Bash")));
        // answered in the terminal → the tool runs → working again, same turn timer.
        assert_eq!(s.apply(ev("a", SessionState::Active), t0 + secs(40), WALL), Outcome::Changed);
        let a = s.get("a").unwrap();
        assert_eq!((a.phase, a.detail.as_deref(), a.turn_start), (Phase::Working, None, Some(t0 + secs(1))));
        // done: "Selesai · 3m 12s".
        assert_eq!(s.apply(ev("a", SessionState::Done), t0 + secs(193), WALL), Outcome::Finished { took: Some(secs(192)), failed: false });
        let a = s.get("a").unwrap();
        assert_eq!((a.phase, a.finished_at, s.busy()), (Phase::Done, Some(t0 + secs(193)), 0));
        // A second Stop (another stop hook continued the turn and it stopped again later) is
        // not a second "Selesai" pill without a new turn.
        assert_eq!(s.apply(ev("a", SessionState::Done), t0 + secs(194), WALL), Outcome::Ignored);
        // Background work after the turn does not reopen it.
        assert_eq!(s.apply(ev("a", SessionState::Active), t0 + secs(195), WALL), Outcome::Ignored);
        assert_eq!(s.get("a").unwrap().phase, Phase::Done);
        // Next prompt: a fresh turn.
        assert_eq!(s.apply(ev("a", SessionState::Working), t0 + secs(300), WALL), Outcome::Changed);
        assert_eq!((s.get("a").unwrap().took, s.get("a").unwrap().turn_start), (None, Some(t0 + secs(300))));
        // end: removed.
        assert_eq!(s.apply(ev("a", SessionState::End), t0 + secs(301), WALL), Outcome::Removed);
        assert!(s.is_empty());
        assert_eq!(s.apply(ev("a", SessionState::End), t0 + secs(302), WALL), Outcome::Ignored);
    }

    #[test]
    fn claude_session_failure_subagents_and_unknown_sessions() {
        let t0 = Instant::now();
        let mut s = Sessions::default();
        // Moku started mid-turn: the first thing it hears is a tool → working from now.
        assert_eq!(s.apply(ev("b", SessionState::Active), t0, WALL), Outcome::Changed);
        assert_eq!(s.get("b").unwrap().phase, Phase::Working);
        // Subagents are counted, the phase stays.
        s.apply(ev("b", SessionState::Subagent), t0 + secs(1), WALL);
        assert_eq!(s.apply(ev("b", SessionState::Subagent), t0 + secs(2), WALL), Outcome::Changed);
        assert_eq!((s.get("b").unwrap().subagents, s.get("b").unwrap().phase), (2, Phase::Working));
        // A turn that fails (rate limit): "Berhenti", error kept.
        let f = Event { detail: Some("rate_limit".into()), ..ev("b", SessionState::Failed) };
        assert_eq!(s.apply(f, t0 + secs(62), WALL), Outcome::Finished { took: Some(secs(62)), failed: true });
        assert_eq!(s.get("b").unwrap().detail.as_deref(), Some("rate_limit"));
        // A Stop for a session never seen working: finished, length unknown.
        assert_eq!(s.apply(ev("c", SessionState::Done), t0, WALL), Outcome::Finished { took: None, failed: false });
        // needs_input for an unknown session creates it.
        assert_eq!(s.apply(ev("d", SessionState::NeedsInput), t0, WALL), Outcome::Changed);
        assert_eq!(s.get("d").unwrap().phase, Phase::NeedsInput);
        // A new turn resets the subagent count.
        s.apply(ev("b", SessionState::Working), t0 + secs(70), WALL);
        assert_eq!(s.get("b").unwrap().subagents, 0);
        // Empty id → ignored.
        assert_eq!(s.apply(ev("  ", SessionState::Working), t0, WALL), Outcome::Ignored);
    }

    #[test]
    fn claude_session_out_of_order_events_are_ignored() {
        let t0 = Instant::now();
        let mut s = Sessions::default();
        s.apply(at("a", SessionState::Working, WALL - 10_000), t0, WALL);
        s.apply(at("a", SessionState::Done, WALL - 2_000), t0 + secs(8), WALL);
        // A background PostToolUse hook from before the Stop arrives late: ignored.
        assert_eq!(s.apply(at("a", SessionState::Active, WALL - 3_000), t0 + secs(9), WALL), Outcome::Ignored);
        assert_eq!(s.get("a").unwrap().phase, Phase::Done);
        // Same for a late notification.
        assert_eq!(s.apply(at("a", SessionState::NeedsInput, WALL - 2_500), t0 + secs(9), WALL), Outcome::Ignored);
        // An event without ts is always applied; a newer one too.
        assert_eq!(s.apply(at("a", SessionState::Working, WALL - 1_000), t0 + secs(9), WALL), Outcome::Changed);
        // `end` wins whatever its ts.
        assert_eq!(s.apply(at("a", SessionState::End, 1), t0 + secs(10), WALL), Outcome::Removed);
    }

    #[test]
    fn claude_session_timer_counts_from_the_event_ts() {
        let t0 = Instant::now() + secs(3600);
        let mut s = Sessions::default();
        // The prompt was submitted 192 s ago (a delayed background hook): the timer says so.
        s.apply(at("a", SessionState::Working, WALL - 192_000), t0, WALL);
        assert_eq!(s.get("a").unwrap().turn_start, Some(t0 - secs(192)));
        assert_eq!(s.apply(at("a", SessionState::Done, WALL), t0, WALL), Outcome::Finished { took: Some(secs(192)), failed: false });
        // A ts in the future or absurdly old counts from now.
        s.apply(at("b", SessionState::Working, WALL + 50_000), t0, WALL);
        assert_eq!(s.get("b").unwrap().turn_start, Some(t0));
        s.apply(at("c", SessionState::Working, 5), t0, WALL);
        assert_eq!(s.get("c").unwrap().turn_start, Some(t0));
    }

    #[test]
    fn claude_session_staleness() {
        let t0 = Instant::now();
        let stale = secs(30 * 60);
        let mut s = Sessions::default();
        s.apply(ev("a", SessionState::Working), t0, WALL);
        s.apply(ev("b", SessionState::Working), t0 + secs(600), WALL);
        assert_eq!(s.next_stale(stale), Some(t0 + stale));
        assert!(s.sweep(t0 + stale - secs(1), stale).is_empty());
        // Any event keeps a session alive.
        s.apply(ev("b", SessionState::Subagent), t0 + secs(1700), WALL);
        assert_eq!(s.sweep(t0 + stale, stale), vec!["a".to_string()]);
        assert_eq!(s.next_stale(stale), Some(t0 + secs(1700) + stale));
        assert_eq!(s.sweep(t0 + secs(1700) + stale, stale), vec!["b".to_string()]);
        assert_eq!(s.next_stale(stale), None);
    }

    #[test]
    fn claude_several_sessions() {
        let t0 = Instant::now();
        let mut s = Sessions::default();
        s.apply(ev("a", SessionState::Working), t0, WALL);
        let b = Event { project: Some("blog".into()), ..ev("b", SessionState::Working) };
        s.apply(b, t0 + secs(1), WALL);
        s.apply(ev("c", SessionState::Start), t0 + secs(2), WALL);
        assert_eq!((s.len(), s.busy()), (3, 2));
        // Independent states; list order = first seen.
        s.apply(Event { project: None, ..ev("b", SessionState::NeedsInput) }, t0 + secs(3), WALL);
        s.apply(ev("a", SessionState::Done), t0 + secs(4), WALL);
        let phases: Vec<(&str, Phase)> = s.iter().map(|x| (x.id.as_str(), x.phase)).collect();
        assert_eq!(phases, vec![("a", Phase::Done), ("b", Phase::NeedsInput), ("c", Phase::Idle)]);
        assert_eq!(s.get("b").unwrap().project, "blog");
        // A project name is kept when later events omit it.
        s.apply(Event { project: None, ..ev("b", SessionState::Active) }, t0 + secs(5), WALL);
        assert_eq!(s.get("b").unwrap().project, "blog");
        // The list is bounded: the least recently active session makes room.
        for i in 0..MAX_SESSIONS {
            s.apply(ev(&format!("x{i}"), SessionState::Start), t0 + secs(10 + i as u64), WALL);
        }
        assert_eq!(s.len(), MAX_SESSIONS);
        assert!(s.get("c").is_none() && s.get("a").is_none() && s.get("b").is_none());
        // Long text is cut.
        let long = Event { id: "y".repeat(500), project: Some("p".repeat(500)), detail: Some("d".repeat(500)), ..ev("", SessionState::NeedsInput) };
        s.apply(long, t0 + secs(100), WALL);
        let y = s.iter().last().unwrap();
        assert_eq!((y.id.chars().count(), y.project.chars().count(), y.detail.as_ref().unwrap().chars().count()), (MAX_ID + 1, MAX_PROJECT + 1, MAX_DETAIL + 1));
    }

    #[test]
    fn sessions_of_different_agents_never_mix() {
        let t0 = Instant::now();
        let mut s = Sessions::default();
        // The same id from three agents: three sessions.
        let from = |agent, state| Event { agent, ..ev("same-id", state) };
        for a in [Agent::Claude, Agent::Codex, Agent::Gemini] {
            assert_eq!(s.apply(from(a, SessionState::Working), t0, WALL), Outcome::Changed, "{a:?}");
        }
        assert_eq!((s.len(), s.busy()), (3, 3));
        // Claude Code keeps its plain id (what v0.2 stored); the others are scoped by name.
        let ids: Vec<(&str, Agent)> = s.iter().map(|x| (x.id.as_str(), x.agent)).collect();
        assert_eq!(ids, vec![("same-id", Agent::Claude), ("codex:same-id", Agent::Codex), ("gemini:same-id", Agent::Gemini)]);
        // Events only reach their own agent's session.
        assert_eq!(s.apply(from(Agent::Codex, SessionState::Done), t0 + secs(5), WALL), Outcome::Finished { took: Some(secs(5)), failed: false });
        assert_eq!(s.get("same-id").unwrap().phase, Phase::Working);
        assert_eq!(s.get("gemini:same-id").unwrap().phase, Phase::Working);
        assert_eq!(s.get("codex:same-id").unwrap().phase, Phase::Done);
        // End removes only its own.
        assert_eq!(s.apply(from(Agent::Gemini, SessionState::End), t0 + secs(6), WALL), Outcome::Removed);
        assert_eq!((s.len(), s.get("same-id").is_some()), (2, true));
        // An empty id is empty for every agent (no "codex:" session).
        assert_eq!(scoped(Agent::Codex, "  "), "");
        assert_eq!(s.apply(Event { agent: Agent::Codex, ..ev(" ", SessionState::Working) }, t0, WALL), Outcome::Ignored);
        // Long ids are cut once, then scoped.
        assert_eq!(scoped(Agent::Codex, &"z".repeat(500)).chars().count(), "codex:".len() + MAX_ID + 1);
    }

    #[test]
    fn agent_wire_names() {
        assert_eq!(Agent::from_wire(None), Some(Agent::Claude));
        assert_eq!(Agent::from_wire(Some("")), Some(Agent::Claude));
        assert_eq!(Agent::from_wire(Some("claude")), Some(Agent::Claude));
        assert_eq!(Agent::from_wire(Some(" Codex ")), Some(Agent::Codex));
        assert_eq!(Agent::from_wire(Some("gemini")), Some(Agent::Gemini));
        assert_eq!(Agent::from_wire(Some("copilot")), None);
        for a in [Agent::Claude, Agent::Codex, Agent::Gemini] {
            assert_eq!(Agent::from_wire(Some(a.slug())), Some(a));
        }
    }

    #[test]
    fn claude_elapsed_and_ago_text() {
        assert_eq!(elapsed(secs(12), false), "12s");
        assert_eq!(elapsed(secs(192), false), "3m 12s");
        assert_eq!(elapsed(secs(3900), false), "1j 5m");
        assert_eq!(elapsed(secs(3900), true), "1h 5m");
        assert_eq!(ago(secs(30), false), "baru saja");
        assert_eq!(ago(secs(300), true), "5 min ago");
        assert_eq!(ago(secs(7300), false), "2 jam lalu");
    }
}
