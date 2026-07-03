use crate::crypto::{hash_password, verify_password};
use crate::markdown;
use crate::vim::{self, Mode, Vim};
use crate::storage::{
    Prefs, Tab,
    default_tabs, load_config, load_prefs, load_tabs,
    save_config, save_prefs, save_tabs,
};
use eframe::egui::{self, Color32, FontFamily, FontId, Key as EKey, Modifiers, RichText, Stroke, Vec2};
use rfd::FileDialog;
use std::{collections::HashMap, fs, path::PathBuf};
use zeroize::{Zeroize, Zeroizing};

// ─── Constants ────────────────────────────────────────────────────────────────

pub const MAX_TABS:      usize = 5;
const MAX_UNDO:          usize = 50;
const UNDO_INTERVAL:     f64   = 1.5;

// Editor font-size bounds (must match the Preferences slider range).
const FONT_MIN:     f32 = 10.0;
const FONT_MAX:     f32 = 28.0;
const FONT_DEFAULT: f32 = 14.0;

/// Selectable editor fonts: (display name, registered egui family key).
/// An empty family key falls back to the built-in monospace family.
/// Embedded fonts are registered in `setup_fonts` under the same key.
const EDITOR_FONTS: &[(&str, &str)] = &[
    ("Monospace",      ""),
    ("JetBrains Mono", "JetBrains Mono"),
    ("Hack",           "Hack"),
];

/// Convert a character index into a byte offset within `s`.
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

// ─── Material Icons font ──────────────────────────────────────────────────────

static MATERIAL_ICONS: &[u8] = include_bytes!("../assets/MaterialIcons-Regular.ttf");

// Embedded selectable editor fonts (registered as named families in setup_fonts).
static JETBRAINS_MONO: &[u8] = include_bytes!("../assets/JetBrainsMono-Regular.ttf");
static HACK_MONO:      &[u8] = include_bytes!("../assets/Hack-Regular.ttf");

// Codepoints from the Material Icons Regular font.
const ICON_SAVE:      &str = "\u{e161}"; // save
const ICON_UNDO:      &str = "\u{e166}"; // undo
const ICON_REDO:      &str = "\u{e15a}"; // redo
const ICON_EXPORT:    &str = "\u{e2c3}"; // file_upload
const ICON_IMPORT:    &str = "\u{e2c4}"; // file_download
const ICON_LOCK:      &str = "\u{e897}"; // lock
const ICON_LOCK_OPEN: &str = "\u{e898}"; // lock_open
const ICON_SEARCH:    &str = "\u{e8b6}"; // search
const ICON_CLOSE:     &str = "\u{e5cd}"; // close
const ICON_PREV:      &str = "\u{e5c7}"; // keyboard_arrow_up
const ICON_NEXT:      &str = "\u{e5c5}"; // keyboard_arrow_down
const ICON_ADD:       &str = "\u{e145}"; // add  (expand replace row)
const ICON_REMOVE:    &str = "\u{e15b}"; // remove (collapse replace row)
const ICON_CHEVRON_L: &str = "\u{e5c4}"; // arrow_back    (tab scroll left)
const ICON_CHEVRON_R: &str = "\u{e5c8}"; // arrow_forward (tab scroll right)

/// Load the Material Icons font into egui as a fallback for the Proportional
/// family. Called once at startup from main.rs.
pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "material_icons".to_owned(),
        egui::FontData::from_static(MATERIAL_ICONS),
    );
    // Append as fallback so icon codepoints render from this font while normal
    // text still uses the default Ubuntu-Light / Hack fonts.
    fonts.families
        .entry(FontFamily::Proportional)
        .or_default()
        .push("material_icons".to_owned());
    fonts.families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("material_icons".to_owned());

    // Register the embedded editor fonts as named families. Each keeps the
    // default monospace fallback chain (icons, emoji, broad Unicode) so glyphs
    // missing from the chosen font still render.
    fonts.font_data.insert("jetbrains_mono".to_owned(), egui::FontData::from_static(JETBRAINS_MONO));
    fonts.font_data.insert("hack_mono".to_owned(),      egui::FontData::from_static(HACK_MONO));

    let fallback = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();

    let mut jb = vec!["jetbrains_mono".to_owned()];
    jb.extend(fallback.clone());
    fonts.families.insert(FontFamily::Name("JetBrains Mono".into()), jb);

    let mut hk = vec!["hack_mono".to_owned()];
    hk.extend(fallback);
    fonts.families.insert(FontFamily::Name("Hack".into()), hk);

    ctx.set_fonts(fonts);
}

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
    search_open:        bool,
    search_needs_focus: bool,
    search_query:       String,
    replace_query:      String,
    replace_mode:       bool,
    search_regex:       bool,
    search_regex_error: bool,
    search_results:     Vec<(usize, usize)>,
    search_idx:         usize,

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

    // Tab bar scroll
    tab_scroll_offset: f32,
    tab_overflow:      bool,

    // Selection latched when the editor context menu opens, so right-click
    // doesn't lose the highlighted range the menu operates on.
    context_sel: Option<(usize, usize)>,

    // Vim mode state (only active when prefs.vim_mode is enabled).
    vim: Vim,

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
            search_needs_focus: false,
            search_query:       String::new(),
            replace_query:      String::new(),
            replace_mode:       false,
            search_regex:       false,
            search_regex_error: false,
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
            tab_scroll_offset:  0.0,
            tab_overflow:       false,
            context_sel:        None,
            vim:                Vim::default(),
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
        self.search_open        = false;
        self.search_needs_focus = false;
        self.search_results.clear();
        self.search_regex_error = false;
        self.tab_scroll_offset  = 0.0;
        self.tab_overflow       = false;
        self.prefs_open         = false;
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
        // Start in Vim Normal mode with the editor focused so keys are captured.
        self.vim.reset_to_normal();
        if self.prefs.vim_mode {
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("editor")));
        }
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
        self.search_regex_error = false;
        if self.search_query.is_empty() { return; }
        let content = self.tabs.get(self.active_tab)
            .map(|t| t.content.as_str())
            .unwrap_or("");
        if self.search_regex {
            match regex::Regex::new(&self.search_query) {
                Ok(re) => {
                    for m in re.find_iter(content) {
                        if m.start() < m.end() {
                            self.search_results.push((m.start(), m.end()));
                        }
                    }
                }
                Err(_) => { self.search_regex_error = true; return; }
            }
        } else {
            let q  = self.search_query.to_lowercase();
            let c  = content.to_lowercase();
            let qb = q.as_bytes();
            let cb = c.as_bytes();
            let mut i = 0;
            while i + qb.len() <= cb.len() {
                if cb[i..i + qb.len()] == *qb {
                    self.search_results.push((i, i + qb.len()));
                    i += qb.len();
                } else {
                    i += 1;
                }
            }
        }
        if self.search_idx >= self.search_results.len() {
            self.search_idx = 0;
        }
    }

    fn replace_current(&mut self) {
        let Some(&(start, end)) = self.search_results.get(self.search_idx) else { return };
        let r = self.replace_query.clone();
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.content.replace_range(start..end, &r);
            self.dirty = true;
        }
        self.run_search();
    }

    fn skip_match(&mut self) {
        if !self.search_results.is_empty() {
            self.search_idx = (self.search_idx + 1) % self.search_results.len();
        }
    }

    fn replace_all(&mut self) {
        if self.search_query.is_empty() { return; }
        let r = self.replace_query.clone();
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            if self.search_regex {
                if let Ok(re) = regex::Regex::new(&self.search_query) {
                    tab.content = re.replace_all(&tab.content, r.as_str()).into_owned();
                    self.dirty  = true;
                }
            } else {
                let q = self.search_query.clone();
                tab.content = tab.content.replace(&q, &r);
                self.dirty  = true;
            }
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

    // ── Font size ───────────────────────────────────────────────────────────────

    fn set_font_size(&mut self, size: f32) {
        let clamped = size.clamp(FONT_MIN, FONT_MAX).round();
        if (clamped - self.prefs.font_size).abs() > f32::EPSILON {
            self.prefs.font_size = clamped;
            self.persist_prefs();
        }
    }

    // ── Editor clipboard / selection (right-click menu) ──────────────────────────

    /// Current selection in the editor as a `(start, end)` char-index range,
    /// read from the persisted TextEdit state so it works even without focus.
    fn editor_selection(&self, ctx: &egui::Context, editor_id: egui::Id) -> Option<(usize, usize)> {
        egui::text_edit::TextEditState::load(ctx, editor_id)
            .and_then(|s| s.cursor.char_range())
            .map(|r| {
                let a = r.primary.index;
                let b = r.secondary.index;
                (a.min(b), a.max(b))
            })
    }

    fn editor_set_cursor(&self, ctx: &egui::Context, editor_id: egui::Id, range: egui::text::CCursorRange) {
        let mut state = egui::text_edit::TextEditState::load(ctx, editor_id).unwrap_or_default();
        state.cursor.set_char_range(Some(range));
        state.store(ctx, editor_id);
        ctx.memory_mut(|m| m.request_focus(editor_id));
    }

    fn editor_copy(&mut self, ctx: &egui::Context, sel: Option<(usize, usize)>) {
        if let Some((a, b)) = sel {
            if b > a {
                let s: String = self.tabs[self.active_tab].content.chars().skip(a).take(b - a).collect();
                ctx.output_mut(|o| o.copied_text = s);
            }
        }
    }

    fn editor_cut(&mut self, ctx: &egui::Context, editor_id: egui::Id, sel: Option<(usize, usize)>, now: f64) {
        if let Some((a, b)) = sel {
            if b > a {
                let content = &mut self.tabs[self.active_tab].content;
                let ba = char_to_byte(content, a);
                let bb = char_to_byte(content, b);
                let cut = content[ba..bb].to_string();
                content.replace_range(ba..bb, "");
                ctx.output_mut(|o| o.copied_text = cut);
                self.dirty = true;
                self.last_edit_time = now;
                self.editor_set_cursor(ctx, editor_id,
                    egui::text::CCursorRange::one(egui::text::CCursor::new(a)));
                self.context_sel = None;
            }
        }
    }

    fn editor_paste(&mut self, ctx: &egui::Context, editor_id: egui::Id, sel: Option<(usize, usize)>, now: f64) {
        let pasted = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok());
        let Some(text) = pasted else { return };
        if text.is_empty() { return; }
        let (a, b) = sel.unwrap_or_else(|| {
            let n = self.tabs[self.active_tab].content.chars().count();
            (n, n)
        });
        let content = &mut self.tabs[self.active_tab].content;
        let ba = char_to_byte(content, a);
        let bb = char_to_byte(content, b);
        content.replace_range(ba..bb, &text);
        self.dirty = true;
        self.last_edit_time = now;
        let new_pos = a + text.chars().count();
        self.editor_set_cursor(ctx, editor_id,
            egui::text::CCursorRange::one(egui::text::CCursor::new(new_pos)));
        self.context_sel = None;
    }

    fn editor_select_all(&mut self, ctx: &egui::Context, editor_id: egui::Id) {
        let n = self.tabs[self.active_tab].content.chars().count();
        self.editor_set_cursor(ctx, editor_id, egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(n),
        ));
    }

    /// `(can_undo, can_redo)` for the active tab.
    fn active_undo_state(&self) -> (bool, bool) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            let pos = *self.undo_positions.get(&tab.id).unwrap_or(&0);
            let len = self.undo_stacks.get(&tab.id).map(|s| s.len()).unwrap_or(0);
            (pos > 0, pos + 1 < len)
        } else {
            (false, false)
        }
    }

    /// Resolve the configured editor-font name to a registered egui font family.
    /// Unknown names fall back to the built-in monospace family.
    fn editor_font_family(&self) -> FontFamily {
        match EDITOR_FONTS.iter().find(|(name, _)| self.prefs.editor_font == *name) {
            Some((_, fam)) if !fam.is_empty() => FontFamily::Name((*fam).into()),
            _ => FontFamily::Monospace,
        }
    }

    // ── Vim mode ────────────────────────────────────────────────────────────────

    fn vim_cursor_index(&self, ctx: &egui::Context, editor_id: egui::Id) -> usize {
        egui::text_edit::TextEditState::load(ctx, editor_id)
            .and_then(|s| s.cursor.char_range())
            .map(|r| r.primary.index)
            .unwrap_or(0)
    }

    fn vim_set_cursor(&self, ctx: &egui::Context, editor_id: egui::Id, idx: usize) {
        self.editor_set_cursor(ctx, editor_id,
            egui::text::CCursorRange::one(egui::text::CCursor::new(idx)));
    }

    /// Drive Vim mode for this frame. Called before the TextEdit is shown, only
    /// when Vim is enabled and the editor holds keyboard focus.
    fn vim_step(&mut self, ctx: &egui::Context, editor_id: egui::Id, now: f64) {
        match self.vim.mode {
            Mode::Insert => {
                // Esc leaves Insert mode, nudging the cursor left one column (Vim).
                if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, EKey::Escape)) {
                    let idx  = self.vim_cursor_index(ctx, editor_id);
                    let text: Vec<char> = self.tabs[self.active_tab].content.chars().collect();
                    let ni = vim::left(&text, idx);
                    self.vim_set_cursor(ctx, editor_id, vim::clamp_normal(&text, ni));
                    // Snapshot the inserted text as one undo unit.
                    let id = self.tabs[self.active_tab].id;
                    self.push_undo_snap(id, self.tabs[self.active_tab].content.clone());
                    self.vim.reset_to_normal();
                }
            }
            Mode::Normal => self.vim_normal_input(ctx, editor_id, now),
        }
    }

    fn vim_normal_input(&mut self, ctx: &egui::Context, editor_id: egui::Id, now: f64) {
        // Ctrl-r → redo (Vim), before the generic key strip below.
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, EKey::R)) {
            self.do_redo(ctx);
        }

        // Collect this frame's typed characters and whether Escape was pressed.
        let mut typed: Vec<char> = Vec::new();
        let mut escape = false;
        ctx.input(|i| {
            for ev in &i.events {
                match ev {
                    egui::Event::Text(t) => typed.extend(t.chars()),
                    egui::Event::Key { key: EKey::Escape, pressed: true, .. } => escape = true,
                    _ => {}
                }
            }
        });
        if escape {
            self.vim.pending_g = false;
        }
        for c in typed {
            self.vim_normal_char(c, ctx, editor_id, now);
        }

        // Starve the TextEdit of key/text input so Normal-mode keys never insert.
        ctx.input_mut(|i| i.events.retain(|e|
            !matches!(e, egui::Event::Text(_) | egui::Event::Key { .. })));
    }

    fn vim_normal_char(&mut self, c: char, ctx: &egui::Context, editor_id: egui::Id, now: f64) {
        let text: Vec<char> = self.tabs[self.active_tab].content.chars().collect();
        let i = self.vim_cursor_index(ctx, editor_id).min(text.len());

        // Second key of a `g` sequence.
        if self.vim.pending_g {
            self.vim.pending_g = false;
            if c == 'g' {
                let ni = vim::clamp_normal(&text, vim::buffer_top(&text));
                self.vim_set_cursor(ctx, editor_id, ni);
                self.vim.want_col = None;
            }
            return;
        }

        // Pure cursor motions.
        let motion: Option<usize> = match c {
            'h' => Some(vim::left(&text, i)),
            'l' => Some(vim::right(&text, i)),
            '0' => Some(vim::line_start(&text, i)),
            '^' => Some(vim::first_non_blank(&text, i)),
            '$' => Some(vim::line_last(&text, i)),
            'w' => Some(vim::clamp_normal(&text, vim::word_forward(&text, i))),
            'b' => Some(vim::clamp_normal(&text, vim::word_backward(&text, i))),
            'e' => Some(vim::clamp_normal(&text, vim::word_end(&text, i))),
            'G' => Some(vim::clamp_normal(&text, vim::buffer_bottom(&text))),
            'j' => {
                let wc = self.vim.want_col.unwrap_or_else(|| vim::col(&text, i));
                self.vim.want_col = Some(wc);
                Some(vim::down(&text, i, wc))
            }
            'k' => {
                let wc = self.vim.want_col.unwrap_or_else(|| vim::col(&text, i));
                self.vim.want_col = Some(wc);
                Some(vim::up(&text, i, wc))
            }
            _ => None,
        };
        if let Some(ni) = motion {
            if !matches!(c, 'j' | 'k') {
                self.vim.want_col = None;
            }
            self.vim_set_cursor(ctx, editor_id, ni);
            return;
        }

        match c {
            'g' => self.vim.pending_g = true,
            'u' => self.do_undo(ctx),

            // Enter Insert mode.
            'i' => self.vim.mode = Mode::Insert,
            'a' => {
                let ni = (i + 1).min(vim::line_end(&text, i));
                self.vim_set_cursor(ctx, editor_id, ni);
                self.vim.mode = Mode::Insert;
            }
            'I' => {
                let ni = vim::first_non_blank(&text, i);
                self.vim_set_cursor(ctx, editor_id, ni);
                self.vim.mode = Mode::Insert;
            }
            'A' => {
                let ni = vim::line_end(&text, i);
                self.vim_set_cursor(ctx, editor_id, ni);
                self.vim.mode = Mode::Insert;
            }

            // Editing commands (each a single undo step).
            'x' => {
                let last = vim::line_last(&text, i);
                if i < text.len() && text[i] != '\n' {
                    let id = self.tabs[self.active_tab].id;
                    self.push_undo_snap(id, self.tabs[self.active_tab].content.clone());
                    let content = &mut self.tabs[self.active_tab].content;
                    let ba = char_to_byte(content, i);
                    let bb = char_to_byte(content, i + 1);
                    content.replace_range(ba..bb, "");
                    self.push_undo_snap(id, self.tabs[self.active_tab].content.clone());
                    self.dirty = true;
                    self.last_edit_time = now;
                    let text2: Vec<char> = self.tabs[self.active_tab].content.chars().collect();
                    self.vim_set_cursor(ctx, editor_id, vim::clamp_normal(&text2, i.min(last)));
                }
            }
            'o' => self.vim_open_line(ctx, editor_id, now, true),
            'O' => self.vim_open_line(ctx, editor_id, now, false),
            _ => {}
        }
    }

    /// `o` / `O`: open a new line below/above and enter Insert mode.
    fn vim_open_line(&mut self, ctx: &egui::Context, editor_id: egui::Id, now: f64, below: bool) {
        let text: Vec<char> = self.tabs[self.active_tab].content.chars().collect();
        let i = self.vim_cursor_index(ctx, editor_id).min(text.len());
        let insert_at = if below { vim::line_end(&text, i) } else { vim::line_start(&text, i) };

        let id = self.tabs[self.active_tab].id;
        self.push_undo_snap(id, self.tabs[self.active_tab].content.clone());
        let content = &mut self.tabs[self.active_tab].content;
        let byte = char_to_byte(content, insert_at);
        content.insert(byte, '\n');
        self.dirty = true;
        self.last_edit_time = now;

        let cursor = if below { insert_at + 1 } else { insert_at };
        self.vim_set_cursor(ctx, editor_id, cursor);
        self.vim.mode = Mode::Insert;
        self.vim.want_col = None;
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
        if self.screen == Screen::Editor && self.prefs.auto_lock && self.last_activity_time > 0.0 {
            let elapsed  = now - self.last_activity_time;
            let timeout  = self.prefs.auto_lock_delay * 60.0;
            if elapsed >= timeout {
                self.save_now(ctx);
                self.lock();
            } else {
                // Schedule a repaint at the exact moment the lock should fire so
                // update() runs even when the window is minimized or otherwise idle.
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(timeout - elapsed));
            }
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

        // Markdown live-preview pane (hidden for locked tabs).
        let show_preview = self.prefs.preview_open
            && self.tabs.get(self.active_tab).map(|t| !t.locked).unwrap_or(false);
        if show_preview {
            egui::SidePanel::right("md_preview")
                .resizable(true)
                .min_width(180.0)
                .default_width((ctx.screen_rect().width() * 0.42).max(220.0))
                .show(ctx, |ui| {
                    self.ui_markdown_preview(ui);
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_text_editor(ui, ctx, now);
        });
        if self.prefs_open { self.ui_prefs(ctx); }
        self.ui_modals(ctx);
    }

    fn ui_markdown_preview(&mut self, ui: &mut egui::Ui) {
        let dim = Color32::from_rgb(136, 136, 160);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("PREVIEW").size(10.0).strong().color(dim));
        });
        ui.add_space(4.0);
        ui.separator();
        let content = self.tabs.get(self.active_tab).map(|t| t.content.clone()).unwrap_or_default();
        let base    = self.prefs.font_size;
        let dark    = self.prefs.dark_mode;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(4.0);
                markdown::render(ui, &content, base, dark);
            });
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let ctrl = ctx.input(|i| i.modifiers.ctrl || i.modifiers.mac_cmd);

        // Ctrl + mouse wheel adjusts the editor font size.
        let wheel: f32 = ctx.input(|i| {
            i.events.iter().filter_map(|ev| {
                if let egui::Event::MouseWheel { delta, modifiers, .. } = ev {
                    if modifiers.ctrl || modifiers.mac_cmd || modifiers.command {
                        return Some(delta.y);
                    }
                }
                None
            }).sum()
        });
        if wheel != 0.0 {
            let step = if wheel > 0.0 { 1.0 } else { -1.0 };
            self.set_font_size(self.prefs.font_size + step);
        }

        if ctrl && ctx.input(|i| i.key_pressed(EKey::S)) { self.save_now(ctx); }

        if ctrl && ctx.input(|i| i.key_pressed(EKey::Comma)) {
            self.prefs_open = !self.prefs_open;
            if self.prefs_open { self.prefs_just_opened = true; }
            else { self.persist_prefs(); }
        }

        if ctrl && ctx.input(|i| i.key_pressed(EKey::F)) {
            self.search_open = true; self.replace_mode = false; self.search_needs_focus = true;
        }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::H)) {
            self.search_open = true; self.replace_mode = true; self.search_needs_focus = true;
        }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::T)) { if self.tabs.len() < MAX_TABS { self.add_tab(); } }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::L)) { self.save_now(ctx); self.lock(); }

        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, EKey::Z)) { self.do_undo(ctx); }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, EKey::Y)) { self.do_redo(ctx); }

        if ctrl && ctx.input(|i| i.key_pressed(EKey::E)) { self.do_export_tab(ctx); }
        if ctrl && ctx.input(|i| i.key_pressed(EKey::I)) { self.do_import_tab(ctx); }

        if (self.search_open || self.prefs_open)
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, EKey::Escape))
        {
            if self.search_open {
                self.search_open = false;
            } else {
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
                    ui.label(RichText::new("securenotes").monospace().size(13.0).strong().color(accent));
                    ui.separator();

                    if ui.button(RichText::new(format!("{ICON_SAVE} Save")).size(12.0))
                        .on_hover_text("Save (Ctrl+S)").clicked()
                    { self.save_now(ctx); }

                    let (can_undo, can_redo) = self.active_undo_state();
                    if ui.add_enabled(can_undo,
                        egui::Button::new(RichText::new(ICON_UNDO).size(15.0)))
                        .on_hover_text("Undo (Ctrl+Z)").clicked()
                    { self.do_undo(ctx); }
                    if ui.add_enabled(can_redo,
                        egui::Button::new(RichText::new(ICON_REDO).size(15.0)))
                        .on_hover_text("Redo (Ctrl+Y)").clicked()
                    { self.do_redo(ctx); }

                    ui.separator();

                    if ui.button(RichText::new(format!("{ICON_EXPORT} Export")).size(12.0))
                        .on_hover_text("Export current tab as .txt (Ctrl+E)").clicked()
                    { self.do_export_tab(ctx); }

                    if ui.button(RichText::new(format!("{ICON_IMPORT} Import")).size(12.0))
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
                    ui.add(
                        egui::Button::new(
                            RichText::new(status_text).size(12.0).strong().monospace().color(status_color)
                        )
                        .fill(status_bg)
                        .min_size(Vec2::new(0.0, 28.0))
                        .sense(egui::Sense::hover())
                    ).on_hover_text(if self.dirty { "Unsaved changes" } else { "All saved" });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(RichText::new(format!("{ICON_LOCK} Lock")).size(12.0))
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

                        if ui.button(RichText::new(format!("{ICON_SEARCH} Find")).size(12.0))
                            .on_hover_text("Find / Replace (Ctrl+F / Ctrl+H)").clicked()
                        { self.search_open = true; self.replace_mode = false; self.search_needs_focus = true; }

                        let pvc = if self.prefs.preview_open { accent } else { ui.visuals().text_color() };
                        if ui.button(RichText::new("Preview").size(12.0).color(pvc))
                            .on_hover_text("Toggle Markdown preview").clicked()
                        {
                            self.prefs.preview_open = !self.prefs.preview_open;
                            self.persist_prefs();
                        }
                    });
                });
            });
    }

    // ── Tab bar ───────────────────────────────────────────────────────────────

    fn ui_tabbar(&mut self, ctx: &egui::Context) {
        let accent       = Color32::from_rgb(124, 106, 247);
        let locked_color = Color32::from_rgb(200, 160, 60);
        let dim          = Color32::from_rgb(80, 80, 100);
        let bg     = if self.prefs.dark_mode { Color32::from_rgb(22,22,26)  } else { Color32::from_rgb(228,228,223) };
        let border = if self.prefs.dark_mode { Color32::from_rgb(46,46,56)  } else { Color32::from_rgb(204,204,196) };

        egui::TopBottomPanel::top("tabbar")
            .exact_height(36.0)
            .frame(egui::Frame::none().fill(bg).stroke(Stroke::new(1.0, border)))
            .show(ctx, |ui| {
                const TAB_NAME_MAX: usize = 11;
                const TAB_NAME_W:   f32   = 76.0;
                const TAB_LOCK_W:   f32   = 22.0;
                const TAB_CLOSE_W:  f32   = 18.0;
                const ARROW_W:      f32   = 28.0;

                let mut switch_to   = None;
                let mut close_idx   = None;
                let mut rename_idx  = None;
                let mut lock_idx    = None;
                let mut export_idx  = None;
                let mut new_tab_req = false;
                let mut import_req  = false;

                let snapshot: Vec<(usize, String, bool, bool)> = self.tabs.iter().enumerate()
                    .map(|(i, t)| (i, t.name.clone(), i == self.active_tab, t.locked))
                    .collect();

                // Show arrows whenever there are 2+ tabs so layout is always stable.
                let show_arrows = self.tabs.len() >= 2;

                // Pre-compute the exact width we can give the scroll area so that the
                // right arrow and + button always fit.  All measurements are approximate
                // (item_spacing varies) but erring on the side of slightly smaller
                // prevents the right-side buttons from being pushed off-screen.
                let total_avail = ui.available_width();
                let arrow_budget = if show_arrows { ARROW_W * 2.0 + 16.0 } else { 0.0 };
                let right_budget = 60.0; // separator (~10) + + button (32) + spacing (~18)
                let scroll_w = (total_avail - arrow_budget - right_budget).max(50.0);

                let text_color = ui.visuals().text_color();

                ui.horizontal(|ui| {
                    // ── Left scroll arrow ─────────────────────────────────────
                    if show_arrows {
                        let can_go = self.tab_scroll_offset > 0.0;
                        let icon_color = if can_go { text_color } else { dim };
                        let resp = ui.add(
                            egui::Button::new(RichText::new(ICON_CHEVRON_L).size(16.0).color(icon_color))
                                .frame(false)
                                .min_size(Vec2::new(ARROW_W, 28.0))
                        ).on_hover_text("Scroll tabs left");
                        if resp.clicked() && can_go {
                            self.tab_scroll_offset = (self.tab_scroll_offset - 130.0).max(0.0);
                        }
                    }

                    // ── Scrollable tab strip ──────────────────────────────────
                    let scroll_out = egui::ScrollArea::horizontal()
                        .id_source("tabbar_scroll")
                        .auto_shrink([false, false])
                        .max_width(scroll_w)
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .scroll_offset(egui::Vec2::new(self.tab_scroll_offset, 0.0))
                        .show(ui, |ui| {
                            ui.set_min_width(scroll_w);
                            ui.horizontal(|ui| {
                                ui.add_space(12.0);

                                for (i, name, is_active, is_locked) in &snapshot {
                                    let i         = *i;
                                    let is_active = *is_active;
                                    let is_locked = *is_locked;

                                    // Inline rename input
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

                                    let truncated: String = if name.chars().count() > TAB_NAME_MAX {
                                        format!("{}…", name.chars().take(TAB_NAME_MAX - 1).collect::<String>())
                                    } else {
                                        name.clone()
                                    };

                                    // ── Lock icon ─────────────────────────────
                                    let lock_icon  = if is_locked { ICON_LOCK } else { ICON_LOCK_OPEN };
                                    let lock_color = if is_locked { locked_color } else { dim };
                                    let lock_btn = ui.add(
                                        egui::Button::new(RichText::new(lock_icon).size(11.0).color(lock_color))
                                            .frame(false)
                                            .min_size(Vec2::new(TAB_LOCK_W, 28.0))
                                    ).on_hover_text(if is_locked { "Click to unlock tab" } else { "Click to lock tab" });
                                    if lock_btn.clicked() {
                                        if is_locked { switch_to = Some(i); } else { lock_idx = Some(i); }
                                    }

                                    // ── Tab name ──────────────────────────────
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
                                            }).inner
                                    } else {
                                        ui.add(egui::Button::new(text).frame(false)
                                            .min_size(Vec2::new(TAB_NAME_W, 28.0)))
                                    };
                                    if resp.clicked()        { switch_to  = Some(i); }
                                    if resp.double_clicked() && !is_locked { rename_idx = Some(i); }

                                    // Tab-header right-click menu (mirrors the toolbar actions).
                                    resp.context_menu(|ui| {
                                        ui.add_enabled_ui(!is_locked, |ui| {
                                            if ui.button("Rename").clicked() { rename_idx = Some(i); ui.close_menu(); }
                                        });
                                        if is_locked {
                                            if ui.button("Unlock…").clicked() { switch_to = Some(i); ui.close_menu(); }
                                        } else if ui.button("Lock").clicked() {
                                            lock_idx = Some(i); ui.close_menu();
                                        }
                                        if snapshot.len() > 1 && ui.button("Close").clicked() {
                                            close_idx = Some(i); ui.close_menu();
                                        }
                                        ui.separator();
                                        if ui.button("New tab").clicked() { new_tab_req = true; ui.close_menu(); }
                                        ui.add_enabled_ui(!is_locked, |ui| {
                                            if ui.button("Export…").clicked() { export_idx = Some(i); ui.close_menu(); }
                                        });
                                        if ui.button("Import…").clicked() { import_req = true; ui.close_menu(); }
                                    });

                                    // ── Close button ──────────────────────────
                                    if !is_locked {
                                        let hovered = resp.hovered() || lock_btn.hovered();
                                        let close_color = if hovered || is_active {
                                            Color32::from_rgb(136, 136, 160)
                                        } else {
                                            Color32::from_rgba_unmultiplied(136, 136, 160, 60)
                                        };
                                        if ui.add(
                                            egui::Button::new(
                                                RichText::new(ICON_CLOSE).size(14.0).color(close_color)
                                            ).frame(false).min_size(Vec2::new(TAB_CLOSE_W, 28.0))
                                        ).clicked() { close_idx = Some(i); }
                                    }

                                    if i + 1 < self.tabs.len() { ui.separator(); }
                                }
                            });
                        });

                    // Sync scroll state from this frame's output.
                    self.tab_scroll_offset = scroll_out.state.offset.x;
                    self.tab_overflow = scroll_out.content_size.x > scroll_out.inner_rect.width() + 1.0;

                    let max_scroll = (scroll_out.content_size.x
                        - scroll_out.inner_rect.width()).max(0.0);

                    // Mouse wheel scrolls the tab strip (up → left, down → right).
                    if ui.rect_contains_pointer(ui.max_rect()) {
                        let delta = ctx.input(|i| i.smooth_scroll_delta.y);
                        if delta != 0.0 {
                            self.tab_scroll_offset =
                                (self.tab_scroll_offset - delta).clamp(0.0, max_scroll);
                        }
                    }

                    // ── Right scroll arrow ───────────────────────────────────
                    if show_arrows {
                        let can_go = self.tab_scroll_offset < max_scroll - 1.0;
                        let icon_color = if can_go { text_color } else { dim };
                        let resp = ui.add(
                            egui::Button::new(RichText::new(ICON_CHEVRON_R).size(16.0).color(icon_color))
                                .frame(false)
                                .min_size(Vec2::new(ARROW_W, 28.0))
                        ).on_hover_text("Scroll tabs right");
                        if resp.clicked() && can_go {
                            self.tab_scroll_offset += 130.0;
                        }
                    }

                    // ── + new tab button (pinned right of arrows) ─────────────
                    ui.separator();
                    let can_add   = self.tabs.len() < MAX_TABS;
                    let add_color = if can_add { text_color } else { Color32::from_rgb(80,80,90) };
                    if ui.add_enabled(can_add,
                        egui::Button::new(RichText::new("+").size(18.0).color(add_color))
                            .frame(false).min_size(Vec2::new(32.0, 28.0))
                    ).on_hover_text("New tab (Ctrl+T)").clicked() { self.add_tab(); }
                });

                if let Some(i) = switch_to  { self.active_tab = i; }
                if let Some(i) = close_idx  {
                    if self.tabs.len() > 1 { self.modal = Modal::CloseTab(i); }
                }
                if let Some(i) = rename_idx {
                    self.rename_buf   = self.tabs[i].name.clone();
                    self.renaming_tab = Some(i);
                }
                if let Some(i) = lock_idx {
                    self.tabs[i].locked = true;
                    self.dirty = true;
                }
                if new_tab_req && self.tabs.len() < MAX_TABS { self.add_tab(); }
                if import_req { self.do_import_tab(ctx); }
                if let Some(i) = export_idx {
                    if i < self.tabs.len() {
                        self.active_tab = i;
                        self.do_export_tab(ctx);
                    }
                }
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
                    if self.prefs.vim_mode {
                        let mc = match self.vim.mode {
                            Mode::Normal => Color32::from_rgb(124, 106, 247),
                            Mode::Insert => Color32::from_rgb(92, 224, 138),
                        };
                        ui.label(RichText::new(self.vim.mode.label())
                            .size(11.0).strong().monospace().color(mc));
                        ui.separator();
                    }
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
                    ui.separator();
                    ui.label(RichText::new("Font ").size(11.0).monospace().color(dim));
                    ui.label(RichText::new(format!("{}px", self.prefs.font_size as u32))
                        .size(11.0).monospace().color(val));
                    if (self.prefs.font_size - FONT_DEFAULT).abs() > f32::EPSILON {
                        if ui.add(egui::Button::new(
                                RichText::new("reset").size(10.0).monospace()
                                    .color(Color32::from_rgb(124, 106, 247)))
                                .frame(false))
                            .on_hover_text("Reset font size to default (Ctrl+scroll to change)")
                            .clicked()
                        {
                            self.set_font_size(FONT_DEFAULT);
                        }
                    }

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
        let height = if self.replace_mode { 68.0 } else { 38.0 };
        let bg     = if self.prefs.dark_mode { Color32::from_rgb(28,28,34)  } else { Color32::from_rgb(235,235,230) };
        let border = if self.prefs.dark_mode { Color32::from_rgb(46,46,56)  } else { Color32::from_rgb(204,204,196) };
        let dim    = Color32::from_rgb(136, 136, 160);
        let accent = Color32::from_rgb(124, 106, 247);
        let error  = Color32::from_rgb(224, 80, 80);

        egui::TopBottomPanel::top("search_bar")
            .exact_height(height)
            .frame(egui::Frame::none()
                .fill(bg)
                .stroke(Stroke::new(1.0, border))
                .inner_margin(egui::Margin::symmetric(10.0, 4.0)))
            .show(ctx, |ui| {
                egui::Grid::new("search_grid")
                    .num_columns(2)
                    .spacing([6.0, 4.0])
                    .show(ui, |ui| {
                        // ── Row 1: Find ───────────────────────────────────────
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.set_min_width(60.0);
                            ui.label(RichText::new("Find:").size(12.0).color(dim));
                        });
                        ui.horizontal(|ui| {
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.search_query)
                                    .id(egui::Id::new("search_input"))
                                    .desired_width(200.0)
                                    .font(FontId::monospace(12.0))
                                    .hint_text("search…")
                            );
                            if self.search_needs_focus {
                                resp.request_focus();
                                self.search_needs_focus = false;
                            }
                            if resp.changed() { self.run_search(); }

                            // Regex toggle — highlighted when active, red when pattern is invalid
                            let rx_color = if self.search_regex_error {
                                error
                            } else if self.search_regex {
                                accent
                            } else {
                                dim
                            };
                            let rx_btn = ui.add(
                                egui::Button::new(RichText::new(".*").size(11.0).monospace().color(rx_color))
                                    .min_size(Vec2::new(24.0, 18.0))
                            ).on_hover_text("Toggle regex mode");
                            if rx_btn.clicked() {
                                self.search_regex = !self.search_regex;
                                self.run_search();
                            }

                            let count_str = if self.search_regex_error {
                                "bad pattern".to_string()
                            } else if self.search_results.is_empty() {
                                if self.search_query.is_empty() { String::new() } else { "no match".to_string() }
                            } else {
                                format!("{}/{}", self.search_idx + 1, self.search_results.len())
                            };
                            let count_color = if self.search_regex_error { error } else { dim };
                            if !count_str.is_empty() {
                                ui.label(RichText::new(&count_str).size(11.0).color(count_color));
                            }

                            if ui.add(egui::Button::new(
                                    RichText::new(ICON_PREV).size(16.0)
                                ).min_size(Vec2::new(20.0, 18.0)))
                                .on_hover_text("Previous").clicked()
                                && !self.search_results.is_empty()
                            {
                                let len = self.search_results.len();
                                self.search_idx = (self.search_idx + len - 1) % len;
                            }
                            if ui.add(egui::Button::new(
                                    RichText::new(ICON_NEXT).size(16.0)
                                ).min_size(Vec2::new(20.0, 18.0)))
                                .on_hover_text("Next").clicked()
                                && !self.search_results.is_empty()
                            {
                                self.search_idx = (self.search_idx + 1) % self.search_results.len();
                            }

                            ui.separator();
                            let (rep_icon, rep_tip) = if self.replace_mode {
                                (ICON_REMOVE, "Hide Replace")
                            } else {
                                (ICON_ADD, "Show Replace")
                            };
                            if ui.add(egui::Button::new(
                                    RichText::new(format!("{rep_icon} Replace")).size(12.0)
                                ).min_size(Vec2::new(0.0, 18.0)))
                                .on_hover_text(rep_tip).clicked()
                            {
                                self.replace_mode = !self.replace_mode;
                            }
                            if ui.add(egui::Button::new(
                                    RichText::new(ICON_CLOSE).size(16.0)
                                ).min_size(Vec2::new(20.0, 18.0)))
                                .on_hover_text("Close (Escape)").clicked()
                            {
                                self.search_open = false;
                            }
                        });
                        ui.end_row();

                        // ── Row 2: Replace (only in replace mode) ─────────────
                        if self.replace_mode {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.set_min_width(60.0);
                                ui.label(RichText::new("Replace:").size(12.0).color(dim));
                            });
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.replace_query)
                                        .desired_width(200.0)
                                        .font(FontId::monospace(12.0))
                                        .hint_text("replacement…")
                                );
                                let has_match = !self.search_results.is_empty();
                                if ui.add_enabled(has_match, egui::Button::new(
                                    RichText::new("Replace").size(11.0)
                                )).on_hover_text("Replace current match").clicked() {
                                    self.replace_current();
                                }
                                if ui.small_button("Replace All").on_hover_text("Replace all matches").clicked() {
                                    self.replace_all();
                                }
                                if ui.add_enabled(has_match, egui::Button::new(
                                    RichText::new("Skip").size(11.0)
                                )).on_hover_text("Skip to next match").clicked() {
                                    self.skip_match();
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
    }

    fn ui_text_editor(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, now: f64) {
        if self.tabs.is_empty() { return; }

        // Locked tab overlay
        if self.tabs[self.active_tab].locked {
            let bg = if self.prefs.dark_mode { Color32::from_rgb(18,18,22) } else { Color32::from_rgb(240,240,235) };
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.add_space(ui.available_height() / 3.0);
                ui.label(RichText::new(ICON_LOCK).size(40.0));
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
        // The editor's visible viewport (below the tab bar, above the status bar).
        // Captured here at the panel level because a ScrollArea's clip rect is only
        // tight when its content overflows — when the content fits, it extends past
        // the viewport and must not be used to bound the line-number gutter.
        let viewport = ui.available_rect_before_wrap();

        ui.visuals_mut().selection.bg_fill =
            Color32::from_rgba_unmultiplied(124, 106, 247, 180);
        ui.visuals_mut().selection.stroke =
            Stroke::new(1.0, Color32::from_rgb(180, 170, 255));

        let editor_id = egui::Id::new("editor");

        // Vim: drive the mode machine and (in Normal mode) strip key events
        // *before* the TextEdit runs, so keystrokes navigate instead of inserting.
        // Gated on editor focus (so other text fields are untouched) and on no
        // modal being open.
        let editor_focused = ctx.memory(|m| m.has_focus(editor_id));
        let vim_engaged = self.prefs.vim_mode && editor_focused && self.modal == Modal::None;
        if vim_engaged {
            self.vim_step(ctx, editor_id, now);
        }
        let vim_normal_active = vim_engaged && self.vim.mode == Mode::Normal;

        let font_size   = self.prefs.font_size;
        let word_wrap   = self.prefs.word_wrap;
        let line_nums   = self.prefs.line_numbers;
        let rel_nums    = self.prefs.relative_numbers && line_nums;
        let dark        = self.prefs.dark_mode;
        let font_id     = FontId::new(font_size, self.editor_font_family());

        // Gutter width sized to the digit count of the highest line number.
        let total_lines = self.tabs[self.active_tab].content
            .bytes().filter(|&b| b == b'\n').count() + 1;
        let digits   = total_lines.to_string().len().max(2);
        let gutter_w = if line_nums { font_size * 0.6 * digits as f32 + 18.0 } else { 0.0 };

        let gutter_bg = if dark { Color32::from_rgb(28,28,34) } else { Color32::from_rgb(228,228,223) };
        let divider   = if dark { Color32::from_rgb(46,46,56) } else { Color32::from_rgb(204,204,196) };
        let num_color = Color32::from_rgb(110, 110, 130);
        let cur_color = Color32::from_rgb(124, 106, 247);

        let scroll = if word_wrap {
            egui::ScrollArea::vertical()
        } else {
            egui::ScrollArea::both()
        };

        scroll
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let fid = font_id.clone();
                let mut layouter = move |ui: &egui::Ui, text: &str, wrap_width: f32| {
                    let job = egui::text::LayoutJob::simple(
                        text.to_owned(),
                        fid.clone(),
                        ui.visuals().text_color(),
                        if word_wrap { wrap_width } else { f32::INFINITY },
                    );
                    ui.fonts(|f| f.layout_job(job))
                };

                let text_w = if word_wrap { (available.x - gutter_w).max(50.0) } else { f32::INFINITY };

                let output = ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if gutter_w > 0.0 { ui.add_space(gutter_w); }
                    egui::TextEdit::multiline(&mut self.tabs[self.active_tab].content)
                        .id(editor_id)
                        .font(font_id.clone())
                        .layouter(&mut layouter)
                        .desired_width(text_w)
                        .desired_rows(1)
                        .min_size(Vec2::new((available.x - gutter_w).max(50.0), available.y))
                        .frame(false)
                        .lock_focus(true)
                        .show(ui)
                }).inner;

                if output.response.changed() {
                    self.dirty = true;
                    self.last_edit_time = now;
                }

                // Track the live selection so the right-click menu can act on it.
                // egui collapses the selection the instant the right mouse button is
                // *pressed* (a frame before the menu opens), so we can't read it then.
                // Instead we continuously remember the most recent non-empty selection
                // and clear it only when the selection genuinely empties for a reason
                // other than that right-press. On the right-press we also restore the
                // visual highlight that egui just cleared.
                //
                // In Vim Normal mode, Vim owns the selection/cursor, so this tracker
                // is bypassed to avoid the two systems fighting.
                if vim_normal_active {
                    self.context_sel = None;
                } else {
                    let cur_sel = output.cursor_range.as_ref()
                        .map(|cr| {
                            let a = cr.primary.ccursor.index;
                            let b = cr.secondary.ccursor.index;
                            (a.min(b), a.max(b))
                        })
                        .filter(|&(a, b)| b > a)
                        .or_else(|| self.editor_selection(ctx, editor_id).filter(|&(a, b)| b > a));
                    let sec_pressed = ctx.input(|i| i.pointer.secondary_pressed());
                    match cur_sel {
                        Some(s)              => self.context_sel = Some(s),
                        None if !sec_pressed => self.context_sel = None,
                        None                 => {}
                    }
                    if sec_pressed && output.response.hovered() {
                        if let Some((a, b)) = self.context_sel {
                            self.editor_set_cursor(ctx, editor_id, egui::text::CCursorRange::two(
                                egui::text::CCursor::new(a),
                                egui::text::CCursor::new(b),
                            ));
                        }
                    }
                }

                // Right-click context menu for the editor.
                let menu_sel = self.context_sel;
                let has_sel  = menu_sel.map(|(a, b)| b > a).unwrap_or(false);
                output.response.context_menu(|ui| {
                    ui.add_enabled_ui(has_sel, |ui| {
                        if ui.button("Cut").clicked()  { self.editor_cut(ctx, editor_id, menu_sel, now); ui.close_menu(); }
                        if ui.button("Copy").clicked() { self.editor_copy(ctx, menu_sel);                ui.close_menu(); }
                    });
                    if ui.button("Paste").clicked()      { self.editor_paste(ctx, editor_id, menu_sel, now); ui.close_menu(); }
                    ui.separator();
                    if ui.button("Select All").clicked() { self.editor_select_all(ctx, editor_id); ui.close_menu(); }
                    ui.separator();
                    if ui.button("Undo").clicked()       { self.do_undo(ctx); ui.close_menu(); }
                    if ui.button("Redo").clicked()       { self.do_redo(ctx); ui.close_menu(); }
                });

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

                // Draw search match highlights using the painter after the TextEdit renders.
                // Semi-transparent rects are drawn over the text; the text remains readable
                // through the highlight because of the alpha channel.
                if self.search_open && !self.search_results.is_empty() {
                    let text    = &self.tabs[self.active_tab].content;
                    let painter = ui.painter();

                    for (match_idx, &(start_byte, end_byte)) in self.search_results.iter().enumerate() {
                        let sb = start_byte.min(text.len());
                        let eb = end_byte.min(text.len());
                        let start_char = text[..sb].chars().count();
                        let end_char   = text[..eb].chars().count();

                        let color = if match_idx == self.search_idx {
                            Color32::from_rgba_unmultiplied(124, 106, 247, 140)
                        } else {
                            Color32::from_rgba_unmultiplied(200, 170, 60, 80)
                        };

                        let sc = egui::text::CCursor { index: start_char, prefer_next_row: false };
                        let ec = egui::text::CCursor { index: end_char,   prefer_next_row: false };
                        let start_cur = output.galley.from_ccursor(sc);
                        let end_cur   = output.galley.from_ccursor(ec);

                        let start_row = start_cur.rcursor.row;
                        let end_row   = end_cur.rcursor.row.min(output.galley.rows.len().saturating_sub(1));

                        for row_idx in start_row..=end_row {
                            if row_idx >= output.galley.rows.len() { break; }
                            let row   = &output.galley.rows[row_idx];
                            let x0 = if row_idx == start_row {
                                output.galley.pos_from_cursor(&start_cur).min.x
                            } else {
                                row.rect.min.x
                            };
                            let x1 = if row_idx == end_row {
                                output.galley.pos_from_cursor(&end_cur).min.x
                            } else {
                                row.rect.max.x
                            };
                            let rect = egui::Rect::from_min_max(
                                output.galley_pos + egui::Vec2::new(x0, row.rect.min.y),
                                output.galley_pos + egui::Vec2::new(x1.max(x0 + 4.0), row.rect.max.y),
                            );
                            painter.rect_filled(rect, 2.0, color);
                        }
                    }
                }

                // Line-number gutter — painted on top of the text. Numbers are
                // pinned to the left of the viewport (so horizontal scrolling never
                // moves them) and aligned to each galley row, which keeps them
                // correct even when a logical line wraps over several visual rows.
                if line_nums {
                    let origin  = output.galley_pos;
                    // Clip the gutter strictly to the editor viewport so it never
                    // paints over the panels above/below when the content fits.
                    let painter = ui.painter().with_clip_rect(viewport);

                    let gutter_rect = egui::Rect::from_min_max(
                        egui::pos2(viewport.min.x, viewport.min.y),
                        egui::pos2(viewport.min.x + gutter_w, viewport.max.y),
                    );
                    painter.rect_filled(gutter_rect, 0.0, gutter_bg);
                    painter.line_segment(
                        [egui::pos2(viewport.min.x + gutter_w, viewport.min.y),
                         egui::pos2(viewport.min.x + gutter_w, viewport.max.y)],
                        Stroke::new(1.0, divider),
                    );

                    let num_font = FontId::monospace((font_size - 1.0).max(8.0));
                    let cur      = self.cursor_line;
                    let mut logical  = 0usize;
                    let mut new_line = true;
                    for row in &output.galley.rows {
                        if new_line {
                            let label = if rel_nums {
                                if logical == cur {
                                    (logical + 1).to_string()
                                } else {
                                    (logical as isize - cur as isize).unsigned_abs().to_string()
                                }
                            } else {
                                (logical + 1).to_string()
                            };
                            let color = if logical == cur { cur_color } else { num_color };
                            painter.text(
                                egui::pos2(viewport.min.x + gutter_w - 6.0, origin.y + row.rect.min.y),
                                egui::Align2::RIGHT_TOP,
                                label,
                                num_font.clone(),
                                color,
                            );
                        }
                        new_line = row.ends_with_newline;
                        if row.ends_with_newline { logical += 1; }
                    }
                }

                // Vim Normal-mode block cursor, painted over the character the
                // cursor sits on (using the galley, like the search highlights).
                if vim_normal_active {
                    if let Some(cr) = output.cursor_range.as_ref() {
                        let idx  = cr.primary.ccursor.index;
                        let cc   = output.galley.from_ccursor(egui::text::CCursor::new(idx));
                        let rect = output.galley.pos_from_cursor(&cc);
                        let w    = font_size * 0.6;
                        let min  = output.galley_pos + rect.min.to_vec2();
                        let block = egui::Rect::from_min_size(min, egui::vec2(w, rect.height()));
                        ui.painter().rect_filled(
                            block, 1.0,
                            Color32::from_rgba_unmultiplied(124, 106, 247, 110),
                        );
                    }
                }
            });

        // Tell egui's Focus::begin_frame that this widget owns Escape.  begin_frame
        // runs on RawInput before update() and unconditionally clears focus when
        // Escape is pressed and the focused widget's EventFilter has escape:false
        // (the default).  consume_key cannot help because it operates on InputState,
        // not RawInput.  Setting escape:true here takes effect on the next frame and
        // prevents that focus-clearing path from firing while the editor is active.
        ctx.memory_mut(|mem| mem.set_focus_lock_filter(
            editor_id,
            egui::EventFilter {
                tab:                true,
                horizontal_arrows:  true,
                vertical_arrows:    true,
                escape:             true,
            },
        ));
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

                                // Editor
                                ui.label(RichText::new("EDITOR").size(10.0).strong().color(lc));
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Vim mode").size(11.0));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let prev = self.prefs.vim_mode;
                                        ui.checkbox(&mut self.prefs.vim_mode, "");
                                        if self.prefs.vim_mode != prev {
                                            self.persist_prefs();
                                            self.vim.reset_to_normal();
                                            if self.prefs.vim_mode {
                                                ctx.memory_mut(|m| m.request_focus(egui::Id::new("editor")));
                                            }
                                        }
                                    });
                                });
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Word wrap").size(11.0));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let prev = self.prefs.word_wrap;
                                        ui.checkbox(&mut self.prefs.word_wrap, "");
                                        if self.prefs.word_wrap != prev { self.persist_prefs(); }
                                    });
                                });
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Line numbers").size(11.0));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let prev = self.prefs.line_numbers;
                                        ui.checkbox(&mut self.prefs.line_numbers, "");
                                        if self.prefs.line_numbers != prev { self.persist_prefs(); }
                                    });
                                });
                                if self.prefs.line_numbers {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("Relative numbers").size(11.0));
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            let prev = self.prefs.relative_numbers;
                                            ui.checkbox(&mut self.prefs.relative_numbers, "");
                                            if self.prefs.relative_numbers != prev { self.persist_prefs(); }
                                        });
                                    });
                                }
                                if EDITOR_FONTS.len() > 1 {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("Font").size(11.0));
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            egui::ComboBox::from_id_source("editor_font")
                                                .selected_text(self.prefs.editor_font.clone())
                                                .show_ui(ui, |ui| {
                                                    for (name, _) in EDITOR_FONTS {
                                                        if ui.selectable_label(self.prefs.editor_font == *name, *name).clicked() {
                                                            self.prefs.editor_font = (*name).to_string();
                                                            self.persist_prefs();
                                                        }
                                                    }
                                                });
                                        });
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
