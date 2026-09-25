use ratatui::text::Line;

use crate::editor::EditorState;
use crate::markdown::table_layout::{
    cells_of, hard_wrap_ranges, last_cluster_start, raw_pipe_positions, rendered_pipe_cells,
    rendered_pipe_positions, str_cells, wrap_cell_with_indices, CellOverlay,
};

/// Overlay for a cell whose raw markdown is wider than its rendered cell: hard-wraps the
/// source into chunks of at most `cell_width` cells and returns the chunk under the cursor, so
/// the cell scrolls horizontally as the user types (Raw mode shows the whole cell).
///
/// Hard-wrap ([`hard_wrap_ranges`]) rather than word-wrap so chunk boundaries depend only on
/// glyph widths and the cursor never jumps chunks mid-word.
pub(super) fn compute_cell_chunk_overlay(
    raw_row: &str,
    rendered_line: &Line<'_>,
    cursor_col_raw: usize,
) -> Option<CellOverlay> {
    let raw_pipes = raw_pipe_positions(raw_row);
    let rendered_pipes = rendered_pipe_cells(rendered_line);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }

    let col_count = raw_pipes.len() - 1;
    let preceding = raw_pipes
        .iter()
        .take_while(|&&p| p < cursor_col_raw)
        .count();
    let cell_idx = preceding.saturating_sub(1).min(col_count - 1);

    let raw_cell_start = raw_pipes[cell_idx] + 1;
    let raw_cell_end = raw_pipes[cell_idx + 1];
    let raw_cell_text: String = raw_row
        .chars()
        .skip(raw_cell_start)
        .take(raw_cell_end - raw_cell_start)
        .collect();
    let rendered_start = rendered_pipes[cell_idx] + 1;
    let rendered_end = rendered_pipes[cell_idx + 1];
    let cell_width = rendered_end.saturating_sub(rendered_start);
    if cell_width == 0 {
        return None;
    }

    let raw_chars: Vec<char> = raw_cell_text.chars().collect();
    if str_cells(&raw_cell_text) <= cell_width {
        // Fits — `compute_cell_overlay` should have been chosen; let the caller fall through.
        return None;
    }

    let chunks = hard_wrap_ranges(&raw_chars, cell_width);
    let cursor_in_cell = cursor_col_raw.saturating_sub(raw_cell_start);
    let chunk = chunks
        .iter()
        .rfind(|r| r.start <= cursor_in_cell)
        .unwrap_or(&chunks[0]);
    let chunk_start_chars = chunk.start;
    let chunk_chars = &raw_chars[chunk.clone()];
    let chunk: String = chunk_chars.iter().collect();
    // A cursor past a full chunk's last cell shows on its last glyph.
    let mut col_in_chunk = cursor_in_cell - chunk_start_chars;
    if cells_of(&chunk_chars[..col_in_chunk.min(chunk_chars.len())]) >= cell_width {
        col_in_chunk = last_cluster_start(chunk_chars);
    }

    // Byte offset of the chunk's first char inside `raw_row`, for selection mapping.
    let chunk_byte_start = raw_row
        .char_indices()
        .nth(raw_cell_start + chunk_start_chars)
        .map(|(b, _)| b)
        .unwrap_or(raw_row.len());

    Some(CellOverlay {
        rendered_start,
        rendered_end,
        raw_text: chunk,
        cursor_in_cell: Some(col_in_chunk),
        raw_cell_byte_start: chunk_byte_start,
    })
}

/// Cursor position inside a *wrapped* table cell (one that broke onto several rendered
/// sub-lines). `RenderedView::render` uses it to move `cursor_rendered_line` onto the right
/// sub and place the cursor indicator at `visual_col`.
pub(super) struct WrappedCellOverlay {
    /// Sub-line index in `editor.parsed.lines` of the row's first rendered sub.
    pub(super) row_first_line_idx: usize,
    /// One entry per rendered sub-line of the row, painted on
    /// `editor.parsed.lines[row_first_line_idx + i]`. Trailing entries are blank when the raw
    /// text wraps to fewer chunks than the row's height, so the painter wipes the stale tail.
    pub(super) subs: Vec<CellOverlay>,
    /// Index within `subs` that contains the cursor.
    pub(super) cursor_sub: usize,
    /// Char index into the rendered row for the cursor indicator (a `cursor_col_override`,
    /// not a cell column); the jitter-delay branch draws it here so nothing jumps when the
    /// reveal fires.
    pub(super) visual_col: usize,
}

/// One `CellOverlay` per rendered sub-line of the cursor's row, mapping word-wrap chunks of the
/// raw cell text onto the sub-lines. Returns `None` for single-sub rows, which
/// `compute_cell_overlay` / `compute_cell_chunk_overlay` handle.
///
/// Raw text is wider than rendered (markers the renderer drops), so it routinely wraps to more
/// chunks than the row has sub-lines; the overlay then scrolls a `row_height`-chunk window
/// containing the cursor's chunk onto the row.
pub(super) fn compute_wrapped_cell_overlay(
    editor: &EditorState,
    block_lines_range: std::ops::Range<usize>,
    data_row_idx: usize,
    cursor_col_raw: usize,
    raw_block_source: &str,
) -> Option<WrappedCellOverlay> {
    use crate::ui::table_view::{classify_table_sub_lines, TableSubLineKind};

    let block_lines = editor.parsed.lines.get(block_lines_range.clone())?;
    let kinds = classify_table_sub_lines(block_lines);

    let row_start_local = kinds.iter().position(|k| {
        matches!(
            k,
            TableSubLineKind::DataRow { row, sub: 0 } if *row == data_row_idx
        )
    })?;
    let row_height = kinds[row_start_local..]
        .iter()
        .take_while(|k| matches!(k, TableSubLineKind::DataRow { row, .. } if *row == data_row_idx))
        .count();

    // Every wrap sub-line of a row has identical pipe cells (see `render_table_row`); the
    // char positions differ per sub-line once a wide glyph sits left of the cell.
    let first_line = block_lines.get(row_start_local)?;
    let rendered_pipes = rendered_pipe_cells(first_line);
    let raw_row = raw_block_source.split('\n').nth(data_row_idx + 2)?;
    let raw_pipes = raw_pipe_positions(raw_row);
    if raw_pipes.len() < 2 || rendered_pipes.len() != raw_pipes.len() {
        return None;
    }

    let col_count = raw_pipes.len() - 1;
    let preceding = raw_pipes
        .iter()
        .take_while(|&&p| p < cursor_col_raw)
        .count();
    let cell_idx = preceding.saturating_sub(1).min(col_count - 1);

    let raw_cell_start_char = raw_pipes[cell_idx] + 1;
    let raw_cell_end_char = raw_pipes[cell_idx + 1];
    let raw_cell_text: String = raw_row
        .chars()
        .skip(raw_cell_start_char)
        .take(raw_cell_end_char - raw_cell_start_char)
        .collect();
    let cell_rendered_start = rendered_pipes[cell_idx] + 1;
    let cell_rendered_end = rendered_pipes[cell_idx + 1];
    // Minus the leading/trailing padding space the renderer emits around cell content.
    let content_width = cell_rendered_end
        .saturating_sub(cell_rendered_start)
        .saturating_sub(2);
    if content_width == 0 {
        return None;
    }

    // Wrap the *trimmed* content: the pad whitespace belongs to the rendered pad columns, and
    // the click mapper (`coord::table_click_to_raw_col`) wraps trimmed text too — identical
    // input keeps the overlay's chunks in lockstep with the chunk a click resolves into.
    let raw_chars: Vec<char> = raw_cell_text.chars().collect();
    let raw_leading = raw_chars.iter().take_while(|c| c.is_whitespace()).count();
    let raw_trailing = raw_chars
        .iter()
        .rev()
        .take_while(|c| c.is_whitespace())
        .count();
    let content_chars = raw_chars.len().saturating_sub(raw_leading + raw_trailing);
    let trimmed: String = raw_chars[raw_leading..raw_leading + content_chars]
        .iter()
        .collect();

    // Word-wrap drops whitespace at break points, so a cursor on dropped whitespace maps to
    // the start of the next visible row.
    let wrapped = wrap_cell_with_indices(&trimmed, content_width);
    if wrapped.is_empty() {
        return None;
    }

    // Single-sub rows fall back to the horizontal-scroll overlays. Multi-sub rows must not:
    // replacing one sub-line would leave the others painted as a stale tail.
    if row_height <= 1 {
        return None;
    }

    // Offsets are relative to the trimmed content; a cursor on the leading pad clamps to the
    // first content char.
    let cursor_in_cell = cursor_col_raw.saturating_sub(raw_cell_start_char + raw_leading);
    let last_idx = wrapped.len() - 1;
    let mut cursor_sub = last_idx;
    let mut cursor_col_in_chunk = wrapped[last_idx].1.chars().count();
    for (i, (start_idx, row_text)) in wrapped.iter().enumerate() {
        let next_start = wrapped.get(i + 1).map(|(s, _)| *s).unwrap_or(usize::MAX);
        if cursor_in_cell < next_start {
            cursor_sub = i;
            let row_chars = row_text.chars().count();
            let pos_in_row = cursor_in_cell.saturating_sub(*start_idx);
            cursor_col_in_chunk = pos_in_row.min(row_chars);
            break;
        }
    }

    // Bottom-anchored minimal scroll: the click mapper resolves a click on sub-line `s` to
    // chunk `s`, so any chunk below `row_height` must stay on its own sub-line or the text
    // jumps upward when the reveal fires.
    let window_start = cursor_sub.saturating_sub(row_height - 1);
    let window = &wrapped[window_start..(window_start + row_height).min(wrapped.len())];
    let cursor_sub = cursor_sub - window_start;

    // Char index → byte offset, with a sentinel so indexing past the last char is safe.
    let raw_row_byte_at: Vec<usize> = raw_row
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(raw_row.len()))
        .collect();

    let mut subs: Vec<CellOverlay> = Vec::with_capacity(window.len());
    for (i, (start_in_cell, chunk_text)) in window.iter().enumerate() {
        // Chunks are trimmed, so paint one column right of the cell edge and let the
        // renderer's leading pad space show through.
        let painted_start = cell_rendered_start + 1;
        let chunk_first_char_in_row = raw_cell_start_char + raw_leading + start_in_cell;
        let raw_cell_byte_start = raw_row_byte_at
            .get(chunk_first_char_in_row)
            .copied()
            .unwrap_or(raw_row.len());
        let cursor_in_cell = if i == cursor_sub {
            Some(cursor_col_in_chunk.min(chunk_text.chars().count()))
        } else {
            None
        };
        subs.push(CellOverlay {
            rendered_start: painted_start,
            rendered_end: cell_rendered_end,
            raw_text: chunk_text.clone(),
            cursor_in_cell,
            raw_cell_byte_start,
        });
    }

    // Fewer chunks than sub-lines (styled and raw wraps break differently, and an edit can
    // shrink the raw text before the row height is re-parsed): pad with blank overlays so
    // `overlay_raw_cell` wipes the stale rendered tail.
    let cell_end_byte = raw_row_byte_at
        .get(raw_cell_end_char)
        .copied()
        .unwrap_or(raw_row.len());
    while subs.len() < row_height {
        subs.push(CellOverlay {
            rendered_start: cell_rendered_start,
            rendered_end: cell_rendered_end,
            raw_text: String::new(),
            cursor_in_cell: None,
            raw_cell_byte_start: cell_end_byte,
        });
    }

    // Chunks paint one cell past the pipe (the pad), which is two chars on in the row's text.
    let cursor_line = block_lines
        .get(row_start_local + cursor_sub)
        .unwrap_or(first_line);
    let visual_col = rendered_pipe_positions(cursor_line)
        .get(cell_idx)
        .map_or(subs[cursor_sub].rendered_start, |&pipe| pipe + 2)
        + cursor_col_in_chunk;

    Some(WrappedCellOverlay {
        row_first_line_idx: block_lines_range.start + row_start_local,
        subs,
        cursor_sub,
        visual_col,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::document::Buffer;

    /// Chunks are cut by cells, not chars: each fits the cell's six cells, and the cursor lands
    /// in the chunk that holds it.
    #[test]
    fn chunk_overlay_cuts_wide_text_by_cells() {
        let line = Line::from("│ abcd │");
        let raw = "| 日本語日本語 |";
        // Cell text ` 日本語日本語 ` chunks as ` 日本` | `語日本` | `語 `.
        let second_go = raw
            .char_indices()
            .filter(|(_, c)| *c == '語')
            .nth(1)
            .unwrap()
            .0;
        let cursor = raw[..second_go].chars().count();
        let ov = compute_cell_chunk_overlay(raw, &line, cursor).expect("wider than the cell");
        assert_eq!(ov.raw_text, "語 ");
        assert_eq!(ov.cursor_in_cell, Some(0));
        assert_eq!((ov.rendered_start, ov.rendered_end), (1, 7));
        assert_eq!(ov.raw_cell_byte_start, second_go);

        for cursor in 1..raw.chars().count() {
            let ov = compute_cell_chunk_overlay(raw, &line, cursor).unwrap();
            assert!(str_cells(&ov.raw_text) <= 6, "{:?}", ov.raw_text);
            let col = ov.cursor_in_cell.unwrap();
            let before: String = ov.raw_text.chars().take(col).collect();
            assert!(str_cells(&before) < 6, "cursor cell inside the overlay");
        }
    }

    /// A cluster that straddles a chunk boundary moves whole into the next chunk: every chunk
    /// but one holding a single over-wide cluster fits the cell, so the cursor stays on screen.
    #[test]
    fn chunk_overlay_never_packs_a_cluster_past_the_cell() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let line = Line::from("│ abcd │");
        let raw = format!("| ab{family}xyzw |");
        let mut texts = Vec::new();
        for cursor in 1..raw.chars().count() {
            let ov = compute_cell_chunk_overlay(&raw, &line, cursor).expect("wider than the cell");
            assert!(
                str_cells(&ov.raw_text) <= 6 || ov.raw_text == family,
                "{:?}",
                ov.raw_text
            );
            if texts.last() != Some(&ov.raw_text) {
                texts.push(ov.raw_text);
            }
        }
        assert_eq!(texts, vec![" ab", family, "xyzw "]);
    }

    /// A cursor past a full last chunk clamps onto its last glyph — the `e`, not the zero-width
    /// accent after it, which the painter would skip.
    #[test]
    fn chunk_overlay_clamps_a_cursor_onto_the_last_cluster() {
        let line = Line::from("│ abcd │");
        // Cell text ` abcdefghije\u{301}` chunks as ` abcde` | `fghije\u{301}` (six cells each).
        let raw = "| abcdefghije\u{301}|";
        let closing = raw.chars().count() - 1;
        let ov = compute_cell_chunk_overlay(raw, &line, closing).expect("wider than the cell");
        assert_eq!(ov.raw_text, "fghije\u{301}");
        assert_eq!(ov.cursor_in_cell, Some(5));
    }

    /// `visual_col` is a char index into the cursor's sub-line: a wide glyph in the column to
    /// the left puts the cell fewer chars than cells in.
    #[test]
    fn wrapped_overlay_visual_col_counts_chars_past_wide_glyphs() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let src = "| a | b |\n|---|---|\n| 日本 | aa bb cc dd |\n";
        let mut state = crate::editor::EditorState::new(Buffer::from_str(src), theme);
        state.set_viewport_width(16);
        let lines_range = 0..state.parsed.lines.len();
        let raw_row = src.lines().nth(2).unwrap();
        let cursor_col = raw_row.chars().position(|c| c == 'a').unwrap();
        let overlay = compute_wrapped_cell_overlay(&state, lines_range, 0, cursor_col, src)
            .expect("the second column wraps");
        assert_eq!(overlay.cursor_sub, 0);
        let line: String = state.parsed.lines[overlay.row_first_line_idx]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            line.contains('日'),
            "fixture: the glyphs share the cursor's sub-line"
        );
        assert_eq!(line.chars().nth(overlay.visual_col), Some('a'), "{line:?}");
    }

    /// Raw text wider than the rendered height must take the multi-sub path; the single-sub
    /// fallback would leave the row's other wrap rows as a stale tail.
    #[test]
    fn raw_wider_than_rendered_height_scrolls_vertically() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let src = "| a | b |\n|---|---|\n| x | `aa` `bb` `cc` `dd` `ee` |\n";
        let mut state = crate::editor::EditorState::new(Buffer::from_str(src), theme);
        state.set_viewport_width(18);

        let lines_range = 0..state.parsed.lines.len();
        let raw_row = "| x | `aa` `bb` `cc` `dd` `ee` |";
        let cursor_col = raw_row.find("cc").unwrap(); // ASCII: byte == char col

        let overlay = compute_wrapped_cell_overlay(&state, lines_range, 0, cursor_col, src)
            .expect("multi-sub row must use the wrapped-cell overlay, not the chunk fallback");

        assert!(overlay.subs.len() >= 2, "fixture row must wrap");
        assert!(
            overlay.subs.iter().all(|s| !s.raw_text.is_empty()),
            "raw chunks must cover every sub-line: {:?}",
            overlay.subs.iter().map(|s| &s.raw_text).collect::<Vec<_>>()
        );
        let joined: String = overlay.subs.iter().map(|s| s.raw_text.as_str()).collect();
        assert!(
            joined.contains('`'),
            "overlay must reveal raw backticks: {joined:?}"
        );
        assert!(overlay.cursor_sub < overlay.subs.len());
    }

    /// Regression: untrimmed wrapping plus a top-anchored window yanked the cursor's chunk up
    /// to the first sub-line and scrolled the span's start out of view.
    #[test]
    fn click_on_second_wrap_line_keeps_span_start_visible() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let src = "| a | b |\n|---|---|\n| x | `tracing-appender` |\n";
        let mut state = crate::editor::EditorState::new(Buffer::from_str(src), theme);
        state.set_viewport_width(17);

        let lines_range = 0..state.parsed.lines.len();
        let raw_row = "| x | `tracing-appender` |";
        // 'a' of "appender" renders on the row's second wrap sub-line.
        let cursor_col = raw_row.find("appender").unwrap(); // ASCII: byte == char col

        let overlay = compute_wrapped_cell_overlay(&state, lines_range, 0, cursor_col, src)
            .expect("wrapped code-span cell must use the multi-sub overlay");

        assert_eq!(overlay.subs.len(), 2, "fixture row wraps to two sub-lines");
        assert!(
            overlay.cursor_sub > 0,
            "cursor clicked on the second sub-line must stay below the first chunk"
        );
        assert!(
            overlay.subs[0].raw_text.starts_with('`'),
            "the span's start must stay visible on the first sub-line: {:?}",
            overlay.subs.iter().map(|s| &s.raw_text).collect::<Vec<_>>()
        );
        assert!(
            overlay.subs.iter().all(|s| !s.raw_text.is_empty()),
            "no sub-line may be wasted on a pad-space chunk: {:?}",
            overlay.subs.iter().map(|s| &s.raw_text).collect::<Vec<_>>()
        );
    }
}
