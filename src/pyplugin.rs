//! Flow Launcher Python plugins, JSON-RPC v1: every call is `pythonw <entry> <request-json>`
//! and the reply is JSON on stdout. Nothing stays resident between queries.

use crate::config::PluginConfig;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::util::CREATE_NO_WINDOW;

#[derive(Clone)]
pub struct PyResult {
    pub title: String,
    pub sub: String,
    /// Absolute icon path, if the result has a local one.
    pub icon: Option<String>,
    pub score: i64,
    pub action: Option<RpcAction>,
}

#[derive(Clone)]
pub struct RpcAction {
    pub method: String,
    pub params: Value,
    pub hide: bool,
}

/// A Flow plugin's settings, sent as "settings" with every request.
pub fn file_settings(p: &PluginConfig) -> Value {
    std::fs::read_to_string(&p.settings_file).ok().and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok()).unwrap_or(json!({}))
}

fn request(method: &str, params: Value, settings: &Value) -> String {
    json!({ "method": method, "parameters": params, "settings": settings }).to_string()
}

fn command(python: &str, p: &PluginConfig, method: &str, params: Value, settings: &Value) -> Command {
    let mut cmd = Command::new(python);
    cmd.arg("-B")
        .arg(Path::new(&p.dir).join(&p.entry))
        .arg(request(method, params, settings))
        .current_dir(&p.dir)
        .env("PYTHONIOENCODING", "utf-8")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null());
    cmd
}

/// Runs a query. The child is parked in `slot` so a newer query can kill it mid-flight.
/// `warm` is an already started process that reads the request from stdin (see vd::warm_up).
pub fn query(
    python: &str,
    p: &PluginConfig,
    text: &str,
    settings: &Value,
    slot: &Arc<Mutex<Option<Child>>>,
    warm: Option<Child>,
) -> Result<Vec<PyResult>, String> {
    let warm = warm.and_then(|mut c| {
        let mut stdin = c.stdin.take()?;
        writeln!(stdin, "{}", request("query", json!([text]), settings)).ok()?;
        Some(c)
    });
    let mut child = match warm {
        Some(c) => c,
        None => command(python, p, "query", json!([text]), settings)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Не удалось запустить Python: {e}"))?,
    };
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    *slot.lock().unwrap() = Some(child);
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let mut stdout = Vec::new();
    let _ = out.read_to_end(&mut stdout);
    let stderr = err_reader.join().unwrap_or_default();
    let status = slot.lock().unwrap().take().and_then(|mut c| c.wait().ok());
    if status.is_none() {
        return Err("cancelled".into());
    }
    let stdout = String::from_utf8_lossy(&stdout);
    // Plugins sometimes print junk before the JSON; take the reply from the first brace.
    let reply = stdout.find('{').and_then(|i| serde_json::from_str::<Value>(stdout[i..].trim()).ok());
    let Some(reply) = reply else {
        let msg = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("пустой ответ").trim().to_string();
        return Err(msg);
    };
    let items = get(&reply, "result").and_then(Value::as_array).cloned().unwrap_or_default();
    Ok(items.iter().map(|r| parse_result(p, r)).collect())
}

/// Runs an action method detached (downloads can take minutes; Flow would wait, we don't need to).
pub fn call(python: &str, p: &PluginConfig, settings: &Value, a: &RpcAction) {
    let _ = command(python, p, &a.method, a.params.clone(), settings).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// Case-insensitive field lookup: Flow accepts both `JsonRPCAction` and `jsonRPCAction` etc.
fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object()?.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
}

fn parse_result(p: &PluginConfig, r: &Value) -> PyResult {
    let s = |k| get(r, k).and_then(Value::as_str).unwrap_or_default().to_string();
    let icon_rel = s("IcoPath");
    let icon = [icon_rel.as_str(), p.icon.as_str()].into_iter().find_map(|i| {
        if i.is_empty() || i.starts_with("http") {
            return None;
        }
        let path = Path::new(&p.dir).join(i.replace('/', "\\"));
        path.exists().then(|| path.to_string_lossy().into())
    });
    let action = get(r, "JsonRPCAction").filter(|a| a.is_object()).map(|a| RpcAction {
        method: get(a, "method").and_then(Value::as_str).unwrap_or_default().to_string(),
        params: get(a, "parameters").cloned().unwrap_or(json!([])),
        hide: !get(a, "dontHideAfterAction").and_then(Value::as_bool).unwrap_or(false),
    });
    PyResult {
        title: s("Title"),
        sub: s("SubTitle"),
        icon,
        score: get(r, "Score").and_then(Value::as_i64).unwrap_or(0),
        action: action.filter(|a| !a.method.is_empty()),
    }
}
