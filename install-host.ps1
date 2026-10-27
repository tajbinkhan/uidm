# Installs UIDM for the current user:
#   - builds uidm-host, uidm-daemon and uidm-gui if they are missing,
#   - registers the native-messaging host with Chrome, Edge and Firefox,
#   - adds a "UIDM" Start menu shortcut for the desktop window.
#
#   .\install-host.ps1
#   .\install-host.ps1 -NoShortcut
#   .\install-host.ps1 -ExtensionId <id>   # only if you changed the manifest key
#
# The extension's manifest carries a fixed public key, so its ID is always
# epmjakacaejhdhldkpbjnakhfmjjbhfm and you do not need to pass one. Firefox
# loads a copy of the extension with its own manifest, which this script makes
# in the firefox folder (with installer\make-firefox.ps1); its ID is
# uidm@uidm.local.
# Run .\install-host.ps1 -Uninstall to remove the registration and shortcut,
# the "Start with Windows" entry and the notification registration.

param(
    [string]$ExtensionId = "epmjakacaejhdhldkpbjnakhfmjjbhfm",
    [string]$FirefoxId = "uidm@uidm.local",
    [switch]$NoShortcut,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$release = Join-Path $root "target\release"
$hostName = "com.uidm.host"
# Generated per machine, so it lives outside the repo; host\com.uidm.host.json
# is only a template.
$manifestPath = Join-Path $env:LOCALAPPDATA "UIDM\com.uidm.host.json"
# Firefox names allowed extensions by ID instead of by origin, so it gets its
# own manifest.
$firefoxManifestPath = Join-Path $env:LOCALAPPDATA "UIDM\com.uidm.host.firefox.json"
$firefoxKey = "HKCU:\Software\Mozilla\NativeMessagingHosts\$hostName"
$firefoxExtension = Join-Path $root "firefox"
$shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "UIDM.lnk"
$browserKeys = @(
    "HKCU:\Software\Google\Chrome\NativeMessagingHosts\$hostName",
    "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\$hostName"
)
# Written by the daemon so Windows shows UIDM's notifications; only removed.
$registryKeys = $browserKeys + $firefoxKey + "HKCU:\Software\Classes\AppUserModelId\UIDM.DownloadManager"
# Written by the daemon when "Start with Windows" is on.
$runKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$runValue = "UIDM"

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

$exes = "uidm-host.exe", "uidm-daemon.exe", "uidm-gui.exe" | ForEach-Object { Join-Path $release $_ }
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

# The host manifest points the browser at uidm-host.exe and names the only
# extension allowed to launch it.
$manifest = [ordered]@{
    name            = $hostName
    description     = "UIDM download manager native-messaging host"
    type            = "stdio"
    path            = Join-Path $release "uidm-host.exe"
    allowed_origins = @("chrome-extension://$ExtensionId/")
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

& (Join-Path $root "installer\make-firefox.ps1") -Out $firefoxExtension

if (-not $NoShortcut) {
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($shortcut)
    $link.TargetPath = Join-Path $release "uidm-gui.exe"
    $link.WorkingDirectory = $release
    $link.Description = "UIDM download manager"
    $link.Save()
    Write-Host "Added Start menu shortcut $shortcut"
}

Write-Host ""
Write-Host "Next: load the extension once."
Write-Host "  1. Open chrome://extensions (or edge://extensions) and turn on Developer mode."
Write-Host "  2. Click 'Load unpacked' and choose $(Join-Path $root 'extension')."
Write-Host "  3. Its ID should read $ExtensionId."
Write-Host "In Firefox, open about:debugging, choose This Firefox, then Load Temporary Add-on,"
Write-Host "pick $(Join-Path $firefoxExtension 'manifest.json'), and allow it to access all websites."
Write-Host "Then open UIDM from the Start menu, or just download something in the browser."
