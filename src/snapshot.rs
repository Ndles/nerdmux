//! Snapshots of the sessions, kept on disk so they can come back after a
//! crash or a reboot.
//!
//! The server rewrites the snapshot of its sessions while it runs and
//! deletes it when it ends cleanly. One left behind at start-up means the
//! last server did not end cleanly: it becomes the restorable snapshot,
//! which is offered until it is restored or replaced by the next one.
//!
//! What is saved: session and window names, the split layout, which pane
//! and window had the focus, each pane's directory, the program running in
//! it and the last lines of its text. The sessions' environment is not
//! saved, since it may hold secrets; restored sessions take the environment
//! of the client that restores them.

use crate::status;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::Term;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Bumped when the format changes in a way older servers cannot read.
pub const VERSION: u32 = 1;

/// Lines of text kept per pane, screen included.
pub const TEXT_LINES: usize = 1000;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub version: u32,
    /// When it was written, in seconds since the Unix epoch.
    pub saved: u64,
    #[serde(default)]
    pub sessions: Vec<SavedSession>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SavedSession {
    pub name: String,
    /// Index of the window that had the focus.
    pub active: usize,
    pub windows: Vec<SavedWindow>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SavedWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Index, in layout order, of the pane that had the focus.
    pub active: usize,
    #[serde(default)]
    pub zoomed: bool,
    pub layout: SavedNode,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SavedNode {
    Pane(SavedPane),
    Split {
        /// `"right"`: `a` left of `b`; `"down"`: `a` above `b`.
        split: String,
        ratio: f32,
        a: Box<SavedNode>,
        b: Box<SavedNode>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct SavedPane {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The program in the foreground, with its arguments; empty when the
    /// shell itself was.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// The pane's last lines, with their colors as escape sequences.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

impl SavedNode {
    pub fn panes(&self) -> Vec<&SavedPane> {
        match self {
            SavedNode::Pane(p) => vec![p],
            SavedNode::Split { a, b, .. } => {
                let mut v = a.panes();
                v.extend(b.panes());
                v
            }
        }
    }
}

impl SavedSession {
    /// "2 windows, 3 panes", for the restore picker.
    pub fn summary(&self) -> String {
        let panes: usize = self.windows.iter().map(|w| w.layout.panes().len()).sum();
        let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        format!("{}, {}", plural(self.windows.len(), "window"), plural(panes, "pane"))
    }

    /// Directory of the pane that had the focus.
    pub fn place(&self) -> Option<&str> {
        let w = self.windows.get(self.active).or(self.windows.first())?;
        let panes = w.layout.panes();
        panes.get(w.active).or(panes.first())?.cwd.as_deref()
    }
}

// ===== pane text ============================================================

/// The last `TEXT_LINES` lines of the pane's normal screen, up to but not
/// including the cursor's line (the live prompt, which the new shell draws
/// again). Wrapped rows are joined, so the text reflows at a new width.
pub fn pane_text<T>(term: &Term<T>) -> String {
    let grid = term.grid();
    let cursor = grid.cursor.point.line.0;
    let top = (cursor - TEXT_LINES as i32).max(grid.topmost_line().0);
    let mut out = String::new();
    let mut style: Option<String> = None;
    let mut lines = Vec::new();
    for y in top..cursor {
        let row = &grid[Line(y)];
        let wrapped = row[Column(grid.columns() - 1)].flags.contains(Flags::WRAPLINE);
        // Trailing blanks in the default style are not worth keeping.
        let end = (0..grid.columns())
            .rev()
            .find(|x| {
                let c = &row[Column(*x)];
                c.c != ' ' || status::sgr(c) != "0"
            })
            .map_or(0, |x| x + 1);
        let mut line = String::new();
        let width = if wrapped { grid.columns() } else { end };
        for x in 0..width {
            let cell: &Cell = &row[Column(x)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let sgr = status::sgr(cell);
            if style.as_deref() != Some(sgr.as_str()) {
                line.push_str(&format!("\x1b[{sgr}m"));
                style = Some(sgr);
            }
            line.push(if cell.c.is_control() { ' ' } else { cell.c });
            if let Some(extra) = cell.zerowidth() {
                line.extend(extra.iter().filter(|c| !c.is_control()));
            }
        }
        lines.push((line, wrapped));
    }
    // Leading and trailing empty lines carry nothing.
    while lines.last().is_some_and(|(l, w)| !w && plain(l).is_empty()) {
        lines.pop();
    }
    let first = lines.iter().position(|(l, _)| !plain(l).is_empty()).unwrap_or(lines.len());
    for (line, wrapped) in &lines[first..] {
        out.push_str(line);
        if !wrapped {
            out.push_str("\r\n");
        }
    }
    if style.is_some() {
        out.push_str("\x1b[0m");
    }
    out
}

/// `line` without its escape sequences.
fn plain(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out.trim_end().to_string()
}

// ===== programs =============================================================

/// Programs that are safe to start again: viewers, editors and monitors
/// that change nothing by starting.
const SAFE: &[&str] = &[
    "vim",
    "nvim",
    "vi",
    "view",
    "nano",
    "micro",
    "hx",
    "helix",
    "kak",
    "emacs",
    "less",
    "more",
    "most",
    "man",
    "top",
    "htop",
    "btop",
    "atop",
    "nvtop",
    "glances",
    "lazygit",
    "lazydocker",
    "tig",
    "gitui",
    "yazi",
    "ranger",
    "nnn",
    "lf",
    "mc",
    "k9s",
    "tail",
    "journalctl",
];

/// Whether `command` may run again on restore, given the user's extra
/// programs. Only the program's name counts, never its path.
pub fn safe_to_rerun(command: &[String], extra: &[String]) -> bool {
    let Some(program) = command.first() else { return false };
    let name = program.rsplit('/').next().unwrap_or(program);
    SAFE.contains(&name) || extra.iter().any(|e| e == name)
}

/// `args` as a command line a POSIX shell reads back as the same words.
pub fn shell_words(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            let safe = !a.is_empty()
                && a.chars().all(|c| c.is_ascii_alphanumeric() || "_-./=:@%+,~".contains(c))
                && !a.starts_with('~');
            if safe {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ===== files ================================================================

/// Where snapshots live: `$XDG_STATE_HOME/nerdmux`, by default
/// `~/.local/state/nerdmux`.
pub fn dir() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").filter(|v| !v.is_empty()).map(|h| PathBuf::from(h).join(".local/state"))
    })?;
    Some(state.join("nerdmux"))
}

/// The snapshot files of the server at `socket`: the live one and the
/// restorable one. Named after the whole socket path, so servers on
/// different sockets keep their own.
pub fn paths(socket: &str) -> Option<(PathBuf, PathBuf)> {
    let key: String = socket
        .trim_start_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    let dir = dir()?;
    Some((dir.join(format!("{key}.toml")), dir.join(format!("{key}.restorable.toml"))))
}

/// Write `snap` to `path` so that the file is either the old one or the new
/// one, even across a power loss. Only the user can read it.
pub fn save(path: &Path, snap: &Snapshot) -> std::io::Result<()> {
    let text = toml_edit::ser::to_string_pretty(snap).map_err(std::io::Error::other)?;
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no directory"))?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Read the snapshot at `path`. A file that cannot be read as one is moved
/// aside (to `*.bad`) so it is not tried again.
pub fn load(path: &Path) -> Option<Snapshot> {
    let text = fs::read_to_string(path).ok()?;
    match toml_edit::de::from_str::<Snapshot>(&text) {
        Ok(s) if s.version <= VERSION => Some(s),
        Ok(s) => {
            eprintln!("nerdmux: {} is from a newer version ({}); left alone", path.display(), s.version);
            None
        }
        Err(e) => {
            eprintln!("nerdmux: cannot read {}: {}", path.display(), e.message());
            let _ = fs::rename(path, path.with_extension("bad"));
            None
        }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    struct Size(usize, usize);
    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }
        fn screen_lines(&self) -> usize {
            self.1
        }
        fn columns(&self) -> usize {
            self.0
        }
    }

    fn term(cols: usize, rows: usize, output: &str) -> Term<VoidListener> {
        let mut t = Term::new(Config::default(), &Size(cols, rows), VoidListener);
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(&mut t, output.as_bytes());
        t
    }

    #[test]
    fn text_keeps_colors_joins_wrapped_rows_and_leaves_out_the_prompt() {
        let t = term(10, 6, "\r\n\x1b[31mred\x1b[0m plain\r\n0123456789abc\r\n$ ");
        assert_eq!(pane_text(&t), "\x1b[0;31mred\x1b[0m plain\r\n0123456789abc\r\n\x1b[0m");
        // Replaying it at another width gives the same text, reflowed.
        let again = term(5, 6, &pane_text(&t));
        assert_eq!(again.grid()[Line(2)][Column(4)].c, '4');
        assert_eq!(pane_text(&term(10, 4, "$ ")), "");
    }

    #[test]
    fn only_known_programs_run_again() {
        let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert!(safe_to_rerun(&words("/usr/bin/nvim app.py"), &[]));
        assert!(safe_to_rerun(&words("btop"), &[]));
        assert!(!safe_to_rerun(&words("ssh prod"), &[]));
        assert!(!safe_to_rerun(&words("python3 app.py"), &[]));
        assert!(safe_to_rerun(&words("k8s-tool x"), &["k8s-tool".into()]));
        assert!(!safe_to_rerun(&[], &[]));
    }

    #[test]
    fn quotes_what_the_shell_would_split() {
        let args: Vec<String> = ["nvim", "my file.txt", "it's", "a/b-c.rs", "$HOME", ""].map(String::from).to_vec();
        assert_eq!(shell_words(&args), r"nvim 'my file.txt' 'it'\''s' a/b-c.rs '$HOME' ''");
    }

    #[test]
    fn snapshots_round_trip_through_the_file() {
        let pane = |cwd: &str, cmd: &[&str]| {
            SavedNode::Pane(SavedPane {
                cwd: Some(cwd.into()),
                command: cmd.iter().map(|s| s.to_string()).collect(),
                text: "\x1b[0;31mred\x1b[0m\r\nplain \"quoted\"\r\n".into(),
            })
        };
        let snap = Snapshot {
            version: VERSION,
            saved: 1_760_000_000,
            sessions: vec![SavedSession {
                name: "web".into(),
                active: 1,
                windows: vec![
                    SavedWindow { name: Some("edit".into()), active: 0, zoomed: true, layout: pane("/srv", &["nvim"]) },
                    SavedWindow {
                        name: None,
                        active: 1,
                        zoomed: false,
                        layout: SavedNode::Split {
                            split: "right".into(),
                            ratio: 0.4,
                            a: Box::new(pane("/a", &[])),
                            b: Box::new(pane("/b", &["less", "x y"])),
                        },
                    },
                ],
            }],
        };
        let dir = std::env::temp_dir().join(format!("nerdmux-snap-{}", std::process::id()));
        let path = dir.join("s.toml");
        save(&path, &snap).unwrap();
        assert_eq!(load(&path), Some(snap.clone()));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(snap.sessions[0].summary(), "2 windows, 3 panes");
        assert_eq!(snap.sessions[0].place(), Some("/b"));
        // A damaged file is set aside, not read again.
        fs::write(&path, "sessions = 3").unwrap();
        assert_eq!(load(&path), None);
        assert!(path.with_extension("bad").exists() && !path.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
