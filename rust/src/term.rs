//! An embedded terminal emulator.
//!
//! A child process runs on a pty; a reader thread forwards its output to the UI
//! thread, where a VT parser turns it into a screen of cells that we paint. Used for
//! both the bottom-panel shell and the Claude Code panel.

use crate::app::Ev;
use crate::screen::{Buffer, Rect, Rgb, Style, BOLD, DIM, ITALIC, UNDERLINE};
use crate::theme::VSTheme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Instant;

const SCROLLBACK: usize = 5000;

/// Collects the answers a terminal owes its program (device attributes, cursor position).
#[derive(Default)]
struct Replies {
    pending: Vec<u8>,
}

impl vt100::Callbacks for Replies {
    fn unhandled_csi(&mut self, screen: &mut vt100::Screen, i1: Option<u8>, _i2: Option<u8>, params: &[&[u16]], c: char) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c) {
            (None, 'c') if first == 0 => self.pending.extend_from_slice(b"\x1b[?6c"),
            (None, 'n') if first == 5 => self.pending.extend_from_slice(b"\x1b[0n"),
            (None, 'n') if first == 6 => {
                let (row, col) = screen.cursor_position();
                self.pending.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            _ => {}
        }
    }
}

fn nz(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value.max(1)).unwrap_or(NonZeroU16::MIN)
}

pub struct Term {
    pub id: usize,
    command: Vec<String>,
    cwd: PathBuf,
    parser: vt100::Parser<Replies>,
    master: Option<Box<dyn MasterPty + Send>>,
    input: Option<Sender<Vec<u8>>>,
    pid: Option<u32>,
    /// Bumped on every (re)start so output from an old process is ignored.
    pub generation: u64,
    pub exited: bool,
    cols: u16,
    rows: u16,
    tx: Sender<Ev>,
    /// Input typed before the process has started (e.g. Run File with the panel hidden).
    queued: Vec<u8>,
    /// Where the terminal was last painted (for mouse coordinates).
    pub area: Rect,
    /// Text selected with the mouse: (anchor, head) as (history line, column), where
    /// history line 0 is the oldest line in the scrollback. Columns are cell indexes;
    /// the end of a selection is exclusive.
    selection: Option<(Spot, Spot)>,
    last_click: Option<(Instant, Spot, u8)>,
}

/// A cell position that stays put while the view scrolls: (history line, column).
type Spot = (usize, u16);

/// Characters that end a word for double-click selection (like VS Code's terminal).
fn word_char(c: char) -> bool {
    !c.is_whitespace() && !"()[]{}<>'\"`|,;│─".contains(c)
}

impl Term {
    pub fn new(id: usize, command: Vec<String>, cwd: PathBuf, tx: Sender<Ev>) -> Term {
        Term {
            id,
            command,
            cwd,
            parser: vt100::Parser::new_with_callbacks(nz(24), nz(80), SCROLLBACK, Replies::default()),
            master: None,
            input: None,
            pid: None,
            generation: 0,
            exited: false,
            cols: 0,
            rows: 0,
            tx,
            queued: Vec::new(),
            area: Rect::default(),
            selection: None,
            last_click: None,
        }
    }

    pub fn running(&self) -> bool {
        self.master.is_some()
    }

    // -- process -----------------------------------------------------------

    fn spawn(&mut self) {
        let (cols, rows) = (self.cols.max(2), self.rows.max(2));
        self.parser = vt100::Parser::new_with_callbacks(nz(rows), nz(cols), SCROLLBACK, Replies::default());
        self.exited = false;
        self.generation += 1;
        self.selection = None;
        if let Err(error) = self.try_spawn(cols, rows) {
            let name = self.command.first().cloned().unwrap_or_default();
            self.exited = true;
            self.parser.process(format!("tscode: cannot run {name}: {error}\r\n\x1b[0;2m[Press Enter to try again]\x1b[0m").as_bytes());
        }
    }

    fn try_spawn(&mut self, cols: u16, rows: u16) -> Result<(), String> {
        let text = |error: &dyn std::fmt::Display| error.to_string();
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| text(&e))?;
        let program = self.command.first().ok_or("nothing to run")?;
        let mut command = CommandBuilder::new(program);
        command.args(&self.command[1..]);
        command.cwd(&self.cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env_remove("COLUMNS");
        command.env_remove("LINES");
        let mut child = pair.slave.spawn_command(command).map_err(|e| text(&e))?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| text(&e))?;
        let mut writer = pair.master.take_writer().map_err(|e| text(&e))?;
        self.pid = child.process_id();
        self.master = Some(pair.master);

        let (id, generation, tx) = (self.id, self.generation, self.tx.clone());
        thread::spawn(move || {
            let mut chunk = vec![0u8; 65536];
            loop {
                match reader.read(&mut chunk) {
                    Ok(count) if count > 0 => {
                        if tx.send(Ev::Pty { term: id, generation, data: chunk[..count].to_vec() }).is_err() {
                            return;
                        }
                    }
                    _ => break,
                }
            }
            let _ = tx.send(Ev::PtyExit { term: id, generation });
        });
        // Writes go through a thread so a program that isn't reading can't stall the UI.
        let (input, queued) = channel::<Vec<u8>>();
        thread::spawn(move || {
            while let Ok(data) = queued.recv() {
                if writer.write_all(&data).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        });
        if !self.queued.is_empty() {
            let _ = input.send(std::mem::take(&mut self.queued));
        }
        self.input = Some(input);
        thread::spawn(move || {
            let _ = child.wait(); // reap it
        });
        Ok(())
    }

    /// Output arrived from the process.
    pub fn feed(&mut self, generation: u64, data: &[u8]) {
        if generation != self.generation {
            return;
        }
        self.parser.process(data);
        let replies = std::mem::take(&mut self.parser.callbacks_mut().pending);
        if !replies.is_empty() {
            self.write(&replies);
        }
    }

    /// The process is gone (its side of the pty closed).
    pub fn closed(&mut self, generation: u64) {
        if generation != self.generation || !self.running() {
            return;
        }
        self.master = None;
        self.input = None;
        self.pid = None;
        self.exited = true;
        self.parser.process(b"\r\n\x1b[0;2m[Process exited - press Enter to restart]\x1b[0m");
    }

    pub fn kill(&mut self) {
        if let Some(pid) = self.pid.take() {
            // The child leads its own session, so this reaches everything it started.
            unsafe {
                libc::killpg(pid as i32, libc::SIGHUP);
            }
        }
        self.master = None;
        self.input = None;
        self.generation += 1;
    }

    pub fn restart(&mut self) {
        self.kill();
        if self.cols >= 2 && self.rows >= 2 {
            self.spawn();
        }
    }

    /// The widget got a (new) size. The process starts the first time this happens.
    pub fn resize(&mut self, cols: i32, rows: i32) {
        if cols < 2 || rows < 2 {
            return;
        }
        let (cols, rows) = (cols.min(1000) as u16, rows.min(1000) as u16);
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.selection = None; // the text reflows, so the old cells no longer mean anything
        if !self.running() {
            if !self.exited {
                self.spawn();
            }
            return;
        }
        self.parser.screen_mut().set_scrollback(0);
        self.parser.set_size(nz(rows), nz(cols));
        if let Some(master) = &self.master {
            let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    // -- writing to the process ---------------------------------------------

    pub fn write(&mut self, data: &[u8]) {
        match &self.input {
            Some(input) => {
                let _ = input.send(data.to_vec());
            }
            None if !self.exited && self.generation == 0 => self.queued.extend_from_slice(data),
            None => {}
        }
    }

    /// Type a command line into the terminal and press Enter.
    pub fn send_line(&mut self, text: &str) {
        self.write(format!("{text}\r").as_bytes());
    }

    // -- events ---------------------------------------------------------------

    /// A key press while the terminal has focus. Returns true when the screen changed.
    pub fn key(&mut self, key: &KeyEvent) -> bool {
        if self.exited {
            if key.code == KeyCode::Enter {
                self.restart();
            }
            return true;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if shift && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            let page = self.rows as i32 - 1;
            self.scroll_by(if key.code == KeyCode::PageUp { page } else { -page });
            return true;
        }
        let Some(data) = self.translate(key) else { return false };
        // Typing ends a selection, like in VS Code's terminal.
        let selected = self.selection.take().is_some();
        let scrolled = self.parser.screen().scrollback() > 0;
        if scrolled {
            self.parser.screen_mut().set_scrollback(0);
        }
        self.write(&data);
        scrolled || selected
    }

    fn translate(&self, key: &KeyEvent) -> Option<Vec<u8>> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let text = |s: &str| Some(s.as_bytes().to_vec());
        let cursor = |final_byte: char| -> Option<Vec<u8>> {
            let modifier = match (ctrl, alt, shift) {
                (false, false, false) => 0,
                (false, false, true) => 2,
                (false, true, false) => 3,
                (true, false, false) => 5,
                (true, false, true) => 6,
                _ => return None,
            };
            Some(if modifier == 0 {
                let lead = if self.parser.screen().application_cursor() { "\x1bO" } else { "\x1b[" };
                format!("{lead}{final_byte}").into_bytes()
            } else {
                format!("\x1b[1;{modifier}{final_byte}").into_bytes()
            })
        };
        match key.code {
            KeyCode::Enter if shift || alt => text("\x1b\r"),
            KeyCode::Enter if ctrl => text("\n"),
            KeyCode::Enter => text("\r"),
            KeyCode::Tab if shift => text("\x1b[Z"),
            KeyCode::Tab => text("\t"),
            KeyCode::BackTab => text("\x1b[Z"),
            KeyCode::Backspace if ctrl => text("\x17"),
            KeyCode::Backspace if alt => text("\x1b\x7f"),
            KeyCode::Backspace => text("\x7f"),
            KeyCode::Esc => text("\x1b"),
            KeyCode::Null => text("\0"),
            KeyCode::Delete if ctrl => text("\x1b[3;5~"),
            KeyCode::Delete => text("\x1b[3~"),
            KeyCode::Insert => text("\x1b[2~"),
            KeyCode::PageUp => text("\x1b[5~"),
            KeyCode::PageDown => text("\x1b[6~"),
            KeyCode::Up => cursor('A'),
            KeyCode::Down => cursor('B'),
            KeyCode::Right => cursor('C'),
            KeyCode::Left => cursor('D'),
            KeyCode::Home => cursor('H'),
            KeyCode::End => cursor('F'),
            KeyCode::F(n) => text(match n {
                1 => "\x1bOP",
                2 => "\x1bOQ",
                3 => "\x1bOR",
                4 => "\x1bOS",
                5 => "\x1b[15~",
                7 => "\x1b[18~",
                8 => "\x1b[19~",
                9 => "\x1b[20~",
                10 => "\x1b[21~",
                11 => "\x1b[23~",
                12 => "\x1b[24~",
                _ => return None,
            }),
            KeyCode::Char(c) => {
                let mut out = Vec::new();
                if alt {
                    out.push(0x1b);
                }
                if ctrl {
                    let lower = c.to_ascii_lowercase();
                    match lower {
                        ' ' | '@' => out.push(0),
                        'a'..='z' => out.push(lower as u8 & 0x1f),
                        '[' | '\\' | ']' | '^' | '_' => out.push(lower as u8 & 0x1f),
                        _ => return None,
                    }
                } else {
                    let mut utf8 = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
                }
                Some(out)
            }
            _ => None,
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.selection = None;
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.parser.screen().bracketed_paste() {
            self.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.write(text.as_bytes());
        }
    }

    /// True when the program inside asked for mouse reports we know how to send.
    pub fn mouse_tracking(&self) -> bool {
        let screen = self.parser.screen();
        screen.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr
            && screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    pub fn mouse_motion(&self) -> bool {
        matches!(
            self.parser.screen().mouse_protocol_mode(),
            vt100::MouseProtocolMode::ButtonMotion | vt100::MouseProtocolMode::AnyMotion
        )
    }

    /// Send a mouse report; `x`/`y` are screen coordinates.
    pub fn send_mouse(&mut self, code: u32, x: i32, y: i32, release: bool) {
        let (x, y) = ((x - self.area.x).max(0) + 1, (y - self.area.y).max(0) + 1);
        self.write(format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).as_bytes());
    }

    pub fn wheel(&mut self, up: bool, x: i32, y: i32) {
        if self.mouse_tracking() {
            self.send_mouse(if up { 64 } else { 65 }, x, y, false);
        } else if self.parser.screen().alternate_screen() {
            // The alternate screen has no scrollback: let the program scroll itself.
            let lead = if self.parser.screen().application_cursor() { "\x1bO" } else { "\x1b[" };
            self.write(format!("{lead}{}", if up { 'A' } else { 'B' }).repeat(3).as_bytes());
        } else {
            self.scroll_by(if up { 3 } else { -3 });
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let current = self.parser.screen().scrollback() as i32;
        self.parser.screen_mut().set_scrollback((current + delta).max(0) as usize);
    }

    // -- selection ------------------------------------------------------------

    /// History line shown on the top row right now.
    fn top_line(&self) -> usize {
        let screen = self.parser.screen();
        screen.scrollback_len().saturating_sub(screen.scrollback())
    }

    /// The cell under screen position (x, y), clamped into the terminal.
    fn spot_at(&self, x: i32, y: i32) -> (u16, Spot) {
        let row = (y - self.area.y).clamp(0, self.rows.max(1) as i32 - 1) as u16;
        let col = (x - self.area.x).clamp(0, self.cols.max(1) as i32 - 1) as u16;
        (row, (self.top_line() + row as usize, col))
    }

    /// The selection as an ordered, end-exclusive range; None when nothing is selected.
    fn range(&self) -> Option<(Spot, Spot)> {
        let (anchor, head) = self.selection?;
        if anchor == head {
            return None;
        }
        let (start, mut end) = if anchor <= head { (anchor, head) } else { (head, anchor) };
        end.1 = (end.1 + 1).min(self.cols);
        Some((start, end))
    }

    pub fn has_selection(&self) -> bool {
        self.range().is_some()
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Left button pressed at (x, y): start a selection. A second click on the same cell
    /// selects the word there, a third the whole row. `extend` (Shift) moves the far end.
    pub fn select_start(&mut self, x: i32, y: i32, extend: bool) {
        let (row, spot) = self.spot_at(x, y);
        if let (true, Some((anchor, _))) = (extend, self.selection) {
            self.selection = Some((anchor, spot));
            return;
        }
        let now = Instant::now();
        let clicks = match self.last_click {
            Some((at, last, count)) if last == spot && now.duration_since(at).as_millis() < 400 => count % 3 + 1,
            _ => 1,
        };
        self.last_click = Some((now, spot, clicks));
        let screen = self.parser.screen();
        let is_word = |col: u16| {
            screen.cell(row, col).map_or(false, |cell| cell.contents().chars().next().map_or(false, word_char))
        };
        self.selection = Some(match clicks {
            2 if is_word(spot.1) => {
                let (mut start, mut end) = (spot.1, spot.1);
                while start > 0 && is_word(start - 1) {
                    start -= 1;
                }
                while end + 1 < self.cols && is_word(end + 1) {
                    end += 1;
                }
                ((spot.0, start), (spot.0, end))
            }
            3 => ((spot.0, 0), (spot.0, self.cols.saturating_sub(1))),
            _ => (spot, spot),
        });
    }

    /// The mouse moved with the button down: move the far end, scrolling at the edges.
    pub fn select_to(&mut self, x: i32, y: i32) {
        if y < self.area.y {
            self.scroll_by(1);
        } else if y >= self.area.bottom() {
            self.scroll_by(-1);
        }
        let (_, spot) = self.spot_at(x, y);
        if let Some((anchor, _)) = self.selection {
            self.selection = Some((anchor, spot));
        }
    }

    /// Select everything: the scrollback and the screen.
    pub fn select_all(&mut self) {
        let last = self.parser.screen().scrollback_len() + self.rows.max(1) as usize - 1;
        self.selection = Some(((0, 0), (last, self.cols.saturating_sub(1))));
    }

    /// The selected text, with wrapped rows joined back into one line.
    pub fn selected_text(&mut self) -> Option<String> {
        let (start, mut end) = self.range()?;
        let rows = self.rows.max(1) as usize;
        let cols = self.cols;
        let screen = self.parser.screen_mut();
        let saved = screen.scrollback();
        let len = screen.scrollback_len();
        // Lines that fell out of the scrollback since the selection was made are gone.
        if end.0 >= len + rows {
            end = (len + rows - 1, cols);
        }
        let mut text = String::new();
        let mut line = start.0;
        // vt100 only reads what is on screen, so walk the selection a screenful at a time.
        while line <= end.0 {
            let last = (line + rows - 1).min(end.0);
            screen.set_scrollback(len.saturating_sub(line));
            let top = len - screen.scrollback();
            let (r0, r1) = ((line - top) as u16, (last - top) as u16);
            let c0 = if line == start.0 { start.1 } else { 0 };
            let c1 = if last == end.0 { end.1 } else { cols };
            text.push_str(&screen.contents_between(r0, c0, r1, c1));
            if last < end.0 && !screen.row_wrapped(r1) {
                text.push('\n');
            }
            line = last + 1;
        }
        screen.set_scrollback(saved);
        let text = text.trim_end_matches('\n').to_string();
        (!text.is_empty()).then_some(text)
    }

    // -- painting -------------------------------------------------------------

    /// Paint the screen into `area`. Returns where the process's cursor is, if it should show.
    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, theme: &VSTheme, focused: bool, line_cursor: bool) -> Option<(i32, i32)> {
        self.area = area;
        let default_fg = theme.c("terminal.foreground");
        let default_bg = theme.c("terminal.background");
        buf.fill(area, Style::new(default_fg, default_bg));
        let screen = self.parser.screen();
        let resolve = |color: vt100::Color, default: Rgb| match color {
            vt100::Color::Default => default,
            vt100::Color::Idx(index) => indexed(theme, index),
            vt100::Color::Rgb(r, g, b) => Rgb(r, g, b),
        };
        let (rows, cols) = (self.rows.min(area.h as u16), self.cols.min(area.w as u16));
        let selected = self.range();
        let selection_bg = theme.c("terminal.selectionBackground");
        let top = screen.scrollback_len().saturating_sub(screen.scrollback());
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let (mut fg, mut bg) = (resolve(cell.fgcolor(), default_fg), resolve(cell.bgcolor(), default_bg));
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if let Some((start, end)) = selected {
                    let spot = (top + row as usize, col);
                    if spot >= start && spot < end {
                        bg = selection_bg;
                    }
                }
                let mut attr = 0;
                for (on, bit) in [(cell.bold(), BOLD), (cell.dim(), DIM), (cell.italic(), ITALIC), (cell.underline(), UNDERLINE)] {
                    if on {
                        attr |= bit;
                    }
                }
                let style = Style { fg, bg, attr };
                let (x, y) = (area.x + col as i32, area.y + row as i32);
                let contents = cell.contents();
                let ch = contents.chars().next().unwrap_or(' ');
                buf.put(x, y, ch, style);
                if cell.is_wide() {
                    buf.put(x + 1, y, '\0', style);
                }
            }
        }
        if !self.running() || screen.scrollback() > 0 || screen.hide_cursor() {
            return None;
        }
        let (row, col) = screen.cursor_position();
        let (x, y) = (area.x + (col.min(self.cols.saturating_sub(1))) as i32, area.y + row as i32);
        if !area.contains(x, y) {
            return None;
        }
        if focused && line_cursor {
            return Some((x, y)); // the app shows the terminal's real bar cursor here
        }
        // Block cursor: solid when focused, dim otherwise.
        let ch = buf.get(x, y).map(|cell| cell.ch).filter(|c| *c != '\0').unwrap_or(' ');
        let style = if focused {
            Style::new(default_bg, theme.c("terminalCursor.foreground"))
        } else {
            Style::new(default_fg, theme.c("disabledForeground"))
        };
        buf.put(x, y, ch, style);
        None
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.kill();
    }
}

/// xterm's 256-colour palette; the first 16 come from the theme.
fn indexed(theme: &VSTheme, index: u8) -> Rgb {
    match index {
        0..=15 => theme.ansi[index as usize],
        16..=231 => {
            let n = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        _ => {
            let gray = 8 + (index - 232) * 10;
            Rgb(gray, gray, gray)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal with no process, showing `text` in a `cols` x `rows` area at (10, 5).
    fn term(cols: u16, rows: u16, text: &str) -> Term {
        let (tx, _rx) = channel();
        let mut term = Term::new(0, vec![], PathBuf::from("/"), tx);
        term.cols = cols;
        term.rows = rows;
        term.parser = vt100::Parser::new_with_callbacks(nz(rows), nz(cols), SCROLLBACK, Replies::default());
        term.area = Rect::new(10, 5, cols as i32, rows as i32);
        term.feed(0, text.as_bytes());
        term
    }

    #[test]
    fn drag_selects_across_rows() {
        let mut t = term(20, 4, "hello world\r\nsecond line");
        t.select_start(16, 5, false); // the "w" of world
        t.select_to(15, 6); // through "second"
        assert_eq!(t.selected_text().as_deref(), Some("world\nsecond"));
    }

    #[test]
    fn click_without_drag_selects_nothing() {
        let mut t = term(20, 4, "hello");
        t.select_start(11, 5, false);
        assert!(!t.has_selection());
        assert_eq!(t.selected_text(), None);
    }

    #[test]
    fn double_click_selects_a_word() {
        let mut t = term(30, 4, "cargo build --release");
        t.select_start(17, 5, false);
        t.select_start(17, 5, false);
        assert_eq!(t.selected_text().as_deref(), Some("build"));
    }

    #[test]
    fn wrapped_rows_copy_as_one_line() {
        let mut t = term(10, 4, "abcdefghijklmno");
        t.select_all();
        assert_eq!(t.selected_text().as_deref(), Some("abcdefghijklmno"));
    }

    #[test]
    fn select_all_reaches_into_the_scrollback() {
        let lines: Vec<String> = (1..=12).map(|n| format!("line {n}")).collect();
        let mut t = term(20, 4, &lines.join("\r\n"));
        t.select_all();
        assert_eq!(t.selected_text().unwrap(), lines.join("\n"));
        assert_eq!(t.parser.screen().scrollback(), 0, "the view goes back to where it was");
    }

    #[test]
    fn selection_follows_its_text_as_output_scrolls() {
        let mut t = term(20, 3, "first\r\nsecond");
        t.select_start(10, 5, false);
        t.select_to(14, 5); // "first"
        t.feed(0, b"\r\nthree\r\nfour\r\nfive");
        assert_eq!(t.selected_text().as_deref(), Some("first"));
    }

    #[test]
    fn typing_clears_the_selection() {
        let mut t = term(20, 4, "hello");
        t.select_all();
        t.key(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!t.has_selection());
    }
}
