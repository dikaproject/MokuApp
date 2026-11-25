<#
.SYNOPSIS
  Scripted check of the "polite island" (v0.3, track O) against a RUNNING scratch island: anchor /
  offset, move aside (fade and slide), hide until the top edge, per-app hide rules, follow the cursor
  and a named monitor. Prints PASS / FAIL per check and exits 1 when something failed.

  Never sends input to any window and never moves the real cursor: the pointer is a FAKE one fed
  with `mokuctl debug cursor x y --dt ms` (the same code path as the real pointer, with a virtual
  clock), the foreground app is pretended with `placement.debug_fg`, and the second monitor is
  invented with `placement.debug_monitors`. It rewrites the scratch island's config.toml (and puts
  the old one back at the end).

  The island must run with a scratch MOKU_DATA_DIR and MOKU_DEBUG=1, and so must this script's
  environment (it refuses otherwise: it would reach your own island), e.g.

    $env:MOKU_DATA_DIR = "<repo>\.build\scratch-data"; $env:MOKU_DEBUG = "1"
    Start-Process "<bin>\moku.exe"
    .\polite-test.ps1
  Usage: .\polite-test.ps1 [-Bin <folder with mokuctl.exe>]
#>
param([string]$Bin = "")
$ErrorActionPreference = "Stop"
. "$PSScriptRoot\_common.ps1"
Assert-TestIsland
if (-not $env:MOKU_DATA_DIR) { throw "set `$env:MOKU_DATA_DIR (the scratch folder of the test island): this script rewrites its config.toml" }
if (-not $Bin) { $Bin = Get-DefaultBin }
$ctl = Join-Path $Bin "mokuctl.exe"
$cfgPath = Join-Path $env:MOKU_DATA_DIR "config.toml"
$saved = if (Test-Path $cfgPath) { Get-Content $cfgPath -Raw } else { $null }
$failed = 0
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class PoliteFg {
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
"@
# The process name of the real foreground window (read only: nothing is sent to it).
function ForegroundName {
    $h = [PoliteFg]::GetForegroundWindow(); $p = 0
    [void][PoliteFg]::GetWindowThreadProcessId($h, [ref]$p)
    if ($p) { (Get-Process -Id $p -ErrorAction SilentlyContinue).ProcessName }
}

function State { (& $ctl debug state | Out-String | ConvertFrom-Json) }
function Cursor([int]$x, [int]$y, [int]$dt = 16, [switch]$down) {
    $a = @("debug", "cursor", "$x", "$y", "--dt", "$dt"); if ($down) { $a += "--down" }
    (& $ctl @a | Out-String | ConvertFrom-Json)
}
function SetCfg([string]$extra) {
    "[onboarding]`ncompleted = true`n$extra" | Set-Content -Path $cfgPath -Encoding utf8
    Start-Sleep -Milliseconds 800   # the island reloads the file
}
function Check([string]$name, [bool]$ok, [string]$detail = "") {
    if ($ok) { "PASS  $name" } else { $script:failed++; "FAIL  $name  $detail" }
}
# A fast flick along the island's row, from the left: 1100 px/s, ending just before the island.
# Every path starts with a sample after a long rest, so the jump from the previous position is not
# taken for a movement (the history only keeps the last 140 ms).
function Flick($hm, [int]$fromDx = -130) {
    $row = [int](($hm[1] + $hm[3]) / 2)
    $null = Cursor ($hm[0] + $fromDx) $row 600
    foreach ($i in 1..3) { $null = Cursor ($hm[0] + $fromDx + 33 * $i) $row 30 }
}
function Leave { $null = Cursor 1500 700 100; $null = Cursor 1500 700 450 }

try {
    $null = & $ctl debug cursor real
    SetCfg ""
    $s = State
    Check "starts at home with the input window on" ($s.away -eq "home" -and -not $s.passthrough) "away=$($s.away) passthrough=$($s.passthrough)"
    $mon = $s.monitor.rect; $scale = $s.scale
    $monW = $mon[2] - $mon[0]; $monCx = ($mon[0] + $mon[2]) / 2
    $lim = $monW / 2 - 332 * $scale
    function CentreOf($st) { ($st.home[0] + $st.home[2]) / 2 }

    # ---- anchor and offset -----------------------------------------------------------------
    foreach ($c in @(@("center", 0, 0), @("left", 0, -$lim), @("right", 0, $lim), @("center", 100, 100 * $scale), @("left", 100, (-$lim + 100 * $scale)), @("right", 2000, $lim), @("left", -2000, -$lim))) {
        SetCfg "[island]`nanchor = `"$($c[0])`"`noffset_x = $($c[1])`n"
        $s = State
        $got = (CentreOf $s) - $monCx
        Check "anchor $($c[0]) offset $($c[1]): centre $([int]$got) px from the monitor's centre (want $([int]$c[2]))" ([Math]::Abs($got - $c[2]) -le 4) "got $got"
    }
    SetCfg ""

    # ---- move aside: fade ----------------------------------------------------------------------
    SetCfg "[island]`nmove_aside = `"fade`"`n"
    $s = State; $hm = $s.home
    Flick $hm
    $s = State
    Check "a fast flick through the island fades it" ($s.away -eq "fade") "away=$($s.away) verdict=$($s.placement.verdict)"
    Check "... and clicks go through (the input window is hidden)" ($s.passthrough -eq $true)
    $null = Cursor 1500 700 100
    $s = State
    Check "... it stays away just after the pointer left" ($s.away -eq "fade")
    $null = Cursor 1500 700 450
    $s = State
    Check "... and is back 450 ms after the pointer left the zone" ($s.away -eq "home" -and -not $s.passthrough) "away=$($s.away)"

    # Homing in: braking on the way, aimed at the middle.
    $cx = [int](($hm[0] + $hm[2]) / 2)
    $null = Cursor $cx 300 600
    foreach ($y in @(262, 228, 198, 172, 150, 132, 118)) { $null = Cursor $cx $y 30 }
    $s = State
    Check "a pointer that brakes on its way to the island does not dodge it" ($s.away -eq "home" -and $s.placement.verdict -eq "target") "away=$($s.away) verdict=$($s.placement.verdict)"
    Leave

    # Along the toolbar under the island (the address bar): graze.
    $y = $hm[3] + 6
    $null = Cursor ($hm[0] - 200) $y 600
    foreach ($i in 1..4) { $null = Cursor ($hm[0] - 200 + 23 * $i) $y 30 }
    $s = State
    Check "a pointer on its way along the toolbar under the island steps it aside" ($s.away -eq "fade") "away=$($s.away) verdict=$($s.placement.verdict)"
    Leave

    # A panel opened by the hotkey: never away.
    Flick $hm
    $null = & $ctl debug open-tab media; Start-Sleep -Milliseconds 300
    $s = State
    Check "an open panel brings it home at once" ($s.away -eq "home" -and $s.mode -eq "panel") "away=$($s.away) mode=$($s.mode)"
    $null = & $ctl debug close
    Leave

    # ---- slide, and reduce motion ------------------------------------------------------------------
    SetCfg "[island]`nmove_aside = `"slide`"`n"
    $s = State; $hm = $s.home    # the idle pill again, not the panel
    Flick $hm
    $s = State
    Check "slide: shifted aside" ($s.away -eq "slide") "away=$($s.away)"
    Leave
    SetCfg "[island]`nmove_aside = `"slide`"`n[appearance]`nanimations = `"reduced`"`n"
    $s = State
    Check "Kurangi is reported as reduced" ($s.animations -eq "reduced")
    Flick $hm; Leave
    SetCfg "[island]`nmove_aside = `"slide`"`n[appearance]`nanimations = `"off`"`n"
    $s = State
    Check "Mati is reported as off" ($s.animations -eq "off")
    Flick $hm
    $s = State
    Check "Mati: the island still steps aside (no movement, just state)" ($s.away -eq "slide" -and $s.passthrough)
    Leave
    $null = & $ctl debug cursor real
    SetCfg ""

    # ---- hide until the top edge -------------------------------------------------------------------------
    SetCfg "[island]`nhide_until_edge = true`n"
    $s = State; $hm = $s.home
    $null = Cursor 300 600 16
    $s = State
    Check "hidden until the edge: away=hide, clicks go through" ($s.away -eq "hide" -and $s.passthrough -eq $true) "away=$($s.away)"
    $cx = [int](($hm[0] + $hm[2]) / 2)
    foreach ($i in 0..4) { $null = Cursor $cx 0 40 }
    $s = State
    Check "the pointer resting on the top edge near it shows it" ($s.away -eq "home" -and -not $s.passthrough) "away=$($s.away)"
    $null = Cursor 300 600 100; $null = Cursor 300 600 1700; $null = Cursor 300 600 600
    $s = State
    Check "it hides again once the pointer has been away" ($s.away -eq "hide") "away=$($s.away)"
    # A flick across the top edge is not a touch.
    $null = Cursor 100 0 16; $null = Cursor 400 0 30; $null = Cursor 700 0 30; $null = Cursor 1400 0 30; $null = Cursor 1900 0 30
    $s = State
    Check "a flick across the top edge reveals nothing" ($s.away -eq "hide") "away=$($s.away)"
    $null = & $ctl debug cursor real
    SetCfg ""
    $s = State
    Check "turning the option off shows the island" ($s.away -eq "home")

    # ---- hide when idle -------------------------------------------------------------------------------------------
    # The pointer is fake with a virtual clock: the machine only moves on samples, so every "wait" is a sample with --dt.
    SetCfg "[island]`nhide_when_idle = true`n"
    $s = State; $hm = $s.home
    $null = Cursor 300 600 16
    $s = State
    Check "idle: hidden, clicks go through" ($s.away -eq "hide" -and $s.passthrough -eq $true -and $s.placement.hide_mode -eq "idle") "away=$($s.away) mode=$($s.placement.hide_mode)"
    $null = & $ctl debug activity polite.busy 40 "Lagu"
    Start-Sleep -Milliseconds 300
    $s = State
    Check "any activity (music priority) shows it at once" ($s.away -eq "home" -and -not $s.passthrough) "away=$($s.away)"
    $null = Cursor 300 600 5000
    $s = State
    Check "... and keeps it up while the activity lasts" ($s.away -eq "home") "away=$($s.away)"
    $null = & $ctl debug clear-activity polite.busy
    $null = Cursor 300 600 100; $null = Cursor 300 600 1000
    $s = State
    Check "after the activity it holds about 2 s (no blinking between pills)" ($s.away -eq "home") "away=$($s.away)"
    $null = Cursor 300 600 1500
    $s = State
    Check "... then hides again" ($s.away -eq "hide" -and $s.passthrough -eq $true) "away=$($s.away)"
    $cx = [int](($hm[0] + $hm[2]) / 2)
    foreach ($i in 0..4) { $null = Cursor $cx 0 40 }
    $s = State
    Check "idle-hidden: resting on the top edge near it shows it" ($s.away -eq "home" -and -not $s.passthrough) "away=$($s.away)"
    $null = Cursor 300 600 100; $null = Cursor 300 600 1700; $null = Cursor 300 600 600
    $s = State
    Check "... and it hides once the pointer is away and nothing is going on" ($s.away -eq "hide") "away=$($s.away)"
    $null = & $ctl debug open-tab media; Start-Sleep -Milliseconds 300
    $s = State
    Check "an open panel shows it" ($s.away -eq "home" -and $s.mode -eq "panel") "away=$($s.away) mode=$($s.mode)"
    $null = & $ctl debug close
    # Both flags on in a hand-edited file: "always" wins, so music does not keep it up.
    SetCfg "[island]`nhide_until_edge = true`nhide_when_idle = true`n"
    $null = & $ctl debug activity polite.busy 40 "Lagu"
    $null = Cursor 300 600 16; $null = Cursor 300 600 4500
    $s = State
    Check "both flags: 'always' wins (music does not keep it up)" ($s.away -eq "hide" -and $s.placement.hide_mode -eq "always") "away=$($s.away) mode=$($s.placement.hide_mode)"
    $null = & $ctl debug clear-activity polite.busy
    $null = & $ctl debug cursor real
    SetCfg ""

    # ---- per-app hide rule -------------------------------------------------------------------------------------
    SetCfg "[island]`nhide_apps = [`"chrome.exe`", `"Code.exe`"]`n"
    foreach ($c in @(@("chrome.exe", $true), @("notepad.exe", $false), @("CODE", $true), @("C:\Program Files\Google\Chrome\chrome.exe", $true), @("chrome2.exe", $false))) {
        $null = & $ctl debug click "placement.debug_fg:$($c[0])"; Start-Sleep -Milliseconds 150
        $s = State
        Check "foreground $($c[0]) -> hidden=$($c[1])" ($s.app_hidden -eq $c[1] -and $s.hidden -eq $c[1]) "app_hidden=$($s.app_hidden) hidden=$($s.hidden)"
    }
    $null = & $ctl debug click "placement.debug_fg:chrome.exe"; Start-Sleep -Milliseconds 150
    SetCfg ""
    $s = State
    Check "emptying the list shows the island again" (-not $s.app_hidden -and -not $s.hidden)
    $null = & $ctl debug click "placement.debug_fg:-"

    # The REAL foreground app (no pretending): list its name, the island must hide; empty list, it must show.
    $fg = ForegroundName
    if ($fg -and $fg -notmatch '^(moku|moku-settings|ApplicationFrameHost)$') {
        SetCfg "[island]`nhide_apps = [`"$fg`"]`n"
        $s = State
        if ((ForegroundName) -eq $fg) {
            Check "the real foreground app is read: listing it hides the island" ($s.app_hidden -eq $true -and $s.hidden -eq $true) "app_hidden=$($s.app_hidden)"
        } else { "SKIP  real foreground check (the foreground app changed meanwhile)" }
        SetCfg ""
        $s = State
        Check "... and emptying the list shows it again" ($s.app_hidden -eq $false -and $s.hidden -eq $false)
    } else { "SKIP  real foreground check (no foreground app that is not Moku)" }

    # ---- follow the cursor and a named monitor, on invented monitors ---------------------------------------------
    $null = & $ctl debug click "placement.debug_monitors:1920x1080@0,0;1920x1080@1920,0;1600x900@-1600,0"
    SetCfg "[island]`nmonitor = `"cursor`"`n"
    $s = State
    Check "follow the cursor starts on the monitor under the (real) pointer or the primary one" ($s.monitor.name -ne "") "monitor=$($s.monitor.name)"
    $null = Cursor 100 500 16; $null = Cursor 100 500 400
    $s = State
    Check "...on the first invented monitor once the pointer is there" ($s.monitor.name -eq '\\.\DISPLAY1') "monitor=$($s.monitor.name)"
    $null = Cursor 2500 500 100
    $null = Cursor 2500 500 200
    $s = State
    Check "300 ms are not over yet: the island stays" ($s.monitor.name -eq '\\.\DISPLAY1') "monitor=$($s.monitor.name)"
    $null = Cursor 100 500 50   # back to monitor 1 in the middle of the debounce
    $null = Cursor 2500 500 100
    $null = Cursor 2500 500 250
    $s = State
    Check "crossing back and forth restarts the debounce" ($s.monitor.name -eq '\\.\DISPLAY1') "monitor=$($s.monitor.name)"
    $null = Cursor 2500 500 100
    $s = State
    Check "300 ms on monitor 2: the island moves there" ($s.monitor.name -eq '\\.\DISPLAY2' -and $s.monitor.rect[0] -eq 1920) "monitor=$($s.monitor.name) rect=$($s.monitor.rect -join ',')"
    Check "... and its window is on that monitor" ($s.home[0] -ge 1920 -and $s.home[2] -le 3840) "home=$($s.home -join ',')"
    $null = Cursor -800 400 100; $null = Cursor -800 400 400
    $s = State
    Check "a monitor with negative coordinates works too" ($s.monitor.name -eq '\\.\DISPLAY3' -and $s.home[2] -le 0) "monitor=$($s.monitor.name) home=$($s.home -join ',')"
    $null = & $ctl debug cursor real
    SetCfg "[island]`nmonitor = `"\\\\.\\DISPLAY2`"`n"
    $s = State
    Check "a named monitor" ($s.monitor.name -eq '\\.\DISPLAY2') "monitor=$($s.monitor.name)"
    SetCfg "[island]`nmonitor = `"\\\\.\\DISPLAY9`"`n"
    $s = State
    Check "a named monitor that is not connected: the primary one" ($s.monitor.name -eq '\\.\DISPLAY1') "monitor=$($s.monitor.name)"
    SetCfg ""
    $null = & $ctl debug click "placement.debug_monitors:"
    $s = State
    Check "the real monitors again" ($s.placement.monitors.Count -ge 1 -and $s.placement.monitors[0].name -ne '\\.\DISPLAY3') ($s.placement.monitors | ConvertTo-Json -Compress)
}
finally {
    $null = & $ctl debug cursor real 2>$null
    $null = & $ctl debug click "placement.debug_fg:-" 2>$null
    $null = & $ctl debug click "placement.debug_monitors:" 2>$null
    if ($null -ne $saved) { Set-Content -Path $cfgPath -Value $saved -Encoding utf8 -NoNewline } else { Remove-Item $cfgPath -ErrorAction SilentlyContinue }
}
if ($failed) { "$failed check(s) FAILED"; exit 1 }
"all checks passed"
