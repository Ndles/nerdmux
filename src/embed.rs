//! A terminal for other programs to embed: nerdmux's terminal engine (a
//! program on a pty, its screen and scrollback) and its theme, without the
//! server, sessions or key bindings around them.
//!
//! This module is nerdmux's library API; the rest of the crate is private.
//! It speaks in `crossterm` and `ratatui` types, re-exported here, so a
//! caller can use the same versions without depending on them by number.
//!
//! ```no_run
//! # use nerdmux::embed::*;
//! # fn demo(buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect) -> anyhow::Result<()> {
//! let (tx, rx) = std::sync::mpsc::sync_channel::<TerminalEvent>(64);
//! let env: Vec<(String, String)> = std::env::vars().collect();
//! let opts = Options { cols: 80, rows: 24, command: vec!["bash".into()], cwd: None, env: &env };
//! let mut term = Terminal::spawn(1, opts, tx)?;
//! while let Ok(TerminalEvent::Output(_, bytes)) = rx.recv() {
//!     term.feed(&bytes);
//!     term.draw(buf, area, &load_theme(), true);
//! }
//! # Ok(()) }
//! ```

use crate::pane::{Pane, SpawnOpts};
use std::path::PathBuf;
use std::sync::mpsc::SyncSender;
use std::time::Instant;

pub use crate::config::Theme;
pub use crate::pane::{CursorInfo, Notice, PaneEvent as TerminalEvent};
pub use {crossterm, ratatui};

/// How to start a terminal.
pub struct Options<'a> {
    pub cols: u16,
    pub rows: u16,
    /// The program and its arguments.
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// The program's whole environment.
    pub env: &'a [(String, String)],
}

/// A program running on a pty, with the screen it draws.
pub struct Terminal {
    pane: Pane,
}

impl Terminal {
    /// Starts `opts.command`. Its output and its end arrive on `events`,
    /// tagged with `id`, as `TerminalEvent`s for the caller to pass to `feed`.
    pub fn spawn<E: From<TerminalEvent> + Send + 'static>(id: u32, opts: Options, events: SyncSender<E>) -> anyhow::Result<Terminal> {
        let opts = SpawnOpts {
            cols: opts.cols,
            rows: opts.rows,
            command: opts.command,
            cwd: opts.cwd,
            env: opts.env,
            // Every host has terminfo for this one.
            term: "xterm-256color",
            scrollback: 10_000,
            socket: None,
        };
        Ok(Terminal { pane: Pane::spawn(id, opts, events)? })
    }

    /// Takes the program's output. What it asks of the world outside the
    /// screen (a bell, a clipboard write) comes back for the caller to allow
    /// or ignore.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Notice> {
        self.pane.feed(bytes)
    }

    /// Sends a key as the program expects it. Returns false for keys a
    /// terminal program cannot be sent, such as releases.
    pub fn send_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match crate::client::convert_key(key) {
            Some(k) => {
                self.pane.send_key(k.normalized());
                true
            }
            None => false,
        }
    }

    /// Pastes text, bracketed when the program asked for that.
    pub fn paste(&mut self, text: &str) {
        self.pane.send_paste(text);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.pane.resize(cols, rows);
    }

    /// Draws a screen the program has held back too long (a synchronized
    /// update it never finished). Call it now and then; true means redraw.
    pub fn tick(&mut self, now: Instant) -> bool {
        self.pane.flush_stale_sync(now)
    }

    /// The title the program set, as it sent it: clean it before drawing it
    /// if the program is not trusted.
    pub fn title(&self) -> &str {
        &self.pane.title
    }

    /// Where the cursor is, within the terminal, when it is shown.
    pub fn cursor(&self) -> Option<CursorInfo> {
        self.pane.cursor()
    }

    /// Draws the screen into `area`; `active` false fades it, for a program
    /// that has ended or a terminal without the focus.
    pub fn draw(&mut self, buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect, theme: &Theme, active: bool) {
        crate::render::draw_pane(buf, &mut [], area, &mut self.pane, theme, active, !active);
    }
}

/// The theme from the user's nerdmux config, or the default one.
pub fn load_theme() -> Theme {
    crate::config::Config::load().theme
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use std::time::Duration;

    #[test]
    fn a_program_runs_draws_and_ends() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<TerminalEvent>(64);
        let env = vec![("PATH".to_string(), std::env::var("PATH").unwrap_or_default())];
        let command = vec!["printf".into(), "hello from embed".into()];
        let mut term = Terminal::spawn(7, Options { cols: 40, rows: 4, command, cwd: None, env: &env }, tx).unwrap();
        let mut ended = false;
        while !ended {
            match rx.recv_timeout(Duration::from_secs(5)).expect("the program reports back") {
                TerminalEvent::Output(7, bytes) => assert!(term.feed(&bytes).is_empty()),
                TerminalEvent::Eof(7) | TerminalEvent::Exit(7) => ended = true,
                other => panic!("unexpected {other:?}"),
            }
        }
        let area = Rect::new(0, 0, 40, 4);
        let mut buf = Buffer::empty(area);
        term.draw(&mut buf, area, &Theme::default(), false);
        let first: String = (0..40).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(first.trim_end(), "hello from embed");
    }
}
