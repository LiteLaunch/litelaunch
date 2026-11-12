//! The launcher window: a native EDIT for the query, results painted with GDI below it.
//! Win32 calls that can re-enter the window procedure are made outside the state borrow.

use crate::config::{self, Config};
use crate::icons::Icons;
use crate::pyplugin::{self, PyResult, RpcAction};
use crate::search::{Action, Engine, Item, PluginRef};
use crate::{CMD_EXIT, CMD_NOTIFY, CMD_SHOW, apps};
use serde_json::Value;
use std::cell::RefCell;
use std::process::Child;
use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HSTRING, PCWSTR, Result, w};

mod input;
mod paint;
mod theme;
mod tray;
mod worker;

use input::*;
use paint::*;
use theme::*;
use tray::*;
// Ours (WM_USER + 5) over the windows crate's constant of the same name.
use tray::NIN_BALLOONUSERCLICK;
use worker::*;


pub const CLASS: PCWSTR = w!("LiteLaunchWindow");

/// Set while show() runs: activation can bounce on the way to the foreground, and a
/// WA_INACTIVE from that must not hide the window we are bringing up.
static SHOWING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The window that was active before the launcher showed: clipboard entries are pasted there.
static PREV_FG: AtomicIsize = AtomicIsize::new(0);

const WM_APP_PY: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_APPS: u32 = WM_APP + 3;
const HOTKEY_ID: i32 = 1;
const ID_EDIT: i32 = 100;
struct State {
    hwnd: HWND,
    edit: HWND,
    engine: Engine,
    items: Vec<Item>,
    /// The query the items are for (pins are keyed by it).
    query: String,
    /// While the actions menu is open: the results to go back to, with their selection.
    menu: Option<(Vec<Item>, usize, usize)>,
    sel: usize,
    top: usize,
    hover: Option<usize>,
    tracking_leave: bool,
    /// Cursor position when the window was shown; hover starts once it moves.
    mouse_anchor: Option<POINT>,
    scale: f32,
    theme: Theme,
    bg_brush: HBRUSH,
    fonts: Fonts,
    icons: Icons,
    seq: Arc<AtomicU64>,
    jobs: Sender<Job>,
    py_child: Arc<Mutex<Option<Child>>>,
    py_reply: Arc<Mutex<Option<PyReply>>>,
    vd_warm: crate::vd::Warm,
    apps_at: Instant,
    scanning: bool,
    taskbar_created: u32,
}

/// The EDIT's original window procedure. Not in State: it is needed while State is borrowed
/// (GetWindowText on the EDIT goes through the subclass).
static EDIT_PROC: AtomicIsize = AtomicIsize::new(0);
/// EDIT text color, background color and background brush, for WM_CTLCOLOREDIT —
/// which also arrives while State is borrowed (the EDIT repaints inside Win32 calls).
static EDIT_TEXT: AtomicU32 = AtomicU32::new(0);
static EDIT_BG: AtomicU32 = AtomicU32::new(0);
static EDIT_BRUSH: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` on the state unless it is already borrowed (re-entrant message).
fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Things to do after the state borrow is released.
enum After {
    None,
    Hide,
    SetQuery(String),
    Requery,
}

impl State {
    fn select(&mut self, i: usize) {
        if self.items.is_empty() {
            return;
        }
        self.sel = i.min(self.items.len() - 1);
        let rows = self.visible_rows();
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + rows {
            self.top = self.sel + 1 - rows;
        }
    }

    fn query_text(&self) -> String {
        unsafe {
            let len = GetWindowTextLengthW(self.edit);
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(self.edit, &mut buf);
            String::from_utf16_lossy(&buf[..n as usize])
        }
    }

    /// Recomputes results for the current text; plugin queries go to the worker.
    fn run_query(&mut self) {
        let text = self.query_text();
        self.query = text.clone();
        self.menu = None;
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        // A newer query makes the running plugin call pointless.
        if let Some(mut c) = self.py_child.lock().unwrap().take() {
            let _ = c.kill();
        }
        let out = self.engine.search(&text);
        self.items = out.items;
        if out.warm_vd {
            crate::vd::warm_up(&self.vd_warm, &self.engine.cfg.python);
        }
        if let Some((plugin, q)) = out.plugin_query
            && let Some((config, settings)) = self.plugin(plugin)
        {
            let icon = std::path::Path::new(&config.dir).join(&config.icon);
            let icon = icon.exists().then(|| icon.to_string_lossy().into());
            let wait = if plugin == PluginRef::Vd { "Получаю форматы видео" } else { "Запрос к плагину" };
            self.items.push(Item::new(format!("{} …", config.name), wait, icon, 0, Action::Nothing));
            let _ = self.jobs.send(Job { seq, plugin, config, settings, text: q });
        }
        self.sel = 0;
        self.top = 0;
        self.hover = None;
    }

    fn take_py_reply(&mut self, seq: u64) {
        let Some((g, plugin, text, reply)) = self.py_reply.lock().unwrap().take() else { return };
        if g != seq || g != self.seq.load(Ordering::SeqCst) {
            return;
        }
        let Some((config, _)) = self.plugin(plugin) else { return };
        self.items = match reply {
            Ok(results) => {
                if plugin == PluginRef::Vd {
                    self.engine.vd_cache.put(&text, &results);
                }
                crate::search::py_items(plugin, results)
            }
            Err(e) => vec![Item::new(format!("{}: ошибка", config.name), e, None, 0, Action::Nothing)],
        };
        self.sel = 0;
        self.top = 0;
    }

    /// A plugin's config and the settings sent with its requests.
    fn plugin(&self, plugin: PluginRef) -> Option<(crate::config::PluginConfig, Value)> {
        match plugin {
            PluginRef::User(i) => self.engine.cfg.plugins.get(i).map(|p| (p.clone(), pyplugin::file_settings(p))),
            PluginRef::Vd => {
                let vd = &self.engine.cfg.vd;
                Some((crate::vd::plugin_config(vd), serde_json::to_value(vd).unwrap_or_default()))
            }
        }
    }

    fn activate(&mut self, i: usize, admin: bool) -> After {
        let Some(item) = self.items.get(i) else { return After::None };
        match &item.action {
            Action::Launch(target) => {
                let target = target.clone();
                shell_open(&target, if admin { w!("runas") } else { w!("open") });
                self.engine.usage.bump(&target);
                After::Hide
            }
            Action::Copy(text) => {
                copy_to_clipboard(self.hwnd, text);
                After::Hide
            }
            Action::Kill(pids) => {
                crate::procs::kill(pids);
                // Give the processes a moment to exit, then refresh the list.
                std::thread::sleep(Duration::from_millis(150));
                After::Requery
            }
            Action::SetQuery(q) => After::SetQuery(q.clone()),
            Action::Shell(cmd) => {
                let cmd = cmd.clone();
                crate::shell::run(&mut self.engine.shell_history, &cmd, admin);
                After::Hide
            }
            Action::Clip(text) => {
                let text = text.clone();
                copy_to_clipboard(self.hwnd, &text);
                let copy_only = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                if !copy_only {
                    crate::clip::paste_into(PREV_FG.load(Ordering::Relaxed));
                }
                After::Hide
            }
            Action::ClipForget(text) => {
                let text = text.clone();
                self.engine.clip.forget(&text);
                After::Requery
            }
            Action::ClipClear => {
                self.engine.clip.clear();
                After::Requery
            }
            Action::ShellForget(cmd) => {
                let cmd = cmd.clone();
                self.engine.shell_history.forget(&cmd);
                After::Requery
            }
            Action::Py { plugin, action } => {
                let (plugin, action) = (*plugin, action.clone());
                self.py_action(plugin, &action)
            }
            Action::Yad(op) => {
                crate::yad::run(op, &self.engine.cfg.yad, &self.engine.cfg.python);
                After::Hide
            }
            Action::Vd(op) => {
                crate::vd::run(op, &self.engine.cfg.vd, &self.engine.cfg.python);
                After::Hide
            }
            Action::Zapret(op) => {
                let op = op.clone();
                if matches!(op, crate::zapret::Op::ChooseFolder) {
                    // The folder dialog would open under the launcher window.
                    hide(self.hwnd);
                }
                let (changed, after) = crate::zapret::run(&op, &mut self.engine.cfg.zapret);
                if changed {
                    crate::config::save(&self.engine.cfg);
                }
                match after {
                    crate::zapret::After::Hide => After::Hide,
                    crate::zapret::After::Requery => After::Requery,
                    crate::zapret::After::SetQuery(q) => After::SetQuery(q),
                }
            }
            Action::RunAsAdmin(file) => {
                shell_open(&file.clone(), w!("runas"));
                After::Hide
            }
            Action::Reveal(file) => {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{file}\"")).spawn();
                After::Hide
            }
            Action::RemovePlugin(kw) => {
                let kw = kw.clone();
                self.engine.remove_plugin(&kw);
                After::SetQuery(String::new())
            }
            Action::Hide(target) => {
                let target = target.clone();
                self.engine.hidden.add(&target);
                After::Requery
            }
            Action::Nothing => After::None,
        }
    }

    /// Right arrow: replaces the results with the selected one's actions. False if it has none.
    fn open_menu(&mut self) -> bool {
        let Some(actions) = self.items.get(self.sel).map(|it| self.engine.actions(it)).filter(|a| !a.is_empty()) else { return false };
        let results = std::mem::replace(&mut self.items, actions);
        self.menu = Some((results, self.sel, self.top));
        self.sel = 0;
        self.top = 0;
        self.hover = None;
        true
    }

    /// Left arrow / Esc in the menu: back to the results. False if no menu was open.
    fn close_menu(&mut self) -> bool {
        let Some((items, sel, top)) = self.menu.take() else { return false };
        self.items = items;
        self.sel = sel;
        self.top = top;
        self.hover = None;
        true
    }

    /// Plugin action: Flow API calls are handled here, anything else goes back to the plugin.
    fn py_action(&mut self, plugin: PluginRef, a: &RpcAction) -> After {
        let arg = |i: usize| a.params.get(i).and_then(Value::as_str).unwrap_or_default().to_string();
        match a.method.as_str() {
            "Flow.Launcher.ChangeQuery" => return After::SetQuery(arg(0)),
            "Flow.Launcher.OpenUrl" | "Flow.Launcher.OpenDirectory" => shell_open(&arg(0), w!("open")),
            "Flow.Launcher.ShellRun" => shell_open(&format!("cmd.exe /c {}", arg(0)), w!("open")),
            "Flow.Launcher.CopyToClipboard" => copy_to_clipboard(self.hwnd, &arg(0)),
            "Flow.Launcher.ShowMsg" => notify(self.hwnd, &arg(0), &arg(1)),
            m if m.starts_with("Flow.Launcher.") => {}
            _ => {
                if let Some((config, settings)) = self.plugin(plugin) {
                    pyplugin::call(&self.engine.cfg.python, &config, &settings, a);
                }
            }
        }
        if a.hide { After::Hide } else { After::None }
    }
}


fn shell_open(target: &str, verb: PCWSTR) {
    // "cmd.exe /c ..." style targets carry their own arguments.
    let (file, params) = match target.split_once(".exe ") {
        Some((f, p)) if !target.starts_with("shell:") => (format!("{f}.exe"), p.to_string()),
        _ => (target.to_string(), String::new()),
    };
    unsafe {
        ShellExecuteW(None, verb, &HSTRING::from(file), &HSTRING::from(params), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

fn copy_to_clipboard(hwnd: HWND, text: &str) {
    let w = wide(text);
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(h);
                let _ = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(h.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

fn rescan_apps(hwnd: HWND) {
    let Some(list) = with(|s| {
        if s.scanning {
            return None;
        }
        s.scanning = true;
        Some(s.engine.apps.clone())
    })
    .flatten() else {
        return;
    };
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let found = apps::scan();
        if !found.is_empty() {
            *list.write().unwrap() = found;
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_APP_APPS, WPARAM(0), LPARAM(0));
        }
    });
}

/// Resizes to fit the results, keeping the top edge in place.
fn relayout(hwnd: HWND) {
    let Some((h, w)) = with(|s| (s.height(), s.px(s.engine.cfg.width as f32))) else { return };
    unsafe {
        let _ = SetWindowPos(hwnd, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn place_edit(hwnd: HWND) {
    let Some((edit, font, x, top, band, right)) = with(|s| {
        let right = s.px(s.engine.cfg.width as f32) - s.px(PAD + 12.0);
        // Without an icon the query starts just past the selected row's accent bar.
        let x = if s.engine.cfg.search_icon.is_empty() { PAD + 14.0 } else { TEXT_X };
        let (top, bottom) = s.query_band();
        (s.edit, s.fonts.input, s.px(x), top, bottom - top, right)
    }) else {
        return;
    };
    unsafe {
        // The EDIT is exactly one line tall, so centering it centers the text.
        let hdc = GetDC(Some(hwnd));
        let old = SelectObject(hdc, font.into());
        let mut tm = TEXTMETRICW::default();
        let _ = GetTextMetricsW(hdc, &mut tm);
        SelectObject(hdc, old);
        ReleaseDC(Some(hwnd), hdc);
        // Place the baseline, not the line box: the box has room for accents above the caps
        // and for descenders, so positioning "by box" puts the letters off.
        let baseline = top + (band as f32 * BASELINE_AT).round() as i32;
        let y = baseline - tm.tmAscent;
        let _ = SetWindowPos(edit, None, x, y, right - x, tm.tmHeight, SWP_NOZORDER | SWP_NOACTIVATE);
        SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        // No inner margins: the text starts exactly at the title column.
        SendMessageW(edit, EM_SETMARGINS, Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)), Some(LPARAM(0)));
    }
}

fn requery(hwnd: HWND) {
    with(|s| s.run_query());
    relayout(hwnd);
}

fn set_query(hwnd: HWND, q: &str) {
    let Some(edit) = with(|s| s.edit) else { return };
    unsafe {
        // EN_CHANGE from this runs the query.
        let _ = SetWindowTextW(edit, &HSTRING::from(q));
        let n = q.encode_utf16().count();
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(n)), Some(LPARAM(n as isize)));
    }
    let _ = hwnd;
}

fn hide(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// Centers the window on the monitor under the cursor, a quarter of the way down.
fn show(hwnd: HWND, query: Option<&str>) {
    let stale = with(|s| {
        s.apply_theme();
        s.apps_at.elapsed() > Duration::from_secs(600)
    });
    if stale == Some(true) {
        rescan_apps(hwnd);
    }
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;
        // Move first so WM_DPICHANGED fires for the target monitor before sizing.
        let _ = SetWindowPos(hwnd, None, wa.left + 1, wa.top + 1, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
    match query {
        Some(q) => set_query(hwnd, q),
        None => {
            let keep = with(|s| s.engine.cfg.keep_last_query).unwrap_or(true);
            if keep {
                requery(hwnd);
            } else {
                set_query(hwnd, "");
            }
        }
    }
    relayout(hwnd);
    let Some((edit, w)) = with(|s| (s.edit, s.px(s.engine.cfg.width as f32))) else { return };
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST), &mut mi);
        let wa = mi.rcWork;
        let x = wa.left + (wa.right - wa.left - w) / 2;
        let y = wa.top + (wa.bottom - wa.top) / 4;
        with(|s| {
            s.mouse_anchor = Some(pt);
            SESSION_LAYOUT.store(0, Ordering::Relaxed);
            s.hover = None;
        });
        let fg = GetForegroundWindow();
        if fg != hwnd {
            PREV_FG.store(fg.0 as isize, Ordering::Relaxed);
        }
        SHOWING.store(true, Ordering::Relaxed);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        bring_to_front(hwnd);
        SHOWING.store(false, Ordering::Relaxed);
        let _ = SetFocus(Some(edit));
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
    }
}

/// Plain SetForegroundWindow first: after WM_HOTKEY (or AllowSetForegroundWindow from a second
/// instance) we hold the foreground right. Attaching to the foreground thread is only a fallback —
/// done up front, it hands our window that thread's activation state and deactivates it mid-show.
fn bring_to_front(hwnd: HWND) {
    unsafe {
        let _ = BringWindowToTop(hwnd);
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            return;
        }
        let fg_thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let me = windows::Win32::System::Threading::GetCurrentThreadId();
        let attached = fg_thread != 0 && fg_thread != me && windows::Win32::System::Threading::AttachThreadInput(me, fg_thread, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = windows::Win32::System::Threading::AttachThreadInput(me, fg_thread, false);
        }
    }
}

fn run_after(hwnd: HWND, after: After) {
    match after {
        After::None => {}
        After::Hide => hide(hwnd),
        After::SetQuery(q) => set_query(hwnd, &q),
        After::Requery => {
            let sel = with(|s| s.sel).unwrap_or(0);
            requery(hwnd);
            with(|s| s.select(sel));
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
    }
}

fn activate(hwnd: HWND, index: Option<usize>, admin: bool) {
    let after = with(|s| s.activate(index.unwrap_or(s.sel), admin)).unwrap_or(After::None);
    run_after(hwnd, after);
}

fn move_selection(hwnd: HWND, delta: isize) {
    with(|s| {
        if s.items.is_empty() {
            return;
        }
        let n = s.items.len() as isize;
        let i = (s.sel as isize + delta).rem_euclid(n);
        // Paging stops at the ends instead of wrapping.
        let i = if delta.abs() > 1 { (s.sel as isize + delta).clamp(0, n - 1) } else { i };
        s.select(i as usize);
    });
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Opens (`open`) or closes the actions menu and resizes the window. Returns whether it did.
fn toggle_menu(hwnd: HWND, open: bool) -> bool {
    let done = with(|s| if open { s.open_menu() } else { s.close_menu() }).unwrap_or(false);
    if done {
        relayout(hwnd);
    }
    done
}

/// Ctrl+P: pins the selected app/game to the current query, or unpins it.
fn toggle_pin(hwnd: HWND) {
    let Some(target) = with(|s| {
        let target = s.items.get(s.sel)?.target()?.to_string();
        let query = s.query.clone();
        s.engine.pins.toggle(&query, &target);
        Some(target)
    })
    .flatten() else {
        return;
    };
    requery_keeping(hwnd, &target);
}

/// Ctrl+Up/Down: moves the selected pinned result within the query's pins.
fn shift_pin(hwnd: HWND, delta: isize) {
    let Some(target) = with(|s| {
        let item = s.items.get(s.sel).filter(|it| it.pinned)?;
        let target = item.target()?.to_string();
        let query = s.query.clone();
        s.engine.pins.shift(&query, &target, delta);
        Some(target)
    })
    .flatten() else {
        return;
    };
    requery_keeping(hwnd, &target);
}

/// Re-runs the query and keeps the selection on `target` wherever it ended up.
fn requery_keeping(hwnd: HWND, target: &str) {
    requery(hwnd);
    with(|s| {
        if let Some(i) = s.items.iter().position(|it| it.target() == Some(target)) {
            s.select(i);
        }
    });
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            with(|s| s.paint(hdc));
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_CTLCOLOREDIT => {
            let hdc = HDC(wp.0 as _);
            unsafe {
                SetTextColor(hdc, COLORREF(EDIT_TEXT.load(Ordering::Relaxed)));
                SetBkColor(hdc, COLORREF(EDIT_BG.load(Ordering::Relaxed)));
            }
            LRESULT(EDIT_BRUSH.load(Ordering::Relaxed))
        }
        WM_COMMAND => {
            if (wp.0 & 0xFFFF) as i32 == ID_EDIT && ((wp.0 >> 16) & 0xFFFF) as u32 == EN_CHANGE {
                requery(hwnd);
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            let visible = unsafe { IsWindowVisible(hwnd).as_bool() && GetForegroundWindow() == hwnd };
            if visible { hide(hwnd) } else { show(hwnd, None) }
            LRESULT(0)
        }
        WM_CLIPBOARDUPDATE => {
            let on = with(|s| !s.engine.cfg.clip_keyword.is_empty()).unwrap_or(false);
            if let Some(text) = on.then(crate::clip::read_new).flatten() {
                with(|s| s.engine.clip.add(text));
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE && !SHOWING.load(Ordering::Relaxed) {
                hide(hwnd);
            }
            LRESULT(0)
        }
        WM_APP_PY => {
            with(|s| s.take_py_reply(wp.0 as u64));
            relayout(hwnd);
            LRESULT(0)
        }
        WM_APP_APPS => {
            with(|s| {
                s.scanning = false;
                s.apps_at = Instant::now();
            });
            if unsafe { IsWindowVisible(hwnd).as_bool() } {
                let sel = with(|s| s.sel).unwrap_or(0);
                requery(hwnd);
                with(|s| s.select(sel));
            }
            LRESULT(0)
        }
        WM_APP_TRAY => {
            match (lp.0 & 0xFFFF) as u32 {
                WM_LBUTTONUP => show(hwnd, None),
                WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(hwnd),
                NIN_BALLOONUSERCLICK => {
                    let folder = std::mem::take(&mut *NOTICE_FOLDER.lock().unwrap());
                    if !folder.is_empty() {
                        let _ = std::process::Command::new("explorer.exe").arg(folder).spawn();
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_COPYDATA => {
            let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
            let text = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
            let text = String::from_utf16_lossy(text);
            match cds.dwData {
                CMD_EXIT => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                CMD_SHOW => show(hwnd, (!text.is_empty()).then_some(text.as_str())),
                CMD_NOTIFY => notify_from_json(hwnd, &text),
                _ => {}
            }
            LRESULT(1)
        }
        WM_MOUSEMOVE => {
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            // Windows sends a move when the window appears under a resting cursor; that's not hover.
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            let resting = with(|s| s.mouse_anchor.is_some_and(|a| a.x == pt.x && a.y == pt.y)).unwrap_or(false);
            if resting {
                return LRESULT(0);
            }
            with(|s| s.mouse_anchor = None);
            let changed = with(|s| {
                if !s.tracking_leave {
                    let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    unsafe {
                        let _ = TrackMouseEvent(&mut t);
                    }
                    s.tracking_leave = true;
                }
                let h = s.row_at(y);
                std::mem::replace(&mut s.hover, h) != h
            });
            if changed == Some(true) {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with(|s| {
                s.tracking_leave = false;
                s.hover = None;
            });
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            if let Some(i) = with(|s| s.row_at(y)).flatten() {
                with(|s| s.select(i));
                activate(hwnd, Some(i), false);
            }
            LRESULT(0)
        }
        // Right click on a result: its actions menu, like Right arrow.
        WM_RBUTTONUP => {
            let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
            let row = with(|s| if s.menu.is_some() { None } else { s.row_at(y) }).flatten();
            if let Some(i) = row {
                with(|s| s.select(i));
                if !toggle_menu(hwnd, true) {
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16;
            with(|s| {
                let max_top = s.items.len().saturating_sub(s.visible_rows());
                s.top = if delta > 0 { s.top.saturating_sub(1) } else { (s.top + 1).min(max_top) };
                if s.sel < s.top || s.sel >= s.top + s.visible_rows() {
                    s.sel = s.sel.clamp(s.top, s.top + s.visible_rows().saturating_sub(1));
                }
            });
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            with(|s| s.apply_dpi());
            place_edit(hwnd);
            relayout(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &tray_data(hwnd));
                let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
                let _ = RemoveClipboardFormatListener(hwnd);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        m if Some(m) == with(|s| s.taskbar_created) => {
            // Explorer restarted: the tray icon is gone.
            add_tray(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

pub fn run(show_now: bool) -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let cfg = config::load();
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            CLASS,
            w!("LiteLaunch"),
            WS_POPUP | WS_CLIPCHILDREN,
            0,
            0,
            cfg.width as i32,
            100,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32);
        // A 1px glass margin is what makes DWM draw the shadow around a borderless window.
        let margins = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 0, cyBottomHeight: 1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);

        let edit = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            None,
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            0,
            0,
            100,
            30,
            Some(hwnd),
            Some(HMENU(ID_EDIT as _)),
            Some(instance.into()),
            None,
        )?;
        SendMessageW(edit, EM_SETCUEBANNER, Some(WPARAM(1)), Some(LPARAM(w!("…").as_ptr() as isize)));
        EDIT_PROC.store(SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize), Ordering::Relaxed);

        let apps_list = Arc::new(RwLock::new(Vec::new()));
        let seq = Arc::new(AtomicU64::new(0));
        let py_child = Arc::new(Mutex::new(None));
        let py_reply = Arc::new(Mutex::new(None));
        let vd_warm: crate::vd::Warm = Arc::new(Mutex::new(None));
        let jobs = spawn_py_worker(cfg.clone(), hwnd, seq.clone(), py_child.clone(), py_reply.clone(), vd_warm.clone());
        let hotkey = parse_hotkey(&cfg.hotkey);
        let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        let state = State {
            hwnd,
            edit,
            engine: Engine::new(cfg.clone(), apps_list),
            items: Vec::new(),
            query: String::new(),
            menu: None,
            sel: 0,
            top: 0,
            hover: None,
            tracking_leave: false,
            mouse_anchor: None,
            scale,
            theme: LIGHT,
            bg_brush: CreateSolidBrush(COLORREF(LIGHT.bg)),
            fonts: Fonts::new(scale, cfg.input_font_size),
            icons: Icons::new((ICON * scale).round() as i32),
            seq,
            jobs,
            py_child,
            py_reply,
            vd_warm,
            apps_at: Instant::now(),
            scanning: false,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
        };
        STATE.with(|s| *s.borrow_mut() = Some(state));
        with(|s| s.apply_theme());
        place_edit(hwnd);
        add_tray(hwnd);
        let _ = AddClipboardFormatListener(hwnd);
        rescan_apps(hwnd);
        std::thread::spawn(crate::yad::install);
        std::thread::spawn(crate::vd::install);
        std::thread::spawn(crate::zapret::install);

        // After a restart the old instance may still hold the hotkey for a moment.
        let mut registered = false;
        if let Some((mods, vk)) = hotkey {
            for _ in 0..20 {
                if RegisterHotKey(Some(hwnd), HOTKEY_ID, mods, vk).is_ok() {
                    registered = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if !registered {
            notify(hwnd, "LiteLaunch", &format!("Не удалось занять {}: она уже используется (Flow Launcher?). Поменяйте hotkey в config.json.", cfg.hotkey));
        }
        if show_now {
            show(hwnd, None);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
