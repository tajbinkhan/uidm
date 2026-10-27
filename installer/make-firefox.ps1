# Makes the Firefox copy of the extension:
#
#   .\installer\make-firefox.ps1 -Out firefox
#
# Chrome and Edge load the extension folder as it is. Firefox needs another
# manifest: background scripts instead of a service worker, its own extension
# ID, and no Chrome key; Chrome warns about the first two in Manifest V3, so
# they cannot share one file. The copy is the extension folder without its
# tests, with manifest.json changed by extension\firefox.json: each key there
# replaces the manifest's, and null removes it. extension\test\harness.js
# does the same for the tests.

param([Parameter(Mandatory = $true)][string]$Out)

$ErrorActionPreference = "Stop"
$source = Join-Path (Split-Path $PSScriptRoot) "extension"

if (Test-Path $Out) {
    # Only ever replace an earlier copy, never some other folder.
    if ((Get-ChildItem $Out) -and -not (Test-Path (Join-Path $Out "manifest.json"))) {
        throw "$Out is not empty and holds no extension"
    }
    Remove-Item -Recurse -Force $Out
}
New-Item -ItemType Directory -Force $Out | Out-Null
Get-ChildItem $source | Where-Object { $_.Name -notin "test", "firefox.json", "manifest.json" } |
    Copy-Item -Destination $Out -Recurse

$manifest = Get-Content -Raw (Join-Path $source "manifest.json") | ConvertFrom-Json
$changes = Get-Content -Raw (Join-Path $source "firefox.json") | ConvertFrom-Json
foreach ($change in $changes.PSObject.Properties) {
    $manifest.PSObject.Properties.Remove($change.Name)
    if ($null -ne $change.Value) {
        $manifest | Add-Member -NotePropertyName $change.Name -NotePropertyValue $change.Value
    }
}
# UTF-8 without a byte order mark, which Windows PowerShell's Set-Content
# would add.
$path = Join-Path (Resolve-Path $Out) "manifest.json"
[IO.File]::WriteAllText($path, ($manifest | ConvertTo-Json -Depth 10))
Write-Host "Wrote the Firefox extension to $Out"
