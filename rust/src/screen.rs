//! The cell buffer everything is painted into, and the renderer that sends only the
//! cells that changed to the real terminal.

use std::fmt::Write as _;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Parse "#rrggbb" (also "#rgb"; any alpha is ignored). Bad input gives black.
    pub fn hex(s: &str) -> Rgb {
        let h = s.trim().trim_start_matches('#');
        let h: String = if h.len() == 3 || h.len() == 4 {
            h.chars().flat_map(|c| [c, c]).collect()
        } else {
            h.to_string()
        };
        if h.len() < 6 || !h.is_ascii() {
            return Rgb(0, 0, 0);
        }
        let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
        Rgb(p(0), p(2), p(4))
    }

    /// `t` of the way from self to `o`.
    pub fn mix(self, o: Rgb, t: f32) -> Rgb {
        let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8;
        Rgb(f(self.0, o.0), f(self.1, o.1), f(self.2, o.2))
    }
}

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const UNDERLINE: u8 = 4;
pub const STRIKE: u8 = 8;
pub const DIM: u8 = 16;
pub const UNDER2: u8 = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Style {
    pub fg: Rgb,
    pub bg: Rgb,
    pub attr: u8,
}

impl Style {
    pub fn new(fg: Rgb, bg: Rgb) -> Style {
        Style { fg, bg, attr: 0 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w: w.max(0), h: h.max(0) }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// Cells a character occupies. Control characters count as nothing.
pub fn cw(c: char) -> i32 {
    c.width().unwrap_or(0) as i32
}

pub fn width(s: &str) -> i32 {
    s.chars().map(cw).sum()
}

/// Cut `s` to at most `max` cells, ending in an ellipsis when something was dropped.
pub fn ellipsize(s: &str, max: i32) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        if used + cw(c) > max - 1 {
            break;
        }
        used += cw(c);
        out.push(c);
    }
    out.push('…');
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    /// '\0' marks the right half of a wide character.
    pub ch: char,
    pub st: Style,
}

pub struct Buffer {
    pub w: i32,
    pub h: i32,
    cells: Vec<Cell>,
}

impl Buffer {
    pub fn new(w: i32, h: i32, st: Style) -> Buffer {
        Buffer { w, h, cells: vec![Cell { ch: ' ', st }; (w.max(0) * h.max(0)) as usize] }
    }

    fn idx(&self, x: i32, y: i32) -> Option<usize> {
        (x >= 0 && y >= 0 && x < self.w && y < self.h).then(|| (y * self.w + x) as usize)
    }

    pub fn get(&self, x: i32, y: i32) -> Option<Cell> {
        self.idx(x, y).map(|i| self.cells[i])
    }

    pub fn put(&mut self, x: i32, y: i32, ch: char, st: Style) {
        if let Some(i) = self.idx(x, y) {
            self.cells[i] = Cell { ch, st };
        }
    }

    pub fn fill(&mut self, r: Rect, st: Style) {
        for y in r.y.max(0)..r.bottom().min(self.h) {
            for x in r.x.max(0)..r.right().min(self.w) {
                self.cells[(y * self.w + x) as usize] = Cell { ch: ' ', st };
            }
        }
    }

    /// Change the style of existing cells without touching their characters.
    pub fn restyle(&mut self, r: Rect, f: impl Fn(&mut Style)) {
        for y in r.y.max(0)..r.bottom().min(self.h) {
            for x in r.x.max(0)..r.right().min(self.w) {
                f(&mut self.cells[(y * self.w + x) as usize].st);
            }
        }
    }

    /// Draw text over whatever background is already there.
    pub fn print(&mut self, mut x: i32, y: i32, s: &str, fg: Rgb, attr: u8, max_x: i32) -> i32 {
        for c in s.chars() {
            let w = cw(c);
            if w == 0 {
                continue;
            }
            if x + w > max_x {
                break;
            }
            if let Some(i) = self.idx(x, y) {
                let bg = self.cells[i].st.bg;
                self.cells[i] = Cell { ch: c, st: Style { fg, bg, attr } };
                if w == 2 {
                    if let Some(j) = self.idx(x + 1, y) {
                        self.cells[j] = Cell { ch: '\0', st: Style { fg, bg, attr } };
                    }
                }
            }
            x += w;
        }
        x
    }

    /// A rounded box outline (the fill is left as it is).
    pub fn round_box(&mut self, r: Rect, fg: Rgb) {
        if r.w < 2 || r.h < 2 {
            return;
        }
        let (x0, y0, x1, y1) = (r.x, r.y, r.right() - 1, r.bottom() - 1);
        for x in x0 + 1..x1 {
            self.print(x, y0, "─", fg, 0, x + 1);
            self.print(x, y1, "─", fg, 0, x + 1);
        }
        for y in y0 + 1..y1 {
            self.print(x0, y, "│", fg, 0, x0 + 1);
            self.print(x1, y, "│", fg, 0, x1 + 1);
        }
        self.print(x0, y0, "╭", fg, 0, x0 + 1);
        self.print(x1, y0, "╮", fg, 0, x1 + 1);
        self.print(x0, y1, "╰", fg, 0, x0 + 1);
        self.print(x1, y1, "╯", fg, 0, x1 + 1);
    }
}

/// Sends a frame to the terminal, writing only what differs from the previous frame.
#[derive(Default)]
pub struct Renderer {
    prev: Option<Buffer>,
}

impl Renderer {
    /// Forget the last frame (after a resize): the next one is drawn in full.
    pub fn reset(&mut self) {
        self.prev = None;
    }

    pub fn render(&mut self, buf: &Buffer, cursor: Option<(i32, i32)>) -> String {
        let mut out = String::with_capacity(4096);
        out.push_str("\x1b[?2026h\x1b[?25l");
        let prev = self.prev.as_ref().filter(|p| p.w == buf.w && p.h == buf.h);
        if prev.is_none() {
            out.push_str("\x1b[0m\x1b[2J");
        }
        let mut style: Option<Style> = None;
        for y in 0..buf.h {
            let mut at: i32 = -1; // where the terminal's cursor is on this row, if we know
            for x in 0..buf.w {
                let i = (y * buf.w + x) as usize;
                let cell = buf.cells[i];
                if let Some(p) = prev {
                    if p.cells[i] == cell {
                        continue;
                    }
                }
                if cell.ch == '\0' {
                    continue;
                }
                if at != x {
                    let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
                }
                if style != Some(cell.st) {
                    let s = cell.st;
                    out.push_str("\x1b[0");
                    for (bit, code) in [(BOLD, ";1"), (DIM, ";2"), (ITALIC, ";3"), (UNDERLINE, ";4"), (UNDER2, ";21"), (STRIKE, ";9")] {
                        if s.attr & bit != 0 {
                            out.push_str(code);
                        }
                    }
                    let _ = write!(out, ";38;2;{};{};{};48;2;{};{};{}m", s.fg.0, s.fg.1, s.fg.2, s.bg.0, s.bg.1, s.bg.2);
                    style = Some(s);
                }
                out.push(cell.ch);
                at = x + cw(cell.ch).max(1);
            }
        }
        out.push_str("\x1b[0m");
        if let Some((x, y)) = cursor {
            if x >= 0 && y >= 0 && x < buf.w && y < buf.h {
                let _ = write!(out, "\x1b[{};{}H\x1b[?25h", y + 1, x + 1);
            }
        }
        out.push_str("\x1b[?2026l");
        self.prev = Some(Buffer { w: buf.w, h: buf.h, cells: buf.cells.clone() });
        out
    }
}
