//! Drawing: turns server state into a cell buffer per client, then into the
//! smallest stream of escape sequences that updates that client's terminal.

use crate::config::{Config, StatusPos, Theme};
use crate::keys::{self, Hint};
use crate::layout::{Geometry, PaneId};
use crate::overlay::{Confirm, Overlay, Picker, Prompt};
use crate::pane::Pane;
use crate::server::{window_area, window_title, Client, ClientId, Server, Session};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::collections::HashMap;
use std::io::Write;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// ===== small drawing helpers ================================================

fn text_width(s: &str) -> u16 {
    UnicodeWidthStr::width(s).min(u16::MAX as usize) as u16
}

/// Shorten `s` to at most `max` columns, marking the cut with an ellipsis.
fn truncate(s: &str, max: u16) -> String {
    if text_width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0u16;
    for c in s.chars() {
        let w = c.width().unwrap_or(0) as u16;
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Keep the end of `s` that fits in `max` columns.
fn tail(s: &str, max: u16) -> &str {
    let mut used = 0u16;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        let w = c.width().unwrap_or(0) as u16;
        if used + w > max {
            break;
        }
        used += w;
        start = i;
    }
    &s[start..]
}

/// Write `text` at (x, y), clipped to `limit` (exclusive) and the buffer.
/// Returns the column after the last cell written.
fn put(buf: &mut Buffer, x: u16, y: u16, text: &str, limit: u16, style: Style) -> u16 {
    let limit = limit.min(buf.area.width);
    let mut x = x;
    if y >= buf.area.height {
        return x;
    }
    for c in text.chars() {
        let w = c.width().unwrap_or(0) as u16;
        if w == 0 {
            continue;
        }
        if x + w > limit {
            break;
        }
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(c);
            cell.modifier = Modifier::empty();
            cell.skip = false;
            cell.set_style(style);
        }
        for extra in 1..w {
            if let Some(cell) = buf.cell_mut((x + extra, y)) {
                cell.set_symbol(" ");
                cell.modifier = Modifier::empty();
                cell.skip = false;
                cell.set_style(style);
            }
        }
        x += w;
    }
    x
}

fn fill(buf: &mut Buffer, rect: Rect, style: Style) {
    let rect = rect.intersection(buf.area);
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.modifier = Modifier::empty();
                cell.skip = false;
                cell.set_style(style);
            }
        }
    }
}

fn centered(cols: u16, rows: u16, w: u16, h: u16, top_bias: bool) -> Rect {
    let w = w.min(cols);
    let h = h.min(rows);
    let x = (cols - w) / 2;
    let y = if top_bias { ((rows - h) / 4).min(rows - h) } else { (rows - h) / 2 };
    Rect::new(x, y, w, h)
}

/// Draw a framed popup and return its inner area.
fn popup(buf: &mut Buffer, rect: Rect, title: &str, footer: &str, frame: Color, theme: &Theme) -> Rect {
    let rect = rect.intersection(buf.area);
    if rect.width < 2 || rect.height < 2 {
        return Rect::new(rect.x, rect.y, 0, 0);
    }
    let body = Style::default().fg(theme.popup_fg).bg(theme.popup_bg);
    let line = Style::default().fg(frame).bg(theme.popup_bg);
    fill(buf, rect, body);
    let (x0, y0, x1, y1) = (rect.x, rect.y, rect.x + rect.width - 1, rect.y + rect.height - 1);
    for x in x0 + 1..x1 {
        put(buf, x, y0, "─", x1, line);
        put(buf, x, y1, "─", x1, line);
    }
    for y in y0 + 1..y1 {
        put(buf, x0, y, "│", x0 + 1, line);
        put(buf, x1, y, "│", x1 + 1, line);
    }
    put(buf, x0, y0, "╭", x0 + 1, line);
    put(buf, x1, y0, "╮", x1 + 1, line);
    put(buf, x0, y1, "╰", x0 + 1, line);
    put(buf, x1, y1, "╯", x1 + 1, line);
    if !title.is_empty() && rect.width > 6 {
        let t = format!(" {} ", truncate(title, rect.width - 6));
        put(buf, x0 + 2, y0, &t, x1, line.add_modifier(Modifier::BOLD));
    }
    if !footer.is_empty() && rect.width > 6 {
        let f = format!(" {} ", truncate(footer, rect.width - 6));
        let fx = x1.saturating_sub(text_width(&f) + 1).max(x0 + 1);
        put(buf, fx, y1, &f, x1, Style::default().fg(theme.dim).bg(theme.popup_bg));
    }
    Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2)
}

/// A filled label such as ` COPY ` in the status bar.
fn badge(theme: &Theme, bg: Color) -> Style {
    Style::default().fg(theme.accent_fg).bg(bg).add_modifier(Modifier::BOLD)
}

// ===== panes ================================================================

/// Translate a color from a pane's program into one for the client's
/// terminal. Returns the color and whether it is a "dim" variant.
fn map_color(c: TermColor, foreground: bool, theme: &Theme) -> (Color, bool) {
    let base = |i: usize| if theme.own_palette { theme.palette[i] } else { Color::Indexed(i as u8) };
    let default = || match (theme.own_palette, foreground) {
        (true, true) => theme.fg,
        (true, false) => theme.bg,
        (false, _) => Color::Reset,
    };
    match c {
        TermColor::Spec(rgb) => (Color::Rgb(rgb.r, rgb.g, rgb.b), false),
        TermColor::Indexed(i) if i < 16 => (base(i as usize), false),
        TermColor::Indexed(i) => (Color::Indexed(i), false),
        TermColor::Named(n) => match n {
            NamedColor::Foreground | NamedColor::BrightForeground => {
                (if theme.own_palette { theme.fg } else { Color::Reset }, false)
            }
            NamedColor::Background => (if theme.own_palette { theme.bg } else { Color::Reset }, false),
            NamedColor::Cursor => (default(), false),
            NamedColor::DimForeground => (if theme.own_palette { theme.fg } else { Color::Reset }, true),
            NamedColor::DimBlack => (base(0), true),
            NamedColor::DimRed => (base(1), true),
            NamedColor::DimGreen => (base(2), true),
            NamedColor::DimYellow => (base(3), true),
            NamedColor::DimBlue => (base(4), true),
            NamedColor::DimMagenta => (base(5), true),
            NamedColor::DimCyan => (base(6), true),
            NamedColor::DimWhite => (base(7), true),
            other => (base((other as usize).min(15)), false),
        },
    }
}

fn draw_pane(buf: &mut Buffer, rect: Rect, pane: &mut Pane, theme: &Theme, focused: bool, faded: bool) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    // Panes without the focus sit on a darker background.
    let pane_bg = if theme.own_palette && !focused { Some(theme.inactive_bg) } else { None };
    if let Some(bg) = pane_bg {
        fill(buf, rect.intersection(buf.area), Style::default().fg(theme.fg).bg(bg));
    }
    let matches = pane.visible_matches();
    let content = pane.term.renderable_content();
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let (bw, bh) = (buf.area.width, buf.area.height);
    let mut scratch = String::new();

    for indexed in content.display_iter {
        let row = indexed.point.line.0 + offset;
        let col = indexed.point.column.0;
        if row < 0 || row >= rect.height as i32 || col >= rect.width as usize {
            continue;
        }
        let (x, y) = (rect.x + col as u16, rect.y + row as u16);
        if x >= bw || y >= bh {
            continue;
        }
        let cell = indexed.cell;
        let flags = cell.flags;
        let (mut fg, dim_fg) = map_color(cell.fg, true, theme);
        let (mut bg, _) = map_color(cell.bg, false, theme);
        if let (Some(pane_bg), TermColor::Named(NamedColor::Background)) = (pane_bg, cell.bg) {
            bg = pane_bg;
        }
        let mut modifier = Modifier::empty();
        if flags.contains(Flags::BOLD) {
            modifier |= Modifier::BOLD;
        }
        if flags.contains(Flags::DIM) || dim_fg || faded {
            modifier |= Modifier::DIM;
        }
        if flags.contains(Flags::ITALIC) {
            modifier |= Modifier::ITALIC;
        }
        if flags.intersects(Flags::ALL_UNDERLINES) {
            modifier |= Modifier::UNDERLINED;
        }
        if flags.contains(Flags::INVERSE) {
            modifier |= Modifier::REVERSED;
        }
        if flags.contains(Flags::STRIKEOUT) {
            modifier |= Modifier::CROSSED_OUT;
        }
        let point = indexed.point;
        if selection.as_ref().is_some_and(|s| s.contains(point)) {
            fg = theme.accent_fg;
            bg = theme.magenta;
            modifier.remove(Modifier::REVERSED | Modifier::DIM);
        } else if matches.iter().any(|m| *m.start() <= point && point <= *m.end()) {
            fg = theme.accent_fg;
            bg = theme.green;
            modifier.remove(Modifier::REVERSED | Modifier::DIM);
        }

        let Some(target) = buf.cell_mut((x, y)) else { continue };
        let wide = flags.contains(Flags::WIDE_CHAR);
        let clipped_wide = wide && col + 1 >= rect.width as usize;
        let c = cell.c;
        target.skip = false;
        if flags.contains(Flags::WIDE_CHAR_SPACER) {
            // The second half of a wide character: the terminal fills it in
            // when the character itself is printed, so never draw it.
            target.set_symbol(" ");
            target.skip = !(col == 0 || flags.contains(Flags::HIDDEN));
        } else if flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            || clipped_wide
            || flags.contains(Flags::HIDDEN)
            || c.is_control()
        {
            target.set_symbol(" ");
        } else {
            scratch.clear();
            scratch.push(c);
            if let Some(extra) = cell.zerowidth() {
                scratch.extend(extra.iter().filter(|z| !z.is_control()));
            }
            // The pane's grid decided how many cells this character takes.
            // If combining marks or a different Unicode table would make it
            // look wider here, the diff would skip a neighboring cell, so
            // fall back to something that is certainly one cell.
            if !wide && UnicodeWidthStr::width(scratch.as_str()) != 1 {
                scratch.clear();
                scratch.push(if c.width() == Some(1) { c } else { '\u{fffd}' });
            }
            target.set_symbol(&scratch);
        }
        target.fg = fg;
        target.bg = bg;
        target.modifier = modifier;
    }
}

/// Small label in the pane's top-right corner while scrolled back or copying.
fn draw_pane_badge(buf: &mut Buffer, rect: Rect, pane: &Pane, theme: &Theme) {
    let (offset, history) = pane.scroll_position();
    let (label, color) = match &pane.copy {
        Some(copy) if copy.search.is_some() => {
            (format!(" COPY  find: {}  {offset}/{history} ", copy.query), theme.green)
        }
        Some(_) => (format!(" COPY  {offset}/{history} "), theme.green),
        None if offset > 0 => (format!(" ↑ {offset}/{history} "), theme.cyan),
        None => return,
    };
    let label = truncate(&label, rect.width);
    let x = rect.x + rect.width - text_width(&label).min(rect.width);
    put(buf, x, rect.y, &label, rect.x + rect.width, badge(theme, color));
}

/// Separator lines between panes, plus an amber outline around the focused
/// one. The layout keeps a one-cell frame around split panes; it stays blank
/// except where it outlines the focused pane.
fn draw_borders(buf: &mut Buffer, area: Rect, geom: &Geometry, active: PaneId, theme: &Theme) {
    if geom.panes.len() < 2 || area.width == 0 || area.height == 0 {
        return;
    }
    let (w, h) = (area.width as usize, area.height as usize);
    let mut border = vec![true; w * h];
    for (_, r) in &geom.panes {
        for y in r.y..r.y + r.height {
            for x in r.x..r.x + r.width {
                let (gx, gy) = ((x - area.x) as usize, (y - area.y) as usize);
                if gx < w && gy < h {
                    border[gy * w + gx] = false;
                }
            }
        }
    }
    // The outer frame, if the layout kept one.
    let inner = crate::server::framed(area);
    let framed = inner != area;
    let on_frame = |x: i32, y: i32| framed && (x == 0 || y == 0 || x == w as i32 - 1 || y == h as i32 - 1);
    let is = |x: i32, y: i32| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && border[y as usize * w + x as usize]
            && !on_frame(x, y)
    };
    let normal = Style::default().fg(theme.border);
    for gy in 0..h {
        for gx in 0..w {
            let (x, y) = (gx as i32, gy as i32);
            if !is(x, y) {
                continue;
            }
            let glyph = match (is(x, y - 1), is(x, y + 1), is(x - 1, y), is(x + 1, y)) {
                (true, true, true, true) => "┼",
                (true, true, true, false) => "┤",
                (true, true, false, true) => "├",
                (true, false, true, true) => "┴",
                (false, true, true, true) => "┬",
                (true, false, true, false) => "┘",
                (true, false, false, true) => "└",
                (false, true, true, false) => "┐",
                (false, true, false, true) => "┌",
                (false, false, true, _) | (false, false, _, true) => "─",
                _ => "│",
            };
            let (ax, ay) = (area.x + gx as u16, area.y + gy as u16);
            put(buf, ax, ay, glyph, ax + 1, normal);
        }
    }

    // Outline the focused pane with thin lines hugging its edges.
    let Some(act) = geom.rect_of(active) else { return };
    if act.width == 0 || act.height == 0 {
        return;
    }
    let hot = Style::default().fg(theme.accent);
    let mut mark = |x: i32, y: i32, glyph: &str| {
        let (gx, gy) = (x - area.x as i32, y - area.y as i32);
        if gx >= 0 && gy >= 0 && (gx as usize) < w && (gy as usize) < h && border[gy as usize * w + gx as usize] {
            put(buf, x as u16, y as u16, glyph, x as u16 + 1, hot);
        }
    };
    let (left, top) = (act.x as i32 - 1, act.y as i32 - 1);
    let (right, bottom) = (act.x as i32 + act.width as i32, act.y as i32 + act.height as i32);
    for x in left + 1..right {
        mark(x, top, "▁");
        mark(x, bottom, "▔");
    }
    for y in top + 1..bottom {
        mark(left, y, "▕");
        mark(right, y, "▏");
    }
}

fn draw_pane_numbers(buf: &mut Buffer, geom: &Geometry, order: &[PaneId], active: PaneId, theme: &Theme) {
    for (i, pid) in order.iter().enumerate().take(10) {
        let Some(rect) = geom.rect_of(*pid) else { continue };
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        let label = format!("  {}  ", (i + 1) % 10);
        let style = badge(theme, if *pid == active { theme.accent } else { theme.purple });
        let lw = text_width(&label).min(rect.width);
        let x = rect.x + (rect.width - lw) / 2;
        let y = rect.y + rect.height / 2;
        if rect.height >= 3 {
            fill(buf, Rect::new(x, y - 1, lw, 3), style);
        }
        put(buf, x, y, &label, rect.x + rect.width, style);
    }
}

// ===== status bar ===========================================================

struct StatusInfo<'a> {
    session: &'a Session,
    panes: &'a HashMap<PaneId, Pane>,
    config: &'a Config,
    prefix_armed: bool,
    message: Option<&'a str>,
}

fn draw_status(buf: &mut Buffer, y: u16, cols: u16, info: &StatusInfo, tabs: &mut Vec<(u16, u16, u32)>) {
    let theme = &info.config.theme;
    let base = Style::default().fg(theme.status_fg).bg(theme.status_bg);
    fill(buf, Rect::new(0, y, cols, 1), base);
    let window = info.session.window();
    let active_pane = info.panes.get(&window.active);

    // Right side, built first so the tabs know how much room is left.
    let mut right: Vec<(String, Style)> = Vec::new();
    if let Some(msg) = info.message {
        right.push((format!(" {} ", truncate(msg, cols.saturating_sub(24).max(10))), base.fg(theme.green)));
    }
    if info.prefix_armed {
        right.push((format!(" {} … ", keys::key_label(info.config.prefix)), badge(theme, theme.magenta)));
    }
    if active_pane.is_some_and(|p| p.in_copy_mode()) {
        right.push((" COPY ".into(), badge(theme, theme.green)));
    }
    if window.sync {
        right.push((" ALL PANES ".into(), badge(theme, theme.red)));
    }
    if window.zoomed {
        right.push((" ZOOM ".into(), badge(theme, theme.purple)));
    }
    // Where the focused pane is: its directory, git branch and how many
    // panes share the window. Dropped first when room is short.
    let mut place: Vec<(String, Style)> = Vec::new();
    if let Some((dir, branch)) = active_pane.and_then(|p| p.place.as_ref()) {
        if cols >= 60 {
            place.push((format!(" {} ", short_path(&crate::server::tidy_path(dir), 32)), base.fg(theme.status_fg)));
        }
        if let Some(b) = branch.as_ref().filter(|_| cols >= 80) {
            place.push((format!(" {} ", truncate(b, 24)), base.fg(theme.green)));
        }
    }
    let panes = window.layout.count();
    if panes > 1 && cols >= 50 {
        place.push((format!(" {panes} panes "), base.fg(theme.dim)));
    }
    if !place.is_empty() && info.message.is_none() {
        // Badges stay at the far right; the place sits just before them.
        right.splice(0..0, place);
        right.push((" ".into(), base));
    }
    let right_width: u16 = right.iter().map(|(t, _)| text_width(t)).sum();
    let right_start = cols.saturating_sub(right_width);

    // Left: session name, then one tab per window.
    let name = format!(" {} ", truncate(&info.session.name, 24));
    let mut x = put(buf, 0, y, &name, right_start, base.fg(theme.accent).add_modifier(Modifier::BOLD));
    x = put(buf, x, y, "│ ", right_start, base.fg(theme.border));

    let labels: Vec<(String, String)> = info
        .session
        .windows
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let title = truncate(&window_title(w, info.panes), 20);
            (format!(" {} ", i + 1), format!("{title}{} ", if w.bell { " !" } else { "" }))
        })
        .collect();
    let widths: Vec<u16> = labels.iter().map(|(a, b)| text_width(a) + text_width(b)).collect();
    let active_index = info.session.active_index();
    let room = right_start.saturating_sub(x + 1);
    // Scroll the tab strip so the active window is always visible.
    let mut first = 0;
    while first < active_index && widths[first..=active_index].iter().sum::<u16>() + 2 > room {
        first += 1;
    }
    if first > 0 {
        x = put(buf, x, y, "‹ ", right_start, base.fg(theme.dim));
    }
    for (i, w) in info.session.windows.iter().enumerate().skip(first) {
        if x + widths[i] > right_start.saturating_sub(1) {
            put(buf, x, y, "›", right_start, base.fg(theme.dim));
            break;
        }
        let start = x;
        let (num, title) = &labels[i];
        if i == active_index {
            let style = badge(theme, theme.accent);
            x = put(buf, x, y, num, right_start, style);
            x = put(buf, x, y, title, right_start, style);
        } else {
            x = put(buf, x, y, num, right_start, base.fg(theme.cyan));
            let style = if w.bell { base.fg(theme.red).add_modifier(Modifier::BOLD) } else { base };
            x = put(buf, x, y, title, right_start, style);
        }
        tabs.push((start, x, w.id));
    }

    let mut rx = right_start;
    for (text, style) in &right {
        rx = put(buf, rx, y, text, cols, *style);
    }
}

/// Shorten a path from the left to at most `max` columns, keeping whole
/// trailing components: `~/src/very/deep/repo` → `…/deep/repo`.
fn short_path(path: &str, max: u16) -> String {
    if text_width(path) <= max {
        return path.to_string();
    }
    let parts: Vec<&str> = path.split('/').collect();
    let mut out = String::new();
    for part in parts.iter().rev() {
        let next = if out.is_empty() { part.to_string() } else { format!("{part}/{out}") };
        if text_width(&next) + 2 > max {
            break;
        }
        out = next;
    }
    if out.is_empty() {
        format!("…{}", tail(path, max.saturating_sub(1)))
    } else {
        format!("…/{out}")
    }
}

// ===== popups ===============================================================

/// Fit a key list such as `Shift+←↓↑→  Ctrl+←↓↑→` into `max` columns by
/// dropping whole alternatives from the end rather than cutting one in half.
fn fit_keys(keys: &str, max: u16) -> String {
    let mut groups: Vec<&str> = keys.split("  ").collect();
    while groups.len() > 1 && text_width(&groups.join("  ")) > max {
        groups.pop();
    }
    truncate(&groups.join("  "), max)
}

use keys::CATEGORIES;

enum HintRow<'a> {
    Header(&'a str),
    Entry(&'a Hint),
    Blank,
}

/// Arrange hints into columns of at most `nrows` lines. A category stays
/// together in one column whenever it fits.
fn hint_columns<'a>(hints: &'a [Hint], nrows: usize) -> Vec<Vec<HintRow<'a>>> {
    let mut cols: Vec<Vec<HintRow>> = vec![Vec::new()];
    for cat in CATEGORIES {
        let entries: Vec<&Hint> = hints.iter().filter(|h| h.category == cat).collect();
        if entries.is_empty() {
            continue;
        }
        let size = entries.len() + 1;
        let used = cols.last().map_or(0, Vec::len);
        if used > 0 {
            // Continue below a blank line if the whole category fits there,
            // or if it has to be split anyway and there is room to start.
            let fits = used + 1 + size <= nrows;
            let must_split = size > nrows && used + 3 <= nrows;
            match cols.last_mut() {
                Some(col) if fits || must_split => col.push(HintRow::Blank),
                _ => cols.push(Vec::new()),
            }
        }
        let rows = std::iter::once(HintRow::Header(cat)).chain(entries.into_iter().map(HintRow::Entry));
        for row in rows {
            let used = cols.last().map_or(0, Vec::len);
            // Never leave a heading alone at the bottom of a column.
            let orphan = matches!(row, HintRow::Header(_)) && used + 1 >= nrows && used > 0;
            if used >= nrows || orphan {
                cols.push(Vec::new());
            }
            if let Some(col) = cols.last_mut() {
                col.push(row);
            }
        }
    }
    cols
}

/// Draw the key guide along the bottom of `bounds`. Returns the area used.
fn draw_which_key(buf: &mut Buffer, bounds: Rect, config: &Config) -> Option<Rect> {
    let theme = &config.theme;
    let hints = keys::hints(&config.prefix_table);
    if bounds.width < 24 || bounds.height < 5 {
        return None;
    }
    let key_w = hints.iter().map(|h| text_width(&fit_keys(&h.keys, 16))).max().unwrap_or(4);
    let desc_w = hints.iter().map(|h| text_width(&h.desc)).max().unwrap_or(10).min(30);
    let col_w = key_w + 2 + desc_w + 3;
    let inner_w = bounds.width - 4;
    let ncols = (inner_w / col_w).max(1) as usize;
    let max_rows = (bounds.height - 2) as usize;

    // The shortest popup whose columns fit side by side.
    let total = hints.len() + CATEGORIES.len();
    let start = total.div_ceil(ncols).clamp(3, max_rows.max(3));
    let nrows = (start..=max_rows).find(|n| hint_columns(&hints, *n).len() <= ncols).unwrap_or(max_rows.max(1));
    let columns = hint_columns(&hints, nrows);
    let nrows = columns.iter().take(ncols).map(Vec::len).max().unwrap_or(1).min(max_rows);

    let height = nrows as u16 + 2;
    let rect = Rect::new(bounds.x, bounds.y + bounds.height - height, bounds.width, height);
    let title = format!("{} then…", keys::key_label(config.prefix));
    let inner = popup(buf, rect, &title, "Esc cancel", theme.accent, theme);
    let body = Style::default().fg(theme.popup_fg).bg(theme.popup_bg);
    for (c, column) in columns.iter().enumerate().take(ncols) {
        let x = inner.x + 1 + c as u16 * col_w;
        let limit = (x + col_w - 1).min(inner.x + inner.width);
        for (r, row) in column.iter().enumerate().take(nrows) {
            let y = inner.y + r as u16;
            match row {
                HintRow::Blank => {}
                HintRow::Header(name) => {
                    put(buf, x, y, name, limit, body.fg(theme.magenta).add_modifier(Modifier::BOLD));
                }
                HintRow::Entry(h) => {
                    let keys = fit_keys(&h.keys, key_w);
                    put(buf, x, y, &keys, limit, body.fg(theme.cyan).add_modifier(Modifier::BOLD));
                    put(buf, x + key_w + 2, y, &truncate(&h.desc, desc_w), limit, body);
                }
            }
        }
    }
    Some(rect)
}

enum HelpLine {
    Heading(String),
    Note(String),
    /// Chord, key after the leader, description.
    Row(String, String, String),
    /// Key and description in a mode with keys of its own.
    Fixed(&'static str, &'static str),
    Blank,
}

fn help_lines(config: &Config) -> Vec<HelpLine> {
    let mut lines = vec![
        HelpLine::Note(format!(
            "Chords work any time. For the second column press the leader, {}, then the key.",
            keys::key_label(config.prefix)
        )),
        HelpLine::Blank,
        HelpLine::Row("Chord".into(), "Leader +".into(), String::new()),
    ];
    let rows = keys::help_rows(&config.direct_table, &config.prefix_table);
    for cat in CATEGORIES {
        let entries: Vec<&keys::HelpRow> = rows.iter().filter(|r| r.category == cat).collect();
        if entries.is_empty() {
            continue;
        }
        lines.push(HelpLine::Heading(cat.into()));
        for r in entries {
            lines.push(HelpLine::Row(r.chord.clone(), r.leader.clone(), r.desc.clone()));
        }
        lines.push(HelpLine::Blank);
    }
    let mut section = |title: &str, rows: &[(&'static str, &'static str)]| {
        lines.push(HelpLine::Heading(title.into()));
        lines.extend(rows.iter().map(|(k, d)| HelpLine::Fixed(k, d)));
        lines.push(HelpLine::Blank);
    };
    section(
        "In resize mode",
        &[
            ("←↓↑→", "move the border of the focused pane"),
            ("Shift+←↓↑→", "the same, in bigger steps"),
            ("Ctrl+←↓↑→", "move the pane itself"),
            ("Enter  Esc", "done"),
        ],
    );
    section(
        "In the settings panel",
        &[
            ("type", "filter the list"),
            ("↑↓  Enter", "pick a setting and change it"),
            ("←→  Shift+←→", "flip a switch, cycle a choice, step a number"),
            ("Del", "back to the default"),
            ("Enter on a key", "press the new key; with Alt it is a chord"),
        ],
    );
    section(
        "In the session manager",
        &[
            ("↑↓  Enter", "pick a session and switch to it"),
            ("n  r  x", "new / rename / kill session"),
            ("/", "filter by name"),
        ],
    );
    section(
        "In copy mode",
        &[
            ("←↓↑→", "move"),
            ("Ctrl+←→", "previous / next word"),
            ("Home  End", "start / end of the line"),
            ("PgUp  PgDn", "a page up / down"),
            ("Ctrl+Home  Ctrl+End", "oldest / newest line"),
            ("Shift+any of these", "select while moving"),
            ("Ctrl+a", "select everything"),
            ("Ctrl+l", "select whole lines"),
            ("Ctrl+r", "select a rectangle"),
            ("Ctrl+c  Enter", "copy the selection and leave"),
            ("Ctrl+f", "find text"),
            ("F3  Shift+F3", "older / newer match"),
            ("Esc", "clear the selection, then leave"),
        ],
    );
    section(
        "Mouse",
        &[
            ("click", "focus a pane, or pick a window tab"),
            ("drag a border", "resize panes"),
            ("drag", "select text and copy it"),
            ("double / triple click", "copy a word / a line"),
            ("wheel", "scroll back through output"),
            ("middle click", "paste"),
            ("Shift+drag", "your terminal's own selection"),
        ],
    );
    lines.pop();
    lines
}

fn draw_help(buf: &mut Buffer, cols: u16, rows: u16, scroll: &mut u16, config: &Config) {
    let theme = &config.theme;
    let lines = help_lines(config);
    let width = |pick: fn(&HelpLine) -> Option<&String>| {
        lines.iter().filter_map(pick).map(|k| text_width(k)).max().unwrap_or(0).min(26)
    };
    let chord_w = width(|l| match l {
        HelpLine::Row(c, _, _) => Some(c),
        _ => None,
    });
    let leader_w = width(|l| match l {
        HelpLine::Row(_, k, _) => Some(k),
        _ => None,
    });
    let rect = centered(cols, rows, 90, rows.saturating_sub(2), false);
    let inner = popup(buf, rect, "Keys", "↑↓ PgUp PgDn scroll · Esc close", theme.accent, theme);
    if inner.height == 0 {
        return;
    }
    let max_scroll = (lines.len() as u16).saturating_sub(inner.height);
    *scroll = (*scroll).min(max_scroll);
    let body = Style::default().fg(theme.popup_fg).bg(theme.popup_bg);
    let limit = inner.x + inner.width;
    let key_style = body.fg(theme.cyan).add_modifier(Modifier::BOLD);
    let leader_x = inner.x + 2 + chord_w + 2;
    let desc_x = leader_x + leader_w + 2;
    for (i, line) in lines.iter().skip(*scroll as usize).take(inner.height as usize).enumerate() {
        let y = inner.y + i as u16;
        match line {
            HelpLine::Blank => {}
            HelpLine::Heading(t) => {
                put(buf, inner.x + 1, y, t, limit, body.fg(theme.magenta).add_modifier(Modifier::BOLD));
            }
            HelpLine::Note(t) => {
                put(buf, inner.x + 1, y, t, limit, body.fg(theme.dim));
            }
            HelpLine::Row(chord, leader, desc) if desc.is_empty() => {
                // Column titles.
                put(buf, inner.x + 2, y, chord, limit, body.fg(theme.dim));
                put(buf, leader_x, y, leader, limit, body.fg(theme.dim));
            }
            HelpLine::Row(chord, leader, desc) => {
                put(buf, inner.x + 2, y, &truncate(chord, chord_w), limit, key_style);
                put(buf, leader_x, y, &truncate(leader, leader_w), limit, key_style);
                put(buf, desc_x, y, desc, limit, body);
            }
            HelpLine::Fixed(key, desc) => {
                let end = put(buf, inner.x + 2, y, key, limit, key_style);
                put(buf, desc_x.max(end + 2), y, desc, limit, body);
            }
        }
    }
}

fn draw_picker(buf: &mut Buffer, cols: u16, rows: u16, picker: &mut Picker, theme: &Theme) -> Option<(u16, u16)> {
    let width = cols.saturating_sub(4).clamp(20, 78);
    let list_h = (picker.items.len() as u16).clamp(1, rows.saturating_sub(8).max(1));
    let rect = centered(cols, rows, width, list_h + 4, true);
    let footer = if picker.manage {
        "Enter switch · n new · r rename · x kill".to_string()
    } else {
        format!("{}/{}", picker.shown.len(), picker.items.len())
    };
    let inner = popup(buf, rect, &picker.title, &footer, theme.accent, theme);
    if inner.height < 3 || inner.width < 6 {
        return None;
    }
    let body = Style::default().fg(theme.popup_fg).bg(theme.popup_bg);
    let limit = inner.x + inner.width;
    let mut x = put(buf, inner.x + 1, inner.y, "› ", limit, body.fg(theme.cyan).add_modifier(Modifier::BOLD));
    let room = limit.saturating_sub(x + 1);
    if picker.manage && !picker.filtering {
        put(buf, x, inner.y, "/ to filter", limit, body.fg(theme.dim));
    } else if picker.query.is_empty() {
        put(buf, x, inner.y, "type to filter", limit, body.fg(theme.dim));
    } else {
        x = put(buf, x, inner.y, tail(&picker.query, room), limit, body);
    }
    // The manager takes letters as commands, so it shows no text cursor
    // until filtering starts.
    let typing = !picker.manage || picker.filtering;
    let cursor = (x.min(limit.saturating_sub(1)), inner.y);
    for sx in inner.x..limit {
        put(buf, sx, inner.y + 1, "─", limit, body.fg(theme.border));
    }
    let list_h = (inner.height - 2) as usize;
    picker.clamp_scroll(list_h);
    if picker.shown.is_empty() {
        put(buf, inner.x + 2, inner.y + 2, "nothing matches", limit, body.fg(theme.dim));
    }
    for (row, shown_index) in picker.shown.iter().enumerate().skip(picker.scroll).take(list_h) {
        let item = &picker.items[*shown_index];
        let y = inner.y + 2 + (row - picker.scroll) as u16;
        let selected = row == picker.selected;
        let style = if selected { Style::default().fg(theme.accent_fg).bg(theme.accent) } else { body };
        if selected {
            fill(buf, Rect::new(inner.x, y, inner.width, 1), style);
        }
        let marker = if item.current { "●" } else { " " };
        let marker_style = if selected { style } else { style.fg(theme.green) };
        let mut x = put(buf, inner.x + 1, y, marker, limit, marker_style);
        // With a query the list is flat, so the tree indentation would mislead.
        let indent = if picker.query.trim().is_empty() { item.indent * 2 } else { 0 };
        x += 1 + indent;
        let label_style = if item.indent == 0 || selected { style.add_modifier(Modifier::BOLD) } else { style };
        x = put(buf, x, y, &item.label, limit.saturating_sub(1), label_style);
        if !item.detail.is_empty() {
            let detail_style = if selected { style } else { style.fg(theme.cyan) };
            let room = limit.saturating_sub(x + 3);
            if room >= 4 {
                // Paths are most telling at their end, so cut them on the left.
                let is_path = item.detail.starts_with('/') || item.detail.starts_with('~');
                let detail = if is_path && text_width(&item.detail) > room {
                    format!("…{}", tail(&item.detail, room - 1))
                } else {
                    truncate(&item.detail, room)
                };
                let dx = limit - 1 - text_width(&detail);
                put(buf, dx, y, &detail, limit, detail_style);
            }
        }
    }
    typing.then_some(cursor)
}

fn draw_settings(
    buf: &mut Buffer,
    cols: u16,
    rows: u16,
    panel: &mut crate::settings::Panel,
    config: &Config,
) -> Option<(u16, u16)> {
    use crate::settings::Mode;
    let theme = &config.theme;
    let list = panel.rows(config);
    panel.selected = panel.selected.min(list.len().saturating_sub(1));
    let width = cols.saturating_sub(4).clamp(20, 76);
    let rect = centered(cols, rows, width, rows.saturating_sub(2), false);
    let footer = match panel.mode {
        Mode::Browse => "Enter change · ←→ adjust · Del reset · Esc close",
        Mode::Edit(_) => "Enter save · Esc cancel",
        Mode::Capture => "press the new key · Esc cancel",
    };
    let inner = popup(buf, rect, "Settings", footer, theme.accent, theme);
    if inner.height < 4 || inner.width < 10 {
        return None;
    }
    let body = Style::default().fg(theme.popup_fg).bg(theme.popup_bg);
    let limit = inner.x + inner.width;

    // Filter line, with the file the panel writes on the right.
    let path = crate::server::tidy_path(&crate::config::config_path());
    let room = limit.saturating_sub(inner.x + 4);
    let mut x = put(buf, inner.x + 1, inner.y, "› ", limit, body.fg(theme.cyan).add_modifier(Modifier::BOLD));
    if panel.query.is_empty() {
        put(buf, x, inner.y, "type to filter", limit, body.fg(theme.dim));
    } else {
        x = put(buf, x, inner.y, tail(&panel.query, room / 2), limit, body);
    }
    let filter_cursor = (x, inner.y);
    let p = truncate(&path, room / 2);
    put(buf, limit.saturating_sub(text_width(&p) + 1), inner.y, &p, limit, body.fg(theme.dim));
    for sx in inner.x..limit {
        put(buf, sx, inner.y + 1, "─", limit, body.fg(theme.border));
    }

    // Rows with a heading line wherever the section changes.
    enum Line {
        Head(&'static str),
        Row(usize),
    }
    let mut lines = Vec::new();
    let mut section = "";
    for (i, r) in list.iter().enumerate() {
        if r.section != section {
            section = r.section;
            lines.push(Line::Head(section));
        }
        lines.push(Line::Row(i));
    }
    let height = (inner.height - 2) as usize;
    let at = lines.iter().position(|l| matches!(l, Line::Row(i) if *i == panel.selected)).unwrap_or(0);
    // Keep the selection visible, and its heading too when it is the first row.
    let want = if at > 0 && matches!(lines[at - 1], Line::Head(_)) { at - 1 } else { at };
    if want < panel.scroll {
        panel.scroll = want;
    } else if at >= panel.scroll + height {
        panel.scroll = at + 1 - height;
    }
    if list.is_empty() {
        put(buf, inner.x + 2, inner.y + 2, "nothing matches", limit, body.fg(theme.dim));
    }

    let label_w = 32u16.min(inner.width / 2);
    let value_x = inner.x + 3 + label_w;
    let mut cursor = None;
    for (n, line) in lines.iter().skip(panel.scroll).take(height).enumerate() {
        let y = inner.y + 2 + n as u16;
        let i = match line {
            Line::Head(name) => {
                put(buf, inner.x + 1, y, name, limit, body.fg(theme.magenta).add_modifier(Modifier::BOLD));
                continue;
            }
            Line::Row(i) => *i,
        };
        let r = &list[i];
        let selected = i == panel.selected;
        let style = if selected { Style::default().fg(theme.accent_fg).bg(theme.accent) } else { body };
        if selected {
            fill(buf, Rect::new(inner.x, y, inner.width, 1), style);
        }
        if r.custom {
            // Set in the file, not a default.
            put(buf, inner.x + 1, y, "•", limit, if selected { style } else { style.fg(theme.accent) });
        }
        put(buf, inner.x + 3, y, &truncate(&r.label, label_w), value_x, style);
        let mut vx = value_x;
        match (&panel.mode, selected) {
            (Mode::Edit(text), true) => {
                let room = limit.saturating_sub(vx + 2);
                vx = put(buf, vx, y, tail(text, room), limit, style.add_modifier(Modifier::BOLD));
                cursor = Some((vx.min(limit.saturating_sub(1)), y));
                continue;
            }
            (Mode::Capture, true) => {
                put(buf, vx, y, "press a key…", limit, style.add_modifier(Modifier::BOLD));
                continue;
            }
            _ => {}
        }
        if let Some(c) = r.swatch {
            vx = put(buf, vx, y, "██", limit, style.fg(c));
            vx += 1;
        }
        let value_style = if selected { style.add_modifier(Modifier::BOLD) } else { style.fg(theme.cyan) };
        let value = if r.value.is_empty() { "—".to_string() } else { r.value.clone() };
        put(buf, vx, y, &truncate(&value, limit.saturating_sub(vx + 1)), limit, value_style);
    }
    match panel.mode {
        Mode::Edit(_) => cursor,
        Mode::Capture => None,
        Mode::Browse => Some(filter_cursor),
    }
}

fn draw_prompt(buf: &mut Buffer, cols: u16, rows: u16, prompt: &Prompt, theme: &Theme) -> Option<(u16, u16)> {
    let width = cols.saturating_sub(4).clamp(20, 60);
    let rect = centered(cols, rows, width, 3, true);
    let inner = popup(buf, rect, &prompt.title, "Enter ok · Esc cancel", theme.cyan, theme);
    if inner.height == 0 || inner.width < 3 {
        return None;
    }
    let body = Style::default().fg(theme.popup_fg).bg(theme.popup_bg);
    let limit = inner.x + inner.width;
    let room = inner.width.saturating_sub(3);
    let x = put(buf, inner.x + 1, inner.y, tail(&prompt.text, room), limit, body);
    Some((x.min(limit.saturating_sub(1)), inner.y))
}

fn draw_confirm(buf: &mut Buffer, cols: u16, rows: u16, confirm: &Confirm, theme: &Theme) {
    let hint = "y or Enter: yes     any other key: no";
    let width = (text_width(&confirm.text).max(text_width(hint)) + 6).min(cols.saturating_sub(2));
    let rect = centered(cols, rows, width, 5, true);
    let inner = popup(buf, rect, "Confirm", "", theme.red, theme);
    if inner.height < 3 {
        return;
    }
    let body = Style::default().fg(theme.popup_fg).bg(theme.popup_bg);
    let limit = inner.x + inner.width;
    let text = truncate(&confirm.text, inner.width.saturating_sub(4));
    put(buf, inner.x + 2, inner.y, &text, limit, body.add_modifier(Modifier::BOLD));
    put(buf, inner.x + 2, inner.y + 2, hint, limit, body.fg(theme.dim));
}

/// One line above the status bar explaining arrange mode.
fn draw_arrange_hint(buf: &mut Buffer, bounds: Rect, theme: &Theme) {
    if bounds.height == 0 {
        return;
    }
    let y = bounds.y + bounds.height - 1;
    let body = Style::default().fg(theme.popup_fg).bg(theme.popup_bg);
    fill(buf, Rect::new(bounds.x, y, bounds.width, 1), body);
    let limit = bounds.x + bounds.width;
    let mut x = put(buf, bounds.x, y, " ARRANGE ", limit, badge(theme, theme.cyan));
    let key = body.fg(theme.cyan).add_modifier(Modifier::BOLD);
    for (keys, what) in
        [("←↓↑→", "resize"), ("Shift+←↓↑→", "bigger steps"), ("Ctrl+←↓↑→", "move pane"), ("Enter", "done")]
    {
        x = put(buf, x, y, "  ", limit, body);
        x = put(buf, x, y, keys, limit, key);
        x = put(buf, x, y, " ", limit, body);
        x = put(buf, x, y, what, limit, body);
    }
}

// ===== frame assembly =======================================================

struct Frame {
    buf: Buffer,
    cursor: Option<(u16, u16, u8)>,
    any_motion: bool,
}

fn compose(client: &mut Client, session: &Session, panes: &mut HashMap<PaneId, Pane>, config: &Config) -> Frame {
    let theme = &config.theme;
    let (cols, rows) = (client.cols, client.rows);
    let mut buf = Buffer::empty(Rect::new(0, 0, cols, rows));
    if theme.own_palette {
        // The whole screen takes the theme's background, whatever the
        // terminal underneath is set to.
        fill(&mut buf, Rect::new(0, 0, cols, rows), Style::default().fg(theme.fg).bg(theme.bg));
    }
    let area = window_area(config.status, session.cols, session.rows);
    let window = session.window();
    let geom = window.geometry(area);

    // A client larger than the session shows the unused part as dots.
    if cols > session.cols || rows > session.rows {
        let style = Style::default().fg(theme.border);
        for y in 0..rows {
            for x in 0..cols {
                if x >= session.cols || y >= session.rows {
                    put(&mut buf, x, y, "·", cols, style);
                }
            }
        }
    }

    draw_borders(&mut buf, area, &geom, window.active, theme);
    let mut cursor = None;
    for (pid, rect) in &geom.panes {
        let Some(pane) = panes.get_mut(pid) else { continue };
        let focused = *pid == window.active;
        draw_pane(&mut buf, *rect, pane, theme, focused, config.dim_inactive && !focused);
        draw_pane_badge(&mut buf, *rect, pane, theme);
        if *pid == window.active {
            if let Some(c) = pane.cursor() {
                let (x, y) = (rect.x + c.col, rect.y + c.row);
                if c.col < rect.width && c.row < rect.height && x < cols && y < rows {
                    cursor = Some((x, y, c.style));
                }
            }
        }
    }

    client.tabs.clear();
    let status_row = match config.status {
        StatusPos::Top => Some(0),
        StatusPos::Bottom => Some(rows.saturating_sub(1)),
        StatusPos::Off => None,
    };
    if let Some(y) = status_row {
        let info = StatusInfo {
            session,
            panes,
            config,
            prefix_armed: client.prefix.is_some(),
            message: client.message.as_ref().map(|(m, _)| m.as_str()),
        };
        draw_status(&mut buf, y, cols, &info, &mut client.tabs);
    } else if let Some((msg, _)) = &client.message {
        // No status bar: show messages on the last line instead.
        let text = format!(" {} ", truncate(msg, cols.saturating_sub(2)));
        put(&mut buf, 0, rows.saturating_sub(1), &text, cols, badge(theme, theme.green));
    }

    // The part of the screen popups may use: everything but the status bar.
    let bounds = match config.status {
        StatusPos::Top => Rect::new(0, 1, cols, rows.saturating_sub(1)),
        StatusPos::Bottom => Rect::new(0, 0, cols, rows.saturating_sub(1)),
        StatusPos::Off => Rect::new(0, 0, cols, rows),
    };
    let hints_due = client.prefix.as_ref().is_some_and(|p| p.shown && p.repeat_until.is_none());
    if hints_due && client.overlay.is_none() {
        // The popup may cover the pane's cursor; do not let it show through.
        if let Some(used) = draw_which_key(&mut buf, bounds, config) {
            if cursor.is_some_and(|(x, y, _)| crate::layout::contains(used, x, y)) {
                cursor = None;
            }
        }
    }

    match client.overlay.as_mut() {
        Some(Overlay::Help { scroll }) => {
            draw_help(&mut buf, cols, rows, scroll, config);
            cursor = None;
        }
        Some(Overlay::Picker(picker)) => {
            cursor = draw_picker(&mut buf, cols, rows, picker, theme).map(|(x, y)| (x, y, 6));
        }
        Some(Overlay::Settings(panel)) => {
            cursor = draw_settings(&mut buf, cols, rows, panel, config).map(|(x, y)| (x, y, 6));
        }
        Some(Overlay::Prompt(prompt)) => {
            cursor = draw_prompt(&mut buf, cols, rows, prompt, theme).map(|(x, y)| (x, y, 6));
        }
        Some(Overlay::Confirm(confirm)) => {
            draw_confirm(&mut buf, cols, rows, confirm, theme);
            cursor = None;
        }
        Some(Overlay::PaneNumbers) => {
            let order = window.layout.panes();
            draw_pane_numbers(&mut buf, &geom, &order, window.active, theme);
            cursor = None;
        }
        Some(Overlay::Arrange) => {
            draw_arrange_hint(&mut buf, bounds, theme);
            cursor = None;
        }
        None => {}
    }

    let any_motion = config.mouse
        && client.overlay.is_none()
        && panes.get(&window.active).is_some_and(|p| p.wants_mouse() && p.mode().contains(TermMode::MOUSE_MOTION));
    Frame { buf, cursor, any_motion }
}

/// The closest of the 256 standard colors, for terminals without true color.
pub fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let step =
        |v: u8| LEVELS.iter().enumerate().min_by_key(|(_, l)| (**l as i32 - v as i32).abs()).map_or(0, |(i, _)| i);
    let dist = |c: (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(c.0, r) + d(c.1, g) + d(c.2, b)
    };
    let (ri, gi, bi) = (step(r), step(g), step(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let gray_step = ((r as u32 + g as u32 + b as u32) / 3).saturating_sub(8).min(238) / 10;
    let gray = 8 + 10 * gray_step as u8;
    if dist((gray, gray, gray)) < dist(cube) {
        232 + gray_step.min(23) as u8
    } else {
        (16 + 36 * ri + 6 * gi + bi) as u8
    }
}

fn push_color(out: &mut Vec<u8>, color: Color, fg: bool, truecolor: bool) {
    let base: u16 = if fg { 30 } else { 40 };
    let index = |i: u8, out: &mut Vec<u8>| {
        let _ = match i {
            0..=7 => write!(out, ";{}", base + i as u16),
            8..=15 => write!(out, ";{}", base + 60 + (i as u16 - 8)),
            _ => write!(out, ";{};5;{}", base + 8, i),
        };
    };
    match color {
        Color::Reset => {
            let _ = write!(out, ";{}", base + 9);
        }
        Color::Indexed(i) => index(i, out),
        Color::Rgb(r, g, b) if truecolor => {
            let _ = write!(out, ";{};2;{r};{g};{b}", base + 8);
        }
        Color::Rgb(r, g, b) => index(nearest_256(r, g, b), out),
        Color::Black => index(0, out),
        Color::Red => index(1, out),
        Color::Green => index(2, out),
        Color::Yellow => index(3, out),
        Color::Blue => index(4, out),
        Color::Magenta => index(5, out),
        Color::Cyan => index(6, out),
        Color::Gray => index(7, out),
        Color::DarkGray => index(8, out),
        Color::LightRed => index(9, out),
        Color::LightGreen => index(10, out),
        Color::LightYellow => index(11, out),
        Color::LightBlue => index(12, out),
        Color::LightMagenta => index(13, out),
        Color::LightCyan => index(14, out),
        Color::White => index(15, out),
    }
}

/// Whether terminals may disagree with us about how wide `symbol` is.
fn width_is_uncertain(symbol: &str) -> bool {
    let mut chars = symbol.chars();
    let Some(c) = chars.next() else { return false };
    if chars.next().is_some() {
        return true;
    }
    let u = c as u32;
    // Alphabets, box drawing, blocks, braille, CJK and Hangul are safe.
    u >= 0x2190
        && !matches!(
            u,
            0x2500..=0x259F
                | 0x2800..=0x28FF
                | 0x2E80..=0xA4CF
                | 0xAC00..=0xD7A3
                | 0xF900..=0xFAFF
                | 0xFE30..=0xFE4F
                | 0xFF00..=0xFF60
                | 0xFFE0..=0xFFE6
                | 0x20000..=0x3FFFD
        )
}

/// Encode changed cells as cursor moves, SGR changes and text.
pub fn emit_cells(out: &mut Vec<u8>, updates: &[(u16, u16, &Cell)], cols: u16, truecolor: bool) {
    const MODS: [(Modifier, u8); 8] = [
        (Modifier::BOLD, 1),
        (Modifier::DIM, 2),
        (Modifier::ITALIC, 3),
        (Modifier::UNDERLINED, 4),
        (Modifier::SLOW_BLINK, 5),
        (Modifier::REVERSED, 7),
        (Modifier::HIDDEN, 8),
        (Modifier::CROSSED_OUT, 9),
    ];
    if updates.is_empty() {
        return;
    }
    let (mut fg, mut bg, mut mods) = (Color::Reset, Color::Reset, Modifier::empty());
    let mut expected: Option<(u16, u16)> = None;
    for &(x, y, cell) in updates {
        if expected != Some((x, y)) {
            let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
        }
        if cell.modifier != mods || cell.fg != fg || cell.bg != bg {
            out.extend_from_slice(b"\x1b[");
            let start = out.len();
            if cell.modifier != mods {
                // Attributes cannot all be switched off one by one portably,
                // so start over.
                out.extend_from_slice(b";0");
                for (m, code) in MODS {
                    if cell.modifier.contains(m) {
                        let _ = write!(out, ";{code}");
                    }
                }
                if cell.fg != Color::Reset {
                    push_color(out, cell.fg, true, truecolor);
                }
                if cell.bg != Color::Reset {
                    push_color(out, cell.bg, false, truecolor);
                }
            } else {
                if cell.fg != fg {
                    push_color(out, cell.fg, true, truecolor);
                }
                if cell.bg != bg {
                    push_color(out, cell.bg, false, truecolor);
                }
            }
            // Parameters were written with a leading ';' each; drop the first.
            out.remove(start);
            out.push(b'm');
            fg = cell.fg;
            bg = cell.bg;
            mods = cell.modifier;
        }
        let symbol = cell.symbol();
        let width = UnicodeWidthStr::width(symbol) as u16;
        if symbol.is_empty() || width == 0 {
            out.push(b' ');
        } else {
            out.extend_from_slice(symbol.as_bytes());
        }
        let next = x + width.max(1);
        // Terminals disagree about the width of emoji and symbols; after
        // one of those, say where the cursor is rather than assume.
        expected = if next < cols && !width_is_uncertain(symbol) { Some((next, y)) } else { None };
    }
    out.extend_from_slice(b"\x1b[0m");
}

impl Server {
    /// Draw the client's view and return the bytes that bring its terminal
    /// up to date, or `None` when nothing changed.
    pub(crate) fn render_client(&mut self, cid: ClientId) -> Option<Vec<u8>> {
        let Server { clients, sessions, panes, config, .. } = self;
        let client = clients.get_mut(&cid)?;
        let session = sessions.iter().find(|s| s.id == client.session)?;
        let frame = compose(client, session, panes, config);

        let full = client.full_redraw || client.prev.area != frame.buf.area;
        let blank;
        let prev = if full {
            blank = Buffer::empty(frame.buf.area);
            &blank
        } else {
            &client.prev
        };
        let updates = prev.diff(&frame.buf);
        let mut pending = std::mem::take(&mut client.pending);
        if config.set_title {
            let window = session.window();
            let own = panes.get(&window.active).map(|p| p.title.trim()).filter(|t| !t.is_empty());
            let what = own.map(str::to_string).unwrap_or_else(|| window_title(window, panes));
            let title: String =
                format!("{} › {}", session.name, what).chars().filter(|c| !c.is_control()).take(160).collect();
            if title != client.title {
                pending.extend_from_slice(format!("\x1b]2;{title}\x07").as_bytes());
                client.title = title;
            }
        }
        let cursor_moved = frame.cursor != client.cursor;
        let motion_changed = frame.any_motion != client.any_motion;
        if !full && updates.is_empty() && pending.is_empty() && !cursor_moved && !motion_changed {
            return None;
        }

        let mut out = Vec::with_capacity(updates.len() * 6 + pending.len() + 64);
        // Synchronized update: the terminal applies the frame in one go.
        out.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
        out.extend_from_slice(&pending);
        if full {
            out.extend_from_slice(b"\x1b[0m\x1b[2J");
        }
        emit_cells(&mut out, &updates, frame.buf.area.width, client.truecolor);
        if motion_changed {
            out.extend_from_slice(if frame.any_motion { b"\x1b[?1003h" } else { b"\x1b[?1003l" });
            client.any_motion = frame.any_motion;
        }
        if let Some((x, y, style)) = frame.cursor {
            if style != client.cursor_style || full {
                let _ = write!(out, "\x1b[{style} q");
                client.cursor_style = style;
            }
            let _ = write!(out, "\x1b[{};{}H\x1b[?25h", y + 1, x + 1);
        }
        out.extend_from_slice(b"\x1b[?2026l");

        client.cursor = frame.cursor;
        client.prev = frame.buf;
        client.full_redraw = false;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_columns_keep_categories_together() {
        let config = Config::default();
        let hints = keys::hints(&config.prefix_table);
        let entries = |col: &Vec<HintRow>| col.iter().filter(|r| matches!(r, HintRow::Entry(_))).count();
        for nrows in 1..40 {
            let cols = hint_columns(&hints, nrows);
            assert!(cols.iter().all(|c| c.len() <= nrows), "column longer than {nrows}");
            assert_eq!(cols.iter().map(entries).sum::<usize>(), hints.len(), "hints lost at {nrows} rows");
            for col in &cols {
                assert!(!matches!(col.last(), Some(HintRow::Header(_))) || nrows == 1, "orphan header at {nrows}");
            }
        }
        // With room for the largest category, each one starts a column or
        // follows a complete one: no column begins with a bare entry.
        let cols = hint_columns(&hints, 17);
        assert!(cols.iter().all(|c| matches!(c.first(), Some(HintRow::Header(_)))));
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("hello", 0), "");
        assert_eq!(truncate("日本語テキスト", 5), "日本…");
        assert_eq!(tail("hello world", 5), "world");
        assert_eq!(tail("hi", 5), "hi");
        assert_eq!(tail("日本語", 4), "本語");
    }

    #[test]
    fn emits_minimal_sequences() {
        let mut prev = Buffer::empty(Rect::new(0, 0, 10, 2));
        let mut next = prev.clone();
        let mut out = Vec::new();
        emit_cells(&mut out, &prev.diff(&next), 10, true);
        assert!(out.is_empty());

        let bold_red = Style::default().fg(Color::Indexed(1)).add_modifier(Modifier::BOLD);
        put(&mut next, 2, 1, "ab", 10, bold_red);
        put(&mut next, 4, 1, "c", 10, bold_red);
        put(&mut next, 5, 1, "d", 10, bold_red.fg(Color::Indexed(200)).bg(Color::Rgb(1, 2, 3)));
        emit_cells(&mut out, &prev.diff(&next), 10, true);
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            "\x1b[2;3H\x1b[0;1;31mabc\x1b[38;5;200;48;2;1;2;3md\x1b[0m"
        );

        // Wide characters advance the cursor by two columns.
        prev = next.clone();
        put(&mut next, 0, 0, "日x", 10, Style::default());
        out.clear();
        emit_cells(&mut out, &prev.diff(&next), 10, true);
        assert_eq!(String::from_utf8(out.clone()).unwrap(), "\x1b[1;1H日x\x1b[0m");

        // After an emoji the position is stated again, in case the
        // terminal's idea of its width differs.
        prev = next.clone();
        put(&mut next, 0, 0, "😀ab", 10, Style::default());
        out.clear();
        emit_cells(&mut out, &prev.diff(&next), 10, true);
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[1;1H😀\x1b[1;3Hab\x1b[0m");
    }

    #[test]
    fn falls_back_to_256_colors() {
        assert_eq!(nearest_256(0, 0, 0), 16);
        assert_eq!(nearest_256(255, 255, 255), 231);
        assert_eq!(nearest_256(255, 0, 0), 196);
        assert_eq!(nearest_256(0xFF, 0xB0, 0x00), 214); // amber
        assert_eq!(nearest_256(0x00, 0xF0, 0xFF), 51); // bright cyan
        assert_eq!(nearest_256(0x80, 0x80, 0x80), 244);
        assert_eq!(nearest_256(0x0A, 0x0A, 0x12), 232);

        let prev = Buffer::empty(Rect::new(0, 0, 4, 1));
        let mut next = prev.clone();
        put(&mut next, 0, 0, "x", 4, Style::default().fg(Color::Rgb(0xFF, 0xB0, 0x00)).bg(Color::Rgb(10, 10, 18)));
        let mut out = Vec::new();
        emit_cells(&mut out, &prev.diff(&next), 4, false);
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[1;1H\x1b[38;5;214;48;5;232mx\x1b[0m");
    }

    #[test]
    fn default_theme_is_warm_amber_and_muted() {
        let t = Theme::default();
        let rgb = |c: Color| match c {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => panic!("the default theme is defined in RGB"),
        };
        let luminance = |c: Color| {
            let (r, g, b) = rgb(c);
            let lin = |v: u8| {
                let v = v as f64 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
        };
        let contrast = |a: Color, b: Color| {
            let (a, b) = (luminance(a), luminance(b));
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        };
        let saturation = |c: Color| {
            let (r, g, b) = rgb(c);
            let (max, min) = (r.max(g).max(b) as u32, r.min(g).min(b) as u32);
            (max - min) * 100 / max.max(1)
        };
        // Dark, warm surfaces: red at least as strong as blue, no blue cast.
        for surface in [t.bg, t.status_bg, t.popup_bg, t.palette[0], t.border] {
            let (r, _, b) = rgb(surface);
            assert!(luminance(surface) < 0.05, "{surface:?} is not dark");
            assert!(r >= b, "{surface:?} has a cold cast");
        }
        // Amber: strong red, mid green, little blue, and the most saturated
        // color in the theme.
        for amber in [t.accent, t.cursor] {
            let (r, g, b) = rgb(amber);
            assert!(r > 200 && (140..=190).contains(&g) && b < 90, "{amber:?} is not amber");
        }
        // Everything else is muted (no neon) but still easy to read on the
        // background and the status bar.
        let roles = [t.fg, t.status_fg, t.cyan, t.magenta, t.green, t.purple, t.red, t.yellow];
        for c in roles.iter().chain(&t.palette[1..16]) {
            assert!(saturation(*c) < saturation(t.accent), "{c:?} competes with the amber");
            assert!(saturation(*c) <= 60, "{c:?} is too loud");
            assert!(contrast(*c, t.bg) >= 4.5, "{c:?} is too dark to read");
        }
        for c in roles {
            assert!(contrast(c, t.status_bg) >= 4.5, "{c:?} is hard to read in the status bar");
        }
        // Badge text stays readable on every badge color.
        for c in [t.accent, t.cyan, t.magenta, t.green, t.purple, t.red] {
            assert!(contrast(t.accent_fg, c) >= 4.5, "badge text unreadable on {c:?}");
        }
    }

    #[test]
    fn popup_survives_tiny_buffers() {
        let theme = Theme::default();
        for (w, h) in [(0, 0), (1, 1), (2, 2), (3, 3), (8, 2), (30, 4)] {
            let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
            popup(&mut buf, Rect::new(0, 0, 40, 10), "A fairly long title", "footer", theme.accent, &theme);
            let mut picker = Picker::new("t", Vec::new());
            draw_picker(&mut buf, w, h, &mut picker, &theme);
            let prompt =
                Prompt { title: "x".into(), text: "hello".into(), kind: crate::overlay::PromptKind::RenameWindow };
            draw_prompt(&mut buf, w, h, &prompt, &theme);
            let config = Config::default();
            draw_help(&mut buf, w, h, &mut 0, &config);
            draw_which_key(&mut buf, Rect::new(0, 0, w, h), &config);
            draw_arrange_hint(&mut buf, Rect::new(0, 0, w, h), &theme);
        }
    }
}
