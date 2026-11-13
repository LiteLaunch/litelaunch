# Installs a private Python for LiteLaunch plugins (yad, vd) into %APPDATA%\LiteLaunch\python:
# the official embeddable build from python.org plus pip. Run by the launcher in a console window
# when a plugin needs Python and none is installed. -Dir overrides the target (tests).
param([string]$Dir = "$env:APPDATA\LiteLaunch\python")
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$version = '3.12.10'
try {
    [Console]::OutputEncoding = [Text.Encoding]::UTF8
    Write-Host "Python $version для плагинов LiteLaunch → $Dir" -ForegroundColor Cyan
    $tmp = Join-Path $env:TEMP "litelaunch-python-$PID"
    New-Item -ItemType Directory -Force $tmp | Out-Null
    $zip = Join-Path $tmp 'python.zip'
    Write-Host 'Скачиваю Python (11 МБ)…'
    curl.exe -fL --progress-bar -o $zip "https://www.python.org/ftp/python/$version/python-$version-embed-amd64.zip"
    if ($LASTEXITCODE) { throw 'не удалось скачать Python' }
    if (Test-Path $Dir) { Remove-Item $Dir -Recurse -Force }
    Expand-Archive $zip $Dir -Force
    # The ._pth file keeps this Python isolated (no PYTHONPATH, no other installs from the registry);
    # `import site` in it turns on site-packages, where pip goes. Plugins add their own folder to
    # sys.path: with ._pth the script's folder isn't added.
    foreach ($pth in Get-ChildItem $Dir -Filter '*._pth') {
        $lines = (Get-Content $pth.FullName) -replace '^#\s*import site', 'import site'
        [IO.File]::WriteAllLines($pth.FullName, $lines)
    }
    Write-Host 'Ставлю pip…'
    $getpip = Join-Path $tmp 'get-pip.py'
    curl.exe -fsSL -o $getpip 'https://bootstrap.pypa.io/get-pip.py'
    if ($LASTEXITCODE) { throw 'не удалось скачать get-pip.py' }
    & "$Dir\python.exe" $getpip --no-warn-script-location --disable-pip-version-check
    if ($LASTEXITCODE) { throw 'pip не установился' }
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host ''
    Write-Host 'Готово. Вернитесь в LiteLaunch: плагин предложит поставить свои библиотеки.' -ForegroundColor Green
} catch {
    Write-Host ''
    Write-Host "Ошибка: $_" -ForegroundColor Red
}
Write-Host 'Окно закроется через 15 секунд…'
Start-Sleep 15
