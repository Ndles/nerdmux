//! The server: owns every session, window and pane, and draws for each
//! attached client. Runs as a background process so sessions outlive the
//! terminal they were started from.

use crate::config::{Config, StatusPos};
use crate::input;
use crate::keys::{self, Action, Direction};
use crate::layout::{self, Axis, Geometry, Node, PaneId, Preset};
use crate::overlay::{
    edit_text, Confirm, Overlay, Pending, PickItem, Picker, Prompt, PromptKind, SessionId, Target, WindowId,
};
use crate::pane::{Notice, Pane, SpawnOpts};
use crate::proto::{
    read_msg, write_msg, Key, KeyCode, Mouse, MouseButton, MouseKind, Request, ALT, C2S, CTRL, PROTOCOL_VERSION, S2C,
    SHIFT, SUPER,
};
use crate::render::Caps;
use crate::settings;
use alacritty_terminal::index::Point;
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::cell::Hyperlink;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vi_mode::ViMotion;
use base64::Engine;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::collections::BTreeSet;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

mod restore;

pub type ClientId = u32;

/// Minimum time between two frames sent to clients.
const FRAME: Duration = Duration::from_millis(8);
const MESSAGE_TIME: Duration = Duration::from_millis(2500);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Frames a client may have in flight before it is considered behind.
const MAX_BACKLOG: usize = 2;
const STALE_POLL: Duration = Duration::from_millis(30);

pub enum Event {
    PtyOutput(PaneId, Vec<u8>),
    PtyEof(PaneId),
    ChildExit(PaneId),
    NewClient(UnixStream),
    ClientMsg(ClientId, C2S),
    ClientGone(ClientId),
}

#[derive(Clone)]
pub struct Window {
    pub id: WindowId,
    /// Name given by the user; otherwise the foreground program's name is shown.
    pub name: Option<String>,
    pub layout: Node,
    pub active: PaneId,
    pub last: Option<PaneId>,
    pub zoomed: bool,
    /// Typing goes to every pane of the window.
    pub sync: bool,
    pub preset: Option<Preset>,
    pub bell: bool,
}

impl Window {
    /// Where the split layout goes inside the window `area`.
    pub fn layout_area(&self, area: Rect, pad: Pad) -> Rect {
        pane_area(area, pad, self.layout.count())
    }

    pub fn geometry(&self, area: Rect, pad: Pad) -> Geometry {
        if self.zoomed {
            Geometry { panes: vec![(self.active, pane_area(area, pad, 1))], seps: Vec::new() }
        } else {
            self.layout.geometry(self.layout_area(area, pad))
        }
    }
}

/// Cells kept free around the panes: `padding` columns either side and
/// `padding_rows` rows on the edge away from the status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pad {
    pub x: u16,
    pub top: u16,
    pub bottom: u16,
}

impl Pad {
    pub fn new(config: &Config) -> Pad {
        let (x, y) = (config.padding, config.padding_rows);
        match config.status {
            StatusPos::Top => Pad { x, top: 0, bottom: y },
            _ => Pad { x, top: y, bottom: 0 },
        }
    }
}

/// Where the panes of a window with `count` of them go inside `area`.
/// Several panes keep a ring of one cell around them, so the focused one can
/// be outlined on every side; where there is padding, the ring takes its
/// outer cells, so the text starts in the same place with one pane or many.
pub fn pane_area(area: Rect, pad: Pad, count: usize) -> Rect {
    // A small terminal keeps every cell for the panes.
    let x = if area.width < pad.x.saturating_mul(2).saturating_add(20) { 0 } else { pad.x };
    let (mut top, mut bottom) = (pad.top, pad.bottom);
    if area.height < top.saturating_add(bottom).saturating_add(8) {
        (top, bottom) = (0, 0);
    }
    let ring = count >= 2 && area.width >= 8 && area.height >= 5;
    let (x, top, bottom) = if ring { (x.max(1), top.max(1), bottom.max(1)) } else { (x, top, bottom) };
    Rect::new(area.x + x, area.y + top, area.width - 2 * x, area.height - top - bottom)
}

pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub windows: Vec<Window>,
    pub active: WindowId,
    pub last: Option<WindowId>,
    pub cols: u16,
    pub rows: u16,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// Higher means used more recently.
    pub used: u64,
}

impl Session {
    pub fn active_index(&self) -> usize {
        self.windows.iter().position(|w| w.id == self.active).unwrap_or(0)
    }

    pub fn window(&self) -> &Window {
        &self.windows[self.active_index()]
    }

    pub fn window_mut(&mut self) -> &mut Window {
        let i = self.active_index();
        &mut self.windows[i]
    }

    pub fn select(&mut self, wid: WindowId) {
        if wid != self.active && self.windows.iter().any(|w| w.id == wid) {
            self.last = Some(self.active);
            self.active = wid;
        }
        self.window_mut().bell = false;
    }
}

pub struct Prefix {
    pub since: Instant,
    /// Set while a repeatable action keeps the prefix armed.
    pub repeat_until: Option<Instant>,
    /// Whether the key hints are on screen yet.
    pub shown: bool,
}

#[derive(Clone)]
pub enum Drag {
    Border { path: Vec<bool>, area: Rect, axis: Axis },
    Select { pane: PaneId, start: Point, ty: SelectionType },
    Forward { pane: PaneId },
}

/// The sending half of a client connection. `queued` counts messages the
/// writer thread has not finished writing, so the server can tell when a
/// client is not keeping up.
#[derive(Clone)]
pub struct Link {
    tx: SyncSender<S2C>,
    queued: Arc<AtomicUsize>,
}

impl Link {
    fn send(&self, msg: S2C) -> Result<(), TrySendError<S2C>> {
        self.queued.fetch_add(1, Ordering::SeqCst);
        self.tx.try_send(msg).inspect_err(|_| {
            self.queued.fetch_sub(1, Ordering::SeqCst);
        })
    }

    fn backlog(&self) -> usize {
        self.queued.load(Ordering::SeqCst)
    }
}

pub struct Client {
    pub id: ClientId,
    link: Link,
    /// A frame was skipped because the client was behind; draw when it catches up.
    stale: bool,
    pub cols: u16,
    pub rows: u16,
    pub session: SessionId,
    pub last_session: Option<SessionId>,
    /// What this client's terminal currently shows.
    pub prev: Buffer,
    /// The link of each cell of `prev`.
    pub prev_links: Vec<Option<Hyperlink>>,
    pub full_redraw: bool,
    pub prefix: Option<Prefix>,
    pub overlay: Option<Overlay>,
    /// A prompt or confirmation opened from the session manager is showing;
    /// the manager comes back when it closes, with this session selected.
    pub back_to_sessions: Option<SessionId>,
    pub message: Option<(String, Instant)>,
    /// Clickable window tabs in the status bar: (first column, end column, window).
    pub tabs: Vec<(u16, u16, WindowId)>,
    pub drag: Option<Drag>,
    last_click: Option<(Instant, u16, u16, u8)>,
    /// Cursor state last sent to the terminal: (col, row, DECSCUSR style).
    pub cursor: Option<(u16, u16, u8)>,
    pub cursor_style: u8,
    pub any_motion: bool,
    /// Raw bytes to pass through with the next frame (bell, clipboard).
    pub pending: Vec<u8>,
    /// Title last sent to the client's terminal.
    pub title: String,
    /// What the client's terminal can show.
    pub caps: Caps,
}

pub struct Server {
    pub config: Config,
    /// GNOME held Alt+Space on the desktop the last client attached from.
    gnome_took_leader: bool,
    pub sessions: Vec<Session>,
    pub panes: HashMap<PaneId, Pane>,
    pub clients: BTreeMap<ClientId, Client>,
    /// Connections that have not said hello yet.
    conns: HashMap<ClientId, Link>,
    /// Sessions whose panes must be resized before the next frame.
    layout_pending: BTreeSet<SessionId>,
    tx: SyncSender<Event>,
    socket: String,
    next_id: u32,
    clock: u64,
    paste_buffer: String,
    auto_copy_command: Option<String>,
    dirty: bool,
    shutdown: Option<String>,
    started: Instant,
    had_session: bool,
    last_poll: Instant,
    /// The snapshot of the sessions kept on disk, and restoring from it.
    snap: restore::SnapState,
}

/// Where windows are drawn inside a `cols` x `rows` terminal.
pub fn window_area(status: StatusPos, cols: u16, rows: u16) -> Rect {
    match status {
        StatusPos::Off => Rect::new(0, 0, cols, rows),
        _ if rows < 2 => Rect::new(0, 0, cols, rows),
        StatusPos::Top => Rect::new(0, 1, cols, rows - 1),
        StatusPos::Bottom => Rect::new(0, 0, cols, rows - 1),
    }
}

/// A path with the home directory written as `~`.
pub fn tidy_path(path: &std::path::Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let s = path.to_string_lossy().into_owned();
    match s.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => s,
    }
}

pub fn window_title(win: &Window, panes: &HashMap<PaneId, Pane>) -> String {
    match &win.name {
        Some(n) => n.clone(),
        None => panes.get(&win.active).map(|p| p.name.clone()).unwrap_or_default(),
    }
}

/// The leader used while GNOME holds Alt+Space.
const GNOME_LEADER: Key = Key::ctrl(' ');

/// GNOME binds Alt+Space to the window menu and takes it before the
/// terminal sees it. Only asked of a desktop the client sits on, not over
/// ssh; `None` when the client is not on a GNOME desktop.
fn gnome_takes_alt_space(env: &[(String, String)]) -> Option<bool> {
    let get = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    let gnome = get("XDG_CURRENT_DESKTOP").is_some_and(|d| d.to_ascii_lowercase().contains("gnome"));
    if !gnome || get("SSH_CONNECTION").is_some() || !find_in_path("gsettings") {
        return None;
    }
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.wm.keybindings", "activate-window-menu"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).to_ascii_lowercase().contains("<alt>space"))
}

fn find_in_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

fn detect_copy_command() -> Option<String> {
    if cfg!(target_os = "macos") {
        return Some("pbcopy".into());
    }
    let has = |var: &str| std::env::var_os(var).is_some_and(|v| !v.is_empty());
    if has("WAYLAND_DISPLAY") && find_in_path("wl-copy") {
        return Some("wl-copy".into());
    }
    if has("DISPLAY") {
        if find_in_path("xclip") {
            return Some("xclip -selection clipboard -in".into());
        }
        if find_in_path("xsel") {
            return Some("xsel --clipboard --input".into());
        }
    }
    None
}

fn run_copy_command(cmd: String, text: String) {
    let _ = thread::Builder::new().name("copy".into()).spawn(move || {
        use std::process::{Command, Stdio};
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg(&cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    });
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && !name.chars().any(|c| c.is_control())
}

impl Server {
    /// `terminating` is set when the server is told to stop (SIGTERM).
    pub fn new(socket: String, tx: SyncSender<Event>, terminating: Arc<AtomicBool>) -> Server {
        let now = Instant::now();
        let mut server = Server {
            config: Config::load(),
            gnome_took_leader: false,
            sessions: Vec::new(),
            panes: HashMap::new(),
            clients: BTreeMap::new(),
            conns: HashMap::new(),
            layout_pending: BTreeSet::new(),
            tx,
            socket,
            next_id: 1,
            clock: 1,
            paste_buffer: String::new(),
            auto_copy_command: detect_copy_command(),
            dirty: false,
            shutdown: None,
            started: now,
            had_session: false,
            last_poll: now,
            snap: restore::SnapState::new(terminating),
        };
        server.snapshot_start();
        server
    }

    fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn bump(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    // ===== main loop ========================================================

    pub fn run(mut self, rx: Receiver<Event>) {
        let mut last_render = Instant::now() - Duration::from_secs(1);
        let mut panics = 0u32;
        loop {
            // Told to stop: leave at once, before handling the panes that
            // end along with us, so the snapshot keeps them.
            if self.snap.terminating() {
                break;
            }
            let now = Instant::now();
            let mut deadline = now + Duration::from_millis(500);
            if self.dirty {
                deadline = deadline.min(last_render + FRAME);
            }
            if let Some(t) = self.next_timer() {
                deadline = deadline.min(t);
            }
            let first = match rx.recv_timeout(deadline.saturating_duration_since(now)) {
                Ok(ev) => Some(ev),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            // A bug in handling one event must not take every session down
            // with it: log the panic and carry on.
            let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(ev) = first {
                    self.handle(ev);
                    let batch = Instant::now();
                    while batch.elapsed() < Duration::from_millis(4) && !self.snap.terminating() {
                        match rx.try_recv() {
                            Ok(ev) => self.handle(ev),
                            Err(_) => break,
                        }
                    }
                }
                self.tick(Instant::now());
                if self.shutdown.is_none() && self.dirty && last_render.elapsed() >= FRAME {
                    self.render_all();
                    last_render = Instant::now();
                    self.dirty = false;
                }
            }));
            match step {
                Ok(()) => panics = 0,
                Err(_) => {
                    panics += 1;
                    eprintln!("nerdmux: recovered from an internal error ({panics})");
                    for c in self.clients.values_mut() {
                        c.full_redraw = true;
                        c.overlay = None;
                    }
                    self.dirty = true;
                    if panics > 20 {
                        self.snap.failed = true;
                        self.shutdown = Some("[nerdmux server failed]".into());
                    }
                }
            }
            if self.shutdown.is_some() {
                break;
            }
        }
        // Stop being reachable first, so a client arriving now starts a
        // fresh server instead of talking to one that is on its way out.
        let _ = std::fs::remove_file(&self.socket);
        let message = self.shutdown.take().unwrap_or_else(|| "[server exited]".into());
        for (_, c) in std::mem::take(&mut self.clients) {
            let _ = c.link.send(S2C::Exit { message: message.clone(), code: 0 });
        }
        self.panes.clear();
        // Give writer threads a moment to deliver the goodbye.
        thread::sleep(Duration::from_millis(80));
        self.snapshot_end();
    }

    fn next_timer(&self) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut consider = |t: Instant| next = Some(next.map_or(t, |n| n.min(t)));
        for c in self.clients.values() {
            if let Some(p) = &c.prefix {
                match p.repeat_until {
                    Some(t) => consider(t),
                    None if !p.shown => consider(p.since + self.config.which_key_delay),
                    None => {}
                }
            }
            if let Some((_, until)) = &c.message {
                consider(*until);
            }
            if c.stale {
                consider(Instant::now() + STALE_POLL);
            }
        }
        for p in self.panes.values() {
            if let Some(t) = p.sync_deadline() {
                consider(t);
            }
        }
        if let Some(t) = self.snap.next_timer() {
            consider(t);
        }
        next
    }

    fn tick(&mut self, now: Instant) {
        let delay = self.config.which_key_delay;
        for c in self.clients.values_mut() {
            if c.stale && c.link.backlog() < MAX_BACKLOG {
                self.dirty = true;
            }
            if c.message.as_ref().is_some_and(|(_, until)| now >= *until) {
                c.message = None;
                self.dirty = true;
            }
            if let Some(p) = c.prefix.as_mut() {
                match p.repeat_until {
                    Some(t) if now >= t => {
                        c.prefix = None;
                        self.dirty = true;
                    }
                    None if !p.shown && now >= p.since + delay => {
                        p.shown = true;
                        self.dirty = true;
                    }
                    _ => {}
                }
            }
        }
        for p in self.panes.values_mut() {
            if p.flush_stale_sync(now) {
                self.dirty = true;
            }
        }
        if now.duration_since(self.last_poll) >= Duration::from_millis(500) {
            self.last_poll = now;
            for s in &self.sessions {
                for w in &s.windows {
                    if let Some(p) = self.panes.get_mut(&w.active) {
                        if p.refresh_name() {
                            self.dirty = true;
                        }
                    }
                }
            }
        }
        self.snapshot_tick(now);
        if self.shutdown.is_none() && self.sessions.is_empty() {
            let idle = self.clients.is_empty() && self.conns.is_empty();
            if self.had_session || (idle && now.duration_since(self.started) > Duration::from_secs(5)) {
                self.shutdown = Some("[exited]".into());
            }
        }
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::PtyOutput(id, bytes) => {
                let Some(pane) = self.panes.get_mut(&id) else { return };
                let notices = pane.feed(&bytes);
                self.dirty = true;
                self.snapshot_output(id);
                for n in notices {
                    match n {
                        Notice::Bell => self.bell(id),
                        Notice::Clipboard(text) => self.set_clipboard(text, None),
                    }
                }
            }
            Event::PtyEof(id) => self.close_pane(id),
            Event::ChildExit(id) => self.close_pane(id),
            Event::NewClient(stream) => self.accept(stream),
            Event::ClientMsg(id, msg) => self.client_msg(id, msg),
            Event::ClientGone(id) => {
                self.conns.remove(&id);
                if self.clients.remove(&id).is_some() {
                    self.dirty = true;
                }
            }
        }
    }

    // ===== connections ======================================================

    fn accept(&mut self, stream: UnixStream) {
        let id = self.id();
        let Ok(mut read_half) = stream.try_clone() else { return };
        let mut write_half = stream;
        let events = self.tx.clone();
        let reader = thread::Builder::new().name(format!("client-{id}-read")).spawn(move || loop {
            match read_msg::<_, C2S>(&mut read_half) {
                Ok(msg) => {
                    if events.send(Event::ClientMsg(id, msg)).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = events.send(Event::ClientGone(id));
                    break;
                }
            }
        });
        if reader.is_err() {
            return;
        }
        // Frames go through a bounded queue: a client that stops reading
        // loses frames (and gets a full redraw later) instead of blocking
        // everyone else.
        let (tx, rx) = mpsc::sync_channel::<S2C>(16);
        let queued = Arc::new(AtomicUsize::new(0));
        let written = queued.clone();
        let writer = thread::Builder::new().name(format!("client-{id}-write")).spawn(move || {
            for msg in rx {
                let last = matches!(msg, S2C::Exit { .. });
                let failed = write_msg(&mut write_half, &msg).is_err();
                written.fetch_sub(1, Ordering::SeqCst);
                if failed || last {
                    break;
                }
            }
            let _ = write_half.shutdown(std::net::Shutdown::Both);
        });
        if writer.is_ok() {
            self.conns.insert(id, Link { tx, queued });
        }
    }

    fn client_msg(&mut self, id: ClientId, msg: C2S) {
        match msg {
            C2S::Hello { version, cols, rows, cwd, env, request } => {
                let Some(link) = self.conns.remove(&id) else { return };
                if version != PROTOCOL_VERSION {
                    let message =
                        "nerdmux: this client and the running server are different versions; run `nerdmux kill-server` and start again"
                            .to_string();
                    let _ = link.send(S2C::Exit { message, code: 1 });
                    return;
                }
                self.hello(id, link, cols, rows, cwd, env, request);
            }
            // Input must never overtake a resize: a program has to learn
            // its new size before it sees keys typed after the change.
            C2S::Key(key) => {
                self.flush_layout();
                self.on_key(id, key.normalized());
            }
            C2S::Mouse(m) => {
                if !matches!(m.kind, MouseKind::Drag(_)) {
                    self.flush_layout();
                }
                self.on_mouse(id, m);
            }
            C2S::Paste(text) => {
                self.flush_layout();
                self.on_paste(id, text);
            }
            C2S::Resize(cols, rows) => {
                if let Some(c) = self.clients.get_mut(&id) {
                    c.cols = cols.max(1);
                    c.rows = rows.max(1);
                    c.prev = Buffer::empty(Rect::new(0, 0, c.cols, c.rows));
                    c.prev_links = vec![None; c.prev.content.len()];
                    c.full_redraw = true;
                    self.adopt_client_size(id);
                    self.dirty = true;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn hello(
        &mut self,
        id: ClientId,
        link: Link,
        cols: u16,
        rows: u16,
        cwd: String,
        env: Vec<(String, String)>,
        request: Request,
    ) {
        let reply = |message: String, code: i32| {
            let _ = link.send(S2C::Exit { message, code });
        };
        let caps = Caps::detect(&env);
        let attaching =
            matches!(request, Request::New { detached: false, .. } | Request::Attach { .. } | Request::Restore);
        let gnome = if attaching { gnome_takes_alt_space(&env) } else { None };
        match request {
            Request::List => {
                let mut out = String::new();
                for s in &self.sessions {
                    let panes: usize = s.windows.iter().map(|w| w.layout.count()).sum();
                    let attached = self.clients.values().filter(|c| c.session == s.id).count();
                    out.push_str(&format!(
                        "{}: {} window{}, {} pane{}{}\n",
                        s.name,
                        s.windows.len(),
                        if s.windows.len() == 1 { "" } else { "s" },
                        panes,
                        if panes == 1 { "" } else { "s" },
                        if attached > 0 { " (attached)" } else { "" }
                    ));
                }
                reply(out.trim_end().to_string(), 0);
            }
            Request::KillServer => {
                self.snap.clean = true;
                reply(String::new(), 0);
                self.shutdown = Some("[server exited]".into());
            }
            Request::KillSession { target } => match self.find_session(&target) {
                Some(sid) => {
                    self.remove_session(sid);
                    reply(String::new(), 0);
                }
                None => reply(format!("nerdmux: no session named {target:?}"), 1),
            },
            Request::New { name, command, detached } => {
                match self.create_session(name, command, cwd, env, cols, rows) {
                    Ok(sid) if detached => {
                        let name = self.session_name(sid);
                        reply(format!("created session {name}"), 0);
                    }
                    Ok(sid) => self.attach(id, link.clone(), cols, rows, sid, caps),
                    Err(e) => reply(format!("nerdmux: {e}"), 1),
                }
            }
            Request::Attach { target, create } => {
                let found = match &target {
                    Some(t) => match self.find_session(t) {
                        Some(sid) => Some(sid),
                        None => {
                            reply(format!("nerdmux: no session named {t:?}"), 1);
                            return;
                        }
                    },
                    None => self.sessions.iter().max_by_key(|s| s.used).map(|s| s.id),
                };
                match found {
                    Some(sid) => self.attach(id, link.clone(), cols, rows, sid, caps),
                    None if create => match self.create_session(None, Vec::new(), cwd, env, cols, rows) {
                        Ok(sid) => {
                            self.note_placeholder(sid);
                            self.attach(id, link.clone(), cols, rows, sid, caps);
                        }
                        Err(e) => return reply(format!("nerdmux: {e}"), 1),
                    },
                    None => return reply("nerdmux: no sessions".into(), 1),
                }
                // After a crash or reboot, the first client is offered the
                // saved sessions.
                self.offer_restore(id, false);
            }
            Request::Restore => {
                let found = self.sessions.iter().max_by_key(|s| s.used).map(|s| s.id);
                let sid = match found {
                    Some(sid) => sid,
                    None => match self.create_session(None, Vec::new(), cwd, env, cols, rows) {
                        Ok(sid) => {
                            self.note_placeholder(sid);
                            sid
                        }
                        Err(e) => return reply(format!("nerdmux: {e}"), 1),
                    },
                };
                self.attach(id, link.clone(), cols, rows, sid, caps);
                self.offer_restore(id, true);
            }
            Request::Action { target, pane, action } => {
                let Some(action) = Action::parse(&action) else {
                    reply(format!("nerdmux: unknown action {action:?}"), 1);
                    return;
                };
                let by_pane = pane.and_then(|p| {
                    self.sessions.iter().find(|s| s.windows.iter().any(|w| w.layout.contains(p))).map(|s| s.id)
                });
                let sid = match &target {
                    Some(t) => self.find_session(t),
                    None => by_pane.or_else(|| self.sessions.iter().max_by_key(|s| s.used).map(|s| s.id)),
                };
                let Some(sid) = sid else {
                    reply("nerdmux: no such session".into(), 1);
                    return;
                };
                let cid = self.clients.values().find(|c| c.session == sid).map(|c| c.id);
                self.run_action(cid, sid, action);
                reply(String::new(), 0);
            }
        }
        if let Some(taken) = gnome {
            self.gnome_took_leader = taken;
            self.pick_leader();
            let text = match (taken, self.config.prefix_chosen) {
                (true, false) => Some(format!(
                    "GNOME keeps Alt+Space for its window menu, so the leader is {} here",
                    keys::key_label(GNOME_LEADER)
                )),
                (true, true) if self.config.prefix == Key::alt(' ') => {
                    Some("GNOME keeps Alt+Space for its window menu: free it, or set another prefix".to_string())
                }
                _ => None,
            };
            if let (Some(text), Some(c)) = (text, self.clients.get_mut(&id)) {
                c.message = Some((text, Instant::now() + MESSAGE_TIME * 3));
            }
        }
    }

    fn attach(&mut self, id: ClientId, link: Link, cols: u16, rows: u16, sid: SessionId, caps: Caps) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let mut client = Client {
            id,
            link,
            stale: false,
            cols,
            rows,
            session: sid,
            last_session: None,
            prev: Buffer::empty(Rect::new(0, 0, cols, rows)),
            prev_links: vec![None; cols as usize * rows as usize],
            full_redraw: true,
            prefix: None,
            overlay: None,
            back_to_sessions: None,
            message: None,
            tabs: Vec::new(),
            drag: None,
            last_click: None,
            cursor: None,
            cursor_style: 0,
            any_motion: false,
            pending: Vec::new(),
            title: String::new(),
            caps,
        };
        if self.config.mouse {
            client.pending.extend_from_slice(MOUSE_ON);
        }
        client.pending.extend_from_slice(&self.terminal_colors());
        if let Some(w) = self.config.warnings.first() {
            client.message = Some((w.clone(), Instant::now() + MESSAGE_TIME * 2));
        }
        self.clients.insert(id, client);
        self.adopt_client_size(id);
        self.dirty = true;
    }

    /// Escape sequences that tint the terminal's cursor and default
    /// background to match the theme. The background also colors the
    /// terminal's window padding, so the screen has no seam around its edge.
    fn terminal_colors(&self) -> Vec<u8> {
        use ratatui::style::Color::Rgb;
        let theme = &self.config.theme;
        let mut out = match (theme.own_palette, theme.cursor) {
            (true, Rgb(r, g, b)) => format!("\x1b]12;#{r:02x}{g:02x}{b:02x}\x07").into_bytes(),
            _ => b"\x1b]112\x07".to_vec(),
        };
        match (theme.own_palette, theme.bg) {
            (true, Rgb(r, g, b)) => out.extend_from_slice(format!("\x1b]11;#{r:02x}{g:02x}{b:02x}\x07").as_bytes()),
            _ => out.extend_from_slice(b"\x1b]111\x07"),
        }
        out
    }

    fn render_all(&mut self) {
        self.flush_layout();
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for cid in ids {
            let Some(c) = self.clients.get_mut(&cid) else { continue };
            // A client that still has frames queued is not keeping up (slow
            // link, suspended terminal). Skip it: its next frame is diffed
            // against the last one it actually received, so nothing is lost.
            if c.link.backlog() >= MAX_BACKLOG {
                c.stale = true;
                continue;
            }
            c.stale = false;
            let Some(bytes) = self.render_client(cid) else { continue };
            let Some(c) = self.clients.get_mut(&cid) else { continue };
            match c.link.send(S2C::Output(bytes)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    c.full_redraw = true;
                    c.stale = true;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.clients.remove(&cid);
                }
            }
        }
    }

    fn drop_client(&mut self, cid: ClientId, message: String) {
        if let Some(c) = self.clients.remove(&cid) {
            let _ = c.link.send(S2C::Exit { message, code: 0 });
        }
        self.dirty = true;
    }

    fn message(&mut self, cid: Option<ClientId>, text: impl Into<String>) {
        if let Some(c) = cid.and_then(|id| self.clients.get_mut(&id)) {
            c.message = Some((text.into(), Instant::now() + MESSAGE_TIME));
            self.dirty = true;
        }
    }

    // ===== sessions, windows, panes =========================================

    fn find_session(&self, target: &str) -> Option<SessionId> {
        if let Some(s) = self.sessions.iter().find(|s| s.name == target) {
            return Some(s.id);
        }
        let mut matches = self.sessions.iter().filter(|s| s.name.starts_with(target));
        match (matches.next(), matches.next()) {
            (Some(s), None) => Some(s.id),
            _ => None,
        }
    }

    fn session_name(&self, sid: SessionId) -> String {
        self.sessions.iter().find(|s| s.id == sid).map(|s| s.name.clone()).unwrap_or_default()
    }

    fn session_index(&self, sid: SessionId) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == sid)
    }

    fn create_session(
        &mut self,
        name: Option<String>,
        command: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
        cols: u16,
        rows: u16,
    ) -> Result<SessionId, String> {
        let name = match name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
            Some(n) => {
                if !valid_name(&n) {
                    return Err(format!("bad session name {n:?}"));
                }
                if self.sessions.iter().any(|s| s.name == n) {
                    return Err(format!("a session named {n:?} already exists"));
                }
                n
            }
            None => (0..).map(|i| i.to_string()).find(|n| !self.sessions.iter().any(|s| &s.name == n)).unwrap(),
        };
        let sid = self.id();
        let used = self.bump();
        let (cols, rows) = if cols < 2 || rows < 2 { (80, 24) } else { (cols, rows) };
        self.sessions.push(Session {
            id: sid,
            name,
            windows: Vec::new(),
            active: 0,
            last: None,
            cols,
            rows,
            env,
            cwd: PathBuf::from(cwd),
            used,
        });
        let command = (!command.is_empty()).then_some(command);
        match self.new_window(sid, command, None) {
            Ok(_) => {
                self.had_session = true;
                Ok(sid)
            }
            Err(e) => {
                self.sessions.retain(|s| s.id != sid);
                Err(e)
            }
        }
    }

    fn session_area(&self, sid: SessionId) -> Option<Rect> {
        let s = self.sessions.iter().find(|s| s.id == sid)?;
        Some(window_area(self.config.status, s.cols, s.rows))
    }

    fn spawn_pane(
        &mut self,
        sid: SessionId,
        cols: u16,
        rows: u16,
        command: Option<Vec<String>>,
        cwd: Option<PathBuf>,
    ) -> Result<PaneId, String> {
        let id = self.id();
        let focus = self.bump();
        let session = self.sessions.iter().find(|s| s.id == sid).ok_or("no such session")?;
        let mut env = session.env.clone();
        let command = command.unwrap_or_else(|| {
            let shell = self
                .config
                .shell
                .clone()
                .or_else(|| session.env.iter().find(|(k, _)| k == "SHELL").map(|(_, v)| v.clone()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/bin/sh".to_string());
            let mut argv = vec![shell];
            argv.extend(self.config.shell_args.iter().cloned());
            if self.config.prompt {
                let dir = Path::new(&self.socket).with_extension("shell");
                if std::fs::create_dir_all(&dir).is_ok() {
                    let mut sgr = b"\\e[1".to_vec();
                    crate::render::push_color(&mut sgr, self.config.theme.accent, true, true);
                    sgr.push(b'm');
                    let sgr = String::from_utf8_lossy(&sgr);
                    crate::prompt::wrap(&mut argv, &mut env, &dir, &sgr, &self.config.prompt_mark);
                }
            }
            argv
        });
        let program = command[0].clone();
        let opts = SpawnOpts {
            cols,
            rows,
            command,
            cwd: cwd.or_else(|| Some(session.cwd.clone())),
            env: &env,
            term: &self.config.term,
            scrollback: self.config.scrollback,
            socket: &self.socket,
        };
        let mut pane = Pane::spawn(id, opts, self.tx.clone()).map_err(|e| format!("cannot start {program}: {e}"))?;
        pane.last_focus = focus;
        self.panes.insert(id, pane);
        Ok(id)
    }

    fn new_window(
        &mut self,
        sid: SessionId,
        command: Option<Vec<String>>,
        cwd: Option<PathBuf>,
    ) -> Result<WindowId, String> {
        let area = self.session_area(sid).ok_or("no such session")?;
        let pid = self.spawn_pane(sid, area.width, area.height, command, cwd)?;
        let wid = self.id();
        let session = self.sessions.iter_mut().find(|s| s.id == sid).ok_or("no such session")?;
        session.windows.push(Window {
            id: wid,
            name: None,
            layout: Node::Leaf(pid),
            active: pid,
            last: None,
            zoomed: false,
            sync: false,
            preset: None,
            bell: false,
        });
        if session.windows.len() == 1 {
            session.active = wid;
        } else {
            session.select(wid);
        }
        self.dirty = true;
        Ok(wid)
    }

    /// Note that a session's panes need resizing. The work happens once,
    /// right before the next frame, however many changes pile up (dragging a
    /// border or a terminal window produces a burst of them).
    fn relayout(&mut self, sid: SessionId) {
        self.layout_pending.insert(sid);
        self.dirty = true;
    }

    fn flush_layout(&mut self) {
        for sid in std::mem::take(&mut self.layout_pending) {
            let Some(area) = self.session_area(sid) else { continue };
            let Some(session) = self.sessions.iter().find(|s| s.id == sid) else { continue };
            for w in &session.windows {
                for (pid, rect) in w.geometry(area, Pad::new(&self.config)).panes {
                    if let Some(p) = self.panes.get_mut(&pid) {
                        p.resize(rect.width, rect.height);
                    }
                }
            }
        }
    }

    /// A session is as large as the client that used it last.
    fn adopt_client_size(&mut self, cid: ClientId) {
        let Some(c) = self.clients.get(&cid) else { return };
        let (sid, cols, rows) = (c.session, c.cols, c.rows);
        let used = self.bump();
        let Some(s) = self.sessions.iter_mut().find(|s| s.id == sid) else { return };
        s.used = used;
        if s.cols != cols || s.rows != rows {
            s.cols = cols;
            s.rows = rows;
            self.relayout(sid);
        }
    }

    fn switch_client(&mut self, cid: ClientId, sid: SessionId) {
        if !self.sessions.iter().any(|s| s.id == sid) {
            return;
        }
        let Some(c) = self.clients.get_mut(&cid) else { return };
        if c.session != sid {
            c.last_session = Some(c.session);
            c.session = sid;
            c.drag = None;
        }
        self.adopt_client_size(cid);
        self.dirty = true;
    }

    fn remove_session(&mut self, sid: SessionId) {
        let Some(idx) = self.session_index(sid) else { return };
        let session = self.sessions.remove(idx);
        for w in &session.windows {
            for pid in w.layout.panes() {
                self.panes.remove(&pid);
            }
        }
        let fallback = self.sessions.iter().max_by_key(|s| s.used).map(|s| s.id);
        let affected: Vec<ClientId> = self.clients.values().filter(|c| c.session == sid).map(|c| c.id).collect();
        for cid in affected {
            // Back to the session the client came from, if it is still there.
            let previous = self.clients.get(&cid).and_then(|c| c.last_session);
            let next = previous.filter(|p| self.sessions.iter().any(|s| s.id == *p)).or(fallback);
            match next {
                Some(next) => {
                    self.switch_client(cid, next);
                    if let Some(c) = self.clients.get_mut(&cid) {
                        c.last_session = None;
                        c.overlay = None;
                    }
                }
                None => self.drop_client(cid, "[exited]".into()),
            }
        }
        for c in self.clients.values_mut() {
            if c.last_session == Some(sid) {
                c.last_session = None;
            }
        }
        self.dirty = true;
    }

    /// Client `cid` closes session `sid`: it goes back to the session it came
    /// from, or the most recently used one, and when `sid` was the last,
    /// to a fresh session, which takes over the name if it was a number.
    fn close_session(&mut self, cid: ClientId, sid: SessionId) {
        if self.sessions.len() > 1 {
            self.remove_session(sid);
            return;
        }
        let Some(old) = self.sessions.iter().find(|s| s.id == sid) else { return };
        let (env, cwd, name) = (old.env.clone(), old.cwd.to_string_lossy().into_owned(), old.name.clone());
        let (cols, rows) = self.clients.get(&cid).map_or((80, 24), |c| (c.cols, c.rows));
        let fresh = match self.create_session(None, Vec::new(), cwd, env, cols, rows) {
            Ok(fresh) => fresh,
            Err(e) => {
                self.message(Some(cid), format!("cannot start a new session: {e}"));
                return;
            }
        };
        self.switch_client(cid, fresh);
        self.remove_session(sid);
        if name.parse::<u32>().is_ok() {
            if let Some(s) = self.sessions.iter_mut().find(|s| s.id == fresh) {
                s.name = name;
            }
        }
        if let Some(c) = self.clients.get_mut(&cid) {
            c.last_session = None;
        }
    }

    fn remove_window(&mut self, sid: SessionId, wid: WindowId) {
        let Some(session) = self.sessions.iter_mut().find(|s| s.id == sid) else { return };
        let Some(idx) = session.windows.iter().position(|w| w.id == wid) else { return };
        let window = session.windows.remove(idx);
        for pid in window.layout.panes() {
            self.panes.remove(&pid);
        }
        if session.windows.is_empty() {
            self.remove_session(sid);
            return;
        }
        if session.last == Some(wid) {
            session.last = None;
        }
        if session.active == wid {
            let next = session
                .last
                .filter(|l| session.windows.iter().any(|w| w.id == *l))
                .unwrap_or_else(|| session.windows[idx.min(session.windows.len() - 1)].id);
            session.active = next;
            session.last = None;
            session.window_mut().bell = false;
        }
        self.dirty = true;
    }

    fn locate_pane(&self, pid: PaneId) -> Option<(SessionId, WindowId)> {
        for s in &self.sessions {
            for w in &s.windows {
                if w.layout.contains(pid) {
                    return Some((s.id, w.id));
                }
            }
        }
        None
    }

    fn close_pane(&mut self, pid: PaneId) {
        let Some((sid, wid)) = self.locate_pane(pid) else {
            self.panes.remove(&pid);
            return;
        };
        let Some(session) = self.sessions.iter_mut().find(|s| s.id == sid) else { return };
        let Some(window) = session.windows.iter_mut().find(|w| w.id == wid) else { return };
        if matches!(window.layout, Node::Leaf(id) if id == pid) {
            self.remove_window(sid, wid);
            return;
        }
        window.layout.remove(pid);
        window.zoomed = false;
        if window.last == Some(pid) {
            window.last = None;
        }
        if window.active == pid {
            let panes = window.layout.panes();
            let next = window
                .last
                .filter(|l| panes.contains(l))
                .or_else(|| panes.iter().copied().max_by_key(|p| self.panes.get(p).map(|p| p.last_focus).unwrap_or(0)))
                .unwrap_or(panes[0]);
            window.active = next;
            window.last = None;
        }
        self.panes.remove(&pid);
        self.relayout(sid);
    }

    fn focus_pane(&mut self, sid: SessionId, pid: PaneId) {
        let stamp = self.bump();
        let Some(session) = self.sessions.iter_mut().find(|s| s.id == sid) else { return };
        let window = session.window_mut();
        if !window.layout.contains(pid) {
            return;
        }
        let was_zoomed = window.zoomed;
        if window.active != pid {
            window.last = Some(window.active);
            window.active = pid;
            window.zoomed = false;
        }
        if let Some(p) = self.panes.get_mut(&pid) {
            p.last_focus = stamp;
        }
        if was_zoomed && !self.sessions.iter().any(|s| s.id == sid && s.window().zoomed) {
            self.relayout(sid);
        }
        self.dirty = true;
    }

    fn active_pane(&self, sid: SessionId) -> Option<PaneId> {
        self.sessions.iter().find(|s| s.id == sid).map(|s| s.window().active)
    }

    /// The panes that receive typed input: the active one, or all of the
    /// window when input is synchronized.
    fn input_targets(&self, sid: SessionId) -> Vec<PaneId> {
        let Some(s) = self.sessions.iter().find(|s| s.id == sid) else { return Vec::new() };
        let w = s.window();
        if w.sync {
            w.layout.panes()
        } else {
            vec![w.active]
        }
    }

    fn split(&mut self, cid: Option<ClientId>, sid: SessionId, axis: Option<Axis>) {
        let Some(area) = self.session_area(sid) else { return };
        let Some(session) = self.sessions.iter().find(|s| s.id == sid) else { return };
        let window = session.window();
        let active = window.active;
        let pad = Pad::new(&self.config);
        let rect = window.layout.geometry(window.layout_area(area, pad)).rect_of(active).unwrap_or(area);
        // Terminal cells are roughly twice as tall as wide.
        let axis = axis.unwrap_or(if rect.width as u32 >= rect.height as u32 * 5 / 2 {
            Axis::Horizontal
        } else {
            Axis::Vertical
        });
        let fits = match axis {
            Axis::Horizontal => rect.width >= 5,
            Axis::Vertical => rect.height >= 3,
        };
        if !fits {
            self.message(cid, "pane too small to split");
            return;
        }
        // Work out the size the new pane will have, so its program starts
        // at the right size instead of being resized a moment later.
        let mut preview = window.layout.clone();
        preview.split(active, axis, PaneId::MAX);
        let size = preview.geometry(pane_area(area, pad, 2)).rect_of(PaneId::MAX).unwrap_or(rect);
        let cwd = if self.config.inherit_cwd { self.panes.get(&active).and_then(|p| p.cwd()) } else { None };
        let pid = match self.spawn_pane(sid, size.width, size.height, None, cwd) {
            Ok(pid) => pid,
            Err(e) => {
                self.message(cid, e);
                return;
            }
        };
        let Some(session) = self.sessions.iter_mut().find(|s| s.id == sid) else { return };
        let window = session.window_mut();
        window.layout.split(active, axis, pid);
        window.zoomed = false;
        window.preset = None;
        window.last = Some(active);
        window.active = pid;
        self.relayout(sid);
    }

    // ===== clipboard ========================================================

    fn set_clipboard(&mut self, text: String, cid: Option<ClientId>) {
        if text.is_empty() {
            return;
        }
        // OSC 52 asks each client's terminal to fill the system clipboard,
        // which also works over SSH.
        if text.len() <= 1_000_000 {
            let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
            let osc = format!("\x1b]52;c;{encoded}\x07");
            for c in self.clients.values_mut() {
                c.pending.extend_from_slice(osc.as_bytes());
            }
        }
        if let Some(cmd) = self.config.copy_command.clone().or_else(|| self.auto_copy_command.clone()) {
            run_copy_command(cmd, text.clone());
        }
        let lines = text.lines().count();
        let what = if lines > 1 { format!("{lines} lines") } else { format!("{} characters", text.chars().count()) };
        self.paste_buffer = text;
        self.message(cid, format!("copied {what}"));
        self.dirty = true;
    }

    fn bell(&mut self, pid: PaneId) {
        let Some((sid, wid)) = self.locate_pane(pid) else { return };
        let Some(session) = self.sessions.iter_mut().find(|s| s.id == sid) else { return };
        if session.active != wid {
            if let Some(w) = session.windows.iter_mut().find(|w| w.id == wid) {
                w.bell = true;
            }
        }
        for c in self.clients.values_mut().filter(|c| c.session == sid) {
            c.pending.push(0x07);
        }
    }

    // ===== input ============================================================

    fn on_key(&mut self, cid: ClientId, key: Key) {
        let now = Instant::now();
        self.adopt_client_size(cid);
        let Some(client) = self.clients.get_mut(&cid) else { return };
        let sid = client.session;
        self.dirty = true;

        if client.overlay.is_some() {
            self.overlay_key(cid, key);
            return;
        }

        if let Some(prefix) = client.prefix.take() {
            let repeating = prefix.repeat_until.is_some();
            let expired = prefix.repeat_until.is_some_and(|t| now >= t);
            if !expired {
                // Escape cancels a pending prefix. During a repeat there is
                // nothing pending, so it belongs to the program in the pane.
                if key.code == KeyCode::Esc && key.mods == 0 && !repeating {
                    return;
                }
                if key == self.config.prefix && !repeating {
                    self.run_action(Some(cid), sid, Action::SendPrefix);
                    return;
                }
                let bound = keys::lookup(&self.config.prefix_table, key).cloned();
                // While repeating, only repeatable actions count; anything
                // else is ordinary typing again (so `prefix H H x` resizes
                // twice and then types an x rather than closing the pane).
                match bound.filter(|a| !repeating || a.repeatable()) {
                    Some(action) => {
                        let repeat = action.repeatable();
                        self.run_action(Some(cid), sid, action);
                        if repeat {
                            let until = Instant::now() + self.config.repeat_time;
                            if let Some(c) = self.clients.get_mut(&cid) {
                                c.prefix = Some(Prefix { since: now, repeat_until: Some(until), shown: false });
                            }
                        }
                        return;
                    }
                    None if !repeating => {
                        self.message(Some(cid), format!("{} is not bound; ? lists keys", keys::key_label(key)));
                        return;
                    }
                    // A key outside the repeat set ends the repeat and is
                    // handled like any other key.
                    None => {}
                }
            }
        }

        if key == self.config.prefix {
            if let Some(c) = self.clients.get_mut(&cid) {
                c.prefix = Some(Prefix { since: now, repeat_until: None, shown: false });
            }
            return;
        }

        if let Some(action) = keys::lookup(&self.config.direct_table, key).cloned() {
            self.run_action(Some(cid), sid, action);
            return;
        }

        // Programs in panes have no notion of Super; an unbound Super chord
        // must not arrive there as a plain letter.
        if key.mods & SUPER != 0 {
            return;
        }
        let Some(active) = self.active_pane(sid) else { return };
        if self.panes.get(&active).is_some_and(|p| p.in_copy_mode()) {
            self.copy_key(cid, active, key);
            return;
        }
        for pid in self.input_targets(sid) {
            if let Some(p) = self.panes.get_mut(&pid) {
                if !p.in_copy_mode() {
                    p.send_key(key);
                }
            }
        }
    }

    fn on_paste(&mut self, cid: ClientId, text: String) {
        let Some(client) = self.clients.get_mut(&cid) else { return };
        let sid = client.session;
        self.dirty = true;
        match client.overlay.as_mut() {
            Some(Overlay::Prompt(p)) => {
                p.text.push_str(text.lines().next().unwrap_or(""));
                return;
            }
            Some(Overlay::Picker(p)) => {
                // In the session manager a paste starts a filter, so it shows.
                p.filtering = true;
                p.query.push_str(text.lines().next().unwrap_or(""));
                p.refilter();
                return;
            }
            Some(_) => return,
            None => {}
        }
        for pid in self.input_targets(sid) {
            if let Some(p) = self.panes.get_mut(&pid) {
                if !p.in_copy_mode() {
                    p.send_paste(&text);
                }
            }
        }
    }

    /// Keys while a pane is in copy mode. They follow ordinary editors:
    /// arrows and Home/End/PgUp/PgDn move, Shift selects, Ctrl+C copies.
    fn copy_key(&mut self, cid: ClientId, pid: PaneId, key: Key) {
        use KeyCode::*;
        let Some(pane) = self.panes.get_mut(&pid) else { return };
        if key.mods & (ALT | SUPER) != 0 {
            return;
        }
        let page = pane.rows.max(2) as i32;
        let shift = key.mods & SHIFT != 0;
        let ctrl = key.mods & CTRL != 0;

        // Ctrl+Up/Down scroll the view without moving the cursor.
        if ctrl && !shift && matches!(key.code, Up | Down) {
            pane.scroll(if key.code == Up { 1 } else { -1 });
            return;
        }
        enum Move {
            Step(ViMotion),
            Page(i32),
            Top,
            Bottom,
        }
        let movement = match key.code {
            Left if ctrl => Some(Move::Step(ViMotion::SemanticLeft)),
            Right if ctrl => Some(Move::Step(ViMotion::SemanticRight)),
            Left => Some(Move::Step(ViMotion::Left)),
            Right => Some(Move::Step(ViMotion::Right)),
            Up => Some(Move::Step(ViMotion::Up)),
            Down => Some(Move::Step(ViMotion::Down)),
            Home if ctrl => Some(Move::Top),
            End if ctrl => Some(Move::Bottom),
            Home => Some(Move::Step(ViMotion::First)),
            End => Some(Move::Step(ViMotion::Last)),
            PageUp => Some(Move::Page(page)),
            PageDown => Some(Move::Page(-page)),
            _ => None,
        };
        if let Some(movement) = movement {
            // As in any editor: moving with Shift held selects, moving
            // without it lets go of the selection.
            if shift {
                pane.begin_selection();
            } else {
                pane.clear_selection();
            }
            match movement {
                Move::Step(m) => pane.motion(m),
                Move::Page(lines) => pane.copy_scroll(lines),
                Move::Top => pane.copy_goto_top(),
                Move::Bottom => pane.copy_goto_bottom(),
            }
            return;
        }

        match key.code {
            Esc => {
                if pane.has_selection() {
                    pane.clear_selection();
                } else {
                    pane.exit_copy();
                }
            }
            Enter | Char('c') if key.code == Enter || ctrl => {
                let text = pane.selection_text();
                pane.exit_copy();
                if let Some(t) = text {
                    self.set_clipboard(t, Some(cid));
                }
            }
            Char('a') if ctrl => pane.select_all(),
            Char('l') if ctrl => pane.toggle_selection(SelectionType::Lines),
            Char('r') if ctrl => pane.toggle_selection(SelectionType::Block),
            Char('f') if ctrl => {
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.overlay = Some(Overlay::Prompt(Prompt {
                        title: "Find in scrollback".into(),
                        text: String::new(),
                        kind: PromptKind::Search { pane: pid, forward: false },
                    }));
                }
            }
            F(3) | Char('g') if key.code == F(3) || ctrl => {
                // F3 continues towards older output, Shift+F3 comes back.
                let searching = pane.copy.as_ref().is_some_and(|c| c.search.is_some());
                if !searching {
                    self.message(Some(cid), "nothing to find yet; press Ctrl+f first");
                } else if !pane.search_next(shift) {
                    self.message(Some(cid), "no more matches");
                }
            }
            _ => {}
        }
    }

    fn overlay_key(&mut self, cid: ClientId, key: Key) {
        use KeyCode::*;
        let Some(client) = self.clients.get_mut(&cid) else { return };
        let Some(mut overlay) = client.overlay.take() else { return };
        let sid = client.session;
        let ctrl = key.mods == CTRL;
        let plain = key.mods == 0;
        let cancel = (key.code == Esc && plain) || (ctrl && matches!(key.code, Char('c') | Char('g')));
        let mut keep = true;
        let mut chosen: Option<Target> = None;
        let mut submitted: Option<(PromptKind, String)> = None;
        let mut confirmed: Option<Pending> = None;
        let mut pane_number: Option<usize> = None;

        let mut arrange: Option<Action> = None;
        let mut replay = false;
        // A session manager command: the key and the selected session.
        let mut manage: Option<(char, Option<SessionId>)> = None;
        // Saved sessions to bring back, or the restore picker was dismissed.
        let mut restore: Option<Vec<usize>> = None;
        let mut restore_dismissed = false;

        match &mut overlay {
            Overlay::Help { scroll } => match key.code {
                _ if cancel => keep = false,
                Char('q') | Enter => keep = false,
                Down => *scroll = scroll.saturating_add(1),
                Up => *scroll = scroll.saturating_sub(1),
                Char(' ') | PageDown => *scroll = scroll.saturating_add(15),
                PageUp => *scroll = scroll.saturating_sub(15),
                Home => *scroll = 0,
                End => *scroll = u16::MAX,
                _ => {}
            },
            Overlay::Picker(p) if p.manage && p.filtering && cancel => {
                p.filtering = false;
                p.query.clear();
                p.refilter();
            }
            Overlay::Picker(p) if p.manage && !p.filtering && matches!(key.code, Char(_)) && plain => {
                let Char(ch) = key.code else { unreachable!() };
                let selected = match p.current().map(|i| &i.target) {
                    Some(Target::Session(s)) => Some(*s),
                    _ => None,
                };
                match ch {
                    '/' => p.filtering = true,
                    'j' => p.move_by(1),
                    'k' => p.move_by(-1),
                    'q' => keep = false,
                    'n' | 'r' | 'x' => {
                        manage = Some((ch, selected));
                        keep = false;
                    }
                    _ => {}
                }
            }
            Overlay::Picker(p) => match key.code {
                _ if cancel => keep = false,
                Enter => {
                    chosen = p.current().map(|i| i.target.clone());
                    keep = false;
                }
                Up | BackTab => p.move_by(-1),
                Down | Tab => p.move_by(1),
                PageUp => p.move_by(-10),
                PageDown => p.move_by(10),
                _ => {
                    if edit_text(&mut p.query, key) {
                        p.refilter();
                    }
                }
            },
            Overlay::Prompt(p) => match key.code {
                _ if cancel => keep = false,
                Enter => {
                    submitted = Some((p.kind.clone(), p.text.clone()));
                    keep = false;
                }
                _ => {
                    edit_text(&mut p.text, key);
                }
            },
            Overlay::Settings(panel) => keep = self.settings_key(cid, panel, key),
            Overlay::Confirm(c) => {
                if matches!(key.code, Char('y') | Char('Y') | Enter) && plain {
                    confirmed = Some(c.then.clone());
                }
                keep = false;
            }
            Overlay::Restore(r) => match key.code {
                _ if cancel => {
                    keep = false;
                    restore_dismissed = true;
                }
                Up => r.selected = r.selected.saturating_sub(1),
                Down => r.selected = (r.selected + 1).min(r.items.len().saturating_sub(1)),
                Char(' ') if plain => {
                    if let Some(item) = r.items.get_mut(r.selected) {
                        item.checked = !item.checked;
                    }
                }
                Enter => {
                    restore = Some(r.items.iter().filter(|i| i.checked).map(|i| i.index).collect());
                    keep = false;
                }
                _ => {}
            },
            Overlay::PaneNumbers => {
                if let Char(d @ '0'..='9') = key.code {
                    pane_number = Some(d as usize - '0' as usize);
                }
                keep = false;
            }
            Overlay::Arrange => {
                let dir = match key.code {
                    Left => Some(Direction::Left),
                    Right => Some(Direction::Right),
                    Up => Some(Direction::Up),
                    Down => Some(Direction::Down),
                    _ => None,
                };
                let sideways = matches!(dir, Some(Direction::Left | Direction::Right));
                match (dir, key.mods) {
                    (Some(d), 0) => arrange = Some(Action::Resize(d, if sideways { 2 } else { 1 })),
                    (Some(d), SHIFT) => arrange = Some(Action::Resize(d, if sideways { 8 } else { 4 })),
                    (Some(d), CTRL) => arrange = Some(Action::Swap(d)),
                    (None, 0) if matches!(key.code, Enter | Esc) => keep = false,
                    // Anything else ends the mode and is then handled as usual.
                    _ => {
                        keep = false;
                        replay = true;
                    }
                }
            }
        }
        if keep {
            if let Some(c) = self.clients.get_mut(&cid) {
                c.overlay = Some(overlay);
            }
            if let Some(action) = arrange {
                self.run_action(Some(cid), sid, action);
            }
            return;
        }
        if replay {
            self.on_key(cid, key);
            return;
        }

        if let Some(target) = chosen {
            match target {
                Target::Session(s) => self.switch_client(cid, s),
                Target::Window(s, w) => {
                    self.switch_client(cid, s);
                    if let Some(session) = self.sessions.iter_mut().find(|x| x.id == s) {
                        session.select(w);
                    }
                }
                Target::Pane(s, w, p) => {
                    self.switch_client(cid, s);
                    if let Some(session) = self.sessions.iter_mut().find(|x| x.id == s) {
                        session.select(w);
                    }
                    self.focus_pane(s, p);
                }
                Target::Action(a) => self.run_action(Some(cid), sid, a),
            }
        }
        if let Some((kind, text)) = submitted {
            self.prompt_done(cid, sid, kind, text);
        }
        if let Some(pending) = confirmed {
            match pending {
                Pending::KillPane(p) => self.close_pane(p),
                Pending::KillWindow(s, w) => self.remove_window(s, w),
                Pending::KillSession(s) => self.close_session(cid, s),
            }
        }
        if let Some(n) = pane_number {
            let panes =
                self.sessions.iter().find(|s| s.id == sid).map(|s| s.window().layout.panes()).unwrap_or_default();
            let index = if n == 0 { 9 } else { n - 1 };
            if let Some(pid) = panes.get(index) {
                self.focus_pane(sid, *pid);
            }
        }
        if let Some((command, target)) = manage {
            self.session_command(cid, sid, command, target);
        }
        match restore {
            Some(picks) if !picks.is_empty() => self.restore_sessions(cid, picks),
            Some(_) => self.restore_dismissed(),
            None if restore_dismissed => self.restore_dismissed(),
            None => {}
        }
        // A prompt or confirmation that came from the session manager has
        // closed: go back to the manager.
        let Some(c) = self.clients.get_mut(&cid) else { return };
        if let (Some(selected), None) = (c.back_to_sessions, &c.overlay) {
            c.back_to_sessions = None;
            let sid = c.session;
            self.run_action(Some(cid), sid, Action::Sessions);
            if let Some(Overlay::Picker(p)) = self.clients.get_mut(&cid).and_then(|c| c.overlay.as_mut()) {
                if let Some(i) = p.items.iter().position(|i| i.target == Target::Session(selected)) {
                    p.selected = i;
                }
            }
        }
    }

    /// `n`, `r` or `x` pressed in the session manager.
    fn session_command(&mut self, cid: ClientId, sid: SessionId, command: char, target: Option<SessionId>) {
        let name =
            |server: &Server| target.and_then(|t| server.sessions.iter().find(|s| s.id == t)).map(|s| s.name.clone());
        let overlay = match (command, target) {
            // The new session takes over the client, so there is nothing to go back to.
            ('n', _) => {
                self.run_action(Some(cid), sid, Action::NewSession);
                return;
            }
            ('r', Some(t)) => Overlay::Prompt(Prompt {
                title: "Rename session".into(),
                text: name(self).unwrap_or_default(),
                kind: PromptKind::RenameSession(t),
            }),
            ('x', Some(t)) => Overlay::Confirm(Confirm {
                text: format!("Kill session {} and everything running in it?", name(self).unwrap_or_default()),
                then: Pending::KillSession(t),
            }),
            _ => return,
        };
        if let Some(c) = self.clients.get_mut(&cid) {
            c.overlay = Some(overlay);
            c.back_to_sessions = target;
        }
    }

    fn prompt_done(&mut self, cid: ClientId, sid: SessionId, kind: PromptKind, text: String) {
        let text = text.trim().to_string();
        match kind {
            PromptKind::RenameWindow => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == sid) {
                    // An empty name goes back to following the running program.
                    s.window_mut().name = (!text.is_empty()).then_some(text);
                }
            }
            PromptKind::RenameSession(target) => {
                if text.is_empty() {
                    return;
                }
                if !valid_name(&text) {
                    self.message(Some(cid), "that name is too long");
                } else if self.sessions.iter().any(|s| s.name == text && s.id != target) {
                    self.message(Some(cid), format!("a session named {text:?} already exists"));
                } else if let Some(s) = self.sessions.iter_mut().find(|s| s.id == target) {
                    s.name = text;
                }
            }
            PromptKind::NewSession => {
                let Some(current) = self.sessions.iter().find(|s| s.id == sid) else { return };
                let env = current.env.clone();
                let cwd = self
                    .panes
                    .get(&current.window().active)
                    .and_then(|p| p.cwd())
                    .unwrap_or_else(|| current.cwd.clone());
                let (cols, rows) = (current.cols, current.rows);
                let name = (!text.is_empty()).then_some(text);
                match self.create_session(name, Vec::new(), cwd.to_string_lossy().into_owned(), env, cols, rows) {
                    Ok(new) => self.switch_client(cid, new),
                    Err(e) => self.message(Some(cid), e),
                }
            }
            PromptKind::Search { pane, forward } => {
                if text.is_empty() {
                    return;
                }
                let found = self.panes.get_mut(&pane).map(|p| p.search(&text, forward));
                if found == Some(false) {
                    self.message(Some(cid), format!("no match for {text:?}"));
                }
            }
        }
    }

    fn set_drag(&mut self, cid: ClientId, drag: Option<Drag>) {
        if let Some(c) = self.clients.get_mut(&cid) {
            c.drag = drag;
        }
    }

    fn on_mouse(&mut self, cid: ClientId, m: Mouse) {
        if !self.config.mouse {
            return;
        }
        let now = Instant::now();
        if !matches!(m.kind, MouseKind::Moved) {
            self.adopt_client_size(cid);
        }
        let status = self.config.status;
        let Some(client) = self.clients.get_mut(&cid) else { return };
        let sid = client.session;

        // Popups swallow the mouse; the wheel moves through pickers and help.
        if let Some(overlay) = client.overlay.as_mut() {
            match (overlay, m.kind) {
                (Overlay::Picker(p), MouseKind::ScrollUp) => p.move_by(-1),
                (Overlay::Picker(p), MouseKind::ScrollDown) => p.move_by(1),
                (Overlay::Help { scroll }, MouseKind::ScrollUp) => *scroll = scroll.saturating_sub(3),
                (Overlay::Help { scroll }, MouseKind::ScrollDown) => *scroll = scroll.saturating_add(3),
                _ => return,
            }
            self.dirty = true;
            return;
        }

        let status_row = match status {
            StatusPos::Top => Some(0),
            StatusPos::Bottom => Some(client.rows.saturating_sub(1)),
            StatusPos::Off => None,
        };
        let drag = client.drag.clone();
        let last_click = client.last_click;
        if drag.is_none() && status_row == Some(m.row) {
            let tab = client.tabs.iter().find(|(a, b, _)| m.col >= *a && m.col < *b).map(|t| t.2);
            match m.kind {
                MouseKind::Down(MouseButton::Left) => {
                    if let (Some(wid), Some(s)) = (tab, self.sessions.iter_mut().find(|s| s.id == sid)) {
                        s.select(wid);
                        self.dirty = true;
                    }
                }
                MouseKind::ScrollUp => self.run_action(Some(cid), sid, Action::PrevWindow),
                MouseKind::ScrollDown => self.run_action(Some(cid), sid, Action::NextWindow),
                _ => {}
            }
            return;
        }

        let Some(area) = self.session_area(sid) else { return };
        let Some(session) = self.sessions.iter().find(|s| s.id == sid) else { return };
        let geom = session.window().geometry(area, Pad::new(&self.config));
        let local = |rect: Rect| {
            let col = m.col.clamp(rect.x, rect.x + rect.width.saturating_sub(1)) - rect.x;
            let row = m.row.clamp(rect.y, rect.y + rect.height.saturating_sub(1)) - rect.y;
            (col, row)
        };

        match drag {
            Some(Drag::Border { path, area: split_area, axis }) => {
                match m.kind {
                    MouseKind::Drag(_) => {
                        let cells = match axis {
                            Axis::Horizontal => m.col as i32 - split_area.x as i32,
                            Axis::Vertical => m.row as i32 - split_area.y as i32,
                        };
                        if let Some(session) = self.sessions.iter_mut().find(|s| s.id == sid) {
                            let window = session.window_mut();
                            window.layout.set_split(&path, split_area, cells);
                            window.preset = None;
                        }
                        self.relayout(sid);
                    }
                    MouseKind::Up(_) => self.set_drag(cid, None),
                    _ => {}
                }
                return;
            }
            Some(Drag::Select { pane, start, ty }) => {
                match m.kind {
                    MouseKind::Drag(_) => {
                        if let (Some(p), Some(rect)) = (self.panes.get_mut(&pane), geom.rect_of(pane)) {
                            let (col, row) = local(rect);
                            let end = p.point_at(col, row);
                            if end != start || ty != SelectionType::Simple {
                                p.select_range(ty, start, end);
                            } else {
                                p.clear_selection();
                            }
                            self.dirty = true;
                        }
                    }
                    MouseKind::Up(_) => {
                        self.set_drag(cid, None);
                        if let Some(text) = self.panes.get(&pane).and_then(|p| p.selection_text()) {
                            self.set_clipboard(text, Some(cid));
                        }
                    }
                    _ => {}
                }
                return;
            }
            Some(Drag::Forward { pane }) => {
                if matches!(m.kind, MouseKind::Up(_)) {
                    self.set_drag(cid, None);
                }
                if let (Some(p), Some(rect)) = (self.panes.get_mut(&pane), geom.rect_of(pane)) {
                    let (col, row) = local(rect);
                    p.send_mouse(m, col, row);
                }
                return;
            }
            None => {}
        }

        match m.kind {
            MouseKind::Down(button) => {
                if let Some(sep) = geom.sep_at(m.col, m.row) {
                    if button == MouseButton::Left {
                        let drag = Drag::Border { path: sep.path.clone(), area: sep.area, axis: sep.axis };
                        self.set_drag(cid, Some(drag));
                    }
                    return;
                }
                let Some((pid, rect)) = geom.pane_at(m.col, m.row) else { return };
                let (col, row) = local(rect);
                let clicks = match last_click {
                    Some((at, c, r, n)) if now.duration_since(at) < DOUBLE_CLICK && c == m.col && r == m.row => {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                self.focus_pane(sid, pid);
                let paste = self.paste_buffer.clone();
                let Some(pane) = self.panes.get_mut(&pid) else { return };
                let Some(client) = self.clients.get_mut(&cid) else { return };
                self.dirty = true;
                if pane.wants_mouse() {
                    pane.send_mouse(m, col, row);
                    client.drag = Some(Drag::Forward { pane: pid });
                    return;
                }
                match button {
                    MouseButton::Left => {
                        client.last_click = Some((now, m.col, m.row, clicks));
                        let ty = match clicks {
                            2 => SelectionType::Semantic,
                            3 => SelectionType::Lines,
                            _ => SelectionType::Simple,
                        };
                        let start = pane.point_at(col, row);
                        pane.clear_selection();
                        if ty != SelectionType::Simple {
                            pane.select_range(ty, start, start);
                        }
                        client.drag = Some(Drag::Select { pane: pid, start, ty });
                    }
                    MouseButton::Middle => {
                        if !pane.in_copy_mode() && !paste.is_empty() {
                            pane.send_paste(&paste);
                        }
                    }
                    MouseButton::Right => {}
                }
            }
            MouseKind::ScrollUp | MouseKind::ScrollDown => {
                let Some((pid, rect)) = geom.pane_at(m.col, m.row) else { return };
                let (col, row) = local(rect);
                let Some(pane) = self.panes.get_mut(&pid) else { return };
                let up = m.kind == MouseKind::ScrollUp;
                let mode = pane.mode();
                if pane.wants_mouse() {
                    pane.send_mouse(m, col, row);
                } else if !pane.in_copy_mode() && mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
                    // Full-screen programs without mouse support get arrow
                    // keys, so the wheel still scrolls pagers and editors.
                    let key = Key::plain(if up { KeyCode::Up } else { KeyCode::Down });
                    for _ in 0..3 {
                        pane.write(input::encode_key(key, mode));
                    }
                } else {
                    pane.scroll(if up { 3 } else { -3 });
                }
                self.dirty = true;
            }
            MouseKind::Moved => {
                if let Some((pid, rect)) = geom.pane_at(m.col, m.row) {
                    if let Some(pane) = self.panes.get_mut(&pid) {
                        if pane.wants_mouse() && pane.mode().contains(TermMode::MOUSE_MOTION) {
                            pane.send_mouse(m, m.col - rect.x, m.row - rect.y);
                        }
                    }
                }
            }
            MouseKind::Up(_) | MouseKind::Drag(_) => {}
        }
    }

    // ===== actions ==========================================================

    pub fn run_action(&mut self, cid: Option<ClientId>, sid: SessionId, action: Action) {
        self.dirty = true;
        let Some(sidx) = self.session_index(sid) else { return };
        let Some(area) = self.session_area(sid) else { return };
        let active = self.sessions[sidx].window().active;
        let wid = self.sessions[sidx].active;
        let set_overlay = |server: &mut Server, overlay: Overlay| {
            if let Some(c) = cid.and_then(|id| server.clients.get_mut(&id)) {
                c.overlay = Some(overlay);
                c.back_to_sessions = None;
            }
        };

        match action {
            Action::None => {}

            // ----- windows -----
            Action::NewWindow => {
                let cwd = if self.config.inherit_cwd { self.panes.get(&active).and_then(|p| p.cwd()) } else { None };
                if let Err(e) = self.new_window(sid, None, cwd) {
                    self.message(cid, e);
                }
            }
            Action::NextWindow | Action::PrevWindow => {
                let s = &mut self.sessions[sidx];
                let n = s.windows.len();
                let i = s.active_index();
                let next = if action == Action::NextWindow { (i + 1) % n } else { (i + n - 1) % n };
                let wid = s.windows[next].id;
                s.select(wid);
            }
            Action::LastWindow => {
                let s = &mut self.sessions[sidx];
                match s.last.filter(|l| s.windows.iter().any(|w| w.id == *l)) {
                    Some(l) => s.select(l),
                    None => self.message(cid, "no previous window"),
                }
            }
            Action::SelectWindow(n) => {
                let s = &mut self.sessions[sidx];
                let index = if n == 0 { 9 } else { n - 1 };
                match s.windows.get(index).map(|w| w.id) {
                    Some(wid) => s.select(wid),
                    None => self.message(cid, format!("no window {n}")),
                }
            }
            Action::KillWindow => {
                if cid.is_some() {
                    let title = window_title(self.sessions[sidx].window(), &self.panes);
                    let index = self.sessions[sidx].active_index() + 1;
                    set_overlay(
                        self,
                        Overlay::Confirm(Confirm {
                            text: format!("Close window {index} ({title})?"),
                            then: Pending::KillWindow(sid, wid),
                        }),
                    );
                } else {
                    self.remove_window(sid, wid);
                }
            }
            Action::RenameWindow => {
                let text = window_title(self.sessions[sidx].window(), &self.panes);
                set_overlay(
                    self,
                    Overlay::Prompt(Prompt { title: "Rename window".into(), text, kind: PromptKind::RenameWindow }),
                );
            }
            Action::MoveWindowLeft | Action::MoveWindowRight => {
                let s = &mut self.sessions[sidx];
                let n = s.windows.len();
                let i = s.active_index();
                let j = if action == Action::MoveWindowLeft { (i + n - 1) % n } else { (i + 1) % n };
                s.windows.swap(i, j);
            }

            // ----- panes -----
            Action::SplitRight => self.split(cid, sid, Some(Axis::Horizontal)),
            Action::SplitDown => self.split(cid, sid, Some(Axis::Vertical)),
            Action::SplitAuto => self.split(cid, sid, None),
            Action::KillPane => {
                if cid.is_some() {
                    let name = self.panes.get(&active).map(|p| p.name.clone()).unwrap_or_default();
                    set_overlay(
                        self,
                        Overlay::Confirm(Confirm {
                            text: format!("Close this pane ({name})?"),
                            then: Pending::KillPane(active),
                        }),
                    );
                } else {
                    self.close_pane(active);
                }
            }
            Action::Zoom => {
                let w = self.sessions[sidx].window_mut();
                if w.layout.count() > 1 {
                    w.zoomed = !w.zoomed;
                    self.relayout(sid);
                }
            }
            Action::Focus(dir) => {
                let w = self.sessions[sidx].window();
                let area = w.layout_area(area, Pad::new(&self.config));
                let geom = w.layout.geometry(area);
                let panes = &self.panes;
                let rank = |id: PaneId| panes.get(&id).map(|p| p.last_focus).unwrap_or(0);
                if let Some(next) = layout::neighbor(&geom.panes, area, active, dir, rank) {
                    self.focus_pane(sid, next);
                }
            }
            Action::NextPane | Action::PrevPane => {
                let panes = self.sessions[sidx].window().layout.panes();
                if let Some(i) = panes.iter().position(|p| *p == active) {
                    let n = panes.len();
                    let next = if action == Action::NextPane { (i + 1) % n } else { (i + n - 1) % n };
                    self.focus_pane(sid, panes[next]);
                }
            }
            Action::LastPane => {
                let w = self.sessions[sidx].window();
                match w.last.filter(|l| w.layout.contains(*l)) {
                    Some(l) => self.focus_pane(sid, l),
                    None => self.message(cid, "no previous pane"),
                }
            }
            Action::SwapNext | Action::SwapPrev => {
                let w = self.sessions[sidx].window_mut();
                let panes = w.layout.panes();
                if let Some(i) = panes.iter().position(|p| *p == active) {
                    let n = panes.len();
                    let other = if action == Action::SwapNext { (i + 1) % n } else { (i + n - 1) % n };
                    if panes[other] != active {
                        w.layout.swap(active, panes[other]);
                        w.zoomed = false;
                        self.relayout(sid);
                    }
                }
            }
            Action::Swap(dir) => {
                let w = self.sessions[sidx].window_mut();
                let area = w.layout_area(area, Pad::new(&self.config));
                let geom = w.layout.geometry(area);
                let panes = &self.panes;
                let rank = |id: PaneId| panes.get(&id).map(|p| p.last_focus).unwrap_or(0);
                if let Some(other) = layout::neighbor(&geom.panes, area, active, dir, rank) {
                    // The focused pane keeps the focus and takes the other's place.
                    w.layout.swap(active, other);
                    w.zoomed = false;
                    w.preset = None;
                    self.relayout(sid);
                }
            }
            Action::Arrange => {
                if self.sessions[sidx].window().layout.count() < 2 {
                    self.message(cid, "only one pane here; split first (Alt+Enter)");
                } else {
                    let w = self.sessions[sidx].window_mut();
                    if w.zoomed {
                        w.zoomed = false;
                        self.relayout(sid);
                    }
                    set_overlay(self, Overlay::Arrange);
                }
            }
            Action::Resize(dir, n) => {
                let w = self.sessions[sidx].window_mut();
                let area = w.layout_area(area, Pad::new(&self.config));
                if !w.zoomed && w.layout.resize(area, active, dir, n) {
                    w.preset = None;
                    self.relayout(sid);
                }
            }
            Action::NextLayout => {
                let w = self.sessions[sidx].window_mut();
                let panes = w.layout.panes();
                if panes.len() > 1 {
                    let kind = w.preset.map(|p| p.next()).unwrap_or(Preset::EvenHorizontal);
                    w.layout = Node::preset(kind, &panes);
                    w.preset = Some(kind);
                    w.zoomed = false;
                    self.relayout(sid);
                    self.message(cid, format!("layout: {}", kind.name()));
                }
            }
            Action::BreakPane => {
                let s = &mut self.sessions[sidx];
                if s.window().layout.count() < 2 {
                    self.message(cid, "this is the only pane in the window");
                    return;
                }
                let w = s.window_mut();
                w.layout.remove(active);
                w.zoomed = false;
                w.last = None;
                w.active = w.layout.panes()[0];
                let new_wid = self.id();
                let s = &mut self.sessions[sidx];
                s.windows.push(Window {
                    id: new_wid,
                    name: None,
                    layout: Node::Leaf(active),
                    active,
                    last: None,
                    zoomed: false,
                    sync: false,
                    preset: None,
                    bell: false,
                });
                s.select(new_wid);
                self.relayout(sid);
            }
            Action::DisplayPanes => set_overlay(self, Overlay::PaneNumbers),
            Action::ToggleSync => {
                let w = self.sessions[sidx].window_mut();
                w.sync = !w.sync;
                let state = if w.sync { "typing goes to all panes" } else { "typing goes to this pane only" };
                self.message(cid, state);
            }
            Action::ClearHistory => {
                if let Some(p) = self.panes.get_mut(&active) {
                    p.clear_history();
                }
            }

            // ----- sessions -----
            Action::NewSession => set_overlay(
                self,
                Overlay::Prompt(Prompt {
                    title: "New session name (empty for a number)".into(),
                    text: String::new(),
                    kind: PromptKind::NewSession,
                }),
            ),
            Action::NextSession | Action::PrevSession => {
                let n = self.sessions.len();
                let next = if action == Action::NextSession { (sidx + 1) % n } else { (sidx + n - 1) % n };
                let target = self.sessions[next].id;
                if let Some(cid) = cid {
                    self.switch_client(cid, target);
                }
            }
            Action::LastSession => {
                let last = cid.and_then(|id| self.clients.get(&id)).and_then(|c| c.last_session);
                match (cid, last) {
                    (Some(cid), Some(last)) => self.switch_client(cid, last),
                    _ => self.message(cid, "no previous session"),
                }
            }
            Action::RenameSession => {
                let text = self.sessions[sidx].name.clone();
                set_overlay(
                    self,
                    Overlay::Prompt(Prompt {
                        title: "Rename session".into(),
                        text,
                        kind: PromptKind::RenameSession(sid),
                    }),
                );
            }
            Action::KillSession => {
                if cid.is_some() {
                    let name = self.sessions[sidx].name.clone();
                    set_overlay(
                        self,
                        Overlay::Confirm(Confirm {
                            text: format!("Kill session {name} and everything running in it?"),
                            then: Pending::KillSession(sid),
                        }),
                    );
                } else {
                    self.remove_session(sid);
                }
            }
            Action::Detach => {
                if let Some(cid) = cid {
                    let name = self.sessions[sidx].name.clone();
                    self.drop_client(cid, format!("[detached from session {name}]"));
                }
            }
            Action::Switcher => {
                let picker = self.build_switcher(sid);
                set_overlay(self, Overlay::Picker(picker));
            }
            Action::Sessions => {
                let picker = self.build_sessions(sid);
                set_overlay(self, Overlay::Picker(picker));
            }
            Action::RestoreSessions => {
                if let Some(cid) = cid {
                    self.offer_restore(cid, true);
                }
            }

            // ----- other -----
            Action::CopyMode | Action::CopyModePageUp => {
                if let Some(p) = self.panes.get_mut(&active) {
                    if action == Action::CopyMode && p.in_copy_mode() {
                        p.exit_copy();
                        return;
                    }
                    let entering = !p.in_copy_mode();
                    p.enter_copy();
                    if action == Action::CopyModePageUp {
                        let page = p.rows.max(2) as i32;
                        p.copy_scroll(page);
                    }
                    if entering {
                        if let Some(c) = cid.and_then(|id| self.clients.get_mut(&id)) {
                            let hint = "arrows move · Shift selects · Ctrl+c copies · Ctrl+f finds · Esc leaves";
                            c.message = Some((hint.into(), Instant::now() + MESSAGE_TIME * 2));
                        }
                    }
                }
            }
            Action::Paste => {
                if self.paste_buffer.is_empty() {
                    self.message(cid, "nothing copied yet");
                    return;
                }
                let text = self.paste_buffer.clone();
                for pid in self.input_targets(sid) {
                    if let Some(p) = self.panes.get_mut(&pid) {
                        if !p.in_copy_mode() {
                            p.send_paste(&text);
                        }
                    }
                }
            }
            Action::Palette => {
                let picker = self.build_palette();
                set_overlay(self, Overlay::Picker(picker));
            }
            Action::Help => set_overlay(self, Overlay::Help { scroll: 0 }),
            Action::SendPrefix => {
                let key = self.config.prefix;
                for pid in self.input_targets(sid) {
                    if let Some(p) = self.panes.get_mut(&pid) {
                        p.send_key(key);
                    }
                }
            }
            Action::ReloadConfig => {
                let text = self.reload_config().unwrap_or_else(|| "config reloaded".to_string());
                self.message(cid, text);
            }
            Action::Settings => set_overlay(self, Overlay::Settings(settings::Panel::open())),
        }
    }

    /// The default leader, Alt+Space, unless GNOME holds it on the desktop
    /// the last client attached from. A leader from the config always wins.
    fn pick_leader(&mut self) {
        if !self.config.prefix_chosen {
            self.config.prefix = if self.gnome_took_leader { GNOME_LEADER } else { Key::alt(' ') };
        }
    }

    /// Read the config file again and apply it everywhere. Returns the first
    /// problem found in it.
    fn reload_config(&mut self) -> Option<String> {
        let had_mouse = self.config.mouse;
        self.config = Config::load();
        self.pick_leader();
        let mouse = self.config.mouse;
        let colors = self.terminal_colors();
        for c in self.clients.values_mut() {
            c.full_redraw = true;
            c.pending.extend_from_slice(&colors);
            if mouse != had_mouse {
                c.pending.extend_from_slice(if mouse { MOUSE_ON } else { MOUSE_OFF });
            }
        }
        let sids: Vec<SessionId> = self.sessions.iter().map(|s| s.id).collect();
        for s in sids {
            self.relayout(s);
        }
        self.config.warnings.first().cloned()
    }

    /// A key in the settings panel. Returns false when the panel closes.
    fn settings_key(&mut self, cid: ClientId, panel: &mut settings::Panel, key: Key) -> bool {
        use settings::{Kind, Mode, RowId, SETTINGS};
        use KeyCode::*;
        let plain = key.mods == 0;
        let cancel = (key.code == Esc && plain) || (key.mods == CTRL && matches!(key.code, Char('c') | Char('g')));
        let rows = panel.rows(&self.config);
        panel.selected = panel.selected.min(rows.len().saturating_sub(1));
        let id = rows.get(panel.selected).map(|r| r.id.clone());
        let setting = match &id {
            Some(RowId::Setting(i)) => Some(&SETTINGS[*i]),
            _ => None,
        };
        // (what to write, message on success)
        let mut saved: Option<(Result<(), String>, String)> = None;

        match panel.mode.clone() {
            Mode::Capture => {
                panel.mode = Mode::Browse;
                if cancel {
                    return true;
                }
                match (&id, setting) {
                    (_, Some(set)) if set.kind == Kind::Leader => {
                        if key.mods & (CTRL | ALT | SUPER) == 0 {
                            self.message(Some(cid), "the leader needs Ctrl, Alt or Super, or typing would trigger it");
                            return true;
                        }
                        let spec = settings::key_spec(key);
                        saved = Some((
                            settings::write("prefix", Some(toml_edit::value(spec))),
                            format!("leader is now {}", keys::key_label(key)),
                        ));
                    }
                    (Some(RowId::Binding(action)), _) => {
                        if key == self.config.prefix {
                            self.message(Some(cid), "that is the leader key itself");
                            return true;
                        }
                        let result = settings::bind(&self.config, action, key);
                        let text = match &result {
                            Ok((chord, before)) => {
                                let shown = if *chord {
                                    keys::key_label(key)
                                } else {
                                    format!("{} {}", keys::key_label(self.config.prefix), keys::key_label(key))
                                };
                                let was = before.as_ref().map(|a| format!(" (it was: {})", a.describe()));
                                format!("{shown}: {}{}", action.describe(), was.unwrap_or_default())
                            }
                            Err(_) => String::new(),
                        };
                        saved = Some((result.map(|_| ()), text));
                    }
                    _ => {}
                }
            }
            Mode::Edit(mut text) => {
                if cancel {
                    panel.mode = Mode::Browse;
                } else if key.code == Enter && plain {
                    if let Some(set) = setting {
                        let value = if text.trim().is_empty() && set.kind == Kind::Text {
                            Ok(None)
                        } else {
                            settings::parse_input(set, &text).map(Some)
                        };
                        match value {
                            Ok(v) => {
                                panel.mode = Mode::Browse;
                                saved = Some((settings::write(set.key, v), format!("{} saved", set.key)));
                            }
                            Err(e) => self.message(Some(cid), e),
                        }
                    }
                } else {
                    edit_text(&mut text, key);
                    panel.mode = Mode::Edit(text);
                }
            }
            Mode::Browse => match key.code {
                _ if cancel => {
                    if panel.query.is_empty() {
                        return false;
                    }
                    panel.query.clear();
                    panel.selected = 0;
                }
                Up => panel.move_by(-1, rows.len()),
                Down => panel.move_by(1, rows.len()),
                PageUp => panel.move_by(-10, rows.len()),
                PageDown => panel.move_by(10, rows.len()),
                Home => panel.selected = 0,
                End => panel.selected = rows.len().saturating_sub(1),
                Delete => match (&id, setting) {
                    (_, Some(set)) => saved = Some((settings::write(set.key, None), format!("{} reset", set.key))),
                    (Some(RowId::Binding(a)), _) => {
                        saved = Some((settings::reset_binding(a), format!("{}: default keys", a.describe())))
                    }
                    _ => {}
                },
                Enter | Left | Right if key.mods & !SHIFT == 0 => {
                    let step: i64 = match key.code {
                        Left => -1,
                        Right => 1,
                        _ => 0,
                    };
                    match (&id, setting) {
                        (Some(RowId::Binding(_)), _) if step == 0 => panel.mode = Mode::Capture,
                        (_, Some(set)) => {
                            let (now, _) = settings::current(set, &self.config);
                            let value = match set.kind {
                                Kind::Bool => Some(toml_edit::value(now != "on")),
                                Kind::Choice(options) => {
                                    let i = options.iter().position(|o| *o == now).unwrap_or(0) as i64;
                                    let n = options.len() as i64;
                                    let next = (i + if step == 0 { 1 } else { step }).rem_euclid(n);
                                    Some(toml_edit::value(options[next as usize]))
                                }
                                Kind::Number { step: by, min, max, .. } if step != 0 => {
                                    let by = if key.mods & SHIFT != 0 { by * 10 } else { by };
                                    let n = (settings::number(set, &self.config) + step * by).clamp(min, max);
                                    Some(toml_edit::value(n))
                                }
                                Kind::Leader if step == 0 => {
                                    panel.mode = Mode::Capture;
                                    None
                                }
                                Kind::Number { .. } | Kind::Text | Kind::Color if step == 0 => {
                                    let start = match set.kind {
                                        Kind::Number { .. } => settings::number(set, &self.config).to_string(),
                                        _ => now,
                                    };
                                    panel.mode = Mode::Edit(start);
                                    None
                                }
                                _ => None,
                            };
                            if let Some(v) = value {
                                saved = Some((settings::write(set.key, Some(v)), format!("{} saved", set.key)));
                            }
                        }
                        _ => {}
                    }
                }
                _ => {
                    if edit_text(&mut panel.query, key) {
                        panel.select_best(&self.config);
                    }
                }
            },
        }

        if let Some((result, text)) = saved {
            match result {
                Ok(()) => {
                    let problem = self.reload_config();
                    panel.reread();
                    self.message(Some(cid), problem.unwrap_or(text));
                }
                Err(e) => self.message(Some(cid), e),
            }
        }
        true
    }

    fn build_switcher(&self, current: SessionId) -> Picker {
        let tidy = |p: PathBuf| tidy_path(&p);
        let mut items = Vec::new();
        for s in &self.sessions {
            let attached = self.clients.values().filter(|c| c.session == s.id).count();
            let n = s.windows.len();
            items.push(PickItem {
                label: s.name.clone(),
                detail: format!(
                    "{n} window{}{}",
                    if n == 1 { "" } else { "s" },
                    if attached > 0 { ", attached" } else { "" }
                ),
                indent: 0,
                current: false,
                target: Target::Session(s.id),
            });
            for (i, w) in s.windows.iter().enumerate() {
                let panes = w.layout.panes();
                let cwd = self.panes.get(&w.active).and_then(|p| p.cwd()).map(&tidy).unwrap_or_default();
                items.push(PickItem {
                    label: format!("{}: {}", i + 1, window_title(w, &self.panes)),
                    detail: if panes.len() > 1 { format!("{} panes", panes.len()) } else { cwd },
                    indent: 1,
                    current: s.id == current && w.id == s.active,
                    target: Target::Window(s.id, w.id),
                });
                if panes.len() > 1 {
                    for (n, pid) in panes.iter().enumerate() {
                        let Some(p) = self.panes.get(pid) else { continue };
                        items.push(PickItem {
                            label: format!("{}.{} {}", i + 1, n + 1, p.name),
                            detail: p.cwd().map(&tidy).unwrap_or_default(),
                            indent: 2,
                            current: false,
                            target: Target::Pane(s.id, w.id, *pid),
                        });
                    }
                }
            }
        }
        Picker::new("Go to", items)
    }

    /// The session manager: one row per session with its windows and the
    /// directory of its focused pane.
    fn build_sessions(&self, current: SessionId) -> Picker {
        let mut items = Vec::new();
        for s in &self.sessions {
            let attached = self.clients.values().any(|c| c.session == s.id);
            let tabs: Vec<String> = s.windows.iter().map(|w| window_title(w, &self.panes)).collect();
            let cwd = self.panes.get(&s.window().active).and_then(|p| p.cwd()).map(|p| tidy_path(&p));
            let mut detail = tabs.join(" · ");
            if let Some(cwd) = cwd {
                detail = format!("{detail}   {cwd}");
            }
            items.push(PickItem {
                label: if attached && s.id != current { format!("{} (attached)", s.name) } else { s.name.clone() },
                detail,
                indent: 0,
                current: s.id == current,
                target: Target::Session(s.id),
            });
        }
        let mut picker = Picker::new("Sessions", items);
        picker.manage = true;
        picker
    }

    fn build_palette(&self) -> Picker {
        let prefix = keys::key_label(self.config.prefix);
        let items = Action::palette()
            .into_iter()
            .map(|action| {
                let mut bound: Vec<String> = self
                    .config
                    .direct_table
                    .iter()
                    .filter(|(_, a)| *a == action)
                    .map(|(k, _)| keys::key_label(*k))
                    .take(1)
                    .collect();
                bound.extend(
                    self.config
                        .prefix_table
                        .iter()
                        .filter(|(_, a)| *a == action)
                        .map(|(k, _)| format!("{prefix} {}", keys::key_label(*k)))
                        .take(1),
                );
                PickItem {
                    label: action.describe(),
                    detail: bound.join("  or  "),
                    indent: 0,
                    current: false,
                    target: Target::Action(action),
                }
            })
            .collect();
        Picker::new("Commands", items)
    }
}

pub const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1002h\x1b[?1006h";
pub const MOUSE_OFF: &[u8] = b"\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l";

// ===== process setup ========================================================

/// Directory holding the socket, the lock and the server log.
pub fn runtime_dir() -> PathBuf {
    // SAFETY: getuid has no failure modes.
    let uid = unsafe { libc::getuid() };
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("nerdmux");
    }
    let tmp = std::env::var_os("TMPDIR").filter(|d| !d.is_empty()).map(PathBuf::from);
    tmp.unwrap_or_else(|| PathBuf::from("/tmp")).join(format!("nerdmux-{uid}"))
}

pub fn default_socket() -> PathBuf {
    match std::env::var_os("NERDMUX_SOCKET").filter(|s| !s.is_empty()) {
        Some(s) => PathBuf::from(s),
        None => runtime_dir().join("default.sock"),
    }
}

/// Every pane needs a few descriptors and the default soft limit on macOS
/// is only 256, so ask for more room up front.
fn raise_fd_limit() {
    const WANT: libc::rlim_t = 4096;
    // SAFETY: plain getrlimit/setrlimit calls on a properly sized struct.
    unsafe {
        let mut lim: libc::rlimit = std::mem::zeroed();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return;
        }
        let target = if lim.rlim_max == libc::RLIM_INFINITY { WANT } else { WANT.min(lim.rlim_max) };
        if lim.rlim_cur < target {
            lim.rlim_cur = target;
            libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

/// Refuse to use a default runtime directory someone else controls: the
/// client sends its environment to whatever listens on the socket there.
pub fn check_private_dir(socket: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let Some(dir) = socket.parent() else { return Ok(()) };
    if dir != runtime_dir() {
        return Ok(());
    }
    let Ok(meta) = std::fs::symlink_metadata(dir) else { return Ok(()) };
    // SAFETY: getuid has no failure modes.
    let uid = unsafe { libc::getuid() };
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        anyhow::bail!(
            "{} must be a directory owned by you and closed to others (chmod 700); refusing to use it",
            dir.display()
        );
    }
    Ok(())
}

/// Run the server on `socket` until its last session ends.
pub fn serve(socket: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    use std::os::unix::io::AsRawFd;

    let dir = socket.parent().unwrap_or(Path::new("/tmp"));
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    check_private_dir(socket)?;

    // One server per socket: hold an exclusive lock for our whole life.
    let lock_path = socket.with_extension("lock");
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(&lock_path)?;
    // A server that is shutting down may still hold it for a moment, so
    // retry briefly; if it stays taken, a healthy server is already running.
    let deadline = Instant::now() + Duration::from_secs(3);
    // SAFETY: flock on a file descriptor we own.
    while unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        if Instant::now() >= deadline {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }

    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));

    // SAFETY: setting standard signal dispositions before any thread exists.
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let _ = std::env::set_current_dir("/");
    raise_fd_limit();

    let (tx, rx) = mpsc::sync_channel::<Event>(256);
    let accept_tx = tx.clone();
    thread::Builder::new().name("accept".into()).spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if accept_tx.send(Event::NewClient(stream)).is_err() {
                        break;
                    }
                }
                // Out of descriptors or similar: wait rather than spin.
                Err(_) => thread::sleep(Duration::from_millis(100)),
            }
        }
    })?;

    // SIGTERM (a reboot, a logout) and SIGINT stop the server without
    // touching the snapshot, so the sessions are offered back next time.
    let terminating = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        let _ = signal_hook::flag::register(signal, terminating.clone());
    }
    let server = Server::new(socket.to_string_lossy().into_owned(), tx, terminating);
    server.run(rx);
    // The lock file stays: removing it could let two servers hold "the" lock.
    drop(lock);
    Ok(())
}
