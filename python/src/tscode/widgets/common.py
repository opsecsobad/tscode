"""Small shared widgets: clickable icon buttons, splitters and modal dialogs."""

from __future__ import annotations

from rich.text import Text
from textual import events
from textual.app import ComposeResult
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widget import Widget
from textual.widgets import Button, Input, Label, OptionList, Static
from textual.widgets.option_list import Option


class ActionButton(Static):
    """A label that runs an app action when clicked (title-bar / header icons)."""

    def __init__(self, label: str, action: str, tooltip: str | None = None, **kwargs) -> None:
        super().__init__(label, **kwargs)
        self.app_action = action
        if tooltip:
            self.tooltip = tooltip

    async def on_click(self, event: events.Click) -> None:
        event.stop()
        await self.app.run_action(self.app_action)


class Splitter(Widget):
    """A draggable divider that resizes `target`.

    `edge` says which side of the target the splitter sits on:
    "right" (target is left of it), "left" (target is right of it) or
    "top" (target is below it).
    """

    def __init__(self, target: str, edge: str, minimum: int, **kwargs) -> None:
        super().__init__(classes=f"splitter -{'h' if edge == 'top' else 'v'}", **kwargs)
        self.target_selector = target
        self.edge = edge
        self.minimum = minimum
        self._dragging = False

    def render(self) -> Text:
        if self.edge == "top":
            return Text("─" * self.size.width)
        return Text("\n".join("│" * self.size.height))

    def on_mouse_down(self, event: events.MouseDown) -> None:
        self._dragging = True
        self.capture_mouse()
        event.stop()

    def on_mouse_up(self, event: events.MouseUp) -> None:
        self._dragging = False
        self.release_mouse()
        event.stop()

    def on_mouse_move(self, event: events.MouseMove) -> None:
        if not self._dragging:
            return
        target = self.app.query_one(self.target_selector)
        region = target.region
        screen = self.app.size
        if self.edge == "right":
            size = event.screen_x - region.x
            target.styles.width = max(self.minimum, min(size, screen.width - 30))
        elif self.edge == "left":
            size = region.right - event.screen_x - 1
            target.styles.width = max(self.minimum, min(size, screen.width - 30))
        else:
            size = region.bottom - event.screen_y - 1
            target.styles.height = max(self.minimum, min(size, screen.height - 8))


class MenuScreen(ModalScreen[str | None]):
    """A dropdown menu. Items are (label, shortcut, action) or None for a separator.

    `anchor_bottom` places the menu so it ends just above row `y` (menus that open upward).
    """

    BINDINGS = [("escape", "dismiss(None)", "Close")]

    def __init__(self, items: list[tuple[str, str, str] | None], x: int, y: int = 1,
                 anchor_bottom: bool = False) -> None:
        super().__init__()
        self.items = items
        self.x, self.y = x, (y - len(items) - 2 if anchor_bottom else y)
        self.items = items

    def compose(self) -> ComposeResult:
        rows = [i for i in self.items if i]
        left = max(len(i[0]) for i in rows)
        right = max(len(i[1]) for i in rows)
        options: list[Option | None] = []
        for item in self.items:
            if item is None:
                options.append(None)
                continue
            label, shortcut, action = item
            text = Text(" " + label.ljust(left + 4))
            text.append(shortcut.rjust(right) + " ", style="dim")
            options.append(Option(text, id=action))
        # The box gives the rounded border a solid backdrop (a border drawn straight on the
        # transparent modal would show the wrong colour behind its corners).
        box = Vertical(OptionList(*options, id="menu"), classes="popup-box")
        box.styles.offset = (max(0, self.x), max(0, self.y))
        box.styles.width = left + right + 8
        box.styles.height = len(self.items) + 2
        yield box

    def on_option_list_option_selected(self, event: OptionList.OptionSelected) -> None:
        self.dismiss(event.option.id)

    def on_click(self, event: events.Click) -> None:
        if event.widget is self:
            self.dismiss(None)


class ChoiceModal(ModalScreen[str | None]):
    """VS Code style confirmation dialog. Returns the label of the chosen button."""

    BINDINGS = [("escape", "dismiss(None)", "Cancel")]

    def __init__(self, message: str, detail: str, buttons: list[str]) -> None:
        super().__init__()
        self.message, self.detail, self.buttons = message, detail, buttons

    def compose(self) -> ComposeResult:
        with Vertical(classes="popup-box dialog-box"):
            with Vertical(classes="dialog"):
                yield Label(self.message, classes="dialog-title")
                if self.detail:
                    yield Label(self.detail, classes="dialog-detail")
                with Horizontal(classes="dialog-buttons"):
                    for index, label in enumerate(self.buttons):
                        yield Button(label, classes="default-button" if index == 0 else "", name=label)

    def on_button_pressed(self, event: Button.Pressed) -> None:
        self.dismiss(event.button.name)


class InputModal(ModalScreen[str | None]):
    """A quick-input box at the top of the window (like VS Code's Save As prompt)."""

    BINDINGS = [("escape", "dismiss(None)", "Cancel")]

    def __init__(self, prompt: str, value: str = "", placeholder: str = "") -> None:
        super().__init__()
        self.prompt, self.value, self.placeholder = prompt, value, placeholder

    def compose(self) -> ComposeResult:
        with Vertical(classes="popup-box quick-box"):
            with Vertical(classes="quick-input"):
                yield Input(self.value, placeholder=self.placeholder)
                yield Label(self.prompt, classes="quick-hint")

    def on_input_submitted(self, event: Input.Submitted) -> None:
        self.dismiss(event.value.strip() or None)

    def on_click(self, event: events.Click) -> None:
        if event.widget is self:
            self.dismiss(None)


class QuickPick(ModalScreen[str | None]):
    """VS Code's quick pick: a list at the top of the window. Returns the chosen id.

    `on_highlight(id)` fires as the selection moves, for live previews (theme picker).
    """

    BINDINGS = [("escape", "dismiss(None)", "Cancel")]

    def __init__(self, title: str, choices: list[tuple[str, str]], current: str | None = None,
                 on_highlight=None) -> None:
        super().__init__()
        self.title_text, self.choices, self.current = title, choices, current
        self.on_highlight = on_highlight

    def compose(self) -> ComposeResult:
        options = [Option(" " + label, id=choice_id) for choice_id, label in self.choices]
        with Vertical(classes="popup-box quick-box"):
            with Vertical(classes="quick-input"):
                yield Label(self.title_text, classes="quick-hint")
                yield OptionList(*options, id="quick-list")

    def on_mount(self) -> None:
        ids = [choice_id for choice_id, _ in self.choices]
        if self.current in ids:
            self.query_one(OptionList).highlighted = ids.index(self.current)

    def on_option_list_option_highlighted(self, event: OptionList.OptionHighlighted) -> None:
        if self.on_highlight and event.option.id:
            self.on_highlight(event.option.id)

    def on_option_list_option_selected(self, event: OptionList.OptionSelected) -> None:
        self.dismiss(event.option.id)

    def on_click(self, event: events.Click) -> None:
        if event.widget is self:
            self.dismiss(None)
