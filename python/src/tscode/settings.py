"""Tiny persisted settings (~/.config/tscode/settings.json). Override the path with TSCODE_SETTINGS."""

from __future__ import annotations

import json
import os
from pathlib import Path


def settings_path() -> Path:
    override = os.environ.get("TSCODE_SETTINGS")
    if override:
        return Path(override)
    config = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    return config / "tscode" / "settings.json"


def load_settings() -> dict:
    try:
        data = json.loads(settings_path().read_text(encoding="utf-8"))
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def save_setting(key: str, value) -> None:
    path = settings_path()
    if not path.parent.is_dir() and path.parent.name != "tscode":
        return
    data = load_settings()
    data[key] = value
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    except OSError:
        pass


def cursor_style_path() -> Path:
    """State file written by the system-wide `cursor-style-toggle` command (Super+\\)."""
    state = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state")
    return state / "cursor-style"


def cursor_is_line() -> bool:
    """True for a thin line cursor (the default, like VS Code), False for a block."""
    try:
        return cursor_style_path().read_text().strip() != "block"
    except OSError:
        return True


def spotify_enabled() -> bool:
    """The Spotify player is shelved: off unless `"spotify_enabled": true` is in the
    settings file (or TSCODE_SPOTIFY=1 is set)."""
    if os.environ.get("TSCODE_SPOTIFY"):
        return os.environ["TSCODE_SPOTIFY"] not in ("0", "false", "")
    return bool(load_settings().get("spotify_enabled", False))
