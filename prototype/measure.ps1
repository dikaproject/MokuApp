<#
.SYNOPSIS
  Measure memory and CPU of a prototype process.

.EXAMPLE
  .\measure.ps1 -Exe .\island-composition\target\release\moku-island.exe -Args "--state idle" -Seconds 30 -Label comp-idle
  .\measure.ps1 -Exe ... -Args "--demo" -Seconds 30 -Label comp-demo

  Starts the exe, waits -Warmup seconds, then samples once per second for -Seconds:
  working set, private bytes, CPU % (of one core and of the whole machine), GPU dedicated
  memory when the performance counter exists. Kills the process at the end, prints a summary
  and appends one JSON line to results\measurements.jsonl.
#>
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$Args = "",
    [int]$Seconds = 30,
    [int]$Warmup = 5,
    [string]$Label = ""
)

$ErrorActionPreference = "Stop"
$exePath = (Resolve-Path $Exe).Path
if (-not $Label) { $Label = [IO.Path]::GetFileNameWithoutExtension($exePath) + " " + $Args }
$cores = [Environment]::ProcessorCount

# Only one prototype may own the top of the screen at a time (shot.ps1 uses the same mutex).
$mutex = New-Object System.Threading.Mutex($false, "Global\MokuProtoScreen")
if (-not $mutex.WaitOne([TimeSpan]::FromMinutes(15))) { throw "Timed out waiting for the screen lock" }
$proc = $null
try {
$proc = if ($Args) { Start-Process -FilePath $exePath -ArgumentList $Args -PassThru } else { Start-Process -FilePath $exePath -PassThru }
try {
    Start-Sleep -Seconds $Warmup
    if ($proc.HasExited) { throw "Process exited during warmup (exit code $($proc.ExitCode))" }

    $ws = @(); $priv = @(); $cpu1 = @(); $gpu = @()
    $proc.Refresh()
    $lastCpu = $proc.TotalProcessorTime.TotalMilliseconds
    $lastT = [Diagnostics.Stopwatch]::StartNew()
    $gpuCounter = "\GPU Process Memory(pid_$($proc.Id)*)\Dedicated Usage"

    for ($i = 0; $i -lt $Seconds; $i++) {
        Start-Sleep -Milliseconds 1000
        if ($proc.HasExited) { throw "Process exited during sampling (exit code $($proc.ExitCode))" }
        $proc.Refresh()
        $nowCpu = $proc.TotalProcessorTime.TotalMilliseconds
        $elapsed = $lastT.Elapsed.TotalMilliseconds
        $lastT.Restart()
        $cpu1 += [math]::Round(100.0 * ($nowCpu - $lastCpu) / $elapsed, 2)
        $lastCpu = $nowCpu
        $ws += [math]::Round($proc.WorkingSet64 / 1MB, 1)
        $priv += [math]::Round($proc.PrivateMemorySize64 / 1MB, 1)
        try {
            $s = (Get-Counter -Counter $gpuCounter -ErrorAction Stop).CounterSamples | Measure-Object -Property CookedValue -Sum
            $gpu += [math]::Round($s.Sum / 1MB, 1)
        } catch { }
    }
}
finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
}
}
finally {
    $mutex.ReleaseMutex()
    $mutex.Dispose()
}

function Stat($a) {
    if (-not $a -or $a.Count -eq 0) { return $null }
    $m = $a | Measure-Object -Average -Maximum -Minimum
    [ordered]@{ avg = [math]::Round($m.Average, 2); min = $m.Minimum; max = $m.Maximum }
}

$size = [math]::Round((Get-Item $exePath).Length / 1KB, 0)
$result = [ordered]@{
    label            = $Label
    exe              = $exePath
    args             = $Args
    when             = (Get-Date).ToString("s")
    seconds          = $Seconds
    exe_kb           = $size
    working_set_mb   = Stat $ws
    private_mb       = Stat $priv
    cpu_pct_one_core = Stat $cpu1
    cpu_pct_machine  = Stat ($cpu1 | ForEach-Object { [math]::Round($_ / $cores, 3) })
    gpu_dedicated_mb = Stat $gpu
}

$resultsDir = Join-Path $PSScriptRoot "results"
New-Item -ItemType Directory -Force $resultsDir | Out-Null
($result | ConvertTo-Json -Compress -Depth 4) | Add-Content -Path (Join-Path $resultsDir "measurements.jsonl") -Encoding utf8

"== $Label =="
"exe size        : $size KB"
"working set MB  : avg $($result.working_set_mb.avg)  max $($result.working_set_mb.max)"
"private MB      : avg $($result.private_mb.avg)  max $($result.private_mb.max)"
"CPU % (1 core)  : avg $($result.cpu_pct_one_core.avg)  max $($result.cpu_pct_one_core.max)"
"CPU % (machine) : avg $($result.cpu_pct_machine.avg)"
if ($result.gpu_dedicated_mb) { "GPU dedicated MB: avg $($result.gpu_dedicated_mb.avg)  max $($result.gpu_dedicated_mb.max)" } else { "GPU dedicated MB: (counter not available)" }
