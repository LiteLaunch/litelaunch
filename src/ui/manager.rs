//! The plugin manager: `litelaunch --plugins`, a window in a process of its own, so it costs the
//! launcher nothing while closed. Lists installed plugins, the ones in the catalog
//! (plugins.json, see catalog.rs) and the built-in ones, with details and buttons to install,
//! update, remove, or turn a built-in one on and off.
//!
//! Every change stops the launcher, does its work and starts the launcher again: the launcher
//! loads plugins and reads its config only at startup. The manager never loads plugin DLLs
//! itself (`litelaunch --plugin-info` tells it what is installed), so it never holds them.

use super::*;
use crate::catalog::{self, Catalog, Entry};
use crate::plugins::Installed;
use litelaunch_plugin::Info;
use std::collections::HashMap;

pub const MCLASS: PCWSTR = w!("LiteLaunchPlugins");

const WM_INSTALLED: u32 = WM_APP + 10;
const WM_CATALOG: u32 = WM_APP + 11;
const WM_ICONS: u32 = WM_APP + 12;
const WM_DONE: u32 = WM_APP + 13;

// Hand-offs from worker threads; the message tells the window to pick them up.
static INSTALLED_IN: Mutex<Option<Installed>> = Mutex::new(None);
static CATALOG_IN: Mutex<Option<std::result::Result<Catalog, String>>> = Mutex::new(None);
static ICONS_IN: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
static DONE_IN: Mutex<Option<std::result::Result<String, String>>> = Mutex::new(None);

// Layout in DIPs.
const WIDTH: f32 = 880.0;
const HEIGHT: f32 = 580.0;
const LIST_W: f32 = 300.0;
const PAD: f32 = 8.0;
const HEAD_H: f32 = 34.0;
const ROW_H: f32 = 52.0;
const ICON: f32 = 32.0;
const BIG_ICON: f32 = 48.0;
const STATUS_H: f32 = 30.0;

/// Built-in features with a keyword: they can be turned off (the keyword cleared) and on.
#[derive(Clone, Copy, PartialEq)]
enum Builtin {
    Kill,
    Steam,
    Clip,
    Shell,
}

impl Builtin {
    fn default_keyword(self) -> &'static str {
        match self {
            Builtin::Kill => "kill",
            Builtin::Steam => "st",
            Builtin::Clip => "cb",
            Builtin::Shell => ">",
        }
    }

    fn keyword(self, cfg: &mut Config) -> &mut String {
        match self {
            Builtin::Kill => &mut cfg.kill_keyword,
            Builtin::Steam => &mut cfg.steam_keyword,
            Builtin::Clip => &mut cfg.clip_keyword,
            Builtin::Shell => &mut cfg.shell_keyword,
        }
    }
}

#[derive(Clone)]
enum Btn {
    Install(Entry),
    Remove(String, String),
    Toggle(Builtin, String, bool),
    Open(String),
    Retry,
}

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Installed,
    Available,
    Builtin,
}

struct Card {
    /// Stable across refreshes, to keep the selection.
    key: String,
    section: Section,
    name: String,
    /// Under the name in the list.
    sub: String,
    icon: Option<String>,
    glyph: Option<char>,
    /// Under the name in the details: version and keyword.
    head: String,
    /// "Установлен", "Доступно обновление 1.2.0"...
    status: String,
    /// Status in the accent color (something to do).
    status_hot: bool,
    body: String,
    /// Muted lines under the body: requirements, author, data folder.
    meta: Vec<String>,
    /// Label, action, primary.
    buttons: Vec<(String, Btn, bool)>,
}

enum Row {
    Head(&'static str),
    Card(usize),
}

struct MFonts {
    big: HFONT,
    name: HFONT,
    body: HFONT,
    small: HFONT,
    head: HFONT,
    glyph: HFONT,
    big_glyph: HFONT,
}

impl MFonts {
    fn new(scale: f32) -> Self {
        let make = |px: f32, weight: i32, face: PCWSTR| unsafe {
            CreateFontW(-(px * scale).round() as i32, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, face)
        };
        let ui = w!("Segoe UI");
        let icons = w!("Segoe Fluent Icons");
        Self {
            big: make(22.0, 600, ui),
            name: make(14.0, 400, ui),
            body: make(13.5, 400, ui),
            small: make(12.0, 400, ui),
            head: make(12.0, 600, ui),
            glyph: make(18.0, 400, icons),
            big_glyph: make(26.0, 400, icons),
        }
    }

    fn delete(&self) {
        for f in [self.big, self.name, self.body, self.small, self.head, self.glyph, self.big_glyph] {
            unsafe {
                let _ = DeleteObject(f.into());
            }
        }
    }
}

enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

struct M {
    hwnd: HWND,
    scale: f32,
    theme: Theme,
    fonts: MFonts,
    icons: Icons,
    big_icons: Icons,
    cfg: Config,
    installed: Option<Installed>,
    catalog: Load<Catalog>,
    catalog_icons: HashMap<String, String>,
    cards: Vec<Card>,
    rows: Vec<Row>,
    sel: usize,
    /// List scroll offset, px.
    scroll: i32,
    hover_row: Option<usize>,
    hover_btn: Option<usize>,
    /// Button rectangles from the last paint.
    btn_rects: Vec<(RECT, usize)>,
    /// What is being done right now; buttons are off meanwhile.
    busy: Option<String>,
    /// The outcome of the last action: text, is an error.
    status: Option<(String, bool)>,
}

thread_local! {
    static MS: RefCell<Option<M>> = const { RefCell::new(None) };
}

fn with_m<R>(f: impl FnOnce(&mut M) -> R) -> Option<R> {
    MS.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn enc(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn data_dir(id: &str) -> std::path::PathBuf {
    config::dir().join(id)
}

impl M {
    fn px(&self, dip: f32) -> i32 {
        (dip * self.scale).round() as i32
    }

    fn apply_theme(&mut self) {
        let system_dark = !apps_use_light_theme();
        let (theme, dark) = match self.cfg.theme.as_str() {
            "light" => (LIGHT, false),
            "dark" => (DARK, true),
            "amoled" => (AMOLED, true),
            "windows" => (windows_theme(system_dark), system_dark),
            _ => (if system_dark { DARK } else { LIGHT }, system_dark),
        };
        self.theme = theme;
        let on: BOOL = dark.into();
        unsafe {
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const _ as _, size_of::<BOOL>() as u32);
        }
    }

    fn apply_dpi(&mut self) {
        self.scale = unsafe { GetDpiForWindow(self.hwnd) } as f32 / 96.0;
        self.fonts.delete();
        self.fonts = MFonts::new(self.scale);
        self.icons.reset(self.px(ICON));
        self.big_icons.reset(self.px(BIG_ICON));
    }

    fn entry(&self, id: &str) -> Option<&Entry> {
        match &self.catalog {
            Load::Ready(c) => c.plugins.iter().find(|e| e.id == id),
            _ => None,
        }
    }

    /// Rebuilds the cards from what is known, keeping the selected one.
    fn rebuild(&mut self) {
        let selected = self.selected_card().map(|c| c.key.clone());
        let mut cards = Vec::new();
        let installed = self.installed.as_ref().map(|i| i.plugins.clone()).unwrap_or_default();
        for (id, info) in &installed {
            cards.push(self.installed_card(id, info));
        }
        match &self.catalog {
            Load::Loading => cards.push(Card {
                key: "!catalog".into(),
                section: Section::Available,
                name: "Загружаю список плагинов…".into(),
                sub: "С GitHub".into(),
                icon: None,
                glyph: Some('\u{E896}'),
                head: String::new(),
                status: String::new(),
                status_hot: false,
                body: "Список плагинов скачивается с GitHub. Установленные плагины и встроенные функции можно настраивать уже сейчас.".into(),
                meta: Vec::new(),
                buttons: Vec::new(),
            }),
            Load::Failed(e) => cards.push(Card {
                key: "!catalog".into(),
                section: Section::Available,
                name: "Список плагинов недоступен".into(),
                sub: "Нет связи с GitHub?".into(),
                icon: None,
                glyph: Some('\u{E783}'),
                head: String::new(),
                status: String::new(),
                status_hot: false,
                body: format!("Не удалось получить список плагинов.\n\n{e}"),
                meta: vec![format!("Откуда: {}", self.catalog_url())],
                buttons: vec![("Повторить".into(), Btn::Retry, true)],
            }),
            Load::Ready(c) => {
                for e in c.plugins.iter().filter(|e| !installed.iter().any(|(id, _)| *id == e.id)) {
                    cards.push(self.available_card(e));
                }
            }
        }
        cards.extend(self.builtin_cards());

        let mut rows = Vec::new();
        for (section, title) in [(Section::Installed, "Установленные"), (Section::Available, "Доступные"), (Section::Builtin, "Встроенные")] {
            let idx: Vec<usize> = (0..cards.len()).filter(|&i| cards[i].section == section).collect();
            if idx.is_empty() {
                continue;
            }
            rows.push(Row::Head(title));
            rows.extend(idx.into_iter().map(Row::Card));
        }
        self.cards = cards;
        self.rows = rows;
        let keep = selected.and_then(|k| self.rows.iter().position(|r| matches!(r, Row::Card(i) if self.cards[*i].key == k)));
        self.sel = keep.or_else(|| self.rows.iter().position(|r| matches!(r, Row::Card(_)))).unwrap_or(0);
    }

    fn catalog_url(&self) -> String {
        if self.cfg.plugin_catalog.trim().is_empty() { catalog::DEFAULT_URL.to_string() } else { self.cfg.plugin_catalog.trim().to_string() }
    }

    fn installed_card(&self, id: &str, info: &Info) -> Card {
        let entry = self.entry(id).cloned();
        let keyword = self
            .cfg
            .plugin_settings
            .get(id)
            .unwrap_or(&info.defaults)
            .get("keyword")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let update = entry.as_ref().filter(|e| catalog::newer(&e.version, &info.version) && e.abi == litelaunch_plugin::ABI);
        let mut buttons = Vec::new();
        if let Some(e) = update {
            buttons.push((format!("Обновить до {}", e.version), Btn::Install(e.clone()), true));
        }
        buttons.push(("Удалить".into(), Btn::Remove(id.to_string(), info.name.clone()), false));
        if let Some(repo) = entry.as_ref().map(|e| e.repo.clone()).filter(|r| !r.is_empty()) {
            buttons.push(("Исходники".into(), Btn::Open(repo), false));
        }
        let data = data_dir(id);
        if data.is_dir() {
            buttons.push(("Папка данных".into(), Btn::Open(data.to_string_lossy().into_owned()), false));
        }
        let mut meta = Vec::new();
        if let Some(e) = &entry {
            if !e.requires.is_empty() {
                meta.push(format!("Нужно: {}", e.requires.join(", ")));
            }
            if !e.author.is_empty() {
                meta.push(format!("Автор: {}", e.author));
            }
        }
        meta.push(format!("Данные и настройки: {} (остаются после удаления)", data.to_string_lossy()));
        let kw = if keyword.is_empty() { "ключевое слово выключено".to_string() } else { format!("ключевое слово «{keyword}»") };
        Card {
            key: format!("p:{id}"),
            section: Section::Installed,
            name: info.name.clone(),
            sub: match update {
                Some(e) => format!("Есть обновление: {}", e.version),
                None => format!("{} · {}", info.version, if keyword.is_empty() { "выключен" } else { &keyword }),
            },
            icon: info.icon.clone().filter(|p| std::path::Path::new(p).is_file()),
            glyph: Some('\u{EA86}'),
            head: format!("Версия {} · {kw}", info.version),
            status: match update {
                Some(e) => format!("Доступно обновление: {} → {}", info.version, e.version),
                None => "Установлен".into(),
            },
            status_hot: update.is_some(),
            body: entry.as_ref().map(|e| e.details.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| info.description.clone()),
            meta,
            buttons,
        }
    }

    fn available_card(&self, e: &Entry) -> Card {
        let compatible = e.abi == litelaunch_plugin::ABI;
        let mut buttons = Vec::new();
        if compatible {
            let size = if e.size > 0 { format!(" ({} КБ)", e.size.div_ceil(1024)) } else { String::new() };
            buttons.push((format!("Установить{size}"), Btn::Install(e.clone()), true));
        }
        if !e.repo.is_empty() {
            buttons.push(("Исходники".into(), Btn::Open(e.repo.clone()), false));
        }
        let mut meta = Vec::new();
        if !e.requires.is_empty() {
            meta.push(format!("Нужно: {}", e.requires.join(", ")));
        }
        if !e.author.is_empty() {
            meta.push(format!("Автор: {}", e.author));
        }
        Card {
            key: format!("p:{}", e.id),
            section: Section::Available,
            name: e.name.clone(),
            sub: e.description.clone(),
            icon: self.catalog_icons.get(&e.id).cloned(),
            glyph: Some('\u{EA86}'),
            head: format!("Версия {} · ключевое слово «{}»", e.version, e.keyword),
            status: if compatible { "Не установлен".into() } else { "Нужна более новая версия LiteLaunch".into() },
            status_hot: false,
            body: if e.details.is_empty() { e.description.clone() } else { e.details.clone() },
            meta,
            buttons,
        }
    }

    fn builtin_cards(&self) -> Vec<Card> {
        let fixed = |key: &str, name: &str, sub: &str, glyph: char, body: &str| Card {
            key: format!("b:{key}"),
            section: Section::Builtin,
            name: name.into(),
            sub: sub.into(),
            icon: None,
            glyph: Some(glyph),
            head: "Встроен в LiteLaunch · без ключевого слова".into(),
            status: "Всегда включён".into(),
            status_hot: false,
            body: body.into(),
            meta: Vec::new(),
            buttons: Vec::new(),
        };
        let mut cfg = self.cfg.clone();
        let mut switchable = |b: Builtin, name: &str, glyph: char, body: &str| {
            let kw = b.keyword(&mut cfg).clone();
            let on = !kw.is_empty();
            let shown = if on { kw.clone() } else { b.default_keyword().to_string() };
            Card {
                key: format!("b:{}", b.default_keyword()),
                section: Section::Builtin,
                name: name.into(),
                sub: if on { format!("«{kw}»") } else { "выключен".into() },
                icon: None,
                glyph: Some(glyph),
                head: format!("Встроен в LiteLaunch · ключевое слово «{shown}»"),
                status: if on { "Включён".into() } else { "Выключен".into() },
                status_hot: false,
                body: body.into(),
                meta: vec!["Ключевое слово можно поменять в config.json (трей → Настройки).".into()],
                buttons: vec![if on {
                    ("Выключить".into(), Btn::Toggle(b, name.into(), false), false)
                } else {
                    ("Включить".into(), Btn::Toggle(b, name.into(), true), true)
                }],
            }
        };
        vec![
            fixed(
                "apps",
                "Программы",
                "Поиск по меню «Пуск»",
                '\u{E71D}',
                "Ищет программы из меню «Пуск», обычные и из Microsoft Store. Чем чаще вы запускаете программу, тем выше она в списке.\n\nCtrl+Shift+Enter запускает от имени администратора, стрелка вправо открывает меню действий, Ctrl+P закрепляет результат за текущим запросом.",
            ),
            fixed(
                "calc",
                "Калькулятор",
                "2+2*2, sqrt(16)^2",
                '\u{E8EF}',
                "Напишите выражение прямо в строке поиска: 2+2*2, sqrt(16)^2, (3+4)/7. Результат появится первой строкой, Enter копирует его.",
            ),
            switchable(
                Builtin::Kill,
                "Process Killer",
                '\u{E74D}',
                "Показывает запущенные процессы, сгруппированные по программе и отсортированные по занятой памяти. Enter завершает все экземпляры.",
            ),
            switchable(Builtin::Steam, "Steam", '\u{E7FC}', "Установленные игры Steam из всех библиотек. Enter запускает игру через Steam."),
            switchable(
                Builtin::Clip,
                "История буфера обмена",
                '\u{E77F}',
                "Помнит, что вы копировали. Enter вставляет запись в окно, где вы были, Shift+Enter только копирует.\n\nВыключенная история ничего не записывает.",
            ),
            switchable(
                Builtin::Shell,
                "Командная строка",
                '\u{E756}',
                "Выполняет команду PowerShell в Windows Terminal: «> ipconfig». Помнит историю команд, Ctrl+Shift+Enter запускает от имени администратора.",
            ),
        ]
    }

    fn selected_card(&self) -> Option<&Card> {
        match self.rows.get(self.sel)? {
            Row::Card(i) => self.cards.get(*i),
            Row::Head(_) => None,
        }
    }

    fn row_h(&self, r: &Row) -> i32 {
        self.px(match r {
            Row::Head(_) => HEAD_H,
            Row::Card(_) => ROW_H,
        })
    }

    fn list_height(&self) -> i32 {
        self.rows.iter().map(|r| self.row_h(r)).sum::<i32>() + self.px(PAD * 2.0)
    }

    fn client(&self) -> RECT {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        rc
    }

    fn list_bottom(&self) -> i32 {
        self.client().bottom - self.px(STATUS_H)
    }

    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        if x >= self.px(LIST_W) || y >= self.list_bottom() {
            return None;
        }
        let mut top = self.px(PAD) - self.scroll;
        for (i, r) in self.rows.iter().enumerate() {
            let h = self.row_h(r);
            if y >= top && y < top + h {
                return matches!(r, Row::Card(_)).then_some(i);
            }
            top += h;
        }
        None
    }

    /// Scrolls the list so the selected row is visible.
    fn reveal(&mut self) {
        let top: i32 = self.px(PAD) + self.rows[..self.sel.min(self.rows.len())].iter().map(|r| self.row_h(r)).sum::<i32>();
        let h = self.rows.get(self.sel).map_or(0, |r| self.row_h(r));
        let view = self.list_bottom();
        if top - self.scroll < 0 {
            self.scroll = top - self.px(HEAD_H + PAD);
        } else if top + h - self.scroll > view {
            self.scroll = top + h - view + self.px(PAD);
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let max = (self.list_height() - self.list_bottom()).max(0);
        self.scroll = self.scroll.clamp(0, max);
    }

    fn move_sel(&mut self, delta: isize) {
        let cards: Vec<usize> = (0..self.rows.len()).filter(|&i| matches!(self.rows[i], Row::Card(_))).collect();
        let Some(pos) = cards.iter().position(|&i| i == self.sel) else { return };
        let next = (pos as isize + delta).clamp(0, cards.len() as isize - 1) as usize;
        self.sel = cards[next];
        self.reveal();
    }
}

// ---------- painting ----------

unsafe fn fill(hdc: HDC, r: RECT, color: u32, radius: i32) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color));
        if radius == 0 {
            FillRect(hdc, &r, brush);
        } else {
            let ob = SelectObject(hdc, brush.into());
            let op = SelectObject(hdc, GetStockObject(NULL_PEN));
            let _ = RoundRect(hdc, r.left, r.top, r.right + 1, r.bottom + 1, radius, radius);
            SelectObject(hdc, op);
            SelectObject(hdc, ob);
        }
        let _ = DeleteObject(brush.into());
    }
}

/// Draws text; returns its height.
unsafe fn text(hdc: HDC, font: HFONT, color: u32, s: &str, mut r: RECT, flags: DRAW_TEXT_FORMAT) -> i32 {
    // An empty slice has a dangling pointer, which DrawTextW reads: nothing to draw anyway.
    if s.is_empty() {
        return 0;
    }
    unsafe {
        let mut w = enc(s);
        SelectObject(hdc, font.into());
        SetTextColor(hdc, COLORREF(color));
        DrawTextW(hdc, &mut w, &mut r, flags | DT_NOPREFIX)
    }
}

/// Size of wrapped text in a box `width` wide.
unsafe fn measure(hdc: HDC, font: HFONT, s: &str, width: i32, flags: DRAW_TEXT_FORMAT) -> (i32, i32) {
    if s.is_empty() {
        return (0, 0);
    }
    unsafe {
        let mut w = enc(s);
        let mut r = RECT { left: 0, top: 0, right: width, bottom: 0 };
        SelectObject(hdc, font.into());
        DrawTextW(hdc, &mut w, &mut r, flags | DT_CALCRECT | DT_NOPREFIX);
        (r.right, r.bottom)
    }
}

impl M {
    fn paint(&mut self, hdc: HDC) {
        let rc = self.client();
        unsafe {
            let mem = CreateCompatibleDC(Some(hdc));
            let bmp = CreateCompatibleBitmap(hdc, rc.right, rc.bottom);
            let old = SelectObject(mem, bmp.into());
            SetBkMode(mem, TRANSPARENT);
            fill(mem, rc, self.theme.bg, 0);
            let list_w = self.px(LIST_W);
            // The list sits on a slightly different shade, the details on the plain background.
            fill(mem, RECT { left: 0, top: 0, right: list_w, bottom: rc.bottom }, mix(self.theme.text, self.theme.bg, 0.03), 0);
            fill(mem, RECT { left: list_w, top: 0, right: list_w + self.px(1.0).max(1), bottom: rc.bottom }, self.theme.line, 0);

            self.paint_list(mem, rc);
            self.paint_details(mem, rc);
            self.paint_status(mem, rc);

            let _ = BitBlt(hdc, 0, 0, rc.right, rc.bottom, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
        }
    }

    unsafe fn paint_list(&mut self, hdc: HDC, rc: RECT) {
        let list_w = self.px(LIST_W);
        let bottom = self.list_bottom();
        unsafe {
            let clip = CreateRectRgn(0, 0, list_w, bottom);
            SelectClipRgn(hdc, Some(clip));
            let mut top = self.px(PAD) - self.scroll;
            let (left, right) = (self.px(PAD), list_w - self.px(PAD));
            for i in 0..self.rows.len() {
                let h = self.row_h(&self.rows[i]);
                if top + h > 0 && top < bottom {
                    match &self.rows[i] {
                        Row::Head(t) => {
                            let r = RECT { left: left + self.px(12.0), top: top + self.px(10.0), right, bottom: top + h };
                            text(hdc, self.fonts.head, self.theme.muted, &t.to_uppercase(), r, DT_SINGLELINE | DT_VCENTER);
                        }
                        Row::Card(ci) => {
                            let bg = mix(self.theme.text, self.theme.bg, 0.03);
                            if i == self.sel || Some(i) == self.hover_row {
                                let color = if i == self.sel { self.theme.sel } else { blend(self.theme.sel, bg) };
                                fill(hdc, RECT { left, top, right, bottom: top + h }, color, self.px(8.0));
                                if i == self.sel {
                                    let inset = self.px(16.0);
                                    let bar = self.px(3.0);
                                    fill(hdc, RECT { left, top: top + inset, right: left + bar, bottom: top + h - inset }, self.theme.accent, bar);
                                }
                            }
                            let card = &self.cards[*ci];
                            let (ix, iy, isz) = (left + self.px(12.0), top + (h - self.px(ICON)) / 2, self.px(ICON));
                            let icon = card.icon.clone();
                            let drawn = icon.as_deref().is_some_and(|k| self.icons.draw(hdc, k, ix, iy));
                            let card = &self.cards[*ci];
                            if !drawn {
                                self.paint_glyph(hdc, card.glyph.unwrap_or('\u{EA86}'), RECT { left: ix, top: iy, right: ix + isz, bottom: iy + isz }, false);
                            }
                            let tx = ix + isz + self.px(12.0);
                            let tr = right - self.px(8.0);
                            text(hdc, self.fonts.name, self.theme.text, &card.name, RECT { left: tx, top: top + self.px(7.0), right: tr, bottom: top + self.px(28.0) }, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
                            let sub_color = if card.status_hot { self.theme.accent } else { self.theme.muted };
                            text(hdc, self.fonts.small, sub_color, &card.sub, RECT { left: tx, top: top + self.px(27.0), right: tr, bottom: top + self.px(45.0) }, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
                        }
                    }
                }
                top += h;
            }
            SelectClipRgn(hdc, None);
            let _ = DeleteObject(clip.into());
        }
        let _ = rc;
    }

    /// A Segoe Fluent glyph on a soft tile, for rows without an icon.
    unsafe fn paint_glyph(&self, hdc: HDC, g: char, r: RECT, big: bool) {
        unsafe {
            fill(hdc, r, mix(self.theme.accent, self.theme.bg, 0.16), self.px(if big { 12.0 } else { 8.0 }));
            let font = if big { self.fonts.big_glyph } else { self.fonts.glyph };
            text(hdc, font, self.theme.accent, &g.to_string(), r, DT_SINGLELINE | DT_CENTER | DT_VCENTER);
        }
    }

    unsafe fn paint_details(&mut self, hdc: HDC, rc: RECT) {
        self.btn_rects.clear();
        let Some(ci) = (match self.rows.get(self.sel) {
            Some(Row::Card(i)) => Some(*i),
            _ => None,
        }) else {
            return;
        };
        let left = self.px(LIST_W + 32.0);
        let right = rc.right - self.px(32.0);
        let bottom = self.list_bottom();
        let mut y = self.px(28.0);
        unsafe {
            let big = self.px(BIG_ICON);
            let icon = self.cards[ci].icon.clone();
            let drawn = icon.as_deref().is_some_and(|k| self.big_icons.draw(hdc, k, left, y));
            let card = &self.cards[ci];
            if !drawn {
                self.paint_glyph(hdc, card.glyph.unwrap_or('\u{EA86}'), RECT { left, top: y, right: left + big, bottom: y + big }, true);
            }
            let tx = left + big + self.px(16.0);
            text(hdc, self.fonts.big, self.theme.text, &card.name, RECT { left: tx, top: y - self.px(2.0), right, bottom: y + self.px(30.0) }, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
            text(hdc, self.fonts.small, self.theme.muted, &card.head, RECT { left: tx, top: y + self.px(30.0), right, bottom: y + big }, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
            y += big + self.px(20.0);

            if !card.status.is_empty() {
                let color = if card.status_hot { self.theme.accent } else { self.theme.muted };
                let h = text(hdc, self.fonts.body, color, &format!("● {}", card.status), RECT { left, top: y, right, bottom }, DT_SINGLELINE);
                y += h + self.px(14.0);
            }

            if !card.buttons.is_empty() {
                let bh = self.px(32.0);
                let mut x = left;
                let off = self.busy.is_some();
                for (bi, (label, _, primary)) in card.buttons.iter().enumerate() {
                    let (tw, _) = measure(hdc, self.fonts.body, label, 10_000, DT_SINGLELINE);
                    let r = RECT { left: x, top: y, right: x + tw + self.px(32.0), bottom: y + bh };
                    let hover = self.hover_btn == Some(bi) && !off;
                    let (bg, fg) = if *primary {
                        let bg = if hover { mix(self.theme.accent, self.theme.bg, 0.85) } else { self.theme.accent };
                        (bg, if self.theme.bg > 0x808080 { rgb(0xFFFFFF) } else { rgb(0x000000) })
                    } else {
                        let base = mix(self.theme.text, self.theme.bg, 0.08);
                        (if hover { mix(self.theme.text, self.theme.bg, 0.14) } else { base }, self.theme.text)
                    };
                    let (bg, fg) = if off { (mix(self.theme.text, self.theme.bg, 0.06), self.theme.muted) } else { (bg, fg) };
                    fill(hdc, r, bg, self.px(6.0));
                    text(hdc, self.fonts.body, fg, label, r, DT_SINGLELINE | DT_CENTER | DT_VCENTER);
                    self.btn_rects.push((r, bi));
                    x = r.right + self.px(8.0);
                }
                y += bh + self.px(22.0);
            }

            let width = right - left;
            if !card.body.is_empty() {
                let (_, h) = measure(hdc, self.fonts.body, &card.body, width, DT_WORDBREAK);
                text(hdc, self.fonts.body, self.theme.text, &card.body, RECT { left, top: y, right, bottom: bottom.min(y + h) }, DT_WORDBREAK);
                y += h + self.px(16.0);
            }
            for line in &card.meta {
                if y >= bottom {
                    break;
                }
                let (_, h) = measure(hdc, self.fonts.small, line, width, DT_WORDBREAK);
                text(hdc, self.fonts.small, self.theme.muted, line, RECT { left, top: y, right, bottom }, DT_WORDBREAK);
                y += h + self.px(6.0);
            }
        }
    }

    unsafe fn paint_status(&self, hdc: HDC, rc: RECT) {
        let top = self.list_bottom();
        unsafe {
            fill(hdc, RECT { left: 0, top, right: rc.right, bottom: rc.bottom }, self.theme.bg, 0);
            fill(hdc, RECT { left: 0, top, right: rc.right, bottom: top + self.px(1.0).max(1) }, self.theme.line, 0);
            let (msg, color) = match (&self.busy, &self.status) {
                (Some(b), _) => (format!("{b}…"), self.theme.text),
                (None, Some((s, true))) => (s.clone(), rgb(0xD13438)),
                (None, Some((s, false))) => (s.clone(), self.theme.muted),
                (None, None) => {
                    let n = self.installed.as_ref().map_or(0, |i| i.plugins.len());
                    (format!("Установлено плагинов: {n} · список: {}", self.catalog_url()), self.theme.muted)
                }
            };
            let r = RECT { left: self.px(16.0), top, right: rc.right - self.px(16.0), bottom: rc.bottom };
            text(hdc, self.fonts.small, color, &msg, r, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        }
    }
}

// ---------- background work ----------

/// Asks `litelaunch --plugin-info` what is installed.
fn load_installed(hwnd: HWND) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new(std::env::current_exe().unwrap_or_default())
            .arg("--plugin-info")
            .creation_flags(crate::util::CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .output();
        let installed = match out {
            Ok(o) => serde_json::from_slice::<Installed>(&o.stdout).unwrap_or_else(|e| Installed { errors: vec![format!("--plugin-info: {e}")], ..Default::default() }),
            Err(e) => Installed { errors: vec![format!("--plugin-info: {e}")], ..Default::default() },
        };
        *INSTALLED_IN.lock().unwrap() = Some(installed);
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_INSTALLED, WPARAM(0), LPARAM(0));
        }
    });
}

fn load_catalog(hwnd: HWND, url: String) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let c = catalog::load(&url);
        let entries = c.as_ref().map(|c| c.plugins.clone()).unwrap_or_default();
        *CATALOG_IN.lock().unwrap() = Some(c);
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_CATALOG, WPARAM(0), LPARAM(0));
        }
        let icons: HashMap<String, String> = entries.iter().filter_map(|e| Some((e.id.clone(), catalog::icon(e)?))).collect();
        *ICONS_IN.lock().unwrap() = Some(icons);
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_ICONS, WPARAM(0), LPARAM(0));
        }
    });
}

/// Closes the running launcher and waits for it to be gone; false if none was running.
fn stop_launcher() -> bool {
    unsafe {
        let Ok(hwnd) = FindWindowW(CLASS, None) else { return false };
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let process = windows::Win32::System::Threading::OpenProcess(windows::Win32::System::Threading::PROCESS_SYNCHRONIZE, false, pid).ok();
        let cds = COPYDATASTRUCT { dwData: crate::CMD_EXIT, cbData: 0, lpData: std::ptr::null_mut() };
        SendMessageW(hwnd, WM_COPYDATA, None, Some(LPARAM(&cds as *const _ as isize)));
        if let Some(h) = process {
            windows::Win32::System::Threading::WaitForSingleObject(h, 5000);
            let _ = CloseHandle(h);
        }
        true
    }
}

fn start_launcher() {
    let _ = std::process::Command::new(std::env::current_exe().unwrap_or_default()).spawn();
}

fn run_button(hwnd: HWND, btn: Btn) {
    match btn {
        Btn::Open(target) => {
            shell_open(&target, w!("open"));
            return;
        }
        Btn::Retry => {
            let url = with_m(|m| {
                m.catalog = Load::Loading;
                m.rebuild();
                m.catalog_url()
            });
            if let Some(url) = url {
                load_catalog(hwnd, url);
            }
            invalidate(hwnd);
            return;
        }
        _ => {}
    }
    if let Btn::Remove(_, name) = &btn {
        let q = HSTRING::from(format!("Удалить «{name}»?\n\nЕго настройки и данные останутся: при повторной установке он продолжит с того же места."));
        let r = unsafe { MessageBoxW(Some(hwnd), &q, w!("Плагины LiteLaunch"), MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) };
        if r != IDYES {
            return;
        }
    }
    let busy = match &btn {
        Btn::Install(e) => format!("Устанавливаю {} {}", e.name, e.version),
        Btn::Remove(_, name) => format!("Удаляю {name}"),
        Btn::Toggle(_, name, on) => format!("{} {name}", if *on { "Включаю" } else { "Выключаю" }),
        _ => String::new(),
    };
    with_m(|m| {
        m.busy = Some(busy);
        m.status = None;
    });
    invalidate(hwnd);
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        // The launcher reads plugins and its config only at startup: stop it, change, start it.
        stop_launcher();
        let r = match btn {
            Btn::Install(e) => catalog::install(&e).map(|_| format!("{} {} установлен, LiteLaunch перезапущен", e.name, e.version)),
            Btn::Remove(id, name) => catalog::remove(&id).map(|_| format!("{name} удалён")),
            Btn::Toggle(b, name, on) => {
                let mut cfg = config::load();
                *b.keyword(&mut cfg) = if on { b.default_keyword().to_string() } else { String::new() };
                config::save(&cfg);
                Ok(format!("{name} {}", if on { "включён" } else { "выключен" }))
            }
            _ => Ok(String::new()),
        };
        start_launcher();
        *DONE_IN.lock().unwrap() = Some(r);
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_DONE, WPARAM(0), LPARAM(0));
        }
    });
}

fn click_button(hwnd: HWND, i: usize) {
    let btn = with_m(|m| {
        if m.busy.is_some() {
            return None;
        }
        m.selected_card().and_then(|c| c.buttons.get(i)).map(|b| b.1.clone())
    })
    .flatten();
    if let Some(b) = btn {
        run_button(hwnd, b);
    }
}

// ---------- window ----------

unsafe extern "system" fn mproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let xy = || ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            with_m(|m| m.paint(hdc));
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            with_m(|m| m.clamp_scroll());
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let scale = with_m(|m| m.scale).unwrap_or(1.0);
            let info = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
            info.ptMinTrackSize = POINT { x: (640.0 * scale) as i32, y: (420.0 * scale) as i32 };
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy();
            let changed = with_m(|m| {
                let row = m.row_at(x, y);
                let btn = m.btn_rects.iter().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(_, i)| *i);
                let changed = row != m.hover_row || btn != m.hover_btn;
                m.hover_row = row;
                m.hover_btn = btn;
                if changed {
                    let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    unsafe {
                        let _ = TrackMouseEvent(&mut t);
                    }
                }
                changed
            });
            if changed == Some(true) {
                invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let on_btn = with_m(|m| m.hover_btn.is_some() && m.busy.is_none()).unwrap_or(false);
            if on_btn && (lp.0 & 0xFFFF) as u32 == HTCLIENT {
                unsafe {
                    let _ = SetCursor(LoadCursorW(None, IDC_HAND).ok());
                }
                return LRESULT(1);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_MOUSELEAVE => {
            with_m(|m| {
                m.hover_row = None;
                m.hover_btn = None;
            });
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy();
            enum Hit {
                Row,
                Btn(usize),
                None,
            }
            let hit = with_m(|m| {
                if let Some(i) = m.row_at(x, y) {
                    m.sel = i;
                    m.hover_btn = None;
                    return Hit::Row;
                }
                match m.btn_rects.iter().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom) {
                    Some((_, i)) => Hit::Btn(*i),
                    None => Hit::None,
                }
            });
            match hit {
                Some(Hit::Row) => invalidate(hwnd),
                Some(Hit::Btn(i)) => click_button(hwnd, i),
                _ => {}
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
            with_m(|m| {
                m.scroll -= delta * m.px(ROW_H) / 120;
                m.clamp_scroll();
            });
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = VIRTUAL_KEY(wp.0 as u16);
            match key {
                VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_HOME | VK_END => {
                    let d = match key {
                        VK_UP => -1,
                        VK_DOWN => 1,
                        VK_PRIOR => -5,
                        VK_NEXT => 5,
                        VK_HOME => -1000,
                        _ => 1000,
                    };
                    with_m(|m| m.move_sel(d));
                    invalidate(hwnd);
                }
                VK_RETURN => click_button(hwnd, 0),
                VK_ESCAPE => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                _ => {}
            }
            LRESULT(0)
        }
        WM_INSTALLED => {
            let got = INSTALLED_IN.lock().unwrap().take();
            with_m(|m| {
                if let Some(i) = got {
                    if !i.errors.is_empty() && m.busy.is_none() {
                        m.status = Some((format!("Не загрузились: {}", i.errors.join("; ")), true));
                    }
                    m.installed = Some(i);
                }
                m.rebuild();
            });
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_CATALOG => {
            let got = CATALOG_IN.lock().unwrap().take();
            with_m(|m| {
                m.catalog = match got {
                    Some(Ok(c)) => Load::Ready(c),
                    Some(Err(e)) => Load::Failed(e),
                    None => Load::Loading,
                };
                m.rebuild();
            });
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_ICONS => {
            let got = ICONS_IN.lock().unwrap().take();
            with_m(|m| {
                m.catalog_icons = got.unwrap_or_default();
                m.rebuild();
            });
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_DONE => {
            let got = DONE_IN.lock().unwrap().take();
            with_m(|m| {
                m.busy = None;
                m.status = got.map(|r| match r {
                    Ok(s) => (s, false),
                    Err(e) => (e, true),
                });
                m.cfg = config::load();
            });
            load_installed(hwnd);
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            with_m(|m| m.apply_theme());
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            with_m(|m| m.apply_dpi());
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// `litelaunch --plugins`: runs the manager window until it is closed.
pub fn run_manager() -> Result<()> {
    unsafe {
        // One manager at a time: a second start brings the first one up.
        if let Ok(h) = FindWindowW(MCLASS, None) {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
            return Ok(());
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let instance = GetModuleHandleW(None)?;
        let icon = LoadIconW(Some(instance.into()), PCWSTR(1 as _)).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(mproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: icon,
            hIconSm: icon,
            lpszClassName: MCLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            MCLASS,
            w!("Плагины LiteLaunch"),
            WS_OVERLAPPEDWINDOW & !WS_MAXIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIDTH as i32,
            HEIGHT as i32,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        let cfg = config::load();
        let mut m = M {
            hwnd,
            scale,
            theme: LIGHT,
            fonts: MFonts::new(scale),
            icons: Icons::new((ICON * scale).round() as i32),
            big_icons: Icons::new((BIG_ICON * scale).round() as i32),
            cfg,
            installed: None,
            catalog: Load::Loading,
            catalog_icons: HashMap::new(),
            cards: Vec::new(),
            rows: Vec::new(),
            sel: 0,
            scroll: 0,
            hover_row: None,
            hover_btn: None,
            btn_rects: Vec::new(),
            busy: None,
            status: None,
        };
        m.apply_theme();
        m.rebuild();
        let url = m.catalog_url();
        MS.with(|s| *s.borrow_mut() = Some(m));

        // Centered on the monitor under the cursor. Moving there first lets the window take that
        // monitor's DPI (WM_DPICHANGED); then it is sized for it and centered by its real size.
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST), &mut mi);
        let wa = mi.rcWork;
        let _ = SetWindowPos(hwnd, None, wa.left + 1, wa.top + 1, 0, 0, SWP_NOZORDER | SWP_NOSIZE);
        let scale = with_m(|m| m.scale).unwrap_or(scale);
        let _ = SetWindowPos(hwnd, None, 0, 0, (WIDTH * scale) as i32, (HEIGHT * scale) as i32, SWP_NOZORDER | SWP_NOMOVE);
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let _ = SetWindowPos(hwnd, None, wa.left + (wa.right - wa.left - w) / 2, wa.top + (wa.bottom - wa.top - h) / 2, 0, 0, SWP_NOZORDER | SWP_NOSIZE);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);

        load_installed(hwnd);
        load_catalog(hwnd, url);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
