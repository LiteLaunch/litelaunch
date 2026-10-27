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

pub const CLASS: PCWSTR = w!("LiteLaunchWindow");

const WM_APP_PY: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_APPS: u32 = WM_APP + 3;
const HOTKEY_ID: i32 = 1;
const TRAY_ID: u32 = 1;
const ID_EDIT: i32 = 100;
const MENU_SHOW: usize = 1;
const MENU_RESCAN: usize = 2;
const MENU_CONFIG: usize = 3;
const MENU_AUTOSTART: usize = 4;
const MENU_RESTART: usize = 5;
const MENU_EXIT: usize = 6;
const MENU_UNHIDE: usize = 7;

// Layout in DIPs.
const PAD: f32 = 8.0;
/// Query band proportions measured on Flow Launcher (16 pt query, 71 px band at 125%):
/// the band from the window top to the separator is 3.55 em tall, and the text baseline
/// sits at 62% of it — caps 30 px from the top, baseline 27 px above the separator.
const BAND_EM: f32 = 3.55;
const BASELINE_AT: f32 = 0.62;
const ROW_H: f32 = 52.0;
/// Rows without a subtitle (apps, games) are shorter.
const ROW_H_SHORT: f32 = 42.0;
const ICON: f32 = 32.0;
const FONT_TITLE: f32 = 15.0;
const FONT_SUB: f32 = 12.0;
/// Left edge of the icon column and of the text column (query and result titles), from the window edge.
const ICON_X: f32 = PAD + 14.0;
const TEXT_X: f32 = ICON_X + ICON + 14.0;

/// Colors as COLORREF (0x00BBGGRR).
#[derive(Clone, Copy)]
struct Theme {
    bg: u32,
    sel: u32,
    text: u32,
    muted: u32,
    accent: u32,
    line: u32,
}

const fn rgb(c: u32) -> u32 {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

const LIGHT: Theme = Theme {
    bg: rgb(0xF9F9F9),
    sel: rgb(0xE9E9E9),
    text: rgb(0x1B1B1B),
    muted: rgb(0x6E6E6E),
    accent: rgb(0x005FB8),
    line: rgb(0xE0E0E0),
};
const DARK: Theme = Theme {
    bg: rgb(0x202020),
    sel: rgb(0x2F2F2F),
    text: rgb(0xF0F0F0),
    muted: rgb(0x9A9A9A),
    accent: rgb(0x60CDFF),
    line: rgb(0x333333),
};
const AMOLED: Theme = Theme {
    bg: rgb(0x000000),
    sel: rgb(0x161616),
    text: rgb(0xEDEDED),
    muted: rgb(0x8C8C8C),
    accent: rgb(0x60CDFF),
    line: rgb(0x1C1C1C),
};

/// Light or dark base tinted with the Windows accent color: the selection bar gets the accent
/// shade Windows itself uses on that background, the selected row a faint wash of it.
fn windows_theme(dark: bool) -> Theme {
    let base = if dark { DARK } else { LIGHT };
    let Some(palette) = accent_palette() else { return base };
    // AccentPalette: Light3, Light2, Light1, Accent, Dark1, Dark2, Dark3 (+1 unused).
    let accent = if dark { palette[1] } else { palette[4] };
    Theme { accent, sel: mix(palette[3], base.bg, if dark { 0.22 } else { 0.16 }), ..base }
}

/// The system accent palette as COLORREFs, from Explorer's registry key.
fn accent_palette() -> Option<[u32; 8]> {
    let mut raw = [0u8; 32];
    let mut size = raw.len() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(raw.as_mut_ptr() as _),
            Some(&mut size),
        )
    };
    if r.is_err() || size != 32 {
        return None;
    }
    // Stored as R, G, B, A — which is COLORREF byte order once alpha is dropped.
    Some(std::array::from_fn(|i| u32::from_le_bytes([raw[i * 4], raw[i * 4 + 1], raw[i * 4 + 2], 0])))
}

/// `a` over `b` with weight `t` (COLORREFs).
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| {
        let (x, y) = (((a >> s) & 0xFF) as f32, ((b >> s) & 0xFF) as f32);
        ((x * t + y * (1.0 - t)).round() as u32) << s
    };
    ch(0) | ch(8) | ch(16)
}

struct Fonts {
    input: HFONT,
    title: HFONT,
    sub: HFONT,
    /// For a text `search_icon` (symbols and emoji).
    glyph: HFONT,
    /// Segoe Fluent Icons, for the pin mark.
    icons: HFONT,
    /// Segoe Fluent Icons at icon size, for the actions menu rows.
    actions: HFONT,
}

impl Fonts {
    fn new(scale: f32, input: f32) -> Self {
        let make = |px: f32, weight: i32, face: PCWSTR| unsafe {
            CreateFontW(
                -(px * scale).round() as i32,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                face,
            )
        };
        let ui = w!("Segoe UI");
        Self {
            input: make(input, 400, ui),
            title: make(FONT_TITLE, 400, ui),
            sub: make(FONT_SUB, 400, ui),
            glyph: make(input, 400, w!("Segoe UI Emoji")),
            icons: make(FONT_SUB, 400, w!("Segoe Fluent Icons")),
            actions: make(20.0, 400, w!("Segoe Fluent Icons")),
        }
    }

    fn delete(&self) {
        unsafe {
            for f in [self.input, self.title, self.sub, self.glyph, self.icons, self.actions] {
                let _ = DeleteObject(f.into());
            }
        }
    }
}

struct Job {
    seq: u64,
    plugin: PluginRef,
    /// The plugin as configured when the job was made (indices shift when one is removed).
    config: crate::config::PluginConfig,
    settings: Value,
    text: String,
}

/// A finished query: its sequence number, plugin, query text and outcome.
type PyReply = (u64, PluginRef, String, std::result::Result<Vec<PyResult>, String>);

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
    fn px(&self, dip: f32) -> i32 {
        (dip * self.scale).round() as i32
    }

    /// Results reachable with Alt+1..N: the first `max_results` of the list (at most 9),
    /// wherever the list is scrolled.
    fn shortcut_rows(&self) -> usize {
        self.items.len().min(self.engine.cfg.max_results).min(9)
    }

    fn visible_rows(&self) -> usize {
        self.items.len().min(self.engine.cfg.max_results)
    }

    fn height(&self) -> i32 {
        let rows = self.visible_rows();
        if rows == 0 {
            self.query_band().1
        } else {
            self.row_top(rows) + self.px(PAD)
        }
    }

    /// Height of result `i`: shorter when it has no subtitle.
    fn row_h(&self, i: usize) -> i32 {
        let short = self.items.get(i).is_some_and(|it| it.sub.is_empty());
        self.px(if short { ROW_H_SHORT } else { ROW_H })
    }

    /// Top of the `visible_index`-th visible row (rows can differ in height).
    fn row_top(&self, visible_index: usize) -> i32 {
        let first = self.px(PAD + self.input_h() + 1.0 + PAD);
        first + (0..visible_index).map(|k| self.row_h(self.top + k)).sum::<i32>()
    }

    fn row_at(&self, y: i32) -> Option<usize> {
        (0..self.visible_rows())
            .find(|&vi| {
                let top = self.row_top(vi);
                y >= top && y < top + self.row_h(self.top + vi)
            })
            .map(|vi| self.top + vi)
    }

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
                let vd = self.engine.cfg.vd.as_ref()?;
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
            Action::Py { plugin, action } => {
                let (plugin, action) = (*plugin, action.clone());
                self.py_action(plugin, &action)
            }
            Action::Yad(op) => {
                if let Some(cfg) = &self.engine.cfg.yad {
                    crate::yad::run(op, cfg, &self.engine.cfg.python);
                }
                After::Hide
            }
            Action::Vd(op) => {
                if let Some(cfg) = &self.engine.cfg.vd {
                    crate::vd::run(op, cfg, &self.engine.cfg.python);
                }
                After::Hide
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

    fn paint(&mut self, hdc: HDC) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
            let mem = CreateCompatibleDC(Some(hdc));
            let bmp = CreateCompatibleBitmap(hdc, rc.right, rc.bottom);
            let old_bmp = SelectObject(mem, bmp.into());
            FillRect(mem, &rc, self.bg_brush);
            SetBkMode(mem, TRANSPARENT);

            self.paint_search_icon(mem);

            if !self.items.is_empty() {
                let y = self.px(PAD + self.input_h());
                let line = CreateSolidBrush(COLORREF(self.theme.line));
                FillRect(mem, &RECT { left: 0, top: y, right: rc.right, bottom: y + self.px(1.0).max(1) }, line);
                let _ = DeleteObject(line.into());
            }

            let icon = self.px(ICON);
            for vi in 0..self.visible_rows() {
                let i = self.top + vi;
                let row_h = self.row_h(i);
                let Some(item) = self.items.get(i) else { break };
                let top = self.row_top(vi);
                let left = self.px(PAD);
                let right = rc.right - self.px(PAD);
                if i == self.sel || Some(i) == self.hover {
                    let color = if i == self.sel { self.theme.sel } else { blend(self.theme.sel, self.theme.bg) };
                    let brush = CreateSolidBrush(COLORREF(color));
                    let old_b = SelectObject(mem, brush.into());
                    let old_p = SelectObject(mem, GetStockObject(NULL_PEN));
                    let r = self.px(8.0);
                    let _ = RoundRect(mem, left, top, right + 1, top + row_h + 1, r, r);
                    SelectObject(mem, old_p);
                    SelectObject(mem, old_b);
                    let _ = DeleteObject(brush.into());
                    if i == self.sel {
                        let accent = CreateSolidBrush(COLORREF(self.theme.accent));
                        let bar = self.px(3.0);
                        let inset = self.px(16.0);
                        let ab = SelectObject(mem, accent.into());
                        let ap = SelectObject(mem, GetStockObject(NULL_PEN));
                        let _ = RoundRect(mem, left, top + inset, left + bar + 1, top + row_h - inset + 1, bar, bar);
                        SelectObject(mem, ap);
                        SelectObject(mem, ab);
                        let _ = DeleteObject(accent.into());
                    }
                }

                let ix = self.px(ICON_X);
                let iy = top + (row_h - icon) / 2;
                if let Some(g) = item.glyph {
                    let mut text: Vec<u16> = g.to_string().encode_utf16().collect();
                    let mut gr = RECT { left: ix, top: iy, right: ix + icon, bottom: iy + icon };
                    SelectObject(mem, self.fonts.actions.into());
                    SetTextColor(mem, COLORREF(self.theme.text));
                    DrawTextW(mem, &mut text, &mut gr, DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX);
                }
                let drawn = item.glyph.is_some() || item.icon.as_deref().is_some_and(|k| self.icons.draw(mem, k, ix, iy));
                if !drawn {
                    self.icons.draw(mem, &std::env::current_exe().unwrap_or_default().to_string_lossy(), ix, iy);
                }

                // "Alt+N" badge on the right: Alt+digit runs the N-th visible row.
                let mut text_right = right - self.px(12.0);
                if i < self.shortcut_rows() {
                    let row_bg = if i == self.sel {
                        self.theme.sel
                    } else if Some(i) == self.hover {
                        blend(self.theme.sel, self.theme.bg)
                    } else {
                        self.theme.bg
                    };
                    let mut label = wide(&format!("Alt+{}", i + 1));
                    label.pop();
                    SelectObject(mem, self.fonts.sub.into());
                    let mut m = RECT::default();
                    DrawTextW(mem, &mut label, &mut m, DT_SINGLELINE | DT_CALCRECT | DT_NOPREFIX);
                    let (pw, ph) = (self.px(6.0), self.px(3.0));
                    let (bw, bh) = (m.right + 2 * pw, m.bottom + 2 * ph);
                    let badge = RECT { left: text_right - bw, top: top + (row_h - bh) / 2, right: text_right, bottom: top + (row_h + bh) / 2 };
                    let brush = CreateSolidBrush(COLORREF(mix(self.theme.text, row_bg, 0.08)));
                    let ob = SelectObject(mem, brush.into());
                    let op = SelectObject(mem, GetStockObject(NULL_PEN));
                    let r = self.px(4.0);
                    let _ = RoundRect(mem, badge.left, badge.top, badge.right + 1, badge.bottom + 1, r, r);
                    SelectObject(mem, op);
                    SelectObject(mem, ob);
                    let _ = DeleteObject(brush.into());
                    let mut br = badge;
                    SetTextColor(mem, COLORREF(self.theme.muted));
                    DrawTextW(mem, &mut label, &mut br, DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX);
                    text_right = badge.left - self.px(12.0);
                }
                if item.pinned {
                    let mut pin: Vec<u16> = "\u{E718}".encode_utf16().collect();
                    let w = self.px(16.0);
                    let mut pr = RECT { left: text_right - w, top, right: text_right, bottom: top + row_h };
                    SelectObject(mem, self.fonts.icons.into());
                    SetTextColor(mem, COLORREF(self.theme.accent));
                    DrawTextW(mem, &mut pin, &mut pr, DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX);
                    text_right = pr.left - self.px(8.0);
                }

                let tx = self.px(TEXT_X);
                let mut title = wide(&item.title);
                title.pop();
                let has_sub = !item.sub.is_empty();
                let mut tr = if has_sub {
                    RECT { left: tx, top: top + self.px(7.0), right: text_right, bottom: top + self.px(29.0) }
                } else {
                    RECT { left: tx, top, right: text_right, bottom: top + row_h }
                };
                SelectObject(mem, self.fonts.title.into());
                SetTextColor(mem, COLORREF(self.theme.text));
                DrawTextW(mem, &mut title, &mut tr, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
                if has_sub {
                    let mut sub = wide(&item.sub);
                    sub.pop();
                    let mut sr = RECT { left: tx, top: top + self.px(28.0), right: text_right, bottom: top + self.px(46.0) };
                    SelectObject(mem, self.fonts.sub.into());
                    SetTextColor(mem, COLORREF(self.theme.muted));
                    DrawTextW(mem, &mut sub, &mut sr, DT_SINGLELINE | DT_VCENTER | DT_PATH_ELLIPSIS | DT_NOPREFIX);
                }
            }

            let _ = BitBlt(hdc, 0, 0, rc.right, rc.bottom, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old_bmp);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
        }
    }

    /// Height of the query band below the top padding, DIPs: `input_height` if set,
    /// otherwise Flow's proportions for the font size (see BAND_EM).
    fn input_h(&self) -> f32 {
        let cfg = &self.engine.cfg;
        let band = if cfg.input_height > 0.0 { cfg.input_height } else { cfg.input_font_size * BAND_EM };
        band - PAD
    }

    /// Top and bottom of the area the query is centered in, px: from the window's top edge to
    /// the separator (or the bottom edge when there are no results), so the gaps match visually.
    fn query_band(&self) -> (i32, i32) {
        (0, self.px(PAD + self.input_h()))
    }

    /// `search_icon` from the config, in the icon column: an image/exe/ico path is drawn as
    /// an icon, any other text (a symbol, an emoji) as a glyph; empty draws nothing.
    fn paint_search_icon(&mut self, hdc: HDC) {
        let spec = self.engine.cfg.search_icon.clone();
        if spec.is_empty() {
            return;
        }
        let band = self.query_band();
        if std::path::Path::new(&spec).exists() {
            let size = self.px(ICON.min(self.engine.cfg.input_font_size * 1.4));
            let x = self.px(ICON_X) + (self.px(ICON) - size) / 2;
            self.icons.draw_sized(hdc, &spec, x, band.0 + (band.1 - band.0 - size) / 2, size);
            return;
        }
        let mut text: Vec<u16> = spec.encode_utf16().collect();
        let mut r = RECT { left: self.px(ICON_X), top: band.0, right: self.px(ICON_X + ICON), bottom: band.1 };
        unsafe {
            SelectObject(hdc, self.fonts.glyph.into());
            SetTextColor(hdc, COLORREF(self.theme.muted));
            DrawTextW(hdc, &mut text, &mut r, DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX);
        }
    }

    fn apply_theme(&mut self) {
        let system_dark = !apps_use_light_theme();
        let (theme, dark) = match self.engine.cfg.theme.as_str() {
            "light" => (LIGHT, false),
            "dark" => (DARK, true),
            "amoled" => (AMOLED, true),
            "windows" => (windows_theme(system_dark), system_dark),
            _ => (if system_dark { DARK } else { LIGHT }, system_dark),
        };
        self.theme = theme;
        unsafe {
            let _ = DeleteObject(self.bg_brush.into());
            self.bg_brush = CreateSolidBrush(COLORREF(self.theme.bg));
            EDIT_TEXT.store(self.theme.text, Ordering::Relaxed);
            EDIT_BG.store(self.theme.bg, Ordering::Relaxed);
            EDIT_BRUSH.store(self.bg_brush.0 as isize, Ordering::Relaxed);
            let on: BOOL = dark.into();
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const _ as _, size_of::<BOOL>() as u32);
            // The 1px window border in the theme's line color (a grey frame breaks AMOLED black).
            let border = self.theme.line;
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);
        }
    }

    fn apply_dpi(&mut self) {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        self.scale = dpi as f32 / 96.0;
        self.fonts.delete();
        self.fonts = Fonts::new(self.scale, self.engine.cfg.input_font_size);
        self.icons.reset(self.px(ICON));
    }
}

/// Mid-tone between two COLORREFs, for hover.
fn blend(a: u32, b: u32) -> u32 {
    let ch = |s: u32| (((a >> s) & 0xFF) + ((b >> s) & 0xFF)) / 2;
    ch(0) | (ch(8) << 8) | (ch(16) << 16)
}

fn apps_use_light_theme() -> bool {
    let mut v = 1u32;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut _ as _),
            Some(&mut size),
        )
    };
    r.is_err() || v != 0
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

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: TRAY_ID, ..Default::default() }
}

fn copy_wide(dst: &mut [u16], s: &str) {
    let max = dst.len() - 1;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(max).chain(Some(0))) {
        *d = c;
    }
}

fn add_tray(hwnd: HWND) {
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
static NOTICE_FOLDER: Mutex<String> = Mutex::new(String::new());

/// Notification from the tray icon (Windows 11 shows it as a toast).
fn notify(hwnd: HWND, title: &str, text: &str) {
    notify_with(hwnd, title, text, None, "");
}

/// `icon`: resource id of a large icon to show instead of the generic info one.
fn notify_with(hwnd: HWND, title: &str, text: &str, icon: Option<u16>, folder: &str) {
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
fn notify_from_json(hwnd: HWND, json: &str) {
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
const YAD_ICON: u16 = 2;
const VD_ICON: u16 = 3;
/// Sent to the tray callback when the user clicks a notification.
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

fn autostart_enabled() -> bool {
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, w!("LiteLaunch"), RRF_RT_REG_SZ, None, None, Some(&mut size)).is_ok() }
}

fn set_autostart(on: bool) {
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

fn tray_menu(hwnd: HWND) {
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
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, 0, 0, SWP_NOSIZE | SWP_SHOWWINDOW);
        bring_to_front(hwnd);
        let _ = SetFocus(Some(edit));
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
    }
}

fn bring_to_front(hwnd: HWND) {
    unsafe {
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

/// Deletes the word before the caret (Ctrl+Backspace, which a plain EDIT doesn't support).
fn delete_word(edit: HWND) {
    unsafe {
        let mut start = 0u32;
        let mut end = 0u32;
        SendMessageW(edit, EM_GETSEL, Some(WPARAM(&mut start as *mut _ as usize)), Some(LPARAM(&mut end as *mut _ as isize)));
        let len = GetWindowTextLengthW(edit);
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(edit, &mut buf) as usize;
        let chars = &buf[..n];
        let mut i = start as usize;
        if start != end {
            i = start as usize;
        } else {
            while i > 0 && chars[i - 1] == b' ' as u16 {
                i -= 1;
            }
            while i > 0 && chars[i - 1] != b' ' as u16 {
                i -= 1;
            }
        }
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(i)), Some(LPARAM(end as isize)));
        let empty = [0u16];
        SendMessageW(edit, EM_REPLACESEL, Some(WPARAM(1)), Some(LPARAM(empty.as_ptr() as isize)));
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

/// The caret is at the end of the query (nothing selected or a selection reaching the end),
/// so Right arrow has nothing left to move over.
fn caret_at_end(edit: HWND) -> bool {
    unsafe {
        let mut start = 0u32;
        let mut end = 0u32;
        SendMessageW(edit, EM_GETSEL, Some(WPARAM(&mut start as *mut _ as usize)), Some(LPARAM(&mut end as *mut _ as isize)));
        end as i32 >= GetWindowTextLengthW(edit)
    }
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

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let parent = unsafe { GetParent(hwnd).unwrap_or_default() };
    let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    match msg {
        WM_KEYDOWN => match VIRTUAL_KEY(wp.0 as u16) {
            VK_DOWN if ctrl => return { shift_pin(parent, 1); LRESULT(0) },
            VK_UP if ctrl => return { shift_pin(parent, -1); LRESULT(0) },
            VK_DOWN => return { move_selection(parent, 1); LRESULT(0) },
            VK_UP => return { move_selection(parent, -1); LRESULT(0) },
            VIRTUAL_KEY(0x50) if ctrl => return { toggle_pin(parent); LRESULT(0) }, // Ctrl+P
            VK_NEXT => return { move_selection(parent, 5); LRESULT(0) },
            VK_PRIOR => return { move_selection(parent, -5); LRESULT(0) },
            VK_RETURN => return { activate(parent, None, ctrl && shift); LRESULT(0) },
            VK_ESCAPE => return { if !toggle_menu(parent, false) { hide(parent) }; LRESULT(0) },
            VK_LEFT if with(|s| s.menu.is_some()).unwrap_or(false) => return { toggle_menu(parent, false); LRESULT(0) },
            VK_RIGHT if !ctrl && !shift && caret_at_end(hwnd) => {
                if toggle_menu(parent, true) {
                    return LRESULT(0);
                }
            }
            VK_TAB => {
                // Tab completes a keyword hint or the selected title.
                let q = with(|s| {
                    s.items.get(s.sel).map(|it| match &it.action {
                        Action::SetQuery(q) => q.clone(),
                        _ => it.title.clone(),
                    })
                })
                .flatten();
                if let Some(q) = q {
                    set_query(parent, &q);
                }
                return LRESULT(0);
            }
            _ => {}
        },
        // Swallow the chars of keys handled above so the EDIT doesn't beep or insert them.
        WM_CHAR => match wp.0 as u32 {
            0x0D | 0x1B | 0x09 | 0x10 => return LRESULT(0), // 0x10: Ctrl+P
            0x7F => return { delete_word(hwnd); LRESULT(0) },
            0x01 => return { unsafe { SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1))) }; LRESULT(0) },
            c => {
                if let Some(en) = english_char(c, lp) {
                    let prev = EDIT_PROC.load(Ordering::Relaxed);
                    return unsafe { CallWindowProcW(std::mem::transmute::<isize, WNDPROC>(prev), hwnd, msg, WPARAM(en as usize), lp) };
                }
            }
        },
        // Alt+1..9: run the N-th visible row (the badge on its right).
        WM_SYSKEYDOWN if (0x31..=0x39).contains(&wp.0) => {
            let n = wp.0 - 0x31;
            if let Some(i) = with(|s| (n < s.shortcut_rows()).then_some(n)).flatten() {
                with(|s| s.select(i));
                activate(parent, Some(i), false);
            }
            return LRESULT(0);
        }
        // Its WM_SYSCHAR would otherwise beep.
        WM_SYSCHAR if (0x31..=0x39).contains(&wp.0) => return LRESULT(0),
        WM_MOUSEWHEEL => {
            unsafe { SendMessageW(parent, msg, Some(wp), Some(lp)) };
            return LRESULT(0);
        }
        _ => {}
    }
    let _ = shift;
    let prev = EDIT_PROC.load(Ordering::Relaxed);
    unsafe { CallWindowProcW(std::mem::transmute::<isize, WNDPROC>(prev), hwnd, msg, wp, lp) }
}

/// Keyboard layout of the first keystroke since the window was shown (0: none yet).
static SESSION_LAYOUT: AtomicIsize = AtomicIsize::new(0);
/// SESSION_LAYOUT value once the user switched layouts in this session.
const LAYOUT_SWITCHED: isize = -1;

fn is_english(hkl: HKL) -> bool {
    hkl.0 as usize & 0x3FF == 0x09 // LANG_ENGLISH in the low word
}

/// With `force_english`, the character the same key would type in an English layout:
/// typing "туфлиц" in a Russian layout gives "telegram". The system layout is left alone.
/// Switching the layout while the window is open turns this off until it is shown again,
/// so Cyrillic can still be typed on purpose.
fn english_char(c: u32, lp: LPARAM) -> Option<u16> {
    if c < 0x20 || !with(|s| s.engine.cfg.force_english).unwrap_or(false) {
        return None;
    }
    unsafe {
        let current = GetKeyboardLayout(0);
        let session = SESSION_LAYOUT.compare_exchange(0, current.0 as isize, Ordering::Relaxed, Ordering::Relaxed).unwrap_or_else(|v| v);
        if session == LAYOUT_SWITCHED {
            return None;
        }
        if session != 0 && session != current.0 as isize {
            // Switched on purpose; stays off even if they switch back.
            SESSION_LAYOUT.store(LAYOUT_SWITCHED, Ordering::Relaxed);
            return None;
        }
        if is_english(current) {
            return None;
        }
        let mut layouts = [HKL::default(); 32];
        let n = GetKeyboardLayoutList(Some(&mut layouts)) as usize;
        let en = layouts[..n.min(32)].iter().copied().find(|&l| is_english(l))?;
        let scan = ((lp.0 >> 16) & 0xFF) as u32;
        let vk = MapVirtualKeyExW(scan, MAPVK_VSC_TO_VK_EX, Some(en));
        let mut state = [0u8; 256];
        GetKeyboardState(&mut state).ok()?;
        let mut buf = [0u16; 4];
        // Flag 4: don't touch the kernel's dead-key state.
        (ToUnicodeEx(vk, scan, &state, &mut buf, 4, Some(en)) == 1 && buf[0] >= 0x20).then_some(buf[0])
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
        WM_ACTIVATE => {
            if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE {
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

/// "Alt+Space" -> (MOD_ALT, VK_SPACE).
fn parse_hotkey(s: &str) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = MOD_NOREPEAT;
    let mut vk = None;
    for part in s.split('+').map(|p| p.trim().to_lowercase()) {
        match part.as_str() {
            "alt" => mods |= MOD_ALT,
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "shift" => mods |= MOD_SHIFT,
            "win" => mods |= MOD_WIN,
            "space" => vk = Some(VK_SPACE.0 as u32),
            "tab" => vk = Some(VK_TAB.0 as u32),
            "enter" => vk = Some(VK_RETURN.0 as u32),
            "`" | "oem3" => vk = Some(VK_OEM_3.0 as u32),
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => vk = Some(k.to_ascii_uppercase().as_bytes()[0] as u32),
            k if k.starts_with('f') && k[1..].parse::<u32>().is_ok_and(|n| (1..=24).contains(&n)) => {
                vk = Some(VK_F1.0 as u32 + k[1..].parse::<u32>().unwrap() - 1)
            }
            _ => return None,
        }
    }
    vk.map(|v| (mods, v))
}

fn spawn_py_worker(
    cfg: Config,
    hwnd: HWND,
    seq: Arc<AtomicU64>,
    child: Arc<Mutex<Option<Child>>>,
    reply: Arc<Mutex<Option<PyReply>>>,
    vd_warm: crate::vd::Warm,
) -> Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        while let Ok(mut job) = rx.recv() {
            // Debounce: wait until typing pauses, keep only the newest job.
            while let Ok(j) = rx.recv_timeout(Duration::from_millis(250)) {
                job = j;
            }
            if job.seq != seq.load(Ordering::SeqCst) {
                continue;
            }
            let warm = if job.plugin == PluginRef::Vd { crate::vd::take_warm(&vd_warm) } else { None };
            let r = pyplugin::query(&cfg.python, &job.config, &job.text, &job.settings, &child, warm);
            if matches!(&r, Err(e) if e == "cancelled") {
                continue;
            }
            *reply.lock().unwrap() = Some((job.seq, job.plugin, job.text, r));
            unsafe {
                let _ = PostMessageW(Some(HWND(h as _)), WM_APP_PY, WPARAM(job.seq as usize), LPARAM(0));
            }
        }
    });
    tx
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
        SendMessageW(edit, EM_SETCUEBANNER, Some(WPARAM(1)), Some(LPARAM(w!("Поиск программ, kill, st, vd, yad…").as_ptr() as isize)));
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
        rescan_apps(hwnd);
        std::thread::spawn(crate::yad::install);
        std::thread::spawn(crate::vd::install);

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
