//! Wire protocol between the thin client and the server.
//!
//! Every message is a little-endian `u32` length followed by the body. The
//! body is written by hand (see `Wire` below): integers little-endian,
//! strings and lists as a `u32` length then their contents, enums as a
//! one-byte tag then their fields. Small enough to read in one sitting, and
//! no serialization library between the client and the server.

use std::io::{self, Read, Write};

pub const PROTOCOL_VERSION: u32 = 6;
const MAX_FRAME: u32 = 64 * 1024 * 1024;

pub const SHIFT: u8 = 1;
pub const ALT: u8 = 2;
pub const CTRL: u8 = 4;
/// Only reported by terminals that speak the kitty keyboard protocol.
pub const SUPER: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Esc,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub code: KeyCode,
    pub mods: u8,
}

impl Key {
    pub const fn new(code: KeyCode, mods: u8) -> Self {
        Key { code, mods }
    }

    pub const fn plain(code: KeyCode) -> Self {
        Key { code, mods: 0 }
    }

    pub const fn ch(c: char) -> Self {
        Key { code: KeyCode::Char(c), mods: 0 }
    }

    pub const fn ctrl(c: char) -> Self {
        Key { code: KeyCode::Char(c), mods: CTRL }
    }

    pub const fn alt(c: char) -> Self {
        Key { code: KeyCode::Char(c), mods: ALT }
    }

    /// Canonical form used for binding lookups: shift is carried by the
    /// character itself for printable keys, so it is dropped there.
    pub fn normalized(mut self) -> Self {
        match self.code {
            KeyCode::Char(c) => {
                // Terminals report Shift+w either as `W` or as `w` with the
                // shift bit; settle on the capital letter.
                if self.mods & SHIFT != 0 && c.is_ascii_lowercase() {
                    self.code = KeyCode::Char(c.to_ascii_uppercase());
                }
                self.mods &= !SHIFT;
            }
            KeyCode::BackTab => self.mods &= !SHIFT,
            _ => {}
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    Moved,
    ScrollUp,
    ScrollDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mouse {
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
    pub mods: u8,
}

/// What the client wants from the server. Sent once, inside `Hello`.
#[derive(Debug, Clone)]
pub enum Request {
    /// Attach to `target`, or to the most recently used session. With
    /// `create`, make a session when there is nothing to attach to.
    Attach {
        target: Option<String>,
        create: bool,
    },
    /// Create a session; attach to it unless `detached`.
    New {
        name: Option<String>,
        command: Vec<String>,
        detached: bool,
    },
    List,
    KillSession {
        target: String,
    },
    KillServer,
    /// Run a named action (`split-right`, `new-window`, ...) from the shell.
    Action {
        target: Option<String>,
        pane: Option<u32>,
        action: String,
    },
    /// Attach (starting a session if there is none) and offer the saved
    /// sessions back.
    Restore,
}

#[derive(Debug, Clone)]
pub enum C2S {
    Hello { version: u32, cols: u16, rows: u16, cwd: String, env: Vec<(String, String)>, request: Request },
    Key(Key),
    Mouse(Mouse),
    Paste(String),
    Resize(u16, u16),
}

#[derive(Debug, Clone)]
pub enum S2C {
    /// Raw bytes for the client's terminal.
    Output(Vec<u8>),
    /// The client should restore its terminal, print `message` and exit.
    Exit { message: String, code: i32 },
}

pub fn write_msg<W: Write, T: Wire>(w: &mut W, msg: &T) -> io::Result<()> {
    let mut frame = vec![0u8; 4];
    msg.put(&mut frame);
    let len = (frame.len() - 4) as u32;
    frame[..4].copy_from_slice(&len.to_le_bytes());
    w.write_all(&frame)?;
    w.flush()
}

pub fn read_msg<R: Read, T: Wire>(r: &mut R) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > MAX_FRAME {
        return Err(bad("frame too large"));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    let mut rest = body.as_slice();
    let msg = T::get(&mut rest)?;
    if !rest.is_empty() {
        return Err(bad("trailing bytes in message"));
    }
    Ok(msg)
}

// ===== encoding =============================================================

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// A value that can be written to and read from a message body.
pub trait Wire: Sized {
    fn put(&self, out: &mut Vec<u8>);
    fn get(input: &mut &[u8]) -> io::Result<Self>;
}

fn take<'a>(input: &mut &'a [u8], n: usize) -> io::Result<&'a [u8]> {
    if input.len() < n {
        return Err(bad("message ends early"));
    }
    let (head, rest) = input.split_at(n);
    *input = rest;
    Ok(head)
}

macro_rules! wire_int {
    ($($t:ty),*) => {$(
        impl Wire for $t {
            fn put(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            fn get(input: &mut &[u8]) -> io::Result<Self> {
                let bytes = take(input, std::mem::size_of::<$t>())?;
                Ok(<$t>::from_le_bytes(bytes.try_into().expect("length checked")))
            }
        }
    )*};
}
wire_int!(u8, u16, u32, i32);

impl Wire for bool {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        match u8::get(input)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(bad("bad bool")),
        }
    }
}

impl Wire for char {
    fn put(&self, out: &mut Vec<u8>) {
        (*self as u32).put(out);
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        char::from_u32(u32::get(input)?).ok_or_else(|| bad("bad char"))
    }
}

/// Raw bytes: a length, then the bytes as they are.
fn put_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    (bytes.len() as u32).put(out);
    out.extend_from_slice(bytes);
}

fn get_bytes<'a>(input: &mut &'a [u8]) -> io::Result<&'a [u8]> {
    let len = u32::get(input)? as usize;
    take(input, len)
}

impl Wire for String {
    fn put(&self, out: &mut Vec<u8>) {
        put_bytes(self.as_bytes(), out);
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        String::from_utf8(get_bytes(input)?.to_vec()).map_err(|_| bad("string is not UTF-8"))
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            None => out.push(0),
            Some(v) => {
                out.push(1);
                v.put(out);
            }
        }
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        match u8::get(input)? {
            0 => Ok(None),
            1 => Ok(Some(T::get(input)?)),
            _ => Err(bad("bad option")),
        }
    }
}

impl<A: Wire, B: Wire> Wire for (A, B) {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
        self.1.put(out);
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        Ok((A::get(input)?, B::get(input)?))
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn put(&self, out: &mut Vec<u8>) {
        (self.len() as u32).put(out);
        for v in self {
            v.put(out);
        }
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        let n = u32::get(input)? as usize;
        // Every element takes at least a byte: a bogus count cannot make us
        // reserve more than the message holds.
        if n > input.len() {
            return Err(bad("list longer than the message"));
        }
        (0..n).map(|_| T::get(input)).collect()
    }
}

impl Wire for KeyCode {
    fn put(&self, out: &mut Vec<u8>) {
        use KeyCode::*;
        let tag: u8 = match self {
            Char(c) => {
                out.push(0);
                c.put(out);
                return;
            }
            F(n) => {
                out.push(1);
                out.push(*n);
                return;
            }
            Enter => 2,
            Tab => 3,
            BackTab => 4,
            Backspace => 5,
            Esc => 6,
            Left => 7,
            Right => 8,
            Up => 9,
            Down => 10,
            Home => 11,
            End => 12,
            PageUp => 13,
            PageDown => 14,
            Insert => 15,
            Delete => 16,
        };
        out.push(tag);
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        use KeyCode::*;
        Ok(match u8::get(input)? {
            0 => Char(char::get(input)?),
            1 => F(u8::get(input)?),
            2 => Enter,
            3 => Tab,
            4 => BackTab,
            5 => Backspace,
            6 => Esc,
            7 => Left,
            8 => Right,
            9 => Up,
            10 => Down,
            11 => Home,
            12 => End,
            13 => PageUp,
            14 => PageDown,
            15 => Insert,
            16 => Delete,
            _ => return Err(bad("unknown key")),
        })
    }
}

impl Wire for Key {
    fn put(&self, out: &mut Vec<u8>) {
        self.code.put(out);
        self.mods.put(out);
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        Ok(Key { code: KeyCode::get(input)?, mods: u8::get(input)? })
    }
}

impl Wire for MouseButton {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        });
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        Ok(match u8::get(input)? {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            _ => return Err(bad("unknown mouse button")),
        })
    }
}

impl Wire for MouseKind {
    fn put(&self, out: &mut Vec<u8>) {
        use MouseKind::*;
        match self {
            Down(b) => {
                out.push(0);
                b.put(out);
            }
            Up(b) => {
                out.push(1);
                b.put(out);
            }
            Drag(b) => {
                out.push(2);
                b.put(out);
            }
            Moved => out.push(3),
            ScrollUp => out.push(4),
            ScrollDown => out.push(5),
        }
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        use MouseKind::*;
        Ok(match u8::get(input)? {
            0 => Down(MouseButton::get(input)?),
            1 => Up(MouseButton::get(input)?),
            2 => Drag(MouseButton::get(input)?),
            3 => Moved,
            4 => ScrollUp,
            5 => ScrollDown,
            _ => return Err(bad("unknown mouse event")),
        })
    }
}

impl Wire for Mouse {
    fn put(&self, out: &mut Vec<u8>) {
        self.kind.put(out);
        self.col.put(out);
        self.row.put(out);
        self.mods.put(out);
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        Ok(Mouse { kind: MouseKind::get(input)?, col: u16::get(input)?, row: u16::get(input)?, mods: u8::get(input)? })
    }
}

impl Wire for Request {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Request::Attach { target, create } => {
                out.push(0);
                target.put(out);
                create.put(out);
            }
            Request::New { name, command, detached } => {
                out.push(1);
                name.put(out);
                command.put(out);
                detached.put(out);
            }
            Request::List => out.push(2),
            Request::KillSession { target } => {
                out.push(3);
                target.put(out);
            }
            Request::KillServer => out.push(4),
            Request::Action { target, pane, action } => {
                out.push(5);
                target.put(out);
                pane.put(out);
                action.put(out);
            }
            Request::Restore => out.push(6),
        }
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        Ok(match u8::get(input)? {
            0 => Request::Attach { target: Wire::get(input)?, create: Wire::get(input)? },
            1 => Request::New { name: Wire::get(input)?, command: Wire::get(input)?, detached: Wire::get(input)? },
            2 => Request::List,
            3 => Request::KillSession { target: Wire::get(input)? },
            4 => Request::KillServer,
            5 => Request::Action { target: Wire::get(input)?, pane: Wire::get(input)?, action: Wire::get(input)? },
            6 => Request::Restore,
            _ => return Err(bad("unknown request")),
        })
    }
}

impl Wire for C2S {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            C2S::Hello { version, cols, rows, cwd, env, request } => {
                out.push(0);
                version.put(out);
                cols.put(out);
                rows.put(out);
                cwd.put(out);
                env.put(out);
                request.put(out);
            }
            C2S::Key(k) => {
                out.push(1);
                k.put(out);
            }
            C2S::Mouse(m) => {
                out.push(2);
                m.put(out);
            }
            C2S::Paste(text) => {
                out.push(3);
                text.put(out);
            }
            C2S::Resize(cols, rows) => {
                out.push(4);
                cols.put(out);
                rows.put(out);
            }
        }
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        Ok(match u8::get(input)? {
            0 => C2S::Hello {
                version: Wire::get(input)?,
                cols: Wire::get(input)?,
                rows: Wire::get(input)?,
                cwd: Wire::get(input)?,
                env: Wire::get(input)?,
                request: Wire::get(input)?,
            },
            1 => C2S::Key(Wire::get(input)?),
            2 => C2S::Mouse(Wire::get(input)?),
            3 => C2S::Paste(Wire::get(input)?),
            4 => C2S::Resize(Wire::get(input)?, Wire::get(input)?),
            _ => return Err(bad("unknown client message")),
        })
    }
}

impl Wire for S2C {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            S2C::Output(bytes) => {
                out.push(0);
                put_bytes(bytes, out);
            }
            S2C::Exit { message, code } => {
                out.push(1);
                message.put(out);
                code.put(out);
            }
        }
    }
    fn get(input: &mut &[u8]) -> io::Result<Self> {
        Ok(match u8::get(input)? {
            0 => S2C::Output(get_bytes(input)?.to_vec()),
            1 => S2C::Exit { message: Wire::get(input)?, code: Wire::get(input)? },
            _ => return Err(bad("unknown server message")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Wire + std::fmt::Debug>(msg: T) -> T {
        let mut buf = Vec::new();
        write_msg(&mut buf, &msg).unwrap();
        read_msg(&mut buf.as_slice()).unwrap()
    }

    #[test]
    fn messages_survive_the_wire() {
        let hello = C2S::Hello {
            version: PROTOCOL_VERSION,
            cols: 120,
            rows: 40,
            cwd: "/home/ü/ž".into(),
            env: vec![("TERM".into(), "xterm-256color".into()), ("EMPTY".into(), String::new())],
            request: Request::New {
                name: Some("work".into()),
                command: vec!["make".into(), "-j8".into()],
                detached: true,
            },
        };
        assert_eq!(format!("{:?}", round_trip(hello.clone())), format!("{hello:?}"));
        for request in [
            Request::Attach { target: None, create: true },
            Request::List,
            Request::KillSession { target: "x".into() },
            Request::KillServer,
            Request::Action { target: Some("w".into()), pane: Some(7), action: "split-right".into() },
            Request::Restore,
        ] {
            let msg = C2S::Hello { version: 1, cols: 1, rows: 1, cwd: String::new(), env: Vec::new(), request };
            assert_eq!(format!("{:?}", round_trip(msg.clone())), format!("{msg:?}"));
        }
        let keys = [
            Key::new(KeyCode::Char('😀'), ALT | CTRL),
            Key::new(KeyCode::F(12), SHIFT),
            Key::plain(KeyCode::Delete),
            Key::new(KeyCode::BackTab, SUPER),
        ];
        for k in keys {
            assert!(matches!(round_trip(C2S::Key(k)), C2S::Key(got) if got == k));
        }
        let m = Mouse { kind: MouseKind::Drag(MouseButton::Middle), col: 300, row: 2, mods: SHIFT };
        assert!(matches!(round_trip(C2S::Mouse(m)), C2S::Mouse(got) if got == m));
        assert!(matches!(round_trip(C2S::Resize(500, 9)), C2S::Resize(500, 9)));
        let out = S2C::Output((0..=255).collect());
        assert!(matches!(round_trip(out), S2C::Output(b) if b.len() == 256 && b[255] == 255));
        assert!(
            matches!(round_trip(S2C::Exit { message: "bye".into(), code: -3 }), S2C::Exit { message, code: -3 } if message == "bye")
        );
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        // Truncated, unknown tags, bad UTF-8, absurd lengths, trailing bytes.
        let frames: [&[u8]; 6] = [
            &[1, 0, 0, 0, 9],
            &[2, 0, 0, 0, 1, 0],
            &[6, 0, 0, 0, 3, 2, 0, 0, 0, 0xff],
            &[5, 0, 0, 0, 3, 0xff, 0xff, 0xff, 0xff],
            &[3, 0, 0, 0, 4, 1, 0],
            &[4, 0, 0, 0, 1, 2, 0, 0],
        ];
        for f in frames {
            assert!(read_msg::<_, C2S>(&mut &f[..]).is_err(), "{f:?}");
        }
        assert!(read_msg::<_, S2C>(&mut &[0xff, 0xff, 0xff, 0xff][..]).is_err());
    }
}
