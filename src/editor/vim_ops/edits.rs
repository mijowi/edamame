//! Single-key edits driven from `vim_feed`.  CP3 implements `p`/`P`
//! (paste the unnamed register); `r{c}`, `~`, `J`, `x`/`X`, `o`/`O` land
//! in later checkpoints (`x`/`X` are expressed via `execute_operator` in
//! CP3, so only paste lives here for now).  See
//! `docs/vim-implementation-plan.md` §2.4.
//!
//! Paste takes the register *contents* (`text` + `linewise`) rather than a
//! `VimRegister`, so this editor-layer module needs no `use crate::input`.

use crate::document::{next_grapheme_offset, EditDelta};
use crate::editor::vim_ops::motion::{first_non_blank, line_end_offset};
use crate::editor::EditorState;

/// Paste the register `text` `count` times.  `after` selects `p` (after
/// the cursor / below the line) vs. `P` (before the cursor / above the
/// line); `linewise` selects the open-a-new-line behavior.  A no-op for
/// an empty register.
pub fn paste(editor: &mut EditorState, text: &str, linewise: bool, count: u32, after: bool) {
    if text.is_empty() {
        return;
    }
    let repeated = text.repeat(count.max(1) as usize);
    if linewise {
        paste_linewise(editor, &repeated, after);
    } else {
        paste_charwise(editor, &repeated, after);
    }
}

/// Charwise paste: insert after (`p`) or at (`P`) the cursor, leaving the
/// cursor on the last inserted char (vim's convention).
fn paste_charwise(editor: &mut EditorState, text: &str, after: bool) {
    let cursor = editor.cursor.offset;
    let line = editor.buffer.char_to_line(cursor);
    let line_content_end = line_end_offset(&editor.buffer, line);
    let insert_at = if after && cursor < line_content_end {
        next_grapheme_offset(&editor.buffer, cursor)
    } else {
        cursor
    };
    let inserted_chars = text.chars().count();
    editor.apply_delta(EditDelta {
        offset: insert_at,
        removed: String::new(),
        inserted: text.to_owned(),
    });
    // `apply_delta` parks the cursor past the inserted text; vim leaves it
    // on the final pasted char.
    editor.cursor.offset = (insert_at + inserted_chars)
        .saturating_sub(1)
        .max(insert_at)
        .min(editor.buffer.len_chars());
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

/// Linewise paste: open a fresh line below (`p`) or above (`P`) the
/// cursor's line and drop the register's whole lines there, landing the
/// cursor on the first non-blank of the first pasted line.
fn paste_linewise(editor: &mut EditorState, text: &str, after: bool) {
    let cursor = editor.cursor.offset;
    let line = editor.buffer.char_to_line(cursor);
    let line_count = editor.buffer.line_count();
    let len = editor.buffer.len_chars();

    // `text` always ends in '\n' (linewise register).  Where it lands and
    // whether a separator newline is needed depends on the insertion site.
    let (insert_at, payload, first_line_offset) = if after {
        if line + 1 < line_count {
            // Clean line boundary just below the cursor's line.
            let at = editor.buffer.line_to_char(line + 1);
            (at, text.to_owned(), at)
        } else {
            // Cursor is on the document's last line (no trailing newline):
            // prepend a separator and drop the register's own trailing one.
            let body = text.strip_suffix('\n').unwrap_or(text);
            (len, format!("\n{body}"), len + 1)
        }
    } else {
        let at = editor.buffer.line_to_char(line);
        (at, text.to_owned(), at)
    };

    editor.apply_delta(EditDelta {
        offset: insert_at,
        removed: String::new(),
        inserted: payload,
    });

    let landing = first_line_offset.min(editor.buffer.len_chars());
    let landing_line = editor.buffer.char_to_line(landing);
    editor.cursor.offset = first_non_blank(&editor.buffer, landing_line);
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}
