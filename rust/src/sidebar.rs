//! The primary side bar's views: Explorer, Search (ripgrep) and Source Control (git status).

use crate::app::Ev;
use crate::icons::{file_icon, icon};
use crate::screen::{ellipsize, width, Buffer, Rect, Rgb, Style};
use crate::theme::VSTheme;
use crate::util::which;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::SystemTime;

const HIDDEN: &[&str] = &[".git", ".DS_Store"];

/// A single-line text field (search box, Save As prompt, command palette).
#[derive(Default)]
pub struct TextInput {
    pub value: String,
    /// Cursor position in characters.
    pub cursor: usize,
}

impl TextInput {
    pub fn new(value: &str) -> TextInput {
        TextInput { value: value.to_string(), cursor: value.chars().count() }
    }

    fn byte(&self, col: usize) -> usize {
        self.value.char_indices().nth(col).map(|(i, _)| i).unwrap_or(self.value.len())
    }

    pub fn insert(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        let at = self.byte(self.cursor);
        self.value.insert_str(at, &text);
        self.cursor += text.chars().count();
    }

    /// Returns true if the key was used (the value or cursor may have changed).
    pub fn key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let length = self.value.chars().count();
        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = length,
            KeyCode::Char('u') if ctrl => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                let mut utf8 = [0u8; 4];
                self.insert(c.encode_utf8(&mut utf8));
            }
            KeyCode::Backspace if ctrl => {
                let chars: Vec<char> = self.value.chars().collect();
                let mut start = self.cursor;
                while start > 0 && chars[start - 1].is_whitespace() {
                    start -= 1;
                }
                while start > 0 && !chars[start - 1].is_whitespace() {
                    start -= 1;
                }
                self.value.replace_range(self.byte(start)..self.byte(self.cursor), "");
                self.cursor = start;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.value.replace_range(self.byte(self.cursor - 1)..self.byte(self.cursor), "");
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < length => {
                self.value.replace_range(self.byte(self.cursor)..self.byte(self.cursor + 1), "");
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(length),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = length,
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        true
    }

    /// Paint the field on one row (padding of one cell each side). Returns the cursor position.
    pub fn draw(&self, buf: &mut Buffer, area: Rect, theme: &VSTheme, bg: Rgb, placeholder: &str) -> (i32, i32) {
        buf.fill(area, Style::new(theme.c("foreground"), bg));
        let inner = (area.w - 2).max(1);
        if self.value.is_empty() {
            buf.print(area.x + 1, area.y, placeholder, theme.c("disabledForeground"), 0, area.right() - 1);
            return (area.x + 1, area.y);
        }
        // Keep the cursor in view on long values.
        let before: String = self.value.chars().take(self.cursor).collect();
        let skip = (width(&before) - inner + 1).max(0);
        let mut x = area.x + 1 - skip;
        for c in self.value.chars() {
            let w = crate::screen::cw(c);
            if x >= area.x + 1 && x + w <= area.right() - 1 {
                buf.print(x, area.y, &c.to_string(), theme.c("foreground"), 0, area.right() - 1);
            }
            x += w;
        }
        (area.x + 1 + width(&before) - skip, area.y)
    }
}

/// Row colours for a list: (foreground override, background) for a row's state.
fn row_style(theme: &VSTheme, selected: bool, focused: bool, hovered: bool) -> (Option<Rgb>, Option<Rgb>) {
    if selected && focused {
        (Some(theme.c("list.activeSelectionForeground")), Some(theme.c("list.activeSelectionBackground")))
    } else if selected {
        (None, Some(theme.c("list.inactiveSelectionBackground")))
    } else if hovered {
        (None, Some(theme.c("list.hoverBackground")))
    } else {
        (None, None)
    }
}

fn scroll_into_view(top: &mut usize, selected: usize, height: i32) {
    let height = height.max(1) as usize;
    if selected < *top {
        *top = selected;
    } else if selected >= *top + height {
        *top = selected + 1 - height;
    }
}

fn clamp_top(top: &mut usize, total: usize, height: i32) {
    *top = (*top).min(total.saturating_sub(height.max(1) as usize));
}

// -- Explorer -------------------------------------------------------------------------

pub struct Entry {
    pub path: PathBuf,
    name: String,
    pub is_dir: bool,
    depth: usize,
    expanded: bool,
}

pub struct Explorer {
    root: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
    top: usize,
    mtimes: HashMap<PathBuf, SystemTime>,
    pub area: Rect,
}

fn read_folder(path: &Path, depth: usize) -> Vec<Entry> {
    let Ok(listing) = fs::read_dir(path) else { return Vec::new() };
    let mut entries: Vec<Entry> = listing
        .flatten()
        .filter_map(|item| {
            let name = item.file_name().to_string_lossy().to_string();
            if HIDDEN.contains(&name.as_str()) {
                return None;
            }
            let path = item.path();
            Some(Entry { is_dir: path.is_dir(), path, name, depth, expanded: false })
        })
        .collect();
    entries.sort_by_key(|entry| (!entry.is_dir, entry.name.to_lowercase()));
    entries
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

impl Explorer {
    pub fn new(root: PathBuf) -> Explorer {
        let mut explorer =
            Explorer { root, entries: Vec::new(), selected: 0, top: 0, mtimes: HashMap::new(), area: Rect::default() };
        explorer.reload();
        explorer
    }

    fn build(&self, path: &Path, depth: usize, expanded: &HashSet<PathBuf>, out: &mut Vec<Entry>) {
        for mut entry in read_folder(path, depth) {
            let open = entry.is_dir && expanded.contains(&entry.path);
            entry.expanded = open;
            let child = entry.path.clone();
            out.push(entry);
            if open {
                self.build(&child, depth + 1, expanded, out);
            }
        }
    }

    /// Re-read the tree from disk, keeping open folders open and the selection in place.
    pub fn reload(&mut self) {
        let expanded: HashSet<PathBuf> = self.entries.iter().filter(|e| e.expanded).map(|e| e.path.clone()).collect();
        let selected = self.entries.get(self.selected).map(|e| e.path.clone());
        let mut entries = Vec::new();
        self.build(&self.root.clone(), 0, &expanded, &mut entries);
        self.entries = entries;
        self.selected = selected
            .and_then(|path| self.entries.iter().position(|e| e.path == path))
            .unwrap_or(self.selected.min(self.entries.len().saturating_sub(1)));
        self.mtimes.clear();
        self.remember_mtimes();
    }

    fn watched(&self) -> Vec<PathBuf> {
        let mut folders = vec![self.root.clone()];
        folders.extend(self.entries.iter().filter(|e| e.expanded).map(|e| e.path.clone()));
        folders
    }

    fn remember_mtimes(&mut self) {
        for folder in self.watched() {
            if let Some(time) = modified(&folder) {
                self.mtimes.insert(folder, time);
            }
        }
    }

    /// Reload when an open folder changed on disk (e.g. Claude created a file).
    pub fn poll(&mut self) -> bool {
        let changed = self.watched().iter().any(|folder| match (modified(folder), self.mtimes.get(folder)) {
            (Some(now), Some(before)) => now != *before,
            (Some(_), None) => true,
            _ => false,
        });
        if changed {
            self.reload();
        }
        changed
    }

    fn toggle(&mut self, index: usize) {
        let Some(entry) = self.entries.get_mut(index) else { return };
        if !entry.is_dir {
            return;
        }
        entry.expanded = !entry.expanded;
        let (path, depth, open) = (entry.path.clone(), entry.depth, entry.expanded);
        if open {
            let children = read_folder(&path, depth + 1);
            self.entries.splice(index + 1..index + 1, children);
            if let Some(time) = modified(&path) {
                self.mtimes.insert(path, time);
            }
        } else {
            let end = self.entries[index + 1..].iter().position(|e| e.depth <= depth).map(|i| index + 1 + i).unwrap_or(self.entries.len());
            self.entries.drain(index + 1..end);
        }
    }

    pub fn collapse_all(&mut self) {
        self.entries.retain(|entry| entry.depth == 0);
        for entry in &mut self.entries {
            entry.expanded = false;
        }
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        self.top = 0;
    }

    /// Open the selected row: folders toggle, files are returned to be opened.
    fn activate(&mut self, index: usize) -> Option<PathBuf> {
        let entry = self.entries.get(index)?;
        if entry.is_dir {
            self.toggle(index);
            None
        } else {
            Some(entry.path.clone())
        }
    }

    pub fn key(&mut self, key: &KeyEvent) -> Option<PathBuf> {
        if self.entries.is_empty() {
            return None;
        }
        let last = self.entries.len() - 1;
        let page = (self.area.h - 1).max(1) as usize;
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(last),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(page),
            KeyCode::PageDown => self.selected = (self.selected + page).min(last),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            KeyCode::Enter | KeyCode::Char(' ') => return self.activate(self.selected),
            KeyCode::Right => {
                let entry = &self.entries[self.selected];
                if entry.is_dir && !entry.expanded {
                    self.toggle(self.selected);
                } else if entry.is_dir && self.selected < last {
                    self.selected += 1;
                }
            }
            KeyCode::Left => {
                let entry = &self.entries[self.selected];
                if entry.is_dir && entry.expanded {
                    self.toggle(self.selected);
                } else if entry.depth > 0 {
                    let depth = entry.depth;
                    if let Some(parent) = self.entries[..self.selected].iter().rposition(|e| e.depth < depth) {
                        self.selected = parent;
                    }
                }
            }
            _ => return None,
        }
        scroll_into_view(&mut self.top, self.selected, self.area.h);
        None
    }

    pub fn click(&mut self, y: i32) -> Option<PathBuf> {
        let index = self.top + (y - self.area.y).max(0) as usize;
        if index >= self.entries.len() {
            return None;
        }
        self.selected = index;
        self.activate(index)
    }

    pub fn scroll(&mut self, delta: i32) {
        self.top = (self.top as i32 + delta).max(0) as usize;
        clamp_top(&mut self.top, self.entries.len(), self.area.h);
    }

    /// Select the row at screen row `y` without opening it (right-click). None = empty space.
    pub fn select_at(&mut self, y: i32) -> Option<PathBuf> {
        let index = self.top + (y - self.area.y).max(0) as usize;
        let path = self.entries.get(index)?.path.clone();
        self.selected = index;
        Some(path)
    }

    /// Where a new file goes, like VS Code: the selected folder, or the folder of the selected file.
    pub fn target_dir(&self) -> PathBuf {
        match self.entries.get(self.selected) {
            Some(entry) if entry.is_dir => entry.path.clone(),
            Some(entry) => entry.path.parent().unwrap_or(&self.root).to_path_buf(),
            None => self.root.clone(),
        }
    }

    /// Re-read the tree, open the folders above `path` and select it.
    pub fn reveal(&mut self, path: &Path) {
        self.reload();
        let mut chain: Vec<&Path> = path.ancestors().skip(1).take_while(|p| p.starts_with(&self.root) && *p != self.root).collect();
        chain.reverse();
        for folder in chain {
            if let Some(index) = self.entries.iter().position(|e| e.path == folder) {
                if !self.entries[index].expanded {
                    self.toggle(index);
                }
            }
        }
        if let Some(index) = self.entries.iter().position(|e| e.path == path) {
            self.selected = index;
            scroll_into_view(&mut self.top, index, self.area.h);
        }
    }

    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, theme: &VSTheme, focused: bool, mouse: (i32, i32)) {
        self.area = area;
        clamp_top(&mut self.top, self.entries.len(), area.h);
        let fg = theme.c("sideBar.foreground");
        for (row, entry) in self.entries.iter().enumerate().skip(self.top).take(area.h.max(0) as usize) {
            let y = area.y + (row - self.top) as i32;
            let hovered = area.contains(mouse.0, mouse.1) && mouse.1 == y;
            let (row_fg, row_bg) = row_style(theme, row == self.selected, focused, hovered);
            if let Some(bg) = row_bg {
                buf.fill(Rect::new(area.x, y, area.w, 1), Style::new(fg, bg));
            }
            let text_fg = row_fg.unwrap_or(fg);
            let mut x = area.x + 1 + entry.depth as i32 * 2;
            if entry.is_dir {
                let glyph = icon(if entry.expanded { "chevron_down" } else { "chevron_right" });
                x = buf.print(x, y, glyph, text_fg, 0, area.right()) + 1;
            } else {
                let (glyph, colour) = file_icon(&entry.path, theme);
                x = buf.print(x, y, glyph, colour, 0, area.right()) + 1;
            }
            buf.print(x, y, &entry.name, text_fg, 0, area.right());
        }
    }
}

// -- Search ---------------------------------------------------------------------------

struct Hit {
    line: usize,
    text: String,
    /// Character range of the match within `text`.
    mark: Option<(usize, usize)>,
}

struct Group {
    name: String,
    hits: Vec<Hit>,
    expanded: bool,
}

#[derive(Clone, Copy)]
enum Row {
    File(usize),
    Hit(usize, usize),
}

/// Find in files, backed by ripgrep. Results are grouped by file, like VS Code.
pub struct Search {
    workspace: PathBuf,
    pub input: TextInput,
    pub summary: String,
    groups: Vec<Group>,
    selected: usize,
    top: usize,
    generation: u64,
    pub area: Rect,
}

impl Search {
    pub fn new(workspace: PathBuf) -> Search {
        Search {
            workspace,
            input: TextInput::default(),
            summary: "Type a term and press Enter.".to_string(),
            groups: Vec::new(),
            selected: 0,
            top: 0,
            generation: 0,
            area: Rect::default(),
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (g, group) in self.groups.iter().enumerate() {
            rows.push(Row::File(g));
            if group.expanded {
                rows.extend((0..group.hits.len()).map(|h| Row::Hit(g, h)));
            }
        }
        rows
    }

    pub fn has_results(&self) -> bool {
        !self.groups.is_empty()
    }

    /// Start a search in the background; the answer comes back as `Ev::Search`.
    pub fn run(&mut self, tx: &Sender<Ev>) {
        let query = self.input.value.clone();
        self.groups.clear();
        self.selected = 0;
        self.top = 0;
        self.generation += 1;
        if query.is_empty() {
            self.summary = "Type a term and press Enter.".to_string();
            return;
        }
        let Some(rg) = which("rg") else {
            self.summary = "Search needs ripgrep. Install `rg` and try again.".to_string();
            return;
        };
        self.summary = "Searching...".to_string();
        let (generation, workspace, tx) = (self.generation, self.workspace.clone(), tx.clone());
        thread::spawn(move || {
            let output = Command::new(rg)
                .args(["--line-number", "--no-heading", "--color=never", "--smart-case", "--fixed-strings", "--max-count=50", "--max-columns=300", "--"])
                .arg(&query)
                .arg(".")
                .current_dir(workspace)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output();
            let text = output.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            let _ = tx.send(Ev::Search { generation, query, output: text });
        });
    }

    pub fn finished(&mut self, generation: u64, query: &str, output: &str) {
        if generation != self.generation {
            return;
        }
        let folded = query.to_lowercase();
        let query_len = query.chars().count();
        let mut total = 0;
        for raw in output.lines().take(1000) {
            let Some((name, rest)) = raw.split_once(':') else { continue };
            let Some((number, text)) = rest.split_once(':') else { continue };
            let Ok(line) = number.parse::<usize>() else { continue };
            let name = name.strip_prefix("./").unwrap_or(name);
            let text = text.trim();
            // Keep the match in view on long lines.
            let hit = match text.to_lowercase().find(&folded) {
                Some(byte) => {
                    let start = text.get(..byte).map(|s| s.chars().count()).unwrap_or(0);
                    let cut = start.saturating_sub(12);
                    let shown: String = text.chars().skip(cut).collect();
                    let offset = start - cut + usize::from(cut > 0);
                    Hit { line, text: if cut > 0 { format!("…{shown}") } else { shown }, mark: Some((offset, offset + query_len)) }
                }
                None => Hit { line, text: text.to_string(), mark: None },
            };
            match self.groups.iter_mut().find(|group| group.name == name) {
                Some(group) => group.hits.push(hit),
                None => self.groups.push(Group { name: name.to_string(), hits: vec![hit], expanded: true }),
            }
            total += 1;
        }
        self.summary = if total > 0 {
            format!("{total} results in {} files", self.groups.len())
        } else {
            "No results found. Check the spelling, or search for fewer words.".to_string()
        };
    }

    fn activate(&mut self, index: usize) -> Option<(PathBuf, usize)> {
        match *self.rows().get(index)? {
            Row::File(g) => {
                self.groups[g].expanded = !self.groups[g].expanded;
                None
            }
            Row::Hit(g, h) => Some((self.workspace.join(&self.groups[g].name), self.groups[g].hits[h].line)),
        }
    }

    /// Keys in the result list. Returns a (file, line) to open.
    pub fn key(&mut self, key: &KeyEvent) -> Option<(PathBuf, usize)> {
        let count = self.rows().len();
        if count == 0 {
            return None;
        }
        let page = (self.area.h - 1).max(1) as usize;
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(count - 1),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(page),
            KeyCode::PageDown => self.selected = (self.selected + page).min(count - 1),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = count - 1,
            KeyCode::Enter | KeyCode::Char(' ') => return self.activate(self.selected),
            _ => return None,
        }
        scroll_into_view(&mut self.top, self.selected, self.area.h);
        None
    }

    pub fn click(&mut self, y: i32) -> Option<(PathBuf, usize)> {
        let index = self.top + (y - self.area.y).max(0) as usize;
        if index >= self.rows().len() {
            return None;
        }
        self.selected = index;
        self.activate(index)
    }

    pub fn scroll(&mut self, delta: i32) {
        self.top = (self.top as i32 + delta).max(0) as usize;
        let count = self.rows().len();
        clamp_top(&mut self.top, count, self.area.h);
    }

    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, theme: &VSTheme, focused: bool, mouse: (i32, i32)) {
        self.area = area;
        let rows = self.rows();
        clamp_top(&mut self.top, rows.len(), area.h);
        let fg = theme.c("sideBar.foreground");
        let dim = theme.c("descriptionForeground");
        let match_bg = theme.c("editor.findMatchHighlightBackground");
        for (index, row) in rows.iter().enumerate().skip(self.top).take(area.h.max(0) as usize) {
            let y = area.y + (index - self.top) as i32;
            let hovered = area.contains(mouse.0, mouse.1) && mouse.1 == y;
            let (row_fg, row_bg) = row_style(theme, index == self.selected, focused, hovered);
            if let Some(bg) = row_bg {
                buf.fill(Rect::new(area.x, y, area.w, 1), Style::new(fg, bg));
            }
            let text_fg = row_fg.unwrap_or(fg);
            match *row {
                Row::File(g) => {
                    let group = &self.groups[g];
                    let path = Path::new(&group.name);
                    let glyph = icon(if group.expanded { "chevron_down" } else { "chevron_right" });
                    let mut x = buf.print(area.x + 1, y, glyph, text_fg, 0, area.right()) + 1;
                    let (glyph, colour) = file_icon(path, theme);
                    x = buf.print(x, y, glyph, colour, 0, area.right()) + 1;
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    x = buf.print(x, y, &name, text_fg, 0, area.right()) + 2;
                    let folder = path.parent().map(|p| p.to_string_lossy().to_string()).filter(|p| !p.is_empty());
                    let tail = match folder {
                        Some(folder) => format!("{folder}  {}", group.hits.len()),
                        None => group.hits.len().to_string(),
                    };
                    buf.print(x, y, &tail, row_fg.unwrap_or(dim), 0, area.right());
                }
                Row::Hit(g, h) => {
                    let hit = &self.groups[g].hits[h];
                    let start = area.x + 5;
                    buf.print(start, y, &hit.text, text_fg, 0, area.right());
                    if let Some((from, to)) = hit.mark {
                        let before: String = hit.text.chars().take(from).collect();
                        let marked: String = hit.text.chars().skip(from).take(to - from).collect();
                        let x = start + width(&before);
                        let span = Rect::new(x, y, width(&marked).min(area.right() - x), 1);
                        buf.restyle(span, |style| style.bg = match_bg);
                    }
                }
            }
        }
    }
}

// -- Source Control -------------------------------------------------------------------

struct Change {
    path: PathBuf,
    shown: String,
    folder: String,
    code: char,
}

/// What `git status` came back with.
pub enum ScmOutcome {
    Status(String),
    NoGit,
    NotARepository,
}

/// Read-only list of changed files from `git status`.
pub struct Scm {
    workspace: PathBuf,
    pub summary: String,
    changes: Vec<Change>,
    selected: usize,
    top: usize,
    generation: u64,
    pub area: Rect,
}

impl Scm {
    pub fn new(workspace: PathBuf) -> Scm {
        Scm { workspace, summary: String::new(), changes: Vec::new(), selected: 0, top: 0, generation: 0, area: Rect::default() }
    }

    pub fn reload(&mut self, tx: &Sender<Ev>) {
        self.generation += 1;
        let (generation, workspace, tx) = (self.generation, self.workspace.clone(), tx.clone());
        thread::spawn(move || {
            let outcome = match Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(workspace)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
            {
                Err(_) => ScmOutcome::NoGit,
                Ok(output) if !output.status.success() => ScmOutcome::NotARepository,
                Ok(output) => ScmOutcome::Status(String::from_utf8_lossy(&output.stdout).to_string()),
            };
            let _ = tx.send(Ev::Scm { generation, outcome });
        });
    }

    pub fn finished(&mut self, generation: u64, outcome: ScmOutcome) {
        if generation != self.generation {
            return;
        }
        self.changes.clear();
        self.selected = 0;
        self.top = 0;
        let status = match outcome {
            ScmOutcome::NoGit => {
                self.summary = "Source control needs git. Install it and reopen this view.".to_string();
                return;
            }
            ScmOutcome::NotARepository => {
                self.summary = "This folder is not a Git repository yet. Run `git init` in the terminal to start one.".to_string();
                return;
            }
            ScmOutcome::Status(status) => status,
        };
        for raw in status.lines() {
            if raw.len() < 4 {
                continue;
            }
            let code = raw[..2].trim().chars().next().unwrap_or('?');
            let name = raw.get(3..).unwrap_or("").rsplit(" -> ").next().unwrap_or("").trim_matches('"');
            let path = Path::new(name);
            let shown = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(name.to_string());
            let folder = path.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            self.changes.push(Change { path: self.workspace.join(name), shown, folder, code });
        }
        self.summary = if self.changes.is_empty() {
            "No changes. Everything is committed.".to_string()
        } else {
            format!("Changes  {}", self.changes.len())
        };
    }

    fn activate(&self, index: usize) -> Option<PathBuf> {
        self.changes.get(index).map(|change| change.path.clone()).filter(|path| path.is_file())
    }

    pub fn key(&mut self, key: &KeyEvent) -> Option<PathBuf> {
        if self.changes.is_empty() {
            return None;
        }
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(self.changes.len() - 1),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.changes.len() - 1,
            KeyCode::Enter | KeyCode::Char(' ') => return self.activate(self.selected),
            _ => return None,
        }
        scroll_into_view(&mut self.top, self.selected, self.area.h);
        None
    }

    pub fn click(&mut self, y: i32) -> Option<PathBuf> {
        let index = self.top + (y - self.area.y).max(0) as usize;
        if index >= self.changes.len() {
            return None;
        }
        self.selected = index;
        self.activate(index)
    }

    pub fn scroll(&mut self, delta: i32) {
        self.top = (self.top as i32 + delta).max(0) as usize;
        clamp_top(&mut self.top, self.changes.len(), self.area.h);
    }

    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, theme: &VSTheme, focused: bool, mouse: (i32, i32)) {
        self.area = area;
        clamp_top(&mut self.top, self.changes.len(), area.h);
        let fg = theme.c("sideBar.foreground");
        for (index, change) in self.changes.iter().enumerate().skip(self.top).take(area.h.max(0) as usize) {
            let y = area.y + (index - self.top) as i32;
            let hovered = area.contains(mouse.0, mouse.1) && mouse.1 == y;
            let (row_fg, row_bg) = row_style(theme, index == self.selected, focused, hovered);
            if let Some(bg) = row_bg {
                buf.fill(Rect::new(area.x, y, area.w, 1), Style::new(fg, bg));
            }
            let kind = match change.code {
                'A' => "added",
                'D' | 'U' => "deleted",
                '?' | 'R' => "untracked",
                _ => "modified",
            };
            let colour = theme.c(&format!("gitDecoration.{kind}ResourceForeground"));
            let (glyph, icon_colour) = file_icon(&change.path, theme);
            let mut x = buf.print(area.x + 1, y, glyph, icon_colour, 0, area.right()) + 1;
            let letter = if change.code == '?' { 'U' } else { change.code }.to_string();
            let room = area.right() - 2 - x - 2;
            x = buf.print(x, y, &ellipsize(&change.shown, room.max(4)), row_fg.unwrap_or(colour), 0, area.right() - 2) + 2;
            if !change.folder.is_empty() {
                buf.print(x, y, &change.folder, row_fg.unwrap_or(theme.c("descriptionForeground")), 0, area.right() - 3);
            }
            buf.print(area.right() - 2, y, &letter, row_fg.unwrap_or(colour), 0, area.right());
        }
    }
}
