<#
.SYNOPSIS
  Fair CPU comparison: the app's own CPU plus the Desktop Window Manager's (dwm.exe), because a
  Windows.UI.Composition app hands its animation work to DWM, so its own CPU looks near zero.

  For each case: start the exe with --demo (or nothing for the baseline), wait -Warmup s,
  sample "% Processor Time" of the app and of dwm for -Seconds s, stop the app.
  Values are % of ONE core. Appends JSON lines to results\dwm-compare.jsonl.
#>
param([int]$Seconds = 20, [int]$Warmup = 4)
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$cases = @(
    @{ label = "baseline (no island)"; exe = $null },
    @{ label = "composition --demo"; exe = "$root\island-composition\target\release\moku-island.exe" },
    @{ label = "slint software --demo"; exe = "$root\island-slint\target\release\moku-island-slint-sw.exe" },
    @{ label = "slint opengl --demo"; exe = "$root\island-slint\target\release\moku-island-slint.exe" }
)

$mutex = New-Object System.Threading.Mutex($false, "Global\MokuProtoScreen")
if (-not $mutex.WaitOne([TimeSpan]::FromMinutes(15))) { throw "Timed out waiting for the screen lock" }
try {
    foreach ($c in $cases) {
        $proc = $null
        if ($c.exe) { $proc = Start-Process -FilePath $c.exe -ArgumentList "--demo" -PassThru }
        try {
            Start-Sleep -Seconds $Warmup
            $counters = @("\Process(dwm)\% Processor Time")
            if ($proc) { $counters += "\Process($([IO.Path]::GetFileNameWithoutExtension($c.exe)))\% Processor Time" }
            $samples = Get-Counter -Counter $counters -SampleInterval 1 -MaxSamples $Seconds
            $dwm = @(); $app = @()
            foreach ($s in $samples) {
                foreach ($cs in $s.CounterSamples) {
                    if ($cs.Path -match '\(dwm\)') { $dwm += $cs.CookedValue } else { $app += $cs.CookedValue }
                }
            }
            $d = [math]::Round(($dwm | Measure-Object -Average).Average, 2)
            $a = if ($app.Count) { [math]::Round(($app | Measure-Object -Average).Average, 2) } else { 0 }
            $r = [ordered]@{ label = $c.label; app_cpu = $a; dwm_cpu = $d; total = [math]::Round($a + $d, 2); seconds = $Seconds; when = (Get-Date).ToString("s") }
            ($r | ConvertTo-Json -Compress) | Add-Content (Join-Path $root "results\dwm-compare.jsonl") -Encoding utf8
            "{0,-24} app {1,6}%   dwm {2,6}%   total {3,6}%   (one core)" -f $r.label, $r.app_cpu, $r.dwm_cpu, $r.total
        }
        finally { if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force } }
        Start-Sleep -Seconds 2
    }
}
finally { $mutex.ReleaseMutex(); $mutex.Dispose() }
