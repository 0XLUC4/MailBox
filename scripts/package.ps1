# Builds the release exe and writes the release assets to dist/ (see README, "Releases"):
#   dist/Mailbox-<version>-windows-x64.zip   mailbox.exe + README + icon
#   dist/SHA256SUMS.txt                       "<sha256>  <zip name>" (checked by the in-app updater)
#
#   powershell -ExecutionPolicy Bypass -File scripts/package.ps1 [-SkipBuild]
#
# Portable means "no installer": unzip anywhere and run mailbox.exe. Data still lives in
# %APPDATA%\mailbox (or MAILBOX_DATA_DIR / --data). Honors CARGO_TARGET_DIR. Publishes nothing.
param([switch]$SkipBuild)

$ErrorActionPreference = 'Stop'
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
    $stage = Join-Path $dist 'Mailbox'
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    New-Item -ItemType Directory -Force $stage | Out-Null
    Copy-Item $exe (Join-Path $stage 'mailbox.exe')
    Copy-Item (Join-Path $root 'README.md'), (Join-Path $root 'assets\icon.ico') $stage

    $name = "Mailbox-$version-windows-x64.zip"
    $zip = Join-Path $dist $name
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -Force
    Remove-Item -Recurse -Force $stage

    $hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
    # LF line ending, no BOM: the format of sha256sum.
    [System.IO.File]::WriteAllText((Join-Path $dist 'SHA256SUMS.txt'), "$hash  $name`n")

    $size = [math]::Round((Get-Item $zip).Length / 1MB, 1)
    Write-Output "Wrote $zip ($size MB) and SHA256SUMS.txt"
}
finally {
    Pop-Location
}
