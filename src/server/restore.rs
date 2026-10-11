//! The server's side of `crate::snapshot`: keeping the snapshot of its
//! sessions up to date, and bringing saved sessions back.
//!
//! The snapshot is written a moment after the layout changes and now and
//! then while panes print. A removal waits like any other change, so that
//! when a reboot ends the shells and then the server, the server is told to
//! stop (SIGTERM) before it writes down panes it is about to lose: it then
//! exits without touching the file, and the snapshot shows the sessions as
//! they were.

use super::*;
use crate::overlay::{RestoreItem, RestorePicker};
use crate::procinfo;
use crate::snapshot::{self, SavedNode, SavedPane, SavedSession, SavedWindow, Snapshot};
use std::sync::atomic::AtomicBool;

pub struct SnapState {
    /// The live and the restorable snapshot files, when they can be kept.
    files: Option<(PathBuf, PathBuf)>,
    restorable: Option<Snapshot>,
    /// The saved sessions were offered to the first client already.
    offered: bool,
    /// When the next write is due.
    due: Option<Instant>,
    last_save: Instant,
    next_check: Instant,
    /// The layout as last seen, to notice changes.
    shape: String,
    /// The text last captured from each pane; kept while a pane shows its
    /// alternate screen, whose normal text cannot be read then.
    text: HashMap<PaneId, String>,
    /// Commands waiting to be typed into restored shells.
    typing: HashMap<PaneId, Typing>,
    /// The session made for the first client after start-up; restoring
    /// replaces it while nobody has typed into it.
    placeholder: Option<SessionId>,
    /// Set by SIGTERM or SIGINT: stop at once and leave the snapshot alone.
    pub terminating: Arc<AtomicBool>,
    /// The server ends because it was told to (`kill-server`).
    pub clean: bool,
    /// The server ends because of a failure: keep the snapshot.
    pub failed: bool,
    /// How long a change waits before it is written.
    delay: Duration,
}

struct Typing {
    bytes: Vec<u8>,
    since: Instant,
    /// When the shell last printed something.
    output: Option<Instant>,
}

impl Typing {
    /// Type once the shell has drawn its prompt and gone quiet, or after a
    /// while regardless. Typed earlier, the text shows up before the
    /// prompt, and some shells print it twice.
    fn ready(&self, now: Instant) -> bool {
        self.output.is_some_and(|t| now.duration_since(t) >= Duration::from_millis(150))
            || now.duration_since(self.since) >= Duration::from_secs(3)
    }
}

impl SnapState {
    pub fn new(terminating: Arc<AtomicBool>) -> SnapState {
        let now = Instant::now();
        // NERDMUX_SNAPSHOT_MS shortens the waits, for tests.
        let ms = std::env::var("NERDMUX_SNAPSHOT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
        SnapState {
            files: None,
            restorable: None,
            offered: false,
            due: None,
            last_save: now,
            next_check: now,
            shape: String::new(),
            text: HashMap::new(),
            typing: HashMap::new(),
            placeholder: None,
            terminating,
            clean: false,
            failed: false,
            delay: Duration::from_millis(ms),
        }
    }

    /// When something is due: a write or a command to type.
    pub fn next_timer(&self) -> Option<Instant> {
        let typing = self.typing.values().map(|t| match t.output {
            Some(o) => (o + Duration::from_millis(150)).min(t.since + Duration::from_secs(3)),
            None => t.since + Duration::from_secs(3),
        });
        typing.chain(self.due).min()
    }

    pub fn terminating(&self) -> bool {
        self.terminating.load(Ordering::SeqCst)
    }
}

impl Server {
    /// At start-up. A live snapshot left behind means the previous server
    /// did not end cleanly: it becomes the restorable one.
    pub(super) fn snapshot_start(&mut self) {
        let Some((live, restorable)) = snapshot::paths(&self.socket) else { return };
        if !self.config.restore {
            let _ = std::fs::remove_file(&live);
            let _ = std::fs::remove_file(&restorable);
            return;
        }
        if live.exists() {
            let _ = std::fs::rename(&live, &restorable);
        }
        self.snap.restorable = snapshot::load(&restorable).filter(|s| !s.sessions.is_empty());
        self.snap.files = Some((live, restorable));
    }

    /// When the server stops. A clean end removes the snapshot; being told
    /// to stop, or failing, leaves it to be offered next time.
    pub(super) fn snapshot_end(&mut self) {
        let Some((live, _)) = self.snap.files.clone() else { return };
        if self.snap.terminating() || self.snap.failed {
            return;
        }
        if !self.snap.clean {
            // The last pane ended. A reboot ends the shells moments before
            // it tells the server to stop: give that a moment to arrive.
            // The server's lock is still held, so a new server waits.
            let until = Instant::now() + self.snap.delay.min(Duration::from_secs(1));
            while Instant::now() < until {
                if self.snap.terminating() {
                    return;
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
        let _ = std::fs::remove_file(&live);
    }

    /// Called from `tick`: type restored commands, and write the snapshot
    /// when it is due.
    pub(super) fn snapshot_tick(&mut self, now: Instant) {
        let ready: Vec<PaneId> = self.snap.typing.iter().filter(|(_, t)| t.ready(now)).map(|(p, _)| *p).collect();
        for pid in ready {
            if let (Some(t), Some(pane)) = (self.snap.typing.remove(&pid), self.panes.get(&pid)) {
                pane.write(t.bytes);
            }
        }
        if self.snap.files.is_none() || !self.config.restore || self.sessions.is_empty() || self.snap.terminating() {
            return;
        }
        if now >= self.snap.next_check {
            self.snap.next_check = now + Duration::from_millis(500);
            let shape = self.shape();
            if shape != self.snap.shape {
                self.snap.shape = shape;
                self.snap.due.get_or_insert(now + self.snap.delay);
            } else if now.duration_since(self.snap.last_save) >= self.snap.delay * 10
                && self.panes.values().any(|p| p.printed)
            {
                self.snap.due.get_or_insert(now);
            }
        }
        if self.snap.due.is_some_and(|d| now >= d) {
            self.snap.due = None;
            self.save_snapshot();
        }
    }

    /// A pane printed: a command waiting for its shell waits a bit longer.
    pub(super) fn snapshot_output(&mut self, pid: PaneId) {
        if let Some(t) = self.snap.typing.get_mut(&pid) {
            t.output = Some(Instant::now());
        }
    }

    /// Sessions, windows and layouts in a form that changes when they do.
    fn shape(&self) -> String {
        let mut out = String::new();
        for s in &self.sessions {
            out.push_str(&format!("{}|{}|", s.name, s.active));
            for w in &s.windows {
                out.push_str(&format!("{}|{:?}|{}|{}|{:?};", w.id, w.name, w.active, w.zoomed, w.layout));
            }
        }
        out
    }

    fn save_snapshot(&mut self) {
        let Some((live, _)) = self.snap.files.clone() else { return };
        let snap = self.capture();
        if let Err(e) = snapshot::save(&live, &snap) {
            eprintln!("nerdmux: cannot save the sessions to {}: {e}", live.display());
        }
        self.snap.last_save = Instant::now();
    }

    fn capture(&mut self) -> Snapshot {
        let panes = &self.panes;
        self.snap.text.retain(|pid, _| panes.contains_key(pid));
        let shapes: Vec<(String, usize, Vec<Window>)> =
            self.sessions.iter().map(|s| (s.name.clone(), s.active_index(), s.windows.clone())).collect();
        let mut sessions = Vec::new();
        for (name, active, windows) in shapes {
            let windows = windows
                .iter()
                .map(|w| SavedWindow {
                    name: w.name.clone(),
                    active: w.layout.panes().iter().position(|p| *p == w.active).unwrap_or(0),
                    zoomed: w.zoomed,
                    layout: self.saved_node(&w.layout),
                })
                .collect();
            sessions.push(SavedSession { name, active, windows });
        }
        Snapshot { version: snapshot::VERSION, saved: snapshot::now(), sessions }
    }

    fn saved_node(&mut self, node: &Node) -> SavedNode {
        match node {
            Node::Leaf(pid) => SavedNode::Pane(self.saved_pane(*pid)),
            Node::Split { axis, ratio, a, b } => SavedNode::Split {
                split: match axis {
                    Axis::Horizontal => "right",
                    Axis::Vertical => "down",
                }
                .into(),
                ratio: *ratio,
                a: Box::new(self.saved_node(a)),
                b: Box::new(self.saved_node(b)),
            },
        }
    }

    fn saved_pane(&mut self, pid: PaneId) -> SavedPane {
        let Some(pane) = self.panes.get_mut(&pid) else { return SavedPane::default() };
        let cwd = pane.cwd().map(|p| p.to_string_lossy().into_owned());
        let command = foreground_command(pane);
        let fresh = pane.printed || !self.snap.text.contains_key(&pid);
        if fresh && !pane.mode().contains(TermMode::ALT_SCREEN) {
            self.snap.text.insert(pid, snapshot::pane_text(&pane.term));
            pane.printed = false;
        }
        SavedPane { cwd, command, text: self.snap.text.get(&pid).cloned().unwrap_or_default() }
    }

    // ----- bringing sessions back ------------------------------------------

    /// Show client `cid` the saved sessions. Unless `asked`, only once
    /// after start-up and only when there are some. Restored ones are no
    /// longer in the snapshot, so whatever is listed has not come back yet;
    /// a name now taken gets a suffix.
    pub(super) fn offer_restore(&mut self, cid: ClientId, asked: bool) {
        if !asked && self.snap.offered {
            return;
        }
        self.snap.offered = true;
        let Some(snap) = &self.snap.restorable else {
            if asked {
                self.message(Some(cid), "no saved sessions to restore");
            }
            return;
        };
        let items: Vec<RestoreItem> = snap
            .sessions
            .iter()
            .enumerate()
            .map(|(index, s)| RestoreItem {
                index,
                name: s.name.clone(),
                summary: s.summary(),
                place: s.place().map(|p| tidy_path(Path::new(p))).unwrap_or_default(),
                checked: true,
            })
            .collect();
        let picker = RestorePicker { saved: snap.saved, items, selected: 0 };
        if let Some(c) = self.clients.get_mut(&cid) {
            c.overlay = Some(Overlay::Restore(picker));
            c.prefix = None;
        }
        self.dirty = true;
    }

    /// The first client after start-up got a fresh session, `sid`.
    pub(super) fn note_placeholder(&mut self, sid: SessionId) {
        if self.snap.restorable.is_some() && !self.snap.offered {
            self.snap.placeholder = Some(sid);
        }
    }

    /// The restore picker was closed without restoring.
    pub(super) fn restore_dismissed(&mut self) {
        self.snap.placeholder = None;
    }

    /// Bring back the saved sessions at `picks` (indices into the
    /// restorable snapshot) and show the first of them to `cid`.
    pub(super) fn restore_sessions(&mut self, cid: ClientId, picks: Vec<usize>) {
        let Some(snap) = self.snap.restorable.clone() else { return };
        let Some(client) = self.clients.get(&cid) else { return };
        let (cols, rows) = (client.cols, client.rows);
        let Some(current) = self.sessions.iter().find(|s| s.id == client.session) else { return };
        let (env, cwd) = (current.env.clone(), current.cwd.clone());

        // The session made for this client steps aside, so a saved session
        // with the same name (often `0`) keeps it.
        let placeholder = self.snap.placeholder.take().filter(|p| self.untouched(*p));
        let placeholder_name = placeholder.and_then(|p| {
            let s = self.sessions.iter_mut().find(|s| s.id == p)?;
            Some(std::mem::replace(&mut s.name, format!("restoring-{p}")))
        });

        let mut first = None;
        let mut failed = Vec::new();
        for i in &picks {
            let Some(saved) = snap.sessions.get(*i) else { continue };
            match self.rebuild_session(saved, &env, &cwd, cols, rows) {
                Some(sid) => {
                    first.get_or_insert(sid);
                }
                None => failed.push(saved.name.clone()),
            }
        }

        // What was restored is no longer offered.
        let mut rest = snap.clone();
        rest.sessions =
            snap.sessions.iter().enumerate().filter(|(i, _)| !picks.contains(i)).map(|(_, s)| s.clone()).collect();
        if let Some((_, path)) = &self.snap.files {
            if rest.sessions.is_empty() {
                let _ = std::fs::remove_file(path);
            } else if let Err(e) = snapshot::save(path, &rest) {
                eprintln!("nerdmux: cannot update {}: {e}", path.display());
            }
        }
        self.snap.restorable = (!rest.sessions.is_empty()).then_some(rest);

        match (first, placeholder) {
            (Some(sid), p) => {
                self.switch_client(cid, sid);
                if let Some(p) = p {
                    self.remove_session(p);
                }
            }
            // Nothing came back: the session keeps its name.
            (None, Some(p)) => {
                if let (Some(s), Some(name)) = (self.sessions.iter_mut().find(|s| s.id == p), placeholder_name) {
                    s.name = name;
                }
            }
            (None, None) => {}
        }
        if !failed.is_empty() {
            self.message(Some(cid), format!("could not restore {}", failed.join(", ")));
        }
        self.dirty = true;
    }

    /// Whether session `sid` is still a single pane nobody typed into.
    fn untouched(&self, sid: SessionId) -> bool {
        let Some(s) = self.sessions.iter().find(|s| s.id == sid) else { return false };
        let panes: Vec<PaneId> = s.windows.iter().flat_map(|w| w.layout.panes()).collect();
        panes.len() == 1 && self.panes.get(&panes[0]).is_some_and(|p| !p.touched)
    }

    fn rebuild_session(
        &mut self,
        saved: &SavedSession,
        env: &[(String, String)],
        cwd: &Path,
        cols: u16,
        rows: u16,
    ) -> Option<SessionId> {
        let mut name = saved.name.clone();
        let mut n = 2;
        while !valid_name(&name) || self.sessions.iter().any(|s| s.name == name) {
            name = format!("{}-{n}", saved.name.chars().take(56).collect::<String>());
            n += 1;
        }
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
            env: env.to_vec(),
            cwd: cwd.to_path_buf(),
            used,
        });
        let area = self.session_area(sid)?;
        for w in &saved.windows {
            let mut panes = Vec::new();
            let layout = match self.rebuild_node(sid, &w.layout, area, &mut panes) {
                Ok(layout) => layout,
                Err(e) => {
                    eprintln!("nerdmux: restoring a window failed: {e}");
                    for p in panes {
                        self.panes.remove(&p);
                        self.snap.typing.remove(&p);
                    }
                    continue;
                }
            };
            let active = panes.get(w.active).or(panes.first()).copied()?;
            let wid = self.id();
            let session = self.sessions.iter_mut().find(|s| s.id == sid)?;
            session.windows.push(Window {
                id: wid,
                name: w.name.clone(),
                layout,
                active,
                last: None,
                zoomed: w.zoomed && panes.len() > 1,
                sync: false,
                preset: None,
                bell: false,
            });
        }
        let session = self.sessions.iter_mut().find(|s| s.id == sid)?;
        let Some(active) = session.windows.get(saved.active).or(session.windows.first()).map(|w| w.id) else {
            self.sessions.retain(|s| s.id != sid);
            return None;
        };
        session.active = active;
        self.had_session = true;
        self.relayout(sid);
        Some(sid)
    }

    fn rebuild_node(
        &mut self,
        sid: SessionId,
        node: &SavedNode,
        area: Rect,
        panes: &mut Vec<PaneId>,
    ) -> Result<Node, String> {
        Ok(match node {
            SavedNode::Pane(saved) => {
                let cwd = saved.cwd.as_ref().map(PathBuf::from).filter(|d| d.is_dir());
                let pid = self.spawn_pane(sid, area.width, area.height, None, cwd)?;
                panes.push(pid);
                if let Some(pane) = self.panes.get_mut(&pid) {
                    pane.feed(saved.text.as_bytes());
                }
                if !saved.command.is_empty() {
                    // Safe programs start again; anything else is typed for
                    // the user to run with Enter.
                    let mut bytes = snapshot::shell_words(&saved.command).into_bytes();
                    if snapshot::safe_to_rerun(&saved.command, &self.config.restore_programs) {
                        bytes.push(b'\r');
                    }
                    self.snap.typing.insert(pid, Typing { bytes, since: Instant::now(), output: None });
                }
                Node::Leaf(pid)
            }
            SavedNode::Split { split, ratio, a, b } => Node::Split {
                axis: if split == "down" { Axis::Vertical } else { Axis::Horizontal },
                ratio: if ratio.is_finite() { ratio.clamp(0.05, 0.95) } else { 0.5 },
                a: Box::new(self.rebuild_node(sid, a, area, panes)?),
                b: Box::new(self.rebuild_node(sid, b, area, panes)?),
            },
        })
    }
}

/// The program in the foreground of `pane` with its arguments, or nothing
/// when that is the shell itself or a pipeline.
fn foreground_command(pane: &Pane) -> Vec<String> {
    let (Some(shell), Some(fg)) = (pane.pid.map(|p| p as i32), pane.foreground_pid()) else { return Vec::new() };
    if fg == shell || procinfo::group_children(fg, shell) > 1 {
        return Vec::new();
    }
    procinfo::cmdline(fg).unwrap_or_default()
}
