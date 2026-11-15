"""Entry point: `tscode [path]` - path is a folder (workspace) or a file to open."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from .app import TerminalStudioCode
from .theme import available_themes


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="tscode", description="Terminal Studio Code - VS Code in your terminal."
    )
    parser.add_argument("path", nargs="?", default=".", help="folder to open, or a file (default: current folder)")
    parser.add_argument("--theme", help="colour theme id for this run (see --list-themes)")
    parser.add_argument("--list-themes", action="store_true", help="print the available theme ids and exit")
    args = parser.parse_args()

    if args.list_themes:
        for theme_id, label in available_themes().items():
            print(f"{theme_id:24} {label}")
        return
    if args.theme and args.theme not in available_themes():
        sys.exit(f"tscode: unknown theme '{args.theme}'. Run `tscode --list-themes` to see the choices.")

    target = Path(args.path).expanduser().resolve()
    if target.is_dir():
        workspace, files = target, []
    elif target.is_file():
        workspace, files = target.parent, [target]
    else:
        sys.exit(f"tscode: no such file or directory: {args.path}")

    TerminalStudioCode(workspace, files, theme=args.theme).run()


if __name__ == "__main__":
    main()
