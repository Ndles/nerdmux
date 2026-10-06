//! User configuration: `~/.config/nerdmux/config.toml`.

use crate::keys::{self, Action, Table};
use crate::proto::Key;
use ratatui::style::Color;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusPos {
    Top,
    Bottom,
    Off,
}

/// Colors. The default look is a warm charcoal screen with amber as the
/// one strong color and muted, earthy tones for everything else.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    /// Paint panes with this theme's background, text color and 16-color
    /// palette. When false, panes keep the colors of the terminal nerdmux
    /// runs in and only nerdmux's own chrome is themed.
    pub own_palette: bool,
    pub bg: Color,
    /// Background of panes that do not have the focus.
    pub inactive_bg: Color,
    pub fg: Color,
    pub cursor: Color,
    /// ANSI colors 0-15 as programs in panes see them.
    pub palette: [Color; 16],
    /// The main color: focus, the active tab, frames and titles.
    pub accent: Color,
    /// Text drawn on top of `accent` or another badge color.
    pub accent_fg: Color,
    pub border: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    pub dim: Color,
    /// Key names, tab numbers.
    pub cyan: Color,
    /// Headings, the prefix badge, selected text.
    pub magenta: Color,
    /// Messages, the copy badge, search matches.
    pub green: Color,
    /// The zoom badge, pane numbers.
    pub purple: Color,
    /// Bells and the "typing in all panes" badge.
    pub red: Color,
    pub yellow: Color,
    pub popup_bg: Color,
    pub popup_fg: Color,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            own_palette: true,
            bg: rgb(0x15120F),
            inactive_bg: rgb(0x0E0C0A),
            fg: rgb(0xE6C58F),
            cursor: rgb(0xE8A23A),
            palette: [
                rgb(0x2A241E), // black
                rgb(0xD0705E), // red
                rgb(0xA3B57A), // green
                rgb(0xE3B062), // yellow
                rgb(0x7E9CB8), // blue
                rgb(0xB48CA8), // magenta
                rgb(0x7FB4A8), // cyan
                rgb(0xD9CCB4), // white
                rgb(0x8A7D6C), // bright black
                rgb(0xE08A78),
                rgb(0xB9C990),
                rgb(0xEDC67F),
                rgb(0x9AB4CC),
                rgb(0xC9A3BE),
                rgb(0x9CCBBE),
                rgb(0xF2E8D5),
            ],
            accent: rgb(0xE8A23A),
            accent_fg: rgb(0x15120F),
            border: rgb(0x3A322A),
            status_bg: rgb(0x1D1915),
            status_fg: rgb(0xB59A72),
            dim: rgb(0x7A6E60),
            cyan: rgb(0x7FB4A8),
            magenta: rgb(0xC98A8F),
            green: rgb(0xA3B57A),
            purple: rgb(0xA894B8),
            red: rgb(0xD0705E),
            yellow: rgb(0xE3C26E),
            popup_bg: rgb(0x1B1814),
            popup_fg: rgb(0xE6C58F),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub prefix: Key,
    pub prefix_table: Table,
    pub direct_table: Table,
    pub shell: Option<String>,
    pub shell_args: Vec<String>,
    pub term: String,
    pub scrollback: usize,
    pub mouse: bool,
    pub status: StatusPos,
    pub which_key_delay: Duration,
    pub repeat_time: Duration,
    pub copy_command: Option<String>,
    pub inherit_cwd: bool,
    pub dim_inactive: bool,
    pub set_title: bool,
    /// Ask the terminal for the kitty keyboard protocol (Super, Ctrl+Shift chords).
    pub extended_keys: bool,
    pub theme: Theme,
    /// Problems found while loading; shown to the user on attach.
    pub warnings: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            prefix: Key::alt(' '),
            prefix_table: keys::default_prefix_table(),
            direct_table: keys::default_direct_table(),
            shell: None,
            shell_args: Vec::new(),
            term: "xterm-256color".into(),
            scrollback: 10_000,
            mouse: true,
            status: StatusPos::Bottom,
            which_key_delay: Duration::from_millis(400),
            repeat_time: Duration::from_millis(700),
            copy_command: None,
            inherit_cwd: true,
            dim_inactive: false,
            set_title: true,
            extended_keys: true,
            theme: Theme::default(),
            warnings: Vec::new(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Raw {
    prefix: Option<String>,
    shell: Option<String>,
    shell_args: Option<Vec<String>>,
    term: Option<String>,
    scrollback: Option<usize>,
    mouse: Option<bool>,
    status: Option<String>,
    alt_bindings: Option<bool>,
    which_key_delay_ms: Option<u64>,
    repeat_time_ms: Option<u64>,
    copy_command: Option<String>,
    inherit_cwd: Option<bool>,
    dim_inactive: Option<bool>,
    set_title: Option<bool>,
    extended_keys: Option<bool>,
    theme: BTreeMap<String, ThemeValue>,
    keys: RawKeys,
}

/// An entry of `[theme]`: a color (`"#rrggbb"`, a name or a palette index),
/// a switch, or the list of 16 palette colors.
#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum ThemeValue {
    Bool(bool),
    Int(i64),
    Text(String),
    List(Vec<ThemeValue>),
    /// Anything else (a float, a table): reported as a bad color.
    Other(serde::de::IgnoredAny),
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawKeys {
    prefix: BTreeMap<String, String>,
    direct: BTreeMap<String, String>,
}

pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("NERDMUX_CONFIG") {
        return PathBuf::from(p);
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
    };
    base.join("nerdmux").join("config.toml")
}

pub fn parse_color(v: &ThemeValue) -> Option<Color> {
    match v {
        ThemeValue::Int(n) if (0..=255).contains(n) => Some(Color::Indexed(*n as u8)),
        ThemeValue::Text(s) => {
            let s = s.trim();
            if let Some(hex) = s.strip_prefix('#') {
                if hex.len() == 6 && hex.is_ascii() {
                    let p = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
                    return Some(Color::Rgb(p(0)?, p(2)?, p(4)?));
                }
                return None;
            }
            Some(match s.to_ascii_lowercase().as_str() {
                "default" | "reset" => Color::Reset,
                "black" => Color::Indexed(0),
                "red" => Color::Indexed(1),
                "green" => Color::Indexed(2),
                "yellow" => Color::Indexed(3),
                "blue" => Color::Indexed(4),
                "magenta" => Color::Indexed(5),
                "cyan" => Color::Indexed(6),
                "white" => Color::Indexed(7),
                "gray" | "grey" => Color::Indexed(8),
                other => Color::Indexed(other.parse::<u8>().ok()?),
            })
        }
        _ => None,
    }
}

impl Config {
    /// Load the config file. Never fails: problems become `warnings` and the
    /// affected settings keep their defaults.
    pub fn load() -> Config {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => Config::from_toml(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => {
                let mut c = Config::default();
                c.warnings.push(format!("cannot read {}: {e}", path.display()));
                c
            }
        }
    }

    pub fn from_toml(text: &str) -> Config {
        let mut c = Config::default();
        let raw: Raw = match toml_edit::de::from_str(text) {
            Ok(r) => r,
            Err(e) => {
                let first = e.message().lines().next().unwrap_or("syntax error").to_string();
                c.warnings.push(format!("config ignored: {first}"));
                return c;
            }
        };
        if let Some(p) = raw.prefix {
            match keys::parse_key(&p) {
                Some(k) => c.prefix = k,
                None => c.warnings.push(format!("config: bad prefix key {p:?}")),
            }
        }
        if raw.alt_bindings == Some(false) {
            c.direct_table.clear();
        }
        c.shell = raw.shell.filter(|s| !s.is_empty());
        if let Some(a) = raw.shell_args {
            c.shell_args = a;
        }
        if let Some(t) = raw.term.filter(|s| !s.is_empty()) {
            c.term = t;
        }
        if let Some(n) = raw.scrollback {
            c.scrollback = n.min(1_000_000);
        }
        if let Some(m) = raw.mouse {
            c.mouse = m;
        }
        if let Some(s) = raw.status {
            match s.to_ascii_lowercase().as_str() {
                "top" => c.status = StatusPos::Top,
                "bottom" => c.status = StatusPos::Bottom,
                "off" | "none" | "hidden" => c.status = StatusPos::Off,
                _ => c.warnings.push(format!("config: status must be top, bottom or off, not {s:?}")),
            }
        }
        if let Some(ms) = raw.which_key_delay_ms {
            c.which_key_delay = Duration::from_millis(ms);
        }
        if let Some(ms) = raw.repeat_time_ms {
            c.repeat_time = Duration::from_millis(ms);
        }
        c.copy_command = raw.copy_command.filter(|s| !s.is_empty());
        if let Some(b) = raw.inherit_cwd {
            c.inherit_cwd = b;
        }
        if let Some(b) = raw.dim_inactive {
            c.dim_inactive = b;
        }
        if let Some(b) = raw.set_title {
            c.set_title = b;
        }
        if let Some(b) = raw.extended_keys {
            c.extended_keys = b;
        }
        for (name, value) in &raw.theme {
            let t = &mut c.theme;
            match (name.as_str(), value) {
                ("own_palette", ThemeValue::Bool(b)) => {
                    t.own_palette = *b;
                    continue;
                }
                ("palette", ThemeValue::List(items)) => {
                    let colors: Vec<Color> = items.iter().filter_map(parse_color).collect();
                    match <[Color; 16]>::try_from(colors) {
                        Ok(p) => t.palette = p,
                        Err(_) => c.warnings.push("config: theme.palette needs exactly 16 colors".into()),
                    }
                    continue;
                }
                _ => {}
            }
            let Some(color) = parse_color(value) else {
                c.warnings.push(format!("config: bad color for theme.{name}"));
                continue;
            };
            match name.as_str() {
                "bg" | "background" => t.bg = color,
                "inactive_bg" => t.inactive_bg = color,
                "fg" | "foreground" => t.fg = color,
                "cursor" => t.cursor = color,
                "accent" => t.accent = color,
                "accent_fg" => t.accent_fg = color,
                "border" => t.border = color,
                "status_bg" => t.status_bg = color,
                "status_fg" => t.status_fg = color,
                "dim" => t.dim = color,
                "cyan" => t.cyan = color,
                "magenta" => t.magenta = color,
                "green" => t.green = color,
                "purple" => t.purple = color,
                "red" => t.red = color,
                "yellow" => t.yellow = color,
                "popup_bg" => t.popup_bg = color,
                "popup_fg" => t.popup_fg = color,
                _ => c.warnings.push(format!("config: unknown theme color {name:?}")),
            }
        }
        let mut warnings = Vec::new();
        let mut apply = |table: &mut Table, entries: &BTreeMap<String, String>, which: &str| {
            for (key, action) in entries {
                let Some(k) = keys::parse_key(key) else {
                    warnings.push(format!("config: bad key {key:?} in keys.{which}"));
                    continue;
                };
                let Some(a) = Action::parse(action) else {
                    warnings.push(format!("config: unknown action {action:?} for {key:?}"));
                    continue;
                };
                keys::bind(table, k, a);
            }
        };
        apply(&mut c.prefix_table, &raw.keys.prefix, "prefix");
        apply(&mut c.direct_table, &raw.keys.direct, "direct");
        c.warnings.extend(warnings);
        c
    }
}

pub const EXAMPLE: &str = r##"# nerdmux configuration. Every setting is optional; the values shown are the defaults.

prefix = "alt+space"         # the leader: press it, then a key (Alt+Space h lists them)
# shell = "/bin/zsh"         # default: $SHELL
# shell_args = ["-l"]        # extra arguments for the shell
scrollback = 10000           # lines of history per pane (a full one takes roughly 25 MB)
mouse = true                 # click to focus, drag borders, wheel to scroll, drag to copy
status = "bottom"            # "top", "bottom" or "off"
alt_bindings = true          # set to false to drop the built-in chords (Alt+arrows, Alt+1-9, ...)
extended_keys = true         # ask the terminal for Super and Ctrl+Shift chords (kitty protocol)
which_key_delay_ms = 400     # how long after the prefix the key guide pops up
repeat_time_ms = 700         # window in which resize keys repeat without the prefix
inherit_cwd = true           # new panes start in the directory of the current pane
dim_inactive = false         # fade the text of panes that do not have the focus
set_title = true             # show "session › program" as the terminal's title
# copy_command = "pbcopy"    # extra clipboard command; OSC 52 is always sent
term = "xterm-256color"      # $TERM inside panes

[theme]                      # "#rrggbb", a 0-255 palette index, or a color name
own_palette = true           # false: panes keep your terminal's own colors
bg = "#15120F"               # warm charcoal; the terminal's padding is tinted to match
inactive_bg = "#0E0C0A"      # darker background for panes without the focus
fg = "#E6C58F"               # soft amber text
accent = "#E8A23A"           # amber: focus, active tab, frames
cyan = "#7FB4A8"             # teal: key names, tab numbers
magenta = "#C98A8F"          # dusty rose: headings, prefix badge, selected text
green = "#A3B57A"            # sage: messages, copy badge, search matches
purple = "#A894B8"           # mauve: zoom badge, pane numbers
red = "#D0705E"              # brick: bells, sync badge
# palette = ["#2A241E", "#D0705E", ...]   # all 16 ANSI colors, black to bright white

[keys.direct]                # chords pressed on their own
# "super+left"   = "focus left"      # Super needs a terminal with the kitty protocol
# "ctrl+shift+t" = "new-window"      # so do Ctrl+Shift chords
# "alt+enter"    = "none"            # "none" removes a default binding
# "alt+z"        = "zoom"            # Alt+letter takes that key away from the shell

[keys.prefix]                # pressed after the leader
# "e" = "split-right"
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Direction;
    use crate::proto::{KeyCode, ALT, SUPER};

    #[test]
    fn example_parses_cleanly() {
        let c = Config::from_toml(EXAMPLE);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!(c.prefix, Key::alt(' '));
        assert_eq!(c.prefix_table, keys::default_prefix_table());
        assert_eq!(c.direct_table, keys::default_direct_table());
        // The template documents the real defaults.
        assert_eq!(c.theme, Theme::default());
    }

    #[test]
    fn overrides() {
        let c = Config::from_toml(
            r##"
            prefix = "ctrl+a"
            status = "top"
            extended_keys = false
            [theme]
            accent = "#ff8800"
            border = "red"
            own_palette = false
            [keys.prefix]
            "v" = "split-right"
            "c" = "none"
            [keys.direct]
            "alt+left" = "none"
            "super+left" = "focus left"
            "alt+enter" = "resize left 7"
            "##,
        );
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!(c.prefix, Key::ctrl('a'));
        assert_eq!(c.status, StatusPos::Top);
        assert!(!c.extended_keys);
        assert_eq!(c.theme.accent, Color::Rgb(0xff, 0x88, 0));
        assert_eq!(c.theme.border, Color::Indexed(1));
        assert!(!c.theme.own_palette);
        assert_eq!(keys::lookup(&c.prefix_table, Key::ch('v')), Some(&Action::SplitRight));
        assert_eq!(keys::lookup(&c.prefix_table, Key::ch('c')), None);
        assert_eq!(keys::lookup(&c.direct_table, Key::new(KeyCode::Left, ALT)), None);
        assert_eq!(
            keys::lookup(&c.direct_table, Key::new(KeyCode::Left, SUPER)),
            Some(&Action::Focus(Direction::Left))
        );
        assert_eq!(
            keys::lookup(&c.direct_table, Key::new(KeyCode::Enter, ALT)),
            Some(&Action::Resize(Direction::Left, 7))
        );
    }

    #[test]
    fn custom_palette() {
        let sixteen: Vec<String> = (0..16).map(|i| format!("\"#0000{i:02x}\"")).collect();
        let c = Config::from_toml(&format!("[theme]\npalette = [{}]\n", sixteen.join(", ")));
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!(c.theme.palette[15], Color::Rgb(0, 0, 15));
        let c = Config::from_toml("[theme]\npalette = [1, 2, 3]\n");
        assert_eq!(c.warnings.len(), 1);
        assert_eq!(c.theme.palette, Theme::default().palette);
    }

    #[test]
    fn mistakes_become_warnings() {
        let c = Config::from_toml("prefix = \"ctrl+\"\n[keys.direct]\n\"alt+q\" = \"fly\"\n[theme]\naccent = 999\n");
        assert_eq!(c.warnings.len(), 3, "{:?}", c.warnings);
        assert_eq!(c.prefix, Key::alt(' '));
        let c = Config::from_toml("no_such_setting = 1");
        assert_eq!(c.warnings.len(), 1);
        assert!(c.warnings[0].contains("no_such_setting"), "{:?}", c.warnings);
    }
}
