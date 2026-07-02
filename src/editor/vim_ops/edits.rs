//! Single-key edits driven from `vim_feed`.  CP3 implements `p`/`P`
//! (paste the unnamed register); CP4 adds the remaining Normal primitives
//! that mutate the buffer directly: `r{c}` (replace), `~` (toggle case),
//! `J` (join lines), and `>>`/`<<` (indent / outdent).  `x`/`X`/`D`/`C`/`Y`
//! are expressed via `execute_operator` in `vim_feed`, and `u`/`Ctrl-R`
//! reuse the existing undo/redo path, so they don't appear here.  CP6 adds
//! the Visual-mode range edits: `toggle_case_range` (`~`), `set_case_range`
//! (`u`/`U` force-case), `replace_char_range` (`r{c}`), and
//! `replace_range_with` (`p` paste-over).  See
//! `docs/vim-implementation-plan.md` §2.4.
//!
//! Every primitive issues a *single* [`EditDelta`] so the whole command is
//! one undo unit (`3>>`, `3J`, `3rx`).  Paste and the CP4 edits take plain
//! values (register contents, a char, a count) rather than any vim type, so
//! this editor-layer module needs no `use crate::input`.

use crate::document::{next_grapheme_offset, EditDelta};
use crate::editor::edit_ops::{apply_byte_delta, cursor_byte};
use crate::editor::list_edit;
use crate::editor::vim_ops::motion::{first_non_blank, line_end_offset};
use crate::editor::{EditorState, Mode};

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

// ── Replace / toggle-case ───────────────────────────────────────────────────────

/// `r{c}`: replace `count` characters at the cursor with `c`, in one delta.
/// A no-op (vim beeps) when fewer than `count` characters remain on the line
/// — `r` never replaces the trailing newline or spills onto the next line.
/// The cursor lands on the last replaced character.
pub fn replace_char(editor: &mut EditorState, c: char, count: u32) {
    let count = count.max(1) as usize;
    let cursor = editor.cursor.offset;
    let line = editor.buffer.char_to_line(cursor);
    let line_end = line_end_offset(&editor.buffer, line);
    if cursor + count > line_end {
        return; // not enough room on the line
    }
    let removed = editor.buffer.slice_to_string(cursor, cursor + count);
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

/// `~`: toggle the case of `count` characters at the cursor (clamped to the
/// line content), as one delta, then advance the cursor past them — vim's
/// `~` behavior.  Non-cased characters pass through unchanged.
pub fn toggle_case(editor: &mut EditorState, count: u32) {
    let count = count.max(1) as usize;
    let cursor = editor.cursor.offset;
    let line = editor.buffer.char_to_line(cursor);
    let line_end = line_end_offset(&editor.buffer, line);
    let n = count.min(line_end.saturating_sub(cursor));
    if n == 0 {
        return; // at (or past) the line content end — nothing to toggle
    }
    let removed = editor.buffer.slice_to_string(cursor, cursor + n);
    let inserted: String = removed.chars().map(toggle_case_char).collect();
    editor.apply_delta(EditDelta {
        offset: cursor,
        removed,
        inserted,
    });
    editor.cursor.offset = (cursor + n).min(line_end);
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

/// Toggle the case of every character in `[start, end)` as a single delta,
/// leaving the cursor at `start`.  Used by Visual-mode `~`, where the range
/// is the highlighted span (charwise) or the line-expanded range (VisualLine).
/// Non-cased characters (including any newline inside the range) pass through
/// unchanged; a range with nothing to toggle records no delta.
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
        return; // nothing cased in the range
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

/// Force the case of every character in `[start, end)` to lower
/// (`upper == false`) or upper (`upper == true`) as a single delta, leaving
/// the cursor at `start`.  Used by Visual-mode `u` / `U`.  Newlines and
/// already-correct / non-cased characters pass through unchanged; a range
/// with nothing to change records no delta.
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
        return; // nothing to recase in the range
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

/// Visual-mode `r{c}`: replace every character in `[start, end)` with `c` as a
/// single delta, preserving any newlines inside the range (so a multi-line
/// selection keeps its line breaks, matching vim).  The cursor lands at
/// `start`.  An empty range records no delta.
pub fn replace_char_range(editor: &mut EditorState, start: usize, end: usize, c: char) {
    let len = editor.buffer.len_chars();
    let start = start.min(len);
    let end = end.min(len);
    if start >= end {
        return;
    }
    let removed = editor.buffer.slice_to_string(start, end);
    let inserted: String = removed
        .chars()
        .map(|ch| if ch == '\n' { '\n' } else { c })
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

/// Visual-mode `p`: replace `[start, end)` with `text` as a single delta,
/// landing the cursor on the last inserted char (vim's charwise-paste
/// convention).  `text` is the register contents, already normalized by the
/// caller (a trailing newline appended when a charwise register is dropped
/// over whole lines).  A no-op when both the range and `text` are empty.
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
    editor.cursor.offset = (start + inserted_chars)
        .saturating_sub(1)
        .max(start)
        .min(editor.buffer.len_chars());
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
}

/// Swap the case of a single character; leave non-cased characters as-is.
fn toggle_case_char(c: char) -> char {
    if c.is_uppercase() {
        c.to_lowercase().next().unwrap_or(c)
    } else if c.is_lowercase() {
        c.to_uppercase().next().unwrap_or(c)
    } else {
        c
    }
}

/// Force a single character to upper (`upper`) or lower case; leave non-cased
/// characters as-is.
fn force_case_char(c: char, upper: bool) -> char {
    if upper {
        c.to_uppercase().next().unwrap_or(c)
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

// ── Join ────────────────────────────────────────────────────────────────────────

/// `J`: join the current line with the line(s) below it as a *single* delta
/// (so `3J` is one undo).  A bare `J` (or `2J`) joins one line below; `3J`
/// joins two, and so on.  Each join removes the intervening newline and the
/// next line's leading whitespace and inserts a single separating space —
/// unless the text before the join already ends in whitespace or the joined
/// line is empty, in which case no space is added.  The cursor lands on the
/// first join column (vim's convention).
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
        // Strip the joined line's leading whitespace.
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

/// `>>` / `<<`: indent (`right`) or outdent buffer lines `first..=last` by one
/// `indent_width` step, as a single delta.  Indent prepends `indent_width`
/// spaces to every line that holds non-blank content (blank lines stay empty,
/// matching vim); outdent strips up to `indent_width` leading spaces, or one
/// leading tab, per line.  The cursor lands on the first non-blank of `first`.
/// CP4 does the plain-indent case; list-aware indenting is wired in CP10 (§2.5).
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
        return; // nothing changed (e.g. outdent of already-flush lines)
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

// ── List-aware wiring (CP10) ──────────────────────────────────────────────────────
//
// `o`/`O`, `dd`, and `>>`/`<<` reuse the byte-oriented `list_edit` primitives —
// the same ones the non-vim editing path drives — so vim list editing and
// arrow-key list editing produce identical structure.  Each goes through
// `edit_ops::apply_byte_delta` (byte → char conversion + cursor placement), and
// every primitive is a *single* `EditDelta` (`continue_item` and `indent_item`
// renumber inline), keeping `o`/`>>` one undo unit.  All bail out in `Mode::Raw`,
// where the markers are hand-editable source the engine must not rewrite.

/// `o` / `O` inside a Markdown list: continue the list with a fresh empty item
/// instead of a bare newline, auto-renumbering an ordered list.  `below`
/// (`o`) continues from the cursor's item; `O` continues from the *previous*
/// item so the new marker lands above the current one.  Returns `true` when a
/// list continuation was applied; `false` (cursor not in a list, in Raw mode,
/// or `O` on the list's first item) lets the caller fall back to a plain
/// newline open.  The cursor is left just past the new marker, ready for
/// Insert.
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
    // The byte to continue from: the end of the current item's content line
    // (`o`), or the previous item's (`O`).  `O` on the first item has no
    // earlier item to split from, so it falls back to a plain open-above.
    let at = if below {
        info.items[item_idx].line_end
    } else if item_idx > 0 {
        info.items[item_idx - 1].line_end
    } else {
        return false;
    };
    let Some(res) = list_edit::continue_item(&info, &source, at) else {
        return false;
    };
    apply_byte_delta(editor, res.delta, res.cursor_byte);
    true
}

/// After a linewise delete (`dd`, `dj`, `Vd`, …) renumber the ordered list
/// surrounding the cursor so its sequence stays monotonic.  No-op for bullet
/// lists, already-sequential lists, in Raw mode, or when the cursor is not in
/// a list — so it is safe to call after *any* linewise operator (a yank or a
/// `cc` leaves the numbers untouched and records no delta).
///
/// Delegates to [`edit_ops::list_renumber_at_cursor`], the shared post-edit
/// recovery hook: a `dd` lands the cursor on the line below, and the
/// parser-driven renumber walks every ordered run in the surrounding list
/// block (loose-list gaps included), each restarting under its own parent.
pub fn renumber_list_at_cursor(editor: &mut EditorState) {
    crate::editor::edit_ops::list_renumber_at_cursor(editor);
}

/// `>>` / `<<` inside a Markdown list: indent (`right`) or outdent the cursor's
/// list item one level via the structure-aware `list_edit` primitives — which
/// reset an indented ordered item to a fresh `1.` and renumber the surrounding
/// run, matching the non-vim `Tab` / `Shift+Tab` behavior.  Returns `true` when
/// handled; `false` (not in a list, in Raw mode, or an outdent with no indent
/// to strip) lets the caller fall back to the plain space-based `indent_lines`.
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
    // Moving the item between nesting levels can leave a stale marker number in
    // the destination ordered list — an outdented item carries its old nested
    // `1.` into the outer run, and an item indented onto an existing nested run
    // joins it as a duplicate `1.`.  The rendered view always shows sequential
    // numbers, so the raw source would otherwise disagree.  Renumber the list
    // now at the cursor's new level, mirroring the global renumber pass the
    // non-vim `Tab` / `Shift+Tab` path runs after every edit.  A no-op for
    // bullet lists and already-sequential runs.
    renumber_list_at_cursor(editor);
    true
}

/// Drop up to `indent_width` leading spaces, or a single leading tab, from
/// `content`.  Returns a sub-slice (all stripped chars are single-byte).
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
