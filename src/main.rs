/*!
 * SecureNote — Native Desktop Edition (egui / eframe)
 *
 * Cross-platform: Windows · macOS · Linux
 * Encrypted note storage is wire-format compatible with the web (Rust/Node) version.
 *
 * Crypto wire format: base64( salt[32] || iv[12] || tag[16] || ciphertext )
 */

// NOTE: We do NOT set #![windows_subsystem = "windows"] here.
// eframe sets it internally for release builds. Setting it ourselves
// causes silent startup failures on Windows because panics and early
// process::exit() calls produce no visible output whatsoever.

use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use clap::Parser;
use eframe::egui::{self, Color32, FontId, Key as EKey, RichText, Stroke, Vec2};
use hmac::Hmac;
use pbkdf2::pbkdf2;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{fs, path::PathBuf, process};
use subtle::ConstantTimeEq;
use eframe::wgpu;

// ─── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "secure-note", about = "SecureNote — encrypted notepad (native GUI)")]
struct Cli {
    #[arg(long, default_value = "./secure-notes")]
    data: PathBuf,
}

// ─── Preferences (persisted to prefs.json) ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Prefs {
    font_size:       f32,
    dark_mode:       bool,
    auto_save:       bool,
    auto_save_delay: f64,
    // Window geometry — restored on next launch
    win_x:      Option<f32>,
    win_y:      Option<f32>,
    win_w:      Option<f32>,
    win_h:      Option<f32>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            font_size:       14.0,
            dark_mode:       true,
            auto_save:       true,
            auto_save_delay: 3.0,
            win_x: None, win_y: None,
            win_w: None, win_h: None,
        }
    }
}

fn load_prefs(path: &PathBuf) -> Prefs {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_prefs(path: &PathBuf, prefs: &Prefs) {
    if let Ok(s) = serde_json::to_string_pretty(prefs) {
        let _ = fs::write(path, s);
    }
}

// ─── Crypto ───────────────────────────────────────────────────────────────────

const SALT_LEN: usize = 32;
const IV_LEN:   usize = 12;
const TAG_LEN:  usize = 16;
const PBKDF2_ITER: u32 = 310_000;

fn derive_key(password: &str, salt: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::<Hmac<Sha256>>(password.as_bytes(), salt, PBKDF2_ITER, &mut key)
        .expect("pbkdf2 failed");
    key
}

fn encrypt(plaintext: &str, password: &str) -> String {
    let mut salt = [0u8; SALT_LEN];
    let mut iv   = [0u8; IV_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    rand::thread_rng().fill_bytes(&mut iv);

    let key_bytes = derive_key(password, &salt);
    let cipher    = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));
    let ct_tag    = cipher
        .encrypt(Nonce::from_slice(&iv), plaintext.as_bytes())
        .expect("encryption failed");

    let ct_len    = ct_tag.len() - TAG_LEN;
    let (ct, tag) = ct_tag.split_at(ct_len);

    let mut out = Vec::with_capacity(SALT_LEN + IV_LEN + TAG_LEN + ct.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&iv);
    out.extend_from_slice(tag);
    out.extend_from_slice(ct);
    B64.encode(&out)
}

fn decrypt(b64: &str, password: &str) -> Result<String, &'static str> {
    let buf = B64.decode(b64.trim()).map_err(|_| "base64 error")?;
    if buf.len() < SALT_LEN + IV_LEN + TAG_LEN {
        return Err("ciphertext too short");
    }
    let salt       = &buf[..SALT_LEN];
    let iv         = &buf[SALT_LEN..SALT_LEN + IV_LEN];
    let tag        = &buf[SALT_LEN + IV_LEN..SALT_LEN + IV_LEN + TAG_LEN];
    let ciphertext = &buf[SALT_LEN + IV_LEN + TAG_LEN..];

    let key_bytes = derive_key(password, salt);
    let cipher    = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));

    let mut ct_tag = Vec::with_capacity(ciphertext.len() + TAG_LEN);
    ct_tag.extend_from_slice(ciphertext);
    ct_tag.extend_from_slice(tag);

    let plain = cipher
        .decrypt(Nonce::from_slice(iv), ct_tag.as_slice())
        .map_err(|_| "decryption failed — wrong password?")?;

    String::from_utf8(plain).map_err(|_| "utf-8 decode failed")
}

fn hash_password(pw: &str) -> String {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let salt_hex = hex::encode(salt);
    let mut hash = [0u8; 64];
    pbkdf2::<Hmac<Sha256>>(pw.as_bytes(), salt_hex.as_bytes(), PBKDF2_ITER, &mut hash)
        .expect("pbkdf2 failed");
    format!("{}:{}", salt_hex, hex::encode(hash))
}

fn verify_password(pw: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.splitn(2, ':').collect();
    if parts.len() != 2 { return false; }
    let expected = match hex::decode(parts[1]) { Ok(v) => v, Err(_) => return false };
    let mut actual = vec![0u8; expected.len()];
    if pbkdf2::<Hmac<Sha256>>(pw.as_bytes(), parts[0].as_bytes(), PBKDF2_ITER, &mut actual).is_err() {
        return false;
    }
    actual.ct_eq(&expected).into()
}

// ─── Data types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Tab {
    id:      u32,
    name:    String,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Config {
    #[serde(rename = "passwordHash")]
    password_hash: Option<String>,
}

fn default_tabs() -> Vec<Tab> {
    vec![Tab { id: 1, name: "Note 1".into(), content: String::new() }]
}

fn load_config(path: &PathBuf) -> Config {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(path: &PathBuf, cfg: &Config) {
    if let Ok(s) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(path, s);
    }
}

fn load_tabs(notes_file: &PathBuf, password: &str) -> Result<Vec<Tab>, &'static str> {
    if !notes_file.exists() { return Ok(default_tabs()); }
    let enc = fs::read_to_string(notes_file).map_err(|_| "read error")?;
    if enc.trim().is_empty() { return Ok(default_tabs()); }
    let raw = decrypt(enc.trim(), password)?;
    if raw.trim_start().starts_with('[') {
        serde_json::from_str(&raw).map_err(|_| "json parse error")
    } else {
        Ok(vec![Tab { id: 1, name: "Note 1".into(), content: raw }])
    }
}

fn save_tabs(notes_file: &PathBuf, password: &str, tabs: &[Tab]) -> Result<(), String> {
    let safe: Vec<Tab> = tabs.iter().take(5).map(|t| Tab {
        id:      t.id,
        name:    t.name.chars().take(64).collect(),
        content: t.content.clone(),
    }).collect();
    let json = serde_json::to_string(&safe).map_err(|e| e.to_string())?;
    let enc  = encrypt(&json, password);
    fs::write(notes_file, enc).map_err(|e| e.to_string())
}

// ─── App state ────────────────────────────────────────────────────────────────

const MAX_TABS: usize = 5;

#[derive(Debug, Clone, PartialEq)]
enum Screen { Lock, Editor }

#[derive(Debug, Clone, PartialEq)]
enum Modal {
    None,
    Erase,
    ChangePassword,
    /// Confirm closing tab at this index
    CloseTab(usize),
}

struct SecureNote {
    notes_file:  PathBuf,
    config_file: PathBuf,
    prefs_file:  PathBuf,
    lock_file:   PathBuf,
    data_dir:    String,    // resolved absolute path shown in status bar

    // Lock screen
    screen:         Screen,
    password_input: String,
    confirm_input:  String,
    lock_error:     String,
    is_setup:       bool,

    // Session
    session_password: String,

    // Tabs
    tabs:       Vec<Tab>,
    active_tab: usize,
    dirty:      bool,

    // Auto-save timing
    last_edit_time: f64,

    // Find / Replace
    search_open:    bool,
    search_query:   String,
    replace_query:  String,
    replace_mode:   bool,
    search_results: Vec<(usize, usize)>,
    search_idx:     usize,

    // Cursor position (updated from TextEditOutput each frame)
    cursor_line: usize,
    cursor_col:  usize,

    // Preferences (persisted)
    prefs:            Prefs,
    prefs_open:       bool,
    prefs_just_opened: bool,  // true for one frame after opening, suppresses click-outside

    // Modals
    modal:      Modal,
    cp_current: String,
    cp_new:     String,
    cp_confirm: String,
    cp_error:   String,

    // Inline tab rename
    renaming_tab: Option<usize>,
    rename_buf:   String,

    // Toast
    toast_msg:    String,
    toast_expire: f64,
}

impl SecureNote {
    fn new(notes_file: PathBuf, config_file: PathBuf, prefs_file: PathBuf,
           lock_file: PathBuf, data_dir: String) -> Self {
        let cfg      = load_config(&config_file);
        let is_setup = cfg.password_hash.is_some();
        let prefs    = load_prefs(&prefs_file);
        Self {
            notes_file,
            config_file,
            prefs_file,
            lock_file,
            data_dir,
            screen:           Screen::Lock,
            password_input:   String::new(),
            confirm_input:    String::new(),
            lock_error:       String::new(),
            is_setup,
            session_password: String::new(),
            tabs:             vec![],
            active_tab:       0,
            dirty:            false,
            last_edit_time:   0.0,
            search_open:      false,
            search_query:     String::new(),
            replace_query:    String::new(),
            replace_mode:     false,
            search_results:   vec![],
            search_idx:       0,
            cursor_line:      0,
            cursor_col:       0,
            prefs,
            prefs_open:        false,
            prefs_just_opened: false,
            modal:            Modal::None,
            cp_current:       String::new(),
            cp_new:           String::new(),
            cp_confirm:       String::new(),
            cp_error:         String::new(),
            renaming_tab:     None,
            rename_buf:       String::new(),
            toast_msg:        String::new(),
            toast_expire:     0.0,
        }
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        if self.prefs.dark_mode { ctx.set_visuals(egui::Visuals::dark()); }
        else                    { ctx.set_visuals(egui::Visuals::light()); }
    }

    fn persist_prefs(&self) {
        save_prefs(&self.prefs_file, &self.prefs);
    }

    fn toast(&mut self, msg: impl Into<String>, ctx: &egui::Context) {
        self.toast_msg    = msg.into();
        self.toast_expire = ctx.input(|i| i.time) + 2.5;
    }

    fn save_now(&mut self, ctx: &egui::Context) {
        match save_tabs(&self.notes_file, &self.session_password, &self.tabs) {
            Ok(_)  => { self.dirty = false; self.toast("Saved", ctx); }
            Err(e) => { self.toast(format!("Save failed: {e}"), ctx); }
        }
    }

    fn lock(&mut self) {
        self.screen           = Screen::Lock;
        self.session_password = String::new();
        self.tabs             = vec![];
        self.active_tab       = 0;
        self.dirty            = false;
        self.search_open      = false;
        self.prefs_open       = false;
        self.modal            = Modal::None;
    }

    fn try_unlock(&mut self, ctx: &egui::Context) {
        let pw = self.password_input.clone();
        if pw.is_empty() { self.lock_error = "Password is required.".into(); return; }

        if !self.is_setup {
            let c = self.confirm_input.clone();
            if c.is_empty() { self.lock_error = "Please confirm your password.".into(); return; }
            if pw != c      { self.lock_error = "Passwords do not match.".into(); return; }
        }

        let mut cfg = load_config(&self.config_file);

        if !self.is_setup {
            cfg.password_hash = Some(hash_password(&pw));
            save_config(&self.config_file, &cfg);
            let tabs = default_tabs();
            if save_tabs(&self.notes_file, &pw, &tabs).is_err() {
                self.lock_error = "Failed to create notes file.".into();
                return;
            }
            self.session_password = pw;
            self.tabs             = tabs;
            self.active_tab       = 0;
            self.enter_editor(ctx);
        } else {
            let stored = cfg.password_hash.as_deref().unwrap_or("");
            if !verify_password(&pw, stored) {
                self.lock_error = "Incorrect password.".into();
                return;
            }
            match load_tabs(&self.notes_file, &pw) {
                Ok(tabs) => {
                    self.session_password = pw;
                    self.tabs             = tabs;
                    self.active_tab       = 0;
                    self.enter_editor(ctx);
                }
                Err(e) => { self.lock_error = e.to_string(); }
            }
        }
    }

    fn enter_editor(&mut self, ctx: &egui::Context) {
        self.screen         = Screen::Editor;
        self.lock_error     = String::new();
        self.password_input = String::new();
        self.confirm_input  = String::new();
        self.dirty          = false;
        self.apply_theme(ctx);
    }

    fn add_tab(&mut self) {
        if self.tabs.len() >= MAX_TABS { return; }
        let id   = self.tabs.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        let name = format!("Note {}", self.tabs.len() + 1);
        self.tabs.push(Tab { id, name, content: String::new() });
        self.active_tab = self.tabs.len() - 1;
        self.dirty = true;
    }

    /// Actually remove the tab — called only after confirmation.
    fn close_tab_confirmed(&mut self, idx: usize) {
        if self.tabs.len() <= 1 { return; }
        self.tabs.remove(idx);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.dirty = true;
    }

    fn run_search(&mut self) {
        self.search_results.clear();
        let q = self.search_query.to_lowercase();
        if q.is_empty() { return; }
        let content = self.tabs.get(self.active_tab)
            .map(|t| t.content.to_lowercase())
            .unwrap_or_default();
        let qb = q.as_bytes();
        let cb = content.as_bytes();
        let mut i = 0;
        while i + qb.len() <= cb.len() {
            if cb[i..i + qb.len()] == *qb {
                self.search_results.push((i, i + qb.len()));
                i += qb.len();
            } else {
                i += 1;
            }
        }
        if self.search_idx >= self.search_results.len() {
            self.search_idx = 0;
        }
    }

    fn replace_all(&mut self) {
        let q = self.search_query.clone();
        let r = self.replace_query.clone();
        if q.is_empty() { return; }
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.content = tab.content.replace(&q, &r);
            self.dirty  = true;
        }
        self.run_search();
    }

    fn erase_all(&mut self) {
        let _ = fs::remove_file(&self.notes_file);
        let _ = fs::remove_file(&self.config_file);
        self.is_setup = false;
        self.lock();
    }

    fn change_password(&mut self, ctx: &egui::Context) {
        let current = self.cp_current.clone();
        let new_pw  = self.cp_new.clone();
        let confirm = self.cp_confirm.clone();

        if new_pw.is_empty() { self.cp_error = "New password is required.".into(); return; }
        if new_pw != confirm  { self.cp_error = "Passwords do not match.".into(); return; }

        let mut cfg = load_config(&self.config_file);
        if !verify_password(&current, cfg.password_hash.as_deref().unwrap_or("")) {
            self.cp_error = "Current password is incorrect.".into();
            return;
        }
        if let Err(e) = save_tabs(&self.notes_file, &new_pw, &self.tabs) {
            self.cp_error = format!("Re-encrypt failed: {e}");
            return;
        }
        cfg.password_hash = Some(hash_password(&new_pw));
        save_config(&self.config_file, &cfg);
        self.session_password = new_pw;
        self.modal = Modal::None;
        self.cp_current.clear(); self.cp_new.clear();
        self.cp_confirm.clear(); self.cp_error.clear();
        self.toast("Password updated", ctx);
    }
}

// ─── UI ───────────────────────────────────────────────────────────────────────

impl eframe::App for SecureNote {
    fn on_exit(&mut self) {
        // Save unsaved notes silently
        if self.dirty && !self.session_password.is_empty() {
            let _ = save_tabs(&self.notes_file, &self.session_password, &self.tabs);
        }
        // Remove the single-instance lock file
        let _ = fs::remove_file(&self.lock_file);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = ctx.input(|i| i.time);

        // Persist window geometry every frame so it survives crashes too
        let rect = ctx.input(|i| i.screen_rect());
        // screen_rect gives us the inner size; we also grab the outer pos via viewport
        let outer = ctx.input(|i| i.viewport().outer_rect);
        if let Some(r) = outer {
            let changed = self.prefs.win_x != Some(r.min.x)
                || self.prefs.win_y != Some(r.min.y)
                || self.prefs.win_w != Some(r.width())
                || self.prefs.win_h != Some(r.height());
            if changed {
                self.prefs.win_x = Some(r.min.x);
                self.prefs.win_y = Some(r.min.y);
                self.prefs.win_w = Some(r.width());
                self.prefs.win_h = Some(r.height());
                self.persist_prefs();
            }
        }
        let _ = rect;

        // Auto-save
        if self.dirty && self.prefs.auto_save && self.last_edit_time > 0.0
            && now - self.last_edit_time >= self.prefs.auto_save_delay
        {
            self.save_now(ctx);
        }

        match self.screen.clone() {
            Screen::Lock   => self.ui_lock(ctx),
            Screen::Editor => self.ui_editor(ctx, now),
        }

        // Toast overlay
        if now < self.toast_expire && !self.toast_msg.is_empty() {
            egui::Area::new("toast".into())
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -40.0])
                .order(egui::Order::Tooltip)
                .show(ctx, |ui| {
                    egui::Frame::none()
                        .fill(Color32::from_rgba_unmultiplied(40, 40, 50, 230))
                        .rounding(8.0)
                        .inner_margin(egui::Margin::symmetric(16.0, 8.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(&self.toast_msg).size(12.0).color(Color32::WHITE));
                        });
                });
            ctx.request_repaint();
        }
    }
}

impl SecureNote {
    // ── Lock screen ───────────────────────────────────────────────────────────

    fn ui_lock(&mut self, ctx: &egui::Context) {
        ctx.set_visuals(egui::Visuals::dark());

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::from_rgb(14, 14, 16)))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() / 4.0);

                    egui::Frame::none()
                        .fill(Color32::from_rgb(24, 24, 28))
                        .rounding(12.0)
                        .stroke(Stroke::new(1.0, Color32::from_rgb(46, 46, 56)))
                        .inner_margin(egui::Margin::same(32.0))
                        .show(ui, |ui| {
                            ui.set_max_width(360.0);

                            ui.vertical_centered(|ui| {
                                ui.label(RichText::new("🔐").size(32.0));
                                ui.add_space(4.0);
                                ui.label(RichText::new("SecureNote")
                                    .size(22.0).strong()
                                    .color(Color32::from_rgb(226, 226, 232)));
                                ui.label(RichText::new("AES-256-GCM · PBKDF2 · offline")
                                    .size(11.0).monospace()
                                    .color(Color32::from_rgb(136, 136, 160)));
                            });

                            ui.add_space(20.0);

                            if !self.is_setup {
                                egui::Frame::none()
                                    .fill(Color32::from_rgba_unmultiplied(224, 180, 92, 20))
                                    .rounding(6.0)
                                    .stroke(Stroke::new(1.0,
                                        Color32::from_rgba_unmultiplied(224, 180, 92, 50)))
                                    .inner_margin(egui::Margin::same(10.0))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(RichText::new(
                                            "No vault found. Set a master password to get started.\n\
                                             This password cannot be recovered.")
                                            .size(11.0)
                                            .color(Color32::from_rgb(224, 180, 92)));
                                    });
                                ui.add_space(12.0);
                            }

                            ui.label(RichText::new("MASTER PASSWORD").size(10.0).strong()
                                .color(Color32::from_rgb(136, 136, 160)));
                            ui.add_space(4.0);
                            let pw_resp = ui.add(
                                egui::TextEdit::singleline(&mut self.password_input)
                                    .password(true)
                                    .desired_width(f32::INFINITY)
                                    .font(FontId::monospace(15.0))
                                    .hint_text("Enter password…")
                            );
                            if pw_resp.lost_focus() && ctx.input(|i| i.key_pressed(EKey::Enter)) {
                                self.try_unlock(ctx);
                            }

                            if !self.is_setup {
                                ui.add_space(10.0);
                                ui.label(RichText::new("CONFIRM PASSWORD").size(10.0).strong()
                                    .color(Color32::from_rgb(136, 136, 160)));
                                ui.add_space(4.0);
                                let cf_resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.confirm_input)
                                        .password(true)
                                        .desired_width(f32::INFINITY)
                                        .font(FontId::monospace(15.0))
                                        .hint_text("Confirm password…")
                                );
                                if cf_resp.lost_focus() && ctx.input(|i| i.key_pressed(EKey::Enter)) {
                                    self.try_unlock(ctx);
                                }
                            }

                            if !self.lock_error.is_empty() {
                                ui.add_space(6.0);
                                ui.label(RichText::new(&self.lock_error.clone())
                                    .size(11.0).color(Color32::from_rgb(224, 92, 92)));
                            }

                            ui.add_space(16.0);

                            let label = if self.is_setup { "Unlock" } else { "Create Vault" };
                            if ui.add(
                                egui::Button::new(RichText::new(label).size(14.0).strong())
                                    .fill(Color32::from_rgb(124, 106, 247))
                                    .min_size(Vec2::new(ui.available_width(), 38.0))
                            ).clicked() {
                                self.try_unlock(ctx);
                            }

                            if self.password_input.is_empty() && self.lock_error.is_empty() {
                                pw_resp.request_focus();
                            }
                        });
                });
            });
    }

    // ── Editor ────────────────────────────────────────────────────────────────

    fn ui_editor(&mut self, ctx: &egui::Context, now: f64) {
        self.handle_shortcuts(ctx);
        self.ui_toolbar(ctx);
        self.ui_tabbar(ctx);
        if self.search_open { self.ui_search_bar(ctx); }
        self.ui_statusbar(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_text_editor(ui, now);
        });
        if self.prefs_open { self.ui_prefs(ctx); }
        self.ui_modals(ctx);
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.mac_cmd);

        if ctrl && ctx.input(|i| i.key_pressed(EKey::S))     { self.save_now(ctx); }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::Comma)) {
            self.prefs_open = !self.prefs_open;
            if self.prefs_open { self.prefs_just_opened = true; }
            else { self.persist_prefs(); }
        }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::F))     { self.search_open = true; self.replace_mode = false; }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::H))     { self.search_open = true; self.replace_mode = true; }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::T))     { if self.tabs.len() < MAX_TABS { self.add_tab(); } }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::L))     { self.save_now(ctx); self.lock(); }
        if ctx.input(|i| i.key_pressed(EKey::Escape)) {
            if self.search_open {
                self.search_open = false;
            } else if self.prefs_open {
                self.prefs_open = false;
                self.persist_prefs();
            }
        }

        for (k, idx) in [
            (EKey::Num1, 0usize), (EKey::Num2, 1), (EKey::Num3, 2),
            (EKey::Num4, 3),      (EKey::Num5, 4),
        ] {
            if ctrl && ctx.input(|i| i.key_pressed(k)) && idx < self.tabs.len() {
                self.active_tab = idx;
            }
        }
    }

    // ── Toolbar ───────────────────────────────────────────────────────────────

    fn ui_toolbar(&mut self, ctx: &egui::Context) {
        let accent = Color32::from_rgb(124, 106, 247);
        let bg     = if self.prefs.dark_mode { Color32::from_rgb(24,24,28)  } else { Color32::from_rgb(235,235,230) };
        let border = if self.prefs.dark_mode { Color32::from_rgb(46,46,56)  } else { Color32::from_rgb(204,204,196) };

        egui::TopBottomPanel::top("toolbar")
            .exact_height(46.0)
            .frame(egui::Frame::none()
                .fill(bg)
                .stroke(Stroke::new(1.0, border))
                .inner_margin(egui::Margin::symmetric(12.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("▣ SNote").monospace().size(13.0).strong().color(accent));
                    ui.separator();

                    if ui.button(RichText::new("💾 Save").size(12.0))
                        .on_hover_text("Save (Ctrl+S)").clicked()
                    { self.save_now(ctx); }

                    ui.separator();

                    let theme_lbl = if self.prefs.dark_mode { "Light" } else { "Dark" };
                    if ui.button(RichText::new(theme_lbl).size(12.0))
                        .on_hover_text("Toggle theme").clicked()
                    { self.prefs.dark_mode = !self.prefs.dark_mode; self.apply_theme(ctx); self.persist_prefs(); }

                    ui.separator();

                    let (status_text, status_color, status_bg) = if self.dirty {
                        ("UNSAVED", Color32::from_rgb(224, 140, 40), Color32::from_rgba_unmultiplied(224, 140, 40, 30))
                    } else {
                        ("SAVED",   Color32::from_rgb(60, 180, 100), Color32::from_rgba_unmultiplied(60, 180, 100, 30))
                    };
                    egui::Frame::none()
                        .fill(status_bg)
                        .rounding(4.0)
                        .inner_margin(egui::Margin::symmetric(6.0, 2.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(status_text)
                                .size(10.0).strong().monospace()
                                .color(status_color));
                        })
                        .response
                        .on_hover_text(if self.dirty { "Unsaved changes" } else { "All saved" });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(RichText::new("🔒 Lock").size(12.0))
                            .on_hover_text("Save & lock (Ctrl+L)").clicked()
                        { self.save_now(ctx); self.lock(); }

                        let pc = if self.prefs_open { accent } else { ui.visuals().text_color() };
                        if ui.button(RichText::new("Prefs").size(12.0).color(pc))
                            .on_hover_text("Preferences (Ctrl+,)").clicked()
                        {
                            self.prefs_open = !self.prefs_open;
                            if self.prefs_open { self.prefs_just_opened = true; }
                            else { self.persist_prefs(); }
                        }

                        if ui.button(RichText::new("🔍 Find").size(12.0))
                            .on_hover_text("Find / Replace (Ctrl+F / Ctrl+H)").clicked()
                        { self.search_open = true; self.replace_mode = false; }
                    });
                });
            });
    }

    // ── Tab bar ───────────────────────────────────────────────────────────────

    fn ui_tabbar(&mut self, ctx: &egui::Context) {
        let accent = Color32::from_rgb(124, 106, 247);
        let bg     = if self.prefs.dark_mode { Color32::from_rgb(22,22,26)  } else { Color32::from_rgb(228,228,223) };
        let border = if self.prefs.dark_mode { Color32::from_rgb(46,46,56)  } else { Color32::from_rgb(204,204,196) };

        egui::TopBottomPanel::top("tabbar")
            .exact_height(36.0)
            .frame(egui::Frame::none().fill(bg).stroke(Stroke::new(1.0, border)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);

                    let mut switch_to  = None;
                    let mut close_idx  = None;
                    let mut rename_idx = None;

                    let snapshot: Vec<(usize, String, bool)> = self.tabs.iter().enumerate()
                        .map(|(i, t)| (i, t.name.clone(), i == self.active_tab))
                        .collect();

                    for (i, name, is_active) in &snapshot {
                        let i = *i;
                        let is_active = *is_active;

                        // Inline rename input
                        if self.renaming_tab == Some(i) {
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.rename_buf)
                                    .desired_width(100.0)
                                    .font(FontId::proportional(12.0))
                            );
                            let commit = resp.lost_focus()
                                || ctx.input(|inp| inp.key_pressed(EKey::Enter));
                            let cancel = ctx.input(|inp| inp.key_pressed(EKey::Escape));
                            if commit {
                                let v = self.rename_buf.trim().to_string();
                                if !v.is_empty() {
                                    self.tabs[i].name = v.chars().take(64).collect();
                                    self.dirty = true;
                                }
                                self.renaming_tab = None;
                            } else if cancel {
                                self.renaming_tab = None;
                            } else {
                                resp.request_focus();
                            }
                            if i + 1 < self.tabs.len() { ui.separator(); }
                            continue;
                        }

                        let label = name.as_str();
                        let mut text = RichText::new(label).size(12.0);
                        if is_active { text = text.color(accent); }

                        let tab_bg = if is_active && self.dirty {
                            Some(Color32::from_rgba_unmultiplied(224, 140, 40, 18))
                        } else {
                            None
                        };

                        let resp = if let Some(fill) = tab_bg {
                            egui::Frame::none()
                                .fill(fill)
                                .inner_margin(egui::Margin::symmetric(2.0, 0.0))
                                .show(ui, |ui| {
                                    ui.add(egui::Button::new(text).frame(false).min_size(Vec2::new(0.0, 28.0)))
                                })
                                .inner
                        } else {
                            ui.add(egui::Button::new(text).frame(false).min_size(Vec2::new(0.0, 28.0)))
                        };
                        if resp.clicked()        { switch_to  = Some(i); }
                        if resp.double_clicked() { rename_idx = Some(i); }

                        if is_active || resp.hovered() {
                            if ui.add(
                                egui::Button::new(
                                    RichText::new("×").size(13.0)
                                        .color(Color32::from_rgb(136, 136, 160))
                                ).frame(false).min_size(Vec2::new(18.0, 18.0))
                            ).clicked() { close_idx = Some(i); }
                        }

                        if i + 1 < self.tabs.len() { ui.separator(); }
                    }

                    ui.separator();
                    let can_add   = self.tabs.len() < MAX_TABS;
                    let add_color = if can_add { ui.visuals().text_color() } else { Color32::from_rgb(80,80,90) };
                    if ui.add_enabled(can_add,
                        egui::Button::new(RichText::new("+").size(18.0).color(add_color))
                            .frame(false).min_size(Vec2::new(32.0, 28.0))
                    ).on_hover_text("New tab (Ctrl+T)").clicked() { self.add_tab(); }

                    if let Some(i) = switch_to  { self.active_tab = i; }
                    // Instead of closing immediately, open the confirmation modal
                    if let Some(i) = close_idx  {
                        if self.tabs.len() > 1 {
                            self.modal = Modal::CloseTab(i);
                        }
                    }
                    if let Some(i) = rename_idx {
                        self.rename_buf   = self.tabs[i].name.clone();
                        self.renaming_tab = Some(i);
                    }
                });
            });
    }

    // ── Status bar ────────────────────────────────────────────────────────────

    fn ui_statusbar(&mut self, ctx: &egui::Context) {
        let content = self.tabs.get(self.active_tab).map(|t| t.content.as_str()).unwrap_or("");
        let words   = content.split_whitespace().count();
        let chars   = content.len();

        let bg     = if self.prefs.dark_mode { Color32::from_rgb(24,24,28)  } else { Color32::from_rgb(235,235,230) };
        let border = if self.prefs.dark_mode { Color32::from_rgb(46,46,56)  } else { Color32::from_rgb(204,204,196) };
        let dim    = Color32::from_rgb(85, 85, 106);
        let val    = Color32::from_rgb(136, 136, 160);
        let path_c = Color32::from_rgb(70, 70, 90);

        egui::TopBottomPanel::bottom("statusbar")
            .min_height(28.0)
            .frame(egui::Frame::none()
                .fill(bg)
                .stroke(Stroke::new(1.0, border))
                .inner_margin(egui::Margin::symmetric(14.0, 4.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // Ln / Col
                    ui.label(RichText::new("Ln ").size(11.0).monospace().color(dim));
                    ui.label(RichText::new((self.cursor_line + 1).to_string()).size(11.0).monospace().color(val));
                    ui.label(RichText::new("  Col ").size(11.0).monospace().color(dim));
                    ui.label(RichText::new((self.cursor_col + 1).to_string()).size(11.0).monospace().color(val));
                    ui.separator();
                    // Words / Chars
                    ui.label(RichText::new("Words ").size(11.0).monospace().color(dim));
                    ui.label(RichText::new(words.to_string()).size(11.0).monospace().color(val));
                    ui.separator();
                    ui.label(RichText::new("Chars ").size(11.0).monospace().color(dim));
                    ui.label(RichText::new(chars.to_string()).size(11.0).monospace().color(val));

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("enc").size(11.0).monospace()
                            .color(Color32::from_rgb(92, 224, 138)));
                        ui.label(RichText::new("  |  ").size(11.0).monospace().color(Color32::from_rgb(60,60,70)));
                        let (sb_text, sb_color) = if self.dirty {
                            ("unsaved", Color32::from_rgb(224, 140, 40))
                        } else {
                            ("saved",   Color32::from_rgb(60, 180, 100))
                        };
                        ui.label(RichText::new(sb_text).size(11.0).monospace().color(sb_color));
                        ui.label(RichText::new("  |  ").size(11.0).monospace().color(Color32::from_rgb(60,60,70)));
                        // Data directory path
                        ui.label(RichText::new(&self.data_dir).size(10.0).monospace().color(path_c))
                            .on_hover_text("Notes stored at this path");
                    });
                });
            });
    }

    // ── Find / Replace bar ────────────────────────────────────────────────────

    fn ui_search_bar(&mut self, ctx: &egui::Context) {
        let height = if self.replace_mode { 72.0 } else { 40.0 };
        let bg     = if self.prefs.dark_mode { Color32::from_rgb(28,28,34)  } else { Color32::from_rgb(235,235,230) };
        let border = if self.prefs.dark_mode { Color32::from_rgb(46,46,56)  } else { Color32::from_rgb(204,204,196) };

        egui::TopBottomPanel::top("search_bar")
            .exact_height(height)
            .frame(egui::Frame::none()
                .fill(bg)
                .stroke(Stroke::new(1.0, border))
                .inner_margin(egui::Margin::symmetric(10.0, 6.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Find:").size(12.0));
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.search_query)
                            .desired_width(200.0)
                            .font(FontId::monospace(12.0))
                            .hint_text("search…")
                    );
                    if resp.changed() { self.run_search(); }

                    let count_str = if self.search_results.is_empty() {
                        "No matches".to_string()
                    } else {
                        format!("{} match{}", self.search_results.len(),
                            if self.search_results.len() == 1 { "" } else { "es" })
                    };
                    ui.label(RichText::new(count_str).size(11.0).color(Color32::from_rgb(136,136,160)));

                    if ui.button("^").on_hover_text("Previous").clicked() && !self.search_results.is_empty() {
                        let len = self.search_results.len();
                        self.search_idx = (self.search_idx + len - 1) % len;
                    }
                    if ui.button("v").on_hover_text("Next").clicked() && !self.search_results.is_empty() {
                        self.search_idx = (self.search_idx + 1) % self.search_results.len();
                    }

                    let rep_lbl = if self.replace_mode { "[-] Replace" } else { "[+] Replace" };
                    if ui.button(RichText::new(rep_lbl).size(11.0)).clicked() {
                        self.replace_mode = !self.replace_mode;
                    }

                    if ui.button(RichText::new("[x]").size(11.0)).clicked() {
                        self.search_open = false;
                    }
                });

                if self.replace_mode {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Replace:").size(12.0));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.replace_query)
                                .desired_width(200.0)
                                .font(FontId::monospace(12.0))
                                .hint_text("replacement…")
                        );
                        if ui.button(RichText::new("Replace All").size(11.0)).clicked() {
                            self.replace_all();
                        }
                    });
                }
            });
    }

    fn ui_text_editor(&mut self, ui: &mut egui::Ui, now: f64) {
        if self.tabs.is_empty() { return; }

        let available = ui.available_size();

        // Make the cursor clearly visible by boosting the selection colour.
        ui.visuals_mut().selection.bg_fill =
            Color32::from_rgba_unmultiplied(124, 106, 247, 180);
        ui.visuals_mut().selection.stroke =
            Stroke::new(1.0, Color32::from_rgb(180, 170, 255));

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let output = egui::TextEdit::multiline(&mut self.tabs[self.active_tab].content)
                    .font(FontId::monospace(self.prefs.font_size))
                    .desired_width(available.x)
                    .desired_rows(1)
                    .min_size(available)
                    .frame(false)
                    .lock_focus(true)
                    .show(ui);
                if output.response.changed() { self.dirty = true; self.last_edit_time = now; }

                // Update cursor position for status bar
                if let Some(cursor) = output.cursor_range {
                    let text    = self.tabs[self.active_tab].content.as_str();
                    let idx     = cursor.primary.ccursor.index.min(text.len());
                    let before  = &text[..idx];
                    self.cursor_line = before.chars().filter(|&c| c == '\n').count();
                    self.cursor_col  = before.rfind('\n').map(|p| idx - p - 1).unwrap_or(idx);
                }
            });
    }

    // ── Preferences panel ─────────────────────────────────────────────────────

    fn ui_prefs(&mut self, ctx: &egui::Context) {
        let panel_w  = 280.0;
        let screen_w = ctx.screen_rect().width();
        let bg       = if self.prefs.dark_mode { Color32::from_rgb(24,24,28)  } else { Color32::from_rgb(235,235,230) };
        let border   = if self.prefs.dark_mode { Color32::from_rgb(46,46,56)  } else { Color32::from_rgb(204,204,196) };
        let lc       = Color32::from_rgb(136, 136, 160);

        let resp = egui::Area::new("prefs_panel".into())
            .fixed_pos([screen_w - panel_w, 82.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(bg)
                    .stroke(Stroke::new(1.0, border))
                    .rounding(egui::Rounding { nw: 8.0, ne: 0.0, sw: 8.0, se: 0.0 })
                    .inner_margin(egui::Margin::same(20.0))
                    .show(ui, |ui| {
                        ui.set_min_width(panel_w - 2.0);
                        ui.set_max_width(panel_w - 2.0);

                        ui.label(RichText::new("Preferences").size(13.0).strong());
                        ui.add_space(12.0);

                        // Auto-save
                        ui.label(RichText::new("AUTO SAVE").size(10.0).strong().color(lc));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Save automatically").size(11.0));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let prev = self.prefs.auto_save;
                                ui.checkbox(&mut self.prefs.auto_save, "");
                                if self.prefs.auto_save != prev { self.persist_prefs(); }
                            });
                        });
                        if self.prefs.auto_save {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Delay (s)").size(11.0));
                                let prev = self.prefs.auto_save_delay;
                                ui.add(egui::Slider::new(&mut self.prefs.auto_save_delay, 1.0..=30.0).integer());
                                if (self.prefs.auto_save_delay - prev).abs() > f64::EPSILON { self.persist_prefs(); }
                            });
                        }

                        ui.add(egui::Separator::default());

                        // Font size
                        ui.label(RichText::new("FONT SIZE").size(10.0).strong().color(lc));
                        ui.horizontal(|ui| {
                            let prev = self.prefs.font_size;
                            ui.add(egui::Slider::new(&mut self.prefs.font_size, 10.0..=28.0).integer());
                            if (self.prefs.font_size - prev).abs() > f32::EPSILON { self.persist_prefs(); }
                            ui.label(RichText::new(format!("{}px", self.prefs.font_size as u32))
                                .size(11.0).monospace());
                        });

                        ui.add(egui::Separator::default());

                        // Security
                        ui.label(RichText::new("SECURITY").size(10.0).strong().color(lc));
                        if ui.button(RichText::new("Change master password…").size(12.0)).clicked() {
                            self.cp_current.clear(); self.cp_new.clear();
                            self.cp_confirm.clear(); self.cp_error.clear();
                            self.modal = Modal::ChangePassword;
                        }

                        ui.add_space(6.0);
                        ui.add(egui::Separator::default());

                        // Danger zone
                        ui.label(RichText::new("DANGER ZONE").size(10.0).strong()
                            .color(Color32::from_rgb(224, 92, 92)));
                        if ui.add(
                            egui::Button::new(
                                RichText::new("Erase all data…").size(12.0)
                                    .color(Color32::from_rgb(224, 92, 92))
                            ).stroke(Stroke::new(1.0, Color32::from_rgb(224, 92, 92)))
                        ).clicked() { self.modal = Modal::Erase; }
                    });
            });

        // Close prefs when user clicks outside — but skip the frame it was opened
        // (that click is what opened it; without the guard it closes immediately).
        if self.prefs_just_opened {
            self.prefs_just_opened = false;
        } else {
            let panel_rect = resp.response.rect;
            let clicked_outside = ctx.input(|i| i.pointer.any_click())
                && ctx.input(|i| {
                    i.pointer.interact_pos()
                        .map(|p| !panel_rect.contains(p))
                        .unwrap_or(false)
                });
            if clicked_outside {
                self.prefs_open = false;
                self.persist_prefs();
            }
        }
    }

    // ── Modals ────────────────────────────────────────────────────────────────

    fn ui_modals(&mut self, ctx: &egui::Context) {
        match self.modal.clone() {
            Modal::None => {}

            Modal::CloseTab(idx) => {
                let tab_name = self.tabs.get(idx).map(|t| t.name.clone()).unwrap_or_default();
                let mut open = true;
                egui::Window::new("Close tab?")
                    .collapsible(false).resizable(false).min_width(320.0)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.label(format!("Close \"{}\" and delete its contents?", tab_name));
                        ui.label(RichText::new("This cannot be undone.").strong());
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() { self.modal = Modal::None; }
                            if ui.add(
                                egui::Button::new(
                                    RichText::new("Close & delete").color(Color32::from_rgb(224,92,92))
                                ).stroke(Stroke::new(1.0, Color32::from_rgb(224,92,92)))
                            ).clicked() {
                                self.close_tab_confirmed(idx);
                                self.modal = Modal::None;
                            }
                        });
                    });
                if !open { self.modal = Modal::None; }
            }

            Modal::Erase => {
                let mut open = true;
                egui::Window::new("Erase all data?")
                    .collapsible(false).resizable(false).min_width(340.0)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.label("This will permanently delete all notes and reset the master password.");
                        ui.label(RichText::new("This cannot be undone.").strong());
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() { self.modal = Modal::None; }
                            if ui.add(
                                egui::Button::new(
                                    RichText::new("Erase everything").color(Color32::from_rgb(224,92,92))
                                ).stroke(Stroke::new(1.0, Color32::from_rgb(224,92,92)))
                            ).clicked() { self.erase_all(); }
                        });
                    });
                if !open { self.modal = Modal::None; }
            }

            Modal::ChangePassword => {
                let mut open      = true;
                let mut do_change = false;
                egui::Window::new("Change Master Password")
                    .collapsible(false).resizable(false).min_width(340.0)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        let lc = Color32::from_rgb(136, 136, 160);
                        ui.label(RichText::new("CURRENT PASSWORD").size(10.0).strong().color(lc));
                        ui.add(egui::TextEdit::singleline(&mut self.cp_current)
                            .password(true).desired_width(f32::INFINITY).hint_text("Current password…"));
                        ui.add_space(8.0);
                        ui.label(RichText::new("NEW PASSWORD").size(10.0).strong().color(lc));
                        ui.add(egui::TextEdit::singleline(&mut self.cp_new)
                            .password(true).desired_width(f32::INFINITY).hint_text("New password…"));
                        ui.add_space(8.0);
                        ui.label(RichText::new("CONFIRM NEW").size(10.0).strong().color(lc));
                        ui.add(egui::TextEdit::singleline(&mut self.cp_confirm)
                            .password(true).desired_width(f32::INFINITY).hint_text("Confirm…"));
                        if !self.cp_error.is_empty() {
                            ui.add_space(6.0);
                            ui.label(RichText::new(&self.cp_error.clone()).size(11.0)
                                .color(Color32::from_rgb(224, 92, 92)));
                        }
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() { self.modal = Modal::None; }
                            if ui.add(
                                egui::Button::new(RichText::new("Update Password"))
                                    .fill(Color32::from_rgb(124, 106, 247))
                            ).clicked() { do_change = true; }
                        });
                    });
                if !open     { self.modal = Modal::None; }
                if do_change { self.change_password(ctx); }
            }
        }
    }
}

// ─── Fatal error helper ───────────────────────────────────────────────────────
//
// On Windows release builds there is no console, so we show a message box
// for any startup failure instead of silently exiting.

fn fatal_error(msg: &str) -> ! {
    #[cfg(target_os = "windows")]
    {
        // Use the Windows MessageBoxW API via a raw FFI call — no extra crate needed.
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        let title: Vec<u16> = OsStr::new("SecureNote — Error")
            .encode_wide().chain(std::iter::once(0)).collect();
        let text: Vec<u16> = OsStr::new(msg)
            .encode_wide().chain(std::iter::once(0)).collect();

        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(hwnd: *mut std::ffi::c_void,
                           text: *const u16, caption: *const u16,
                           utype: u32) -> i32;
        }
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10); }
    }

    #[cfg(not(target_os = "windows"))]
    eprintln!("SecureNote error: {}", msg);

    process::exit(1);
}

// ─── Single-instance lock ─────────────────────────────────────────────────────
//
// Writes our PID to app.lock on startup.
// On the next launch, reads the stored PID and checks if it is still alive.
// Uses platform-native checks with no external crates.

fn pid_is_running(pid: u32) -> bool {
    if pid == 0 { return false; }

    #[cfg(target_os = "linux")]
    { std::path::Path::new(&format!("/proc/{}", pid)).exists() }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[cfg(target_os = "windows")]
    {
        // OpenProcess with SYNCHRONIZE (0x00100000) — succeeds only if the
        // process exists and we have permission to observe it.
        #[link(name = "kernel32")]
        extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }
        const SYNCHRONIZE: u32 = 0x00100000;
        unsafe {
            let h = OpenProcess(SYNCHRONIZE, 0, pid);
            if h.is_null() {
                false
            } else {
                CloseHandle(h);
                true
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    { false }
}

fn acquire_lock(lock_path: &PathBuf) -> bool {
    if let Ok(contents) = fs::read_to_string(lock_path) {
        if let Ok(pid) = contents.trim().parse::<u32>() {
            if pid != process::id() && pid_is_running(pid) {
                return false;
            }
        }
    }
    let _ = fs::write(lock_path, process::id().to_string());
    true
}

// ─── wgpu configuration ───────────────────────────────────────────────────────
//
// On Windows we explicitly request the DX12 backend and allow wgpu to fall
// back to the WARP software rasterizer when no physical GPU is available.
// This makes the app work on CI runners and machines without GPU drivers.
// On Linux and macOS the defaults are used (Vulkan / Metal).

fn wgpu_config() -> eframe::egui_wgpu::WgpuConfiguration {
    #[cfg(target_os = "windows")]
    {
        eframe::egui_wgpu::WgpuConfiguration {
            supported_backends: wgpu::Backends::DX12,
            device_descriptor: std::sync::Arc::new(|_adapter| {
                wgpu::DeviceDescriptor {
                    label: Some("secure-note"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                }
            }),
            power_preference: wgpu::PowerPreference::None,
            ..Default::default()
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        eframe::egui_wgpu::WgpuConfiguration::default()
    }
}

// ─── Entry point ──────────────────────────────────────────────────────────────
//
// Custom icon: place icon.png in the data directory — loaded at runtime,
// no recompile needed. Falls back to the built-in placeholder if absent.

fn main() -> eframe::Result<()> {
    let cli = Cli::parse();

    if let Err(e) = fs::create_dir_all(&cli.data) {
        fatal_error(&format!("Could not create data directory {:?}: {}", cli.data, e));
    }

    let data_abs = cli.data.canonicalize().unwrap_or_else(|_| cli.data.clone());
    let data_str = data_abs.display().to_string();

    let notes_file  = data_abs.join("notes.enc");
    let config_file = data_abs.join("config.json");
    let prefs_file  = data_abs.join("prefs.json");
    let icon_file   = data_abs.join("icon.png");
    let lock_file   = data_abs.join("app.lock");

    // ── Single instance ───────────────────────────────────────────────────────
    if !acquire_lock(&lock_file) {
        fatal_error("SecureNote is already running.\n\nOnly one instance is allowed at a time.");
    }

    // ── Prefs (needed for window geometry before building options) ────────────
    let prefs = load_prefs(&prefs_file);

    // ── Icon ──────────────────────────────────────────────────────────────────
    let icon_data = fs::read(&icon_file)
        .ok()
        .and_then(|b| eframe::icon_data::from_png_bytes(&b).ok())
        .unwrap_or_else(|| eframe::icon_data::from_png_bytes(ICON_PNG).unwrap_or_default());

    // ── Window geometry ───────────────────────────────────────────────────────
    let mut vp = egui::ViewportBuilder::default()
        .with_title("SecureNote")
        .with_min_inner_size([600.0, 400.0])
        .with_icon(icon_data);

    if let (Some(x), Some(y)) = (prefs.win_x, prefs.win_y) {
        vp = vp.with_position([x, y]);
    }
    vp = vp.with_inner_size([
        prefs.win_w.unwrap_or(900.0),
        prefs.win_h.unwrap_or(640.0),
    ]);

    let options = eframe::NativeOptions {
        viewport: vp,
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_config(),
        ..Default::default()
    };

    eframe::run_native(
        "SecureNote",
        options,
        Box::new(|cc| {
            let app = SecureNote::new(notes_file, config_file, prefs_file, lock_file, data_str);
            app.apply_theme(&cc.egui_ctx);
            Box::new(app)
        }),
    )
}

// ─── Built-in 1×1 transparent PNG placeholder icon ────────────────────────────

static ICON_PNG: &[u8] = &[
    0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a,
    0x00,0x00,0x00,0x0d,0x49,0x48,0x44,0x52,
    0x00,0x00,0x00,0x01,0x00,0x00,0x00,0x01,
    0x08,0x02,0x00,0x00,0x00,0x90,0x77,0x53,
    0xde,0x00,0x00,0x00,0x0c,0x49,0x44,0x41,
    0x54,0x08,0xd7,0x63,0xf8,0xcf,0xc0,0x00,
    0x00,0x00,0x02,0x00,0x01,0xe2,0x21,0xbc,
    0x33,0x00,0x00,0x00,0x00,0x49,0x45,0x4e,
    0x44,0xae,0x42,0x60,0x82,
];
