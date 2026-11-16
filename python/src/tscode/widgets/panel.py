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


# Coding agents the right-hand panel can run: (name, command). Only installed ones are offered.
AGENTS = [
    ("Claude Code", "claude"),
    ("Codex", "codex"),
    ("Grok", "grok"),
    ("Antigravity", "agy"),
    ("Gemini", "gemini"),
    ("OpenCode", "opencode"),
    ("Cursor Agent", "cursor-agent"),
]


def agent_command(index: int) -> list[str] | None:
    """The command line for an agent, if it is installed. TSCODE_CLAUDE_CMD overrides Claude's."""
    override = os.environ.get("TSCODE_CLAUDE_CMD")
    if index == 0 and override:
        return shlex.split(override)
    command = AGENTS[index][1]
    return [command] if shutil.which(command) else None


class ClaudePanel(Vertical):
    """Secondary side bar: a coding agent's real CLI (Claude Code, Codex, ...) in an embedded
    terminal, with the Spotify player docked in its bottom corner. Nothing runs until an
    agent is picked from the Agents menu."""

    def __init__(self, workspace: Path, theme: VSTheme) -> None:
        super().__init__(id="claude-panel")
        self.workspace = workspace
        self.vs_theme = theme
        self.agent: int | None = None  # index into AGENTS of the running agent

    async def start(self, index: int) -> None:
        """Run agent `index` here, replacing whichever one is running."""
        command = agent_command(index)
        if command is None:
            return
        await self.stop()
        self.agent = index
        self.query_one("#agent-title", Static).update(AGENTS[index][0].upper())
        for placeholder in self.query(".agent-empty"):
            placeholder.display = False
        terminal = Terminal(command, self.workspace, theme=self.app.vs_theme, id="claude-terminal")
        await self.mount(terminal, after=self.query_one(".sidebar-header"))

    async def stop(self) -> None:
        for terminal in self.query("#claude-terminal"):
            await terminal.remove()  # unmounting kills the process
        self.agent = None
        self.query_one("#agent-title", Static).update("AGENT")
        for placeholder in self.query(".agent-empty"):
            placeholder.display = True

    def compose(self) -> ComposeResult:
        with Horizontal(classes="sidebar-header"):
            yield Static(icon("claude"), classes="claude-mark")
            yield Static("AGENT", classes="sidebar-title", id="agent-title")
            yield ActionButton(icon("add"), "restart_claude", "New Conversation (restarts the agent)", classes="header-button")
            yield ActionButton(icon("close"), "toggle_claude", "Hide Agent Panel (Ctrl+Alt+B)", classes="header-button")
        yield Static(
            "No agent is running.\n\n"
            "Pick one (Claude Code, Codex, Grok, Antigravity...) from the Agents menu in the top-right corner.",
            classes="sidebar-note agent-empty",
        )
        yield Static(classes="filler agent-empty")
        if self.app.spotify_enabled:
            yield SpotifyPlayer()

    @property
    def terminal(self) -> Terminal | None:
        found = self.query("#claude-terminal")
        return found.first(Terminal) if found else None
