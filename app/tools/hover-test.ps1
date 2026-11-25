<#
.SYNOPSIS
  Interactive smoke test of a running island: checks click-through outside the shape
  (WindowFromPoint), moves the cursor onto the pill (hover → dwell → panel), takes screenshots,
  moves it away (collapse), and restores the cursor. Never clicks. Takes the screen lock used
  by prototype\shot.ps1 / measure.ps1.
  The island must have been started with MOKU_DEBUG=1 in its environment (`debug close` is
  refused otherwise): $env:MOKU_DEBUG = "1"; & "$Bin\moku.exe"
  The island must run with a scratch MOKU_DATA_DIR, and so must this script's environment (it
  refuses otherwise: it would reach your own island).
  Usage: .\hover-test.ps1 [-Bin <folder with moku.exe + mokuctl.exe>] [-OutDir <folder>]
  Defaults: <repo>\.build\main\release (or $env:CARGO_TARGET_DIR\release), <repo>\prototype\results\v01
#>
param([string]$Bin = "", [string]$OutDir = "")
$ErrorActionPreference = "Stop"
. "$PSScriptRoot\_common.ps1"
Assert-TestIsland
if (-not $Bin) { $Bin = Get-DefaultBin }
if (-not $OutDir) { $OutDir = Join-Path $RepoRoot "prototype\results\v01" }
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public static class HU {
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern int GetSystemMetrics(int n);
  [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
  public static string ClassAt(int x, int y) { var p = new POINT { X = x, Y = y }; var h = WindowFromPoint(p); var s = new StringBuilder(256); GetClassName(h, s, 256); return s.ToString(); }
}
"@
[HU]::SetProcessDPIAware() | Out-Null
function State { (& "$Bin\mokuctl.exe" debug state | Out-String) | ConvertFrom-Json }
function Shot($name, $h = 420) {
    $w = 1000; $sw = [HU]::GetSystemMetrics(0)
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen([int](($sw - $w) / 2), 0, 0, 0, $bmp.Size)
    $p = Join-Path $OutDir $name; $bmp.Save($p, [System.Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose(); "saved $p"
}
$mutex = New-Object System.Threading.Mutex($false, "Global\MokuProtoScreen")
if (-not $mutex.WaitOne([TimeSpan]::FromMinutes(5))) { throw "screen lock" }
$orig = New-Object HU+POINT; [HU]::GetCursorPos([ref]$orig) | Out-Null
try {
    $closed = (& "$Bin\mokuctl.exe" debug close | Out-String)
    if ($closed -match '"error"') { throw "debug ops refused: start moku.exe with MOKU_DEBUG=1 ($closed)" }
    Start-Sleep -Milliseconds 600
    $s = State
    $sw = [HU]::GetSystemMetrics(0)
    $winW = [int][math]::Round(784 * $s.scale)
    $left = [int](($sw - $winW) / 2)
    $m = $s.hit.main
    $cx = $left + $m[0] + [int]($m[2] / 2); $cy = $m[1] + [int]($m[3] / 2)
    "pill centre: $cx,$cy  class there: $([HU]::ClassAt($cx, $cy))"
    "beside pill ($($left + $m[0] - 30),$cy): $([HU]::ClassAt($left + $m[0] - 30, $cy))  (must not be MokuIsland*)"
    [HU]::SetCursorPos($cx, $cy) | Out-Null
    [HU]::SetCursorPos($cx + 1, $cy) | Out-Null
    Start-Sleep -Milliseconds 250
    "after enter: mode=$((State).mode)"
    Shot "v01-hover.png" 200
    Start-Sleep -Milliseconds 900
    "after dwell: mode=$((State).mode) tab=$((State).tab)"
    Shot "v01-hover-panel.png"
    [HU]::SetCursorPos($cx, 600) | Out-Null
    Start-Sleep -Milliseconds 800
    "after leave: mode=$((State).mode)"
} finally {
    [HU]::SetCursorPos($orig.X, $orig.Y) | Out-Null
    $mutex.ReleaseMutex()
}
