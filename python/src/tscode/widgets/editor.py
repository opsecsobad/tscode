"""The editor group: tab strip, breadcrumbs and one text editor per open file."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from rich.cells import cell_len
from rich.segment import Segment
from rich.style import Style
from rich.text import Text
from textual import events, work
from textual.app import ComposeResult
from textual.containers import Vertical
from textual.message import Message
from textual.strip import Strip
from textual.widget import Widget
from textual.widgets import ContentSwitcher, Static, TextArea
from textual.widgets.text_area import Selection

from .. import completion
from ..icons import file_icon, icon
from ..diagnostics import Diagnostic, check
from ..languages import language_for, register_extra_languages
from ..theme import VSTheme
from .common import ChoiceModal, InputModal

MAX_FILE_BYTES = 5_000_000

EDITOR_MENU = [
    ("Cut", "Ctrl+X", "editor('cut')"),
    ("Copy", "Ctrl+C", "editor('copy')"),
    ("Paste", "Ctrl+V", "editor('paste')"),
    None,
    ("Select All", "Ctrl+A", "editor('select_all')"),
    None,
    ("Command Palette...", "F1", "command_palette"),
]

class CodeEditor(TextArea):
    """One open text document."""

    def __init__(self, text: str, path: Path | None, title: str, theme: VSTheme, id: str) -> None:
        # TextArea builds its syntax highlight map during its constructor. Our
        # override adds diagnostic ranges, so this must exist before super().
        self.diagnostics: list[Diagnostic] = []
        super().__init__(
            text, soft_wrap=False, tab_behavior="indent", show_line_numbers=True, id=id
        )
        self.path = path
        self.title = title
        self.saved_text = text if path else ""
        self.mtime = _mtime(path)
        self.dirty = False
        self.vs_theme = theme
        self._check_timer = None
        self._check_version = 0
        # Code completion: the open list, the word whose list was dismissed with Esc,
        # and whether the next text change (Backspace / Delete) should re-filter it.
        self.suggest: completion.Suggest | None = None
        self._suggest_dismissed: tuple[int, int] | None = None
        self._suggest_refilter = False
        self.apply_language()

    def on_mount(self) -> None:
        self.apply_theme(self.vs_theme)
        self.schedule_check(0.05)

    def apply_theme(self, theme: VSTheme) -> None:
        self.vs_theme = theme
        text_theme = theme.text_area_theme(
            line_cursor=getattr(self.app, "cursor_line", False), language=self.language)
        self.register_theme(text_theme)
        self.theme = text_theme.name

    def apply_language(self) -> None:
        language, self.language_name = language_for(self.path)
        # Optional grammars are registered only when a document actually needs
        # them. This also covers changing a file type through Save As.
        if language:
            register_extra_languages(self, [language])
        self.language = language if language in self.available_languages else None
        # VS Code wraps prose by default and leaves code alone.
        self.soft_wrap = self.language_name in ("Markdown", "Plain Text") and self.path is not None
        if self.is_mounted:  # Save As changed the file type: colours and checks follow
            self.apply_theme(self.vs_theme)
            self.schedule_check(0.05)

    # -- diagnostics (error squiggles) ------------------------------------------

    @property
    def error_count(self) -> int:
        return sum(1 for d in self.diagnostics if d.severity == "error")

    @property
    def warning_count(self) -> int:
        return len(self.diagnostics) - self.error_count

    def schedule_check(self, delay: float = 0.4) -> None:
        """Re-check the file shortly after the last edit."""
        if self._check_timer is not None:
            self._check_timer.stop()
        self._check_timer = self.set_timer(delay, self._run_check)

    @work(exclusive=True, group="diagnostics")
    async def _run_check(self) -> None:
        self._check_version += 1
        version = self._check_version
        tree = getattr(self.document, "_syntax_tree", None)
        found = await check(self.language, self.text, self.path, tree)
        if version != self._check_version or found == self.diagnostics:
            return
        self.diagnostics = found
        self._paint_diagnostics()
        self.post_message(EditorArea.DiagnosticsChanged())

    def _build_highlight_map(self) -> None:
        super()._build_highlight_map()
        self._add_diagnostic_highlights()

    def _add_diagnostic_highlights(self) -> None:
        """Squiggles ride on the syntax highlight map (columns are UTF-8 byte offsets)."""
        line_count = self.document.line_count
        for diagnostic in self.diagnostics:
            if diagnostic.line >= line_count:
                continue
            line = self.document.get_line(diagnostic.line)
            start = len(line[:diagnostic.column].encode("utf-8"))
            end = None if diagnostic.end_column is None else len(line[:diagnostic.end_column].encode("utf-8"))
            self._highlights[diagnostic.line].append((start, end, f"diag.{diagnostic.severity}"))

    def _paint_diagnostics(self) -> None:
        for row in list(self._highlights):
            self._highlights[row] = [h for h in self._highlights[row] if not h[2].startswith("diag.")]
        self._add_diagnostic_highlights()
        self._line_cache.clear()
        self.refresh()

    # -- code completion (the suggest widget) --------------------------------------

    @property
    def completion_language(self) -> str | None:
        return language_for(self.path)[0]

    async def _on_key(self, event: events.Key) -> None:
        if self.suggest is not None:
            model = self.suggest
            moves = {"up": -1, "down": 1, "pageup": -model.visible, "pagedown": model.visible}
            if event.key in moves or event.key in ("enter", "tab", "escape"):
                event.stop()
                event.prevent_default()
                if event.key in moves:
                    model.step(moves[event.key])
                    self._place_suggest()
                elif event.key == "escape":
                    # Stay quiet for the rest of this word, like VS Code.
                    self._suggest_dismissed = (model.row, model.start)
                    self.close_suggest()
                else:
                    self.accept_suggest()
                return
        if event.is_printable and event.character and not self.read_only:
            await super()._on_key(event)
            self._suggest_typed(event.character)
            return
        if event.key in ("backspace", "delete"):
            self._suggest_refilter = self.suggest is not None
        else:
            self.close_suggest()
        await super()._on_key(event)

    def on_text_area_changed(self, event: TextArea.Changed) -> None:
        if self._suggest_refilter:
            self._suggest_refilter = False
            self._refilter_suggest()

    def on_blur(self, event: events.Blur) -> None:
        self.close_suggest()

    def on_mouse_scroll_down(self, event: events.MouseScrollDown) -> None:
        self.close_suggest()

    def on_mouse_scroll_up(self, event: events.MouseScrollUp) -> None:
        self.close_suggest()

    def _suggest_typed(self, character: str) -> None:
        """A character was typed: narrow, open or close the list."""
        language = self.completion_language
        row, col = self.cursor_location
        before = self.document.get_line(row)[:col]
        if completion.is_word(character, completion.word_extra(language)):
            if self.suggest is not None:
                self._refilter_suggest()
            else:
                self.open_suggest()
        elif completion.is_trigger(language, before):
            self.open_suggest()
        else:
            self.close_suggest()

    def open_suggest(self, manual: bool = False) -> None:
        """Open the list at the cursor. `manual` = Ctrl+Space: no quiet zones, empty word allowed."""
        self.close_suggest()
        if self.selection.start != self.selection.end:
            return
        row, col = self.cursor_location
        line = self.document.get_line(row)
        language = self.completion_language
        start = completion.word_start(line, col, completion.word_extra(language))
        typed, before = line[start:col], line[:start]
        triggered = not typed and completion.is_trigger(language, before)
        if not manual:
            wordish = any(c.isalpha() for c in typed) or (typed != "" and all(c in "$@#" for c in typed))
            if (not completion.auto_suggest(language)
                    or completion.quiet_at(language, line, start, self._highlights.get(row, []))
                    or typed[:1].isdigit()
                    or not (triggered or wordish)
                    or self._suggest_dismissed == (row, start)):
                return
        others = [
            doc.document.lines for doc in (self.parent.children if self.parent else [])
            if isinstance(doc, CodeEditor) and doc is not self and doc.completion_language == language
        ]
        items = completion.candidates(completion.Context(language, self.document.lines, row, start, others))
        model = completion.Suggest(row, start, manual or triggered, items, completion.ignore_case(language))
        if model.filter(typed):
            self.suggest = model
            self._place_suggest()

    def _refilter_suggest(self) -> None:
        """The word under the cursor changed: filter again, or close when it no longer applies."""
        model = self.suggest
        if model is None:
            return
        row, col = self.cursor_location
        typed = self.document.get_line(row)[model.start:col]
        extra = completion.word_extra(self.completion_language)
        still_here = (row == model.row and col >= model.start and self.selection.start == self.selection.end
                      and all(completion.is_word(c, extra) for c in typed) and (model.explicit or typed))
        if still_here and model.filter(typed):
            self._place_suggest()
        else:
            self.close_suggest()

    def accept_suggest(self) -> None:
        """Insert the highlighted suggestion (one undo step)."""
        model, self.suggest = self.suggest, None
        self.close_suggest()
        item = model.current if model else None
        row, col = self.cursor_location
        if item is None or row != model.row or col < model.start:
            return
        line = self.document.get_line(row)
        indent = line[:len(line) - len(line.lstrip(" \t"))]
        text, select = completion.insert_text(
            item, line[model.start:col], indent, completion.ignore_case(self.completion_language))
        self.replace(text, (row, model.start), (row, col), maintain_selection_offset=False)
        if select is not None:
            def location(n: int) -> tuple[int, int]:
                lines = text[:n].split("\n")
                return (row + len(lines) - 1, (model.start if len(lines) == 1 else 0) + len(lines[-1]))
            self.selection = Selection(location(select[0]), location(select[1]))

    def close_suggest(self) -> None:
        self.suggest = None
        widget = getattr(self.app, "suggest_widget", None)
        if widget is not None and widget.editor is self:
            widget.hide()

    def _place_suggest(self) -> None:
        """Show the list under the start of the word (after the cursor has moved on screen)."""
        def place() -> None:
            widget = getattr(self.app, "suggest_widget", None)
            model = self.suggest
            if widget is None or model is None or not self.has_focus:
                return
            row, col = self.cursor_location
            offset = self.cursor_screen_offset
            typed = self.document.get_line(row)[model.start:col]
            widget.show(self, model, (offset.x - cell_len(typed), offset.y))

        self.call_after_refresh(place)

    def on_mouse_down(self, event: events.MouseDown) -> None:
        self.close_suggest()
        if event.button == 3:
            event.prevent_default()  # a right-click keeps the selection; it opens the menu

    def on_click(self, event: events.Click) -> None:
        if event.button == 3:
            event.stop()
            self.focus()
            self.app.context_menu(EDITOR_MENU, event.screen_x, event.screen_y)

    def on_mouse_move(self, event: events.MouseMove) -> None:
        """Hovering a squiggle shows what is wrong."""
        message = None
        if self.diagnostics:
            row, column = self.get_target_document_location(event)
            message = next((d.message for d in self.diagnostics if d.covers(row, column)), None)
        if message != self.tooltip:
            self.tooltip = message

    def update_dirty(self) -> None:
        # Cached: comparing the whole text is too slow to do on every repaint.
        self.dirty = self.text != self.saved_text

    def write_to_disk(self) -> None:
        assert self.path is not None
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.path.write_text(self.text, encoding="utf-8")
        self.saved_text = self.text
        self.dirty = False
        self.mtime = _mtime(self.path)

    def reload_if_changed(self) -> bool:
        """Pick up external edits (e.g. made by Claude) unless we have unsaved changes."""
        if self.path is None:
            return False
        mtime = _mtime(self.path)
        if mtime is None or mtime == self.mtime:
            return False
        self.mtime = mtime
        if self.dirty:
            return False
        try:
            text = self.path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return False
        if text == self.saved_text:
            return False
        location = self.cursor_location
        self.saved_text = text
        self.load_text(text)
        row = min(location[0], self.document.line_count - 1)
        self.move_cursor((row, min(location[1], len(self.document.get_line(row)))))
        return True


class NotEditable(Static):
    """Stand-in for files we can't show as text."""

    dirty = False
    language_name = "Binary"

    def __init__(self, path: Path, reason: str, id: str) -> None:
        super().__init__(reason, id=id, classes="not-editable")
        self.path = path
        self.title = path.name


def _mtime(path: Path | None) -> float | None:
    try:
        return path.stat().st_mtime if path else None
    except OSError:
        return None


@dataclass
class _Tab:
    doc_id: str
    start: int
    end: int


class TabBar(Widget):
    """Two rows: a thin accent line over the active tab, then the tab labels."""

    class Activated(Message):
        def __init__(self, doc_id: str) -> None:
            super().__init__()
            self.doc_id = doc_id

    class CloseRequested(Message):
        def __init__(self, doc_id: str) -> None:
            super().__init__()
            self.doc_id = doc_id

    def __init__(self) -> None:
        super().__init__(id="tabbar")
        self._docs: list = []
        self._active: str | None = None
        self._tabs: list[_Tab] = []

    def set_tabs(self, docs: list, active: str | None) -> None:
        self._docs, self._active = docs, active
        self.refresh()

    def render_line(self, y: int) -> Strip:
        theme: VSTheme = self.app.vs_theme
        strip_bg = theme["editorGroupHeader.tabsBackground"]
        segments: list[Segment] = []
        self._tabs = []
        x = 0
        for doc in self._docs:
            active = doc.id == self._active
            bg = theme["tab.activeBackground" if active else "tab.inactiveBackground"]
            fg = theme["tab.activeForeground" if active else "tab.inactiveForeground"]
            if getattr(doc, "error_count", 0):
                fg = theme["list.errorForeground"]
            elif getattr(doc, "warning_count", 0):
                fg = theme["list.warningForeground"]
            glyph, colour = file_icon(doc.path or doc.title, theme)
            mark = icon("dot") if doc.dirty else icon("close")
            label = Text.assemble(
                " ", (glyph + " ", colour), f"{doc.title}  ", (mark, fg if (active or doc.dirty) else bg), " ",
                style=Style(color=fg, bgcolor=bg),
            )
            width = label.cell_len
            self._tabs.append(_Tab(doc.id, x, x + width))
            if y == 0:
                line = "▔" if active else " "
                segments.append(Segment(line * width, Style(color=theme["tab.activeBorderTop"], bgcolor=bg)))
                segments.append(Segment(" ", Style(bgcolor=strip_bg)))
            else:
                segments.extend(label.render(self.app.console))
                segments.append(Segment("│", Style(color=theme["border"], bgcolor=strip_bg)))
            x += width + 1
        return Strip(segments).adjust_cell_length(self.size.width, Style(bgcolor=strip_bg))

    def on_mouse_down(self, event: events.MouseDown) -> None:
        for tab in self._tabs:
            if tab.start <= event.x < tab.end:
                event.stop()
                if event.button == 3:
                    self.app.tab_menu(tab.doc_id, event.screen_x, event.screen_y)
                elif event.button == 2 or event.x >= tab.end - 3:
                    self.post_message(self.CloseRequested(tab.doc_id))
                else:
                    self.post_message(self.Activated(tab.doc_id))
                return


WELCOME = [
    ("Show All Commands", "F1"),
    ("Go to File", "Ctrl+P"),
    ("Find in Files", "Search icon"),
    ("Toggle Terminal", "Ctrl+`"),
    ("Toggle Claude Code", "Ctrl+Alt+B"),
    ("Focus Next Part", "F6"),
]


class Welcome(Static):
    """The shortcut watermark VS Code shows when no editor is open."""

    def __init__(self) -> None:
        text = Text(justify="center")
        text.append(f"{icon('logo')}\n\n", style="bold")
        text.append("Terminal Studio Code\n\n\n")
        for label, keys in WELCOME:
            text.append(f"{label:>20}   ")
            text.append(f"{keys:<14}\n\n", style="bold")
        super().__init__(text, id="welcome")


class EditorArea(Vertical):
    class StateChanged(Message):
        """Active document, cursor or dirty state changed."""

    class DiagnosticsChanged(Message):
        """Some document's problems changed."""

    def apply_theme(self, theme: VSTheme) -> None:
        self.vs_theme = theme
        for doc in self.docs:
            if isinstance(doc, CodeEditor):
                doc.apply_theme(theme)
        self.query_one(TabBar).refresh()

    def __init__(self, workspace: Path, theme: VSTheme) -> None:
        super().__init__(id="editor-area")
        self.workspace = workspace
        self.vs_theme = theme
        self._counter = 0
        self._untitled = 0

    def compose(self) -> ComposeResult:
        yield TabBar()
        yield Static("", id="breadcrumbs")
        with ContentSwitcher(initial="welcome", id="editors"):
            yield Welcome()

    def on_mount(self) -> None:
        self.set_interval(1.5, self._poll_disk)
        self._sync()

    # -- queries --------------------------------------------------------------

    @property
    def switcher(self) -> ContentSwitcher:
        return self.query_one("#editors", ContentSwitcher)

    @property
    def docs(self) -> list:
        return [w for w in self.switcher.children if not isinstance(w, Welcome)]

    @property
    def active(self) -> CodeEditor | NotEditable | None:
        current = self.switcher.current
        for doc in self.docs:
            if doc.id == current:
                return doc
        return None

    @property
    def active_editor(self) -> CodeEditor | None:
        doc = self.active
        return doc if isinstance(doc, CodeEditor) else None

    def dirty_docs(self) -> list[CodeEditor]:
        return [d for d in self.docs if d.dirty]

    # -- opening --------------------------------------------------------------

    def _next_id(self) -> str:
        self._counter += 1
        return f"doc-{self._counter}"

    def open_file(self, path: Path, line: int | None = None) -> None:
        path = path.resolve()
        for doc in self.docs:
            if doc.path == path:
                self.activate(doc.id, line)
                return
        try:
            if path.stat().st_size > MAX_FILE_BYTES:
                raise ValueError("The file is not displayed in the text editor because it is too large.")
            data = path.read_bytes()
            if b"\0" in data[:8192]:
                raise ValueError(
                    "The file is not displayed in the text editor because it is either binary "
                    "or uses an unsupported text encoding."
                )
            doc = CodeEditor(data.decode("utf-8"), path, path.name, self.vs_theme, self._next_id())
        except OSError as exc:
            self.app.notify(f"Unable to open '{path.name}': {exc.strerror or exc}", severity="error")
            return
        except ValueError as exc:  # includes UnicodeDecodeError
            reason = str(exc) if not isinstance(exc, UnicodeDecodeError) else (
                "The file is not displayed in the text editor because it is either binary "
                "or uses an unsupported text encoding."
            )
            doc = NotEditable(path, reason, self._next_id())
        self._add(doc, line)

    def new_untitled(self) -> None:
        self._untitled += 1
        title = f"Untitled-{self._untitled}"
        self._add(CodeEditor("", None, title, self.vs_theme, self._next_id()))

    def _add(self, doc, line: int | None = None) -> None:
        self.switcher.mount(doc)
        self.activate(doc.id, line)

    def activate(self, doc_id: str, line: int | None = None) -> None:
        for doc in self.docs:
            if isinstance(doc, CodeEditor):
                doc.close_suggest()
        self.switcher.current = doc_id
        doc = self.active
        if isinstance(doc, CodeEditor):
            if line is not None:
                row = max(0, min(line - 1, doc.document.line_count - 1))
                doc.move_cursor((row, 0), center=True)
            doc.focus()
        self._sync()

    def cycle(self, step: int) -> None:
        docs = self.docs
        if len(docs) > 1 and self.active:
            index = (docs.index(self.active) + step) % len(docs)
            self.activate(docs[index].id)

    # -- saving ---------------------------------------------------------------

    def save(self, doc: CodeEditor | None = None, then=None, save_as: bool = False) -> None:
        """Save `doc` (default: the active editor). `then` runs after a successful save."""
        doc = doc or self.active_editor
        if doc is None:
            return
        if doc.path is None or save_as:
            def chosen(value: str | None) -> None:
                if not value:
                    return
                path = (self.workspace / Path(value).expanduser()).resolve()
                doc.path, doc.title = path, path.name
                doc.apply_language()
                self._write(doc, then)

            start = str(doc.path.relative_to(self.workspace)) if (
                doc.path and doc.path.is_relative_to(self.workspace)
            ) else (str(doc.path) if doc.path else "")
            self.app.push_screen(
                InputModal("Save As - path relative to the workspace folder (Enter to confirm, Esc to cancel)",
                           value=start, placeholder="file name"),
                chosen,
            )
        else:
            self._write(doc, then)

    def save_all(self) -> None:
        """Save every file with changes. Untitled ones ask for a name, one at a time."""
        for doc in self.dirty_docs():
            if doc.path is not None:
                self._write(doc)
        self._save_next_untitled()

    def _save_next_untitled(self) -> None:
        doc = next((d for d in self.dirty_docs() if d.path is None), None)
        if doc is not None:
            self.activate(doc.id)  # so you can see which file the name is for
            self.save(doc, then=self._save_next_untitled)

    def _write(self, doc: CodeEditor, then=None) -> None:
        try:
            doc.write_to_disk()
        except OSError as exc:
            self.app.notify(f"Failed to save '{doc.title}': {exc.strerror or exc}", severity="error")
            return
        self._sync()
        if then:
            then()

    # -- closing --------------------------------------------------------------

    def close(self, doc_id: str | None = None) -> None:
        doc = next((d for d in self.docs if d.id == doc_id), None) if doc_id else self.active
        if doc is None:
            return
        if not doc.dirty:
            self._remove(doc)
            return

        def answered(choice: str | None) -> None:
            if choice == "Save":
                self.save(doc, then=lambda: self._remove(doc))
            elif choice == "Don't Save":
                self._remove(doc)

        self.app.push_screen(
            ChoiceModal(
                f"Do you want to save the changes you made to {doc.title}?",
                "Your changes will be lost if you don't save them.",
                ["Save", "Don't Save", "Cancel"],
            ),
            answered,
        )

    def _remove(self, doc) -> None:
        docs = self.docs
        index = docs.index(doc)
        remaining = [d for d in docs if d is not doc]
        if self.switcher.current == doc.id:
            if remaining:
                self.switcher.current = remaining[min(index, len(remaining) - 1)].id
            else:
                self.switcher.current = "welcome"
        doc.remove()
        self._sync(remaining)
        if self.active_editor:
            self.active_editor.focus()

    # -- state ----------------------------------------------------------------

    def _poll_disk(self) -> None:
        changed = [d for d in self.docs if isinstance(d, CodeEditor) and d.reload_if_changed()]
        if changed:
            self._sync()

    def _sync(self, docs: list | None = None) -> None:
        docs = self.docs if docs is None else docs
        active = self.active
        self.query_one(TabBar).set_tabs(docs, active.id if active else None)
        self.set_class(not docs, "-empty")
        crumbs = self.query_one("#breadcrumbs", Static)
        if active and active.path:
            path = active.path
            parts = path.relative_to(self.workspace).parts if path.is_relative_to(self.workspace) else path.parts
            crumbs.update(f" {icon('sep')} ".join(parts))
        else:
            crumbs.update(active.title if active else "")
        self.post_message(self.StateChanged())

    def on_tab_bar_activated(self, event: TabBar.Activated) -> None:
        self.activate(event.doc_id)

    def on_tab_bar_close_requested(self, event: TabBar.CloseRequested) -> None:
        self.close(event.doc_id)

    def on_text_area_changed(self, event: TextArea.Changed) -> None:
        if isinstance(event.text_area, CodeEditor):
            event.text_area.update_dirty()
            event.text_area.schedule_check()
        self._sync()

    def on_text_area_selection_changed(self, event: TextArea.SelectionChanged) -> None:
        self.post_message(self.StateChanged())
