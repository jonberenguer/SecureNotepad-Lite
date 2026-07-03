# Vim Mode

An opt-in, toggleable Vim editing mode for the editor. Toggle it from the
**Vim** button in the toolbar or in **Preferences → EDITOR → Vim mode**
(persisted). When disabled, the editor behaves exactly as it does without Vim
mode.

The current mode is shown at the left of the status bar
(`NORMAL` / `INSERT` / `VISUAL` / `V-LINE` / `V-BLOCK`). In Normal mode a block
cursor is drawn over the character under the cursor. The pending (partial)
command — the Vim `showcmd` — is shown near the right of the status bar.

Yanks and deletes go to the **OS clipboard** (so they paste in other apps) and
also to an in-memory register. The app's **clipboard auto-clear** still applies —
once the clipboard is cleared, `p` / `P` fall back to the in-memory register, so
in-app paste keeps working. Paste prefers the OS clipboard, so text copied
elsewhere pastes into the editor too.

---

## Supported commands

### Modes
| Key | Action |
|---|---|
| `i` `a` `I` `A` | Insert before / after cursor / line start / line end |
| `o` `O` | Open line below / above and insert |
| `v` `V` `Ctrl-v` | Visual (charwise) / Visual-Line / Visual-Block |
| `Esc` | Return to Normal (from Insert or Visual) |

### Motions (count-aware, e.g. `5j`, `3w`)
| Key | Motion |
|---|---|
| `h` `j` `k` `l` | Left / down / up / right (`j`/`k` keep the column; sticky after `$`) |
| `←` `↓` `↑` `→` | Arrow keys also navigate in Normal / Visual mode |
| `0` `^` `$` | Line start / first non-blank / line end |
| `w` `b` `e` | Word forward / back / end |
| `{` `}` | Paragraph back / forward |
| `gg` `G` | First / last line; `{count}gg` / `{count}G` jump to line N |
| `f` `F` `t` `T` {char} | Find char forward/back, till forward/back (on the line) |
| `;` `,` | Repeat last `f`/`t` in same / opposite direction |

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

**Visual-Block** (`Ctrl-v`) selects a rectangle:
- `d` / `x` delete the block, `y` yank it, `c` / `s` change it.
- `I` / `A` insert / append on every row — text typed on the top row is
  replicated to the other rows when you press `Esc`.

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
- **Undo does not restore the exact cursor position.** After `u` / `Ctrl-r` the
  caret is clamped to a valid position rather than moved back to where the edit
  occurred.

> Change/insert commands (`cw`, `cc`, `s`, `o`, `i`, …) now collapse into a
> **single** undo step: entering Insert snapshots the buffer once and `Esc`
> snapshots it again, and the time-based auto-snapshot is suppressed while
> inserting.

### Motions
- **Word motions are simplified.** `w` / `b` / `e` use a basic
  word / punctuation / whitespace classification. There are no separate
  `W` / `B` / `E` (WORD) motions.
- **Paragraph motions are approximate.** `{` / `}` are based on blank lines and
  do not implement Vim's full paragraph rules.

### Registers / clipboard
- **Single unnamed register.** Yank/delete write to the OS clipboard and one
  in-memory register; there are no named registers (`"a`), explicit `"+`, or
  macros (`q`). *(Tier 3)*
- **Linewise-ness is inferred on paste.** `p` treats the clipboard as linewise
  only when it still matches the last yank; text copied from another app pastes
  charwise.

### Search & substitute
- **Rust regex, not Vim regex.** Search and `:s` use the Rust `regex` crate, so
  Vim-specific syntax (`\<`, `\v`, `\{`, …) differs. Replacement uses **`$1`**
  capture-group syntax, not `\1`.
- **`/` cannot appear inside `:s` patterns or replacements** — the command is
  split naively on `/`.
- **`:s` flags:** only `g` (global on the line) is supported. No `c`
  (confirm), inline `i`, count, etc.
- **Search highlight clears when you edit the buffer.** Rather than showing stale
  match positions, the highlight is dropped on any edit; re-run the search (`/`)
  to show matches again.
- **No incremental search or search history.** Matching happens on `Enter`, and
  previous patterns are not recalled.
- **`:q` saves before locking** (safer for a notes app than Vim's discard-by-
  default). Use `:q!` to lock without saving.

### Insert mode
- **Insert mode is plain editing.** Vim insert-mode shortcuts (`Ctrl-w`,
  `Ctrl-u`, `Ctrl-r{reg}`, …) are not implemented; Insert behaves like the normal
  editor.

### Visual-Block
- **Block yank/paste is not a true block.** A yanked block is stored as its rows
  joined by newlines and pastes back as plain text, not re-inserted as a column.
- **Block `I`/`A` replicate simple text only.** The text typed on the top row is
  copied to the other rows on `Esc`; if you type a newline or move off the top
  row during the insert, replication is skipped. `A` does not pad short lines.

### Not yet implemented *(Tier 3)*
- `.` (repeat last change)
- Text objects (`ciw`, `di"`, `ca(` …)
- Marks (`m` / `` ` ``), jumps
- `>>` / `<<` indentation, `J` join, `~` / `gu` / `gU` case operators

### Display
- **Block cursor width is approximate** (based on `font_size × 0.6`), so it may
  be slightly narrow/wide for some glyphs.
