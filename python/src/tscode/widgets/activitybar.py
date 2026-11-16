"""The icon strip on the far left."""

from __future__ import annotations

from textual import events
from textual.app import ComposeResult
from textual.containers import Vertical
from textual.message import Message
from textual.widgets import Static

from ..icons import icon

VIEWS = [
    ("explorer", "files", "Explorer (Ctrl+Shift+E)"),
    ("search", "search", "Search"),
    ("scm", "scm", "Source Control"),
    ("debug", "debug", "Run and Debug"),
    ("extensions", "extensions", "Extensions"),
]


class ActivityItem(Static):
    def __init__(self, item_id: str, icon_name: str, tooltip: str) -> None:
        super().__init__(icon(icon_name), id=f"activity-{item_id}", classes="activity-item")
        self.item_id = item_id
        self.tooltip = tooltip

    def on_click(self, event: events.Click) -> None:
        event.stop()
        self.post_message(ActivityBar.Selected(self.item_id))


class ActivityBar(Vertical):
    class Selected(Message):
        def __init__(self, item_id: str) -> None:
            super().__init__()
            self.item_id = item_id

    def __init__(self) -> None:
        super().__init__(id="activitybar")

    def compose(self) -> ComposeResult:
        for item_id, icon_name, tooltip in VIEWS:
            yield ActivityItem(item_id, icon_name, tooltip)
        yield ActivityItem("claude", "claude", "Agent Panel (Ctrl+Alt+B)")
        yield Static(classes="spacer")
        yield ActivityItem("account", "account", "Accounts")
        yield ActivityItem("settings", "gear", "Manage")

    def set_active(self, view: str | None, claude_open: bool) -> None:
        """Mark the visible side-bar view (None = side bar hidden) and the Claude toggle."""
        for item in self.query(ActivityItem):
            active = item.item_id == view or (item.item_id == "claude" and claude_open)
            item.set_class(active, "-active")
