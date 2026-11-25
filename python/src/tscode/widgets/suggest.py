"""The suggest widget: VS Code's completion list, hanging under the word being typed.

One instance lives on the main screen (overlay layer, absolutely positioned). The
focused `CodeEditor` drives it: it opens it, moves the highlight and accepts from it;
the widget only paints and turns clicks / the wheel into the same calls.
"""

from __future__ import annotations

from rich.cells import cell_len
from rich.segment import Segment
from rich.style import Style
from textual import events
from textual.strip import Strip
from textual.widget import Widget

from ..completion import Suggest


class SuggestWidget(Widget):
    can_focus = False

    def __init__(self) -> None:
        super().__init__(id="suggest")
        self.model: Suggest | None = None
        self.editor = None  # the CodeEditor that owns the open list
        self._hover: int | None = None
        self.display = False

    # -- opening and placing -----------------------------------------------------

    def show(self, editor, model: Suggest, anchor: tuple[int, int]) -> None:
        """Show `model` for a word starting at screen cell `anchor`."""
        self.editor, self.model = editor, model
        model.keep_visible()
        screen_w, screen_h = self.app.size
        top, bottom = 1, screen_h - 1  # between the title bar and the status bar
        label_w = min(40, max((cell_len(item.label) for item, _ in model.shown), default=0))
        detail_w = min(26, max((cell_len(item.detail) for item, _ in model.shown), default=0))
        width = min(max(label_w + (detail_w + 3 if detail_w else 0) + 6, 26), 70, screen_w)
        height = min(model.visible + 2, bottom - top)
        # Icon and a space sit left of the word, so the labels line up with what you typed.
        x = max(0, min(anchor[0] - 4, screen_w - width))
        y = anchor[1] + 1
        if y + height > bottom:
            y = anchor[1] - height if anchor[1] - height >= top else max(top, bottom - height)
        self.styles.offset = (x, y)
        self.styles.width = width
        self.styles.height = height
        self.display = True
        self.refresh()

    def hide(self) -> None:
        self.model = None
        self.editor = None
        self._hover = None
        self.display = False

    @property
    def is_open(self) -> bool:
        return self.model is not None

    # -- painting --------------------------------------------------------------------

    def render_line(self, y: int) -> Strip:
        theme = self.app.vs_theme
        width, height = self.size.width, self.size.height
        bg = theme["editorSuggestWidget.background"]
        fg = theme["editorSuggestWidget.foreground"]
        border = Style(color=theme["editorSuggestWidget.border"], bgcolor=bg)
        base = Style(color=fg, bgcolor=bg)
        model = self.model
        if model is None or width < 4:
            return Strip.blank(width, base)
        if y == 0 or y == height - 1:
            left, right = ("╭", "╮") if y == 0 else ("╰", "╯")
            return Strip([Segment(left + "─" * (width - 2) + right, border)])
        rows = height - 2
        index = model.top + y - 1
        if index >= len(model.shown):
            return Strip([Segment("│", border), Segment(" " * (width - 2), base), Segment("│", border)])
        item, positions = model.shown[index]
        selected = index == model.selected
        if selected:
            row_bg = theme["editorSuggestWidget.selectedBackground"]
            row_fg = theme["editorSuggestWidget.selectedForeground"]
            highlight = theme["editorSuggestWidget.focusHighlightForeground"]
        else:
            row_bg = theme["list.hoverBackground"] if index == self._hover else bg
            row_fg, highlight = fg, theme["editorSuggestWidget.highlightForeground"]
        row = Style(color=row_fg, bgcolor=row_bg)
        icon_colour = theme["icon.monoForeground"] if theme.mono_icons else theme[item.kind.colour_key]
        inner = width - 2
        segments = [Segment(" ", row), Segment(item.kind.glyph, Style(color=icon_colour, bgcolor=row_bg)), Segment(" ", row)]
        used = 1 + cell_len(item.kind.glyph) + 1
        room = inner - used - 1
        label = item.label
        hits = set(positions)
        shown = 0
        for i, c in enumerate(label):
            w = cell_len(c)
            if shown + w > room:
                break
            segments.append(Segment(c, Style(color=highlight, bgcolor=row_bg, bold=True) if i in hits else row))
            shown += w
        used += shown
        if selected and item.detail:
            space = inner - used - 3
            if space >= 4:
                detail = item.detail if cell_len(item.detail) <= space else item.detail[: space - 1] + "…"
                gap = inner - used - cell_len(detail) - 1
                segments.append(Segment(" " * gap, row))
                segments.append(Segment(detail, Style(color=theme["descriptionForeground"], bgcolor=row_bg)))
                used += gap + cell_len(detail)
        segments.append(Segment(" " * max(0, inner - used), row))
        # A thumb on the right edge when the list scrolls.
        edge, edge_style = "│", border
        total = len(model.shown)
        if total > rows > 0:
            size = max(1, rows * rows // total)
            offset = model.top * (rows - size) // max(1, total - rows)
            if offset <= y - 1 < offset + size:
                edge, edge_style = "┃", Style(color=theme["scrollbarSlider.hoverBackground"], bgcolor=bg)
        return Strip([Segment("│", border), *segments, Segment(edge, edge_style)]).crop(0, width)

    # -- mouse -------------------------------------------------------------------------

    def _row_at(self, event: events.MouseEvent) -> int | None:
        if self.model is None or not (0 < event.y < self.size.height - 1 and 0 < event.x < self.size.width - 1):
            return None
        index = self.model.top + event.y - 1
        return index if index < len(self.model.shown) else None

    def on_mouse_move(self, event: events.MouseMove) -> None:
        hover = self._row_at(event)
        if hover != self._hover:
            self._hover = hover
            self.refresh()

    def on_leave(self, event: events.Leave) -> None:
        self._hover = None
        self.refresh()

    def on_mouse_down(self, event: events.MouseDown) -> None:
        event.stop()
        event.prevent_default()  # the editor keeps focus

    def on_click(self, event: events.Click) -> None:
        event.stop()
        index = self._row_at(event)
        if index is not None and self.model is not None and self.editor is not None:
            self.model.selected = index
            self.editor.accept_suggest()

    def on_mouse_scroll_down(self, event: events.MouseScrollDown) -> None:
        event.stop()
        if self.model:
            self.model.scroll(1)
            self.refresh()

    def on_mouse_scroll_up(self, event: events.MouseScrollUp) -> None:
        event.stop()
        if self.model:
            self.model.scroll(-1)
            self.refresh()
