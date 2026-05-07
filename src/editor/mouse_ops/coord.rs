use ratatui::text::Line;

use crate::editor::table_edit;
use crate::editor::{EditorState, Mode};
use crate::markdown::table_layout;
use crate::ui::line_render;

/// Look up the rendered `Line` and the visual column within it that
/// correspond to document-area row `row`.  Accounts for scroll and wrap; the
/// returned visual col is `row - y_of_first_row_of_line`.
pub(super) fn rendered_line_at_row(
    state: &EditorState,
    row: usize,
) -> Option<(Line<'static>, usize)> {
    let lines = &state.parsed.lines;
    if lines.is_empty() {
        return None;
    }
    let (mut line_idx, mut first_sub_row) =
        state.rendered_line_at_visual_row(state.scroll.saturating_add(row), state.viewport_width);
    let mut y = 0usize;
    while let Some(line) = lines.get(line_idx) {
        let rows_used = line_render::visual_rows_for_line(line, state.viewport_width).max(1);
        let visible_rows = rows_used.saturating_sub(first_sub_row).max(1);
        if y < visible_rows {
            return Some((line.clone(), first_sub_row));
        }
        y += visible_rows;
        line_idx += 1;
        first_sub_row = 0;
    }
    None
}

/// True if the span covering char-col `col` in `line` has `modifier` set.
pub(super) fn span_at_col_has_modifier(
    line: &Line<'_>,
    col: usize,
    modifier: ratatui::style::Modifier,
) -> bool {
    let mut walk = 0usize;
    for span in &line.spans {
        let span_len = span.content.chars().count();
        if col < walk + span_len {
            return span.style.add_modifier.contains(modifier);
        }
        walk += span_len;
    }
    false
}

/// Translate a click at `(col, row)` in the document area to a buffer char
/// offset.  Accounts for current scroll and visual-row wrap.
///
/// Returns `None` only when the buffer is empty — all clicks are clamped to
/// the nearest valid position (end of line / end of buffer) so the caller
/// never has to handle "click landed in whitespace past the document".
pub(super) fn click_to_char_offset(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> Option<usize> {
    match state.mode {
        Mode::Raw => Some(raw_click_to_offset(state, col, row, viewport_width)),
        Mode::Preview | Mode::Rendered => {
            Some(rendered_click_to_offset(state, col, row, viewport_width))
        }
    }
}

/// Raw-mode click: walk buffer lines from `state.scroll`, accumulating each
/// line's wrapped visual-row count, and translate the click into a char
/// offset on the appropriate visual sub-row.  Cell-aware so wide chars
/// align and the cursor lands where the user sees it.
fn raw_click_to_offset(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> usize {
    let line_count = state.buffer.line_count();
    let width = viewport_width.max(1);
    let (mut target_line, mut first_sub_row) = state.raw_line_at_visual_row(state.scroll, width);
    let mut y = 0usize;
    while target_line < line_count {
        let text = state
            .buffer
            .line(target_line)
            .map(|s| s.trim_end_matches('\n').to_owned())
            .unwrap_or_default();
        let rows = line_render::visual_rows_of_str(&text, width);
        let used = rows.len().max(1).saturating_sub(first_sub_row).max(1);
        if row < y + used {
            let sub_row = first_sub_row + row - y;
            let line_start = state.buffer.line_to_char(target_line);
            let row_tuple = rows.get(sub_row).copied().unwrap_or((0, 0, 0));
            let raw_col = char_in_row_at_cell(&text, row_tuple, col, 0, sub_row + 1 == rows.len());
            return line_start + raw_col;
        }
        y += used;
        target_line += 1;
        first_sub_row = 0;
    }
    state.buffer.len_chars()
}

/// Cell-aware "click landed in this visual row" → char column on the
/// logical line.  Mirrors `state::raw_col_for_visual_cells` but lives here
/// because the public copy belongs to mouse hit-testing.  See that
/// function for the wide-char snap-past rule and forbidden indent zone.
fn char_in_row_at_cell(
    text: &str,
    row: (usize, usize, usize),
    target_cell: usize,
    indent: usize,
    is_last_row: bool,
) -> usize {
    let (start, end, next_start) = row;
    let max_char_in_row = if is_last_row {
        end
    } else {
        next_start.saturating_sub(1).max(start)
    };
    let row_chars = text.chars().skip(start).take(end - start);
    let in_row = line_render::char_idx_at_cell_col(row_chars, target_cell, indent);
    (start + in_row).min(max_char_in_row)
}

/// Rendered/Preview click: walk through rendered lines from `state.scroll`,
/// accumulating each line's visual-row count, and find which rendered line
/// and sub-row the click landed on.  Then map that rendered sub-line back to
/// a source byte using the source map.
fn rendered_click_to_offset(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> usize {
    let lines = &state.parsed.lines;
    if lines.is_empty() {
        return 0;
    }
    let (mut idx, mut first_sub_row) =
        state.rendered_line_at_visual_row(state.scroll, viewport_width);
    let mut y = 0usize;
    while idx < lines.len() {
        // Per-line lookup against `ParsedDoc`'s O(1) visual-row cache —
        // historically this called `visual_rows_for_line` directly, which
        // adds up on rapid mouse-move events over a long document.
        let rows_used = state
            .parsed
            .visual_rows_for_line_at(idx, viewport_width)
            .max(1);
        let used = rows_used.saturating_sub(first_sub_row).max(1);
        if row < y + used {
            let sub_row = first_sub_row + row - y;
            return rendered_sub_line_to_offset(state, idx, sub_row, col, viewport_width);
        }
        y += used;
        idx += 1;
        first_sub_row = 0;
    }
    state.buffer.len_chars()
}

/// Map `(rendered_line_idx, sub_row_within_line, col)` to a buffer char
/// offset.
///
/// Strategy:
/// 1. Look up the block that produced the rendered line.
/// 2. Compute which raw source line within the block corresponds to this
///    rendered line (skipping the table-top border row when relevant).
/// 3. Within that raw source line, advance `col` chars and convert to a char
///    offset on the rope.
///
/// For blocks with inline formatting (`**bold**` rendering as `bold`), the
/// rendered column may diverge slightly from the raw column.  Given the
/// Phase 1 reveal semantics turn the cursor's line into raw text within
/// `RAW_REVEAL_DELAY`, the click lands at an approximate position that the
/// user can refine with a second click if needed.
pub fn rendered_sub_line_to_offset(
    state: &EditorState,
    rendered_line_idx: usize,
    sub_row_within_line: usize,
    col: usize,
    viewport_width: usize,
) -> usize {
    let buffer_len = state.buffer.len_chars();
    let source = state.buffer.contents();
    let Some(block_start_byte) = state
        .parsed
        .source_map
        .original_byte_for_rendered_line(rendered_line_idx)
    else {
        return buffer_len;
    };
    let Some(block_range) = state
        .parsed
        .source_map
        .original_range_for_byte(block_start_byte)
    else {
        return buffer_len;
    };
    let block_end = block_range.end.min(source.len());
    // Tolerate stale source-map ranges that land mid-grapheme: when a
    // pending in-line edit has shifted byte offsets after the cursor,
    // direct slicing would panic at the char-boundary check.  Mouse
    // dispatch flushes the parse before reaching here so the empty-
    // string fallback is defence-in-depth, not the routine path.
    let block_text = source.get(block_range.start..block_end).unwrap_or("");

    // How deep into the block's rendered lines did we click?
    let rendered_span = state
        .parsed
        .source_map
        .rendered_lines_for_byte(block_start_byte);
    let sub_idx_in_block = rendered_line_idx.saturating_sub(rendered_span.start);

    // Table click → raw-row index.  Phase 13: classify the rendered
    // sub-line by leading box-drawing glyph instead of relying on a
    // fixed alternating-line pattern, since data rows may now span
    // multiple rendered lines after cell-wrap.
    let is_table = table_edit::is_table_block(block_text);
    let raw_line_idx = if is_table {
        let block_lines = state
            .parsed
            .lines
            .get(rendered_span.start..rendered_span.end.min(state.parsed.lines.len()))
            .unwrap_or(&[]);
        let kinds = crate::ui::table_view::classify_table_sub_lines(block_lines);
        match kinds.get(sub_idx_in_block) {
            Some(crate::ui::table_view::TableSubLineKind::TopBorder)
            | Some(crate::ui::table_view::TableSubLineKind::Header { .. }) => 0, // header line
            Some(crate::ui::table_view::TableSubLineKind::ThickSeparator) => 2, // alignment-row → first data row
            Some(crate::ui::table_view::TableSubLineKind::DataRow { row, .. }) => row + 2,
            Some(crate::ui::table_view::TableSubLineKind::ThinSeparator) => {
                // A separator click snaps to the data row immediately
                // preceding it.  Walk back through `kinds` to find it.
                let mut row = 0usize;
                for k in &kinds[..sub_idx_in_block] {
                    if let crate::ui::table_view::TableSubLineKind::DataRow { row: r, .. } = k {
                        row = *r;
                    }
                }
                row + 2
            }
            Some(crate::ui::table_view::TableSubLineKind::BottomBorder) | None => {
                // Bottom border or out-of-range — snap to the last data
                // row.  Total data rows = info.rows.len() - 2 (header +
                // alignment).  Tables always have at least one data row
                // for `is_table_block` to be true.
                let last_data = block_text.split('\n').count().saturating_sub(2);
                last_data.max(2)
            }
        }
    } else {
        sub_idx_in_block
    };

    // Blank-line "virtual blocks" have no content.  The renderer produces
    // a single empty line for them; place the cursor at block start.
    if block_text.is_empty() {
        return state.buffer.rope().byte_to_char(block_range.start);
    }

    // Walk raw source lines to find the byte start of the target raw line.
    let mut byte_cursor = 0usize;
    let mut line_byte_start = 0usize;
    let mut line_byte_end = block_text.len();
    let mut found_idx = 0usize;
    for (i, line) in block_text.split('\n').enumerate() {
        if i == raw_line_idx {
            line_byte_start = byte_cursor;
            line_byte_end = byte_cursor + line.len();
            found_idx = i;
            break;
        }
        byte_cursor += line.len() + 1;
        if byte_cursor >= block_text.len() {
            // Clamp when raw_line_idx points past the block's last line.
            line_byte_start = byte_cursor.saturating_sub(line.len() + 1);
            line_byte_end = block_text.len();
            found_idx = i;
            break;
        }
    }
    let line_text = &block_text[line_byte_start..line_byte_end];
    let rendered_line = &state.parsed.lines[rendered_line_idx];

    // Tables: rendered cells are padded to layout width, so a simple col →
    // char mapping lands clicks on the wrong cell whenever the rendered cell
    // is wider than its raw counterpart.  Map through the pipe positions
    // instead so the click stays inside the cell the user clicked on.
    let raw_col = if is_table && rendered_line.spans.iter().any(|s| s.content.contains('│')) {
        let row_width = line_row_width(rendered_line, sub_row_within_line);
        let clamped_col = col.min(row_width);
        if let Some(c) = table_click_to_raw_col(line_text, rendered_line, clamped_col) {
            c
        } else {
            clamped_col
        }
    } else {
        // Non-table click: walk the rendered line's wrap layout to find
        // which sub-row the click landed on, then translate the click's
        // cell column into a char position using the cell-aware mapping
        // (wide-char snap-past, hanging-indent forbidden zone).  Falls
        // back to row 0 if the rendered line had fewer wrap rows than the
        // sub_row_within_line we were told.
        let indent = line_render::compute_hanging_indent(rendered_line);
        let rendered_chars: Vec<(char, ratatui::style::Style)> = rendered_line
            .spans
            .iter()
            .flat_map(|span| span.content.chars().map(move |c| (c, span.style)))
            .collect();
        let viewport = viewport_width.max(1);
        let rows = line_render::visual_rows_of_chars(&rendered_chars, viewport, indent);
        let sub = sub_row_within_line.min(rows.len().saturating_sub(1));
        let (start, end, next_start) = rows.get(sub).copied().unwrap_or((0, 0, 0));
        let row_indent = if sub == 0 { 0 } else { indent };
        let is_last_row = sub + 1 == rows.len();
        let max_in_row = if is_last_row {
            end
        } else {
            next_start.saturating_sub(1).max(start)
        };
        let row_chars = rendered_chars
            .iter()
            .skip(start)
            .take(end - start)
            .map(|(c, _)| *c);
        let in_row = line_render::char_idx_at_cell_col(row_chars, col, row_indent);
        let rendered_idx = (start + in_row).min(max_in_row);

        // Translate the rendered char index back to a raw char column on
        // `line_text`.  For lines whose rendered form drops or transforms
        // syntax characters (links, code spans), the rendered→raw map
        // makes the cursor land where the user clicked rather than at the
        // matching rendered char's *position* in the raw text.  When the
        // map's rendered length doesn't match the line's actual rendered
        // length (headings/lists/blockquotes have prefix glyphs the map
        // doesn't model) we fall back to the 1:1 column mapping that's
        // been in use since Phase 5.
        let actual_rendered_count = rendered_chars.len();
        let map = rendered_to_raw_char_map(line_text);
        if map.len().saturating_sub(1) == actual_rendered_count {
            map.get(rendered_idx)
                .copied()
                .unwrap_or_else(|| line_text.chars().count())
        } else {
            rendered_idx
        }
    };

    // Advance `raw_col` chars into the raw line.
    let line_char_count = line_text.chars().count();
    let raw_col = raw_col.min(line_char_count);
    let mut byte_offset_in_line = 0usize;
    for (char_idx, ch) in line_text.chars().enumerate() {
        if char_idx == raw_col {
            break;
        }
        byte_offset_in_line += ch.len_utf8();
    }
    let max_byte_in_line = line_text.len();
    let byte_in_block = line_byte_start + byte_offset_in_line.min(max_byte_in_line);
    let absolute_byte = block_range.start + byte_in_block.min(block_text.len());

    let _ = found_idx;
    state
        .buffer
        .rope()
        .byte_to_char(absolute_byte.min(source.len()))
        .min(buffer_len)
}

/// Cell-aware mapping from a rendered column to a raw column for table rows.
///
/// Locates the cell the click falls in by walking the rendered line's `│`
/// positions, then maps the click's position *within* the rendered cell to
/// the matching raw cell:
/// - clicks on actual content chars map 1:1 to the raw content char,
/// - clicks on leading padding land on the first raw content char,
/// - clicks on trailing padding land just past the last non-whitespace char
///   in the raw cell so the cursor never jumps into the next cell.
///
/// Returns `None` when the line doesn't parse as a table row (alignment
/// separator, border) — caller falls back to the default char-by-char map.
fn table_click_to_raw_col(
    raw_line: &str,
    rendered_line: &Line<'_>,
    rendered_col: usize,
) -> Option<usize> {
    let raw_pipes = table_layout::raw_pipe_positions(raw_line);
    let rendered_pipes = table_layout::rendered_pipe_positions(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }
    let col_count = rendered_pipes.len() - 1;

    // Which cell does `rendered_col` fall in?  Cell `i` spans
    // (rendered_pipes[i] + 1) .. rendered_pipes[i + 1] (content area).
    let cell_idx = (0..col_count)
        .find(|&i| rendered_col < rendered_pipes[i + 1])
        .unwrap_or(col_count - 1);
    let rend_cell_start = rendered_pipes[cell_idx] + 1;
    let rend_cell_end = rendered_pipes[cell_idx + 1];
    let raw_cell_start = raw_pipes[cell_idx] + 1;
    let raw_cell_end = raw_pipes[cell_idx + 1];

    let raw_cell_text: String = raw_line
        .chars()
        .skip(raw_cell_start)
        .take(raw_cell_end - raw_cell_start)
        .collect();

    // Clamp the click into the rendered cell's span so clicks on the opening
    // pipe land at the start of the cell's content.
    let _ = rend_cell_end;
    let clicked = rendered_col.max(rend_cell_start);
    let rend_offset_in_cell = clicked.saturating_sub(rend_cell_start);

    // Partition the raw cell into leading-ws / content / trailing-ws.
    let raw_chars: Vec<char> = raw_cell_text.chars().collect();
    let raw_leading = raw_chars.iter().take_while(|c| c.is_whitespace()).count();
    let raw_trailing = raw_chars
        .iter()
        .rev()
        .take_while(|c| c.is_whitespace())
        .count();
    let content_chars = raw_chars.len().saturating_sub(raw_leading + raw_trailing);

    // The renderer always emits exactly one leading space before the cell
    // content (see `render_table_row`).  A click on that leading space should
    // land on the first raw content char; clicks past the content's last
    // non-whitespace char clamp to "just past last content char" so the
    // cursor never jumps into the next cell via trailing padding.
    let raw_offset_in_cell = if rend_offset_in_cell <= 1 {
        raw_leading
    } else {
        let content_col = rend_offset_in_cell - 1;
        raw_leading + content_col.min(content_chars)
    };

    Some(raw_cell_start + raw_offset_in_cell)
}

/// Width in cells of visual sub-row `sub_row` of the rendered line.  Clicks
/// past the line's content are clamped to this bound before being mapped into
/// the raw source, so the user can click "past the end" and still land at the
/// line's last valid cursor position.
///
/// Currently returns the full character count of the line regardless of
/// sub-row — a conservative upper bound that keeps clicks off the next line.
/// A precise per-row bound would require re-running the line-wrap algorithm
/// here; the conservative bound is correct at the character level and only
/// loses precision for clicks deep in the padding of wrapped lines.
fn line_row_width(line: &Line<'_>, _sub_row: usize) -> usize {
    line.spans.iter().map(|s| s.content.chars().count()).sum()
}

/// Inverse of [`rendered_to_raw_char_map`] for a paragraph-style line:
/// given a raw char column on `raw_line`, return the rendered char
/// column it corresponds to on `rendered_line`.  Used by the jitter-
/// delay cursor overlay (`RenderedView`) so the cursor indicator lands
/// at the same visual column the click handler placed it — without
/// this, the indicator briefly draws at the raw column (e.g. col 1 of
/// the rendered "File link", on `i`) before the raw reveal switches
/// the line to its raw form (col 1 of `[File link]`, on `F`), and the
/// cursor visibly jumps.
///
/// Returns `None` when the rendered count of `rendered_line` doesn't
/// match the rendered count produced by `rendered_to_raw_char_map`
/// (headings/lists/blockquotes/highlights — caller falls back to a
/// 1:1 mapping, matching the click-handler's fallback).
pub fn paragraph_raw_col_to_rendered_col(
    raw_line: &str,
    rendered_line: &Line<'_>,
    raw_col: usize,
) -> Option<usize> {
    let actual_rendered_count: usize = rendered_line
        .spans
        .iter()
        .map(|s| s.content.chars().count())
        .sum();
    let map = rendered_to_raw_char_map(raw_line);
    if map.len().saturating_sub(1) != actual_rendered_count {
        return None;
    }
    // Map entries are non-decreasing (each rendered char's raw position
    // strictly advances).  Find the smallest rendered idx whose raw
    // position is `>= raw_col`.  When `raw_col` lands on a non-rendered
    // marker (e.g. the `[` of `[link]`) this returns the rendered idx
    // immediately after the marker — the same place the click handler
    // would have parked the cursor.
    let pos = map
        .iter()
        .position(|&r| r >= raw_col)
        .unwrap_or(map.len() - 1);
    Some(pos.min(actual_rendered_count))
}

/// Build a map from rendered character index → raw character index on a
/// single source line.
///
/// The renderer drops or transforms certain syntax characters: a link's
/// `[`, `](url)` markers leave only the bracket text on screen; a code
/// span's backticks become surrounding spaces.  As a result, the rendered
/// column the user clicked at doesn't correspond directly to the same
/// column in the raw text — clicks inside `File link` (rendered) are off
/// by one against `[File link](./plan.md)` (raw), and clicks past the
/// rendered end of the line land mid-URL instead of at the raw line's
/// end.
///
/// This map is built by re-parsing `raw_line` with `pulldown-cmark` and
/// recording the raw byte position of every rendered character emitted
/// by inline `Text`, `Code`, and `SoftBreak`/`HardBreak` events.  Marker
/// bytes (asterisks, brackets, the URL portion of a link) sit in the
/// gaps between events and are correctly skipped.
///
/// The returned vector has length `rendered_char_count + 1`: entry `i`
/// is the raw char index that produced rendered char `i`, and the final
/// entry is the raw char index just past the last rendered char (so a
/// click past the rendered end maps to the line's raw end).
///
/// Caller is responsible for falling back to a 1:1 mapping when the
/// returned length doesn't match the actual rendered char count of the
/// line (e.g. for headings/list items/blockquotes whose rendered prefix
/// glyphs aren't represented in the raw text).
pub(super) fn rendered_to_raw_char_map(raw_line: &str) -> Vec<usize> {
    use pulldown_cmark::{Event, Options, Parser};

    // Build a byte→char index lookup so events can report their offsets
    // in raw bytes (pulldown-cmark's native unit) and we can translate
    // those back to char indices that our caller and `line_text` work
    // in.  The trailing `byte_to_char[raw_line.len()] = total_chars`
    // entry covers the past-end sentinel.
    let mut byte_to_char = vec![0usize; raw_line.len() + 1];
    let mut char_idx = 0usize;
    for (byte_idx, _) in raw_line.char_indices() {
        byte_to_char[byte_idx] = char_idx;
        char_idx += 1;
    }
    byte_to_char[raw_line.len()] = char_idx;
    let total_chars = char_idx;

    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION;

    let mut map: Vec<usize> = Vec::new();

    for (event, range) in Parser::new_ext(raw_line, opts).into_offset_iter() {
        let lookup = |byte: usize| {
            byte_to_char
                .get(byte.min(byte_to_char.len().saturating_sub(1)))
                .copied()
                .unwrap_or(total_chars)
        };

        match event {
            Event::Text(_s) => {
                let slice_end = range.end.min(raw_line.len());
                let raw_slice = &raw_line[range.start..slice_end];
                let mut byte_cursor = range.start;
                let mut rest = raw_slice;
                loop {
                    match rest.find("==") {
                        None => break,
                        Some(start) => {
                            let after_open = &rest[start + 2..];
                            match after_open.find("==") {
                                None => break,
                                Some(rel_end) => {
                                    for c in rest[..start].chars() {
                                        map.push(lookup(byte_cursor));
                                        byte_cursor += c.len_utf8();
                                    }
                                    byte_cursor += 2; // skip opening ==
                                    for c in after_open[..rel_end].chars() {
                                        map.push(lookup(byte_cursor));
                                        byte_cursor += c.len_utf8();
                                    }
                                    byte_cursor += 2; // skip closing ==
                                    rest = &after_open[rel_end + 2..];
                                }
                            }
                        }
                    }
                }
                for c in rest.chars() {
                    map.push(lookup(byte_cursor));
                    byte_cursor += c.len_utf8();
                }
            }
            // Code spans render as `" <inner> "` — the opening and closing
            // backticks become surrounding spaces.  Map the leading space
            // to the opening backtick, the inner text 1:1, and the
            // trailing space to the closing backtick.
            Event::Code(s) => {
                map.push(lookup(range.start));
                let mut byte = range.start + 1;
                for c in s.chars() {
                    map.push(lookup(byte));
                    byte += c.len_utf8();
                }
                map.push(lookup(range.end.saturating_sub(1)));
            }
            // Soft- and hard-breaks render as a single space character.
            Event::SoftBreak | Event::HardBreak => {
                map.push(lookup(range.start));
            }
            // Inline tags (`Strong`, `Emphasis`, `Strikethrough`, `Link`)
            // are handled implicitly: their inner `Text` events walk the
            // content, while the marker bytes (`**`, `*`, `~~`, `[`,
            // `](url)`) sit in the gaps that no `Text` event covers and
            // never get pushed.
            _ => {}
        }
    }

    map.push(total_chars);
    map
}
