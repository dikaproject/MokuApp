<#
.SYNOPSIS
  Claude Code hook for Moku: permission cards (PreToolUse) and live session status (all other
  events).

.DESCRIPTION
  Reads the hook input JSON from stdin.

  Every event except PreToolUse (SessionStart, UserPromptSubmit, PostToolUse, Notification,
  Stop, StopFailure, SubagentStop, SessionEnd) is handed unchanged to `mokuctl claude-hook`,
  which updates the session pill ("Bekerja...", "Butuh input", "Selesai"). Nothing is printed
  (UserPromptSubmit / SessionStart stdout would become Claude's context) and the exit code is
  0. mokuctl gives up after 100 ms; most of this script's time is PowerShell's own start-up
  (0.3-0.5 s), so for these events prefer calling mokuctl.exe directly, or mark the hook
  "async": true (see app/docs/claude-code-hook.md).

  PreToolUse: shows an AI permission card with `mokuctl ask`, and prints the decision Claude
  Code expects:

    {"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"|"deny"|"ask",
                           "permissionDecisionReason":"..."}}

  Setujui (approve) -> "allow", Tolak (deny) -> "deny".
  No answer within -Timeout seconds -> "ask" (Claude Code shows its normal prompt).
  The card always shows the whole command (a long one in a scrollable review view; Setujui
  works only after scrolling to its end). Commands over 4000 characters are not sent to Moku.
  Moku paused from the tray -> error -> no output (normal prompt at once).
  Moku not running, mokuctl not found, AI cards off, unreadable input, or a permission mode
  that would not prompt anyway (bypassPermissions, dontAsk, plan; acceptEdits for file edits)
  -> no output, exit 0: Claude Code continues with its normal permission flow.
  Always exits 0, so a problem here never blocks a tool call.

  The pipe: this script never names a pipe. mokuctl finds the island's per-user pipe itself
  (and refuses a pipe whose server is not run by the same user); `MOKU_PIPE` / `MOKU_DATA_DIR`
  in the environment (tests, a scratch island) are passed through to it unchanged.

  Install: see app/docs/claude-code-hook.md. Test without Claude Code:
    '{"tool_name":"Bash","tool_input":{"command":"npm test"},"cwd":"C:\\src\\toko"}' |
      powershell -NoProfile -ExecutionPolicy Bypass -File claude-hook.ps1

.PARAMETER Mokuctl
  Path to mokuctl.exe. Default: $env:MOKUCTL, then mokuctl on PATH, then next to this script or in
  the folder above it.
.PARAMETER Timeout
  Seconds to wait for an answer (default 120). Keep the hook's "timeout" in settings.json larger.
.PARAMETER Lang
  "id" (default) or "en" for the card text.
#>
param(
    [string]$Mokuctl = "",
    [int]$Timeout = 120,
    [ValidateSet("id", "en")][string]$Lang = "id"
)
$ErrorActionPreference = "Stop"
$utf8 = New-Object System.Text.UTF8Encoding $false
try { [Console]::InputEncoding = $utf8 } catch {}
try { [Console]::OutputEncoding = $utf8 } catch {}
$OutputEncoding = $utf8

function T([string]$id, [string]$en) { if ($Lang -eq "en") { $en } else { $id } }

function Write-Decision([string]$decision, [string]$reason) {
    $o = @{ hookSpecificOutput = @{ hookEventName = "PreToolUse"; permissionDecision = $decision; permissionDecisionReason = $reason } }
    [Console]::Out.Write(($o | ConvertTo-Json -Compress -Depth 4))
    [Console]::Out.Flush()
    exit 0
}

function Find-Mokuctl {
    foreach ($c in @($Mokuctl, $env:MOKUCTL)) { if ($c -and (Test-Path -LiteralPath $c)) { return $c } }
    $cmd = Get-Command mokuctl -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($cmd) { return $cmd.Source }
    # Next to this script, or in the folder above it (an installed Moku: <dir>\tools\claude-hook.ps1,
    # <dir>\mokuctl.exe).
    foreach ($d in @($PSScriptRoot, (Split-Path -Parent $PSScriptRoot))) {
        $here = Join-Path $d "mokuctl.exe"
        if (Test-Path -LiteralPath $here) { return $here }
    }
    return $null
}

# ---- read the hook input -------------------------------------------------------------------
try { $raw = [Console]::In.ReadToEnd() } catch { exit 0 }

# ---- session status: every event but PreToolUse -----------------------------------------------
# Cheap check first: no JSON parsing (ConvertFrom-Json alone costs ~0.1 s) on this path.
$event = if ($raw -match '"hook_event_name"\s*:\s*"([A-Za-z]+)"') { $Matches[1] } else { "" }
if ($event -and $event -ne "PreToolUse") {
    $ctl = Find-Mokuctl
    if ($ctl) {
        try { $raw | & $ctl claude-hook 2>$null | Out-Null } catch {}
    }
    exit 0
}

try { $in = $raw | ConvertFrom-Json } catch { exit 0 }
if (-not $in -or -not $in.tool_name) { exit 0 }

$tool = [string]$in.tool_name
$ti = $in.tool_input
$mode = [string]$in.permission_mode
$fileTools = @("Edit", "MultiEdit", "Write", "NotebookEdit")
if ($mode -in @("bypassPermissions", "dontAsk", "plan")) { exit 0 }
if ($mode -eq "acceptEdits" -and $tool -in $fileTools) { exit 0 }

# ---- what the card says ----------------------------------------------------------------------
$projectDir = if ($env:CLAUDE_PROJECT_DIR) { $env:CLAUDE_PROJECT_DIR } elseif ($in.cwd) { [string]$in.cwd } else { "" }
$project = if ($projectDir) { Split-Path -Leaf ($projectDir.TrimEnd('\', '/')) } else { "" }

function Rel([string]$path) {
    if (-not $path) { return "" }
    if ($projectDir -and $path.StartsWith($projectDir, [StringComparison]::OrdinalIgnoreCase)) {
        $r = $path.Substring($projectDir.Length).TrimStart('\', '/')
        if ($r) { return $r }
    }
    return $path
}

switch -Regex ($tool) {
    "^Bash$" { $what = T "Jalankan perintah" "Run a command"; $command = [string]$ti.command }
    "^PowerShell$" { $what = T "Jalankan perintah PowerShell" "Run a PowerShell command"; $command = [string]$ti.command }
    "^(Edit|MultiEdit)$" { $what = T "Ubah file" "Edit a file"; $command = Rel ([string]$ti.file_path) }
    "^Write$" { $what = T "Tulis file" "Write a file"; $command = Rel ([string]$ti.file_path) }
    "^NotebookEdit$" { $what = T "Ubah notebook" "Edit a notebook"; $command = Rel ([string]$ti.notebook_path) }
    "^WebFetch$" { $what = T "Buka halaman web" "Fetch a web page"; $command = [string]$ti.url }
    "^WebSearch$" { $what = T "Cari di web" "Search the web"; $command = [string]$ti.query }
    default {
        $what = (T "Pakai alat " "Use tool ") + $tool
        $command = if ($ti) { $ti | ConvertTo-Json -Compress -Depth 20 } else { "" }
    }
}
# The card must show exactly what would be approved: never clip. Longer than the island can
# show for review -> no output, Claude Code asks in the terminal as usual.
if ($command.Length -gt 4000) { exit 0 }
$detail = if ($project) { "$what $([char]0x00B7) $project" } else { $what }
$ask = @{
    title        = T "Claude Code minta izin" "Claude Code asks for permission"
    detail       = $detail
    command      = $command
    timeout_secs = $Timeout
    # Which agent asks, and whether `command` is a shell command line (Bash / PowerShell): only
    # then the card offers "Selalu izinkan perintah ini" and applies the user's rules in Moku.
    agent        = "claude"
}
if ($tool -in @("Bash", "PowerShell")) { $ask.kind = "shell" }

# ---- ask Moku --------------------------------------------------------------------------------
$ctl = Find-Mokuctl
if (-not $ctl) { exit 0 }
try {
    $json = $ask | ConvertTo-Json -Compress
    $out = $json | & $ctl ask --stdin --json 2>$null
    $code = $LASTEXITCODE
} catch { exit 0 }
$resp = $null
try { $resp = ($out | Out-String).Trim() | ConvertFrom-Json } catch {}

if ($code -eq 3 -or -not $resp -or $resp.type -ne "answer") {
    # Not running, AI cards off, busy, ...: let Claude Code decide as usual.
    exit 0
}
switch ($code) {
    0 { Write-Decision "allow" (T "Disetujui di Moku" "Approved in Moku") }
    1 { Write-Decision "deny" (T "Pengguna menolak lewat Moku." "The user denied this in Moku.") }
    default { Write-Decision "ask" (T "Tidak dijawab di Moku dalam $Timeout detik." "No answer in Moku within $Timeout seconds.") }
}
