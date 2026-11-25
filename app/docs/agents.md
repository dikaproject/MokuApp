# Coding agents on the island: Claude Code, Codex CLI, Gemini CLI

Moku shows what a coding agent is doing as a pill per session (project name, "Bekerja…" with a
timer, "Butuh input", "Selesai"), lists the sessions in the Notifikasi tab, and, where the agent
allows it, shows its permission requests as a card you can answer on the island. One program does
the work for every agent: `mokuctl agent-hook --agent <claude|codex|gemini>`.

Formats and behaviour below are the agents' own documentation, read on 2026-11-25. Every input
format is also a fixture in `app\mokuctl\tests\fixtures\agent-hook\`, and the tests run the real
`mokuctl` against them. **None of the three agents was run against a real island for this work**:
the hooks are verified with the documented input and a fake island, not with a live Codex or
Gemini CLI session (see "What was and was not verified").

## What each agent supports

| | Claude Code | OpenAI Codex CLI | Gemini CLI |
|---|---|---|---|
| Session pills | **yes**, all events | **yes**, hooks | **yes**, hooks |
| Permission cards on the island | **yes** (`PreToolUse`, through `tools\claude-hook.ps1`) | **yes** (`PermissionRequest` hook: allow / deny) | **no**: see below |
| "Selalu izinkan perintah ini" | yes (Bash, PowerShell) | yes (Bash) | not applicable (no cards) |
| Mechanism | hooks in `settings.json` | hooks in `hooks.json` / `config.toml`; the older `notify` program | hooks in `settings.json` |

Why Gemini CLI gets no card: its `BeforeTool` hook can only *deny* ("Set to `deny` ... to prevent
the tool from executing"), the documentation does not say that `allow` skips the CLI's own
confirmation, and the `Notification` hook "cannot block alerts or grant permissions
automatically". A card whose **Setujui** did not actually approve would be worse than none, so
Gemini CLI is status only: its `Notification` (`ToolPermission`) turns the pill into "Butuh
input", and you answer in the terminal.

**Antigravity CLI** (Google's successor to Gemini CLI for individual users since 2026-06-18; Gemini
CLI stays for enterprise and API-key users) has hooks with another shape (camelCase input,
`PreToolUse` must print `{"decision": ...}`, `.agents/hooks.json`). It is **not implemented**: its
documentation is thin on exit codes and Windows, and a hook that must print a decision cannot be
the "never in the agent's way" kind. `mokuctl session` still works as a manual bridge for it.

## `mokuctl agent-hook`

```text
mokuctl agent-hook --agent claude|codex|gemini [--print] [--lang id|en] [--timeout <s>] [<json>]
```

Reads the hook input from stdin (Codex's `notify` passes it as the one argument: then stdin is
never read), sends the session status to the island, and for a Codex `PermissionRequest` shows the
card and prints Codex's decision.

* **Never in the agent's way.** Exit code 0 whatever happens. Nothing on stdout except a decision
  you gave on the island (Gemini CLI wants JSON on stdout, so it always gets `{}`). A status event
  gets the island 100 ms in total; with Moku stopped the hook returns in about 30 ms (measured by
  `cargo test -p mokuctl --test agent_hook`, limit 200 ms, run with Moku down). Only a permission
  card waits, up to `--timeout` (default 120 s); with Moku down or the card off it returns at once
  and prints nothing, so the agent asks in its own prompt.
* **Your words stay yours.** Prompts and replies in the hook input (`prompt`,
  `last-assistant-message`, `input-messages`, ...) are never copied into a request.
* `--print` shows the requests it would send (nothing is sent). `claude` is the same as
  `claude-hook` (status only; the card for Claude Code is the PowerShell script).
* Sessions of different agents never mix, even with equal ids. A session of Codex or Gemini CLI
  carries the agent's name as a small tag on the pill; Claude Code's pills look as before.
* Per agent switches: `[claude] enabled`, `[agents] codex`, `[agents] gemini` (Settings → Notifikasi).

## Claude Code

Unchanged: see `claude-code-hook.md`. Official docs: <https://code.claude.com/docs/en/hooks>.
New in v0.3: the permission card tells the island which agent asks and whether the command is a
shell command (`mokuctl ask --kind shell --agent claude`), which is what "always allow" needs.

## OpenAI Codex CLI

Official docs: hooks <https://developers.openai.com/codex/hooks> (now
<https://learn.chatgpt.com/docs/hooks>), `notify` and notifications
<https://developers.openai.com/codex/config-advanced>.

**Hooks (recommended).** `~/.codex/hooks.json` (Windows: `%USERPROFILE%\.codex\hooks.json`), a
trusted project's `.codex/hooks.json`, or `[hooks]` in `config.toml`. Events used: `SessionStart`,
`UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PermissionRequest`, `Stop`, `SubagentStop`,
`Interrupt`, `SessionEnd`. Input has `session_id`, `cwd`, `hook_event_name`, `turn_id`,
`tool_name`, `tool_input` (`Bash`: `tool_input.command`). The project name is the last folder of
`cwd`.

```json
{
  "hooks": {
    "SessionStart":     [ { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent codex", "timeout": 5 } ] } ],
    "UserPromptSubmit": [ { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent codex", "timeout": 5 } ] } ],
    "PostToolUse":      [ { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent codex", "timeout": 5 } ] } ],
    "PermissionRequest": [
      { "matcher": "*", "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent codex", "timeout": 180, "statusMessage": "Menunggu izin di Moku" } ] }
    ],
    "Stop":             [ { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent codex", "timeout": 5 } ] } ],
    "Interrupt":        [ { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent codex", "timeout": 5 } ] } ],
    "SessionEnd":       [ { "hooks": [ { "type": "command", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent codex", "timeout": 5 } ] } ]
  }
}
```

* `timeout` is in **seconds** (Codex's default is 600). The `PermissionRequest` timeout must be
  longer than the card's wait (120 s), or Codex gives up on the hook before you answered.
* Codex only runs hooks you reviewed and trusted: open `/hooks` in Codex and trust them.
* The documentation shows `command` as one string and offers `commandWindows` to override it on
  Windows; how Codex splits that string on Windows is not documented. If your Codex version runs it
  through a shell that needs it, quote the path (`"\"C:\\...\\mokuctl.exe\" agent-hook ..."`).
* The card: "Codex minta izin · Jalankan perintah · <project>", the command in the monospace box,
  **Tolak** / **Setujui**. **Setujui** prints `{"hookSpecificOutput":{"hookEventName":
  "PermissionRequest","decision":{"behavior":"allow"}}}`, **Tolak** the same with `deny` and a
  `message`. No answer in time, Moku down, cards off: nothing is printed and Codex shows its own
  prompt. `ask` is not supported by Codex's `PreToolUse`, which is why the card hangs on
  `PermissionRequest`. Other tools (`apply_patch`, MCP tools) get a card too (the patch or the
  input as JSON), but never an "always allow" rule.
* `Interrupt` (you pressed Esc) shows "Berhenti · dihentikan".

**`notify` (older, one event).** In `~/.codex/config.toml`:

```toml
notify = ["C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe", "agent-hook", "--agent", "codex"]
```

Codex runs it with one JSON argument for `agent-turn-complete` (`thread-id`, `turn-id`, `cwd`,
`input-messages`, `last-assistant-message`). That gives the "Selesai" pill and nothing else. If you
use hooks, do not also set `notify`: the two may name the same session differently.

## Gemini CLI

Official docs: <https://geminicli.com/docs/hooks/> and the reference
<https://geminicli.com/docs/hooks/reference/>. Replacement notice for individual users:
<https://developers.googleblog.com/an-important-update-transitioning-gemini-cli-to-antigravity-cli/>.

`~/.gemini/settings.json` (or the project's `.gemini/settings.json`), `hooks` object. Events used:
`SessionStart`, `BeforeAgent`, `BeforeTool`, `AfterTool`, `Notification`, `AfterAgent`,
`SessionEnd`. Input has `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `timestamp`;
`Notification` adds `notification_type` (`ToolPermission`), `message`, `details`.

```json
{
  "hooks": {
    "SessionStart": [ { "matcher": "*", "hooks": [ { "type": "command", "name": "moku", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent gemini", "timeout": 5000 } ] } ],
    "BeforeAgent":  [ { "matcher": "*", "hooks": [ { "type": "command", "name": "moku", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent gemini", "timeout": 5000 } ] } ],
    "AfterTool":    [ { "matcher": "*", "hooks": [ { "type": "command", "name": "moku", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent gemini", "timeout": 5000 } ] } ],
    "Notification": [ { "matcher": "*", "hooks": [ { "type": "command", "name": "moku", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent gemini", "timeout": 5000 } ] } ],
    "AfterAgent":   [ { "matcher": "*", "hooks": [ { "type": "command", "name": "moku", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent gemini", "timeout": 5000 } ] } ],
    "SessionEnd":   [ { "matcher": "*", "hooks": [ { "type": "command", "name": "moku", "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Moku\\mokuctl.exe agent-hook --agent gemini", "timeout": 5000 } ] } ]
  }
}
```

* `timeout` is in **milliseconds** (default 60000). Hooks must print nothing but JSON on stdout:
  `agent-hook --agent gemini` prints `{}`.
* `BeforeTool` is left out on purpose: it fires for every tool call and Moku has nothing to say
  at that moment (`AfterTool` already marks the session as busy). `BeforeAgent` (your prompt
  reached the agent, its text is not read) starts the "Bekerja…" timer, `AfterAgent` ends it.
* Whether your Gemini CLI version needs a switch to run hooks (older ones had an experimental
  flag) is not stated on the pages above: check `/hooks` in Gemini CLI.

## Always allow this command

**Off by default** (`[agents] always_allow = false`, Settings → Notifikasi → "Aturan Selalu izinkan
perintah ini"). With it on, a shell-command card from Claude Code or Codex shows **Selalu izinkan
perintah ini** under Tolak / Setujui, with the exact command it would remember ("Berlaku hanya untuk
perintah yang sama persis dari agen ini"). Pressing it approves this command and saves a rule. The next
ask from the same agent for **the same command** is approved at once, without a card (a short
"Diizinkan otomatis" pill says so).

* **Exactly that command, nothing more.** The rule is the whole command the card showed, with the
  spaces between words normalised (inside a quoted string they stay). `npm test` allows `npm test`
  and not `npm test --script-shell=./x.cmd`, `npm test -- --watch`, `npm testing`, `npx npm test`,
  `sudo npm test` or `NPM TEST`; `git grep foo` does not allow `git grep foo --open-files-in-pager=calc`.
  An argument is where the danger is (a flag can name a program to run), so no rule covers other
  arguments.
* **Only plain commands.** A command line with `;` `&` `|` `<` `>` `` ` `` `$` `(` `)` `{` `}` `%` `^` `!`, a line
  break, or a control or invisible character never matches any rule, however the rule reads:
  `npm test` does **not** allow `npm test && rm -rf /`. You then see the card as usual.
* **What gets a rule.** One plain command line of at most 200 characters whose program is given by
  name (not a path). Never a shell or interpreter (`bash`, `cmd`, `powershell`, `python`, `node`,
  `npx` ...), a downloader (`curl`, `iwr`), a program that runs other programs from its arguments
  (`find`, `make`, `sed`, `awk`, `tar`, `env`, `xargs`, `start`, `ii`, `explorer`, `forfiles`,
  `rundll32`, `regsvr32`, `mshta`, `wscript`, `cscript`, `msiexec` ...), or a destructive or system
  command (`rm`, `del`, `mv`, `reg`, `ssh` ...), and never `npm exec` / `pnpm dlx` / `yarn dlx`. The
  button is not offered for those, and a hand-edited rule like that is dropped when the file is read.
* **Optional prefix rules** exist only by editing `agent-rules.json` (`"mode": "prefix"`, a program and
  at least one subcommand: `git grep`). A prefix rule also allows extra words, but only plain
  positional ones: an extra word that starts with `-` or contains `=` `/` `\` `:` `@` `~` `*` `?` or a
  quote makes the command not match. The settings app lists them; it does not create them.
* **Per agent.** A rule made on a Codex card does not allow Claude Code's commands.
* **Only what you listed.** With no rules nothing is ever auto-approved; a card that does not say
  it is a shell command (`kind`), a file edit, a patch, an MCP tool, a client with its own buttons,
  gets no rule and no auto-approval.
* **A long command** that does not fit the card can get a rule only from the review view, after
  you scrolled to its last line (the same rule as **Setujui**).
* **Stored** in `%APPDATA%\Moku\agent-rules.json`, listed and removable in Settings → Notifikasi →
  "Selalu izinkan perintah" (you can also add one by hand, with the same checks). The island
  looks at the file at every card, so a removal is in force at once. "Hapus semua data" deletes it.
  `[agents] always_allow = false` (the default) switches every rule off and hides the button.
* **A rule Moku did not write does not act.** Moku keeps its own copy of what it wrote
  (`agent-rules.trusted.json`). A rule in `agent-rules.json` that is not in that copy (an agent or a
  program edited the file) is *pending*: the island shows the pill "Aturan izin diubah di luar
  Moku" (once per change, checked at every card and every 30 s while the feature is on) and ignores
  the rule until you press **Percayai** (or **Buang**) in Settings → Notifikasi. Removing a rule in the
  file takes effect at once. This stops accidental and naive edits; a program running as you that
  knows this scheme can rewrite both files, which no file-based design can prevent.
* **Moku never writes to an agent's own settings** (Claude Code's `permissions.allow`, Codex's
  rules, Gemini's `tools.allowed`). The rules only act inside Moku, and Claude Code's own deny /
  ask rules still apply on top of an `allow` from the hook.
* **Windows resolves a bare program name from the current folder first** (`cmd.exe` does), so a
  repository that ships its own `git.cmd` changes what an approved `git status` runs. A rule is an
  agreement about the text of the command, not about the program it ends up starting.

## What was and was not verified

Verified (automated): the input of each agent from its documented format (`mokuctl` unit tests and
`tests\agent_hook.rs` with the real binary), every session state, no card for events that have none,
prompts and replies never copied, fail-open and the time with Moku down, Codex's `notify` argument
without reading stdin, the decision JSON of a Codex card against a fake island (approve, deny,
timeout), the matching rules above (exact command, extra arguments such as `--script-shell=`, `;` `&&` `|`,
invisible characters, the programs that can never get a rule, the pending state of an edited file),
the rules file and its checks, and the island's handling of sessions per agent.

Not verified: a live Codex CLI or Gemini CLI session. The exact way Codex splits `command` on
Windows, whether your Gemini CLI version runs hooks without a flag, and whether Codex trusts the
hook on first use are as documented, not observed.
