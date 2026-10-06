//! The client: puts the terminal in raw mode, forwards input events to the
//! server and writes whatever the server draws.

use crate::proto::{
    read_msg, write_msg, Key, KeyCode, Mouse, MouseButton, MouseKind, Request, ALT, C2S, CTRL, PROTOCOL_VERSION, S2C,
    SHIFT, SUPER,
};
use crate::server::MOUSE_OFF;
use anyhow::{bail, Context};
use crossterm::event::{self, Event, KeyEventKind, KeyModifiers, MouseEventKind};
use std::io::{IsTerminal, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// The server went away before answering; worth one more try, since it was
/// most likely shutting down just as we connected.
#[derive(Debug)]
struct ServerGone;

impl std::fmt::Display for ServerGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the nerdmux server closed the connection")
    }
}

impl std::error::Error for ServerGone {}

/// Save the window title, enter the alternate screen, turn on bracketed paste.
const ENTER: &[u8] = b"\x1b[22;0t\x1b[?1049h\x1b[?2004h\x1b[H\x1b[2J";
/// Undo `ENTER`, and give back the cursor shape, cursor and background colors and title.
const LEAVE_TAIL: &[u8] =
    b"\x1b[?2004l\x1b[0 q\x1b]112\x07\x1b]111\x07\x1b[0m\x1b[?25h\x1b[?2026l\x1b[?1049l\x1b[23;0t";
/// Kitty keyboard protocol: report chords unambiguously (flag 1) together
/// with the shifted character (flag 4). This is what makes Super and
/// Ctrl+Shift chords reach us on terminals that support it.
const KEYS_ON: &[u8] = b"\x1b[>5u";
const KEYS_OFF: &[u8] = b"\x1b[<u";

static EXTENDED_KEYS: AtomicBool = AtomicBool::new(false);

fn restore_terminal() {
    let mut out = std::io::stdout();
    if EXTENDED_KEYS.swap(false, Ordering::SeqCst) {
        let _ = out.write_all(KEYS_OFF);
    }
    let _ = out.write_all(MOUSE_OFF);
    let _ = out.write_all(LEAVE_TAIL);
    let _ = out.flush();
    let _ = crossterm::terminal::disable_raw_mode();
}

fn spawn_server(socket: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = socket.parent().context("socket path has no directory")?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(socket.with_extension("log"))?;
    let exe = std::env::current_exe().context("cannot find my own executable")?;
    let mut cmd = Command::new(exe);
    cmd.arg("__server").arg(socket).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // SAFETY: setsid is async-signal-safe; it detaches the server from this
    // terminal so closing the terminal does not take the sessions with it.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    // The server forks once more and its first process exits at once, so
    // waiting here returns immediately and leaves no zombie behind.
    cmd.spawn().context("cannot start the nerdmux server")?.wait()?;
    Ok(())
}

fn connect(socket: &Path, start: bool) -> anyhow::Result<Option<UnixStream>> {
    crate::server::check_private_dir(socket)?;
    if let Ok(s) = UnixStream::connect(socket) {
        return Ok(Some(s));
    }
    if !start {
        return Ok(None);
    }
    spawn_server(socket)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(s) = UnixStream::connect(socket) {
            return Ok(Some(s));
        }
        thread::sleep(Duration::from_millis(20));
    }
    bail!("the nerdmux server did not start; see {}", socket.with_extension("log").display())
}

fn convert_key(k: event::KeyEvent) -> Option<Key> {
    use event::KeyCode as K;
    if k.kind == KeyEventKind::Release {
        return None;
    }
    let mut mods = 0u8;
    if k.modifiers.contains(KeyModifiers::SHIFT) {
        mods |= SHIFT;
    }
    if k.modifiers.contains(KeyModifiers::ALT) {
        mods |= ALT;
    }
    if k.modifiers.contains(KeyModifiers::CONTROL) {
        mods |= CTRL;
    }
    if k.modifiers.contains(KeyModifiers::SUPER) {
        mods |= SUPER;
    }
    let code = match k.code {
        K::Char(c) => KeyCode::Char(c),
        K::Null => {
            mods |= CTRL;
            KeyCode::Char(' ')
        }
        K::Enter => KeyCode::Enter,
        K::Tab => KeyCode::Tab,
        K::BackTab => KeyCode::BackTab,
        K::Backspace => KeyCode::Backspace,
        K::Esc => KeyCode::Esc,
        K::Left => KeyCode::Left,
        K::Right => KeyCode::Right,
        K::Up => KeyCode::Up,
        K::Down => KeyCode::Down,
        K::Home => KeyCode::Home,
        K::End => KeyCode::End,
        K::PageUp => KeyCode::PageUp,
        K::PageDown => KeyCode::PageDown,
        K::Insert => KeyCode::Insert,
        K::Delete => KeyCode::Delete,
        K::F(n) if (1..=12).contains(&n) => KeyCode::F(n),
        _ => return None,
    };
    Some(Key::new(code, mods))
}

fn convert_mouse(m: event::MouseEvent) -> Option<Mouse> {
    let button = |b: event::MouseButton| match b {
        event::MouseButton::Left => MouseButton::Left,
        event::MouseButton::Middle => MouseButton::Middle,
        event::MouseButton::Right => MouseButton::Right,
    };
    let kind = match m.kind {
        MouseEventKind::Down(b) => MouseKind::Down(button(b)),
        MouseEventKind::Up(b) => MouseKind::Up(button(b)),
        MouseEventKind::Drag(b) => MouseKind::Drag(button(b)),
        MouseEventKind::Moved => MouseKind::Moved,
        MouseEventKind::ScrollUp => MouseKind::ScrollUp,
        MouseEventKind::ScrollDown => MouseKind::ScrollDown,
        _ => return None,
    };
    let mut mods = 0u8;
    if m.modifiers.contains(KeyModifiers::SHIFT) {
        mods |= SHIFT;
    }
    if m.modifiers.contains(KeyModifiers::ALT) {
        mods |= ALT;
    }
    if m.modifiers.contains(KeyModifiers::CONTROL) {
        mods |= CTRL;
    }
    Some(Mouse { kind, col: m.column, row: m.row, mods })
}

fn hello(request: Request, cols: u16, rows: u16) -> C2S {
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".into());
    let env = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .filter(|(k, _)| k != "NERDMUX" && k != "NERDMUX_PANE")
        .collect();
    C2S::Hello { version: PROTOCOL_VERSION, cols, rows, cwd, env, request }
}

/// Send a request that does not take over the terminal and print the answer.
fn one_shot(mut stream: UnixStream, request: Request) -> anyhow::Result<i32> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    write_msg(&mut stream, &hello(request, cols, rows))?;
    loop {
        match read_msg::<_, S2C>(&mut stream) {
            Ok(S2C::Exit { message, code }) => {
                if !message.is_empty() {
                    if code == 0 {
                        println!("{message}");
                    } else {
                        eprintln!("{message}");
                    }
                }
                return Ok(code);
            }
            Ok(S2C::Output(_)) => {}
            Err(_) => return Err(ServerGone.into()),
        }
    }
}

fn interactive(mut stream: UnixStream, request: Request) -> anyhow::Result<i32> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("nerdmux needs a terminal to attach to (stdin and stdout must be a tty)");
    }
    let (cols, rows) = crossterm::terminal::size().context("cannot read the terminal size")?;
    write_msg(&mut stream, &hello(request, cols, rows))?;

    // Wait for the first answer before touching the terminal, so errors
    // such as "no such session" print normally.
    let first = match read_msg::<_, S2C>(&mut stream) {
        Ok(S2C::Exit { message, code }) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            return Ok(code);
        }
        Ok(S2C::Output(bytes)) => bytes,
        Err(_) => return Err(ServerGone.into()),
    };

    // From here on a vanished terminal must not kill us before the
    // terminal state is restored.
    // SAFETY: changing a signal disposition; no handler code is involved.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    crossterm::terminal::enable_raw_mode().context("cannot switch the terminal to raw mode")?;
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));
    // `kill` or a closing terminal should still leave the terminal usable.
    if let Ok(mut signals) =
        signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP])
    {
        thread::Builder::new().name("signals".into()).spawn(move || {
            if signals.forever().next().is_some() {
                restore_terminal();
                std::process::exit(1);
            }
        })?;
    }
    let mut out = std::io::stdout();
    // Ask before drawing anything: the answer arrives on the input stream.
    let extended = crate::config::Config::load().extended_keys
        && crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    out.write_all(ENTER)?;
    if extended {
        EXTENDED_KEYS.store(true, Ordering::SeqCst);
        out.write_all(KEYS_ON)?;
    }
    out.write_all(&first)?;
    out.flush()?;

    let mut input = stream.try_clone()?;
    thread::Builder::new().name("input".into()).spawn(move || loop {
        let msg = match event::read() {
            Ok(Event::Key(k)) => convert_key(k).map(C2S::Key),
            Ok(Event::Mouse(m)) => convert_mouse(m).map(C2S::Mouse),
            Ok(Event::Paste(text)) => Some(C2S::Paste(text)),
            Ok(Event::Resize(c, r)) => Some(C2S::Resize(c, r)),
            Ok(_) => None,
            Err(_) => break,
        };
        if let Some(msg) = msg {
            if write_msg(&mut input, &msg).is_err() {
                break;
            }
        }
    })?;

    let (message, code) = loop {
        match read_msg::<_, S2C>(&mut stream) {
            Ok(S2C::Output(bytes)) => {
                if out.write_all(&bytes).and_then(|_| out.flush()).is_err() {
                    break (String::new(), 1);
                }
            }
            Ok(S2C::Exit { message, code }) => break (message, code),
            Err(_) => break ("[lost the nerdmux server]".to_string(), 1),
        }
    };
    restore_terminal();
    if !message.is_empty() {
        println!("{message}");
    }
    Ok(code)
}

pub fn run(socket: &Path, request: Request) -> anyhow::Result<i32> {
    let attaches = match &request {
        Request::Attach { .. } => true,
        Request::New { detached, .. } => !detached,
        _ => false,
    };
    let may_start = matches!(&request, Request::New { .. } | Request::Attach { create: true, .. });
    if attaches && std::env::var_os("NERDMUX").is_some_and(|v| !v.is_empty()) {
        bail!("already inside nerdmux; use the switcher (Alt+s) to change session, or unset $NERDMUX to nest anyway");
    }
    let mut attempt = 0;
    loop {
        attempt += 1;
        let Some(stream) = connect(socket, may_start)? else {
            if matches!(request, Request::List) {
                println!("no nerdmux server is running");
                return Ok(0);
            }
            eprintln!("nerdmux: no server is running; start one with `nerdmux`");
            return Ok(1);
        };
        let result = if attaches { interactive(stream, request.clone()) } else { one_shot(stream, request.clone()) };
        match result {
            Err(e) if e.is::<ServerGone>() && attempt < 3 => thread::sleep(Duration::from_millis(150)),
            other => return other,
        }
    }
}
