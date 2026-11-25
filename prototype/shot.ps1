<#
.SYNOPSIS
  Screenshot the top-centre of the primary screen while a prototype shows a given state.

.EXAMPLE
  .\shot.ps1 -Exe .\island-composition\target\release\moku-island.exe -Args "--state music-panel" -Out results\comp-music-panel.png

  Starts the exe, waits -Delay seconds (let animations settle), captures a -Width x -Height
  region centred at the top of the primary screen (physical pixels), kills the process.
  Use -NoStart to capture whatever is on screen right now.
#>
param(
    [string]$Exe = "",
    [string]$Args = "",
    [Parameter(Mandatory = $true)][string]$Out,
    [double]$Delay = 2.5,
    [int]$Width = 1000,
    [int]$Height = 520,
    [switch]$NoStart
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class DpiUtil {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int nIndex);
}
"@
[DpiUtil]::SetProcessDPIAware() | Out-Null
$screenW = [DpiUtil]::GetSystemMetrics(0)

$proc = $null
# Only one prototype may own the top of the screen at a time (measure.ps1 uses the same mutex).
$mutex = New-Object System.Threading.Mutex($false, "Global\MokuProtoScreen")
if (-not $mutex.WaitOne([TimeSpan]::FromMinutes(15))) { throw "Timed out waiting for the screen lock" }
try {
if (-not $NoStart) {
    $exePath = (Resolve-Path $Exe).Path
    $proc = if ($Args) { Start-Process -FilePath $exePath -ArgumentList $Args -PassThru } else { Start-Process -FilePath $exePath -PassThru }
}
try {
    Start-Sleep -Milliseconds ([int]($Delay * 1000))
    if ($proc -and $proc.HasExited) { throw "Process exited early (exit code $($proc.ExitCode))" }
    $x = [int](($screenW - $Width) / 2)
    $bmp = New-Object System.Drawing.Bitmap $Width, $Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($x, 0, 0, 0, (New-Object System.Drawing.Size $Width, $Height), [System.Drawing.CopyPixelOperation]::SourceCopy)
    $g.Dispose()
    $outPath = [IO.Path]::GetFullPath((Join-Path (Get-Location) $Out))
    New-Item -ItemType Directory -Force ([IO.Path]::GetDirectoryName($outPath)) | Out-Null
    $bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    "saved $outPath"
}
finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
}
}
finally {
    $mutex.ReleaseMutex()
    $mutex.Dispose()
}
