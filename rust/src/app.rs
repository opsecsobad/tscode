//! The application shell: state, key bindings, mouse routing and every action.
//!
//! Everything the user can do is an `Action`. Menus, palette commands, key bindings
//! and clickable icons all name one, and `App::run` carries it out.

use crate::completion::{self, Suggest};
use crate::editor::{Doc, EdCmd, Pos, MAX_FILE_BYTES, SCROLLBAR_W};
use crate::highlight::{spawn_checker, spawn_highlighter, DiagRequest, DiagResult, HlRequest, HlResult};
use crate::popups::{After, Choice, ChoiceThen, InputBox, InputKind, Menu, MenuItem, MenuOutcome, Modal, Palette, PaletteOutcome, PickOutcome, QuickPick, Target};
use crate::screen::Rect;
use crate::settings::{cursor_is_line, home, load_settings, save_setting};
use crate::sidebar::{Explorer, Scm, ScmOutcome, Search};
use crate::term::Term;
use crate::theme::{available_themes, load_theme, terminal_theme_stamp, VSTheme};
use crate::util::{shell_quote, shell_split, which};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

/// Everything that can wake the UI thread.
pub enum Ev {
    Input(Event),
    Pty { term: usize, generation: u64, data: Vec<u8> },
    PtyExit { term: usize, generation: u64 },
    Highlight(HlResult),
    Diagnostics(DiagResult),
    Search { generation: u64, query: String, output: String },
    Scm { generation: u64, outcome: ScmOutcome },
    Files(Vec<String>),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    Explorer,
    Search,
    Scm,
    Debug,
    Extensions,
}

/// Which part of the window receives key presses.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Focus {
    Editor,
    Shell,
    Claude,
    Explorer,
    SearchInput,
    SearchResults,
    Scm,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Quit,
    Save,
    SaveAs,
    SaveAll,
    CloseEditor,
    QuickOpen,
    NewFile,
    ToggleSidebar,
    TogglePanel,
    ToggleClaude,
    CommandPalette,
    ShowView(View),
    SelectTheme,
    FocusTerminal,
    RestartTerminal,
    FocusClaude,
    RestartClaude,
    FocusNextPart,
    FocusPreviousPart,
    NextEditor,
    PreviousEditor,
    Editor(EdCmd),
    /// Open the suggest widget at the cursor (Ctrl+Space).
    TriggerSuggest,
    RunFile,
    About,
    RefreshExplorer,
    CollapseExplorer,
    RefreshScm,
    // Reached only by clicking.
    OpenMenu(usize),
    Activity(&'static str),
    PanelTab(usize),
    ActivateDoc(u64),
    CloseDoc(u64),
    CloseOthers(u64),
    CloseAll,
    // Explorer file operations; they act on the right-clicked row, or the selection.
    NewFileIn,
    NewFolder,
    Rename,
    Delete,
    CopyPath(bool),
    // The agent panel.
    AgentMenu,
    StartAgent(usize),
    StopAgent,
}

/// Coding agents the right-hand panel can run: (name, command). Only installed ones are offered.
pub const AGENTS: &[(&str, &str)] = &[
    ("Claude Code", "claude"),
    ("Codex", "codex"),
    ("Grok", "grok"),
    ("Antigravity", "agy"),
    ("Gemini", "gemini"),
    ("OpenCode", "opencode"),
    ("Cursor Agent", "cursor-agent"),
];

/// The command line for an agent, if it is installed. TSCODE_CLAUDE_CMD overrides Claude's.
fn agent_command(index: usize) -> Option<Vec<String>> {
    if index == 0 {
        if let Ok(command) = std::env::var("TSCODE_CLAUDE_CMD") {
            if !command.trim().is_empty() {
                return Some(shell_split(&command));
            }
        }
    }
    let command = AGENTS.get(index)?.1;
    which(command).map(|_| vec![command.to_string()])
}

/// Keys a focused terminal needs for itself (readline, Claude Code, vim...). The
/// matching app shortcuts are switched off while a terminal has focus.
fn terminal_owned(action: Action) -> bool {
    matches!(
        action,
        Action::Save
            | Action::CloseEditor
            | Action::QuickOpen
            | Action::ToggleSidebar
            | Action::TogglePanel
            | Action::NewFile
            | Action::TriggerSuggest
    )
}

/// The app's keyboard shortcuts.
fn binding(key: &KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    Some(match key.code {
        KeyCode::F(1) => Action::CommandPalette,
        KeyCode::F(6) if shift => Action::FocusPreviousPart,
        KeyCode::F(6) => Action::FocusNextPart,
        KeyCode::PageDown if ctrl => Action::NextEditor,
        KeyCode::PageUp if ctrl => Action::PreviousEditor,
        // Ctrl+Space arrives as NUL in terminals without the kitty keyboard protocol.
        KeyCode::Null => Action::TriggerSuggest,
        KeyCode::Char(c) if ctrl => match (c.to_ascii_lowercase(), shift || c.is_ascii_uppercase(), alt) {
            ('q', _, false) => Action::Quit,
            ('b', false, true) => Action::ToggleClaude,
            ('s', false, true) => Action::SaveAll,
            ('p', true, false) => Action::CommandPalette,
            ('e', true, false) => Action::ShowView(View::Explorer),
            ('`', _, false) => Action::FocusTerminal,
            (' ', _, false) => Action::TriggerSuggest,
            ('s', false, false) => Action::Save,
            ('w', false, false) => Action::CloseEditor,
            ('p', false, false) => Action::QuickOpen,
            ('n', false, false) => Action::NewFile,
            ('b', false, false) => Action::ToggleSidebar,
            ('j', false, false) => Action::TogglePanel,
            _ => return None,
        },
        _ => return None,
    })
}

const fn item(label: &'static str, shortcut: &'static str, action: Action) -> MenuItem {
    Some((label, shortcut, action))
}

/// The menu bar: label, shortcut, action. None = separator. Only things that really work are listed.
pub const MENUS: &[(&str, &[MenuItem])] = &[
    ("File", &[
        item("New Text File", "Ctrl+N", Action::NewFile),
        item("Open File...", "Ctrl+P", Action::QuickOpen),
        None,
        item("Save", "Ctrl+S", Action::Save),
        item("Save As...", "", Action::SaveAs),
        item("Save All", "Ctrl+Alt+S", Action::SaveAll),
        None,
        item("Color Theme...", "", Action::SelectTheme),
        None,
        item("Close Editor", "Ctrl+W", Action::CloseEditor),
        None,
        item("Exit", "Ctrl+Q", Action::Quit),
    ]),
    ("Edit", &[
        item("Undo", "Ctrl+Z", Action::Editor(EdCmd::Undo)),
        item("Redo", "Ctrl+Y", Action::Editor(EdCmd::Redo)),
        None,
        item("Cut", "Ctrl+X", Action::Editor(EdCmd::Cut)),
        item("Copy", "Ctrl+C", Action::Editor(EdCmd::Copy)),
        item("Paste", "Ctrl+V", Action::Editor(EdCmd::Paste)),
    ]),
    ("Selection", &[
        item("Select All", "Ctrl+A", Action::Editor(EdCmd::SelectAll)),
        item("Select Line", "Ctrl+L", Action::Editor(EdCmd::SelectLine)),
    ]),
    ("View", &[
        item("Command Palette...", "F1", Action::CommandPalette),
        None,
        item("Explorer", "Ctrl+Shift+E", Action::ShowView(View::Explorer)),
        item("Search", "", Action::ShowView(View::Search)),
        item("Source Control", "", Action::ShowView(View::Scm)),
        None,
        item("Toggle Primary Side Bar", "Ctrl+B", Action::ToggleSidebar),
        item("Toggle Panel", "Ctrl+J", Action::TogglePanel),
        item("Toggle Agent Panel", "Ctrl+Alt+B", Action::ToggleClaude),
    ]),
    ("Go", &[
        item("Go to File...", "Ctrl+P", Action::QuickOpen),
        None,
        item("Next Editor", "Ctrl+PgDn", Action::NextEditor),
        item("Previous Editor", "Ctrl+PgUp", Action::PreviousEditor),
        item("Focus Next Part", "F6", Action::FocusNextPart),
    ]),
    ("Run", &[item("Run Active File in Terminal", "", Action::RunFile)]),
    ("Terminal", &[
        item("Focus Terminal", "Ctrl+`", Action::FocusTerminal),
        item("Restart Terminal", "", Action::RestartTerminal),
        None,
        item("Start Agent...", "", Action::AgentMenu),
        item("Focus Agent", "", Action::FocusClaude),
        item("Restart Agent", "", Action::RestartClaude),
        item("Stop Agent", "", Action::StopAgent),
    ]),
    ("Help", &[item("Show All Commands", "F1", Action::CommandPalette), item("About", "", Action::About)]),
];

/// Show All Commands: (title, action).
const COMMANDS: &[(&str, Action)] = &[
    ("File: New Text File", Action::NewFile),
    ("File: New File...", Action::NewFileIn),
    ("File: New Folder...", Action::NewFolder),
    ("File: Save", Action::Save),
    ("File: Save As...", Action::SaveAs),
    ("File: Save All", Action::SaveAll),
    ("View: Close Editor", Action::CloseEditor),
    ("Go to File...", Action::QuickOpen),
    ("Trigger Suggest", Action::TriggerSuggest),
    ("View: Show Explorer", Action::ShowView(View::Explorer)),
    ("View: Show Search", Action::ShowView(View::Search)),
    ("View: Show Source Control", Action::ShowView(View::Scm)),
    ("View: Toggle Primary Side Bar Visibility", Action::ToggleSidebar),
    ("View: Toggle Panel Visibility", Action::TogglePanel),
    ("View: Toggle Agent Panel", Action::ToggleClaude),
    ("View: Focus Next Part", Action::FocusNextPart),
    ("View: Open Next Editor", Action::NextEditor),
    ("View: Open Previous Editor", Action::PreviousEditor),
    ("Terminal: Focus Terminal", Action::FocusTerminal),
    ("Terminal: Kill and Restart Terminal", Action::RestartTerminal),
    ("Terminal: Run Active File in Terminal", Action::RunFile),
    ("Agent: Start...", Action::AgentMenu),
    ("Agent: Focus", Action::FocusClaude),
    ("Agent: New Conversation", Action::RestartClaude),
    ("Agent: Stop", Action::StopAgent),
    ("Preferences: Color Theme", Action::SelectTheme),
    ("Explorer: Refresh", Action::RefreshExplorer),
    ("Help: About", Action::About),
    ("File: Exit", Action::Quit),
];

const RUNNERS: &[(&str, &str)] = &[("py", "python3"), ("sh", "bash"), ("bash", "bash"), ("js", "node"), ("mjs", "node")];

pub const PANEL_TABS: &[(&str, Option<&str>)] = &[
    ("PROBLEMS", Some("No problems have been detected in the workspace.")),
    ("OUTPUT", Some("Nothing has written output yet.")),
    ("DEBUG CONSOLE", Some("Debugging isn't built yet. Run the open file from Run > Run Active File in Terminal.")),
    ("TERMINAL", None),
    ("PORTS", Some("No forwarded ports.")),
];
pub const TERMINAL_TAB: usize = 3;

const SKIP_DIRS: &[&str] = &[
    ".git", "node_modules", ".venv", "venv", "__pycache__", ".mypy_cache", ".pytest_cache", ".ruff_cache", "target", "dist",
    "build", ".next",
];
const MAX_FILES: usize = 20_000;

/// Every file in the workspace (relative paths), for Quick Open.
fn walk_files(root: &Path, folder: &Path, found: &mut Vec<String>) {
    let Ok(listing) = fs::read_dir(folder) else { return };
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    dirs.push(name);
                }
            }
            Ok(_) => files.push(name),
            Err(_) => {}
        }
    }
    files.sort();
    dirs.sort();
    for name in files {
        if found.len() >= MAX_FILES {
            return;
        }
        let path = folder.join(name);
        found.push(path.strip_prefix(root).unwrap_or(&path).to_string_lossy().to_string());
    }
    for name in dirs {
        if found.len() >= MAX_FILES {
            return;
        }
        walk_files(root, &folder.join(name), found);
    }
}

/// Make a path absolute and tidy without needing it to exist.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Current branch (or short commit) read straight from .git/HEAD.
fn git_branch(workspace: &Path) -> Option<String> {
    for folder in workspace.ancestors() {
        let head = folder.join(".git").join("HEAD");
        if head.is_file() {
            let reference = fs::read_to_string(head).ok()?.trim().to_string();
            return Some(match reference.strip_prefix("ref: ") {
                Some(name) => name.strip_prefix("refs/heads/").unwrap_or(name).to_string(),
                None => reference.chars().take(7).collect(),
            });
        }
    }
    None
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// A clickable region registered while painting.
pub struct Hit {
    pub rect: Rect,
    pub action: Action,
    pub tip: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Drag {
    Sidebar,
    Panel,
    Claude,
    Select,
    Scrollbar,
    /// Forwarding the mouse to a program that tracks it.
    Term(usize),
    /// Selecting text in a terminal.
    TermSelect(usize),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

pub struct Toast {
    pub title: Option<String>,
    pub text: String,
    pub severity: Severity,
    until: Instant,
}

/// Where things were painted in the last frame (None = not on screen).
#[derive(Default)]
pub struct Layout {
    pub sidebar: Option<Rect>,
    pub sidebar_split: Option<Rect>,
    pub panel: Option<Rect>,
    pub panel_split: Option<Rect>,
    pub claude: Option<Rect>,
    pub claude_split: Option<Rect>,
    pub editor: Option<Rect>,
    pub explorer: Option<Rect>,
    pub search_input: Option<Rect>,
    pub search_results: Option<Rect>,
    pub scm: Option<Rect>,
    pub shell: Option<Rect>,
    pub claude_term: Option<Rect>,
    pub gear: Rect,
    pub agent_button: Rect,
    pub menu_x: Vec<i32>,
}

pub struct App {
    pub workspace: PathBuf,
    pub theme: Rc<VSTheme>,
    terminal_stamp: Option<(PathBuf, u64, i64, i64)>,
    /// Thin line cursor (default) or block.
    pub cursor_line: bool,
    tx: Sender<Ev>,
    hl_tx: Sender<HlRequest>,
    diag_tx: Sender<DiagRequest>,

    pub sidebar_visible: bool,
    pub panel_visible: bool,
    pub claude_visible: bool,
    pub sidebar_w: i32,
    pub panel_h: i32,
    pub claude_w: i32,
    pub view: View,
    pub panel_tab: usize,
    pub focus: Focus,

    pub docs: Vec<Doc>,
    pub active: Option<u64>,
    doc_counter: u64,
    untitled: u32,
    /// Per document: the version last sent to the highlighter.
    highlight_sent: Vec<(u64, u64)>,
    pub explorer: Explorer,
    pub search: Search,
    pub scm: Scm,
    pub shell: Term,
    pub shell_name: String,
    /// The running agent's terminal (None until one is picked from the Agents menu).
    pub claude: Option<Term>,
    /// Index into AGENTS of the agent in `claude`.
    pub agent: usize,
    agent_generation: u64,
    /// The path a context menu was opened on.
    context: Option<PathBuf>,
    /// Mouse pointer shape last sent to the terminal.
    shape: &'static str,
    pub modals: Vec<Modal>,
    /// The suggest widget (code completion), when open.
    pub suggest: Option<Suggest>,
    /// The word whose suggestions were dismissed with Esc: (doc, row, start column).
    suggest_dismissed: Option<(u64, usize, usize)>,
    pub toasts: Vec<Toast>,
    clipboard: String,
    files: Vec<String>,
    pub branch: Option<String>,

    pub size: (i32, i32),
    pub mouse: (i32, i32),
    hover_since: Instant,
    pub tooltip: Option<String>,
    pub hits: Vec<Hit>,
    pub lay: Layout,
    pub drag: Option<Drag>,
    pub dirty: bool,
    pub quit: bool,
    /// Escape sequences to send to the real terminal with the next frame.
    pub out: String,
    timers: [Instant; 4],
}

impl App {
    pub fn new(workspace: PathBuf, open: Vec<PathBuf>, theme: Option<String>, tx: Sender<Ev>, size: (i32, i32)) -> App {
        let saved = load_settings().get("theme").and_then(|v| v.as_str()).map(str::to_string);
        let theme = load_theme(theme.or(saved).as_deref());
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| which("bash").map(|p| p.to_string_lossy().to_string()))
            .unwrap_or_else(|| "/bin/sh".to_string());
        let now = Instant::now();
        let mut app = App {
            theme: Rc::new(theme),
            terminal_stamp: terminal_theme_stamp(),
            cursor_line: cursor_is_line(),
            hl_tx: spawn_highlighter(tx.clone()),
            diag_tx: spawn_checker(tx.clone()),
            sidebar_visible: true,
            panel_visible: true,
            // No agent starts by itself: pick one from the Agents menu in the title bar.
            claude_visible: false,
            sidebar_w: 32,
            panel_h: 14,
            claude_w: 64,
            view: View::Explorer,
            panel_tab: TERMINAL_TAB,
            focus: Focus::Explorer,
            docs: Vec::new(),
            active: None,
            doc_counter: 0,
            untitled: 0,
            highlight_sent: Vec::new(),
            explorer: Explorer::new(workspace.clone()),
            search: Search::new(workspace.clone()),
            scm: Scm::new(workspace.clone()),
            shell_name: Path::new(&shell).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            shell: Term::new(0, vec![shell], workspace.clone(), tx.clone()),
            claude: None,
            agent: 0,
            agent_generation: 0,
            context: None,
            shape: "default",
            modals: Vec::new(),
            suggest: None,
            suggest_dismissed: None,
            toasts: Vec::new(),
            clipboard: String::new(),
            files: Vec::new(),
            branch: git_branch(&workspace),
            size,
            mouse: (-1, -1),
            hover_since: now,
            tooltip: None,
            hits: Vec::new(),
            lay: Layout::default(),
            drag: None,
            dirty: true,
            quit: false,
            out: String::new(),
            timers: [now; 4],
            workspace,
            tx,
        };
        for path in open {
            app.open_file(&path, None);
        }
        app
    }

    // -- events -----------------------------------------------------------------

    pub fn handle(&mut self, event: Ev) {
        self.dirty = true;
        match event {
            Ev::Input(Event::Key(key)) => self.on_key(key),
            Ev::Input(Event::Mouse(mouse)) => self.on_mouse(mouse),
            Ev::Input(Event::Paste(text)) => self.on_paste(&text),
            Ev::Input(Event::Resize(w, h)) => self.size = (w as i32, h as i32),
            Ev::Input(_) => {}
            Ev::Pty { term, generation, data } => {
                if let Some(terminal) = self.term(term) {
                    terminal.feed(generation, &data);
                }
            }
            Ev::PtyExit { term, generation } => {
                if let Some(terminal) = self.term(term) {
                    terminal.closed(generation);
                }
            }
            Ev::Highlight(result) => {
                if let Some(doc) = self.docs.iter_mut().find(|d| d.id == result.doc) {
                    if doc.version == result.version && result.lines.len() == doc.lines.len() {
                        doc.spans = result.lines;
                    }
                }
            }
            Ev::Diagnostics(result) => {
                if let Some(doc) = self.docs.iter_mut().find(|d| d.id == result.doc) {
                    if doc.version == result.version {
                        doc.diagnostics = result.diagnostics;
                    }
                }
            }
            Ev::Search { generation, query, output } => self.search.finished(generation, &query, &output),
            Ev::Scm { generation, outcome } => self.scm.finished(generation, outcome),
            Ev::Files(files) => {
                self.files = files;
                let candidates = self.file_candidates();
                if let Some(Modal::Palette(palette)) = self.modals.last_mut() {
                    if palette.files {
                        palette.set_candidates(candidates);
                    }
                }
            }
        }
        self.sync_docs();
        // Tell the terminal which mouse pointer to show (OSC 22; ignored where unsupported).
        let shape = self.pointer_shape();
        if shape != self.shape {
            self.shape = shape;
            self.out.push_str(&format!("\x1b]22;{shape}\x1b\\"));
        }
    }

    /// I-beam over text, hand over things you can click, arrows over the splitters.
    fn pointer_shape(&self) -> &'static str {
        let (x, y) = self.mouse;
        let within = |rect: Option<Rect>| rect.map_or(false, |r| r.contains(x, y));
        match self.drag {
            Some(Drag::Sidebar | Drag::Claude) => return "ew-resize",
            Some(Drag::Panel) => return "ns-resize",
            Some(Drag::Select | Drag::TermSelect(_)) => return "text",
            _ => {}
        }
        if !self.modals.is_empty() {
            "default"
        } else if self.suggest.as_ref().map_or(false, |s| s.contains(x, y)) {
            "pointer"
        } else if within(self.lay.sidebar_split) || within(self.lay.claude_split) {
            "ew-resize"
        } else if within(self.lay.panel_split) {
            "ns-resize"
        } else if self.hits.iter().any(|hit| hit.rect.contains(x, y)) {
            "pointer"
        } else if within(self.lay.editor) {
            match self.active_doc() {
                Some(doc) if doc.is_text() && doc.over_text(x, y) => "text",
                _ => "default",
            }
        } else if within(self.lay.search_input) || self.term_at(x, y).is_some() {
            "text"
        } else if within(self.lay.explorer) || within(self.lay.search_results) || within(self.lay.scm) {
            "pointer"
        } else {
            "default"
        }
    }

    fn term(&mut self, id: usize) -> Option<&mut Term> {
        if id == 0 {
            Some(&mut self.shell)
        } else {
            self.claude.as_mut()
        }
    }

    fn focused_term(&mut self) -> Option<&mut Term> {
        match self.focus {
            Focus::Shell => Some(&mut self.shell),
            Focus::Claude => self.claude.as_mut(),
            _ => None,
        }
    }

    /// Send changed documents to the highlighter and hand copied text to the system clipboard.
    fn sync_docs(&mut self) {
        for doc in &mut self.docs {
            if let Some(text) = doc.copied.take() {
                // OSC 52: ask the terminal to put this on the system clipboard.
                self.out.push_str(&format!("\x1b]52;c;{}\x07", base64(text.as_bytes())));
            }
            let Some(language) = doc.language.filter(|_| doc.is_text()) else { continue };
            let sent = self.highlight_sent.iter_mut().find(|(id, _)| *id == doc.id);
            match sent {
                Some((_, version)) if *version == doc.version => continue,
                Some((_, version)) => *version = doc.version,
                None => self.highlight_sent.push((doc.id, doc.version)),
            }
            let _ = self.hl_tx.send(HlRequest { doc: doc.id, version: doc.version, language, text: doc.plain_text() });
        }
    }

    /// Housekeeping that runs a few times a second.
    pub fn tick(&mut self) {
        let now = Instant::now();
        // The cursor style can be toggled system-wide while we run.
        let line = cursor_is_line();
        if line != self.cursor_line {
            self.cursor_line = line;
            self.dirty = true;
        }
        for doc in &mut self.docs {
            if doc.check_at.map_or(false, |at| at <= now) {
                doc.check_at = None;
                if doc.is_text() && doc.language.is_some() {
                    let _ = self.diag_tx.send(DiagRequest {
                        doc: doc.id,
                        version: doc.version,
                        language: doc.language,
                        text: doc.plain_text(),
                        path: doc.path.clone(),
                    });
                } else if !doc.diagnostics.is_empty() {
                    doc.diagnostics.clear();
                    self.dirty = true;
                }
            }
        }
        let before = self.toasts.len();
        self.toasts.retain(|toast| toast.until > now);
        self.dirty |= self.toasts.len() != before;
        if self.tooltip.is_none() && self.modals.is_empty() && now.duration_since(self.hover_since) > Duration::from_millis(600) {
            self.tooltip = self.tip_under_mouse();
            self.dirty |= self.tooltip.is_some();
        }
        let due = |timer: &mut Instant, seconds: f32| {
            let due = now.duration_since(*timer).as_secs_f32() >= seconds;
            if due {
                *timer = now;
            }
            due
        };
        // Files edited outside (e.g. by Claude) reload when they have no unsaved changes.
        if due(&mut self.timers[0], 1.5) {
            let mut changed = false;
            for doc in &mut self.docs {
                changed |= doc.reload_if_changed();
            }
            if changed {
                self.dirty = true;
                self.sync_docs();
            }
        }
        if due(&mut self.timers[1], 2.0) && self.explorer.poll() {
            self.dirty = true;
        }
        // Re-skin when the desktop theme changes underneath the "terminal" theme.
        if due(&mut self.timers[2], 3.0) {
            let stamp = terminal_theme_stamp();
            if stamp != self.terminal_stamp {
                self.terminal_stamp = stamp;
                if self.theme.id == "terminal" {
                    self.apply_theme("terminal", false);
                }
            }
        }
        if due(&mut self.timers[3], 5.0) {
            let branch = git_branch(&self.workspace);
            if branch != self.branch {
                self.branch = branch;
                self.dirty = true;
            }
        }
    }

    fn tip_under_mouse(&self) -> Option<String> {
        let (x, y) = self.mouse;
        if let Some(hit) = self.hits.iter().rev().find(|hit| hit.rect.contains(x, y)) {
            return hit.tip.map(str::to_string);
        }
        if self.lay.editor.map_or(false, |rect| rect.contains(x, y)) {
            return self.active_doc().and_then(|doc| doc.problem_at(x, y)).map(str::to_string);
        }
        None
    }

    // -- keys ---------------------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let bound = binding(&key);
        if bound == Some(Action::Quit) {
            // Works everywhere, so you can never get stuck inside a popup or terminal.
            let asking = matches!(self.modals.last(), Some(Modal::Choice(choice)) if matches!(choice.then, ChoiceThen::Quit));
            if !asking {
                self.run(Action::Quit);
            }
            return;
        }
        if !self.modals.is_empty() {
            self.modal_key(&key);
            return;
        }
        let in_terminal = matches!(self.focus, Focus::Shell | Focus::Claude);
        if let Some(action) = bound {
            if !(in_terminal && terminal_owned(action)) {
                self.run(action);
                return;
            }
        }
        match self.focus {
            Focus::Shell | Focus::Claude => {
                // Ctrl+C (or Ctrl+Shift+C) with text selected copies it instead of interrupting.
                let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && matches!(key.code, KeyCode::Char('c' | 'C'));
                if ctrl_c && self.copy_terminal_selection() {
                    return;
                }
                if let Some(terminal) = self.focused_term() {
                    terminal.key(&key);
                }
            }
            Focus::Editor => {
                if self.suggest_key(&key) {
                    return;
                }
                let App { docs, active, clipboard, .. } = self;
                let mut edited = false;
                if let Some(doc) = docs.iter_mut().find(|d| Some(d.id) == *active && d.is_text()) {
                    let version = doc.version;
                    doc.key(&key, clipboard);
                    edited = doc.version != version;
                }
                self.suggest_after_key(&key, edited);
            }
            Focus::Explorer => {
                if let Some(path) = self.explorer.key(&key) {
                    self.open_file(&path, None);
                }
            }
            Focus::SearchInput => match key.code {
                KeyCode::Enter => self.search.run(&self.tx),
                KeyCode::Down | KeyCode::Tab if self.search.has_results() => self.focus = Focus::SearchResults,
                _ => {
                    self.search.input.key(&key);
                }
            },
            Focus::SearchResults => {
                if matches!(key.code, KeyCode::BackTab | KeyCode::Esc) {
                    self.focus = Focus::SearchInput;
                } else if let Some((path, line)) = self.search.key(&key) {
                    self.open_file(&path, Some(line));
                }
            }
            Focus::Scm => {
                if let Some(path) = self.scm.key(&key) {
                    self.open_file(&path, None);
                }
            }
        }
    }

    fn modal_key(&mut self, key: &KeyEvent) {
        match self.modals.last_mut() {
            Some(Modal::Menu(menu)) => {
                let owner = menu.owner;
                match menu.key(key) {
                    MenuOutcome::Nothing => {}
                    MenuOutcome::Close => {
                        self.modals.pop();
                    }
                    MenuOutcome::Run(action) => {
                        self.modals.pop();
                        self.run(action);
                    }
                    MenuOutcome::Switch(step) => {
                        if let Some(index) = owner {
                            let count = MENUS.len() as i32;
                            self.modals.pop();
                            self.run(Action::OpenMenu((index as i32 + step).rem_euclid(count) as usize));
                        }
                    }
                }
            }
            Some(Modal::Choice(choice)) => {
                let then = choice.then;
                if let Some(answer) = choice.key(key) {
                    self.modals.pop();
                    self.answered(then, answer);
                }
            }
            Some(Modal::Input(input)) => match key.code {
                KeyCode::Esc => {
                    self.modals.pop();
                }
                KeyCode::Enter => {
                    let (value, doc, after, kind) = (input.input.value.trim().to_string(), input.doc, input.after, input.kind.clone());
                    self.modals.pop();
                    if value.is_empty() {
                    } else if kind == InputKind::SaveAs {
                        self.save_as(doc, &value, after);
                    } else {
                        self.file_op(kind, &value);
                    }
                }
                _ => {
                    input.input.key(key);
                }
            },
            Some(Modal::Pick(pick)) => {
                let outcome = pick.key(key);
                self.picked(outcome);
            }
            Some(Modal::Palette(palette)) => {
                let outcome = palette.key(key);
                self.palette_outcome(outcome);
            }
            None => {}
        }
    }

    fn picked(&mut self, outcome: PickOutcome) {
        let Some(Modal::Pick(pick)) = self.modals.last() else { return };
        let (current, original) = (pick.current().map(str::to_string), pick.original.clone());
        match outcome {
            PickOutcome::Nothing => {}
            PickOutcome::Moved => {
                if let Some(id) = current.filter(|id| *id != self.theme.id) {
                    self.apply_theme(&id, false);
                }
            }
            PickOutcome::Chosen => {
                self.modals.pop();
                self.apply_theme(&current.unwrap_or(original), true);
            }
            PickOutcome::Cancelled => {
                self.modals.pop();
                self.apply_theme(&original, false);
            }
        }
    }

    fn palette_outcome(&mut self, outcome: PaletteOutcome) {
        match outcome {
            PaletteOutcome::Nothing => {}
            PaletteOutcome::Close => {
                self.modals.pop();
            }
            PaletteOutcome::Run(target) => {
                self.modals.pop();
                match target {
                    Target::Action(action) => self.run(action),
                    Target::File(relative) => {
                        let path = self.workspace.join(relative);
                        self.open_file(&path, None);
                    }
                }
            }
        }
    }

    fn on_paste(&mut self, text: &str) {
        match self.modals.last_mut() {
            Some(Modal::Input(input)) => input.input.insert(text),
            Some(Modal::Palette(palette)) => palette.paste(text),
            Some(_) => {}
            None => match self.focus {
                Focus::Shell | Focus::Claude => {
                    if let Some(terminal) = self.focused_term() {
                        terminal.paste(text);
                    }
                }
                Focus::Editor => {
                    self.suggest = None;
                    if let Some(doc) = self.active_editor() {
                        doc.insert(text);
                    }
                }
                Focus::SearchInput => self.search.input.insert(text),
                _ => {}
            },
        }
    }

    // -- code completion (the suggest widget) ---------------------------------------

    /// Keys the open suggest widget takes for itself. True if the key was used.
    fn suggest_key(&mut self, key: &KeyEvent) -> bool {
        let Some(suggest) = self.suggest.as_mut() else { return false };
        if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return false;
        }
        match key.code {
            KeyCode::Up => suggest.step(-1),
            KeyCode::Down => suggest.step(1),
            KeyCode::PageUp => suggest.step(-suggest.page()),
            KeyCode::PageDown => suggest.step(suggest.page()),
            KeyCode::Enter | KeyCode::Tab if !key.modifiers.contains(KeyModifiers::SHIFT) => self.accept_suggest(),
            KeyCode::Esc => {
                // Stay quiet for the rest of this word, like VS Code.
                self.suggest_dismissed = Some((suggest.doc, suggest.row, suggest.start));
                self.suggest = None;
            }
            _ => return false,
        }
        true
    }

    /// After a key reached the editor: narrow, open or close the widget.
    fn suggest_after_key(&mut self, key: &KeyEvent, edited: bool) {
        let typed = match key.code {
            KeyCode::Char(c) if edited && !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => Some(c),
            _ => None,
        };
        let Some((language, before)) = self.active_doc().filter(|d| d.is_text()).map(|doc| {
            let line = &doc.lines[doc.cursor.row];
            (doc.language, line.chars().take(doc.cursor.col).collect::<String>())
        }) else {
            self.suggest = None;
            return;
        };
        match typed {
            Some(c) if completion::is_word(c, completion::word_extra(language)) => {
                if self.suggest.is_some() {
                    self.refilter_suggest();
                } else {
                    self.open_suggest(false);
                }
            }
            Some(_) if completion::is_trigger(language, &before) => {
                self.suggest = None;
                self.open_suggest(false);
            }
            None if edited && matches!(key.code, KeyCode::Backspace | KeyCode::Delete) && self.suggest.is_some() => {
                self.refilter_suggest();
            }
            _ => self.suggest = None,
        }
    }

    /// Open the widget at the cursor. `manual` = Ctrl+Space: no quiet zones, empty word allowed.
    fn open_suggest(&mut self, manual: bool) {
        self.suggest = None;
        let Some(doc) = self.active_doc().filter(|d| d.is_text() && d.selection().is_none()) else { return };
        let (row, col) = (doc.cursor.row, doc.cursor.col);
        let language = doc.language;
        let line = &doc.lines[row];
        let extra = completion::word_extra(language);
        let start = completion::word_start(line, col, extra);
        let typed: String = line.chars().skip(start).take(col - start).collect();
        let before: String = line.chars().take(start).collect();
        let triggered = typed.is_empty() && completion::is_trigger(language, &before);
        if !manual {
            let leading = typed.chars().next();
            let wordish = typed.chars().any(char::is_alphabetic) || typed.chars().all(|c| "$@#".contains(c)) && !typed.is_empty();
            if !completion::auto_suggest(language)
                || completion::quiet_at(doc, row, start)
                || leading.map_or(false, |c| c.is_ascii_digit())
                || !(triggered || wordish)
                || self.suggest_dismissed == Some((doc.id, row, start))
            {
                return;
            }
        }
        let others: Vec<&[String]> = self
            .docs
            .iter()
            .filter(|d| d.id != doc.id && d.is_text() && d.language == language)
            .map(|d| d.lines.as_slice())
            .collect();
        let items = completion::candidates(&completion::Context { language, lines: &doc.lines, row, start, others });
        let mut suggest = Suggest::new(doc.id, row, start, manual || triggered, items);
        suggest.ignore_case = completion::ignore_case(language);
        if suggest.filter(&typed) {
            self.suggest = Some(suggest);
        }
    }

    /// The word under the cursor changed: filter again, or close when it no longer applies.
    fn refilter_suggest(&mut self) {
        let Some(suggest) = self.suggest.as_mut() else { return };
        let Some(doc) = self.docs.iter().find(|d| d.id == suggest.doc && Some(d.id) == self.active) else {
            self.suggest = None;
            return;
        };
        let extra = completion::word_extra(doc.language);
        let line = &doc.lines[doc.cursor.row];
        let typed: String = line.chars().skip(suggest.start).take(doc.cursor.col.saturating_sub(suggest.start)).collect();
        let still_here = doc.cursor.row == suggest.row
            && doc.cursor.col >= suggest.start
            && doc.selection().is_none()
            && typed.chars().all(|c| completion::is_word(c, extra))
            && (suggest.explicit || !typed.is_empty());
        if !still_here || !suggest.filter(&typed) {
            self.suggest = None;
        }
    }

    /// Insert the highlighted suggestion.
    fn accept_suggest(&mut self) {
        let Some(suggest) = self.suggest.take() else { return };
        let Some(item) = suggest.current().cloned() else { return };
        let active = self.active;
        let Some(doc) = self.docs.iter_mut().find(|d| d.id == suggest.doc && Some(d.id) == active) else { return };
        if doc.cursor.row != suggest.row || doc.cursor.col < suggest.start {
            return;
        }
        let line = &doc.lines[doc.cursor.row];
        let typed: String = line.chars().skip(suggest.start).take(doc.cursor.col - suggest.start).collect();
        let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let (text, select) = completion::insert_text(&item, &typed, &indent, completion::ignore_case(doc.language));
        doc.complete(suggest.start, &text, select);
    }

    /// Clicks and the wheel over the widget; any other click closes it. True if used.
    fn suggest_mouse(&mut self, kind: MouseEventKind, x: i32, y: i32) -> bool {
        let Some(suggest) = self.suggest.as_mut() else { return false };
        if suggest.contains(x, y) {
            match kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(row) = suggest.row_at(x, y) {
                        suggest.selected = row;
                        self.accept_suggest();
                    }
                }
                MouseEventKind::ScrollUp => suggest.scroll(-1),
                MouseEventKind::ScrollDown => suggest.scroll(1),
                _ => {}
            }
            return true;
        }
        if matches!(kind, MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
            self.suggest = None;
        }
        false
    }

    /// Where the widget hangs: under the start of the word being completed.
    pub fn suggest_anchor(&self) -> Option<(i32, i32)> {
        let suggest = self.suggest.as_ref()?;
        if self.focus != Focus::Editor || !self.modals.is_empty() || self.active != Some(suggest.doc) {
            return None;
        }
        let doc = self.active_doc()?;
        if doc.cursor.row != suggest.row || doc.cursor.col < suggest.start {
            return None;
        }
        doc.screen_pos(Pos { row: suggest.row, col: suggest.start })
    }

    // -- mouse --------------------------------------------------------------------

    fn on_mouse(&mut self, mouse: MouseEvent) {
        let (x, y) = (mouse.column as i32, mouse.row as i32);
        if (x, y) != self.mouse {
            self.hover_since = Instant::now();
            self.tooltip = None;
        }
        self.mouse = (x, y);
        if !self.modals.is_empty() {
            self.modal_mouse(mouse.kind, x, y);
            return;
        }
        if self.suggest_mouse(mouse.kind, x, y) {
            return;
        }
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let within = |rect: Option<Rect>| rect.map_or(false, |r| r.contains(x, y));
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(x, y, shift),
            MouseEventKind::Down(button) => {
                let hit = self.hits.iter().rev().find(|hit| hit.rect.contains(x, y)).map(|hit| hit.action);
                let tracking = self.term_at(x, y).and_then(|id| self.term(id)).map_or(false, |t| t.mouse_tracking());
                if let (MouseButton::Middle, Some(Action::ActivateDoc(id) | Action::CloseDoc(id))) = (button, hit) {
                    self.close_doc(id);
                } else if button == MouseButton::Right && !tracking {
                    self.context_menu(x, y, hit);
                } else if let Some(id) = self.term_at(x, y) {
                    let code = if button == MouseButton::Middle { 1 } else { 2 };
                    if let Some(terminal) = self.term(id).filter(|t| t.mouse_tracking()) {
                        terminal.send_mouse(code, x, y, false);
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.drag {
                // Splitters: `sidebar_w` etc. follow the mouse, within sensible limits.
                Some(Drag::Sidebar) => {
                    let left = self.lay.sidebar.map_or(5, |r| r.x);
                    self.sidebar_w = (x - left).clamp(16, (self.size.0 - 30).max(16));
                }
                Some(Drag::Claude) => self.claude_w = (self.size.0 - x - 1).clamp(30, (self.size.0 - 30).max(30)),
                Some(Drag::Panel) => {
                    let bottom = self.lay.panel.map_or(self.size.1 - 1, |r| r.bottom());
                    self.panel_h = (bottom - y - 1).clamp(4, (self.size.1 - 8).max(4));
                }
                Some(Drag::Select) => {
                    if let Some(doc) = self.active_editor() {
                        doc.mouse_drag(x, y);
                    }
                }
                Some(Drag::Scrollbar) => {
                    if let Some(doc) = self.active_editor() {
                        doc.scrollbar_to(y);
                    }
                }
                Some(Drag::Term(id)) => {
                    if let Some(terminal) = self.term(id).filter(|t| t.mouse_tracking() && t.mouse_motion()) {
                        terminal.send_mouse(32, x, y, false);
                    }
                }
                Some(Drag::TermSelect(id)) => {
                    if let Some(terminal) = self.term(id) {
                        terminal.select_to(x, y);
                    }
                }
                None => {}
            },
            MouseEventKind::Up(button) => {
                if let Some(Drag::Term(id)) = self.drag {
                    let code = match button {
                        MouseButton::Left => 0,
                        MouseButton::Middle => 1,
                        MouseButton::Right => 2,
                    };
                    if let Some(terminal) = self.term(id).filter(|t| t.mouse_tracking()) {
                        terminal.send_mouse(code, x, y, true);
                    }
                }
                self.drag = None;
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                let step = if up { -3 } else { 3 };
                if within(self.lay.editor) {
                    if let Some(doc) = self.active_editor() {
                        if shift {
                            doc.scroll_sideways(step * 2);
                        } else {
                            doc.scroll(step);
                        }
                    }
                } else if within(self.lay.explorer) {
                    self.explorer.scroll(step);
                } else if within(self.lay.search_results) {
                    self.search.scroll(step);
                } else if within(self.lay.scm) {
                    self.scm.scroll(step);
                } else if let Some(id) = self.term_at(x, y) {
                    if let Some(terminal) = self.term(id) {
                        terminal.wheel(up, x, y);
                    }
                }
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
                if within(self.lay.editor) {
                    let step = if mouse.kind == MouseEventKind::ScrollLeft { -6 } else { 6 };
                    if let Some(doc) = self.active_editor() {
                        doc.scroll_sideways(step);
                    }
                }
            }
            _ => {}
        }
    }

    fn term_at(&self, x: i32, y: i32) -> Option<usize> {
        if self.lay.shell.map_or(false, |r| r.contains(x, y)) {
            Some(0)
        } else if self.lay.claude_term.map_or(false, |r| r.contains(x, y)) {
            Some(1)
        } else {
            None
        }
    }

    fn mouse_down(&mut self, x: i32, y: i32, shift: bool) {
        let within = |rect: Option<Rect>| rect.map_or(false, |r| r.contains(x, y));
        for (rect, drag) in [
            (self.lay.sidebar_split, Drag::Sidebar),
            (self.lay.panel_split, Drag::Panel),
            (self.lay.claude_split, Drag::Claude),
        ] {
            if within(rect) {
                self.drag = Some(drag);
                return;
            }
        }
        if let Some(action) = self.hits.iter().rev().find(|hit| hit.rect.contains(x, y)).map(|hit| hit.action) {
            self.run(action);
            return;
        }
        if within(self.lay.editor) {
            let bar = self.lay.editor.map_or(i32::MAX, |r| r.right() - SCROLLBAR_W);
            if let Some(doc) = self.active_editor() {
                if x >= bar {
                    doc.scrollbar_to(y);
                    self.drag = Some(Drag::Scrollbar);
                } else {
                    doc.mouse_down(x, y, shift);
                    self.drag = Some(Drag::Select);
                }
                self.focus = Focus::Editor;
            }
        } else if within(self.lay.explorer) {
            self.focus = Focus::Explorer;
            if let Some(path) = self.explorer.click(y) {
                self.open_file(&path, None);
            }
        } else if within(self.lay.search_input) {
            self.focus = Focus::SearchInput;
        } else if within(self.lay.search_results) {
            self.focus = Focus::SearchResults;
            if let Some((path, line)) = self.search.click(y) {
                self.open_file(&path, Some(line));
            }
        } else if within(self.lay.scm) {
            self.focus = Focus::Scm;
            if let Some(path) = self.scm.click(y) {
                self.open_file(&path, None);
            }
        } else if let Some(id) = self.term_at(x, y) {
            self.focus = if id == 0 { Focus::Shell } else { Focus::Claude };
            let Some(terminal) = self.term(id) else { return };
            // Programs that track the mouse get the click; Shift+drag selects text anyway.
            if terminal.mouse_tracking() && !shift {
                terminal.clear_selection();
                terminal.send_mouse(0, x, y, false);
                self.drag = Some(Drag::Term(id));
            } else {
                terminal.select_start(x, y, shift);
                self.drag = Some(Drag::TermSelect(id));
            }
        }
    }

    /// The right-click menu for whatever is under the mouse.
    fn context_menu(&mut self, x: i32, y: i32, hit: Option<Action>) {
        let within = |rect: Option<Rect>| rect.map_or(false, |r| r.contains(x, y));
        let mut items: Vec<MenuItem> = Vec::new();
        if let Some(Action::ActivateDoc(id) | Action::CloseDoc(id)) = hit {
            self.context = self.docs.iter().find(|doc| doc.id == id).and_then(|doc| doc.path.clone());
            items.extend([
                item("Close", "Ctrl+W", Action::CloseDoc(id)),
                item("Close Others", "", Action::CloseOthers(id)),
                item("Close All", "", Action::CloseAll),
            ]);
            if self.context.is_some() {
                items.extend([None, item("Copy Path", "", Action::CopyPath(false)), item("Copy Relative Path", "", Action::CopyPath(true))]);
            }
        } else if hit.is_some() {
            return;
        } else if within(self.lay.explorer) {
            self.focus = Focus::Explorer;
            self.context = self.explorer.select_at(y);
            items.extend([item("New File...", "", Action::NewFileIn), item("New Folder...", "", Action::NewFolder), None]);
            if self.context.is_some() {
                items.extend([
                    item("Rename...", "", Action::Rename),
                    item("Delete", "", Action::Delete),
                    None,
                    item("Copy Path", "", Action::CopyPath(false)),
                    item("Copy Relative Path", "", Action::CopyPath(true)),
                ]);
            } else {
                items.extend([
                    item("Refresh Explorer", "", Action::RefreshExplorer),
                    item("Collapse Folders", "", Action::CollapseExplorer),
                ]);
            }
        } else if within(self.lay.editor) && self.active_editor().is_some() {
            self.focus = Focus::Editor;
            items.extend([
                item("Cut", "Ctrl+X", Action::Editor(EdCmd::Cut)),
                item("Copy", "Ctrl+C", Action::Editor(EdCmd::Copy)),
                item("Paste", "Ctrl+V", Action::Editor(EdCmd::Paste)),
                None,
                item("Select All", "Ctrl+A", Action::Editor(EdCmd::SelectAll)),
                None,
                item("Command Palette...", "F1", Action::CommandPalette),
            ]);
        } else if self.term_at(x, y) == Some(0) {
            self.focus = Focus::Shell;
            items.extend(self.terminal_edit_items());
            items.extend([
                item("Restart Terminal", "", Action::RestartTerminal),
                item("Hide Panel", "Ctrl+J", Action::TogglePanel),
            ]);
        } else if within(self.lay.claude) {
            if self.term_at(x, y) == Some(1) {
                self.focus = Focus::Claude;
                items.extend(self.terminal_edit_items());
            }
            items.push(item("Switch Agent...", "", Action::AgentMenu));
            if self.claude.is_some() {
                items.extend([item("New Conversation", "", Action::RestartClaude), item("Stop Agent", "", Action::StopAgent)]);
            }
            items.extend([None, item("Hide Agent Panel", "Ctrl+Alt+B", Action::ToggleClaude)]);
        } else {
            return;
        }
        self.modals.push(Modal::Menu(Menu::new(items, x, y, false, None)));
    }

    /// Copy / Paste / Select All for the focused terminal's right-click menu.
    fn terminal_edit_items(&mut self) -> Vec<MenuItem> {
        let selected = self.focused_term().map_or(false, |terminal| terminal.has_selection());
        let mut items = Vec::new();
        if selected {
            items.push(item("Copy", "Ctrl+C", Action::Editor(EdCmd::Copy)));
        }
        if !self.clipboard.is_empty() {
            items.push(item("Paste", "Ctrl+Shift+V", Action::Editor(EdCmd::Paste)));
        }
        items.extend([item("Select All", "", Action::Editor(EdCmd::SelectAll)), None]);
        items
    }

    /// Copy the focused terminal's selection to the clipboard. False when nothing is selected.
    fn copy_terminal_selection(&mut self) -> bool {
        let Some(terminal) = self.focused_term() else { return false };
        let Some(text) = terminal.selected_text() else { return false };
        terminal.clear_selection();
        self.set_clipboard(text);
        true
    }

    /// Put text on our clipboard and, through the terminal (OSC 52), the system's.
    fn set_clipboard(&mut self, text: String) {
        self.out.push_str(&format!("\x1b]52;c;{}\x07", base64(text.as_bytes())));
        self.clipboard = text;
    }

    /// Create or rename a file/folder from the name typed into the input box.
    fn file_op(&mut self, kind: InputKind, name: &str) {
        let (target, old) = match &kind {
            InputKind::NewFile(dir) | InputKind::NewFolder(dir) => (normalize(&dir.join(name)), None),
            InputKind::Rename(old) => (normalize(&old.with_file_name(name)), Some(old.clone())),
            InputKind::SaveAs => return,
        };
        if target.exists() {
            self.notify(None, &format!("'{name}' already exists in this folder. Choose a different name."), Severity::Warning, 5);
            return;
        }
        let done = match (&kind, &old) {
            (_, Some(old)) => fs::rename(old, &target),
            (InputKind::NewFolder(_), _) => fs::create_dir_all(&target),
            _ => target.parent().map_or(Ok(()), fs::create_dir_all).and_then(|_| fs::write(&target, "")),
        };
        if let Err(error) = done {
            self.notify(None, &format!("Could not create '{name}': {error}"), Severity::Error, 6);
            return;
        }
        if let Some(old) = old {
            // Open editors follow the file to its new name.
            for doc in &mut self.docs {
                let moved = doc.path.as_ref().and_then(|path| path.strip_prefix(&old).ok().map(|rest| rest.to_path_buf()));
                if let Some(rest) = moved {
                    let path = if rest.as_os_str().is_empty() { target.clone() } else { target.join(rest) };
                    doc.title = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    doc.path = Some(path);
                }
            }
        }
        self.explorer.reveal(&target);
        if matches!(kind, InputKind::NewFile(_)) {
            self.open_file(&target, None);
        }
    }

    /// Delete the context path: to the Trash when `gio` is there, otherwise for good.
    fn delete_context(&mut self, trash: bool) {
        let Some(path) = self.context.take().filter(|path| *path != self.workspace) else { return };
        let done = if trash {
            match std::process::Command::new("gio").arg("trash").arg(&path).output() {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
                Err(error) => Err(error.to_string()),
            }
        } else if path.is_dir() && !path.is_symlink() {
            fs::remove_dir_all(&path).map_err(|e| e.to_string())
        } else {
            fs::remove_file(&path).map_err(|e| e.to_string())
        };
        if let Err(error) = done {
            self.notify(None, &format!("Could not delete '{}': {error}", path.display()), Severity::Error, 6);
            return;
        }
        let gone: Vec<u64> = self.docs.iter().filter(|doc| doc.path.as_ref().map_or(false, |p| p.starts_with(&path))).map(|doc| doc.id).collect();
        for id in gone {
            self.remove(id);
        }
        self.explorer.reload();
    }

    /// Ask for a name for a new file or folder in the context folder (or the selected one).
    fn ask_new(&mut self, folder: bool) {
        let dir = match self.context.take() {
            Some(path) if path.is_dir() => path,
            Some(path) => path.parent().unwrap_or(&self.workspace).to_path_buf(),
            None => self.explorer.target_dir(),
        };
        let shown = dir.strip_prefix(&self.workspace).unwrap_or(&dir).to_string_lossy().to_string();
        let place = if shown.is_empty() { "the workspace folder".to_string() } else { format!("{shown}/") };
        let what = if folder { "New Folder" } else { "New File" };
        let prompt = format!("{what} in {place} (Enter to create, Esc to cancel)");
        let kind = if folder { InputKind::NewFolder(dir) } else { InputKind::NewFile(dir) };
        let hint = if folder { "folder name" } else { "file name" };
        self.modals.push(Modal::Input(InputBox::new(&prompt, "", hint, 0, After::Nothing).kind(kind)));
    }

    fn modal_mouse(&mut self, kind: MouseEventKind, x: i32, y: i32) {
        let down = kind == MouseEventKind::Down(MouseButton::Left);
        match self.modals.last_mut() {
            Some(Modal::Menu(menu)) => {
                if down {
                    // Clicking another title-bar menu switches straight to it.
                    let other = self.lay.menu_x.iter().zip(MENUS).position(|(mx, (name, _))| y == 0 && x >= *mx && x < mx + name.len() as i32 + 2);
                    match menu.click(x, y) {
                        MenuOutcome::Run(action) => {
                            self.modals.pop();
                            self.run(action);
                        }
                        MenuOutcome::Close => {
                            let same = other.is_some() && other == menu.owner;
                            self.modals.pop();
                            if let Some(index) = other.filter(|_| !same) {
                                self.run(Action::OpenMenu(index));
                            }
                        }
                        _ => {}
                    }
                } else if kind == MouseEventKind::Moved {
                    menu.hover(x, y);
                }
            }
            Some(Modal::Choice(choice)) if down => {
                let then = choice.then;
                if let Some(answer) = choice.click(x, y) {
                    self.modals.pop();
                    self.answered(then, answer);
                }
            }
            Some(Modal::Input(input)) if down => {
                if !input.contains(x, y) {
                    self.modals.pop();
                }
            }
            Some(Modal::Pick(pick)) if down => {
                let outcome = pick.click(x, y);
                self.picked(outcome);
            }
            Some(Modal::Palette(palette)) => {
                if down {
                    let outcome = palette.click(x, y);
                    self.palette_outcome(outcome);
                } else if kind == MouseEventKind::ScrollUp {
                    palette.scroll(-3);
                } else if kind == MouseEventKind::ScrollDown {
                    palette.scroll(3);
                }
            }
            _ => {}
        }
    }

    // -- helpers ------------------------------------------------------------------

    pub fn active_doc(&self) -> Option<&Doc> {
        self.docs.iter().find(|doc| Some(doc.id) == self.active)
    }

    /// The active document, if it is a text editor.
    fn active_editor(&mut self) -> Option<&mut Doc> {
        let active = self.active;
        self.docs.iter_mut().find(|doc| Some(doc.id) == active && doc.is_text())
    }

    pub fn notify(&mut self, title: Option<&str>, text: &str, severity: Severity, seconds: u64) {
        self.toasts.push(Toast {
            title: title.map(str::to_string),
            text: text.to_string(),
            severity,
            until: Instant::now() + Duration::from_secs(seconds),
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    fn set_visible(&mut self, part: &str, visible: bool) {
        let focus_inside = match part {
            "sidebar" => matches!(self.focus, Focus::Explorer | Focus::SearchInput | Focus::SearchResults | Focus::Scm),
            "panel" => self.focus == Focus::Shell,
            _ => self.focus == Focus::Claude,
        };
        if !visible && focus_inside {
            self.focus = Focus::Editor;
        }
        match part {
            "sidebar" => self.sidebar_visible = visible,
            "panel" => self.panel_visible = visible,
            _ => self.claude_visible = visible,
        }
    }

    /// Switch every part of the UI to another theme, live.
    fn apply_theme(&mut self, id: &str, persist: bool) {
        self.theme = Rc::new(load_theme(Some(id)));
        if persist {
            save_setting("theme", serde_json::Value::String(self.theme.id.clone()));
        }
    }

    // -- documents ----------------------------------------------------------------

    fn next_id(&mut self) -> u64 {
        self.doc_counter += 1;
        self.doc_counter
    }

    pub fn open_file(&mut self, path: &Path, line: Option<usize>) {
        const BINARY: &str =
            "The file is not displayed in the text editor because it is either binary or uses an unsupported text encoding.";
        let path = fs::canonicalize(path).unwrap_or_else(|_| normalize(path));
        if let Some(id) = self.docs.iter().find(|doc| doc.path.as_deref() == Some(&path)).map(|doc| doc.id) {
            self.activate(id, line);
            return;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let data = fs::metadata(&path).and_then(|meta| {
            if meta.len() > MAX_FILE_BYTES {
                Ok(None)
            } else {
                fs::read(&path).map(Some)
            }
        });
        let id = self.next_id();
        let doc = match data {
            Err(error) => {
                self.notify(None, &format!("Unable to open '{name}': {error}"), Severity::Error, 6);
                return;
            }
            Ok(None) => Doc::not_editable(id, path, "The file is not displayed in the text editor because it is too large."),
            Ok(Some(bytes)) => {
                let binary = bytes.iter().take(8192).any(|byte| *byte == 0);
                match String::from_utf8(bytes) {
                    Ok(text) if !binary => Doc::new(id, &text, Some(path), name),
                    _ => Doc::not_editable(id, path, BINARY),
                }
            }
        };
        self.docs.push(doc);
        self.activate(id, line);
    }

    fn new_untitled(&mut self) {
        self.untitled += 1;
        let id = self.next_id();
        self.docs.push(Doc::new(id, "", None, format!("Untitled-{}", self.untitled)));
        self.activate(id, None);
    }

    fn activate(&mut self, id: u64, line: Option<usize>) {
        self.active = Some(id);
        if let Some(doc) = self.active_editor() {
            if let Some(line) = line {
                doc.go_to_line(line.saturating_sub(1));
            }
            self.focus = Focus::Editor;
        }
    }

    fn cycle(&mut self, step: i32) {
        let Some(index) = self.docs.iter().position(|doc| Some(doc.id) == self.active) else { return };
        if self.docs.len() > 1 {
            let next = (index as i32 + step).rem_euclid(self.docs.len() as i32) as usize;
            self.activate(self.docs[next].id, None);
        }
    }

    /// Save a document. `after` runs once the save has succeeded.
    fn save(&mut self, id: u64, after: After, save_as: bool) {
        let Some(doc) = self.docs.iter().find(|doc| doc.id == id && doc.is_text()) else { return };
        if doc.path.is_some() && !save_as {
            self.write(id, after);
            return;
        }
        let start = match &doc.path {
            Some(path) => path.strip_prefix(&self.workspace).unwrap_or(path).to_string_lossy().to_string(),
            None => String::new(),
        };
        self.modals.push(Modal::Input(InputBox::new(
            "Save As - path relative to the workspace folder (Enter to confirm, Esc to cancel)",
            &start,
            "file name",
            id,
            after,
        )));
    }

    fn save_as(&mut self, id: u64, value: &str, after: After) {
        let typed = match value.strip_prefix("~/") {
            Some(rest) => home().join(rest),
            None => PathBuf::from(value),
        };
        let path = normalize(&self.workspace.join(typed));
        if let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) {
            doc.title = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            doc.path = Some(path);
            // The file type may have changed: colours and checks follow.
            doc.apply_language();
            doc.spans = vec![Vec::new(); doc.lines.len()];
            doc.diagnostics.clear();
            self.highlight_sent.retain(|(doc_id, _)| *doc_id != id);
        }
        self.write(id, after);
    }

    fn write(&mut self, id: u64, after: After) {
        let Some(doc) = self.docs.iter_mut().find(|doc| doc.id == id) else { return };
        if let Err(error) = doc.write_to_disk() {
            let message = format!("Failed to save '{}': {error}", doc.title);
            self.notify(None, &message, Severity::Error, 6);
            return;
        }
        self.explorer.poll();
        match after {
            After::Nothing => {}
            After::Close => self.remove(id),
            After::Run => self.run_in_terminal(id),
            After::SaveAll => self.save_next_untitled(),
        }
    }

    /// Save All: ask for a name for the next untitled file with changes, one at a time.
    fn save_next_untitled(&mut self) {
        let next = self.docs.iter().find(|doc| doc.dirty && doc.is_text() && doc.path.is_none()).map(|doc| doc.id);
        if let Some(id) = next {
            self.activate(id, None); // so you can see which file the name is for
            self.save(id, After::SaveAll, false);
        }
    }

    fn close_doc(&mut self, id: u64) {
        let Some(doc) = self.docs.iter().find(|doc| doc.id == id) else { return };
        if !doc.dirty {
            self.remove(id);
            return;
        }
        self.modals.push(Modal::Choice(Choice::new(
            format!("Do you want to save the changes you made to {}?", doc.title),
            "Your changes will be lost if you don't save them.".to_string(),
            vec!["Save", "Don't Save", "Cancel"],
            ChoiceThen::CloseDoc(id),
        )));
    }

    fn remove(&mut self, id: u64) {
        let Some(index) = self.docs.iter().position(|doc| doc.id == id) else { return };
        self.docs.remove(index);
        self.highlight_sent.retain(|(doc_id, _)| *doc_id != id);
        if self.active == Some(id) {
            self.active = self.docs.get(index.min(self.docs.len().saturating_sub(1))).map(|doc| doc.id);
        }
        if self.docs.is_empty() && self.focus == Focus::Editor && self.sidebar_visible && self.view == View::Explorer {
            self.focus = Focus::Explorer; // nothing left to type into
        }
    }

    fn answered(&mut self, then: ChoiceThen, answer: Option<&'static str>) {
        match (then, answer) {
            (ChoiceThen::CloseDoc(id), Some("Save")) => self.save(id, After::Close, false),
            (ChoiceThen::CloseDoc(id), Some("Don't Save")) => self.remove(id),
            (ChoiceThen::Quit, Some("Don't Save")) => self.quit = true,
            (ChoiceThen::Delete, Some("Move to Trash")) => self.delete_context(true),
            (ChoiceThen::Delete, Some("Delete")) => self.delete_context(false),
            (ChoiceThen::Quit, Some("Save All")) => {
                let saved: Vec<u64> = self.docs.iter().filter(|doc| doc.dirty && doc.path.is_some()).map(|doc| doc.id).collect();
                for id in saved {
                    self.write(id, After::Nothing);
                }
                if self.docs.iter().any(|doc| doc.dirty) {
                    self.notify(None, "Untitled files still have unsaved changes. Save or close them first.", Severity::Warning, 6);
                } else {
                    self.quit = true;
                }
            }
            _ => {}
        }
    }

    fn run_in_terminal(&mut self, id: u64) {
        let Some(path) = self.docs.iter().find(|doc| doc.id == id).and_then(|doc| doc.path.clone()) else { return };
        let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let Some((_, runner)) = RUNNERS.iter().find(|(ext, _)| *ext == extension) else { return };
        self.set_visible("panel", true);
        self.panel_tab = TERMINAL_TAB;
        self.focus = Focus::Shell;
        self.shell.send_line(&format!("{} {}", shell_quote(runner), shell_quote(&path.to_string_lossy())));
    }

    fn file_candidates(&self) -> Vec<(String, Target)> {
        self.files.iter().map(|file| (file.clone(), Target::File(file.clone()))).collect()
    }

    /// Focusable regions in F6 order.
    fn parts(&self) -> Vec<Focus> {
        let mut parts = Vec::new();
        if self.active_doc().map_or(false, |doc| doc.is_text()) {
            parts.push(Focus::Editor);
        }
        if self.panel_visible {
            parts.push(Focus::Shell);
        }
        if self.claude_visible && self.claude.is_some() {
            parts.push(Focus::Claude);
        }
        if self.sidebar_visible && self.view == View::Explorer {
            parts.push(Focus::Explorer);
        }
        parts
    }

    fn focus_part(&mut self, step: i32) {
        let parts = self.parts();
        if parts.is_empty() {
            return;
        }
        let index = parts.iter().position(|part| *part == self.focus).map(|i| i as i32).unwrap_or(-1);
        self.focus = parts[(index + step).rem_euclid(parts.len() as i32) as usize];
        if self.focus == Focus::Shell {
            self.panel_tab = TERMINAL_TAB;
        }
    }

    // -- actions ------------------------------------------------------------------

    pub fn run(&mut self, action: Action) {
        match action {
            Action::Quit => {
                let dirty: Vec<&Doc> = self.docs.iter().filter(|doc| doc.dirty).collect();
                if dirty.is_empty() {
                    self.quit = true;
                    return;
                }
                let names: Vec<&str> = dirty.iter().take(5).map(|doc| doc.title.as_str()).collect();
                self.modals.push(Modal::Choice(Choice::new(
                    format!("Do you want to save the changes to the following {} file(s)?", dirty.len()),
                    format!("{}\n\nYour changes will be lost if you don't save them.", names.join(", ")),
                    vec!["Save All", "Don't Save", "Cancel"],
                    ChoiceThen::Quit,
                )));
            }
            Action::Save | Action::SaveAs => {
                if let Some(id) = self.active {
                    self.save(id, After::Nothing, action == Action::SaveAs);
                }
            }
            Action::SaveAll => {
                let named: Vec<u64> =
                    self.docs.iter().filter(|doc| doc.dirty && doc.is_text() && doc.path.is_some()).map(|doc| doc.id).collect();
                for id in named {
                    self.write(id, After::Nothing);
                }
                self.save_next_untitled();
            }
            Action::CloseEditor => {
                if let Some(id) = self.active {
                    self.close_doc(id);
                }
            }
            Action::QuickOpen => {
                // Walk the workspace in the background; the list fills in when it is ready.
                let (root, tx) = (self.workspace.clone(), self.tx.clone());
                thread::spawn(move || {
                    let mut found = Vec::new();
                    walk_files(&root, &root, &mut found);
                    let _ = tx.send(Ev::Files(found));
                });
                self.modals.push(Modal::Palette(Palette::new(true, "Search files by name", self.file_candidates())));
            }
            Action::CommandPalette => {
                let commands = COMMANDS.iter().map(|(title, action)| (title.to_string(), Target::Action(*action))).collect();
                self.modals.push(Modal::Palette(Palette::new(false, "Type the name of a command to run", commands)));
            }
            Action::NewFile => self.new_untitled(),
            Action::ToggleSidebar => self.set_visible("sidebar", !self.sidebar_visible),
            Action::TogglePanel => self.set_visible("panel", !self.panel_visible),
            Action::ToggleClaude => {
                self.set_visible("claude", !self.claude_visible);
                if self.claude_visible {
                    self.run(Action::FocusClaude);
                }
            }
            Action::ShowView(view) => {
                self.set_visible("sidebar", true);
                self.view = view;
                match view {
                    View::Explorer => self.focus = Focus::Explorer,
                    View::Search => self.focus = Focus::SearchInput,
                    View::Scm => {
                        self.focus = Focus::Scm;
                        self.scm.reload(&self.tx);
                    }
                    _ => {}
                }
            }
            Action::SelectTheme => {
                let title = "Select Color Theme (Up/Down to preview, Enter to keep)";
                self.modals.push(Modal::Pick(QuickPick::new(title, available_themes(), &self.theme.id)));
            }
            Action::FocusTerminal => {
                if self.focus == Focus::Shell {
                    // Ctrl+` toggles, like VS Code.
                    self.set_visible("panel", false);
                } else {
                    self.set_visible("panel", true);
                    self.panel_tab = TERMINAL_TAB;
                    self.focus = Focus::Shell;
                }
            }
            Action::RestartTerminal => {
                self.set_visible("panel", true);
                self.panel_tab = TERMINAL_TAB;
                self.focus = Focus::Shell;
                self.shell.restart();
            }
            Action::FocusClaude => {
                self.set_visible("claude", true);
                if self.claude.is_some() {
                    self.focus = Focus::Claude;
                }
            }
            Action::RestartClaude => {
                self.run(Action::FocusClaude);
                match &mut self.claude {
                    Some(claude) => claude.restart(),
                    None => self.run(Action::AgentMenu),
                }
            }
            Action::AgentMenu => {
                let running = self.claude.is_some().then_some(self.agent);
                let mut items: Vec<MenuItem> = (0..AGENTS.len())
                    .filter(|index| agent_command(*index).is_some())
                    .map(|index| item(AGENTS[index].0, if running == Some(index) { "running" } else { "" }, Action::StartAgent(index)))
                    .collect();
                if items.is_empty() {
                    self.notify(None, "No coding agent is installed. Install Claude Code, Codex, Grok or another agent CLI, then try again.", Severity::Warning, 8);
                    return;
                }
                if running.is_some() {
                    items.extend([None, item("New Conversation", "", Action::RestartClaude), item("Stop Agent", "", Action::StopAgent)]);
                }
                let button = self.lay.agent_button;
                self.modals.push(Modal::Menu(Menu::new(items, button.x, 1, false, None)));
            }
            Action::StartAgent(index) => {
                let Some(command) = agent_command(index) else {
                    let (name, command) = AGENTS.get(index).copied().unwrap_or(("That agent", "it"));
                    self.notify(None, &format!("{name} isn't installed (no `{command}` command found)."), Severity::Warning, 6);
                    return;
                };
                if self.claude.is_none() || self.agent != index {
                    // Output still in flight from the old agent must not reach the new one.
                    if let Some(old) = self.claude.take() {
                        self.agent_generation = old.generation + 1;
                    }
                    let mut term = Term::new(1, command, self.workspace.clone(), self.tx.clone());
                    term.generation = self.agent_generation;
                    self.claude = Some(term);
                    self.agent = index;
                }
                self.run(Action::FocusClaude);
            }
            Action::StopAgent => {
                if let Some(old) = self.claude.take() {
                    self.agent_generation = old.generation + 1;
                }
                if self.focus == Focus::Claude {
                    self.focus = Focus::Editor;
                }
            }
            Action::NewFileIn => self.ask_new(false),
            Action::NewFolder => self.ask_new(true),
            Action::Rename => {
                let Some(path) = self.context.clone() else { return };
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let prompt = format!("Rename '{name}' (Enter to confirm, Esc to cancel)");
                self.modals.push(Modal::Input(InputBox::new(&prompt, &name, "new name", 0, After::Nothing).kind(InputKind::Rename(path))));
            }
            Action::Delete => {
                let Some(path) = self.context.clone() else { return };
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let extra = if path.is_dir() { " and its contents" } else { "" };
                let (button, detail) = match which("gio") {
                    Some(_) => ("Move to Trash", "You can restore it from the Trash."),
                    None => ("Delete", "This cannot be undone."),
                };
                self.modals.push(Modal::Choice(Choice::new(
                    format!("Are you sure you want to delete '{name}'{extra}?"),
                    detail.to_string(),
                    vec![button, "Cancel"],
                    ChoiceThen::Delete,
                )));
            }
            Action::CopyPath(relative) => {
                let Some(path) = self.context.clone() else { return };
                let shown = if relative { path.strip_prefix(&self.workspace).unwrap_or(&path) } else { &path };
                self.set_clipboard(shown.to_string_lossy().to_string());
            }
            Action::CloseOthers(keep) => {
                let ids: Vec<u64> = self.docs.iter().map(|doc| doc.id).filter(|id| *id != keep).collect();
                for id in ids {
                    self.close_doc(id);
                }
            }
            Action::CloseAll => {
                let ids: Vec<u64> = self.docs.iter().map(|doc| doc.id).collect();
                for id in ids {
                    self.close_doc(id);
                }
            }
            Action::FocusNextPart => self.focus_part(1),
            Action::FocusPreviousPart => self.focus_part(-1),
            Action::NextEditor => self.cycle(1),
            Action::PreviousEditor => self.cycle(-1),
            Action::Editor(command) if matches!(self.focus, Focus::Shell | Focus::Claude) => match command {
                // With a terminal focused, Copy / Paste / Select All act on the terminal.
                EdCmd::Copy => {
                    self.copy_terminal_selection();
                }
                EdCmd::Paste => {
                    let text = self.clipboard.clone();
                    if let Some(terminal) = self.focused_term().filter(|_| !text.is_empty()) {
                        terminal.paste(&text);
                    }
                }
                EdCmd::SelectAll => {
                    if let Some(terminal) = self.focused_term() {
                        terminal.select_all();
                    }
                }
                _ => {
                    self.focus = Focus::Editor;
                    self.run(action);
                }
            },
            Action::TriggerSuggest => {
                if self.focus == Focus::Editor {
                    self.open_suggest(true);
                }
            }
            Action::Editor(command) => {
                self.suggest = None;
                let App { docs, active, clipboard, focus, .. } = self;
                if let Some(doc) = docs.iter_mut().find(|d| Some(d.id) == *active && d.is_text()) {
                    *focus = Focus::Editor;
                    doc.command(command, clipboard);
                }
            }
            Action::RunFile => {
                let Some((id, path, dirty, title)) = self
                    .active_doc()
                    .filter(|doc| doc.is_text() && doc.path.is_some())
                    .map(|doc| (doc.id, doc.path.clone().unwrap_or_default(), doc.dirty, doc.title.clone()))
                else {
                    self.notify(None, "Open and save a file first.", Severity::Warning, 5);
                    return;
                };
                let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                if !RUNNERS.iter().any(|(ext, _)| *ext == extension) {
                    let kind = if extension.is_empty() { title } else { format!(".{extension}") };
                    self.notify(None, &format!("Don't know how to run '{kind}' files yet."), Severity::Warning, 5);
                } else if dirty {
                    self.save(id, After::Run, false);
                } else {
                    self.run_in_terminal(id);
                }
            }
            Action::About => {
                let text = format!(
                    "Version {} (Rust)\nTheme: {}\nWorkspace: {}",
                    env!("CARGO_PKG_VERSION"),
                    self.theme.label,
                    self.workspace.display()
                );
                self.notify(Some("Terminal Studio Code"), &text, Severity::Info, 8);
            }
            Action::RefreshExplorer => self.explorer.reload(),
            Action::CollapseExplorer => self.explorer.collapse_all(),
            Action::RefreshScm => self.scm.reload(&self.tx),
            Action::OpenMenu(index) => {
                let x = self.lay.menu_x.get(index).copied().unwrap_or(0);
                self.modals.push(Modal::Menu(Menu::new(MENUS[index].1.to_vec(), x, 1, false, Some(index))));
            }
            Action::Activity(item) => match item {
                "claude" => self.run(Action::ToggleClaude),
                "settings" => {
                    let items = vec![
                        item_owned("Command Palette...", "F1", Action::CommandPalette),
                        None,
                        item_owned("Color Theme...", "", Action::SelectTheme),
                    ];
                    let gear = self.lay.gear;
                    self.modals.push(Modal::Menu(Menu::new(items, gear.right() + 1, gear.bottom(), true, None)));
                }
                "account" => self.notify(None, "Accounts aren't built yet.", Severity::Info, 5),
                _ => {
                    let view = match item {
                        "search" => View::Search,
                        "scm" => View::Scm,
                        "debug" => View::Debug,
                        "extensions" => View::Extensions,
                        _ => View::Explorer,
                    };
                    if self.sidebar_visible && self.view == view {
                        self.set_visible("sidebar", false);
                    } else {
                        self.run(Action::ShowView(view));
                    }
                }
            },
            Action::PanelTab(tab) => {
                self.panel_tab = tab;
                if tab == TERMINAL_TAB {
                    self.focus = Focus::Shell;
                } else if self.focus == Focus::Shell {
                    self.focus = Focus::Editor;
                }
            }
            Action::ActivateDoc(id) => self.activate(id, None),
            Action::CloseDoc(id) => self.close_doc(id),
        }
    }
}

fn item_owned(label: &'static str, shortcut: &'static str, action: Action) -> MenuItem {
    Some((label, shortcut, action))
}
