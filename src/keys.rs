//! Actions, key specs and the default binding tables.

use crate::proto::{Key, KeyCode, ALT, CTRL, SHIFT, SUPER};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn name(self) -> &'static str {
        match self {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "left" | "l" => Direction::Left,
            "right" | "r" => Direction::Right,
            "up" | "u" => Direction::Up,
            "down" | "d" => Direction::Down,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    // Windows
    NewWindow,
    NextWindow,
    PrevWindow,
    LastWindow,
    SelectWindow(usize),
    KillWindow,
    RenameWindow,
    MoveWindowLeft,
    MoveWindowRight,
    // Panes
    SplitRight,
    SplitDown,
    SplitAuto,
    KillPane,
    Zoom,
    Focus(Direction),
    NextPane,
    PrevPane,
    LastPane,
    SwapNext,
    SwapPrev,
    /// Trade places with the pane in that direction.
    Swap(Direction),
    Resize(Direction, u16),
    /// Arrange mode: arrows resize, Ctrl+arrows move the pane, Enter ends.
    Arrange,
    NextLayout,
    BreakPane,
    DisplayPanes,
    ToggleSync,
    ClearHistory,
    // Sessions
    NewSession,
    NextSession,
    PrevSession,
    LastSession,
    RenameSession,
    KillSession,
    Detach,
    Switcher,
    /// List of sessions with keys to switch, create, rename and kill.
    Sessions,
    /// Offer the sessions saved before a crash or reboot.
    RestoreSessions,
    // Other
    CopyMode,
    CopyModePageUp,
    Paste,
    Palette,
    Help,
    SendPrefix,
    ReloadConfig,
    /// The settings panel.
    Settings,
    None,
}

impl Action {
    pub fn name(&self) -> String {
        match self {
            Action::NewWindow => "new-window".into(),
            Action::NextWindow => "next-window".into(),
            Action::PrevWindow => "previous-window".into(),
            Action::LastWindow => "last-window".into(),
            Action::SelectWindow(n) => format!("select-window {n}"),
            Action::KillWindow => "kill-window".into(),
            Action::RenameWindow => "rename-window".into(),
            Action::MoveWindowLeft => "move-window-left".into(),
            Action::MoveWindowRight => "move-window-right".into(),
            Action::SplitRight => "split-right".into(),
            Action::SplitDown => "split-down".into(),
            Action::SplitAuto => "split-auto".into(),
            Action::KillPane => "kill-pane".into(),
            Action::Zoom => "zoom".into(),
            Action::Focus(d) => format!("focus {}", d.name()),
            Action::NextPane => "next-pane".into(),
            Action::PrevPane => "previous-pane".into(),
            Action::LastPane => "last-pane".into(),
            Action::SwapNext => "swap-next".into(),
            Action::SwapPrev => "swap-previous".into(),
            Action::Swap(d) => format!("swap {}", d.name()),
            Action::Resize(d, n) => format!("resize {} {n}", d.name()),
            Action::Arrange => "arrange".into(),
            Action::NextLayout => "next-layout".into(),
            Action::BreakPane => "break-pane".into(),
            Action::DisplayPanes => "display-panes".into(),
            Action::ToggleSync => "toggle-sync".into(),
            Action::ClearHistory => "clear-history".into(),
            Action::NewSession => "new-session".into(),
            Action::NextSession => "next-session".into(),
            Action::PrevSession => "previous-session".into(),
            Action::LastSession => "last-session".into(),
            Action::RenameSession => "rename-session".into(),
            Action::KillSession => "kill-session".into(),
            Action::Detach => "detach".into(),
            Action::Switcher => "switcher".into(),
            Action::Sessions => "sessions".into(),
            Action::RestoreSessions => "restore-sessions".into(),
            Action::CopyMode => "copy-mode".into(),
            Action::CopyModePageUp => "copy-mode-page-up".into(),
            Action::Paste => "paste".into(),
            Action::Palette => "command-palette".into(),
            Action::Help => "help".into(),
            Action::SendPrefix => "send-prefix".into(),
            Action::ReloadConfig => "reload-config".into(),
            Action::Settings => "settings".into(),
            Action::None => "none".into(),
        }
    }

    pub fn parse(s: &str) -> Option<Action> {
        let mut parts = s.split_whitespace();
        let head = parts.next()?.to_ascii_lowercase();
        let arg1 = parts.next().map(|s| s.to_ascii_lowercase());
        let arg2 = parts.next();
        Some(match head.as_str() {
            "new-window" => Action::NewWindow,
            "next-window" => Action::NextWindow,
            "previous-window" | "prev-window" => Action::PrevWindow,
            "last-window" => Action::LastWindow,
            "select-window" => Action::SelectWindow(arg1?.parse().ok()?),
            "kill-window" | "close-window" => Action::KillWindow,
            "rename-window" => Action::RenameWindow,
            "move-window-left" => Action::MoveWindowLeft,
            "move-window-right" => Action::MoveWindowRight,
            "split-right" | "split-horizontal" => Action::SplitRight,
            "split-down" | "split-vertical" => Action::SplitDown,
            "split-auto" | "new-pane" => Action::SplitAuto,
            "kill-pane" | "close-pane" => Action::KillPane,
            "zoom" | "toggle-zoom" => Action::Zoom,
            "focus" => Action::Focus(Direction::parse(&arg1?)?),
            "focus-left" => Action::Focus(Direction::Left),
            "focus-right" => Action::Focus(Direction::Right),
            "focus-up" => Action::Focus(Direction::Up),
            "focus-down" => Action::Focus(Direction::Down),
            "next-pane" => Action::NextPane,
            "previous-pane" | "prev-pane" => Action::PrevPane,
            "last-pane" => Action::LastPane,
            "swap-next" => Action::SwapNext,
            "swap-previous" | "swap-prev" => Action::SwapPrev,
            "swap" => Action::Swap(Direction::parse(&arg1?)?),
            "arrange" | "resize-mode" => Action::Arrange,
            "resize" => {
                let dir = Direction::parse(&arg1?)?;
                let n = match arg2 {
                    Some(n) => n.parse().ok()?,
                    None => 1,
                };
                Action::Resize(dir, n)
            }
            "next-layout" => Action::NextLayout,
            "break-pane" => Action::BreakPane,
            "display-panes" => Action::DisplayPanes,
            "toggle-sync" | "sync" => Action::ToggleSync,
            "clear-history" => Action::ClearHistory,
            "new-session" => Action::NewSession,
            "next-session" => Action::NextSession,
            "previous-session" | "prev-session" => Action::PrevSession,
            "last-session" => Action::LastSession,
            "rename-session" => Action::RenameSession,
            "kill-session" => Action::KillSession,
            "detach" => Action::Detach,
            "switcher" | "choose-tree" | "find" => Action::Switcher,
            "sessions" | "session-manager" => Action::Sessions,
            "restore-sessions" | "restore" => Action::RestoreSessions,
            "copy-mode" => Action::CopyMode,
            "copy-mode-page-up" => Action::CopyModePageUp,
            "paste" => Action::Paste,
            "command-palette" | "palette" => Action::Palette,
            "help" => Action::Help,
            "send-prefix" => Action::SendPrefix,
            "reload-config" => Action::ReloadConfig,
            "settings" | "preferences" => Action::Settings,
            "none" | "unbind" => Action::None,
            _ => return None,
        })
    }

    pub fn describe(&self) -> String {
        match self {
            Action::NewWindow => "new window".into(),
            Action::NextWindow => "next window".into(),
            Action::PrevWindow => "previous window".into(),
            Action::LastWindow => "last used window".into(),
            Action::SelectWindow(n) => format!("go to window {n}"),
            Action::KillWindow => "close window".into(),
            Action::RenameWindow => "rename window".into(),
            Action::MoveWindowLeft => "move window left".into(),
            Action::MoveWindowRight => "move window right".into(),
            Action::SplitRight => "split right".into(),
            Action::SplitDown => "split down".into(),
            Action::SplitAuto => "new pane".into(),
            Action::KillPane => "close pane".into(),
            Action::Zoom => "zoom pane (toggle)".into(),
            Action::Focus(d) => format!("focus pane {}", d.name()),
            Action::NextPane => "next pane".into(),
            Action::PrevPane => "previous pane".into(),
            Action::LastPane => "last used pane".into(),
            Action::SwapNext => "swap with next pane".into(),
            Action::SwapPrev => "swap with previous pane".into(),
            Action::Swap(d) => format!("move pane {}", d.name()),
            Action::Resize(d, n) => format!("resize {} by {n}", d.name()),
            Action::Arrange => "resize mode (arrows, Enter ends)".into(),
            Action::NextLayout => "cycle layout".into(),
            Action::BreakPane => "move pane to new window".into(),
            Action::DisplayPanes => "go to pane by number".into(),
            Action::ToggleSync => "type in all panes".into(),
            Action::ClearHistory => "clear scrollback".into(),
            Action::NewSession => "new session".into(),
            Action::NextSession => "next session".into(),
            Action::PrevSession => "previous session".into(),
            Action::LastSession => "last used session".into(),
            Action::RenameSession => "rename session".into(),
            Action::KillSession => "kill session".into(),
            Action::Detach => "detach".into(),
            Action::Switcher => "find a session, window or pane".into(),
            Action::Sessions => "manage sessions".into(),
            Action::RestoreSessions => "restore saved sessions".into(),
            Action::CopyMode => "copy / scroll mode".into(),
            Action::CopyModePageUp => "scroll back one page".into(),
            Action::Paste => "paste".into(),
            Action::Palette => "command palette".into(),
            Action::Help => "show all keys".into(),
            Action::SendPrefix => "send the leader key to the pane".into(),
            Action::ReloadConfig => "reload config".into(),
            Action::Settings => "settings".into(),
            Action::None => "nothing".into(),
        }
    }

    pub fn category(&self) -> &'static str {
        match self {
            Action::NewWindow
            | Action::NextWindow
            | Action::PrevWindow
            | Action::LastWindow
            | Action::SelectWindow(_)
            | Action::KillWindow
            | Action::RenameWindow
            | Action::MoveWindowLeft
            | Action::MoveWindowRight => "Windows",
            Action::SplitRight
            | Action::SplitDown
            | Action::SplitAuto
            | Action::KillPane
            | Action::Zoom
            | Action::Focus(_)
            | Action::NextPane
            | Action::PrevPane
            | Action::LastPane
            | Action::SwapNext
            | Action::SwapPrev
            | Action::Swap(_)
            | Action::Arrange
            | Action::Resize(..)
            | Action::NextLayout
            | Action::BreakPane
            | Action::DisplayPanes
            | Action::ToggleSync
            | Action::ClearHistory => "Panes",
            Action::NewSession
            | Action::NextSession
            | Action::PrevSession
            | Action::LastSession
            | Action::RenameSession
            | Action::KillSession
            | Action::Detach
            | Action::Switcher
            | Action::Sessions
            | Action::RestoreSessions => "Sessions",
            Action::CopyMode | Action::CopyModePageUp | Action::Paste => "Copy & paste",
            _ => "General",
        }
    }

    /// Repeatable actions keep the prefix armed for a moment, so
    /// `prefix H H H` resizes three times.
    pub fn repeatable(&self) -> bool {
        matches!(
            self,
            Action::Resize(..) | Action::NextWindow | Action::PrevWindow | Action::NextPane | Action::PrevPane
        )
    }

    /// Everything offered in the command palette.
    pub fn palette() -> Vec<Action> {
        use Action::*;
        use Direction::*;
        vec![
            SplitAuto,
            SplitRight,
            SplitDown,
            KillPane,
            Zoom,
            Focus(Left),
            Focus(Down),
            Focus(Up),
            Focus(Right),
            NextPane,
            PrevPane,
            LastPane,
            Arrange,
            Swap(Left),
            Swap(Down),
            Swap(Up),
            Swap(Right),
            SwapNext,
            SwapPrev,
            Resize(Left, 5),
            Resize(Down, 5),
            Resize(Up, 5),
            Resize(Right, 5),
            NextLayout,
            BreakPane,
            DisplayPanes,
            ToggleSync,
            ClearHistory,
            NewWindow,
            NextWindow,
            PrevWindow,
            LastWindow,
            RenameWindow,
            KillWindow,
            MoveWindowLeft,
            MoveWindowRight,
            Sessions,
            Switcher,
            NewSession,
            NextSession,
            PrevSession,
            LastSession,
            RenameSession,
            KillSession,
            Detach,
            RestoreSessions,
            CopyMode,
            Paste,
            Help,
            Settings,
            ReloadConfig,
        ]
    }
}

pub fn parse_key(spec: &str) -> Option<Key> {
    const MODS: &[(&str, u8)] = &[
        ("ctrl+", CTRL),
        ("ctrl-", CTRL),
        ("control+", CTRL),
        ("c-", CTRL),
        ("alt+", ALT),
        ("alt-", ALT),
        ("meta+", ALT),
        ("opt+", ALT),
        ("option+", ALT),
        ("m-", ALT),
        ("shift+", SHIFT),
        ("shift-", SHIFT),
        ("s-", SHIFT),
        ("super+", SUPER),
        ("super-", SUPER),
        ("cmd+", SUPER),
        ("command+", SUPER),
        ("win+", SUPER),
    ];
    let mut rest = spec.trim();
    let mut mods = 0u8;
    'outer: loop {
        let lower = rest.to_ascii_lowercase();
        for (prefix, m) in MODS {
            if lower.starts_with(prefix) && rest.len() > prefix.len() {
                mods |= m;
                rest = &rest[prefix.len()..];
                continue 'outer;
            }
        }
        break;
    }
    let mut chars = rest.chars();
    let first = chars.next()?;
    let code = if chars.next().is_none() {
        // "Ctrl+B" means the b key; only an explicit shift asks for more.
        let ctrl_only = mods & CTRL != 0 && mods & SHIFT == 0;
        KeyCode::Char(if ctrl_only { first.to_ascii_lowercase() } else { first })
    } else {
        match rest.to_ascii_lowercase().as_str() {
            "enter" | "return" | "cr" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "backtab" | "btab" => KeyCode::BackTab,
            "backspace" | "bs" | "bspace" => KeyCode::Backspace,
            "esc" | "escape" => KeyCode::Esc,
            "space" => KeyCode::Char(' '),
            "plus" => KeyCode::Char('+'),
            "minus" => KeyCode::Char('-'),
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" | "ppage" => KeyCode::PageUp,
            "pagedown" | "pgdn" | "npage" => KeyCode::PageDown,
            "insert" | "ins" => KeyCode::Insert,
            "delete" | "del" => KeyCode::Delete,
            name => {
                let n: u8 = name.strip_prefix('f')?.parse().ok()?;
                if !(1..=12).contains(&n) {
                    return None;
                }
                KeyCode::F(n)
            }
        }
    };
    Some(Key::new(code, mods).normalized())
}

/// How a key is shown to the user: `Ctrl+Alt+←`, `Alt+Shift+w`, `Super+1`.
pub fn key_label(key: Key) -> String {
    let mut out = String::new();
    if key.mods & SUPER != 0 {
        out.push_str("Super+");
    }
    if key.mods & CTRL != 0 {
        out.push_str("Ctrl+");
    }
    if key.mods & ALT != 0 {
        out.push_str("Alt+");
    }
    // A capital letter is the letter key held with Shift; say so.
    let shifted_letter = matches!(key.code, KeyCode::Char(c) if c.is_ascii_uppercase());
    if key.mods & SHIFT != 0 || shifted_letter {
        out.push_str("Shift+");
    }
    match key.code {
        KeyCode::Char(' ') => out.push_str("Space"),
        KeyCode::Char(c) => out.push(c.to_ascii_lowercase()),
        KeyCode::Enter => out.push_str("Enter"),
        KeyCode::Tab => out.push_str("Tab"),
        KeyCode::BackTab => out.push_str("Shift+Tab"),
        KeyCode::Backspace => out.push_str("Backspace"),
        KeyCode::Esc => out.push_str("Esc"),
        KeyCode::Left => out.push('←'),
        KeyCode::Right => out.push('→'),
        KeyCode::Up => out.push('↑'),
        KeyCode::Down => out.push('↓'),
        KeyCode::Home => out.push_str("Home"),
        KeyCode::End => out.push_str("End"),
        KeyCode::PageUp => out.push_str("PgUp"),
        KeyCode::PageDown => out.push_str("PgDn"),
        KeyCode::Insert => out.push_str("Ins"),
        KeyCode::Delete => out.push_str("Del"),
        KeyCode::F(n) => out.push_str(&format!("F{n}")),
    }
    out
}

pub type Table = Vec<(Key, Action)>;

pub fn lookup(table: &Table, key: Key) -> Option<&Action> {
    table.iter().find(|(k, _)| *k == key).map(|(_, a)| a)
}

/// Bind `key` in `table`, replacing any earlier binding. `Action::None`
/// removes the binding.
pub fn bind(table: &mut Table, key: Key, action: Action) {
    table.retain(|(k, _)| *k != key);
    if action != Action::None {
        table.push((key, action));
    }
}

/// Keys pressed after the leader (the prefix, `Alt+Space` by default). Each
/// is the first letter of what it does where that is possible, so the
/// leader frees the shell's own Alt keys without costing memorability.
pub fn default_prefix_table() -> Table {
    use Action::*;
    use Direction::*;
    let c = Key::ch;
    let p = Key::plain;
    let m = Key::new;
    let mut t: Table = vec![
        // Panes
        (p(KeyCode::Enter), SplitAuto),
        (c('|'), SplitRight),
        (c('\\'), SplitRight),
        (c('-'), SplitDown),
        (c('x'), KillPane),
        (c('z'), Zoom),
        (p(KeyCode::Left), Focus(Left)),
        (p(KeyCode::Down), Focus(Down)),
        (p(KeyCode::Up), Focus(Up)),
        (p(KeyCode::Right), Focus(Right)),
        (m(KeyCode::Left, SHIFT), Swap(Left)),
        (m(KeyCode::Down, SHIFT), Swap(Down)),
        (m(KeyCode::Up, SHIFT), Swap(Up)),
        (m(KeyCode::Right, SHIFT), Swap(Right)),
        (m(KeyCode::Left, CTRL), Resize(Left, 5)),
        (m(KeyCode::Down, CTRL), Resize(Down, 3)),
        (m(KeyCode::Up, CTRL), Resize(Up, 3)),
        (m(KeyCode::Right, CTRL), Resize(Right, 5)),
        (c('r'), Arrange),
        (c('l'), NextLayout),
        (c('g'), DisplayPanes),
        (c('b'), BreakPane),
        (c('a'), ToggleSync),
        (c('k'), ClearHistory),
        // Windows
        (c('t'), NewWindow),
        (c('w'), KillWindow),
        (c('n'), RenameWindow),
        (p(KeyCode::Tab), LastWindow),
        (c('<'), MoveWindowLeft),
        (c('>'), MoveWindowRight),
        // Sessions
        (c('s'), Sessions),
        (c('f'), Switcher),
        (c('S'), NewSession),
        (c('N'), RenameSession),
        (c('d'), Detach),
        // Copy & paste
        (c('c'), CopyMode),
        (p(KeyCode::PageUp), CopyModePageUp),
        (c('v'), Paste),
        // General
        (c('p'), Palette),
        (c(','), Settings),
        (c('?'), Help),
        (c('h'), Help),
        (c('R'), ReloadConfig),
        (m(KeyCode::Char(' '), ALT), SendPrefix),
    ];
    for n in 1..=9usize {
        t.push((c(char::from(b'0' + n as u8)), SelectWindow(n)));
    }
    t
}

/// Chords that work on their own. Only arrows, digits and Enter: letters
/// with Alt belong to the shell (word motions, history search, ...).
/// `Alt` moves between panes, `Alt+Shift` one level further out (windows
/// sideways, sessions up and down), `Ctrl+Alt` resizes.
pub fn default_direct_table() -> Table {
    use Action::*;
    use Direction::*;
    let a = Key::alt;
    let m = Key::new;
    let mut t: Table = vec![
        // Panes
        (m(KeyCode::Enter, ALT), SplitAuto),
        (m(KeyCode::Left, ALT), Focus(Left)),
        (m(KeyCode::Down, ALT), Focus(Down)),
        (m(KeyCode::Up, ALT), Focus(Up)),
        (m(KeyCode::Right, ALT), Focus(Right)),
        (m(KeyCode::Left, CTRL | ALT), Resize(Left, 3)),
        (m(KeyCode::Down, CTRL | ALT), Resize(Down, 2)),
        (m(KeyCode::Up, CTRL | ALT), Resize(Up, 2)),
        (m(KeyCode::Right, CTRL | ALT), Resize(Right, 3)),
        // Windows
        (m(KeyCode::Left, ALT | SHIFT), PrevWindow),
        (m(KeyCode::Right, ALT | SHIFT), NextWindow),
        // Sessions
        (m(KeyCode::Up, ALT | SHIFT), PrevSession),
        (m(KeyCode::Down, ALT | SHIFT), NextSession),
        // Copy & paste
        (m(KeyCode::PageUp, ALT), CopyModePageUp),
    ];
    for n in 1..=9usize {
        t.push((a(char::from(b'0' + n as u8)), SelectWindow(n)));
    }
    t
}

/// Sections of the key guide, in the order they are shown.
pub const CATEGORIES: [&str; 5] = ["Panes", "Windows", "Sessions", "Copy & paste", "General"];

/// One line of the key guide.
pub struct Hint {
    pub keys: String,
    pub desc: String,
    pub category: &'static str,
    /// Hints with the same group describe the same thing; used to line up
    /// a chord and its leader key in the help.
    pub group: String,
}

/// One line of the full key list: what a chord does and which key after
/// the leader does the same. Either key may be missing.
pub struct HelpRow {
    pub category: &'static str,
    pub chord: String,
    pub leader: String,
    pub desc: String,
}

/// Merge the two binding tables into one list ordered by what keys do.
pub fn help_rows(direct: &Table, prefix: &Table) -> Vec<HelpRow> {
    let (d, p) = (hints(direct), hints(prefix));
    let mut rows: Vec<HelpRow> = Vec::new();
    for h in p.iter().chain(d.iter().filter(|h| !p.iter().any(|x| x.group == h.group))) {
        let chord = d.iter().find(|x| x.group == h.group).map(|x| x.keys.clone()).unwrap_or_default();
        let leader = p.iter().find(|x| x.group == h.group).map(|x| x.keys.clone()).unwrap_or_default();
        rows.push(HelpRow { category: h.category, chord, leader, desc: h.desc.clone() });
    }
    rows
}

/// Actions that read better as one line: the four arrows, both directions
/// of a pair, the ten digits.
fn hint_group(action: &Action) -> (String, Option<&'static str>) {
    match action {
        Action::Focus(_) => ("focus".into(), Some("focus pane")),
        Action::Resize(_, n) => (format!("resize {n}"), Some("resize pane")),
        Action::Swap(_) => ("swap".into(), Some("move pane")),
        Action::SelectWindow(_) => ("select-window".into(), Some("go to window")),
        Action::PrevWindow | Action::NextWindow => ("cycle-window".into(), Some("previous / next window")),
        Action::PrevSession | Action::NextSession => ("cycle-session".into(), Some("previous / next session")),
        Action::MoveWindowLeft | Action::MoveWindowRight => ("move-window".into(), Some("move window left / right")),
        Action::SwapPrev | Action::SwapNext => ("swap-cycle".into(), Some("swap with previous / next pane")),
        other => (other.name(), None),
    }
}

/// Collapse a binding table into readable hints.
pub fn hints(table: &Table) -> Vec<Hint> {
    let mut out: Vec<Hint> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (_, action) in table {
        let (group, shared) = hint_group(action);
        // Resize steps of different sizes are still "resize pane": merge them.
        let group = if group.starts_with("resize ") { "resize".to_string() } else { group };
        if seen.contains(&group) {
            continue;
        }
        seen.push(group.clone());
        let members: Vec<&(Key, Action)> = table
            .iter()
            .filter(|(_, a)| {
                let g = hint_group(a).0;
                g == group || (group == "resize" && g.starts_with("resize "))
            })
            .collect();
        let keys = if group == "select-window" && members.len() > 2 {
            let first = key_label(members[0].0);
            let last = key_label(members[members.len() - 1].0);
            format!("{first}…{}", last.chars().last().map(String::from).unwrap_or_default())
        } else {
            compact_keys(&members)
        };
        let desc = match shared {
            // A pair with only one half bound keeps that half's own name.
            Some(text) if members.len() > 1 || matches!(action, Action::Focus(_) | Action::Resize(..)) => {
                text.to_string()
            }
            _ => action.describe(),
        };
        out.push(Hint { keys, desc, category: action.category(), group });
    }
    out
}

/// `Alt+← Alt+↓ Alt+↑ Alt+→` → `Alt+←↓↑→`; `( )` → `(/)`.
fn compact_keys(members: &[&(Key, Action)]) -> String {
    let is_arrow = |t: &str| matches!(t, "←" | "→" | "↑" | "↓");
    let mut groups: Vec<(String, bool, Vec<String>)> = Vec::new();
    for (key, _) in members {
        let label = key_label(*key);
        let (prefix, tail) = match label.rfind('+') {
            Some(i) if i + 1 < label.len() => (label[..=i].to_string(), label[i + 1..].to_string()),
            // A label ending in '+' is the plus key itself, e.g. "Alt++".
            Some(i) if i > 0 => (label[..i].to_string(), "+".to_string()),
            _ => (String::new(), label.clone()),
        };
        let arrow = is_arrow(&tail);
        match groups.iter_mut().find(|(p, a, _)| *p == prefix && *a == arrow) {
            Some((_, _, tails)) => tails.push(tail),
            None => groups.push((prefix, arrow, vec![tail])),
        }
    }
    groups
        .into_iter()
        .map(|(prefix, arrow, tails)| format!("{prefix}{}", tails.join(if arrow { "" } else { "/" })))
        .collect::<Vec<_>>()
        .join("  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_specs() {
        assert_eq!(parse_key("ctrl+b"), Some(Key::ctrl('b')));
        assert_eq!(parse_key("C-b"), Some(Key::ctrl('b')));
        assert_eq!(parse_key("Ctrl+B"), Some(Key::ctrl('b')));
        assert_eq!(parse_key("ctrl+shift+b"), Some(Key::ctrl('B')));
        assert_eq!(parse_key("alt+w"), Some(Key::alt('w')));
        assert_eq!(parse_key("alt+shift+w"), Some(Key::alt('W')));
        assert_eq!(parse_key("alt+W"), Some(Key::alt('W')));
        assert_eq!(parse_key("alt+left"), Some(Key::new(KeyCode::Left, ALT)));
        assert_eq!(parse_key("ctrl+alt+left"), Some(Key::new(KeyCode::Left, CTRL | ALT)));
        assert_eq!(parse_key("alt+shift+up"), Some(Key::new(KeyCode::Up, ALT | SHIFT)));
        assert_eq!(parse_key("super+left"), Some(Key::new(KeyCode::Left, SUPER)));
        assert_eq!(parse_key("cmd+shift+t"), Some(Key::new(KeyCode::Char('T'), SUPER)));
        assert_eq!(parse_key("super+1"), Some(Key::new(KeyCode::Char('1'), SUPER)));
        assert_eq!(parse_key("alt+enter"), Some(Key::new(KeyCode::Enter, ALT)));
        assert_eq!(parse_key("alt+-"), Some(Key::alt('-')));
        assert_eq!(parse_key("-"), Some(Key::ch('-')));
        assert_eq!(parse_key("%"), Some(Key::ch('%')));
        assert_eq!(parse_key("space"), Some(Key::ch(' ')));
        assert_eq!(parse_key("f5"), Some(Key::plain(KeyCode::F(5))));
        assert_eq!(parse_key("shift+f3"), Some(Key::new(KeyCode::F(3), SHIFT)));
        assert_eq!(parse_key("ctrl+space"), Some(Key::ctrl(' ')));
        assert_eq!(parse_key("f13"), None);
        assert_eq!(parse_key(""), None);
    }

    #[test]
    fn labels_spell_out_the_chord() {
        assert_eq!(key_label(Key::new(KeyCode::Left, CTRL | ALT)), "Ctrl+Alt+←");
        assert_eq!(key_label(Key::alt('W')), "Alt+Shift+w");
        assert_eq!(key_label(Key::alt('<')), "Alt+<");
        assert_eq!(key_label(Key::new(KeyCode::Char('T'), SUPER)), "Super+Shift+t");
        assert_eq!(key_label(Key::new(KeyCode::Enter, ALT)), "Alt+Enter");
        // What is shown can be typed back into the config file.
        for table in [default_prefix_table(), default_direct_table()] {
            for (key, _) in table {
                let typed =
                    key_label(key).replace('←', "left").replace('→', "right").replace('↑', "up").replace('↓', "down");
                assert_eq!(parse_key(&typed), Some(key), "{typed}");
            }
        }
    }

    #[test]
    fn action_names_round_trip() {
        let mut all = Action::palette();
        all.extend([Action::SelectWindow(3), Action::CopyModePageUp, Action::SendPrefix, Action::None]);
        for action in all {
            assert_eq!(Action::parse(&action.name()), Some(action.clone()), "{}", action.name());
        }
    }

    #[test]
    fn default_tables_have_no_duplicate_keys() {
        for table in [default_prefix_table(), default_direct_table()] {
            for (i, (key, _)) in table.iter().enumerate() {
                assert!(!table[i + 1..].iter().any(|(k, _)| k == key), "duplicate {}", key_label(*key));
            }
        }
    }

    #[test]
    fn default_chords_avoid_letter_navigation() {
        // Moving around is done with arrows, never with h/j/k/l.
        for table in [default_prefix_table(), default_direct_table()] {
            for (key, action) in table {
                let directional = matches!(action, Action::Focus(_) | Action::Resize(..) | Action::Swap(_));
                assert!(
                    !directional || matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down),
                    "{} is bound to {}",
                    key_label(key),
                    action.name()
                );
            }
        }
        // Every direct chord carries a modifier, so plain typing is never taken.
        assert!(default_direct_table().iter().all(|(k, _)| k.mods & (ALT | CTRL | SUPER) != 0));
    }

    #[test]
    fn hints_compact_related_keys() {
        let hints = hints(&default_direct_table());
        let find = |desc: &str| hints.iter().find(|h| h.desc == desc).map(|h| h.keys.as_str());
        assert_eq!(find("focus pane"), Some("Alt+←↓↑→"));
        assert_eq!(find("resize pane"), Some("Ctrl+Alt+←↓↑→"));
        assert_eq!(find("previous / next window"), Some("Alt+Shift+←→"));
        assert_eq!(find("previous / next session"), Some("Alt+Shift+↑↓"));
        assert_eq!(find("go to window"), Some("Alt+1…9"));
        let prefix = super::hints(&default_prefix_table());
        let find = |desc: &str| prefix.iter().find(|h| h.desc == desc).map(|h| h.keys.as_str());
        assert_eq!(find("move pane"), Some("Shift+←↓↑→"));
        assert_eq!(find("move window left / right"), Some("</>"));
        assert_eq!(find("show all keys"), Some("?/h"));
        assert_eq!(find("go to window"), Some("1…9"));
    }

    #[test]
    fn default_chords_leave_the_shells_alt_letters_alone() {
        // Alt+letter is word motion, history search, ... in readline and fish.
        for (key, action) in default_direct_table() {
            if let KeyCode::Char(c) = key.code {
                assert!(!c.is_alphabetic(), "{} is bound to {}", key_label(key), action.name());
            }
        }
    }

    #[test]
    fn help_lines_up_chords_and_leader_keys() {
        let rows = help_rows(&default_direct_table(), &default_prefix_table());
        let row = |desc: &str| rows.iter().find(|r| r.desc == desc).unwrap();
        assert_eq!(row("focus pane").chord, "Alt+←↓↑→");
        assert_eq!(row("focus pane").leader, "←↓↑→");
        assert_eq!(row("new pane").chord, "Alt+Enter");
        assert_eq!(row("new pane").leader, "Enter");
        assert_eq!(row("resize pane").leader, "Ctrl+←↓↑→");
        assert_eq!(row("zoom pane (toggle)").chord, "");
        // Every row is in one of the help's sections.
        for r in &rows {
            assert!(CATEGORIES.contains(&r.category), "{} has no section", r.desc);
        }
    }
}
