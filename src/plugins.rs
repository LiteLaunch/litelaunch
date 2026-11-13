//! Native plugins: DLLs built with the litelaunch-plugin SDK, one per folder in
//! %APPDATA%\LiteLaunch\plugins\<id>\<id>.dll. Loaded once at startup and never unloaded
//! (installing or removing one takes a restart). Calls go through the SDK's three C functions
//! with JSON inside; a panic in a plugin comes back as an error.

use litelaunch_plugin::ffi::{AbiFn, CallFn, FreeFn};
use litelaunch_plugin::{ABI, After, Host, Info, Item, Reply, Request, Response};
use serde_json::Value;
use std::path::{Path, PathBuf};
use windows::Win32::System::LibraryLoader::*;
use windows::core::{HSTRING, s};

pub struct Native {
    pub id: String,
    pub info: Info,
    /// %APPDATA%\LiteLaunch\<id>.
    pub data_dir: PathBuf,
    call: CallFn,
    free: FreeFn,
}

/// Where plugin DLLs live.
pub fn dir() -> PathBuf {
    crate::config::dir().join("plugins")
}

/// Loads and initializes every plugin in `dir()`. A folder whose DLL is missing, broken or built
/// for another interface version is skipped (and reported in the returned errors).
pub fn load_all() -> (Vec<Native>, Vec<String>) {
    crate::catalog::cleanup();
    let (found, errors) = load_dir();
    for n in &found {
        // Unpacks the plugin's files (milliseconds when nothing changed); before the first query,
        // so its icons exist when the results are drawn.
        n.init();
    }
    (found, errors)
}

/// What `litelaunch --plugin-info` prints: the installed plugins, without initializing them.
/// The plugin manager asks a separate process, so it never holds the DLLs itself.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Installed {
    pub plugins: Vec<(String, Info)>,
    pub errors: Vec<String>,
}

pub fn describe_all() -> Installed {
    let (found, errors) = load_dir();
    Installed { plugins: found.into_iter().map(|n| (n.id, n.info)).collect(), errors }
}

fn load_dir() -> (Vec<Native>, Vec<String>) {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir()) else { return (found, errors) };
    let mut folders: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    folders.sort();
    for folder in folders {
        let id = folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let dll = folder.join(format!("{id}.dll"));
        if !dll.is_file() {
            continue;
        }
        match load(&dll, id.clone(), crate::config::dir().join(&id)) {
            Ok(n) => found.push(n),
            Err(e) => errors.push(format!("{id}: {e}")),
        }
    }
    (found, errors)
}

fn load(dll: &Path, id: String, data_dir: PathBuf) -> Result<Native, String> {
    unsafe {
        // The plugin's own folder is searched for its dependencies; the current directory is not.
        let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
        let lib = LoadLibraryExW(&HSTRING::from(dll.as_os_str()), None, flags).map_err(|e| format!("не загрузилась DLL: {e}"))?;
        let abi: AbiFn = std::mem::transmute(GetProcAddress(lib, s!("ll_abi")).ok_or("это не плагин LiteLaunch")?);
        let call: CallFn = std::mem::transmute(GetProcAddress(lib, s!("ll_call")).ok_or("нет ll_call")?);
        let free: FreeFn = std::mem::transmute(GetProcAddress(lib, s!("ll_free")).ok_or("нет ll_free")?);
        let version = abi();
        if version != ABI {
            let _ = windows::Win32::Foundation::FreeLibrary(lib);
            return Err(format!("плагин для интерфейса v{version}, лаунчер знает v{ABI}: обновите их"));
        }
        let mut n = Native { id, info: Info::default(), data_dir, call, free };
        let res = n.send(&Request::Info { host: n.host() });
        n.info = res.info.ok_or_else(|| res.error.unwrap_or_else(|| "не ответил на info".into()))?;
        if n.info.name.is_empty() {
            n.info.name = n.id.clone();
        }
        Ok(n)
    }
}

impl Native {
    /// What the plugin is told with every call; Python is looked up each time (it may have just
    /// been installed).
    pub fn host(&self) -> Host {
        let (python, python_ok) = crate::python::current();
        Host { data_dir: self.data_dir.clone(), python, python_ok }
    }

    fn send(&self, req: &Request) -> Response {
        let bytes = serde_json::to_vec(req).unwrap_or_default();
        let mut len = 0usize;
        unsafe {
            let p = (self.call)(bytes.as_ptr(), bytes.len(), &mut len);
            if p.is_null() {
                return Response { error: Some("пустой ответ".into()), ..Default::default() };
            }
            let res = serde_json::from_slice(std::slice::from_raw_parts(p, len))
                .unwrap_or_else(|e| Response { error: Some(format!("непонятный ответ: {e}")), ..Default::default() });
            (self.free)(p, len);
            res
        }
    }

    /// The user's settings for this plugin, or its defaults.
    pub fn settings<'a>(&'a self, all: &'a std::collections::BTreeMap<String, Value>) -> &'a Value {
        all.get(&self.id).unwrap_or(&self.info.defaults)
    }

    pub fn keyword(&self, settings: &Value) -> String {
        settings.get("keyword").and_then(Value::as_str).unwrap_or_default().to_string()
    }

    pub fn init(&self) {
        self.send(&Request::Init { host: self.host() });
    }

    pub fn query(&self, settings: &Value, q: &str) -> Result<Reply, String> {
        let res = self.send(&Request::Query { host: self.host(), settings: settings.clone(), q: q.to_string() });
        res.reply.ok_or_else(|| res.error.unwrap_or_default())
    }

    pub fn query_slow(&self, settings: &Value, data: Value) -> Result<Vec<Item>, String> {
        let res = self.send(&Request::QuerySlow { host: self.host(), settings: settings.clone(), data });
        res.reply.map(|r| r.items).ok_or_else(|| res.error.unwrap_or_default())
    }

    pub fn cancel(&self) {
        self.send(&Request::Cancel);
    }

    pub fn actions(&self, settings: &Value, item: Item) -> Vec<Item> {
        self.send(&Request::Actions { host: self.host(), settings: settings.clone(), item }).reply.map(|r| r.items).unwrap_or_default()
    }

    /// Runs an action: what to do next and the new settings, if they changed.
    pub fn run(&self, settings: &Value, op: Value) -> Result<(After, Option<Value>), String> {
        let res = self.send(&Request::Run { host: self.host(), settings: settings.clone(), op });
        match (res.after, res.error) {
            (_, Some(e)) => Err(e),
            (after, None) => Ok((after.unwrap_or_default(), res.settings)),
        }
    }
}
