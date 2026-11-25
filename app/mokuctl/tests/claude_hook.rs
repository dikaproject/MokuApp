//! The Claude Code hook, end to end, without an island:
//! * `app/tools/claude-hook.ps1` run under Windows PowerShell with a fake `mokuctl` (a .ps1
//!   that records its arguments and stdin and prints a canned answer): the PreToolUse
//!   permission decision, and every other event handed to `mokuctl claude-hook` with nothing
//!   printed;
//! * the real `mokuctl claude-hook`: the session request for every event (`--print`), no output
//!   and exit 0 always, and the time it takes when Moku is not running or does not answer
//!   (must stay under 200 ms).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A pipe name only this test process uses (and nobody serves unless the test starts a server):
/// the tests never touch the pipe of a real island.
fn isolated_pipe(tag: &str) -> String {
    format!(r"\\.\pipe\moku-test-{tag}-{}", std::process::id())
}

/// A scratch data folder name of this test process (the island's pipe name derives from it).
fn scratch_data_dir() -> PathBuf {
    std::env::temp_dir().join(format!("moku-hook-test-data-{}", std::process::id()))
}

fn hook() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("tools").join("claude-hook.ps1")
}

/// System folders only (PowerShell needs them; no mokuctl there).
fn system_path() -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    format!(r"{root}\System32;{root};{root}\System32\WindowsPowerShell\v1.0")
}

struct Run {
    stdout: String,
    code: i32,
    /// What the fake mokuctl got on stdin (the ask JSON), if it was called.
    asked: Option<String>,
    /// The fake mokuctl's arguments, if it was called.
    args: Option<String>,
    took: Duration,
}

fn run(tag: &str, input: &str, fake_out: &str, fake_code: i32, use_fake: bool) -> Option<Run> {
    let dir = std::env::temp_dir().join(format!("moku-hook-test-{tag}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let fake = dir.join("mokuctl.ps1");
    let log = dir.join("asked.json");
    let args_log = dir.join("args.txt");
    let _ = std::fs::remove_file(&log);
    std::fs::write(
        &fake,
        "$data = @($input) -join \"`n\"\r\n\
         [IO.File]::WriteAllText($env:FAKE_LOG, $data, (New-Object Text.UTF8Encoding $false))\r\n\
         [IO.File]::WriteAllText($env:FAKE_ARGS, ($args -join ' '), (New-Object Text.UTF8Encoding $false))\r\n\
         if (($args -join ' ') -eq 'claude-hook') { Write-Output 'noise that must not reach Claude Code'; exit 0 }\r\n\
         if (($args -join ' ') -ne 'ask --stdin --json') { Write-Output 'bad args'; exit 64 }\r\n\
         Write-Output $env:FAKE_OUT\r\n\
         exit [int]$env:FAKE_CODE\r\n",
    )
    .ok()?;
    let ctl = if use_fake { fake.clone() } else { dir.join("missing").join("mokuctl.exe") };
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(hook())
        .arg("-Mokuctl")
        .arg(&ctl)
        .env("FAKE_LOG", &log)
        .env("FAKE_ARGS", &args_log)
        .env("FAKE_OUT", fake_out)
        .env("FAKE_CODE", fake_code.to_string())
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("MOKUCTL")
        // Never reach a real island: no mokuctl from PATH (cargo puts the freshly built one
        // there), and a pipe name and data folder of this test only for any mokuctl that still
        // runs (nothing serves that pipe).
        .env("PATH", system_path())
        .env("MOKU_PIPE", isolated_pipe(&format!("never-{tag}")))
        .env("MOKU_DATA_DIR", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?; // no PowerShell → test skipped
    let t = Instant::now();
    child.stdin.take()?.write_all(input.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    let took = t.elapsed();
    let asked = std::fs::read_to_string(&log).ok();
    let args = std::fs::read_to_string(&args_log).ok();
    let _ = std::fs::remove_dir_all(&dir);
    Some(Run { stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(), code: out.status.code().unwrap_or(-1), asked, args, took })
}

fn decision(r: &Run) -> (String, String) {
    let v: serde_json::Value = serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("not JSON: {e}: {:?}", r.stdout));
    let h = &v["hookSpecificOutput"];
    assert_eq!(h["hookEventName"], "PreToolUse", "{v}");
    (h["permissionDecision"].as_str().unwrap_or("").to_string(), h["permissionDecisionReason"].as_str().unwrap_or("").to_string())
}

const BASH: &str = r#"{"session_id":"s","cwd":"C:\\src\\toko-online","permission_mode":"default","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"npm install express && echo \"siap\"","description":"Install"},"tool_use_id":"t1"}"#;

#[test]
fn claude_hook_output_format() {
    // Approved → allow; the card got a readable title/detail and the command with its quotes.
    let Some(r) = run("approve", BASH, r#"{"type":"answer","choice":"approve"}"#, 0, true) else {
        eprintln!("powershell.exe not available; skipped");
        return;
    };
    assert_eq!(r.code, 0);
    assert_eq!(decision(&r).0, "allow");
    let asked: serde_json::Value = serde_json::from_str(r.asked.as_deref().unwrap()).unwrap();
    assert_eq!(asked["title"], "Claude Code minta izin");
    assert_eq!(asked["detail"], "Jalankan perintah \u{00B7} toko-online");
    assert_eq!(asked["command"], "npm install express && echo \"siap\"");
    assert_eq!(asked["timeout_secs"], 120);
    // v0.3: the card says which agent asks and that this is a shell command (the "always allow"
    // rules of the island apply to those only).
    assert_eq!((asked["agent"].as_str(), asked["kind"].as_str()), (Some("claude"), Some("shell")));

    // Denied → deny with a reason for Claude.
    let r = run("deny", BASH, r#"{"type":"answer","choice":"deny"}"#, 1, true).unwrap();
    let (d, why) = decision(&r);
    assert_eq!(d, "deny");
    assert!(!why.is_empty());

    // Timed out → ask (Claude Code's normal prompt).
    let r = run("timeout", BASH, r#"{"type":"answer","choice":"timeout"}"#, 2, true).unwrap();
    assert_eq!(decision(&r).0, "ask");

    // Moku not running (exit 3) or an island error → no output: normal permission flow.
    let r = run("down", BASH, r#"{"type":"error","message":"Moku is not running"}"#, 3, true).unwrap();
    assert_eq!((r.stdout.as_str(), r.code), ("", 0));
    let r = run("off", BASH, r#"{"type":"error","message":"AI permission cards are turned off"}"#, 1, true).unwrap();
    assert_eq!((r.stdout.as_str(), r.code), ("", 0), "an error is not a denial");
    // mokuctl not found → no output.
    let r = run("missing", BASH, "", 0, false).unwrap();
    assert_eq!((r.stdout.as_str(), r.code), ("", 0));

    // Modes that would not prompt anyway → Moku is not asked at all.
    let bypass = BASH.replace("\"default\"", "\"bypassPermissions\"");
    let r = run("bypass", &bypass, r#"{"type":"answer","choice":"approve"}"#, 0, true).unwrap();
    assert_eq!((r.stdout.as_str(), r.asked.is_none()), ("", true));
    let edit = r#"{"cwd":"C:\\src\\toko-online","permission_mode":"acceptEdits","hook_event_name":"PreToolUse","tool_name":"Edit","tool_input":{"file_path":"C:\\src\\toko-online\\src\\app.ts","old_string":"a","new_string":"b"}}"#;
    let r = run("accept", edit, r#"{"type":"answer","choice":"approve"}"#, 0, true).unwrap();
    assert!(r.stdout.is_empty() && r.asked.is_none());
    // Edit in default mode → path relative to the project.
    let r = run("edit", &edit.replace("acceptEdits", "default"), r#"{"type":"answer","choice":"approve"}"#, 0, true).unwrap();
    assert_eq!(decision(&r).0, "allow");
    let asked: serde_json::Value = serde_json::from_str(r.asked.as_deref().unwrap()).unwrap();
    assert_eq!(asked["detail"], "Ubah file \u{00B7} toko-online");
    assert_eq!(asked["command"], "src\\app.ts");
    // A file edit is not a shell command: never a candidate for an "always allow" rule.
    assert_eq!((asked["agent"].as_str(), asked["kind"].as_str()), (Some("claude"), None));

    // Garbage input → no output, exit 0.
    let r = run("garbage", "not json at all", "", 0, true).unwrap();
    assert_eq!((r.stdout.as_str(), r.code), ("", 0));
}

/// One hook input per Claude Code event, with the fields the docs list for it.
fn events() -> Vec<(&'static str, String)> {
    let common = r#""session_id":"9f1c2d","transcript_path":"C:\\Users\\u\\.claude\\projects\\p\\9f1c2d.jsonl","cwd":"C:\\src\\toko-online","permission_mode":"default""#;
    [
        ("SessionStart", r#""source":"startup","model":"claude-x""#),
        ("UserPromptSubmit", r#""prompt":"tambahkan tes untuk \"keranjang\"""#),
        ("PostToolUse", r#""tool_name":"Bash","tool_input":{"command":"npm test"},"tool_response":{"stdout":"ok"},"tool_use_id":"t1""#),
        ("PostToolUseFailure", r#""tool_name":"Bash","tool_input":{"command":"npm test"},"tool_use_id":"t1","error":"exit 1""#),
        ("Notification", r#""message":"Claude needs your permission to use Bash","notification_type":"permission_prompt""#),
        ("Notification", r#""message":"Claude is waiting for your input","notification_type":"idle_prompt""#),
        ("Stop", r#""stop_hook_active":false,"last_assistant_message":"Selesai.","background_tasks":[],"session_crons":[]"#),
        ("StopFailure", r#""error":"rate_limit","last_assistant_message":"API Error: Rate limit reached""#),
        ("SubagentStop", r#""stop_hook_active":false,"agent_id":"a1","agent_type":"Explore","agent_transcript_path":"x","last_assistant_message":"done""#),
        ("SessionEnd", r#""reason":"prompt_input_exit""#),
    ]
    .into_iter()
    .map(|(e, extra)| (e, format!("{{{common},\"hook_event_name\":\"{e}\",{extra}}}")))
    .collect()
}

#[test]
fn claude_hook_script_hands_status_events_to_mokuctl() {
    for (event, input) in events() {
        let Some(r) = run(&format!("ev-{event}"), &input, "", 0, true) else {
            eprintln!("powershell.exe not available; skipped");
            return;
        };
        // Nothing printed (UserPromptSubmit / SessionStart stdout would become Claude's
        // context), even though the fake mokuctl printed something; exit 0.
        assert_eq!((r.stdout.as_str(), r.code), ("", 0), "{event}");
        assert_eq!(r.args.as_deref(), Some("claude-hook"), "{event}");
        let got: serde_json::Value = serde_json::from_str(r.asked.as_deref().unwrap_or("")).unwrap_or_else(|e| panic!("{event}: stdin not passed on intact: {e}"));
        let sent: serde_json::Value = serde_json::from_str(&input).unwrap();
        assert_eq!(got, sent, "{event}: the hook input reaches mokuctl unchanged");
        eprintln!("script {event}: {:?}", r.took);
    }
    // mokuctl missing → still nothing, exit 0.
    let r = run("ev-missing", &events()[0].1, "", 0, false).unwrap();
    assert_eq!((r.stdout.as_str(), r.code, r.args), ("", 0, None));
}

fn mokuctl() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mokuctl"));
    // The data folder of a scratch island: even a mokuctl that was given no MOKU_PIPE could not
    // resolve to the user's own pipe.
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

#[test]
fn claude_hook_native_request_for_every_event() {
    let nobody = isolated_pipe("print");
    let expect = [
        ("SessionStart", Some(("start", None))),
        ("UserPromptSubmit", Some(("working", None))),
        ("PostToolUse", Some(("active", None))),
        ("PostToolUseFailure", Some(("active", None))),
        ("Notification", Some(("needs_input", Some("Claude needs your permission to use Bash")))),
        ("Notification", None), // idle_prompt
        ("Stop", Some(("done", None))),
        ("StopFailure", Some(("failed", Some("rate_limit")))),
        ("SubagentStop", Some(("subagent", None))),
        ("SessionEnd", Some(("end", None))),
    ];
    for ((event, input), (e2, want)) in events().into_iter().zip(expect) {
        assert_eq!(event, e2);
        let (out, code, _) = native(&["claude-hook", "--print"], &input, &nobody);
        assert_eq!(code, 0, "{event}");
        match want {
            None => assert_eq!(out, "", "{event}"),
            Some((state, detail)) => {
                let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{event}: {e}: {out:?}"));
                assert_eq!(v["type"], "session", "{v}");
                assert_eq!(v["id"], "9f1c2d", "{v}");
                assert_eq!(v["project"], "toko-online", "{v}");
                assert_eq!(v["state"], state, "{event}: {v}");
                assert_eq!(v["detail"].as_str(), detail, "{event}: {v}");
                assert!(v["ts"].as_u64().is_some_and(|t| t > 1_700_000_000_000), "{v}");
            }
        }
    }
}

/// Every non-ask event returns within 200 ms when Moku is not running or never answers, prints
/// nothing and exits 0.
#[test]
fn claude_hook_fails_open_within_200ms() {
    // 200 ms on a developer machine; a shared CI runner (cold disk, antivirus, noisy neighbours)
    // starts a process much more slowly, so there the wall-clock limit is 1 s. The hook's own
    // budget (BUDGET, 100 ms, asserted in-process in src/claude_hook.rs) is unchanged.
    let limit = if std::env::var_os("CI").is_some() { Duration::from_millis(1000) } else { Duration::from_millis(200) };
    // Warm-up: the first start of a fresh exe can include an antivirus scan.
    let _ = native(&["claude-hook"], "{}", &isolated_pipe("warmup"));

    // 1. No island at all.
    let down = isolated_pipe("down");
    let mut worst = Duration::ZERO;
    for (event, input) in events() {
        let (out, code, took) = native(&["claude-hook"], &input, &down);
        assert_eq!((out.as_str(), code), ("", 0), "{event}");
        assert!(took < limit, "{event} took {took:?} with Moku down (limit {limit:?})");
        worst = worst.max(took);
    }
    eprintln!("claude-hook, Moku down: slowest event {worst:?}");
    // Garbage input as well.
    let (out, code, took) = native(&["claude-hook"], "not json", &down);
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(took < limit, "garbage input took {took:?} (limit {limit:?})");

    // 2. An "island" that accepts the connection and never answers (hung UI thread).
    let name = format!("moku-test-hung-{}", std::process::id());
    let server = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "$s = New-Object System.IO.Pipes.NamedPipeServerStream('{name}', [System.IO.Pipes.PipeDirection]::InOut, 4); \
             [Console]::Out.WriteLine('ready'); [Console]::Out.Flush(); $s.WaitForConnection(); Start-Sleep -Seconds 20"
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut server) = server else {
        eprintln!("powershell.exe not available; hung-island case skipped");
        return;
    };
    let mut line = String::new();
    let _ = BufReader::new(server.stdout.take().unwrap()).read_line(&mut line);
    assert_eq!(line.trim(), "ready");
    let pipe = format!(r"\\.\pipe\{name}");
    let (out, code, took) = native(&["claude-hook"], &events()[1].1, &pipe);
    let _ = server.kill();
    let _ = server.wait();
    assert_eq!((out.as_str(), code), ("", 0));
    assert!(took < limit, "took {took:?} with a hung island (limit {limit:?})");
    eprintln!("claude-hook, hung island: {took:?}");
}

/// `mokuctl pipe-name` is what scripts use to find the island: it follows MOKU_PIPE, then
/// MOKU_DATA_DIR, then the user.
#[test]
fn mokuctl_pipe_name_follows_the_environment() {
    let name = |pipe: Option<&str>, dir: Option<&str>| {
        let mut c = mokuctl();
        c.arg("pipe-name").env_remove("MOKU_PIPE").env_remove("MOKU_DATA_DIR");
        if let Some(p) = pipe {
            c.env("MOKU_PIPE", p);
        }
        if let Some(d) = dir {
            c.env("MOKU_DATA_DIR", d);
        }
        let out = c.spawn().unwrap().wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0));
        (String::from_utf8_lossy(&out.stdout).trim().to_string(), String::from_utf8_lossy(&out.stderr).trim().to_string())
    };
    // 1. MOKU_PIPE wins.
    assert_eq!(name(Some(&isolated_pipe("named")), Some(r"E:\d")), (isolated_pipe("named"), String::new()));
    // 2. A data folder: its own pipe, the same for every spelling of the folder.
    let (a, e) = name(None, Some(r"D:\scratch\moku-data"));
    assert!(e.is_empty() && a.starts_with(r"\\.\pipe\moku-") && a.len() == r"\\.\pipe\moku-".len() + 12, "{a}");
    assert_eq!(a, name(None, Some(r"d:/scratch/moku-data/")).0);
    assert_ne!(a, name(None, Some(r"D:\scratch\other-data")).0);
    // 3. Nothing: one pipe per user (never the old shared name).
    let (user, _) = name(None, None);
    assert!(user.starts_with(r"\\.\pipe\moku-S-1-"), "{user}");
    // A MOKU_PIPE that is no pipe name is ignored, with a warning on stderr.
    let (n, e) = name(Some("moku-test"), None);
    assert_eq!(n, user);
    assert!(e.contains("MOKU_PIPE"), "{e:?}");
}

/// For the record (not a limit): the PowerShell script path when Moku is down. Most of it is
/// powershell.exe starting; the docs recommend mokuctl.exe directly for status events.
#[test]
fn claude_hook_script_path_timing_when_moku_is_down() {
    let ctl = PathBuf::from(env!("CARGO_BIN_EXE_mokuctl"));
    let input = &events()[6].1; // Stop
    let mut took = vec![];
    for _ in 0..3 {
        let t = Instant::now();
        let child = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(hook())
            .arg("-Mokuctl")
            .arg(&ctl)
            .env("MOKU_PIPE", isolated_pipe("script-down"))
            .env("MOKU_DATA_DIR", scratch_data_dir())
            .env("PATH", system_path())
            .env_remove("CLAUDE_PROJECT_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { return };
        let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
        let out = child.wait_with_output().unwrap();
        assert_eq!((String::from_utf8_lossy(&out.stdout).trim(), out.status.code()), ("", Some(0)));
        took.push(t.elapsed());
    }
    eprintln!("claude-hook.ps1 (Stop, Moku down): {took:?}");
    // Fail open in any case: far below Claude Code's default hook timeout.
    assert!(took.iter().all(|t| *t < Duration::from_secs(5)));
}
