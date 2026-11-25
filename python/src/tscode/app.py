"""The application shell: layout, key bindings and actions."""

from __future__ import annotations

import shlex
import shutil
import subprocess
from importlib.metadata import version
from pathlib import Path

from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.command import CommandPalette
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widgets import Tree

from .commands import CommandProvider, FileProvider
from .settings import cursor_is_line, load_settings, save_setting, spotify_enabled
from .theme import available_themes, load_theme, terminal_theme_stamp
from .widgets.activitybar import ActivityBar
from .widgets.common import ChoiceModal, InputModal, MenuScreen, QuickPick, Splitter
from .widgets.editor import CodeEditor, EditorArea
from .widgets.panel import AGENTS, ClaudePanel, Panel, agent_command
from .widgets.spotify import SpotifyPlayer
from .widgets.sidebar import ExplorerTree, OpenFile, Sidebar, SourceControlView
from .widgets.statusbar import StatusBar
from .widgets.suggest import SuggestWidget
from .widgets.terminal import Terminal
from .widgets.titlebar import TitleBar, agent_label

RUNNERS = {".py": "python3", ".sh": "bash", ".bash": "bash", ".js": "node", ".mjs": "node"}

# Keys a focused terminal needs for itself (readline, Claude Code, vim...). The
# matching app shortcuts are switched off while a terminal has focus.
TERMINAL_OWNED = {"save", "close_editor", "quick_open", "toggle_sidebar", "toggle_panel", "new_file",
                  "trigger_suggest"}
KEYBOARD_ACTIONS = TERMINAL_OWNED | {
    "command_palette", "show_view", "select_theme", "toggle_claude", "focus_terminal", "save_all",
    "focus_next_part", "focus_previous_part", "next_editor", "previous_editor",
}


class TerminalStudioCode(App):
    CSS_PATH = "app.tcss"
    TITLE = "Terminal Studio Code"
    COMMANDS = {CommandProvider}
    COMMAND_PALETTE_BINDING = "f1"

    BINDINGS = [
        Binding("ctrl+q", "quit", "Exit", priority=True),
        Binding("ctrl+s", "save", "Save", priority=True),
        Binding("ctrl+w", "close_editor", "Close Editor", priority=True),
        Binding("ctrl+p", "quick_open", "Go to File", priority=True),
        Binding("ctrl+n", "new_file", "New File", priority=True),
        Binding("ctrl+b", "toggle_sidebar", "Toggle Side Bar", priority=True),
        Binding("ctrl+j", "toggle_panel", "Toggle Panel", priority=True),
        Binding("ctrl+alt+b", "toggle_claude", "Toggle Claude Code", priority=True),
        Binding("ctrl+alt+s", "save_all", "Save All", priority=True),
        # Textual only adds its own palette key when no binding names the action, so list F1 here.
        Binding("f1", "command_palette", "Command Palette", priority=True),
        Binding("ctrl+shift+p", "command_palette", "Command Palette", priority=True),
        Binding("ctrl+shift+e", "show_view('explorer')", "Explorer", priority=True),
        Binding("ctrl+grave_accent", "focus_terminal", "Terminal", priority=True),
        Binding("f6", "focus_next_part", "Focus Next Part", priority=True),
        Binding("shift+f6", "focus_previous_part", "Focus Previous Part", priority=True),
        Binding("ctrl+pagedown", "next_editor", "Next Editor", priority=True),
        Binding("ctrl+pageup", "previous_editor", "Previous Editor", priority=True),
        # Ctrl+Space arrives as NUL (ctrl+@) in terminals without the kitty keyboard protocol.
        Binding("ctrl+space,ctrl+@", "trigger_suggest", "Trigger Suggest", priority=True),
    ]

    def __init__(self, workspace: Path, open_files: list[Path] | None = None,
                 theme: str | None = None) -> None:
        # `theme` is a theme id; default is the saved choice, then Dark Modern.
        self.vs_theme = load_theme(theme or load_settings().get("theme"))
        self.workspace = workspace.resolve()
        self._open_on_start = open_files or []
        self._terminal_stamp = terminal_theme_stamp()
        self.cursor_line = cursor_is_line()  # thin line cursor (default) or block
        self._hw_cursor = False
        self.spotify_enabled = spotify_enabled()  # shelved feature, off by default
        self._menu_path: Path | None = None  # the path a context menu was opened on
        super().__init__()
        self.register_theme(self.vs_theme.textual_theme())
        self.theme = self.vs_theme.name

    def get_css_variables(self) -> dict[str, str]:
        return {**super().get_css_variables(), **self.vs_theme.css_variables()}

    # -- layout ---------------------------------------------------------------

    def compose(self) -> ComposeResult:
        yield TitleBar(self.workspace.name)
        with Horizontal(id="body"):
            yield ActivityBar()
            yield Sidebar(self.workspace)
            yield Splitter("#sidebar", "right", 16, id="sidebar-splitter")
            with Vertical(id="center"):
                yield EditorArea(self.workspace, self.vs_theme)
                yield Splitter("#panel", "top", 4, id="panel-splitter")
                yield Panel(self.workspace, self.vs_theme)
            yield Splitter("#claude-panel", "left", 30, id="claude-splitter")
            yield ClaudePanel(self.workspace, self.vs_theme)
        yield StatusBar(self.workspace)
        self.suggest_widget = SuggestWidget()  # code completion list (overlay)
        yield self.suggest_widget

    def on_mount(self) -> None:
        # No agent starts by itself: pick one from the Agents menu in the title bar.
        self._set_visible("claude", False)
        for player in self.query(SpotifyPlayer):
            player.display = load_settings().get("spotify", True)
        for path in self._open_on_start:
            self.open_file(path)
        if not self._open_on_start:
            self.query_one(ExplorerTree).focus()
        self.set_interval(3.0, self._follow_terminal_theme)
        self.set_interval(0.1, self._sync_cursor)

    # -- cursor ---------------------------------------------------------------
    # Terminals can't draw a thin bar inside a cell, but their own cursor can be one.
    # In line mode we stop painting block cursors and show the real terminal cursor
    # (as a bar) wherever the focused editor/terminal has its insertion point. Textual
    # already parks the real cursor at `app.cursor_position` after every repaint.

    def _sync_cursor(self) -> None:
        line = cursor_is_line()
        if line != self.cursor_line:  # toggled system-wide (Super+\)
            self.cursor_line = line
            self.editors.apply_theme(self.vs_theme)
            for terminal in self.query(Terminal):
                terminal.refresh()
        focused = self.focused
        show = False
        if self.cursor_line and isinstance(focused, CodeEditor):
            show = True
        elif self.cursor_line and isinstance(focused, Terminal):
            offset = focused.cursor_screen_offset
            if offset is not None:
                show = True
                if self.cursor_position != offset:
                    self.cursor_position = offset
                    focused.refresh()  # a repaint is what moves the real cursor
        if show != self._hw_cursor:
            self._hw_cursor = show
            self._write_terminal("\x1b[6 q\x1b[?25h" if show else "\x1b[?25l")

    def _write_terminal(self, sequence: str) -> None:
        if self._driver is not None and not self.is_headless:
            self._driver.write(sequence)
            self._driver.flush()

    def exit(self, *args, **kwargs) -> None:
        self._write_terminal("\x1b[?25l\x1b[0 q")  # give the terminal its own cursor shape back
        super().exit(*args, **kwargs)

    # -- themes ---------------------------------------------------------------

    def apply_theme(self, theme_id: str, persist: bool = False) -> None:
        """Switch every part of the UI to another theme, live."""
        theme = load_theme(theme_id)
        self.vs_theme = theme
        self.register_theme(theme.textual_theme())
        self.theme = theme.name  # re-reads get_css_variables() and restyles the app
        self.editors.apply_theme(theme)
        for terminal in self.query(Terminal):
            terminal.vs_theme = theme
            terminal.refresh()
        for tree in self.query(Tree):
            tree._invalidate()  # labels carry icon colours
        if persist:
            save_setting("theme", theme.id)

    def _follow_terminal_theme(self) -> None:
        """Re-skin when the desktop theme changes underneath the "terminal" theme."""
        stamp = terminal_theme_stamp()
        if stamp != self._terminal_stamp:
            self._terminal_stamp = stamp
            if self.vs_theme.id == "terminal":
                self.apply_theme("terminal")

    def action_select_theme(self) -> None:
        original = self.vs_theme.id

        def preview(theme_id: str) -> None:
            if theme_id != self.vs_theme.id:
                self.apply_theme(theme_id)

        def chosen(theme_id: str | None) -> None:
            self.apply_theme(theme_id or original, persist=theme_id is not None)

        self.push_screen(
            QuickPick("Select Color Theme (Up/Down to preview, Enter to keep)",
                      list(available_themes().items()), original, preview),
            chosen,
        )

    @property
    def editors(self) -> EditorArea:
        return self.query_one(EditorArea)

    @property
    def sidebar(self) -> Sidebar:
        return self.query_one(Sidebar)

    @property
    def panel(self) -> Panel:
        return self.query_one(Panel)

    @property
    def claude(self) -> ClaudePanel:
        return self.query_one(ClaudePanel)

    def _set_visible(self, part: str, visible: bool) -> None:
        widget = {"sidebar": self.sidebar, "panel": self.panel, "claude": self.claude}[part]
        if not visible and self.focused is not None and widget in self.focused.ancestors_with_self:
            self.set_focus(self.editors.active_editor)
        widget.display = visible
        self.query_one(f"#{part}-splitter").display = visible
        self.query_one(ActivityBar).set_active(
            self.sidebar.view if self.sidebar.display else None, self.claude.display
        )

    def check_action(self, action: str, parameters: tuple[object, ...]) -> bool | None:
        if action in KEYBOARD_ACTIONS and isinstance(self.screen, ModalScreen):
            return False
        if action in TERMINAL_OWNED and isinstance(self.focused, Terminal):
            return False
        return True

    # -- messages -------------------------------------------------------------

    def open_file(self, path: Path, line: int | None = None) -> None:
        self.editors.open_file(Path(path), line)

    def on_open_file(self, message: OpenFile) -> None:
        self.open_file(message.path, message.line)

    def on_editor_area_state_changed(self, message: EditorArea.StateChanged) -> None:
        for status in self.query(StatusBar):  # may already be gone during shutdown
            status.set_editor(self.editors.active)

    def on_editor_area_diagnostics_changed(self, message: EditorArea.DiagnosticsChanged) -> None:
        for status in self.query(StatusBar):  # may already be gone during shutdown
            status.set_diagnostics(self.editors.docs)

    def on_activity_bar_selected(self, message: ActivityBar.Selected) -> None:
        item = message.item_id
        if item == "claude":
            self.action_toggle_claude()
        elif item == "settings":
            gear = self.query_one("#activity-settings")

            async def picked(action: str | None) -> None:
                if action:
                    await self.run_action(action)

            self.push_screen(
                MenuScreen(
                    [("Command Palette...", "F1", "command_palette"),
                     None,
                     ("Color Theme...", "", "select_theme")],
                    x=gear.region.right + 1, y=gear.region.bottom, anchor_bottom=True,
                ),
                picked,
            )
        elif item == "account":
            self.notify("Accounts aren't built yet.")
        elif self.sidebar.display and self.sidebar.view == item:
            self._set_visible("sidebar", False)
        else:
            self.action_show_view(item)

    # -- context menus --------------------------------------------------------

    def context_menu(self, items: list, x: int, y: int) -> None:
        """Open a right-click menu at (x, y); the chosen item's app action runs."""

        async def picked(action: str | None) -> None:
            if action:
                await self.run_action(action)

        self.push_screen(MenuScreen(items, x=x, y=y), picked)

    def explorer_menu(self, path: Path | None, x: int, y: int) -> None:
        self._menu_path = path
        items: list = [("New File...", "", "new_file_in"), ("New Folder...", "", "new_folder"), None]
        if path is not None:
            items += [("Rename...", "", "rename"), ("Delete", "", "delete"), None,
                      ("Copy Path", "", "copy_path(False)"), ("Copy Relative Path", "", "copy_path(True)")]
        else:
            items += [("Refresh Explorer", "", "refresh_explorer"), ("Collapse Folders", "", "collapse_explorer")]
        self.context_menu(items, x, y)

    def tab_menu(self, doc_id: str, x: int, y: int) -> None:
        doc = next((d for d in self.editors.docs if d.id == doc_id), None)
        self._menu_path = getattr(doc, "path", None)
        items: list = [("Close", "Ctrl+W", f"close_tab('{doc_id}')"),
                       ("Close Others", "", f"close_others('{doc_id}')"),
                       ("Close All", "", "close_others('')")]
        if self._menu_path is not None:
            items += [None, ("Copy Path", "", "copy_path(False)"), ("Copy Relative Path", "", "copy_path(True)")]
        self.context_menu(items, x, y)

    def terminal_menu(self, terminal: Terminal, x: int, y: int) -> None:
        items: list = [("Copy", "Ctrl+C", "editor('copy')")] if terminal.has_selection else []
        if self.clipboard:
            items.append(("Paste", "Ctrl+Shift+V", "editor('paste')"))
        items += [("Select All", "", "editor('select_all')"), None]
        if terminal is self.claude.terminal:
            items += [("Switch Agent...", "", "agent_menu"), ("New Conversation", "", "restart_claude"),
                     ("Stop Agent", "", "stop_agent"), None, ("Hide Agent Panel", "Ctrl+Alt+B", "toggle_claude")]
        else:
            items += [("Restart Terminal", "", "restart_terminal"), ("Hide Panel", "Ctrl+J", "toggle_panel")]
        self.context_menu(items, x, y)

    # -- explorer file operations ---------------------------------------------

    def _ask_new(self, folder: bool) -> None:
        """Ask for a name, then create a file/folder in the right-clicked (or selected) folder."""
        tree = self.query_one(ExplorerTree)
        path, self._menu_path = self._menu_path, None
        if path is None:
            node = tree.cursor_node
            path = node.data.path if node is not None and node.data else self.workspace
        parent = path if path.is_dir() else path.parent
        shown = "" if parent == self.workspace else str(parent.relative_to(self.workspace)) + "/"

        def chosen(name: str | None) -> None:
            if not name:
                return
            target = parent / name
            if target.exists():
                self.notify(f"'{name}' already exists in this folder. Choose a different name.", severity="warning")
                return
            try:
                if folder:
                    target.mkdir(parents=True)
                else:
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.touch()
            except OSError as error:
                self.notify(f"Could not create '{name}': {error}", severity="error")
                return
            tree.reload()
            if not folder:
                self.open_file(target)

        what = "New Folder" if folder else "New File"
        self.push_screen(
            InputModal(f"{what} in {shown or 'the workspace folder'} (Enter to create, Esc to cancel)",
                       placeholder="folder name" if folder else "file name"),
            chosen,
        )

    def action_new_file_in(self) -> None:
        self._ask_new(folder=False)

    def action_new_folder(self) -> None:
        self._ask_new(folder=True)

    def action_rename(self) -> None:
        old = self._menu_path
        if old is None:
            return

        def chosen(name: str | None) -> None:
            if not name or name == old.name:
                return
            target = old.with_name(name)
            if target.exists():
                self.notify(f"'{name}' already exists in this folder. Choose a different name.", severity="warning")
                return
            try:
                old.rename(target)
            except OSError as error:
                self.notify(f"Could not rename '{old.name}': {error}", severity="error")
                return
            for doc in self.editors.docs:  # open editors follow the file to its new name
                path = getattr(doc, "path", None)
                if path is not None and (path == old or old in path.parents):
                    doc.path = target / path.relative_to(old) if path != old else target
                    doc.title = doc.path.name
            self.editors._sync()
            self.query_one(ExplorerTree).reload()

        self.push_screen(InputModal(f"Rename '{old.name}' (Enter to confirm, Esc to cancel)", old.name), chosen)

    def action_delete(self) -> None:
        path = self._menu_path
        if path is None or path == self.workspace:
            return
        trash = shutil.which("gio") is not None

        def answered(choice: str | None) -> None:
            if choice not in ("Move to Trash", "Delete"):
                return
            try:
                if trash:
                    done = subprocess.run(["gio", "trash", str(path)], capture_output=True, text=True)
                    if done.returncode != 0:
                        raise OSError(done.stderr.strip())
                elif path.is_dir() and not path.is_symlink():
                    shutil.rmtree(path)
                else:
                    path.unlink()
            except OSError as error:
                self.notify(f"Could not delete '{path.name}': {error}", severity="error")
                return
            for doc in list(self.editors.docs):
                doc_path = getattr(doc, "path", None)
                if doc_path is not None and (doc_path == path or path in doc_path.parents):
                    self.editors._remove(doc)
            self.query_one(ExplorerTree).reload()

        self.push_screen(
            ChoiceModal(
                f"Are you sure you want to delete '{path.name}'{' and its contents' if path.is_dir() else ''}?",
                "You can restore it from the Trash." if trash else "This cannot be undone.",
                ["Move to Trash" if trash else "Delete", "Cancel"],
            ),
            answered,
        )

    def action_copy_path(self, relative: bool) -> None:
        path = self._menu_path
        if path is None:
            return
        if relative and path.is_relative_to(self.workspace):
            path = path.relative_to(self.workspace)
        self.copy_to_clipboard(str(path))

    def action_close_tab(self, doc_id: str) -> None:
        self.editors.close(doc_id)

    def action_close_others(self, keep: str) -> None:
        """Close every editor except `keep` ('' closes them all)."""
        for doc in list(self.editors.docs):
            if doc.id != keep:
                self.editors.close(doc.id)

    # -- agents ---------------------------------------------------------------

    def action_agent_menu(self) -> None:
        running = self.claude.agent
        items: list = [(name, "running" if index == running else "", f"start_agent({index})")
                       for index, (name, _) in enumerate(AGENTS) if agent_command(index)]
        if not items:
            self.notify("No coding agent is installed. Install Claude Code, Codex, Grok or another agent CLI, "
                        "then try again.", severity="warning")
            return
        if running is not None:
            items += [None, ("New Conversation", "", "restart_claude"), ("Stop Agent", "", "stop_agent")]
        button = self.query_one("#agent-button")
        self.context_menu(items, button.region.x, 1)

    async def action_start_agent(self, index: int) -> None:
        if agent_command(index) is None:
            name, command = AGENTS[index]
            self.notify(f"{name} isn't installed (no `{command}` command found).", severity="warning")
            return
        if self.claude.agent != index:
            await self.claude.start(index)
            self.query_one("#agent-button").update(agent_label(AGENTS[index][0]))
        self.action_focus_claude()

    async def action_stop_agent(self) -> None:
        if self.focused is not None and self.focused is self.claude.terminal:
            self.set_focus(self.editors.active_editor)
        await self.claude.stop()
        self.query_one("#agent-button").update(agent_label(None))

    # -- file actions ---------------------------------------------------------

    def action_new_file(self) -> None:
        self.editors.new_untitled()

    def action_save(self) -> None:
        self.editors.save()

    def action_save_as(self) -> None:
        self.editors.save(save_as=True)

    def action_save_all(self) -> None:
        self.editors.save_all()

    def action_close_editor(self) -> None:
        self.editors.close()

    def action_next_editor(self) -> None:
        self.editors.cycle(1)

    def action_previous_editor(self) -> None:
        self.editors.cycle(-1)

    def action_quick_open(self) -> None:
        if not CommandPalette.is_open(self):
            self.push_screen(CommandPalette(providers=[FileProvider], placeholder="Search files by name"))

    def action_trigger_suggest(self) -> None:
        """Open the code completion list at the cursor (Ctrl+Space)."""
        editor = self.editors.active_editor
        if editor is None or isinstance(self.focused, Terminal):
            return
        editor.focus()
        editor.call_after_refresh(editor.open_suggest, True)

    def action_editor(self, name: str) -> None:
        """Run a TextArea action (undo, redo, cut, copy, paste, select_all...) on the active editor.

        With a terminal focused, Copy / Paste / Select All act on the terminal instead."""
        terminal = self.screen_stack[0].focused
        if isinstance(terminal, Terminal) and name in ("copy", "paste", "select_all"):
            if name == "copy":
                terminal.copy_selection()
            elif name == "paste":
                terminal.paste(self.clipboard)
            else:
                terminal.select_all()
            return
        editor = self.editors.active_editor
        if editor is not None:
            editor.focus()
            getattr(editor, f"action_{name}")()

    async def action_quit(self) -> None:
        dirty = self.editors.dirty_docs()
        if not dirty:
            self.exit()
            return

        def answered(choice: str | None) -> None:
            if choice == "Don't Save":
                self.exit()
            elif choice == "Save All":
                for doc in dirty:
                    if doc.path is not None:
                        self.editors.save(doc)
                if self.editors.dirty_docs():
                    self.notify("Untitled files still have unsaved changes. Save or close them first.",
                                severity="warning")
                else:
                    self.exit()

        names = ", ".join(d.title for d in dirty[:5])
        self.push_screen(
            ChoiceModal(
                f"Do you want to save the changes to the following {len(dirty)} file(s)?",
                f"{names}\n\nYour changes will be lost if you don't save them.",
                ["Save All", "Don't Save", "Cancel"],
            ),
            answered,
        )

    # -- view actions ---------------------------------------------------------

    def action_show_view(self, view: str) -> None:
        self._set_visible("sidebar", True)
        self.sidebar.show_view(view)
        self._set_visible("sidebar", True)  # refresh the activity bar highlight

    def action_toggle_sidebar(self) -> None:
        self._set_visible("sidebar", not self.sidebar.display)

    def action_toggle_panel(self) -> None:
        self._set_visible("panel", not self.panel.display)

    def action_toggle_claude(self) -> None:
        self._set_visible("claude", not self.claude.display)
        if self.claude.display:
            self.action_focus_claude()

    def action_toggle_spotify(self) -> None:
        if not self.spotify_enabled:
            return
        player = self.query_one(SpotifyPlayer)
        player.display = not player.display
        save_setting("spotify", player.display)
        if player.display:
            self._set_visible("claude", True)  # the player lives in the right-hand column

    def action_spotify(self, command: str) -> None:
        """Spotify player commands: login, logout, toggle, next, previous, shuffle, search, volume_up/down."""
        if not self.spotify_enabled:
            return
        player = self.query_one(SpotifyPlayer)
        if not player.display:
            self.action_toggle_spotify()
        player.command(command)

    def action_focus_terminal(self) -> None:
        terminal = self.panel.terminal
        if terminal.has_focus:  # Ctrl+` toggles, like VS Code
            self._set_visible("panel", False)
            return
        self._set_visible("panel", True)
        self.panel.show_tab("terminal")

    def action_restart_terminal(self) -> None:
        self._set_visible("panel", True)
        self.panel.show_tab("terminal")
        self.panel.terminal.restart()

    def action_focus_claude(self) -> None:
        self._set_visible("claude", True)
        if self.claude.terminal is not None:
            self.claude.terminal.focus()

    def action_restart_claude(self) -> None:
        self.action_focus_claude()
        if self.claude.terminal is not None:
            self.claude.terminal.restart()
        else:
            self.action_agent_menu()

    def _parts(self) -> list:
        """Focusable regions in F6 order."""
        parts = [self.editors.active_editor]
        if self.panel.display:
            parts.append(self.panel.terminal)
        if self.claude.display:
            parts.append(self.claude.terminal)
        if self.sidebar.display and self.sidebar.view == "explorer":
            parts.append(self.query_one(ExplorerTree))
        return [p for p in parts if p is not None]

    def _focus_part(self, step: int) -> None:
        parts = self._parts()
        if not parts:
            return
        index = parts.index(self.focused) if self.focused in parts else -1
        parts[(index + step) % len(parts)].focus()

    def action_focus_next_part(self) -> None:
        self._focus_part(1)

    def action_focus_previous_part(self) -> None:
        self._focus_part(-1)

    def action_refresh_explorer(self) -> None:
        self.query_one(ExplorerTree).reload()

    def action_collapse_explorer(self) -> None:
        tree = self.query_one(ExplorerTree)
        for node in tree.root.children:
            node.collapse_all()

    def action_refresh_scm(self) -> None:
        self.query_one(SourceControlView).reload()

    def action_run_file(self) -> None:
        editor = self.editors.active_editor
        if editor is None or editor.path is None:
            self.notify("Open and save a file first.", severity="warning")
            return
        runner = RUNNERS.get(editor.path.suffix.lower())
        if runner is None:
            self.notify(f"Don't know how to run '{editor.path.suffix or editor.title}' files yet.",
                        severity="warning")
            return

        def run() -> None:
            self._set_visible("panel", True)
            self.panel.show_tab("terminal")
            self.panel.terminal.send_line(shlex.join([runner, str(editor.path)]))

        if editor.dirty:
            self.editors.save(editor, then=run)
        else:
            run()

    def action_about(self) -> None:
        self.notify(
            f"Version {version('tscode')}\nTextual {version('textual')}\n"
            f"Theme: {self.vs_theme.label}\nWorkspace: {self.workspace}",
            title="Terminal Studio Code",
            timeout=8,
        )
