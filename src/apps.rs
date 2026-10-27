//! Installed programs: the shell's Applications folder (shell:AppsFolder), the same list
//! the Start menu shows — Win32 shortcuts and Store/UWP apps alike.

use windows::Win32::Storage::EnhancedStorage::PKEY_Link_TargetParsingPath;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::UI::Shell::PropertiesSystem::PSGetPropertyKeyFromName;
use windows::core::{Interface, PCWSTR, PWSTR, w};

#[derive(Clone)]
pub struct App {
    pub name: String,
    /// Parsing name: an AppUserModelID or a path. Launch with `shell:AppsFolder\<id>`.
    pub id: String,
    /// Where the program lives, for the subtitle: the exe path, "Microsoft Store", or empty.
    pub location: String,
    /// Store apps: their logo without the colored tile plate (the shell's icon has the plate).
    pub icon: Option<String>,
    /// The program's file, when it is one on disk (for "run as admin", "show in folder").
    pub file: Option<String>,
    /// Part of Windows itself (see `is_system`), matched by "@windows" in the blacklist.
    pub system: bool,
}

impl App {
    pub fn target(&self) -> String {
        format!("shell:AppsFolder\\{}", self.id)
    }
}

/// Enumerates apps. Runs on a worker thread (initializes COM for it).
pub fn scan() -> Vec<App> {
    let mut apps = Vec::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let Ok(folder) = SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None) else {
            return apps;
        };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else {
            return apps;
        };
        loop {
            let mut batch: [Option<IShellItem>; 32] = Default::default();
            let mut fetched = 0;
            let _ = items.Next(&mut batch, Some(&mut fetched));
            if fetched == 0 {
                break;
            }
            for item in batch.iter().take(fetched as usize).flatten() {
                let (Some(name), Some(id)) = (display_name(item, SIGDN_NORMALDISPLAY), display_name(item, SIGDN_PARENTRELATIVEPARSING))
                else {
                    continue;
                };
                if is_noise(&name, &id) {
                    continue;
                }
                let location = location(item, &id);
                let icon = if id.contains('!') { unplated_logo(item) } else { None };
                let file = std::path::Path::new(&location).is_file().then(|| location.clone());
                let system = is_system(item, &id, &location);
                apps.push(App { name, id, location, icon, file, system });
            }
        }
        CoUninitialize();
    }
    // The same program often appears twice (a shortcut path and a registered AppUserModelID);
    // keep one per name, preferring the one that knows where the program lives.
    apps.sort_by_key(|a| (a.name.to_lowercase(), a.location.is_empty(), !a.id.contains('\\')));
    apps.dedup_by(|a, b| a.name.to_lowercase() == b.name.to_lowercase());
    apps
}

fn display_name(item: &IShellItem, kind: SIGDN) -> Option<String> {
    unsafe {
        let p: PWSTR = item.GetDisplayName(kind).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s
    }
}

/// Shortcut target if the app is a shortcut, the path itself if the ID is one
/// (often rooted at a known-folder GUID), "Microsoft Store" for packaged apps.
fn location(item: &IShellItem, id: &str) -> String {
    const KNOWN: &[(&str, &str)] = &[
        ("{6D809377-6AF0-444B-8957-A3773F02200E}", "C:\\Program Files"),
        ("{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}", "C:\\Program Files (x86)"),
        ("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}", "C:\\Windows\\System32"),
        ("{F38BF404-1D43-42F2-9305-67DE0B28FC23}", "C:\\Windows"),
    ];
    let target = unsafe {
        item.cast::<IShellItem2>().ok().and_then(|i2| {
            let p = i2.GetString(&PKEY_Link_TargetParsingPath).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as _));
            s
        })
    };
    if let Some(t) = target.filter(|t| !t.is_empty()) {
        // Steam's Start menu shortcuts point to steam://rungameid/<id>.
        return if t.starts_with("steam://") { "Steam".into() } else { t };
    }
    if id.contains('\\') {
        let mut s = id.to_string();
        for (guid, path) in KNOWN {
            if s.to_uppercase().starts_with(guid) {
                s = format!("{path}{}", &s[guid.len()..]);
            }
        }
        return s;
    }
    if id.contains('!') { "Microsoft Store".into() } else { String::new() }
}

/// Windows' own entries: Store apps published by Microsoft/Windows (Settings, Snipping Tool,
/// Get Started...), shell items (Control Panel, Run), and shortcuts to programs in %WINDIR%
/// (Services, Task Manager...). A shortcut that runs cmd/powershell on someone else's files
/// ("Anaconda Prompt", "Developer Command Prompt for VS") is the other program's, not Windows'.
fn is_system(item: &IShellItem, id: &str, location: &str) -> bool {
    let id = id.to_lowercase();
    // Package family = <name>_<publisher id>: cw5n1h2txyewy is Windows, 8wekyb3d8bbwe Microsoft.
    if let Some((family, _)) = id.split_once('!') {
        return family.ends_with("_cw5n1h2txyewy") || family.ends_with("_8wekyb3d8bbwe");
    }
    if id.starts_with("microsoft.windows.") || id.starts_with("windows.") {
        return true;
    }
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()).to_lowercase();
    if !location.to_lowercase().starts_with(&windir) {
        return false;
    }
    let args = string_property(item, w!("System.Link.Arguments")).unwrap_or_default();
    let args = crate::util::expand_env(&args).to_lowercase();
    // Every path in the arguments must be inside Windows too.
    args.match_indices(":\\").all(|(i, _)| i > 0 && args[i - 1..].starts_with(&windir))
}

/// A string property of a shell item by its canonical name ("System.Tile.SmallLogoPath").
fn string_property(item: &IShellItem, name: PCWSTR) -> Option<String> {
    unsafe {
        let mut key = PROPERTYKEY::default();
        PSGetPropertyKeyFromName(name, &mut key).ok()?;
        let p = item.cast::<IShellItem2>().ok()?.GetString(&key).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.filter(|s| !s.is_empty())
    }
}

/// The Start menu draws Store app logos from `<logo>.targetsize-N_altform-unplated.png`,
/// a transparent variant without the tile plate. Picks the smallest one at least 48 px.
fn unplated_logo(item: &IShellItem) -> Option<String> {
    let dir = string_property(item, w!("System.AppUserModel.PackageInstallPath"))?;
    let logo = string_property(item, w!("System.Tile.SmallLogoPath"))?;
    let logo = std::path::Path::new(&dir).join(logo);
    let stem = logo.file_stem()?.to_string_lossy().to_lowercase();
    let mut sizes: Vec<(u32, std::path::PathBuf)> = std::fs::read_dir(logo.parent()?)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            let rest = name.strip_prefix(&stem)?.strip_prefix(".targetsize-")?;
            let size = rest.strip_suffix("_altform-unplated.png")?.parse().ok()?;
            Some((size, e.path()))
        })
        .collect();
    sizes.sort_by_key(|(s, _)| *s);
    let pick = sizes.iter().find(|(s, _)| *s >= 48).or(sizes.last())?;
    Some(pick.1.to_string_lossy().into())
}

/// Uninstallers, help files and web links that the Start menu also lists.
fn is_noise(name: &str, id: &str) -> bool {
    let n = name.to_lowercase();
    let id = id.to_lowercase();
    n.contains("uninstall")
        || n.contains("un-install")
        || n.contains("удалить")
        || n.contains("удаление")
        || id.starts_with("http://")
        || id.starts_with("https://")
        || id.ends_with(".url")
        || id.ends_with(".chm")
        || id.ends_with(".txt")
        || id.ends_with(".pdf")
        || id.ends_with(".html")
}
