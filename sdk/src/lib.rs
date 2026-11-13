//! LiteLaunch plugin SDK. A plugin is a DLL built from a `cdylib` crate that implements
//! [`Plugin`] and calls [`export_plugin!`].
//!
//! The DLL interface is three C functions (see [`ffi`]); everything else travels through them as
//! JSON ([`Request`] in, [`Response`] out). No Rust type crosses the boundary, so the launcher and
//! a plugin may be built by different compiler versions. A panic inside the plugin is caught
//! and comes back as an error; it never unwinds into the launcher.
//!
//! The launcher loads plugins once at startup and never unloads them: installing, updating or
//! removing one takes a restart.

pub mod fuzzy;
pub mod util;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// Version of the DLL interface. The launcher skips plugins built for another one.
pub const ABI: u32 = 1;

/// A result row.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Item {
    pub title: String,
    pub sub: String,
    /// Image, .ico or .exe path to take the icon from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub score: i64,
    pub action: Action,
    /// The file the row is about, for the actions menu ("Показать в папке").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// A Segoe Fluent Icons glyph drawn instead of `icon` (actions menu rows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glyph: Option<char>,
    /// Hide the launcher before running the action (for actions that open a dialog).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hide_first: bool,
}

impl Item {
    pub fn new(title: impl Into<String>, sub: impl Into<String>, icon: Option<String>, score: i64, action: Action) -> Self {
        Self { title: title.into(), sub: sub.into(), icon, score, action, ..Default::default() }
    }
}

/// What Enter on a row does.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    #[default]
    Nothing,
    /// Replaces the query text.
    SetQuery(String),
    /// Copies the text and hides the launcher.
    Copy(String),
    /// Opens a file, folder, URL or `shell:` target.
    Launch(String),
    /// Handed back to the plugin's [`Plugin::run`]: its own `Op`, serialized.
    Plugin(Value),
    /// Installs LiteLaunch's private Python (for plugins whose `Host::python_ok` is false).
    InstallPython,
}

impl Action {
    /// An action that comes back to [`Plugin::run`] as `op`.
    pub fn op<T: Serialize>(op: &T) -> Self {
        Action::Plugin(serde_json::to_value(op).unwrap_or(Value::Null))
    }
}

/// What the launcher does after [`Plugin::run`].
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "snake_case")]
pub enum After {
    Nothing,
    #[default]
    Hide,
    /// Runs the query again (the list changed), keeping the selection.
    Requery,
    SetQuery(String),
}

/// What the launcher tells the plugin with every call.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Host {
    /// The plugin's own data folder, %APPDATA%\LiteLaunch\<id>: unpacked files, tokens, libraries.
    pub data_dir: PathBuf,
    /// pythonw.exe to run Python with: the configured one, LiteLaunch's private one, or one on PATH.
    pub python: String,
    /// False when there is no Python at all: offer [`Action::InstallPython`] instead.
    pub python_ok: bool,
}

/// The row a Python plugin shows when there is no Python yet.
pub fn install_python_item(icon: Option<String>) -> Item {
    Item::new(
        "Установить Python для плагина",
        "Python не найден. Enter скачает его с python.org (~25 МБ) в отдельном окне, только для LiteLaunch",
        icon,
        0,
        Action::InstallPython,
    )
}

/// Describes the plugin (asked once, after loading).
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Info {
    pub name: String,
    pub description: String,
    pub version: String,
    /// The plugin's default settings; the launcher keeps the user's copy in config.json.
    /// A string field `keyword` in them is the query keyword.
    pub defaults: Value,
    /// Icon for the keyword hint row.
    pub icon: Option<String>,
}

/// The answer to a query.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(default)]
pub struct Reply {
    pub items: Vec<Item>,
    /// Slow work to do in the background (a network call, a Python process): the launcher shows
    /// `items` plus a "…" row, waits until typing pauses, then calls [`Plugin::query_slow`] with
    /// this value on another thread. A newer query calls [`Plugin::cancel`] first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slow: Option<Value>,
    /// Subtitle of the "…" row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slow_note: Option<String>,
}

impl From<Vec<Item>> for Reply {
    fn from(items: Vec<Item>) -> Self {
        Reply { items, ..Default::default() }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum Request {
    Info { host: Host },
    /// Once after loading, before any query: unpack files. Keep it fast, it delays startup.
    Init { host: Host },
    Query { host: Host, settings: Value, q: String },
    QuerySlow { host: Host, settings: Value, data: Value },
    Cancel,
    Actions { host: Host, settings: Value, item: Item },
    Run { host: Host, settings: Value, op: Value },
}

/// Every field is optional; which ones are set depends on the request.
#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<Info>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<Reply>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<After>,
    /// The settings after `run`, if it changed them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A plugin. One instance lives for the whole process; calls come from the launcher's UI thread,
/// except `query_slow`, which comes from a background thread.
pub trait Plugin: Default + Send + Sync + 'static {
    /// The plugin's settings, kept in config.json under the plugin's id.
    type Settings: Serialize + DeserializeOwned + Default;
    /// The plugin's own actions ([`Action::op`]).
    type Op: Serialize + DeserializeOwned;

    fn info(&self, host: &Host) -> Info;
    fn init(&self, _host: &Host) {}
    fn query(&self, host: &Host, settings: &Self::Settings, q: &str) -> Reply;
    fn query_slow(&self, _host: &Host, _settings: &Self::Settings, _data: Value) -> Result<Vec<Item>, String> {
        Err("не поддерживается".into())
    }
    fn cancel(&self) {}
    /// Extra rows for an item's actions menu (Right arrow).
    fn actions(&self, _host: &Host, _settings: &Self::Settings, _item: &Item) -> Vec<Item> {
        Vec::new()
    }
    /// Runs one of the plugin's actions. Changes to `settings` are saved by the launcher.
    fn run(&self, host: &Host, settings: &mut Self::Settings, op: Self::Op) -> After;
}

/// The plugin side of a call: request JSON in, response JSON out.
pub fn dispatch<P: Plugin>(p: &P, req: &[u8]) -> Vec<u8> {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(p, req))).unwrap_or_else(|e| {
        let msg = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_default();
        Response { error: Some(format!("сбой плагина: {msg}")), ..Default::default() }
    });
    serde_json::to_vec(&res).unwrap_or_default()
}

fn settings_of<S: DeserializeOwned + Default>(v: Value) -> S {
    serde_json::from_value(v).unwrap_or_default()
}

fn handle<P: Plugin>(p: &P, req: &[u8]) -> Response {
    let req: Request = match serde_json::from_slice(req) {
        Ok(r) => r,
        Err(e) => return Response { error: Some(format!("неверный запрос: {e}")), ..Default::default() },
    };
    let mut res = Response::default();
    match req {
        Request::Info { host } => {
            let mut info = p.info(&host);
            info.defaults = serde_json::to_value(P::Settings::default()).unwrap_or(Value::Null);
            res.info = Some(info);
        }
        Request::Init { host } => p.init(&host),
        Request::Query { host, settings, q } => res.reply = Some(p.query(&host, &settings_of(settings), &q)),
        Request::QuerySlow { host, settings, data } => match p.query_slow(&host, &settings_of(settings), data) {
            Ok(items) => res.reply = Some(items.into()),
            Err(e) => res.error = Some(e),
        },
        Request::Cancel => p.cancel(),
        Request::Actions { host, settings, item } => res.reply = Some(p.actions(&host, &settings_of(settings), &item).into()),
        Request::Run { host, settings, op } => {
            let before = settings.clone();
            let mut s: P::Settings = settings_of(settings);
            match serde_json::from_value::<P::Op>(op) {
                Ok(op) => res.after = Some(p.run(&host, &mut s, op)),
                Err(e) => res.error = Some(format!("неизвестное действие: {e}")),
            }
            let after = serde_json::to_value(&s).unwrap_or(Value::Null);
            if after != before {
                res.settings = Some(after);
            }
        }
    }
    res
}

/// The exported C functions. A buffer returned by `ll_call` must go back to `ll_free`
/// of the same DLL (each DLL has its own allocator).
pub mod ffi {
    pub type AbiFn = unsafe extern "C" fn() -> u32;
    pub type CallFn = unsafe extern "C" fn(req: *const u8, len: usize, out_len: *mut usize) -> *mut u8;
    pub type FreeFn = unsafe extern "C" fn(buf: *mut u8, len: usize);

    /// Moves the response to the caller.
    pub fn leak(v: Vec<u8>, out_len: *mut usize) -> *mut u8 {
        let b = v.into_boxed_slice();
        unsafe { *out_len = b.len() };
        Box::into_raw(b) as *mut u8
    }

    /// # Safety
    /// `buf`/`len` must come from `leak`.
    pub unsafe fn free(buf: *mut u8, len: usize) {
        if !buf.is_null() {
            drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(buf, len)) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Echo;

    #[derive(Serialize, Deserialize, Default, PartialEq)]
    #[serde(default)]
    struct Settings {
        keyword: String,
        count: u32,
    }

    #[derive(Serialize, Deserialize)]
    enum Op {
        Bump,
        Boom,
    }

    impl Plugin for Echo {
        type Settings = Settings;
        type Op = Op;
        fn info(&self, _: &Host) -> Info {
            Info { name: "Echo".into(), ..Default::default() }
        }
        fn query(&self, _: &Host, s: &Settings, q: &str) -> Reply {
            vec![Item::new(format!("{q} {}", s.count), "", None, 0, Action::op(&Op::Bump))].into()
        }
        fn run(&self, _: &Host, s: &mut Settings, op: Op) -> After {
            match op {
                Op::Bump => s.count += 1,
                Op::Boom => panic!("boom"),
            }
            After::Requery
        }
    }

    fn call(req: Request) -> Response {
        serde_json::from_slice(&dispatch(&Echo, &serde_json::to_vec(&req).unwrap())).unwrap()
    }

    #[test]
    fn round_trip() {
        let host = Host::default();
        let info = call(Request::Info { host: host.clone() }).info.unwrap();
        assert_eq!(info.defaults, serde_json::json!({"keyword": "", "count": 0}));
        let settings = serde_json::json!({"count": 2});
        let r = call(Request::Query { host: host.clone(), settings: settings.clone(), q: "hi".into() }).reply.unwrap();
        assert_eq!(r.items[0].title, "hi 2");
        let Action::Plugin(op) = r.items[0].action.clone() else { panic!() };
        let res = call(Request::Run { host: host.clone(), settings, op });
        assert!(matches!(res.after, Some(After::Requery)));
        assert_eq!(res.settings.unwrap()["count"], 3);
    }

    #[test]
    fn panic_is_an_error() {
        let op = serde_json::to_value(Op::Boom).unwrap();
        let res = call(Request::Run { host: Host::default(), settings: Value::Null, op });
        assert!(res.error.unwrap().contains("boom"));
    }
}

/// Exports `ll_abi`, `ll_call` and `ll_free` for the plugin type (which must be `Default`).
#[macro_export]
macro_rules! export_plugin {
    ($t:ty) => {
        static __LL_PLUGIN: ::std::sync::LazyLock<$t> = ::std::sync::LazyLock::new(<$t as ::std::default::Default>::default);

        #[unsafe(no_mangle)]
        pub extern "C" fn ll_abi() -> u32 {
            $crate::ABI
        }

        /// # Safety
        /// `req` must point to `len` readable bytes and `out_len` to a writable usize.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn ll_call(req: *const u8, len: usize, out_len: *mut usize) -> *mut u8 {
            let req = unsafe { ::std::slice::from_raw_parts(req, len) };
            $crate::ffi::leak($crate::dispatch(&*__LL_PLUGIN, req), out_len)
        }

        /// # Safety
        /// The buffer must come from `ll_call` of this DLL.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn ll_free(buf: *mut u8, len: usize) {
            unsafe { $crate::ffi::free(buf, len) }
        }
    };
}
