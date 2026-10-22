//! End-to-end tests: start a real server, attach over the socket exactly as
//! the client does, replay what the server draws into a terminal emulator
//! and check what a user would see.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use nerdmux::proto::{
    read_msg, write_msg, Key, KeyCode, Mouse, MouseButton, MouseKind, Request, ALT, C2S, CTRL, PROTOCOL_VERSION, S2C,
    SHIFT, SUPER,
};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_nerdmux");
const TIMEOUT: Duration = Duration::from_secs(10);
static COUNTER: AtomicU32 = AtomicU32::new(0);

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

/// A private server with one session called `main`, torn down on drop.
struct Harness {
    dir: PathBuf,
    shell: String,
}

impl Harness {
    fn new() -> Harness {
        Harness::with_config("")
    }

    fn with_config(config: &str) -> Harness {
        Harness::start(config, "/bin/sh")
    }

    fn start(config: &str, shell: &str) -> Harness {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        // Unix socket paths are short; keep the directory name small.
        let dir = PathBuf::from("/tmp").join(format!("nerdmux-e2e-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Panes start at the left edge, so the tests can read them by column.
        let config = if config.contains("padding") {
            config.to_string()
        } else {
            // Top-level keys must come before the first table.
            let (head, tables): (Vec<&str>, Vec<&str>) = {
                let lines: Vec<&str> = config.lines().collect();
                let at = lines.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(lines.len());
                (lines[..at].to_vec(), lines[at..].to_vec())
            };
            [head, vec!["padding = 0", "padding_rows = 0"], tables].concat().join("\n")
        };
        std::fs::write(dir.join("config.toml"), config).unwrap();
        let h = Harness { dir, shell: shell.to_string() };
        let out = h.nerdmux(&["new", "-d", "-s", "main"]);
        assert!(out.status.success(), "could not start a session: {}", String::from_utf8_lossy(&out.stderr));
        h
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("s.sock")
    }

    fn server_log(&self) -> String {
        std::fs::read_to_string(self.dir.join("s.log")).unwrap_or_default()
    }

    fn env(&self) -> Vec<(String, String)> {
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        [
            ("PATH", path.as_str()),
            ("HOME", self.dir.to_str().unwrap()),
            ("SHELL", self.shell.as_str()),
            ("PS1", "$ "),
            ("ENV", ""),
            ("LANG", "C.UTF-8"),
            ("NERDMUX_SOCKET", self.socket().to_str().unwrap()),
            ("NERDMUX_CONFIG", self.dir.join("config.toml").to_str().unwrap()),
            // Snapshots are written a moment after changes; keep it short.
            ("NERDMUX_SNAPSHOT_MS", "200"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    /// Run the command-line client.
    fn nerdmux(&self, args: &[&str]) -> Output {
        // NERDMUX_TEST_BIN points the suite at another build of the binary.
        let bin = std::env::var("NERDMUX_TEST_BIN").unwrap_or_else(|_| BIN.to_string());
        Command::new(bin).args(args).env_clear().envs(self.env()).current_dir(&self.dir).output().unwrap()
    }

    fn stdout(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.nerdmux(args).stdout).into_owned()
    }

    fn connect(&self, cols: u16, rows: u16, request: Request) -> Client {
        self.connect_env(cols, rows, request, &[])
    }

    /// Connect as a client whose environment has `extra` on top of the usual.
    fn connect_env(&self, cols: u16, rows: u16, request: Request, extra: &[(&str, &str)]) -> Client {
        let mut stream = UnixStream::connect(self.socket()).expect("server socket");
        let mut env = self.env();
        env.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let hello = C2S::Hello {
            version: PROTOCOL_VERSION,
            cols,
            rows,
            cwd: self.dir.to_string_lossy().into_owned(),
            env,
            request,
        };
        write_msg(&mut stream, &hello).unwrap();
        Client::new(stream, cols, rows)
    }

    fn attach(&self, cols: u16, rows: u16) -> Client {
        let mut c = self.connect(cols, rows, Request::Attach { target: Some("main".into()), create: false });
        c.wait_for("the shell prompt", |s| s.iter().any(|l| l.trim_start().starts_with('$')));
        c
    }
}

impl Harness {
    /// The live snapshot file of this server, once written.
    fn snapshot(&self) -> Option<PathBuf> {
        let dir = self.dir.join(".local/state/nerdmux");
        std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            name.ends_with(".toml") && !name.ends_with(".restorable.toml")
        })
    }

    /// Wait until the live snapshot mentions everything in `needles`.
    fn wait_snapshot(&self, needles: &[&str]) -> String {
        let start = Instant::now();
        loop {
            let text = self.snapshot().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            if needles.iter().all(|n| text.contains(n)) {
                return text;
            }
            assert!(start.elapsed() < TIMEOUT, "snapshot never had {needles:?}:\n{text}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The server's process id, found by its command line.
    fn server_pid(&self) -> i32 {
        let pattern = format!("__server {}", self.socket().display());
        let out = Command::new("pgrep").args(["-f", &pattern]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().next().and_then(|l| l.trim().parse().ok()).expect("server process")
    }

    /// Processes the server started: the panes' shells.
    fn shell_pids(&self, server: i32) -> Vec<i32> {
        let out = Command::new("pgrep").args(["-P", &server.to_string()]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect()
    }

    /// Wait for the server to be gone, socket and process.
    fn wait_server_gone(&self, pid: i32) {
        let start = Instant::now();
        // SAFETY: signal 0 only asks whether the process exists.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(start.elapsed() < TIMEOUT, "server did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Start a server with no sessions, as plain `nerdmux` does.
    fn start_server(&self) {
        let bin = std::env::var("NERDMUX_TEST_BIN").unwrap_or_else(|_| BIN.to_string());
        let status = Command::new(bin)
            .arg("__server")
            .arg(self.socket())
            .env_clear()
            .envs(self.env())
            .current_dir(&self.dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(self.dir.join("s2.log")).unwrap())
            .status()
            .unwrap();
        assert!(status.success());
        let start = Instant::now();
        while UnixStream::connect(self.socket()).is_err() {
            assert!(start.elapsed() < TIMEOUT, "server did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.nerdmux(&["kill-server"]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Client {
    stream: UnixStream,
    rx: Receiver<S2C>,
    term: Term<VoidListener>,
    parser: Processor,
    exit: Option<(String, i32)>,
    /// Everything the server sent, for checks on raw escape sequences.
    raw: Vec<u8>,
}

impl Client {
    fn new(stream: UnixStream, cols: u16, rows: u16) -> Client {
        let mut reader = stream.try_clone().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(msg) = read_msg::<_, S2C>(&mut reader) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        let term = Term::new(TermConfig::default(), &Size(cols as usize, rows as usize), VoidListener);
        Client { stream, rx, term, parser: Processor::new(), exit: None, raw: Vec::new() }
    }

    fn pump(&mut self, wait: Duration) {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(S2C::Output(bytes)) => {
                    self.parser.advance(&mut self.term, &bytes);
                    self.raw.extend_from_slice(&bytes);
                }
                Ok(S2C::Exit { message, code }) => self.exit = Some((message, code)),
                Err(_) => break,
            }
        }
    }

    /// The cell where `needle` starts on screen.
    fn cell_at(&self, needle: &str) -> &Cell {
        let (y, line) = self.screen().into_iter().enumerate().find(|(_, l)| l.contains(needle)).expect(needle);
        let x = line[..line.find(needle).unwrap()].chars().count();
        &self.term.grid()[Line(y as i32)][Column(x)]
    }

    fn screen(&self) -> Vec<String> {
        let grid = self.term.grid();
        (0..self.term.screen_lines())
            .map(|y| {
                let row = &grid[Line(y as i32)];
                let line: String = (0..self.term.columns())
                    .filter(|x| !row[Column(*x)].flags.contains(Flags::WIDE_CHAR_SPACER))
                    .map(|x| row[Column(x)].c)
                    .collect();
                line.trim_end().to_string()
            })
            .collect()
    }

    fn dump(&self) -> String {
        self.screen().iter().map(|l| format!("  |{l}")).collect::<Vec<_>>().join("\n")
    }

    /// Poll until the screen satisfies `pred`; panic with the screen otherwise.
    fn wait_for(&mut self, what: &str, pred: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.pump(Duration::from_millis(15));
            if pred(&self.screen()) {
                return;
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for {what}; exit={:?}; screen:\n{}", self.exit, self.dump());
            }
        }
    }

    fn wait_text(&mut self, needle: &str) {
        self.wait_for(&format!("{needle:?} on screen"), |s| s.iter().any(|l| l.contains(needle)));
    }

    fn wait_gone(&mut self, needle: &str) {
        self.wait_for(&format!("{needle:?} to disappear"), |s| !s.iter().any(|l| l.contains(needle)));
    }

    fn wait_exit(&mut self) -> (String, i32) {
        let deadline = Instant::now() + TIMEOUT;
        while self.exit.is_none() {
            self.pump(Duration::from_millis(15));
            assert!(Instant::now() < deadline, "server never told the client to exit; screen:\n{}", self.dump());
        }
        self.exit.clone().unwrap()
    }

    fn status(&self) -> String {
        self.screen().last().cloned().unwrap_or_default()
    }

    fn send(&mut self, msg: C2S) {
        // A write can fail right after the server said goodbye (detach,
        // session ended); the Exit message is what the tests then look at.
        if write_msg(&mut self.stream, &msg).is_err() {
            self.pump(Duration::from_millis(200));
            assert!(self.exit.is_some(), "the server hung up without an Exit message");
        }
    }

    fn key(&mut self, key: Key) {
        self.send(C2S::Key(key));
    }

    fn code(&mut self, code: KeyCode) {
        self.key(Key::plain(code));
    }

    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            self.key(Key::ch(c));
        }
    }

    /// Type a shell command and press Enter.
    fn run(&mut self, command: &str) {
        self.typed(command);
        self.code(KeyCode::Enter);
    }

    /// The leader (Alt+Space) followed by `c`.
    fn prefix(&mut self, c: char) {
        self.key(Key::alt(' '));
        self.key(Key::ch(c));
    }

    fn prefix_code(&mut self, code: KeyCode) {
        self.key(Key::alt(' '));
        self.code(code);
    }

    fn alt(&mut self, c: char) {
        self.key(Key::alt(c));
    }

    /// A non-character key with modifiers, e.g. `chord(KeyCode::Left, ALT | SHIFT)`.
    fn chord(&mut self, code: KeyCode, mods: u8) {
        self.key(Key::new(code, mods));
    }

    fn raw_text(&self) -> String {
        String::from_utf8_lossy(&self.raw).into_owned()
    }

    fn mouse(&mut self, kind: MouseKind, col: u16, row: u16) {
        self.send(C2S::Mouse(Mouse { kind, col, row, mods: 0 }));
    }

    fn click(&mut self, col: u16, row: u16) {
        self.mouse(MouseKind::Down(MouseButton::Left), col, row);
        self.mouse(MouseKind::Up(MouseButton::Left), col, row);
    }

    /// Column of the first vertical separator on the top row, if any.
    /// Column of the first vertical separator, read on the first pane row
    /// (row 0 is the frame kept around split panes).
    fn vsep(&self) -> Option<usize> {
        let row: Vec<char> = self.screen()[1].chars().collect();
        (1..row.len().saturating_sub(1)).find(|&i| matches!(row[i], '│' | '▏' | '▕' | '├' | '┤' | '┬' | '┼'))
    }

    fn has_hsep(&self) -> bool {
        self.screen().iter().any(|l| l.contains("───") || l.contains("▔▔▔") || l.contains("▁▁▁"))
    }

    /// Column at which `needle` starts, on whichever line holds it.
    fn col_of(&self, needle: &str) -> Option<usize> {
        self.screen().iter().find_map(|l| l.find(needle).map(|b| l[..b].chars().count()))
    }
}

#[test]
fn attach_shows_shell_and_status_bar() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    c.run("echo hello-$((40+2))");
    c.wait_text("hello-42");
    let status = c.status();
    assert!(status.contains("main"), "status bar should name the session: {status:?}");
    assert!(status.contains("1 sh"), "status bar should list the window: {status:?}");
    assert_eq!(h.stdout(&["ls"]).trim(), "main: 1 window, 1 pane (attached)");
}

#[test]
fn padding_keeps_panes_off_the_edges() {
    let h = Harness::with_config("padding = 2");
    let mut c = h.attach(80, 24);
    c.run("echo hello-$((40+2))");
    c.wait_text("hello-42");
    let line = c.screen().into_iter().find(|l| l.contains("hello-42") && !l.contains("echo")).unwrap();
    assert!(line.starts_with("  hello-42"), "output should start two columns in: {line:?}");
    assert!(c.status().starts_with(" main"), "the status bar keeps its place: {:?}", c.status());
    // One row is left free at the top, away from the status bar.
    assert!(c.screen()[0].trim().is_empty(), "the top row should be empty: {:?}", c.screen()[0]);
    assert!(c.screen()[1].trim_start().starts_with('$'), "the prompt starts on the second row: {}", c.dump());
}

#[test]
fn padding_holds_the_focus_outline_when_split() {
    let h = Harness::with_config("padding = 1\npadding_rows = 1");
    let mut c = h.attach(100, 30);
    c.run("echo one-$((0+1))");
    c.wait_text("one-1");
    let col = c.col_of("one-1").unwrap();
    let row = c.screen().iter().position(|l| l.trim_start().starts_with('$')).unwrap();
    c.prefix('\\');
    c.wait_for("the outline", |s| s.iter().any(|l| l.contains("▔▔▔")));
    // The outline takes the padding: the left pane's text stays where it was,
    // and nothing else is pushed in from the edges.
    assert_eq!(c.col_of("one-1"), Some(col), "text moved when the window split:\n{}", c.dump());
    assert!(c.screen()[row].trim_start().starts_with('$'), "the first row moved:\n{}", c.dump());
    assert!(c.screen()[row - 1].contains("▁▁▁"), "the top edge belongs in the padding row:\n{}", c.dump());
}

#[test]
fn splits_and_directional_focus() {
    let h = Harness::new();
    let mut c = h.attach(100, 30);
    c.prefix('\\');
    c.wait_for("a vertical separator", |s| s[1].contains('▕') || s[1].contains('│'));
    let sep = c.vsep().unwrap();
    assert!((45..=55).contains(&sep), "separator should be near the middle, got column {sep}");
    // The focused (right) pane is outlined on all four sides.
    let screen = c.screen();
    assert!(screen[0].contains("▁▁▁"), "top edge missing:\n{}", c.dump());
    assert!(screen.iter().any(|l| l.contains("▔▔▔")), "bottom edge missing:\n{}", c.dump());

    // The new pane has the focus and sits on the right.
    c.run("echo right-$((1+1))");
    c.wait_text("right-2");
    assert!(c.col_of("right-2").unwrap() > sep);

    c.chord(KeyCode::Left, ALT);
    c.run("echo left-$((1+2))");
    c.wait_text("left-3");
    assert!(c.col_of("left-3").unwrap() < sep);

    // Focus wraps around the edge of the window.
    c.chord(KeyCode::Left, ALT);
    c.run("echo wrapped-$((2+2))");
    c.wait_text("wrapped-4");
    assert!(c.col_of("wrapped-4").unwrap() > sep);

    c.prefix('-');
    c.wait_for("a horizontal separator", |s| s.iter().any(|l| l.contains("▁▁▁") || l.contains("───")));
    assert!(c.has_hsep());
    assert_eq!(h.stdout(&["ls"]).trim(), "main: 1 window, 3 panes (attached)");

    // The prefix reaches the same actions, also with arrows.
    c.key(Key::alt(' '));
    c.code(KeyCode::Up);
    c.run("echo upper-$((3+3))");
    c.wait_text("upper-6");
    let upper_row = c.screen().iter().position(|l| l.contains("upper-6")).unwrap();
    c.chord(KeyCode::Down, ALT);
    c.run("echo lower-$((4+4))");
    c.wait_text("lower-8");
    assert!(c.screen().iter().position(|l| l.contains("lower-8")).unwrap() > upper_row);

    // Alt+Enter splits wherever there is most room.
    c.chord(KeyCode::Enter, ALT);
    c.wait_for("a fourth pane", |_| h.stdout(&["ls"]).contains("4 panes"));
}

#[test]
fn windows_can_be_created_selected_and_renamed() {
    let h = Harness::new();
    let mut c = h.attach(90, 24);
    c.run("echo first-$((5+5))");
    c.wait_text("first-10");

    c.prefix('t');
    c.wait_for("a second tab", |s| s.last().unwrap().contains("2 sh"));
    c.wait_gone("first-10");
    c.run("echo second-$((10+10))");
    c.wait_text("second-20");

    c.alt('1');
    c.wait_text("first-10");
    c.wait_gone("second-20");
    c.chord(KeyCode::Right, ALT | SHIFT);
    c.wait_text("second-20");
    c.chord(KeyCode::Left, ALT | SHIFT);
    c.wait_text("first-10");
    c.chord(KeyCode::Right, ALT | SHIFT);
    c.wait_text("second-20");
    c.prefix_code(KeyCode::Tab);
    c.wait_text("first-10");

    // Rename: the prompt starts with the current name, Ctrl+u clears it.
    c.prefix('n');
    c.wait_text("Rename window");
    c.key(Key::ctrl('u'));
    c.typed("editor");
    c.code(KeyCode::Enter);
    c.wait_for("the new name in the status bar", |s| s.last().unwrap().contains("1 editor"));

    // Clicking a tab selects it. Its title is the program's name, which on
    // macOS turns from `sh` into `bash`, so look for the number.
    let col = c.status().find(" 2 ").map(|b| c.status()[..b].chars().count() + 1).unwrap() as u16;
    c.click(col, 23);
    c.wait_text("second-20");
}

#[test]
fn sessions_survive_detach() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    c.run("echo keep-$((6*7))");
    c.wait_text("keep-42");
    c.prefix('d');
    let (message, code) = c.wait_exit();
    assert!(message.contains("detached"), "{message:?}");
    assert_eq!(code, 0);
    assert_eq!(h.stdout(&["ls"]).trim(), "main: 1 window, 1 pane");

    // A new client, with a different size, sees the same shell.
    let mut c = h.attach(100, 30);
    c.wait_text("keep-42");
    c.run("echo again-$((1+1))");
    c.wait_text("again-2");
}

#[test]
fn copy_mode_selects_and_pastes() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    c.run("echo copy-me-$((100+23))");
    c.wait_text("copy-me-123");

    c.prefix('c');
    c.wait_for("the copy badge", |s| s.last().unwrap().contains("COPY"));
    c.wait_text("Shift selects");
    // Letters do nothing here; this is not vi.
    c.typed("kkv0y");
    c.pump(Duration::from_millis(150));
    assert!(c.status().contains("COPY") && !c.status().contains("copied"), "{}", c.dump());
    // Up one line, to its start, Shift+End selects it, Ctrl+C copies.
    c.code(KeyCode::Up);
    c.code(KeyCode::Home);
    c.chord(KeyCode::End, SHIFT);
    c.key(Key::ctrl('c'));
    c.wait_text("copied");
    c.wait_for("copy mode to end", |s| !s.last().unwrap().contains("COPY"));

    c.typed("echo got:");
    c.prefix('v');
    c.code(KeyCode::Enter);
    c.wait_text("got:copy-me-123");

    // The copy also went to the client's terminal clipboard (OSC 52).
    let raw = String::from_utf8_lossy(&c.raw).into_owned();
    assert!(raw.contains("\x1b]52;c;Y29weS1tZS0xMjM="), "no OSC 52 with the copied text");
}

#[test]
fn copy_mode_search_finds_text_in_scrollback() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    c.run("seq 1 300");
    c.wait_text("300");
    c.chord(KeyCode::PageUp, ALT);
    c.wait_for("copy mode, one page up", |s| s.last().unwrap().contains("COPY") && !s.iter().any(|l| l == "300"));
    c.key(Key::ctrl('f'));
    c.wait_text("Find in scrollback");
    c.typed("14");
    c.code(KeyCode::Enter);
    // Searching runs towards older output: 214 is the first hit going up.
    let top_is = |s: &[String], n: &str| s.iter().any(|l| l.starts_with(n));
    c.wait_for("line 214", |s| top_is(s, "214"));
    c.code(KeyCode::F(3));
    c.code(KeyCode::F(3));
    c.wait_for("an older match scrolled into view", |s| top_is(s, "148"));
    c.code(KeyCode::Esc);
    c.wait_for("the live screen again", |s| s.iter().any(|l| l == "300") && !s.last().unwrap().contains("COPY"));
}

#[test]
fn closing_panes_and_ending_the_session() {
    let h = Harness::new();
    let mut c = h.attach(100, 30);
    c.prefix('\\');
    c.wait_for("a split", |s| s[1].contains('▕'));

    // Declining keeps the pane.
    c.prefix('x');
    c.wait_text("Close this pane");
    c.typed("n");
    c.wait_gone("Close this pane");
    assert!(c.vsep().is_some());

    c.prefix('x');
    c.wait_text("Close this pane");
    c.typed("y");
    c.wait_for("a single pane", |s| !s[1].contains('▏') && !s[1].contains('▕') && !s[1].contains('│'));

    // When the last shell exits the session ends and the client is told.
    c.run("exit");
    let (message, code) = c.wait_exit();
    assert_eq!((message.as_str(), code), ("[exited]", 0));
    let deadline = Instant::now() + TIMEOUT;
    while h.socket().exists() {
        assert!(Instant::now() < deadline, "server kept running without sessions");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(h.stdout(&["ls"]).trim(), "no nerdmux server is running");
}

#[test]
fn panes_follow_the_client_size() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    c.run("stty size");
    c.wait_text("23 80");
    c.send(C2S::Resize(100, 30));
    c.term.resize(Size(100, 30));
    c.run("stty size");
    c.wait_text("29 100");
    c.prefix('\\');
    c.wait_for("a split", |s| s[1].contains('▕'));
    c.run("stty size");
    c.wait_text("27 48");
}

#[test]
fn prefix_shows_key_hints_after_a_pause() {
    let h = Harness::new();
    let mut c = h.attach(120, 40);
    c.key(Key::alt(' '));
    c.wait_text("Alt+Space then…");
    c.wait_text("split right");
    c.wait_text("detach");
    c.code(KeyCode::Esc);
    c.wait_gone("Alt+Space then…");
    // Escape canceled the prefix: the next key is ordinary input.
    c.run("echo typed-$((7+7))");
    c.wait_text("typed-14");

    // The full key list scrolls.
    c.prefix('?');
    c.wait_text("Chords work any time");
    c.wait_text("Alt+←↓↑→");
    c.code(KeyCode::End);
    c.wait_text("your terminal's own selection");
    c.code(KeyCode::Esc);
    c.wait_gone("your terminal's own selection");
}

#[test]
fn wheel_scrolls_history_and_typing_returns() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    c.run("seq 1 100");
    c.wait_for("the end of seq", |s| s.iter().any(|l| l == "100"));
    for _ in 0..10 {
        c.mouse(MouseKind::ScrollUp, 10, 5);
    }
    c.wait_for("older lines", |s| s.iter().any(|l| l.starts_with("60")) && !s.iter().any(|l| l == "100"));
    c.wait_for("the scroll position badge", |s| s[0].contains("30/"));
    c.run("echo back-$((8+8))");
    c.wait_text("back-16");
    assert!(c.screen().iter().any(|l| l == "100"));
}

#[test]
fn several_sessions_and_the_switcher() {
    let h = Harness::new();
    let out = h.nerdmux(&["new", "-d", "-s", "other"]);
    assert!(out.status.success());
    let out = h.nerdmux(&["new", "-d", "-s", "other"]);
    assert!(!out.status.success(), "duplicate session names must be refused");
    assert_eq!(h.stdout(&["ls"]).trim(), "main: 1 window, 1 pane\nother: 1 window, 1 pane");

    let mut c = h.attach(90, 24);
    c.run("echo in-main-$((1+1))");
    c.wait_text("in-main-2");

    c.chord(KeyCode::Down, ALT | SHIFT);
    c.wait_for("the other session", |s| s.last().unwrap().trim_start().starts_with("other"));
    c.wait_gone("in-main-2");
    c.run("echo in-other-$((2+2))");
    c.wait_text("in-other-4");

    // The switcher lists every session and filters as you type.
    c.prefix('f');
    c.wait_text("Go to");
    c.typed("main");
    c.wait_for("the filtered list", |s| s.iter().any(|l| l.contains("1/")));
    c.code(KeyCode::Enter);
    c.wait_text("in-main-2");

    // Killing a session moves its clients to another one.
    c.chord(KeyCode::Up, ALT | SHIFT);
    c.wait_text("in-other-4");
    assert!(h.nerdmux(&["kill-session", "-t", "other"]).status.success());
    c.wait_text("in-main-2");
    assert_eq!(h.stdout(&["ls"]).trim(), "main: 1 window, 1 pane (attached)");
    assert!(!h.nerdmux(&["kill-session", "-t", "nope"]).status.success());
}

#[test]
fn mouse_focus_and_border_drag() {
    let h = Harness::new();
    let mut c = h.attach(100, 30);
    c.prefix('\\');
    c.wait_for("a split", |s| s[1].contains('▕'));
    let sep = c.vsep().unwrap() as u16;

    c.click(5, 5);
    c.run("echo clicked-$((9+9))");
    c.wait_text("clicked-18");
    assert!(c.col_of("clicked-18").unwrap() < sep as usize);

    c.mouse(MouseKind::Down(MouseButton::Left), sep, 10);
    c.mouse(MouseKind::Drag(MouseButton::Left), 30, 10);
    c.mouse(MouseKind::Up(MouseButton::Left), 30, 10);
    c.wait_for("the separator at column 30", |s| s[1].chars().nth(30) == Some('▏'));
    c.run("stty size");
    c.wait_text("27 29");
}

#[test]
fn zoom_sync_and_layouts() {
    let h = Harness::new();
    let mut c = h.attach(100, 30);
    c.prefix('\\');
    c.wait_for("a split", |s| s[1].contains('▕'));

    c.prefix('z');
    c.wait_for("zoom", |s| s.last().unwrap().contains("ZOOM"));
    assert!(c.vsep().is_none());
    c.prefix('z');
    c.wait_for("unzoom", |s| !s.last().unwrap().contains("ZOOM") && s[1].contains('▕'));

    c.prefix('a');
    c.wait_for("the all-panes badge", |s| s.last().unwrap().contains("ALL PANES"));
    c.run("echo both-$((11+11))");
    c.wait_for("the command in both panes", |s| s.iter().any(|l| l.matches("both-22").count() == 2));
    c.prefix('a');
    c.wait_for("typing in one pane again", |s| !s.last().unwrap().contains("ALL PANES"));

    c.prefix('l');
    c.wait_text("layout: even-horizontal");
    c.prefix('l');
    c.wait_text("layout: even-vertical");
    assert!(c.has_hsep());
}

#[test]
fn actions_from_the_command_line() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    assert!(h.nerdmux(&["action", "split-down"]).status.success());
    c.wait_for("a horizontal separator", |s| s.iter().any(|l| l.contains("▁▁▁")));
    assert!(h.nerdmux(&["action", "new-window"]).status.success());
    c.wait_for("a second tab", |s| s.last().unwrap().contains("2 sh"));
    assert!(h.nerdmux(&["action", "select-window", "1"]).status.success());
    c.wait_for("the split window", |s| s.iter().any(|l| l.contains("▁▁▁") || l.contains("▔▔▔")));
    let out = h.nerdmux(&["action", "no-such-action"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown action"));
}

#[test]
fn command_palette_runs_actions() {
    let h = Harness::new();
    let mut c = h.attach(100, 30);
    c.prefix('p');
    c.wait_text("Commands");
    c.typed("split right");
    c.wait_for("the filtered list", |s| s.iter().any(|l| l.contains("split right") && l.contains("Alt+Space |")));
    c.code(KeyCode::Enter);
    c.wait_for("a split", |s| s[1].contains('▕'));
    c.wait_gone("Commands");
}

#[test]
fn pasted_text_reaches_the_shell() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    // Shells with bracketed paste (bash 5.1 and later, zsh, fish) insert a
    // pasted newline instead of running the line, as in any terminal; the
    // command runs on Enter.
    c.send(C2S::Paste("echo pasted-$((2+3))".into()));
    c.wait_text("echo pasted-$((2+3))");
    c.code(KeyCode::Enter);
    c.wait_text("pasted-5");
}

#[test]
fn config_file_changes_prefix_status_and_keys() {
    let h = Harness::with_config(
        r#"
        prefix = "ctrl+a"
        status = "top"
        [keys.prefix]
        "v" = "split-right"
        [keys.direct]
        "f5" = "new-window"
        "alt+t" = "none"
        "#,
    );
    let mut c = h.attach(100, 30);
    assert!(c.screen()[0].contains("main"), "status bar should be on top:\n{}", c.dump());

    c.key(Key::ctrl('a'));
    c.typed("v");
    c.wait_for("a split", |s| s[2].contains('▕'));

    c.code(KeyCode::F(5));
    c.wait_for("a second tab", |s| s[0].contains("2 sh"));

    // Unbound Alt+t goes to the shell as ESC t instead of opening a window.
    c.alt('t');
    c.pump(Duration::from_millis(300));
    assert!(!c.screen()[0].contains("3 sh"));

    // The old prefix is now just a key for the program in the pane.
    c.key(Key::ctrl('b'));
    c.typed("c");
    c.pump(Duration::from_millis(300));
    assert!(!c.screen()[0].contains("3 sh"));
}

#[test]
fn full_screen_programs_get_their_own_screen() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    c.run("echo before-$((1+0))");
    c.wait_text("before-1");
    // Enter the alternate screen, draw at a fixed position, then leave.
    c.run("printf '\\033[?1049h\\033[2J\\033[5;10HALT-SCREEN'; sleep 1; printf '\\033[?1049l'");
    c.wait_for("alternate screen content", |s| s[4].get(9..) == Some("ALT-SCREEN"));
    assert!(!c.screen().iter().any(|l| l.contains("before-1")));
    c.wait_for("the normal screen again", |s| s[4].is_empty() && s.iter().any(|l| l.contains("before-1")));
}

#[test]
fn rejects_strangers_politely() {
    let h = Harness::new();
    // Wrong protocol version.
    let mut stream = UnixStream::connect(h.socket()).unwrap();
    let hello = C2S::Hello {
        version: PROTOCOL_VERSION + 1,
        cols: 80,
        rows: 24,
        cwd: "/".into(),
        env: Vec::new(),
        request: Request::List,
    };
    write_msg(&mut stream, &hello).unwrap();
    match read_msg::<_, S2C>(&mut stream).unwrap() {
        S2C::Exit { message, code } => {
            assert_eq!(code, 1);
            assert!(message.contains("kill-server"), "{message:?}");
        }
        other => panic!("unexpected reply {other:?}"),
    }

    // Unknown session.
    let mut c = h.connect(80, 24, Request::Attach { target: Some("ghost".into()), create: false });
    let (message, code) = c.wait_exit();
    assert_eq!(code, 1);
    assert!(message.contains("no session named"), "{message:?}");

    // Garbage on the socket must not disturb the server.
    {
        use std::io::Write;
        let mut junk = UnixStream::connect(h.socket()).unwrap();
        junk.write_all(&[0xff; 64]).unwrap();
    }
    let mut c = h.attach(80, 24);
    c.run("echo alive-$((3*3))");
    c.wait_text("alive-9");
}

#[test]
fn two_clients_share_a_session() {
    let h = Harness::new();
    let mut a = h.attach(80, 24);
    let mut b = h.attach(80, 24);
    a.run("echo shared-$((4*4))");
    a.wait_text("shared-16");
    b.wait_text("shared-16");
    b.prefix('t');
    a.wait_for("the window opened by the other client", |s| s.last().unwrap().contains("2 sh"));
    assert_eq!(h.stdout(&["ls"]).trim(), "main: 2 windows, 2 panes (attached)");
    b.prefix('d');
    b.wait_exit();
    a.run("echo still-$((5*5))");
    a.wait_text("still-25");
}

#[test]
fn repeat_keys_do_not_swallow_typing() {
    let h = Harness::new();
    let mut c = h.attach(100, 30);
    c.prefix('\\');
    c.wait_for("a split", |s| s[1].contains('▕'));
    let before = c.vsep().unwrap();

    // `prefix Ctrl+←` resizes and stays armed: another one resizes again...
    c.key(Key::alt(' '));
    c.chord(KeyCode::Left, CTRL);
    c.chord(KeyCode::Left, CTRL);
    c.wait_for("the border to move twice", |s| s[1].chars().position(|ch| ch == '▕') == Some(before - 10));
    // ...but x is not a repeat key, so it is typed rather than treated as
    // `prefix x` (close pane).
    c.typed("x");
    c.pump(Duration::from_millis(300));
    assert!(!c.screen().iter().any(|l| l.contains("Close this pane")), "{}", c.dump());
    c.wait_for("the x at the prompt", |s| s.iter().any(|l| l.contains("$ x")));
}

#[test]
fn emoji_and_wide_text_stay_aligned() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    c.run("printf '\\360\\237\\230\\200ab|\\346\\227\\245\\346\\234\\254|e\\314\\201x|\\n'");
    c.wait_for("the printed line", |s| s.iter().any(|l| l == "😀ab|日本|e\u{301}x|" || l == "😀ab|日本|ex|"));
    // Overwrite the same cells with plain text: nothing stale may remain.
    c.run("clear; echo plain-$((6+6))");
    c.wait_for("a clean screen", |s| s[0] == "plain-12" && !s.iter().any(|l| l.contains('日') || l.contains('😀')));
}

#[test]
fn sets_the_terminal_title() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    // The shell's own title is ignored: shells set one at every prompt.
    c.run("printf '\\033]2;shell-title\\007'");
    // A program's counts while it is in the foreground.
    c.run("sh -c \"printf '\\033]2;custom-title\\007'; sleep 1\"");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        c.pump(Duration::from_millis(20));
        let raw = String::from_utf8_lossy(&c.raw).into_owned();
        if raw.contains("\x1b]2;main › sh\x07") && raw.contains("\x1b]2;main › custom-title\x07") {
            assert!(!raw.contains("\x1b]2;main › shell-title"), "the shell's title was shown");
            break;
        }
        assert!(Instant::now() < deadline, "titles not seen in the output");
    }
}

#[test]
fn a_stalled_client_does_not_hold_up_the_others() {
    let h = Harness::new();
    // This client attaches and then never reads a single byte.
    let mut stalled = UnixStream::connect(h.socket()).unwrap();
    let hello = C2S::Hello {
        version: PROTOCOL_VERSION,
        cols: 80,
        rows: 24,
        cwd: "/".into(),
        env: h.env(),
        request: Request::Attach { target: Some("main".into()), create: false },
    };
    write_msg(&mut stalled, &hello).unwrap();

    let mut c = h.attach(80, 24);
    let started = Instant::now();
    c.run("seq 1 200000 | tail -n 3; echo done-$((20+1))");
    c.wait_text("done-21");
    assert!(started.elapsed() < Duration::from_secs(8), "output crawled: {:?}", started.elapsed());
    c.run("echo responsive-$((30+1))");
    c.wait_text("responsive-31");

    // Once the slow client reads again it ends up with the same picture.
    let mut late = Client::new(stalled, 80, 24);
    late.wait_text("responsive-31");
    late.wait_for("the same screen as the healthy client", |s| s[..23] == c.screen()[..23]);
}

/// Throw a few thousand random keys, clicks, pastes and resizes at the
/// server. It must neither die nor report an internal error.
#[test]
fn survives_random_input() {
    // `cat` as the shell: random typing cannot run anything.
    let h = Harness::start("", "/bin/cat");
    let mut c = h.connect(80, 24, Request::Attach { target: Some("main".into()), create: false });
    c.wait_for("the status bar", |s| s.last().unwrap().contains("main"));

    // NERDMUX_FUZZ_SEED / NERDMUX_FUZZ_EVENTS let you explore other runs.
    let env_num = |name: &str, default: u64| std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
    let mut seed = env_num("NERDMUX_FUZZ_SEED", 0x9e3779b97f4a7c15).max(1);
    let events = env_num("NERDMUX_FUZZ_EVENTS", 3000);
    let mut rnd = |n: u32| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as u32
    };
    let arrows =
        [KeyCode::Left, KeyCode::Right, KeyCode::Up, KeyCode::Down, KeyCode::PageUp, KeyCode::Home, KeyCode::End];
    let plain = [KeyCode::Enter, KeyCode::Esc, KeyCode::Tab, KeyCode::PageDown, KeyCode::Backspace, KeyCode::F(3)];
    let bound: Vec<Key> = "123456789"
        .chars()
        .map(Key::alt)
        .chain("|\\-xzrlgbaktwn<>sfSNdcvp?hR123456789".chars().map(Key::ch))
        .chain("afclrg".chars().map(Key::ctrl))
        .chain(arrows.iter().flat_map(|code| {
            [0, ALT, SHIFT, CTRL, ALT | SHIFT, CTRL | ALT, CTRL | SHIFT, SUPER].map(|mods| Key::new(*code, mods))
        }))
        .chain([Key::new(KeyCode::Enter, ALT), Key::new(KeyCode::F(3), SHIFT)])
        .chain(plain.map(Key::plain))
        .collect();
    let (mut cols, mut rows) = (80u16, 24u16);
    let mut detached = 0;
    for i in 0..events {
        match rnd(100) {
            0..=44 => c.key(bound[rnd(bound.len() as u32) as usize]),
            45..=59 => c.key(Key::alt(' ')),
            60..=69 => c.key(Key::ch(char::from(b'a' + rnd(26) as u8))),
            70..=84 => {
                let button = [MouseButton::Left, MouseButton::Middle, MouseButton::Right][rnd(3) as usize];
                let kind = match rnd(6) {
                    0 => MouseKind::Down(button),
                    1 => MouseKind::Up(button),
                    2 => MouseKind::Drag(button),
                    3 => MouseKind::ScrollUp,
                    4 => MouseKind::ScrollDown,
                    _ => MouseKind::Moved,
                };
                c.mouse(kind, rnd(cols as u32 + 3) as u16, rnd(rows as u32 + 3) as u16);
            }
            85..=92 => {
                cols = if rnd(4) == 0 { 1 + rnd(6) as u16 } else { 1 + rnd(140) as u16 };
                rows = if rnd(4) == 0 { 1 + rnd(4) as u16 } else { 1 + rnd(50) as u16 };
                c.send(C2S::Resize(cols, rows));
                c.term.resize(Size(cols.max(2) as usize, rows as usize));
            }
            _ => c.send(C2S::Paste("pasted text\nwith lines 日本 😀\n".into())),
        }
        if i % 50 == 0 {
            c.pump(Duration::from_millis(5));
        }
        match c.exit.take() {
            // Random keys found `detach`: come back and carry on.
            Some((message, _)) if message.contains("detached") => {
                detached += 1;
                c = h.connect(cols, rows, Request::Attach { target: None, create: false });
                c.pump(Duration::from_millis(50));
            }
            Some(other) => {
                c.exit = Some(other);
                break;
            }
            None => {}
        }
    }
    println!("fuzz: {detached} detaches, final exit {:?}, sessions: {:?}", c.exit, h.stdout(&["ls"]));
    c.pump(Duration::from_millis(300));
    let log = h.server_log();
    assert!(!log.contains("internal error") && !log.contains("panicked"), "server log:\n{log}");
    // Either the session is still there, or random keys legitimately ended it.
    if c.exit.is_none() {
        assert!(h.nerdmux(&["ls"]).status.success());
    }
}

#[test]
fn arrange_mode_resizes_and_moves_panes_with_arrows() {
    let h = Harness::new();
    let mut c = h.attach(100, 30);
    c.run("echo first-$((1+0))");
    c.prefix('\\');
    c.wait_for("a split", |s| s[1].contains('▕'));
    let before = c.vsep().unwrap();
    c.run("echo second-$((1+1))");
    c.wait_text("second-2");

    c.prefix('r');
    c.wait_for("the arrange hint", |s| s.iter().any(|l| l.contains("ARRANGE") && l.contains("move pane")));
    c.code(KeyCode::Left);
    c.code(KeyCode::Left);
    c.wait_for("the border two steps left", |s| s[1].chars().position(|ch| ch == '▕') == Some(before - 4));
    c.chord(KeyCode::Right, SHIFT);
    c.wait_for("a bigger step right", |s| s[1].chars().position(|ch| ch == '▕') == Some(before + 4));

    // Ctrl+← moves the focused pane to the other side; it keeps the focus.
    assert!(c.col_of("second-2").unwrap() > c.col_of("first-1").unwrap());
    c.chord(KeyCode::Left, CTRL);
    c.wait_for("the panes to trade places", |s| {
        let col = |n: &str| s.iter().find_map(|l| l.find(n));
        matches!((col("second-2"), col("first-1")), (Some(a), Some(b)) if a < b)
    });
    c.code(KeyCode::Enter);
    c.wait_for("arrange mode to end", |s| !s.iter().any(|l| l.contains("ARRANGE")));
    c.run("echo still-here-$((2+2))");
    c.wait_text("still-here-4");
    assert!(c.col_of("still-here-4").unwrap() < c.col_of("first-1").unwrap());

    // The direct chord resizes without entering the mode.
    let at = c.screen()[1].chars().position(|ch| ch == '▏').unwrap();
    c.chord(KeyCode::Right, CTRL | ALT);
    c.wait_for("the border to follow Ctrl+Alt+→", |s| s[1].chars().position(|ch| ch == '▏') == Some(at + 3));

    // Any other chord leaves the mode and still does its job.
    c.prefix('r');
    c.wait_for("arrange mode", |s| s.iter().any(|l| l.contains("ARRANGE")));
    c.prefix('t');
    c.wait_for("a new window and no arrange badge", |s| {
        s.last().unwrap().contains("2 sh") && !s.iter().any(|l| l.contains("ARRANGE"))
    });
}

#[test]
fn letters_are_never_navigation() {
    let h = Harness::new();
    let mut c = h.attach(100, 30);
    c.prefix('\\');
    c.wait_for("a split", |s| s[1].contains('▕'));
    c.run("echo right-$((5+5))");
    c.wait_text("right-10");
    // Alt+letters belong to the program in the pane: no focus change, no
    // help, no layout change.
    for letter in ['h', 'j', 'k', 'l'] {
        c.alt(letter);
    }
    c.pump(Duration::from_millis(200));
    c.code(KeyCode::Backspace);
    c.key(Key::ctrl('u'));
    c.run("echo same-pane-$((6+6))");
    c.wait_text("same-pane-12");
    assert!(c.col_of("same-pane-12").unwrap() > c.vsep().unwrap(), "focus moved:\n{}", c.dump());
    assert!(!c.status().contains("layout:"), "Alt+l must not cycle the layout:\n{}", c.dump());
    assert!(!c.screen().iter().any(|l| l.contains("Chords work any time")), "{}", c.dump());
}

#[test]
fn super_chords_work_when_bound() {
    let h = Harness::with_config(
        r#"
        [keys.direct]
        "super+left" = "focus left"
        "super+t" = "new-window"
        "ctrl+shift+w" = "kill-window"
        "#,
    );
    let mut c = h.attach(100, 30);
    c.prefix('\\');
    c.wait_for("a split", |s| s[1].contains('▕'));
    c.chord(KeyCode::Left, SUPER);
    c.run("echo via-super-$((7+7))");
    c.wait_text("via-super-14");
    assert!(c.col_of("via-super-14").unwrap() < c.vsep().unwrap());

    c.key(Key::new(KeyCode::Char('t'), SUPER));
    c.wait_for("a second tab", |s| s.last().unwrap().contains("2 sh"));
    // Terminals with the kitty protocol report Ctrl+Shift+w as `w` plus
    // both modifier bits; that must match the binding too.
    c.key(Key::new(KeyCode::Char('w'), CTRL | SHIFT));
    c.wait_text("Close window 2");
    c.typed("y");
    c.wait_for("one tab again", |s| !s.last().unwrap().contains("2 sh"));
    // An unbound Super chord is dropped rather than typed.
    c.key(Key::new(KeyCode::Char('q'), SUPER));
    c.run("echo clean-$((8+8))");
    c.wait_text("clean-16");
}

#[test]
fn theme_is_amber_on_dark_with_fallback_for_256_colors() {
    let h = Harness::new();
    let attach = Request::Attach { target: Some("main".into()), create: false };

    // A true-color terminal gets the exact palette.
    let mut rich = h.connect_env(80, 24, attach.clone(), &[("COLORTERM", "truecolor")]);
    rich.wait_for("the status bar", |s| s.last().unwrap().contains("main"));
    let raw = rich.raw_text();
    assert!(raw.contains("48;2;21;18;15"), "dark background missing");
    assert!(raw.contains("38;2;230;197;143"), "amber text color missing");
    assert!(raw.contains("48;2;232;162;58"), "amber active tab missing");
    assert!(raw.contains("48;2;29;25;21"), "status bar background missing");
    assert!(raw.contains("\x1b]12;#e8a23a\x07"), "cursor is not tinted amber");
    assert!(raw.contains("\x1b]11;#15120f\x07"), "terminal background is not tinted");

    // Without it, the nearest of the 256 standard colors is used.
    let mut plain = h.connect_env(80, 24, attach, &[]);
    plain.wait_for("the status bar", |s| s.last().unwrap().contains("main"));
    let raw = plain.raw_text();
    assert!(!raw.contains("38;2;") && !raw.contains("48;2;"), "24-bit color sent to a 256-color terminal");
    assert!(raw.contains("48;5;179"), "amber fallback missing");
    assert!(raw.contains("48;5;233"), "dark fallback missing");

    // Colors printed by programs land in the theme palette: ANSI green is #A3B57A.
    rich.run("printf '\\033[32mgreen-text\\033[0m\\n'");
    rich.wait_text("green-text");
    assert!(rich.raw_text().contains("38;2;163;181;122"));
}

#[test]
fn panes_can_keep_the_terminals_own_colors() {
    let h = Harness::with_config("[theme]\nown_palette = false\n");
    let mut c = h.connect_env(
        80,
        24,
        Request::Attach { target: Some("main".into()), create: false },
        &[("COLORTERM", "truecolor")],
    );
    c.wait_for("the status bar", |s| s.last().unwrap().contains("main"));
    c.run("printf '\\033[32mgreen-text\\033[0m\\n'");
    c.wait_text("green-text");
    let raw = c.raw_text();
    assert!(raw.contains("\x1b[32m") || raw.contains(";32m"), "ANSI green should pass through as index 2");
    assert!(!raw.contains("48;2;21;18;15"), "the theme background must not be painted");
    assert!(!raw.contains("\x1b]11;#"), "the terminal background must not be tinted");
}

#[test]
fn closing_a_session_goes_back_or_starts_afresh() {
    let h = Harness::new();
    assert!(h.nerdmux(&["new", "-d", "-s", "other"]).status.success());
    let mut c = h.attach(100, 30);
    let session = |c: &Client| c.status().split_whitespace().next().unwrap_or_default().to_string();
    let close = |c: &mut Client| {
        c.key(Key::alt(' '));
        c.typed("X");
        c.wait_text("Kill session");
        c.typed("y");
        c.wait_gone("Kill session");
    };

    // From main, a new session; closing it goes back to main, where the
    // client came from, rather than to `other`.
    c.prefix('S');
    c.wait_text("New session name");
    c.typed("work");
    c.code(KeyCode::Enter);
    c.wait_for("the new session", |s| s.last().unwrap().trim_start().starts_with("work"));
    close(&mut c);
    c.wait_for("back in main", |s| s.last().unwrap().trim_start().starts_with("main"));

    // Closing the others leaves the last one; closing that starts a fresh
    // session instead of ending nerdmux.
    assert!(h.nerdmux(&["kill-session", "-t", "other"]).status.success());
    c.run("echo old-''shell");
    c.wait_text("old-shell");
    close(&mut c);
    c.wait_gone("old-shell");
    c.wait_for("a shell in the fresh session", |s| s.iter().any(|l| l.starts_with('$')));
    assert_eq!(session(&c), "0");
    assert_eq!(h.stdout(&["ls"]).trim(), "0: 1 window, 1 pane (attached)");
}

#[test]
fn session_manager_creates_renames_and_kills() {
    let h = Harness::new();
    let mut c = h.attach(100, 30);
    c.run("echo in-main-$((3+3))");
    c.wait_text("in-main-6");

    // n: a new session, named at the prompt, takes over the client.
    c.prefix('s');
    c.wait_text("Enter switch · n new · r rename · x kill");
    c.typed("n");
    c.wait_text("New session name");
    c.typed("work");
    c.code(KeyCode::Enter);
    c.wait_for("the new session", |s| s.last().unwrap().trim_start().starts_with("work"));

    // r: rename the selected session; the manager comes back afterwards.
    c.prefix('s');
    c.wait_text("Enter switch");
    c.code(KeyCode::Up);
    c.typed("r");
    c.wait_text("Rename session");
    c.key(Key::ctrl('u'));
    c.typed("first");
    c.code(KeyCode::Enter);
    c.wait_for("the renamed session in the manager", |s| {
        s.iter().any(|l| l.contains("Enter switch")) && s.iter().any(|l| l.contains("first"))
    });

    // x: kill it, after confirming; the manager stays open.
    c.typed("x");
    c.wait_text("Kill session first");
    c.typed("y");
    c.wait_for("the manager without the killed session", |s| {
        s.iter().any(|l| l.contains("Enter switch")) && !s.iter().any(|l| l.contains("first"))
    });
    c.code(KeyCode::Esc);
    c.wait_gone("Enter switch");
    assert_eq!(h.stdout(&["ls"]).trim(), "work: 1 window, 1 pane (attached)");
}

#[test]
fn status_bar_shows_directory_branch_and_panes() {
    let h = Harness::new();
    let repo = std::env::temp_dir().join(format!("nerdmux-status-{}", std::process::id()));
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/topic\n").unwrap();
    let mut c = h.attach(120, 30);
    c.run(&format!("cd {}", repo.display()));
    c.wait_for("the branch in the status bar", |s| s.last().unwrap().contains("topic"));
    assert!(c.status().contains(&format!("nerdmux-status-{}", std::process::id())), "{}", c.status());
    c.prefix('\\');
    c.wait_for("the pane count", |s| s.last().unwrap().contains("2 panes"));
    if std::env::var_os("NERDMUX_SHOW").is_some() {
        c.prefix('?');
        c.wait_text("Chords work any time");
        eprintln!("{}", c.dump());
        c.code(KeyCode::Esc);
        c.key(Key::alt(' '));
        c.wait_text("Alt+Space then…");
        eprintln!("{}", c.dump());
        c.code(KeyCode::Esc);
        c.prefix('s');
        c.wait_text("Enter switch");
        eprintln!("{}", c.dump());
        c.code(KeyCode::Esc);
        c.prefix(',');
        c.wait_text("Enter change");
        eprintln!("{}", c.dump());
    }
    std::fs::remove_dir_all(&repo).unwrap();
}

#[test]
fn underline_styles_and_links_reach_terminals_that_show_them() {
    let h = Harness::new();
    let attach = Request::Attach { target: Some("main".into()), create: false };
    // The words are split in the command so only the output contains them.
    let print = "printf '\\033[4:3;58:2::208:112:94m%s%s\\033[0m \\033]8;;https://example.com/x\\033\\\\%s%s\\033]8;;\\033\\\\\\n' cur ly link ed";

    let mut rich = h.connect_env(100, 24, attach.clone(), &[("TERM", "xterm-kitty"), ("COLORTERM", "truecolor")]);
    rich.wait_for("the shell prompt", |s| s.iter().any(|l| l.starts_with('$')));
    rich.run(print);
    rich.wait_text("curly linked");
    let curly = rich.cell_at("curly");
    assert!(curly.flags.contains(Flags::UNDERCURL), "{:?}", curly.flags);
    assert_eq!(format!("{:?}", curly.underline_color()), "Some(Spec(Rgb { r: 208, g: 112, b: 94 }))");
    assert!(curly.hyperlink().is_none());
    let linked = rich.cell_at("linked");
    assert_eq!(linked.hyperlink().map(|l| l.uri().to_string()).as_deref(), Some("https://example.com/x"));
    assert!(rich.cell_at("$").hyperlink().is_none(), "the link leaked past its text");

    // An unknown terminal gets a plain underline and no link.
    let mut plain = h.connect_env(100, 24, attach, &[("TERM", "xterm-256color")]);
    plain.wait_text("curly linked");
    let curly = plain.cell_at("curly");
    assert!(curly.flags.contains(Flags::UNDERLINE) && !curly.flags.contains(Flags::UNDERCURL), "{:?}", curly.flags);
    assert!(curly.underline_color().is_none());
    assert!(plain.cell_at("linked").hyperlink().is_none());
    assert!(!plain.raw_text().contains("\x1b]8;"), "a link was sent to a terminal that may not take it");
}

#[test]
fn programs_can_ask_which_attributes_are_set() {
    let h = Harness::new();
    let mut c = h.attach(100, 24);
    // Set bold and a curly underline, ask (DECRQSS), and print the reply
    // the pane sends back as input.
    c.run("stty -icanon -echo min 0 time 10; printf '\\033[1;4:3m\\033P$qm\\033\\\\\\033[0m'; dd bs=64 count=1 2>/dev/null | od -An -c; stty sane");
    c.wait_text("4   :   3");
    let screen = c.screen().join("\n");
    assert!(screen.contains("033   P   1   $   r   0   ;   1   ;   4   :   3   m 033   \\"), "{screen}");
}

#[test]
fn mouse_reporting_survives_programs_that_track_all_motion() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    assert!(c.term.mode().contains(TermMode::MOUSE_DRAG), "drag reporting is on from the start");
    // A program asks for every movement and then stops. Terminals keep one
    // mouse mode at a time, so the drag mode must be asked for again.
    c.run("printf '\\033[?1003h'; sleep 0.3; printf '\\033[?1003l'; echo motion-''done");
    c.wait_text("motion-done");
    c.pump(Duration::from_millis(200));
    let mode = *c.term.mode();
    assert!(mode.intersects(TermMode::MOUSE_MODE), "the terminal no longer reports the mouse: {mode:?}");
    assert!(!mode.contains(TermMode::MOUSE_MOTION), "{mode:?}");
}

/// Two windows: the first split, with output, a directory and a command
/// that may not run again; the second running `less`, which may.
fn build_sessions_to_lose(h: &Harness) -> Client {
    let work = h.dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(h.dir.join("notes.txt"), "notes-content\n").unwrap();
    let mut c = h.attach(100, 30);
    c.run(&format!("cd {}", work.display()));
    c.run("echo before-''crash");
    c.wait_text("before-crash");
    c.prefix('\\');
    c.wait_for("a second pane", |s| s.last().unwrap().contains("2 panes"));
    c.run("sleep 300");
    c.prefix('t');
    c.wait_for("a second window", |s| s.last().unwrap().contains("2 "));
    c.run(&format!("less {}", h.dir.join("notes.txt").display()));
    c.wait_text("notes-content");
    let mut expected = vec!["before-crash", "/work"];
    if PROGRAMS_COME_BACK {
        expected.extend(["\"sleep\"", "\"less\""]);
    }
    h.wait_snapshot(&expected);
    c
}

/// Running programs are found through /proc, so only Linux restores them;
/// elsewhere panes come back with their directory and text.
const PROGRAMS_COME_BACK: bool = cfg!(target_os = "linux");

/// Restore what `build_sessions_to_lose` made, through the picker that a
/// new server offers, and check it came back.
fn restore_and_check(h: &Harness) {
    h.start_server();
    let mut c = h.connect(100, 30, Request::Attach { target: None, create: true });
    c.wait_text("Restore sessions?");
    assert!(c.screen().join("\n").contains("2 windows, 3 panes"), "{}", c.dump());
    c.code(KeyCode::Enter);
    c.wait_for("the restored session", |s| s.last().unwrap().contains("main"));
    if PROGRAMS_COME_BACK {
        // The second window had the focus and runs `less` again.
        c.wait_text("notes-content");
    }
    c.prefix('1');
    c.wait_text("before-crash");
    assert!(c.vsep().is_some(), "the split is back:\n{}", c.dump());
    // `sleep` is typed again, waiting for Enter, in the right-hand pane
    // that had the focus, below the old text that shows it once already.
    let typed = if PROGRAMS_COME_BACK { 2 } else { 1 };
    c.wait_for("the typed command", |s| s.iter().filter(|l| l.contains("$ sleep 300")).count() == typed);
    c.key(Key::new(KeyCode::Char('u'), CTRL));
    c.run("pwd");
    c.wait_for("the old directory", |s| s.iter().any(|l| l.contains("/work") && !l.contains("cd ")));
    // The empty session made for this client is gone; only `main` is left.
    let list = h.stdout(&["ls"]);
    assert!(list.starts_with("main:") && list.lines().count() == 1, "{list}");
}

#[test]
fn sessions_come_back_after_the_server_dies() {
    let h = Harness::new();
    let _c = build_sessions_to_lose(&h);
    let pid = h.server_pid();
    // SAFETY: plain signal to a process this test started.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    h.wait_server_gone(pid);
    restore_and_check(&h);
}

#[test]
fn sessions_come_back_after_a_reboot() {
    let h = Harness::new();
    let _c = build_sessions_to_lose(&h);
    let pid = h.server_pid();
    // A reboot ends everything at once: the shells and the server.
    for shell in h.shell_pids(pid) {
        unsafe { libc::kill(shell, libc::SIGKILL) };
    }
    unsafe { libc::kill(pid, libc::SIGTERM) };
    h.wait_server_gone(pid);
    restore_and_check(&h);
}

#[test]
fn unnamed_sessions_keep_their_number() {
    let h = Harness::new();
    // An unnamed session is `0`, like the one a new server makes for its
    // first client.
    assert!(h.nerdmux(&["new", "-d"]).status.success());
    assert!(h.nerdmux(&["kill-session", "-t", "main"]).status.success());
    h.wait_snapshot(&["name = \"0\""]);
    let crash = |h: &Harness| {
        let pid = h.server_pid();
        unsafe { libc::kill(pid, libc::SIGKILL) };
        h.wait_server_gone(pid);
        h.start_server();
    };
    let sessions =
        |h: &Harness| h.stdout(&["ls"]).lines().map(|l| l.split(':').next().unwrap().to_string()).collect::<Vec<_>>();

    crash(&h);
    let mut c = h.connect(80, 24, Request::Attach { target: None, create: true });
    c.wait_text("Restore sessions?");
    c.code(KeyCode::Enter);
    c.wait_gone("Restore sessions?");
    c.pump(Duration::from_millis(200));
    assert_eq!(sessions(&h), ["0"]);

    // Starting fresh first, then restoring: the name is taken by then.
    h.wait_snapshot(&["name = \"0\""]);
    drop(c);
    crash(&h);
    let mut c = h.connect(80, 24, Request::Attach { target: None, create: true });
    c.wait_text("Restore sessions?");
    c.code(KeyCode::Esc);
    c.wait_gone("Restore sessions?");
    assert!(h.nerdmux(&["action", "restore-sessions"]).status.success());
    c.wait_text("Restore sessions?");
    c.code(KeyCode::Enter);
    c.wait_gone("Restore sessions?");
    c.pump(Duration::from_millis(200));
    let mut names = sessions(&h);
    names.sort();
    assert_eq!(names, ["0", "0-2"]);
}

#[test]
fn nothing_is_offered_after_a_clean_exit() {
    let h = Harness::new();
    let mut c = h.attach(80, 24);
    c.run("echo kept-''once");
    h.wait_snapshot(&["kept-once"]);
    let pid = h.server_pid();
    let out = h.nerdmux(&["kill-server"]);
    assert!(out.status.success());
    h.wait_server_gone(pid);
    assert!(h.snapshot().is_none(), "kill-server leaves no snapshot");

    // Leaving the last shell is a clean end too, after a short wait.
    h.start_server();
    let mut c = h.connect(80, 24, Request::Attach { target: None, create: true });
    c.wait_for("the shell prompt", |s| s.iter().any(|l| l.starts_with('$')));
    c.pump(Duration::from_millis(300));
    assert!(!c.screen().join("\n").contains("Restore sessions?"), "{}", c.dump());
    h.wait_snapshot(&["sessions"]);
    let pid = h.server_pid();
    c.run("exit");
    h.wait_server_gone(pid);
    assert!(h.snapshot().is_none(), "the snapshot outlived a clean exit");
    let restorable = h.dir.join(".local/state/nerdmux");
    let left: Vec<_> = std::fs::read_dir(&restorable).map(|d| d.filter_map(|e| e.ok()).collect()).unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn settings_panel_edits_the_config_file() {
    let h = Harness::with_config("# my own comment\nmouse = true\n");
    let file = || std::fs::read_to_string(h.dir.join("config.toml")).unwrap();
    let mut c = h.attach(110, 34);
    c.prefix('\\');
    c.wait_for("a split", |s| s[1].contains('▕'));
    c.prefix(',');
    c.wait_text("Enter change");
    // The about line, from Cargo.toml.
    c.wait_text(&format!("NërdMUX {} · Pavel Kuksa · MIT · github.com/Ndles/nerdmux", env!("CARGO_PKG_VERSION")));

    // A number: Enter opens it for typing.
    c.typed("delay");
    c.wait_for("the filtered list", |s| !s.iter().any(|l| l.contains("dim inactive")));
    c.code(KeyCode::Enter);
    c.key(Key::ctrl('u'));
    c.typed("150");
    c.code(KeyCode::Enter);
    c.wait_for("the new value", |s| s.iter().any(|l| l.contains("150 ms")));
    assert!(file().contains("which_key_delay_ms = 150"), "{}", file());

    // A color, written under [theme].
    c.code(KeyCode::Esc);
    c.typed("accent");
    c.code(KeyCode::Enter);
    c.key(Key::ctrl('u'));
    c.typed("#ff8800");
    c.code(KeyCode::Enter);
    c.wait_text("#FF8800");
    assert!(file().contains("[theme]\naccent = \"#ff8800\""), "{}", file());

    // A switch flips with an arrow, and Del puts the default back.
    c.code(KeyCode::Esc);
    c.typed("mouse");
    c.code(KeyCode::Right);
    c.wait_for("mouse off", |s| s.iter().any(|l| l.contains("mouse") && l.contains(" off")));
    assert!(file().contains("mouse = false"), "{}", file());
    c.code(KeyCode::Delete);
    c.wait_for("mouse back on", |s| s.iter().any(|l| l.contains("mouse") && l.contains(" on ")));
    assert!(!file().contains("mouse ="), "{}", file());

    // A key: Enter waits for it; with Alt it becomes a chord.
    c.code(KeyCode::Esc);
    c.typed("zoom pane");
    c.code(KeyCode::Enter);
    c.wait_text("press a key");
    c.key(Key::alt('z'));
    c.wait_for("the new chord", |s| s.iter().any(|l| l.contains("Alt+z") && l.contains("zoom")));
    assert!(file().contains("[keys.direct]\n\"alt+z\" = \"zoom\""), "{}", file());
    c.code(KeyCode::Esc);
    c.code(KeyCode::Esc);
    c.wait_gone("Enter change");
    c.key(Key::alt('z'));
    c.wait_for("zoom", |s| s.last().unwrap().contains("ZOOM"));

    // The user's own lines are untouched.
    assert!(file().starts_with("# my own comment\n"), "{}", file());
}
