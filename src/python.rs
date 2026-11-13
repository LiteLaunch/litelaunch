//! Which Python plugins run with: the one in the config if it exists, else LiteLaunch's private
//! Python in %APPDATA%\LiteLaunch\python (installed on request by python-setup.ps1: the official
//! embeddable build plus pip), else Python from PATH. Users without Python get a row offering
//! to install the private one (litelaunch_plugin::Action::InstallPython).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SETUP: &[u8] = include_bytes!("python-setup.ps1");

/// The configured value ("pythonw" by default), set once at startup.
static CONFIGURED: Mutex<String> = Mutex::new(String::new());
/// The last answer, kept once Python is found (until the config changes, which takes a restart).
static FOUND: Mutex<Option<String>> = Mutex::new(None);

pub fn init(configured: &str) {
    *CONFIGURED.lock().unwrap() = configured.trim().to_string();
}

pub fn private_dir() -> PathBuf {
    crate::config::dir().join("python")
}

/// The pythonw.exe to run plugins with, and whether it exists. When none does, the private
/// Python's path (where it will be) and false.
pub fn current() -> (String, bool) {
    if let Some(p) = FOUND.lock().unwrap().clone() {
        return (p, true);
    }
    let found = resolve();
    if let Some(p) = &found {
        *FOUND.lock().unwrap() = Some(p.clone());
    }
    match found {
        Some(p) => (p, true),
        None => (private_dir().join("pythonw.exe").to_string_lossy().into_owned(), false),
    }
}

fn resolve() -> Option<String> {
    let configured = CONFIGURED.lock().unwrap().clone();
    let configured = if configured.is_empty() { "pythonw".to_string() } else { configured };
    let as_path = Path::new(&configured);
    if as_path.is_absolute() {
        if as_path.is_file() {
            return Some(configured);
        }
    } else if let Some(p) = on_path(&configured) {
        return Some(p);
    }
    let private = private_dir().join("pythonw.exe");
    private.is_file().then(|| private.to_string_lossy().into_owned())
}

/// `pythonw` → the first `pythonw.exe` on PATH. Windows' Store stubs (the alias that opens the
/// Store when Python isn't installed) are only python.exe/python3.exe, so a pythonw.exe found in
/// WindowsApps is a real Store install.
fn on_path(name: &str) -> Option<String> {
    let file = if name.to_lowercase().ends_with(".exe") { name.to_string() } else { format!("{name}.exe") };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(&file)).find(|p| {
        let stub = p.to_string_lossy().to_lowercase().contains("\\windowsapps\\") && !file.eq_ignore_ascii_case("pythonw.exe");
        p.is_file() && !stub
    })
    .map(|p| p.to_string_lossy().into_owned())
}

/// Installs the private Python in a console window (download progress, pip output).
pub fn install() {
    use std::os::windows::process::CommandExt;
    let script = crate::config::dir().join("python-setup.ps1");
    let _ = std::fs::create_dir_all(crate::config::dir());
    // Windows PowerShell 5.1 reads a BOM-less script as ANSI: the Russian text needs the BOM.
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(SETUP.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(SETUP));
    let _ = std::fs::write(&script, bytes);
    let _ = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .creation_flags(crate::util::CREATE_NEW_CONSOLE)
        .spawn();
}
