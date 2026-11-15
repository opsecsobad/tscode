"""Spotify Web API client: login (OAuth with PKCE) and playback control.

All methods block on the network; call them with `asyncio.to_thread`. Spotify plays
the audio on one of the user's devices (the desktop app, a phone...) - this only
remote-controls it, which Spotify allows for Premium accounts.

Tokens live in ~/.config/tscode/spotify.json (mode 0600). Environment overrides, used
by the tests: TSCODE_SPOTIFY_FILE, TSCODE_SPOTIFY_ACCOUNTS, TSCODE_SPOTIFY_API,
TSCODE_SPOTIFY_PORT.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import secrets
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path

SCOPES = "user-read-playback-state user-modify-playback-state user-read-currently-playing"
SEARCH_LIMIT = 10  # the most Spotify returns to apps in development mode


def callback_port() -> int:
    return int(os.environ.get("TSCODE_SPOTIFY_PORT") or 8765)


def redirect_uri() -> str:
    # Spotify only accepts the loopback address written as an IP, not "localhost".
    return f"http://127.0.0.1:{callback_port()}/callback"


def _accounts_url() -> str:
    return os.environ.get("TSCODE_SPOTIFY_ACCOUNTS") or "https://accounts.spotify.com"


def _api_url() -> str:
    return os.environ.get("TSCODE_SPOTIFY_API") or "https://api.spotify.com/v1"


def _store_path() -> Path:
    override = os.environ.get("TSCODE_SPOTIFY_FILE")
    if override:
        return Path(override)
    config = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    return config / "tscode" / "spotify.json"


class SpotifyError(Exception):
    """Something the user should be told, phrased for them."""


class NoDeviceError(SpotifyError):
    """Spotify has no device to play on."""


@dataclass
class Track:
    uri: str
    title: str
    artists: str
    duration_ms: int


@dataclass
class Playback:
    track: Track | None
    is_playing: bool
    progress_ms: int
    device: str
    volume: int | None
    shuffle: bool
    fetched_at: float  # time.monotonic() when this was read

    def progress_now(self) -> int:
        """Progress estimated for right now, without asking Spotify again."""
        elapsed = int((time.monotonic() - self.fetched_at) * 1000) if self.is_playing else 0
        limit = self.track.duration_ms if self.track else 0
        return min(self.progress_ms + elapsed, limit)


def _track(item: dict | None) -> Track | None:
    if not item:
        return None
    artists = ", ".join(a.get("name", "") for a in item.get("artists") or item.get("show") and [item["show"]] or [])
    return Track(item.get("uri", ""), item.get("name", ""), artists, int(item.get("duration_ms") or 0))


class SpotifyClient:
    def __init__(self) -> None:
        self._path = _store_path()
        self._data: dict = {}
        try:
            loaded = json.loads(self._path.read_text(encoding="utf-8"))
            if isinstance(loaded, dict):
                self._data = loaded
        except (OSError, ValueError):
            pass

    # -- stored state ---------------------------------------------------------

    def _save(self) -> None:
        try:
            self._path.parent.mkdir(parents=True, exist_ok=True)
            self._path.touch(mode=0o600, exist_ok=True)
            os.chmod(self._path, 0o600)
            self._path.write_text(json.dumps(self._data, indent=2) + "\n", encoding="utf-8")
        except OSError:
            pass  # still works for this session

    @property
    def client_id(self) -> str:
        return self._data.get("client_id", "")

    @client_id.setter
    def client_id(self, value: str) -> None:
        self._data["client_id"] = value.strip()
        self._save()

    @staticmethod
    def valid_client_id(value: str) -> bool:
        return re.fullmatch(r"[0-9a-fA-F]{32}", value.strip()) is not None

    @property
    def logged_in(self) -> bool:
        return bool(self._data.get("refresh_token"))

    def logout(self) -> None:
        for key in ("refresh_token", "access_token", "expires_at"):
            self._data.pop(key, None)
        self._save()

    # -- login ----------------------------------------------------------------

    def authorize_url(self) -> tuple[str, str, str]:
        """Return (url to open in the browser, state, code verifier)."""
        verifier = secrets.token_urlsafe(64)
        challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
        state = secrets.token_urlsafe(16)
        query = urllib.parse.urlencode({
            "client_id": self.client_id,
            "response_type": "code",
            "redirect_uri": redirect_uri(),
            "code_challenge_method": "S256",
            "code_challenge": challenge,
            "state": state,
            "scope": SCOPES,
        })
        return f"{_accounts_url()}/authorize?{query}", state, verifier

    def finish_login(self, code: str, verifier: str) -> None:
        self._token_request({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": redirect_uri(),
            "client_id": self.client_id,
            "code_verifier": verifier,
        })

    def _refresh(self) -> None:
        try:
            self._token_request({
                "grant_type": "refresh_token",
                "refresh_token": self._data.get("refresh_token", ""),
                "client_id": self.client_id,
            })
        except SpotifyError as error:
            if getattr(error, "rejected", False):
                self.logout()
                raise SpotifyError("Your Spotify login has expired. Log in again.") from None
            raise

    def _token_request(self, form: dict) -> None:
        request = urllib.request.Request(
            f"{_accounts_url()}/api/token",
            data=urllib.parse.urlencode(form).encode(),
            headers={"Content-Type": "application/x-www-form-urlencoded"},
        )
        try:
            with urllib.request.urlopen(request, timeout=15) as response:
                tokens = json.loads(response.read())
        except urllib.error.HTTPError as error:
            detail = _error_text(error)
            failure = SpotifyError(f"Spotify refused the login: {detail}. Check the Client ID and redirect URI.")
            failure.rejected = True
            raise failure from None
        except (urllib.error.URLError, TimeoutError, ValueError):
            raise SpotifyError("Can't reach Spotify. Check your internet connection.") from None
        self._data["access_token"] = tokens["access_token"]
        self._data["expires_at"] = time.time() + int(tokens.get("expires_in", 3600)) - 60
        if tokens.get("refresh_token"):
            self._data["refresh_token"] = tokens["refresh_token"]
        self._save()

    # -- API ------------------------------------------------------------------

    def _request(self, method: str, path: str, params: dict | None = None, body: dict | None = None,
                 retry: bool = True):
        if not self.logged_in:
            raise SpotifyError("Log in to Spotify first.")
        if not self._data.get("access_token") or time.time() >= self._data.get("expires_at", 0):
            self._refresh()
        url = _api_url() + path + ("?" + urllib.parse.urlencode(params) if params else "")
        data = json.dumps(body).encode() if body is not None else (b"" if method in ("PUT", "POST") else None)
        request = urllib.request.Request(url, data=data, method=method, headers={
            "Authorization": f"Bearer {self._data['access_token']}",
            "Content-Type": "application/json",
        })
        try:
            with urllib.request.urlopen(request, timeout=15) as response:
                raw = response.read()
        except urllib.error.HTTPError as error:
            reason, message = _error_parts(error)
            if error.code == 401 and retry:
                self._data["expires_at"] = 0
                return self._request(method, path, params, body, retry=False)
            if error.code == 404 and (reason == "NO_ACTIVE_DEVICE" or "device" in message.lower()):
                raise NoDeviceError("No Spotify device is open.") from None
            if error.code == 403 and (reason == "PREMIUM_REQUIRED" or "premium" in message.lower()):
                raise SpotifyError("Spotify only lets Premium accounts control playback.") from None
            if error.code == 429:
                wait = error.headers.get("Retry-After", "a few")
                raise SpotifyError(f"Spotify is limiting requests. Try again in {wait} seconds.") from None
            raise SpotifyError(f"Spotify said: {message or error.reason}.") from None
        except (urllib.error.URLError, TimeoutError):
            raise SpotifyError("Can't reach Spotify. Check your internet connection.") from None
        if not raw.strip():
            return None
        try:
            return json.loads(raw)
        except ValueError:
            return None  # some player endpoints answer with a bare string

    def playback(self) -> Playback | None:
        """What is playing, or None when no device is active."""
        state = self._request("GET", "/me/player", {"additional_types": "episode"})
        if not state:
            return None
        device = state.get("device") or {}
        return Playback(
            track=_track(state.get("item")),
            is_playing=bool(state.get("is_playing")),
            progress_ms=int(state.get("progress_ms") or 0),
            device=device.get("name", ""),
            volume=device.get("volume_percent") if device.get("supports_volume", True) else None,
            shuffle=bool(state.get("shuffle_state")),
            fetched_at=time.monotonic(),
        )

    def devices(self) -> list[dict]:
        return (self._request("GET", "/me/player/devices") or {}).get("devices") or []

    def activate_device(self) -> bool:
        """Make some open device the active one. False when none is open."""
        devices = [d for d in self.devices() if not d.get("is_restricted")]
        if not devices:
            return False
        chosen = next((d for d in devices if d.get("is_active")), None) or next(
            (d for d in devices if d.get("type") == "Computer"), devices[0])
        self._request("PUT", "/me/player", body={"device_ids": [chosen["id"]], "play": False})
        return True

    def _command(self, method: str, path: str, params: dict | None = None, body: dict | None = None) -> None:
        """A playback command; wakes an idle device first if Spotify has none active."""
        try:
            self._request(method, path, params, body)
        except NoDeviceError:
            if not self.activate_device():
                raise
            time.sleep(0.6)  # the transfer takes a moment to register
            self._request(method, path, params, body)

    def play(self) -> None:
        self._command("PUT", "/me/player/play")

    def pause(self) -> None:
        self._command("PUT", "/me/player/pause")

    def next(self) -> None:
        self._command("POST", "/me/player/next")

    def previous(self) -> None:
        self._command("POST", "/me/player/previous")

    def play_track(self, uri: str) -> None:
        self._command("PUT", "/me/player/play", body={"uris": [uri]})

    def seek(self, position_ms: int) -> None:
        self._command("PUT", "/me/player/seek", {"position_ms": max(0, int(position_ms))})

    def set_volume(self, percent: int) -> None:
        self._command("PUT", "/me/player/volume", {"volume_percent": max(0, min(100, int(percent)))})

    def set_shuffle(self, on: bool) -> None:
        self._command("PUT", "/me/player/shuffle", {"state": "true" if on else "false"})

    def search(self, query: str) -> list[Track]:
        found = self._request("GET", "/search", {"q": query, "type": "track", "limit": SEARCH_LIMIT}) or {}
        return [t for t in map(_track, (found.get("tracks") or {}).get("items") or []) if t and t.uri]


def _error_parts(error: urllib.error.HTTPError) -> tuple[str, str]:
    """(reason code, human message) from a Spotify error response."""
    try:
        payload = json.loads(error.read())
    except (ValueError, OSError):
        return "", ""
    detail = payload.get("error") if isinstance(payload, dict) else None
    if isinstance(detail, dict):
        return str(detail.get("reason", "")), str(detail.get("message", ""))
    if isinstance(payload, dict):
        return str(detail or ""), str(payload.get("error_description", detail or ""))
    return "", ""


def _error_text(error: urllib.error.HTTPError) -> str:
    reason, message = _error_parts(error)
    return message or reason or f"HTTP {error.code}"
