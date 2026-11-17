"""An embedded terminal emulator widget.

A child process runs on a pty; its output is parsed by pyte into a screen
buffer which we paint with `render_line`. Used for both the bottom-panel shell
and the Claude Code panel.
"""

from __future__ import annotations

import asyncio
import codecs
import fcntl
import os
import pty
import re
import signal
import struct
import subprocess
import sys
import termios
import time
from functools import lru_cache
from pathlib import Path

import pyte
from rich.segment import Segment
from rich.style import Style
from textual import events
from textual.geometry import Size
from textual.message import Message
from textual.strip import Strip
from textual.widget import Widget

from ..theme import DEFAULT_THEME, VSTheme

# Sequences pyte misparses (it would print stray characters, restore the cursor
# or switch on underline): kitty keyboard protocol, XTVERSION, modifyOtherKeys,
# DCS and APC strings. Strip them before feeding.
_STRIP = re.compile(
    r"\x1b\[[<>=][0-9;:]*[a-zA-Z~]"
    r"|\x1b\[\?[0-9;]*u"
    r"|\x1bP.*?\x1b\\"
    r"|\x1b_.*?\x1b\\",
    re.DOTALL,
)
# An escape sequence cut off at the end of a read; held back until the rest arrives.
_PARTIAL = re.compile(r"\x1b(\[[<>=?]?[0-9;:]*|[P_][^\x1b]*\x1b?)?$", re.DOTALL)

_TRAMPOLINE = """
import fcntl, os, sys, termios
fcntl.ioctl(0, termios.TIOCSCTTY, 0)
try:
    os.execvp(sys.argv[1], sys.argv[1:])
except OSError as exc:
    sys.exit(f"tscode: cannot run {sys.argv[1]}: {exc.strerror}")
"""

_ALT_MODES = (47, 1047, 1049)
_PRIV = 5  # pyte stores private modes shifted left by 5

_KEYS = {
    "enter": "\r",
    "tab": "\t",
    "shift+tab": "\x1b[Z",
    "backspace": "\x7f",
    "ctrl+backspace": "\x17",
    "alt+backspace": "\x1b\x7f",
    "escape": "\x1b",
    "shift+enter": "\x1b\r",
    "alt+enter": "\x1b\r",
    "ctrl+enter": "\n",
    "delete": "\x1b[3~",
    "ctrl+delete": "\x1b[3;5~",
    "insert": "\x1b[2~",
    "pageup": "\x1b[5~",
    "pagedown": "\x1b[6~",
    "ctrl+@": "\x00",
    "ctrl+space": "\x00",
    "f1": "\x1bOP",
    "f2": "\x1bOQ",
    "f3": "\x1bOR",
    "f4": "\x1bOS",
    "f5": "\x1b[15~",
    "f7": "\x1b[18~",
    "f8": "\x1b[19~",
    "f9": "\x1b[20~",
    "f10": "\x1b[21~",
    "f11": "\x1b[23~",
    "f12": "\x1b[24~",
}
_CURSOR_KEYS = {"up": "A", "down": "B", "right": "C", "left": "D", "home": "H", "end": "F"}
_MODIFIERS = {"shift": 2, "alt": 3, "ctrl": 5, "ctrl+shift": 6}

# Keys the terminal never swallows, so the app can always be driven from inside it.
PASSTHROUGH = {"f1", "f6", "shift+f6", "ctrl+q", "ctrl+grave_accent"}

# Characters that end a word for double-click selection (like VS Code's terminal).
_WORD_BREAKS = set(" \t()[]{}<>'\"`|,;│─")


class _Screen(pyte.HistoryScreen):
    """pyte screen + alternate-screen support + replies routed to the pty."""

    def __init__(self, columns: int, lines: int, on_reply) -> None:
        super().__init__(columns, lines, history=5000, ratio=0.5)
        self.in_alt = False
        self._saved = None
        self._on_reply = on_reply
        self._drawing = False

    def draw(self, data: str) -> None:
        self._drawing = True
        try:
            super().draw(data)
        finally:
            self._drawing = False

    def linefeed(self) -> None:
        # Mark rows that ran past the right edge (autowrap happens inside draw), so a
        # copied selection joins them back into one line. A newline the program sent
        # clears the mark when the row is reused.
        self.buffer[self.cursor.y].wrapped = self._drawing
        super().linefeed()

    def write_process_input(self, data: str) -> None:
        self._on_reply(data)

    def set_mode(self, *modes, **kwargs) -> None:
        if kwargs.get("private") and any(m in _ALT_MODES for m in modes) and not self.in_alt:
            self._saved = (self.buffer, self.cursor.x, self.cursor.y)
            self.buffer = type(self.buffer)(self.buffer.default_factory)
            self.in_alt = True
            self.dirty.update(range(self.lines))
        super().set_mode(*modes, **kwargs)

    def reset_mode(self, *modes, **kwargs) -> None:
        if kwargs.get("private") and any(m in _ALT_MODES for m in modes) and self.in_alt:
            self.buffer, x, y = self._saved
            self.cursor.x, self.cursor.y = min(x, self.columns - 1), min(y, self.lines - 1)
            self._saved = None
            self.in_alt = False
            self.dirty.update(range(self.lines))
        super().reset_mode(*modes, **kwargs)

    def index(self) -> None:
        if self.in_alt:  # the alternate screen has no scrollback
            pyte.Screen.index(self)
        else:
            super().index()

    def has_mode(self, private_mode: int) -> bool:
        return (private_mode << _PRIV) in self.mode


class Terminal(Widget, can_focus=True):
    """A pty-backed terminal. Spawns `command` the first time it gets a size."""

    ALLOW_SELECT = False  # selection is our own (it has to reach into the scrollback)

    class Exited(Message):
        def __init__(self, terminal: "Terminal") -> None:
            super().__init__()
            self.terminal = terminal

    def __init__(
        self,
        command: list[str],
        cwd: Path | str,
        *,
        theme: VSTheme = DEFAULT_THEME,
        env: dict[str, str] | None = None,
        id: str | None = None,
        classes: str | None = None,
    ) -> None:
        super().__init__(id=id, classes=classes)
        self.command = command
        self.cwd = str(cwd)
        self.extra_env = env or {}
        self.vs_theme = theme
        self._screen = _Screen(80, 24, self._reply)
        self._stream = pyte.Stream(self._screen)
        self._decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self._carry = ""
        self._fd: int | None = None
        self._pid: int | None = None
        self._proc: subprocess.Popen | None = None
        self._out = b""
        self._scroll = 0  # lines scrolled back into history
        self.exited = False
        # Mouse selection: (anchor, head) as (history line, column), where history line 0
        # is the oldest line in the scrollback. The end of a selection is inclusive here.
        self._selection: tuple[tuple[int, int], tuple[int, int]] | None = None
        self._selecting = False
        self._last_click: tuple[float, tuple[int, int], int] | None = None

    # -- process -----------------------------------------------------------

    @property
    def running(self) -> bool:
        return self._fd is not None

    def _spawn(self) -> None:
        cols, rows = max(self.size.width, 2), max(self.size.height, 2)
        self._screen = _Screen(cols, rows, self._reply)
        self._stream = pyte.Stream(self._screen)
        self._decoder.reset()
        self._carry, self._scroll, self.exited = "", 0, False
        self._selection = None
        env = {**os.environ, "TERM": "xterm-256color", "COLORTERM": "truecolor", **self.extra_env}
        env.pop("COLUMNS", None)
        env.pop("LINES", None)
        # Spawn through subprocess (not os.forkpty): forking a threaded Python process
        # can deadlock the child. The trampoline makes the pty its controlling terminal.
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        try:
            self._proc = subprocess.Popen(
                [sys.executable, "-c", _TRAMPOLINE, *self.command],
                stdin=slave, stdout=slave, stderr=slave,
                cwd=self.cwd, env=env, start_new_session=True,
            )
        finally:
            os.close(slave)
        pid, fd = self._proc.pid, master
        self._pid, self._fd = pid, fd
        os.set_blocking(fd, False)
        self._set_winsize(rows, cols)
        asyncio.get_running_loop().add_reader(fd, self._on_readable)

    def _set_winsize(self, rows: int, cols: int) -> None:
        if self._fd is not None:
            fcntl.ioctl(self._fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def _on_readable(self) -> None:
        try:
            data = os.read(self._fd, 65536)
        except BlockingIOError:
            return
        except OSError:
            data = b""
        if not data:
            self._closed()
            return
        self.feed(self._decoder.decode(data))

    def feed(self, text: str) -> None:
        """Feed terminal output (already decoded) into the emulator."""
        text = self._carry + text
        self._carry = ""
        partial = _PARTIAL.search(text)
        if partial and len(partial.group()) < 4096:
            self._carry = partial.group()
            text = text[: partial.start()]
        self._stream.feed(_STRIP.sub("", text))
        self._screen.dirty.clear()
        self.refresh()

    def _closed(self) -> None:
        loop = asyncio.get_running_loop()
        if self._fd is not None:
            loop.remove_reader(self._fd)
            loop.remove_writer(self._fd)
            try:
                os.close(self._fd)
            except OSError:
                pass
        self._reap()
        self._fd = self._pid = None
        self._out = b""
        self.exited = True
        self.feed("\r\n\x1b[0;2m[Process exited - press Enter to restart]\x1b[0m")
        self.post_message(self.Exited(self))

    def kill(self) -> None:
        if self._pid is not None:
            try:
                os.killpg(self._pid, signal.SIGHUP)
            except ProcessLookupError:
                pass
            if self._fd is not None:
                loop = asyncio.get_running_loop()
                loop.remove_reader(self._fd)
                loop.remove_writer(self._fd)
                os.close(self._fd)
            self._reap()
            self._fd = self._pid = None
            self._out = b""

    def _reap(self) -> None:
        proc, self._proc = self._proc, None
        if proc is not None:
            try:
                proc.wait(timeout=0.2)
            except subprocess.TimeoutExpired:
                pass  # still shutting down; Popen reaps it later

    def restart(self) -> None:
        self.kill()
        if self.size.width and self.size.height:
            self._spawn()
            self.refresh()

    def on_unmount(self) -> None:
        self.kill()

    # -- writing to the process ---------------------------------------------

    def _reply(self, data: str) -> None:
        self.write(data)

    def write(self, data: str | bytes) -> None:
        """Send input to the process."""
        if self._fd is None:
            return
        self._out += data.encode() if isinstance(data, str) else data
        self._flush()

    def _flush(self) -> None:
        if self._fd is None:
            return
        loop = asyncio.get_running_loop()
        try:
            sent = os.write(self._fd, self._out)
            self._out = self._out[sent:]
        except BlockingIOError:
            pass
        except OSError:
            self._out = b""
        if self._out:
            loop.add_writer(self._fd, self._flush)
        else:
            loop.remove_writer(self._fd)

    def send_line(self, text: str) -> None:
        """Type a command line into the terminal and press Enter."""
        self.write(text + "\r")

    # -- events ---------------------------------------------------------------

    def on_resize(self, event: events.Resize) -> None:
        cols, rows = event.size.width, event.size.height
        if cols < 2 or rows < 2:
            return
        if self._fd is None:
            if not self.exited:
                self._spawn()
            return
        self._screen.resize(rows, cols)
        self._set_winsize(rows, cols)
        self._scroll = 0
        self._selection = None  # the text reflows, so the old cells no longer mean anything
        self.refresh()

    def on_focus(self) -> None:
        self.refresh()

    def on_blur(self) -> None:
        self.refresh()

    def on_key(self, event: events.Key) -> None:
        key = event.key
        if key in PASSTHROUGH:
            return
        event.stop()
        event.prevent_default()
        if self.exited:
            if key == "enter":
                self.restart()
            return
        # Ctrl+C (or Ctrl+Shift+C) with text selected copies it instead of interrupting.
        if key in ("ctrl+c", "ctrl+shift+c") and self.copy_selection():
            return
        if key in ("shift+pageup", "shift+pagedown"):
            self._scroll_by(self.size.height - 1 if key == "shift+pageup" else 1 - self.size.height)
            return
        data = self._translate(key, event.character)
        if data:
            if self._selection is not None:  # typing ends a selection, like in VS Code
                self._selection = None
                self.refresh()
            if self._scroll:
                self._scroll = 0
                self.refresh()
            self.write(data)

    def _translate(self, key: str, character: str | None) -> str:
        if key in _KEYS:
            return _KEYS[key]
        mods, _, base = key.rpartition("+")
        if base in _CURSOR_KEYS:
            final = _CURSOR_KEYS[base]
            if not mods:
                return ("\x1bO" if self._screen.has_mode(1) else "\x1b[") + final
            if mods in _MODIFIERS:
                return f"\x1b[1;{_MODIFIERS[mods]}{final}"
            return ""
        if mods == "alt":
            return "\x1b" + (self._translate(base, None) or (base if len(base) == 1 else ""))
        if mods == "ctrl" and len(base) == 1 and base.isalpha():
            return chr(ord(base.lower()) & 0x1F)
        return character or ""

    def on_paste(self, event: events.Paste) -> None:
        event.stop()
        self.paste(event.text)

    def paste(self, text: str) -> None:
        if not text:
            return
        if self._selection is not None:
            self._selection = None
            self.refresh()
        text = text.replace("\r\n", "\r").replace("\n", "\r")
        if self._screen.has_mode(2004):
            text = f"\x1b[200~{text}\x1b[201~"
        self.write(text)

    def _mouse_tracking(self) -> bool:
        s = self._screen
        return s.has_mode(1006) and (s.has_mode(1000) or s.has_mode(1002) or s.has_mode(1003))

    def _send_mouse(self, code: int, x: int, y: int, release: bool = False) -> None:
        self.write(f"\x1b[<{code};{x + 1};{y + 1}{'m' if release else 'M'}")

    def on_click(self, event: events.Click) -> None:
        if event.button == 3 and not self._mouse_tracking():
            event.stop()
            self.app.terminal_menu(self, event.screen_x, event.screen_y)

    def on_mouse_down(self, event: events.MouseDown) -> None:
        self.focus()
        # Programs that track the mouse get the click; Shift+drag selects text anyway.
        if self._mouse_tracking() and not event.shift:
            self._send_mouse({1: 0, 2: 1, 3: 2}.get(event.button, 0), event.x, event.y)
        elif event.button == 1:
            self._select_start(event.x, event.y, extend=event.shift)
            self._selecting = True
            self.capture_mouse()

    def on_mouse_up(self, event: events.MouseUp) -> None:
        if self._selecting:
            self._selecting = False
            self.release_mouse()
        elif self._mouse_tracking():
            self._send_mouse({1: 0, 2: 1, 3: 2}.get(event.button, 0), event.x, event.y, release=True)

    def on_mouse_move(self, event: events.MouseMove) -> None:
        if self._selecting:
            self._select_to(event.x, event.y)
        elif event.button and self._mouse_tracking() and (
            self._screen.has_mode(1002) or self._screen.has_mode(1003)
        ):
            self._send_mouse(32 + {1: 0, 2: 1, 3: 2}.get(event.button, 0), event.x, event.y)

    def on_mouse_scroll_up(self, event: events.MouseScrollUp) -> None:
        event.stop()
        self._wheel(event, up=True)

    def on_mouse_scroll_down(self, event: events.MouseScrollDown) -> None:
        event.stop()
        self._wheel(event, up=False)

    def _wheel(self, event: events.MouseEvent, up: bool) -> None:
        if self._mouse_tracking():
            self._send_mouse(64 if up else 65, event.x, event.y)
        elif self._screen.in_alt:
            self.write(self._translate("up" if up else "down", None) * 3)
        else:
            self._scroll_by(3 if up else -3)

    def _scroll_by(self, delta: int) -> None:
        limit = len(self._screen.history.top)
        new = max(0, min(limit, self._scroll + delta))
        if new != self._scroll:
            self._scroll = new
            self.refresh()

    # -- selection ------------------------------------------------------------

    def _top_line(self) -> int:
        """History line shown on the top row right now."""
        return len(self._screen.history.top) - self._scroll

    def _line(self, index: int):
        history = self._screen.history.top
        return history[index] if index < len(history) else self._screen.buffer[index - len(history)]

    def _spot_at(self, x: int, y: int) -> tuple[int, int]:
        """The cell under widget position (x, y), clamped into the terminal."""
        row = max(0, min(self._screen.lines - 1, y))
        col = max(0, min(self._screen.columns - 1, x))
        return self._top_line() + row, col

    def _range(self) -> tuple[tuple[int, int], tuple[int, int]] | None:
        """The selection as an ordered, end-exclusive range; None when nothing is selected."""
        if self._selection is None or self._selection[0] == self._selection[1]:
            return None
        start, end = sorted(self._selection)
        return start, (end[0], min(end[1] + 1, self._screen.columns))

    @property
    def has_selection(self) -> bool:
        return self._range() is not None

    def clear_selection(self) -> None:
        if self._selection is not None:
            self._selection = None
            self.refresh()

    def _select_start(self, x: int, y: int, extend: bool = False) -> None:
        """Left button pressed: start a selection. A second click on the same cell selects
        the word there, a third the whole row. `extend` (Shift) moves the far end."""
        spot = self._spot_at(x, y)
        if extend and self._selection is not None:
            self._selection = (self._selection[0], spot)
            self.refresh()
            return
        now = time.monotonic()
        last = self._last_click
        clicks = last[2] % 3 + 1 if last and last[1] == spot and now - last[0] < 0.4 else 1
        self._last_click = (now, spot, clicks)
        line, col = spot
        cells = self._line(line)

        def is_word(c: int) -> bool:
            data = cells[c].data
            return bool(data) and data[0] not in _WORD_BREAKS

        if clicks == 2 and is_word(col):
            start = end = col
            while start > 0 and (is_word(start - 1) or cells[start - 1].data == ""):
                start -= 1
            while end + 1 < self._screen.columns and (is_word(end + 1) or cells[end + 1].data == ""):
                end += 1
            self._selection = ((line, start), (line, end))
        elif clicks == 3:
            self._selection = ((line, 0), (line, self._screen.columns - 1))
        else:
            self._selection = (spot, spot)
        self.refresh()

    def _select_to(self, x: int, y: int) -> None:
        """The mouse moved with the button down: move the far end, scrolling at the edges."""
        if y < 0:
            self._scroll_by(1)
        elif y >= self.size.height:
            self._scroll_by(-1)
        if self._selection is not None:
            self._selection = (self._selection[0], self._spot_at(x, y))
            self.refresh()

    def select_all(self) -> None:
        """Select everything: the scrollback and the screen."""
        last = len(self._screen.history.top) + self._screen.lines - 1
        self._selection = ((0, 0), (last, self._screen.columns - 1))
        self.refresh()

    def selected_text(self) -> str | None:
        """The selected text, with wrapped rows joined back into one line."""
        span = self._range()
        if span is None:
            return None
        (first, start_col), (last, end_col) = span
        final = len(self._screen.history.top) + self._screen.lines - 1
        if last > final:  # rows that are gone since the selection was made
            last, end_col = final, self._screen.columns
        parts: list[str] = []
        for index in range(first, last + 1):
            cells = self._line(index)
            lo = start_col if index == first else 0
            hi = end_col if index == last else self._screen.columns
            text = "".join(cells[c].data for c in range(lo, hi))
            wrapped = getattr(cells, "wrapped", False) and index < last
            parts.append(text if wrapped else text.rstrip() + ("\n" if index < last else ""))
        text = "".join(parts).rstrip("\n")
        return text or None

    def copy_selection(self) -> bool:
        """Copy the selection to the clipboard and clear it. False when nothing is selected."""
        text = self.selected_text()
        if text is None:
            return False
        self.app.copy_to_clipboard(text)
        self.clear_selection()
        return True

    @property
    def cursor_screen_offset(self):
        """Screen position of the process's cursor, or None when it shouldn't be shown."""
        cursor = self._screen.cursor
        if not self.running or self._scroll or cursor.hidden:
            return None
        region = self.content_region
        return region.offset + (min(cursor.x, self._screen.columns - 1), cursor.y)

    # -- painting -------------------------------------------------------------

    def get_content_width(self, container: Size, viewport: Size) -> int:
        return container.width

    def get_content_height(self, container: Size, viewport: Size, width: int) -> int:
        return container.height

    def render_line(self, y: int) -> Strip:
        screen = self._screen
        width = self.size.width
        theme = self.vs_theme
        if self._scroll:
            history = screen.history.top
            index = len(history) - self._scroll + y
            line = history[index] if index < len(history) else screen.buffer[index - len(history)]
            cursor_x = -1
        else:
            line = screen.buffer[y]
            cursor = screen.cursor
            cursor_x = cursor.x if (y == cursor.y and not cursor.hidden and self.running) else -1

        segments: list[Segment] = []
        run: list[str] = []
        run_key = None
        focused = self.has_focus
        span = self._range()
        sel_from = sel_to = -1  # columns of this row inside the selection
        if span is not None:
            (first, start_col), (last, end_col) = span
            index = self._top_line() + y
            if first <= index <= last:
                sel_from = start_col if index == first else 0
                sel_to = end_col if index == last else screen.columns
        if focused and getattr(self.app, "cursor_line", False):
            cursor_x = -1  # line-cursor mode: the app shows the terminal's real bar cursor here
        for x in range(min(width, screen.columns)):
            char = line[x]
            if char.data == "":  # right half of a wide character
                continue
            key = (
                char.fg, char.bg, char.bold, char.italics, char.underscore,
                char.strikethrough, char.reverse,
                (2 if focused else 1) if x == cursor_x else 0,
                sel_from <= x < sel_to,
            )
            if key != run_key:
                if run:
                    segments.append(Segment("".join(run), _style(theme, *run_key)))
                run, run_key = [], key
            run.append(char.data)
        if run:
            segments.append(Segment("".join(run), _style(theme, *run_key)))
        strip = Strip(segments)
        return strip.adjust_cell_length(width, _style(theme, "default", "default", *(False,) * 5, 0, False))


def _resolve(theme: VSTheme, name: str, default: str) -> str:
    if name == "default":
        return default
    if name in theme.ansi:
        return theme.ansi[name]
    if len(name) == 6:
        try:
            int(name, 16)
            return "#" + name
        except ValueError:
            pass
    return default


@lru_cache(maxsize=4096)
def _style(theme, fg, bg, bold, italics, underscore, strikethrough, reverse, cursor, selected) -> Style:
    color = _resolve(theme, fg, theme["terminal.foreground"])
    bgcolor = _resolve(theme, bg, theme["terminal.background"])
    if reverse:
        color, bgcolor = bgcolor, color
    if selected:
        bgcolor = theme["terminal.selectionBackground"]
    if cursor == 2:  # focused: solid block
        color, bgcolor = theme["terminal.background"], theme["terminalCursor.foreground"]
    elif cursor == 1:  # unfocused: dim block
        bgcolor = theme["disabledForeground"]
    return Style(
        color=color, bgcolor=bgcolor, bold=bold or None, italic=italics or None,
        underline=underscore or None, strike=strikethrough or None,
    )
