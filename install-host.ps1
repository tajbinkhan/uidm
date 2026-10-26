# Registers the UIDM native-messaging host with Chrome and Edge for the
# current user. Run after loading the test extension, because you need its ID.
#
#   .\install-host.ps1 -ExtensionId <id-from-chrome://extensions>
#
# Re-run it whenever the extension ID or the exe path changes.

param(
    [Parameter(Mandatory = $true)]
    [string]$ExtensionId
)

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$exe = Join-Path $root "target\release\uidm-host.exe"
$manifestPath = Join-Path $root "host\com.uidm.host.json"
$hostName = "com.uidm.host"

if (-not (Test-Path $exe)) {
    Write-Host "Building release binary..."
    & cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

# Write the manifest with the real exe path and this extension's origin.
# Native-messaging JSON needs forward or escaped-backslash paths.
$manifest = [ordered]@{
    name            = $hostName
    description     = "UIDM download manager native-messaging host"
    type            = "stdio"
    path            = $exe
    allowed_origins = @("chrome-extension://$ExtensionId/")
}
$manifest | ConvertTo-Json -Depth 4 | Set-Content -Path $manifestPath -Encoding utf8
Write-Host "Wrote $manifestPath"

# Point each browser at the manifest via a per-user registry key.
$targets = @(
    "HKCU:\Software\Google\Chrome\NativeMessagingHosts\$hostName",
    "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\$hostName"
)
foreach ($key in $targets) {
    New-Item -Path $key -Force | Out-Null
    Set-ItemProperty -Path $key -Name "(default)" -Value $manifestPath
    Write-Host "Registered $key"
}

Write-Host "Done. Reload the extension, then open its popup and click a button."
Write-Host "Host stderr goes to the browser's native-messaging log."
