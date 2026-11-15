"""The Spotify player in the bottom-right corner (under the Claude panel)."""

from __future__ import annotations

import asyncio
import shutil
import subprocess
import urllib.parse
import webbrowser

from rich.text import Text
from textual import events, work
from textual.app import ComposeResult
from textual.containers import Horizontal, Vertical
from textual.screen import ModalScreen
from textual.widget import Widget
from textual.widgets import ContentSwitcher, Input, Label, OptionList, Static
from textual.widgets.option_list import Option

from ..icons import icon
from ..spotify import (
    NoDeviceError, Playback, SpotifyClient, SpotifyError, Track, callback_port, redirect_uri,
)
from .common import ActionButton

LOGIN_TIMEOUT = 300  # seconds to finish logging in in the browser
REFRESH_EVERY = 4  # seconds between asking Spotify what is playing
CALLBACK_PAGE = (
    "<!doctype html><meta charset=utf-8><title>tscode</title>"
    "<body style='font:16px system-ui;background:#121212;color:#fff;text-align:center;padding-top:20vh'>"
    "<h2>{title}</h2><p>{text}</p></body>"
)


def clock(ms: int) -> str:
    seconds = max(0, ms) // 1000
    return f"{seconds // 60}:{seconds % 60:02d}"


class SpotifySetup(ModalScreen[str | None]):
    """One-time prompt for the user's Spotify app Client ID."""

    BINDINGS = [("escape", "dismiss(None)", "Cancel")]

    def compose(self) -> ComposeResult:
        with Vertical(classes="popup-box dialog-box"):
            with Vertical(classes="dialog"):
                yield Label("Connect Spotify", classes="dialog-title")
                yield Label(
                    "Spotify asks every app to bring its own key. You set it up once:\n\n"
                    "1. Open developer.spotify.com/dashboard and choose Create app.\n"
                    f"2. Set the redirect URI to {redirect_uri()}\n"
                    "   and tick Web API.\n"
                    "3. Copy the app's Client ID and paste it below.\n\n"
                    "Controlling playback needs Spotify Premium.",
                    classes="dialog-detail",
                )
                yield Input(placeholder="Client ID", id="spotify-client-id")
                yield Label("Enter to continue, Esc to cancel", classes="dialog-detail", id="spotify-setup-hint")

    def on_input_submitted(self, event: Input.Submitted) -> None:
        value = event.value.strip()
        if SpotifyClient.valid_client_id(value):
            self.dismiss(value)
        else:
            self.query_one("#spotify-setup-hint", Label).update(
                "That doesn't look like a Client ID. It is 32 letters and digits, shown on your app's page."
            )


class SpotifySearch(ModalScreen[str | None]):
    """Find a song. Returns the chosen track's URI."""

    BINDINGS = [("escape", "dismiss(None)", "Cancel")]

    def __init__(self, client: SpotifyClient) -> None:
        super().__init__()
        self.client = client
        self._tracks: list[Track] = []

    def compose(self) -> ComposeResult:
        with Vertical(classes="popup-box quick-box"):
            with Vertical(classes="quick-input"):
                yield Input(placeholder="Search Spotify for a song")
                yield Label("Enter to search", classes="quick-hint", id="spotify-search-hint")
                yield OptionList(id="quick-list")

    def on_mount(self) -> None:
        self.query_one(OptionList).display = False

    def on_input_submitted(self, event: Input.Submitted) -> None:
        if event.value.strip():
            self.run_search(event.value.strip())

    @work(exclusive=True)
    async def run_search(self, query: str) -> None:
        hint = self.query_one("#spotify-search-hint", Label)
        results = self.query_one(OptionList)
        hint.update("Searching...")
        try:
            self._tracks = await asyncio.to_thread(self.client.search, query)
        except SpotifyError as error:
            hint.update(str(error))
            return
        results.clear_options()
        results.display = bool(self._tracks)
        if not self._tracks:
            hint.update("No songs found. Try fewer words or check the spelling.")
            return
        hint.update("Enter to play")
        for track in self._tracks:
            line = Text(f" {track.title}  {track.artists}  {clock(track.duration_ms)}")
            line.stylize("dim", len(track.title) + 2)
            results.add_option(Option(line))
        results.highlighted = 0
        results.focus()

    def on_option_list_option_selected(self, event: OptionList.OptionSelected) -> None:
        self.dismiss(self._tracks[event.option_index].uri)

    def on_click(self, event: events.Click) -> None:
        if event.widget is self:
            self.dismiss(None)


class ProgressLine(Widget):
    """Track position: a bar you can click to seek, with elapsed / total time."""

    def __init__(self) -> None:
        super().__init__(id="spotify-progress")
        self.position = 0
        self.duration = 0

    def set(self, position: int, duration: int) -> None:
        if (position // 1000, duration) != (self.position // 1000, self.duration):
            self.position, self.duration = position, duration
            self.refresh()

    def _bar_width(self) -> int:
        return max(4, self.size.width - len(self._times()) - 1)

    def _times(self) -> str:
        return f"{clock(self.position)} / {clock(self.duration)}"

    def render(self) -> Text:
        theme = self.app.vs_theme
        width = self._bar_width()
        done = round(width * self.position / self.duration) if self.duration else 0
        bar = Text("━" * done, style=theme["spotify.accent"])
        bar.append("━" * (width - done), style=theme["scrollbarSlider.hoverBackground"])
        bar.append(" " + self._times(), style=theme["descriptionForeground"])
        return bar

    def on_click(self, event: events.Click) -> None:
        event.stop()
        if self.duration and event.x < self._bar_width():
            self.app.query_one(SpotifyPlayer).seek(int(self.duration * event.x / self._bar_width()))


class SpotifyPlayer(Vertical):
    def __init__(self) -> None:
        super().__init__(id="spotify")
        self.client = SpotifyClient()
        self.state: Playback | None = None
        self._ticks = 0
        self._login_waiting: asyncio.Future | None = None
        self._launched_app = False

    def compose(self) -> ComposeResult:
        with Horizontal(classes="spotify-header"):
            yield Static(icon("spotify"), classes="spotify-mark")
            yield Static("SPOTIFY", classes="spotify-title")
            yield Static("", id="spotify-device")
            yield ActionButton(icon("search"), "spotify('search')", "Search for a song", classes="header-button -session")
            yield ActionButton(icon("sign_out"), "spotify('logout')", "Log out of Spotify", classes="header-button -session")
            yield ActionButton(icon("close"), "toggle_spotify", "Hide the Spotify player", classes="header-button")
        with ContentSwitcher(initial="spotify-login", id="spotify-views"):
            with Vertical(id="spotify-login"):
                yield Static("Play music without leaving the editor.", id="spotify-note")
                yield ActionButton("Log in to Spotify", "spotify('login')", id="spotify-login-button")
            with Vertical(id="spotify-player"):
                yield Static("", id="spotify-track")
                yield ProgressLine()
                with Horizontal(classes="spotify-controls"):
                    yield ActionButton(icon("shuffle"), "spotify('shuffle')", "Shuffle", id="spotify-shuffle", classes="media-button")
                    yield ActionButton(icon("media_prev"), "spotify('previous')", "Previous", classes="media-button")
                    yield ActionButton(icon("media_play"), "spotify('toggle')", "Play / Pause", id="spotify-toggle", classes="media-button")
                    yield ActionButton(icon("media_next"), "spotify('next')", "Next", classes="media-button")
                    yield ActionButton(icon("volume_down"), "spotify('volume_down')", "Volume down", classes="media-button")
                    yield Static("", id="spotify-volume")
                    yield ActionButton(icon("volume_up"), "spotify('volume_up')", "Volume up", classes="media-button")

    def on_mount(self) -> None:
        self._show_session()
        self.set_interval(1.0, self._tick)
        if self.client.logged_in:
            self.refresh_state()

    # -- display --------------------------------------------------------------

    def _show_session(self) -> None:
        logged_in = self.client.logged_in
        self.query_one(ContentSwitcher).current = "spotify-player" if logged_in else "spotify-login"
        for button in self.query(".-session"):
            button.display = logged_in
        if not logged_in:
            self.state = None
            self.query_one("#spotify-device", Static).update("")
            self.query_one("#spotify-note", Static).update("Play music without leaving the editor.")
            self.query_one("#spotify-login-button").display = True

    def _render_state(self) -> None:
        state = self.state
        track = self.query_one("#spotify-track", Static)
        if state is None or state.track is None:
            track.update(Text("Nothing is playing. Search for a song, or press play.", style="dim"))
            self.query_one(ProgressLine).set(0, 0)
        else:
            # One string styled by range (separate spans lose their leading spaces).
            line = Text(f"{state.track.title}  {state.track.artists}", no_wrap=True, overflow="ellipsis")
            line.stylize("dim", len(state.track.title))
            track.update(line)
            self.query_one(ProgressLine).set(state.progress_now(), state.track.duration_ms)
        playing = bool(state and state.is_playing)
        self.query_one("#spotify-toggle", Static).update(icon("media_pause") if playing else icon("media_play"))
        self.query_one("#spotify-shuffle").set_class(bool(state and state.shuffle), "-on")
        self.query_one("#spotify-device", Static).update(state.device if state else "")
        volume = state.volume if state else None
        self.query_one("#spotify-volume", Static).update(f"{volume}%" if volume is not None else "")

    def _tick(self) -> None:
        if not self.client.logged_in or not self.display:
            return
        self._ticks += 1
        state = self.state
        if state and state.track and state.is_playing:
            position = state.progress_now()
            self.query_one(ProgressLine).set(position, state.track.duration_ms)
            if position >= state.track.duration_ms:  # song ended: find out what's next
                self.refresh_state()
                return
        if self._ticks % REFRESH_EVERY == 0:
            self.refresh_state()

    @work(exclusive=True, group="spotify-state")
    async def refresh_state(self, delay: float = 0.0) -> None:
        if delay:
            await asyncio.sleep(delay)
        try:
            self.state = await asyncio.to_thread(self.client.playback)
        except SpotifyError as error:
            if not self.client.logged_in:  # the login was revoked or expired
                self._show_session()
                self.app.notify(str(error), title="Spotify", severity="warning")
            return
        self._render_state()

    # -- commands -------------------------------------------------------------

    def command(self, name: str) -> None:
        """Entry point for the app's `spotify(...)` action."""
        if name == "login":
            self.login()
        elif name == "logout":
            self.client.logout()
            self._show_session()
        elif not self.client.logged_in:
            self.app.notify("Log in to Spotify first.", title="Spotify")
        elif name == "search":
            def chosen(uri: str | None) -> None:
                if uri:
                    self.send(lambda: self.client.play_track(uri))

            self.app.push_screen(SpotifySearch(self.client), chosen)
        elif name == "toggle":
            playing = bool(self.state and self.state.is_playing)
            self.send(self.client.pause if playing else self.client.play)
        elif name == "next":
            self.send(self.client.next)
        elif name == "previous":
            self.send(self.client.previous)
        elif name == "shuffle":
            on = not (self.state and self.state.shuffle)
            self.send(lambda: self.client.set_shuffle(on))
        elif name in ("volume_up", "volume_down"):
            current = self.state.volume if self.state and self.state.volume is not None else 50
            target = current + (10 if name == "volume_up" else -10)
            self.send(lambda: self.client.set_volume(target))

    def seek(self, position_ms: int) -> None:
        self.send(lambda: self.client.seek(position_ms))

    @work(group="spotify-command")
    async def send(self, call) -> None:
        """Run one playback command, starting the Spotify app if nothing can play."""
        try:
            try:
                await asyncio.to_thread(call)
            except NoDeviceError:
                if not await self._start_local_spotify():
                    raise SpotifyError(
                        "No Spotify device is open. Start Spotify on this computer or your phone, then try again."
                    ) from None
                await asyncio.to_thread(call)
        except SpotifyError as error:
            self.app.notify(str(error), title="Spotify", severity="warning")
            return
        self.refresh_state(delay=0.4)  # Spotify needs a moment before it reports the change

    async def _start_local_spotify(self) -> bool:
        """Launch the desktop app (once) and wait for it to show up as a device."""
        binary = shutil.which("spotify")
        if not binary or self._launched_app:
            return False
        self._launched_app = True
        self.app.notify("Starting Spotify on this computer...", title="Spotify")
        subprocess.Popen([binary], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, start_new_session=True)
        for _ in range(20):
            await asyncio.sleep(1.0)
            try:
                if await asyncio.to_thread(self.client.activate_device):
                    await asyncio.sleep(0.6)
                    return True
            except SpotifyError:
                pass
        return False

    # -- login ----------------------------------------------------------------

    def login(self) -> None:
        if self._login_waiting is not None:  # second click while waiting: start over
            self._login_waiting.cancel()
            return
        if self.client.client_id:
            self.browser_login()
            return

        def got_id(client_id: str | None) -> None:
            if client_id:
                self.client.client_id = client_id
                self.browser_login()

        self.app.push_screen(SpotifySetup(), got_id)

    @work(exclusive=True, group="spotify-login")
    async def browser_login(self) -> None:
        note = self.query_one("#spotify-note", Static)
        button = self.query_one("#spotify-login-button", Static)
        url, state, verifier = self.client.authorize_url()
        loop = asyncio.get_running_loop()
        waiting: asyncio.Future = loop.create_future()

        async def handle(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
            try:
                request_line = (await reader.readline()).decode("latin-1")
                target = request_line.split(" ")[1] if " " in request_line else ""
                query = urllib.parse.parse_qs(urllib.parse.urlsplit(target).query)
                if not target.startswith("/callback"):
                    title, text, result = "Not found", "", None
                elif query.get("state", [""])[0] != state:
                    title, text, result = "Login failed", "This login link is out of date. Start again from tscode.", None
                elif "code" in query:
                    title, text, result = "You're logged in", "You can close this tab and go back to tscode.", query["code"][0]
                else:
                    reason = query.get("error", ["cancelled"])[0]
                    title, text = "Login cancelled", "Nothing was connected. You can close this tab."
                    result = SpotifyError(f"Spotify login was not completed ({reason}).")
                page = CALLBACK_PAGE.format(title=title, text=text).encode()
                writer.write(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n"
                             b"Connection: close\r\nContent-Length: " + str(len(page)).encode() + b"\r\n\r\n" + page)
                await writer.drain()
                if result is not None and not waiting.done():
                    waiting.set_result(result)
            finally:
                writer.close()

        try:
            server = await asyncio.start_server(handle, "127.0.0.1", callback_port())
        except OSError:
            self.app.notify(
                f"Port {callback_port()} is in use, so Spotify can't send the login back. "
                "Close whatever is using it and try again.", title="Spotify", severity="error")
            return
        self._login_waiting = waiting
        note.update("Finish logging in in your browser. Waiting for Spotify...")
        button.update("Cancel")
        try:
            opened = await asyncio.to_thread(webbrowser.open, url)
            if not opened:
                self.app.notify(f"Open this link to log in:\n{url}", title="Spotify", timeout=60)
            result = await asyncio.wait_for(waiting, LOGIN_TIMEOUT)
            if isinstance(result, SpotifyError):
                raise result
            note.update("Connecting...")
            await asyncio.to_thread(self.client.finish_login, result, verifier)
        except asyncio.TimeoutError:
            self.app.notify("The Spotify login timed out. Try again when you're ready.", title="Spotify")
        except asyncio.CancelledError:
            pass  # cancelled from the button
        except SpotifyError as error:
            self.app.notify(str(error), title="Spotify", severity="error")
        finally:
            self._login_waiting = None
            server.close()
            button.update("Log in to Spotify")
        self._show_session()
        if self.client.logged_in:
            self._render_state()
            self.refresh_state()
