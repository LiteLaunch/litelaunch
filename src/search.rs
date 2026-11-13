//! Turns a query into results: keyword routing to Process Killer / Steam / Python plugins,
//! otherwise programs + calculator + keyword hints. Pinned results go first.

use crate::apps::App;
use crate::config::{Config, Hidden, Pins, Usage};
use crate::plugins::Native;
use crate::pyplugin::{PyResult, RpcAction};
use crate::{calc, fuzzy, procs, steam};
use serde_json::Value;
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
    /// A PowerShell command (see shell.rs).
    Shell(String),
    ShellForget(String),
    /// A clipboard history entry: paste it (see clip.rs).
    Clip(String),
    ClipForget(String),
    ClipClear,
    SetQuery(String),
    /// Index into the config's Flow plugins.
    Py { plugin: usize, action: RpcAction },
    /// A native plugin's own action (index into Engine::natives), handed back to it.
    Native { plugin: usize, op: Value, hide_first: bool },
    RunAsAdmin(String),
    /// Opens Explorer with the file selected.
    Reveal(String),
    Hide(String),
    /// Removes a Python plugin from the config, or disables a built-in one (by keyword).
    RemovePlugin(String),
    /// Opens the plugin manager window.
    OpenManager,
    /// Installs LiteLaunch's private Python (see python.rs).
    InstallPython,
    /// Downloads and runs the newer LiteLaunch installer (update.rs).
    UpdateLauncher,
    /// Asks GitHub for a newer LiteLaunch now.
    CheckUpdates,
    Nothing,
}

/// A native plugin's row as a result.
pub fn native_item(plugin: usize, it: litelaunch_plugin::Item) -> Item {
    use litelaunch_plugin::Action as A;
    let action = match it.action {
        A::Nothing => Action::Nothing,
        A::SetQuery(q) => Action::SetQuery(q),
        A::Copy(s) => Action::Copy(s),
        A::Launch(t) => Action::Launch(t),
        A::Plugin(op) => Action::Native { plugin, op, hide_first: it.hide_first },
        A::InstallPython => Action::InstallPython,
    };
    let mut item = Item::new(it.title, it.sub, it.icon, it.score, action).with_file(it.file);
    item.glyph = it.glyph;
    item
}

/// The plugin's view of one of its rows (for its actions menu).
fn to_native(item: &Item) -> litelaunch_plugin::Item {
    let action = match &item.action {
        Action::Native { op, .. } => litelaunch_plugin::Action::Plugin(op.clone()),
        _ => litelaunch_plugin::Action::Nothing,
    };
    let mut it = litelaunch_plugin::Item::new(item.title.clone(), item.sub.clone(), item.icon.clone(), item.score, action);
    it.file = item.file.clone();
    it
}

/// Slow plugin work for the background worker.
pub enum Slow {
    /// A Flow plugin (index into the config's plugins) and the query text.
    Py(usize, String),
    /// A native plugin (index into Engine::natives), its data for query_slow, and the "…" row note.
    Native(usize, Value, Option<String>),
}

/// Plugin results as items, best first.
pub fn py_items(plugin: usize, mut results: Vec<PyResult>) -> Vec<Item> {
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
    /// Native (DLL) plugins, see plugins.rs.
    pub natives: Vec<Arc<Native>>,
    pub shell_history: crate::shell::History,
    pub clip: crate::clip::History,
    games: Vec<steam::Game>,
    games_at: Option<Instant>,
}

/// What the UI should do next besides showing `items`.
pub struct Outcome {
    pub items: Vec<Item>,
    /// Plugin work to run in the background.
    pub slow: Option<Slow>,
}

impl Engine {
    pub fn new(mut cfg: Config, apps: Arc<RwLock<Vec<App>>>, natives: Vec<Arc<Native>>) -> Self {
        // A newly installed plugin: its default settings go into config.json, where they can be edited.
        let mut added = false;
        for n in &natives {
            if !cfg.plugin_settings.contains_key(&n.id) && !n.info.defaults.is_null() {
                cfg.plugin_settings.insert(n.id.clone(), n.info.defaults.clone());
                added = true;
            }
        }
        if added {
            crate::config::save(&cfg);
        }
        Self { cfg, apps, usage: Usage::load(), pins: Pins::load(), hidden: Hidden::load(), natives, shell_history: crate::shell::History::load(), clip: crate::clip::History::load(), games: Vec::new(), games_at: None }
    }

    /// A native plugin's current settings.
    pub fn native_settings(&self, i: usize) -> Value {
        self.natives.get(i).map(|n| n.settings(&self.cfg.plugin_settings).clone()).unwrap_or(Value::Null)
    }

    /// Runs a native plugin's action; saves its settings if it changed them.
    pub fn run_native(&mut self, i: usize, op: Value) -> Result<litelaunch_plugin::After, String> {
        let Some(n) = self.natives.get(i).cloned() else { return Err("плагин не загружен".into()) };
        let (after, settings) = n.run(&self.native_settings(i), op).map_err(|e| format!("{}: {e}", n.info.name))?;
        if let Some(s) = settings {
            self.cfg.plugin_settings.insert(n.id.clone(), s);
            crate::config::save(&self.cfg);
        }
        Ok(after)
    }

    pub fn search(&mut self, query: &str) -> Outcome {
        let mut out = Outcome { items: Vec::new(), slow: None };
        if query.trim().is_empty() {
            return out;
        }
        if let Some(cmd) = crate::shell::strip(&self.cfg.shell_keyword, query) {
            out.items = crate::shell::search(&self.shell_history, cmd);
            return out;
        }
        if let Some((kw, rest)) = query.split_once(' ') {
            if !self.cfg.kill_keyword.is_empty() && kw.eq_ignore_ascii_case(&self.cfg.kill_keyword) {
                out.items = self.kill(rest.trim());
                return out;
            }
            if !self.cfg.clip_keyword.is_empty() && kw.eq_ignore_ascii_case(&self.cfg.clip_keyword) {
                out.items = crate::clip::search(&self.clip, rest.trim());
                return out;
            }
            if !self.cfg.steam_keyword.is_empty() && kw.eq_ignore_ascii_case(&self.cfg.steam_keyword) {
                out.items = self.steam(rest.trim());
                self.apply_pins(query, &mut out.items);
                return out;
            }
            for (i, n) in self.natives.iter().enumerate() {
                let settings = n.settings(&self.cfg.plugin_settings);
                let nkw = n.keyword(settings);
                if nkw.is_empty() || !kw.eq_ignore_ascii_case(&nkw) {
                    continue;
                }
                match n.query(settings, rest.trim()) {
                    Ok(reply) => {
                        out.items = reply.items.into_iter().map(|it| native_item(i, it)).collect();
                        out.slow = reply.slow.map(|data| Slow::Native(i, data, reply.slow_note));
                    }
                    Err(e) => out.items = vec![Item::new(format!("{}: ошибка", n.info.name), e, n.info.icon.clone(), 0, Action::Nothing)],
                }
                return out;
            }
            if let Some(i) = self.cfg.plugins.iter().position(|p| p.keyword.eq_ignore_ascii_case(kw)) {
                out.slow = Some(Slow::Py(i, rest.trim().to_string()));
                return out;
            }
        }
        out.items = self.global(query.trim());
        self.apply_pins(query, &mut out.items);
        out
    }

    /// The actions menu for a result: its own actions, plus removing the plugin on keyword rows.
    pub fn actions(&self, item: &Item) -> Vec<Item> {
        let mut v = Vec::new();
        if let Action::Native { plugin, .. } = &item.action
            && let Some(n) = self.natives.get(*plugin)
        {
            let extra = n.actions(n.settings(&self.cfg.plugin_settings), to_native(item));
            v.extend(extra.into_iter().map(|it| native_item(*plugin, it)));
        }
        v.extend(item.actions());
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
        if let Action::Clip(text) = &item.action {
            let row = |glyph: char, title: &str, action: Action| {
                let mut it = Item::new(title, item.title.clone(), None, 0, action);
                it.glyph = Some(glyph);
                it
            };
            v.push(row('\u{E8C8}', "Скопировать", Action::Copy(text.clone())));
            v.push(row('\u{E74D}', "Удалить из истории", Action::ClipForget(text.clone())));
            v.push(row('\u{E894}', "Очистить всю историю буфера", Action::ClipClear));
        }
        if let (Action::Shell(cmd), "Из истории") = (&item.action, item.sub.as_str()) {
            let mut it = Item::new("Удалить из истории", cmd.clone(), None, 0, Action::ShellForget(cmd.clone()));
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
        if cfg.clip_keyword.eq_ignore_ascii_case(kw) {
            cfg.clip_keyword.clear();
        }
        if cfg.shell_keyword.eq_ignore_ascii_case(kw) {
            cfg.shell_keyword.clear();
        }
        for n in &self.natives {
            let mut settings = n.settings(&cfg.plugin_settings).clone();
            if n.keyword(&settings).eq_ignore_ascii_case(kw)
                && let Some(obj) = settings.as_object_mut()
            {
                obj.insert("keyword".into(), Value::String(String::new()));
                cfg.plugin_settings.insert(n.id.clone(), settings);
            }
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
            // Steam games are found only through the Steam keyword.
            if app.is_steam_game() || self.hidden.contains(&app.target()) || self.blacklisted(app) {
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
        hints.push((self.cfg.clip_keyword.clone(), "История буфера обмена".into(), None));
        hints.push((self.cfg.shell_keyword.clone(), "Shell (PowerShell)".into(), None));
        for n in &self.natives {
            hints.push((n.keyword(n.settings(&self.cfg.plugin_settings)), n.info.name.clone(), n.info.icon.clone()));
        }
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
        // The plugin manager: "плагины", "plugins", "расширения".
        let manager = ["Плагины", "Plugins", "Расширения"].iter().filter_map(|w| fuzzy::score(w, q)).max();
        if let Some(sc) = manager.filter(|&s| s >= 450) {
            let exe = std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned());
            items.push(Item::new("Плагины", "Установить, обновить, удалить, включить и выключить", exe, sc as i64, Action::OpenManager));
        }
        // Updates of LiteLaunch itself: "обновить", "update".
        let update = ["Обновить LiteLaunch", "Обновление", "Update LiteLaunch"].iter().filter_map(|w| fuzzy::score(w, q)).max();
        if let Some(sc) = update.filter(|&s| s >= 450) {
            let exe = std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned());
            let item = match crate::update::AVAILABLE.lock().unwrap().clone() {
                Some(r) => Item::new(
                    format!("Обновить LiteLaunch до {}", r.version),
                    format!("Сейчас {} · Enter скачает и установит, плагины и настройки сохранятся", crate::update::CURRENT),
                    exe,
                    sc as i64 + 100,
                    Action::UpdateLauncher,
                ),
                None => Item::new("Проверить обновления LiteLaunch", format!("Установлена версия {}", crate::update::CURRENT), exe, sc as i64, Action::CheckUpdates),
            };
            items.push(item);
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
