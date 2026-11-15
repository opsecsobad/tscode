"""The bottom status bar."""

from __future__ import annotations

from pathlib import Path

from textual.app import ComposeResult
from textual.containers import Horizontal
from textual.widgets import Static

from ..icons import icon
from .common import ActionButton


def git_branch(workspace: Path) -> str | None:
    """Current branch (or short commit) read straight from .git/HEAD."""
    for folder in (workspace, *workspace.parents):
        head = folder / ".git" / "HEAD"
        if head.is_file():
            try:
                ref = head.read_text().strip()
            except OSError:
                return None
            return ref.removeprefix("ref: refs/heads/") if ref.startswith("ref: ") else ref[:7]
    return None


class StatusBar(Horizontal):
    def __init__(self, workspace: Path) -> None:
        super().__init__(id="statusbar")
        self.workspace = workspace

    def compose(self) -> ComposeResult:
        yield Static(f" {icon('remote')} ", id="status-remote")
        yield ActionButton("", "show_view('scm')", "Source Control", id="status-branch", classes="status-item")
        yield Static(f"{icon('error')} 0 {icon('warning')} 0", id="status-problems", classes="status-item")
        yield Static(classes="spacer")
        yield Static("", id="status-position", classes="status-item -editor")
        yield Static("Spaces: 4", classes="status-item -editor")
        yield Static("UTF-8", classes="status-item -editor")
        yield Static("LF", classes="status-item -editor")
        yield Static("", id="status-language", classes="status-item -editor")
        yield ActionButton(icon("claude"), "toggle_claude", "Toggle Claude Code (Ctrl+Alt+B)",
                           classes="status-item claude-mark", id="status-claude-mark")
        yield ActionButton("Claude", "toggle_claude", "Toggle Claude Code (Ctrl+Alt+B)",
                           classes="status-item", id="status-claude")
        yield Static(icon("bell"), classes="status-item")

    def on_mount(self) -> None:
        self.refresh_branch()
        self.set_interval(5.0, self.refresh_branch)
        self.set_editor(None)

    def refresh_branch(self) -> None:
        branch = git_branch(self.workspace)
        item = self.query_one("#status-branch", Static)
        item.display = branch is not None
        if branch:
            item.update(f"{icon('branch')} {branch}")

    def set_editor(self, doc) -> None:
        """Show cursor/language info for the active document (None = no editor)."""
        has_editor = doc is not None and hasattr(doc, "cursor_location")
        for item in self.query(".-editor"):
            item.display = has_editor
        if doc is not None:
            language = self.query_one("#status-language", Static)
            language.display = True
            language.update(doc.language_name)
        if has_editor:
            row, column = doc.cursor_location
            selected = len(doc.selected_text)
            text = f"Ln {row + 1}, Col {column + 1}" + (f" ({selected} selected)" if selected else "")
            self.query_one("#status-position", Static).update(text)

    def set_diagnostics(self, docs) -> None:
        """Refresh the workspace's error and warning totals."""
        errors = sum(getattr(doc, "error_count", 0) for doc in docs)
        warnings = sum(getattr(doc, "warning_count", 0) for doc in docs)
        self.query_one("#status-problems", Static).update(
            f"{icon('error')} {errors} {icon('warning')} {warnings}"
        )
