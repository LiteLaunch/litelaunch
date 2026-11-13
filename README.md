<img src="assets/icon-256.png" width="96" align="right" alt="">

# LiteLaunch

Native keystroke launcher for Windows, a lightweight replacement for Flow Launcher.
Rust + Win32 + GDI, one ~900 KB exe, ~7 MB private memory (Flow: ~190 MB).

| Query | What it does |
|---|---|
| `tel` | Programs from the Start menu (Win32 and Store), ranked by match and launch count. Ctrl+Shift+Enter — run as admin |
| `2+2*2`, `sqrt(16)^2` | Calculator, Enter copies the result |
| `kill chrome` | Process Killer: processes grouped by exe, sorted by memory; Enter kills all instances |
| `st dead` | Installed Steam games from every library |
| `yad <url>` | Yandex Music downloader (plugin): the query is native, Python (`ymd`) runs only for the download. Empty query offers a link from the clipboard; `yad y0_…` saves the token, `yad reset token` clears it |
| `vd <url>` | Video downloader (yt-dlp, plugin): the link is checked natively, Python runs only to read the formats and to download. Empty query offers a link from the clipboard |
| `z <name>` | zapret (Flowseal) config picker (plugin): Enter restarts winws.exe with the chosen `general*.bat` (UAC), Right arrow adds it to favorites. Keeps its own zapret in `%APPDATA%\LiteLaunch\zapret` (or a folder you pick), installs and updates it from GitHub keeping your settings; rows for `service.bat` and the folder |
| `<keyword> …` | Python plugins in Flow Launcher's JSON-RPC v1 format (`plugins` in the config), run only while a query is in flight |

Keys: ↑/↓, PgUp/PgDn, Enter, Tab (complete), Esc, Ctrl+Backspace.

## Установка

1. Скачайте `LiteLaunch-<версия>-x64-setup.exe` со страницы [Releases](https://github.com/LiteLaunch/litelaunch/releases/latest)
   и запустите. Права администратора не нужны, нужна Windows 10 или 11 (64-бит).
2. Windows может показать синее окно «Windows защитила ваш компьютер»: установщик не подписан платным
   сертификатом. Нажмите «Подробнее» → «Выполнить в любом случае».
3. В установщике отметьте нужные плагины (их можно поставить и потом) и автозапуск.

Дальше LiteLaunch живёт в трее. **Alt+Space** открывает поиск: пишите название программы, выражение
(`2+2*2`) или ключевое слово плагина (`vd`, `yad`, `z`, `st`, `kill`, `cb`, `>`).

- **Плагины** — `плагины` в поиске или трей → «Плагины…»: установить, обновить, удалить, включить или
  выключить встроенные функции, с описанием каждого.
- **Настройки** — трей → «Настройки (config.json)»: горячая клавиша, тема, ширина окна, ключевые слова.
- **Обновления** LiteLaunch проверяет сам раз в день и сообщает уведомлением; `обновить` в поиске ставит
  новую версию, плагины и настройки сохраняются.
- **Python** нужен плагинам `vd` и `yad`. Если его нет, плагин предложит поставить свой (~25 МБ с
  python.org, только для LiteLaunch, систему не трогает).
- **Удаление** — «Параметры → Приложения → LiteLaunch»; установщик спросит, удалить ли настройки и данные.

## Config

`%APPDATA%\LiteLaunch\config.json`, created with defaults on first run. Tray icon → "Настройки" opens it, "Перезапустить" applies it.

`python`: pythonw.exe for the plugins; if it doesn't exist, LiteLaunch's private Python in `%APPDATA%\LiteLaunch\python`
(the official embeddable build plus pip, installed from a plugin row by `src/python-setup.ps1`), else one on PATH.
`check_updates`: ask GitHub for a new release once a day (`src/update.rs`); an update downloads the setup exe, checks
GitHub's SHA-256 and runs it with `/SILENT /restart=1`.

`litelaunch --show [query]` opens the running instance, `litelaunch --exit` closes it.

## Plugins

`yad`, `vd` and `z` are DLLs in `%APPDATA%\LiteLaunch\plugins\<id>\<id>.dll`, loaded at startup
(installing or removing one takes a restart). The installer offers them as components. Each keeps its settings in
`plugin_settings.<id>` of the config and its data in `%APPDATA%\LiteLaunch\<id>`.

A plugin is a `cdylib` crate built against the SDK in `sdk/` (crate `litelaunch-plugin`): implement the
`Plugin` trait and call `export_plugin!`. The DLL exports three C functions (`ll_abi`, `ll_call`,
`ll_free`) and everything else goes through them as JSON, so no Rust type crosses the boundary and a
panic in a plugin comes back as an error instead of taking the launcher down. Tray notifications from a
plugin's helper process use `<id>.ico` from its data folder.

```
powershell -File scripts\build-plugins.ps1            # → target\plugins\out\<id>\<id>.dll
powershell -File scripts\build-plugins.ps1 -Install   # also into %APPDATA%, restarting the launcher
```

### Plugin manager

Tray → «Плагины…», or type `плагины` in the launcher: `litelaunch --plugins`, a window in its own process
(nothing while closed). It lists installed plugins, the ones in the catalog and the built-in features, with
descriptions, and installs, updates and removes plugins or turns built-ins on and off. Each change stops the
launcher, applies, and starts it again. Downloads go through `curl.exe` and are checked against the catalog's
SHA-256; removing a plugin keeps its data and settings. The manager never loads plugin DLLs itself
(`litelaunch --plugin-info` reports what is installed), so it never locks them.

The catalog is `plugins.json` in this repository (`plugin_catalog` in the config overrides it with a URL or a
local file). Each plugin repo has a `plugin.json` with its name and description; the scripts add version, size and
hash:

```
powershell -File scripts\make-catalog.ps1 -Local     # target\plugins\out\plugins.json with local paths, for testing
powershell -File scripts\release-plugins.ps1          # GitHub release per new plugin version (gh), then plugins.json
```

## yad

Scripts live in `plugins/yad/` (its own repo) and are embedded in the DLL; on start they are unpacked to
`%APPDATA%\LiteLaunch\yad` (token in `token.json`, DPAPI-encrypted; deps in `lib`).
If `lib` is missing, `yad` offers to build it with pip. Check: `python yad.py doctor`.

`plugins/yad/ymd_fast.py` runs ymd's CLI in-process with three replacements: one keep-alive HTTP session,
a pool of `parallel_downloads` tracks (default 3), and a track download that fetches the cover once
per album and streams the file to disk with on-the-fly decryption. 11-track album: 53 s → 19 s,
byte-identical files, peak RAM ~110 MB (was ~97 MB + a second Python process).

## vd

Replaces Flow's AnyVideo Downloader. `plugins/vd/vd.py` is embedded in the DLL and unpacked to `%APPDATA%\LiteLaunch\vd`.
- No Python until the text is a link; results are cached per link for an hour.
- The query saves yt-dlp's unprocessed info to `%TEMP%\LiteLaunch-vd`; the download reuses it
  instead of extracting again (processed info gives 403 on YouTube https formats).
- YouTube is asked with the `visionos` client alone (~2 s instead of ~4.5 s for the default set,
  whose `tv` client solves the player JS in node); default clients if that returns nothing.
- yt-dlp updates itself weekly in the background (`pip --target lib.new`, then folders are swapped).
- `vd.prewarm` (off by default): start Python when `vd ` is typed so a pasted link skips the
  ~0.7 s import; costs ~50 MB for up to 30 s.

## Layout

```
src/            the launcher
sdk/            litelaunch-plugin: the plugin SDK (DLL interface, result types, shared helpers)
plugins/yad/    submodule: litelaunch-yad (yad.dll + its Python worker)
plugins/vd/     submodule: litelaunch-vd  (vd.dll + its Python worker)
plugins/zapret/ submodule: litelaunch-zapret (zapret.dll + update.ps1 and icons)
scripts/        build-plugins.ps1, make-catalog.ps1, release-plugins.ps1
plugins.json    the plugin catalog the manager reads
assets/         the app icon (icon.ico), the tray icon (tray.ico) and their SVG sources
```

Plugins live in their own repositories in the [LiteLaunch](https://github.com/LiteLaunch) organization
and are pulled in as git submodules:
[litelaunch-yad](https://github.com/LiteLaunch/litelaunch-yad),
[litelaunch-vd](https://github.com/LiteLaunch/litelaunch-vd),
[litelaunch-zapret](https://github.com/LiteLaunch/litelaunch-zapret).

```
git clone --recursive https://github.com/LiteLaunch/litelaunch.git
git submodule update --remote      # take the plugins' latest commits
```

## Build

```
cargo build --release                       # the launcher
powershell -File scripts\build-plugins.ps1  # the plugins
```

## Installer

```
powershell -File installer\build.ps1     # → target\installer\LiteLaunch-<version>-x64-setup.exe
```

Needs Inno Setup 6 (`winget install JRSoftware.InnoSetup`). The installer is x64-only and per-user
(no admin): `%LOCALAPPDATA%\Programs\LiteLaunch`, plugins as components (vd ticked by default), Start menu shortcut, optional autostart and desktop
icon. It closes a running instance with `--exit` before updating; the uninstaller removes autostart
and asks whether to delete `%APPDATA%\LiteLaunch`.

## Credits

LiteLaunch grew out of [Flow Launcher](https://github.com/Flow-Launcher/Flow.Launcher) and speaks its
JSON-RPC v1 plugin protocol. The built-in features keep the keywords of the Flow plugins they take over
from; their code is written from scratch:

- `st`: [Steam Search](https://github.com/Garulf/Steam-Search) by Garulf.
- `vd`: [AnyVideo Downloader](https://github.com/z1nc0r3/AnyVideo-Downloader-Flow-Plugin) by z1nc0r3.

Downloads go through [yt-dlp](https://github.com/yt-dlp/yt-dlp) (`vd`) and
[yandex-music-downloader](https://github.com/llistochek/yandex-music-downloader) (`yad`); `z` drives
Flowseal's [zapret-discord-youtube](https://github.com/Flowseal/zapret-discord-youtube).
