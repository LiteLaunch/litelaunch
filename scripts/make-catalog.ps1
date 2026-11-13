# Builds the plugin catalog (plugins.json) from each plugin's plugin.json, Cargo.toml version and
# its built DLL (scripts\build-plugins.ps1 first).
#   default: plugins.json in the repo root, pointing at GitHub releases
#            (https://github.com/LiteLaunch/litelaunch-<id>/releases/download/v<version>/<id>.dll)
#   -Local:  target\plugins\out\plugins.json pointing at the local files, for testing the manager
#            (set "plugin_catalog" in config.json to that file).
param([switch]$Local)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    $abi = [int](Select-String sdk\src\lib.rs -Pattern '^pub const ABI: u32 = (\d+);').Matches[0].Groups[1].Value
    $entries = foreach ($dir in Get-ChildItem plugins -Directory) {
        $manifest = Join-Path $dir.FullName 'plugin.json'
        if (-not (Test-Path $manifest)) { continue }
        $m = Get-Content $manifest -Raw -Encoding UTF8 | ConvertFrom-Json
        $id = $m.id
        $version = (Select-String (Join-Path $dir.FullName 'Cargo.toml') -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
        $dll = Resolve-Path "target\plugins\out\$id\$id.dll"
        $repo = "https://github.com/LiteLaunch/litelaunch-$id"
        if ($Local) {
            $download = $dll.Path
            $icon = (Resolve-Path (Join-Path $dir.FullName 'icon.png')).Path
        } else {
            $download = "$repo/releases/download/v$version/$id.dll"
            $icon = "https://raw.githubusercontent.com/LiteLaunch/litelaunch-$id/main/icon.png"
        }
        [ordered]@{
            id          = $id
            name        = $m.name
            version     = $version
            abi         = $abi
            keyword     = $m.keyword
            description = $m.description
            details     = $m.details
            requires    = @($m.requires)
            author      = $m.author
            repo        = $repo
            icon        = $icon
            download    = $download
            sha256      = (Get-FileHash $dll -Algorithm SHA256).Hash.ToLower()
            size        = (Get-Item $dll).Length
        }
    }
    $out = if ($Local) { 'target\plugins\out\plugins.json' } else { 'plugins.json' }
    $json = [ordered]@{ plugins = @($entries) } | ConvertTo-Json -Depth 5
    # UTF-8 without BOM, whatever the PowerShell version.
    [System.IO.File]::WriteAllText((Join-Path $root $out), $json + "`n", (New-Object System.Text.UTF8Encoding $false))
    Write-Host "$out — $(@($entries).Count) plugins"
} finally { Pop-Location }
