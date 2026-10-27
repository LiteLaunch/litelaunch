//! Steam Search: installed games from every library's appmanifest_*.acf.

use std::path::{Path, PathBuf};
use windows::Win32::System::Registry::*;
use windows::core::w;

pub struct Game {
    pub name: String,
    pub appid: String,
    /// Icon source: the game's exe, a Start menu shortcut, or Steam's cached JPEG.
    pub icon: Option<String>,
    /// The game's main exe, if found.
    pub exe: Option<String>,
}

/// Tools that Steam installs alongside games.
const SKIP: &[&str] = &["228980", "1070560", "1391110", "1628350", "1493710", "2180100", "1826330", "961940", "1245040"];

pub fn root() -> Option<PathBuf> {
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Valve\\Steam"),
            w!("SteamPath"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as _),
            Some(&mut size),
        )
    };
    if r.is_err() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    // SteamPath uses forward slashes, which the shell parser rejects.
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len]).replace('/', "\\")))
}

pub fn games() -> Vec<Game> {
    let Some(root) = root() else { return Vec::new() };
    let mut libs = vec![root.clone()];
    if let Ok(vdf) = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf")) {
        for v in values(&vdf, "path") {
            let p = PathBuf::from(v.replace("\\\\", "\\"));
            if !libs.contains(&p) {
                libs.push(p);
            }
        }
    }
    let mut games = Vec::new();
    for lib in libs {
        let Ok(entries) = std::fs::read_dir(lib.join("steamapps")) else { continue };
        for e in entries.flatten() {
            let fname = e.file_name().to_string_lossy().to_string();
            if !(fname.starts_with("appmanifest_") && fname.ends_with(".acf")) {
                continue;
            }
            let Ok(acf) = std::fs::read_to_string(e.path()) else { continue };
            let (Some(appid), Some(name)) = (values(&acf, "appid").next(), values(&acf, "name").next()) else { continue };
            if SKIP.contains(&appid.as_str()) || name.starts_with("Proton") || name.starts_with("Steam Linux Runtime") {
                continue;
            }
            // The game's exe has a proper icon with transparency; Steam's cached one is a JPEG.
            let dir = values(&acf, "installdir").next().map(|d| lib.join("steamapps").join("common").join(d));
            let exe = dir.and_then(|d| game_exe(&d, &name));
            let icon = exe.clone().or_else(|| icon(&root, &appid));
            games.push(Game { name, appid, icon, exe });
        }
    }
    games.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    games
}

/// The main exe in the game folder's root: the one whose name matches the game best
/// (blender.exe over blender-launcher.exe), ties going to the bigger file. Helpers are skipped.
fn game_exe(dir: &Path, name: &str) -> Option<String> {
    const HELPERS: &[&str] = &["crash", "unins", "setup", "redist", "vc_", "dxsetup", "report"];
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().to_lowercase();
            let stem = file.strip_suffix(".exe")?.to_string();
            if HELPERS.iter().any(|h| stem.contains(h)) {
                return None;
            }
            let score = crate::fuzzy::score(name, &stem).unwrap_or(0);
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            Some((score, size, e.path()))
        })
        .max_by_key(|(score, size, _)| (*score, *size))
        .map(|(_, _, p)| p.to_string_lossy().into())
}

/// Values of `"key"		"value"` lines in a VDF/ACF file.
fn values<'a>(text: &'a str, key: &'a str) -> impl Iterator<Item = String> + 'a {
    let quoted = format!("\"{key}\"");
    text.lines().filter_map(move |l| {
        let l = l.trim();
        let rest = l.strip_prefix(quoted.as_str())?.trim();
        Some(rest.strip_prefix('"')?.strip_suffix('"')?.to_string())
    })
}

/// The library cache keeps a small square icon: `<appid>\<sha1>.jpg` (new layout) or `<appid>_icon.jpg` (old).
fn icon(root: &Path, appid: &str) -> Option<String> {
    let cache = root.join("appcache").join("librarycache");
    let old = cache.join(format!("{appid}_icon.jpg"));
    if old.exists() {
        return Some(old.to_string_lossy().into());
    }
    std::fs::read_dir(cache.join(appid)).ok()?.flatten().find_map(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        // The icon is the only file named by its 40-char hash.
        (name.len() == 44 && name.ends_with(".jpg")).then(|| e.path().to_string_lossy().into())
    })
}
