//! `mokuctl agent-hook --agent <claude|codex|gemini>`: the hook of a coding agent (JSON on stdin,
//! or one JSON argument for Codex's `notify`) → a session status on the island and, where the
//! agent's hook can carry a decision, a permission card whose answer goes back to the agent.
//!
//! The formats below are the agents' own documentation (checked 2026-11-25; every format is
//! also a fixture in `tests/fixtures/agent-hook/`, see `docs/agents.md` for the links):
//!
//! | agent | mechanism | status | permission card |
//! |---|---|---|---|
//! | Claude Code | hooks (`claude_hook.rs`; the card is `tools/claude-hook.ps1`) | all events | yes (PreToolUse, by the script) |
//! | Codex CLI | hooks (`~/.codex/hooks.json`) and the older `notify` program | SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, PermissionRequest, Stop, SubagentStop, Interrupt, SessionEnd (`notify`: `agent-turn-complete` only) | yes: `PermissionRequest` accepts `allow` / `deny` |
//! | Gemini CLI | hooks (`~/.gemini/settings.json`) | SessionStart, BeforeAgent, BeforeTool, AfterTool, Notification, AfterAgent, SessionEnd | no: a BeforeTool hook can only deny, and the Notification hook "cannot grant permissions" |
//!
//! Like `claude-hook`, an agent hook never gets in the agent's way: exit code 0 whatever happens,
//! nothing on stdout except a decision the user gave on the island (Gemini wants JSON on stdout,
//! so it always gets `{}`), and the island gets [`BUDGET`] for a status event. Only a permission
//! card waits, up to its timeout (default 120 s), and only for the agents that can use the answer.
//! Message texts of the agent (prompts, replies) are never read, kept or sent.

use std::time::Duration;

use moku_core::ipc::{self, Request, Response, SessionState};
use serde_json::Value;

pub use crate::claude_hook::BUDGET;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Gemini,
}

impl Agent {
    pub fn parse(s: &str) -> Option<Agent> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Some(Agent::Claude),
            "codex" | "openai-codex" => Some(Agent::Codex),
            "gemini" | "gemini-cli" => Some(Agent::Gemini),
            _ => None,
        }
    }

    /// The `agent` of IPC requests.
    pub fn slug(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Gemini => "gemini",
        }
    }

    /// The name on the card.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Gemini => "Gemini CLI",
        }
    }
}

/// A permission the agent waits for (Codex `PermissionRequest`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub tool: String,
    /// The text for the card's monospace box: the command, a patch, or the tool's input.
    pub command: String,
    /// `command` is a shell command line (the only kind that can get an "always allow" rule).
    pub shell: bool,
    pub project: Option<String>,
}

/// What one hook input means.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub session: Option<Request>,
    pub permission: Option<Permission>,
}

impl Parsed {
    fn none() -> Parsed {
        Parsed { session: None, permission: None }
    }
}

/// Last folder of a path (`C:\src\toko-online\` → `toko-online`; a drive root is no project).
fn folder_name(path: &str) -> Option<String> {
    let name = path.trim().trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next()?.trim();
    (!name.is_empty() && !name.ends_with(':')).then(|| name.to_string())
}

fn json_of(raw: &[u8]) -> Option<Value> {
    let s = String::from_utf8_lossy(raw);
    let v: Value = serde_json::from_str(s.trim_start_matches('\u{feff}').trim()).ok()?;
    v.is_object().then_some(v)
}

fn session(agent: Agent, id: &str, project: Option<String>, state: SessionState, detail: Option<String>, now_ms: u64) -> Request {
    Request::Session { id: id.to_string(), project, state, detail, ts: Some(now_ms), agent: Some(agent.slug().to_string()) }
}

/// The sentence the island translates (`localize_notification`): what a card or a notification
/// of an agent other than Claude Code says in `detail`.
fn needs_permission(tool: &str) -> String {
    format!("Needs your permission to use {tool}")
}

/// Codex tool input → the card text. `Bash`: the command; `apply_patch`: the patch; other tools
/// (MCP ...): the input as compact JSON, so the user sees what would run.
fn codex_card(tool: &str, input: &Value) -> (String, bool) {
    let command = input.get("command").and_then(|c| c.as_str());
    match (tool, command) {
        ("Bash", Some(c)) => (c.to_string(), true),
        (_, Some(c)) => (c.to_string(), false),
        _ => (serde_json::to_string(input).unwrap_or_default(), false),
    }
}

/// Codex: hooks (`hook_event_name` + `session_id`) or the `notify` program's JSON
/// (`"type":"agent-turn-complete"`, `thread-id`, `cwd`).
fn parse_codex(v: &Value, now_ms: u64) -> Parsed {
    let o = v.as_object().expect("checked by json_of");
    let text = |k: &str| o.get(k).and_then(|x| x.as_str()).map(str::trim).filter(|x| !x.is_empty());
    let project = text("cwd").and_then(folder_name);
    // The older `notify` mechanism: one event, `agent-turn-complete`. The messages in it
    // (`input-messages`, `last-assistant-message`) are never looked at.
    if let (Some("agent-turn-complete"), Some(id)) = (text("type"), text("thread-id")) {
        return Parsed { session: Some(session(Agent::Codex, id, project, SessionState::Done, None, now_ms)), permission: None };
    }
    let (Some(id), Some(event)) = (text("session_id"), text("hook_event_name")) else { return Parsed::none() };
    let st = |state, detail| Parsed { session: Some(session(Agent::Codex, id, project.clone(), state, detail, now_ms)), permission: None };
    match event {
        "SessionStart" => st(SessionState::Start, None),
        "UserPromptSubmit" => st(SessionState::Working, None),
        "PreToolUse" | "PostToolUse" => st(SessionState::Active, None),
        "PermissionRequest" => {
            let tool = text("tool_name").unwrap_or("tool");
            let (command, shell) = codex_card(tool, o.get("tool_input").unwrap_or(&Value::Null));
            Parsed {
                session: Some(session(Agent::Codex, id, project.clone(), SessionState::NeedsInput, Some(needs_permission(tool)), now_ms)),
                permission: Some(Permission { tool: tool.to_string(), command, shell, project }),
            }
        }
        "Stop" => st(SessionState::Done, None),
        "Interrupt" => st(SessionState::Failed, Some("interrupted".into())),
        "SubagentStop" => st(SessionState::Subagent, None),
        "SessionEnd" => st(SessionState::End, None),
        // PreCompact, PostCompact, PreModelRequest, PostModelRequest, SubagentStart: nothing to show.
        _ => Parsed::none(),
    }
}

/// Gemini CLI hooks: `session_id`, `cwd`, `hook_event_name`; `Notification` adds `message`.
fn parse_gemini(v: &Value, now_ms: u64) -> Parsed {
    let o = v.as_object().expect("checked by json_of");
    let text = |k: &str| o.get(k).and_then(|x| x.as_str()).map(str::trim).filter(|x| !x.is_empty());
    let (Some(id), Some(event)) = (text("session_id"), text("hook_event_name")) else { return Parsed::none() };
    let project = text("cwd").and_then(folder_name);
    let (state, detail) = match event {
        "SessionStart" => (SessionState::Start, None),
        // BeforeAgent = the user's prompt reached the agent (its text is not read).
        "BeforeAgent" => (SessionState::Working, None),
        "BeforeTool" | "AfterTool" => (SessionState::Active, None),
        // The CLI is asking the user (`notification_type: ToolPermission`, `message` = what).
        "Notification" => (SessionState::NeedsInput, text("message").map(str::to_string)),
        "AfterAgent" => (SessionState::Done, None),
        "SessionEnd" => (SessionState::End, None),
        // BeforeModel / AfterModel / BeforeToolSelection / PreCompress: too frequent, nothing to show.
        _ => return Parsed::none(),
    };
    Parsed { session: Some(session(Agent::Gemini, id, project, state, detail, now_ms)), permission: None }
}

/// The meaning of one hook input. `project_dir` = the agent's project-dir variable, when it sets
/// one (Claude Code's `CLAUDE_PROJECT_DIR`).
pub fn parse(agent: Agent, raw: &[u8], project_dir: Option<&str>, now_ms: u64) -> Parsed {
    match agent {
        Agent::Claude => Parsed { session: crate::claude_hook::request(raw, project_dir, now_ms), permission: None },
        Agent::Codex => json_of(raw).map(|v| parse_codex(&v, now_ms)).unwrap_or_else(Parsed::none),
        Agent::Gemini => json_of(raw).map(|v| parse_gemini(&v, now_ms)).unwrap_or_else(Parsed::none),
    }
}

/// What the user chose on the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// What the hook prints on stdout, if anything. Codex `PermissionRequest` takes a decision;
/// Gemini wants JSON on stdout (`{}` = nothing to say); Claude Code and Codex status hooks print
/// nothing (the stdout of some events would become the agent's context).
pub fn stdout(agent: Agent, permission: bool, decision: Option<(Decision, &str)>) -> Option<String> {
    match (agent, decision) {
        (Agent::Codex, Some((Decision::Allow, _))) if permission => Some(r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}"#.to_string()),
        (Agent::Codex, Some((Decision::Deny, why))) if permission => {
            let v = serde_json::json!({"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":why}}});
            Some(v.to_string())
        }
        (Agent::Gemini, _) => Some("{}".to_string()),
        _ => None,
    }
}

/// Card language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Id,
    En,
}

fn tr(lang: Lang, id: &str, en: &str) -> String {
    if lang == Lang::En { en } else { id }.to_string()
}

/// The `Ask` for a permission. Same wording as `claude-hook.ps1` ("<what> · <project>").
pub fn card(agent: Agent, p: &Permission, lang: Lang, timeout_secs: u32) -> Request {
    let what = match p.tool.as_str() {
        "Bash" => tr(lang, "Jalankan perintah", "Run a command"),
        "apply_patch" => tr(lang, "Ubah file (patch)", "Edit files (patch)"),
        t => format!("{}{t}", tr(lang, "Pakai alat ", "Use tool ")),
    };
    let detail = match &p.project {
        Some(pr) => format!("{what} \u{00B7} {pr}"),
        None => what,
    };
    let title = match lang {
        Lang::Id => format!("{} minta izin", agent.name()),
        Lang::En => format!("{} asks for permission", agent.name()),
    };
    Request::Ask {
        title,
        detail: Some(detail),
        command: Some(p.command.clone()),
        options: vec![],
        timeout_secs: Some(timeout_secs),
        kind: p.shell.then(|| "shell".to_string()),
        agent: Some(agent.slug().to_string()),
    }
}

/// The island's answer → a decision (`None` = no decision: timeout, an error, an unknown choice;
/// the agent then asks the user itself).
pub fn decision_of(resp: &Result<Response, ipc::IpcError>) -> Option<Decision> {
    match resp {
        Ok(Response::Answer { choice }) => match choice.to_ascii_lowercase().as_str() {
            "approve" | "allow" | "setujui" | "izinkan" => Some(Decision::Allow),
            "deny" | "tolak" => Some(Decision::Deny),
            _ => None,
        },
        _ => None,
    }
}

/// How the command was invoked.
pub struct Options {
    pub print: bool,
    pub lang: Lang,
    pub timeout_secs: u32,
}

/// The whole command. Returns what to print on stdout. `print` shows the requests instead of
/// sending them. Never fails: anything wrong means no decision.
pub fn run(agent: Agent, raw: &[u8], opts: &Options, pipe: &str, project_dir: Option<&str>, now_ms: u64) -> Option<String> {
    let parsed = parse(agent, raw, project_dir, now_ms);
    if opts.print {
        let mut requests: Vec<Request> = parsed.session.clone().into_iter().collect();
        requests.extend(parsed.permission.iter().map(|p| card(agent, p, opts.lang, opts.timeout_secs)));
        return Some(requests.iter().map(ipc::encode).collect());
    }
    if let Some(s) = &parsed.session {
        let _ = crate::claude_hook::send_within(pipe, s, BUDGET);
    }
    let mut decision = None;
    if let Some(p) = &parsed.permission {
        // The island answers when the user does, or `timeout` after the card's wait; the extra
        // 15 s is only the safety net for an island that never answers.
        let wait = Duration::from_secs(opts.timeout_secs as u64 + 15);
        let answer = ipc::send_to(pipe, &card(agent, p, opts.lang, opts.timeout_secs), Some(wait));
        decision = decision_of(&answer);
        // The turn goes on (or the denied tool is skipped): back to "Bekerja…".
        if let Some(Request::Session { id, project, .. }) = &parsed.session {
            let back = session(agent, id, project.clone(), SessionState::Active, None, now_ms.max(crate::now_ms()));
            let _ = crate::claude_hook::send_within(pipe, &back, BUDGET);
        }
    }
    let why = match opts.lang {
        Lang::Id => "Pengguna menolak lewat Moku.",
        Lang::En => "The user denied this in Moku.",
    };
    stdout(agent, parsed.permission.is_some(), decision.map(|d| (d, why)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures: the documented input of each agent (tests/fixtures/agent-hook/).
    const CODEX_SESSION_START: &str = include_str!("../tests/fixtures/agent-hook/codex-session-start.json");
    const CODEX_USER_PROMPT: &str = include_str!("../tests/fixtures/agent-hook/codex-user-prompt-submit.json");
    const CODEX_PRE_TOOL: &str = include_str!("../tests/fixtures/agent-hook/codex-pre-tool-use.json");
    const CODEX_POST_TOOL: &str = include_str!("../tests/fixtures/agent-hook/codex-post-tool-use.json");
    const CODEX_PERMISSION: &str = include_str!("../tests/fixtures/agent-hook/codex-permission-request.json");
    const CODEX_PERMISSION_PATCH: &str = include_str!("../tests/fixtures/agent-hook/codex-permission-request-patch.json");
    const CODEX_STOP: &str = include_str!("../tests/fixtures/agent-hook/codex-stop.json");
    const CODEX_INTERRUPT: &str = include_str!("../tests/fixtures/agent-hook/codex-interrupt.json");
    const CODEX_SUBAGENT_STOP: &str = include_str!("../tests/fixtures/agent-hook/codex-subagent-stop.json");
    const CODEX_SESSION_END: &str = include_str!("../tests/fixtures/agent-hook/codex-session-end.json");
    const CODEX_PRE_COMPACT: &str = include_str!("../tests/fixtures/agent-hook/codex-pre-compact.json");
    const CODEX_NOTIFY: &str = include_str!("../tests/fixtures/agent-hook/codex-notify-agent-turn-complete.json");
    const GEMINI_SESSION_START: &str = include_str!("../tests/fixtures/agent-hook/gemini-session-start.json");
    const GEMINI_BEFORE_AGENT: &str = include_str!("../tests/fixtures/agent-hook/gemini-before-agent.json");
    const GEMINI_BEFORE_TOOL: &str = include_str!("../tests/fixtures/agent-hook/gemini-before-tool.json");
    const GEMINI_AFTER_TOOL: &str = include_str!("../tests/fixtures/agent-hook/gemini-after-tool.json");
    const GEMINI_NOTIFICATION: &str = include_str!("../tests/fixtures/agent-hook/gemini-notification.json");
    const GEMINI_AFTER_AGENT: &str = include_str!("../tests/fixtures/agent-hook/gemini-after-agent.json");
    const GEMINI_SESSION_END: &str = include_str!("../tests/fixtures/agent-hook/gemini-session-end.json");
    const GEMINI_BEFORE_MODEL: &str = include_str!("../tests/fixtures/agent-hook/gemini-before-model.json");

    fn sess(agent: Agent, raw: &str) -> Option<(String, Option<String>, SessionState, Option<String>)> {
        match parse(agent, raw.as_bytes(), None, 77).session? {
            Request::Session { id, project, state, detail, ts, agent: a } => {
                assert_eq!(ts, Some(77));
                assert_eq!(a.as_deref(), Some(agent.slug()), "every session says which agent it is");
                Some((id, project, state, detail))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn codex_hooks_map_to_session_states() {
        use SessionState::*;
        let id = "019a-thread";
        let proj = Some("toko-online".to_string());
        assert_eq!(sess(Agent::Codex, CODEX_SESSION_START), Some((id.into(), proj.clone(), Start, None)));
        assert_eq!(sess(Agent::Codex, CODEX_USER_PROMPT), Some((id.into(), proj.clone(), Working, None)));
        assert_eq!(sess(Agent::Codex, CODEX_PRE_TOOL), Some((id.into(), proj.clone(), Active, None)));
        assert_eq!(sess(Agent::Codex, CODEX_POST_TOOL), Some((id.into(), proj.clone(), Active, None)));
        assert_eq!(sess(Agent::Codex, CODEX_STOP), Some((id.into(), proj.clone(), Done, None)));
        assert_eq!(sess(Agent::Codex, CODEX_SUBAGENT_STOP), Some((id.into(), proj.clone(), Subagent, None)));
        assert_eq!(sess(Agent::Codex, CODEX_SESSION_END), Some((id.into(), proj.clone(), End, None)));
        assert_eq!(sess(Agent::Codex, CODEX_INTERRUPT), Some((id.into(), proj.clone(), Failed, Some("interrupted".into()))));
        // Events this feature does not show.
        assert_eq!(sess(Agent::Codex, CODEX_PRE_COMPACT), None);
        // No card for events that are not permission requests.
        for raw in [CODEX_SESSION_START, CODEX_USER_PROMPT, CODEX_PRE_TOOL, CODEX_STOP] {
            assert_eq!(parse(Agent::Codex, raw.as_bytes(), None, 1).permission, None);
        }
    }

    #[test]
    fn codex_permission_request_makes_a_card_and_waits_status() {
        let p = parse(Agent::Codex, CODEX_PERMISSION.as_bytes(), None, 5);
        // The session shows "Butuh input" while the card waits.
        let Some(Request::Session { state, detail, agent, .. }) = &p.session else { panic!("{p:?}") };
        assert_eq!(*state, SessionState::NeedsInput);
        assert_eq!(detail.as_deref(), Some("Needs your permission to use Bash"));
        assert_eq!(agent.as_deref(), Some("codex"));
        let perm = p.permission.clone().unwrap();
        assert_eq!(perm, Permission { tool: "Bash".into(), command: "npm install express".into(), shell: true, project: Some("toko-online".into()) });
        // The card: Codex's name, the command exactly, a shell command of agent codex.
        let Request::Ask { title, detail, command, options, timeout_secs, kind, agent } = card(Agent::Codex, &perm, Lang::Id, 120) else { panic!() };
        assert_eq!(title, "Codex minta izin");
        assert_eq!(detail.as_deref(), Some("Jalankan perintah \u{00B7} toko-online"));
        assert_eq!(command.as_deref(), Some("npm install express"));
        assert!(options.is_empty());
        assert_eq!((timeout_secs, kind.as_deref(), agent.as_deref()), (Some(120), Some("shell"), Some("codex")));
        let Request::Ask { title, detail, .. } = card(Agent::Codex, &perm, Lang::En, 60) else { panic!() };
        assert_eq!((title.as_str(), detail.as_deref()), ("Codex asks for permission", Some("Run a command \u{00B7} toko-online")));
    }

    #[test]
    fn codex_patch_and_other_tools_are_cards_but_never_shell_commands() {
        let perm = parse(Agent::Codex, CODEX_PERMISSION_PATCH.as_bytes(), None, 5).permission.unwrap();
        assert_eq!(perm.tool, "apply_patch");
        assert!(perm.command.starts_with("*** Begin Patch"), "{}", perm.command);
        assert!(!perm.shell, "a patch is not a command line: no always-allow");
        let Request::Ask { kind, detail, .. } = card(Agent::Codex, &perm, Lang::Id, 120) else { panic!() };
        assert_eq!(kind, None);
        assert_eq!(detail.as_deref(), Some("Ubah file (patch) \u{00B7} toko-online"));
        // An MCP tool: its input as JSON; never "shell".
        let mcp = br#"{"session_id":"s","cwd":"C:\\x\\blog","hook_event_name":"PermissionRequest","tool_name":"mcp__fs__write","tool_input":{"path":"a.txt","text":"hi"}}"#;
        let perm = parse(Agent::Codex, mcp, None, 1).permission.unwrap();
        assert_eq!((perm.tool.as_str(), perm.shell), ("mcp__fs__write", false));
        assert_eq!(serde_json::from_str::<Value>(&perm.command).unwrap(), serde_json::json!({"path":"a.txt","text":"hi"}));
        // Another tool that has a `command` field is shown, but is not Bash: no rule either.
        let other = br#"{"session_id":"s","hook_event_name":"PermissionRequest","tool_name":"exec","tool_input":{"command":"ls"}}"#;
        let perm = parse(Agent::Codex, other, None, 1).permission.unwrap();
        assert_eq!((perm.command.as_str(), perm.shell), ("ls", false));
        // No tool input at all: still a card (with `null`), never a crash.
        let bare = br#"{"session_id":"s","hook_event_name":"PermissionRequest","tool_name":"Bash"}"#;
        assert!(parse(Agent::Codex, bare, None, 1).permission.is_some());
    }

    #[test]
    fn codex_notify_program_gets_one_json_argument() {
        // `notify = ["mokuctl", "agent-hook", "--agent", "codex"]` → Codex appends the JSON.
        let got = sess(Agent::Codex, CODEX_NOTIFY);
        assert_eq!(got, Some(("019a-thread".into(), Some("toko-online".into()), SessionState::Done, None)));
        assert_eq!(parse(Agent::Codex, CODEX_NOTIFY.as_bytes(), None, 1).permission, None);
        // The messages in it are never copied into the request.
        let line = ipc::encode(&parse(Agent::Codex, CODEX_NOTIFY.as_bytes(), None, 1).session.unwrap());
        assert!(!line.contains("rahasia") && !line.contains("Selesai"), "{line}");
        // Another notify type, or no thread id: nothing.
        assert!(parse(Agent::Codex, br#"{"type":"something-new","thread-id":"t"}"#, None, 1).session.is_none());
        assert!(parse(Agent::Codex, br#"{"type":"agent-turn-complete"}"#, None, 1).session.is_none());
    }

    #[test]
    fn gemini_hooks_map_to_session_states() {
        use SessionState::*;
        let id = "gem-5f2a";
        let proj = Some("blog".to_string());
        assert_eq!(sess(Agent::Gemini, GEMINI_SESSION_START), Some((id.into(), proj.clone(), Start, None)));
        assert_eq!(sess(Agent::Gemini, GEMINI_BEFORE_AGENT), Some((id.into(), proj.clone(), Working, None)));
        assert_eq!(sess(Agent::Gemini, GEMINI_BEFORE_TOOL), Some((id.into(), proj.clone(), Active, None)));
        assert_eq!(sess(Agent::Gemini, GEMINI_AFTER_TOOL), Some((id.into(), proj.clone(), Active, None)));
        assert_eq!(sess(Agent::Gemini, GEMINI_NOTIFICATION), Some((id.into(), proj.clone(), NeedsInput, Some("Please approve: run_shell_command".into()))));
        assert_eq!(sess(Agent::Gemini, GEMINI_AFTER_AGENT), Some((id.into(), proj.clone(), Done, None)));
        assert_eq!(sess(Agent::Gemini, GEMINI_SESSION_END), Some((id.into(), proj, End, None)));
        assert_eq!(sess(Agent::Gemini, GEMINI_BEFORE_MODEL), None);
        // Gemini has no card: a BeforeTool hook can only deny.
        for raw in [GEMINI_BEFORE_TOOL, GEMINI_NOTIFICATION] {
            assert_eq!(parse(Agent::Gemini, raw.as_bytes(), None, 1).permission, None);
        }
        // The prompt and the reply are never copied.
        let line = ipc::encode(&parse(Agent::Gemini, GEMINI_AFTER_AGENT.as_bytes(), None, 1).session.unwrap());
        assert!(!line.contains("rahasia"), "{line}");
    }

    #[test]
    fn claude_is_handled_by_the_claude_adapter_unchanged() {
        let raw = br#"{"session_id":"abc","cwd":"C:\\src\\toko-online","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#;
        let p = parse(Agent::Claude, raw, Some("C:\\src\\toko-online"), 9);
        assert_eq!(p.session, Some(Request::Session { id: "abc".into(), project: Some("toko-online".into()), state: SessionState::Working, detail: None, ts: Some(9), agent: None }));
        // The card for PreToolUse stays with tools/claude-hook.ps1.
        let pre = br#"{"session_id":"abc","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        assert_eq!(parse(Agent::Claude, pre, None, 9).permission, None);
    }

    #[test]
    fn bad_input_is_nothing() {
        for agent in [Agent::Claude, Agent::Codex, Agent::Gemini] {
            for raw in ["", "not json", "[1,2]", "null", "{}", r#"{"session_id":"s"}"#, r#"{"hook_event_name":"Stop"}"#, r#"{"session_id":"","hook_event_name":"Stop"}"#] {
                assert_eq!(parse(agent, raw.as_bytes(), None, 1), Parsed::none(), "{agent:?} {raw:?}");
            }
        }
        // BOM tolerated (Windows PowerShell pipes).
        let bom = format!("\u{feff}{CODEX_STOP}");
        assert!(parse(Agent::Codex, bom.as_bytes(), None, 1).session.is_some());
    }

    #[test]
    fn agent_names_parse() {
        assert_eq!(Agent::parse("codex"), Some(Agent::Codex));
        assert_eq!(Agent::parse(" Gemini "), Some(Agent::Gemini));
        assert_eq!(Agent::parse("claude"), Some(Agent::Claude));
        assert_eq!(Agent::parse("copilot"), None);
        for a in [Agent::Claude, Agent::Codex, Agent::Gemini] {
            assert_eq!(Agent::parse(a.slug()), Some(a));
        }
    }

    #[test]
    fn stdout_is_a_decision_only_when_the_agent_can_use_one() {
        let allow = stdout(Agent::Codex, true, Some((Decision::Allow, "x"))).unwrap();
        let v: Value = serde_json::from_str(&allow).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
        let deny = stdout(Agent::Codex, true, Some((Decision::Deny, "Pengguna menolak \"ini\""))).unwrap();
        let v: Value = serde_json::from_str(&deny).unwrap();
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(v["hookSpecificOutput"]["decision"]["message"], "Pengguna menolak \"ini\"");
        // No answer (timeout, island down, card off): nothing, so Codex asks in its own prompt.
        assert_eq!(stdout(Agent::Codex, true, None), None);
        // A status event never prints, even with a "decision" by mistake.
        assert_eq!(stdout(Agent::Codex, false, Some((Decision::Allow, ""))), None);
        assert_eq!(stdout(Agent::Claude, false, None), None);
        // Gemini wants JSON on stdout, always `{}`: it never gets a decision from Moku.
        assert_eq!(stdout(Agent::Gemini, false, None).as_deref(), Some("{}"));
        assert_eq!(stdout(Agent::Gemini, false, Some((Decision::Allow, ""))).as_deref(), Some("{}"));
    }

    #[test]
    fn answers_map_to_decisions_and_everything_else_is_no_decision() {
        let ans = |c: &str| decision_of(&Ok(Response::Answer { choice: c.into() }));
        assert_eq!(ans("approve"), Some(Decision::Allow));
        assert_eq!(ans("Setujui"), Some(Decision::Allow));
        assert_eq!(ans("deny"), Some(Decision::Deny));
        assert_eq!(ans("Tolak"), Some(Decision::Deny));
        assert_eq!(ans("timeout"), None);
        assert_eq!(ans("dismissed"), None);
        assert_eq!(ans("something else"), None);
        assert_eq!(decision_of(&Ok(Response::Ok)), None);
        assert_eq!(decision_of(&Ok(Response::Error { message: "off".into() })), None);
        assert_eq!(decision_of(&Err(ipc::IpcError::NotRunning)), None);
        assert_eq!(decision_of(&Err(ipc::IpcError::Timeout)), None);
    }

    #[test]
    fn print_shows_the_requests_and_sends_nothing() {
        let o = Options { print: true, lang: Lang::Id, timeout_secs: 120 };
        // A pipe nobody serves: --print must not even try it.
        let out = run(Agent::Codex, CODEX_PERMISSION.as_bytes(), &o, r"\\.\pipe\moku-test-print-nobody", None, 5).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert!(lines[0].starts_with(r#"{"type":"session""#) && lines[0].contains(r#""state":"needs_input""#) && lines[0].contains(r#""agent":"codex""#), "{}", lines[0]);
        assert!(lines[1].starts_with(r#"{"type":"ask""#) && lines[1].contains(r#""kind":"shell""#), "{}", lines[1]);
    }
}
