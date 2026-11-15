//! Which language a file is: the tree-sitter grammar id and the name shown in the status bar.

use std::path::Path;

type Language = (Option<&'static str>, &'static str);

const EXTENSIONS: &[(&str, &str, &str)] = &[
    ("py", "python", "Python"), ("pyi", "python", "Python"),
    ("c", "c", "C"), ("h", "c", "C"),
    ("cpp", "cpp", "C++"), ("cc", "cpp", "C++"), ("cxx", "cpp", "C++"),
    ("hpp", "cpp", "C++"), ("hh", "cpp", "C++"),
    ("js", "javascript", "JavaScript"), ("mjs", "javascript", "JavaScript"),
    ("cjs", "javascript", "JavaScript"), ("jsx", "javascript", "JavaScript JSX"),
    ("ts", "typescript", "TypeScript"), ("mts", "typescript", "TypeScript"),
    ("tsx", "tsx", "TypeScript JSX"),
    ("java", "java", "Java"),
    ("json", "json", "JSON"), ("jsonc", "json", "JSON with Comments"),
    ("html", "html", "HTML"), ("htm", "html", "HTML"),
    ("css", "css", "CSS"), ("tcss", "css", "CSS"),
    ("md", "markdown", "Markdown"), ("markdown", "markdown", "Markdown"),
    ("toml", "toml", "TOML"),
    ("yaml", "yaml", "YAML"), ("yml", "yaml", "YAML"),
    ("rs", "rust", "Rust"),
    ("go", "go", "Go"),
    ("sh", "bash", "Shell Script"), ("bash", "bash", "Shell Script"), ("zsh", "bash", "Shell Script"),
    ("sql", "sql", "SQL"),
    ("xml", "xml", "XML"), ("svg", "xml", "XML"),
    ("rb", "ruby", "Ruby"),
    ("lua", "lua", "Lua"),
    ("php", "php", "PHP"),
];

const FILENAMES: &[(&str, &str, &str)] = &[
    (".bashrc", "bash", "Shell Script"), (".bash_profile", "bash", "Shell Script"),
    (".zshrc", "bash", "Shell Script"), (".profile", "bash", "Shell Script"),
    ("pkgbuild", "bash", "Shell Script"),
    ("uv.lock", "toml", "TOML"), ("cargo.lock", "toml", "TOML"),
];

pub fn language_for(path: Option<&Path>) -> Language {
    let Some(path) = path else { return (None, "Plain Text") };
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if let Some((_, id, label)) = FILENAMES.iter().find(|(key, _, _)| *key == name) {
        return (Some(id), label);
    }
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match EXTENSIONS.iter().find(|(key, _, _)| *key == extension) {
        Some((_, id, label)) => (Some(id), label),
        None => (None, "Plain Text"),
    }
}
