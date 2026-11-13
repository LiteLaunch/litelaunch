# Builds every plugin under plugins\ (a folder with Cargo.toml) into target\plugins\out\<id>\<id>.dll.
# -Install also puts them into %APPDATA%\LiteLaunch\plugins, restarting a running launcher
# (a loaded DLL can't be overwritten).
param([switch]$Install)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    $ids = Get-ChildItem plugins -Directory | Where-Object { Test-Path (Join-Path $_.FullName 'Cargo.toml') } | ForEach-Object Name
    foreach ($id in $ids) {
        cargo build --release --manifest-path "plugins\$id\Cargo.toml" --target-dir target\plugins
        if ($LASTEXITCODE) { throw "cargo build failed: $id" }
        $out = "target\plugins\out\$id"
        New-Item -ItemType Directory -Force $out | Out-Null
        Copy-Item "target\plugins\release\$id.dll" $out -Force
    }
    if ($Install) {
        $exe = "$env:LOCALAPPDATA\Programs\LiteLaunch\litelaunch.exe"
        $running = Get-Process litelaunch -ErrorAction SilentlyContinue
        if ($running) {
            & $exe --exit
            $running | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue
        }
        foreach ($id in $ids) {
            $dest = "$env:APPDATA\LiteLaunch\plugins\$id"
            New-Item -ItemType Directory -Force $dest | Out-Null
            Copy-Item "target\plugins\out\$id\$id.dll" $dest -Force
        }
        # Through Explorer, so the launcher doesn't inherit this shell's environment.
        if (Test-Path $exe) { explorer.exe $exe }
    }
    Get-ChildItem target\plugins\out\*\*.dll | Select-Object Name, Length
} finally { Pop-Location }
