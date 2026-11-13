//! Small system helpers shared by the launcher and its plugins.

use std::path::Path;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::core::HSTRING;

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// `%USERPROFILE%\Music` -> `C:\Users\...\Music`; the input as is if it cannot be expanded.
pub fn expand_env(s: &str) -> String {
    let src = HSTRING::from(s);
    let mut buf = vec![0u16; 1024];
    let n = unsafe { ExpandEnvironmentStringsW(&src, Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return s.to_string();
    }
    String::from_utf16_lossy(&buf[..n - 1])
}

pub fn clipboard_text() -> String {
    unsafe {
        if OpenClipboard(None).is_err() {
            return String::new();
        }
        let mut text = String::new();
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT.0 as u32) {
            let hg = windows::Win32::Foundation::HGLOBAL(h.0);
            let p = GlobalLock(hg) as *const u16;
            if !p.is_null() {
                let len = (0..).take_while(|&i| *p.add(i) != 0).count();
                text = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
                let _ = GlobalUnlock(hg);
            }
        }
        let _ = CloseClipboard();
        text
    }
}

/// A JSON file, BOM or not; None if missing or invalid.
pub fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(std::fs::read_to_string(path).ok()?.trim_start_matches('\u{feff}')).ok()
}

/// Writes a plugin's embedded files into `dir`, only those that differ from what is there.
pub fn unpack(dir: &Path, files: &[(&str, &[u8])]) {
    let _ = std::fs::create_dir_all(dir);
    for (name, bytes) in files {
        let path = dir.join(name);
        if std::fs::read(&path).ok().as_deref() != Some(*bytes) {
            let _ = std::fs::write(&path, bytes);
        }
    }
}

/// python.exe next to the configured pythonw.exe, for work worth watching in a console (pip).
pub fn console_python(python: &str) -> String {
    let console = python.to_lowercase().replace("pythonw.exe", "python.exe");
    if Path::new(&console).exists() { console } else { "python".into() }
}
