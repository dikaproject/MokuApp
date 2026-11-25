<#
  Interactive smoke test for the composition island (run from prototype\):
    .\island-composition\interact-test.ps1
  Starts the exe in interactive mode, checks hit-testing with WindowFromPoint (which window would
  receive a click at a point), then moves the cursor onto the pill to trigger hover and the
  400 ms dwell → panel, then away to collapse, taking screenshots into results\. Restores the
  cursor afterwards. Takes the same screen lock as shot.ps1 / measure.ps1.
#>
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public static class U {
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
[U]::SetProcessDPIAware() | Out-Null
$sw = [U]::GetSystemMetrics(0)
$cx = [int]($sw / 2)
$exe = (Resolve-Path "island-composition\target\release\moku-island.exe").Path

function Shot($name) {
    $w = 1000; $h = 360; $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($cx - $w / 2, 0, 0, 0, (New-Object System.Drawing.Size $w, $h)); $g.Dispose()
    $bmp.Save((Join-Path $PWD "results\$name.png")); $bmp.Dispose()
}

$mutex = New-Object System.Threading.Mutex($false, "Global\MokuProtoScreen")
$null = $mutex.WaitOne([TimeSpan]::FromMinutes(15))
$saved = New-Object U+POINT; [U]::GetCursorPos([ref]$saved) | Out-Null
try {
    [U]::SetCursorPos($cx, 600) | Out-Null
    $p = Start-Process -FilePath $exe -PassThru
    Start-Sleep -Milliseconds 1500
    "idle: click target at pill centre      = " + [U]::ClassAt($cx, 26)
    "idle: click target 6px left of pill    = " + [U]::ClassAt($cx - 62, 26)
    "idle: click target in shadow below     = " + [U]::ClassAt($cx, 60)
    "idle: click target in window margin    = " + [U]::ClassAt($cx - 300, 200)
    [U]::SetCursorPos($cx, 26) | Out-Null
    Start-Sleep -Milliseconds 200
    Shot "comp-live-hover"
    Start-Sleep -Milliseconds 900
    Shot "comp-live-panel"
    "panel: click target inside panel       = " + [U]::ClassAt($cx - 250, 200)
    "panel: click target below panel        = " + [U]::ClassAt($cx, 300)
    [U]::SetCursorPos($cx, 600) | Out-Null
    Start-Sleep -Milliseconds 900
    Shot "comp-live-collapsed"
    "after leave: click target at old panel = " + [U]::ClassAt($cx - 250, 200)
    "process still running: " + (-not $p.HasExited)
    Stop-Process -Id $p.Id -Force
} finally {
    [U]::SetCursorPos($saved.X, $saved.Y) | Out-Null
    $mutex.ReleaseMutex()
}
