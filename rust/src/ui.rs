//! Painting: lays the window out like VS Code and draws every part into a cell buffer.
//!
//! All colours come from the active `VSTheme`; none are written here as literals.

use crate::app::{Action, App, Focus, Hit, Severity, View, AGENTS, MENUS, PANEL_TABS};
use crate::icons::{file_icon, icon};
use crate::popups::{wrap_text, Modal};
use crate::screen::{ellipsize, width, Buffer, Rect, Rgb, Style, BOLD};
use crate::theme::VSTheme;
use std::rc::Rc;

const ACTIVITY: &[(&str, &str, &str)] = &[
    ("explorer", "files", "Explorer (Ctrl+Shift+E)"),
    ("search", "search", "Search"),
    ("scm", "scm", "Source Control"),
    ("debug", "debug", "Run and Debug"),
    ("extensions", "extensions", "Extensions"),
    ("claude", "claude", "Agent Panel (Ctrl+Alt+B)"),
];

const WELCOME: &[(&str, &str)] = &[
    ("Show All Commands", "F1"),
    ("Go to File", "Ctrl+P"),
    ("Find in Files", "Search icon"),
    ("Toggle Terminal", "Ctrl+`"),
    ("Toggle Claude Code", "Ctrl+Alt+B"),
    ("Focus Next Part", "F6"),
];

impl App {
    fn hovered(&self, rect: Rect) -> bool {
        self.modals.is_empty() && rect.contains(self.mouse.0, self.mouse.1)
    }

    /// The mouse position parts of the window should react to (nowhere while a popup is open).
    fn pointer(&self) -> (i32, i32) {
        if self.modals.is_empty() {
            self.mouse
        } else {
            (-1, -1)
        }
    }

    /// A one-row clickable label with a cell of padding on each side. Returns the x after it.
    #[allow(clippy::too_many_arguments)]
    fn button(&mut self, buf: &mut Buffer, x: i32, y: i32, label: &str, fg: Rgb, hover: (Option<Rgb>, Option<Rgb>), action: Action, tip: Option<&'static str>) -> i32 {
        let rect = Rect::new(x, y, width(label) + 2, 1);
        let mut colour = fg;
        if self.hovered(rect) {
            colour = hover.0.unwrap_or(fg);
            if let Some(bg) = hover.1 {
                buf.restyle(rect, |style| style.bg = bg);
            }
        }
        buf.print(x + 1, y, label, colour, 0, rect.right());
        self.hits.push(Hit { rect, action, tip });
        rect.right()
    }

    /// Paint one frame. Returns the buffer and where the real cursor should sit, if anywhere.
    pub fn draw(&mut self) -> (Buffer, Option<(i32, i32)>) {
        let theme: Rc<VSTheme> = self.theme.clone();
        let t: &VSTheme = &theme;
        let (w, h) = self.size;
        let mut buf = Buffer::new(w, h, Style::new(t.c("foreground"), t.c("editor.background")));
        self.hits.clear();
        self.lay.sidebar = None;
        self.lay.sidebar_split = None;
        self.lay.panel = None;
        self.lay.panel_split = None;
        self.lay.claude = None;
        self.lay.claude_split = None;
        self.lay.editor = None;
        self.lay.explorer = None;
        self.lay.search_input = None;
        self.lay.search_results = None;
        self.lay.scm = None;
        self.lay.shell = None;
        self.lay.claude_term = None;
        let mut cursor = None;
        if w < 20 || h < 6 {
            return (buf, None);
        }

        // -- layout: title bar / [activity | side bar | editor+panel | Claude] / status bar
        let body_h = h - 2;
        let mut left = 5;
        if self.sidebar_visible {
            let sidebar_w = self.sidebar_w.min(w - left - 12).max(8);
            self.lay.sidebar = Some(Rect::new(left, 1, sidebar_w, body_h));
            self.lay.sidebar_split = Some(Rect::new(left + sidebar_w, 1, 1, body_h));
            left += sidebar_w + 1;
        }
        let mut right = w;
        if self.claude_visible {
            let claude_w = self.claude_w.min(w - left - 12).max(8);
            self.lay.claude = Some(Rect::new(w - claude_w, 1, claude_w, body_h));
            self.lay.claude_split = Some(Rect::new(w - claude_w - 1, 1, 1, body_h));
            right = w - claude_w - 1;
        }
        let center = Rect::new(left, 1, right - left, body_h);
        let mut editor_area = center;
        if self.panel_visible {
            let panel_h = self.panel_h.min(body_h - 4).max(3);
            self.lay.panel = Some(Rect::new(center.x, center.bottom() - panel_h, center.w, panel_h));
            self.lay.panel_split = Some(Rect::new(center.x, center.bottom() - panel_h - 1, center.w, 1));
            editor_area = Rect::new(center.x, 1, center.w, body_h - panel_h - 1);
        }

        self.draw_titlebar(&mut buf, t);
        self.draw_activity(&mut buf, t, Rect::new(0, 1, 5, body_h));
        if let Some(area) = self.lay.sidebar {
            cursor = cursor.or(self.draw_sidebar(&mut buf, t, area));
        }
        cursor = cursor.or(self.draw_editor_area(&mut buf, t, editor_area));
        if let Some(area) = self.lay.panel {
            cursor = cursor.or(self.draw_panel(&mut buf, t, area));
        }
        if let Some(area) = self.lay.claude {
            cursor = cursor.or(self.draw_claude(&mut buf, t, area));
        }
        self.draw_splitters(&mut buf, t);
        self.draw_statusbar(&mut buf, t);

        // -- the suggest widget hangs under the word being completed, over everything else
        match self.suggest_anchor() {
            Some(anchor) => {
                let mouse = self.pointer();
                if let Some(suggest) = self.suggest.as_mut() {
                    suggest.draw(&mut buf, t, anchor, Rect::new(0, 1, w, h - 2), mouse);
                }
            }
            None => self.suggest = None,
        }

        // -- popups sit on top and take over the mouse and the cursor
        if !self.modals.is_empty() {
            self.hits.clear();
            cursor = None;
        }
        let mouse = self.mouse;
        for modal in &mut self.modals {
            match modal {
                Modal::Menu(menu) => menu.draw(&mut buf, t),
                Modal::Choice(choice) => choice.draw(&mut buf, t, mouse),
                Modal::Input(input) => cursor = Some(input.draw(&mut buf, t)),
                Modal::Pick(pick) => pick.draw(&mut buf, t, mouse),
                Modal::Palette(palette) => cursor = Some(palette.draw(&mut buf, t, mouse)),
            }
        }
        self.draw_toasts(&mut buf, t);
        self.draw_tooltip(&mut buf, t);

        // Line mode shows the terminal's real cursor as a thin bar; block mode paints a cell.
        if let Some((x, y)) = cursor {
            if !self.cursor_line {
                if let Some(cell) = buf.get(x, y) {
                    let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
                    buf.put(x, y, ch, Style::new(cell.st.bg, t.c("editorCursor.foreground")));
                }
                cursor = None;
            }
        }
        (buf, cursor)
    }

    // -- title bar --------------------------------------------------------------------

    fn draw_titlebar(&mut self, buf: &mut Buffer, t: &VSTheme) {
        let w = self.size.0;
        let fg = t.c("titleBar.activeForeground");
        let select = t.c("list.activeSelectionBackground");
        buf.fill(Rect::new(0, 0, w, 1), Style::new(fg, t.c("titleBar.activeBackground")));
        let mut x = buf.print(1, 0, icon("logo"), t.c("logo.foreground"), 0, w) + 1;
        let open_menu = match self.modals.last() {
            Some(Modal::Menu(menu)) => menu.owner,
            _ => None,
        };
        self.lay.menu_x.clear();
        for (index, (name, _)) in MENUS.iter().enumerate() {
            self.lay.menu_x.push(x);
            if open_menu == Some(index) {
                buf.restyle(Rect::new(x, 0, width(name) + 2, 1), |style| style.bg = select);
            }
            x = self.button(buf, x, 0, name, fg, (None, Some(select)), Action::OpenMenu(index), None);
        }
        let menus_end = x;

        // Layout toggles and close, right-aligned.
        let buttons: [(&str, Action, &'static str); 4] = [
            ("layout_left", Action::ToggleSidebar, "Toggle Primary Side Bar (Ctrl+B)"),
            ("layout_panel", Action::TogglePanel, "Toggle Panel (Ctrl+J)"),
            ("layout_right", Action::ToggleClaude, "Toggle Agent Panel (Ctrl+Alt+B)"),
            ("win_close", Action::Quit, "Close"),
        ];
        let buttons_w: i32 = buttons.iter().map(|(name, _, _)| width(icon(name)) + 2).sum();
        // The agent picker sits left of them: the running agent's name, or "Agents".
        let agent = if self.claude.is_some() { AGENTS[self.agent].0 } else { "Agents" };
        let agent = format!("{} {} {}", icon("claude"), agent, icon("chevron_down"));
        let mut bx = (w - buttons_w - width(&agent) - 3).max(menus_end);
        let buttons_start = bx;
        self.lay.agent_button = Rect::new(bx, 0, width(&agent) + 2, 1);
        bx = self.button(buf, bx, 0, &agent, fg, (None, Some(select)), Action::AgentMenu, Some("Start a coding agent in the side panel")) + 1;
        for (name, action, tip) in buttons {
            let hover = if name == "win_close" {
                (Some(t.c("button.foreground")), Some(t.c("windowClose.hoverBackground")))
            } else {
                (None, Some(select))
            };
            bx = self.button(buf, bx, 0, icon(name), fg, hover, action, Some(tip));
        }

        // The command center: a pill in the middle of whatever room is left.
        let arrows = format!("{}  {} ", icon("arrow_left"), icon("arrow_right"));
        let caps = width(icon("cap_left")) + width(icon("cap_right"));
        let room = buttons_start - menus_end - width(&arrows) - caps;
        if room < 14 {
            return;
        }
        let pill_w = (room / 2).min(42);
        let mut x = menus_end + (room - pill_w) / 2;
        x = buf.print(x, 0, &arrows, t.c("disabledForeground"), 0, w);
        let pill_bg = t.c("input.background");
        x = buf.print(x, 0, icon("cap_left"), pill_bg, 0, w);
        let pill = Rect::new(x, 0, pill_w, 1);
        let pill_fg = if self.hovered(pill) { t.c("foreground") } else { t.c("commandCenter.foreground") };
        buf.fill(pill, Style::new(pill_fg, pill_bg));
        let name = self.workspace.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let label = ellipsize(&format!("{} {}", icon("search"), name), pill_w - 2);
        buf.print(x + (pill_w - width(&label)) / 2, 0, &label, pill_fg, 0, pill.right());
        self.hits.push(Hit { rect: pill, action: Action::QuickOpen, tip: Some("Search files (Ctrl+P)") });
        buf.print(pill.right(), 0, icon("cap_right"), pill_bg, 0, w);
    }

    // -- activity bar -----------------------------------------------------------------

    fn draw_activity(&mut self, buf: &mut Buffer, t: &VSTheme, area: Rect) {
        let bg = t.c("activityBar.background");
        buf.fill(area, Style::new(t.c("activityBar.inactiveForeground"), bg));
        for y in area.y..area.bottom() {
            buf.put(area.x + 4, y, '│', Style::new(t.c("border"), bg));
        }
        let top_end = area.y + ACTIVITY.len() as i32 * 3;
        let bottom = [("account", "account", "Accounts"), ("settings", "gear", "Manage")];
        let bottom_start = (area.bottom() - bottom.len() as i32 * 3).max(top_end);
        let items = ACTIVITY
            .iter()
            .enumerate()
            .map(|(i, item)| (area.y + i as i32 * 3, *item))
            .chain(bottom.iter().enumerate().map(|(i, item)| (bottom_start + i as i32 * 3, *item)));
        let current = match self.view {
            View::Explorer => "explorer",
            View::Search => "search",
            View::Scm => "scm",
            View::Debug => "debug",
            View::Extensions => "extensions",
        };
        for (y, (id, glyph, tip)) in items.collect::<Vec<_>>() {
            if y + 3 > area.bottom() {
                continue;
            }
            let rect = Rect::new(area.x, y, 4, 3);
            let active = (self.sidebar_visible && id == current) || (id == "claude" && self.claude_visible);
            let fg = if id == "claude" {
                t.c("claude.accent")
            } else if active || self.hovered(rect) {
                t.c("activityBar.foreground")
            } else {
                t.c("activityBar.inactiveForeground")
            };
            if active {
                for row in y..y + 3 {
                    buf.put(area.x, row, '▎', Style::new(t.c("activityBar.activeBorder"), bg));
                }
            }
            buf.print(area.x + 2, y + 1, icon(glyph), fg, 0, area.x + 4);
            if id == "settings" {
                self.lay.gear = rect;
            }
            self.hits.push(Hit { rect, action: Action::Activity(id), tip: Some(tip) });
        }
    }

    // -- side bar ---------------------------------------------------------------------

    fn sidebar_title(&self, buf: &mut Buffer, t: &VSTheme, area: Rect, title: &str) {
        buf.print(area.x + 2, area.y, title, t.c("sideBarTitle.foreground"), 0, area.right() - 1);
    }

    /// A paragraph of quiet text. Returns the row after it (including one blank row).
    fn note(&self, buf: &mut Buffer, t: &VSTheme, x: i32, y: i32, right: i32, bottom: i32, text: &str) -> i32 {
        let mut row = y;
        for line in wrap_text(text, right - x) {
            if row >= bottom {
                break;
            }
            buf.print(x, row, &line, t.c("descriptionForeground"), 0, right);
            row += 1;
        }
        row + 1
    }

    fn draw_sidebar(&mut self, buf: &mut Buffer, t: &VSTheme, area: Rect) -> Option<(i32, i32)> {
        let fg = t.c("sideBar.foreground");
        let quiet = t.c("descriptionForeground");
        let hover = (Some(t.c("foreground")), Some(t.c("list.activeSelectionBackground")));
        buf.fill(area, Style::new(fg, t.c("sideBar.background")));
        let pointer = self.pointer();
        let (right, bottom) = (area.right(), area.bottom());
        match self.view {
            View::Explorer => {
                self.sidebar_title(buf, t, area, "EXPLORER");
                buf.print(right - 3, area.y, icon("ellipsis"), quiet, 0, right - 1);
                // Section header: the workspace name and the tree's buttons.
                let y = area.y + 2;
                buf.fill(Rect::new(area.x, y, area.w, 1), Style::new(fg, t.c("sideBarSectionHeader.background")));
                let actions: [(&str, Action, &'static str); 4] = [
                    ("new_file", Action::NewFileIn, "New File..."),
                    ("new_folder", Action::NewFolder, "New Folder..."),
                    ("refresh", Action::RefreshExplorer, "Refresh Explorer"),
                    ("collapse_all", Action::CollapseExplorer, "Collapse Folders in Explorer"),
                ];
                let actions_w: i32 = actions.iter().map(|(name, _, _)| width(icon(name)) + 2).sum();
                let name = self.workspace.file_name().map(|n| n.to_string_lossy().to_uppercase()).unwrap_or_default();
                let title = ellipsize(&format!("{} {}", icon("chevron_down"), name), area.w - actions_w - 2);
                buf.print(area.x + 1, y, &title, fg, BOLD, right - actions_w);
                let mut x = right - actions_w;
                for (name, action, tip) in actions {
                    x = self.button(buf, x, y, icon(name), quiet, hover, action, Some(tip));
                }
                let tree = Rect::new(area.x, y + 1, area.w, bottom - y - 1);
                self.lay.explorer = Some(tree);
                self.explorer.draw(buf, tree, t, self.focus == Focus::Explorer, pointer);
                None
            }
            View::Search => {
                self.sidebar_title(buf, t, area, "SEARCH");
                let field = Rect::new(area.x + 2, area.y + 2, area.w - 4, 1);
                self.lay.search_input = Some(field);
                let cursor = self.search.input.draw(buf, field, t, t.c("input.background"), "Search");
                let y = self.note(buf, t, area.x + 2, area.y + 4, right - 2, bottom, &self.search.summary.clone());
                let list = Rect::new(area.x, y, area.w, bottom - y);
                self.lay.search_results = Some(list);
                self.search.draw(buf, list, t, self.focus == Focus::SearchResults, pointer);
                (self.focus == Focus::SearchInput).then_some(cursor)
            }
            View::Scm => {
                self.sidebar_title(buf, t, area, "SOURCE CONTROL");
                let x = right - 1 - (width(icon("refresh")) + 2);
                self.button(buf, x, area.y, icon("refresh"), quiet, hover, Action::RefreshScm, Some("Refresh"));
                let y = self.note(buf, t, area.x + 2, area.y + 2, right - 2, bottom, &self.scm.summary.clone());
                let list = Rect::new(area.x, y, area.w, bottom - y);
                self.lay.scm = Some(list);
                self.scm.draw(buf, list, t, self.focus == Focus::Scm, pointer);
                None
            }
            View::Debug | View::Extensions => {
                let (title, text) = if self.view == View::Debug {
                    ("RUN AND DEBUG", "Debugging isn't built yet.\n\nTo run the open file, choose Run > Run Active File in Terminal.")
                } else {
                    ("EXTENSIONS", "Extensions aren't built yet.\n\nClaude Code is already included: open it from the ✻ icon.")
                };
                self.sidebar_title(buf, t, area, title);
                self.note(buf, t, area.x + 2, area.y + 2, right - 2, bottom, text);
                None
            }
        }
    }

    // -- editor group -----------------------------------------------------------------

    fn draw_editor_area(&mut self, buf: &mut Buffer, t: &VSTheme, area: Rect) -> Option<(i32, i32)> {
        let bg = t.c("editor.background");
        buf.fill(area, Style::new(t.c("foreground"), bg));
        if self.docs.is_empty() || area.h < 4 {
            self.draw_welcome(buf, t, area);
            return None;
        }
        self.draw_tabs(buf, t, Rect::new(area.x, area.y, area.w, 2));

        // Breadcrumbs: the path of the active file.
        let Some(index) = self.docs.iter().position(|doc| Some(doc.id) == self.active) else { return None };
        let crumbs = match &self.docs[index].path {
            Some(path) => {
                let shown = path.strip_prefix(&self.workspace).unwrap_or(path);
                let parts: Vec<String> = shown.iter().map(|part| part.to_string_lossy().to_string()).collect();
                parts.join(&format!(" {} ", icon("sep")))
            }
            None => self.docs[index].title.clone(),
        };
        let crumbs = ellipsize(&crumbs, area.w - 4);
        buf.print(area.x + 2, area.y + 2, &crumbs, t.c("breadcrumb.foreground"), 0, area.right() - 2);

        let view = Rect::new(area.x, area.y + 3, area.w, area.h - 3);
        let focused = self.focus == Focus::Editor && self.modals.is_empty();
        let line_cursor = self.cursor_line;
        let doc = &mut self.docs[index];
        if let Some(reason) = &doc.not_editable {
            let mut y = view.y + 2;
            for line in wrap_text(reason, view.w - 8) {
                buf.print(view.x + 4, y, &line, t.c("descriptionForeground"), 0, view.right() - 4);
                y += 1;
            }
            return None;
        }
        self.lay.editor = Some(view);
        doc.draw(buf, view, t, focused, line_cursor)
    }

    /// Two rows: a thin accent line over the active tab, then the tab labels.
    fn draw_tabs(&mut self, buf: &mut Buffer, t: &VSTheme, area: Rect) {
        let strip = t.c("editorGroupHeader.tabsBackground");
        buf.fill(area, Style::new(t.c("tab.inactiveForeground"), strip));
        struct Tab {
            id: u64,
            width: i32,
            active: bool,
            glyph: &'static str,
            glyph_fg: Rgb,
            title: String,
            mark: &'static str,
            fg: Rgb,
            bg: Rgb,
            mark_fg: Rgb,
        }
        let tabs: Vec<Tab> = self
            .docs
            .iter()
            .map(|doc| {
                let active = Some(doc.id) == self.active;
                let bg = t.c(if active { "tab.activeBackground" } else { "tab.inactiveBackground" });
                let mut fg = t.c(if active { "tab.activeForeground" } else { "tab.inactiveForeground" });
                if doc.error_count() > 0 {
                    fg = t.c("list.errorForeground");
                } else if doc.warning_count() > 0 {
                    fg = t.c("list.warningForeground");
                }
                let fallback = std::path::PathBuf::from(&doc.title);
                let (glyph, glyph_fg) = file_icon(doc.path.as_deref().unwrap_or(&fallback), t);
                let mark = icon(if doc.dirty { "dot" } else { "close" });
                let hovering = self.modals.is_empty() && self.mouse.1 >= area.y && self.mouse.1 < area.bottom();
                Tab {
                    id: doc.id,
                    width: 1 + width(glyph) + 1 + width(&doc.title) + 2 + width(mark) + 1,
                    active,
                    glyph,
                    glyph_fg,
                    title: doc.title.clone(),
                    mark,
                    fg,
                    bg,
                    // The close mark shows on the active tab, on unsaved ones, and while pointing at the strip.
                    mark_fg: if active || doc.dirty || hovering { fg } else { bg },
                }
            })
            .collect();
        // Slide the strip so the active tab is always in view.
        let mut offset = 0;
        let mut x = 0;
        for tab in &tabs {
            if tab.active && x + tab.width > area.w {
                offset = x + tab.width + 1 - area.w;
            }
            x += tab.width + 1;
        }
        let mut x = area.x - offset;
        for tab in tabs {
            let (start, end) = (x, x + tab.width);
            if end > area.x && start < area.right() {
                let rect = Rect::new(start.max(area.x), area.y, end.min(area.right()) - start.max(area.x), 2);
                buf.fill(rect, Style::new(tab.fg, tab.bg));
                if tab.active {
                    for cx in rect.x..rect.right() {
                        buf.put(cx, area.y, '▔', Style::new(t.c("tab.activeBorderTop"), tab.bg));
                    }
                }
                let y = area.y + 1;
                let mut cx = buf.print(start + 1, y, tab.glyph, tab.glyph_fg, 0, area.right()) + 1;
                cx = buf.print(cx, y, &tab.title, tab.fg, 0, area.right()) + 2;
                buf.print(cx, y, tab.mark, tab.mark_fg, 0, area.right());
                buf.print(end, y, "│", t.c("border"), 0, area.right());
                self.hits.push(Hit { rect, action: Action::ActivateDoc(tab.id), tip: None });
                let close = Rect::new(end - 3, area.y, 3, 2);
                if close.x >= area.x {
                    self.hits.push(Hit { rect: close, action: Action::CloseDoc(tab.id), tip: None });
                }
            }
            x = end + 1;
        }
    }

    /// The shortcut watermark VS Code shows when no editor is open.
    fn draw_welcome(&self, buf: &mut Buffer, t: &VSTheme, area: Rect) {
        let fg = t.c("disabledForeground");
        let rows = 5 + WELCOME.len() as i32 * 2;
        let mut y = area.y + ((area.h - rows) / 2).max(0);
        let centre = |text: &str| area.x + (area.w - width(text)) / 2;
        let logo = icon("logo");
        let name = "Terminal Studio Code";
        let lines: [(i32, &str, u8); 2] = [(0, logo, BOLD), (2, name, 0)];
        for (offset, text, attr) in lines {
            if y + offset < area.bottom() {
                buf.print(centre(text).max(area.x), y + offset, text, fg, attr, area.right());
            }
        }
        y += 5;
        for (label, keys) in WELCOME {
            if y >= area.bottom() {
                break;
            }
            let x = (area.x + (area.w - 37) / 2).max(area.x);
            let x = buf.print(x, y, &format!("{label:>20}   "), fg, 0, area.right());
            buf.print(x, y, keys, fg, BOLD, area.right());
            y += 2;
        }
    }

    // -- bottom panel -----------------------------------------------------------------

    fn draw_panel(&mut self, buf: &mut Buffer, t: &VSTheme, area: Rect) -> Option<(i32, i32)> {
        let bg = t.c("panel.background");
        let quiet = t.c("descriptionForeground");
        buf.fill(area, Style::new(t.c("foreground"), bg));

        // Like VS Code, drop what doesn't fit: first the shell name, then inactive tabs.
        let room = area.w - 2;
        let needed: i32 = PANEL_TABS.iter().map(|(label, _)| width(label) + 2).sum();
        let shell_label = format!("{} {}", icon("terminal"), self.shell_name);
        let shell_w = width(&shell_label) + 2;
        let show_shell = room >= needed + 6 + shell_w;
        let mut budget = room - 6 - if show_shell { shell_w } else { 0 } - (width(PANEL_TABS[self.panel_tab].0) + 2);
        let mut x = area.x + 2;
        for (index, (label, _)) in PANEL_TABS.iter().enumerate() {
            let active = index == self.panel_tab;
            let cost = width(label) + 2;
            if !active {
                if budget < cost {
                    continue;
                }
                budget -= cost;
            }
            let rect = Rect::new(x, area.y, width(label), 2);
            let lit = active || self.hovered(rect);
            buf.print(x, area.y, label, t.c(if lit { "panelTitle.activeForeground" } else { "panelTitle.inactiveForeground" }), 0, area.right());
            if active {
                // A thin line hugging the label: VS Code's active-tab underline.
                for cx in x..rect.right().min(area.right()) {
                    buf.put(cx, area.y + 1, '▔', Style::new(t.c("panelTitle.activeBorder"), bg));
                }
            }
            self.hits.push(Hit { rect, action: Action::PanelTab(index), tip: None });
            x = rect.right() + 2;
        }
        let hover = (Some(t.c("foreground")), Some(t.c("list.activeSelectionBackground")));
        let close_x = area.right() - (width(icon("close")) + 2);
        let trash_x = close_x - (width(icon("trash")) + 2);
        if trash_x > x {
            self.button(buf, trash_x, area.y, icon("trash"), quiet, hover, Action::RestartTerminal, Some("Kill Terminal and start a new one"));
            self.button(buf, close_x, area.y, icon("close"), quiet, hover, Action::TogglePanel, Some("Hide Panel (Ctrl+J)"));
            if show_shell {
                buf.print(trash_x - shell_w + 1, area.y, &shell_label, quiet, 0, trash_x);
            }
        }

        let view = Rect::new(area.x + 2, area.y + 2, area.w - 2, area.h - 2);
        match PANEL_TABS[self.panel_tab].1 {
            Some(text) => {
                self.note(buf, t, view.x, view.y, view.right(), view.bottom(), text);
                None
            }
            None => {
                self.lay.shell = Some(view);
                self.shell.resize(view.w, view.h);
                let focused = self.focus == Focus::Shell && self.modals.is_empty();
                self.shell.draw(buf, view, t, focused, self.cursor_line)
            }
        }
    }

    // -- Claude Code panel ------------------------------------------------------------

    fn draw_claude(&mut self, buf: &mut Buffer, t: &VSTheme, area: Rect) -> Option<(i32, i32)> {
        let quiet = t.c("descriptionForeground");
        buf.fill(area, Style::new(t.c("sideBar.foreground"), t.c("sideBar.background")));
        let x = buf.print(area.x + 2, area.y, icon("claude"), t.c("claude.accent"), 0, area.right());
        let title = if self.claude.is_some() { AGENTS[self.agent].0.to_uppercase() } else { "AGENT".to_string() };
        buf.print(x + 1, area.y, &title, t.c("sideBarTitle.foreground"), 0, area.right() - 1);
        let hover = (Some(t.c("foreground")), Some(t.c("list.activeSelectionBackground")));
        let close_x = area.right() - 1 - (width(icon("close")) + 2);
        let add_x = close_x - (width(icon("add")) + 2);
        if add_x > x + 12 {
            self.button(buf, add_x, area.y, icon("add"), quiet, hover, Action::RestartClaude, Some("New Conversation (restarts the agent)"));
            self.button(buf, close_x, area.y, icon("close"), quiet, hover, Action::ToggleClaude, Some("Hide Agent Panel (Ctrl+Alt+B)"));
        }
        let view = Rect::new(area.x + 1, area.y + 2, area.w - 1, area.h - 2);
        let focused = self.focus == Focus::Claude && self.modals.is_empty();
        let line_cursor = self.cursor_line;
        match &mut self.claude {
            Some(claude) => {
                self.lay.claude_term = Some(view);
                claude.resize(view.w, view.h);
                claude.draw(buf, view, t, focused, line_cursor)
            }
            None => {
                let text = "No agent is running.\n\n\
                            Pick one (Claude Code, Codex, Grok, Antigravity...) from the Agents menu in the top-right corner.";
                self.note(buf, t, area.x + 2, area.y + 2, area.right() - 2, area.bottom(), text);
                None
            }
        }
    }

    // -- splitters --------------------------------------------------------------------

    fn draw_splitters(&mut self, buf: &mut Buffer, t: &VSTheme) {
        use crate::app::Drag;
        let splitters = [
            (self.lay.sidebar_split, Drag::Sidebar, '│', "sideBar.background"),
            (self.lay.claude_split, Drag::Claude, '│', "sideBar.background"),
            (self.lay.panel_split, Drag::Panel, '─', "panel.background"),
        ];
        for (rect, drag, ch, bg) in splitters {
            let Some(rect) = rect else { continue };
            let lit = self.drag == Some(drag) || (self.drag.is_none() && self.hovered(rect));
            let style = Style::new(t.c(if lit { "focusBorder" } else { "border" }), t.c(bg));
            for y in rect.y..rect.bottom() {
                for x in rect.x..rect.right() {
                    buf.put(x, y, ch, style);
                }
            }
        }
    }

    // -- status bar -------------------------------------------------------------------

    fn draw_statusbar(&mut self, buf: &mut Buffer, t: &VSTheme) {
        let (w, h) = self.size;
        let y = h - 1;
        let fg = t.c("statusBar.foreground");
        let hover = (None, Some(t.c("statusBarItem.hoverBackground")));
        buf.fill(Rect::new(0, y, w, 1), Style::new(fg, t.c("statusBar.background")));
        let remote = format!(" {} ", icon("remote"));
        buf.fill(Rect::new(0, y, width(&remote), 1), Style::new(fg, t.c("statusBarItem.remoteBackground")));
        let mut x = buf.print(0, y, &remote, t.c("statusBarItem.remoteForeground"), 0, w);
        if let Some(branch) = self.branch.clone() {
            let label = format!("{} {}", icon("branch"), branch);
            x = self.button(buf, x, y, &label, fg, hover, Action::ShowView(View::Scm), Some("Source Control"));
        }
        let errors: usize = self.docs.iter().map(|doc| doc.error_count()).sum();
        let warnings: usize = self.docs.iter().map(|doc| doc.warning_count()).sum();
        let problems = format!("{} {} {} {}", icon("error"), errors, icon("warning"), warnings);
        let left_end = buf.print(x + 1, y, &problems, fg, 0, w) + 1;

        // Right side, laid out from the edge inwards.
        let mut items: Vec<(String, Option<Action>, Option<Rgb>)> = Vec::new();
        if let Some(doc) = self.active_doc() {
            if doc.is_text() {
                let selected = doc.selected_len();
                let mut position = format!("Ln {}, Col {}", doc.cursor.row + 1, doc.cursor.col + 1);
                if selected > 0 {
                    position += &format!(" ({selected} selected)");
                }
                items.extend([position, "Spaces: 4".into(), "UTF-8".into(), "LF".into()].map(|text| (text, None, None)));
            }
            items.push((doc.language_name.to_string(), None, None));
        }
        items.push((format!("{} Claude", icon("claude")), Some(Action::ToggleClaude), Some(t.c("claude.accent"))));
        items.push((icon("bell").to_string(), None, None));
        let total: i32 = items.iter().map(|(text, _, _)| width(text) + 2).sum();
        let mut x = w - total;
        for (text, action, accent) in items {
            let item_w = width(&text) + 2;
            if x < left_end {
                x += item_w;
                continue;
            }
            match action {
                Some(action) => {
                    let end = self.button(buf, x, y, &text, fg, hover, action, Some("Toggle Claude Code (Ctrl+Alt+B)"));
                    if let Some(accent) = accent {
                        buf.print(x + 1, y, icon("claude"), accent, 0, end);
                    }
                }
                None => {
                    let rect = Rect::new(x, y, item_w, 1);
                    if self.hovered(rect) {
                        if let Some(bg) = hover.1 {
                            buf.restyle(rect, |style| style.bg = bg);
                        }
                    }
                    buf.print(x + 1, y, &text, fg, 0, rect.right());
                }
            }
            x += item_w;
        }
    }

    // -- notifications and tooltips ---------------------------------------------------

    fn draw_toasts(&self, buf: &mut Buffer, t: &VSTheme) {
        let (w, h) = self.size;
        let box_w = 48.min(w - 4);
        let mut bottom = h - 2;
        for toast in self.toasts.iter().rev() {
            let mut lines = wrap_text(&toast.text, box_w - 4);
            let titled = toast.title.is_some();
            let box_h = lines.len() as i32 + 2 + i32::from(titled);
            let rect = Rect::new(w - box_w - 2, bottom - box_h, box_w, box_h);
            if rect.y < 1 {
                break;
            }
            buf.fill(rect, Style::new(t.c("foreground"), t.c("menu.background")));
            let accent = match toast.severity {
                Severity::Info => t.c("focusBorder"),
                Severity::Warning => t.c("editorWarning.foreground"),
                Severity::Error => t.c("editorError.foreground"),
            };
            for y in rect.y..rect.bottom() {
                buf.print(rect.x, y, "▎", accent, 0, rect.x + 1);
            }
            let mut y = rect.y + 1;
            if let Some(title) = &toast.title {
                buf.print(rect.x + 2, y, title, t.c("foreground"), BOLD, rect.right() - 1);
                y += 1;
            }
            for line in lines.drain(..) {
                buf.print(rect.x + 2, y, &line, t.c("foreground"), 0, rect.right() - 1);
                y += 1;
            }
            bottom = rect.y - 1;
        }
    }

    fn draw_tooltip(&self, buf: &mut Buffer, t: &VSTheme) {
        let Some(text) = &self.tooltip else { return };
        let (w, h) = self.size;
        let lines = wrap_text(text, (w - 6).min(60));
        let box_w = lines.iter().map(|line| width(line)).max().unwrap_or(0) + 2;
        let box_h = lines.len() as i32;
        let x = (self.mouse.0 + 1).min(w - box_w).max(0);
        let below = self.mouse.1 + 1;
        let y = if below + box_h <= h - 1 { below } else { (self.mouse.1 - box_h).max(0) };
        buf.fill(Rect::new(x, y, box_w, box_h), Style::new(t.c("foreground"), t.c("input.background")));
        for (row, line) in lines.iter().enumerate() {
            buf.print(x + 1, y + row as i32, line, t.c("foreground"), 0, x + box_w);
        }
    }
}
