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
        let scrolled = self.parser.screen().scrollback() > 0;
        if scrolled {
            self.parser.screen_mut().set_scrollback(0);
        }
        self.write(&data);
        scrolled
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
