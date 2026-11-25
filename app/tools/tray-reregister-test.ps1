<#
.SYNOPSIS
  Check that the island re-adds its tray icon after Explorer restarts (Review Focus 4),
  without restarting Explorer: posts the registered "TaskbarCreated" message to the island's
  tray window and checks `mokuctl debug state` → tray_adds and appbar_adds (the full-screen
  appbar is re-registered too) went up by one. Only uses `debug state` (no MOKU_DEBUG needed).
  Needs a running moku.exe (a scratch island: MOKU_DATA_DIR set in the island's and this script's
  environment, so mokuctl cannot reach your own island).
  Usage: .\tray-reregister-test.ps1 [-Bin <folder with mokuctl.exe>]   (default <repo>\.build\main\release)
#>
param([string]$Bin = "")
$ErrorActionPreference = "Stop"
. "$PSScriptRoot\_common.ps1"
Assert-TestIsland
if (-not $Bin) { $Bin = Get-DefaultBin }
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class TB {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern uint RegisterWindowMessage(string s);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
}
"@
function State { (& "$Bin\mokuctl.exe" debug state | Out-String) | ConvertFrom-Json }
function Adds { (State).tray_adds }
$before = Adds
$appbarBefore = (State).appbar_adds
$msg = [TB]::RegisterWindowMessage("TaskbarCreated")
$h = [TB]::FindWindow("MokuTray", [NullString]::Value)
if ($h -eq [IntPtr]::Zero) { throw "MokuTray window not found (is moku.exe running?)" }
[TB]::PostMessage($h, $msg, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
Start-Sleep -Milliseconds 500
$after = Adds
$appbarAfter = (State).appbar_adds
if ($after -eq $before + 1) { "PASS: tray icon re-added (tray_adds $before -> $after)" } else { "FAIL: tray_adds $before -> $after"; exit 1 }
if ($appbarAfter -eq $appbarBefore + 1) { "PASS: full-screen appbar re-registered (appbar_adds $appbarBefore -> $appbarAfter)" } else { "FAIL: appbar_adds $appbarBefore -> $appbarAfter"; exit 1 }
