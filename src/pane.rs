//! A pane: one pseudo-terminal, the program running in it, and the
//! emulated screen (with scrollback) that program draws on.

use crate::input;
use crate::layout::PaneId;
use crate::procinfo;
use crate::proto::{Key, Mouse};
use crate::status;
use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction as GridDir, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{self, Config as TermConfig, Term, TermMode};
use alacritty_terminal::vi_mode::ViMotion;
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle, Processor};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Collects the events the emulator raises while parsing output.
#[derive(Clone)]
pub struct Listener(Arc<Mutex<Vec<TermEvent>>>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        match event {
            TermEvent::PtyWrite(_)
            | TermEvent::Title(_)
            | TermEvent::ResetTitle
            | TermEvent::Bell
            | TermEvent::ClipboardStore(..)
            | TermEvent::TextAreaSizeRequest(_) => {
                if let Ok(mut q) = self.0.lock() {
                    q.push(event);
                }
            }
            _ => {}
        }
    }
}

struct TermSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// What a pane's threads report: output to feed, and the end of the
/// program. Whoever owns the pane turns these into events of its own
/// through `From`.
#[derive(Debug)]
pub enum PaneEvent {
    Output(PaneId, Vec<u8>),
    Eof(PaneId),
    Exit(PaneId),
}

/// Things the server has to react to after a pane processed output.
#[derive(Debug, PartialEq)]
pub enum Notice {
    Bell,
    Clipboard(String),
}

pub struct CopyMode {
    pub search: Option<RegexSearch>,
    pub query: String,
    pub forward: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorInfo {
    pub col: u16,
    pub row: u16,
    /// Parameter for DECSCUSR (`CSI n SP q`); 0 is the terminal's default.
    pub style: u8,
}

pub struct SpawnOpts<'a> {
    pub cols: u16,
    pub rows: u16,
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: &'a [(String, String)],
    pub term: &'a str,
    pub scrollback: usize,
    /// The nerdmux server's socket, so programs in the pane can find it;
    /// `None` for a pane that does not live in a nerdmux server.
    pub socket: Option<&'a str>,
}

pub struct Pane {
    pub term: Term<Listener>,
    parser: Processor,
    /// Spots the status requests the emulator leaves unanswered.
    status: status::Scanner,
    events: Arc<Mutex<Vec<TermEvent>>>,
    master: Box<dyn MasterPty + Send>,
    writer: mpsc::Sender<Vec<u8>>,
    pub pid: Option<u32>,
    /// Set once the child has been reaped, so its pid is never signalled again.
    reaped: Arc<AtomicBool>,
    /// Title set by the program through OSC 0/2.
    pub title: String,
    /// Process group that set `title`; the title is dropped once another
    /// one takes the foreground.
    title_owner: Option<i32>,
    /// The pane runs a shell, whose own titles are ignored: shells set one
    /// at every prompt ("user@host: dir"), and the program name says more.
    shell: bool,
    /// Name of the foreground program, refreshed periodically.
    pub name: String,
    /// Working directory of the foreground program and the git branch
    /// there, refreshed along with `name`.
    pub place: Option<(PathBuf, Option<String>)>,
    pub cols: u16,
    pub rows: u16,
    pub copy: Option<CopyMode>,
    pub last_focus: u64,
    spawned: Instant,
    /// The program printed something since the snapshot last took its text.
    pub printed: bool,
    /// Someone typed or pasted into the pane.
    pub touched: bool,
}

/// Marker for "the program never chose a cursor shape": DECSCUSR cannot
/// select a hollow block, so seeing one means the default is still active.
const UNSET_CURSOR: CursorShape = CursorShape::HollowBlock;

impl Pane {
    pub fn spawn<E: From<PaneEvent> + Send + 'static>(
        id: PaneId,
        opts: SpawnOpts,
        tx: SyncSender<E>,
    ) -> anyhow::Result<Pane> {
        let cols = opts.cols.max(2);
        let rows = opts.rows.max(1);
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

        let mut cmd = CommandBuilder::new(&opts.command[0]);
        cmd.args(&opts.command[1..]);
        cmd.env_clear();
        for (k, v) in opts.env {
            cmd.env(k, v);
        }
        for k in ["COLUMNS", "LINES", "TERMCAP", "TMUX", "TMUX_PANE", "STY", "WINDOWID", "TERM_SESSION_ID"] {
            cmd.env_remove(k);
        }
        // The outer terminal's identity: left in, shells load its integration
        // (VTE's sets the title and prompt marks) and its tools reach past us.
        for k in OUTER_TERMINAL_VARS {
            cmd.env_remove(k);
        }
        cmd.env("TERM", opts.term);
        cmd.env("TERM_PROGRAM", "nerdmux");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        if let Some(socket) = opts.socket {
            cmd.env("NERDMUX", socket);
            cmd.env("NERDMUX_PANE", id.to_string());
        }
        if let Some(cwd) = opts.cwd.filter(|p| p.is_dir()) {
            cmd.cwd(cwd);
        }

        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let pid = child.process_id();
        let name = base_name(&opts.command[0]);
        let shell = SHELLS.contains(&name.as_str());

        let mut reader = pair.master.try_clone_reader()?;
        let reader_tx = tx.clone();
        thread::Builder::new().name(format!("pane-{id}-read")).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if reader_tx.send(PaneEvent::Output(id, buf[..n].to_vec()).into()).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let _ = reader_tx.send(PaneEvent::Eof(id).into());
        })?;

        let reaped = Arc::new(AtomicBool::new(false));
        let reaped_flag = reaped.clone();
        thread::Builder::new().name(format!("pane-{id}-wait")).spawn(move || {
            let _ = child.wait();
            reaped_flag.store(true, Ordering::SeqCst);
            let _ = tx.send(PaneEvent::Exit(id).into());
        })?;

        // Writes go through a thread so a program that stops reading its
        // input can never stall the whole server.
        let mut pty_writer = pair.master.take_writer()?;
        let (writer, writer_rx) = mpsc::channel::<Vec<u8>>();
        thread::Builder::new().name(format!("pane-{id}-write")).spawn(move || {
            for data in writer_rx {
                if pty_writer.write_all(&data).is_err() {
                    break;
                }
                let _ = pty_writer.flush();
            }
        })?;

        let events = Arc::new(Mutex::new(Vec::new()));
        let config = TermConfig {
            scrolling_history: opts.scrollback,
            default_cursor_style: CursorStyle { shape: UNSET_CURSOR, blinking: false },
            ..TermConfig::default()
        };
        let size = TermSize { cols: cols as usize, rows: rows as usize };
        let term = Term::new(config, &size, Listener(events.clone()));

        Ok(Pane {
            term,
            parser: Processor::new(),
            status: status::Scanner::default(),
            events,
            master: pair.master,
            writer,
            pid,
            reaped,
            title: String::new(),
            title_owner: None,
            shell,
            name,
            place: None,
            cols,
            rows,
            copy: None,
            last_focus: 0,
            spawned: Instant::now(),
            printed: false,
            touched: false,
        })
    }

    /// Feed program output into the emulator.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Notice> {
        self.printed = true;
        let mut notices = Vec::new();
        let mut start = 0;
        for (i, b) in bytes.iter().enumerate() {
            if let Some(request) = self.status.push(*b) {
                // Answer with the state right after the request, and after
                // whatever the emulator owes for the output before it, so
                // replies keep their order.
                self.parser.advance(&mut self.term, &bytes[start..=i]);
                start = i + 1;
                notices.extend(self.drain_events());
                self.write(status::reply(&request, &self.term.grid().cursor.template));
            }
        }
        self.parser.advance(&mut self.term, &bytes[start..]);
        notices.extend(self.drain_events());
        notices
    }

    /// A program may start a synchronized update and never finish it; the
    /// parser buffers output meanwhile, so flush it once the timeout passes.
    pub fn flush_stale_sync(&mut self, now: Instant) -> bool {
        match self.parser.sync_timeout().sync_timeout() {
            Some(deadline) if now >= deadline => {
                self.parser.stop_sync(&mut self.term);
                self.drain_events();
                true
            }
            _ => false,
        }
    }

    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    fn drain_events(&mut self) -> Vec<Notice> {
        let pending: Vec<TermEvent> = match self.events.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => return Vec::new(),
        };
        let mut notices = Vec::new();
        for event in pending {
            match event {
                TermEvent::PtyWrite(text) => self.write(text.into_bytes()),
                TermEvent::Title(t) => {
                    let owner = self.foreground_pid();
                    if self.shell && owner == self.pid.map(|p| p as i32) {
                        self.title.clear();
                    } else {
                        self.title = t;
                        self.title_owner = owner;
                    }
                }
                TermEvent::ResetTitle => self.title.clear(),
                TermEvent::Bell => notices.push(Notice::Bell),
                TermEvent::ClipboardStore(_, text) => notices.push(Notice::Clipboard(text)),
                TermEvent::TextAreaSizeRequest(format) => {
                    let size = WindowSize { num_lines: self.rows, num_cols: self.cols, cell_width: 8, cell_height: 16 };
                    self.write(format(size).into_bytes());
                }
                _ => {}
            }
        }
        notices
    }

    /// Queue raw bytes for the program.
    pub fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.writer.send(bytes);
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(2);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(TermSize { cols: cols as usize, rows: rows as usize });
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn in_copy_mode(&self) -> bool {
        self.copy.is_some()
    }

    /// Typing snaps the view back to the live screen, like any terminal.
    fn snap_to_bottom(&mut self) {
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
        }
        self.term.selection = None;
    }

    pub fn send_key(&mut self, key: Key) {
        self.touched = true;
        self.snap_to_bottom();
        let bytes = input::encode_key(key, self.mode());
        self.write(bytes);
    }

    pub fn send_paste(&mut self, text: &str) {
        self.touched = true;
        self.snap_to_bottom();
        let bytes = input::encode_paste(text, self.mode());
        self.write(bytes);
    }

    /// Forward a mouse event at pane-local coordinates, if the program wants it.
    pub fn send_mouse(&mut self, m: Mouse, col: u16, row: u16) -> bool {
        match input::encode_mouse(m, col, row, self.mode()) {
            Some(bytes) => {
                self.write(bytes);
                true
            }
            None => false,
        }
    }

    pub fn wants_mouse(&self) -> bool {
        !self.in_copy_mode() && self.mode().intersects(TermMode::MOUSE_MODE)
    }

    // ----- scrollback -----------------------------------------------------

    /// (lines scrolled back, lines of history available)
    pub fn scroll_position(&self) -> (usize, usize) {
        (self.term.grid().display_offset(), self.term.grid().history_size())
    }

    /// Scroll the view; positive is towards older output.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn clear_history(&mut self) {
        self.term.grid_mut().clear_history();
        self.term.scroll_display(Scroll::Bottom);
    }

    // ----- copy mode ------------------------------------------------------

    pub fn enter_copy(&mut self) {
        if self.copy.is_none() {
            self.term.selection = None;
            if !self.mode().contains(TermMode::VI) {
                self.term.toggle_vi_mode();
            }
            self.copy = Some(CopyMode { search: None, query: String::new(), forward: true });
        }
    }

    pub fn exit_copy(&mut self) {
        if self.copy.take().is_some() {
            self.term.selection = None;
            if self.mode().contains(TermMode::VI) {
                self.term.toggle_vi_mode();
            }
            self.term.scroll_display(Scroll::Bottom);
        }
    }

    pub fn motion(&mut self, motion: ViMotion) {
        self.term.vi_motion(motion);
    }

    /// Move the copy cursor and the view together; positive is up.
    pub fn copy_scroll(&mut self, lines: i32) {
        let cursor = self.term.vi_mode_cursor.scroll(&self.term, lines);
        self.term.vi_mode_cursor = cursor;
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn copy_goto_top(&mut self) {
        let point = Point::new(self.term.topmost_line(), Column(0));
        self.term.vi_goto_point(point);
    }

    pub fn copy_goto_bottom(&mut self) {
        let point = Point::new(self.term.bottommost_line(), Column(0));
        self.term.vi_goto_point(point);
    }

    pub fn toggle_selection(&mut self, ty: SelectionType) {
        let point = self.term.vi_mode_cursor.point;
        match &mut self.term.selection {
            Some(sel) if sel.ty == ty && !sel.is_empty() => self.term.selection = None,
            Some(sel) if !sel.is_empty() => sel.ty = ty,
            _ => {
                let mut sel = Selection::new(ty, point, Side::Left);
                sel.include_all();
                self.term.selection = Some(sel);
            }
        }
    }

    /// Start selecting at the copy cursor unless a selection already exists.
    pub fn begin_selection(&mut self) {
        if !self.has_selection() {
            let mut sel = Selection::new(SelectionType::Simple, self.term.vi_mode_cursor.point, Side::Left);
            sel.include_all();
            self.term.selection = Some(sel);
        }
    }

    /// Select everything, scrollback included.
    pub fn select_all(&mut self) {
        let start = Point::new(self.term.topmost_line(), Column(0));
        let end = Point::new(self.term.bottommost_line(), self.term.last_column());
        self.select_range(SelectionType::Simple, start, end);
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// Start a search from the copy cursor. Returns whether anything matched.
    pub fn search(&mut self, query: &str, forward: bool) -> bool {
        let regex = RegexSearch::new(query).or_else(|_| RegexSearch::new(&escape_regex(query)));
        let Some(copy) = self.copy.as_mut() else { return false };
        copy.query = query.to_string();
        copy.forward = forward;
        copy.search = regex.ok();
        self.search_next(false)
    }

    pub fn search_next(&mut self, reverse: bool) -> bool {
        let Some(copy) = self.copy.as_mut() else { return false };
        let Some(regex) = copy.search.as_mut() else { return false };
        let dir = if copy.forward != reverse { GridDir::Right } else { GridDir::Left };
        let cursor = self.term.vi_mode_cursor.point;
        let origin = match dir {
            GridDir::Right => cursor.add(&self.term, Boundary::None, 1),
            GridDir::Left => cursor.sub(&self.term, Boundary::None, 1),
        };
        match self.term.search_next(regex, origin, dir, Side::Left, None) {
            Some(found) => {
                self.term.vi_goto_point(*found.start());
                true
            }
            None => false,
        }
    }

    /// Search matches inside the visible part of the screen, for highlighting.
    pub fn visible_matches(&mut self) -> Vec<Match> {
        let Some(regex) = self.copy.as_mut().and_then(|c| c.search.as_mut()) else {
            return Vec::new();
        };
        let offset = self.term.grid().display_offset() as i32;
        let start = Point::new(Line(-offset), Column(0));
        let end = Point::new(Line(self.term.screen_lines() as i32 - 1 - offset), self.term.last_column());
        RegexIter::new(start, end, GridDir::Right, &self.term, regex).take(400).collect()
    }

    // ----- mouse selection ------------------------------------------------

    /// Grid point under a pane-local cell.
    pub fn point_at(&self, col: u16, row: u16) -> Point {
        let col = (col as usize).min(self.term.columns().saturating_sub(1));
        let row = (row as usize).min(self.term.screen_lines().saturating_sub(1));
        term::viewport_to_point(self.term.grid().display_offset(), Point::new(row, Column(col)))
    }

    pub fn select_range(&mut self, ty: SelectionType, start: Point, end: Point) {
        let forward = end >= start;
        let (s_side, e_side) = if forward { (Side::Left, Side::Right) } else { (Side::Right, Side::Left) };
        let mut sel = Selection::new(ty, start, s_side);
        sel.update(end, e_side);
        self.term.selection = Some(sel);
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    // ----- introspection --------------------------------------------------

    /// Where and how to draw the cursor, or `None` when it is hidden or
    /// scrolled out of view.
    pub fn cursor(&self) -> Option<CursorInfo> {
        let content = self.term.renderable_content();
        if content.cursor.shape == CursorShape::Hidden {
            return None;
        }
        let point = term::point_to_viewport(content.display_offset, content.cursor.point)?;
        if point.line >= self.rows as usize || point.column.0 >= self.cols as usize {
            return None;
        }
        let blinking = self.term.cursor_style().blinking;
        let style = if self.in_copy_mode() {
            2
        } else {
            match content.cursor.shape {
                CursorShape::Block => 2 - blinking as u8,
                CursorShape::Underline => 4 - blinking as u8,
                CursorShape::Beam => 6 - blinking as u8,
                _ => 0,
            }
        };
        Some(CursorInfo { col: point.column.0 as u16, row: point.line as u16, style })
    }

    /// Process group in the foreground of this pane's terminal.
    pub fn foreground_pid(&self) -> Option<i32> {
        self.master.process_group_leader().filter(|p| *p > 0).or(self.pid.map(|p| p as i32))
    }

    pub fn cwd(&self) -> Option<PathBuf> {
        self.foreground_pid().and_then(procinfo::cwd).or_else(|| self.pid.and_then(|p| procinfo::cwd(p as i32)))
    }

    /// Re-read the foreground program's name, directory and git branch.
    /// Returns true when any of them changed.
    pub fn refresh_name(&mut self) -> bool {
        // Right after the fork the child still carries our own name.
        if self.spawned.elapsed() < Duration::from_millis(300) {
            return false;
        }
        let mut changed = false;
        let fg = self.foreground_pid();
        if !self.title.is_empty() && self.title_owner != fg {
            self.title.clear();
            changed = true;
        }
        match fg.and_then(procinfo::name) {
            Some(n) if n != self.name => {
                self.name = n;
                changed = true;
            }
            _ => {}
        }
        let place = self.cwd().map(|dir| {
            let branch = procinfo::git_branch(&dir);
            (dir, branch)
        });
        if place != self.place {
            self.place = place;
            changed = true;
        }
        changed
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // Closing the pty hangs up the foreground job. Also tell the child
        // directly, and insist if it ignores the hint. The pid is only
        // signalled while the child is unreaped, so it cannot have been reused.
        let Some(pid) = self.pid.map(|p| p as i32).filter(|p| *p > 1) else { return };
        if self.reaped.load(Ordering::SeqCst) {
            return;
        }
        // SAFETY: plain signal delivery to a child we spawned.
        unsafe {
            libc::kill(pid, libc::SIGHUP);
        }
        let reaped = self.reaped.clone();
        let _ = thread::Builder::new().name("pane-reap".into()).spawn(move || {
            thread::sleep(Duration::from_millis(1500));
            if !reaped.load(Ordering::SeqCst) {
                // SAFETY: as above.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        });
    }
}

/// Variables naming the terminal nerdmux runs in, kept out of panes.
const OUTER_TERMINAL_VARS: &[&str] = &[
    "VTE_VERSION",
    "GNOME_TERMINAL_SCREEN",
    "GNOME_TERMINAL_SERVICE",
    "PTYXIS_VERSION",
    "KONSOLE_VERSION",
    "KONSOLE_DBUS_SERVICE",
    "KONSOLE_DBUS_SESSION",
    "KONSOLE_DBUS_WINDOW",
    "ALACRITTY_WINDOW_ID",
    "ALACRITTY_SOCKET",
    "ALACRITTY_LOG",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "KITTY_LISTEN_ON",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "WEZTERM_EXECUTABLE",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "WT_SESSION",
    "WT_PROFILE_ID",
];

const SHELLS: &[&str] = &["bash", "zsh", "fish", "sh", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "elvish", "xonsh"];

fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).trim_start_matches('-').to_string()
}

fn escape_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
