//! The settings panel: every option with its current value, changed in place
//! and written back to the config file. Edits go through `toml_edit`, so the
//! user's comments and layout survive.

use crate::config::{self, Config, StatusPos, Theme};
use crate::keys::{self, Action, Table};
use crate::proto::{Key, KeyCode, ALT, CTRL, SHIFT, SUPER};
use ratatui::style::Color;
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table as TomlTable};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Bool,
    Choice(&'static [&'static str]),
    Number {
        step: i64,
        min: i64,
        max: i64,
        unit: &'static str,
    },
    Text,
    Color,
    /// The leader key itself: set by pressing it.
    Leader,
}

/// One option of the config file.
pub struct Setting {
    pub section: &'static str,
    /// Dotted path in the file: `mouse`, `theme.accent`.
    pub key: &'static str,
    pub kind: Kind,
    /// What the panel calls it; says what it does without further notes.
    pub label: &'static str,
}

const fn s(section: &'static str, key: &'static str, kind: Kind, label: &'static str) -> Setting {
    Setting { section, key, kind, label }
}

const fn num(step: i64, min: i64, max: i64, unit: &'static str) -> Kind {
    Kind::Number { step, min, max, unit }
}

pub const SETTINGS: &[Setting] = &[
    s("General", "prefix", Kind::Leader, "leader key"),
    s("General", "status", Kind::Choice(&["bottom", "top", "off"]), "status bar position"),
    s("General", "padding", num(1, 0, 8, "columns"), "space beside the panes"),
    s("General", "padding_rows", num(1, 0, 4, "rows"), "space above the panes"),
    s("General", "mouse", Kind::Bool, "mouse support"),
    s("General", "which_key_delay_ms", num(50, 0, 5000, "ms"), "key hints delay"),
    s("General", "repeat_time_ms", num(100, 0, 5000, "ms"), "resize repeat window"),
    s("General", "dim_inactive", Kind::Bool, "fade unfocused panes"),
    s("General", "inherit_cwd", Kind::Bool, "new panes use current directory"),
    s("General", "set_title", Kind::Bool, "set terminal title"),
    s("General", "prompt", Kind::Bool, "nerdmux prompt in new shells"),
    s("General", "prompt_mark", Kind::Text, "prompt mark"),
    s("General", "scrollback", num(1000, 0, 1_000_000, "lines"), "scrollback per new pane"),
    s("General", "extended_keys", Kind::Bool, "extended keys (kitty protocol)"),
    s("General", "restore", Kind::Bool, "offer sessions back after a crash"),
    s("General", "copy_command", Kind::Text, "extra clipboard command"),
    s("General", "shell", Kind::Text, "shell for new panes"),
    s("General", "term", Kind::Text, "TERM for new panes"),
    s("Theme", "theme.own_palette", Kind::Bool, "theme colors inside panes"),
    s("Theme", "theme.bg", Kind::Color, "background"),
    s("Theme", "theme.inactive_bg", Kind::Color, "unfocused pane background"),
    s("Theme", "theme.fg", Kind::Color, "text"),
    s("Theme", "theme.accent", Kind::Color, "accent"),
    s("Theme", "theme.accent_fg", Kind::Color, "text on badges"),
    s("Theme", "theme.cursor", Kind::Color, "cursor"),
    s("Theme", "theme.border", Kind::Color, "pane borders"),
    s("Theme", "theme.status_bg", Kind::Color, "status bar background"),
    s("Theme", "theme.status_fg", Kind::Color, "status bar text"),
    s("Theme", "theme.dim", Kind::Color, "hints"),
    s("Theme", "theme.cyan", Kind::Color, "key names and tab numbers"),
    s("Theme", "theme.magenta", Kind::Color, "headings and selection"),
    s("Theme", "theme.green", Kind::Color, "messages and git branch"),
    s("Theme", "theme.purple", Kind::Color, "zoom badge and pane numbers"),
    s("Theme", "theme.red", Kind::Color, "bells and alerts"),
    s("Theme", "theme.popup_bg", Kind::Color, "popup background"),
    s("Theme", "theme.popup_fg", Kind::Color, "popup text"),
];

/// Where the Theme section reads and writes: the theme every Nërd tool
/// shares, or nerdmux's own `[theme]`, which goes on top of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scope {
    Shared,
    Own,
}

/// The Theme section's heading, saying which file it changes.
pub fn theme_heading(scope: Scope) -> &'static str {
    match scope {
        Scope::Shared => "Theme · all Nërd tools",
        Scope::Own => "Theme · nerdmux only",
    }
}

/// The file a setting is written to in `scope`.
pub fn path_for(key: &str, scope: Scope) -> std::path::PathBuf {
    if key.starts_with("theme.") && scope == Scope::Shared {
        config::shared_theme_path()
    } else {
        config::config_path()
    }
}

/// What a row of the panel stands for.
#[derive(Clone, Debug, PartialEq)]
pub enum RowId {
    Setting(usize),
    Binding(Action),
}

pub struct Row {
    pub id: RowId,
    pub section: &'static str,
    pub label: String,
    pub value: String,
    pub swatch: Option<Color>,
    /// Set in the file the row writes to, rather than left at the default.
    pub custom: bool,
    /// For a shared theme color that nerdmux sets for itself: what it uses.
    pub own: Option<(String, Color)>,
    /// Name in the config file or of the action, found by the filter too.
    pub key: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    Browse,
    /// Typing a new value for the selected row.
    Edit(String),
    /// Waiting for the key to bind to the selected row.
    Capture,
}

pub struct Panel {
    pub query: String,
    pub selected: usize,
    pub scroll: usize,
    pub mode: Mode,
    /// Dotted keys present in the file, and actions with bindings there.
    pub custom_keys: Vec<String>,
    pub custom_actions: Vec<String>,
    /// Which theme the Theme section shows and changes.
    pub scope: Scope,
    /// Dotted `theme.` keys present in the shared theme file, and the
    /// theme it makes on its own.
    pub shared_keys: Vec<String>,
    pub shared_theme: Theme,
}

impl Panel {
    pub fn open() -> Panel {
        let mut p = Panel {
            query: String::new(),
            selected: 0,
            scroll: 0,
            mode: Mode::Browse,
            custom_keys: Vec::new(),
            custom_actions: Vec::new(),
            scope: Scope::Shared,
            shared_keys: Vec::new(),
            shared_theme: Theme::default(),
        };
        p.reread();
        p
    }

    /// Note which settings the file sets, after it changed.
    pub fn reread(&mut self) {
        let shared = load_doc(&config::shared_theme_path()).unwrap_or_default();
        self.shared_keys =
            SETTINGS.iter().filter(|s| get_item(&shared, s.key).is_some()).map(|s| s.key.into()).collect();
        self.shared_theme = config::shared_theme(&mut Vec::new());
        let doc = load_doc(&config::config_path()).unwrap_or_default();
        self.custom_keys = SETTINGS.iter().filter(|s| get_item(&doc, s.key).is_some()).map(|s| s.key.into()).collect();
        self.custom_actions = Vec::new();
        for table in ["direct", "prefix"] {
            let Some(t) = doc.get("keys").and_then(|k| k.get(table)).and_then(Item::as_table_like) else { continue };
            for (key, value) in t.iter() {
                let Some(v) = value.as_str() else { continue };
                if v == "none" {
                    // Unbinding a default counts against the action it unbinds.
                    let defaults =
                        if table == "direct" { keys::default_direct_table() } else { keys::default_prefix_table() };
                    if let Some(a) = keys::parse_key(key).and_then(|k| keys::lookup(&defaults, k).cloned()) {
                        self.custom_actions.push(a.name());
                    }
                } else if let Some(a) = Action::parse(v) {
                    self.custom_actions.push(a.name());
                }
            }
        }
    }

    pub fn rows(&self, config: &Config) -> Vec<Row> {
        let all = all_rows(config, self);
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            return all;
        }
        all.into_iter()
            .filter(|r| {
                let hay = format!("{} {} {} {}", r.section, r.label, r.key, r.value).to_lowercase();
                q.split_whitespace().all(|t| hay.contains(t))
            })
            .collect()
    }

    /// After the query changed: select the first row whose name holds the
    /// query as typed, else one whose name has all its words.
    pub fn select_best(&mut self, config: &Config) {
        let q = self.query.trim().to_lowercase();
        let rows = self.rows(config);
        let names: Vec<String> = rows.iter().map(|r| r.label.to_lowercase()).collect();
        self.selected = names
            .iter()
            .position(|n| n.contains(&q))
            .or_else(|| names.iter().position(|n| q.split_whitespace().all(|t| n.contains(t))))
            .unwrap_or(0);
    }

    pub fn move_by(&mut self, delta: i32, len: usize) {
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
    }
}

fn all_rows(config: &Config, panel: &Panel) -> Vec<Row> {
    let mut rows = Vec::new();
    for (i, set) in SETTINGS.iter().enumerate() {
        let in_config = panel.custom_keys.iter().any(|k| k == set.key);
        let shared = set.key.starts_with("theme.") && panel.scope == Scope::Shared;
        let (value, swatch, custom, own) = if shared {
            let (value, swatch) = current_in(set, config, &panel.shared_theme);
            let own = in_config.then(|| current(set, config)).and_then(|(v, c)| Some((v, c?)));
            (value, swatch, panel.shared_keys.iter().any(|k| k == set.key), own)
        } else {
            let (value, swatch) = current(set, config);
            (value, swatch, in_config, None)
        };
        rows.push(Row {
            id: RowId::Setting(i),
            section: set.section,
            label: set.label.into(),
            value,
            swatch,
            custom,
            own,
            key: set.key.into(),
        });
    }
    let prefix = keys::key_label(config.prefix);
    for action in bindable() {
        let chord = keys_for(&config.direct_table, &action);
        let leader = keys_for(&config.prefix_table, &action);
        let mut parts = Vec::new();
        if !chord.is_empty() {
            parts.push(chord);
        }
        if !leader.is_empty() {
            parts.push(format!("{prefix} {leader}"));
        }
        rows.push(Row {
            section: "Keys",
            label: action.describe(),
            value: if parts.is_empty() { "—".into() } else { parts.join("  ") },
            swatch: None,
            custom: panel.custom_actions.contains(&action.name()),
            own: None,
            key: action.name(),
            id: RowId::Binding(action),
        });
    }
    rows
}

/// Actions offered for rebinding: the palette plus going to a window by number.
fn bindable() -> Vec<Action> {
    let mut out = Action::palette();
    out.extend((1..=9).map(Action::SelectWindow));
    out
}

fn keys_for(table: &Table, action: &Action) -> String {
    table.iter().filter(|(_, a)| a == action).map(|(k, _)| keys::key_label(*k)).collect::<Vec<_>>().join("/")
}

pub fn color_text(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
        Color::Indexed(i) => i.to_string(),
        Color::Reset => "default".into(),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn theme_color(t: &Theme, name: &str) -> Option<Color> {
    Some(match name {
        "bg" => t.bg,
        "inactive_bg" => t.inactive_bg,
        "fg" => t.fg,
        "cursor" => t.cursor,
        "accent" => t.accent,
        "accent_fg" => t.accent_fg,
        "border" => t.border,
        "status_bg" => t.status_bg,
        "status_fg" => t.status_fg,
        "dim" => t.dim,
        "cyan" => t.cyan,
        "magenta" => t.magenta,
        "green" => t.green,
        "purple" => t.purple,
        "red" => t.red,
        "yellow" => t.yellow,
        "popup_bg" => t.popup_bg,
        "popup_fg" => t.popup_fg,
        _ => return None,
    })
}

fn on_off(b: bool) -> String {
    (if b { "on" } else { "off" }).into()
}

/// The value in force and, for colors, the color itself.
pub fn current(set: &Setting, c: &Config) -> (String, Option<Color>) {
    current_in(set, c, &c.theme)
}

/// The value in force, with theme settings read from `theme`.
pub fn current_in(set: &Setting, c: &Config, theme: &Theme) -> (String, Option<Color>) {
    if let Some(name) = set.key.strip_prefix("theme.") {
        if name == "own_palette" {
            return (on_off(theme.own_palette), None);
        }
        let color = theme_color(theme, name);
        return (color.map(color_text).unwrap_or_default(), color);
    }
    let text = match set.key {
        "prefix" => keys::key_label(c.prefix),
        "status" => match c.status {
            StatusPos::Top => "top",
            StatusPos::Bottom => "bottom",
            StatusPos::Off => "off",
        }
        .into(),
        "mouse" => on_off(c.mouse),
        "which_key_delay_ms" => format!("{} ms", c.which_key_delay.as_millis()),
        "repeat_time_ms" => format!("{} ms", c.repeat_time.as_millis()),
        "dim_inactive" => on_off(c.dim_inactive),
        "inherit_cwd" => on_off(c.inherit_cwd),
        "set_title" => on_off(c.set_title),
        "prompt" => on_off(c.prompt),
        "prompt_mark" => c.prompt_mark.clone(),
        "scrollback" => format!("{} lines", c.scrollback),
        "padding" => format!("{} columns", c.padding),
        "padding_rows" => format!("{} rows", c.padding_rows),
        "extended_keys" => on_off(c.extended_keys),
        "restore" => on_off(c.restore),
        "copy_command" => c.copy_command.clone().unwrap_or_default(),
        "shell" => c.shell.clone().unwrap_or_default(),
        "term" => c.term.clone(),
        _ => String::new(),
    };
    (text, None)
}

/// The number behind a `Number` setting, for stepping it.
pub fn number(set: &Setting, c: &Config) -> i64 {
    match set.key {
        "which_key_delay_ms" => c.which_key_delay.as_millis() as i64,
        "repeat_time_ms" => c.repeat_time.as_millis() as i64,
        "scrollback" => c.scrollback as i64,
        "padding" => c.padding as i64,
        "padding_rows" => c.padding_rows as i64,
        _ => 0,
    }
}

// ===== writing the file =====================================================

pub fn load_doc(path: &Path) -> Result<DocumentMut, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse::<DocumentMut>()
            .map_err(|e| format!("{} has a mistake, fix it first: {}", path.display(), e.message())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut doc = DocumentMut::new();
            let header = if path == config::shared_theme_path() {
                "# The theme every Nërd tool shares (nerdmux, nerdfleet, ...), written by nerdmux's\n\
                 # settings panel (Alt+Space ,). A tool's own [theme] goes on top of it.\n\n"
            } else {
                "# nerdmux configuration, written by the settings panel (Alt+Space ,).\n\
                 # `nerdmux config` prints a commented template of every option.\n\n"
            };
            doc.decor_mut().set_prefix(header);
            Ok(doc)
        }
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn save_doc(path: &Path, doc: &DocumentMut) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    // Write next to the file and rename, so a crash never leaves half a config.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn get_item<'a>(doc: &'a DocumentMut, dotted: &str) -> Option<&'a Item> {
    let mut item = doc.as_item();
    for part in dotted.split('.') {
        item = item.get(part)?;
        if item.is_none() {
            return None;
        }
    }
    Some(item)
}

/// The table at `path`, created (as a `[a.b]` header without an empty
/// `[a]` above it) when missing.
fn table_mut<'a>(doc: &'a mut DocumentMut, path: &[&str]) -> Result<&'a mut TomlTable, String> {
    let mut table = doc.as_table_mut();
    for (i, part) in path.iter().enumerate() {
        if !table.contains_key(part) {
            let mut t = TomlTable::new();
            t.set_implicit(i + 1 < path.len());
            table.insert(part, Item::Table(t));
        }
        table = table
            .get_mut(part)
            .and_then(Item::as_table_mut)
            .ok_or_else(|| format!("{} in the config file is not a table", path[..=i].join(".")))?;
    }
    Ok(table)
}

/// Remove `key` from `table` without losing the comment lines above it:
/// they move to the entry that follows. Returns them when nothing follows,
/// for the caller to keep at the end of the file.
fn remove_kept(table: &mut TomlTable, key: &str) -> String {
    let raw = |p: Option<&toml_edit::RawString>| p.and_then(|p| p.as_str()).map(str::to_string);
    let comment = table.key(key).and_then(|k| raw(k.leaf_decor().prefix())).unwrap_or_default();
    let pos = table.iter().position(|(k, _)| k == key);
    table.remove(key);
    if comment.trim().is_empty() {
        return String::new();
    }
    let Some(next) = pos.and_then(|i| table.iter().nth(i).map(|(k, _)| k.to_string())) else { return comment };
    if let Some(Item::Table(t)) = table.get_mut(&next) {
        let old = raw(t.decor().prefix()).unwrap_or_else(|| "\n".into());
        t.decor_mut().set_prefix(format!("{comment}{old}"));
    } else if let Some(mut k) = table.key_mut(&next) {
        let old = raw(k.leaf_decor().prefix()).unwrap_or_default();
        k.leaf_decor_mut().set_prefix(format!("{comment}{old}"));
    } else {
        return comment;
    }
    String::new()
}

/// Set `key` in `table`. An existing entry keeps its comments and its place.
fn set_kept(table: &mut TomlTable, key: &str, value: Item) {
    match table.get_mut(key) {
        Some(item) => {
            // Keep the spacing around the old value too: `a = 1  # note`.
            if let (Some(old), Item::Value(mut new)) = (item.as_value().map(|v| v.decor().clone()), value.clone()) {
                *new.decor_mut() = old;
                *item = Item::Value(new);
            } else {
                *item = value;
            }
        }
        None => {
            table.insert(key, value);
        }
    }
}

/// Keep comments that lost their entry at the end of the file.
fn keep_at_end(doc: &mut DocumentMut, comment: String) {
    if !comment.is_empty() {
        let old = doc.trailing().as_str().unwrap_or("").to_string();
        doc.set_trailing(format!("{old}\n{comment}"));
    }
}

/// Set `dotted` to `value`, or remove it with `None`, in the file it
/// belongs to in `scope`, and save that file.
pub fn write(dotted: &str, value: Option<Item>, scope: Scope) -> Result<(), String> {
    write_to(&path_for(dotted, scope), dotted, value)
}

fn write_to(path: &Path, dotted: &str, value: Option<Item>) -> Result<(), String> {
    let mut doc = load_doc(path)?;
    let parts: Vec<&str> = dotted.split('.').collect();
    let (last, parents) = parts.split_last().ok_or("empty setting name")?;
    match value {
        Some(v) => {
            set_kept(table_mut(&mut doc, parents)?, last, v);
        }
        None => {
            if get_item(&doc, dotted).is_some() {
                let left = remove_kept(table_mut(&mut doc, parents)?, last);
                keep_at_end(&mut doc, left);
            }
        }
    }
    save_doc(path, &doc)
}

/// Bind `key` to `action`, replacing the action's other keys in the same
/// table (a chord with Alt or Super goes to the chords, anything else after
/// the leader). Returns what the key did before, if anything.
pub fn bind(config: &Config, action: &Action, key: Key) -> Result<(bool, Option<Action>), String> {
    let chord = key.mods & (ALT | SUPER) != 0;
    let (name, table, defaults) = if chord {
        ("direct", &config.direct_table, keys::default_direct_table())
    } else {
        ("prefix", &config.prefix_table, keys::default_prefix_table())
    };
    let before = keys::lookup(table, key).cloned().filter(|a| a != action);
    let path = config::config_path();
    let mut doc = load_doc(&path)?;
    let t = table_mut(&mut doc, &["keys", name])?;
    let mut left = String::new();
    // Drop the action's current keys in this table: entries the file added
    // are removed, built-in ones are switched off.
    for (k, a) in table.iter().filter(|(_, a)| a == action) {
        let spec = key_spec(*k);
        let in_file = t.iter().find(|(fk, _)| keys::parse_key(fk) == Some(*k)).map(|(fk, _)| fk.to_string());
        if let Some(fk) = in_file {
            left.push_str(&remove_kept(t, &fk));
        }
        if keys::lookup(&defaults, *k) == Some(a) {
            set_kept(t, &spec, toml_edit::value("none"));
        }
    }
    // Any other spelling of the new key goes, then the new binding.
    let stale: Vec<String> =
        t.iter().filter(|(fk, _)| keys::parse_key(fk) == Some(key)).map(|(fk, _)| fk.to_string()).collect();
    for fk in stale {
        left.push_str(&remove_kept(t, &fk));
    }
    set_kept(t, &key_spec(key), toml_edit::value(action.name()));
    keep_at_end(&mut doc, left);
    save_doc(&path, &doc)?;
    Ok((chord, before))
}

/// Remove every binding the file makes for `action`, so its built-in keys
/// apply again.
pub fn reset_binding(action: &Action) -> Result<(), String> {
    let path = config::config_path();
    let mut doc = load_doc(&path)?;
    let mut left = String::new();
    for (name, defaults) in [("direct", keys::default_direct_table()), ("prefix", keys::default_prefix_table())] {
        let Some(t) = doc.get_mut("keys").and_then(|k| k.get_mut(name)).and_then(Item::as_table_mut) else {
            continue;
        };
        let drop: Vec<String> = t
            .iter()
            .filter(|(k, v)| match v.as_str() {
                Some("none") => keys::parse_key(k).and_then(|k| keys::lookup(&defaults, k)) == Some(action),
                Some(v) => Action::parse(v).as_ref() == Some(action),
                None => false,
            })
            .map(|(k, _)| k.to_string())
            .collect();
        for k in drop {
            left.push_str(&remove_kept(t, &k));
        }
    }
    keep_at_end(&mut doc, left);
    save_doc(&path, &doc)
}

/// A key written so that `keys::parse_key` reads it back: `ctrl+alt+left`,
/// `alt+shift+t`, `space`.
pub fn key_spec(key: Key) -> String {
    let mut out = String::new();
    for (bit, name) in [(SUPER, "super+"), (CTRL, "ctrl+"), (ALT, "alt+"), (SHIFT, "shift+")] {
        if key.mods & bit != 0 {
            out.push_str(name);
        }
    }
    match key.code {
        KeyCode::Char(c) if c.is_ascii_uppercase() => {
            if key.mods & SHIFT == 0 {
                out.push_str("shift+");
            }
            out.push(c.to_ascii_lowercase());
        }
        KeyCode::Char(' ') => out.push_str("space"),
        KeyCode::Char('+') => out.push_str("plus"),
        KeyCode::Char(c) => out.push(c),
        KeyCode::Enter => out.push_str("enter"),
        KeyCode::Tab => out.push_str("tab"),
        KeyCode::BackTab => out.push_str("backtab"),
        KeyCode::Backspace => out.push_str("backspace"),
        KeyCode::Esc => out.push_str("esc"),
        KeyCode::Left => out.push_str("left"),
        KeyCode::Right => out.push_str("right"),
        KeyCode::Up => out.push_str("up"),
        KeyCode::Down => out.push_str("down"),
        KeyCode::Home => out.push_str("home"),
        KeyCode::End => out.push_str("end"),
        KeyCode::PageUp => out.push_str("pageup"),
        KeyCode::PageDown => out.push_str("pagedown"),
        KeyCode::Insert => out.push_str("insert"),
        KeyCode::Delete => out.push_str("delete"),
        KeyCode::F(n) => out.push_str(&format!("f{n}")),
    }
    out
}

/// Turn typed text into the value for `set`, or say what is wrong with it.
pub fn parse_input(set: &Setting, text: &str) -> Result<Item, String> {
    let text = text.trim();
    match set.kind {
        Kind::Number { min, max, unit, .. } => {
            let digits = text.trim_end_matches(unit).trim();
            let n: i64 = digits.parse().map_err(|_| format!("{text:?} is not a number"))?;
            if !(min..=max).contains(&n) {
                return Err(format!("pick a number from {min} to {max}"));
            }
            Ok(toml_edit::value(n))
        }
        Kind::Color => {
            let v = match text.parse::<i64>() {
                Ok(n) => config::ThemeValue::Int(n),
                Err(_) => config::ThemeValue::Text(text.into()),
            };
            config::parse_color(&v).ok_or_else(|| format!("{text:?} is not a color; try #rrggbb"))?;
            Ok(match v {
                config::ThemeValue::Int(n) => toml_edit::value(n),
                _ => toml_edit::value(text),
            })
        }
        _ => Ok(toml_edit::value(text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_specs_read_back() {
        let keys = [
            Key::alt(' '),
            Key::ctrl('a'),
            Key::alt('T'),
            Key::ch('|'),
            Key::ch('+'),
            Key::ch('S'),
            Key::new(KeyCode::Left, CTRL | ALT),
            Key::new(KeyCode::Up, ALT | SHIFT),
            Key::new(KeyCode::F(5), 0),
            Key::new(KeyCode::Char('t'), SUPER),
            Key::plain(KeyCode::Tab),
            Key::plain(KeyCode::Enter),
        ];
        for k in keys {
            let k = k.normalized();
            assert_eq!(keys::parse_key(&key_spec(k)), Some(k), "{}", key_spec(k));
        }
    }

    #[test]
    fn every_setting_has_a_value_and_parses_back() {
        let c = Config::default();
        for set in SETTINGS {
            let (value, swatch) = current(set, &c);
            if set.kind == Kind::Color {
                assert!(swatch.is_some(), "{} has no color", set.key);
                parse_input(set, &value).unwrap();
            }
            if !matches!(set.kind, Kind::Text) {
                assert!(!value.is_empty(), "{} shows nothing", set.key);
            }
        }
        // The file accepts every key the panel writes.
        let mut text = String::new();
        let mut theme = String::new();
        for set in SETTINGS {
            let (value, _) = current(set, &c);
            let item = match set.kind {
                Kind::Bool => (value == "on").to_string(),
                Kind::Number { .. } => number(set, &c).to_string(),
                Kind::Leader => format!("{:?}", key_spec(c.prefix)),
                _ => format!("{value:?}"),
            };
            match set.key.strip_prefix("theme.") {
                Some(name) => theme.push_str(&format!("{name} = {item}\n")),
                None => text.push_str(&format!("{} = {item}\n", set.key)),
            }
        }
        let parsed = Config::from_toml(&format!("{text}[theme]\n{theme}"));
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert_eq!(parsed.theme, c.theme);
        assert_eq!(parsed.prefix, c.prefix);
    }

    #[test]
    fn removing_a_line_keeps_the_comment_above_it() {
        let mut doc: DocumentMut = "# about mouse\nmouse = true\nstatus = \"top\"\n".parse().unwrap();
        let left = remove_kept(doc.as_table_mut(), "mouse");
        assert!(left.is_empty());
        assert_eq!(doc.to_string(), "# about mouse\nstatus = \"top\"\n");
        set_kept(doc.as_table_mut(), "status", toml_edit::value("off"));
        assert_eq!(doc.to_string(), "# about mouse\nstatus = \"off\"\n");

        let mut doc: DocumentMut = "# the last one\nmouse = true\n".parse().unwrap();
        let left = remove_kept(doc.as_table_mut(), "mouse");
        keep_at_end(&mut doc, left);
        assert!(doc.to_string().contains("# the last one"), "{doc}");
    }

    #[test]
    fn bad_input_is_refused() {
        let delay = SETTINGS.iter().find(|s| s.key == "which_key_delay_ms").unwrap();
        assert!(parse_input(delay, "150").is_ok());
        assert!(parse_input(delay, "150 ms").is_ok());
        assert!(parse_input(delay, "soon").is_err());
        assert!(parse_input(delay, "-5").is_err());
        let accent = SETTINGS.iter().find(|s| s.key == "theme.accent").unwrap();
        assert!(parse_input(accent, "#ff8800").is_ok());
        assert!(parse_input(accent, "208").is_ok());
        assert!(parse_input(accent, "orange-ish").is_err());
    }
}
