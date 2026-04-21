use crate::crypto::{encrypt, decrypt};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

// ─── Preferences (persisted to prefs.json) ────────────────────────────────────

fn default_true() -> bool { true }
fn default_auto_lock_delay() -> f64 { 5.0 }
fn default_clipboard_clear_delay() -> f64 { 30.0 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prefs {
    pub font_size:       f32,
    pub dark_mode:       bool,
    pub auto_save:       bool,
    pub auto_save_delay: f64,
    #[serde(default)]
    pub auto_lock:       bool,
    #[serde(default = "default_auto_lock_delay")]
    pub auto_lock_delay: f64,
    #[serde(default = "default_true")]
    pub clipboard_clear:       bool,
    #[serde(default = "default_clipboard_clear_delay")]
    pub clipboard_clear_delay: f64,
    pub win_x: Option<f32>,
    pub win_y: Option<f32>,
    pub win_w: Option<f32>,
    pub win_h: Option<f32>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            font_size:             14.0,
            dark_mode:             true,
            auto_save:             true,
            auto_save_delay:       3.0,
            auto_lock:             false,
            auto_lock_delay:       5.0,
            clipboard_clear:       true,
            clipboard_clear_delay: 30.0,
            win_x: None, win_y: None,
            win_w: None, win_h: None,
        }
    }
}

pub fn load_prefs(path: &PathBuf) -> Prefs {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_prefs(path: &PathBuf, prefs: &Prefs) {
    if let Ok(s) = serde_json::to_string_pretty(prefs) {
        let _ = fs::write(path, s);
    }
}

// ─── Config (persisted to config.json) ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(rename = "passwordHash")]
    pub password_hash: Option<String>,
}

pub fn load_config(path: &PathBuf) -> Config {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_config(path: &PathBuf, cfg: &Config) {
    if let Ok(s) = serde_json::to_string_pretty(cfg) {
        if fs::write(path, s).is_ok() {
            set_private(path);
        }
    }
}

// ─── Tab data ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub id:      u32,
    pub name:    String,
    pub content: String,
    /// Screen-privacy lock. Does not add a separate encryption layer — all
    /// tabs share the same master key.
    #[serde(default)]
    pub locked:  bool,
}

pub fn default_tabs() -> Vec<Tab> {
    vec![Tab { id: 1, name: "Note 1".into(), content: String::new(), locked: false }]
}

pub fn load_tabs(notes_file: &PathBuf, password: &str) -> Result<Vec<Tab>, &'static str> {
    if !notes_file.exists() { return Ok(default_tabs()); }
    let enc = fs::read_to_string(notes_file).map_err(|_| "read error")?;
    if enc.trim().is_empty() { return Ok(default_tabs()); }
    let raw = decrypt(enc.trim(), password)?;
    if raw.trim_start().starts_with('[') {
        serde_json::from_str(&raw).map_err(|_| "json parse error")
    } else {
        Ok(vec![Tab { id: 1, name: "Note 1".into(), content: raw, locked: false }])
    }
}

/// Write tabs atomically: encrypt to a temp file then rename into place.
/// A crash mid-write leaves the old file intact.
pub fn save_tabs(notes_file: &PathBuf, password: &str, tabs: &[Tab]) -> Result<(), String> {
    let safe: Vec<Tab> = tabs.iter().take(5).map(|t| Tab {
        id:      t.id,
        name:    t.name.chars().take(64).collect(),
        content: t.content.clone(),
        locked:  t.locked,
    }).collect();
    let json = serde_json::to_string(&safe).map_err(|e| e.to_string())?;
    let enc  = encrypt(&json, password).map_err(|e| e.to_string())?;

    let tmp = notes_file.with_extension("tmp");
    fs::write(&tmp, &enc).map_err(|e| e.to_string())?;
    fs::rename(&tmp, notes_file).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })?;
    set_private(notes_file);
    Ok(())
}

// ─── File permissions ─────────────────────────────────────────────────────────

/// Set file permissions to owner-read/write only (0600) on Unix. No-op elsewhere.
#[cfg(unix)]
fn set_private(path: &PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_private(_path: &PathBuf) {}
