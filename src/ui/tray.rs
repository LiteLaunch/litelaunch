//! Tray icon: its menu, balloon notifications, autostart.

use super::*;

pub(super) const TRAY_ID: u32 = 1;

pub(super) const MENU_SHOW: usize = 1;

pub(super) const MENU_RESCAN: usize = 2;

pub(super) const MENU_CONFIG: usize = 3;

pub(super) const MENU_AUTOSTART: usize = 4;

pub(super) const MENU_RESTART: usize = 5;

pub(super) const MENU_EXIT: usize = 6;

pub(super) const MENU_UNHIDE: usize = 7;

pub(super) fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: TRAY_ID, ..Default::default() }
}

pub(super) fn copy_wide(dst: &mut [u16], s: &str) {
    let max = dst.len() - 1;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(max).chain(Some(0))) {
        *d = c;
    }
}

pub(super) fn add_tray(hwnd: HWND) {
    unsafe {
        let mut nid = tray_data(hwnd);
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let size = GetSystemMetrics(SM_CXSMICON);
        nid.hIcon = LoadImageW(Some(instance.into()), PCWSTR(1 as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
            .map(|h| HICON(h.0))
            .unwrap_or_default();
        copy_wide(&mut nid.szTip, "LiteLaunch");
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

/// Folder opened by clicking the last notification (empty: the click does nothing).
pub(super) static NOTICE_FOLDER: Mutex<String> = Mutex::new(String::new());

/// Notification from the tray icon (Windows 11 shows it as a toast).
pub(super) fn notify(hwnd: HWND, title: &str, text: &str) {
    notify_with(hwnd, title, text, None, "");
}

/// `icon`: resource id of a large icon to show instead of the generic info one.
pub(super) fn notify_with(hwnd: HWND, title: &str, text: &str, icon: Option<u16>, folder: &str) {
    *NOTICE_FOLDER.lock().unwrap() = folder.to_string();
    unsafe {
        let mut nid = tray_data(hwnd);
        nid.uFlags = NIF_INFO;
        copy_wide(&mut nid.szInfoTitle, title);
        copy_wide(&mut nid.szInfo, if text.is_empty() { " " } else { text });
        nid.dwInfoFlags = NIIF_INFO;
        if let Some(id) = icon {
            let instance = GetModuleHandleW(None).unwrap_or_default();
            let size = GetSystemMetrics(SM_CXICON);
            if let Ok(h) = LoadImageW(Some(instance.into()), PCWSTR(id as _), IMAGE_ICON, size, size, LR_DEFAULTCOLOR) {
                nid.hBalloonIcon = HICON(h.0);
                nid.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
            }
        }
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// A notification sent by a helper process (yad.py) as JSON over WM_COPYDATA.
pub(super) fn notify_from_json(hwnd: HWND, json: &str) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return };
    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let icon = match s("icon").as_str() {
        "yad" => Some(YAD_ICON),
        "vd" => Some(VD_ICON),
        _ => None,
    };
    notify_with(hwnd, &s("title"), &s("text"), icon, &s("folder"));
}

/// Resource id of the yad icon (app.rc).
pub(super) const YAD_ICON: u16 = 2;

pub(super) const VD_ICON: u16 = 3;

/// Sent to the tray callback when the user clicks a notification.
pub(super) const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;

pub(super) const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

pub(super) fn autostart_enabled() -> bool {
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, w!("LiteLaunch"), RRF_RT_REG_SZ, None, None, Some(&mut size)).is_ok() }
}

pub(super) fn set_autostart(on: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key).is_err() {
            return;
        }
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let v = wide(&format!("\"{}\"", exe.display()));
            let bytes = std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2);
            let _ = RegSetValueExW(key, w!("LiteLaunch"), None, REG_SZ, Some(bytes));
        } else {
            let _ = RegDeleteValueW(key, w!("LiteLaunch"));
        }
        let _ = RegCloseKey(key);
    }
}

pub(super) fn tray_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let auto = if autostart_enabled() { MF_CHECKED } else { MF_UNCHECKED };
        let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, w!("Открыть"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_RESCAN, w!("Обновить список программ"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_CONFIG, w!("Настройки (config.json)"));
        let _ = AppendMenuW(menu, MF_STRING | auto, MENU_AUTOSTART, w!("Запускать при входе"));
        let hidden = with(|s| !s.engine.hidden.0.is_empty()).unwrap_or(false);
        let unhide = if hidden { MF_STRING } else { MF_STRING | MF_GRAYED };
        let _ = AppendMenuW(menu, unhide, MENU_UNHIDE, w!("Вернуть скрытые программы"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_RESTART, w!("Перезапустить"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Выход"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            MENU_SHOW => show(hwnd, None),
            MENU_RESCAN => rescan_apps(hwnd),
            MENU_CONFIG => shell_open(&config::path().to_string_lossy(), w!("open")),
            MENU_AUTOSTART => set_autostart(!autostart_enabled()),
            MENU_RESTART => {
                let _ = std::process::Command::new(std::env::current_exe().unwrap_or_default())
                    .args(["--restarted", &std::process::id().to_string()])
                    .spawn();
                let _ = DestroyWindow(hwnd);
            }
            MENU_EXIT => {
                let _ = DestroyWindow(hwnd);
            }
            MENU_UNHIDE => {
                with(|s| s.engine.hidden.clear());
            }
            _ => {}
        }
    }
}
