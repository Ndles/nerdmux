//! Turning key and mouse events back into the byte sequences a program
//! running inside a pane expects.

use crate::proto::{Key, KeyCode, Mouse, MouseButton, MouseKind, ALT, CTRL, SHIFT};
use alacritty_terminal::term::TermMode;

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn mod_param(mods: u8) -> u8 {
    1 + (mods & SHIFT != 0) as u8 + 2 * (mods & ALT != 0) as u8 + 4 * (mods & CTRL != 0) as u8
}

fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        ' ' | '@' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '/' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

pub fn encode_key(key: Key, mode: TermMode) -> Vec<u8> {
    // Super has no place in the classic encodings programs expect.
    let mods = key.mods & !crate::proto::SUPER;
    let alt = mods & ALT != 0;
    let ctrl = mods & CTRL != 0;
    let mut out = Vec::with_capacity(8);

    // `ESC [ 1 ; <mod> <final>` when modified, SS3 or CSI otherwise.
    let cursor_key = |out: &mut Vec<u8>, fin: u8| {
        if mods != 0 {
            out.extend_from_slice(format!("\x1b[1;{}{}", mod_param(mods), fin as char).as_bytes());
        } else if mode.contains(TermMode::APP_CURSOR) {
            out.extend_from_slice(&[0x1b, b'O', fin]);
        } else {
            out.extend_from_slice(&[0x1b, b'[', fin]);
        }
    };
    let tilde_key = |out: &mut Vec<u8>, n: u8| {
        if mods != 0 {
            out.extend_from_slice(format!("\x1b[{n};{}~", mod_param(mods)).as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b[{n}~").as_bytes());
        }
    };

    match key.code {
        KeyCode::Char(c) => {
            if alt {
                out.push(0x1b);
            }
            match ctrl.then(|| ctrl_byte(c)).flatten() {
                Some(b) => out.push(b),
                None => {
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        KeyCode::Enter => {
            if alt {
                out.push(0x1b);
            }
            out.push(b'\r');
        }
        KeyCode::Tab => {
            if mods & SHIFT != 0 {
                out.extend_from_slice(b"\x1b[Z");
            } else {
                if alt {
                    out.push(0x1b);
                }
                out.push(b'\t');
            }
        }
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => {
            if alt {
                out.push(0x1b);
            }
            out.push(if ctrl { 0x08 } else { 0x7f });
        }
        KeyCode::Esc => {
            if alt {
                out.push(0x1b);
            }
            out.push(0x1b);
        }
        KeyCode::Up => cursor_key(&mut out, b'A'),
        KeyCode::Down => cursor_key(&mut out, b'B'),
        KeyCode::Right => cursor_key(&mut out, b'C'),
        KeyCode::Left => cursor_key(&mut out, b'D'),
        KeyCode::Home => cursor_key(&mut out, b'H'),
        KeyCode::End => cursor_key(&mut out, b'F'),
        KeyCode::Insert => tilde_key(&mut out, 2),
        KeyCode::Delete => tilde_key(&mut out, 3),
        KeyCode::PageUp => tilde_key(&mut out, 5),
        KeyCode::PageDown => tilde_key(&mut out, 6),
        KeyCode::F(n @ 1..=4) => {
            let fin = b'P' + (n - 1);
            if mods != 0 {
                out.extend_from_slice(format!("\x1b[1;{}{}", mod_param(mods), fin as char).as_bytes());
            } else {
                out.extend_from_slice(&[0x1b, b'O', fin]);
            }
        }
        KeyCode::F(n) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                _ => return out,
            };
            tilde_key(&mut out, code);
        }
    }
    out
}

/// Encode a mouse event at pane-local cell `(col, row)` (0-based) the way
/// the pane's program asked for it. Returns `None` when the program did not
/// ask for this kind of event.
pub fn encode_mouse(m: Mouse, col: u16, row: u16, mode: TermMode) -> Option<Vec<u8>> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    let button = |b: MouseButton| match b {
        MouseButton::Left => 0u8,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (mut code, release) = match m.kind {
        MouseKind::Down(b) => (button(b), false),
        MouseKind::Up(b) => (button(b), true),
        MouseKind::Drag(b) => {
            if !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                return None;
            }
            (button(b) + 32, false)
        }
        MouseKind::Moved => {
            if !mode.contains(TermMode::MOUSE_MOTION) {
                return None;
            }
            (3 + 32, false)
        }
        MouseKind::ScrollUp => (64, false),
        MouseKind::ScrollDown => (65, false),
    };
    if m.mods & SHIFT != 0 {
        code += 4;
    }
    if m.mods & ALT != 0 {
        code += 8;
    }
    if m.mods & CTRL != 0 {
        code += 16;
    }
    if mode.contains(TermMode::SGR_MOUSE) {
        let fin = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{fin}", col + 1, row + 1).into_bytes());
    }
    if release {
        code = (code & !3) | 3;
    }
    let mut out = vec![0x1b, b'[', b'M', 32 + code];
    if mode.contains(TermMode::UTF8_MOUSE) {
        for v in [col, row] {
            let c = char::from_u32(33 + v as u32)?;
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    } else {
        if col >= 223 || row >= 223 {
            return None;
        }
        out.push(33 + col as u8);
        out.push(33 + row as u8);
    }
    Some(out)
}

/// Prepare pasted text for a pane.
pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        // Do not let the pasted text close the bracket itself.
        let clean = text.replace("\x1b[201~", "").replace('\x1b', "");
        let mut out = Vec::with_capacity(clean.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(key: Key) -> Vec<u8> {
        encode_key(key, TermMode::empty())
    }

    #[test]
    fn plain_and_control_characters() {
        assert_eq!(enc(Key::ch('a')), b"a");
        assert_eq!(enc(Key::ch('é')), "é".as_bytes());
        assert_eq!(enc(Key::ctrl('c')), [3]);
        assert_eq!(enc(Key::ctrl(' ')), [0]);
        assert_eq!(enc(Key::ctrl('4')), [0x1c]);
        assert_eq!(enc(Key::alt('b')), b"\x1bb");
        assert_eq!(enc(Key::new(KeyCode::Char('x'), ALT | CTRL)), [0x1b, 0x18]);
        assert_eq!(enc(Key::plain(KeyCode::Enter)), b"\r");
        assert_eq!(enc(Key::plain(KeyCode::Backspace)), [0x7f]);
        assert_eq!(enc(Key::new(KeyCode::Backspace, ALT)), [0x1b, 0x7f]);
        assert_eq!(enc(Key::plain(KeyCode::BackTab)), b"\x1b[Z");
    }

    #[test]
    fn cursor_and_function_keys() {
        assert_eq!(enc(Key::plain(KeyCode::Up)), b"\x1b[A");
        assert_eq!(encode_key(Key::plain(KeyCode::Up), TermMode::APP_CURSOR), b"\x1bOA");
        assert_eq!(enc(Key::new(KeyCode::Right, CTRL)), b"\x1b[1;5C");
        assert_eq!(enc(Key::new(KeyCode::Left, ALT | SHIFT)), b"\x1b[1;4D");
        assert_eq!(enc(Key::plain(KeyCode::Home)), b"\x1b[H");
        assert_eq!(enc(Key::plain(KeyCode::Delete)), b"\x1b[3~");
        assert_eq!(enc(Key::new(KeyCode::PageUp, SHIFT)), b"\x1b[5;2~");
        assert_eq!(enc(Key::plain(KeyCode::F(1))), b"\x1bOP");
        assert_eq!(enc(Key::plain(KeyCode::F(5))), b"\x1b[15~");
        assert_eq!(enc(Key::new(KeyCode::F(12), CTRL)), b"\x1b[24;5~");
    }

    #[test]
    fn mouse_reports() {
        let click = Mouse { kind: MouseKind::Down(MouseButton::Left), col: 0, row: 0, mods: 0 };
        assert_eq!(encode_mouse(click, 4, 2, TermMode::empty()), None);
        let sgr = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(encode_mouse(click, 4, 2, sgr).unwrap(), b"\x1b[<0;5;3M");
        let up = Mouse { kind: MouseKind::Up(MouseButton::Left), ..click };
        assert_eq!(encode_mouse(up, 4, 2, sgr).unwrap(), b"\x1b[<0;5;3m");
        assert_eq!(encode_mouse(up, 4, 2, TermMode::MOUSE_REPORT_CLICK).unwrap(), [0x1b, b'[', b'M', 35, 37, 35]);
        let drag = Mouse { kind: MouseKind::Drag(MouseButton::Left), ..click };
        assert_eq!(encode_mouse(drag, 4, 2, sgr), None);
        assert_eq!(encode_mouse(drag, 4, 2, sgr | TermMode::MOUSE_DRAG).unwrap(), b"\x1b[<32;5;3M");
        let wheel = Mouse { kind: MouseKind::ScrollDown, mods: CTRL, ..click };
        assert_eq!(encode_mouse(wheel, 0, 0, sgr).unwrap(), b"\x1b[<81;1;1M");
    }

    #[test]
    fn paste() {
        assert_eq!(encode_paste("a\nb\r\nc", TermMode::empty()), b"a\rb\rc");
        assert_eq!(encode_paste("x\x1b[201~y\n", TermMode::BRACKETED_PASTE), b"\x1b[200~xy\n\x1b[201~");
    }
}
