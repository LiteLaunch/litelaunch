//! Settings in %APPDATA%\LiteLaunch\config.json. Created on first run; Flow Launcher's
//! Python plugins (JSON-RPC v1) and its Python path are picked up automatically.

pub use crate::vd::VdConfig;
pub use crate::yad::YadConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    /// "Alt+Space", "Ctrl+Shift+K", "Win+F2"...
    pub hotkey: String,
    /// "auto" follows the Windows light/dark mode; "windows" also takes the Windows accent color;
    /// or a fixed "light", "dark", "amoled" (pure black).
    pub theme: String,
    pub max_results: usize,
    /// Window width in DIPs.
    pub width: u32,
    /// Keep the last query when the window reopens (selected, so typing replaces it).
    pub keep_last_query: bool,
    /// Type Latin letters whatever the keyboard layout (switch layouts in the window to override).
    pub force_english: bool,
    /// Query font size in DIPs; the query band keeps Flow Launcher's proportions (3.55x as tall).
    pub input_font_size: f32,
    /// Height of the query band in DIPs, independent of the font; 0 = 3.55x the font size.
    pub input_height: f32,
    /// Apps left out of results: "@windows" (Windows' own apps, see apps::is_system),
    /// or an app's name, its exe name, or a folder it lives in.
    pub blacklist: Vec<String>,
    /// Shown even if blacklisted; same kinds of entries.
    pub whitelist: Vec<String>,
    /// Left of the query: empty for nothing, a path to an image/.ico/.exe, or any text (a symbol, an emoji).
    pub search_icon: String,
    pub kill_keyword: String,
    pub steam_keyword: String,
    /// pythonw.exe used for the plugins.
    pub python: String,
    pub plugins: Vec<PluginConfig>,
    /// Built-in Yandex Music downloader. None only in configs written before it existed.
    pub yad: Option<YadConfig>,
    /// Built-in video downloader (yt-dlp). None only in configs written before it existed.
    pub vd: Option<VdConfig>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct PluginConfig {
    pub name: String,
    pub keyword: String,
    /// Plugin folder (the one with plugin.json).
    pub dir: String,
    /// Script inside `dir`, from plugin.json ExecuteFileName.
    pub entry: String,
    /// Icon inside `dir`.
    pub icon: String,
    /// Flow's Settings.json for this plugin, sent as "settings" with every request.
    pub settings_file: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "Alt+Space".into(),
            theme: "auto".into(),
            max_results: 6,
            width: 680,
            keep_last_query: true,
            force_english: true,
            input_font_size: 18.0,
            input_height: 0.0,
            search_icon: String::new(),
            blacklist: vec!["@windows".into()],
            whitelist: vec!["Command Prompt".into(), "Windows PowerShell".into(), "PowerShell".into()],
            kill_keyword: "kill".into(),
            steam_keyword: "st".into(),
            python: "pythonw".into(),
            plugins: Vec::new(),
            yad: None,
            vd: None,
        }
    }
}

pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("LiteLaunch")
}

pub fn path() -> PathBuf {
    dir().join("config.json")
}

fn flow_dir() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("FlowLauncher")
}

pub fn load() -> Config {
    let mut cfg = std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str::<Config>(&s).ok())
        .unwrap_or_else(import_from_flow);
    cfg.input_font_size = cfg.input_font_size.clamp(10.0, 40.0);
    if cfg.input_height > 0.0 {
        // At least room for the text itself.
        cfg.input_height = cfg.input_height.clamp(cfg.input_font_size * 1.6, 200.0);
    }
    if cfg.yad.is_none() {
        // yad used to be a Flow Python plugin; it is built in now and keeps its Flow settings.
        cfg.plugins.retain(|p| !p.keyword.eq_ignore_ascii_case("yad"));
        let flow = flow_dir().join("Settings").join("Plugins").join(YAD_FLOW_NAME).join("Settings.json");
        let mut yad = YadConfig::default();
        if let Some(v) = read_json(&flow) {
            for (k, d) in [("download_dir", &mut yad.download_dir), ("quality", &mut yad.quality), ("lyrics_format", &mut yad.lyrics_format)] {
                if let Some(x) = v[k].as_str().filter(|x| !x.is_empty()) {
                    *d = x.to_string();
                }
            }
            for (k, d) in [
                ("skip_existing", &mut yad.skip_existing),
                ("open_folder_after", &mut yad.open_folder_after),
                ("playlist_folder", &mut yad.playlist_folder),
                ("only_music", &mut yad.only_music),
                ("stick_to_artist", &mut yad.stick_to_artist),
            ] {
                if let Some(x) = v[k].as_bool() {
                    *d = x;
                }
            }
        }
        cfg.yad = Some(yad);
    }
    if cfg.vd.is_none() {
        cfg.vd = Some(vd_from_flow(&mut cfg));
    }
    save(&cfg);
    cfg
}

/// vd replaces Flow's AnyVideo Downloader: takes over its keyword and settings.
fn vd_from_flow(cfg: &mut Config) -> VdConfig {
    let mut vd = VdConfig::default();
    let flow = cfg.plugins.iter().position(|p| p.name.starts_with(ANYVIDEO_FLOW_NAME)).map(|i| cfg.plugins.remove(i));
    let settings_file = match &flow {
        Some(p) => {
            vd.keyword = p.keyword.clone();
            PathBuf::from(&p.settings_file)
        }
        None => flow_dir().join("Settings").join("Plugins").join(ANYVIDEO_FLOW_NAME).join("Settings.json"),
    };
    let Some(v) = read_json(&settings_file) else { return vd };
    for (k, d) in [
        ("download_path", &mut vd.download_dir),
        ("preferred_video_format", &mut vd.video_format),
        ("preferred_audio_format", &mut vd.audio_format),
        ("sorting_order", &mut vd.sorting_order),
    ] {
        if let Some(x) = v[k].as_str().filter(|x| !x.is_empty()) {
            *d = x.to_string();
        }
    }
    if let Some(x) = v["auto_open_folder"].as_bool() {
        vd.open_folder_after = x;
    }
    if let Some(x) = v["overwrite_existing_files"].as_bool() {
        vd.overwrite = x;
    }
    if v["use_cookie_file"].as_bool() == Some(true) {
        vd.cookie_file = v["cookie_file_path"].as_str().unwrap_or_default().to_string();
    }
    vd
}

const ANYVIDEO_FLOW_NAME: &str = "AnyVideo Downloader";

const YAD_FLOW_NAME: &str = "Yandex Music Downloader";

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(std::fs::read_to_string(path).ok()?.trim_start_matches('\u{feff}')).ok()
}

pub fn save(cfg: &Config) {
    let _ = std::fs::create_dir_all(dir());
    if let Ok(s) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(path(), s);
    }
}

/// First-run defaults: Python path and v1 Python plugins from an installed Flow Launcher.
fn import_from_flow() -> Config {
    let mut cfg = Config::default();
    let flow = flow_dir();
    let settings = read_json(&flow.join("Settings").join("Settings.json")).unwrap_or_default();
    if let Some(py) = settings["PluginSettings"]["PythonExecutablePath"].as_str().filter(|p| Path::new(p).exists()) {
        cfg.python = py.into();
    }
    if let Some(hk) = settings["Hotkey"].as_str() {
        cfg.hotkey = hk.replace(' ', "");
    }
    if let Some(n) = settings["MaxResultsToShow"].as_u64() {
        cfg.max_results = n.clamp(3, 12) as usize;
    }

    let Ok(entries) = std::fs::read_dir(flow.join("Plugins")) else { return cfg };
    for e in entries.flatten() {
        let dir = e.path();
        let Some(meta) = std::fs::read_to_string(dir.join("plugin.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        else {
            continue;
        };
        // Only the per-call v1 protocol; v2 plugins keep a process alive, which is what we avoid.
        if meta["Language"].as_str().map(|l| l.eq_ignore_ascii_case("python")) != Some(true) {
            continue;
        }
        let name = meta["Name"].as_str().unwrap_or_default().to_string();
        if name == YAD_FLOW_NAME || name.starts_with(ANYVIDEO_FLOW_NAME) {
            continue;
        }
        cfg.plugins.push(PluginConfig {
            keyword: meta["ActionKeyword"].as_str().unwrap_or_default().to_string(),
            dir: dir.to_string_lossy().into(),
            entry: meta["ExecuteFileName"].as_str().unwrap_or("main.py").to_string(),
            icon: meta["IcoPath"].as_str().unwrap_or_default().to_string(),
            settings_file: flow.join("Settings").join("Plugins").join(&name).join("Settings.json").to_string_lossy().into(),
            name,
        });
    }
    cfg
}

/// Launch counts per target, used to rank frequently used results higher.
pub struct Usage(pub HashMap<String, u32>);

impl Usage {
    fn path() -> PathBuf {
        dir().join("usage.json")
    }

    pub fn load() -> Self {
        Self(std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
    }

    pub fn bump(&mut self, key: &str) {
        *self.0.entry(key.to_string()).or_default() += 1;
        let _ = std::fs::create_dir_all(dir());
        if let Ok(s) = serde_json::to_string(&self.0) {
            let _ = std::fs::write(Self::path(), s);
        }
    }

    pub fn get(&self, key: &str) -> u32 {
        self.0.get(key).copied().unwrap_or(0)
    }
}

/// Pinned results: an exact query (lowercased, trimmed) -> launch targets in display order.
/// Edited from the window (Ctrl+P, Ctrl+Up/Down), stored in pins.json.
pub struct Pins(pub HashMap<String, Vec<String>>);

impl Pins {
    fn path() -> PathBuf {
        dir().join("pins.json")
    }

    pub fn load() -> Self {
        Self(read_json(&Self::path()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default())
    }

    fn save(&self) {
        let _ = std::fs::create_dir_all(dir());
        if let Ok(s) = serde_json::to_string_pretty(&self.0) {
            let _ = std::fs::write(Self::path(), s);
        }
    }

    fn key(query: &str) -> String {
        query.trim().to_lowercase()
    }

    pub fn get(&self, query: &str) -> &[String] {
        self.0.get(&Self::key(query)).map_or(&[], Vec::as_slice)
    }

    /// Pins `target` at the end of the query's list, or unpins it.
    pub fn toggle(&mut self, query: &str, target: &str) {
        let key = Self::key(query);
        if key.is_empty() {
            return;
        }
        let list = self.0.entry(key.clone()).or_default();
        match list.iter().position(|t| t == target) {
            Some(i) => {
                list.remove(i);
            }
            None => list.push(target.to_string()),
        }
        if list.is_empty() {
            self.0.remove(&key);
        }
        self.save();
    }

    /// Moves a pinned target one place up (-1) or down (+1).
    pub fn shift(&mut self, query: &str, target: &str, delta: isize) {
        let Some(list) = self.0.get_mut(&Self::key(query)) else { return };
        let Some(i) = list.iter().position(|t| t == target) else { return };
        let j = i as isize + delta;
        if j >= 0 && (j as usize) < list.len() {
            list.swap(i, j as usize);
            self.save();
        }
    }
}

/// Results hidden with "Скрыть из результатов" (launch targets), stored in hidden.json.
/// The tray menu brings them back.
pub struct Hidden(pub Vec<String>);

impl Hidden {
    fn path() -> PathBuf {
        dir().join("hidden.json")
    }

    pub fn load() -> Self {
        Self(read_json(&Self::path()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default())
    }

    fn save(&self) {
        let _ = std::fs::create_dir_all(dir());
        if let Ok(s) = serde_json::to_string_pretty(&self.0) {
            let _ = std::fs::write(Self::path(), s);
        }
    }

    pub fn contains(&self, target: &str) -> bool {
        self.0.iter().any(|t| t == target)
    }

    pub fn add(&mut self, target: &str) {
        if !self.contains(target) {
            self.0.push(target.to_string());
            self.save();
        }
    }

    pub fn clear(&mut self) {
        self.0.clear();
        self.save();
    }
}
