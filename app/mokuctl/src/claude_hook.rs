//! `mokuctl claude-hook`: Claude Code's hook input (JSON on stdin) → `Request::Session`.
//!
//! Event names and input fields as documented at <https://code.claude.com/docs/en/hooks>
//! (checked 2026-11-24): every event has `session_id`, `cwd` and `hook_event_name`;
//! `Notification` adds `message`, optional `title` and `notification_type`; `StopFailure` adds
//! `error`. Claude Code sets `CLAUDE_PROJECT_DIR` for hooks; the project is that folder's name
//! (else `cwd`'s).
//!
//! | `hook_event_name` | state |
//! |---|---|
//! | `SessionStart` | `start` |
//! | `UserPromptSubmit` | `working` |
//! | `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | `active` |
//! | `Notification` that waits for the user (`permission_prompt`, `elicitation_dialog`, `elicitation_url_dialog`, `agent_needs_input`, or no type) | `needs_input` + `message` |
//! | `Notification` `elicitation_response` / `elicitation_complete` | `active` |
//! | other `Notification`s (`idle_prompt`, `auth_success`, …) | — |
//! | `Stop` | `done` |
//! | `StopFailure` | `failed` + `error` |
//! | `SubagentStop` | `subagent` |
//! | `SessionEnd` | `end` |
//! | anything else | — |
//!
//! The hook never gets in Claude Code's way: it prints nothing (stdout of `UserPromptSubmit` and
//! `SessionStart` hooks would become Claude's context), exits 0, and gives the island
//! [`BUDGET`] in total before it gives up.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use moku_core::ipc::{self, IpcError, Request, Response, SessionState};

/// Connect + send + answer, all together. Moku answers a session event at once; a busy or hung
/// island must not slow Claude Code down.
pub const BUDGET: Duration = Duration::from_millis(100);

/// Notification types that mean "Claude waits for you".
const WAITING: [&str; 4] = ["permission_prompt", "elicitation_dialog", "elicitation_url_dialog", "agent_needs_input"];
/// Notification types that mean "answered, Claude goes on".
const RESUMED: [&str; 2] = ["elicitation_response", "elicitation_complete"];

/// Last path component of a folder (`C:\src\toko-online\` → `toko-online`).
fn folder_name(path: &str) -> Option<String> {
    let name = path.trim().trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next()?.trim();
    // A drive root ("C:") is no project name.
    (!name.is_empty() && !name.ends_with(':')).then(|| name.to_string())
}

/// The session request for one hook input, or `None` for events that change nothing.
/// `project_dir` = `CLAUDE_PROJECT_DIR`; `now_ms` stamps the event.
pub fn request(raw: &[u8], project_dir: Option<&str>, now_ms: u64) -> Option<Request> {
    let s = String::from_utf8_lossy(raw);
    let v: serde_json::Value = serde_json::from_str(s.trim_start_matches('\u{feff}').trim()).ok()?;
    let o = v.as_object()?;
    let text = |k: &str| o.get(k).and_then(|x| x.as_str()).map(str::trim).filter(|x| !x.is_empty());
    let id = text("session_id")?.to_string();
    let (state, detail) = match text("hook_event_name")? {
        "SessionStart" => (SessionState::Start, None),
        "UserPromptSubmit" => (SessionState::Working, None),
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => (SessionState::Active, None),
        "Notification" => match text("notification_type") {
            Some(t) if RESUMED.contains(&t) => (SessionState::Active, None),
            None => (SessionState::NeedsInput, text("message")),
            Some(t) if WAITING.contains(&t) => (SessionState::NeedsInput, text("message")),
            Some(_) => return None,
        },
        "Stop" => (SessionState::Done, None),
        "StopFailure" => (SessionState::Failed, text("error")),
        "SubagentStop" => (SessionState::Subagent, None),
        "SessionEnd" => (SessionState::End, None),
        _ => return None,
    };
    let project = project_dir.filter(|p| !p.trim().is_empty()).or(text("cwd")).and_then(folder_name);
    Some(Request::Session { id, project, state, detail: detail.map(str::to_string), ts: Some(now_ms), agent: None })
}

/// Send `req` to `pipe`, giving up after `budget` whatever happens (no island, all pipe
/// instances busy, an island that does not answer).
pub fn send_within(pipe: &str, req: &Request, budget: Duration) -> Result<Response, IpcError> {
    let deadline = Instant::now() + budget;
    let (tx, rx) = mpsc::channel();
    let (pipe, req) = (pipe.to_string(), req.clone());
    // The sender may block (a pipe that is busy for 2 s); this thread is simply left behind
    // when the process exits.
    std::thread::Builder::new()
        .name("send".into())
        .spawn(move || {
            let _ = tx.send(ipc::send_to(&pipe, &req, Some(budget)));
        })
        .map_err(|e| IpcError::Io(e.to_string()))?;
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap_or(Err(IpcError::Timeout))
}

/// The whole `claude-hook` command. Prints only with `print` (the request line, not sent).
pub fn run(raw: &[u8], print: bool, pipe: &str) {
    let dir = std::env::var("CLAUDE_PROJECT_DIR").ok();
    let Some(req) = request(raw, dir.as_deref(), crate::now_ms()) else { return };
    if print {
        print!("{}", ipc::encode(&req));
        return;
    }
    let _ = send_within(pipe, &req, BUDGET);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(event: &str, extra: &str) -> Vec<u8> {
        format!(r#"{{"session_id":"abc123","transcript_path":"C:\\Users\\u\\.claude\\projects\\x.jsonl","cwd":"C:\\src\\toko-online\\web","permission_mode":"default","hook_event_name":"{event}"{extra}}}"#).into_bytes()
    }

    fn state_of(event: &str, extra: &str) -> Option<(SessionState, Option<String>)> {
        match request(&input(event, extra), Some("C:\\src\\toko-online"), 99)? {
            Request::Session { id, project, state, detail, ts, agent } => {
                assert_eq!(agent, None, "Claude Code is the default agent: nothing on the wire");
                assert_eq!((id.as_str(), project.as_deref(), ts), ("abc123", Some("toko-online"), Some(99)));
                Some((state, detail))
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn claude_hook_maps_every_event() {
        use SessionState::*;
        assert_eq!(state_of("SessionStart", r#","source":"startup","model":"claude-x""#), Some((Start, None)));
        assert_eq!(state_of("UserPromptSubmit", r#","prompt":"tulis tes""#), Some((Working, None)));
        assert_eq!(state_of("PreToolUse", r#","tool_name":"Bash","tool_input":{"command":"npm test"},"tool_use_id":"t""#), Some((Active, None)));
        assert_eq!(state_of("PostToolUse", r#","tool_name":"Bash""#), Some((Active, None)));
        assert_eq!(state_of("PostToolUseFailure", r#","tool_name":"Bash""#), Some((Active, None)));
        let msg = Some("Claude needs your permission to use Bash".to_string());
        assert_eq!(state_of("Notification", r#","message":"Claude needs your permission to use Bash","notification_type":"permission_prompt""#), Some((NeedsInput, msg.clone())));
        assert_eq!(state_of("Notification", r#","message":"Pilih satu","title":"Q","notification_type":"elicitation_dialog""#), Some((NeedsInput, Some("Pilih satu".into()))));
        assert_eq!(state_of("Notification", r#","message":"x","notification_type":"agent_needs_input""#).map(|x| x.0), Some(NeedsInput));
        // Older Claude Code without a type: treated as waiting.
        assert_eq!(state_of("Notification", r#","message":"Claude needs your permission to use Bash""#), Some((NeedsInput, msg)));
        assert_eq!(state_of("Notification", r#","message":"ok","notification_type":"elicitation_response""#), Some((Active, None)));
        // Idle reminders and the like change nothing (the turn already ended).
        assert_eq!(state_of("Notification", r#","message":"Claude is waiting for your input","notification_type":"idle_prompt""#), None);
        assert_eq!(state_of("Notification", r#","message":"ok","notification_type":"auth_success""#), None);
        assert_eq!(state_of("Stop", r#","stop_hook_active":false,"last_assistant_message":"Selesai.""#), Some((Done, None)));
        assert_eq!(state_of("StopFailure", r#","error":"rate_limit","last_assistant_message":"API Error: Rate limit reached""#), Some((Failed, Some("rate_limit".into()))));
        assert_eq!(state_of("SubagentStop", r#","agent_id":"a1","agent_type":"Explore","stop_hook_active":false"#), Some((Subagent, None)));
        assert_eq!(state_of("SessionEnd", r#","reason":"prompt_input_exit""#), Some((End, None)));
        // Events this feature does not use.
        for e in ["PreCompact", "PermissionRequest", "SubagentStart", "CwdChanged", "Bogus"] {
            assert_eq!(state_of(e, ""), None, "{e}");
        }
    }

    #[test]
    fn claude_hook_project_and_bad_input() {
        let project = |raw: &[u8], dir: Option<&str>| match request(raw, dir, 0) {
            Some(Request::Session { project, .. }) => project,
            _ => panic!("no request"),
        };
        let stop = input("Stop", "");
        // CLAUDE_PROJECT_DIR first (cwd may be a subfolder), then cwd.
        assert_eq!(project(&stop, Some("D:\\work\\blog\\")).as_deref(), Some("blog"));
        assert_eq!(project(&stop, None).as_deref(), Some("web"));
        assert_eq!(project(&stop, Some("  ")).as_deref(), Some("web"));
        assert_eq!(project(br#"{"session_id":"s","hook_event_name":"Stop","cwd":"/home/u/api/"}"#, None).as_deref(), Some("api"));
        assert_eq!(project(br#"{"session_id":"s","hook_event_name":"Stop","cwd":"C:\\"}"#, None), None);
        assert_eq!(project(br#"{"session_id":"s","hook_event_name":"Stop"}"#, None), None);
        // BOM tolerated.
        assert!(request("\u{feff}{\"session_id\":\"s\",\"hook_event_name\":\"Stop\"}".as_bytes(), None, 0).is_some());
        // No session id, no event, not JSON → nothing.
        assert!(request(br#"{"hook_event_name":"Stop"}"#, None, 0).is_none());
        assert!(request(br#"{"session_id":"","hook_event_name":"Stop"}"#, None, 0).is_none());
        assert!(request(br#"{"session_id":"s"}"#, None, 0).is_none());
        assert!(request(b"not json", None, 0).is_none());
        assert!(request(b"", None, 0).is_none());
        assert!(request(b"[1,2]", None, 0).is_none());
    }

    #[test]
    fn claude_hook_gives_up_quickly_without_moku() {
        let req = request(&input("Stop", ""), None, 0).unwrap();
        let t = Instant::now();
        // A pipe name of this test process only: never the user's island, never another test's.
        let r = send_within(&format!(r"\\.\pipe\moku-test-nobody-here-{}", std::process::id()), &req, BUDGET);
        assert_eq!(r, Err(IpcError::NotRunning));
        assert!(t.elapsed() < BUDGET, "{:?}", t.elapsed());
    }
}
