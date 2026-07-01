//! Operator application — `d` / `c` / `y` against a resolved [`OpRange`].
//!
//! `vim_feed` (input layer) decides the operator and resolves the range;
//! this function (editor layer) performs the buffer mutation as a *single*
//! [`EditDelta`] and reports back the text to store in the register, the
//! linewise flag, and whether to enter Insert (for `c`).  It deliberately
//! returns that data rather than touching `VimState` — the input layer
//! owns the register, so the editor layer stays free of any upward
//! dependency.  See `docs/vim-implementation-plan.md` §2.4.
//!
//! **Single delta is load-bearing.** An operator must issue exactly one
//! `apply_delta` (never N char-deletes), so `3dw` is one `u`.  The whole
//! range is removed in one shot here.

use crate::document::EditDelta;
use crate::editor::vim_ops::motion::{first_non_blank, line_end_offset, OpRange};
use crate::editor::EditorState;

/// Which operator is being applied.  An editor-layer mirror of the input
/// layer's `PendingOp` (which also carries the not-yet-wired indent ops),
/// kept here so `vim_ops` needs no `use crate::input::…`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    Yank,
}

/// What an operator produced, for the caller to fold back into `VimState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpResult {
    /// Text to store in the unnamed register.  Empty when the operator
    /// covered no characters (the caller then leaves the register alone).
    pub register_text: String,
    /// Linewise register flag — drives `p`/`P` open-a-new-line behavior.
    pub linewise: bool,
    /// `true` for `c` (change): the caller switches to Insert sub-mode.
    pub enter_insert: bool,
}

/// Apply `op` over `range`, returning the register payload.  Charwise and
/// linewise spans are handled distinctly: linewise reconstructs the
/// full-line content (with a synthesized trailing newline for the
/// register) and, on delete, consumes a neighboring newline so no blank
/// line is left behind.
pub fn execute_operator(editor: &mut EditorState, op: Operator, range: OpRange) -> OpResult {
    match range {
        OpRange::Chars(r) => exec_charwise(editor, op, r.start, r.end),
        OpRange::Lines { first, last } => exec_linewise(editor, op, first, last),
    }
}

// ── Charwise ──────────────────────────────────────────────────────────────────

fn exec_charwise(editor: &mut EditorState, op: Operator, start: usize, end: usize) -> OpResult {
    let len = editor.buffer.len_chars();
    let start = start.min(len);
    let end = end.min(len);
    let text = if start < end {
        editor.buffer.slice_to_string(start, end)
    } else {
        String::new()
    };
    match op {
        Operator::Yank => {
            // Yank leaves the buffer untouched; cursor parks at the span start.
            // Flash the copied span so the user sees the yank land.
            editor.flash_yank(start, end);
            editor.cursor.offset = start;
            editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
            editor.update_cursor_block();
        }
        Operator::Delete | Operator::Change => {
            if start < end {
                // `apply_delta` sets the cursor to `start` (redo_cursor of a
                // pure deletion), which is the correct post-edit position.
                editor.apply_delta(EditDelta {
                    offset: start,
                    removed: text.clone(),
                    inserted: String::new(),
                });
            } else {
                editor.cursor.offset = start;
            }
        }
    }
    OpResult {
        register_text: text,
        linewise: false,
        enter_insert: op == Operator::Change,
    }
}

// ── Linewise ──────────────────────────────────────────────────────────────────

fn exec_linewise(editor: &mut EditorState, op: Operator, first: usize, last: usize) -> OpResult {
    let buf = &editor.buffer;
    let line_count = buf.line_count();
    if line_count == 0 {
        return OpResult {
            register_text: String::new(),
            linewise: true,
            enter_insert: op == Operator::Change,
        };
    }
    let first = first.min(line_count - 1);
    let last = last.min(line_count - 1).max(first);

    let content_start = buf.line_to_char(first);
    let len = buf.len_chars();
    // End of the line block including its trailing newline (or EOF for the
    // last line, which carries no newline).
    let block_end = if last + 1 < line_count {
        buf.line_to_char(last + 1)
    } else {
        len
    };

    // Register text: the whole lines, always terminated by a newline so the
    // linewise flag and the text stay consistent for `p`.
    let mut register_text = buf.slice_to_string(content_start, block_end);
    if !register_text.ends_with('\n') {
        register_text.push('\n');
    }

    match op {
        Operator::Yank => {
            // Flash the whole yanked line block; the overlay painter
            // clamps each rendered line so the trailing newline of the
            // last line never over-paints.
            editor.flash_yank(content_start, block_end);
            editor.cursor.offset = content_start;
            editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
            editor.update_cursor_block();
        }
        Operator::Delete => {
            // When deleting the document's final line(s) there's no trailing
            // newline to remove, so consume the *preceding* one instead —
            // otherwise a stray empty line would remain.
            let (del_start, del_end) = if block_end >= len && content_start > 0 {
                (content_start - 1, len)
            } else {
                (content_start, block_end)
            };
            let removed = editor.buffer.slice_to_string(del_start, del_end);
            editor.apply_delta(EditDelta {
                offset: del_start,
                removed,
                inserted: String::new(),
            });
            // Park the cursor on the first non-blank of the line that now
            // occupies the deletion point (vim's `dd` landing rule).
            let landing = del_start.min(editor.buffer.len_chars());
            let line = editor.buffer.char_to_line(landing);
            editor.cursor.offset = first_non_blank(&editor.buffer, line);
            editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
            editor.update_cursor_block();
        }
        Operator::Change => {
            // `cc`: clear the line(s)' content but keep one empty line, then
            // enter Insert at its start.  Remove only up to the last line's
            // content end so the terminating newline (and the line itself)
            // survives.
            let content_end = line_end_offset(&editor.buffer, last);
            if content_start < content_end {
                let removed = editor.buffer.slice_to_string(content_start, content_end);
                editor.apply_delta(EditDelta {
                    offset: content_start,
                    removed,
                    inserted: String::new(),
                });
            }
            editor.cursor.offset = content_start.min(editor.buffer.len_chars());
            editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
            editor.update_cursor_block();
        }
    }

    OpResult {
        register_text,
        linewise: true,
        enter_insert: op == Operator::Change,
    }
}
