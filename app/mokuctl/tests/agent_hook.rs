//! `mokuctl agent-hook` (Codex CLI, Gemini CLI, Claude Code), end to end with the real binary:
//! * the requests for the fixtures of each agent's documented hook input (`--print`);
//! * fail-open: exit 0, nothing (Gemini: `{}`) on stdout, and quick when Moku is not running;
//! * Codex's `notify` JSON as the one argument: stdin is never read (an agent may leave it open);
//! * a Codex `PermissionRequest` against a fake island that answers the card: the decision comes
//!   out in Codex's format, and the card says it is a shell command of agent `codex`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The tests start processes and time some of them: one at a time, so that a PowerShell fake
/// island of one test never slows the timing of another.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn isolated_pipe(tag: &str) -> String {
    format!(r"\\.\pipe\moku-test-agent-{tag}-{}", std::process::id())
}

fn scratch_data_dir() -> PathBuf {
    std::env::temp_dir().join(format!("moku-agent-hook-test-data-{}", std::process::id()))
}

fn fixture(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("agent-hook").join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn mokuctl() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mokuctl"));
    c.env_remove("CLAUDE_PROJECT_DIR").env("MOKU_DATA_DIR", scratch_data_dir()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    c
}

/// Run `mokuctl <args>` with `input` on stdin: (stdout, exit code, wall time).
fn native(args: &[&str], input: &str, pipe: &str) -> (String, i32, Duration) {
    let t = Instant::now();
    let mut child = mokuctl().args(args).env("MOKU_PIPE", pipe).spawn().expect("mokuctl");
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    let out = child.wait_with_output().unwrap();
    (String::from_utf8_lossy(&out.stdout).to_string(), out.status.code().unwrap_or(-1), t.elapsed())
}

fn json(line: &str) -> serde_json::Value {
    serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("not JSON: {e}: {line:?}"))
}

#[test]
fn codex_fixtures_become_session_requests() {
    let _serial = serial();
    let nobody = isolated_pipe("print");
    let cases = [
        ("codex-session-start.json", "start"),
        ("codex-user-prompt-submit.json", "working"),
        ("codex-pre-tool-use.json", "active"),
        ("codex-post-tool-use.json", "active"),
        ("codex-stop.json", "done"),
        ("codex-subagent-stop.json", "subagent"),
        ("codex-session-end.json", "end"),
        ("codex-interrupt.json", "failed"),
    ];
    for (file, state) in cases {
        let (out, code, _) = native(&["agent-hook", "--agent", "codex", "--print"], &fixture(file), &nobody);
        assert_eq!(code, 0, "{file}");
        let v = json(out.lines().next().unwrap_or(""));
        assert_eq!((v["type"].as_str(), v["state"].as_str(), v["agent"].as_str()), (Some("session"), Some(state), Some("codex")), "{file}: {out}");
        assert_eq!((v["id"].as_str(), v["project"].as_str()), (Some("019a-thread"), Some("toko-online")), "{file}");
        // What the user typed or the agent answered never travels.
        assert!(!out.contains("rahasia"), "{file}: {out}");
    }
    // An event that shows nothing.
    let (out, code, _) = native(&["agent-hook", "--agent", "codex", "--print"], &fixture("codex-pre-compact.json"), &nobody);
    assert_eq!((out.as_str(), code), ("", 0));
    // The permission request: a status plus the card.
    let (out, _, _) = native(&["agent-hook", "--agent", "codex", "--print"], &fixture("codex-permission-request.json"), &nobody);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert_eq!(json(lines[0])["state"], "needs_input");
    let ask = json(lines[1]);
    assert_eq!((ask["type"].as_str(), ask["kind"].as_str(), ask["agent"].as_str(), ask["command"].as_str()), (Some("ask"), Some("shell"), Some("codex"), Some("npm install express")));
    assert_eq!(ask["title"], "Codex minta izin");
    // English card text, other timeout.
    let (out, _, _) = native(&["agent-hook", "--agent", "codex", "--print", "--lang", "en", "--timeout", "30"], &fixture("codex-permission-request.json"), &nobody);
    let ask = json(out.lines().nth(1).unwrap());
    assert_eq!((ask["title"].as_str(), ask["timeout_secs"].as_u64()), (Some("Codex asks for permission"), Some(30)));
}

#[test]
fn gemini_fixtures_become_session_requests_and_never_a_card() {
    let _serial = serial();
    let nobody = isolated_pipe("print-gemini");
    let cases = [
        ("gemini-session-start.json", "start"),
        ("gemini-before-agent.json", "working"),
        ("gemini-before-tool.json", "active"),
        ("gemini-after-tool.json", "active"),
        ("gemini-notification.json", "needs_input"),
        ("gemini-after-agent.json", "done"),
        ("gemini-session-end.json", "end"),
    ];
    for (file, state) in cases {
        let (out, code, _) = native(&["agent-hook", "--agent", "gemini", "--print"], &fixture(file), &nobody);
        assert_eq!(code, 0, "{file}");
        assert_eq!(out.lines().count(), 1, "{file}: a status, never a card: {out}");
        let v = json(&out);
        assert_eq!((v["state"].as_str(), v["agent"].as_str(), v["project"].as_str(), v["id"].as_str()), (Some(state), Some("gemini"), Some("blog"), Some("gem-5f2a")), "{file}");
        assert!(!out.contains("rahasia"), "{file}: {out}");
    }
    assert_eq!(native(&["agent-hook", "--agent", "gemini", "--print"], &fixture("gemini-before-model.json"), &nobody).0, "");
}

/// Every event returns within 200 ms (1 s on a shared CI runner) when Moku is not running,
/// exits 0, and prints nothing (Gemini CLI wants JSON on stdout: `{}`).
#[test]
fn agent_hooks_fail_open_when_moku_is_down() {
    let _serial = serial();
    let limit = if std::env::var_os("CI").is_some() { Duration::from_millis(1000) } else { Duration::from_millis(200) };
    let _ = native(&["agent-hook", "--agent", "codex"], "{}", &isolated_pipe("warmup"));
    let down = isolated_pipe("down");
    let codex = ["codex-session-start.json", "codex-user-prompt-submit.json", "codex-pre-tool-use.json", "codex-post-tool-use.json", "codex-stop.json", "codex-interrupt.json", "codex-session-end.json"];
    let gemini = ["gemini-session-start.json", "gemini-before-agent.json", "gemini-before-tool.json", "gemini-notification.json", "gemini-after-agent.json", "gemini-session-end.json"];
    let mut worst = Duration::ZERO;
    for (agent, files, expect) in [("codex", &codex[..], ""), ("gemini", &gemini[..], "{}")] {
        for f in files {
            let (out, code, took) = native(&["agent-hook", "--agent", agent], &fixture(f), &down);
            assert_eq!((out.as_str(), code), (expect, 0), "{f}");
            assert!(took < limit, "{f} took {took:?} with Moku down (limit {limit:?})");
            worst = worst.max(took);
        }
        let (out, code, took) = native(&["agent-hook", "--agent", agent], "not json", &down);
        assert_eq!((out.as_str(), code), (expect, 0));
        assert!(took < limit, "garbage took {took:?}");
    }
    // Claude Code through the same command: unchanged (nothing printed).
    let (out, code, took) = native(&["agent-hook", "--agent", "claude"], r#"{"session_id":"s","hook_event_name":"Stop","cwd":"C:\\x\\blog"}"#, &down);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(took < limit, "claude took {took:?}");
    eprintln!("agent-hook, Moku down: slowest event {worst:?}");
    // A permission request with Moku down: no answer, so nothing printed and no wait (Codex asks
    // in its own prompt); it must not sit for the card's 120 s.
    let (out, code, took) = native(&["agent-hook", "--agent", "codex"], &fixture("codex-permission-request.json"), &down);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(took < Duration::from_secs(3), "permission request with Moku down took {took:?}");
}

/// Usage mistakes never fail the agent either.
#[test]
fn agent_hook_usage_mistakes_exit_zero() {
    let _serial = serial();
    let nobody = isolated_pipe("usage");
    for args in [&["agent-hook"][..], &["agent-hook", "--agent", "copilot"], &["agent-hook", "--agent"], &["agent-hook", "--bogus"]] {
        let (out, code, _) = native(args, "{}", &nobody);
        assert_eq!((out.as_str(), code), ("", 0), "{args:?}");
    }
}

/// Codex's `notify`: `["mokuctl", "agent-hook", "--agent", "codex"]`, and Codex appends the JSON
/// as the last argument. stdin must never be read: here it stays open and silent.
#[test]
fn codex_notify_json_argument_does_not_read_stdin() {
    let _serial = serial();
    let arg = fixture("codex-notify-agent-turn-complete.json");
    let mut child = mokuctl().args(["agent-hook", "--agent", "codex", "--print", &arg]).env("MOKU_PIPE", isolated_pipe("notify")).spawn().expect("mokuctl");
    let _stdin_kept_open = child.stdin.take().unwrap();
    let t = Instant::now();
    let out = loop {
        if child.try_wait().unwrap().is_some() {
            break child.wait_with_output().unwrap();
        }
        if t.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            panic!("agent-hook waited for stdin");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let v = json(&String::from_utf8_lossy(&out.stdout));
    assert_eq!((v["state"].as_str(), v["id"].as_str(), v["agent"].as_str(), v["project"].as_str()), (Some("done"), Some("019a-thread"), Some("codex"), Some("toko-online")));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("rahasia"));
}

/// A fake island: serves `count` connections on `name`, logs each request line, answers cards
/// with `choice` and everything else `ok`. Instances are created up front so that a client that
/// connects right after another never finds the pipe missing.
fn fake_island(name: &str, count: usize, choice: &str, log: &Path) -> Option<std::process::Child> {
    let script = format!(
        "$enc = New-Object Text.UTF8Encoding $false\r\n\
         $servers = 1..{count} | ForEach-Object {{ New-Object System.IO.Pipes.NamedPipeServerStream('{name}', [System.IO.Pipes.PipeDirection]::InOut, {count}, [System.IO.Pipes.PipeTransmissionMode]::Byte) }}\r\n\
         [Console]::Out.WriteLine('ready'); [Console]::Out.Flush()\r\n\
         foreach ($s in $servers) {{\r\n\
           $s.WaitForConnection()\r\n\
           $r = New-Object System.IO.StreamReader($s, $enc)\r\n\
           $line = $r.ReadLine()\r\n\
           [IO.File]::AppendAllText('{log}', $line + \"`n\", $enc)\r\n\
           if ($line -match '\"type\":\"ask\"') {{ $resp = '{{\"type\":\"answer\",\"choice\":\"{choice}\"}}' }} else {{ $resp = '{{\"type\":\"ok\"}}' }}\r\n\
           $w = New-Object System.IO.StreamWriter($s, $enc); $w.NewLine = \"`n\"; $w.WriteLine($resp); $w.Flush()\r\n\
           try {{ $s.WaitForPipeDrain() }} catch {{}}\r\n\
           $s.Dispose()\r\n\
         }}\r\n",
        log = log.display()
    );
    let file = std::env::temp_dir().join(format!("moku-fake-island-{name}.ps1"));
    std::fs::write(&file, script).ok()?;
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&file)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut()?).read_line(&mut line).ok()?;
    (line.trim() == "ready").then_some(child)
}

#[test]
fn codex_permission_decision_goes_back_in_codex_format() {
    let _serial = serial();
    for (choice, behavior) in [("approve", "allow"), ("deny", "deny")] {
        let name = format!("moku-test-agent-card-{choice}-{}", std::process::id());
        let log = std::env::temp_dir().join(format!("{name}.log"));
        let _ = std::fs::remove_file(&log);
        let Some(mut island) = fake_island(&name, 3, choice, &log) else {
            eprintln!("powershell.exe not available; skipped");
            return;
        };
        let pipe = format!(r"\\.\pipe\{name}");
        let (out, code, took) = native(&["agent-hook", "--agent", "codex", "--timeout", "30"], &fixture("codex-permission-request.json"), &pipe);
        let _ = island.kill();
        let _ = island.wait();
        assert_eq!(code, 0);
        let v = json(&out);
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PermissionRequest", "{out}");
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], behavior, "{out}");
        if behavior == "deny" {
            assert!(!v["hookSpecificOutput"]["decision"]["message"].as_str().unwrap_or("").is_empty());
        }
        // The island saw the card: a shell command of agent codex, asked for 30 s.
        let logged = std::fs::read_to_string(&log).unwrap_or_default();
        let ask = logged.lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap_or_default()).find(|v| v["type"] == "ask").unwrap_or_else(|| panic!("no ask in {logged:?}"));
        assert_eq!((ask["kind"].as_str(), ask["agent"].as_str(), ask["command"].as_str(), ask["timeout_secs"].as_u64()), (Some("shell"), Some("codex"), Some("npm install express"), Some(30)));
        let _ = std::fs::remove_file(&log);
        eprintln!("codex permission ({choice}) round trip: {took:?}");
    }
    // A card answered `timeout` (or an error): no decision printed, Codex asks the user itself.
    let name = format!("moku-test-agent-card-timeout-{}", std::process::id());
    let log = std::env::temp_dir().join(format!("{name}.log"));
    let Some(mut island) = fake_island(&name, 3, "timeout", &log) else { return };
    let (out, code, _) = native(&["agent-hook", "--agent", "codex", "--timeout", "30"], &fixture("codex-permission-request.json"), &format!(r"\\.\pipe\{name}"));
    let _ = island.kill();
    let _ = island.wait();
    assert_eq!((out.as_str(), code), ("", 0));
    let _ = std::fs::remove_file(&log);
}
