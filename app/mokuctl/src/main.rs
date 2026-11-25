//! `mokuctl` — talk to the Moku island over its per-user named pipe (`moku_core::ipc::pipe_name`).
//!
//! Output: short human text by default (`ask` prints the answer, `notify` prints nothing);
//! `--json` prints the island's raw JSON response line instead. `debug` always prints JSON.
//! Exit codes: 0 ok / `ask` approved, 1 error or `ask` denied, 2 `ask` timed out,
//! 3 Moku is not running, 64 usage error.

use std::io::Read;
use std::process::ExitCode;
use std::time::Duration;

use moku_core::ipc::{self, DebugOp, IpcError, Request, Response, SessionState};

mod agent_hook;
mod claude_hook;

const USAGE: &str = "\
mokuctl — control the Moku island

USAGE
  mokuctl ping
  mokuctl notify --title <t> [--body <b>] [--icon <name>] [--id <id>] [--progress <0..1>]
  mokuctl progress --title <t> --value <0..1 | 0..100 | 64%> [--id <id>] [--body <b>] [--icon <name>]
              (same --id or same title updates in place; --value 1 finishes it)
  mokuctl ask --title <t> [--detail <d>] [--project <p>] [--command <c>] [--option <label>]...
              [--timeout <s>] [--kind shell] [--agent claude|codex|gemini] [--stdin]
              prints approve | deny | <option> | timeout
              exit code: 0 approved, 1 denied (or error), 2 timed out (default 120 s)
              --stdin reads a JSON object {title, detail, command, options, timeout_secs, kind,
              agent} from stdin (UTF-8; safe for commands with quotes); flags override it.
              --kind shell + --agent say the command is a shell command line of that agent: the
              card then offers 'Selalu izinkan perintah ini' and applies the user's rules
  mokuctl todo add <text>
  mokuctl session --id <id> --state <state> [--agent claude|codex|gemini] [--project <p>]
              [--detail <d>] [--ts <ms>] [--stdin]
              agent session status (one pill per id); state: start | working | active |
              needs-input | done | failed | subagent | end
              --stdin reads a JSON object {id, project, state, detail, ts, agent}; flags override it
  mokuctl claude-hook [--print]
              Claude Code hook (SessionStart, UserPromptSubmit, PreToolUse, PostToolUse,
              Notification, Stop, StopFailure, SubagentStop, SessionEnd): reads the hook's JSON
              from stdin and sends the session status. Prints nothing, always exits 0, gives
              up after 100 ms (Moku down or busy never slows Claude Code). --print shows the
              request instead of sending it
  mokuctl agent-hook --agent claude|codex|gemini [--print] [--lang id|en] [--timeout <s>] [<json>]
              hook of a coding agent: reads its JSON from stdin (Codex `notify`: from the one
              argument) and shows the session on the island. Codex permission requests get a
              card; its answer is printed in the format Codex's hook expects. Gemini CLI: status
              only. Exit code 0 always, gives up on a status after 100 ms (docs/agents.md)
  mokuctl pipe-name
              print the pipe name this mokuctl talks to (for scripts and tests)
  mokuctl debug open-tab <id> | close | state | click <action> | screenshot-ready
  mokuctl debug activity <id> <priority> <label> [--secs <s>]
  mokuctl debug clear-activity <id>
  mokuctl debug type <text> [--compose] | key <down|enter|esc|ctrl+backspace|alt+1|...>
              (feed the text input that has focus, e.g. the launcher)
  mokuctl debug cursor <x> <y> [--dt <ms>] [--down] | cursor real
              feed a FAKE pointer position (physical px) to the placement logic (follow the cursor,
              move aside, hide until the top edge); --dt = virtual ms since the last sample
              (default 16). Never moves the real cursor. `cursor real` = the real pointer again
              (test hooks: only `debug state` works unless the island runs with MOKU_DEBUG=1)

OPTIONS
  --json     print the island's JSON response (errors too, on stdout) instead of text
  -h, --help this help

ICONS (--icon)
  bell download upload check warning error info code message calendar timer music
  photo file folder lock shield sync mic ai, or a single Segoe Fluent glyph

EXIT CODES
  0 ok, 1 error / denied, 2 ask timed out, 3 Moku is not running, 64 usage error

ENVIRONMENT
  MOKU_PIPE      pipe name to use, must start with \\\\.\\pipe\\ (tests)
  MOKU_DATA_DIR  talk to the island that uses this data folder (its pipe name is derived from it)
  Without either, the pipe of the current user is used. mokuctl refuses a pipe whose server
  is not run by the same user.
";

/// Flags that take no value.
const SWITCHES: [&str; 5] = ["json", "stdin", "print", "compose", "down"];

struct Opts {
    pos: Vec<String>,
    flags: Vec<(String, String)>,
}

impl Opts {
    fn parse(args: &[String]) -> Result<Opts, String> {
        let (mut pos, mut flags) = (vec![], vec![]);
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if let Some(name) = a.strip_prefix("--") {
                if SWITCHES.contains(&name) {
                    flags.push((name.to_string(), String::new()));
                    i += 1;
                    continue;
                }
                let v = args.get(i + 1).ok_or_else(|| format!("--{name} needs a value"))?;
                flags.push((name.to_string(), v.clone()));
                i += 2;
            } else {
                pos.push(a.clone());
                i += 1;
            }
        }
        Ok(Opts { pos, flags })
    }
    fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == k)
    }
    fn get(&self, k: &str) -> Option<String> {
        self.flags.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }
    fn all(&self, k: &str) -> Vec<String> {
        self.flags.iter().filter(|(n, _)| n == k).map(|(_, v)| v.clone()).collect()
    }
    fn need(&self, k: &str) -> Result<String, String> {
        self.get(k).filter(|v| !v.trim().is_empty()).ok_or_else(|| format!("missing --{k}"))
    }
    fn num(&self, k: &str) -> Result<Option<f32>, String> {
        self.get(k).map(|v| v.trim().parse::<f32>().ok().filter(|f| f.is_finite()).ok_or_else(|| format!("--{k} must be a number"))).transpose()
    }
}

/// `0.64`, `64`, `64%` → 0.64.
fn parse_fraction(v: &str) -> Result<f32, String> {
    let t = v.trim();
    let (t, pct) = match t.strip_suffix('%') {
        Some(s) => (s.trim(), true),
        None => (t, false),
    };
    let f: f32 = t.parse().ok().filter(|f: &f32| f.is_finite()).ok_or_else(|| format!("not a number: {v:?}"))?;
    let f = if pct || f > 1.0 { f / 100.0 } else { f };
    Ok(f.clamp(0.0, 1.0))
}

/// `mokuctl ask --stdin` input. Unknown fields are ignored.
#[derive(Default)]
struct AskInput {
    title: Option<String>,
    detail: Option<String>,
    command: Option<String>,
    options: Vec<String>,
    timeout_secs: Option<u32>,
    kind: Option<String>,
    agent: Option<String>,
}

fn parse_ask_input(raw: &[u8]) -> Result<AskInput, String> {
    let s = String::from_utf8_lossy(raw);
    let s = s.trim_start_matches('\u{feff}').trim();
    if s.is_empty() {
        return Ok(AskInput::default());
    }
    let v: serde_json::Value = serde_json::from_str(s).map_err(|e| format!("--stdin: not JSON: {e}"))?;
    let obj = v.as_object().ok_or("--stdin: expected a JSON object")?;
    let text = |k: &str| obj.get(k).and_then(|x| x.as_str()).map(str::to_string);
    Ok(AskInput {
        title: text("title"),
        detail: text("detail"),
        command: text("command"),
        options: obj.get("options").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|o| o.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        timeout_secs: obj.get("timeout_secs").and_then(|x| x.as_f64()).filter(|t| t.is_finite()).map(|t| t.clamp(1.0, 3600.0) as u32),
        kind: text("kind"),
        agent: text("agent"),
    })
}

fn build_ask(o: &Opts, input: AskInput) -> Result<(Request, Option<Duration>), String> {
    let title = o.get("title").filter(|t| !t.trim().is_empty()).or(input.title.filter(|t| !t.trim().is_empty())).ok_or("missing --title")?;
    let mut detail = o.get("detail").or(input.detail);
    if let Some(p) = o.get("project").filter(|p| !p.trim().is_empty()) {
        detail = Some(match detail {
            Some(d) if !d.trim().is_empty() => format!("{d} · {p}"),
            _ => p,
        });
    }
    let options = if o.has("option") { o.all("option") } else { input.options };
    let timeout = match o.num("timeout")? {
        Some(t) => Some(t.clamp(1.0, 3600.0) as u32),
        None => input.timeout_secs.map(|t| t.clamp(1, 3600)),
    };
    // The island answers `timeout` itself; this is only the safety net if it never does.
    let wait = Duration::from_secs(timeout.unwrap_or(120) as u64 + 15);
    let kind = o.get("kind").or(input.kind).map(|k| k.trim().to_ascii_lowercase()).filter(|k| !k.is_empty());
    if kind.as_deref().is_some_and(|k| k != "shell") {
        return Err("--kind must be: shell".into());
    }
    let agent = o.get("agent").or(input.agent).map(|a| agent_name(&a)).transpose()?;
    Ok((Request::Ask { title, detail, command: o.get("command").or(input.command), options, timeout_secs: timeout, kind, agent }, Some(wait)))
}

/// `--agent` to the name on the wire.
fn agent_name(a: &str) -> Result<String, String> {
    agent_hook::Agent::parse(a).map(|a| a.slug().to_string()).ok_or_else(|| format!("unknown --agent {a:?} (claude, codex, gemini)"))
}

/// `mokuctl session --stdin` input. Unknown fields are ignored.
fn parse_session_input(raw: &[u8]) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let s = String::from_utf8_lossy(raw);
    let s = s.trim_start_matches('\u{feff}').trim();
    if s.is_empty() {
        return Ok(Default::default());
    }
    let v: serde_json::Value = serde_json::from_str(s).map_err(|e| format!("--stdin: not JSON: {e}"))?;
    v.as_object().cloned().ok_or_else(|| "--stdin: expected a JSON object".to_string())
}

fn build_session(o: &Opts, input: serde_json::Map<String, serde_json::Value>, now_ms: u64) -> Result<Request, String> {
    let text = |k: &str| o.get(k).or_else(|| input.get(k).and_then(|x| x.as_str()).map(str::to_string)).filter(|v| !v.trim().is_empty());
    let id = text("id").ok_or("missing --id")?;
    let state = text("state").ok_or("missing --state")?;
    let state = SessionState::parse(&state).ok_or_else(|| format!("unknown --state {state:?} (start, working, active, needs-input, done, failed, subagent, end)"))?;
    let ts = match o.get("ts") {
        Some(t) => Some(t.trim().parse::<u64>().map_err(|_| "--ts must be ms since 1970 (a whole number)")?),
        None => input.get("ts").and_then(|x| x.as_u64()),
    };
    // Claude Code is the default agent: it is not named on the wire.
    let agent = text("agent").map(|a| agent_name(&a)).transpose()?.filter(|a| a != "claude");
    // Without a timestamp the island cannot order events; "now" is what a script means.
    Ok(Request::Session { id, project: text("project"), state, detail: text("detail"), ts: Some(ts.unwrap_or(now_ms)), agent })
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn build(args: &[String], stdin: &mut dyn FnMut() -> Vec<u8>) -> Result<(Request, Option<Duration>), String> {
    let Some(cmd) = args.first() else { return Err("missing command".into()) };
    let o = Opts::parse(&args[1..])?;
    let quick = Some(Duration::from_secs(10));
    Ok(match cmd.as_str() {
        "ping" => (Request::Ping, quick),
        "notify" => (
            Request::Notify {
                title: o.need("title")?,
                body: o.get("body"),
                icon: o.get("icon"),
                id: o.get("id"),
                progress: o.get("progress").map(|p| parse_fraction(&p).map_err(|e| format!("--progress: {e}"))).transpose()?,
            },
            quick,
        ),
        "progress" => {
            let v = o.get("value").ok_or("missing --value")?;
            let v = parse_fraction(&v).map_err(|e| format!("--value: {e}"))?;
            (Request::Notify { title: o.need("title")?, body: o.get("body"), icon: o.get("icon").or(Some("download".into())), id: o.get("id"), progress: Some(v) }, quick)
        }
        "ask" => {
            let input = if o.has("stdin") { parse_ask_input(&stdin())? } else { AskInput::default() };
            build_ask(&o, input)?
        }
        "session" => {
            let input = if o.has("stdin") { parse_session_input(&stdin())? } else { Default::default() };
            (build_session(&o, input, now_ms())?, quick)
        }
        "todo" => match o.pos.first().map(String::as_str) {
            Some("add") if o.pos.len() > 1 => (Request::Todo { add: o.pos[1..].join(" ") }, quick),
            _ => return Err("usage: mokuctl todo add <text>".into()),
        },
        "debug" => {
            let p = &o.pos;
            let arg = |i: usize, what: &str| p.get(i).cloned().ok_or_else(|| format!("missing {what}"));
            let op = match p.first().map(String::as_str) {
                Some("open-tab") => DebugOp::OpenTab { id: arg(1, "tab id")? },
                Some("close") => DebugOp::Close,
                Some("state") => DebugOp::State,
                Some("click") => DebugOp::Click { action: arg(1, "action id")? },
                Some("screenshot-ready") => DebugOp::ScreenshotReady,
                Some("activity") => DebugOp::Activity {
                    id: arg(1, "id")?,
                    priority: arg(2, "priority")?.parse().map_err(|_| "priority must be 0-255")?,
                    label: arg(3, "label")?,
                    secs: o.num("secs")?,
                },
                Some("clear-activity") => DebugOp::ClearActivity { id: arg(1, "id")? },
                Some("type") => DebugOp::Type { text: p[1..].join(" "), compose: o.has("compose") },
                Some("key") => DebugOp::Key { key: arg(1, "key")? },
                Some("cursor") => {
                    // `cursor real` = back to the real pointer; else `cursor <x> <y> [--dt <ms>] [--down]`.
                    let int = |i: usize, what: &str| arg(i, what).and_then(|v| v.trim().parse::<i32>().map_err(|_| format!("{what} must be a whole number")));
                    if p.get(1).is_some_and(|v| v == "real") {
                        DebugOp::Cursor { x: None, y: None, dt: None, down: false }
                    } else {
                        let dt = o.num("dt")?.map(|v| v.clamp(0.0, 600_000.0) as u32);
                        DebugOp::Cursor { x: Some(int(1, "x")?), y: Some(int(2, "y")?), dt, down: o.has("down") }
                    }
                }
                _ => return Err("unknown debug op".into()),
            };
            (Request::Debug(op), Some(Duration::from_secs(15)))
        }
        other => return Err(format!("unknown command {other:?}")),
    })
}

/// Exit code for an `ask` answer: approve-like → 0, deny-like → 1, timeout → 2.
fn ask_exit(choice: &str, options: &[String]) -> u8 {
    let c = choice.to_lowercase();
    if c == "timeout" {
        return 2;
    }
    if ["approve", "allow", "yes", "setujui", "izinkan"].contains(&c.as_str()) {
        return 0;
    }
    if ["deny", "no", "tolak", "dismissed"].contains(&c.as_str()) {
        return 1;
    }
    // Custom options: the last one is the approving one.
    if options.last().is_some_and(|o| o.eq_ignore_ascii_case(choice)) { 0 } else { 1 }
}

/// What to print and the exit code for a response.
fn render(req: &Request, resp: &Response, json: bool) -> (Option<String>, Option<String>, u8) {
    let options = match req {
        Request::Ask { options, .. } => options.as_slice(),
        _ => &[],
    };
    let code = match resp {
        Response::Error { .. } => 1,
        Response::Answer { choice } => ask_exit(choice, options),
        _ => 0,
    };
    if let Response::State { state } = resp {
        return (Some(serde_json::to_string_pretty(state).unwrap_or_default()), None, code);
    }
    if json || matches!(req, Request::Debug(_)) {
        return (Some(ipc::encode(resp).trim_end().to_string()), None, code);
    }
    match resp {
        Response::Ok => (None, None, code),
        Response::Pong { version } => (Some(format!("Moku {version} is running")), None, code),
        Response::Answer { choice } => (Some(choice.clone()), None, code),
        Response::Error { message } => (None, Some(format!("mokuctl: {message}")), code),
        Response::State { .. } => (None, None, code),
    }
}

/// Output for a failure to talk to the island at all.
fn render_ipc_error(req: &Request, e: &IpcError, json: bool) -> (Option<String>, Option<String>, u8) {
    let (message, code) = match e {
        IpcError::NotRunning => ("Moku is not running (start moku.exe, or check that IPC is enabled in Moku settings)".to_string(), 3),
        // mokuctl's own safety net for `ask`: the island normally answers "timeout" itself.
        IpcError::Timeout if matches!(req, Request::Ask { .. }) => {
            let resp = Response::Answer { choice: "timeout".into() };
            return render(req, &resp, json);
        }
        IpcError::Timeout => ("Moku did not answer in time".to_string(), 1),
        e => (e.to_string(), 1),
    };
    if json {
        (Some(ipc::encode(&Response::Error { message }).trim_end().to_string()), None, code)
    } else {
        (None, Some(format!("mokuctl: {message}")), code)
    }
}

fn read_stdin() -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = std::io::stdin().take(ipc::MAX_LINE as u64).read_to_end(&mut buf);
    buf
}

/// `mokuctl agent-hook`: never in the agent's way. Exit 0 whatever happens; stdout is only what
/// the agent expects (a decision the user gave, or `{}` for Gemini CLI). A usage mistake is the
/// one thing said on stderr (the hook is then simply not working, and the agent goes on).
fn agent_hook_command(args: &[String]) -> ExitCode {
    let o = match Opts::parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("mokuctl agent-hook: {e}");
            return ExitCode::SUCCESS;
        }
    };
    let Some(agent) = o.get("agent").and_then(|a| agent_hook::Agent::parse(&a)) else {
        eprintln!("mokuctl agent-hook: --agent claude|codex|gemini is required");
        return ExitCode::SUCCESS;
    };
    let lang = if o.get("lang").is_some_and(|l| l.eq_ignore_ascii_case("en")) { agent_hook::Lang::En } else { agent_hook::Lang::Id };
    let timeout_secs = o.num("timeout").ok().flatten().map(|t| t.clamp(1.0, 3600.0) as u32).unwrap_or(120);
    let opts = agent_hook::Options { print: o.has("print"), lang, timeout_secs };
    // Codex `notify` passes the JSON as the one argument; then stdin is never read (it may be a
    // terminal, and reading it would hang the agent).
    let raw = match o.pos.first() {
        Some(arg) if arg.trim_start().starts_with('{') => arg.clone().into_bytes(),
        _ => read_stdin(),
    };
    let dir = std::env::var("CLAUDE_PROJECT_DIR").ok();
    let dir = dir.as_deref().filter(|_| agent == agent_hook::Agent::Claude);
    if let Some(out) = agent_hook::run(agent, &raw, &opts, &ipc::pipe_name(), dir, now_ms()) {
        print!("{out}");
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::from(if args.is_empty() { 64 } else { 0 });
    }
    if args[0] == "pipe-name" {
        println!("{}", ipc::pipe_name());
        return ExitCode::SUCCESS;
    }
    if args[0] == "claude-hook" {
        // Never in Claude Code's way: no output, exit 0, whatever happens.
        let print = args.iter().any(|a| a == "--print");
        claude_hook::run(&read_stdin(), print, &ipc::pipe_name());
        return ExitCode::SUCCESS;
    }
    if args[0] == "agent-hook" {
        return agent_hook_command(&args[1..]);
    }
    let json = args.iter().any(|a| a == "--json");
    let (req, wait) = match build(&args, &mut read_stdin) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mokuctl: {e}\n\n{USAGE}");
            return ExitCode::from(64);
        }
    };
    let (out, err, code) = match ipc::send_to(&ipc::pipe_name(), &req, wait) {
        Ok(resp) => render(&req, &resp, json),
        Err(e) => render_ipc_error(&req, &e, json),
    };
    if let Some(o) = out {
        println!("{o}");
    }
    if let Some(e) = err {
        eprintln!("{e}");
    }
    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn b(v: &[&str]) -> Result<(Request, Option<Duration>), String> {
        build(&s(v), &mut Vec::new)
    }

    #[test]
    fn parses_every_subcommand() {
        assert_eq!(b(&["ping"]).unwrap().0, Request::Ping);
        assert!(matches!(b(&["notify", "--title", "Tes", "--body", "Halo"]).unwrap().0, Request::Notify { ref title, .. } if title == "Tes"));
        assert!(matches!(b(&["progress", "--title", "x", "--value", "0.5"]).unwrap().0, Request::Notify { progress: Some(p), .. } if p == 0.5));
        assert!(matches!(b(&["progress", "--title", "x", "--value", "64"]).unwrap().0, Request::Notify { progress: Some(p), .. } if (p - 0.64).abs() < 1e-6));
        assert!(matches!(b(&["progress", "--title", "x", "--value", "30%"]).unwrap().0, Request::Notify { progress: Some(p), .. } if (p - 0.3).abs() < 1e-6));
        assert!(matches!(b(&["ask", "--title", "x", "--option", "Tolak", "--option", "Setujui"]).unwrap().0, Request::Ask { ref options, .. } if options.len() == 2));
        assert_eq!(b(&["todo", "add", "beli", "kopi"]).unwrap().0, Request::Todo { add: "beli kopi".into() });
        assert_eq!(b(&["debug", "open-tab", "today"]).unwrap().0, Request::Debug(DebugOp::OpenTab { id: "today".into() }));
        assert_eq!(b(&["debug", "click", "tab.media", "--json"]).unwrap().0, Request::Debug(DebugOp::Click { action: "tab.media".into() }));
        assert!(matches!(b(&["debug", "activity", "t", "50", "Timer", "--secs", "2"]).unwrap().0, Request::Debug(DebugOp::Activity { priority: 50, .. })));
        assert!(b(&["notify"]).is_err());
        assert!(b(&["notify", "--title", "  "]).is_err());
        assert!(b(&["notify", "--title", "x", "--progress", "abc"]).is_err());
        assert!(b(&["progress", "--title", "x", "--value", "NaN"]).is_err());
        assert!(b(&["debug", "nope"]).is_err());
        assert_eq!(b(&["debug", "type", "timer", "10m"]).unwrap().0, Request::Debug(DebugOp::Type { text: "timer 10m".into(), compose: false }));
        assert_eq!(b(&["debug", "type", "x", "--compose"]).unwrap().0, Request::Debug(DebugOp::Type { text: "x".into(), compose: true }));
        assert_eq!(b(&["debug", "key", "down"]).unwrap().0, Request::Debug(DebugOp::Key { key: "down".into() }));
        // A fake pointer: negative coordinates (a monitor left of / above the primary one), a virtual
        // time step and the button; `real` hands the pointer back.
        assert_eq!(b(&["debug", "cursor", "960", "-3", "--dt", "30", "--down"]).unwrap().0, Request::Debug(DebugOp::Cursor { x: Some(960), y: Some(-3), dt: Some(30), down: true }));
        assert_eq!(b(&["debug", "cursor", "5", "6"]).unwrap().0, Request::Debug(DebugOp::Cursor { x: Some(5), y: Some(6), dt: None, down: false }));
        assert_eq!(b(&["debug", "cursor", "real"]).unwrap().0, Request::Debug(DebugOp::Cursor { x: None, y: None, dt: None, down: false }));
        assert!(b(&["debug", "cursor", "5"]).is_err());
        assert!(b(&["debug", "cursor", "five", "6"]).is_err());
        assert!(b(&["ask", "--title"]).is_err());
    }

    #[test]
    fn ask_flags_and_stdin() {
        // --project joins the detail; --timeout is clamped; --json is a switch.
        let (r, wait) = b(&["ask", "--json", "--title", "Claude Code minta izin", "--detail", "Jalankan perintah", "--project", "toko-online", "--command", "npm test", "--timeout", "0"]).unwrap();
        assert_eq!(
            r,
            Request::Ask {
                title: "Claude Code minta izin".into(),
                detail: Some("Jalankan perintah · toko-online".into()),
                command: Some("npm test".into()),
                options: vec![],
                timeout_secs: Some(1),
                kind: None,
                agent: None
            }
        );
        assert_eq!(wait, Some(Duration::from_secs(16)));
        // --stdin: JSON (BOM tolerated, quotes intact); flags override.
        let raw = "\u{feff}{\"title\":\"T\",\"command\":\"echo \\\"hi\\\" && rm -rf x\",\"timeout_secs\":30,\"extra\":1}".as_bytes().to_vec();
        let (r, wait) = build(&s(&["ask", "--stdin", "--detail", "D"]), &mut || raw.clone()).unwrap();
        assert_eq!(r, Request::Ask { title: "T".into(), detail: Some("D".into()), command: Some("echo \"hi\" && rm -rf x".into()), options: vec![], timeout_secs: Some(30), kind: None, agent: None });
        assert_eq!(wait, Some(Duration::from_secs(45)));
        assert!(build(&s(&["ask", "--stdin"]), &mut || b"not json".to_vec()).is_err());
        assert!(build(&s(&["ask", "--stdin"]), &mut || b"{}".to_vec()).is_err(), "title required");
    }

    #[test]
    fn session_flags_and_stdin() {
        let (r, wait) = b(&["session", "--id", "abc", "--state", "needs-input", "--project", "toko-online", "--detail", "Izin Bash", "--ts", "42"]).unwrap();
        assert_eq!(r, Request::Session { id: "abc".into(), project: Some("toko-online".into()), state: SessionState::NeedsInput, detail: Some("Izin Bash".into()), ts: Some(42), agent: None });
        assert_eq!(wait, Some(Duration::from_secs(10)));
        // No --ts: stamped now.
        let before = now_ms();
        let Request::Session { ts: Some(ts), project: None, detail: None, .. } = b(&["session", "--id", "x", "--state", "done"]).unwrap().0 else { panic!("not a session") };
        assert!(ts >= before && ts <= now_ms());
        // --stdin JSON (quotes intact, BOM tolerated); flags override.
        let raw = "\u{feff}{\"id\":\"s1\",\"project\":\"a \\\"b\\\"\",\"state\":\"working\",\"ts\":7,\"x\":1}".as_bytes().to_vec();
        let (r, _) = build(&s(&["session", "--stdin", "--state", "active"]), &mut || raw.clone()).unwrap();
        assert_eq!(r, Request::Session { id: "s1".into(), project: Some("a \"b\"".into()), state: SessionState::Active, detail: None, ts: Some(7), agent: None });
        // Errors.
        assert!(b(&["session", "--state", "done"]).is_err(), "id required");
        assert!(b(&["session", "--id", "x"]).is_err(), "state required");
        assert!(b(&["session", "--id", "x", "--state", "sleeping"]).is_err());
        assert!(b(&["session", "--id", "x", "--state", "done", "--ts", "-5"]).is_err());
        assert!(build(&s(&["session", "--stdin"]), &mut || b"[1]".to_vec()).is_err());
    }

    #[test]
    fn ask_exit_codes() {
        assert_eq!(ask_exit("approve", &[]), 0);
        assert_eq!(ask_exit("deny", &[]), 1);
        assert_eq!(ask_exit("Setujui", &[]), 0);
        assert_eq!(ask_exit("Tolak", &[]), 1);
        assert_eq!(ask_exit("timeout", &[]), 2);
        assert_eq!(ask_exit("Run it", &s(&["Skip", "Run it"])), 0);
        assert_eq!(ask_exit("Skip", &s(&["Skip", "Run it"])), 1);
    }

    #[test]
    fn output_text_and_json() {
        let ask = b(&["ask", "--title", "x"]).unwrap().0;
        let ans = Response::Answer { choice: "approve".into() };
        assert_eq!(render(&ask, &ans, false), (Some("approve".into()), None, 0));
        assert_eq!(render(&ask, &ans, true), (Some("{\"type\":\"answer\",\"choice\":\"approve\"}".into()), None, 0));
        let err = Response::Error { message: "nope".into() };
        assert_eq!(render(&ask, &err, false), (None, Some("mokuctl: nope".into()), 1));
        assert_eq!(render(&Request::Ping, &Response::Pong { version: "0.1.0".into() }, false).0.as_deref(), Some("Moku 0.1.0 is running"));
        assert_eq!(render(&Request::Ping, &Response::Ok, false), (None, None, 0));
        // Debug is always JSON.
        let dbg = Request::Debug(DebugOp::Close);
        assert_eq!(render(&dbg, &Response::Ok, false).0.as_deref(), Some("{\"type\":\"ok\"}"));
        // Not running: exit 3, clear message; JSON on stdout with --json.
        let (o, e, c) = render_ipc_error(&Request::Ping, &IpcError::NotRunning, false);
        assert_eq!((o, c), (None, 3));
        assert!(e.unwrap().contains("Moku is not running"));
        let (o, _, c) = render_ipc_error(&ask, &IpcError::NotRunning, true);
        assert_eq!(c, 3);
        assert!(o.unwrap().starts_with("{\"type\":\"error\""));
        // mokuctl's own ask timeout → "timeout", exit 2.
        assert_eq!(render_ipc_error(&ask, &IpcError::Timeout, false), (Some("timeout".into()), None, 2));
    }
}
