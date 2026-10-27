# Builds UIDM's Windows installer, dist\UIDM-Setup-<version>.exe.
#
#   .\installer\build.ps1              # build the programs, then the installer
#   .\installer\build.ps1 -SkipBuild   # the programs in target\release are current
#
# Needs Inno Setup 6 (winget install JRSoftware.InnoSetup). The version comes
# from Cargo.toml.

param([switch]$SkipBuild)

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot

$match = Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $match) { throw "no version in Cargo.toml" }
$version = $match.Matches[0].Groups[1].Value

if (-not $SkipBuild) {
    Push-Location $root
    try {
        & cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    } finally {
        Pop-Location
    }
}

# Setup installs this beside the extension folder, for Firefox.
& (Join-Path $PSScriptRoot "make-firefox.ps1") -Out (Join-Path $root "firefox")

$iscc = Get-Command ISCC.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
if (-not $iscc) {
    $iscc = @(
        (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"),
        (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"),
        (Join-Path $env:ProgramFiles "Inno Setup 6\ISCC.exe")
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $iscc) { throw "Inno Setup 6 is not installed (winget install JRSoftware.InnoSetup)" }

& $iscc /Q "/DAppVersion=$version" (Join-Path $PSScriptRoot "uidm.iss")
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }
Write-Host "Built $(Join-Path $root "dist\UIDM-Setup-$version.exe")"
