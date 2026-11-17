//! Popups: dropdown menus, confirmation dialogs, the quick-input box, the quick pick
//! (theme picker) and the command palette / Quick Open.

use crate::app::Action;
use crate::screen::{width, Buffer, Rect, Rgb, Style, BOLD, DIM, UNDERLINE};
use crate::sidebar::TextInput;
use crate::theme::VSTheme;
use crate::util::fuzzy;
use std::path::PathBuf;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Word-wrap text to a width, keeping its own line breaks.
pub fn wrap_text(text: &str, max: i32) -> Vec<String> {
    let max = max.max(4);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let extra = if line.is_empty() { 0 } else { 1 };
            if width(&line) + extra + width(word) > max && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
            while width(&line) > max {
                let head: String = line.chars().take(max as usize).collect();
                line = line.chars().skip(max as usize).collect();
                lines.push(head);
            }
        }
        lines.push(line);
    }
    lines
}

/// A box of the given width centred horizontally (never wider than 90% of the window).
fn centred(screen_w: i32, wanted: i32) -> (i32, i32) {
    let w = wanted.min(screen_w * 9 / 10).max(10).min(screen_w);
    ((screen_w - w) / 2, w)
}

fn list_colours(theme: &VSTheme, selected: bool, hovered: bool) -> (Rgb, Option<Rgb>) {
    if selected {
        (theme.c("list.activeSelectionForeground"), Some(theme.c("list.activeSelectionBackground")))
    } else if hovered {
        (theme.c("menu.foreground"), Some(theme.c("list.hoverBackground")))
    } else {
        (theme.c("menu.foreground"), None)
    }
}

// -- dropdown menu ----------------------------------------------------------------------

pub type MenuItem = Option<(&'static str, &'static str, Action)>;

pub enum MenuOutcome {
    Nothing,
    Close,
    Run(Action),
    /// Left/Right: move to the neighbouring title-bar menu.
    Switch(i32),
}

/// A dropdown menu. Items are (label, shortcut, action) or None for a separator.
pub struct Menu {
    items: Vec<MenuItem>,
    x: i32,
    y: i32,
    selected: Option<usize>,
    /// Index of the title-bar menu this belongs to, if any.
    pub owner: Option<usize>,
    rect: Rect,
}

impl Menu {
    /// `anchor_bottom` places the menu so it ends just above row `y` (menus that open upward).
    pub fn new(items: Vec<MenuItem>, x: i32, y: i32, anchor_bottom: bool, owner: Option<usize>) -> Menu {
        let y = if anchor_bottom { y - items.len() as i32 - 2 } else { y };
        let selected = items.iter().position(Option::is_some);
        Menu { items, x: x.max(0), y: y.max(0), selected, owner, rect: Rect::default() }
    }

    fn step(&mut self, delta: i32) {
        let count = self.items.len() as i32;
        let mut index = self.selected.map(|i| i as i32).unwrap_or(if delta > 0 { -1 } else { count });
        for _ in 0..count {
            index = (index + delta).rem_euclid(count);
            if self.items[index as usize].is_some() {
                self.selected = Some(index as usize);
                return;
            }
        }
    }

    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let inside = x > self.rect.x && x < self.rect.right() - 1 && y > self.rect.y && y < self.rect.bottom() - 1;
        let index = (y - self.rect.y - 1) as usize;
        (inside && self.items.get(index).map_or(false, Option::is_some)).then_some(index)
    }

    pub fn key(&mut self, key: &KeyEvent) -> MenuOutcome {
        match key.code {
            KeyCode::Esc => return MenuOutcome::Close,
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::Left => return MenuOutcome::Switch(-1),
            KeyCode::Right => return MenuOutcome::Switch(1),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some((_, _, action)) = self.selected.and_then(|i| self.items[i].clone()) {
                    return MenuOutcome::Run(action);
                }
            }
            _ => {}
        }
        MenuOutcome::Nothing
    }

    pub fn hover(&mut self, x: i32, y: i32) {
        if let Some(row) = self.row_at(x, y) {
            self.selected = Some(row);
        }
    }

    pub fn click(&mut self, x: i32, y: i32) -> MenuOutcome {
        match self.row_at(x, y).and_then(|row| self.items[row].clone()) {
            Some((_, _, action)) => MenuOutcome::Run(action),
            None if self.rect.contains(x, y) => MenuOutcome::Nothing,
            None => MenuOutcome::Close,
        }
    }

    pub fn draw(&mut self, buf: &mut Buffer, theme: &VSTheme) {
        let rows = self.items.iter().flatten();
        let left = rows.clone().map(|item| width(item.0)).max().unwrap_or(0);
        let right = rows.map(|item| width(item.1)).max().unwrap_or(0);
        let (w, h) = (left + right + 8, self.items.len() as i32 + 2);
        let x = self.x.min(buf.w - w).max(0);
        let y = self.y.min(buf.h - h).max(0);
        self.rect = Rect::new(x, y, w, h);
        let (fg, bg) = (theme.c("menu.foreground"), theme.c("menu.background"));
        buf.fill(self.rect, Style::new(fg, bg));
        buf.round_box(self.rect, theme.c("menu.border"));
        for (index, item) in self.items.iter().enumerate() {
            let row_y = y + 1 + index as i32;
            let Some((label, shortcut, _)) = item else {
                buf.print(x + 1, row_y, &"─".repeat((w - 2) as usize), theme.c("border"), 0, x + w - 1);
                continue;
            };
            let mut row_fg = fg;
            if self.selected == Some(index) {
                row_fg = theme.c("menu.selectionForeground");
                buf.fill(Rect::new(x + 1, row_y, w - 2, 1), Style::new(row_fg, theme.c("menu.selectionBackground")));
            }
            buf.print(x + 2, row_y, label, row_fg, 0, x + w - 1);
            buf.print(x + w - 2 - width(shortcut), row_y, shortcut, row_fg, DIM, x + w - 1);
        }
    }
}

// -- confirmation dialog ----------------------------------------------------------------

/// What a dialog's answer is for.
#[derive(Clone, Copy)]
pub enum ChoiceThen {
    CloseDoc(u64),
    Quit,
    /// Delete the path the context menu was opened on.
    Delete,
}

/// VS Code style confirmation dialog. The answer is the label of the chosen button.
pub struct Choice {
    message: String,
    detail: String,
    buttons: Vec<&'static str>,
    focus: usize,
    pub then: ChoiceThen,
    rects: Vec<Rect>,
}

impl Choice {
    pub fn new(message: String, detail: String, buttons: Vec<&'static str>, then: ChoiceThen) -> Choice {
        Choice { message, detail, buttons, focus: 0, then, rects: Vec::new() }
    }

    /// Some(None) = cancelled, Some(Some(label)) = that button.
    pub fn key(&mut self, key: &KeyEvent) -> Option<Option<&'static str>> {
        let count = self.buttons.len();
        match key.code {
            KeyCode::Esc => return Some(None),
            KeyCode::Enter | KeyCode::Char(' ') => return Some(Some(self.buttons[self.focus])),
            KeyCode::Right | KeyCode::Tab => self.focus = (self.focus + 1) % count,
            KeyCode::Left | KeyCode::BackTab => self.focus = (self.focus + count - 1) % count,
            _ => {}
        }
        None
    }

    pub fn click(&mut self, x: i32, y: i32) -> Option<Option<&'static str>> {
        let index = self.rects.iter().position(|rect| rect.contains(x, y))?;
        Some(Some(self.buttons[index]))
    }

    pub fn draw(&mut self, buf: &mut Buffer, theme: &VSTheme, mouse: (i32, i32)) {
        // Dim everything behind the dialog.
        let veil = theme.c("editor.background");
        buf.restyle(Rect::new(0, 0, buf.w, buf.h), |style| {
            style.fg = style.fg.mix(veil, 0.55);
            style.bg = style.bg.mix(veil, 0.55);
        });
        let (x, w) = centred(buf.w, 68);
        let inner = w - 8;
        let message = wrap_text(&self.message, inner);
        let detail = if self.detail.is_empty() { Vec::new() } else { wrap_text(&self.detail, inner) };
        let h = 2 + 2 + message.len() as i32 + if detail.is_empty() { 0 } else { 1 + detail.len() as i32 } + 2 + 1;
        let y = ((buf.h - h) / 2).max(0);
        let rect = Rect::new(x, y, w, h);
        let bg = theme.c("menu.background");
        buf.fill(rect, Style::new(theme.c("foreground"), bg));
        buf.round_box(rect, theme.c("menu.border"));
        let mut row = y + 2;
        for line in &message {
            buf.print(x + 4, row, line, theme.c("foreground"), BOLD, x + w - 4);
            row += 1;
        }
        if !detail.is_empty() {
            row += 1;
            for line in &detail {
                buf.print(x + 4, row, line, theme.c("descriptionForeground"), 0, x + w - 4);
                row += 1;
            }
        }
        row += 2;
        // Buttons sit on the right, the default one first.
        let widths: Vec<i32> = self.buttons.iter().map(|label| (width(label) + 4).max(14)).collect();
        let mut bx = x + w - 4 - widths.iter().sum::<i32>() - 2 * (widths.len() as i32 - 1);
        self.rects.clear();
        for (index, label) in self.buttons.iter().enumerate() {
            let rect = Rect::new(bx, row, widths[index], 1);
            let hovered = rect.contains(mouse.0, mouse.1);
            let (fg, bg) = if index == 0 {
                (theme.c("button.foreground"), theme.c("button.background"))
            } else if hovered {
                (theme.c("foreground"), theme.c("button.secondaryHoverBackground"))
            } else {
                (theme.c("foreground"), theme.c("button.secondaryBackground"))
            };
            buf.fill(rect, Style::new(fg, bg));
            let attr = if index == self.focus { UNDERLINE } else { 0 };
            buf.print(bx + (widths[index] - width(label)) / 2, row, label, fg, attr, rect.right());
            self.rects.push(rect);
            bx += widths[index] + 2;
        }
    }
}

// -- quick input (Save As) --------------------------------------------------------------

/// What happens after a successful save.
#[derive(Clone, Copy, PartialEq)]
pub enum After {
    Nothing,
    Close,
    Run,
    /// Carry on with Save All (the next untitled file).
    SaveAll,
}

/// A quick-input box at the top of the window (like VS Code's Save As prompt).
/// What the typed text is for.
#[derive(Clone, PartialEq)]
pub enum InputKind {
    SaveAs,
    /// Create a file / folder inside this folder.
    NewFile(PathBuf),
    NewFolder(PathBuf),
    Rename(PathBuf),
}

pub struct InputBox {
    pub kind: InputKind,
    prompt: String,
    pub input: TextInput,
    placeholder: &'static str,
    /// The document being saved and what to do afterwards.
    pub doc: u64,
    pub after: After,
    rect: Rect,
}

impl InputBox {
    pub fn new(prompt: &str, value: &str, placeholder: &'static str, doc: u64, after: After) -> InputBox {
        InputBox { kind: InputKind::SaveAs, prompt: prompt.to_string(), input: TextInput::new(value), placeholder, doc, after, rect: Rect::default() }
    }

    pub fn kind(mut self, kind: InputKind) -> InputBox {
        self.kind = kind;
        self
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.rect.contains(x, y)
    }

    pub fn draw(&mut self, buf: &mut Buffer, theme: &VSTheme) -> (i32, i32) {
        let (x, w) = centred(buf.w, 72);
        self.rect = Rect::new(x, 1, w, 4);
        buf.fill(self.rect, Style::new(theme.c("foreground"), theme.c("menu.background")));
        buf.round_box(self.rect, theme.c("menu.border"));
        let field = Rect::new(x + 2, 2, w - 4, 1);
        let cursor = self.input.draw(buf, field, theme, theme.c("input.background"), self.placeholder);
        buf.print(x + 2, 3, &self.prompt, theme.c("descriptionForeground"), 0, x + w - 2);
        cursor
    }
}

// -- quick pick (theme picker) ----------------------------------------------------------

pub enum PickOutcome {
    Nothing,
    /// The highlighted row changed (for live previews).
    Moved,
    Chosen,
    Cancelled,
}

/// VS Code's quick pick: a list at the top of the window.
pub struct QuickPick {
    title: String,
    pub choices: Vec<(String, String)>,
    pub selected: usize,
    /// The id that was current when the picker opened (restored on cancel).
    pub original: String,
    top: usize,
    rect: Rect,
}

impl QuickPick {
    pub fn new(title: &str, choices: Vec<(String, String)>, current: &str) -> QuickPick {
        let selected = choices.iter().position(|(id, _)| id == current).unwrap_or(0);
        QuickPick { title: title.to_string(), choices, selected, original: current.to_string(), top: 0, rect: Rect::default() }
    }

    pub fn current(&self) -> Option<&str> {
        self.choices.get(self.selected).map(|(id, _)| id.as_str())
    }

    pub fn key(&mut self, key: &KeyEvent) -> PickOutcome {
        let count = self.choices.len();
        let before = self.selected;
        match key.code {
            KeyCode::Esc => return PickOutcome::Cancelled,
            KeyCode::Enter => return PickOutcome::Chosen,
            KeyCode::Up => self.selected = (self.selected + count - 1) % count.max(1),
            KeyCode::Down => self.selected = (self.selected + 1) % count.max(1),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = count.saturating_sub(1),
            _ => {}
        }
        if self.selected != before {
            PickOutcome::Moved
        } else {
            PickOutcome::Nothing
        }
    }

    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let first = self.rect.y + 3;
        let inside = x > self.rect.x && x < self.rect.right() - 1 && y >= first && y < self.rect.bottom() - 1;
        let index = self.top + (y - first).max(0) as usize;
        (inside && index < self.choices.len()).then_some(index)
    }

    pub fn click(&mut self, x: i32, y: i32) -> PickOutcome {
        match self.row_at(x, y) {
            Some(row) => {
                self.selected = row;
                PickOutcome::Chosen
            }
            None if self.rect.contains(x, y) => PickOutcome::Nothing,
            None => PickOutcome::Cancelled,
        }
    }

    pub fn draw(&mut self, buf: &mut Buffer, theme: &VSTheme, mouse: (i32, i32)) {
        let (x, w) = centred(buf.w, 72);
        let visible = (self.choices.len() as i32).min(16).min(buf.h - 6).max(1);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + visible as usize {
            self.top = self.selected + 1 - visible as usize;
        }
        self.rect = Rect::new(x, 1, w, visible + 4);
        buf.fill(self.rect, Style::new(theme.c("menu.foreground"), theme.c("menu.background")));
        buf.round_box(self.rect, theme.c("menu.border"));
        buf.print(x + 2, 2, &self.title, theme.c("descriptionForeground"), 0, x + w - 2);
        for (index, (_, label)) in self.choices.iter().enumerate().skip(self.top).take(visible as usize) {
            let y = 4 + (index - self.top) as i32;
            let row = Rect::new(x + 2, y, w - 4, 1);
            let (fg, bg) = list_colours(theme, index == self.selected, row.contains(mouse.0, mouse.1));
            if let Some(bg) = bg {
                buf.fill(row, Style::new(fg, bg));
            }
            buf.print(x + 3, y, label, fg, 0, x + w - 2);
        }
    }
}

// -- command palette / Quick Open -------------------------------------------------------

#[derive(Clone)]
pub enum Target {
    Action(Action),
    /// A path relative to the workspace.
    File(String),
}

pub enum PaletteOutcome {
    Nothing,
    Close,
    Run(Target),
}

/// The fuzzy-search box used for both Show All Commands (F1) and Go to File (Ctrl+P).
pub struct Palette {
    pub files: bool,
    input: TextInput,
    placeholder: &'static str,
    candidates: Vec<(String, Target)>,
    /// (index into candidates, matched character positions)
    results: Vec<(usize, Vec<usize>)>,
    selected: usize,
    top: usize,
    rect: Rect,
    list: Rect,
}

impl Palette {
    pub fn new(files: bool, placeholder: &'static str, candidates: Vec<(String, Target)>) -> Palette {
        let mut palette = Palette {
            files,
            input: TextInput::default(),
            placeholder,
            candidates,
            results: Vec::new(),
            selected: 0,
            top: 0,
            rect: Rect::default(),
            list: Rect::default(),
        };
        palette.refilter();
        palette
    }

    /// The file list arrived (or was refreshed) while the palette is open.
    pub fn set_candidates(&mut self, candidates: Vec<(String, Target)>) {
        self.candidates = candidates;
        self.refilter();
    }

    fn refilter(&mut self) {
        let query = self.input.value.trim();
        self.selected = 0;
        self.top = 0;
        if query.is_empty() {
            let shown = if self.files { 30 } else { self.candidates.len() };
            self.results = (0..self.candidates.len().min(shown)).map(|i| (i, Vec::new())).collect();
            return;
        }
        let mut scored: Vec<(i32, usize, Vec<usize>)> = Vec::new();
        for (index, (label, _)) in self.candidates.iter().enumerate() {
            let Some((mut score, positions)) = fuzzy(query, label) else { continue };
            if self.files {
                // Prefer matches in the file name over matches in the folder path.
                let name = label.rsplit('/').next().unwrap_or(label);
                if let Some((name_score, _)) = fuzzy(query, name) {
                    score += 40 + name_score;
                }
            }
            scored.push((score, index, positions));
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(200);
        self.results = scored.into_iter().map(|(_, index, positions)| (index, positions)).collect();
    }

    pub fn paste(&mut self, text: &str) {
        self.input.insert(text);
        self.refilter();
    }

    fn chosen(&self) -> PaletteOutcome {
        match self.results.get(self.selected) {
            Some((index, _)) => PaletteOutcome::Run(self.candidates[*index].1.clone()),
            None => PaletteOutcome::Nothing,
        }
    }

    pub fn key(&mut self, key: &KeyEvent) -> PaletteOutcome {
        let count = self.results.len();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return PaletteOutcome::Close,
            KeyCode::Enter => return self.chosen(),
            KeyCode::Up => self.selected = if count == 0 { 0 } else { (self.selected + count - 1) % count },
            KeyCode::Down => self.selected = if count == 0 { 0 } else { (self.selected + 1) % count },
            KeyCode::Char('p') if ctrl => self.selected = if count == 0 { 0 } else { (self.selected + count - 1) % count },
            KeyCode::Char('n') if ctrl => self.selected = if count == 0 { 0 } else { (self.selected + 1) % count },
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(count.saturating_sub(1)),
            _ => {
                let before = self.input.value.clone();
                if self.input.key(key) && self.input.value != before {
                    self.refilter();
                }
            }
        }
        PaletteOutcome::Nothing
    }

    pub fn click(&mut self, x: i32, y: i32) -> PaletteOutcome {
        if self.list.contains(x, y) {
            let index = self.top + (y - self.list.y) as usize;
            if index < self.results.len() {
                self.selected = index;
                return self.chosen();
            }
            return PaletteOutcome::Nothing;
        }
        if self.rect.contains(x, y) {
            PaletteOutcome::Nothing
        } else {
            PaletteOutcome::Close
        }
    }

    pub fn scroll(&mut self, delta: i32) {
        let count = self.results.len() as i32;
        self.selected = (self.selected as i32 + delta).clamp(0, (count - 1).max(0)) as usize;
    }

    pub fn draw(&mut self, buf: &mut Buffer, theme: &VSTheme, mouse: (i32, i32)) -> (i32, i32) {
        let (x, w) = centred(buf.w, 72);
        let visible = (self.results.len() as i32).min(16).min(buf.h - 6).max(0);
        if self.selected < self.top {
            self.top = self.selected;
        } else if visible > 0 && self.selected >= self.top + visible as usize {
            self.top = self.selected + 1 - visible as usize;
        }
        let h = 3 + if visible > 0 { visible + 1 } else { 0 };
        self.rect = Rect::new(x, 1, w, h);
        let bg = theme.c("menu.background");
        buf.fill(self.rect, Style::new(theme.c("menu.foreground"), bg));
        buf.round_box(Rect::new(x, 1, w, 3), theme.c("menu.border"));
        let cursor = self.input.draw(buf, Rect::new(x + 1, 2, w - 2, 1), theme, bg, self.placeholder);
        self.list = Rect::new(x + 1, 4, w - 2, visible);
        let highlight = theme.c("list.highlightForeground");
        for (row, (index, positions)) in self.results.iter().enumerate().skip(self.top).take(visible as usize) {
            let y = 4 + (row - self.top) as i32;
            let line = Rect::new(x + 1, y, w - 2, 1);
            let selected = row == self.selected;
            let (fg, row_bg) = list_colours(theme, selected, line.contains(mouse.0, mouse.1));
            if let Some(row_bg) = row_bg {
                buf.fill(line, Style::new(fg, row_bg));
            }
            let mut cx = x + 2;
            for (i, c) in self.candidates[*index].0.chars().enumerate() {
                let matched = positions.contains(&i);
                let (colour, attr) = if matched { (if selected { fg } else { highlight }, BOLD) } else { (fg, 0) };
                cx = buf.print(cx, y, &c.to_string(), colour, attr, x + w - 2);
            }
        }
        cursor
    }
}

pub enum Modal {
    Menu(Menu),
    Choice(Choice),
    Input(InputBox),
    Pick(QuickPick),
    Palette(Palette),
}
