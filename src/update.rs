//! Updates of the launcher itself: the latest GitHub release of LiteLaunch/litelaunch, checked
//! once a day in the background (`check_updates` in the config). A newer version is announced
//! once with a notification; "обновить" in the search or the tray menu installs it: the setup
//! exe is downloaded, checked against GitHub's SHA-256 and run silently, which closes the
//! launcher, replaces it and starts it again (`/restart=1`, see installer/litelaunch.iss).

use crate::util::CREATE_NO_WINDOW;
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LATEST: &str = "https://api.github.com/repos/LiteLaunch/litelaunch/releases/latest";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const DAY: u64 = 24 * 3600;

#[derive(Clone)]
pub struct Release {
    pub version: String,
    pub url: String,
    /// From GitHub's asset digest ("sha256:…"), when it gives one.
    pub sha256: Option<String>,
}

/// A newer release, once known.
pub static AVAILABLE: Mutex<Option<Release>> = Mutex::new(None);

/// update.json: when GitHub was last asked, which version was announced, which one ran last.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct State {
    checked: u64,
    announced: String,
    last_run: String,
}

fn state_path() -> PathBuf {
    crate::config::dir().join("update.json")
}

fn load_state() -> State {
    crate::util::read_json(&state_path()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

fn save_state(s: &State) {
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(state_path(), text);
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn curl() -> Command {
    let mut c = Command::new(PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into())).join("System32\\curl.exe"));
    c.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null());
    c
}

/// The latest release on GitHub, whatever its version.
fn latest() -> Result<Release, String> {
    // For testing an update without publishing: a file:// URL of a release JSON.
    let url = std::env::var("LITELAUNCH_UPDATE_URL").unwrap_or_else(|_| LATEST.to_string());
    let out = curl()
        .args(["-sSfL", "--max-time", "20", "-H", "Accept: application/vnd.github+json", &url])
        .output()
        .map_err(|e| format!("curl не запустился: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let version = v["tag_name"].as_str().unwrap_or_default().trim_start_matches('v').to_string();
    let asset = v["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"].as_str().is_some_and(|n| n.to_lowercase().ends_with("-x64-setup.exe"))))
        .ok_or("в релизе нет установщика")?;
    Ok(Release {
        version,
        url: asset["browser_download_url"].as_str().unwrap_or_default().to_string(),
        sha256: asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_lowercase),
    })
}

/// Asks GitHub now. Ok(Some) when a newer version is out (also kept in AVAILABLE).
pub fn check_now() -> Result<Option<Release>, String> {
    let r = latest()?;
    let newer = crate::catalog::newer(&r.version, CURRENT).then_some(r);
    let mut s = load_state();
    s.checked = now();
    if let Some(r) = &newer {
        // Whoever asked shows it; the background check won't announce it again.
        s.announced = r.version.clone();
    }
    save_state(&s);
    *AVAILABLE.lock().unwrap() = newer.clone();
    Ok(newer)
}

/// Background checks: a minute after start, then whenever a day has passed since the last one.
/// `announce` gets a newer version once (not again for the same version).
pub fn spawn_checker(enabled: bool, announce: impl Fn(&Release) + Send + 'static) {
    if !enabled {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(60));
        loop {
            let before = load_state();
            if now().saturating_sub(before.checked) >= DAY
                && let Ok(Some(r)) = check_now()
                && before.announced != r.version
            {
                announce(&r);
            }
            std::thread::sleep(Duration::from_secs(3 * 3600));
        }
    });
}

/// The version that ran before this one, if it was different (shown as "обновлён до …").
pub fn previous_run() -> Option<String> {
    let mut s = load_state();
    let prev = std::mem::replace(&mut s.last_run, CURRENT.to_string());
    save_state(&s);
    (!prev.is_empty() && prev != CURRENT).then_some(prev)
}

/// Downloads and runs the installer. The installer closes this launcher, so on success nothing
/// after this matters; an error comes back for a notification.
pub fn install(r: &Release) -> Result<(), String> {
    let file = std::env::temp_dir().join(format!("LiteLaunch-{}-x64-setup.exe", r.version));
    let out = curl()
        .args(["-sSfL", "--max-time", "300", "-o"])
        .arg(&file)
        .arg(&r.url)
        .output()
        .map_err(|e| format!("curl не запустился: {e}"))?;
    if !out.status.success() {
        return Err(format!("не удалось скачать: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    if let Some(want) = &r.sha256 {
        let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
        if crate::catalog::sha256_hex(&bytes) != *want {
            let _ = std::fs::remove_file(&file);
            return Err("установщик не совпал с опубликованным (SHA-256): скачивание повреждено".into());
        }
    }
    // /SILENT: only the progress window. /restart=1: start LiteLaunch again when done.
    Command::new(&file)
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/restart=1"])
        .spawn()
        .map_err(|e| format!("установщик не запустился: {e}"))?;
    Ok(())
}
