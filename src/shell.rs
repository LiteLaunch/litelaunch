//! Shell: `> command` runs it in PowerShell 7 inside Windows Terminal, the window stays open.
//! Past commands are kept in %APPDATA%\LiteLaunch\shell_history.json, newest first.

use crate::config::dir;
use crate::fuzzy;
use crate::search::{Action, Item};
use std::path::PathBuf;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

const KEEP: usize = 100;
/// Segoe Fluent Icons "CommandPrompt".
const GLYPH: char = '\u{E756}';

pub struct History(Vec<String>);

impl History {
    fn path() -> PathBuf {
        dir().join("shell_history.json")
    }

    pub fn load() -> Self {
        Self(crate::util::read_json(&Self::path()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default())
    }

    fn save(&self) {
        let _ = std::fs::create_dir_all(dir());
        if let Ok(s) = serde_json::to_string_pretty(&self.0) {
            let _ = std::fs::write(Self::path(), s);
        }
    }

    fn push(&mut self, cmd: &str) {
        self.0.retain(|c| c != cmd);
        self.0.insert(0, cmd.to_string());
        self.0.truncate(KEEP);
        self.save();
    }

    pub fn forget(&mut self, cmd: &str) {
        self.0.retain(|c| c != cmd);
        self.save();
    }
}

/// The query after the keyword: `>dir`, `> dir` and `>` alone all count. None if it is not a shell query.
pub fn strip<'a>(keyword: &str, query: &'a str) -> Option<&'a str> {
    if keyword.is_empty() {
        return None;
    }
    let rest = query.trim_start().strip_prefix(keyword)?;
    // A word keyword ("sh") needs a space after it, so "shell" still finds apps.
    if keyword.chars().all(char::is_alphanumeric) && !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some(rest.trim())
}

pub fn search(history: &History, cmd: &str) -> Vec<Item> {
    let mut items = Vec::new();
    if !cmd.is_empty() {
        items.push(item(cmd, "PowerShell 7 · Windows Terminal · Ctrl+Shift+Enter — от администратора", 10_000));
    }
    let mut past: Vec<Item> = history
        .0
        .iter()
        .enumerate()
        .filter(|(_, c)| c.as_str() != cmd)
        .filter_map(|(i, c)| {
            let score = if cmd.is_empty() { 0 } else { fuzzy::score(c, cmd)? as i64 };
            // Among equal matches the newer command wins.
            Some(item(c, "Из истории", score * 1000 - i as i64))
        })
        .collect();
    past.sort_by(|a, b| b.score.cmp(&a.score));
    items.extend(past);
    items
}

fn item(cmd: &str, sub: &str, score: i64) -> Item {
    let mut it = Item::new(cmd, sub, None, score, Action::Shell(cmd.to_string()));
    it.glyph = Some(GLYPH);
    it
}

/// Opens a terminal tab running `cmd`. -EncodedCommand sidesteps quoting, and Windows Terminal's
/// own `;` separator, whatever the command contains.
pub fn run(history: &mut History, cmd: &str, admin: bool) {
    history.push(cmd);
    let utf16: Vec<u8> = cmd.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let ps = format!("-NoLogo -NoExit -EncodedCommand {}", base64(&utf16));
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let verb = if admin { w!("runas") } else { w!("open") };
    let launch = |file: &str, params: &str| unsafe {
        ShellExecuteW(None, verb, &HSTRING::from(file), &HSTRING::from(params), &HSTRING::from(home.as_str()), SW_SHOWNORMAL).0 as usize > 32
    };
    let shell = if exists_on_path("pwsh.exe") { "pwsh.exe" } else { "powershell.exe" };
    if exists_on_path("wt.exe") && launch("wt.exe", &format!("-d \"{home}\" {shell} {ps}")) {
        return;
    }
    let _ = launch(shell, &ps);
}

fn exists_on_path(exe: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(exe).exists()))
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64() {
        assert_eq!(super::base64(b"Man"), "TWFu");
        assert_eq!(super::base64(b"Ma"), "TWE=");
        assert_eq!(super::base64(b"M"), "TQ==");
    }

    #[test]
    fn strip() {
        assert_eq!(super::strip(">", ">dir"), Some("dir"));
        assert_eq!(super::strip(">", "> git status "), Some("git status"));
        assert_eq!(super::strip(">", ">"), Some(""));
        assert_eq!(super::strip(">", "dir"), None);
        assert_eq!(super::strip("sh", "shell"), None);
        assert_eq!(super::strip("sh", "sh ls"), Some("ls"));
    }
}
