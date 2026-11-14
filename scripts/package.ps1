# Builds the release exe and writes dist/Mailbox-portable.zip (Mailbox.exe + README + icon).
#
#   powershell -ExecutionPolicy Bypass -File scripts/package.ps1 [-SkipBuild]
#
# Portable means "no installer": unzip anywhere and run Mailbox.exe. Data still lives in
# %APPDATA%\mailbox (or MAILBOX_DATA_DIR / --data). Honors CARGO_TARGET_DIR. Publishes nothing.
param([switch]$SkipBuild)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
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
    Copy-Item $exe (Join-Path $stage 'Mailbox.exe')
    Copy-Item (Join-Path $root 'README.md'), (Join-Path $root 'assets\icon.ico') $stage

    $zip = Join-Path $dist 'Mailbox-portable.zip'
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -Force
    $size = [math]::Round((Get-Item $zip).Length / 1MB, 1)
    Write-Output "Wrote $zip ($size MB)"
}
finally {
    Pop-Location
}
