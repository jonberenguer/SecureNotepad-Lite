//! Vim mode — Phase 0 MVP.
//!
//! This module holds the *pure* pieces of Vim mode: the mode/state types and the
//! cursor-motion functions. They operate on the buffer as a `&[char]` slice and
//! return new character indices, with no dependency on egui, so they are easy to
//! reason about and unit-test. All the glue that reads input, moves the egui
//! caret, and mutates buffer text lives in `app.rs`.
//!
//! The cursor is modelled as a character index in `0..=len`. In Normal mode it
//! conceptually sits *on* a character; `clamp_normal` keeps it on a real
//! character of its line (never on the trailing newline unless the line is
//! empty), matching Vim.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Visual,
    VisualLine,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
        }
    }
    pub fn is_visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine)
    }
}

/// Pending operator awaiting a motion (`d`/`c`/`y`), carrying its count.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PendingOp {
    pub op: Op,
    pub count: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Delete,
    Change,
    Yank,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FindKind {
    Forward,  // f
    Back,     // F
    Till,     // t
    TillBack, // T
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MotionKind {
    Exclusive,
    Inclusive,
    Linewise,
}

/// Which command line is open (Vim `/`, `?`, or `:`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CmdKind {
    SearchFwd,
    SearchBack,
    Ex,
}

impl CmdKind {
    pub fn prefix(self) -> &'static str {
        match self {
            CmdKind::SearchFwd => "/",
            CmdKind::SearchBack => "?",
            CmdKind::Ex => ":",
        }
    }
}

/// Persistent Vim state carried on the app.
#[derive(Default)]
pub struct Vim {
    pub mode: Mode,
    /// Set after `g` while waiting for the second key of a `g_` sequence.
    pub pending_g: bool,
    /// Desired column for vertical motion (`j`/`k`), Vim-style.
    pub want_col: Option<usize>,
    /// Accumulated numeric count prefix.
    pub count: Option<usize>,
    /// Operator awaiting a motion.
    pub pending_op: Option<PendingOp>,
    /// `f`/`F`/`t`/`T` awaiting a target character.
    pub pending_find: Option<FindKind>,
    /// `r` awaiting a replacement character.
    pub pending_replace: bool,
    /// Visual-mode anchor (the fixed end of the selection).
    pub visual_anchor: usize,
    /// Visual-mode moving cursor (the end that motions move).
    pub vcursor: usize,
    /// Internal yank/delete register (kept off the OS clipboard for security).
    pub register: String,
    /// Whether the register holds whole lines (linewise).
    pub register_linewise: bool,
    /// Open command line (`/`, `?`, `:`), if any.
    pub cmdline: Option<CmdKind>,
    /// Command-line input buffer.
    pub cmd_buf: String,
    /// Request focus for the command-line field next frame.
    pub cmd_focus: bool,
    /// Whether a `/` search highlight is currently active.
    pub search_active: bool,
    /// Direction of the last search (for `n`/`N`).
    pub search_forward: bool,
}

impl Vim {
    /// Reset transient command state, returning to Normal mode. The register is
    /// preserved (it survives across commands, like Vim).
    pub fn reset_to_normal(&mut self) {
        self.mode = Mode::Normal;
        self.pending_g = false;
        self.want_col = None;
        self.count = None;
        self.pending_op = None;
        self.pending_find = None;
        self.pending_replace = false;
    }

    /// Consume the pending count (default 1).
    pub fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1).max(1)
    }
}

// ─── Character classes (for word motions) ─────────────────────────────────────

fn class(c: char) -> u8 {
    if c == ' ' || c == '\t' || c == '\n' {
        0 // whitespace
    } else if c.is_alphanumeric() || c == '_' {
        1 // word
    } else {
        2 // punctuation
    }
}

// ─── Line helpers ─────────────────────────────────────────────────────────────

/// First character index of the line containing `i`.
pub fn line_start(s: &[char], i: usize) -> usize {
    let mut j = i.min(s.len());
    while j > 0 && s[j - 1] != '\n' {
        j -= 1;
    }
    j
}

/// Index of the `\n` that ends the line containing `i`, or `s.len()` on the last
/// line.
pub fn line_end(s: &[char], i: usize) -> usize {
    let mut j = i.min(s.len());
    while j < s.len() && s[j] != '\n' {
        j += 1;
    }
    j
}

/// Index of the last real character of the line (never the newline); equals
/// `line_start` for an empty line.
pub fn line_last(s: &[char], i: usize) -> usize {
    let start = line_start(s, i);
    let end = line_end(s, i);
    if end > start {
        end - 1
    } else {
        start
    }
}

/// First non-blank character of the line (or line start if all blank/empty).
pub fn first_non_blank(s: &[char], i: usize) -> usize {
    let start = line_start(s, i);
    let end = line_end(s, i);
    let mut j = start;
    while j < end && (s[j] == ' ' || s[j] == '\t') {
        j += 1;
    }
    if j < end {
        j
    } else {
        start
    }
}

/// Column (0-based) of `i` within its line.
pub fn col(s: &[char], i: usize) -> usize {
    i - line_start(s, i)
}

/// Clamp a cursor to a valid Normal-mode position (on a real character of its
/// line, or the empty-line position).
pub fn clamp_normal(s: &[char], i: usize) -> usize {
    let i = i.min(s.len());
    let start = line_start(s, i);
    let end = line_end(s, i);
    if end > start {
        i.min(end - 1)
    } else {
        start
    }
}

// ─── Motions ──────────────────────────────────────────────────────────────────

pub fn left(s: &[char], i: usize) -> usize {
    let start = line_start(s, i);
    if i > start {
        i - 1
    } else {
        i
    }
}

pub fn right(s: &[char], i: usize) -> usize {
    let last = line_last(s, i);
    if i < last {
        i + 1
    } else {
        last
    }
}

pub fn down(s: &[char], i: usize, want_col: usize) -> usize {
    let end = line_end(s, i);
    if end >= s.len() {
        return i; // already on the last line
    }
    let nstart = end + 1;
    let nend = line_end(s, nstart);
    let nlast = if nend > nstart { nend - 1 } else { nstart };
    (nstart + want_col).min(nlast)
}

pub fn up(s: &[char], i: usize, want_col: usize) -> usize {
    let start = line_start(s, i);
    if start == 0 {
        return i; // already on the first line
    }
    let pstart = line_start(s, start - 1);
    let pend = start - 1; // the '\n' ending the previous line
    let plast = if pend > pstart { pend - 1 } else { pstart };
    (pstart + want_col).min(plast)
}

pub fn word_forward(s: &[char], i: usize) -> usize {
    let n = s.len();
    if i >= n {
        return n;
    }
    let mut j = i;
    let c0 = class(s[j]);
    if c0 != 0 {
        while j < n && class(s[j]) == c0 {
            j += 1;
        }
    }
    while j < n && class(s[j]) == 0 {
        j += 1;
    }
    j
}

pub fn word_backward(s: &[char], i: usize) -> usize {
    if i == 0 {
        return 0;
    }
    let mut j = i - 1;
    while j > 0 && class(s[j]) == 0 {
        j -= 1;
    }
    if class(s[j]) == 0 {
        return j;
    }
    let c = class(s[j]);
    while j > 0 && class(s[j - 1]) == c {
        j -= 1;
    }
    j
}

pub fn word_end(s: &[char], i: usize) -> usize {
    let n = s.len();
    if i + 1 >= n {
        return i;
    }
    let mut j = i + 1;
    while j < n && class(s[j]) == 0 {
        j += 1;
    }
    if j >= n {
        return n - 1;
    }
    let c = class(s[j]);
    while j + 1 < n && class(s[j + 1]) == c {
        j += 1;
    }
    j
}

/// Start of the last line's first non-blank character (Vim `G`).
pub fn buffer_bottom(s: &[char]) -> usize {
    first_non_blank(s, s.len())
}

/// First non-blank of the first line (Vim `gg`).
pub fn buffer_top(s: &[char]) -> usize {
    first_non_blank(s, 0)
}

/// `f`/`F`/`t`/`T`: find the `count`-th `ch` on the current line. Returns the
/// resulting cursor index (for `t`/`T`, one short of the match).
pub fn find_in_line(s: &[char], cursor: usize, kind: FindKind, ch: char, count: usize) -> Option<usize> {
    let count = count.max(1);
    let start = line_start(s, cursor);
    let end = line_end(s, cursor);
    match kind {
        FindKind::Forward | FindKind::Till => {
            let mut found = 0;
            let mut j = cursor + 1;
            while j < end {
                if s[j] == ch {
                    found += 1;
                    if found == count {
                        return Some(if kind == FindKind::Till { j - 1 } else { j });
                    }
                }
                j += 1;
            }
            None
        }
        FindKind::Back | FindKind::TillBack => {
            let mut found = 0;
            let mut j = cursor;
            while j > start {
                j -= 1;
                if s[j] == ch {
                    found += 1;
                    if found == count {
                        return Some(if kind == FindKind::TillBack { j + 1 } else { j });
                    }
                }
            }
            None
        }
    }
}

/// `}`: move to the next blank-line paragraph boundary.
pub fn paragraph_forward(s: &[char], i: usize) -> usize {
    let n = s.len();
    let mut k = i;
    while k < n {
        if s[k] == '\n' && (k + 1 >= n || s[k + 1] == '\n') {
            return (k + 1).min(n);
        }
        k += 1;
    }
    n
}

/// `{`: move to the previous blank-line paragraph boundary.
pub fn paragraph_backward(s: &[char], i: usize) -> usize {
    let mut k = i;
    while k > 0 {
        k -= 1;
        if s[k] == '\n' && (k == 0 || s[k - 1] == '\n') {
            return k;
        }
    }
    0
}

/// Character range `[start, end)` an operator should act on, given the cursor and
/// a motion target with its kind. Linewise expands to whole lines (including the
/// trailing newline, so `dd` removes the line). Returns `(start, end, linewise)`.
pub fn op_range(s: &[char], from: usize, target: usize, kind: MotionKind) -> (usize, usize, bool) {
    match kind {
        MotionKind::Exclusive => {
            let (a, b) = (from.min(target), from.max(target));
            (a, b, false)
        }
        MotionKind::Inclusive => {
            let (a, b) = (from.min(target), from.max(target));
            (a, (b + 1).min(s.len()), false)
        }
        MotionKind::Linewise => {
            let (a0, b0) = (from.min(target), from.max(target));
            let mut start = line_start(s, a0);
            let nl = line_end(s, b0);
            let end = if nl < s.len() { nl + 1 } else { nl };
            // Deleting the last line (no trailing newline) also removes the
            // preceding newline so no blank line is left behind.
            if end == s.len() && start > 0 && s[start - 1] == '\n' {
                start -= 1;
            }
            (start, end, true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn line_helpers() {
        let s = v("ab\ncde\n\nfg");
        // "cde" line: indices 3,4,5 ; newline at 6
        assert_eq!(line_start(&s, 4), 3);
        assert_eq!(line_end(&s, 4), 6);
        assert_eq!(line_last(&s, 4), 5);
        // empty line at index 7
        assert_eq!(line_start(&s, 7), 7);
        assert_eq!(line_last(&s, 7), 7);
        assert_eq!(col(&s, 5), 2);
    }

    #[test]
    fn horizontal_motions_stay_on_line() {
        let s = v("abc\ndef");
        assert_eq!(left(&s, 0), 0); // can't cross to prev line
        assert_eq!(right(&s, 2), 2); // stops at last char 'c'
        assert_eq!(right(&s, 0), 1);
        assert_eq!(left(&s, 2), 1);
    }

    #[test]
    fn vertical_motions_keep_column() {
        let s = v("hello\nhi\nworld");
        // from 'l' at col 3 on line 0 → line 1 has only 2 chars → clamp to last
        let start = 3;
        let d = down(&s, start, col(&s, start));
        assert_eq!(d, line_last(&s, 6)); // "hi" last char
        // down again to "world" keeps want_col 3
        let d2 = down(&s, d, 3);
        assert_eq!(col(&s, d2), 3);
    }

    #[test]
    fn word_motions() {
        let s = v("foo bar.baz");
        assert_eq!(word_forward(&s, 0), 4); // → 'bar'
        assert_eq!(word_forward(&s, 4), 7); // 'bar' → '.'
        assert_eq!(word_backward(&s, 7), 4);
        assert_eq!(word_end(&s, 0), 2); // end of 'foo'
    }

    #[test]
    fn clamp_keeps_cursor_on_line() {
        let s = v("ab\n\ncd");
        assert_eq!(clamp_normal(&s, 2), 1); // newline pos → last char 'b'
        assert_eq!(clamp_normal(&s, 3), 3); // empty line stays
    }

    #[test]
    fn find_char_on_line() {
        let s = v("abcabc");
        assert_eq!(find_in_line(&s, 0, FindKind::Forward, 'c', 1), Some(2));
        assert_eq!(find_in_line(&s, 0, FindKind::Forward, 'c', 2), Some(5));
        assert_eq!(find_in_line(&s, 0, FindKind::Till, 'c', 1), Some(1));
        assert_eq!(find_in_line(&s, 5, FindKind::Back, 'a', 1), Some(3));
        assert_eq!(find_in_line(&s, 0, FindKind::Forward, 'z', 1), None);
        // does not cross line boundary
        let s2 = v("ab\ncd");
        assert_eq!(find_in_line(&s2, 0, FindKind::Forward, 'c', 1), None);
    }

    #[test]
    fn operator_ranges() {
        let s = v("hello world");
        // dw from 0 → exclusive [0,6)
        assert_eq!(op_range(&s, 0, 6, MotionKind::Exclusive), (0, 6, false));
        // de from 0 (e→4) inclusive [0,5)
        assert_eq!(op_range(&s, 0, 4, MotionKind::Inclusive), (0, 5, false));
        // linewise dd on a middle line includes its newline
        let s2 = v("a\nb\nc");
        let (st, en, lw) = op_range(&s2, 2, 2, MotionKind::Linewise);
        assert_eq!((st, en, lw), (2, 4, true)); // "b\n"
        // linewise on the last line also removes the preceding newline
        let (st2, en2, _) = op_range(&s2, 4, 4, MotionKind::Linewise);
        assert_eq!((st2, en2), (3, 5)); // "\nc"
    }

    #[test]
    fn paragraph_motions() {
        let s = v("a\nb\n\nc\nd");
        // forward from 0 → blank line at index 4
        assert_eq!(paragraph_forward(&s, 0), 4);
        // backward from end → blank line boundary
        assert_eq!(paragraph_backward(&s, 7), 4);
    }
}
