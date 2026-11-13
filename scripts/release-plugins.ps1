# Publishes plugin DLLs as GitHub releases and refreshes plugins.json, the catalog the plugin
# manager reads. For each plugin whose version (Cargo.toml) has no release yet, creates release
# v<version> in LiteLaunch/litelaunch-<id> with <id>.dll attached.
#
# Before: push the plugin's commits (the release is tagged at its default branch).
# After:  commit and push plugins.json in the launcher repository — users see the new version
#         once it is on GitHub.
# Needs the GitHub CLI (gh), logged in with access to the LiteLaunch organization.
param([string[]]$Only)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    & "$PSScriptRoot\build-plugins.ps1" | Out-Null
    foreach ($dir in Get-ChildItem plugins -Directory) {
        if (-not (Test-Path (Join-Path $dir.FullName 'plugin.json'))) { continue }
        $id = (Get-Content (Join-Path $dir.FullName 'plugin.json') -Raw -Encoding UTF8 | ConvertFrom-Json).id
        if ($Only -and $id -notin $Only) { continue }
        $version = (Select-String (Join-Path $dir.FullName 'Cargo.toml') -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
        $repo = "LiteLaunch/litelaunch-$id"
        gh release view "v$version" -R $repo *> $null
        if ($LASTEXITCODE -eq 0) { Write-Host "$id ${version}: already released"; continue }
        gh release create "v$version" "target\plugins\out\$id\$id.dll" -R $repo --title "$id $version" --notes "LiteLaunch plugin $id $version. Install it from the plugin manager (tray → Плагины…)."
        if ($LASTEXITCODE) { throw "gh release create failed: $id" }
        Write-Host "$id ${version}: released"
    }
    & "$PSScriptRoot\make-catalog.ps1"
} finally { Pop-Location }
