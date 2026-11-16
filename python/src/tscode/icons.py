"""Icon glyphs.

Default set uses Nerd Font codicons/devicons (verified against JetBrainsMono
Nerd Font). Set TSCODE_ICONS=ascii for terminals without a Nerd Font.
"""

from __future__ import annotations

import os
from pathlib import Path

ASCII = os.environ.get("TSCODE_ICONS", "").lower() == "ascii"

_NERD = {
    "logo": "",
    "files": "",
    "search": "",
    "scm": "",
    "debug": "",
    "extensions": "",
    "claude": "✻",
    "account": "",
    "gear": "",
    "chevron_right": "",
    "chevron_down": "",
    "chevron_up": "",
    "close": "",
    "dot": "",
    "terminal": "",
    "add": "",
    "trash": "",
    "ellipsis": "",
    "error": "",
    "warning": "",
    "bell": "",
    "remote": "",
    "arrow_left": "",
    "arrow_right": "",
    "refresh": "",
    "new_file": "",
    "new_folder": "",
    "collapse_all": "",
    "win_close": "",
    "layout_left": "",
    "layout_panel": "",
    "layout_right": "",
    "file": "",
    "branch": "",
    "sep": "",
    "spotify": "",
    "media_prev": "",
    "media_play": "",
    "media_pause": "",
    "media_next": "",
    "shuffle": "",
    "volume_up": "",
    "volume_down": "",
    "sign_out": "",
    "cap_left": "",
    "cap_right": "",
}

_ASCII = {
    "logo": "<>",
    "files": "F",
    "search": "S",
    "scm": "G",
    "debug": "D",
    "extensions": "X",
    "claude": "*",
    "account": "@",
    "gear": "%",
    "chevron_right": ">",
    "chevron_down": "v",
    "chevron_up": "^",
    "close": "x",
    "dot": "o",
    "terminal": ">_",
    "add": "+",
    "trash": "#",
    "ellipsis": "...",
    "error": "x",
    "warning": "!",
    "bell": "o",
    "remote": "><",
    "arrow_left": "<-",
    "arrow_right": "->",
    "refresh": "@",
    "new_file": "+",
    "new_folder": "+/",
    "collapse_all": "=",
    "win_close": "X",
    "layout_left": "[|",
    "layout_panel": "[_]",
    "layout_right": "|]",
    "file": "-",
    "branch": "Y",
    "sep": ">",
    "spotify": "#",
    "media_prev": "|<",
    "media_play": ">",
    "media_pause": "||",
    "media_next": ">|",
    "shuffle": "~",
    "volume_up": "+",
    "volume_down": "-",
    "sign_out": "->",
    "cap_left": "",
    "cap_right": "",
}

ICONS = _ASCII if ASCII else _NERD


def icon(name: str) -> str:
    return ICONS[name]


# (glyph, colour) per file type; colours follow the Seti icon theme.
_FILE_ICONS: dict[str, tuple[str, str]] = {
    ".py": ("", "#4b8bbe"),
    ".js": ("", "#cbcb41"),
    ".mjs": ("", "#cbcb41"),
    ".jsx": ("", "#519aba"),
    ".ts": ("", "#519aba"),
    ".tsx": ("", "#519aba"),
    ".json": ("", "#cbcb41"),
    ".md": ("", "#519aba"),
    ".html": ("", "#e37933"),
    ".css": ("", "#519aba"),
    ".tcss": ("", "#519aba"),
    ".rs": ("", "#dea584"),
    ".go": ("", "#519aba"),
    ".sh": ("", "#8dc149"),
    ".bash": ("", "#8dc149"),
    ".c": ("", "#519aba"),
    ".h": ("", "#a074c4"),
    ".cpp": ("", "#519aba"),
    ".java": ("", "#cc3e44"),
    ".rb": ("", "#cc3e44"),
    ".toml": ("", "#6d8086"),
    ".yaml": ("", "#a074c4"),
    ".yml": ("", "#a074c4"),
    ".ini": ("", "#6d8086"),
    ".cfg": ("", "#6d8086"),
    ".lock": ("", "#6d8086"),
    ".png": ("", "#a074c4"),
    ".jpg": ("", "#a074c4"),
    ".jpeg": ("", "#a074c4"),
    ".gif": ("", "#a074c4"),
    ".svg": ("", "#cbcb41"),
    ".sql": ("", "#f55385"),
    ".txt": ("", "#cccccc"),
}
_NAME_ICONS: dict[str, tuple[str, str]] = {
    ".gitignore": ("", "#41535b"),
    ".gitattributes": ("", "#41535b"),
    "dockerfile": ("", "#519aba"),
    "claude.md": ("✻", "#d97757"),
}
_DEFAULT_FILE = ("", "#cccccc")


def file_icon(path: Path | str, theme=None) -> tuple[str, str]:
    """Return (glyph, colour) for a file. Monochrome themes get one quiet colour."""
    name = Path(path).name.lower()
    glyph, colour = _NAME_ICONS.get(name) or _FILE_ICONS.get(Path(name).suffix) or _DEFAULT_FILE
    if ASCII:
        glyph = "-"
    if theme is not None and theme.mono_icons:
        colour = theme["icon.monoForeground"]
    return glyph, colour
