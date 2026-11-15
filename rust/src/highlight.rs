//! Syntax highlighting and problems (error squiggles), both done off the UI thread.
//!
//! Highlighting: tree-sitter grammars + their highlight queries. The result is a list
//! of coloured spans per line.
//!
//! Problems, best source available first:
//!
//! * Python      the interpreter's own parser (`compile`) - exact syntax errors
//! * C / C++     `gcc -fsyntax-only` - real compiler errors and warnings
//! * everything  tree-sitter: the places the grammar could not parse (ERROR / MISSING)

use crate::app::Ev;
use crate::theme::capture_names;
use crate::util::which;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use tree_sitter::Language;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

const MAX_DIAGNOSTICS: usize = 200;

/// A coloured stretch of one line: byte offsets within the line + index into `capture_names()`.
#[derive(Clone, Copy, Debug)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub id: u16,
}

pub struct HlRequest {
    pub doc: u64,
    pub version: u64,
    pub language: &'static str,
    pub text: String,
}

pub struct HlResult {
    pub doc: u64,
    pub version: u64,
    pub lines: Vec<Vec<Span>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub line: usize,
    /// 0-based, in characters.
    pub column: usize,
    /// Exclusive; None = to the end of the line.
    pub end_column: Option<usize>,
    pub message: String,
    pub error: bool,
}

impl Diagnostic {
    pub fn covers(&self, line: usize, column: usize) -> bool {
        line == self.line && self.column <= column && self.end_column.map(|end| column < end).unwrap_or(true)
    }
}

pub struct DiagRequest {
    pub doc: u64,
    pub version: u64,
    pub language: Option<&'static str>,
    pub text: String,
    pub path: Option<PathBuf>,
}

pub struct DiagResult {
    pub doc: u64,
    pub version: u64,
    pub diagnostics: Vec<Diagnostic>,
}

// -- grammars -------------------------------------------------------------------------

/// (grammar, highlight query, injection query, locals query) for a language id.
fn grammar(name: &str) -> Option<(Language, String, &'static str, &'static str)> {
    let js = tree_sitter_javascript::HIGHLIGHT_QUERY;
    let jsx = tree_sitter_javascript::JSX_HIGHLIGHT_QUERY;
    let ts = tree_sitter_typescript::HIGHLIGHTS_QUERY;
    Some(match name {
        "python" => (tree_sitter_python::LANGUAGE.into(), tree_sitter_python::HIGHLIGHTS_QUERY.into(), "", ""),
        "rust" => (
            tree_sitter_rust::LANGUAGE.into(),
            format!("(identifier) @variable\n{}", tree_sitter_rust::HIGHLIGHTS_QUERY),
            tree_sitter_rust::INJECTIONS_QUERY,
            "",
        ),
        "javascript" => (
            tree_sitter_javascript::LANGUAGE.into(),
            format!("{js}\n{jsx}"),
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        ),
        // The TypeScript query only holds the TypeScript additions; it sits on top of JavaScript's.
        "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            format!("{js}\n{ts}"),
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        "tsx" => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            format!("{js}\n{jsx}\n{ts}"),
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        "json" => (tree_sitter_json::LANGUAGE.into(), tree_sitter_json::HIGHLIGHTS_QUERY.into(), "", ""),
        "html" => (tree_sitter_html::LANGUAGE.into(), tree_sitter_html::HIGHLIGHTS_QUERY.into(), tree_sitter_html::INJECTIONS_QUERY, ""),
        "css" => (tree_sitter_css::LANGUAGE.into(), tree_sitter_css::HIGHLIGHTS_QUERY.into(), "", ""),
        "markdown" => (tree_sitter_md::LANGUAGE.into(), markdown_query(tree_sitter_md::HIGHLIGHT_QUERY_BLOCK), tree_sitter_md::INJECTION_QUERY_BLOCK, ""),
        "markdown_inline" => (
            tree_sitter_md::INLINE_LANGUAGE.into(),
            markdown_query(tree_sitter_md::HIGHLIGHT_QUERY_INLINE),
            tree_sitter_md::INJECTION_QUERY_INLINE,
            "",
        ),
        "toml" => (tree_sitter_toml_ng::LANGUAGE.into(), tree_sitter_toml_ng::HIGHLIGHTS_QUERY.into(), "", ""),
        "yaml" => (tree_sitter_yaml::LANGUAGE.into(), tree_sitter_yaml::HIGHLIGHTS_QUERY.into(), "", ""),
        "go" => (tree_sitter_go::LANGUAGE.into(), tree_sitter_go::HIGHLIGHTS_QUERY.into(), "", ""),
        "bash" => (tree_sitter_bash::LANGUAGE.into(), tree_sitter_bash::HIGHLIGHT_QUERY.into(), "", ""),
        "c" => (tree_sitter_c::LANGUAGE.into(), tree_sitter_c::HIGHLIGHT_QUERY.into(), "", ""),
        // The C++ query only holds the C++ additions; it is meant to sit on top of C's.
        "cpp" => (
            tree_sitter_cpp::LANGUAGE.into(),
            format!("{}\n{}", tree_sitter_c::HIGHLIGHT_QUERY, tree_sitter_cpp::HIGHLIGHT_QUERY),
            "",
            "",
        ),
        "java" => (tree_sitter_java::LANGUAGE.into(), tree_sitter_java::HIGHLIGHTS_QUERY.into(), "", ""),
        "ruby" => (tree_sitter_ruby::LANGUAGE.into(), tree_sitter_ruby::HIGHLIGHTS_QUERY.into(), "", tree_sitter_ruby::LOCALS_QUERY),
        "lua" => (tree_sitter_lua::LANGUAGE.into(), tree_sitter_lua::HIGHLIGHTS_QUERY.into(), "", tree_sitter_lua::LOCALS_QUERY),
        "php" => (tree_sitter_php::LANGUAGE_PHP.into(), tree_sitter_php::HIGHLIGHTS_QUERY.into(), tree_sitter_php::INJECTIONS_QUERY, ""),
        "sql" => (tree_sitter_sequel::LANGUAGE.into(), tree_sitter_sequel::HIGHLIGHTS_QUERY.into(), "", ""),
        "xml" => (tree_sitter_xml::LANGUAGE_XML.into(), tree_sitter_xml::XML_HIGHLIGHT_QUERY.into(), "", ""),
        _ => return None,
    })
}

/// The Markdown grammar names its captures differently from the theme's keys.
fn markdown_query(query: &str) -> String {
    query
        .replace("@text.title", "@markup.heading")
        .replace("@text.emphasis", "@italic")
        .replace("@text.strong", "@bold")
        .replace("@text.reference", "@link.label")
}

/// Names used in fenced code blocks and `<script>` tags -> our language ids.
fn language_alias(name: &str) -> &str {
    match name {
        "py" | "python3" => "python",
        "js" | "jsx" | "node" | "mjs" => "javascript",
        "ts" => "typescript",
        "sh" | "shell" | "zsh" | "console" => "bash",
        "rs" => "rust",
        "yml" => "yaml",
        "c++" | "cxx" | "cc" => "cpp",
        "golang" => "go",
        "rb" => "ruby",
        "md" => "markdown",
        "svg" => "xml",
        "jsonc" => "json",
        other => other,
    }
}

fn make_config(name: &str) -> Option<&'static HighlightConfiguration> {
    let (language, query, injections, locals) = grammar(name)?;
    let mut config = HighlightConfiguration::new(language.clone(), name, &query, injections, locals)
        // Extra patterns that don't fit this grammar: fall back to its own query alone.
        .or_else(|_| match name {
            "typescript" | "tsx" => HighlightConfiguration::new(language.clone(), name, tree_sitter_typescript::HIGHLIGHTS_QUERY, "", ""),
            "rust" => HighlightConfiguration::new(language.clone(), name, tree_sitter_rust::HIGHLIGHTS_QUERY, "", ""),
            "cpp" => HighlightConfiguration::new(language.clone(), name, tree_sitter_cpp::HIGHLIGHT_QUERY, "", ""),
            _ => HighlightConfiguration::new(language.clone(), name, &query, "", ""),
        })
        .ok()?;
    config.configure(&capture_names());
    Some(Box::leak(Box::new(config)))
}

thread_local! {
    /// Grammars are built the first time a file needs them, then kept for good.
    static CONFIGS: RefCell<HashMap<String, Option<&'static HighlightConfiguration>>> = RefCell::new(HashMap::new());
}

fn config_for(name: &str) -> Option<&'static HighlightConfiguration> {
    let name = language_alias(name).to_lowercase();
    if let Some(known) = CONFIGS.with(|configs| configs.borrow().get(&name).copied()) {
        return known;
    }
    let config = make_config(&name);
    CONFIGS.with(|configs| configs.borrow_mut().insert(name, config));
    config
}

// -- highlighting -------------------------------------------------------------------

/// Control-flow keywords are purple in VS Code's themes, other keywords blue. Most
/// grammars only say "keyword", so these are told apart by their text.
const CONTROL_WORDS: &[&str] = &[
    "if", "else", "elif", "for", "while", "return", "try", "except", "finally", "raise", "import", "from", "with",
    "yield", "break", "continue", "match", "case", "await", "switch", "default", "do", "throw", "catch", "loop",
    "goto", "pass", "as", "export", "then", "fi", "done", "esac", "elsif", "unless", "until", "rescue", "ensure",
    "end", "defer", "go", "select", "use", "foreach", "endif", "endfor", "endwhile", "elseif",
];

fn highlight(highlighter: &mut Highlighter, language: &str, text: &str) -> Vec<Vec<Span>> {
    let mut line_starts = vec![0usize];
    line_starts.extend(text.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i + 1));
    let mut lines: Vec<Vec<Span>> = vec![Vec::new(); line_starts.len()];
    let Some(config) = config_for(language) else { return lines };
    let names = capture_names();
    let keyword = names.iter().position(|n| *n == "keyword");
    let control = names.iter().position(|n| *n == "keyword.return");
    let Ok(events) = highlighter.highlight(config, text.as_bytes(), None, None, |name| config_for(name)) else {
        return lines;
    };
    let mut stack: Vec<usize> = Vec::new();
    let mut line = 0usize;
    for event in events {
        match event {
            Ok(HighlightEvent::HighlightStart(h)) => stack.push(h.0),
            Ok(HighlightEvent::HighlightEnd) => {
                stack.pop();
            }
            Ok(HighlightEvent::Source { mut start, end }) => {
                let Some(&top) = stack.last() else { continue };
                let mut id = top;
                if Some(id) == keyword && end - start <= 8 {
                    if let Some(word) = text.get(start..end) {
                        if CONTROL_WORDS.contains(&word) {
                            id = control.unwrap_or(id);
                        }
                    }
                }
                while start < end {
                    while line + 1 < line_starts.len() && line_starts[line + 1] <= start {
                        line += 1;
                    }
                    let line_start = line_starts[line];
                    let line_end = line_starts.get(line + 1).map(|next| next - 1).unwrap_or(text.len());
                    let stop = end.min(line_end);
                    if stop > start {
                        lines[line].push(Span { start: (start - line_start) as u32, end: (stop - line_start) as u32, id: id as u16 });
                    }
                    start = stop.max(line_end + 1).max(start + 1);
                }
            }
            Err(_) => break,
        }
    }
    lines
}

/// Start the highlighting thread. Requests that pile up for one document collapse to the newest.
pub fn spawn_highlighter(tx: Sender<Ev>) -> Sender<HlRequest> {
    let (requests, incoming): (Sender<HlRequest>, Receiver<HlRequest>) = channel();
    thread::spawn(move || {
        let mut highlighter = Highlighter::new();
        while let Ok(first) = incoming.recv() {
            let mut batch = vec![first];
            while let Ok(more) = incoming.try_recv() {
                batch.retain(|request| request.doc != more.doc);
                batch.push(more);
            }
            for request in batch {
                let lines = highlight(&mut highlighter, request.language, &request.text);
                if tx.send(Ev::Highlight(HlResult { doc: request.doc, version: request.version, lines })).is_err() {
                    return;
                }
            }
        }
    });
    requests
}

// -- diagnostics ----------------------------------------------------------------------

fn char_column(line: &str, byte_column: usize) -> usize {
    let mut end = byte_column.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].chars().count()
}

/// Clamp a range to the text and make sure it is at least one character wide.
fn fit(lines: &[&str], line: usize, column: usize, end_column: Option<usize>) -> (usize, usize, Option<usize>) {
    let line = line.min(lines.len().saturating_sub(1));
    let length = lines.get(line).map(|l| l.chars().count()).unwrap_or(0);
    let column = column.min(length);
    let end_column = end_column.map(|end| end.min(length).max(column));
    let past_end = column >= length && length > 0; // starts past the last character: mark that character
    match end_column {
        Some(end) if end <= column => {
            if past_end {
                (line, length - 1, Some(length))
            } else {
                (line, column, Some(column + 1))
            }
        }
        _ if past_end => (line, length - 1, Some(length)),
        _ => (line, column, end_column),
    }
}

fn capitalize(message: &str) -> String {
    let mut chars = message.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Parse errors from a tree-sitter tree.
fn syntax_tree_diagnostics(language: &str, text: &str) -> Vec<Diagnostic> {
    if language == "markdown" {
        return Vec::new();
    }
    let Some((grammar, ..)) = grammar(language) else { return Vec::new() };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&grammar).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(text, None) else { return Vec::new() };
    if !tree.root_node().has_error() {
        return Vec::new();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut found = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if found.len() >= MAX_DIAGNOSTICS {
            break;
        }
        let start = node.start_position();
        let row = start.row.min(lines.len() - 1);
        let column = char_column(lines[row], start.column);
        if node.is_missing() {
            let (line, column, end_column) = fit(&lines, row, column, Some(column));
            found.push(Diagnostic { line, column, end_column, message: format!("'{}' expected.", node.kind()), error: true });
            continue;
        }
        if node.is_error() {
            let end = node.end_position();
            // Underline only the first line of a big broken region.
            let end_column = (end.row == row).then(|| char_column(lines[row], end.column));
            let snippet: String = lines[row].chars().skip(column).take(end_column.map(|e| e.saturating_sub(column)).unwrap_or(20)).collect();
            let snippet: String = snippet.trim().chars().take(20).collect();
            let message = if snippet.is_empty() { "Syntax error.".to_string() } else { format!("Syntax error near '{snippet}'.") };
            let (line, column, end_column) = fit(&lines, row, column, end_column);
            found.push(Diagnostic { line, column, end_column, message, error: true });
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).filter(|child| child.has_error()).collect();
        stack.extend(children.into_iter().rev());
    }
    found.sort_by_key(|d| (d.line, d.column));
    found
}

/// Run a program, feed it `input`, and return what it wrote to the chosen stream.
fn run_with_input(mut command: Command, input: &str, want_stderr: bool) -> Option<String> {
    command.stdin(Stdio::piped());
    if want_stderr {
        command.stdout(Stdio::null()).stderr(Stdio::piped());
    } else {
        command.stdout(Stdio::piped()).stderr(Stdio::null());
    }
    let mut child = command.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let input = input.to_string();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut output = Vec::new();
    if want_stderr {
        child.stderr.take()?.read_to_end(&mut output).ok()?;
    } else {
        child.stdout.take()?.read_to_end(&mut output).ok()?;
    }
    let _ = writer.join();
    let _ = child.wait();
    Some(String::from_utf8_lossy(&output).to_string())
}

const PYTHON_CHECK: &str = r#"
import sys, json, warnings
warnings.simplefilter("ignore")
source = sys.stdin.buffer.read().decode("utf-8", "replace")
try:
    compile(source, sys.argv[1], "exec", dont_inherit=True)
    print("null")
except SyntaxError as e:
    print(json.dumps([e.lineno, e.offset, e.end_lineno, e.end_offset, e.msg]))
except Exception as e:
    print(json.dumps([1, 1, None, None, str(e)]))
"#;

/// Ask the Python interpreter itself. None if python3 can't be used.
fn python_diagnostics(text: &str, filename: &str) -> Option<Vec<Diagnostic>> {
    let python = which("python3")?;
    let mut command = Command::new(python);
    command.args(["-I", "-c", PYTHON_CHECK, filename]);
    let output = run_with_input(command, text, false)?;
    let report: Value = serde_json::from_str(output.trim()).ok()?;
    let Some(report) = report.as_array() else { return Some(Vec::new()) };
    let number = |i: usize| report.get(i).and_then(Value::as_u64).map(|n| n as usize);
    let lines: Vec<&str> = text.split('\n').collect();
    let line = number(0).unwrap_or(1).saturating_sub(1);
    let column = number(1).unwrap_or(1).saturating_sub(1);
    let end = match (number(2), number(3)) {
        (Some(end_line), Some(end_offset)) if Some(end_line) == number(0) && end_offset.saturating_sub(1) > column => Some(end_offset - 1),
        _ => None,
    };
    let message = report.get(4).and_then(Value::as_str).unwrap_or("invalid syntax").trim_end_matches('.');
    let (line, column, end_column) = fit(&lines, line, column, Some(end.unwrap_or(column)));
    Some(vec![Diagnostic { line, column, end_column, message: capitalize(message) + ".", error: true }])
}

/// Compile-check the buffer (unsaved text, via stdin). None if gcc can't be used.
fn gcc_diagnostics(text: &str, path: Option<&PathBuf>, cplusplus: bool) -> Option<Vec<Diagnostic>> {
    let gcc = which("gcc")?;
    let folder = path.and_then(|p| p.parent()).map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let mut command = Command::new(gcc);
    command
        .args(["-fsyntax-only", "-fdiagnostics-format=json", "-Wall", "-x", if cplusplus { "c++" } else { "c" }, "-I"])
        .arg(&folder)
        .arg("-")
        .current_dir(&folder);
    let stderr = run_with_input(command, text, true)?;
    let reports: Value = if stderr.trim().is_empty() { Value::Array(vec![]) } else { serde_json::from_str(stderr.trim()).ok()? };
    let lines: Vec<&str> = text.split('\n').collect();
    let mut found = Vec::new();
    for report in reports.as_array()? {
        let kind = report.get("kind").and_then(Value::as_str).unwrap_or("");
        if !matches!(kind, "error" | "fatal error" | "warning") {
            continue;
        }
        let location = report.get("locations").and_then(|l| l.get(0));
        let Some(caret) = location.and_then(|l| l.get("caret")) else { continue };
        let in_buffer = caret.get("file").and_then(Value::as_str).map(|f| f == "<stdin>").unwrap_or(true);
        let Some(caret_line) = caret.get("line").and_then(Value::as_u64) else { continue };
        if !in_buffer {
            continue;
        }
        let finish = location.and_then(|l| l.get("finish")).unwrap_or(caret);
        let row = (caret_line as usize).saturating_sub(1).min(lines.len() - 1);
        let column_of = |point: &Value| -> usize {
            match point.get("byte-column").and_then(Value::as_u64) {
                Some(byte) => char_column(lines[row], (byte as usize).saturating_sub(1)),
                None => (point.get("column").and_then(Value::as_u64).unwrap_or(1) as usize).saturating_sub(1),
            }
        };
        let column = column_of(caret);
        let same_line = finish.get("line").and_then(Value::as_u64) == Some(caret_line);
        let end = same_line.then(|| column_of(finish) + 1);
        let mut message = capitalize(report.get("message").and_then(Value::as_str).unwrap_or("error"));
        if let Some(option) = report.get("option").and_then(Value::as_str) {
            message += &format!(" ({option})");
        }
        let (line, column, end_column) = fit(&lines, row, column, end);
        found.push(Diagnostic { line, column, end_column, message, error: kind != "warning" });
        if found.len() >= MAX_DIAGNOSTICS {
            break;
        }
    }
    found.sort_by_key(|d| (d.line, d.column));
    Some(found)
}

/// All problems in `text`.
fn check(request: &DiagRequest) -> Vec<Diagnostic> {
    let Some(language) = request.language else { return Vec::new() };
    let found = match language {
        "python" => {
            let name = request.path.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or("<untitled>".into());
            python_diagnostics(&request.text, &name)
        }
        "c" | "cpp" => gcc_diagnostics(&request.text, request.path.as_ref(), language == "cpp"),
        _ => None,
    };
    found.unwrap_or_else(|| syntax_tree_diagnostics(language, &request.text))
}

/// Start the diagnostics thread.
pub fn spawn_checker(tx: Sender<Ev>) -> Sender<DiagRequest> {
    let (requests, incoming): (Sender<DiagRequest>, Receiver<DiagRequest>) = channel();
    thread::spawn(move || {
        while let Ok(first) = incoming.recv() {
            let mut batch = vec![first];
            while let Ok(more) = incoming.try_recv() {
                batch.retain(|request| request.doc != more.doc);
                batch.push(more);
            }
            for request in batch {
                let diagnostics = check(&request);
                if tx.send(Ev::Diagnostics(DiagResult { doc: request.doc, version: request.version, diagnostics })).is_err() {
                    return;
                }
            }
        }
    });
    requests
}
