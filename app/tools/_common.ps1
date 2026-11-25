<#
.SYNOPSIS
  Dot-sourced by the other scripts of this folder (`. "$PSScriptRoot\_common.ps1"`): where things
  are, derived from THIS file's location, never from a hard-coded drive or folder.

  $RepoRoot   the repository (app\tools\.. \..)
  $AppRoot    <repo>\app
  Get-DefaultBin  the release folder of the build: $env:CARGO_TARGET_DIR\release, else <repo>\.build\main\release
  Get-IslandLockPath  the ONE island lock file of every git worktree: <main repo root>\.build\island.lock
  Assert-TestIsland  refuse to drive an island unless MOKU_DATA_DIR or MOKU_PIPE is set
#>
$AppRoot = Split-Path -Parent $PSScriptRoot
$RepoRoot = Split-Path -Parent $AppRoot

function Get-DefaultBin {
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $RepoRoot ".build\main" }
    Join-Path $target "release"
}

# Only one island can run on the machine, however many git worktrees are checked out, so the lock that
# keeps the tracks apart must be ONE file for all of them: `<checkout>\.build\island.lock` is a different
# file in every worktree. The main repository root is the parent of the shared git folder
# (`git rev-parse --path-format=absolute --git-common-dir`; a worktree's own `.git` is only a file that
# points at it), so every worktree resolves to `<main repo root>\.build\island.lock`.
# Outside git (no `.git` in the checkout, git not installed or too old for --path-format, a bare or
# submodule layout where the common dir is not called `.git`) it is `<checkout>\.build\island.lock`, as
# before. `-Root` is only for testing from another checkout's path (read-only; nothing is created).
function Get-IslandLockPath {
    param([string]$Root = $RepoRoot)
    $own = Join-Path $Root ".build\island.lock"
    if (-not (Test-Path -LiteralPath (Join-Path $Root ".git"))) { return $own }
    $git = Get-Command git -ErrorAction SilentlyContinue
    if (-not $git) { return $own }
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        $common = & $git.Source -C $Root rev-parse --path-format=absolute --git-common-dir 2>$null
        if ($LASTEXITCODE -ne 0) { $common = $null }
    }
    catch { $common = $null }
    finally { $ErrorActionPreference = $prev }
    $common = if ($common) { ([string]($common | Select-Object -First 1)).Trim() } else { "" }
    if (-not $common -or (Split-Path -Leaf $common) -ne ".git") { return $own }
    $main = Split-Path -Parent $common
    if (-not $main) { return $own }
    [IO.Path]::GetFullPath((Join-Path $main ".build\island.lock"))
}

# mokuctl finds the island's pipe from MOKU_PIPE, else MOKU_DATA_DIR, else the current user. A
# script that clicks, types or floods must never end up at the user's own island by accident:
# the test island runs with its own data folder, and so must the script that drives it.
function Assert-TestIsland {
    if (-not $env:MOKU_DATA_DIR -and -not $env:MOKU_PIPE) {
        throw "this script drives an island with debug commands: set `$env:MOKU_DATA_DIR (the same scratch folder the test island runs with, e.g. <repo>\.build\scratch-data) or `$env:MOKU_PIPE first, so it can never reach your own island."
    }
}
