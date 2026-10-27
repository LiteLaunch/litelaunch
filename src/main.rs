#![windows_subsystem = "windows"]
//! LiteLaunch: a native keystroke launcher. One Win32 window painted with GDI, a tray icon,
//! a global hotkey. Python plugins run only while a query for them is in flight.

mod apps;
mod calc;
mod config;
mod fuzzy;
mod icons;
mod procs;
mod pyplugin;
mod search;
mod steam;
// Built-in plugins: each lives in its own repository, a submodule under plugins/.
#[path = "../plugins/vd/vd.rs"]
mod vd;
#[path = "../plugins/yad/yad.rs"]
mod yad;
mod ui;

use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::Threading::{CreateMutexW, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

/// WM_COPYDATA kinds sent by a second instance.
pub const CMD_SHOW: usize = 1;
pub const CMD_EXIT: usize = 2;
/// Payload: JSON {title, text, folder, icon} — a notification from yad.py.
pub const CMD_NOTIFY: usize = 3;

fn main() {
    // `litelaunch --show [query]` opens the running instance (optionally with a query);
    // `litelaunch --exit` closes it. A plain second start just opens it.
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--restarted <pid>` (tray "Перезапустить"): the old instance still holds the
    // single-instance mutex while it shuts down, so wait for it to be gone first.
    if let Some(pid) = args.iter().position(|a| a == "--restarted").and_then(|i| args.get(i + 1)?.parse::<u32>().ok()) {
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                WaitForSingleObject(h, 5000);
                let _ = CloseHandle(h);
            }
        }
    }
    unsafe {
        let _mutex = CreateMutexW(None, true, w!("LiteLaunch.SingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(hwnd) = FindWindowW(ui::CLASS, None) {
                let (kind, text) = match args.first().map(String::as_str) {
                    Some("--exit") => (CMD_EXIT, String::new()),
                    _ => (CMD_SHOW, args.get(1).cloned().unwrap_or_default()),
                };
                let wide: Vec<u16> = text.encode_utf16().collect();
                let cds = COPYDATASTRUCT { dwData: kind, cbData: (wide.len() * 2) as u32, lpData: wide.as_ptr() as _ };
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                SendMessageW(hwnd, WM_COPYDATA, None, Some(LPARAM(&cds as *const _ as isize)));
            }
            return;
        }
    }
    if let Err(e) = ui::run(args.iter().any(|a| a == "--show")) {
        let msg = windows::core::HSTRING::from(format!("LiteLaunch: {e}"));
        unsafe { MessageBoxW(None, &msg, w!("LiteLaunch"), MB_ICONERROR) };
    }
}
