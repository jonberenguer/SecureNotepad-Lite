# Vim Mode — Planning & Design Notes

Status: **planning / discussion** (no implementation yet).
Branch: `vim-mode`.

This document captures the plan for an opt-in, toggleable Vim editing mode for
the editor, including why a previous attempt destabilized the app and how to
avoid that.

---

## Verdict

A toggleable Vim mode is **feasible at medium effort**. Two existing pieces of
the app make it much cheaper than building Vim from scratch:

1. **Cursor/selection plumbing already exists.** The editor manipulates the
   cursor and selection directly via `TextEditState.cursor.char_range()` /
   `set_char_range`, and edits the tab `content` in place. That is exactly the
   machinery Vim motions and operators need.
2. **Find & Replace already does the hard work.** Regex search, match list,
   next/prev navigation, replace, and replace-all already exist. Vim's `/`,
   `n`/`N`, and `:s///` are largely a *different front-end over functions we
   already shipped*.

---

## Why the previous attempt broke (egui-specific traps)

egui's `TextEdit::multiline` is a hard-coded **always-insert** widget. Bolting
modal editing on means making that widget stop inserting on demand. The failure
modes:

- **The widget keeps inserting.** In Normal mode, `j` must move down, not type
  "j". egui processes key/text events *inside* `TextEdit::show()`, so the events
  must be handled/removed **before** the widget runs that frame, or it inserts.
- **Global key capture clobbers other fields.** Capturing `h/j/k/l/i/...`
  app-wide breaks the Find box, the inline tab-rename field, the password
  fields, and Preferences. Mode handling must be **scoped** to "editor focused
  and no other text input/modal active".
- **Escape is already contested.** The app installs a focus-lock filter so
  Escape does not unfocus the editor, *and* Escape currently closes Find/Prefs.
  Vim leans on Escape (→ Normal mode). That priority chain must be untangled or
  Escape does two things at once.
- **Undo granularity.** The app has a snapshot undo system. Vim `u` / `Ctrl-r`
  should map to `do_undo` / `do_redo`, and a single operator (e.g. `dd`) must be
  **one** snapshot, not several — needs deliberate boundaries via
  `push_undo_snap`.
- **Block cursor.** egui draws a thin I-beam; Vim users expect a block in Normal
  mode. Cosmetic — can be custom-painted (like the existing search highlights)
  or deferred.

---

## Architecture (keep it isolated and opt-in)

- `prefs.vim_mode: bool` toggle. Off = byte-for-byte current behavior.
- New `src/vim.rs` holding a mostly-pure state machine:
  `enum Mode { Normal, Insert, Visual, VisualLine }`, a pending-operator/count
  buffer, and a function roughly:
  `handle(events, content, cursor) -> { edits, new_cursor, new_mode, register }`.
- In `ui_text_editor`: if Vim on **and** editor focused **and** `Mode != Insert`,
  consume the keystrokes before the `TextEdit`, feed them to the state machine,
  and apply results to `content` + `TextEditState`. In Insert mode the `TextEdit`
  behaves exactly as today.
- Mode indicator in the status bar (`-- NORMAL --` / `-- INSERT --` /
  `-- VISUAL --`).

### Input interception — the make-or-break decision

**Option A — starve the widget of events (recommended).** Keep the `TextEdit`
interactive and focused, but in Normal/Visual mode, right *before*
`TextEdit::show()`, read pending keystrokes, drive the Vim FSM, then remove
`Event::Text` / `Event::Key` from `ctx.input_mut().events` so the widget sees
nothing to insert. egui still renders the caret/selection and auto-scrolls to the
cursor; we drive the *semantics* by setting `TextEditState.cursor` and editing
`content`. Because `handle_shortcuts` runs *before* the editor each frame, app
shortcuts (Ctrl+S, etc.) still fire before we strip events.

**Option B — `.interactive(false)` in Normal mode.** Simpler to reason about
(widget ignores the keyboard), but it stops drawing a cursor, can drop focus, and
forces us to re-implement caret + scrolling. More regressions, less reuse.

Decision: **Option A.**

### Escape chain (with Vim on)

Strict priority order, reusing the existing focus-lock filter:

1. Vim `:` / `/` command line open → close it.
2. Mode is Insert/Visual → return to Normal (consume Escape here; do **not** let
   it bubble).
3. Already Normal → fall through to today's behavior (close Find/Prefs).

---

## Code Integration Audit (against current `main`)

The plan above predates several editor features now on `main` (line-number
gutter, word-wrap layouter, continuous selection tracking, Markdown preview).
Re-audited against the current code, the core approach still holds, but these
integration points must be handled. (Line numbers are approximate.)

### 1. Selection-tracking collision (highest priority)
The editor continuously tracks `self.context_sel` from the live selection every
frame and **restores** it on right-press (`app.rs` ~1815-1835, powering the
right-click menu). Vim Visual mode and a faked block cursor both drive the same
selection via `set_char_range`, so the two systems will fight: a 1-char block
cursor would light up right-click Cut/Copy, and restore-on-right-press would
clobber the Visual anchor.
**Decision:** when vim is on and `mode != Insert`, bypass the `context_sel`
tracker/restore and let vim own the selection.

### 2. Block cursor + gutter / wrap interplay
Paint the block cursor with the **same galley-row technique** already used for
search highlights and line numbers (`app.rs` ~1908-1955), accounting for the
gutter x-offset (already baked into `galley_pos.x`).
With **word wrap on**, `j`/`k` should move by *logical* line (vim `nowrap`
semantics), with `gj`/`gk` for visual rows. Column motions (`0`/`^`/`$`) act on
logical-line char boundaries in `content`, independent of wrapping.

### 3. Scroll-to-cursor
Because vim sets the cursor via `TextEditState` rather than egui's own key
handling, off-screen motions (`j`, `G`, `gg`) will **not** auto-scroll for free.
Compute the caret's row rect from the galley and `scroll_to_rect` it. Verify
early — it's an easy thing to miss and makes navigation feel broken.

### 4. Yank register vs system clipboard (security decision)
The app deliberately clears the OS clipboard after copies. Routing vim `y`/`p`
through the OS clipboard would spray secrets into it on every yank.
**Decision (default):** `y` / `d` / `p` use an **internal vim register**, keeping
secrets off the OS clipboard. Explicit `"+y` / `"+p` may target the system
clipboard. This reinforces the app's existing security posture.

### 5. Escape chain — concrete wiring
Two Escape handlers already exist: search/prefs close (`app.rs` ~1103, consumes
`NONE+Escape`) and the editor focus-lock filter (`app.rs` ~1970, `escape:true` —
**must be kept**). Vim's "Insert/Visual → Normal" slots in **after** the
search/prefs check and **before** egui unfocus, and must live in the editor path
(not `handle_shortcuts`).

### Minor
- Mode-state scope: single global current-mode; reset to Normal on tab switch and
  on lock.
- Interception is gated on `ctx.memory(has_focus(editor_id))`, so the Find box,
  inline tab-rename, and password fields stay unaffected.
- Command-line UX for `/` and `:` still open (reuse Find bar vs new bottom line).

---

## Feature tiers

### Tier 1 — core modal editing (medium)
Motions: `h j k l w b e 0 ^ $ gg G { }`, counts (`5j`), `f/F/t/T`.
Operators: `d c y` + motions, `x D C p P o O i I a A r`.
Visual: `v` / `V` + operators.
Undo: `u` / `Ctrl-r` mapped to existing undo stacks.
~500–800 lines. Main risk is the input-interception + focus integration, not the
motions themselves.

### Tier 2 — search & ex commands (moderate, high payoff via reuse)
- `/foo⏎` → set `search_query`, call `run_search()`, jump to first match past the
  cursor. `n` / `N` ride `search_idx` next/prev.
- `:s/a/b/` and `:%s/a/b/g` → reuse `replace_current` / `replace_all`. Only new
  bit is a **line-range** concept (current replace is whole-buffer).
- `:w` `:q` `:wq` `:noh` → `save_now` / `lock` / clear search.

### Tier 3 — power features (hard; defer/optional)
Text objects (`ciw`, `di"`), `.` repeat, named registers, macros (`q`), marks,
visual-block. The operator-pending / multi-key state machine and `.`-repeat are
the costly, bug-prone parts — and the ones not everyone uses.

---

## De-risking plan

1. **Preserve the known-good state.** Current features are committed and pushed
   to `main`; this branch (`vim-mode`) is for the Vim work.
2. **Phase 0 MVP to prove the foundation:** toggle + mode switching + `h j k l`,
   `i` / `a`, `Esc`, status indicator — with **zero** regressions to typing,
   Find, rename, passwords, and shortcuts. This is precisely the layer that broke
   before; nail it first. Everything after is additive and low-risk.

---

## Open questions (drive scope)

1. **Which Vim features are used day-to-day?** If it's `hjkl`, `w/b`, `dd/yy/p`,
   `ciw`, `/`, `:s`, that is a much smaller build than "full Vim". The long tail
   (macros, marks, registers, visual-block) is where most effort hides.
2. **Cursor style:** real custom-painted block cursor in Normal mode, or accept
   egui's I-beam to start and add the block later?
3. **Command-line UX:** reuse the existing Find bar for `/` and `:` (less work),
   or a dedicated vim-style `:` line pinned at the bottom (more authentic)?
4. **Confirm Insert mode = today's editor exactly** (all current shortcuts live,
   no Vim interference). Assumed yes.

---

## Decisions log

- **Input interception:** Option A — strip `Text`/`Key` events before the
  `TextEdit` in Normal/Visual mode. (Architecture)
- **Selection ownership:** vim owns the selection when `mode != Insert`; the
  `context_sel` right-click tracker/restore is bypassed then. (Audit #1)
- **Yank/paste:** default to an internal register; `"+` targets the OS clipboard.
  (Audit #4)
- (pending) Target scope for the first branch.
- (pending) `j`/`k` default wrap semantics (logical vs visual rows).
- (pending) Command-line UX (reuse Find bar vs a bottom `:` line).
- (pending) Which day-to-day Vim features to include (open question #1).
