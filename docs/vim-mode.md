# Vim Mode

An opt-in, toggleable Vim editing mode for the editor. Enable it in
**Preferences → EDITOR → Vim mode** (persisted). When disabled, the editor
behaves exactly as it does without Vim mode.

The current mode is shown at the left of the status bar
(`NORMAL` / `INSERT` / `VISUAL` / `V-LINE`). In Normal mode a block cursor is
drawn over the character under the cursor.

Yanks and deletes use an **internal register** and are deliberately kept **off
the OS clipboard**, consistent with the app's clipboard-auto-clear security
feature.

---

## Supported commands

### Modes
| Key | Action |
|---|---|
| `i` `a` `I` `A` | Insert before / after cursor / line start / line end |
| `o` `O` | Open line below / above and insert |
| `v` `V` | Visual (charwise) / Visual-Line |
| `Esc` | Return to Normal (from Insert or Visual) |

### Motions (count-aware, e.g. `5j`, `3w`)
| Key | Motion |
|---|---|
| `h` `j` `k` `l` | Left / down / up / right (`j`/`k` keep the column) |
| `0` `^` `$` | Line start / first non-blank / line end |
| `w` `b` `e` | Word forward / back / end |
| `{` `}` | Paragraph back / forward |
| `gg` `G` | First / last line |
| `f` `F` `t` `T` {char} | Find char forward/back, till forward/back (on the line) |

### Operators & edits
| Key | Action |
|---|---|
| `d` `c` `y` + motion | Delete / change / yank over a motion (`dw`, `d$`, `ce`, `y}` …) |
| `dd` `cc` `yy` | Whole-line delete / change / yank (count-aware: `3dd`) |
| `D` `C` | Delete / change to end of line |
| `x` `X` | Delete char under / before cursor (count-aware) |
| `s` `S` | Substitute char / whole line |
| `r`{char} | Replace char(s) under cursor (count-aware) |
| `p` `P` | Paste register after / before cursor (charwise or linewise) |
| `u` · `Ctrl-r` | Undo · redo |

### Visual mode
Motions extend the selection; `d` `x` `c` `s` `y` operate on it, then return to
Normal. `Esc` collapses the selection.

### Search & Ex commands
| Command | Action |
|---|---|
| `/pat` `?pat` | Search forward / backward (regex), highlight matches, jump |
| `n` `N` | Next / previous match (wraps) |
| `:noh` | Clear search highlight |
| `:{number}` | Jump to line N |
| `:s/pat/rep/[g]` | Substitute on the current line |
| `:%s/pat/rep/[g]` | Substitute in the whole file |
| `:w` | Save |
| `:q` `:wq` `:x` | Save and lock (return to the password screen) |
| `:q!` | Lock **without** saving |

---

## Known limitations

These are intentional scope boundaries of the current implementation (Phase 0 →
Tier 2), recorded so behaviour is predictable. Items marked *(Tier 3)* are
planned future work.

### Editing / undo
- **Change is not a single undo step.** `cw` + typing + `Esc` currently takes
  **two** `u` presses to fully revert (the delete and the inserted text are
  separate undo snapshots). Vim collapses this into one.
- **Insert edits may split across undo steps.** A long insert can be broken into
  multiple undo snapshots by the app's time-based auto-snapshot, so `u` may not
  rewind the entire insert at once.
- **Undo does not restore the exact cursor position.** After `u` / `Ctrl-r` the
  caret is clamped to a valid position rather than moved back to where the edit
  occurred.

### Motions
- **Word motions are simplified.** `w` / `b` / `e` use a basic
  word / punctuation / whitespace classification. There are no separate
  `W` / `B` / `E` (WORD) motions.
- **Paragraph motions are approximate.** `{` / `}` are based on blank lines and
  do not implement Vim's full paragraph rules.
- **No `;` / `,`** to repeat the last `f` / `t`.
- **No sticky end-of-line column.** After `$`, moving with `j` / `k` does not
  keep the cursor at line end.
- **`{count}G` / `{count}gg` not supported** — `G` goes to the last line and `gg`
  to the first regardless of count. *(Tier 3)*

### Registers / clipboard
- **Single unnamed register only.** No named registers (`"a`) and no macros
  (`q`). *(Tier 3)*
- **No system-clipboard integration** (`"+y` / `"+p`). Yank/paste use the
  internal register only, by design. *(Tier 3)*

### Search & substitute
- **Rust regex, not Vim regex.** Search and `:s` use the Rust `regex` crate, so
  Vim-specific syntax (`\<`, `\v`, `\{`, …) differs. Replacement uses **`$1`**
  capture-group syntax, not `\1`.
- **`/` cannot appear inside `:s` patterns or replacements** — the command is
  split naively on `/`.
- **`:s` flags:** only `g` (global on the line) is supported. No `c`
  (confirm), inline `i`, count, etc.
- **Highlights can go stale after edits.** Match positions are not recomputed
  when the buffer changes; re-run the search (`/`) to refresh.
- **No incremental search or search history.** Matching happens on `Enter`, and
  previous patterns are not recalled.
- **`:q` saves before locking** (safer for a notes app than Vim's discard-by-
  default). Use `:q!` to lock without saving.

### Insert mode
- **Insert mode is plain editing.** Vim insert-mode shortcuts (`Ctrl-w`,
  `Ctrl-u`, `Ctrl-r{reg}`, …) are not implemented; Insert behaves like the normal
  editor.

### Not yet implemented *(Tier 3)*
- `.` (repeat last change)
- Text objects (`ciw`, `di"`, `ca(` …)
- Visual-block mode (`Ctrl-v`)
- Marks (`m` / `` ` ``), jumps
- `>>` / `<<` indentation, `J` join, `~` / `gu` / `gU` case operators

### Display
- **Block cursor width is approximate** (based on `font_size × 0.6`), so it may
  be slightly narrow/wide for some glyphs.
