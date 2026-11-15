"""The bottom panel (Problems / Output / Debug Console / Terminal / Ports) and the Claude panel."""

from __future__ import annotations

import os
import shlex
import shutil
from pathlib import Path

from textual import events
from textual.app import ComposeResult
from textual.containers import Horizontal, Vertical
from textual.widgets import ContentSwitcher, Static

from ..icons import icon
from ..theme import VSTheme
from .common import ActionButton
from .spotify import SpotifyPlayer
from .terminal import Terminal

PANEL_TABS = [
    ("problems", "PROBLEMS", "No problems have been detected in the workspace."),
    ("output", "OUTPUT", "Nothing has written output yet."),
    ("debug-console", "DEBUG CONSOLE", "Debugging isn't built yet. Run the open file from Run > Run Active File in Terminal."),
    ("terminal", "TERMINAL", None),
    ("ports", "PORTS", "No forwarded ports."),
]


class PanelTab(Static):
    def __init__(self, tab_id: str, label: str) -> None:
        super().__init__(label, classes="panel-tab")
        self.tab_id = tab_id

    def on_click(self, event: events.Click) -> None:
        event.stop()
        self.app.query_one(Panel).show_tab(self.tab_id)


class Panel(Vertical):
    def __init__(self, workspace: Path, theme: VSTheme) -> None:
        super().__init__(id="panel")
        self.workspace = workspace
        self.vs_theme = theme
        self.shell = os.environ.get("SHELL") or shutil.which("bash") or "/bin/sh"

    def compose(self) -> ComposeResult:
        with Horizontal(classes="panel-header"):
            for tab_id, label, _ in PANEL_TABS:
                yield PanelTab(tab_id, label)
            with Horizontal(classes="panel-actions"):
                yield Static(f"{icon('terminal')} {Path(self.shell).name}", classes="panel-shell")
                yield ActionButton(icon("trash"), "restart_terminal", "Kill Terminal and start a new one", classes="header-button")
                yield ActionButton(icon("close"), "toggle_panel", "Hide Panel (Ctrl+J)", classes="header-button")
        with ContentSwitcher(initial="terminal", id="panel-views"):
            for tab_id, _, note in PANEL_TABS:
                if note is None:
                    yield Terminal([self.shell], self.workspace, theme=self.vs_theme, id=tab_id)
                else:
                    yield Static(note, id=tab_id, classes="panel-note")

    def on_mount(self) -> None:
        self.show_tab("terminal")

    @property
    def terminal(self) -> Terminal:
        return self.query_one("#terminal", Terminal)

    def show_tab(self, tab_id: str) -> None:
        self.query_one(ContentSwitcher).current = tab_id
        for tab in self.query(PanelTab):
            tab.set_class(tab.tab_id == tab_id, "-active")
        self._fit_header()
        if tab_id == "terminal":
            self.terminal.focus()

    def on_resize(self, event: events.Resize) -> None:
        self._fit_header()

    def _fit_header(self) -> None:
        """Like VS Code, drop what doesn't fit: first the shell name, then inactive tabs."""
        width = self.size.width - 2
        if width <= 0:
            return
        tabs = list(self.query(PanelTab))
        needed = sum(len(label) + 2 for _, label, _ in PANEL_TABS)
        shell = self.query_one(".panel-shell", Static)
        shell_width = len(Path(self.shell).name) + 4
        shell.display = width >= needed + 6 + shell_width
        budget = width - 6 - (shell_width if shell.display else 0)
        active = next((t for t in tabs if t.has_class("-active")), None)
        if active is not None:
            budget -= len(active.tab_id) + 2
        for tab in tabs:
            if tab is active:
                tab.display = True
                continue
            cost = len(tab.tab_id) + 2
            tab.display = budget >= cost
            if tab.display:
                budget -= cost


def claude_command() -> list[str] | None:
    """The command for the Claude panel. Override with TSCODE_CLAUDE_CMD."""
    override = os.environ.get("TSCODE_CLAUDE_CMD")
    if override:
        return shlex.split(override)
    return ["claude"] if shutil.which("claude") else None


class ClaudePanel(Vertical):
    """Secondary side bar: the real Claude Code CLI in an embedded terminal, with the
    Spotify player docked in its bottom corner."""

    def __init__(self, workspace: Path, theme: VSTheme) -> None:
        super().__init__(id="claude-panel")
        self.workspace = workspace
        self.vs_theme = theme

    def compose(self) -> ComposeResult:
        with Horizontal(classes="sidebar-header"):
            yield Static(icon("claude"), classes="claude-mark")
            yield Static("CLAUDE CODE", classes="sidebar-title")
            yield ActionButton(icon("add"), "restart_claude", "New Conversation (restarts Claude Code)", classes="header-button")
            yield ActionButton(icon("close"), "toggle_claude", "Hide Claude Code (Ctrl+Alt+B)", classes="header-button")
        command = claude_command()
        if command:
            yield Terminal(command, self.workspace, theme=self.vs_theme, id="claude-terminal")
        else:
            yield Static(
                "Claude Code isn't installed, so this panel has nothing to run.\n\n"
                "Install it from https://claude.com/claude-code, then restart tscode.",
                classes="sidebar-note",
            )
            yield Static(classes="filler")
        if self.app.spotify_enabled:
            yield SpotifyPlayer()

    @property
    def terminal(self) -> Terminal | None:
        found = self.query("#claude-terminal")
        return found.first(Terminal) if found else None
