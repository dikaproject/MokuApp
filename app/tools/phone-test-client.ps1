<#
.SYNOPSIS
  A test client for the phone transfer ("Kirim ke HP"): plays the phone's part against a test island.
  LOOPBACK ONLY: it refuses any address that is not 127.x.x.x or ::1, so it can never reach a LAN
  address, and it never touches the Windows Firewall.

.DESCRIPTION
  The island must run with MOKU_DEBUG=1, MOKU_DATA_DIR (its own scratch folder),
  MOKU_PHONE_BIND=127.0.0.1 and MOKU_PHONE_DIR (a scratch receive folder). Start a session on the
  island (`mokuctl debug click phone.receive` or `shelf.send`), then run this script with the same
  MOKU_DATA_DIR: the island writes the session address to <data dir>\phone-debug.txt.

  The script announces, then waits for the user's "Terima" (`mokuctl debug click phone.accept`, from
  another shell or -Accept) before it uploads / downloads.

.EXAMPLE
  .\phone-test-client.ps1 -Mode receive -Files C:\scratch\a.txt,C:\scratch\b.png -Text "hello" -Accept
  .\phone-test-client.ps1 -Mode send -OutDir C:\scratch\got -Accept
  .\phone-test-client.ps1 -Mode probe
#>
param(
    [ValidateSet("receive", "send", "probe")][string]$Mode = "receive",
    [string[]]$Files = @(),
    [string]$Text = "",
    [string]$OutDir = "",
    [string]$Url = "",
    [int]$WaitSeconds = 60,
    # Press "Terima" ourselves with `mokuctl debug click phone.accept` once the card is up.
    [switch]$Accept,
    [string]$Mokuctl = ""
)
$ErrorActionPreference = "Stop"
. "$PSScriptRoot\_common.ps1"
Assert-TestIsland
Add-Type -AssemblyName System.Net.Http

if (-not $Url) {
    $f = Join-Path $env:MOKU_DATA_DIR "phone-debug.txt"
    if (-not (Test-Path $f)) { throw "no session address: start a session on the test island first ($f)" }
    $Url = (Get-Content $f -Raw).Trim()
}
$uri = [Uri]$Url
$ip = $null
if (-not [Net.IPAddress]::TryParse($uri.Host.Trim([char[]]"[]"), [ref]$ip) -or -not [Net.IPAddress]::IsLoopback($ip)) {
    throw "refusing $($uri.Host): this client talks to a loopback address only"
}
if (-not $Mokuctl) { $Mokuctl = if ($env:MOKUCTL) { $env:MOKUCTL } else { Join-Path (Get-DefaultBin) "mokuctl.exe" } }

$http = New-Object System.Net.Http.HttpClient
$http.Timeout = [TimeSpan]::FromSeconds(120)
$http.DefaultRequestHeaders.Add("X-Moku", "1")
$base = $Url.TrimEnd("/") + "/"

function Send-Raw([string]$method, [string]$path, $content = $null, [string]$hostHeader = "") {
    $req = New-Object System.Net.Http.HttpRequestMessage ([System.Net.Http.HttpMethod]::new($method), ($base + $path))
    if ($hostHeader) { $req.Headers.Host = $hostHeader }
    if ($content) { $req.Content = $content }
    $http.SendAsync($req).Result
}

function Wait-Approval {
    $until = (Get-Date).AddSeconds($WaitSeconds)
    $clicked = $false
    while ((Get-Date) -lt $until) {
        $r = Send-Raw "GET" "state"
        $j = $r.Content.ReadAsStringAsync().Result | ConvertFrom-Json
        if ($j.state -eq "allowed") { return $true }
        if ($j.state -in "denied", "ended") { return $false }
        if ($Accept -and -not $clicked -and $j.state -eq "pending") {
            Start-Sleep -Milliseconds 400
            & $Mokuctl debug click phone.accept | Out-Null
            $clicked = $true
        }
        Start-Sleep -Milliseconds 300
    }
    return $false
}

$summary = [ordered]@{ mode = $Mode; url = ($uri.Scheme + "://" + $uri.Authority + "/<token>/") }

switch ($Mode) {
    "probe" {
        $tok = $uri.AbsolutePath.Trim("/")
        $probes = [ordered]@{
            "page with token"          = { Send-Raw "GET" "" }
            "no token (/)"             = { $http.GetAsync("$($uri.Scheme)://$($uri.Authority)/").Result }
            "wrong token"              = { $http.GetAsync("$($uri.Scheme)://$($uri.Authority)/" + ("A" * 22) + "/").Result }
            "traversal"                = { $http.GetAsync("$($uri.Scheme)://$($uri.Authority)/" + $tok + "/f/../../x").Result }
            "wrong Host header"        = { Send-Raw "GET" "" -hostHeader "evil.example" }
            "POST without X-Moku"      = { $c = New-Object System.Net.Http.StringContent "{}"; $req = New-Object System.Net.Http.HttpRequestMessage ([System.Net.Http.HttpMethod]::Post), ($base + "offer"); $req.Content = $c; $h2 = New-Object System.Net.Http.HttpClient; $h2.SendAsync($req).Result }
            "download before approval" = { Send-Raw "GET" "f/0" }
        }
        $out = [ordered]@{}
        foreach ($k in $probes.Keys) { $out[$k] = [int](& $probes[$k]).StatusCode }
        $summary.probes = $out
    }
    "receive" {
        $items = @(foreach ($p in $Files) { Get-Item -LiteralPath $p })
        $offer = @{ files = @($items | ForEach-Object { @{ name = $_.Name; size = $_.Length } }); text = [bool]$Text } | ConvertTo-Json -Depth 4 -Compress
        $r = Send-Raw "POST" "offer" (New-Object System.Net.Http.StringContent $offer, ([Text.Encoding]::UTF8), "application/json")
        $summary.offer = [int]$r.StatusCode
        $summary.approved = Wait-Approval
        if ($summary.approved) {
            $results = @()
            for ($i = 0; $i -lt $items.Count; $i++) {
                $mp = New-Object System.Net.Http.MultipartFormDataContent
                $sc = New-Object System.Net.Http.StreamContent ([IO.File]::OpenRead($items[$i].FullName))
                $mp.Add($sc, "file", $items[$i].Name)
                $results += [int](Send-Raw "POST" "up/$i" $mp).StatusCode
                $mp.Dispose()
            }
            $summary.uploads = $results
            if ($Text) { $summary.text = [int](Send-Raw "POST" "text" (New-Object System.Net.Http.StringContent $Text, ([Text.Encoding]::UTF8), "text/plain")).StatusCode }
        }
    }
    "send" {
        $r = Send-Raw "POST" "offer" (New-Object System.Net.Http.StringContent "{}", ([Text.Encoding]::UTF8), "application/json")
        $summary.offer = [int]$r.StatusCode
        $summary.approved = Wait-Approval
        if ($summary.approved) {
            if (-not $OutDir) { throw "-OutDir is required for -Mode send" }
            New-Item -ItemType Directory -Force $OutDir | Out-Null
            $got = @()
            for ($i = 0; $i -lt 8; $i++) {
                $d = Send-Raw "GET" "f/$i"
                if ([int]$d.StatusCode -ne 200) { $got += [int]$d.StatusCode; break }
                $name = ($d.Content.Headers.ContentDisposition.FileName).Trim('"')
                $bytes = $d.Content.ReadAsByteArrayAsync().Result
                [IO.File]::WriteAllBytes((Join-Path $OutDir ("dl" + $i + "-" + (Split-Path $name -Leaf))), $bytes)
                $got += $bytes.Length
            }
            $summary.downloaded = $got
        }
    }
}
$http.Dispose()
$summary | ConvertTo-Json -Depth 4
