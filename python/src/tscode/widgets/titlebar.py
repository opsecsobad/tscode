"""The top stripe: logo, menu bar, command center, layout toggles, close."""

from __future__ import annotations

from textual import events
from textual.app import ComposeResult
from textual.containers import Horizontal
from textual.widgets import Static

from ..icons import icon
from .common import ActionButton, MenuScreen

# label, shortcut, app action. None = separator. Only things that really work are listed.
MENUS: dict[str, list[tuple[str, str, str] | None]] = {
    "File": [
        ("New Text File", "Ctrl+N", "new_file"),
        ("Open File...", "Ctrl+P", "quick_open"),
        None,
        ("Save", "Ctrl+S", "save"),
        ("Save As...", "", "save_as"),
        ("Save All", "Ctrl+Alt+S", "save_all"),
        None,
        ("Color Theme...", "", "select_theme"),
        None,
        ("Close Editor", "Ctrl+W", "close_editor"),
        None,
        ("Exit", "Ctrl+Q", "quit"),
    ],
    "Edit": [
        ("Undo", "Ctrl+Z", "editor('undo')"),
        ("Redo", "Ctrl+Y", "editor('redo')"),
        None,
        ("Cut", "Ctrl+X", "editor('cut')"),
        ("Copy", "Ctrl+C", "editor('copy')"),
        ("Paste", "Ctrl+V", "editor('paste')"),
    ],
    "Selection": [
        ("Select All", "Ctrl+A", "editor('select_all')"),
        ("Select Line", "", "editor('select_line')"),
    ],
    "View": [
        ("Command Palette...", "F1", "command_palette"),
        None,
        ("Explorer", "Ctrl+Shift+E", "show_view('explorer')"),
        ("Search", "", "show_view('search')"),
        ("Source Control", "", "show_view('scm')"),
        None,
        ("Toggle Primary Side Bar", "Ctrl+B", "toggle_sidebar"),
        ("Toggle Panel", "Ctrl+J", "toggle_panel"),
        ("Toggle Agent Panel", "Ctrl+Alt+B", "toggle_claude"),
    ],
    "Go": [
        ("Go to File...", "Ctrl+P", "quick_open"),
        None,
        ("Next Editor", "Ctrl+PgDn", "next_editor"),
        ("Previous Editor", "Ctrl+PgUp", "previous_editor"),
        ("Focus Next Part", "F6", "focus_next_part"),
    ],
    "Run": [
        ("Run Active File in Terminal", "", "run_file"),
    ],
    "Terminal": [
        ("Focus Terminal", "Ctrl+`", "focus_terminal"),
        ("Restart Terminal", "", "restart_terminal"),
        None,
        ("Start Agent...", "", "agent_menu"),
        ("Focus Agent", "", "focus_claude"),
        ("Restart Agent", "", "restart_claude"),
        ("Stop Agent", "", "stop_agent"),
    ],
    "Help": [
        ("Show All Commands", "F1", "command_palette"),
        ("About", "", "about"),
    ],
}


def agent_label(name: str | None) -> str:
    """The agent picker's text: the running agent's name, or "Agents"."""
    return f"{icon('claude')} {name or 'Agents'} {icon('chevron_down')}"


class MenuButton(Static):
    def __init__(self, name: str) -> None:
        super().__init__(name, classes="menu-button")
        self.menu_name = name

    def on_click(self, event: events.Click) -> None:
        event.stop()
        self.add_class("-open")

        async def done(action: str | None) -> None:
            self.remove_class("-open")
            if action:
                await self.app.run_action(action)

        self.app.push_screen(MenuScreen(MENUS[self.menu_name], x=self.region.x), done)


class TitleBar(Horizontal):
    def __init__(self, workspace_name: str) -> None:
        super().__init__(id="titlebar")
        self.workspace_name = workspace_name

    def compose(self) -> ComposeResult:
        yield Static(icon("logo"), id="logo")
        for name in MENUS:
            yield MenuButton(name)
        yield Static(classes="spacer")
        yield Static(f"{icon('arrow_left')}  {icon('arrow_right')} ", id="nav-arrows")
        # Rounded caps turn the command center into VS Code's pill.
        yield Static(icon("cap_left"), classes="pill-cap")
        yield ActionButton(
            f"{icon('search')} {self.workspace_name}",
            "quick_open",
            "Search files (Ctrl+P)",
            id="command-center",
        )
        yield Static(icon("cap_right"), classes="pill-cap")
        yield Static(classes="spacer")
        yield ActionButton(agent_label(None), "agent_menu", "Start a coding agent in the side panel",
                           id="agent-button", classes="title-button")
        yield ActionButton(icon("layout_left"), "toggle_sidebar", "Toggle Primary Side Bar (Ctrl+B)", classes="title-button")
        yield ActionButton(icon("layout_panel"), "toggle_panel", "Toggle Panel (Ctrl+J)", classes="title-button")
        yield ActionButton(icon("layout_right"), "toggle_claude", "Toggle Agent Panel (Ctrl+Alt+B)", classes="title-button")
        yield ActionButton(icon("win_close"), "quit", "Close", id="win-close", classes="title-button")
