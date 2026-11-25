//! The local API between `mokuctl` (or any script) and the island, over a per-user named pipe
//! (`\\.\pipe\moku-<user SID>`, see [`pipe_name`]).
//!
//! Wire format: one JSON object per line (UTF-8, `\n`-terminated), at most [`MAX_LINE`] bytes.
//! A client connects, writes ONE request line, reads ONE response line, and disconnects.
//! The server gives a client [`READ_TIMEOUT_MS`] to deliver its request line.
//!
//! ```text
//! → {"type":"notify","title":"Build","body":"selesai","id":"ci","progress":null,"icon":null}
//! ← {"type":"ok"}
//! → {"type":"debug","op":"open-tab","id":"today"}
//! ← {"type":"ok"}
//! ```

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Every pipe name starts with this.
pub const PIPE_PREFIX: &str = r"\\.\pipe\";
/// Longest pipe name Windows accepts, prefix included.
const MAX_PIPE_NAME: usize = 256;
/// Longest request or response line accepted, newline included.
pub const MAX_LINE: usize = 64 * 1024;
/// A client must deliver its full request line within this time after connecting.
pub const READ_TIMEOUT_MS: u32 = 2000;

/// Client → island.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Is the island running? → `Pong`.
    Ping,
    /// Show a notification pill (and keep it in the notification centre).
    /// Same `id` again → replaces / updates in place (progress bars). `progress` 0.0–1.0.
    Notify {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
        /// Segoe Fluent icon name or glyph, e.g. `"download"`, `"\u{E896}"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<f32>,
    },
    /// AI permission card. The response (`Answer`) is sent when the user picks an option or the
    /// card times out, so the connection stays open while the card is shown.
    Ask {
        title: String,
        /// Second line, e.g. "Jalankan perintah · toko-online".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// Shown in a monospace box.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        /// Button labels; empty = the default pair (deny, approve).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_secs: Option<u32>,
        /// What `command` is: `"shell"` = a shell command line (the only kind that can get an
        /// "always allow this command" rule, v0.3 track Q); anything else, or absent, is not
        /// (a file path, a URL, a tool call). Never guessed: an old client sends nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Which coding agent asks (`"claude"`, `"codex"`, `"gemini"`). A rule created from this
        /// card applies to this agent only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    /// Local to-do list.
    Todo { add: String },
    /// Status of a coding-agent session (Claude Code via `mokuctl claude-hook`, or any script via
    /// `mokuctl session`). One pill per session `id`; answered `Ok` at once.
    Session {
        /// Stable per session (Claude Code's `session_id`).
        id: String,
        /// Short project name shown on the pill (the project folder's name).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        state: SessionState,
        /// Extra text: the notification message for `needs_input`, the error for `failed`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// When the event happened, in ms since the Unix epoch. An event older than the last one
        /// applied to the session is ignored (hooks that run in the background can arrive out of
        /// order), and `working` counts its elapsed time from here.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ts: Option<u64>,
        /// Which coding agent (`"claude"`, `"codex"`, `"gemini"`); absent = Claude Code (clients
        /// from before v0.3). Sessions of different agents never mix, even with equal ids.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    /// The settings app is about to delete the data folder ("Hapus semua data"): drop the data
    /// held in memory and finish pending writes, then answer `Ok`. Nothing old is written back.
    ClearData,
    /// Test / automation hooks (`mokuctl debug ...`).
    Debug(DebugOp),
}

/// What happened in an agent session ([`Request::Session`]). Wire form: snake_case
/// (`"needs_input"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// The session started (Claude Code `SessionStart`): listed, no pill.
    Start,
    /// A new turn began (`UserPromptSubmit`): "Bekerja…" with a timer from now.
    Working,
    /// Still busy (a tool ran, a question was answered): back to working after `needs_input`,
    /// keeps the turn's timer. Starts a turn only for a session that never had one.
    Active,
    /// Waiting for the user (`Notification`: a permission prompt or a question).
    NeedsInput,
    /// The turn finished (`Stop`): "✓ Selesai · 3m 12s" for a few seconds.
    Done,
    /// The turn ended with an error (`StopFailure`, e.g. a rate limit).
    Failed,
    /// A subagent finished (`SubagentStop`): counted, no state change.
    Subagent,
    /// The session ended (`SessionEnd`): removed.
    End,
}

impl SessionState {
    pub const ALL: [SessionState; 8] = [
        SessionState::Start,
        SessionState::Working,
        SessionState::Active,
        SessionState::NeedsInput,
        SessionState::Done,
        SessionState::Failed,
        SessionState::Subagent,
        SessionState::End,
    ];

    /// The wire name (`"needs_input"`).
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Start => "start",
            SessionState::Working => "working",
            SessionState::Active => "active",
            SessionState::NeedsInput => "needs_input",
            SessionState::Done => "done",
            SessionState::Failed => "failed",
            SessionState::Subagent => "subagent",
            SessionState::End => "end",
        }
    }

    /// Parse a wire name; `-` is accepted for `_` (`needs-input` on the command line).
    pub fn parse(s: &str) -> Option<SessionState> {
        let s = s.trim().to_ascii_lowercase().replace('-', "_");
        SessionState::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

/// `mokuctl debug <op>`. Flattened into the request: `{"type":"debug","op":"click","action":"tab:today"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum DebugOp {
    /// Open the panel on a tab (tab id, e.g. `"media"`, `"today"`).
    OpenTab { id: String },
    /// Collapse the panel back to the pill.
    Close,
    /// → `State` with the island's state as JSON (mode, tab, activities, size...).
    State,
    /// Deliver a click to an action id, as if the widget had been clicked.
    Click { action: String },
    /// → `Ok` once no animation is running (for screenshots).
    ScreenshotReady,
    /// Show a test activity (pill with an icon and a label). `secs` = transient duration;
    /// absent = persistent until `clear-activity`.
    Activity {
        id: String,
        priority: u8,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secs: Option<f32>,
    },
    ClearActivity { id: String },
    /// Type text into the input that has keyboard focus (as if typed; `compose` = set it as the
    /// IME composition instead). Error when no input has focus.
    Type {
        text: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        compose: bool,
    },
    /// Press a key in the focused input: `down`, `enter`, `esc`, `ctrl+backspace`, `alt+1` …
    Key { key: String },
    /// Feed a *fake* pointer position (physical px on the virtual desktop) to the placement logic
    /// (follow the cursor, move aside, hide until the top edge) instead of the real pointer,
    /// which is ignored from then on. `dt` = virtual ms since the previous fake sample (default
    /// 16), `down` = the left button is held. Without `x` / `y`: back to the real pointer. → `State`
    /// with the placement state. Never moves the real cursor.
    Cursor {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dt: Option<u32>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        down: bool,
    },
}

/// Island → client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Pong { version: String },
    /// Answer to `Ask`: the chosen option label, or `"timeout"` / `"dismissed"`.
    Answer { choice: String },
    State { state: serde_json::Value },
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum IpcError {
    /// No island listening on the pipe.
    NotRunning,
    /// Line longer than [`MAX_LINE`].
    Oversized,
    /// Connection closed before a line was read.
    Eof,
    /// No complete line in time.
    Timeout,
    /// Not valid JSON / unknown request.
    Malformed(String),
    Io(String),
    /// The program listening on the pipe does not run as this user (or that could not be
    /// checked): nothing was sent to it.
    Untrusted(String),
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcError::NotRunning => write!(f, "Moku is not running"),
            IpcError::Oversized => write!(f, "message longer than {MAX_LINE} bytes"),
            IpcError::Eof => write!(f, "connection closed"),
            IpcError::Timeout => write!(f, "timed out"),
            IpcError::Malformed(m) => write!(f, "malformed message: {m}"),
            IpcError::Io(m) => write!(f, "{m}"),
            IpcError::Untrusted(m) => write!(f, "refusing to talk to the Moku pipe: {m}"),
        }
    }
}

/// One wire line (JSON + `\n`).
pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).unwrap_or_else(|_| "{\"type\":\"error\",\"message\":\"encode\"}".into());
    s.push('\n');
    s
}

fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, IpcError> {
    if line.len() > MAX_LINE {
        return Err(IpcError::Oversized);
    }
    serde_json::from_str(line.trim()).map_err(|e| IpcError::Malformed(e.to_string()))
}

pub fn decode_request(line: &str) -> Result<Request, IpcError> {
    decode(line)
}

pub fn decode_response(line: &str) -> Result<Response, IpcError> {
    decode(line)
}

/// Read one `\n`-terminated line of at most `max` bytes (newline included). A final line
/// without a newline is accepted at EOF. Returns the line without its newline.
pub fn read_line<R: Read>(r: &mut R, max: usize) -> Result<String, IpcError> {
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        match r.read(&mut byte) {
            Ok(0) => {
                if buf.is_empty() {
                    return Err(IpcError::Eof);
                }
                break;
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
                if buf.len() + 1 > max {
                    return Err(IpcError::Oversized);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // A message-mode pipe reports "more data" as an error; byte-mode never does.
            Err(e) if e.raw_os_error() == Some(234) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                if buf.is_empty() {
                    return Err(IpcError::Eof);
                }
                break;
            }
            Err(e) => return Err(IpcError::Io(e.to_string())),
        }
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf).map_err(|_| IpcError::Malformed("not UTF-8".into()))
}

// ---- the pipe name ---------------------------------------------------------------------------

/// FNV-1a over the data folder's normalised spelling, as 12 hex digits.
fn short_hash(dir: &str) -> String {
    let norm = dir.trim().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in norm.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:012x}", h & 0xffff_ffff_ffff)
}

/// The pipe name for the given environment (`MOKU_PIPE`, `MOKU_DATA_DIR`) and user SID text,
/// plus a warning when `MOKU_PIPE` was set but unusable. Resolution order:
///
/// 1. `MOKU_PIPE`, when it starts with `\\.\pipe\` (and fits Windows' length limit); otherwise it
///    is ignored with a warning;
/// 2. else, when `MOKU_DATA_DIR` is set: `\\.\pipe\moku-<12 hex digits hashed from that folder>`,
///    so an island started with a scratch data folder (every test) never shares a pipe with the
///    user's real island, and a client with the same variable finds exactly that island;
/// 3. else `\\.\pipe\moku-<the current user's SID>`: one pipe per user, so two accounts on one
///    PC cannot take each other's name.
pub fn resolve_pipe_name(moku_pipe: Option<&str>, data_dir: Option<&str>, sid: &str) -> (String, Option<String>) {
    let mut warning = None;
    if let Some(p) = moku_pipe.map(str::trim).filter(|p| !p.is_empty()) {
        let prefixed = p.get(..PIPE_PREFIX.len()).is_some_and(|h| h.eq_ignore_ascii_case(PIPE_PREFIX)) && p.len() > PIPE_PREFIX.len();
        if prefixed && p.len() <= MAX_PIPE_NAME {
            return (p.to_string(), None);
        }
        warning = Some(format!("ignoring MOKU_PIPE: it must start with {PIPE_PREFIX} and be at most {MAX_PIPE_NAME} characters"));
    }
    if let Some(d) = data_dir.map(str::trim).filter(|d| !d.is_empty()) {
        return (format!("{PIPE_PREFIX}moku-{}", short_hash(d)), warning);
    }
    (format!("{PIPE_PREFIX}moku-{sid}"), warning)
}

/// [`pipe_name`] and the `MOKU_PIPE` warning, for callers that log (the island).
pub fn pipe_name_and_warning() -> (String, Option<String>) {
    let sid = crate::winsid::Sid::current_user().and_then(|s| s.to_text()).unwrap_or_else(|| "unknown".into());
    resolve_pipe_name(std::env::var("MOKU_PIPE").ok().as_deref(), std::env::var("MOKU_DATA_DIR").ok().as_deref(), &sid)
}

/// THE pipe name: the island serves on it, `mokuctl` and the settings app connect to it (see
/// [`resolve_pipe_name`] for the order). An unusable `MOKU_PIPE` is ignored with one warning on
/// stderr.
pub fn pipe_name() -> String {
    let (name, warning) = pipe_name_and_warning();
    if let Some(w) = warning {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| eprintln!("moku: {w}"));
    }
    name
}

// ---- the client ------------------------------------------------------------------------------

/// Is the process that serves `pipe` running as `user`? The client calls this before it sends a
/// word: anybody can create a pipe with a given name first, and an `ask` / `notify` would then
/// land in their hands. Unknown counts as "no".
pub fn verify_server_user(pipe: &std::fs::File, user: &crate::winsid::Sid) -> Result<(), IpcError> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    let mut pid = 0u32;
    // SAFETY: the handle belongs to `pipe`, which outlives the call.
    unsafe { windows::Win32::System::Pipes::GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle() as _), &mut pid) }.map_err(|e| IpcError::Untrusted(format!("cannot tell which program listens ({e})")))?;
    match crate::winsid::Sid::of_process(pid) {
        Some(theirs) if theirs.equals(user) => Ok(()),
        Some(_) => Err(IpcError::Untrusted("the program that listens runs as another user".into())),
        None => Err(IpcError::Untrusted("cannot tell whose program listens (it cannot be inspected)".into())),
    }
}

/// Client side: connect to the island, send `req`, wait up to `wait` for the response
/// (`None` = wait forever, used by `Ask`).
pub fn send(req: &Request, wait: Option<Duration>) -> Result<Response, IpcError> {
    send_to(&pipe_name(), req, wait)
}

/// [`send`] to another pipe name (tests). The server's user is checked here too: a pipe served
/// by another user's program is refused ([`IpcError::Untrusted`]) before anything is written.
pub fn send_to(pipe_name: &str, req: &Request, wait: Option<Duration>) -> Result<Response, IpcError> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut pipe = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(pipe_name) {
            Ok(f) => break f,
            // ERROR_FILE_NOT_FOUND: no server.
            Err(e) if e.raw_os_error() == Some(2) => return Err(IpcError::NotRunning),
            // ERROR_PIPE_BUSY: all instances in use; retry briefly.
            Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(IpcError::Io(e.to_string())),
        }
    };
    let me = crate::winsid::Sid::current_user().ok_or_else(|| IpcError::Untrusted("cannot tell which user this is".into()))?;
    verify_server_user(&pipe, &me)?;
    let line = encode(req);
    if line.len() > MAX_LINE {
        return Err(IpcError::Oversized);
    }
    pipe.write_all(line.as_bytes()).map_err(|e| IpcError::Io(e.to_string()))?;
    let _ = pipe.flush();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_line(&mut pipe, MAX_LINE));
    });
    let got = match wait {
        Some(d) => rx.recv_timeout(d).map_err(|_| IpcError::Timeout)?,
        None => rx.recv().map_err(|_| IpcError::Eof)?,
    };
    decode_response(&got?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_requests() -> Vec<Request> {
        vec![
            Request::Ping,
            Request::Notify { title: "Tes".into(), body: Some("Halo".into()), icon: Some("download".into()), id: Some("dl".into()), progress: Some(0.64) },
            Request::Notify { title: "Only title".into(), body: None, icon: None, id: None, progress: None },
            Request::Ask {
                title: "Claude Code minta izin".into(),
                detail: Some("Jalankan perintah · toko-online".into()),
                command: Some("npm install express && npm run dev".into()),
                options: vec!["Tolak".into(), "Setujui".into()],
                timeout_secs: Some(120),
                kind: Some("shell".into()),
                agent: Some("codex".into()),
            },
            Request::Ask { title: "x".into(), detail: None, command: None, options: vec![], timeout_secs: None, kind: None, agent: None },
            Request::Todo { add: "Bayar tagihan internet".into() },
            Request::Session { id: "abc123".into(), project: Some("toko-online".into()), state: SessionState::NeedsInput, detail: Some("Claude needs your permission to use Bash".into()), ts: Some(1_795_000_000_000), agent: None },
            Request::Session { id: "s".into(), project: None, state: SessionState::End, detail: None, ts: None, agent: Some("gemini".into()) },
            Request::ClearData,
            Request::Debug(DebugOp::OpenTab { id: "today".into() }),
            Request::Debug(DebugOp::Close),
            Request::Debug(DebugOp::State),
            Request::Debug(DebugOp::Click { action: "tab:media".into() }),
            Request::Debug(DebugOp::ScreenshotReady),
            Request::Debug(DebugOp::Activity { id: "t".into(), priority: 50, label: "Timer".into(), secs: Some(1.5) }),
            Request::Debug(DebugOp::ClearActivity { id: "t".into() }),
            Request::Debug(DebugOp::Type { text: "vsc".into(), compose: false }),
            Request::Debug(DebugOp::Type { text: "にほ".into(), compose: true }),
            Request::Debug(DebugOp::Key { key: "ctrl+backspace".into() }),
            Request::Debug(DebugOp::Cursor { x: Some(960), y: Some(-3), dt: Some(30), down: true }),
            Request::Debug(DebugOp::Cursor { x: None, y: None, dt: None, down: false }),
        ]
    }

    #[test]
    fn ipc_request_roundtrip() {
        for r in all_requests() {
            let line = encode(&r);
            assert!(line.ends_with('\n'));
            assert_eq!(line.matches('\n').count(), 1, "one line: {line}");
            assert_eq!(decode_request(&line).unwrap(), r, "{line}");
        }
        let responses = vec![
            Response::Ok,
            Response::Pong { version: "0.1.0".into() },
            Response::Answer { choice: "Setujui".into() },
            Response::State { state: serde_json::json!({"mode": "pill"}) },
            Response::Error { message: "nope".into() },
        ];
        for r in responses {
            assert_eq!(decode_response(&encode(&r)).unwrap(), r);
        }
    }

    #[test]
    fn ipc_wire_shape_is_flat() {
        assert_eq!(encode(&Request::Debug(DebugOp::OpenTab { id: "today".into() })), "{\"type\":\"debug\",\"op\":\"open-tab\",\"id\":\"today\"}\n");
        // Minimal hand-written notify (optional fields omitted) is accepted.
        let r = decode_request("{\"type\":\"notify\",\"title\":\"Hai\"}").unwrap();
        assert_eq!(r, Request::Notify { title: "Hai".into(), body: None, icon: None, id: None, progress: None });
    }

    #[test]
    fn ipc_session_wire_shape() {
        let r = Request::Session { id: "abc".into(), project: Some("toko".into()), state: SessionState::Working, detail: None, ts: Some(5), agent: None };
        assert_eq!(encode(&r), "{\"type\":\"session\",\"id\":\"abc\",\"project\":\"toko\",\"state\":\"working\",\"ts\":5}\n");
        let r = Request::Session { id: "t1".into(), project: None, state: SessionState::Done, detail: None, ts: None, agent: Some("codex".into()) };
        assert_eq!(encode(&r), "{\"type\":\"session\",\"id\":\"t1\",\"state\":\"done\",\"agent\":\"codex\"}\n");
        // Hand-written minimal form.
        assert_eq!(
            decode_request("{\"type\":\"session\",\"id\":\"x\",\"state\":\"needs_input\"}").unwrap(),
            Request::Session { id: "x".into(), project: None, state: SessionState::NeedsInput, detail: None, ts: None, agent: None }
        );
        // Every state round-trips through its wire name; the CLI spelling with '-' parses too.
        for s in SessionState::ALL {
            let line = encode(&Request::Session { id: "i".into(), project: None, state: s, detail: None, ts: None, agent: None });
            assert!(line.contains(&format!("\"state\":\"{}\"", s.as_str())), "{line}");
            assert_eq!(SessionState::parse(s.as_str()), Some(s));
        }
        assert_eq!(SessionState::parse(" Needs-Input "), Some(SessionState::NeedsInput));
        assert_eq!(SessionState::parse("sleeping"), None);
        // Unknown state, missing id or state → malformed.
        assert!(matches!(decode_request("{\"type\":\"session\",\"id\":\"x\",\"state\":\"sleeping\"}"), Err(IpcError::Malformed(_))));
        assert!(matches!(decode_request("{\"type\":\"session\",\"state\":\"done\"}"), Err(IpcError::Malformed(_))));
        assert!(matches!(decode_request("{\"type\":\"session\",\"id\":\"x\"}"), Err(IpcError::Malformed(_))));
        // A negative / fractional ts is not a timestamp.
        assert!(decode_request("{\"type\":\"session\",\"id\":\"x\",\"state\":\"done\",\"ts\":-1}").is_err());
    }

    const SID: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";

    #[test]
    fn pipe_name_resolution_order() {
        // 3. nothing set: one pipe per user.
        assert_eq!(resolve_pipe_name(None, None, SID), (format!(r"\\.\pipe\moku-{SID}"), None));
        assert_eq!(resolve_pipe_name(Some(""), Some("  "), SID).0, format!(r"\\.\pipe\moku-{SID}"), "blank values count as unset");
        // 2. a data folder: a pipe of its own, whatever the user.
        let (a, w) = resolve_pipe_name(None, Some(r"D:\scratch\moku-data"), SID);
        assert!(w.is_none() && a.starts_with(r"\\.\pipe\moku-") && a.len() == r"\\.\pipe\moku-".len() + 12, "{a}");
        assert_ne!(a, format!(r"\\.\pipe\moku-{SID}"));
        assert_eq!(a, resolve_pipe_name(None, Some(r"d:/scratch/MOKU-data/"), "S-1-5-other").0, "spelling and user do not matter, only the folder");
        assert_ne!(a, resolve_pipe_name(None, Some(r"D:\scratch\other-data"), SID).0);
        // 1. MOKU_PIPE wins, case-insensitive prefix, kept as typed.
        assert_eq!(resolve_pipe_name(Some(r"\\.\pipe\moku-test-x"), Some(r"E:\d"), SID), (r"\\.\pipe\moku-test-x".to_string(), None));
        assert_eq!(resolve_pipe_name(Some(r"\\.\PIPE\Mine"), None, SID).0, r"\\.\PIPE\Mine");
        // ...unless it is not a pipe name: ignored with a warning, the next rule applies.
        for bad in [r"moku-test", r"\\server\pipe\moku", r"C:\Windows\x", r"\\.\pipe\", r"\\.\pipe", "é\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}"] {
            let (n, w) = resolve_pipe_name(Some(bad), None, SID);
            assert_eq!(n, format!(r"\\.\pipe\moku-{SID}"), "{bad:?}");
            assert!(w.is_some_and(|w| w.contains("MOKU_PIPE")), "{bad:?}");
        }
        let (n, w) = resolve_pipe_name(Some("nope"), Some(r"E:\d"), SID);
        assert_eq!(n, resolve_pipe_name(None, Some(r"E:\d"), SID).0);
        assert!(w.is_some());
        let long = format!(r"\\.\pipe\{}", "x".repeat(300));
        assert!(resolve_pipe_name(Some(&long), None, SID).1.is_some(), "longer than Windows allows");
    }

    #[test]
    fn the_real_pipe_name_is_per_user_or_per_data_folder() {
        // Whatever this test run's environment says, the name is a valid pipe name and, without
        // MOKU_PIPE / MOKU_DATA_DIR, it carries the user's SID (never the old shared name).
        let n = pipe_name();
        assert!(n.starts_with(PIPE_PREFIX) && n != r"\\.\pipe\moku", "{n}");
        if std::env::var_os("MOKU_PIPE").is_none() && std::env::var_os("MOKU_DATA_DIR").is_none() {
            let sid = crate::winsid::Sid::current_user().unwrap().to_text().unwrap();
            assert_eq!(n, format!(r"\\.\pipe\moku-{sid}"));
        }
    }

    /// A pipe served by "someone else" is refused before a word is sent. The server here is this
    /// process (the same user), so "someone else" is simulated by expecting another SID.
    #[test]
    fn the_client_checks_who_serves_the_pipe() {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
        use windows::Win32::System::Pipes::{CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT};
        use windows::core::HSTRING;
        let name = format!(r"\\.\pipe\moku-test-core-verify-{}", std::process::id());
        let server = unsafe { CreateNamedPipeW(&HSTRING::from(name.as_str()), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 4096, 4096, 0, None) };
        assert!(!server.is_invalid());
        let client = std::fs::OpenOptions::new().read(true).write(true).open(&name).expect("connect");
        let me = crate::winsid::Sid::current_user().unwrap();
        assert_eq!(verify_server_user(&client, &me), Ok(()), "same user: fine");
        let system = crate::winsid::Sid::from_text("S-1-5-18").unwrap();
        let err = verify_server_user(&client, &system).unwrap_err();
        assert!(matches!(&err, IpcError::Untrusted(m) if m.contains("another user")), "{err:?}");
        assert!(err.to_string().contains("refusing to talk"), "{err}");
        // A file that is not a pipe at all cannot be verified either.
        let not_a_pipe = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        assert!(matches!(verify_server_user(&not_a_pipe, &me), Err(IpcError::Untrusted(_))));
        unsafe {
            let _ = CloseHandle(server);
        }
    }

    #[test]
    fn ipc_rejects_oversized_line() {
        let big = format!("{{\"type\":\"todo\",\"add\":\"{}\"}}\n", "x".repeat(MAX_LINE));
        assert_eq!(read_line(&mut std::io::Cursor::new(big.as_bytes()), MAX_LINE), Err(IpcError::Oversized));
        assert_eq!(decode_request(&big), Err(IpcError::Oversized));
        // Exactly at the limit (newline included) is fine.
        let ok = format!("{}\n", "y".repeat(MAX_LINE - 1));
        assert_eq!(read_line(&mut std::io::Cursor::new(ok.as_bytes()), MAX_LINE).unwrap().len(), MAX_LINE - 1);
    }

    #[test]
    fn ipc_read_line_edges() {
        let mut c = std::io::Cursor::new(b"{\"type\":\"ping\"}\r\nrest".to_vec());
        assert_eq!(read_line(&mut c, MAX_LINE).unwrap(), "{\"type\":\"ping\"}");
        assert_eq!(read_line(&mut c, MAX_LINE).unwrap(), "rest");
        assert_eq!(read_line(&mut c, MAX_LINE), Err(IpcError::Eof));
        assert!(matches!(decode_request("garbage"), Err(IpcError::Malformed(_))));
        assert!(matches!(decode_request("{\"type\":\"launch_missiles\"}"), Err(IpcError::Malformed(_))));
    }
}
