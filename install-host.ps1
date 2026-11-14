# Installs UDM for the current user:
#   - builds udm-host, udm-daemon, udm-gui, udm-cli and udm-mcp if they are missing,
#   - registers the native-messaging host with Chrome, Edge and Firefox,
#   - adds a "UDM" Start menu shortcut for the desktop window.
#
#   .\install-host.ps1
#   .\install-host.ps1 -NoShortcut
#   .\install-host.ps1 -ExtensionId <id>[,<id>...]   # only if you changed the manifest key
#
# The extension's manifest carries a fixed public key, so the unpacked
# extension's ID is always epmjakacaejhdhldkpbjnakhfmjjbhfm and you do not
# need to pass one. The host lets in every ID in $ExtensionId: the unpacked
# one first, then the ones the stores give their copies, which must match
# CHROMIUM_EXTENSION_IDS in crates\infrastructure\src\browsers.rs (a test
# there checks it; docs\extension-stores.md). Firefox
# loads a copy of the extension with its own manifest; its ID is
# udm@udm.local. In the repository the two are extension\chromium and
# extension\firefox; a portable copy has them as extension and firefox.
# Run .\install-host.ps1 -Uninstall to remove the registration and shortcut,
# the "Start with Windows" entry and the notification registration.
#
# In a portable copy (one with portable.txt in this folder, see
# docs\portable.md) the manifests go in the copy's Data folder and no Start menu
# shortcut is added. UDM registers with the browsers it finds by itself
# there, so this script is only needed to register again or to -Uninstall.

param(
    [string[]]$ExtensionId = @("epmjakacaejhdhldkpbjnakhfmjjbhfm"),
    [string]$FirefoxId = "udm@udm.local",
    [switch]$NoShortcut,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
# "-ExtensionId a,b" reaches the script as one string under powershell -File,
# so split each on commas.
$ExtensionId = @($ExtensionId | ForEach-Object { $_ -split "," } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$root = $PSScriptRoot
$release = Join-Path $root "target\release"
$hostName = "com.udm.host"
$portable = Test-Path -PathType Leaf (Join-Path $root "portable.txt")
# UDM's data folder, where the daemon writes the manifests too.
$dataDir = if ($portable) { Join-Path $root "Data" } else { Join-Path $env:LOCALAPPDATA "UDM" }
if ($portable) { $NoShortcut = $true }
# Generated per machine, so it lives outside the repo; host\com.udm.host.json
# is only a template.
$manifestPath = Join-Path $dataDir "com.udm.host.json"
# Firefox names allowed extensions by ID instead of by origin, so it gets its
# own manifest.
$firefoxManifestPath = Join-Path $dataDir "com.udm.host.firefox.json"
$firefoxKey = "HKCU:\Software\Mozilla\NativeMessagingHosts\$hostName"
$inRepo = Test-Path -PathType Leaf (Join-Path $root "extension\chromium\manifest.json")
$chromiumExtension = if ($inRepo) { Join-Path $root "extension\chromium" } else { Join-Path $root "extension" }
$firefoxExtension = if ($inRepo) { Join-Path $root "extension\firefox" } else { Join-Path $root "firefox" }
$shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "UDM.lnk"
$browserKeys = @(
    "HKCU:\Software\Google\Chrome\NativeMessagingHosts\$hostName",
    "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\$hostName"
)
# Options can register these browsers too; only removed here.
$otherBrowserKeys = @(
    "HKCU:\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\$hostName",
    "HKCU:\Software\Chromium\NativeMessagingHosts\$hostName"
)
# Written by the daemon so Windows shows UDM's notifications; only removed.
$registryKeys = $browserKeys + $otherBrowserKeys + $firefoxKey + "HKCU:\Software\Classes\AppUserModelId\UDM.DownloadManager"
# Written by the daemon when "Start with Windows" is on.
$runKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$runValue = "UDM"

if ($Uninstall) {
    foreach ($key in $registryKeys) {
        if (Test-Path $key) { Remove-Item $key -Force; Write-Host "Removed $key" }
    }
    if (Get-ItemProperty -Path $runKey -Name $runValue -ErrorAction SilentlyContinue) {
        Remove-ItemProperty -Path $runKey -Name $runValue
        Write-Host "Removed the Start with Windows entry"
    }
    if (Test-Path $shortcut) { Remove-Item $shortcut -Force; Write-Host "Removed $shortcut" }
    foreach ($file in $manifestPath, $firefoxManifestPath) {
        if (Test-Path $file) { Remove-Item $file -Force; Write-Host "Removed $file" }
    }
    Write-Host "Done. Your downloaded files were left in place."
    return
}

$exes = "udm-host.exe", "udm-daemon.exe", "udm-gui.exe", "udm-cli.exe", "udm-mcp.exe" | ForEach-Object { Join-Path $release $_ }
if ($exes | Where-Object { -not (Test-Path $_) }) {
    Write-Host "Building release binaries..."
    Push-Location $root
    try {
        & cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    } finally {
        Pop-Location
    }
}

# The host manifest points the browser at udm-host.exe and names the
# extensions allowed to launch it.
$manifest = [ordered]@{
    name            = $hostName
    description     = "UDM download manager native-messaging host"
    type            = "stdio"
    path            = Join-Path $release "udm-host.exe"
    allowed_origins = @($ExtensionId | ForEach-Object { "chrome-extension://$_/" })
}
New-Item -ItemType Directory -Force (Split-Path $manifestPath) | Out-Null
$manifest | ConvertTo-Json -Depth 4 | Set-Content -Path $manifestPath -Encoding utf8
Write-Host "Wrote $manifestPath"

foreach ($key in $browserKeys) {
    New-Item -Path $key -Force | Out-Null
    Set-ItemProperty -Path $key -Name "(default)" -Value $manifestPath
    Write-Host "Registered $key"
}

$firefoxManifest = [ordered]@{
    name               = $hostName
    description        = $manifest.description
    type               = "stdio"
    path               = $manifest.path
    allowed_extensions = @($FirefoxId)
}
$firefoxManifest | ConvertTo-Json -Depth 4 | Set-Content -Path $firefoxManifestPath -Encoding utf8
New-Item -Path $firefoxKey -Force | Out-Null
Set-ItemProperty -Path $firefoxKey -Name "(default)" -Value $firefoxManifestPath
Write-Host "Registered $firefoxKey"

if ($inRepo) {
    & (Join-Path $root "installer\windows\make-firefox.ps1") -Check $firefoxExtension
    if ($LASTEXITCODE -ne 0) { Write-Warning "Firefox would load an old copy of the extension until you run: node installer/make-firefox.js" }
}

if (-not $NoShortcut) {
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($shortcut)
    $link.TargetPath = Join-Path $release "udm-gui.exe"
    $link.WorkingDirectory = $release
    $link.Description = "UDM download manager"
    $link.Save()
    Write-Host "Added Start menu shortcut $shortcut"
}

Write-Host ""
Write-Host "Next: load the extension once."
Write-Host "  1. Open chrome://extensions (or edge://extensions) and turn on Developer mode."
Write-Host "  2. Click 'Load unpacked' and choose $chromiumExtension."
Write-Host "  3. Its ID should read $($ExtensionId[0])."
Write-Host "In Firefox, open about:debugging, choose This Firefox, then Load Temporary Add-on,"
Write-Host "pick $(Join-Path $firefoxExtension 'manifest.json'), and allow it to access all websites."
if ($portable) {
    Write-Host "Then start $(Join-Path $release 'udm-gui.exe'), or just download something in the browser."
} else {
    Write-Host "Then open UDM from the Start menu, or just download something in the browser."
}
