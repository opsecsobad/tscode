//! Tiny persisted settings (~/.config/tscode/settings.json). Override the path with TSCODE_SETTINGS.

use serde_json::{Map, Value};
use std::env;
use std::fs;
use std::path::PathBuf;

pub fn home() -> PathBuf {
    PathBuf::from(env::var_os("HOME").unwrap_or_else(|| "/".into()))
}

fn dir_from_env(var: &str, default: &[&str]) -> PathBuf {
    match env::var_os(var) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => default.iter().fold(home(), |path, part| path.join(part)),
    }
}

pub fn config_dir() -> PathBuf {
    dir_from_env("XDG_CONFIG_HOME", &[".config"])
}

pub fn state_dir() -> PathBuf {
    dir_from_env("XDG_STATE_HOME", &[".local", "state"])
}

pub fn settings_path() -> PathBuf {
    match env::var_os("TSCODE_SETTINGS") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => config_dir().join("tscode").join("settings.json"),
    }
}

pub fn load_settings() -> Map<String, Value> {
    fs::read_to_string(settings_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

pub fn save_setting(key: &str, value: Value) {
    let path = settings_path();
    let Some(parent) = path.parent() else { return };
    if !parent.is_dir() && parent.file_name().map(|n| n != "tscode").unwrap_or(true) {
        return;
    }
    let mut data = load_settings();
    data.insert(key.to_string(), value);
    if fs::create_dir_all(parent).is_ok() {
        if let Ok(text) = serde_json::to_string_pretty(&Value::Object(data)) {
            let _ = fs::write(&path, text + "\n");
        }
    }
}

/// True for a thin line cursor (the default, like VS Code), false for a block.
/// The state file is written by the system-wide `cursor-style-toggle` command.
pub fn cursor_is_line() -> bool {
    match fs::read_to_string(state_dir().join("cursor-style")) {
        Ok(text) => text.trim() != "block",
        Err(_) => true,
    }
}
