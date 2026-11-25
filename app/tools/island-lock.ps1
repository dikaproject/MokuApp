<#
.SYNOPSIS
  Cross-process lock so parallel feature tracks never run two islands at once.

  Only one moku.exe can run (single-instance mutex, one pipe name, one screen top).
  Every track must take this lock BEFORE starting moku.exe and release it AFTER
  stopping it. The lock is a file, so it survives across separate shell calls.

  The file is the same one for every git worktree of the repository:
  <main repo root>\.build\island.lock (see Get-IslandLockPath in _common.ps1), so a track
  working in <repo>\wt\<name> waits for a track working in <repo>, and the other way round.
  Outside git it is <checkout>\.build\island.lock.

.EXAMPLE
  .\island-lock.ps1 acquire -Owner track-a      # waits up to -WaitMinutes, then fails
  .\island-lock.ps1 release -Owner track-a
  .\island-lock.ps1 status

  A lock older than -StaleMinutes (default 15) is treated as abandoned and taken over.
  Keep each island session short (start, test, screenshot, Stop-Process -Name moku, release).
#>
param(
    [Parameter(Mandatory = $true, Position = 0)][ValidateSet("acquire", "release", "status")][string]$Action,
    [string]$Owner = "unknown",
    [int]$WaitMinutes = 30,
    [int]$StaleMinutes = 15
)
$ErrorActionPreference = "Stop"
. "$PSScriptRoot\_common.ps1"
# <main repo root>\.build\island.lock for every worktree (.build is git-ignored); see _common.ps1.
$lock = Get-IslandLockPath
Write-Verbose "island lock file: $lock"
New-Item -ItemType Directory -Force (Split-Path $lock) | Out-Null

function Read-Lock {
    if (Test-Path $lock) { try { return Get-Content $lock -Raw | ConvertFrom-Json } catch { return $null } }
    return $null
}

switch ($Action) {
    "status" {
        $l = Read-Lock
        if ($l) { "held by $($l.owner) since $($l.since)" } else { "free" }
    }
    "release" {
        $l = Read-Lock
        if ($l -and $l.owner -ne $Owner) { throw "lock is held by $($l.owner), not $Owner" }
        Remove-Item $lock -ErrorAction SilentlyContinue
        "released"
    }
    "acquire" {
        $deadline = (Get-Date).AddMinutes($WaitMinutes)
        while ($true) {
            $l = Read-Lock
            if ($l -and $l.owner -eq $Owner) { "already held by $Owner"; break }
            if ($l -and ((Get-Date) - [datetime]$l.since).TotalMinutes -gt $StaleMinutes) {
                Write-Warning "taking over stale lock from $($l.owner)"
                Remove-Item $lock -ErrorAction SilentlyContinue
            }
            try {
                $fs = [IO.File]::Open($lock, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
                $bytes = [Text.Encoding]::UTF8.GetBytes((@{ owner = $Owner; since = (Get-Date).ToString("o") } | ConvertTo-Json -Compress))
                $fs.Write($bytes, 0, $bytes.Length); $fs.Dispose()
                # Make sure no stray island from someone else is still running.
                if (Get-Process -Name moku -ErrorAction SilentlyContinue) { Write-Warning "a moku.exe is still running; stop it: Stop-Process -Name moku" }
                "acquired by $Owner"; break
            }
            catch [System.IO.IOException] {
                if ((Get-Date) -gt $deadline) { throw "timed out waiting for island lock (held by $((Read-Lock).owner))" }
                Start-Sleep -Seconds 5
            }
        }
    }
}
