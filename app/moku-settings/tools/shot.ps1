<#
.SYNOPSIS
  Start moku-settings with a throw-away data folder, capture ONLY its own window with
  PrintWindow, close it again. Never touches the real %APPDATA%\Moku or the real autostart key,
  and never sends any input (no clicks, no keys, no cursor moves).

.EXAMPLE
  .\shot.ps1 -AppArgs "--section privasi" -Out ..\..\..\prototype\results\v01\f-privasi.png
  .\shot.ps1 -AppArgs "--onboarding --step 3" -Fresh -Out ...\f-onboarding-3.png
#>
param(
    # Default: moku-settings.exe in <repo>\.build\main\release (or $env:CARGO_TARGET_DIR\release).
    [string]$Exe = "",
    [string]$AppArgs = "",
    [Parameter(Mandatory = $true)][string]$Out,
    # Start from an empty data folder (config absent: onboarding not completed).
    [switch]$Fresh,
    # Optional extra TOML appended to the seeded config.toml (e.g. "[island]`nstyle = `"notch`"").
    [string]$ExtraToml = "",
    [int]$SettleMs = 1500
)
$ErrorActionPreference = "Stop"
. "$PSScriptRoot\..\..\tools\_common.ps1"
if (-not $Exe) { $Exe = Join-Path (Get-DefaultBin) "moku-settings.exe" }
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

# A throw-away data folder under <repo>\.build (git-ignored), never the real %APPDATA%\Moku.
$data = Join-Path $RepoRoot ".build\settings-shot-data"
if ($Fresh -and (Test-Path $data)) { Remove-Item $data -Recurse -Force }
New-Item -ItemType Directory -Force $data | Out-Null
$cfg = Join-Path $data "config.toml"
if (-not $Fresh) { "[onboarding]`ncompleted = true`n$ExtraToml" | Set-Content -Path $cfg -Encoding utf8 }
elseif ($ExtraToml) { $ExtraToml | Set-Content -Path $cfg -Encoding utf8 }

$env:MOKU_DATA_DIR = $data
$env:MOKU_AUTOSTART_KEY = "Software\MokuSettingsTest\Run-shot"

$p = if ($AppArgs) { Start-Process -FilePath $Exe -ArgumentList $AppArgs -PassThru } else { Start-Process -FilePath $Exe -PassThru }
try {
    $deadline = (Get-Date).AddSeconds(15)
    while ($p.MainWindowHandle -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 200
        $p.Refresh()
        if ($p.HasExited) { throw "moku-settings exited early (code $($p.ExitCode))" }
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
    # Crop the invisible resize border / shadow margin: keep the visible frame only.
    $vis = New-Object W+RECT
    if ([W]::DwmGetWindowAttribute($h, 9, [ref]$vis, 16) -eq 0) {
        $crop = New-Object System.Drawing.Rectangle(($vis.L - $r.L), ($vis.T - $r.T), ($vis.R - $vis.L), ($vis.B - $vis.T))
        $cropped = $bmp.Clone($crop, $bmp.PixelFormat)
        $bmp.Dispose(); $bmp = $cropped; $w = $crop.Width; $ht = $crop.Height
    }
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Output "saved $Out (${w}x${ht}, PrintWindow=$ok)"
}
finally {
    if (-not $p.HasExited) {
        [void]$p.CloseMainWindow()
        if (-not $p.WaitForExit(4000)) { $p.Kill() }
    }
}

