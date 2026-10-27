//! Turns a query into results: keyword routing to Process Killer / Steam / Python plugins,
//! otherwise programs + calculator + keyword hints. Pinned results go first.

use crate::apps::App;
use crate::config::{Config, Hidden, Pins, Usage};
use crate::pyplugin::{PyResult, RpcAction};
use crate::{calc, fuzzy, procs, steam};
use std::sync::{Arc, RwLock};
use std::time::Instant;

pub struct Item {
    pub title: String,
    pub sub: String,
    /// Shell parsing name or file path to take the icon from.
    pub icon: Option<String>,
    pub score: i64,
    pub action: Action,
    /// Pinned to the current query: shown first, in the user's order.
    pub pinned: bool,
    /// The program's file on disk, for the actions menu.
    pub file: Option<String>,
    /// A Segoe Fluent Icons glyph drawn instead of `icon` (actions menu rows).
    pub glyph: Option<char>,
}

impl Item {
    pub fn new(title: impl Into<String>, sub: impl Into<String>, icon: Option<String>, score: i64, action: Action) -> Self {
        Self { title: title.into(), sub: sub.into(), icon, score, action, pinned: false, file: None, glyph: None }
    }

    fn with_file(mut self, file: Option<String>) -> Self {
        self.file = file;
        self
    }

    /// The actions menu (Right arrow) for this result; empty if it has none.
    pub fn actions(&self) -> Vec<Item> {
        let mut v = Vec::new();
        let action = |glyph: char, title: &str, action: Action| {
            let mut it = Item::new(title, self.title.clone(), None, 0, action);
            it.glyph = Some(glyph);
            it
        };
        if let Some(file) = &self.file {
            if file.to_lowercase().ends_with(".exe") {
                v.push(action('\u{E7EF}', "Запустить от имени администратора", Action::RunAsAdmin(file.clone())));
            }
            v.push(action('\u{E838}', "Показать в папке", Action::Reveal(file.clone())));
        }
        if let Some(target) = self.target() {
            v.push(action('\u{ED1A}', "Скрыть из результатов", Action::Hide(target.to_string())));
        }
        v
    }

    /// The launch target of results that can be pinned (apps, games).
    pub fn target(&self) -> Option<&str> {
        match &self.action {
            Action::Launch(t) => Some(t),
            _ => None,
        }
    }
}

pub enum Action {
    /// ShellExecute target; also the usage-count and pin key.
    Launch(String),
    Copy(String),
    Kill(Vec<u32>),
    SetQuery(String),
    Py { plugin: PluginRef, action: RpcAction },
    Yad(crate::yad::Op),
    Vd(crate::vd::Op),
    RunAsAdmin(String),
    /// Opens Explorer with the file selected.
    Reveal(String),
    Hide(String),
    /// Removes a Python plugin from the config, or disables a built-in one (by keyword).
    RemovePlugin(String),
    Nothing,
}

/// Who answers a plugin query: a Flow plugin from the config, or the built-in vd.
#[derive(Clone, Copy, PartialEq)]
pub enum PluginRef {
    User(usize),
    Vd,
}

/// Plugin results as items, best first.
pub fn py_items(plugin: PluginRef, mut results: Vec<PyResult>) -> Vec<Item> {
    results.sort_by(|a, b| b.score.cmp(&a.score));
    results
        .into_iter()
        .map(|r| {
            let action = r.action.map_or(Action::Nothing, |action| Action::Py { plugin, action });
            Item::new(r.title, r.sub, r.icon, r.score, action)
        })
        .collect()
}

pub struct Engine {
    pub cfg: Config,
    pub apps: Arc<RwLock<Vec<App>>>,
    pub usage: Usage,
    pub pins: Pins,
    pub hidden: Hidden,
    pub vd_cache: crate::vd::Cache,
    games: Vec<steam::Game>,
    games_at: Option<Instant>,
}

/// What the UI should do next besides showing `items`.
pub struct Outcome {
    pub items: Vec<Item>,
    /// Python plugin and query text to run in the background.
    pub plugin_query: Option<(PluginRef, String)>,
    /// Start a prewarmed vd Python: a link is probably coming.
    pub warm_vd: bool,
}

impl Engine {
    pub fn new(cfg: Config, apps: Arc<RwLock<Vec<App>>>) -> Self {
        Self { cfg, apps, usage: Usage::load(), pins: Pins::load(), hidden: Hidden::load(), vd_cache: Default::default(), games: Vec::new(), games_at: None }
    }

    pub fn search(&mut self, query: &str) -> Outcome {
        let mut out = Outcome { items: Vec::new(), plugin_query: None, warm_vd: false };
        if query.trim().is_empty() {
            return out;
        }
        if let Some((kw, rest)) = query.split_once(' ') {
            if !self.cfg.kill_keyword.is_empty() && kw.eq_ignore_ascii_case(&self.cfg.kill_keyword) {
                out.items = self.kill(rest.trim());
                return out;
            }
            if !self.cfg.steam_keyword.is_empty() && kw.eq_ignore_ascii_case(&self.cfg.steam_keyword) {
                out.items = self.steam(rest.trim());
                self.apply_pins(query, &mut out.items);
                return out;
            }
            let (yad, vd) = (&self.cfg.yad, &self.cfg.vd);
            if !yad.keyword.is_empty() && kw.eq_ignore_ascii_case(&yad.keyword) {
                out.items = crate::yad::search(yad, rest.trim());
                return out;
            }
            if !vd.keyword.is_empty() && kw.eq_ignore_ascii_case(&vd.keyword) {
                match crate::vd::search(vd, rest.trim(), &self.vd_cache) {
                    crate::vd::Found::Items(items) => {
                        out.warm_vd = vd.prewarm && crate::vd::has_lib() && items.iter().all(|it| matches!(it.action, Action::Nothing | Action::Vd(_) | Action::SetQuery(_)));
                        out.items = items;
                    }
                    crate::vd::Found::Fetch(url) => out.plugin_query = Some((PluginRef::Vd, url)),
                }
                return out;
            }
            if let Some(i) = self.cfg.plugins.iter().position(|p| p.keyword.eq_ignore_ascii_case(kw)) {
                out.plugin_query = Some((PluginRef::User(i), rest.trim().to_string()));
                return out;
            }
        }
        out.items = self.global(query.trim());
        self.apply_pins(query, &mut out.items);
        out
    }

    /// The actions menu for a result: its own actions, plus removing the plugin on keyword rows.
    pub fn actions(&self, item: &Item) -> Vec<Item> {
        let mut v = item.actions();
        if let Action::SetQuery(q) = &item.action {
            let kw = q.trim();
            let title = if self.cfg.plugins.iter().any(|p| p.keyword.eq_ignore_ascii_case(kw)) {
                "Удалить плагин"
            } else {
                "Отключить"
            };
            let mut it = Item::new(title, item.title.clone(), None, 0, Action::RemovePlugin(kw.to_string()));
            it.glyph = Some('\u{E74D}');
            v.push(it);
        }
        v
    }

    /// Python plugins leave the config (their files stay); built-ins lose their keyword,
    /// which turns them off. Saved to config.json; a keyword can be restored there.
    pub fn remove_plugin(&mut self, kw: &str) {
        let cfg = &mut self.cfg;
        cfg.plugins.retain(|p| !p.keyword.eq_ignore_ascii_case(kw));
        if cfg.kill_keyword.eq_ignore_ascii_case(kw) {
            cfg.kill_keyword.clear();
        }
        if cfg.steam_keyword.eq_ignore_ascii_case(kw) {
            cfg.steam_keyword.clear();
        }
        if cfg.yad.keyword.eq_ignore_ascii_case(kw) {
            cfg.yad.keyword.clear();
        }
        if cfg.vd.keyword.eq_ignore_ascii_case(kw) {
            cfg.vd.keyword.clear();
        }
        crate::config::save(cfg);
    }

    /// Puts the query's pinned targets first, in their saved order, whether or not they match it.
    fn apply_pins(&mut self, query: &str, items: &mut Vec<Item>) {
        let pins = self.pins.get(query).to_vec();
        if pins.is_empty() {
            return;
        }
        items.retain(|it| !it.target().is_some_and(|t| pins.iter().any(|p| p == t)));
        let pinned: Vec<Item> = pins
            .iter()
            .filter_map(|t| self.item_for_target(t))
            .map(|mut it| {
                it.pinned = true;
                it
            })
            .collect();
        items.splice(0..0, pinned);
    }

    /// The result for a launch target: an installed app or a Steam game.
    fn item_for_target(&mut self, target: &str) -> Option<Item> {
        if let Some(app) = self.apps.read().unwrap().iter().find(|a| a.target() == target) {
            return Some(app_item(app, 0));
        }
        if target.starts_with("steam://") {
            self.refresh_games();
            return self.games.iter().find(|g| game_target(g) == target).map(|g| game_item(g, 0));
        }
        None
    }

    /// Left out by the config's blacklist and not brought back by the whitelist.
    fn blacklisted(&self, app: &App) -> bool {
        let listed = |list: &[String]| list.iter().any(|p| matches_app(app, p));
        listed(&self.cfg.blacklist) && !listed(&self.cfg.whitelist)
    }

    fn refresh_games(&mut self) {
        if self.games_at.is_none_or(|t| t.elapsed().as_secs() > 60) {
            self.games = steam::games();
            // A Steam shortcut in the Start menu carries the game's real .ico: prefer it.
            let apps = self.apps.read().unwrap();
            for g in &mut self.games {
                if let Some(app) = apps.iter().find(|a| a.id == game_target(g)) {
                    g.icon = Some(app.target());
                }
            }
            drop(apps);
            self.games_at = Some(Instant::now());
        }
    }

    fn boost(&self, target: &str) -> i64 {
        (self.usage.get(target) as i64 * 25).min(300)
    }

    fn global(&mut self, q: &str) -> Vec<Item> {
        let mut items = Vec::new();
        if let Some(v) = calc::eval(q) {
            let s = calc::format(v);
            let calc_icon = Some("C:\\Windows\\System32\\calc.exe".to_string());
            items.push(Item::new(format!("= {s}"), "Калькулятор · Enter — скопировать", calc_icon, 2000, Action::Copy(s)));
        }
        for app in self.apps.read().unwrap().iter() {
            if self.hidden.contains(&app.target()) || self.blacklisted(app) {
                continue;
            }
            if let Some(sc) = fuzzy::score(&app.name, q) {
                items.push(app_item(app, sc as i64 + self.boost(&app.target())));
            }
        }
        // Built-in and plugin keywords, so they can be found by typing their start or name.
        // Steam gets none: "st" should show the Steam app itself; "st " starts the game search.
        let mut hints: Vec<(String, String, Option<String>)> =
            vec![(self.cfg.kill_keyword.clone(), "Process Killer".into(), Some("C:\\Windows\\System32\\Taskmgr.exe".into()))];
        hints.push((self.cfg.yad.keyword.clone(), "Yandex Music Downloader".into(), Some(crate::yad::icon())));
        hints.push((self.cfg.vd.keyword.clone(), "Video Downloader".into(), Some(crate::vd::icon())));
        for p in &self.cfg.plugins {
            let icon = std::path::Path::new(&p.dir).join(&p.icon);
            hints.push((p.keyword.clone(), p.name.clone(), icon.exists().then(|| icon.to_string_lossy().into())));
        }
        for (kw, name, icon) in hints.into_iter().filter(|(kw, _, _)| !kw.is_empty()) {
            let by_kw = kw.to_lowercase().starts_with(&q.to_lowercase());
            if by_kw || fuzzy::score(&name, q).is_some_and(|s| s >= 450) {
                items.push(Item::new(name, "", icon, if by_kw { 900 } else { 300 }, Action::SetQuery(format!("{kw} "))));
            }
        }
        items.sort_by(|a, b| b.score.cmp(&a.score));
        items.truncate(50);
        items
    }

    fn kill(&self, q: &str) -> Vec<Item> {
        let mut items: Vec<Item> = procs::list()
            .into_iter()
            .filter_map(|p| {
                let score = if q.is_empty() { 0 } else { fuzzy::score(&p.name, q)? as i64 };
                let count = if p.pids.len() > 1 { format!(" ×{}", p.pids.len()) } else { String::new() };
                let sub = format!("{} · {}", procs::format_mem(p.mem), p.path.as_deref().unwrap_or("нет доступа к пути"));
                Some(Item::new(format!("{}{count}", p.name), sub, p.path.clone(), score, Action::Kill(p.pids)))
            })
            .collect();
        // Stable: among equal scores the memory order from procs::list stays.
        items.sort_by(|a, b| b.score.cmp(&a.score));
        items
    }

    fn steam(&mut self, q: &str) -> Vec<Item> {
        self.refresh_games();
        let mut items: Vec<Item> = self
            .games
            .iter()
            .filter_map(|g| {
                if self.hidden.contains(&game_target(g)) {
                    return None;
                }
                let score = if q.is_empty() { 0 } else { fuzzy::score(&g.name, q)? as i64 };
                Some(game_item(g, score + self.boost(&game_target(g))))
            })
            .collect();
        items.sort_by(|a, b| b.score.cmp(&a.score));
        items
    }
}

/// A blacklist/whitelist entry: "@windows", an app name, an exe name, or a folder path.
fn matches_app(app: &App, pattern: &str) -> bool {
    if pattern.eq_ignore_ascii_case("@windows") {
        return app.system;
    }
    let p = pattern.to_lowercase();
    let location = app.location.to_lowercase();
    app.name.to_lowercase() == p
        || location.rsplit('\\').next() == Some(p.as_str())
        || (p.contains('\\') && location.starts_with(p.trim_end_matches('\\')))
}

fn app_item(app: &App, score: i64) -> Item {
    let target = app.target();
    let icon = app.icon.clone().unwrap_or_else(|| target.clone());
    Item::new(app.name.clone(), "", Some(icon), score, Action::Launch(target)).with_file(app.file.clone())
}

fn game_target(g: &steam::Game) -> String {
    format!("steam://rungameid/{}", g.appid)
}

fn game_item(g: &steam::Game, score: i64) -> Item {
    Item::new(g.name.clone(), "", g.icon.clone(), score, Action::Launch(game_target(g)))
        .with_file(g.exe.clone())
}
