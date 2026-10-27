# LiteLaunch

Native keystroke launcher for Windows, a lightweight replacement for Flow Launcher.
Rust + Win32 + GDI, one ~600 KB exe, ~7 MB private memory (Flow: ~190 MB).

| Query | What it does |
|---|---|
| `tel` | Programs from the Start menu (Win32 and Store), ranked by match and launch count. Ctrl+Shift+Enter — run as admin |
| `2+2*2`, `sqrt(16)^2` | Calculator, Enter copies the result |
| `kill chrome` | Process Killer: processes grouped by exe, sorted by memory; Enter kills all instances |
| `st dead` | Installed Steam games from every library |
| `yad <url>` | Yandex Music downloader, built in: the query is native, Python (`ymd`) runs only for the download. Empty query offers a link from the clipboard; `yad y0_…` saves the token, `yad reset token` clears it |
| `vd <url>` | Video downloader (yt-dlp), built in: the link is checked natively, Python runs only to read the formats and to download. Empty query offers a link from the clipboard |
| `<keyword> …` | Flow Launcher Python plugins (JSON-RPC v1), run only while a query is in flight |

Keys: ↑/↓, PgUp/PgDn, Enter, Tab (complete), Esc, Ctrl+Backspace.

## Config

`%APPDATA%\LiteLaunch\config.json`, created on first run from Flow Launcher's settings
(hotkey, Python path, v1 Python plugins). Tray icon → "Настройки" opens it, "Перезапустить" applies it.

`litelaunch --show [query]` opens the running instance, `litelaunch --exit` closes it.

## yad

Scripts live in `plugins/yad/` (its own repo) and are embedded in the exe; on start they are unpacked to
`%APPDATA%\LiteLaunch\yad` (token in `token.json`, DPAPI-encrypted; deps in `lib`).
On first run the token, settings and `lib` are taken over from the old Flow plugin.
If `lib` is missing, `yad` offers to build it with pip. Check: `python yad.py doctor`.

`plugins/yad/ymd_fast.py` runs ymd's CLI in-process with three replacements: one keep-alive HTTP session,
a pool of `parallel_downloads` tracks (default 3), and a track download that fetches the cover once
per album and streams the file to disk with on-the-fly decryption. 11-track album: 53 s → 19 s,
byte-identical files, peak RAM ~110 MB (was ~97 MB + a second Python process).

## vd

Replaces Flow's AnyVideo Downloader (its keyword and settings are taken over; yt-dlp is copied from
its `lib` on first run). `plugins/vd/vd.py` is embedded and unpacked to `%APPDATA%\LiteLaunch\vd`.
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
plugins/yad/    submodule: litelaunch-yad (yad.rs, compiled in, + its Python worker)
plugins/vd/     submodule: litelaunch-vd  (vd.rs, compiled in, + its Python worker)
plugins/_archive/litelaunch-vpn/   submodule: the dropped VPN plugin (archived, not built)
assets/         the app icon
```

Plugins live in their own repositories and are pulled in as git submodules:
[litelaunch-yad](https://github.com/Slava-Gusak/litelaunch-yad),
[litelaunch-vd](https://github.com/Slava-Gusak/litelaunch-vd). The dropped VPN plugin is
[litelaunch-vpn](https://github.com/Slava-Gusak/litelaunch-vpn) (archived).

```
git clone --recursive https://github.com/Slava-Gusak/litelaunch.git
git submodule update --remote      # take the plugins' latest commits
```

## Build

```
cargo build --release
```
