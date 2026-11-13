//! Settings in %APPDATA%\LiteLaunch\config.json, created with defaults on first run.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use crate::util::read_json;
use std::path::PathBuf;

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
    /// Shell: `> command` in PowerShell 7 / Windows Terminal; empty turns it off.
    pub shell_keyword: String,
    /// Clipboard history: `cb words`; empty turns it off (and stops recording).
    pub clip_keyword: String,
    /// pythonw.exe used for the plugins.
    pub python: String,
    /// Python plugins speaking Flow Launcher's JSON-RPC v1 (one process per call).
    pub plugins: Vec<PluginConfig>,
    /// Settings of the native (DLL) plugins by plugin id, e.g. "yad": {"keyword": "yad", ...}.
    /// A plugin missing here gets its defaults written on first start.
    pub plugin_settings: BTreeMap<String, Value>,
    /// Where the plugin manager takes the list of plugins from: a URL or a local plugins.json;
    /// empty for the official one on GitHub.
    pub plugin_catalog: String,
    /// Look for a new LiteLaunch on GitHub once a day.
    pub check_updates: bool,
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
    /// A JSON file sent as "settings" with every request (Flow keeps one per plugin).
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
            shell_keyword: ">".into(),
            clip_keyword: "cb".into(),
            python: "pythonw".into(),
            plugins: Vec::new(),
            plugin_settings: BTreeMap::new(),
            plugin_catalog: String::new(),
            check_updates: true,
        }
    }
}

pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("LiteLaunch")
}

pub fn path() -> PathBuf {
    dir().join("config.json")
}

pub fn load() -> Config {
    let raw = read_json(&path());
    let mut cfg = raw.clone().and_then(|v| serde_json::from_value::<Config>(v).ok()).unwrap_or_default();
    // Before plugins became DLLs their settings sat at the top level ("yad": {...}).
    // Saving drops those keys, so the old file is kept once.
    if raw.as_ref().is_some_and(|r| r.get("plugin_settings").is_none() && r.get("yad").is_some()) {
        let _ = std::fs::copy(path(), dir().join("config.before-dll-plugins.json"));
    }
    for id in ["yad", "vd", "zapret"] {
        if let Some(old) = raw.as_ref().and_then(|r| r.get(id)).filter(|v| v.is_object()) {
            cfg.plugin_settings.entry(id.to_string()).or_insert_with(|| old.clone());
        }
    }
    cfg.input_font_size = cfg.input_font_size.clamp(10.0, 40.0);
    if cfg.input_height > 0.0 {
        // At least room for the text itself.
        cfg.input_height = cfg.input_height.clamp(cfg.input_font_size * 1.6, 200.0);
    }
    save(&cfg);
    cfg
}

pub fn save(cfg: &Config) {
    let _ = std::fs::create_dir_all(dir());
    if let Ok(s) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(path(), s);
    }
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
