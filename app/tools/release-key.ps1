<#
.SYNOPSIS
  The signing key of Moku releases: ECDSA P-256 (.NET ECDsaCng, Windows CNG). The island's updater
  only runs an installer whose SHA-256 is signed with this key; the public half is a constant in
  app\moku-core\src\release.rs (UPDATE_PUBLIC_KEY).

.DESCRIPTION
  release-key.ps1 new -Out <path>        make a key pair; save the PRIVATE key to <path> (which must be
                                         OUTSIDE the repository; an existing file is not overwritten
                                         without -Force) and print the line to paste into release.rs
  release-key.ps1 sign <file> [-Key <p>] write <file>.sig: the signature of the SHA-256 of <file>, as
                                         128 hex digits (r || s). The key file is -Key, else the file
                                         named by $env:MOKU_SIGNING_KEY. Used by release.ps1.
  release-key.ps1 verify <file> [-PublicKey <hex>]
                                         check <file>.sig against the public key (default: the one in
                                         release.rs). release.ps1 runs this on what it signed.

  The private key file is text ("-----BEGIN MOKU SIGNING KEY-----", the CNG ECCPRIVATE blob as base64,
  "-----END ...-----"). Keep a backup somewhere safe: whoever has it can publish updates that every
  Moku will install, and nobody can sign new releases without it. For GitHub Actions store the
  whole text of the file as the repository secret MOKU_SIGNING_KEY (release.yml writes it to a
  temporary file and points MOKU_SIGNING_KEY at it). Never commit it.

  Works in Windows PowerShell 5.1 and PowerShell 7 on Windows.

.EXAMPLE
  powershell -File app\tools\release-key.ps1 new -Out "$env:USERPROFILE\moku-keys\release.key"
  $env:MOKU_SIGNING_KEY = "$env:USERPROFILE\moku-keys\release.key"
  powershell -File app\tools\release-key.ps1 sign dist\moku-setup.exe
#>
param(
    [Parameter(Mandatory = $true, Position = 0)][ValidateSet("new", "sign", "verify")][string]$Action,
    [Parameter(Position = 1)][string]$File = "",
    [string]$Out = "",
    [string]$Key = "",
    [string]$PublicKey = "",
    [switch]$Force
)
$ErrorActionPreference = "Stop"
$app = Split-Path -Parent $PSScriptRoot
$repo = Split-Path -Parent $app
$ECDSA_P256_PUBLIC_MAGIC = [uint32]0x31534345   # "ECS1"

Add-Type -AssemblyName System.Core -ErrorAction SilentlyContinue
$cngNs = "System.Security.Cryptography"

function ToHex([byte[]]$b) { ($b | ForEach-Object { $_.ToString("x2") }) -join "" }
function FromHex([string]$s) {
    $s = $s.Trim()
    if ($s.Length % 2 -ne 0 -or $s -notmatch '^[0-9a-fA-F]*$') { throw "not a hex string" }
    $o = New-Object byte[] ($s.Length / 2)
    for ($i = 0; $i -lt $o.Length; $i++) { $o[$i] = [Convert]::ToByte($s.Substring(2 * $i, 2), 16) }
    , $o
}

function Get-Sha256([string]$path) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $fs = [IO.File]::OpenRead($path)
        try { , $sha.ComputeHash($fs) } finally { $fs.Dispose() }
    } finally { $sha.Dispose() }
}

function Read-PrivateKey([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) { throw "signing key file not found: $path" }
    $b64 = (Get-Content -LiteralPath $path -Raw) -split "`r?`n" | Where-Object { $_ -and $_ -notmatch '^-----' } | ForEach-Object { $_.Trim() }
    $blob = [Convert]::FromBase64String(($b64 -join ""))
    [System.Security.Cryptography.CngKey]::Import($blob, [System.Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob)
}

# X || Y of the public half of a CNG key (the ECCPUBLIC blob is: magic, cbKey, X, Y).
function Get-PublicXY($cng) {
    $pub = $cng.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPublicBlob)
    if ($pub.Length -ne 72) { throw "unexpected public key blob ($($pub.Length) bytes); is this a P-256 key?" }
    , $pub[8..71]
}

function New-PublicCngKey([byte[]]$xy) {
    $blob = [byte[]]([BitConverter]::GetBytes($ECDSA_P256_PUBLIC_MAGIC) + [BitConverter]::GetBytes([uint32]32) + $xy)
    [System.Security.Cryptography.CngKey]::Import($blob, [System.Security.Cryptography.CngKeyBlobFormat]::EccPublicBlob)
}

function Get-ConfiguredPublicKey {
    $text = Get-Content -LiteralPath (Join-Path $app "moku-core\src\release.rs") -Raw
    if ($text -match 'pub const UPDATE_PUBLIC_KEY:\s*Option<&str>\s*=\s*Some\("([0-9a-fA-F]{128})"\)') { return $Matches[1] }
    return $null
}

switch ($Action) {
    "new" {
        $path = if ($Out) { $Out } else { $File }
        if (-not $path) { throw "usage: release-key.ps1 new -Out <path for the PRIVATE key, outside the repository>" }
        $full = [IO.Path]::GetFullPath($path)
        $root = ([IO.Path]::GetFullPath($repo)).TrimEnd('\') + '\'
        if ($full.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
            throw "refusing to save the private key inside the repository ($root). Choose a path outside it."
        }
        if ((Test-Path -LiteralPath $full) -and -not $Force) { throw "$full exists; pass -Force to replace it (the old key is lost for good)" }
        New-Item -ItemType Directory -Force (Split-Path -Parent $full) | Out-Null

        $p = New-Object System.Security.Cryptography.CngKeyCreationParameters
        $p.ExportPolicy = [System.Security.Cryptography.CngExportPolicies]::AllowPlaintextExport
        # [NullString]::Value, not $null: PowerShell turns $null into "" for a string parameter, and CNG
        # then makes a persisted key named "" ("Object already exists" on the second run).
        $cng = [System.Security.Cryptography.CngKey]::Create([System.Security.Cryptography.CngAlgorithm]::ECDsaP256, [NullString]::Value, $p)
        $priv = $cng.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob)
        $xyHex = ToHex (Get-PublicXY $cng)
        $cng.Dispose()

        $text = "-----BEGIN MOKU SIGNING KEY-----`n" + [Convert]::ToBase64String($priv) + "`n-----END MOKU SIGNING KEY-----`n"
        [IO.File]::WriteAllText($full, $text, (New-Object System.Text.UTF8Encoding($false)))
        # Only the current user may read it.
        try {
            $acl = Get-Acl -LiteralPath $full
            $acl.SetAccessRuleProtection($true, $false)
            $me = [Security.Principal.WindowsIdentity]::GetCurrent().Name
            $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($me, "FullControl", "Allow")))
            Set-Acl -LiteralPath $full -AclObject $acl
        } catch { Write-Warning "could not restrict the key file's permissions: $_" }

        Write-Host ""
        Write-Host "Private key saved to: $full"
        Write-Host "  Back it up somewhere safe and keep it OUT of the repository. Whoever has it can publish"
        Write-Host "  updates every Moku installs; without it nobody can sign a new release."
        Write-Host "  For GitHub Actions: add the whole text of that file as the repository secret MOKU_SIGNING_KEY."
        Write-Host ""
        Write-Host "Public key: in app\moku-core\src\release.rs replace the line"
        Write-Host '    pub const UPDATE_PUBLIC_KEY: Option<&str> = None;'
        Write-Host "  with"
        Write-Host "    pub const UPDATE_PUBLIC_KEY: Option<&str> = Some(`"$xyHex`");"
        Write-Host ""
        Write-Host "Then sign releases with:  `$env:MOKU_SIGNING_KEY = '$full'; app\tools\release.ps1"
    }
    "sign" {
        if (-not $File) { throw "usage: release-key.ps1 sign <file>" }
        if (-not (Test-Path -LiteralPath $File)) { throw "no such file: $File" }
        $keyPath = if ($Key) { $Key } else { $env:MOKU_SIGNING_KEY }
        if (-not $keyPath) { throw "no signing key: pass -Key <file> or set `$env:MOKU_SIGNING_KEY to the key file" }
        $cng = Read-PrivateKey $keyPath
        try {
            $hash = Get-Sha256 $File
            $ecdsa = New-Object System.Security.Cryptography.ECDsaCng $cng
            $sig = $ecdsa.SignHash($hash)
            if ($sig.Length -ne 64) { throw "unexpected signature length $($sig.Length)" }
            $xy = Get-PublicXY $cng
            # Never write a signature that does not verify with the key's own public half.
            $pubKey = New-PublicCngKey $xy
            if (-not (New-Object System.Security.Cryptography.ECDsaCng $pubKey).VerifyHash($hash, $sig)) { throw "the fresh signature does not verify" }
            $pubKey.Dispose()
            $sigPath = "$File.sig"
            [IO.File]::WriteAllText($sigPath, (ToHex $sig) + "`n", (New-Object System.Text.UTF8Encoding($false)))
            Write-Host "signed $File -> $sigPath (sha256 $(ToHex $hash), key $((ToHex $xy).Substring(0, 16))...)"
        } finally { $cng.Dispose() }
    }
    "verify" {
        if (-not $File) { throw "usage: release-key.ps1 verify <file>" }
        $hex = if ($PublicKey) { $PublicKey } else { Get-ConfiguredPublicKey }
        if (-not $hex) { throw "no public key: UPDATE_PUBLIC_KEY is not set in release.rs (and no -PublicKey given)" }
        $sigText = (Get-Content -LiteralPath "$File.sig" -Raw).Trim().TrimStart([char]0xFEFF)
        $sig = FromHex $sigText
        if ($sig.Length -ne 64) { throw "$File.sig is not 128 hex digits" }
        $xy = FromHex $hex
        if ($xy.Length -ne 64) { throw "the public key is not 128 hex digits" }
        $pubKey = New-PublicCngKey $xy
        try {
            $ok = (New-Object System.Security.Cryptography.ECDsaCng $pubKey).VerifyHash((Get-Sha256 $File), $sig)
        } finally { $pubKey.Dispose() }
        if (-not $ok) { Write-Host "BAD signature for $File"; exit 1 }
        Write-Host "signature of $File is good"
    }
}
