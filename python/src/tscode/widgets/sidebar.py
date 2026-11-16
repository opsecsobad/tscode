"""The primary side bar: Explorer, Search, Source Control (+ placeholder views)."""

from __future__ import annotations

import asyncio
import shutil
from pathlib import Path
from typing import Iterable

from rich.style import Style
from rich.text import Text
from textual import events, work
from textual.app import ComposeResult
from textual.containers import Horizontal, Vertical
from textual.message import Message
from textual.widgets import ContentSwitcher, DirectoryTree, Input, OptionList, Static, Tree
from textual.widgets.option_list import Option

from ..icons import file_icon, icon
from .common import ActionButton

HIDDEN = {".git", ".DS_Store"}
PLACEHOLDERS = {
    "debug": ("RUN AND DEBUG", "Debugging isn't built yet.\n\nTo run the open file, choose Run > Run Active File in Terminal."),
    "extensions": ("EXTENSIONS", "Extensions aren't built yet.\n\nClaude Code is already included: open it from the ✻ icon."),
}


class OpenFile(Message):
    """Ask the app to open a file (optionally at a 1-based line)."""

    def __init__(self, path: Path, line: int | None = None) -> None:
        super().__init__()
        self.path, self.line = path, line


class ExplorerTree(DirectoryTree):
    def __init__(self, path: Path) -> None:
        super().__init__(path, id="explorer-tree", classes="sidebar-tree")
        self.show_root = False
        self.show_guides = False
        self.guide_depth = 2
        self._mtimes: dict[Path, float] = {}

    def on_mount(self) -> None:
        self.set_interval(2.0, self._poll_disk)

    def on_click(self, event: events.Click) -> None:
        """Right-click: select the row (without opening it) and show its menu."""
        if event.button != 3:
            return
        event.prevent_default()
        event.stop()
        line = event.style.meta.get("line")
        node = self.get_node_at_line(line) if line is not None else None
        if node is not None:
            self.move_cursor(node)
        self.focus()
        path = node.data.path if node is not None and node.data else None
        self.app.explorer_menu(path, event.screen_x, event.screen_y)

    def filter_paths(self, paths: Iterable[Path]) -> Iterable[Path]:
        return [p for p in paths if p.name not in HIDDEN]

    def render_label(self, node, base_style: Style, style: Style) -> Text:
        label = node._label.copy()
        label.stylize(style)
        if node._allow_expand:
            glyph = icon("chevron_down") if node.is_expanded else icon("chevron_right")
            prefix = (glyph + " ", base_style)
        else:
            glyph, colour = file_icon(node.data.path if node.data else "", self.app.vs_theme)
            prefix = (glyph + " ", base_style + Style(color=colour))
        return Text.assemble(prefix, label)

    def _poll_disk(self) -> None:
        """Reload directories whose contents changed on disk (e.g. Claude created a file)."""
        nodes = [self.root]
        while nodes:
            node = nodes.pop()
            if node.data is None or not (node is self.root or node.is_expanded):
                continue
            nodes.extend(node.children)
            path = node.data.path
            try:
                mtime = path.stat().st_mtime
            except OSError:
                continue
            if self._mtimes.setdefault(path, mtime) != mtime:
                self._mtimes[path] = mtime
                self.reload_node(node)


class ResultTree(Tree):
    """Side-bar list styled like the Explorer (search results)."""

    def __init__(self, id: str) -> None:
        super().__init__("results", id=id, classes="sidebar-tree")
        self.show_root = False
        self.show_guides = False
        self.guide_depth = 2

    def render_label(self, node, base_style: Style, style: Style) -> Text:
        label = node._label.copy()
        label.stylize(style)
        if node._allow_expand:
            glyph = icon("chevron_down") if node.is_expanded else icon("chevron_right")
            return Text.assemble((glyph + " ", base_style), label)
        return label


class SearchView(Vertical):
    """Find in files, backed by ripgrep. Results are grouped by file, like VS Code."""

    def __init__(self, workspace: Path) -> None:
        super().__init__(id="search")
        self.workspace = workspace

    def compose(self) -> ComposeResult:
        yield Static("SEARCH", classes="sidebar-title")
        yield Input(placeholder="Search", id="search-input")
        yield Static("Type a term and press Enter.", id="search-summary", classes="sidebar-note")
        yield ResultTree("search-results")

    def focus_input(self) -> None:
        self.query_one(Input).focus()

    def on_input_submitted(self, event: Input.Submitted) -> None:
        event.stop()
        self.run_search(event.value)

    @work(exclusive=True)
    async def run_search(self, query: str) -> None:
        tree = self.query_one(ResultTree)
        summary = self.query_one("#search-summary", Static)
        tree.clear()
        if not query:
            summary.update("Type a term and press Enter.")
            return
        if not shutil.which("rg"):
            summary.update("Search needs ripgrep. Install `rg` and try again.")
            return
        proc = await asyncio.create_subprocess_exec(
            "rg", "--line-number", "--no-heading", "--color=never", "--smart-case",
            "--fixed-strings", "--max-count=50", "--max-columns=300", "--", query, ".",
            cwd=self.workspace, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
        )
        out, _ = await proc.communicate()
        theme = self.app.vs_theme
        match_style = Style(bgcolor=theme["editor.findMatchHighlightBackground"])
        dim = Style(color=theme["descriptionForeground"])
        groups: dict[str, list[tuple[int, str]]] = {}
        total = 0
        for raw in out.decode("utf-8", "replace").splitlines()[:1000]:
            name, _, rest = raw.partition(":")
            number, _, text = rest.partition(":")
            if number.isdigit():
                groups.setdefault(name.removeprefix("./"), []).append((int(number), text.strip()))
                total += 1
        folded = query.lower()
        for name, hits in groups.items():
            path = Path(name)
            glyph, colour = file_icon(path, theme)
            # One string, styled by range: separate spans lose their leading spaces in the tree.
            folder = "" if str(path.parent) == "." else f"{path.parent}  "
            label = Text(f"{glyph} {path.name}  {folder}{len(hits)}")
            label.stylize(Style(color=colour), 0, 2)
            label.stylize(dim, len(glyph) + len(path.name) + 2)
            node = tree.root.add(label, expand=True)
            for number, text in hits:
                line = Text(text)
                start = text.lower().find(folded)
                if start >= 0:
                    # Keep the match in view on long lines.
                    cut = max(0, start - 12)
                    line = Text(("…" if cut else "") + text[cut:])
                    offset = start - cut + (1 if cut else 0)
                    line.stylize(match_style, offset, offset + len(query))
                node.add_leaf(line, data=(self.workspace / name, number))
        summary.update(
            f"{total} results in {len(groups)} files" if total
            else "No results found. Check the spelling, or search for fewer words."
        )

    def on_tree_node_selected(self, event: Tree.NodeSelected) -> None:
        event.stop()
        if event.node.data:
            path, line = event.node.data
            self.post_message(OpenFile(path, line))


class SourceControlView(Vertical):
    """Read-only list of changed files from `git status`."""

    STATUS = {"M": "modified", "A": "added", "D": "deleted", "?": "untracked", "R": "untracked", "U": "deleted"}

    def __init__(self, workspace: Path) -> None:
        super().__init__(id="scm")
        self.workspace = workspace
        self._paths: list[Path] = []

    def compose(self) -> ComposeResult:
        with Horizontal(classes="sidebar-header"):
            yield Static("SOURCE CONTROL", classes="sidebar-title")
            yield ActionButton(icon("refresh"), "refresh_scm", "Refresh", classes="header-button")
        yield Static("", id="scm-summary", classes="sidebar-note")
        yield OptionList(id="scm-changes")

    @work(exclusive=True)
    async def reload(self) -> None:
        changes = self.query_one(OptionList)
        summary = self.query_one("#scm-summary", Static)
        changes.clear_options()
        self._paths = []
        try:
            proc = await asyncio.create_subprocess_exec(
                "git", "status", "--porcelain", cwd=self.workspace,
                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
            )
            out, _ = await proc.communicate()
        except FileNotFoundError:
            summary.update("Source control needs git. Install it and reopen this view.")
            return
        if proc.returncode != 0:
            summary.update("This folder is not a Git repository yet. Run `git init` in the terminal to start one.")
            return
        theme = self.app.vs_theme
        options = []
        for raw in out.decode("utf-8", "replace").splitlines():
            code, name = raw[:2].strip()[:1] or "?", raw[3:].split(" -> ")[-1].strip('"')
            colour = theme[f"gitDecoration.{self.STATUS.get(code, 'modified')}ResourceForeground"]
            path = Path(name)
            glyph, icon_colour = file_icon(path, theme)
            shown = path.name or name
            folder = "" if str(path.parent) == "." else f"{path.parent}  "
            line = Text(f"{glyph} {shown}  {folder}{'U' if code == '?' else code}")
            line.stylize(Style(color=icon_colour), 0, 2)
            line.stylize(Style(color=colour), 2, 2 + len(shown))
            line.stylize(Style(color=theme["descriptionForeground"]), 4 + len(shown), 4 + len(shown) + len(folder))
            line.stylize(Style(color=colour), 4 + len(shown) + len(folder))
            options.append(Option(line))
            self._paths.append(self.workspace / name)
        changes.add_options(options)
        summary.update(f"Changes  {len(options)}" if options else "No changes. Everything is committed.")

    def on_option_list_option_selected(self, event: OptionList.OptionSelected) -> None:
        event.stop()
        path = self._paths[event.option_index]
        if path.is_file():
            self.post_message(OpenFile(path))


class Sidebar(Vertical):
    def __init__(self, workspace: Path) -> None:
        super().__init__(id="sidebar")
        self.workspace = workspace

    def compose(self) -> ComposeResult:
        with ContentSwitcher(initial="explorer", id="sidebar-views"):
            with Vertical(id="explorer"):
                with Horizontal(classes="sidebar-header"):
                    yield Static("EXPLORER", classes="sidebar-title")
                    yield Static(icon("ellipsis"), classes="header-button")
                with Horizontal(classes="section-header"):
                    yield Static(
                        f"{icon('chevron_down')} {self.workspace.name.upper()}", classes="section-title"
                    )
                    yield ActionButton(icon("new_file"), "new_file_in", "New File...", classes="header-button")
                    yield ActionButton(icon("new_folder"), "new_folder", "New Folder...", classes="header-button")
                    yield ActionButton(icon("refresh"), "refresh_explorer", "Refresh Explorer", classes="header-button")
                    yield ActionButton(icon("collapse_all"), "collapse_explorer", "Collapse Folders in Explorer", classes="header-button")
                yield ExplorerTree(self.workspace)
            yield SearchView(self.workspace)
            yield SourceControlView(self.workspace)
            for view_id, (title, note) in PLACEHOLDERS.items():
                with Vertical(id=view_id):
                    yield Static(title, classes="sidebar-title")
                    yield Static(note, classes="sidebar-note")

    @property
    def view(self) -> str:
        return self.query_one(ContentSwitcher).current or "explorer"

    def show_view(self, view: str) -> None:
        self.query_one(ContentSwitcher).current = view
        if view == "explorer":
            self.query_one(ExplorerTree).focus()
        elif view == "search":
            self.query_one(SearchView).focus_input()
        elif view == "scm":
            self.query_one(SourceControlView).reload()

    def on_directory_tree_file_selected(self, event: DirectoryTree.FileSelected) -> None:
        event.stop()
        self.post_message(OpenFile(event.path))
