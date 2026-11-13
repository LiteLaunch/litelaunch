//! The plugin catalog: plugins.json from GitHub (or a local file), and installing, updating and
//! removing plugin DLLs. Used by the plugin manager window (`litelaunch --plugins`).
//!
//! A loaded DLL can't be overwritten or deleted, but it can be renamed: an update or removal moves
//! the old file aside as `<id>.dll.old`, and `cleanup` deletes those once nothing holds them.

use crate::util::CREATE_NO_WINDOW;
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the catalog comes from unless config.json's `plugin_catalog` says otherwise.
pub const DEFAULT_URL: &str = "https://raw.githubusercontent.com/LiteLaunch/litelaunch/main/plugins.json";

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Catalog {
    pub plugins: Vec<Entry>,
}

/// A plugin as the catalog describes it.
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The SDK interface version the DLL is built for (litelaunch_plugin::ABI).
    pub abi: u32,
    pub keyword: String,
    /// One line, for the list.
    pub description: String,
    /// Paragraphs for the details pane (blank lines between them).
    pub details: String,
    /// What else it needs: "Python 3.10+", "права администратора"...
    pub requires: Vec<String>,
    pub author: String,
    /// The source repository.
    pub repo: String,
    /// Icon (PNG): a URL, or a local path in a local catalog.
    pub icon: String,
    /// The DLL: a URL, or a local path in a local catalog.
    pub download: String,
    pub sha256: String,
    pub size: u64,
}

/// A URL or a local file.
fn fetch(src: &str) -> Result<Vec<u8>, String> {
    if !src.starts_with("http://") && !src.starts_with("https://") {
        return std::fs::read(src).map_err(|e| format!("{src}: {e}"));
    }
    // curl.exe ships with Windows 10 1803+ and follows GitHub's release redirects.
    let curl = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into())).join("System32\\curl.exe");
    let out = Command::new(curl)
        .args(["-sSfL", "--max-time", "120", src])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("curl не запустился: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("не удалось скачать {src}") } else { err });
    }
    Ok(out.stdout)
}

pub fn load(src: &str) -> Result<Catalog, String> {
    let bytes = fetch(src)?;
    let text = String::from_utf8_lossy(&bytes);
    serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("список плагинов не читается: {e}"))
}

/// The plugin's icon, downloaded into the cache (a local path is used as is).
pub fn icon(e: &Entry) -> Option<String> {
    if e.icon.is_empty() {
        return None;
    }
    if !e.icon.starts_with("http") {
        return Path::new(&e.icon).is_file().then(|| e.icon.clone());
    }
    let path = crate::config::dir().join("cache").join("icons").join(format!("{}-{}.png", e.id, e.version));
    if !path.is_file() {
        let bytes = fetch(&e.icon).ok()?;
        let _ = std::fs::create_dir_all(path.parent()?);
        std::fs::write(&path, bytes).ok()?;
    }
    Some(path.to_string_lossy().into_owned())
}

pub fn sha256_hex(data: &[u8]) -> String {
    use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    let _ = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) };
    out.iter().map(|b| format!("{b:02x}")).collect()
}

fn dll_path(id: &str) -> PathBuf {
    crate::plugins::dir().join(id).join(format!("{id}.dll"))
}

/// Moves an installed DLL aside (it may be loaded); false if there was none.
fn move_aside(dll: &Path) -> Result<bool, String> {
    if !dll.exists() {
        return Ok(false);
    }
    let old = dll.with_extension("dll.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(dll, &old).map_err(|e| format!("не удалось убрать старую версию: {e}"))?;
    Ok(true)
}

/// Downloads, checks and puts the plugin in place. The launcher picks it up on restart.
pub fn install(e: &Entry) -> Result<(), String> {
    if e.id.is_empty() || e.id.contains(['\\', '/', '.', ':']) {
        return Err(format!("странный id плагина: {:?}", e.id));
    }
    if e.abi != litelaunch_plugin::ABI {
        return Err(format!("{} собран для другой версии LiteLaunch: обновите лаунчер", e.name));
    }
    let bytes = fetch(&e.download)?;
    let hash = sha256_hex(&bytes);
    if !e.sha256.eq_ignore_ascii_case(&hash) {
        return Err(format!("файл не совпал с ожидаемым (SHA-256 {hash}): скачивание повреждено или подменено"));
    }
    let dll = dll_path(&e.id);
    std::fs::create_dir_all(dll.parent().unwrap()).map_err(|err| err.to_string())?;
    let tmp = dll.with_extension("dll.new");
    std::fs::write(&tmp, &bytes).map_err(|err| format!("не удалось записать {}: {err}", tmp.display()))?;
    let had_old = move_aside(&dll)?;
    if let Err(err) = std::fs::rename(&tmp, &dll) {
        // Put the previous version back rather than leave none.
        if had_old {
            let _ = std::fs::rename(dll.with_extension("dll.old"), &dll);
        }
        return Err(format!("не удалось установить: {err}"));
    }
    Ok(())
}

/// Removes the plugin's DLL. Its data (%APPDATA%\LiteLaunch\<id>) and settings stay, so a
/// reinstall picks up where it left off.
pub fn remove(id: &str) -> Result<(), String> {
    move_aside(&dll_path(id))?;
    let _ = std::fs::remove_dir(dll_path(id).parent().unwrap());
    Ok(())
}

/// Deletes DLLs moved aside by earlier updates and removals, and folders left empty.
pub fn cleanup() {
    let Ok(dirs) = std::fs::read_dir(crate::plugins::dir()) else { return };
    for d in dirs.flatten().map(|d| d.path()).filter(|p| p.is_dir()) {
        if let Ok(files) = std::fs::read_dir(&d) {
            for f in files.flatten().map(|f| f.path()) {
                let name = f.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                if name.ends_with(".dll.old") || name.ends_with(".dll.new") {
                    let _ = std::fs::remove_file(&f);
                }
            }
        }
        let _ = std::fs::remove_dir(&d);
    }
}

/// `1.10.2` vs `1.9`: numeric parts compare as numbers.
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |s: &str| s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(a) > parts(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("1.10.0", "1.9.3"));
        assert!(newer("1.0.1", "1.0.0"));
        assert!(!newer("1.0.0", "1.0.0"));
        assert!(!newer("0.9", "1.0"));
    }

    #[test]
    fn sha() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
