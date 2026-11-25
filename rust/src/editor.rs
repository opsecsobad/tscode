//! One open document: the text buffer, cursor and selection, undo history, and the
//! code that paints it (line numbers, syntax colours, error squiggles).

use crate::highlight::{Diagnostic, Span};
use crate::languages::language_for;
use crate::screen::{cw, Buffer, Rect, Rgb, Style, UNDER2};
use crate::theme::VSTheme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

/// Columns the editor's scroll bar takes on the right.
pub const SCROLLBAR_W: i32 = 2;

pub const MAX_FILE_BYTES: u64 = 5_000_000;
const TAB: i32 = 4;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Pos {
    pub row: usize,
    /// In characters.
    pub col: usize,
}

/// Menu commands that act on the active editor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EdCmd {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    SelectLine,
}

struct Edit {
    start: Pos,
    removed: String,
    inserted: String,
    cursor_before: Pos,
    anchor_before: Pos,
}

/// One row on screen: a stretch of a document line (a whole line unless it is wrapped).
#[derive(Clone, Copy)]
struct VisualRow {
    row: usize,
    start: usize,
    end: usize,
}

pub struct Doc {
    pub id: u64,
    pub path: Option<PathBuf>,
    pub title: String,
    /// Set for files we can't show as text; holds the explanation.
    pub not_editable: Option<String>,
    pub lines: Vec<String>,
    crlf: bool,
    pub cursor: Pos,
    pub anchor: Pos,
    goal_x: Option<i32>,
    top: usize,
    left: i32,
    follow: bool,
    /// Put the cursor line in the middle of the view on the next paint.
    centre: bool,
    saved_text: String,
    pub dirty: bool,
    mtime: Option<SystemTime>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    pub language: Option<&'static str>,
    pub language_name: &'static str,
    soft_wrap: bool,
    /// Syntax colours per line, from the highlighting thread.
    pub spans: Vec<Vec<Span>>,
    /// Bumped on every text change.
    pub version: u64,
    pub diagnostics: Vec<Diagnostic>,
    /// When to re-check the file for problems (shortly after the last edit).
    pub check_at: Option<Instant>,
    /// Set by a copy/cut: text the app should hand to the system clipboard.
    pub copied: Option<String>,
    area: Rect,
    text_x: i32,
    text_w: i32,
    last_click: Option<(Instant, Pos, u8)>,
}

fn mtime(path: Option<&Path>) -> Option<SystemTime> {
    fs::metadata(path?).ok()?.modified().ok()
}

fn byte_of(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map(|(i, _)| i).unwrap_or(line.len())
}

fn char_len(line: &str) -> usize {
    line.chars().count()
}

fn cell_width(c: char, x: i32) -> i32 {
    if c == '\t' {
        TAB - x % TAB
    } else {
        cw(c).max(1)
    }
}

/// 0 = space, 1 = word character, 2 = punctuation.
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

impl Doc {
    pub fn new(id: u64, text: &str, path: Option<PathBuf>, title: String) -> Doc {
        let mut doc = Doc {
            id,
            title,
            not_editable: None,
            lines: Vec::new(),
            crlf: false,
            cursor: Pos::default(),
            anchor: Pos::default(),
            goal_x: None,
            top: 0,
            left: 0,
            follow: true,
            centre: false,
            saved_text: if path.is_some() { text.to_string() } else { String::new() },
            dirty: false,
            mtime: mtime(path.as_deref()),
            path,
            undo: Vec::new(),
            redo: Vec::new(),
            language: None,
            language_name: "Plain Text",
            soft_wrap: false,
            spans: Vec::new(),
            version: 1,
            diagnostics: Vec::new(),
            check_at: Some(Instant::now()),
            copied: None,
            area: Rect::default(),
            text_x: 0,
            text_w: 80,
            last_click: None,
        };
        doc.set_text(text);
        doc.apply_language();
        doc
    }

    /// A stand-in tab for a file we can't show as text.
    pub fn not_editable(id: u64, path: PathBuf, reason: &str) -> Doc {
        let title = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut doc = Doc::new(id, "", Some(path), title);
        doc.not_editable = Some(reason.to_string());
        doc.language = None;
        doc.language_name = "Binary";
        doc.check_at = None;
        doc
    }

    pub fn is_text(&self) -> bool {
        self.not_editable.is_none()
    }

    fn set_text(&mut self, text: &str) {
        self.crlf = text.contains("\r\n");
        let text = if self.crlf { text.replace("\r\n", "\n") } else { text.to_string() };
        self.lines = text.split('\n').map(str::to_string).collect();
        self.spans = vec![Vec::new(); self.lines.len()];
    }

    pub fn text(&self) -> String {
        self.lines.join(if self.crlf { "\r\n" } else { "\n" })
    }

    /// The text as the highlighter sees it (always "\n" line ends, so offsets line up).
    pub fn plain_text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn apply_language(&mut self) {
        let (language, name) = language_for(self.path.as_deref());
        self.language = language;
        self.language_name = name;
        // VS Code wraps prose by default and leaves code alone.
        self.soft_wrap = matches!(name, "Markdown" | "Plain Text") && self.path.is_some();
        self.check_at = Some(Instant::now());
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.error).count()
    }

    pub fn warning_count(&self) -> usize {
        self.diagnostics.len() - self.error_count()
    }

    // -- disk -----------------------------------------------------------------

    pub fn write_to_disk(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else { return Ok(()) };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = self.text();
        fs::write(&path, &text)?;
        self.saved_text = text;
        self.dirty = false;
        self.mtime = mtime(Some(&path));
        Ok(())
    }

    /// Pick up external edits (e.g. made by Claude) unless we have unsaved changes.
    pub fn reload_if_changed(&mut self) -> bool {
        if !self.is_text() {
            return false;
        }
        let Some(path) = self.path.clone() else { return false };
        let Some(now) = mtime(Some(&path)) else { return false };
        if Some(now) == self.mtime {
            return false;
        }
        self.mtime = Some(now);
        if self.dirty {
            return false;
        }
        let Ok(text) = fs::read_to_string(&path) else { return false };
        if text == self.saved_text {
            return false;
        }
        let cursor = self.cursor;
        self.saved_text = text.clone();
        self.set_text(&text);
        self.undo.clear();
        self.redo.clear();
        let row = cursor.row.min(self.lines.len() - 1);
        self.cursor = Pos { row, col: cursor.col.min(char_len(&self.lines[row])) };
        self.anchor = self.cursor;
        self.changed();
        true
    }

    fn changed(&mut self) {
        self.version += 1;
        self.check_at = Some(Instant::now() + std::time::Duration::from_millis(400));
    }

    // -- editing ----------------------------------------------------------------

    fn clamp(&self, pos: Pos) -> Pos {
        let row = pos.row.min(self.lines.len() - 1);
        Pos { row, col: pos.col.min(char_len(&self.lines[row])) }
    }

    fn slice(&self, start: Pos, end: Pos) -> String {
        if start.row == end.row {
            let line = &self.lines[start.row];
            return line[byte_of(line, start.col)..byte_of(line, end.col)].to_string();
        }
        let first = &self.lines[start.row];
        let mut out = first[byte_of(first, start.col)..].to_string();
        for line in &self.lines[start.row + 1..end.row] {
            out.push('\n');
            out.push_str(line);
        }
        let last = &self.lines[end.row];
        out.push('\n');
        out.push_str(&last[..byte_of(last, end.col)]);
        out
    }

    /// Replace [start, end) with `text`. Returns where the inserted text ends.
    fn replace(&mut self, start: Pos, end: Pos, text: &str) -> Pos {
        let first = &self.lines[start.row];
        let prefix = first[..byte_of(first, start.col)].to_string();
        let last = &self.lines[end.row];
        let suffix = last[byte_of(last, end.col)..].to_string();
        let mut new_lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let last_index = new_lines.len() - 1;
        let end_pos = Pos {
            row: start.row + last_index,
            col: char_len(&new_lines[last_index]) + if last_index == 0 { start.col } else { 0 },
        };
        new_lines[0].insert_str(0, &prefix);
        new_lines[last_index].push_str(&suffix);
        // Keep the old colours on the first line until the highlighter catches up.
        let mut new_spans = vec![Vec::new(); new_lines.len()];
        if let Some(old) = self.spans.get_mut(start.row) {
            new_spans[0] = std::mem::take(old);
        }
        self.lines.splice(start.row..=end.row, new_lines);
        if self.spans.len() > end.row {
            self.spans.splice(start.row..=end.row, new_spans);
        } else {
            self.spans = vec![Vec::new(); self.lines.len()];
        }
        end_pos
    }

    /// An undoable change: replace [start, end) with `text` and put the cursor after it.
    fn edit(&mut self, start: Pos, end: Pos, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let removed = self.slice(start, end);
        if removed.is_empty() && text.is_empty() {
            return;
        }
        let (cursor_before, anchor_before) = (self.cursor, self.anchor);
        let after = self.replace(start, end, &text);
        // Plain typing joins the previous edit, so one undo takes back a whole word.
        let single = text.chars().count() == 1 && removed.is_empty() && !text.contains('\n');
        let joins = single
            && !text.starts_with(char::is_whitespace)
            && self.undo.last().map_or(false, |last| {
                last.removed.is_empty()
                    && !last.inserted.contains('\n')
                    && last.start.row == start.row
                    && last.start.col + last.inserted.chars().count() == start.col
            });
        if joins {
            if let Some(last) = self.undo.last_mut() {
                last.inserted.push_str(&text);
            }
        } else {
            self.undo.push(Edit { start, removed, inserted: text, cursor_before, anchor_before });
        }
        self.redo.clear();
        self.cursor = after;
        self.anchor = after;
        self.after_change();
    }

    fn after_change(&mut self) {
        self.goal_x = None;
        self.follow = true;
        self.dirty = self.text() != self.saved_text;
        self.changed();
    }

    fn end_of(start: Pos, text: &str) -> Pos {
        let rows = text.matches('\n').count();
        let tail = text.rsplit('\n').next().unwrap_or("").chars().count();
        Pos { row: start.row + rows, col: if rows == 0 { start.col + tail } else { tail } }
    }

    fn undo(&mut self) {
        let Some(edit) = self.undo.pop() else { return };
        let end = Doc::end_of(edit.start, &edit.inserted);
        self.replace(edit.start, end, &edit.removed);
        self.cursor = edit.cursor_before;
        self.anchor = edit.anchor_before;
        self.redo.push(edit);
        self.after_change();
    }

    fn redo(&mut self) {
        let Some(edit) = self.redo.pop() else { return };
        let end = Doc::end_of(edit.start, &edit.removed);
        let after = self.replace(edit.start, end, &edit.inserted);
        self.cursor = after;
        self.anchor = after;
        self.undo.push(edit);
        self.after_change();
    }

    /// The selection as (start, end), if there is one.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        (self.cursor != self.anchor).then(|| (self.cursor.min(self.anchor), self.cursor.max(self.anchor)))
    }

    pub fn selected_len(&self) -> usize {
        self.selection().map(|(start, end)| self.slice(start, end).chars().count()).unwrap_or(0)
    }

    /// Type or paste text over the selection.
    pub fn insert(&mut self, text: &str) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        self.edit(start, end, text);
    }

    /// Accept a completion: replace the word from `start_col` to the cursor with `text`
    /// (one undo step) and select `select` (character offsets into `text`), if given.
    pub fn complete(&mut self, start_col: usize, text: &str, select: Option<(usize, usize)>) {
        let start = Pos { row: self.cursor.row, col: start_col.min(self.cursor.col) };
        let end = self.cursor;
        self.edit(start, end, text);
        if let Some((from, to)) = select {
            let prefix = |n: usize| text.chars().take(n).collect::<String>();
            self.anchor = Doc::end_of(start, &prefix(from));
            self.cursor = Doc::end_of(start, &prefix(to));
        }
    }

    /// Screen cell of a document position, if it is in view (as of the last paint).
    pub fn screen_pos(&self, pos: Pos) -> Option<(i32, i32)> {
        let left = if self.soft_wrap { 0 } else { self.left };
        let line_len = char_len(self.lines.get(pos.row)?);
        for (i, vrow) in self.layout(self.area.h).iter().enumerate() {
            let last = vrow.end == line_len;
            if vrow.row == pos.row && pos.col >= vrow.start && (pos.col < vrow.end || (last && pos.col == vrow.end)) {
                let x = self.text_x + self.x_of(pos.row, vrow.start, pos.col) - left;
                return Some((x.max(self.text_x), self.area.y + i as i32));
            }
        }
        None
    }

    fn line_range(&self, row: usize) -> (Pos, Pos) {
        let start = Pos { row, col: 0 };
        if row + 1 < self.lines.len() {
            (start, Pos { row: row + 1, col: 0 })
        } else {
            (start, Pos { row, col: char_len(&self.lines[row]) })
        }
    }

    pub fn command(&mut self, command: EdCmd, clipboard: &mut String) {
        match command {
            EdCmd::Undo => self.undo(),
            EdCmd::Redo => self.redo(),
            EdCmd::Copy | EdCmd::Cut => {
                // With nothing selected these act on the whole line, like VS Code.
                let (start, end) = self.selection().unwrap_or_else(|| self.line_range(self.cursor.row));
                let text = self.slice(start, end);
                if text.is_empty() {
                    return;
                }
                *clipboard = text.clone();
                self.copied = Some(text);
                if command == EdCmd::Cut {
                    self.edit(start, end, "");
                }
            }
            EdCmd::Paste => {
                if !clipboard.is_empty() {
                    let text = clipboard.clone();
                    self.insert(&text);
                }
            }
            EdCmd::SelectAll => {
                let last = self.lines.len() - 1;
                self.anchor = Pos::default();
                self.cursor = Pos { row: last, col: char_len(&self.lines[last]) };
                self.follow = true;
            }
            EdCmd::SelectLine => {
                let (start, end) = self.line_range(self.cursor.row);
                self.anchor = start;
                self.cursor = end;
                self.follow = true;
            }
        }
    }

    // -- movement ---------------------------------------------------------------

    fn move_to(&mut self, pos: Pos, extend: bool) {
        self.cursor = self.clamp(pos);
        if !extend {
            self.anchor = self.cursor;
        }
        self.follow = true;
    }

    /// Jump to a 0-based line, centred in the view.
    pub fn go_to_line(&mut self, row: usize) {
        let row = row.min(self.lines.len() - 1);
        self.move_to(Pos { row, col: 0 }, false);
        self.centre = true;
    }

    fn left_of(&self, pos: Pos) -> Pos {
        if pos.col > 0 {
            Pos { row: pos.row, col: pos.col - 1 }
        } else if pos.row > 0 {
            Pos { row: pos.row - 1, col: char_len(&self.lines[pos.row - 1]) }
        } else {
            pos
        }
    }

    fn right_of(&self, pos: Pos) -> Pos {
        if pos.col < char_len(&self.lines[pos.row]) {
            Pos { row: pos.row, col: pos.col + 1 }
        } else if pos.row + 1 < self.lines.len() {
            Pos { row: pos.row + 1, col: 0 }
        } else {
            pos
        }
    }

    fn word_left(&self, pos: Pos) -> Pos {
        if pos.col == 0 {
            return self.left_of(pos);
        }
        let chars: Vec<char> = self.lines[pos.row].chars().collect();
        let mut col = pos.col;
        while col > 0 && class(chars[col - 1]) == 0 {
            col -= 1;
        }
        if col > 0 {
            let kind = class(chars[col - 1]);
            while col > 0 && class(chars[col - 1]) == kind {
                col -= 1;
            }
        }
        Pos { row: pos.row, col }
    }

    fn word_right(&self, pos: Pos) -> Pos {
        let chars: Vec<char> = self.lines[pos.row].chars().collect();
        if pos.col >= chars.len() {
            return self.right_of(pos);
        }
        let mut col = pos.col;
        while col < chars.len() && class(chars[col]) == 0 {
            col += 1;
        }
        if col < chars.len() {
            let kind = class(chars[col]);
            while col < chars.len() && class(chars[col]) == kind {
                col += 1;
            }
        }
        Pos { row: pos.row, col }
    }

    /// Break a line into the stretches shown on separate rows (one stretch unless wrapping).
    fn wraps(&self, row: usize, width: i32) -> Vec<(usize, usize)> {
        let line = &self.lines[row];
        if !self.soft_wrap || width < 4 {
            return vec![(0, char_len(line))];
        }
        let mut out = Vec::new();
        let (mut start, mut x, mut last_space) = (0usize, 0i32, None::<usize>);
        for (i, c) in line.chars().enumerate() {
            let w = cell_width(c, x);
            if x + w > width && i > start {
                let cut = last_space.filter(|s| *s > start).unwrap_or(i);
                out.push((start, cut));
                start = cut;
                last_space = None;
                x = line.chars().skip(start).take(i - start).fold(0, |x, c| x + cell_width(c, x));
            }
            x += w;
            if c == ' ' {
                last_space = Some(i + 1);
            }
        }
        out.push((start, char_len(line)));
        out
    }

    /// Screen x (relative to the start of its stretch) of a column.
    fn x_of(&self, row: usize, start: usize, col: usize) -> i32 {
        self.lines[row].chars().skip(start).take(col.saturating_sub(start)).fold(0, |x, c| x + cell_width(c, x))
    }

    /// The column in [start, end] closest to a screen x.
    fn col_at(&self, row: usize, start: usize, end: usize, target: i32) -> usize {
        let mut x = 0;
        for (i, c) in self.lines[row].chars().enumerate().skip(start).take(end - start) {
            let w = cell_width(c, x);
            if target < x + (w + 1) / 2 {
                return i;
            }
            x += w;
        }
        end
    }

    fn stretch_of(&self, pos: Pos) -> (Vec<(usize, usize)>, usize) {
        let wraps = self.wraps(pos.row, self.text_w);
        let index = wraps
            .iter()
            .position(|(start, end)| pos.col >= *start && (pos.col < *end || *end == char_len(&self.lines[pos.row])))
            .unwrap_or(wraps.len() - 1);
        (wraps, index)
    }

    fn move_vertical(&mut self, delta: i32, extend: bool) {
        let mut pos = self.cursor;
        let (mut wraps, mut index) = self.stretch_of(pos);
        let goal = self.goal_x.unwrap_or_else(|| self.x_of(pos.row, wraps[index].0, pos.col));
        for _ in 0..delta.abs() {
            if delta < 0 {
                if index > 0 {
                    index -= 1;
                } else if pos.row > 0 {
                    pos.row -= 1;
                    wraps = self.wraps(pos.row, self.text_w);
                    index = wraps.len() - 1;
                } else {
                    break;
                }
            } else if index + 1 < wraps.len() {
                index += 1;
            } else if pos.row + 1 < self.lines.len() {
                pos.row += 1;
                wraps = self.wraps(pos.row, self.text_w);
                index = 0;
            } else {
                break;
            }
        }
        let (start, end) = wraps[index];
        pos.col = self.col_at(pos.row, start, end, goal);
        self.move_to(pos, extend);
        self.goal_x = Some(goal);
    }

    // -- keys -------------------------------------------------------------------

    /// A key press while the editor has focus. Returns true if it was used.
    pub fn key(&mut self, key: &KeyEvent, clipboard: &mut String) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let cursor = self.cursor;
        let goal = self.goal_x.take();
        let page = (self.area.h - 1).max(1);
        match key.code {
            KeyCode::Left if ctrl => self.move_to(self.word_left(cursor), shift),
            KeyCode::Right if ctrl => self.move_to(self.word_right(cursor), shift),
            KeyCode::Left => match self.selection() {
                Some((start, _)) if !shift => self.move_to(start, false),
                _ => self.move_to(self.left_of(cursor), shift),
            },
            KeyCode::Right => match self.selection() {
                Some((_, end)) if !shift => self.move_to(end, false),
                _ => self.move_to(self.right_of(cursor), shift),
            },
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                self.goal_x = goal;
                let delta = match key.code {
                    KeyCode::Up => -1,
                    KeyCode::Down => 1,
                    KeyCode::PageUp => -page,
                    _ => page,
                };
                self.move_vertical(delta, shift);
            }
            KeyCode::Home if ctrl => self.move_to(Pos::default(), shift),
            KeyCode::End if ctrl => {
                let last = self.lines.len() - 1;
                self.move_to(Pos { row: last, col: usize::MAX }, shift);
            }
            KeyCode::Home => {
                // First press: the first non-blank character; again: the very start.
                let (wraps, index) = self.stretch_of(cursor);
                let start = wraps[index].0;
                let indent = self.lines[cursor.row].chars().skip(start).take_while(|c| c.is_whitespace()).count();
                let col = if cursor.col == start + indent { start } else { start + indent };
                self.move_to(Pos { row: cursor.row, col }, shift);
            }
            KeyCode::End => {
                let (wraps, index) = self.stretch_of(cursor);
                let (_, end) = wraps[index];
                let last = index + 1 == wraps.len();
                self.move_to(Pos { row: cursor.row, col: if last { end } else { end.saturating_sub(1) } }, shift);
            }
            KeyCode::Backspace => match self.selection() {
                Some((start, end)) => self.edit(start, end, ""),
                None => {
                    let start = if ctrl || alt { self.word_left(cursor) } else { self.left_of(cursor) };
                    self.edit(start, cursor, "");
                }
            },
            KeyCode::Delete => match self.selection() {
                Some((start, end)) => self.edit(start, end, ""),
                None => {
                    let end = if ctrl { self.word_right(cursor) } else { self.right_of(cursor) };
                    self.edit(cursor, end, "");
                }
            },
            KeyCode::Enter => {
                let indent: String =
                    self.lines[cursor.row].chars().take(cursor.col).take_while(|c| *c == ' ' || *c == '\t').collect();
                self.insert(&format!("\n{indent}"));
            }
            KeyCode::Tab if !shift => match self.selection() {
                Some((start, end)) if start.row != end.row => self.indent_lines(start.row, end.row, true),
                _ => {
                    let x = self.x_of(cursor.row, 0, self.selection().map(|(s, _)| s.col).unwrap_or(cursor.col));
                    self.insert(&" ".repeat((TAB - x % TAB) as usize));
                }
            },
            KeyCode::BackTab | KeyCode::Tab => {
                let (start, end) = self.selection().unwrap_or((cursor, cursor));
                self.indent_lines(start.row, end.row, false);
            }
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                'a' => self.command(EdCmd::SelectAll, clipboard),
                'z' if shift => self.command(EdCmd::Redo, clipboard),
                'z' => self.command(EdCmd::Undo, clipboard),
                'y' => self.command(EdCmd::Redo, clipboard),
                'x' => self.command(EdCmd::Cut, clipboard),
                'c' => self.command(EdCmd::Copy, clipboard),
                'v' => self.command(EdCmd::Paste, clipboard),
                'l' => self.command(EdCmd::SelectLine, clipboard),
                'k' => {
                    let end = Pos { row: cursor.row, col: char_len(&self.lines[cursor.row]) };
                    self.edit(cursor, if end == cursor { self.right_of(cursor) } else { end }, "");
                }
                'u' => self.edit(Pos { row: cursor.row, col: 0 }, cursor, ""),
                _ => return false,
            },
            KeyCode::Char(c) if !alt => {
                let mut utf8 = [0u8; 4];
                self.insert(c.encode_utf8(&mut utf8));
            }
            _ => return false,
        }
        true
    }

    fn indent_lines(&mut self, first: usize, last: usize, indent: bool) {
        let (cursor, anchor) = (self.cursor, self.anchor);
        let start = Pos { row: first, col: 0 };
        let end = Pos { row: last, col: char_len(&self.lines[last]) };
        let mut shift = vec![0i32; last - first + 1];
        let lines: Vec<String> = self.lines[first..=last]
            .iter()
            .enumerate()
            .map(|(i, line)| {
                if indent {
                    if line.is_empty() {
                        return line.clone();
                    }
                    shift[i] = TAB;
                    format!("    {line}")
                } else {
                    let strip = line.chars().take(TAB as usize).take_while(|c| *c == ' ').count();
                    let strip = if strip == 0 && line.starts_with('\t') { 1 } else { strip };
                    shift[i] = -(strip as i32);
                    line.chars().skip(strip).collect()
                }
            })
            .collect();
        if shift.iter().all(|s| *s == 0) {
            return;
        }
        self.edit(start, end, &lines.join("\n"));
        let moved = |pos: Pos| Pos { row: pos.row, col: (pos.col as i32 + shift[pos.row - first]).max(0) as usize };
        self.cursor = self.clamp(moved(cursor));
        self.anchor = self.clamp(moved(anchor));
    }

    // -- layout and mouse -------------------------------------------------------

    /// The rows that fit in the view, starting at the first visible line.
    fn layout(&self, height: i32) -> Vec<VisualRow> {
        let mut rows = Vec::new();
        let mut row = self.top.min(self.lines.len() - 1);
        while (rows.len() as i32) < height && row < self.lines.len() {
            for (start, end) in self.wraps(row, self.text_w) {
                rows.push(VisualRow { row, start, end });
            }
            row += 1;
        }
        rows.truncate(height.max(0) as usize);
        rows
    }

    fn ensure_cursor_visible(&mut self) {
        let height = self.area.h.max(1);
        if self.cursor.row < self.top {
            self.top = self.cursor.row;
        }
        loop {
            // Rows from the top of the view down to the cursor's own row.
            let mut used = 0;
            for row in self.top..=self.cursor.row {
                if row == self.cursor.row {
                    used += self.stretch_of(self.cursor).1 as i32 + 1;
                } else {
                    used += self.wraps(row, self.text_w).len() as i32;
                }
                if used > height {
                    break;
                }
            }
            if used <= height || self.top >= self.cursor.row {
                break;
            }
            self.top += 1;
        }
        if self.soft_wrap {
            self.left = 0;
        } else {
            let x = self.x_of(self.cursor.row, 0, self.cursor.col);
            if x < self.left {
                self.left = (x - 4).max(0);
            } else if x >= self.left + self.text_w {
                self.left = x - self.text_w + 1;
            }
        }
    }

    /// The document position under a screen coordinate.
    fn pos_at(&self, x: i32, y: i32) -> Pos {
        let rows = self.layout(self.area.h);
        let Some(last) = rows.last() else { return Pos::default() };
        let index = (y - self.area.y).max(0) as usize;
        if index >= rows.len() {
            return Pos { row: last.row, col: last.end };
        }
        let vrow = rows[index];
        let target = x - self.text_x + if self.soft_wrap { 0 } else { self.left };
        Pos { row: vrow.row, col: self.col_at(vrow.row, vrow.start, vrow.end, target) }
    }

    /// The problem under the mouse, if any (shown as a tooltip).
    pub fn problem_at(&self, x: i32, y: i32) -> Option<&str> {
        if self.diagnostics.is_empty() || !self.area.contains(x, y) || x < self.text_x {
            return None;
        }
        let pos = self.pos_at(x, y);
        self.diagnostics.iter().find(|d| d.covers(pos.row, pos.col)).map(|d| d.message.as_str())
    }

    pub fn mouse_down(&mut self, x: i32, y: i32, extend: bool) {
        let pos = self.pos_at(x, y);
        let now = Instant::now();
        let clicks = match self.last_click {
            Some((at, last, count)) if last == pos && now.duration_since(at).as_millis() < 400 => count % 3 + 1,
            _ => 1,
        };
        self.last_click = Some((now, pos, clicks));
        self.goal_x = None;
        match clicks {
            2 => {
                let chars: Vec<char> = self.lines[pos.row].chars().collect();
                let at = pos.col.min(chars.len().saturating_sub(1));
                if chars.is_empty() {
                    return;
                }
                let kind = class(chars[at]);
                let (mut start, mut end) = (at, at + 1);
                while start > 0 && class(chars[start - 1]) == kind {
                    start -= 1;
                }
                while end < chars.len() && class(chars[end]) == kind {
                    end += 1;
                }
                self.anchor = Pos { row: pos.row, col: start };
                self.cursor = Pos { row: pos.row, col: end };
            }
            3 => {
                let (start, end) = self.line_range(pos.row);
                self.anchor = start;
                self.cursor = end;
            }
            _ => self.move_to(pos, extend),
        }
    }

    pub fn mouse_drag(&mut self, x: i32, y: i32) {
        if y < self.area.y {
            self.scroll(-1);
        } else if y >= self.area.bottom() {
            self.scroll(1);
        }
        let pos = self.pos_at(x, y.clamp(self.area.y, self.area.bottom() - 1));
        self.cursor = self.clamp(pos);
    }

    pub fn scroll(&mut self, delta: i32) {
        let limit = if self.soft_wrap {
            self.lines.len() as i32 - 1
        } else {
            (self.lines.len() as i32 - self.area.h).max(0)
        };
        self.top = (self.top as i32 + delta).clamp(0, limit.max(0)) as usize;
    }

    /// Jump so the scrollbar thumb sits under screen row `y` (click or drag on the scrollbar).
    pub fn scrollbar_to(&mut self, y: i32) {
        let h = self.area.h.max(1);
        let line = (y - self.area.y).clamp(0, h - 1) as i64 * self.lines.len() as i64 / h as i64;
        self.scroll(line as i32 - h / 2 - self.top as i32);
    }

    /// True when (x, y) is over the text itself, not the line numbers or the scrollbar.
    pub fn over_text(&self, x: i32, y: i32) -> bool {
        self.area.contains(x, y) && x >= self.text_x && x < self.area.right() - SCROLLBAR_W
    }

    pub fn scroll_sideways(&mut self, delta: i32) {
        if !self.soft_wrap {
            self.left = (self.left + delta).max(0);
        }
    }

    // -- painting ---------------------------------------------------------------

    /// The other half of the bracket at (or just before) the cursor.
    fn matching_bracket(&self) -> Option<(Pos, Pos)> {
        const OPEN: &str = "([{";
        const CLOSE: &str = ")]}";
        let chars: Vec<char> = self.lines[self.cursor.row].chars().collect();
        let at = [self.cursor.col, self.cursor.col.wrapping_sub(1)]
            .into_iter()
            .find(|col| chars.get(*col).map_or(false, |c| OPEN.contains(*c) || CLOSE.contains(*c)))?;
        let here = chars[at];
        let forward = OPEN.contains(here);
        let partner = if forward {
            CLOSE.chars().nth(OPEN.chars().position(|c| c == here)?)?
        } else {
            OPEN.chars().nth(CLOSE.chars().position(|c| c == here)?)?
        };
        let origin = Pos { row: self.cursor.row, col: at };
        let mut depth = 0;
        let mut row = origin.row;
        for _ in 0..2000 {
            let line: Vec<char> = if row == origin.row { chars.clone() } else { self.lines[row].chars().collect() };
            let columns: Vec<usize> = if forward {
                (if row == origin.row { at } else { 0 }..line.len()).collect()
            } else {
                (0..if row == origin.row { at + 1 } else { line.len() }).rev().collect()
            };
            for col in columns {
                if line[col] == here {
                    depth += 1;
                } else if line[col] == partner {
                    depth -= 1;
                    if depth == 0 {
                        return Some((origin, Pos { row, col }));
                    }
                }
            }
            if forward {
                row += 1;
                if row >= self.lines.len() {
                    break;
                }
            } else if row == 0 {
                break;
            } else {
                row -= 1;
            }
        }
        None
    }

    /// Paint the document into `area`. Returns the cursor's screen position if it is in view.
    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, theme: &VSTheme, focused: bool, line_cursor: bool) -> Option<(i32, i32)> {
        let bg = theme.c("editor.background");
        let fg = theme.c("editor.foreground");
        buf.fill(area, Style::new(fg, bg));
        let digits = self.lines.len().to_string().len() as i32;
        self.area = area;
        self.text_x = area.x + 1 + digits + 2;
        self.text_w = (area.right() - SCROLLBAR_W - self.text_x).max(1);
        if self.follow {
            self.follow = false;
            if std::mem::take(&mut self.centre) {
                self.top = self.cursor.row.saturating_sub((area.h / 2).max(0) as usize);
            }
            self.ensure_cursor_visible();
        }
        let line_bg = theme.c("editor.lineHighlightBackground");
        let selection_bg = theme.c("editor.selectionBackground");
        let bracket_bg = theme.c("editorBracketMatch.background");
        let number_fg = theme.c("editorLineNumber.foreground");
        let number_active = theme.c("editorLineNumber.activeForeground");
        let error_fg = theme.c("editorError.foreground");
        let warning_fg = theme.c("editorWarning.foreground");
        let syntax = theme.syn(self.language);
        let selection = self.selection();
        let brackets = if selection.is_none() { self.matching_bracket() } else { None };
        let text_right = self.text_x + self.text_w;
        let left = if self.soft_wrap { 0 } else { self.left };
        let mut cursor_at = None;

        let rows = self.layout(area.h);
        for (i, vrow) in rows.iter().enumerate() {
            let y = area.y + i as i32;
            let line = &self.lines[vrow.row];
            let on_cursor_line = vrow.row == self.cursor.row;
            let row_bg = if on_cursor_line { line_bg } else { bg };
            if on_cursor_line {
                buf.fill(Rect::new(area.x + 1, y, area.w - 1, 1), Style::new(fg, row_bg));
            }
            if vrow.start == 0 {
                let number = format!("{:>width$}", vrow.row + 1, width = digits as usize);
                buf.print(area.x + 1, y, &number, if on_cursor_line { number_active } else { number_fg }, 0, self.text_x);
            }
            // Colours for this line: syntax first, problems on top.
            let chars: Vec<char> = line.chars().collect();
            let mut colours: Vec<(Rgb, u8)> = vec![(fg, 0); chars.len()];
            if chars.len() <= 5000 {
                if let Some(spans) = self.spans.get(vrow.row) {
                    let mut offsets = Vec::with_capacity(chars.len() + 1);
                    let mut offset = 0u32;
                    for c in &chars {
                        offsets.push(offset);
                        offset += c.len_utf8() as u32;
                    }
                    let mut next = 0;
                    for span in spans {
                        let Some(style) = syntax.get(span.id as usize) else { continue };
                        while next < chars.len() && offsets[next] < span.start {
                            next += 1;
                        }
                        let mut col = next;
                        while col < chars.len() && offsets[col] < span.end {
                            colours[col] = (style.fg.unwrap_or(fg), style.attr);
                            col += 1;
                        }
                    }
                }
                for problem in self.diagnostics.iter().filter(|d| d.line == vrow.row) {
                    let end = problem.end_column.unwrap_or(chars.len()).min(chars.len());
                    for colour in colours.iter_mut().take(end).skip(problem.column) {
                        *colour = (if problem.error { error_fg } else { warning_fg }, UNDER2);
                    }
                }
            }
            let selected = |col: usize| {
                let pos = Pos { row: vrow.row, col };
                selection.map_or(false, |(start, end)| pos >= start && pos < end)
            };
            let mut x = 0;
            for col in vrow.start..vrow.end {
                let c = chars[col];
                let w = cell_width(c, x);
                let sx = self.text_x + x - left;
                if sx >= self.text_x && sx + w <= text_right {
                    let pos = Pos { row: vrow.row, col };
                    let cell_bg = if selected(col) {
                        selection_bg
                    } else if brackets.map_or(false, |(a, b)| pos == a || pos == b) {
                        bracket_bg
                    } else {
                        row_bg
                    };
                    let (colour, attr) = colours[col];
                    let style = Style { fg: colour, bg: cell_bg, attr };
                    if c == '\t' || cw(c) == 0 {
                        for offset in 0..w {
                            buf.put(sx + offset, y, ' ', style);
                        }
                    } else {
                        buf.put(sx, y, c, style);
                        if w == 2 {
                            buf.put(sx + 1, y, '\0', style);
                        }
                    }
                }
                x += w;
            }
            // A selected line break shows as one selected cell after the text.
            let last_stretch = vrow.end == chars.len();
            if last_stretch && selected(chars.len()) {
                let sx = self.text_x + x - left;
                if sx >= self.text_x && sx < text_right {
                    buf.put(sx, y, ' ', Style::new(fg, selection_bg));
                }
            }
            let holds_cursor = on_cursor_line
                && self.cursor.col >= vrow.start
                && (self.cursor.col < vrow.end || (last_stretch && self.cursor.col == vrow.end));
            if holds_cursor && cursor_at.is_none() {
                let sx = self.text_x + self.x_of(vrow.row, vrow.start, self.cursor.col) - left;
                if sx >= self.text_x && sx < text_right {
                    cursor_at = Some((sx, y));
                }
            }
        }
        // Scroll bar.
        let total = self.lines.len() as i32;
        if total > area.h && area.h > 0 {
            // Measured in eighths of a cell so the thumb glides instead of jumping a row at a time.
            const LOWER: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
            let slider = theme.c("scrollbarSlider.background");
            let track = area.h * 8;
            let size = (track as i64 * area.h as i64 / total as i64).max(8) as i32;
            let offset = (self.top as i64 * (track - size) as i64 / (total - area.h).max(1) as i64).min((track - size) as i64) as i32;
            for y in offset / 8..=(offset + size - 1) / 8 {
                let (cell_top, cell_end) = (y * 8, y * 8 + 8);
                // A thumb edge inside the cell: part-height blocks only grow from the bottom, so the
                // lower edge is drawn as the gap below it with the colours swapped.
                let (ch, style) = if offset > cell_top {
                    (LOWER[(cell_end - offset).clamp(0, 8) as usize], Style::new(slider, bg))
                } else if offset + size < cell_end {
                    (LOWER[(cell_end - offset - size).clamp(0, 8) as usize], Style::new(bg, slider))
                } else {
                    ('█', Style::new(slider, bg))
                };
                for x in area.right() - SCROLLBAR_W..area.right() {
                    buf.put(x, area.y + y, ch, style);
                }
            }
        }
        if !focused {
            return None;
        }
        if !line_cursor {
            // Block cursor: paint the cell ourselves.
            if let Some((x, y)) = cursor_at {
                let ch = buf.get(x, y).map(|cell| cell.ch).filter(|c| *c != '\0').unwrap_or(' ');
                buf.put(x, y, ch, Style::new(bg, theme.c("editorCursor.foreground")));
            }
            return None;
        }
        cursor_at
    }
}
