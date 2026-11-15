//! Icon glyphs (generated from the Python version's icon table).
//!
//! The default set uses Nerd Font codicons/devicons. Set TSCODE_ICONS=ascii for
//! terminals without a Nerd Font.

use crate::screen::Rgb;
use crate::theme::VSTheme;
use std::path::Path;
use std::sync::OnceLock;

pub fn ascii() -> bool {
    static ASCII: OnceLock<bool> = OnceLock::new();
    *ASCII.get_or_init(|| std::env::var("TSCODE_ICONS").map(|v| v.eq_ignore_ascii_case("ascii")).unwrap_or(false))
}

/// (name, Nerd Font glyph, ASCII stand-in)
const ICONS: &[(&str, &str, &str)] = &[
    ("logo", "\u{e70c}", "<>"),
    ("files", "\u{eaf0}", "F"),
    ("search", "\u{ea6d}", "S"),
    ("scm", "\u{ea68}", "G"),
    ("debug", "\u{eb91}", "D"),
    ("extensions", "\u{eae6}", "X"),
    ("claude", "\u{273b}", "*"),
    ("account", "\u{eb99}", "@"),
    ("gear", "\u{eb51}", "%"),
    ("chevron_right", "\u{eab6}", ">"),
    ("chevron_down", "\u{eab4}", "v"),
    ("chevron_up", "\u{eab7}", "^"),
    ("close", "\u{ea76}", "x"),
    ("dot", "\u{ea71}", "o"),
    ("terminal", "\u{ea85}", ">_"),
    ("add", "\u{ea60}", "+"),
    ("trash", "\u{ea81}", "#"),
    ("ellipsis", "\u{ea7c}", "..."),
    ("error", "\u{ea87}", "x"),
    ("warning", "\u{ea6c}", "!"),
    ("bell", "\u{eaa2}", "o"),
    ("remote", "\u{eb3a}", "><"),
    ("arrow_left", "\u{ea9b}", "<-"),
    ("arrow_right", "\u{ea9c}", "->"),
    ("refresh", "\u{eb37}", "@"),
    ("new_file", "\u{ea7f}", "+"),
    ("collapse_all", "\u{eac5}", "="),
    ("win_close", "\u{eab8}", "X"),
    ("layout_left", "\u{ebf3}", "[|"),
    ("layout_panel", "\u{ebf2}", "[_]"),
    ("layout_right", "\u{ebf4}", "|]"),
    ("file", "\u{ea7b}", "-"),
    ("branch", "\u{ea68}", "Y"),
    ("sep", "\u{eab6}", ">"),
    ("spotify", "\u{f1bc}", "#"),
    ("media_prev", "\u{f048}", "|<"),
    ("media_play", "\u{f04b}", ">"),
    ("media_pause", "\u{f04c}", "||"),
    ("media_next", "\u{f051}", ">|"),
    ("shuffle", "\u{f074}", "~"),
    ("volume_up", "\u{f028}", "+"),
    ("volume_down", "\u{f027}", "-"),
    ("sign_out", "\u{ea6e}", "->"),
    ("cap_left", "\u{e0b6}", ""),
    ("cap_right", "\u{e0b4}", ""),
];

pub fn icon(name: &str) -> &'static str {
    for (key, nerd, plain) in ICONS {
        if *key == name {
            return if ascii() { plain } else { nerd };
        }
    }
    "?"
}

/// (extension, glyph, colour); colours follow the Seti icon theme.
const FILE_ICONS: &[(&str, &str, &str)] = &[
    (".py", "\u{e73c}", "#4b8bbe"),
    (".js", "\u{e74e}", "#cbcb41"),
    (".mjs", "\u{e74e}", "#cbcb41"),
    (".jsx", "\u{e74e}", "#519aba"),
    (".ts", "\u{e628}", "#519aba"),
    (".tsx", "\u{e628}", "#519aba"),
    (".json", "\u{e60b}", "#cbcb41"),
    (".md", "\u{e73e}", "#519aba"),
    (".html", "\u{e736}", "#e37933"),
    (".css", "\u{e749}", "#519aba"),
    (".tcss", "\u{e749}", "#519aba"),
    (".rs", "\u{e7a8}", "#dea584"),
    (".go", "\u{e627}", "#519aba"),
    (".sh", "\u{f489}", "#8dc149"),
    (".bash", "\u{f489}", "#8dc149"),
    (".c", "\u{e61e}", "#519aba"),
    (".h", "\u{e61e}", "#a074c4"),
    (".cpp", "\u{e61d}", "#519aba"),
    (".java", "\u{e738}", "#cc3e44"),
    (".rb", "\u{e739}", "#cc3e44"),
    (".toml", "\u{e615}", "#6d8086"),
    (".yaml", "\u{e615}", "#a074c4"),
    (".yml", "\u{e615}", "#a074c4"),
    (".ini", "\u{e615}", "#6d8086"),
    (".cfg", "\u{e615}", "#6d8086"),
    (".lock", "\u{f023}", "#6d8086"),
    (".png", "\u{f1c5}", "#a074c4"),
    (".jpg", "\u{f1c5}", "#a074c4"),
    (".jpeg", "\u{f1c5}", "#a074c4"),
    (".gif", "\u{f1c5}", "#a074c4"),
    (".svg", "\u{f1c5}", "#cbcb41"),
    (".sql", "\u{f1c0}", "#f55385"),
    (".txt", "\u{f15c}", "#cccccc"),
];
const NAME_ICONS: &[(&str, &str, &str)] = &[
    (".gitignore", "\u{e702}", "#41535b"),
    (".gitattributes", "\u{e702}", "#41535b"),
    ("dockerfile", "\u{f308}", "#519aba"),
    ("claude.md", "\u{273b}", "#d97757"),
];
const DEFAULT_FILE: (&str, &str) = ("\u{ea7b}", "#cccccc");

/// (glyph, colour) for a file. Monochrome themes get one quiet colour.
pub fn file_icon(path: &Path, theme: &VSTheme) -> (&'static str, Rgb) {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let suffix = match name.rfind('.') {
        Some(dot) if dot > 0 => &name[dot..],
        _ => "",
    };
    let (mut glyph, colour) = NAME_ICONS
        .iter()
        .find(|(key, _, _)| *key == name)
        .or_else(|| FILE_ICONS.iter().find(|(key, _, _)| *key == suffix))
        .map(|(_, glyph, colour)| (*glyph, *colour))
        .unwrap_or(DEFAULT_FILE);
    if ascii() {
        glyph = "-";
    }
    if theme.mono_icons {
        return (glyph, theme.c("icon.monoForeground"));
    }
    (glyph, Rgb::hex(colour))
}
