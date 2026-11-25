# Claude Code → Moku: permission cards and live session status

Moku hooks into [Claude Code hooks](https://code.claude.com/docs/en/hooks) for two things:

* **Permission cards** (`PreToolUse`): Claude Code's permission request shows as an AI card on
  the island ("Claude Code minta izin · Jalankan perintah · toko-online", the command,
  **Tolak** / **Setujui**), so you can approve or deny without switching to the terminal.
* **Live status** (all other events): one pill per Claude Code session, labelled with the
  project folder's name:

  | Pill | When |
  |---|---|
  | ✦ toko-online  **Bekerja…** 2m 14s | from your prompt (`UserPromptSubmit`) until Claude stops; the time counts up once a second |
  | ✦ toko-online  **Butuh input** | Claude waits for you (`Notification`: a permission prompt or a question in the terminal); back to "Bekerja…" when the next tool runs |
  | ✓ toko-online  **Selesai · 3m 12s** | the turn ended (`Stop`), shown for 6 s |
  | ⚠ toko-online  **Berhenti** | the turn ended with an API error (`StopFailure`, e.g. a rate limit), 6 s |

  A pending permission card always wins over these pills. With several sessions, the second
  one shows as the small bubble next to the pill and the pill says "+N"; the **Notifikasi** tab
  lists every session (state, time, finished subagents) with a × to forget one. A session ends
  with `SessionEnd`, or after 30 minutes without any event (Claude Code was killed).

Two programs do the work:

* `mokuctl claude-hook` (native, **recommended for the status events**): reads the hook input
  from stdin and sends the session status. It prints nothing, always exits 0, and gives Moku
  100 ms in total: with Moku stopped it returns in about 10 ms, with a hung island in about
  120 ms (measured by `cargo test -p mokuctl --test claude_hook`).
* `app/tools/claude-hook.ps1`: the permission card for `PreToolUse`; every other event is handed
  unchanged to `mokuctl claude-hook`. It can be the only hook command if you prefer one entry
  point, but each run costs 0.3–0.5 s of PowerShell start-up, so mark the status events
  `"async": true` in that case (see below).

## Install

1. Install Moku (`moku-setup.exe`): `mokuctl.exe` and `tools\claude-hook.ps1` are in the
   install folder, `%LOCALAPPDATA%\Programs\Moku` (the script finds `mokuctl.exe` in the folder
   above it). From a source checkout, use `<repo>\.build\main\release\mokuctl.exe` (after
   `cargo build --release` in `app/`) and `<repo>\app\tools\claude-hook.ps1`; for the script,
   make `mokuctl.exe` findable any of: `PATH`, the environment variable `MOKUCTL`, `-Mokuctl <path>`
   in the hook command, or a copy next to it.
2. Make sure Moku runs with IPC on (`[ipc] enabled = true`, the default), AI permission cards
   on (`ai_cards = true`) for the cards, and `[claude] enabled = true` (the default; settings
   app → Notifikasi → Claude Code) for the status pills.
3. Add the hooks to a Claude Code settings file — your user settings `~/.claude/settings.json`
   (all projects), or `.claude/settings.json` / `.claude/settings.local.json` in one project.
   Merge them into an existing `"hooks"` object if there is one. Replace `<you>` (or the whole
   path) with where Moku is installed; in JSON, backslashes are doubled.

   **Status events go to `mokuctl.exe claude-hook` directly** (native, about 10 ms, never in
   Claude Code's way). The PowerShell script is only for `PreToolUse`, the permission cards.

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash|PowerShell|Edit|MultiEdit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "powershell.exe",
            "args": [
              "-NoProfile", "-ExecutionPolicy", "Bypass",
              "-File", "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\tools\\claude-hook.ps1",
              "-Mokuctl", "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe"
            ],
            "timeout": 180
          }
        ]
      }
    ],
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "args": ["claude-hook"], "timeout": 5 } ] }
    ],
    "UserPromptSubmit": [
      { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "args": ["claude-hook"], "timeout": 5 } ] }
    ],
    "PostToolUse": [
      { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "args": ["claude-hook"], "timeout": 5, "async": true } ] }
    ],
    "Notification": [
      { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "args": ["claude-hook"], "timeout": 5 } ] }
    ],
    "Stop": [
      { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "args": ["claude-hook"], "timeout": 5 } ] }
    ],
    "StopFailure": [
      { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "args": ["claude-hook"], "timeout": 5 } ] }
    ],
    "SubagentStop": [
      { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "args": ["claude-hook"], "timeout": 5 } ] }
    ],
    "SessionEnd": [
      { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "args": ["claude-hook"], "timeout": 5 } ] }
    ]
  }
}
```

* Only the `PreToolUse` entry has a `matcher`: it picks the tools that ask on the island (here
  shell commands and file edits; use `"Bash"` alone, or add `WebFetch` and MCP tool names
  `mcp__server__tool`). The status entries have none, so they fire for every occurrence
  (`UserPromptSubmit` and `Stop` ignore matchers anyway). `mokuctl` itself
  decides which notifications mean "waiting for you".
* `PostToolUse` is the only frequent event (after every tool call), so it runs `"async": true`:
  Claude never waits for it. Background hooks can reach Moku late or out of order; every event
  carries the time it happened and Moku ignores one older than what it already has, so a late
  "tool ran" never turns a finished session back into "Bekerja…".
* `timeout` (seconds) on `PreToolUse` must be longer than the card's wait (120 s by default,
  `-Timeout`), or Claude Code cancels the hook first (a cancelled hook does not block the tool;
  the normal prompt is used). The status hooks finish in well under a second; `timeout: 5` is a
  safety net. `SessionEnd` hooks share a 1.5 s budget by default, which `mokuctl` fits easily.
* Optional script parameters: `-Timeout <seconds>` (default 120) and `-Lang en` for English
  card text (default Indonesian).
* Older Claude Code versions without exec-form `args`: use shell form, e.g.
  `"command": "\"C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe\" claude-hook"` and
  `"command": "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\tools\\claude-hook.ps1\""`.
* **Only the script:** use the `PreToolUse` command above for every event instead of
  `mokuctl.exe`, and add `"async": true` to all status events (each run costs PowerShell's
  start-up time).

4. Restart Claude Code (or open `/hooks` to review them) and send a prompt: "Bekerja…" appears
   on the island; run something like `npm test` to see a permission card.

The hooks find the island by themselves: `mokuctl` talks to your own user's pipe (never a shared
name) and refuses a pipe whose server is run by another user. Neither the script nor the hook
entries name a pipe. Only for a test island started with its own `MOKU_DATA_DIR` (or `MOKU_PIPE`)
does the hook need the same variable in its environment.

## Always allow, and the other agents

A permission card for a shell command (Bash, PowerShell) also offers **Selalu izinkan perintah
ini** (off by default, Settings → Notifikasi): the command becomes a rule that approves that exact
command from Claude Code at once, with no card. A rule is the whole command (`npm test` allows
`npm test` only: not `npm test --watch`, not `npm test && rm -rf /`), is listed and removable in
Settings, and lives in Moku's own `agent-rules.json`: Moku never writes to Claude Code's settings. Details,
limits and the same cards for OpenAI Codex CLI (and status pills for Gemini CLI):
[`agents.md`](agents.md).

## What each event does

From the hook input (fields as documented on the hooks page, checked 2026-11-24): every event
has `session_id`, `cwd` and `hook_event_name`; `Notification` adds `message`, an optional
`title` and `notification_type`; `StopFailure` adds `error`. The project name is the last folder
of `CLAUDE_PROJECT_DIR` (set by Claude Code for hooks), else of `cwd`.

| Event | Session state | On the island |
|---|---|---|
| `SessionStart` (any `source`) | `start` | listed in Notifikasi ("Siap"), no pill |
| `UserPromptSubmit` | `working` | "Bekerja…", timer from now |
| `PreToolUse` (through the script: permission card), `PostToolUse`, `PostToolUseFailure` | `active` | after "Butuh input": back to "Bekerja…" (same timer); a session Moku never saw start: "Bekerja…" from now |
| `Notification` `permission_prompt`, `elicitation_dialog`, `elicitation_url_dialog`, `agent_needs_input` (or no type) | `needs_input` + message | "Butuh input" (priority 90), message in the list |
| `Notification` `elicitation_response`, `elicitation_complete` | `active` | as above |
| `Notification` `idle_prompt`, `auth_success`, others | — | nothing (the turn already ended) |
| `Stop` | `done` | "✓ Selesai · 3m 12s" for 6 s (not during focus mode), then "Selesai" in the list |
| `StopFailure` | `failed` + `error` | "Berhenti" for 6 s |
| `SubagentStop` | `subagent` | counted ("2 subagen selesai" in the list) |
| `SessionEnd` (any `reason`) | `end` | removed |
| anything else | — | nothing |

The status hooks never print anything: for `UserPromptSubmit` and `SessionStart`, Claude Code
would add stdout to Claude's context. Exit code 0 with no output means "no decision", so they
never block a prompt, a tool or a stop.

## Permission cards (`PreToolUse`)

| On the island | Hook prints | Claude Code |
|---|---|---|
| **Setujui** | `permissionDecision: "allow"` | runs the tool without its own prompt |
| **Tolak** | `permissionDecision: "deny"` (+ reason for Claude) | does not run the tool |
| no answer within 120 s | `permissionDecision: "ask"` | shows its normal prompt in the terminal |
| Moku not running, AI cards off, mokuctl not found, bad input | nothing (exit 0) | normal permission flow |

The output has the shape documented for `PreToolUse`:

```json
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","permissionDecisionReason":"Disetujui di Moku"}}
```

The hook always exits 0, so a problem in the hook never blocks a tool call. Claude Code's own
deny/ask rules still apply on top of what the hook returns.

* The card appears for **every** matched tool call, including ones your Claude Code allow
  rules would have approved silently. Narrow it with the `matcher` (and the hook's `if` field,
  e.g. `"if": "Bash(git push*)"`) if that is too much.
* The hook stays out of the way when Claude Code would not prompt anyway:
  `bypassPermissions`, `dontAsk` and `plan` modes, and file edits in `acceptEdits` mode.
* Several sessions asking at once queue on the island in order ("1 dari 3"). If a session is
  interrupted (the hook process ends), its card disappears.
* You can only approve what you could see in full. A command of up to three card lines is
  shown whole on the card. A longer one shows its start, and **Setujui…** opens a review view
  in the panel with the whole command (wrapped, 11 lines at a time; scroll with the wheel or
  ▲ ▼); its Setujui works only once you scrolled to the last line. Commands over 4000
  characters are not sent to Moku at all (Claude Code asks in the terminal). Nothing is
  shortened or elided; control and invisible characters show as `�`. File edits show the path
  relative to the project.
* Moku paused from the tray: the hook gets an error at once and Claude Code asks in the
  terminal. A full-screen app (with "hide in full screen" on) does not hide a pending card:
  the island stays visible until it is answered.
* The card's title and project come from the hook input: the project is the folder name of
  `CLAUDE_PROJECT_DIR` (or `cwd`).

## Check it without Claude Code

```powershell
# Status: what would be sent (nothing is sent with --print)
'{"session_id":"s1","cwd":"C:\\src\\toko-online","hook_event_name":"UserPromptSubmit","prompt":"hi"}' |
  <path>\mokuctl.exe claude-hook --print
# Status for real: a "Bekerja…" pill, then "Selesai"
mokuctl session --id s1 --project toko-online --state working
mokuctl session --id s1 --state done
# Permission card
'{"tool_name":"Bash","tool_input":{"command":"npm test"},"cwd":"C:\\src\\toko-online","permission_mode":"default","hook_event_name":"PreToolUse"}' |
  powershell -NoProfile -ExecutionPolicy Bypass -File app\tools\claude-hook.ps1 -Mokuctl <path>\mokuctl.exe
```

Click **Setujui** on the island → the script prints the `allow` JSON above. With Moku stopped
it prints nothing. Automated: `cargo test -p mokuctl --test claude_hook` runs the real script
with a fake `mokuctl` for every event, the real `mokuctl claude-hook` for every event, and
times it with Moku stopped and with an island that never answers.

## mokuctl session (for other agents and scripts)

```text
mokuctl session --id <id> --state <state> [--project <p>] [--detail <d>] [--ts <ms>] [--stdin]
```

`state`: `start`, `working`, `active`, `needs-input`, `done`, `failed`, `subagent`, `end` (as
above). `--ts` is when it happened in ms since 1970 (default: now); an event older than the
last one Moku applied to that session is ignored. `--stdin` reads
`{"id","project","state","detail","ts"}` as JSON. The pipe request is
`{"type":"session","id":"s1","project":"toko-online","state":"working","ts":1795000000000}`.

## mokuctl ask (for other agents and scripts)

```text
mokuctl ask --title <t> [--detail <d>] [--project <p>] [--command <c>] [--option <label>]...
            [--timeout <s>] [--stdin] [--json]
```

Prints `approve`, `deny`, the chosen `--option` label, or `timeout`; exit code 0 approved,
1 denied, 2 timed out, 3 Moku not running. With custom `--option`s the last one approves.
`--stdin` reads `{"title","detail","command","options","timeout_secs"}` as JSON from stdin —
use it for commands containing quotes, which Windows PowerShell 5.1 does not pass to native
programs intact. `--json` prints the island's response line (`{"type":"answer","choice":"approve"}`
or `{"type":"error",...}`), which tells a denial apart from an error.
