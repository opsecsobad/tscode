"""Colour themes.

A `VSTheme` is the single source of truth for every colour in the app. It is
keyed the same way VS Code themes are (`editor.background`, `tab.activeBorderTop`...)
so real VS Code theme JSON maps straight onto it (see `load_vscode_theme`).
From one VSTheme we derive:

* CSS variables for app.tcss      -> `css_variables()`   ("editor.background" -> $vs-editor-background)
* the Textual base theme            -> `textual_theme()`   (palette, inputs, option lists...)
* the editor syntax theme           -> `text_area_theme()`
* the terminal's 16 ANSI colours    -> `ansi`

Themes available to the user (`available_themes()`):

* "vscode-dark-modern"  built in
* "terminal"            whatever the desktop/terminal is themed with: Omarchy's current
                        theme (it ships a generated VS Code theme JSON). Reloads live.
* any VS Code theme JSON dropped into ~/.config/tscode/themes/
"""

from __future__ import annotations

import colorsys
import itertools
import json
import os
import re
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

from rich.style import Style
from textual.theme import Theme
from textual.widgets.text_area import TextAreaTheme

_generation = itertools.count(1)


@dataclass(frozen=True, eq=False)
class VSTheme:
    id: str  # stable id used in settings / --theme
    label: str  # shown in the theme picker
    dark: bool
    colors: dict[str, str]
    syntax: dict[str, str]  # tree-sitter capture name -> rich style, e.g. "bold #rrggbb"
    ansi: dict[str, str] = field(default_factory=dict)  # pyte colour name -> "#rrggbb"
    mono_icons: bool = False  # draw file icons in one quiet colour (monochrome themes)
    # Per-language tweaks on top of `syntax`: language id -> {capture name -> style}.
    language_syntax: dict[str, dict[str, str]] = field(default_factory=dict)
    # Unique per load, so re-registering a reloaded theme always takes effect.
    name: str = field(default_factory=lambda: f"tscode-{next(_generation)}")

    def __getitem__(self, key: str) -> str:
        return self.colors[key]

    def css_variables(self) -> dict[str, str]:
        return {"vs-" + k.replace(".", "-"): v for k, v in self.colors.items()}

    def textual_theme(self) -> Theme:
        c = self.colors
        return Theme(
            name=self.name,
            primary=c["focusBorder"],
            secondary=c["focusBorder"],
            accent=c["focusBorder"],
            foreground=c["foreground"],
            background=c["editor.background"],
            surface=c["menu.background"],
            panel=c["sideBar.background"],
            warning=c["editorWarning.foreground"],
            error=c["editorError.foreground"],
            success=c["gitDecoration.addedResourceForeground"],
            dark=self.dark,
        )

    def text_area_theme(self, line_cursor: bool = False, language: str | None = None) -> TextAreaTheme:
        """`line_cursor`: draw no block; the terminal's own bar cursor marks the spot instead."""
        c = self.colors
        bg = c["editor.background"]
        syntax = {**self.syntax, **self.language_syntax.get(language or "", {})}
        styles = {k: Style.parse(v) for k, v in syntax.items()}
        # Error squiggles. Rich cannot request the terminal's curly-underline escape
        # sequence, but a coloured double underline remains conspicuous. Its limited
        # style model means the marker also colours the affected text red or amber.
        styles["diag.error"] = Style(color=c["editorError.foreground"], underline2=True)
        styles["diag.warning"] = Style(color=c["editorWarning.foreground"], underline2=True)
        return TextAreaTheme(
            name=f"{self.name}-{language or 'text'}" + ("-line" if line_cursor else ""),
            base_style=Style(color=c["editor.foreground"], bgcolor=bg),
            gutter_style=Style(color=c["editorLineNumber.foreground"], bgcolor=bg),
            # A null style is falsy, which makes TextArea skip painting the cursor cell.
            cursor_style=Style() if line_cursor else Style(color=bg, bgcolor=c["editorCursor.foreground"]),
            cursor_line_style=Style(bgcolor=c["editor.lineHighlightBackground"]),
            cursor_line_gutter_style=Style(
                color=c["editorLineNumber.activeForeground"],
                bgcolor=c["editor.lineHighlightBackground"],
            ),
            bracket_matching_style=Style(bgcolor=c["editorBracketMatch.background"]),
            selection_style=Style(bgcolor=c["editor.selectionBackground"]),
            syntax_styles=styles,
        )


# --- Dark Modern (VS Code's default dark theme) ------------------------------

_KEYWORD = "#569cd6"
_CONTROL = "#c586c0"
_STRING = "#ce9178"
_NUMBER = "#b5cea8"
_COMMENT = "#6a9955"
_FUNC = "#dcdcaa"
_TYPE = "#4ec9b0"
_VAR = "#9cdcfe"
_CONST = "#4fc1ff"
_PUNCT = "#d4d4d4"

DARK_MODERN = VSTheme(
    id="vscode-dark-modern",
    label="Dark Modern",
    dark=True,
    colors={
        "foreground": "#cccccc",
        "descriptionForeground": "#9d9d9d",
        "disabledForeground": "#6e7681",
        "focusBorder": "#0078d4",
        "border": "#2b2b2b",
        # title bar
        "titleBar.activeBackground": "#181818",
        "titleBar.activeForeground": "#cccccc",
        "commandCenter.background": "#252525",
        "commandCenter.foreground": "#9d9d9d",
        "commandCenter.border": "#3c3c3c",
        # activity bar
        "activityBar.background": "#181818",
        "activityBar.foreground": "#d7d7d7",
        "activityBar.inactiveForeground": "#868686",
        "activityBar.activeBorder": "#0078d4",
        # side bars
        "sideBar.background": "#181818",
        "sideBar.foreground": "#cccccc",
        "sideBarTitle.foreground": "#cccccc",
        "sideBarSectionHeader.background": "#181818",
        "list.hoverBackground": "#2a2d2e",
        "list.activeSelectionBackground": "#04395e",
        "list.activeSelectionForeground": "#ffffff",
        "list.inactiveSelectionBackground": "#37373d",
        "list.highlightForeground": "#2aaaff",
        # editor
        "editor.background": "#1f1f1f",
        "editor.foreground": "#cccccc",
        "editorLineNumber.foreground": "#6e7681",
        "editorLineNumber.activeForeground": "#cccccc",
        "editor.lineHighlightBackground": "#282828",
        "editor.selectionBackground": "#264f78",
        "editorCursor.foreground": "#aeafad",
        "editorBracketMatch.background": "#3a3d41",
        "editorGroupHeader.tabsBackground": "#181818",
        "tab.activeBackground": "#1f1f1f",
        "tab.activeForeground": "#ffffff",
        "tab.inactiveBackground": "#181818",
        "tab.inactiveForeground": "#9d9d9d",
        "tab.activeBorderTop": "#0078d4",
        "breadcrumb.foreground": "#a9a9a9",
        # panel + terminal
        "panel.background": "#181818",
        "panelTitle.activeForeground": "#e7e7e7",
        "panelTitle.inactiveForeground": "#9d9d9d",
        "panelTitle.activeBorder": "#0078d4",
        "terminal.background": "#181818",
        "terminal.foreground": "#cccccc",
        "terminalCursor.foreground": "#cccccc",
        "terminal.selectionBackground": "#264f78",
        # status bar
        "statusBar.background": "#181818",
        "statusBar.foreground": "#cccccc",
        "statusBarItem.remoteBackground": "#0078d4",
        "statusBarItem.remoteForeground": "#ffffff",
        "statusBarItem.hoverBackground": "#2f2f2f",
        # menus, inputs, buttons
        "menu.background": "#1f1f1f",
        "menu.foreground": "#cccccc",
        "menu.selectionBackground": "#0078d4",
        "menu.selectionForeground": "#ffffff",
        "menu.border": "#454545",
        "input.background": "#313131",
        "button.background": "#0078d4",
        "button.foreground": "#ffffff",
        "button.secondaryBackground": "#313131",
        "button.secondaryHoverBackground": "#3c3c3c",
        "input.border": "#3c3c3c",
        "widget.border": "#313131",
        # lists, search, git, diagnostics
        "editor.findMatchHighlightBackground": "#623315",
        "scrollbarSlider.background": "#333333",
        "scrollbarSlider.hoverBackground": "#454545",
        "gitDecoration.modifiedResourceForeground": "#e2c08d",
        "gitDecoration.addedResourceForeground": "#81b88b",
        "gitDecoration.deletedResourceForeground": "#c74e39",
        "gitDecoration.untrackedResourceForeground": "#73c991",
        "editorError.foreground": "#f14c4c",
        "editorWarning.foreground": "#cca700",
        "list.errorForeground": "#f88070",
        "list.warningForeground": "#cca700",
        # ours (not VS Code keys)
        "claude.accent": "#d97757",
        "spotify.accent": "#1ed760",
        "logo.foreground": "#23a9f2",
        "windowClose.hoverBackground": "#e81123",
        "icon.monoForeground": "#9d9d9d",
    },
    syntax={
        "comment": f"italic {_COMMENT}",
        "string": _STRING,
        "string.documentation": _STRING,
        "inline_code": _STRING,
        "info_string": _STRING,
        "number": _NUMBER,
        "float": _NUMBER,
        "boolean": _KEYWORD,
        "constant.builtin": _KEYWORD,
        "constant": _CONST,
        "json.null": _KEYWORD,
        "keyword": _KEYWORD,
        "keyword.function": _KEYWORD,
        "keyword.operator": _KEYWORD,
        "keyword.return": _CONTROL,
        "conditional": _CONTROL,
        "repeat": _CONTROL,
        "exception": _CONTROL,
        "include": _CONTROL,
        "operator": _PUNCT,
        "punctuation.bracket": _PUNCT,
        "punctuation.delimiter": _PUNCT,
        "punctuation.special": _KEYWORD,
        "function": _FUNC,
        "function.call": _FUNC,
        "method": _FUNC,
        "method.call": _FUNC,
        "class": _TYPE,
        "type": _TYPE,
        "type.class": _TYPE,
        "type.builtin": _TYPE,
        "variable": _VAR,
        "variable.parameter": _VAR,
        "tag": _KEYWORD,
        "html.end_tag_error": "#f44747",
        "yaml.field": _KEYWORD,
        "json.label": _VAR,
        "toml.type": _TYPE,
        "toml.datetime": _NUMBER,
        "css.property": _VAR,
        "regex.operator": "#d16969",
        "regex.punctuation.bracket": "#d16969",
        "heading": f"bold {_KEYWORD}",
        "bold": f"bold {_KEYWORD}",
        "italic": "italic",
        "strikethrough": "strike",
        "link.label": _STRING,
        "link.uri": "underline #3794ff",
        "list.marker": "#6796e6",
        # names used by the other grammars' highlight queries
        "attribute": _FUNC,
        "attribute.builtin": _FUNC,
        "constructor": _TYPE,
        "field": _VAR,
        "property": _VAR,
        "parameter": _VAR,
        "function.builtin": _FUNC,
        "function.method": _FUNC,
        "function.method.builtin": _FUNC,
        "function.macro": _KEYWORD,
        "function.special": _KEYWORD,
        "variable.builtin": _KEYWORD,
        "none": _KEYWORD,
        "preproc": _CONTROL,
        "import": _CONTROL,
        "storageclass": _KEYWORD,
        "type.qualifier": _KEYWORD,
        "type.definition": _TYPE,
        "namespace": _TYPE,
        "module": _TYPE,
        "module.builtin": _TYPE,
        "label": "#c8c8c8",
        "delimiter": _PUNCT,
        "escape": "#d7ba7d",
        "string.escape": "#d7ba7d",
        "constant.character": _STRING,
        "string.special": "#d16969",
        "string.special.regex": "#d16969",
        "string.special.symbol": _CONST,
        "tag.attribute": _VAR,
        "tag.delimiter": "#808080",
        "text.uri": "underline #3794ff",
        "text.literal": _STRING,
        "markup.heading": f"bold {_KEYWORD}",
        "heading.marker": f"bold {_KEYWORD}",
        "markup.link": "underline #3794ff",
        "markup.raw": _STRING,
        "media": _CONTROL,
        "keyframes": _CONTROL,
        "supports": _CONTROL,
        "charset": _CONTROL,
    },
    language_syntax={
        # CSS: selectors are gold, at-rules purple, values orange (Dark+).
        "css": {"type": "#d7ba7d", "type.definition": "#d7ba7d", "property": "#d7ba7d",
                "keyword": _CONTROL, "function": _FUNC, "string": _STRING},
        "html": {"constant": _STRING},
        "json": {"string": _STRING},
        "c": {"property": _VAR, "constant": _CONST},
    },
    ansi={
        "black": "#000000",
        "red": "#cd3131",
        "green": "#0dbc79",
        "brown": "#e5e510",  # pyte calls ANSI yellow "brown"
        "blue": "#2472c8",
        "magenta": "#bc3fbc",
        "cyan": "#11a8cd",
        "white": "#e5e5e5",
        "brightblack": "#666666",
        "brightred": "#f14c4c",
        "brightgreen": "#23d18b",
        "brightbrown": "#f5f543",
        "brightblue": "#3b8eea",
        "brightmagenta": "#d670d6",
        "brightcyan": "#29b8db",
        "brightwhite": "#e5e5e5",
    },
)

DEFAULT_THEME = DARK_MODERN


# --- colour helpers ------------------------------------------------------------

def _rgba(value: str) -> tuple[int, int, int, float]:
    h = value.strip().lstrip("#")
    if len(h) in (3, 4):
        h = "".join(ch * 2 for ch in h)
    if len(h) not in (6, 8):
        raise ValueError(f"bad colour {value!r}")
    r, g, b = int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)
    return r, g, b, (int(h[6:8], 16) / 255 if len(h) == 8 else 1.0)


def blend(value: str, over: str) -> str:
    """Flatten a possibly translucent colour (#rrggbbaa) onto an opaque one."""
    r, g, b, a = _rgba(value)
    br, bg, bb, _ = _rgba(over)
    return "#{:02x}{:02x}{:02x}".format(
        round(r * a + br * (1 - a)), round(g * a + bg * (1 - a)), round(b * a + bb * (1 - a))
    )


def mix(a: str, b: str, amount: float) -> str:
    """`amount` of the way from colour a to colour b."""
    ar, ag, ab, _ = _rgba(a)
    br, bg, bb, _ = _rgba(b)
    return "#{:02x}{:02x}{:02x}".format(
        round(ar + (br - ar) * amount), round(ag + (bg - ag) * amount), round(ab + (bb - ab) * amount)
    )


def _saturation(value: str) -> float:
    r, g, b, _ = _rgba(value)
    return colorsys.rgb_to_hsv(r / 255, g / 255, b / 255)[1]


# --- loading VS Code theme JSON --------------------------------------------------

# Our key -> where to look when a theme doesn't define it (another key; resolved recursively).
_FALLBACKS = {
    "foreground": "editor.foreground",
    "descriptionForeground": "foreground",
    "disabledForeground": "descriptionForeground",
    "border": "panel.border",
    "panel.border": "editorGroup.border",
    "titleBar.activeBackground": "sideBar.background",
    "titleBar.activeForeground": "foreground",
    "commandCenter.background": "titleBar.activeBackground",
    "commandCenter.foreground": "descriptionForeground",
    "commandCenter.border": "border",
    "activityBar.background": "sideBar.background",
    "activityBar.foreground": "foreground",
    "activityBar.inactiveForeground": "descriptionForeground",
    "activityBar.activeBorder": "focusBorder",
    "sideBar.background": "editor.background",
    "sideBar.foreground": "foreground",
    "sideBarTitle.foreground": "sideBar.foreground",
    "sideBarSectionHeader.background": "sideBar.background",
    "list.activeSelectionForeground": "foreground",
    "list.inactiveSelectionBackground": "list.activeSelectionBackground",
    "list.highlightForeground": "foreground",
    "list.errorForeground": "editorError.foreground",
    "list.warningForeground": "editorWarning.foreground",
    "editor.foreground": "foreground",
    "editorLineNumber.foreground": "disabledForeground",
    "editorLineNumber.activeForeground": "foreground",
    "editorCursor.foreground": "foreground",
    "editorBracketMatch.background": "editor.selectionBackground",
    "editorGroupHeader.tabsBackground": "sideBar.background",
    "tab.activeBackground": "editor.background",
    "tab.activeForeground": "foreground",
    "tab.inactiveBackground": "editorGroupHeader.tabsBackground",
    "tab.inactiveForeground": "descriptionForeground",
    "tab.activeBorderTop": "focusBorder",
    "breadcrumb.foreground": "descriptionForeground",
    "panel.background": "editor.background",
    "panelTitle.activeForeground": "foreground",
    "panelTitle.inactiveForeground": "descriptionForeground",
    "panelTitle.activeBorder": "focusBorder",
    "terminal.background": "panel.background",
    "terminal.foreground": "foreground",
    "terminalCursor.foreground": "terminal.foreground",
    "terminal.selectionBackground": "editor.selectionBackground",
    "statusBar.background": "sideBar.background",
    "statusBar.foreground": "foreground",
    "statusBarItem.remoteBackground": "focusBorder",
    "statusBarItem.remoteForeground": "statusBar.foreground",
    "menu.background": "editorWidget.background",
    "editorWidget.background": "sideBar.background",
    "menu.foreground": "foreground",
    "menu.selectionBackground": "list.activeSelectionBackground",
    "menu.selectionForeground": "list.activeSelectionForeground",
    "menu.border": "widget.border",
    "widget.border": "editorWidget.border",
    "editorWidget.border": "border",
    "input.border": "widget.border",
    "button.background": "focusBorder",
    "button.foreground": "foreground",
    "button.secondaryBackground": "input.background",
    "editor.findMatchHighlightBackground": "editor.selectionBackground",
    "icon.monoForeground": "descriptionForeground",
}
# Keys that are painted on something other than the editor background.
_PAINTED_ON = {
    "list.hoverBackground": "sideBar.background",
    "list.activeSelectionBackground": "sideBar.background",
    "list.inactiveSelectionBackground": "sideBar.background",
    "menu.selectionBackground": "menu.background",
    "statusBarItem.hoverBackground": "statusBar.background",
    "commandCenter.border": "titleBar.activeBackground",
    "terminal.selectionBackground": "terminal.background",
}
_ANSI_KEYS = {
    "black": "Black", "red": "Red", "green": "Green", "brown": "Yellow",
    "blue": "Blue", "magenta": "Magenta", "cyan": "Cyan", "white": "White",
}
# tree-sitter capture -> TextMate scopes to try, most specific first.
_CAPTURE_SCOPES = {
    "comment": ["comment"],
    "string": ["string"],
    "string.documentation": ["string.quoted.docstring", "string"],
    "inline_code": ["markup.inline.raw", "string"],
    "info_string": ["markup.fenced_code.block", "string"],
    "number": ["constant.numeric"],
    "float": ["constant.numeric.float", "constant.numeric"],
    "boolean": ["constant.language.boolean", "constant.language"],
    "constant.builtin": ["constant.language"],
    "constant": ["variable.other.constant", "constant"],
    "json.null": ["constant.language.null", "constant.language"],
    "keyword": ["keyword"],
    "keyword.function": ["storage.type.function", "storage.type", "keyword"],
    "keyword.operator": ["keyword.operator", "keyword"],
    "keyword.return": ["keyword.control.flow", "keyword.control", "keyword"],
    "conditional": ["keyword.control.conditional", "keyword.control", "keyword"],
    "repeat": ["keyword.control.loop", "keyword.control", "keyword"],
    "exception": ["keyword.control.exception", "keyword.control", "keyword"],
    "include": ["keyword.control.import", "keyword.control", "keyword"],
    "operator": ["keyword.operator"],
    "punctuation.bracket": ["punctuation", "meta.brace"],
    "punctuation.delimiter": ["punctuation.separator", "punctuation"],
    "punctuation.special": ["punctuation.definition.template-expression", "punctuation"],
    "function": ["entity.name.function"],
    "function.call": ["meta.function-call.generic", "entity.name.function"],
    "method": ["entity.name.function.method", "entity.name.function"],
    "method.call": ["entity.name.function.method", "entity.name.function"],
    "class": ["entity.name.type.class", "entity.name.type", "entity.name.class"],
    "type": ["entity.name.type", "support.type"],
    "type.class": ["entity.name.type.class", "entity.name.type"],
    "type.builtin": ["support.type", "entity.name.type"],
    "variable": ["variable"],
    "variable.parameter": ["variable.parameter"],
    "tag": ["entity.name.tag"],
    "html.end_tag_error": ["invalid.illegal", "invalid"],
    "yaml.field": ["entity.name.tag.yaml", "entity.name.tag"],
    "json.label": ["support.type.property-name.json", "support.type.property-name"],
    "toml.type": ["support.type.property-name.table.toml", "entity.name.type"],
    "toml.datetime": ["constant.other.datetime", "constant.numeric"],
    "css.property": ["support.type.property-name.css", "support.type.property-name"],
    "regex.operator": ["string.regexp"],
    "regex.punctuation.bracket": ["string.regexp"],
    "heading": ["markup.heading"],
    "bold": ["markup.bold"],
    "italic": ["markup.italic"],
    "strikethrough": ["markup.strikethrough"],
    "link.label": ["string.other.link.title.markdown", "markup.underline.link"],
    "link.uri": ["markup.underline.link"],
    "list.marker": ["punctuation.definition.list.begin.markdown", "markup.list"],
    "attribute": ["entity.name.function.decorator", "entity.other.attribute-name"],
    "attribute.builtin": ["entity.name.function.decorator", "entity.other.attribute-name"],
    "constructor": ["entity.name.type.class", "entity.name.type"],
    "field": ["variable.other.property", "variable"],
    "property": ["variable.other.property", "support.type.property-name", "variable"],
    "parameter": ["variable.parameter"],
    "function.builtin": ["support.function", "entity.name.function"],
    "function.method": ["entity.name.function.method", "entity.name.function"],
    "function.method.builtin": ["support.function", "entity.name.function"],
    "function.macro": ["entity.name.function.macro", "entity.name.function"],
    "function.special": ["entity.name.function.macro", "entity.name.function"],
    "variable.builtin": ["variable.language"],
    "none": ["constant.language.null", "constant.language"],
    "preproc": ["meta.preprocessor", "keyword.control.directive", "keyword.control"],
    "import": ["keyword.control.import", "keyword.control"],
    "storageclass": ["storage.modifier", "keyword"],
    "type.qualifier": ["storage.modifier", "keyword"],
    "type.definition": ["entity.name.type"],
    "namespace": ["entity.name.namespace", "entity.name.type"],
    "module": ["entity.name.namespace", "entity.name.type"],
    "module.builtin": ["entity.name.namespace", "entity.name.type"],
    "label": ["entity.name.label", "variable"],
    "delimiter": ["punctuation.separator", "punctuation"],
    "escape": ["constant.character.escape"],
    "string.escape": ["constant.character.escape"],
    "constant.character": ["constant.character", "string"],
    "string.special": ["string.regexp", "string"],
    "string.special.regex": ["string.regexp", "string"],
    "string.special.symbol": ["constant.other.symbol", "constant"],
    "tag.attribute": ["entity.other.attribute-name"],
    "tag.delimiter": ["punctuation.definition.tag", "punctuation"],
    "text.uri": ["markup.underline.link"],
    "text.literal": ["markup.inline.raw", "string"],
    "markup.heading": ["markup.heading"],
    "heading.marker": ["markup.heading"],
    "markup.link": ["markup.underline.link"],
    "markup.raw": ["markup.inline.raw", "string"],
    "media": ["keyword.control.at-rule", "keyword.control"],
    "keyframes": ["keyword.control.at-rule", "keyword.control"],
    "supports": ["keyword.control.at-rule", "keyword.control"],
    "charset": ["keyword.control.at-rule", "keyword.control"],
}
_FONT_STYLES = {"bold": "bold", "italic": "italic", "underline": "underline", "strikethrough": "strike"}


def _read_jsonc(path: Path) -> dict:
    raw = path.read_text(encoding="utf-8")
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        raw = re.sub(r"^\s*//.*$", "", raw, flags=re.MULTILINE)  # whole-line comments
        raw = re.sub(r",(\s*[}\]])", r"\1", raw)  # trailing commas
        return json.loads(raw)


def complete_colors(given: dict[str, str], dark: bool) -> dict[str, str]:
    """Fill in every key the app needs and flatten translucent colours."""
    base = DARK_MODERN.colors

    def lookup(key: str, depth: int = 0) -> str | None:
        value = given.get(key)
        if isinstance(value, str) and value.startswith("#"):
            return value
        fallback = _FALLBACKS.get(key)
        return lookup(fallback, depth + 1) if fallback and depth < 8 else None

    background = blend(lookup("editor.background") or base["editor.background"], "#000000" if dark else "#ffffff")
    foreground = lookup("foreground") or ("#cccccc" if dark else "#333333")
    derived = {  # sensible values when a theme says nothing at all
        "list.hoverBackground": mix(background, foreground, 0.06),
        "list.activeSelectionBackground": mix(background, foreground, 0.16),
        "editor.lineHighlightBackground": mix(background, foreground, 0.05),
        "editor.selectionBackground": mix(background, foreground, 0.2),
        "statusBarItem.hoverBackground": mix(background, foreground, 0.12),
        "input.background": mix(background, foreground, 0.08),
        "button.secondaryHoverBackground": mix(background, foreground, 0.2),
        "scrollbarSlider.background": mix(background, foreground, 0.12),
        "scrollbarSlider.hoverBackground": mix(background, foreground, 0.22),
        "border": mix(background, foreground, 0.14),
        "focusBorder": foreground,
        "claude.accent": base["claude.accent"],
        "logo.foreground": base["logo.foreground"],
        "windowClose.hoverBackground": base["windowClose.hoverBackground"],
    }
    raw = {key: lookup(key) or derived.get(key) or base[key] for key in base}
    out = {"editor.background": background}
    for key in ("sideBar.background", "menu.background", "statusBar.background", "titleBar.activeBackground"):
        out[key] = blend(raw[key], background)
    for key, value in raw.items():
        out.setdefault(key, blend(value, out.get(_PAINTED_ON.get(key, ""), background)))
    # Legibility floors: things that must stay visible whatever the theme says.
    def too_close(a: str, b: str) -> bool:
        return sum(abs(x - y) for x, y in zip(_rgba(a)[:3], _rgba(b)[:3])) < 42

    if too_close(out["editor.selectionBackground"], background):
        out["editor.selectionBackground"] = mix(background, foreground, 0.2)
    if too_close(out["list.activeSelectionBackground"], out["sideBar.background"]):
        out["list.activeSelectionBackground"] = mix(out["sideBar.background"], foreground, 0.16)
    if too_close(out["input.background"], out["sideBar.background"]):
        out["input.background"] = mix(out["sideBar.background"], foreground, 0.09)
    if too_close(out["terminal.selectionBackground"], out["terminal.background"]):
        out["terminal.selectionBackground"] = mix(out["terminal.background"], foreground, 0.22)
    if too_close(out["editor.findMatchHighlightBackground"], background):
        out["editor.findMatchHighlightBackground"] = mix(background, foreground, 0.28)
    # A hairline that vanishes into the background would leave the panes unseparated.
    if out["border"] == background:
        out["border"] = derived["border"] if not lookup("border") else mix(background, foreground, 0.14)
    for key in ("menu.border", "widget.border", "input.border", "commandCenter.border"):
        if out[key] == out["menu.background"]:
            out[key] = out["border"]
    return out


def _syntax_from_token_colors(token_colors: list, fallback: dict[str, str]) -> dict[str, str]:
    rules: list[tuple[str, dict]] = []
    for rule in token_colors:
        scopes = rule.get("scope")
        scopes = [scopes] if isinstance(scopes, str) else (scopes or [])
        for scope in scopes:
            for selector in scope.split(","):
                selector = selector.strip()
                if selector and " " not in selector:
                    rules.append((selector, rule.get("settings") or {}))

    def match(scope: str) -> dict | None:
        best, best_len = None, -1
        for selector, settings in rules:  # later rules win ties, like VS Code
            if (scope == selector or scope.startswith(selector + ".")) and len(selector) >= best_len:
                best, best_len = settings, len(selector)
        return best

    syntax = {}
    for capture, scopes in _CAPTURE_SCOPES.items():
        settings = next((m for m in map(match, scopes) if m), None)
        if not settings:
            if capture in fallback:
                syntax[capture] = fallback[capture]
            continue
        parts = [_FONT_STYLES[w] for w in str(settings.get("fontStyle", "")).split() if w in _FONT_STYLES]
        colour = settings.get("foreground")
        if isinstance(colour, str) and colour.startswith("#"):
            parts.append(blend(colour, "#000000")[:7] if len(colour) > 7 else colour)
        if parts:
            syntax[capture] = " ".join(parts)
    return syntax


def load_vscode_theme(path: Path, id: str, label: str | None = None) -> VSTheme:
    """Build a VSTheme from a VS Code colour theme file (`colors` + `tokenColors`)."""
    data = _read_jsonc(path)
    dark = str(data.get("type", "dark")).lower() not in ("light", "hc-light")
    given = data.get("colors") or {}
    colors = complete_colors(given, dark)
    background = colors["terminal.background"]
    ansi = {}
    for name, suffix in _ANSI_KEYS.items():
        normal, bright = given.get(f"terminal.ansi{suffix}"), given.get(f"terminal.ansiBright{suffix}")
        ansi[name] = blend(normal, background) if normal else DARK_MODERN.ansi[name]
        ansi["bright" + name] = blend(bright, background) if bright else ansi[name]
    # No hue anywhere in the palette -> keep our own additions (icons, logo, Claude mark) hueless too.
    chromatic = [c for n, c in ansi.items() if n not in ("black", "white", "brightblack", "brightwhite")]
    mono = max(map(_saturation, chromatic + [colors["focusBorder"]])) < 0.08
    if mono:
        colors["claude.accent"] = colors["foreground"]
        colors["spotify.accent"] = colors["foreground"]
        colors["logo.foreground"] = colors["foreground"]
        colors["windowClose.hoverBackground"] = colors["button.background"]
        colors["button.secondaryBackground"] = mix(colors["menu.background"], colors["foreground"], 0.14)
        colors["button.secondaryHoverBackground"] = mix(colors["menu.background"], colors["foreground"], 0.22)
        for key in ("modified", "added", "deleted", "untracked"):
            colors[f"gitDecoration.{key}ResourceForeground"] = colors["descriptionForeground"]
    return VSTheme(
        id=id,
        label=label or str(data.get("name") or path.stem),
        dark=dark,
        colors=colors,
        syntax=_syntax_from_token_colors(data.get("tokenColors") or [], {} if mono else DARK_MODERN.syntax),
        ansi=ansi,
        mono_icons=mono,
    )


# --- the "terminal" theme (follows the desktop theme) ------------------------------

def _omarchy_dir() -> Path:
    state = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local" / "state")
    return state / "omarchy" / "current"


def terminal_theme_source() -> Path | None:
    """The file the terminal theme is built from, if this machine has one."""
    theme_dir = _omarchy_dir() / "theme"
    for name in ("vscode-theme.json", "colors.toml"):
        if (theme_dir / name).is_file():
            return theme_dir / name
    return None


def terminal_theme_stamp() -> tuple | None:
    """Changes whenever the desktop theme does (used to live-reload)."""
    source = terminal_theme_source()
    try:
        stat = source.stat() if source else None
    except OSError:
        return None
    return (str(source), stat.st_ino, stat.st_mtime_ns) if stat else None


def _theme_from_palette(path: Path, id: str, label: str) -> VSTheme:
    """Fallback for Omarchy themes without a VS Code JSON: build from colors.toml."""
    p = tomllib.loads(path.read_text(encoding="utf-8"))
    bg, fg = p.get("background", "#000000"), p.get("foreground", "#ffffff")
    accent, muted = p.get("accent", fg), p.get("muted", mix(bg, fg, 0.5))
    lighter = p.get("lighter_background", mix(bg, fg, 0.1))
    data = {
        "name": label,
        "type": p.get("mode", "dark"),
        "colors": {
            "editor.background": bg, "foreground": fg, "focusBorder": accent,
            "descriptionForeground": muted, "disabledForeground": p.get("dark_foreground", muted),
            "panel.border": lighter, "editor.selectionBackground": p.get("selection", lighter),
            "list.activeSelectionBackground": lighter, "statusBarItem.remoteForeground": bg,
            "button.foreground": bg,
            **{f"terminal.ansi{s}": p[k] for k, s in (
                ("red", "Red"), ("green", "Green"), ("yellow", "Yellow"), ("blue", "Blue"),
                ("magenta", "Magenta"), ("cyan", "Cyan")) if k in p},
            "terminal.ansiBlack": bg, "terminal.ansiWhite": fg, "terminal.ansiBrightBlack": muted,
        },
        "tokenColors": [
            {"scope": "comment", "settings": {"foreground": muted, "fontStyle": "italic"}},
            {"scope": "string", "settings": {"foreground": p.get("green", fg)}},
            {"scope": "constant", "settings": {"foreground": p.get("orange", fg)}},
            {"scope": "keyword", "settings": {"foreground": p.get("magenta", fg)}},
            {"scope": "entity.name.function", "settings": {"foreground": p.get("blue", fg)}},
            {"scope": "entity.name.type", "settings": {"foreground": p.get("yellow", fg)}},
            {"scope": "variable", "settings": {"foreground": fg}},
        ],
    }
    scratch = Path(os.environ.get("XDG_RUNTIME_DIR") or "/tmp") / f"tscode-palette-{os.getpid()}.json"
    scratch.write_text(json.dumps(data))
    try:
        return load_vscode_theme(scratch, id, label)
    finally:
        scratch.unlink(missing_ok=True)


def terminal_theme_label() -> str:
    try:
        name = (_omarchy_dir() / "theme.name").read_text().strip().replace("-", " ").title()
    except OSError:
        name = ""
    return f"Terminal ({name})" if name else "Terminal"


def load_terminal_theme() -> VSTheme | None:
    source = terminal_theme_source()
    if source is None:
        return None
    label = terminal_theme_label()
    try:
        if source.suffix == ".json":
            return load_vscode_theme(source, "terminal", label)
        return _theme_from_palette(source, "terminal", label)
    except (OSError, ValueError, KeyError):
        return None


# --- registry ----------------------------------------------------------------------

def user_theme_dir() -> Path:
    config = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    return config / "tscode" / "themes"


def available_themes() -> dict[str, str]:
    """Theme id -> label, in picker order."""
    themes = {DARK_MODERN.id: DARK_MODERN.label}
    if terminal_theme_source() is not None:
        themes["terminal"] = terminal_theme_label()
    try:
        for path in sorted(user_theme_dir().glob("*.json")):
            themes[path.stem] = path.stem.replace("-", " ").replace("_", " ").title()
    except OSError:
        pass
    return themes


def load_theme(theme_id: str | None) -> VSTheme:
    """Load a theme by id; anything unknown or broken falls back to Dark Modern."""
    if theme_id == "terminal":
        return load_terminal_theme() or DARK_MODERN
    if theme_id and theme_id != DARK_MODERN.id:
        path = user_theme_dir() / f"{theme_id}.json"
        try:
            return load_vscode_theme(path, theme_id)
        except (OSError, ValueError, KeyError):
            pass
    return DARK_MODERN
