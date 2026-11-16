"""Command palette providers: Quick Open (Ctrl+P) and Show All Commands (F1)."""

from __future__ import annotations

import asyncio
import os
from functools import partial
from pathlib import Path

from textual.command import DiscoveryHit, Hit, Hits, Provider

SKIP_DIRS = {".git", "node_modules", ".venv", "venv", "__pycache__", ".mypy_cache",
             ".pytest_cache", ".ruff_cache", "target", "dist", "build", ".next"}
MAX_FILES = 20_000

# (title, app action)
COMMANDS: list[tuple[str, str]] = [
    ("File: New Text File", "new_file"),
    ("File: New File...", "new_file_in"),
    ("File: New Folder...", "new_folder"),
    ("File: Save", "save"),
    ("File: Save As...", "save_as"),
    ("View: Close Editor", "close_editor"),
    ("Go to File...", "quick_open"),
    ("View: Show Explorer", "show_view('explorer')"),
    ("View: Show Search", "show_view('search')"),
    ("View: Show Source Control", "show_view('scm')"),
    ("View: Toggle Primary Side Bar Visibility", "toggle_sidebar"),
    ("View: Toggle Panel Visibility", "toggle_panel"),
    ("View: Toggle Agent Panel", "toggle_claude"),
    ("View: Focus Next Part", "focus_next_part"),
    ("View: Open Next Editor", "next_editor"),
    ("View: Open Previous Editor", "previous_editor"),
    ("Terminal: Focus Terminal", "focus_terminal"),
    ("Terminal: Kill and Restart Terminal", "restart_terminal"),
    ("Terminal: Run Active File in Terminal", "run_file"),
    ("Agent: Start...", "agent_menu"),
    ("Agent: Focus", "focus_claude"),
    ("Agent: New Conversation", "restart_claude"),
    ("Agent: Stop", "stop_agent"),
    ("Preferences: Color Theme", "select_theme"),
    ("Explorer: Refresh", "refresh_explorer"),
    ("Help: About", "about"),
    ("File: Exit", "quit"),
]


# Only offered when the (shelved) Spotify player is switched on.
SPOTIFY_COMMANDS: list[tuple[str, str]] = [
    ("Spotify: Play / Pause", "spotify('toggle')"),
    ("Spotify: Next Song", "spotify('next')"),
    ("Spotify: Previous Song", "spotify('previous')"),
    ("Spotify: Search for a Song", "spotify('search')"),
    ("Spotify: Toggle Shuffle", "spotify('shuffle')"),
    ("Spotify: Log In", "spotify('login')"),
    ("Spotify: Log Out", "spotify('logout')"),
    ("View: Toggle Spotify Player", "toggle_spotify"),
]


def walk_files(root: Path) -> list[str]:
    found: list[str] = []
    for folder, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS)
        for name in sorted(files):
            found.append(os.path.relpath(os.path.join(folder, name), root))
            if len(found) >= MAX_FILES:
                return found
    return found


class FileProvider(Provider):
    """Quick Open: fuzzy-find a file in the workspace."""

    async def startup(self) -> None:
        self.files = await asyncio.to_thread(walk_files, self.app.workspace)

    def _open(self, relative: str):
        return partial(self.app.open_file, self.app.workspace / relative)

    async def discover(self) -> Hits:
        for relative in self.files[:30]:
            yield DiscoveryHit(relative, self._open(relative))

    async def search(self, query: str) -> Hits:
        matcher = self.matcher(query)
        for relative in self.files:
            score = matcher.match(relative)
            if score > 0:
                # Prefer matches in the file name over matches in the folder path.
                bonus = 0.5 if matcher.match(os.path.basename(relative)) > 0 else 0
                yield Hit(score + bonus, matcher.highlight(relative), self._open(relative))


class CommandProvider(Provider):
    """Show All Commands."""

    def _run(self, action: str):
        return partial(self.app.run_action, action)

    def _commands(self) -> list[tuple[str, str]]:
        return COMMANDS + (SPOTIFY_COMMANDS if self.app.spotify_enabled else [])

    async def discover(self) -> Hits:
        for title, action in self._commands():
            yield DiscoveryHit(title, self._run(action))

    async def search(self, query: str) -> Hits:
        matcher = self.matcher(query)
        for title, action in self._commands():
            score = matcher.match(title)
            if score > 0:
                yield Hit(score, matcher.highlight(title), self._run(action))
