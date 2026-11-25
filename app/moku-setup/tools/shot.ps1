<#
.SYNOPSIS
  Start moku-setup with every location redirected to a throw-away sandbox, capture ONLY its own
  window with PrintWindow, close it again. Sends no input (no clicks, keys or cursor moves).

  -AppArgs "--preview done" shows a state without doing anything. "--go /nolaunch" really
  installs (into the sandbox) as if "Pasang" was pressed; -Wait then waits for the result.

.EXAMPLE
  .\shot.ps1 -AppArgs "--preview ready" -Out ..\..\..\prototype\results\v02\i-ready.png
#>
param(
    # Default: moku-setup.exe in <repo>\.build\main\release (or $env:CARGO_TARGET_DIR\release).
    [string]$Exe = "",
    [string]$AppArgs = "",
    [Parameter(Mandatory = $true)][string]$Out,
    # Default: <repo>\.build\setup-shot-sandbox (git-ignored).
    [string]$Sandbox = "",
    [int]$SettleMs = 1200,
    # Seconds to keep the window open after the capture (e.g. to let a --go run finish).
    [int]$Wait = 0
)
$ErrorActionPreference = "Stop"
. "$PSScriptRoot\..\..\tools\_common.ps1"
if (-not $Exe) { $Exe = Join-Path (Get-DefaultBin) "moku-setup.exe" }
if (-not $Sandbox) { $Sandbox = Join-Path $RepoRoot ".build\setup-shot-sandbox" }
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
}
"@
[void][W]::SetProcessDPIAware()

New-Item -ItemType Directory -Force $Sandbox | Out-Null
$env:MOKU_INSTALL_ROOT = Join-Path $Sandbox "root"
$env:MOKU_REG_ROOT = "Software\MokuSetupTest\shot"
$env:MOKU_DATA_DIR = Join-Path $Sandbox "data"
Remove-Item Env:MOKU_AUTOSTART_KEY -ErrorAction SilentlyContinue

$p = if ($AppArgs) { Start-Process -FilePath $Exe -ArgumentList $AppArgs -PassThru } else { Start-Process -FilePath $Exe -PassThru }
try {
    $deadline = (Get-Date).AddSeconds(15)
    while ($p.MainWindowHandle -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 200
        $p.Refresh()
        if ($p.HasExited) { throw "moku-setup exited early (code $($p.ExitCode))" }
    }
    if ($p.MainWindowHandle -eq [IntPtr]::Zero) { throw "no window appeared" }
    Start-Sleep -Milliseconds $SettleMs
    $p.Refresh()
    $h = $p.MainWindowHandle
    $r = New-Object W+RECT
    [void][W]::GetWindowRect($h, [ref]$r)
    $w = $r.R - $r.L; $ht = $r.B - $r.T
    $bmp = New-Object System.Drawing.Bitmap($w, $ht)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $dc = $g.GetHdc()
    $ok = [W]::PrintWindow($h, $dc, 2)   # PW_RENDERFULLCONTENT
    $g.ReleaseHdc($dc); $g.Dispose()
    New-Item -ItemType Directory -Force (Split-Path -Parent $Out) | Out-Null
    $vis = New-Object W+RECT
    if ([W]::DwmGetWindowAttribute($h, 9, [ref]$vis, 16) -eq 0) {
        $crop = New-Object System.Drawing.Rectangle(($vis.L - $r.L), ($vis.T - $r.T), ($vis.R - $vis.L), ($vis.B - $vis.T))
        $cropped = $bmp.Clone($crop, $bmp.PixelFormat)
        $bmp.Dispose(); $bmp = $cropped; $w = $crop.Width; $ht = $crop.Height
    }
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Output "saved $Out (${w}x${ht}, PrintWindow=$ok)"
    if ($Wait -gt 0) { Start-Sleep -Seconds $Wait }
}
finally {
    if (-not $p.HasExited) {
        [void]$p.CloseMainWindow()
        if (-not $p.WaitForExit(4000)) { $p.Kill() }
    }
}
