//! Wire protocol between the thin client and the server.
//!
//! Every message is a little-endian `u32` length followed by a bincode body.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const PROTOCOL_VERSION: u32 = 4;
const MAX_FRAME: u32 = 64 * 1024 * 1024;

pub const SHIFT: u8 = 1;
pub const ALT: u8 = 2;
pub const CTRL: u8 = 4;
/// Only reported by terminals that speak the kitty keyboard protocol.
pub const SUPER: u8 = 8;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down(MouseButton),
    Up(MouseButton),
    Drag(MouseButton),
    Moved,
    ScrollUp,
    ScrollDown,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mouse {
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
    pub mods: u8,
}

/// What the client wants from the server. Sent once, inside `Hello`.
#[derive(Serialize, Deserialize, Debug, Clone)]
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
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum C2S {
    Hello { version: u32, cols: u16, rows: u16, cwd: String, env: Vec<(String, String)>, request: Request },
    Key(Key),
    Mouse(Mouse),
    Paste(String),
    Resize(u16, u16),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum S2C {
    /// Raw bytes for the client's terminal.
    Output(Vec<u8>),
    /// The client should restore its terminal, print `message` and exit.
    Exit { message: String, code: i32 },
}

pub fn write_msg<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let body = bincode::serialize(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    w.write_all(&frame)?;
    w.flush()
}

pub fn read_msg<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    bincode::deserialize(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
