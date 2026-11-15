"""Problems in a file: where the error squiggles come from.

Sources, best available first:

* Python      the interpreter's own parser (`compile`) - exact syntax errors
* C / C++     `gcc -fsyntax-only` - real compiler errors and warnings (if gcc is installed)
* everything  tree-sitter: the places the grammar could not parse (ERROR / MISSING nodes)
              - works for every highlighted language, as you type
"""

from __future__ import annotations

import asyncio
import json
import shutil
import warnings
from dataclasses import dataclass
from pathlib import Path

MAX_DIAGNOSTICS = 200


@dataclass(frozen=True)
class Diagnostic:
    line: int  # 0-based
    column: int  # 0-based, in characters
    end_column: int | None  # exclusive; None = to the end of the line
    message: str
    severity: str  # "error" or "warning"
    source: str

    def covers(self, line: int, column: int) -> bool:
        return line == self.line and self.column <= column and (self.end_column is None or column < self.end_column)


def _char_column(line_text: str, byte_column: int) -> int:
    return len(line_text.encode("utf-8")[:byte_column].decode("utf-8", "ignore"))


def _fit(lines: list[str], line: int, column: int, end_column: int | None) -> tuple[int, int, int | None]:
    """Clamp a range to the text and make sure it is at least one character wide."""
    line = max(0, min(line, len(lines) - 1))
    length = len(lines[line])
    column = max(0, min(column, length))
    if end_column is not None:
        end_column = max(column, min(end_column, length))
    if end_column is None or end_column > column:
        if column >= length and length:  # starts past the last character: mark that character
            return line, length - 1, length
        return line, column, end_column
    if column >= length and length:
        return line, length - 1, length
    return line, column, column + 1


# -- tree-sitter ------------------------------------------------------------------

def syntax_tree_diagnostics(tree, text: str) -> list[Diagnostic]:
    """Parse errors from a tree-sitter tree."""
    if tree is None or not tree.root_node.has_error:
        return []
    lines = text.split("\n")
    found: list[Diagnostic] = []
    stack = [tree.root_node]
    while stack and len(found) < MAX_DIAGNOSTICS:
        node = stack.pop()
        if node.is_missing:
            row, byte_column = node.start_point
            row = min(row, len(lines) - 1)
            column = _char_column(lines[row], byte_column)
            found.append(Diagnostic(*_fit(lines, row, column, column), f"'{node.type}' expected.", "error", "syntax"))
            continue
        if node.is_error:
            row, byte_column = node.start_point
            end_row, end_byte = node.end_point
            row = min(row, len(lines) - 1)
            column = _char_column(lines[row], byte_column)
            # Underline only the first line of a big broken region.
            end = _char_column(lines[row], end_byte) if end_row == row else None
            snippet = lines[row][column:(end if end is not None else column + 20)].strip()[:20]
            message = f"Syntax error near '{snippet}'." if snippet else "Syntax error."
            found.append(Diagnostic(*_fit(lines, row, column, end), message, "error", "syntax"))
            continue
        stack.extend(child for child in reversed(node.children) if child.has_error)
    return sorted(found, key=lambda d: (d.line, d.column))


# -- Python -----------------------------------------------------------------------

def python_diagnostics(text: str, filename: str) -> list[Diagnostic]:
    lines = text.split("\n")
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            compile(text, filename, "exec", dont_inherit=True)
    except SyntaxError as error:
        line = (error.lineno or 1) - 1
        column = max((error.offset or 1) - 1, 0)
        end = None
        if error.end_lineno == error.lineno and error.end_offset and error.end_offset - 1 > column:
            end = error.end_offset - 1
        message = (error.msg or "invalid syntax").rstrip(".")
        return [Diagnostic(*_fit(lines, line, column, end if end is not None else column), message[0].upper() + message[1:] + ".", "error", "python")]
    except (ValueError, RecursionError, MemoryError) as error:  # e.g. null bytes
        return [Diagnostic(0, 0, None, f"{error}.", "error", "python")]
    return []


# -- C / C++ ----------------------------------------------------------------------

async def gcc_diagnostics(text: str, path: Path | None, cplusplus: bool) -> list[Diagnostic] | None:
    """Compile-check the buffer (unsaved text, via stdin). None if gcc can't be used."""
    gcc = shutil.which("gcc")
    if gcc is None:
        return None
    folder = str(path.parent) if path else "."
    command = [gcc, "-fsyntax-only", "-fdiagnostics-format=json", "-Wall",
               "-x", "c++" if cplusplus else "c", "-I", folder, "-"]
    try:
        process = await asyncio.create_subprocess_exec(
            *command, cwd=folder, stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.PIPE,
        )
        _, stderr = await asyncio.wait_for(process.communicate(text.encode("utf-8")), timeout=15)
    except (OSError, asyncio.TimeoutError):
        return None
    try:
        reports = json.loads(stderr.decode("utf-8", "replace") or "[]")
    except ValueError:
        return None  # an old gcc that can't print JSON
    lines = text.split("\n")
    found: list[Diagnostic] = []
    for report in reports:
        if report.get("kind") not in ("error", "fatal error", "warning"):
            continue
        locations = report.get("locations") or [{}]
        caret = locations[0].get("caret") or {}
        if caret.get("file") not in ("<stdin>", None) or "line" not in caret:
            continue
        finish = locations[0].get("finish") or caret
        row = min(max(caret["line"] - 1, 0), len(lines) - 1)

        def column_of(point: dict) -> int:
            if "byte-column" in point:
                return _char_column(lines[row], point["byte-column"] - 1)
            return point.get("column", 1) - 1

        column = column_of(caret)
        end = column_of(finish) + 1 if finish.get("line") == caret["line"] else None
        option = report.get("option")
        message = report.get("message", "error")
        message = message[0].upper() + message[1:] + (f" ({option})" if option else "")
        severity = "warning" if report["kind"] == "warning" else "error"
        found.append(Diagnostic(*_fit(lines, row, column, end), message, severity, "gcc"))
        if len(found) >= MAX_DIAGNOSTICS:
            break
    return sorted(found, key=lambda d: (d.line, d.column))


# -- entry point --------------------------------------------------------------------

async def check(language: str | None, text: str, path: Path | None, tree) -> list[Diagnostic]:
    """All problems in `text`. `tree` is the editor's tree-sitter tree for that text (or None)."""
    if language == "python":
        return await asyncio.to_thread(python_diagnostics, text, str(path or "<untitled>"))
    if language in ("c", "cpp"):
        found = await gcc_diagnostics(text, path, cplusplus=language == "cpp")
        if found is not None:
            return found
    return syntax_tree_diagnostics(tree, text)
