<#
.SYNOPSIS
  Capture short bursts of frames of ONLY the island, over an opaque backdrop of exactly the
  capture rect, so nothing from the user's screen can end up in a screenshot. Never sends
  input to any window (no SendInput, no mouse / keyboard events, no cursor moves).

.DESCRIPTION
  For every entry of -At (milliseconds after the trigger) the script shows a borderless,
  click-through, never-activated window (a purple-to-orange gradient, like the mockups'
  wallpaper) placed directly BELOW the island window in the z-order, takes -Frames screenshots
  of that rect about -GapMs apart, and destroys the window again: it is on screen for well
  under 300 ms each time. The trigger is a `mokuctl debug click <action>` (needs the island to
  run with MOKU_DEBUG=1) or nothing (-Trigger "").

  Take the island lock first (tools\island-lock.ps1) and run the island with
  MOKU_DATA_DIR / MOKU_DEBUG=1.

.EXAMPLE
  .\island-burst.ps1 -Trigger "system.sample:battery-plugged:48" -At 150,450,800 -Out ..\..\prototype\results\v02\h-charging
  -> h-charging-0150.png, h-charging-0450.png, h-charging-0800.png (with -Frames 2: ...-0150-b.png too)
#>
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [string]$Trigger = "",
    [int[]]$At = @(0),
    [int]$Frames = 1,
    [int]$GapMs = 45,
    # Capture rect around the island, physical px relative to the top-centre of the island window.
    [int]$Width = 420,
    [int]$Height = 76,
    # Default: mokuctl.exe in <repo>\.build\main\release (or $env:CARGO_TARGET_DIR\release), or $env:MOKUCTL.
    [string]$Mokuctl = ""
)
$ErrorActionPreference = "Stop"
. "$PSScriptRoot\_common.ps1"
if (-not $Mokuctl) { $Mokuctl = if ($env:MOKUCTL) { $env:MOKUCTL } else { Join-Path (Get-DefaultBin) "mokuctl.exe" } }
# A trigger is a debug click: only ever on a scratch island (MOKU_DATA_DIR / MOKU_PIPE set).
if ($Trigger) { Assert-TestIsland }
if ($Frames * $GapMs -gt 200) { throw "a burst must stay under 300 ms: lower -Frames or -GapMs" }

Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type -ReferencedAssemblies System.Drawing, System.Windows.Forms @"
using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Runtime.InteropServices;
using System.Windows.Forms;

public static class Native {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindowW(string cls, IntPtr title);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int cx, uint flags);
    [DllImport("user32.dll")] public static extern bool SetLayeredWindowAttributes(IntPtr h, uint key, byte alpha, uint flags);
}

// Opaque, click-through, never activated, not in the task bar / Alt+Tab.
public class Backdrop : Form {
    protected override bool ShowWithoutActivation { get { return true; } }
    protected override CreateParams CreateParams {
        get {
            var cp = base.CreateParams;
            // TOPMOST | TOOLWINDOW | NOACTIVATE | LAYERED | TRANSPARENT
            cp.ExStyle |= 0x8 | 0x80 | 0x08000000 | 0x80000 | 0x20;
            return cp;
        }
    }
    protected override void OnPaint(PaintEventArgs e) {
        using (var b = new LinearGradientBrush(ClientRectangle, Color.Black, Color.Black, 0f)) {
            var cb = new ColorBlend(4);
            cb.Colors = new[] { Color.FromArgb(0x52, 0x38, 0xd4), Color.FromArgb(0x8b, 0x4e, 0xf0), Color.FromArgb(0xf2, 0x52, 0x7f), Color.FromArgb(0xff, 0xb5, 0x6b) };
            cb.Positions = new[] { 0f, 0.38f, 0.72f, 1f };
            b.InterpolationColors = cb;
            e.Graphics.FillRectangle(b, ClientRectangle);
        }
    }
}
"@
# Per-monitor aware (v2), so the capture rect is in true physical pixels also on a monitor whose
# scale differs from the primary one (a system-aware process gets that monitor's coordinates
# virtualised and the backdrop lands next to the island instead of under it).
if (-not [Native]::SetProcessDpiAwarenessContext([IntPtr](-4))) { [void][Native]::SetProcessDPIAware() }

$outFull = if ([IO.Path]::IsPathRooted($Out)) { [IO.Path]::GetFullPath($Out) } else { [IO.Path]::GetFullPath((Join-Path (Get-Location) $Out)) }
New-Item -ItemType Directory -Force ([IO.Path]::GetDirectoryName($outFull)) | Out-Null

$isl = [Native]::FindWindowW("MokuIsland", [IntPtr]::Zero)
if ($isl -eq [IntPtr]::Zero) { throw "the island window was not found (is moku.exe running?)" }
$r = New-Object Native+RECT
[void][Native]::GetWindowRect($isl, [ref]$r)
$cx = [int](($r.L + $r.R) / 2)
$x = $cx - [int]($Width / 2); $y = $r.T

$clock = [Diagnostics.Stopwatch]::StartNew()
if ($Trigger) { & $Mokuctl debug click $Trigger | Out-Null }
$t0 = $clock.ElapsedMilliseconds

foreach ($when in $At) {
    while ($clock.ElapsedMilliseconds - $t0 -lt $when) { Start-Sleep -Milliseconds 5 }
    $form = New-Object Backdrop
    try {
        $form.FormBorderStyle = "None"; $form.ShowInTaskbar = $false; $form.StartPosition = "Manual"
        $form.SetBounds($x, $y, $Width, $Height)
        $form.Show()
        # Directly below the island (both topmost), exactly the capture rect.
        [void][Native]::SetWindowPos($form.Handle, $isl, $x, $y, $Width, $Height, 0x0010 -bor 0x0040)  # NOACTIVATE | SHOWWINDOW
        [void][Native]::SetLayeredWindowAttributes($form.Handle, 0, 255, 2)
        $form.Refresh()
        [Windows.Forms.Application]::DoEvents()
        Start-Sleep -Milliseconds 25
        $shown = [Diagnostics.Stopwatch]::StartNew()
        for ($i = 0; $i -lt $Frames; $i++) {
            $bmp = New-Object System.Drawing.Bitmap $Width, $Height
            $g = [System.Drawing.Graphics]::FromImage($bmp)
            $g.CopyFromScreen($x, $y, 0, 0, (New-Object System.Drawing.Size $Width, $Height), [System.Drawing.CopyPixelOperation]::SourceCopy)
            $g.Dispose()
            $name = "{0}-{1:0000}{2}.png" -f $outFull, $when, $(if ($i -gt 0) { "-" + [char](97 + $i) } else { "" })
            $bmp.Save($name, [System.Drawing.Imaging.ImageFormat]::Png)
            $bmp.Dispose()
            if ($i + 1 -lt $Frames) { Start-Sleep -Milliseconds $GapMs }
        }
    }
    finally {
        $form.Close(); $form.Dispose()
    }
}
"done: $($At.Count) burst(s), rect ${Width}x${Height} at ($x,$y), no input sent"
