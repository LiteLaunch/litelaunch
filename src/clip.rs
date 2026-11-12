//! Clipboard history: `cb [words]`. Texts copied anywhere are kept, newest first, in
//! %APPDATA%\LiteLaunch\clipboard.json. Enter pastes into the window that was active before
//! the launcher, Shift+Enter only copies. Copies that password managers mark as private are skipped.

use crate::config::dir;
use crate::search::{Action, Item};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{HGLOBAL, HWND};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsWindow, SetForegroundWindow};
use windows::core::w;

const KEEP: usize = 100;
/// Longer copies (a whole file, a log) are not worth keeping.
const MAX_CHARS: usize = 20_000;
/// Segoe Fluent Icons "Paste".
const GLYPH: char = '\u{E77F}';

#[derive(Serialize, Deserialize, Clone)]
pub struct Entry {
    pub text: String,
    /// Unix seconds of the last copy.
    pub at: u64,
}

pub struct History(Vec<Entry>);

impl History {
    fn path() -> PathBuf {
        dir().join("clipboard.json")
    }

    pub fn load() -> Self {
        Self(crate::util::read_json(&Self::path()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default())
    }

    fn save(&self) {
        let _ = std::fs::create_dir_all(dir());
        if let Ok(s) = serde_json::to_string(&self.0) {
            let _ = std::fs::write(Self::path(), s);
        }
    }

    /// A new copy goes on top; copying an old text again moves it there.
    pub fn add(&mut self, text: String) {
        if text.trim().is_empty() || text.chars().count() > MAX_CHARS {
            return;
        }
        if self.0.first().is_some_and(|e| e.text == text) {
            return;
        }
        self.0.retain(|e| e.text != text);
        self.0.insert(0, Entry { text, at: now() });
        self.0.truncate(KEEP);
        self.save();
    }

    pub fn forget(&mut self, text: &str) {
        self.0.retain(|e| e.text != text);
        self.save();
    }

    pub fn clear(&mut self) {
        self.0.clear();
        self.save();
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Newest first; with a query, only entries containing every one of its words.
pub fn search(history: &History, q: &str) -> Vec<Item> {
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    let t = now();
    history
        .0
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            let text = e.text.to_lowercase();
            words.iter().all(|w| text.contains(w.as_str()))
        })
        .map(|(i, e)| {
            let lines = e.text.lines().count();
            let size = if lines > 1 { format!("{lines} строк") } else { format!("{} симв.", e.text.chars().count()) };
            let sub = format!("{} · {size} · Enter — вставить, Shift+Enter — скопировать", ago(t.saturating_sub(e.at)));
            let mut it = Item::new(title(&e.text), sub, None, -(i as i64), Action::Clip(e.text.clone()));
            it.glyph = Some(GLYPH);
            it
        })
        .collect()
}

/// One line: whitespace runs (newlines included) collapsed, cut to a sane length.
fn title(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(150) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

fn ago(s: u64) -> String {
    match s {
        0..60 => "только что".into(),
        60..3600 => format!("{} мин назад", s / 60),
        3600..86400 => format!("{} ч назад", s / 3600),
        _ => format!("{} дн назад", s / 86400),
    }
}

/// The clipboard text after WM_CLIPBOARDUPDATE, unless the copy asked not to be recorded
/// (KeePass, Bitwarden, 1Password... set these formats on passwords).
pub fn read_new() -> Option<String> {
    unsafe {
        let private = [w!("ExcludeClipboardContentFromMonitorProcessing"), w!("Clipboard Viewer Ignore")];
        if private.iter().any(|f| IsClipboardFormatAvailable(RegisterClipboardFormatW(*f)).is_ok()) {
            return None;
        }
        if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_err() {
            return None;
        }
        // The copying app may still hold the clipboard for a moment.
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        if !opened {
            return None;
        }
        let read = |format: u32| -> Option<Vec<u8>> {
            let h = GetClipboardData(format).ok()?;
            let hg = HGLOBAL(h.0);
            let p = GlobalLock(hg) as *const u8;
            if p.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(p, GlobalSize(hg)).to_vec();
            let _ = GlobalUnlock(hg);
            Some(bytes)
        };
        // Windows' own history flag: a DWORD 0 means "keep out of history".
        let flag = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
        let excluded = read(flag).is_some_and(|b| b.len() >= 4 && b[..4] == [0, 0, 0, 0]);
        let text = (!excluded).then(|| read(CF_UNICODETEXT.0 as u32)).flatten().map(|b| {
            let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
            String::from_utf16_lossy(&units)
        });
        let _ = CloseClipboard();
        text
    }
}

/// After the launcher hides: back to `prev` and Ctrl+V into it. The text is already on the clipboard.
pub fn paste_into(prev: isize) {
    std::thread::spawn(move || unsafe {
        let prev = HWND(prev as _);
        std::thread::sleep(Duration::from_millis(40));
        if !prev.0.is_null() && IsWindow(Some(prev)).as_bool() {
            let _ = SetForegroundWindow(prev);
            for _ in 0..20 {
                if GetForegroundWindow() == prev {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // Enter/Shift may still be down from choosing the entry; wait so they do not mix in.
        for _ in 0..50 {
            if GetAsyncKeyState(VK_SHIFT.0 as i32) >= 0 && GetAsyncKeyState(VK_MENU.0 as i32) >= 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() }, ..Default::default() },
            },
        };
        let seq = [key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)];
        SendInput(&seq, size_of::<INPUT>() as i32);
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn title() {
        assert_eq!(super::title("  a\n\n b\tc "), "a b c");
        assert_eq!(super::title(&"я".repeat(200)).chars().count(), 151);
    }
}
