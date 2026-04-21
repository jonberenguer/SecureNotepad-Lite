use crate::crypto::{hash_password, verify_password};
use crate::storage::{
    Prefs, Tab,
    default_tabs, load_config, load_prefs, load_tabs,
    save_config, save_prefs, save_tabs,
};
use eframe::egui::{self, Color32, FontId, Key as EKey, Modifiers, RichText, Stroke, Vec2};
use rfd::FileDialog;
use std::{collections::HashMap, fs, path::PathBuf};
use zeroize::{Zeroize, Zeroizing};

// ─── Constants ────────────────────────────────────────────────────────────────

pub const MAX_TABS:      usize = 5;
const MAX_UNDO:          usize = 50;
const UNDO_INTERVAL:     f64   = 1.5;

// ─── App state types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Screen { Lock, Editor }

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    None,
    Erase,
    ChangePassword,
    /// Confirm closing tab at this index.
    CloseTab(usize),
}

// ─── App struct ───────────────────────────────────────────────────────────────

pub struct SecureNote {
    notes_file:  PathBuf,
    config_file: PathBuf,
    prefs_file:  PathBuf,
    lock_file:   PathBuf,
    data_dir:    String,

    // Lock screen
    screen:         Screen,
    password_input: Zeroizing<String>,
    confirm_input:  Zeroizing<String>,
    lock_error:     String,
    is_setup:       bool,

    // Session
    session_password: Zeroizing<String>,

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
    prefs:             Prefs,
    prefs_open:        bool,
    // Prevents the panel from immediately closing when the Prefs button click
    // is detected as "click outside" on the same frame the panel opens.
    prefs_just_opened: bool,

    // Modals
    modal:      Modal,
    cp_current: Zeroizing<String>,
    cp_new:     Zeroizing<String>,
    cp_confirm: Zeroizing<String>,
    cp_error:   String,

    // Per-tab unlock modal
    tab_unlock_pw:    Zeroizing<String>,
    tab_unlock_error: String,

    // Inline tab rename
    renaming_tab: Option<usize>,
    rename_buf:   String,

    // Toast
    toast_msg:    String,
    toast_expire: f64,

    // Auto-lock on idle
    last_activity_time: f64,

    // Clipboard auto-clear
    clipboard_clear_at: Option<f64>,

    // Per-tab undo/redo history keyed by tab.id
    undo_stacks:      HashMap<u32, Vec<String>>,
    undo_positions:   HashMap<u32, usize>,
    undo_last_snap:   f64,
    undo_in_progress: bool,
}

impl SecureNote {
    pub fn new(
        notes_file: PathBuf, config_file: PathBuf,
        prefs_file: PathBuf, lock_file: PathBuf,
        data_dir: String,
    ) -> Self {
        let cfg      = load_config(&config_file);
        let is_setup = cfg.password_hash.is_some();
        let prefs    = load_prefs(&prefs_file);
        Self {
            notes_file,
            config_file,
            prefs_file,
            lock_file,
            data_dir,
            screen:             Screen::Lock,
            password_input:     Zeroizing::default(),
            confirm_input:      Zeroizing::default(),
            lock_error:         String::new(),
            is_setup,
            session_password:   Zeroizing::default(),
            tabs:               vec![],
            active_tab:         0,
            dirty:              false,
            last_edit_time:     0.0,
            search_open:        false,
            search_query:       String::new(),
            replace_query:      String::new(),
            replace_mode:       false,
            search_results:     vec![],
            search_idx:         0,
            cursor_line:        0,
            cursor_col:         0,
            prefs,
            prefs_open:         false,
            prefs_just_opened:  false,
            modal:              Modal::None,
            cp_current:         Zeroizing::default(),
            cp_new:             Zeroizing::default(),
            cp_confirm:         Zeroizing::default(),
            cp_error:           String::new(),
            tab_unlock_pw:      Zeroizing::default(),
            tab_unlock_error:   String::new(),
            renaming_tab:       None,
            rename_buf:         String::new(),
            toast_msg:          String::new(),
            toast_expire:       0.0,
            last_activity_time: 0.0,
            clipboard_clear_at:    None,
            undo_stacks:        HashMap::new(),
            undo_positions:     HashMap::new(),
            undo_last_snap:     0.0,
            undo_in_progress:   false,
        }
    }

    pub fn apply_theme(&self, ctx: &egui::Context) {
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
            Ok(_)  => {
                self.dirty = false;
                if let Some(tab) = self.tabs.get(self.active_tab) {
                    let id      = tab.id;
                    let content = tab.content.clone();
                    self.push_undo_snap(id, content);
                }
                self.toast("Saved", ctx);
            }
            Err(e) => { self.toast(format!("Save failed: {e}"), ctx); }
        }
    }

    fn lock(&mut self) {
        self.session_password.zeroize();
        for tab in &mut self.tabs {
            tab.content.zeroize();
        }
        // Zero undo history snapshots (plaintext content) before freeing.
        for stack in self.undo_stacks.values_mut() {
            for s in stack.iter_mut() {
                s.zeroize();
            }
        }
        self.screen           = Screen::Lock;
        self.session_password = Zeroizing::default();
        self.tabs             = vec![];
        self.active_tab       = 0;
        self.dirty            = false;
        self.search_open      = false;
        self.prefs_open       = false;
        self.modal            = Modal::None;
        self.cp_current.zeroize();
        self.cp_new.zeroize();
        self.cp_confirm.zeroize();
        self.cp_error.clear();
        self.tab_unlock_pw.zeroize();
        self.tab_unlock_error.clear();
        self.undo_stacks.clear();
        self.undo_positions.clear();
        self.last_activity_time    = 0.0;
        self.clipboard_clear_at    = None;
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
            let h = match hash_password(&pw) {
                Ok(h) => h,
                Err(e) => { self.lock_error = e.into(); return; }
            };
            cfg.password_hash = Some(h);
            save_config(&self.config_file, &cfg);
            let tabs = default_tabs();
            if save_tabs(&self.notes_file, &pw, &tabs).is_err() {
                self.lock_error = "Failed to create notes file.".into();
                return;
            }
            self.session_password = pw;
            self.tabs             = tabs;
            self.active_tab       = 0;
            self.init_undo_stacks();
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
                    self.init_undo_stacks();
                    self.enter_editor(ctx);
                }
                Err(e) => { self.lock_error = e.to_string(); }
            }
        }
    }

    fn enter_editor(&mut self, ctx: &egui::Context) {
        self.screen             = Screen::Editor;
        self.lock_error         = String::new();
        self.password_input     = Zeroizing::default();
        self.confirm_input      = Zeroizing::default();
        self.dirty              = false;
        self.last_activity_time = ctx.input(|i| i.time);
        self.apply_theme(ctx);
    }

    fn add_tab(&mut self) {
        if self.tabs.len() >= MAX_TABS { return; }
        let id   = self.tabs.iter().map(|t| t.id).max().unwrap_or(0).saturating_add(1);
        let name = format!("Note {}", self.tabs.len() + 1);
        self.tabs.push(Tab { id, name, content: String::new(), locked: false });
        self.active_tab = self.tabs.len() - 1;
        self.dirty = true;
        self.undo_stacks.insert(id, vec![String::new()]);
        self.undo_positions.insert(id, 0);
    }

    fn add_tab_with(&mut self, name: String, content: String) {
        if self.tabs.len() >= MAX_TABS { return; }
        let id = self.tabs.iter().map(|t| t.id).max().unwrap_or(0).saturating_add(1);
        self.tabs.push(Tab { id, name, content: content.clone(), locked: false });
        self.active_tab = self.tabs.len() - 1;
        self.dirty = true;
        self.undo_stacks.insert(id, vec![content]);
        self.undo_positions.insert(id, 0);
    }

    fn close_tab_confirmed(&mut self, idx: usize) {
        if self.tabs.len() <= 1 { return; }
        let removed_id = self.tabs[idx].id;
        self.tabs.remove(idx);
        self.undo_stacks.remove(&removed_id);
        self.undo_positions.remove(&removed_id);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.dirty = true;
    }

    fn erase_all(&mut self) {
        let _ = fs::remove_file(&self.notes_file);
        let _ = fs::remove_file(&self.config_file);
        let _ = fs::remove_file(&self.lock_file);
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

        // Back up the encrypted file so it can be restored if the config write fails.
        let backup: Option<Zeroizing<Vec<u8>>> = fs::read(&self.notes_file).ok().map(Zeroizing::new);

        if let Err(e) = save_tabs(&self.notes_file, &new_pw, &self.tabs) {
            self.cp_error = format!("Re-encrypt failed: {e}");
            return;
        }

        let h = match hash_password(&new_pw) {
            Ok(h) => h,
            Err(e) => { self.cp_error = e.into(); return; }
        };
        cfg.password_hash = Some(h);
        if let Ok(s) = serde_json::to_string_pretty(&cfg) {
            if let Err(e) = fs::write(&self.config_file, s) {
                // Config write failed — restore original so hash and ciphertext stay in sync.
                if let Some(original) = backup {
                    let _ = fs::write(&self.notes_file, &**original);
                }
                self.cp_error = format!("Failed to save config: {e}");
                return;
            }
        }

        self.session_password = new_pw;
        self.modal = Modal::None;
        self.cp_current.zeroize(); self.cp_new.zeroize();
        self.cp_confirm.zeroize(); self.cp_error.clear();
        self.toast("Password updated", ctx);
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

    // ── Undo / Redo ───────────────────────────────────────────────────────────

    fn init_undo_stacks(&mut self) {
        self.undo_stacks.clear();
        self.undo_positions.clear();
        for tab in &self.tabs {
            self.undo_stacks.insert(tab.id, vec![tab.content.clone()]);
            self.undo_positions.insert(tab.id, 0);
        }
    }

    fn push_undo_snap(&mut self, tab_id: u32, content: String) {
        let stack = self.undo_stacks.entry(tab_id).or_insert_with(Vec::new);
        let pos   = self.undo_positions.entry(tab_id).or_insert(0);

        if stack.get(*pos).map(String::as_str) == Some(&content) { return; }

        stack.truncate(*pos + 1);
        stack.push(content);

        if stack.len() > MAX_UNDO {
            stack.remove(0);
        }
        *pos = stack.len() - 1;
    }

    fn do_undo(&mut self, ctx: &egui::Context) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            let id  = tab.id;
            let pos = *self.undo_positions.get(&id).unwrap_or(&0);
            if pos > 0 {
                let new_pos = pos - 1;
                if let Some(content) = self.undo_stacks.get(&id)
                    .and_then(|s| s.get(new_pos)).cloned()
                {
                    self.undo_in_progress = true;
                    self.tabs[self.active_tab].content = content;
                    *self.undo_positions.entry(id).or_insert(0) = new_pos;
                    self.undo_in_progress = false;
                    self.dirty = true;
                    self.last_edit_time = ctx.input(|i| i.time);
                }
            } else {
                self.toast("Nothing more to undo", ctx);
            }
        }
    }

    fn do_redo(&mut self, ctx: &egui::Context) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            let id    = tab.id;
            let pos   = *self.undo_positions.get(&id).unwrap_or(&0);
            let limit = self.undo_stacks.get(&id).map(|s| s.len()).unwrap_or(0);
            if pos + 1 < limit {
                let new_pos = pos + 1;
                if let Some(content) = self.undo_stacks.get(&id)
                    .and_then(|s| s.get(new_pos)).cloned()
                {
                    self.undo_in_progress = true;
                    self.tabs[self.active_tab].content = content;
                    *self.undo_positions.entry(id).or_insert(0) = new_pos;
                    self.undo_in_progress = false;
                    self.dirty = true;
                    self.last_edit_time = ctx.input(|i| i.time);
                }
            } else {
                self.toast("Nothing more to redo", ctx);
            }
        }
    }

    // ── Per-tab lock ──────────────────────────────────────────────────────────

    fn try_unlock_tab(&mut self, idx: usize, ctx: &egui::Context) {
        let pw = self.tab_unlock_pw.clone();
        if pw.is_empty() {
            self.tab_unlock_error = "Password required.".into();
            return;
        }
        let cfg = load_config(&self.config_file);
        if verify_password(&pw, cfg.password_hash.as_deref().unwrap_or("")) {
            if let Some(tab) = self.tabs.get_mut(idx) {
                tab.locked = false;
                self.dirty = true;
            }
            self.tab_unlock_pw.zeroize();
            self.tab_unlock_error.clear();
            self.modal = Modal::None;
            self.toast("Tab unlocked", ctx);
        } else {
            self.tab_unlock_error = "Incorrect password.".into();
        }
    }

    // ── Export / Import ───────────────────────────────────────────────────────

    fn do_export_tab(&mut self, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get(self.active_tab) else { return };
        if tab.locked {
            self.toast("Unlock this tab before exporting", ctx);
            return;
        }
        let file_name = format!("{}.txt", tab.name);
        let content   = tab.content.clone();
        if let Some(path) = FileDialog::new()
            .set_title("Export note as plain text")
            .set_file_name(&file_name)
            .add_filter("Text files", &["txt"])
            .add_filter("All files", &["*"])
            .save_file()
        {
            match fs::write(&path, content) {
                Ok(_)  => self.toast(format!("Exported to {}", path.display()), ctx),
                Err(e) => self.toast(format!("Export failed: {e}"), ctx),
            }
        }
    }

    fn do_import_tab(&mut self, ctx: &egui::Context) {
        if self.tabs.len() >= MAX_TABS {
            self.toast("Max tabs reached — close a tab first", ctx);
            return;
        }
        if let Some(path) = FileDialog::new()
            .set_title("Import plain text as new tab")
            .add_filter("Text files", &["txt"])
            .add_filter("All files", &["*"])
            .pick_file()
        {
            match fs::read_to_string(&path) {
                Ok(content) => {
                    let name: String = path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("Imported")
                        .chars().take(64).collect();
                    self.add_tab_with(name, content);
                    self.dirty = true;
                    self.toast("Imported", ctx);
                }
                Err(e) => self.toast(format!("Import failed: {e}"), ctx),
            }
        }
    }
}

// ─── eframe App trait ─────────────────────────────────────────────────────────

impl eframe::App for SecureNote {
    fn on_exit(&mut self) {
        if self.dirty && !self.session_password.is_empty() {
            let _ = save_tabs(&self.notes_file, &self.session_password, &self.tabs);
        }
        let _ = fs::remove_file(&self.lock_file);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = ctx.input(|i| i.time);

        if ctx.input(|i| !i.events.is_empty()) {
            self.last_activity_time = now;
        }

        // Persist window geometry every frame so it survives crashes.
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

        // Auto-save.
        if self.dirty && self.prefs.auto_save && self.last_edit_time > 0.0
            && now - self.last_edit_time >= self.prefs.auto_save_delay
        {
            self.save_now(ctx);
        }

        // Undo snapshot on typing inactivity.
        if self.dirty && !self.undo_in_progress && self.last_edit_time > 0.0
            && now - self.last_edit_time >= UNDO_INTERVAL
            && now - self.undo_last_snap  >= UNDO_INTERVAL
        {
            if let Some(tab) = self.tabs.get(self.active_tab) {
                let id      = tab.id;
                let content = tab.content.clone();
                self.push_undo_snap(id, content);
                self.undo_last_snap = now;
            }
        }

        // Auto-lock on idle.
        if self.screen == Screen::Editor
            && self.prefs.auto_lock
            && self.last_activity_time > 0.0
            && now - self.last_activity_time >= self.prefs.auto_lock_delay * 60.0
        {
            self.save_now(ctx);
            self.lock();
        }

        match self.screen.clone() {
            Screen::Lock   => self.ui_lock(ctx),
            Screen::Editor => self.ui_editor(ctx, now),
        }

        // Clipboard auto-clear: write a space via eframe's copied_text so eframe
        // propagates it to the OS clipboard (eframe uses arboard internally).
        let copied_this_frame = ctx.output(|o| !o.copied_text.is_empty());
        if copied_this_frame && self.prefs.clipboard_clear {
            self.clipboard_clear_at = Some(now + self.prefs.clipboard_clear_delay);
        }
        if let Some(clear_at) = self.clipboard_clear_at {
            if now >= clear_at {
                self.clipboard_clear_at = None;
                ctx.output_mut(|o| o.copied_text = " ".into());
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(clear_at - now));
            }
        }

        // Toast overlay.
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

// ─── UI ───────────────────────────────────────────────────────────────────────

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
                                egui::TextEdit::singleline(&mut *self.password_input)
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
                                    egui::TextEdit::singleline(&mut *self.confirm_input)
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
            self.ui_text_editor(ui, ctx, now);
        });
        if self.prefs_open { self.ui_prefs(ctx); }
        self.ui_modals(ctx);
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.mac_cmd);

        if ctrl && ctx.input(|i| i.key_pressed(EKey::S)) { self.save_now(ctx); }

        if ctrl && ctx.input(|i| i.key_pressed(EKey::Comma)) {
            self.prefs_open = !self.prefs_open;
            if self.prefs_open { self.prefs_just_opened = true; }
            else { self.persist_prefs(); }
        }

        if ctrl && ctx.input(|i| i.key_pressed(EKey::F)) { self.search_open = true; self.replace_mode = false; }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::H)) { self.search_open = true; self.replace_mode = true; }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::T)) { if self.tabs.len() < MAX_TABS { self.add_tab(); } }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::L)) { self.save_now(ctx); self.lock(); }

        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, EKey::Z)) { self.do_undo(ctx); }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, EKey::Y)) { self.do_redo(ctx); }

        if ctrl && ctx.input(|i| i.key_pressed(EKey::E)) { self.do_export_tab(ctx); }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::I)) { self.do_import_tab(ctx); }

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

                    if ui.button(RichText::new("↓ Export").size(12.0))
                        .on_hover_text("Export current tab as .txt (Ctrl+E)").clicked()
                    { self.do_export_tab(ctx); }

                    if ui.button(RichText::new("↑ Import").size(12.0))
                        .on_hover_text("Import .txt file as new tab (Ctrl+I)").clicked()
                    { self.do_import_tab(ctx); }

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
        let accent       = Color32::from_rgb(124, 106, 247);
        let locked_color = Color32::from_rgb(200, 160, 60);
        let bg     = if self.prefs.dark_mode { Color32::from_rgb(22,22,26)  } else { Color32::from_rgb(228,228,223) };
        let border = if self.prefs.dark_mode { Color32::from_rgb(46,46,56)  } else { Color32::from_rgb(204,204,196) };

        egui::TopBottomPanel::top("tabbar")
            .exact_height(36.0)
            .frame(egui::Frame::none().fill(bg).stroke(Stroke::new(1.0, border)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);

                    // Tab name is truncated to keep all tabs a consistent width.
                    // Exception: names shorter than the limit display naturally.
                    const TAB_NAME_MAX: usize = 11;
                    const TAB_NAME_W:   f32   = 76.0;  // fixed name-button width
                    const TAB_LOCK_W:   f32   = 22.0;  // always-visible lock icon
                    const TAB_CLOSE_W:  f32   = 18.0;  // hover-only close button

                    let mut switch_to  = None;
                    let mut close_idx  = None;
                    let mut rename_idx = None;
                    let mut lock_idx   = None;

                    let snapshot: Vec<(usize, String, bool, bool)> = self.tabs.iter().enumerate()
                        .map(|(i, t)| (i, t.name.clone(), i == self.active_tab, t.locked))
                        .collect();

                    for (i, name, is_active, is_locked) in &snapshot {
                        let i         = *i;
                        let is_active = *is_active;
                        let is_locked = *is_locked;

                        // Inline rename input — no lock icon while renaming.
                        if self.renaming_tab == Some(i) {
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.rename_buf)
                                    .desired_width(TAB_NAME_W + TAB_LOCK_W)
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

                        // Truncate name to keep tab width consistent.
                        let truncated: String = if name.chars().count() > TAB_NAME_MAX {
                            format!("{}…", name.chars().take(TAB_NAME_MAX - 1).collect::<String>())
                        } else {
                            name.clone()
                        };

                        // ── Lock icon (always visible) ────────────────────────
                        let lock_icon  = if is_locked { "🔒" } else { "🔓" };
                        let lock_color = if is_locked {
                            locked_color
                        } else {
                            Color32::from_rgb(80, 80, 100)
                        };
                        let lock_btn = ui.add(
                            egui::Button::new(RichText::new(lock_icon).size(11.0).color(lock_color))
                                .frame(false)
                                .min_size(Vec2::new(TAB_LOCK_W, 28.0))
                        ).on_hover_text(if is_locked { "Click to unlock tab" } else { "Click to lock tab" });

                        if lock_btn.clicked() {
                            if is_locked {
                                // Navigate to the tab; the inline overlay handles the password.
                                switch_to = Some(i);
                            } else {
                                lock_idx = Some(i);
                            }
                        }

                        // ── Tab name button (fixed width) ─────────────────────
                        let mut text = RichText::new(&truncated).size(12.0);
                        if is_active {
                            text = text.color(if is_locked { locked_color } else { accent });
                        } else if is_locked {
                            text = text.color(Color32::from_rgb(160, 130, 50));
                        }

                        let tab_bg = if is_active && self.dirty && !is_locked {
                            Some(Color32::from_rgba_unmultiplied(224, 140, 40, 18))
                        } else {
                            None
                        };

                        let resp = if let Some(fill) = tab_bg {
                            egui::Frame::none()
                                .fill(fill)
                                .inner_margin(egui::Margin::symmetric(2.0, 0.0))
                                .show(ui, |ui| {
                                    ui.add(egui::Button::new(text).frame(false)
                                        .min_size(Vec2::new(TAB_NAME_W, 28.0)))
                                })
                                .inner
                        } else {
                            ui.add(egui::Button::new(text).frame(false)
                                .min_size(Vec2::new(TAB_NAME_W, 28.0)))
                        };

                        if resp.clicked()        { switch_to  = Some(i); }
                        if resp.double_clicked() && !is_locked { rename_idx = Some(i); }

                        // ── Close button (hover/active, unlocked only) ────────
                        if !is_locked && (is_active || resp.hovered() || lock_btn.hovered()) {
                            if ui.add(
                                egui::Button::new(
                                    RichText::new("×").size(13.0)
                                        .color(Color32::from_rgb(136, 136, 160))
                                ).frame(false).min_size(Vec2::new(TAB_CLOSE_W, 28.0))
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
                    if let Some(i) = close_idx  {
                        if self.tabs.len() > 1 {
                            self.modal = Modal::CloseTab(i);
                        }
                    }
                    if let Some(i) = rename_idx {
                        self.rename_buf   = self.tabs[i].name.clone();
                        self.renaming_tab = Some(i);
                    }
                    if let Some(i) = lock_idx {
                        self.tabs[i].locked = true;
                        self.dirty = true;
                    }
                });
            });
    }

    // ── Status bar ────────────────────────────────────────────────────────────

    fn ui_statusbar(&mut self, ctx: &egui::Context) {
        let content = self.tabs.get(self.active_tab).map(|t| t.content.as_str()).unwrap_or("");
        let words   = content.split_whitespace().count();
        let chars   = content.chars().count();

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
                    ui.label(RichText::new("Ln ").size(11.0).monospace().color(dim));
                    ui.label(RichText::new((self.cursor_line + 1).to_string()).size(11.0).monospace().color(val));
                    ui.label(RichText::new("  Col ").size(11.0).monospace().color(dim));
                    ui.label(RichText::new((self.cursor_col + 1).to_string()).size(11.0).monospace().color(val));
                    ui.separator();
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
                        format!("{}/{}", self.search_idx + 1, self.search_results.len())
                    };
                    ui.label(RichText::new(count_str).size(11.0).color(Color32::from_rgb(136,136,160)));

                    if ui.button("^").on_hover_text("Previous (Shift+Enter)").clicked()
                        && !self.search_results.is_empty()
                    {
                        let len = self.search_results.len();
                        self.search_idx = (self.search_idx + len - 1) % len;
                    }
                    if ui.button("v").on_hover_text("Next (Enter)").clicked()
                        && !self.search_results.is_empty()
                    {
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

    fn ui_text_editor(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, now: f64) {
        if self.tabs.is_empty() { return; }

        // Locked tab overlay
        if self.tabs[self.active_tab].locked {
            let bg = if self.prefs.dark_mode { Color32::from_rgb(18,18,22) } else { Color32::from_rgb(240,240,235) };
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.add_space(ui.available_height() / 3.0);
                ui.label(RichText::new("🔒").size(40.0));
                ui.add_space(8.0);
                ui.label(RichText::new("This tab is locked").size(14.0)
                    .color(Color32::from_rgb(180, 150, 60)));
                ui.add_space(16.0);
                egui::Frame::none()
                    .fill(bg)
                    .rounding(8.0)
                    .inner_margin(egui::Margin::same(16.0))
                    .show(ui, |ui| {
                        ui.set_max_width(280.0);
                        let pw_resp = ui.add(
                            egui::TextEdit::singleline(&mut *self.tab_unlock_pw)
                                .password(true)
                                .desired_width(f32::INFINITY)
                                .font(FontId::monospace(13.0))
                                .hint_text("Master password to unlock…")
                        );
                        if !self.tab_unlock_error.is_empty() {
                            ui.add_space(4.0);
                            ui.label(RichText::new(&self.tab_unlock_error.clone())
                                .size(11.0).color(Color32::from_rgb(224, 92, 92)));
                        }
                        ui.add_space(8.0);
                        let unlock_clicked = ui.add(
                            egui::Button::new(RichText::new("Unlock").size(13.0).strong())
                                .fill(Color32::from_rgb(124, 106, 247))
                                .min_size(Vec2::new(ui.available_width(), 32.0))
                        ).clicked();
                        let enter_pressed = pw_resp.lost_focus()
                            && ctx.input(|i| i.key_pressed(EKey::Enter));
                        if unlock_clicked || enter_pressed {
                            let idx = self.active_tab;
                            self.try_unlock_tab(idx, ctx);
                        }
                        if self.tab_unlock_pw.is_empty() && self.tab_unlock_error.is_empty() {
                            pw_resp.request_focus();
                        }
                    });
            });
            return;
        }

        let available = ui.available_size();

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
                if output.response.changed() {
                    self.dirty = true;
                    self.last_edit_time = now;
                }

                if let Some(cursor) = output.cursor_range {
                    let text     = self.tabs[self.active_tab].content.as_str();
                    let char_idx = cursor.primary.ccursor.index;
                    let byte_idx = text.char_indices().nth(char_idx)
                        .map(|(b, _)| b)
                        .unwrap_or(text.len());
                    let before = &text[..byte_idx];
                    self.cursor_line = before.chars().filter(|&c| c == '\n').count();
                    self.cursor_col  = before.rfind('\n')
                        .map(|p| before[p + 1..].chars().count())
                        .unwrap_or_else(|| before.chars().count());
                }
            });
    }

    // ── Preferences panel ─────────────────────────────────────────────────────
    //
    // The panel is a floating Area anchored to the top-right corner.
    //
    // Width accounting:
    //   panel_w = 300  (total Area width)
    //   inner_margin = 20 px each side = 40 px total
    //   content_w = panel_w - 40 = 260  ← what we pass to set_min/max_width
    //
    // This ensures the Frame fits exactly within panel_w so that right-aligned
    // widgets (checkboxes) are not clipped off-screen.

    fn ui_prefs(&mut self, ctx: &egui::Context) {
        let panel_w    = 300.0_f32;
        let content_w  = panel_w - 40.0;      // account for inner_margin × 2
        let screen_w   = ctx.screen_rect().width();
        let bg         = if self.prefs.dark_mode { Color32::from_rgb(24,24,28)  } else { Color32::from_rgb(235,235,230) };
        let border     = if self.prefs.dark_mode { Color32::from_rgb(46,46,56)  } else { Color32::from_rgb(204,204,196) };
        let lc         = Color32::from_rgb(136, 136, 160);

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
                        ui.set_min_width(content_w);
                        ui.set_max_width(content_w);

                        ui.label(RichText::new("Preferences").size(13.0).strong());
                        ui.add_space(4.0);

                        egui::ScrollArea::vertical()
                            .max_height(ctx.screen_rect().height() - 120.0)
                            .show(ui, |ui| {
                                ui.add_space(8.0);

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

                                // Auto-lock on idle
                                ui.label(RichText::new("AUTO-LOCK").size(10.0).strong().color(lc));
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Lock on idle").size(11.0));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let prev = self.prefs.auto_lock;
                                        ui.checkbox(&mut self.prefs.auto_lock, "");
                                        if self.prefs.auto_lock != prev { self.persist_prefs(); }
                                    });
                                });
                                if self.prefs.auto_lock {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("After (min)").size(11.0));
                                        let prev = self.prefs.auto_lock_delay;
                                        ui.add(egui::Slider::new(&mut self.prefs.auto_lock_delay, 1.0..=60.0).integer());
                                        if (self.prefs.auto_lock_delay - prev).abs() > f64::EPSILON { self.persist_prefs(); }
                                    });
                                }

                                ui.add(egui::Separator::default());

                                // Clipboard auto-clear
                                ui.label(RichText::new("CLIPBOARD").size(10.0).strong().color(lc));
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Clear after copy").size(11.0));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let prev = self.prefs.clipboard_clear;
                                        ui.checkbox(&mut self.prefs.clipboard_clear, "");
                                        if self.prefs.clipboard_clear != prev { self.persist_prefs(); }
                                    });
                                });
                                if self.prefs.clipboard_clear {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("After (s)").size(11.0));
                                        let prev = self.prefs.clipboard_clear_delay;
                                        ui.add(egui::Slider::new(&mut self.prefs.clipboard_clear_delay, 10.0..=120.0).integer());
                                        if (self.prefs.clipboard_clear_delay - prev).abs() > f64::EPSILON { self.persist_prefs(); }
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
                                    self.cp_current.zeroize(); self.cp_new.zeroize();
                                    self.cp_confirm.zeroize(); self.cp_error.clear();
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
                            }); // close ScrollArea
                    });
            });

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
                        ui.add(egui::TextEdit::singleline(&mut *self.cp_current)
                            .password(true).desired_width(f32::INFINITY).hint_text("Current password…"));
                        ui.add_space(8.0);
                        ui.label(RichText::new("NEW PASSWORD").size(10.0).strong().color(lc));
                        ui.add(egui::TextEdit::singleline(&mut *self.cp_new)
                            .password(true).desired_width(f32::INFINITY).hint_text("New password…"));
                        ui.add_space(8.0);
                        ui.label(RichText::new("CONFIRM NEW").size(10.0).strong().color(lc));
                        ui.add(egui::TextEdit::singleline(&mut *self.cp_confirm)
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
