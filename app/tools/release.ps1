<#
.SYNOPSIS
  Build and package a Moku release: moku-setup.exe (installer with the binaries embedded), its
  .sha256, a portable zip and the release notes from CHANGELOG.md. CI's release.yml runs this
  same script.

.DESCRIPTION
  1. Reads the version from app\Cargo.toml ([workspace.package] version, the single source), and
     REFUSES to go on while REPO or UPDATE_PUBLIC_KEY in app\moku-core\src\release.rs is still a
     placeholder, or MOKU_SIGNING_KEY does not point at the private signing key (see
     tools\release-key.ps1). -Dev skips that check and builds an unofficial package.
  2. cargo test --workspace --locked (unless -SkipTests); every cargo call here uses --locked and, in CI, RUSTFLAGS=-D warnings.
  3. cargo build --release for moku, moku-settings, mokuctl.
  4. Stages the payload (the three exes + LICENSE + docs\claude-code-hook.md, agents.md, notifications.md + tools\claude-hook.ps1) and builds moku-setup with
     MOKU_SETUP_PAYLOAD pointing at it (build.rs compresses and embeds it).
  5. Writes to -OutDir:
       moku-setup.exe
       moku-setup.exe.sha256        "<sha256>  moku-setup.exe" (what the updater verifies)
       moku-setup.exe.sig           ECDSA P-256 signature of that SHA-256, made with the key file named
                                    by $env:MOKU_SIGNING_KEY and checked against UPDATE_PUBLIC_KEY in
                                    release.rs (the updater requires it)
       Moku-<version>-portable.zip  the payload (with LICENSE) + README.md + CHANGELOG.md
       release-notes.md             this version's CHANGELOG section

  With -SparsePackage (v0.3, Windows notification mirroring): moku.exe is built with an
  application manifest that ties it to an identity package ("sparse package"), and
  Moku.Identity.msix, that identity package, is written next to the other files, signed with
  -PackageCert when one is given. See app\docs\notifications.md for what the maintainer does
  with it. The script never registers the package and never installs a certificate.

  Nothing is installed and nothing is uploaded.

.EXAMPLE
  powershell -File app\tools\release.ps1
  powershell -File app\tools\release.ps1 -Tag v0.2.0 -OutDir <repo>\dist
  powershell -File app\tools\release.ps1 -SparsePackage -PackageCert C:\keys\moku-package.pfx
     (the password in $env:MOKU_PACKAGE_CERT_PASSWORD; the publisher is read from the certificate)
#>
param(
    # Default: <cargo target dir>\dist
    [string]$OutDir = "",
    # When given (e.g. the pushed tag "v0.2.0"), it must match the workspace version.
    [string]$Tag = "",
    [switch]$SkipTests,
    # A development package: allow the placeholder REPO / missing public key / missing signing key.
    # The updater of such a build stays off, and the result must not be published.
    [switch]$Dev,
    # Also build the identity package (Moku.Identity.msix) and an moku.exe that belongs to it.
    [switch]$SparsePackage,
    # The identity package's publisher, e.g. "CN=Your Name". Must equal the Subject of the
    # certificate that signs the package. Default: the subject of -PackageCert.
    [string]$Publisher = "",
    # A .pfx to sign Moku.Identity.msix with (password: $env:MOKU_PACKAGE_CERT_PASSWORD). Without
    # one the package is left unsigned: the maintainer signs it.
    [string]$PackageCert = "",
    # Only the identity package (no tests, no build, no installer): needs -OutDir and -SparsePackage.
    [switch]$IdentityOnly
)
$ErrorActionPreference = "Stop"

$app = Split-Path -Parent $PSScriptRoot
$repo = Split-Path -Parent $app

function Invoke-Cargo {
    & cargo @args
    if ($LASTEXITCODE -ne 0) { throw "cargo $($args -join ' ') failed ($LASTEXITCODE)" }
}

# ---- version ------------------------------------------------------------------------------
$toml = Get-Content (Join-Path $app "Cargo.toml") -Raw
if ($toml -notmatch '(?ms)^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"') {
    throw "no [workspace.package] version in app\Cargo.toml"
}
$version = $Matches[1]
if ($Tag -and ($Tag -replace '^v', '') -ne $version) {
    throw "tag $Tag does not match the workspace version $version"
}
Write-Host "Moku $version"

# ---- a real release needs a real repository and a signing key ------------------------------------
$releaseRs = Get-Content (Join-Path $app "moku-core\src\release.rs") -Raw
$repoSlug = if ($releaseRs -match 'pub const REPO:\s*&str\s*=\s*"([^"]*)"') { $Matches[1] } else { "" }
$hasPublicKey = $releaseRs -match 'pub const UPDATE_PUBLIC_KEY:\s*Option<&str>\s*=\s*Some\("[0-9a-fA-F]{128}"\)'
$signingKey = $env:MOKU_SIGNING_KEY
$canSign = [bool]($signingKey -and (Test-Path -LiteralPath $signingKey))
$problems = @()
if ($repoSlug -notmatch '^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$' -or $repoSlug -like "OWNER/*") {
    $problems += "REPO in app\moku-core\src\release.rs is still the placeholder ""$repoSlug"": set the real owner/name"
}
if (-not $hasPublicKey) {
    $problems += "UPDATE_PUBLIC_KEY in app\moku-core\src\release.rs is still None: run app\tools\release-key.ps1 new -Out <a path OUTSIDE the repo> and paste the line it prints"
}
if (-not $canSign) {
    $problems += "MOKU_SIGNING_KEY does not point at the private signing key file (release-key.ps1 new makes it; release.yml gets it from the MOKU_SIGNING_KEY secret)"
}
if ($problems.Count -gt 0) {
    if (-not $Dev) {
        throw ("refusing to package a release:" + [Environment]::NewLine + "  - " + ($problems -join ([Environment]::NewLine + "  - ")) + [Environment]::NewLine + "(pass -Dev for an unofficial development package)")
    }
    foreach ($p in $problems) { Write-Warning "-Dev: $p" }
    Write-Warning "-Dev: this package is for testing only (its updater is off); do not publish it"
}

# ---- the identity package (optional) ---------------------------------------------------------------
$makeappx = $null
if ($SparsePackage) {
    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    foreach ($c in @($env:MAKEAPPX) + @(Get-ChildItem $kits -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending | ForEach-Object { Join-Path $_.FullName "x64\makeappx.exe" })) {
        if ($c -and (Test-Path -LiteralPath $c)) { $makeappx = $c; break }
    }
    if (-not $makeappx) { throw "-SparsePackage needs makeappx.exe (Windows SDK, or set MAKEAPPX)" }
    if ($PackageCert -and -not (Test-Path -LiteralPath $PackageCert)) { throw "-PackageCert $PackageCert does not exist" }
    if (-not $Publisher -and $PackageCert) {
        $pw = $env:MOKU_PACKAGE_CERT_PASSWORD
        $cert = if ($pw) { New-Object Security.Cryptography.X509Certificates.X509Certificate2($PackageCert, $pw) } else { New-Object Security.Cryptography.X509Certificates.X509Certificate2($PackageCert) }
        $Publisher = $cert.Subject
    }
    if (-not $Publisher) { throw "-SparsePackage needs -Publisher (or -PackageCert, whose subject is used)" }
    Write-Host "Identity package: publisher $Publisher (makeappx $makeappx)"
}

# Moku.Identity.msix: only the manifest (AllowExternalContent); the files stay in the install
# folder, which is given as the external location when the package is registered (by the
# maintainer / the installer, never by this script).
function New-IdentityPackage([string]$out) {
    New-Item -ItemType Directory -Force $out | Out-Null
    $stage = Join-Path $out "identity-stage"
    New-Item -ItemType Directory -Force $stage | Out-Null
    $manifest = Get-Content (Join-Path $app "packaging\sparse\AppxManifest.xml.in") -Raw
    $fourPart = ($version -replace '-.*$', '').Split('.') + @('0', '0', '0', '0') | Select-Object -First 4
    $manifest = $manifest.Replace("@PACKAGE_NAME@", "Moku.Island").Replace("@APPLICATION_ID@", "MokuIsland").Replace("@VERSION@", ($fourPart -join '.')).Replace("@PUBLISHER@", [Security.SecurityElement]::Escape($Publisher))
    [IO.File]::WriteAllText((Join-Path $stage "AppxManifest.xml"), $manifest, (New-Object System.Text.UTF8Encoding($false)))
    $msix = Join-Path $out "Moku.Identity.msix"
    # /nv: the logo paths in the manifest need not resolve (an identity package has no assets).
    & $makeappx pack /o /d $stage /nv /p $msix | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "makeappx pack failed ($LASTEXITCODE)" }
    Remove-Item $stage -Recurse -Force
    if ($PackageCert) {
        $signtool = Join-Path (Split-Path -Parent $makeappx) "signtool.exe"
        if (-not (Test-Path $signtool)) { throw "signtool.exe is not next to makeappx.exe" }
        $signArgs = @("sign", "/fd", "SHA256", "/f", $PackageCert)
        if ($env:MOKU_PACKAGE_CERT_PASSWORD) { $signArgs += @("/p", $env:MOKU_PACKAGE_CERT_PASSWORD) }
        & $signtool @signArgs $msix | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
    }
    else { Write-Warning "Moku.Identity.msix is NOT signed: sign it with the certificate whose subject is '$Publisher' before it can be registered" }
    Write-Host "wrote $msix"
}
if ($IdentityOnly) {
    if (-not $SparsePackage -or -not $OutDir) { throw "-IdentityOnly needs -SparsePackage and -OutDir" }
    New-IdentityPackage $OutDir
    return
}

# ---- release notes --------------------------------------------------------------------------
$changelog = Join-Path $repo "CHANGELOG.md"
$lines = Get-Content $changelog -Encoding UTF8
$start = -1; $end = $lines.Count
for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match '^## \[?v?([0-9][^\] ]*)\]?') {
        if ($start -ge 0) { $end = $i; break }
        if ($Matches[1] -eq $version) { $start = $i + 1 }
    }
}
if ($start -lt 0) { throw "CHANGELOG.md has no section for $version" }
$notes = ($lines[$start..($end - 1)] -join "`n").Trim()
if (-not $notes) { throw "the CHANGELOG.md section for $version is empty" }

Push-Location $app
try {
    $target = (& cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory
    if (-not $OutDir) { $OutDir = Join-Path $target "dist" }
    $rel = Join-Path $target "release"

    # ---- build ------------------------------------------------------------------------------
    if (-not $SkipTests) { Invoke-Cargo test --workspace --locked }
    # An identity build links the manifest that ties moku.exe to the identity package
    # (build-support\winres.rs reads MOKU_MSIX_PUBLISHER); without -SparsePackage nothing changes.
    if ($SparsePackage) { $env:MOKU_MSIX_PUBLISHER = $Publisher }
    try { Invoke-Cargo build --release --locked -p moku -p moku-settings -p mokuctl }
    finally { Remove-Item Env:MOKU_MSIX_PUBLISHER -ErrorAction SilentlyContinue }

    if (Test-Path $OutDir) { Remove-Item $OutDir -Recurse -Force }
    $payload = Join-Path $OutDir "payload"
    New-Item -ItemType Directory -Force (Join-Path $payload "docs") | Out-Null
    foreach ($exe in "moku.exe", "moku-settings.exe", "mokuctl.exe") {
        Copy-Item (Join-Path $rel $exe) $payload
    }
    Copy-Item (Join-Path $app "docs\claude-code-hook.md") (Join-Path $payload "docs")
    # v0.3: the coding-agent hooks (Codex CLI, Gemini CLI, "always allow") and the notification mirror.
    Copy-Item (Join-Path $app "docs\agents.md") (Join-Path $payload "docs")
    Copy-Item (Join-Path $app "docs\notifications.md") (Join-Path $payload "docs")
    # The Claude Code permission-card hook ships too (docs\claude-code-hook.md points at it); it
    # finds mokuctl.exe in the folder above.
    New-Item -ItemType Directory -Force (Join-Path $payload "tools") | Out-Null
    Copy-Item (Join-Path $app "tools\claude-hook.ps1") (Join-Path $payload "tools")
    # The licence travels with the installer (installed next to the exes) and the portable zip.
    $license = Join-Path $repo "LICENSE"
    if (-not (Test-Path $license)) { throw "LICENSE is missing from the repository root" }
    Copy-Item $license $payload
    # NOTICE carries the GPL section 7 terms (attribution, name and logo): it travels with LICENSE.
    $notice = Join-Path $repo "NOTICE"
    if (-not (Test-Path $notice)) { throw "NOTICE is missing from the repository root" }
    Copy-Item $notice $payload

    $env:MOKU_SETUP_PAYLOAD = $payload
    try { Invoke-Cargo build --release --locked -p moku-setup }
    finally { Remove-Item Env:MOKU_SETUP_PAYLOAD }

    # ---- artifacts --------------------------------------------------------------------------
    $setup = Join-Path $OutDir "moku-setup.exe"
    Copy-Item (Join-Path $rel "moku-setup.exe") $setup
    # The detached signature the updater requires: made with the private key outside the repo,
    # checked against the public key this very build carries.
    if ($canSign) {
        $keyTool = Join-Path $PSScriptRoot "release-key.ps1"
        & $keyTool sign $setup -Key $signingKey
        if ($hasPublicKey) {
            & $keyTool verify $setup
            if ($LASTEXITCODE -ne 0) { throw "the signature does not verify with UPDATE_PUBLIC_KEY in release.rs: the key file and the constant belong to different key pairs" }
        }
    }
    if ($SparsePackage) { New-IdentityPackage $OutDir }
    $hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLowerInvariant()
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::WriteAllText("$setup.sha256", "$hash  moku-setup.exe`n", $utf8)
    [IO.File]::WriteAllText((Join-Path $OutDir "release-notes.md"), "$notes`n", $utf8)

    $zipDir = Join-Path $OutDir "Moku-$version"
    Copy-Item $payload $zipDir -Recurse
    Copy-Item (Join-Path $app "README.md") $zipDir
    Copy-Item $changelog $zipDir
    foreach ($f in "LICENSE", "NOTICE") {
        if (-not (Test-Path (Join-Path $zipDir $f))) { throw "$f did not make it into the portable zip" }
    }
    $zip = Join-Path $OutDir "Moku-$version-portable.zip"
    Compress-Archive -Path (Join-Path $zipDir "*") -DestinationPath $zip
    Remove-Item $zipDir -Recurse -Force
    Remove-Item $payload -Recurse -Force

    Write-Host ""
    Get-ChildItem $OutDir | ForEach-Object { "{0,-34} {1,12:N0} bytes" -f $_.Name, $_.Length } | Write-Host
    Write-Host "sha256 $hash"
}
finally {
    Pop-Location
}
