//! Small helpers shared across the app.

use std::env;
use std::path::PathBuf;

/// Find a program on PATH.
pub fn which(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let path = PathBuf::from(program);
        return path.is_file().then_some(path);
    }
    env::split_paths(&env::var_os("PATH")?).map(|dir| dir.join(program)).find(|path| path.is_file())
}

/// Split a command line the way a shell would (quotes and backslashes).
pub fn shell_split(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') if c == '\'' => quote = None,
            Some('\'') => word.push(c),
            Some(_) if c == '"' => quote = None,
            Some(_) if c == '\\' => word.extend(chars.next()),
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            None if c == '\\' => {
                word.extend(chars.next());
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            None => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// Quote one word so a shell reads it back unchanged.
pub fn shell_quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\"'\"'"))
    }
}

/// Fuzzy match: every character of `query` must appear in `text`, in order.
/// Returns a score (higher is better) and the matched character positions.
pub fn fuzzy(query: &str, text: &str) -> Option<(i32, Vec<usize>)> {
    let query: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let mut positions = Vec::with_capacity(query.len());
    let mut score = 0;
    let mut from = 0;
    let mut previous: Option<usize> = None;
    for &wanted in &query {
        let found = (from..lower.len()).find(|&i| lower[i] == wanted)?;
        let boundary = found == 0 || !chars[found - 1].is_alphanumeric() || (chars[found].is_uppercase() && chars[found - 1].is_lowercase());
        score += 10;
        if boundary {
            score += 8;
        }
        if previous.map(|p| p + 1 == found).unwrap_or(false) {
            score += 12;
        }
        positions.push(found);
        previous = Some(found);
        from = found + 1;
    }
    score -= (chars.len() as i32) / 8;
    Some((score.max(1), positions))
}
