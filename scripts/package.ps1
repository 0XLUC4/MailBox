# Builds the release exe and writes the release assets to dist/ (see README, "Releasing"):
#   dist/Mailbox-<version>-windows-x64.exe   the release mailbox.exe (icon embedded)
#   dist/latest.json                          version, notes, exe URL, SHA-256 and size
#                                             (read by the in-app updater)
#
#   powershell -ExecutionPolicy Bypass -File scripts/package.ps1 [-SkipBuild] [-Notes <text>]
#
# Portable means "no installer": run the exe from anywhere. Data still lives in
# %APPDATA%\mailbox (or MAILBOX_DATA_DIR / --data). Honors CARGO_TARGET_DIR. Publishes nothing.
param(
    [switch]$SkipBuild,
    [string]$Notes = ''
)

$ErrorActionPreference = 'Stop'
$repo = '0XLUC4/MailBox'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    # The workspace version ([workspace.package] in Cargo.toml) names the release.
    $manifest = Get-Content (Join-Path $root 'Cargo.toml') -Raw
    if ($manifest -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"(\d+\.\d+\.\d+)"') {
        throw 'version not found in [workspace.package] of Cargo.toml'
    }
    $version = $Matches[1]

    if (-not $SkipBuild) {
        cargo build --release -p mailbox
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    }
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
    $exe = Join-Path $target 'release\mailbox.exe'
    if (-not (Test-Path $exe)) { throw "$exe not found: build first" }

    $dist = Join-Path $root 'dist'
    New-Item -ItemType Directory -Force $dist | Out-Null
    $name = "Mailbox-$version-windows-x64.exe"
    $asset = Join-Path $dist $name
    Copy-Item $exe $asset -Force

    $file = Get-Item $asset
    $latest = [ordered]@{
        version   = $version
        pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd'T'HH:mm:ss'Z'")
        notes     = $Notes.Trim()
        platforms = [ordered]@{
            'windows-x86_64' = [ordered]@{
                url    = "https://github.com/$repo/releases/download/v$version/$name"
                sha256 = (Get-FileHash -Algorithm SHA256 $asset).Hash.ToLowerInvariant()
                size   = $file.Length
            }
        }
    }
    # UTF-8 without BOM.
    $json = $latest | ConvertTo-Json -Depth 5
    [System.IO.File]::WriteAllText((Join-Path $dist 'latest.json'), "$json`n")

    $size = [math]::Round($file.Length / 1MB, 1)
    Write-Output "Wrote $asset ($size MB) and latest.json"
}
finally {
    Pop-Location
}
