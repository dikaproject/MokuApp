<#
.SYNOPSIS
  Review Focus 5 smoke test against a running island: a client that connects and never
  writes, a garbage line, an oversized line, and a flood of notifications must not hang the
  island; `mokuctl ping` keeps answering throughout.
  Run it against a scratch island only (MOKU_DATA_DIR set in the island's and this script's
  environment): it floods the pipe.
  Usage: .\pipe-robustness-test.ps1 [-Bin <folder with mokuctl.exe>]   (default <repo>\.build\main\release)
#>
param([string]$Bin = "")
$ErrorActionPreference = "Stop"
. "$PSScriptRoot\_common.ps1"
Assert-TestIsland
if (-not $Bin) { $Bin = Get-DefaultBin }
$ctl = "$Bin\mokuctl.exe"
# The pipe of that island, from the one function that names it (mokuctl pipe-name), without \\.\pipe\.
$pipeName = (& $ctl pipe-name | Out-String).Trim() -replace '^\\\\\.\\pipe\\', ''
function Ping { $t = [Diagnostics.Stopwatch]::StartNew(); $r = & $ctl ping; "{0} in {1} ms" -f $r, $t.ElapsedMilliseconds }
function Open {
    $c = New-Object System.IO.Pipes.NamedPipeClientStream(".", $pipeName, [System.IO.Pipes.PipeDirection]::InOut)
    $c.Connect(2000); $c
}
function Send($line) {
    $c = Open
    $w = New-Object System.IO.StreamWriter($c); $w.AutoFlush = $true; $w.Write($line + "`n")
    $r = New-Object System.IO.StreamReader($c); $resp = $r.ReadLine(); $c.Dispose(); $resp
}

"1. silent clients (connect, never write):"
$silent = 1..3 | ForEach-Object { Open }
"   ping while 3 silent clients are connected: $(Ping)"
Start-Sleep -Milliseconds 2300
$closed = 0
foreach ($c in $silent) { try { $b = $c.ReadByte(); if ($b -eq -1) { $closed++ } } catch { $closed++ } ; $c.Dispose() }
"   silent clients dropped by the server after the 2 s timeout: $closed / 3"

"2. garbage line: $(Send 'this is not json')"
"3. unknown type: $(Send '{""type"":""launch""}')"
"4. oversized line (70 KB): " + $(try { Send ('{"type":"todo","add":"' + ('x' * 70000) + '"}') } catch { "connection closed: $($_.Exception.InnerException.Message)" })
"5. flood: 40 notifications"
$t = [Diagnostics.Stopwatch]::StartNew()
1..40 | ForEach-Object { & $ctl notify --title "Flood $_" --id flood | Out-Null }
"   40 notifies in $($t.ElapsedMilliseconds) ms; ping after flood: $(Ping)"
