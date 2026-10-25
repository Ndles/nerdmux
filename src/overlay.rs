//! Popups a client can have open: pickers, prompts, confirmations, help.

use crate::keys::Action;
use crate::layout::PaneId;
use crate::proto::{Key, KeyCode, ALT, CTRL};

pub type SessionId = u32;
pub type WindowId = u32;

pub enum Overlay {
    Help {
        scroll: u16,
    },
    Picker(Picker),
    Prompt(Prompt),
    Confirm(Confirm),
    PaneNumbers,
    /// Arrows resize and move the focused pane until Enter.
    Arrange,
    Settings(Box<crate::settings::Panel>),
    Restore(RestorePicker),
}

/// The sessions saved before a crash or reboot, to tick and bring back.
#[derive(Clone, Debug)]
pub struct RestorePicker {
    /// When the snapshot was written, in seconds since the Unix epoch.
    pub saved: u64,
    pub items: Vec<RestoreItem>,
    pub selected: usize,
}

#[derive(Clone, Debug)]
pub struct RestoreItem {
    /// Index of the session in the restorable snapshot.
    pub index: usize,
    pub name: String,
    /// "2 windows, 3 panes"
    pub summary: String,
    /// Directory of its focused pane.
    pub place: String,
    pub checked: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Session(SessionId),
    Window(SessionId, WindowId),
    Pane(SessionId, WindowId, PaneId),
    Action(Action),
}

pub struct PickItem {
    pub label: String,
    /// Dimmed text after the label (path, key hint, ...).
    pub detail: String,
    pub indent: u16,
    /// Marks where the client currently is.
    pub current: bool,
    pub target: Target,
}

pub struct Picker {
    pub title: String,
    pub query: String,
    pub items: Vec<PickItem>,
    /// Indices into `items` that match the query, best first.
    pub shown: Vec<usize>,
    pub selected: usize,
    pub scroll: usize,
    /// The session manager: letters are commands (new, rename, kill) and
    /// filtering starts with `/`.
    pub manage: bool,
    /// The manager is taking a filter query.
    pub filtering: bool,
}

impl Picker {
    pub fn new(title: &str, items: Vec<PickItem>) -> Picker {
        let selected = items.iter().position(|i| i.current).unwrap_or(0);
        let shown = (0..items.len()).collect();
        Picker {
            title: title.to_string(),
            query: String::new(),
            items,
            shown,
            selected,
            scroll: 0,
            manage: false,
            filtering: false,
        }
    }

    pub fn refilter(&mut self) {
        if self.query.trim().is_empty() {
            self.shown = (0..self.items.len()).collect();
        } else {
            let mut scored: Vec<(i32, usize)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| item_score(&self.query, item).map(|s| (s, i)))
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.shown = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_by(&mut self, delta: i32) {
        if self.shown.is_empty() {
            return;
        }
        let n = self.shown.len() as i32;
        self.selected = (self.selected as i32 + delta).rem_euclid(n) as usize;
    }

    pub fn current(&self) -> Option<&PickItem> {
        self.shown.get(self.selected).map(|i| &self.items[*i])
    }

    /// Keep the selected row inside a list of `height` visible rows.
    pub fn clamp_scroll(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PromptKind {
    RenameWindow,
    RenameSession(SessionId),
    NewSession,
    Search { pane: PaneId, forward: bool },
}

pub struct Prompt {
    pub title: String,
    pub text: String,
    pub kind: PromptKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kill {
    Pane(PaneId),
    Window(SessionId, WindowId),
    Session(SessionId),
}

pub struct Confirm {
    pub text: String,
    pub then: Kill,
}

/// Apply a key to a single-line text field. Returns true if the key was an edit.
pub fn edit_text(text: &mut String, key: Key) -> bool {
    match (key.code, key.mods) {
        (KeyCode::Char(c), 0) => text.push(c),
        (KeyCode::Backspace, 0) => {
            text.pop();
        }
        (KeyCode::Char('u'), CTRL) => text.clear(),
        (KeyCode::Char('w'), CTRL) | (KeyCode::Backspace, ALT) | (KeyCode::Backspace, CTRL) => {
            let trimmed = text.trim_end().len();
            text.truncate(trimmed);
            let cut = text.rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
            text.truncate(cut);
        }
        _ => return false,
    }
    true
}

/// Labels match fuzzily; the detail column (paths, key names) only matches
/// literally, or a long path would match almost any query by accident.
fn item_score(query: &str, item: &PickItem) -> Option<i32> {
    let detail = item.detail.to_lowercase();
    let mut total = 0;
    for term in query.to_lowercase().split_whitespace() {
        match fuzzy_score(term, &item.label) {
            Some(s) => total += s,
            None if detail.contains(term) => total += 5,
            None => return None,
        }
    }
    Some(total)
}

/// Score `text` against a query of space-separated terms, each of which must
/// appear as a subsequence. Higher is better; `None` means no match.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let mut total = 0;
    for term in query.to_lowercase().split_whitespace() {
        total += term_score(&term.chars().collect::<Vec<_>>(), &text)?;
    }
    Some(total)
}

fn term_score(term: &[char], text: &[char]) -> Option<i32> {
    // Try every starting position of the first character and keep the best
    // greedy match; cheap for the short strings a picker holds.
    let first = *term.first()?;
    let mut best: Option<i32> = None;
    for start in (0..text.len()).filter(|i| text[*i] == first) {
        let mut score = 0;
        let mut pos = start;
        let mut prev: Option<usize> = None;
        let mut ok = true;
        for (n, c) in term.iter().enumerate() {
            let found = if n == 0 { Some(start) } else { (pos..text.len()).find(|i| text[*i] == *c) };
            let Some(i) = found else {
                ok = false;
                break;
            };
            score += 10;
            if prev.is_some_and(|p| p + 1 == i) {
                score += 15;
            } else if let Some(p) = prev {
                score -= ((i - p) as i32).min(8);
            }
            if i == 0 || !text[i - 1].is_alphanumeric() {
                score += 12;
            }
            prev = Some(i);
            pos = i + 1;
        }
        if ok {
            score -= (start as i32).min(10) / 2;
            best = Some(best.map_or(score, |b: i32| b.max(score)));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matching() {
        assert!(fuzzy_score("spr", "split-right").is_some());
        assert!(fuzzy_score("xyz", "split-right").is_none());
        assert!(fuzzy_score("", "anything").is_some());
        // Contiguous and word-start matches rank higher.
        let exact = fuzzy_score("split", "split right").unwrap();
        let spread = fuzzy_score("split", "swap pane left in tab").unwrap();
        assert!(exact > spread, "{exact} vs {spread}");
        let word = fuzzy_score("new", "new window").unwrap();
        let inner = fuzzy_score("new", "renew lease").unwrap();
        assert!(word > inner);
        // All terms must match, in any order.
        assert!(fuzzy_score("win new", "new window").is_some());
        assert!(fuzzy_score("win zzz", "new window").is_none());
        assert!(fuzzy_score("NEW", "new window").is_some());
    }

    #[test]
    fn picker_filtering() {
        let item = |label: &str| PickItem {
            label: label.to_string(),
            detail: String::new(),
            indent: 0,
            current: label == "beta",
            target: Target::Session(0),
        };
        let mut p = Picker::new("t", vec![item("alpha"), item("beta"), item("gamma")]);
        assert_eq!(p.current().unwrap().label, "beta");
        p.move_by(1);
        assert_eq!(p.current().unwrap().label, "gamma");
        p.move_by(1);
        assert_eq!(p.current().unwrap().label, "alpha");
        p.query = "mm".into();
        p.refilter();
        assert_eq!(p.shown.len(), 1);
        assert_eq!(p.current().unwrap().label, "gamma");
        p.query = "zzz".into();
        p.refilter();
        assert!(p.current().is_none());
        p.move_by(1);

        // Details match as plain substrings only, and rank below labels.
        let with = |label: &str, detail: &str| PickItem {
            label: label.to_string(),
            detail: detail.to_string(),
            indent: 0,
            current: false,
            target: Target::Session(0),
        };
        let mut p = Picker::new(
            "t",
            vec![with("1: bash", "/srv/top/secret"), with("2: top", "/home"), with("3: vim", "/t/o/p")],
        );
        p.query = "top".into();
        p.refilter();
        let labels: Vec<&str> = p.shown.iter().map(|i| p.items[*i].label.as_str()).collect();
        assert_eq!(labels, ["2: top", "1: bash"]);
    }

    #[test]
    fn text_editing() {
        let mut s = String::from("hello big world");
        assert!(edit_text(&mut s, Key::ctrl('w')));
        assert_eq!(s, "hello big ");
        assert!(edit_text(&mut s, Key::ctrl('w')));
        assert_eq!(s, "hello ");
        assert!(edit_text(&mut s, Key::ch('x')));
        assert!(edit_text(&mut s, Key::plain(KeyCode::Backspace)));
        assert_eq!(s, "hello ");
        assert!(edit_text(&mut s, Key::ctrl('u')));
        assert_eq!(s, "");
        assert!(!edit_text(&mut s, Key::plain(KeyCode::Up)));
    }
}
