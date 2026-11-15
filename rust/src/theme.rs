//! Colour themes.
//!
//! A `VSTheme` is the single source of truth for every colour in the app. It is keyed
//! the same way VS Code themes are (`editor.background`, `tab.activeBorderTop`...) so
//! real VS Code theme JSON maps straight onto it (see `load_vscode_theme`).
//!
//! Themes available to the user (`available_themes()`):
//!
//! * "vscode-dark-modern"  built in
//! * "terminal"            whatever the desktop/terminal is themed with: Omarchy's
//!                         current theme. Reloads live.
//! * any VS Code theme JSON dropped into ~/.config/tscode/themes/

use crate::screen::{Rgb, BOLD, ITALIC, STRIKE, UNDERLINE};
use crate::settings::{config_dir, state_dir};
use crate::theme_data::*;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const DARK_MODERN_ID: &str = "vscode-dark-modern";

/// How one syntax capture is drawn.
#[derive(Clone, Copy, Default, Debug)]
pub struct Syn {
    pub fg: Option<Rgb>,
    pub attr: u8,
}

pub struct VSTheme {
    pub id: String,
    pub label: String,
    pub colors: HashMap<String, Rgb>,
    /// ANSI colours 0-15 for the terminals.
    pub ansi: [Rgb; 16],
    /// Draw file icons in one quiet colour (monochrome themes).
    pub mono_icons: bool,
    syn: Vec<Syn>,
    lang_syn: HashMap<String, Vec<Syn>>,
}

impl VSTheme {
    /// A colour by its VS Code key.
    pub fn c(&self, key: &str) -> Rgb {
        self.colors.get(key).copied().unwrap_or(Rgb(255, 0, 255))
    }

    /// Syntax styles for a language, indexed like `capture_names()`.
    pub fn syn(&self, language: Option<&str>) -> &[Syn] {
        language.and_then(|l| self.lang_syn.get(l)).unwrap_or(&self.syn)
    }
}

/// Every capture name the highlighter recognises (the keys of Dark Modern's syntax table).
pub fn capture_names() -> Vec<&'static str> {
    DM_SYNTAX.iter().map(|(name, _)| *name).collect()
}

fn parse_syn(style: &str) -> Syn {
    let mut syn = Syn::default();
    for word in style.split_whitespace() {
        match word {
            "bold" => syn.attr |= BOLD,
            "italic" => syn.attr |= ITALIC,
            "underline" => syn.attr |= UNDERLINE,
            "strike" => syn.attr |= STRIKE,
            w if w.starts_with('#') => syn.fg = Some(Rgb::hex(w)),
            _ => {}
        }
    }
    syn
}

/// Styles in `capture_names()` order. A capture the theme doesn't know borrows from
/// its parent ("function.method" -> "function").
fn syn_table(syntax: &HashMap<String, String>) -> Vec<Syn> {
    capture_names()
        .iter()
        .map(|name| {
            let mut key: &str = name;
            loop {
                if let Some(style) = syntax.get(key) {
                    return parse_syn(style);
                }
                match key.rfind('.') {
                    Some(dot) => key = &key[..dot],
                    None => return Syn::default(),
                }
            }
        })
        .collect()
}

fn build(
    id: &str,
    label: &str,
    colors: HashMap<String, String>,
    syntax: HashMap<String, String>,
    language_syntax: &[(&str, &str, &str)],
    ansi: [Rgb; 16],
    mono_icons: bool,
) -> VSTheme {
    let mut lang_syn = HashMap::new();
    let mut languages: Vec<&str> = language_syntax.iter().map(|(l, _, _)| *l).collect();
    languages.dedup();
    for language in languages {
        let mut merged = syntax.clone();
        for (l, capture, style) in language_syntax {
            if *l == language {
                merged.insert(capture.to_string(), style.to_string());
            }
        }
        lang_syn.insert(language.to_string(), syn_table(&merged));
    }
    VSTheme {
        id: id.to_string(),
        label: label.to_string(),
        colors: colors.iter().map(|(k, v)| (k.clone(), Rgb::hex(v))).collect(),
        ansi,
        mono_icons,
        syn: syn_table(&syntax),
        lang_syn,
    }
}

fn pairs(table: &[(&str, &str)]) -> HashMap<String, String> {
    table.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn lookup_pair<'a>(table: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// VS Code's default dark theme.
pub fn dark_modern() -> VSTheme {
    let ansi = DM_ANSI.map(Rgb::hex);
    build(DARK_MODERN_ID, "Dark Modern", pairs(DM_COLORS), pairs(DM_SYNTAX), DM_LANGUAGE_SYNTAX, ansi, false)
}

// --- colour helpers ------------------------------------------------------------

fn rgba(value: &str) -> Option<(f64, f64, f64, f64)> {
    let h = value.trim().trim_start_matches('#');
    let h: String = if h.len() == 3 || h.len() == 4 { h.chars().flat_map(|c| [c, c]).collect() } else { h.to_string() };
    if !(h.len() == 6 || h.len() == 8) || !h.is_ascii() {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(f64::from);
    let alpha = if h.len() == 8 { p(6)? / 255.0 } else { 1.0 };
    Some((p(0)?, p(2)?, p(4)?, alpha))
}

fn to_hex(r: f64, g: f64, b: f64) -> String {
    format!("#{:02x}{:02x}{:02x}", r.round() as u8, g.round() as u8, b.round() as u8)
}

/// Flatten a possibly translucent colour (#rrggbbaa) onto an opaque one.
fn blend(value: &str, over: &str) -> Option<String> {
    let (r, g, b, a) = rgba(value)?;
    let (br, bg, bb, _) = rgba(over)?;
    Some(to_hex(r * a + br * (1.0 - a), g * a + bg * (1.0 - a), b * a + bb * (1.0 - a)))
}

/// `amount` of the way from colour a to colour b.
fn mix(a: &str, b: &str, amount: f64) -> Option<String> {
    let (ar, ag, ab, _) = rgba(a)?;
    let (br, bg, bb, _) = rgba(b)?;
    Some(to_hex(ar + (br - ar) * amount, ag + (bg - ag) * amount, ab + (bb - ab) * amount))
}

fn saturation(value: &str) -> f64 {
    let Some((r, g, b, _)) = rgba(value) else { return 0.0 };
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max == 0.0 {
        0.0
    } else {
        (max - min) / max
    }
}

fn too_close(a: &str, b: &str) -> bool {
    match (rgba(a), rgba(b)) {
        (Some(a), Some(b)) => (a.0 - b.0).abs() + (a.1 - b.1).abs() + (a.2 - b.2).abs() < 42.0,
        _ => false,
    }
}

// --- loading VS Code theme JSON --------------------------------------------------

/// JSON, or JSON with whole-line comments and trailing commas.
fn read_jsonc(raw: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str(raw) {
        return Some(value);
    }
    let without_comments: String =
        raw.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    let chars: Vec<char> = without_comments.chars().collect();
    let mut cleaned = String::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        cleaned.push(c);
    }
    serde_json::from_str(&cleaned).ok()
}

fn given_color<'a>(given: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    given.get(key).and_then(Value::as_str).filter(|v| v.starts_with('#'))
}

fn lookup<'a>(given: &'a Map<String, Value>, key: &str, depth: usize) -> Option<&'a str> {
    if let Some(value) = given_color(given, key) {
        return Some(value);
    }
    match lookup_pair(FALLBACKS, key) {
        Some(fallback) if depth < 8 => lookup(given, fallback, depth + 1),
        _ => None,
    }
}

/// Fill in every key the app needs and flatten translucent colours.
fn complete_colors(given: &Map<String, Value>, dark: bool) -> Option<HashMap<String, String>> {
    let base = |key: &str| lookup_pair(DM_COLORS, key).unwrap_or("#ff00ff");
    let background =
        blend(lookup(given, "editor.background", 0).unwrap_or(base("editor.background")), if dark { "#000000" } else { "#ffffff" })?;
    let foreground = lookup(given, "foreground", 0).unwrap_or(if dark { "#cccccc" } else { "#333333" }).to_string();
    let shade = |amount: f64| mix(&background, &foreground, amount);
    // Sensible values when a theme says nothing at all.
    let derived: HashMap<&str, String> = HashMap::from([
        ("list.hoverBackground", shade(0.06)?),
        ("list.activeSelectionBackground", shade(0.16)?),
        ("editor.lineHighlightBackground", shade(0.05)?),
        ("editor.selectionBackground", shade(0.2)?),
        ("statusBarItem.hoverBackground", shade(0.12)?),
        ("input.background", shade(0.08)?),
        ("button.secondaryHoverBackground", shade(0.2)?),
        ("scrollbarSlider.background", shade(0.12)?),
        ("scrollbarSlider.hoverBackground", shade(0.22)?),
        ("border", shade(0.14)?),
        ("focusBorder", foreground.clone()),
        ("claude.accent", base("claude.accent").to_string()),
        ("logo.foreground", base("logo.foreground").to_string()),
        ("windowClose.hoverBackground", base("windowClose.hoverBackground").to_string()),
    ]);
    let raw: Vec<(&str, String)> = DM_COLORS
        .iter()
        .map(|(key, default)| {
            let value = lookup(given, key, 0).map(str::to_string).or_else(|| derived.get(key).cloned()).unwrap_or(default.to_string());
            (*key, value)
        })
        .collect();
    let raw_value = |key: &str| raw.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str()).unwrap_or("#ff00ff");

    let mut out: HashMap<String, String> = HashMap::new();
    out.insert("editor.background".into(), background.clone());
    for key in ["sideBar.background", "menu.background", "statusBar.background", "titleBar.activeBackground"] {
        out.insert(key.into(), blend(raw_value(key), &background)?);
    }
    for (key, value) in &raw {
        if out.contains_key(*key) {
            continue;
        }
        let under = lookup_pair(PAINTED_ON, key).and_then(|k| out.get(k)).cloned().unwrap_or(background.clone());
        out.insert(key.to_string(), blend(value, &under)?);
    }

    // Legibility floors: things that must stay visible whatever the theme says.
    let side = out["sideBar.background"].clone();
    if too_close(&out["editor.selectionBackground"], &background) {
        out.insert("editor.selectionBackground".into(), shade(0.2)?);
    }
    if too_close(&out["list.activeSelectionBackground"], &side) {
        out.insert("list.activeSelectionBackground".into(), mix(&side, &foreground, 0.16)?);
    }
    if too_close(&out["input.background"], &side) {
        out.insert("input.background".into(), mix(&side, &foreground, 0.09)?);
    }
    if too_close(&out["editor.findMatchHighlightBackground"], &background) {
        out.insert("editor.findMatchHighlightBackground".into(), shade(0.28)?);
    }
    // A hairline that vanishes into the background would leave the panes unseparated.
    if out["border"] == background {
        out.insert("border".into(), shade(0.14)?);
    }
    let (menu, border) = (out["menu.background"].clone(), out["border"].clone());
    for key in ["menu.border", "widget.border", "input.border", "commandCenter.border"] {
        if out[key] == menu {
            out.insert(key.into(), border.clone());
        }
    }
    Some(out)
}

fn syntax_from_token_colors(token_colors: &[Value], fallback: &HashMap<String, String>) -> HashMap<String, String> {
    let empty = Map::new();
    let mut rules: Vec<(String, &Map<String, Value>)> = Vec::new();
    for rule in token_colors {
        let settings = rule.get("settings").and_then(Value::as_object).unwrap_or(&empty);
        let scopes: Vec<&str> = match rule.get("scope") {
            Some(Value::String(scope)) => vec![scope.as_str()],
            Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        for scope in scopes {
            for selector in scope.split(',') {
                let selector = selector.trim();
                if !selector.is_empty() && !selector.contains(' ') {
                    rules.push((selector.to_string(), settings));
                }
            }
        }
    }
    // Later rules win ties, like VS Code.
    let find = |scope: &str| -> Option<&Map<String, Value>> {
        let mut best: Option<(&Map<String, Value>, usize)> = None;
        for (selector, settings) in &rules {
            let matches = scope == selector
                || (scope.starts_with(selector.as_str()) && scope.as_bytes().get(selector.len()) == Some(&b'.'));
            if matches && best.map(|(_, len)| selector.len() >= len).unwrap_or(true) {
                best = Some((settings, selector.len()));
            }
        }
        best.map(|(settings, _)| settings).filter(|settings| !settings.is_empty())
    };

    let mut syntax = HashMap::new();
    for (capture, scopes) in CAPTURE_SCOPES {
        let Some(settings) = scopes.iter().find_map(|scope| find(scope)) else {
            if let Some(style) = fallback.get(*capture) {
                syntax.insert(capture.to_string(), style.clone());
            }
            continue;
        };
        let mut parts: Vec<String> = Vec::new();
        for word in settings.get("fontStyle").and_then(Value::as_str).unwrap_or("").split_whitespace() {
            match word {
                "bold" | "italic" | "underline" => parts.push(word.to_string()),
                "strikethrough" => parts.push("strike".to_string()),
                _ => {}
            }
        }
        if let Some(colour) = settings.get("foreground").and_then(Value::as_str).filter(|c| c.starts_with('#')) {
            if colour.len() > 7 {
                parts.extend(blend(colour, "#000000"));
            } else {
                parts.push(colour.to_string());
            }
        }
        if !parts.is_empty() {
            syntax.insert(capture.to_string(), parts.join(" "));
        }
    }
    syntax
}

/// Build a VSTheme from VS Code colour theme data (`colors` + `tokenColors`).
fn theme_from_value(data: &Value, id: &str, label: &str) -> Option<VSTheme> {
    let kind = data.get("type").and_then(Value::as_str).unwrap_or("dark").to_lowercase();
    let dark = kind != "light" && kind != "hc-light";
    let empty = Map::new();
    let given = data.get("colors").and_then(Value::as_object).unwrap_or(&empty);
    let mut colors = complete_colors(given, dark)?;
    let background = colors["terminal.background"].clone();

    let mut ansi_hex: Vec<String> = vec![String::new(); 16];
    for (index, suffix) in ANSI_SUFFIXES.iter().enumerate() {
        let normal = given.get(&format!("terminal.ansi{suffix}")).and_then(Value::as_str);
        let bright = given.get(&format!("terminal.ansiBright{suffix}")).and_then(Value::as_str);
        ansi_hex[index] = match normal {
            Some(value) => blend(value, &background)?,
            None => DM_ANSI[index].to_string(),
        };
        ansi_hex[index + 8] = match bright {
            Some(value) => blend(value, &background)?,
            None => ansi_hex[index].clone(),
        };
    }
    // No hue anywhere in the palette -> keep our own additions (icons, logo, Claude mark) hueless too.
    let chromatic = (1..7).chain(9..15).map(|i| saturation(&ansi_hex[i]));
    let mono = chromatic.chain([saturation(&colors["focusBorder"])]).fold(0.0, f64::max) < 0.08;
    if mono {
        let foreground = colors["foreground"].clone();
        let menu = colors["menu.background"].clone();
        for key in ["claude.accent", "spotify.accent", "logo.foreground"] {
            colors.insert(key.into(), foreground.clone());
        }
        colors.insert("windowClose.hoverBackground".into(), colors["button.background"].clone());
        colors.insert("button.secondaryBackground".into(), mix(&menu, &foreground, 0.14)?);
        colors.insert("button.secondaryHoverBackground".into(), mix(&menu, &foreground, 0.22)?);
        for key in ["modified", "added", "deleted", "untracked"] {
            colors.insert(format!("gitDecoration.{key}ResourceForeground"), colors["descriptionForeground"].clone());
        }
    }
    let tokens = data.get("tokenColors").and_then(Value::as_array).cloned().unwrap_or_default();
    let fallback = if mono { HashMap::new() } else { pairs(DM_SYNTAX) };
    let syntax = syntax_from_token_colors(&tokens, &fallback);
    let mut ansi = [Rgb::default(); 16];
    for (slot, hex) in ansi.iter_mut().zip(&ansi_hex) {
        *slot = Rgb::hex(hex);
    }
    Some(build(id, label, colors, syntax, &[], ansi, mono))
}

fn load_vscode_theme(path: &Path, id: &str, label: Option<&str>) -> Option<VSTheme> {
    let data = read_jsonc(&fs::read_to_string(path).ok()?)?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let name = label.map(str::to_string).or_else(|| data.get("name").and_then(Value::as_str).map(str::to_string)).unwrap_or(stem);
    theme_from_value(&data, id, &name)
}

// --- the "terminal" theme (follows the desktop theme) ------------------------------

fn omarchy_dir() -> PathBuf {
    state_dir().join("omarchy").join("current")
}

/// The file the terminal theme is built from, if this machine has one.
fn terminal_theme_source() -> Option<PathBuf> {
    let theme_dir = omarchy_dir().join("theme");
    ["vscode-theme.json", "colors.toml"].iter().map(|name| theme_dir.join(name)).find(|path| path.is_file())
}

/// Changes whenever the desktop theme does (used to live-reload).
pub fn terminal_theme_stamp() -> Option<(PathBuf, u64, i64, i64)> {
    let source = terminal_theme_source()?;
    let meta = fs::metadata(&source).ok()?;
    Some((source, meta.ino(), meta.mtime(), meta.mtime_nsec()))
}

/// The flat `key = "value"` pairs of a colors.toml palette.
fn read_palette(raw: &str) -> HashMap<String, String> {
    let mut palette = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim();
            let value = value.strip_prefix('"').and_then(|v| v.split('"').next()).unwrap_or(value);
            palette.insert(key.trim().to_string(), value.to_string());
        }
    }
    palette
}

/// Fallback for Omarchy themes without a VS Code JSON: build from colors.toml.
fn theme_from_palette(path: &Path, id: &str, label: &str) -> Option<VSTheme> {
    let p = read_palette(&fs::read_to_string(path).ok()?);
    let get = |key: &str| p.get(key).cloned();
    let bg = get("background").unwrap_or("#000000".into());
    let fg = get("foreground").unwrap_or("#ffffff".into());
    let accent = get("accent").unwrap_or(fg.clone());
    let muted = get("muted").or_else(|| mix(&bg, &fg, 0.5))?;
    let lighter = get("lighter_background").or_else(|| mix(&bg, &fg, 0.1))?;
    let mut colors = json!({
        "editor.background": bg, "foreground": fg, "focusBorder": accent,
        "descriptionForeground": muted, "disabledForeground": get("dark_foreground").unwrap_or(muted.clone()),
        "panel.border": lighter, "editor.selectionBackground": get("selection").unwrap_or(lighter.clone()),
        "list.activeSelectionBackground": lighter, "statusBarItem.remoteForeground": bg,
        "button.foreground": bg,
        "terminal.ansiBlack": bg, "terminal.ansiWhite": fg, "terminal.ansiBrightBlack": muted,
    });
    for (key, suffix) in [("red", "Red"), ("green", "Green"), ("yellow", "Yellow"), ("blue", "Blue"), ("magenta", "Magenta"), ("cyan", "Cyan")] {
        if let Some(value) = get(key) {
            colors[format!("terminal.ansi{suffix}")] = Value::String(value);
        }
    }
    let token = |scope: &str, key: &str| json!({"scope": scope, "settings": {"foreground": get(key).unwrap_or(fg.clone())}});
    let data = json!({
        "name": label,
        "type": get("mode").unwrap_or("dark".into()),
        "colors": colors,
        "tokenColors": [
            {"scope": "comment", "settings": {"foreground": muted, "fontStyle": "italic"}},
            token("string", "green"),
            token("constant", "orange"),
            token("keyword", "magenta"),
            token("entity.name.function", "blue"),
            token("entity.name.type", "yellow"),
            {"scope": "variable", "settings": {"foreground": fg}},
        ],
    });
    theme_from_value(&data, id, label)
}

fn title_case(text: &str) -> String {
    let mut out = String::new();
    let mut start = true;
    for c in text.chars() {
        if start && c.is_alphabetic() {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        start = !c.is_alphanumeric();
    }
    out
}

fn terminal_theme_label() -> String {
    let name = fs::read_to_string(omarchy_dir().join("theme.name")).unwrap_or_default();
    let name = title_case(&name.trim().replace('-', " "));
    if name.is_empty() {
        "Terminal".to_string()
    } else {
        format!("Terminal ({name})")
    }
}

fn load_terminal_theme() -> Option<VSTheme> {
    let source = terminal_theme_source()?;
    let label = terminal_theme_label();
    if source.extension().map(|e| e == "json").unwrap_or(false) {
        load_vscode_theme(&source, "terminal", Some(&label))
    } else {
        theme_from_palette(&source, "terminal", &label)
    }
}

// --- registry ----------------------------------------------------------------------

fn user_theme_dir() -> PathBuf {
    config_dir().join("tscode").join("themes")
}

/// (theme id, label) in picker order.
pub fn available_themes() -> Vec<(String, String)> {
    let mut themes = vec![(DARK_MODERN_ID.to_string(), "Dark Modern".to_string())];
    if terminal_theme_source().is_some() {
        themes.push(("terminal".to_string(), terminal_theme_label()));
    }
    if let Ok(entries) = fs::read_dir(user_theme_dir()) {
        let mut stems: Vec<String> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().map(|e| e == "json").unwrap_or(false))
            .filter_map(|path| path.file_stem().map(|s| s.to_string_lossy().to_string()))
            .collect();
        stems.sort();
        for stem in stems {
            if themes.iter().all(|(id, _)| *id != stem) {
                let label = title_case(&stem.replace(['-', '_'], " "));
                themes.push((stem, label));
            }
        }
    }
    themes
}

/// Load a theme by id; anything unknown or broken falls back to Dark Modern.
pub fn load_theme(theme_id: Option<&str>) -> VSTheme {
    match theme_id {
        Some("terminal") => load_terminal_theme().unwrap_or_else(dark_modern),
        Some(id) if id != DARK_MODERN_ID => {
            load_vscode_theme(&user_theme_dir().join(format!("{id}.json")), id, None).unwrap_or_else(dark_modern)
        }
        _ => dark_modern(),
    }
}
