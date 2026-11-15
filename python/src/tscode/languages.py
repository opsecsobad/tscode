"""Which language a file is, and the extra tree-sitter grammars we add to Textual's.

Textual bundles: bash css go html java javascript json markdown python regex rust sql
toml xml yaml. We add: c cpp typescript tsx ruby lua php (each grammar package ships
its own highlight query).
"""

from __future__ import annotations

import importlib
from functools import lru_cache
from pathlib import Path
from collections.abc import Iterable

# extension -> (tree-sitter language id or None, name shown in the status bar)
EXTENSIONS: dict[str, tuple[str | None, str]] = {
    ".py": ("python", "Python"), ".pyi": ("python", "Python"),
    ".c": ("c", "C"), ".h": ("c", "C"),
    ".cpp": ("cpp", "C++"), ".cc": ("cpp", "C++"), ".cxx": ("cpp", "C++"),
    ".hpp": ("cpp", "C++"), ".hh": ("cpp", "C++"),
    ".js": ("javascript", "JavaScript"), ".mjs": ("javascript", "JavaScript"),
    ".cjs": ("javascript", "JavaScript"), ".jsx": ("javascript", "JavaScript JSX"),
    ".ts": ("typescript", "TypeScript"), ".mts": ("typescript", "TypeScript"),
    ".tsx": ("tsx", "TypeScript JSX"),
    ".java": ("java", "Java"),
    ".json": ("json", "JSON"), ".jsonc": ("json", "JSON with Comments"),
    ".html": ("html", "HTML"), ".htm": ("html", "HTML"),
    ".css": ("css", "CSS"), ".tcss": ("css", "CSS"),
    ".md": ("markdown", "Markdown"), ".markdown": ("markdown", "Markdown"),
    ".toml": ("toml", "TOML"),
    ".yaml": ("yaml", "YAML"), ".yml": ("yaml", "YAML"),
    ".rs": ("rust", "Rust"),
    ".go": ("go", "Go"),
    ".sh": ("bash", "Shell Script"), ".bash": ("bash", "Shell Script"), ".zsh": ("bash", "Shell Script"),
    ".sql": ("sql", "SQL"),
    ".xml": ("xml", "XML"), ".svg": ("xml", "XML"),
    ".rb": ("ruby", "Ruby"),
    ".lua": ("lua", "Lua"),
    ".php": ("php", "PHP"),
}
FILENAMES: dict[str, tuple[str | None, str]] = {
    ".bashrc": ("bash", "Shell Script"), ".bash_profile": ("bash", "Shell Script"),
    ".zshrc": ("bash", "Shell Script"), ".profile": ("bash", "Shell Script"),
    "pkgbuild": ("bash", "Shell Script"),
    "uv.lock": ("toml", "TOML"), "cargo.lock": ("toml", "TOML"),
}


def language_for(path: Path | None) -> tuple[str | None, str]:
    if path is None:
        return (None, "Plain Text")
    return FILENAMES.get(path.name.lower()) or EXTENSIONS.get(path.suffix.lower()) or (None, "Plain Text")


def _bundled_query(name: str) -> str:
    import textual

    return (Path(textual.__file__).parent / "tree-sitter" / "highlights" / f"{name}.scm").read_text()


# language id -> (module, function returning the grammar, highlight query builder)
_EXTRA = {
    "c": ("tree_sitter_c", "language", lambda m: m.HIGHLIGHTS_QUERY),
    # The C++ query only holds the C++ additions; it is meant to sit on top of C's.
    "cpp": ("tree_sitter_cpp", "language",
            lambda m: importlib.import_module("tree_sitter_c").HIGHLIGHTS_QUERY + "\n" + m.HIGHLIGHTS_QUERY),
    # Same for TypeScript on top of JavaScript.
    "typescript": ("tree_sitter_typescript", "language_typescript",
                   lambda m: _bundled_query("javascript") + "\n" + m.HIGHLIGHTS_QUERY),
    "tsx": ("tree_sitter_typescript", "language_tsx",
            lambda m: _bundled_query("javascript") + "\n" + m.HIGHLIGHTS_QUERY),
    "ruby": ("tree_sitter_ruby", "language", lambda m: m.HIGHLIGHTS_QUERY),
    "lua": ("tree_sitter_lua", "language", lambda m: m.HIGHLIGHTS_QUERY),
    "php": ("tree_sitter_php", "language_php", lambda m: m.HIGHLIGHTS_QUERY),
}


@lru_cache(maxsize=None)
def extra_language(name: str) -> tuple[object, str] | None:
    """Load one optional grammar, returning None when it isn't usable."""
    spec = _EXTRA.get(name)
    if spec is None:
        return None
    from tree_sitter import Language, Query

    module_name, function, build_query = spec
    try:
        module = importlib.import_module(module_name)
        language = Language(getattr(module, function)())
        query = build_query(module)
        Query(language, query)  # make sure the query fits this grammar
    except Exception:
        return None
    return language, query


def extra_languages() -> dict[str, tuple[object, str]]:
    """All optional grammars available in this installation (mainly for inspection)."""
    return {name: loaded for name in _EXTRA if (loaded := extra_language(name)) is not None}


def register_extra_languages(text_area, names: Iterable[str] | None = None) -> None:
    """Register only the optional grammars a text area will actually use."""
    for name in _EXTRA if names is None else names:
        loaded = extra_language(name)
        if loaded is not None:
            language, query = loaded
            text_area.register_language(name, language, query)
