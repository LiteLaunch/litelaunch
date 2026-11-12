# Builds target\installer\LiteLaunch-<version>-x64-setup.exe
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    cargo build --release --target-dir target
    if ($LASTEXITCODE) { throw 'cargo build failed' }
    $version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
    $iscc = @(
        (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source,
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $iscc) { throw 'Inno Setup 6 not found: winget install JRSoftware.InnoSetup' }
    & $iscc /Q "/DAppVersion=$version" installer\litelaunch.iss
    if ($LASTEXITCODE) { throw 'ISCC failed' }
    Get-ChildItem target\installer\*.exe | Select-Object Name, Length
} finally { Pop-Location }
