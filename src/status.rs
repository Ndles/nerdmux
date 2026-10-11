//! Status requests (DECRQSS, `ESC P $ q … ESC \`) that the terminal engine
//! does not answer. Programs use the one for text attributes to learn what
//! the terminal supports: nvim sets a curly underline, asks, and draws curly
//! underlines only if the answer still has it.

use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::Color;
use std::fmt::Write;

/// Finds status requests in a program's output, which may arrive split
/// across reads.
#[derive(Default)]
pub struct Scanner {
    state: State,
    request: Vec<u8>,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum State {
    #[default]
    Text,
    Esc,
    /// `ESC P`
    Dcs,
    /// `ESC P $`
    Dollar,
    /// `ESC P $ q`, collecting the request.
    Request,
    /// `ESC` inside the request: the start of the terminator.
    RequestEsc,
}

impl Scanner {
    /// Look at one more byte; returns the request when this byte ends one.
    pub fn push(&mut self, b: u8) -> Option<Vec<u8>> {
        const ESC: u8 = 0x1b;
        self.state = match (self.state, b) {
            (State::Request, ESC) => State::RequestEsc,
            (State::RequestEsc, b'\\') => {
                self.state = State::Text;
                return Some(std::mem::take(&mut self.request));
            }
            (_, ESC) => State::Esc,
            (State::Esc, b'P') | (State::RequestEsc, b'P') => State::Dcs,
            (State::Dcs, b'$') => State::Dollar,
            (State::Dollar, b'q') => {
                self.request.clear();
                State::Request
            }
            // Requests are a few characters; anything else is not one.
            (State::Request, 0x20..=0x7e) if self.request.len() < 8 => {
                self.request.push(b);
                State::Request
            }
            _ => State::Text,
        };
        None
    }
}

/// The answer to `request`, given the attributes text is written with now.
pub fn reply(request: &[u8], template: &Cell) -> Vec<u8> {
    match request {
        b"m" => format!("\x1bP1$r{}m\x1b\\", sgr(template)).into_bytes(),
        // Valid replies for other settings are not offered.
        _ => b"\x1bP0$r\x1b\\".to_vec(),
    }
}

/// The SGR parameters that produce `cell`'s attributes from a reset.
pub fn sgr(cell: &Cell) -> String {
    let mut out = String::from("0");
    let flags = cell.flags;
    let simple = [(Flags::BOLD, "1"), (Flags::DIM, "2"), (Flags::ITALIC, "3")];
    for (flag, code) in simple {
        if flags.contains(flag) {
            out.push(';');
            out.push_str(code);
        }
    }
    let underline = [
        (Flags::UNDERLINE, "4"),
        (Flags::DOUBLE_UNDERLINE, "4:2"),
        (Flags::UNDERCURL, "4:3"),
        (Flags::DOTTED_UNDERLINE, "4:4"),
        (Flags::DASHED_UNDERLINE, "4:5"),
    ];
    if let Some((_, code)) = underline.iter().find(|(flag, _)| flags.contains(*flag)) {
        out.push(';');
        out.push_str(code);
    }
    let more = [(Flags::INVERSE, "7"), (Flags::HIDDEN, "8"), (Flags::STRIKEOUT, "9")];
    for (flag, code) in more {
        if flags.contains(flag) {
            out.push(';');
            out.push_str(code);
        }
    }
    color(&mut out, cell.fg, 30, 38);
    color(&mut out, cell.bg, 40, 48);
    if let Some(c) = cell.underline_color() {
        color(&mut out, c, 0, 58);
    }
    out
}

/// Append `c` as `;<base + n>` for the 16 named colors (`;<extended>:5:n`
/// when there is no base), or in the extended forms. Default colors add
/// nothing.
fn color(out: &mut String, c: Color, base: u16, extended: u16) {
    let _ = match c {
        Color::Named(n) if (n as usize) < 8 && base != 0 => write!(out, ";{}", base + n as u16),
        Color::Named(n) if (n as usize) < 16 && base != 0 => write!(out, ";{}", base + 60 + n as u16 - 8),
        Color::Named(n) if (n as usize) < 16 => write!(out, ";{extended}:5:{}", n as usize),
        Color::Named(_) => Ok(()),
        Color::Indexed(i) => write!(out, ";{extended}:5:{i}"),
        Color::Spec(rgb) => write!(out, ";{extended}:2::{}:{}:{}", rgb.r, rgb.g, rgb.b),
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::{NamedColor, Rgb};

    fn scan(scanner: &mut Scanner, bytes: &[u8]) -> Vec<Vec<u8>> {
        bytes.iter().filter_map(|b| scanner.push(*b)).collect()
    }

    #[test]
    fn finds_requests_even_when_split() {
        let mut s = Scanner::default();
        assert_eq!(scan(&mut s, b"text\x1b[4:3m\x1bP$qm\x1b\\\x1b[c"), vec![b"m".to_vec()]);
        assert!(scan(&mut s, b"\x1bP$").is_empty());
        assert!(scan(&mut s, b"q\"p\x1b").is_empty());
        assert_eq!(scan(&mut s, b"\\"), vec![b"\"p".to_vec()]);
        // Other DCS strings and broken requests are not answered.
        assert!(scan(&mut s, b"\x1bP=1s\x1b\\\x1bP$q\nm\x1b\\\x1bP$qtoolongrequest\x1b\\").is_empty());
    }

    #[test]
    fn reports_the_current_attributes() {
        let mut cell = Cell::default();
        assert_eq!(reply(b"m", &cell), b"\x1bP1$r0m\x1b\\");
        cell.flags = Flags::BOLD | Flags::UNDERCURL | Flags::INVERSE;
        cell.fg = Color::Named(NamedColor::BrightRed);
        cell.bg = Color::Indexed(236);
        cell.set_underline_color(Some(Color::Spec(Rgb { r: 255, g: 0, b: 0 })));
        assert_eq!(reply(b"m", &cell), b"\x1bP1$r0;1;4:3;7;91;48:5:236;58:2::255:0:0m\x1b\\");
        assert_eq!(reply(b"\"p", &cell), b"\x1bP0$r\x1b\\");
    }
}
