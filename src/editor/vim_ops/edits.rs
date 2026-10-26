//! Single-key edits driven from `vim_feed`: `p`/`P`, `r{c}`, `~`, `J`, `>>`/`<<`, and the
//! Visual-mode range forms.  `x`/`X`/`D`/`C`/`Y` go through `execute_operator` instead.
//!
//! Every primitive issues a *single* [`EditDelta`] so the whole command is one undo unit
//! (`3>>`, `3J`, `3rx`).  All take plain values rather than vim types, so this
//! editor-layer module needs no `use crate::input`.

use unicode_segmentation::UnicodeSegmentation;

use crate::document::{next_grapheme_offset, prev_grapheme_offset, Buffer, EditDelta};
use crate::editor::edit_ops::{apply_byte_delta, cursor_byte};
use crate::editor::list_edit;
use crate::editor::vim_ops::motion::{first_non_blank, line_end_offset};
use crate::editor::{EditorState, Mode};

/// Paste `text` — the register, already repeated for a count (see
/// [`repeat_for_paste`](crate::editor::edit_ops::repeat_for_paste)).  `after` selects `p` over
/// `P`; `linewise` selects the open-a-new-line behavior.  A no-op for empty text.
pub fn paste(editor: &mut EditorState, text: &str, linewise: bool, after: bool) {
    if text.is_empty() {
        return;
    }
    if linewise {
        paste_linewise(editor, text, after);
    } else {
        paste_charwise(editor, text, after);
    }
    if text.contains('\n') {
        // Pasted lines can land inside an ordered list, or carry items of their own.
        renumber_list_at_cursor(editor);
    }
}

/// Insert after (`p`) or at (`P`) the cursor, leaving the cursor on the last inserted
/// char (vim's convention).
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
    // `apply_delta` parks past the inserted text; vim leaves the cursor on its last glyph.
    editor.cursor.offset = last_glyph_of(&editor.buffer, insert_at, insert_at + inserted_chars);
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

/// Open a fresh line below (`p`) or above (`P`) and drop the register's whole lines
/// there, landing on the first non-blank of the first pasted line.
fn paste_linewise(editor: &mut EditorState, text: &str, after: bool) {
    let cursor = editor.cursor.offset;
    let line = editor.buffer.char_to_line(cursor);
    let line_count = editor.buffer.line_count();
    let len = editor.buffer.len_chars();

    // `text` always ends in '\n' (linewise register).
    let (insert_at, payload, first_line_offset) = if after {
        if line + 1 < line_count {
            let at = editor.buffer.line_to_char(line + 1);
            (at, text.to_owned(), at)
        } else {
            // Last line, so no trailing newline: move the separator to the front.
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

// ── Replace / toggle-case ───────────────────────────────────────────────────────

/// `r{c}`: replace `count` glyphs (grapheme clusters) at the cursor with one `c` each, in
/// one delta, landing on the last one.  A no-op when fewer than `count` remain on the line —
/// `r` never replaces the newline or spills onto the next line.
pub fn replace_char(editor: &mut EditorState, c: char, count: u32) {
    let count = count.max(1) as usize;
    let cursor = editor.cursor.offset;
    let (end, taken) = glyphs_on_line(&editor.buffer, cursor, count);
    if taken < count {
        return;
    }
    let removed = editor.buffer.slice_to_string(cursor, end);
    let inserted: String = std::iter::repeat_n(c, count).collect();
    editor.apply_delta(EditDelta {
        offset: cursor,
        removed,
        inserted,
    });
    editor.cursor.offset = (cursor + count - 1).min(editor.buffer.len_chars());
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

/// `~`: toggle the case of `count` glyphs (grapheme clusters) at the cursor (clamped to the
/// line content) in one delta, then advance past them.  Non-cased characters pass through.
pub fn toggle_case(editor: &mut EditorState, count: u32) {
    let count = count.max(1) as usize;
    let cursor = editor.cursor.offset;
    let (end, _) = glyphs_on_line(&editor.buffer, cursor, count);
    if end == cursor {
        return;
    }
    let removed = editor.buffer.slice_to_string(cursor, end);
    let inserted: String = removed.chars().map(toggle_case_char).collect();
    editor.apply_delta(EditDelta {
        offset: cursor,
        removed,
        inserted,
    });
    editor.cursor.offset = end;
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

/// Visual-mode `~` over `[start, end)`, as one delta leaving the cursor at `start`.
/// Non-cased characters and newlines pass through; a range with nothing to toggle records
/// no delta.
pub fn toggle_case_range(editor: &mut EditorState, start: usize, end: usize) {
    let len = editor.buffer.len_chars();
    let start = start.min(len);
    let end = end.min(len);
    if start >= end {
        return;
    }
    let removed = editor.buffer.slice_to_string(start, end);
    let inserted: String = removed.chars().map(toggle_case_char).collect();
    if inserted == removed {
        return;
    }
    editor.apply_delta(EditDelta {
        offset: start,
        removed,
        inserted,
    });
    editor.cursor.offset = start;
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

/// Visual-mode `u` / `U`: force `[start, end)` to lower or upper case as one delta,
/// leaving the cursor at `start`.  Records no delta when nothing changes.
pub fn set_case_range(editor: &mut EditorState, start: usize, end: usize, upper: bool) {
    let len = editor.buffer.len_chars();
    let start = start.min(len);
    let end = end.min(len);
    if start >= end {
        return;
    }
    let removed = editor.buffer.slice_to_string(start, end);
    let inserted: String = removed.chars().map(|c| force_case_char(c, upper)).collect();
    if inserted == removed {
        return;
    }
    editor.apply_delta(EditDelta {
        offset: start,
        removed,
        inserted,
    });
    editor.cursor.offset = start;
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

/// Visual-mode `r{c}` over `[start, end)`, as one delta landing at `start`.  Newlines
/// inside the range survive, so a multi-line selection keeps its line breaks (vim).
pub fn replace_char_range(editor: &mut EditorState, start: usize, end: usize, c: char) {
    let len = editor.buffer.len_chars();
    let start = start.min(len);
    let end = end.min(len);
    if start >= end {
        return;
    }
    let removed = editor.buffer.slice_to_string(start, end);
    // One `c` per glyph, so `❤\u{FE0F}` becomes a single `c`, as in vim.  A line break is
    // kept whole: `\r\n` is one glyph, and replacing it would join the lines.
    let c = c.to_string();
    let inserted: String = removed
        .graphemes(true)
        .map(|g| if g.ends_with('\n') { g } else { &c })
        .collect();
    editor.apply_delta(EditDelta {
        offset: start,
        removed,
        inserted,
    });
    editor.cursor.offset = start;
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

/// Visual-mode `p`: replace `[start, end)` with `text` as one delta, landing on the last
/// inserted char.  `text` is already normalized by the caller (a trailing newline appended
/// when a charwise register is dropped over whole lines).
pub fn replace_range_with(editor: &mut EditorState, start: usize, end: usize, text: &str) {
    let len = editor.buffer.len_chars();
    let start = start.min(len);
    let end = end.min(len);
    if start >= end && text.is_empty() {
        return;
    }
    let removed = editor.buffer.slice_to_string(start, end);
    let inserted_chars = text.chars().count();
    editor.apply_delta(EditDelta {
        offset: start,
        removed,
        inserted: text.to_owned(),
    });
    editor.cursor.offset = last_glyph_of(&editor.buffer, start, start + inserted_chars);
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

/// Swap the case of one character; non-cased characters pass through.
fn toggle_case_char(c: char) -> char {
    if c.is_uppercase() {
        c.to_lowercase().next().unwrap_or(c)
    } else if c.is_lowercase() {
        c.to_uppercase().next().unwrap_or(c)
    } else {
        c
    }
}

/// Force one character to upper or lower case; non-cased characters pass through.
fn force_case_char(c: char, upper: bool) -> char {
    if upper {
        c.to_uppercase().next().unwrap_or(c)
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// End of up to `count` glyphs (grapheme clusters) from `cursor`, never past its line's
/// content, and how many glyphs that took: fewer than `count` when the line ran out.
fn glyphs_on_line(buf: &Buffer, cursor: usize, count: usize) -> (usize, usize) {
    let line_end = line_end_offset(buf, buf.char_to_line(cursor));
    let mut end = cursor;
    let mut taken = 0;
    while taken < count && end < line_end {
        end = next_grapheme_offset(buf, end).min(line_end);
        taken += 1;
    }
    (end, taken)
}

/// Start of the glyph holding the last char inserted at `[start, end)`, or `start` when that
/// is empty: where vim leaves the cursor after a paste.  This can be before `start`, when
/// the text opens with a combining mark that joins the glyph before it.
fn last_glyph_of(buf: &Buffer, start: usize, end: usize) -> usize {
    let end = end.min(buf.len_chars());
    if end <= start {
        return start.min(buf.len_chars());
    }
    prev_grapheme_offset(buf, end)
}

// ── Join ────────────────────────────────────────────────────────────────────────

/// `J`: join the current line with the line(s) below as one delta.  `J` and `2J` join one
/// line, `3J` two, and so on.  Each join drops the newline and the next line's leading
/// whitespace and inserts one separating space — unless the preceding text already ends in
/// whitespace or the joined line is empty.  The cursor lands on the first join column.
pub fn join_lines(editor: &mut EditorState, count: u32) {
    let joins = count.max(2) as usize - 1; // 1J / 2J → 1 join; 3J → 2
    let buf = &editor.buffer;
    let start_line = buf.char_to_line(editor.cursor.offset);
    let line_count = buf.line_count();
    if start_line + 1 >= line_count {
        return; // nothing below to join
    }

    let region_start = line_end_offset(buf, start_line);
    let start_line_start = buf.line_to_char(start_line);
    // Whether the text immediately before the next insert is whitespace
    // (so no separating space is added).
    let mut prev_is_ws = region_start == start_line_start
        || (region_start > 0 && buf.rope().char(region_start - 1).is_whitespace());

    let mut replacement = String::new();
    let mut first_join_offset = None;
    let mut last_consumed = start_line;
    for step in 1..=joins {
        let li = start_line + step;
        if li >= line_count {
            break;
        }
        last_consumed = li;
        let li_start = buf.line_to_char(li);
        let li_end = line_end_offset(buf, li);
        let mut content_start = li_start;
        while content_start < li_end && matches!(buf.rope().char(content_start), ' ' | '\t') {
            content_start += 1;
        }
        let content = buf.slice_to_string(content_start, li_end);
        let is_empty = content_start >= li_end;
        let sep = if prev_is_ws || is_empty { "" } else { " " };
        if first_join_offset.is_none() {
            first_join_offset = Some(region_start + replacement.chars().count());
        }
        replacement.push_str(sep);
        replacement.push_str(&content);
        prev_is_ws = content
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace());
    }

    let region_end = line_end_offset(buf, last_consumed);
    let removed = buf.slice_to_string(region_start, region_end);
    editor.apply_delta(EditDelta {
        offset: region_start,
        removed,
        inserted: replacement,
    });
    editor.cursor.offset = first_join_offset
        .unwrap_or(region_start)
        .min(editor.buffer.len_chars());
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

// ── Indent / outdent ────────────────────────────────────────────────────────────

/// `>>` / `<<`: shift lines `first..=last` by one `indent_width` step, as one delta.
/// Indent skips blank lines (vim); outdent strips up to `indent_width` spaces or one tab.
/// The cursor lands on the first non-blank of `first`.  See [`indent_list_item`] for the
/// list-aware path that takes precedence.
pub fn indent_lines(
    editor: &mut EditorState,
    first: usize,
    last: usize,
    right: bool,
    indent_width: usize,
) {
    let line_count = editor.buffer.line_count();
    if line_count == 0 {
        return;
    }
    let last = last.min(line_count - 1);
    let first = first.min(last);
    let start = editor.buffer.line_to_char(first);
    let end = if last + 1 < line_count {
        editor.buffer.line_to_char(last + 1)
    } else {
        editor.buffer.len_chars()
    };
    let region = editor.buffer.slice_to_string(start, end);
    let indent = " ".repeat(indent_width);
    let mut out = String::with_capacity(region.len() + indent_width);
    for line in region.split_inclusive('\n') {
        let (content, nl) = match line.strip_suffix('\n') {
            Some(c) => (c, "\n"),
            None => (line, ""),
        };
        if right {
            if content.chars().any(|c| !c.is_whitespace()) {
                out.push_str(&indent);
            }
            out.push_str(content);
        } else {
            out.push_str(strip_indent(content, indent_width));
        }
        out.push_str(nl);
    }
    if out == region {
        return;
    }
    editor.apply_delta(EditDelta {
        offset: start,
        removed: region,
        inserted: out,
    });
    editor.cursor.offset = first_non_blank(&editor.buffer, first);
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

// ── List-aware wiring ─────────────────────────────────────────────────────────────
//
// `o`/`O`, `dd`, and `>>`/`<<` reuse the byte-oriented `list_edit` primitives the non-vim
// editing path drives, so both produce identical structure.  Each stays a *single*
// `EditDelta` and so one undo unit.  All bail out in `Mode::Raw`, where the markers are
// hand-editable source the engine must not rewrite.

/// `o` / `O` inside a Markdown list: continue with a fresh empty item instead of a bare
/// newline, auto-renumbering an ordered list.  `O` continues from the *previous* item so
/// the new marker lands above the current one; on the first item, which has none, it copies
/// that item's marker above it instead.  `false` (not in a list, or Raw mode) means the caller
/// should fall back to a plain newline open.
pub fn open_list_continue(editor: &mut EditorState, below: bool) -> bool {
    if editor.mode == Mode::Raw {
        return false;
    }
    let source = editor.buffer.contents();
    let byte = cursor_byte(editor);
    let Some(info) = list_edit::find_list_at(&source, byte) else {
        return false;
    };
    let Some(item_idx) = list_edit::cursor_item_idx(&info, byte) else {
        return false;
    };
    let at = if below {
        info.items[item_idx].line_end
    } else if item_idx > 0 {
        info.items[item_idx - 1].line_end
    } else {
        open_first_list_item(editor, &info, &source);
        return true;
    };
    let Some(res) = list_edit::continue_item(&info, &source, at) else {
        return false;
    };
    apply_byte_delta(editor, res.delta, res.cursor_byte);
    true
}

/// `O` on a list's first item: there is no earlier item to continue from, so insert an empty
/// copy of the first item's marker line (indent, marker, and an unchecked `[ ] ` for a task
/// list) before it, then renumber — the copied `1.` pushes the old first item to `2.`.  One
/// undo step: the renumbering folds into the insertion.
fn open_first_list_item(editor: &mut EditorState, info: &list_edit::ListInfo, source: &str) {
    let item = &info.items[0];
    let mut line = source[item.start..item.marker_end].to_owned();
    if item.task.is_some() {
        line.push_str("[ ] ");
    }
    let landing = item.start + line.len();
    line.push('\n');
    apply_byte_delta(
        editor,
        EditDelta {
            offset: item.start,
            removed: String::new(),
            inserted: line,
        },
        landing,
    );
    renumber_list_at_cursor(editor);
}

/// Renumber the ordered list around the cursor after a linewise edit, as part of that edit's
/// undo step.  A no-op for bullet lists, already-sequential lists, Raw mode, or a cursor
/// outside a list, so it is safe after *any* linewise operator or paste — but only directly
/// after an edit, since the renumbering joins the newest undo entry.  Delegates to
/// [`edit_ops::list_renumber_at_cursor`](crate::editor::edit_ops::list_renumber_at_cursor).
pub fn renumber_list_at_cursor(editor: &mut EditorState) {
    crate::editor::edit_ops::list_renumber_at_cursor(editor);
}

/// `>>` / `<<` inside a Markdown list: shift the cursor's item one nesting level via the
/// structure-aware `list_edit` primitives, matching non-vim `Tab` / `Shift+Tab`.  `false`
/// means the caller should fall back to the plain [`indent_lines`].
pub fn indent_list_item(editor: &mut EditorState, right: bool) -> bool {
    if editor.mode == Mode::Raw {
        return false;
    }
    let source = editor.buffer.contents();
    let byte = cursor_byte(editor);
    let Some(info) = list_edit::find_list_at(&source, byte) else {
        return false;
    };
    let res = if right {
        list_edit::indent_item(&info, &source, byte, crate::constants::INDENT_WIDTH)
    } else {
        list_edit::outdent_item(&info, &source, byte, crate::constants::INDENT_WIDTH)
    };
    let Some(res) = res else {
        return false;
    };
    apply_byte_delta(editor, res.delta, res.cursor_byte);
    // Changing nesting level leaves a stale marker number in the destination run (an
    // outdented item carries its old `1.` out; an indented one joins as a duplicate `1.`),
    // and the rendered view always shows sequential numbers.
    renumber_list_at_cursor(editor);
    true
}

/// Drop up to `indent_width` leading spaces, or one leading tab.  Returns a sub-slice
/// (every stripped char is single-byte).
fn strip_indent(content: &str, indent_width: usize) -> &str {
    let mut skip = 0;
    for (i, c) in content.char_indices() {
        if i >= indent_width {
            break;
        }
        match c {
            ' ' => skip = i + 1,
            '\t' => {
                skip = i + 1;
                break;
            }
            _ => break,
        }
    }
    &content[skip..]
}
