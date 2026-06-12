use ratatui::text::Line;

use crate::editor::EditorState;
use crate::markdown::table_layout::{
    raw_pipe_positions, rendered_pipe_positions, wrap_cell_with_indices, CellOverlay,
};

/// Cell-scoped overlay for cells whose raw markdown is too wide to fit
/// in the rendered cell.  Wraps the cell's source bytes onto chunks of
/// the cell's full width, picks the chunk the cursor is on, and
/// returns it as a normal `CellOverlay` so the existing painter can
/// stamp it directly onto the rendered table row.
///
/// Effect: while the cursor is in the cell, the cell horizontally
/// scrolls (per character typed) — the chunk the cursor is on stays
/// visible, with the rest of the source paged off-screen.  Switching
/// to Raw mode is the canonical way to see the entire raw cell at
/// once.
///
/// Hard-wrap (one-char-per-step) rather than word-aware wrap because
/// (a) cursor → chunk mapping is then trivial (`offset / cell_width`),
/// (b) the chunks are predictable as the user types, and
/// (c) word-boundary breaks would force the cursor to jump to a new
/// chunk mid-word, which is jarring during line editing.
pub(super) fn compute_cell_chunk_overlay(
    raw_row: &str,
    rendered_line: &Line<'_>,
    cursor_col_raw: usize,
) -> Option<CellOverlay> {
    let raw_pipes = raw_pipe_positions(raw_row);
    let rendered_pipes = rendered_pipe_positions(rendered_line);
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
    if raw_chars.len() <= cell_width {
        // Cell content fits — `compute_cell_overlay` should have been
        // chosen instead.  Return None so the caller falls through.
        return None;
    }

    // Hard-wrap by `cell_width`.  Cursor's chunk + col-in-chunk are
    // straight integer division / modulo of the cursor's offset
    // within the cell.
    let cursor_in_cell = cursor_col_raw.saturating_sub(raw_cell_start);
    let total_chunks = raw_chars.len().div_ceil(cell_width);
    let max_chunk_idx = total_chunks.saturating_sub(1);
    let chunk_idx = (cursor_in_cell / cell_width).min(max_chunk_idx);
    let col_in_chunk = (cursor_in_cell - chunk_idx * cell_width).min(cell_width.saturating_sub(1));

    let chunk_start_chars = chunk_idx * cell_width;
    let chunk_end_chars = (chunk_start_chars + cell_width).min(raw_chars.len());
    let chunk: String = raw_chars[chunk_start_chars..chunk_end_chars]
        .iter()
        .collect();

    // Selection mapping: byte offset of the chunk's first char inside
    // `raw_row`.  Selection bytes are then intersected with
    // [chunk_byte_start, chunk_byte_start + chunk.len()) and mapped to
    // chars within the chunk.
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

/// Information about the cursor's position inside a *wrapped* table
/// cell — i.e. one whose content broke onto multiple rendered
/// sub-lines because it overflowed the column's allocated width.
///
/// Used by `RenderedView::render` to:
/// 1. Push `cursor_rendered_line` from the row's first sub onto the
///    sub the cursor actually occupies (`sub_offset`).
/// 2. Place the cursor indicator at the right rendered column
///    (`visual_col`) on that sub.
pub(super) struct WrappedCellOverlay {
    /// Sub-line index in `editor.parsed.lines` of the cell's row's
    /// first rendered sub.
    pub(super) row_first_line_idx: usize,
    /// Per-chunk overlay info — one entry per rendered sub-line of the
    /// row.  Index `i` is painted on
    /// `editor.parsed.lines[row_first_line_idx + i]`.  Each entry is
    /// already shaped for `overlay_raw_cell` (rendered_start shifted
    /// for continuation chunks, cursor_in_cell only on the cursor's
    /// chunk).  When the raw text wraps to fewer chunks than the row's
    /// rendered height, the trailing entries are blank (`raw_text`
    /// empty) so the painter wipes the cell's stale rendered tail.
    pub(super) subs: Vec<CellOverlay>,
    /// Index within `subs` that contains the cursor.
    pub(super) cursor_sub: usize,
    /// Document-area-relative rendered column for the cursor.  Used by
    /// the jitter-delay branch to draw the cursor indicator at the
    /// same column the reveal-time overlay will use, so there's no
    /// jump when the reveal fires.
    pub(super) visual_col: usize,
}

/// Resolve the cursor's wrapped-cell layout — one `CellOverlay` per
/// rendered sub-line of the row, mapping the wrap chunks of the raw
/// cell text onto the rendered sub-lines.  Returns `None` for single-
/// sub-line cells in single-sub-line rows (existing single-sub
/// `compute_cell_overlay` / `compute_cell_chunk_overlay` paths handle
/// those).
///
/// The raw cell text is wider than its rendered form (backticks,
/// emphasis markers, link URLs are markers the renderer drops), so it
/// routinely wraps to *more* chunks than the rendered row has
/// sub-lines.  In a multi-sub row the overlay then scrolls vertically:
/// a `row_height`-chunk window containing the cursor's chunk is mapped
/// onto the row's sub-lines, so the raw text keeps wrapping and no
/// sub-line is left showing the stale rendered tail.  Single-sub rows
/// still return `None` and fall back to `compute_cell_chunk_overlay`'s
/// horizontal scroll.
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

    // Find the row's first sub and how many sub-lines it spans.
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

    // Pipe geometry: the row's first sub-line carries the column ranges
    // (every wrap sub-line of the same row has identical pipe positions
    // by construction in `render_table_row`).
    let first_line = block_lines.get(row_start_local)?;
    let rendered_pipes = rendered_pipe_positions(first_line);
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

    // Cell's raw + rendered ranges.
    let raw_cell_start_char = raw_pipes[cell_idx] + 1;
    let raw_cell_end_char = raw_pipes[cell_idx + 1];
    let raw_cell_text: String = raw_row
        .chars()
        .skip(raw_cell_start_char)
        .take(raw_cell_end_char - raw_cell_start_char)
        .collect();
    let cell_rendered_start = rendered_pipes[cell_idx] + 1;
    let cell_rendered_end = rendered_pipes[cell_idx + 1];
    // Effective content width = rendered cell width − 2 leading/trailing
    // padding spaces the renderer always emits around cell content.
    let content_width = cell_rendered_end
        .saturating_sub(cell_rendered_start)
        .saturating_sub(2);
    if content_width == 0 {
        return None;
    }

    // Wrap the cell's *trimmed* content — the pipe-padding whitespace
    // around it belongs to the rendered pad columns, not the content
    // area.  Wrapping it inflates the chunk count (a trailing pad
    // becomes a lone-space chunk that wastes a sub-line) and, more
    // importantly, diverges from the click mapper
    // (`coord::table_click_to_raw_col`), which wraps the trimmed text:
    // identical input keeps the overlay's chunk layout in lockstep
    // with the chunk a click resolves the cursor into.
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

    // Re-run the renderer's word-wrap so we know which sub-line + col
    // the cursor's char index lands on.  Word-wrap drops whitespace at
    // break points, so a cursor on dropped whitespace maps to the start
    // of the next visible row.
    let wrapped = wrap_cell_with_indices(&trimmed, content_width);
    if wrapped.is_empty() {
        return None;
    }

    // Single-sub rows: leave a fitting cell to `compute_cell_overlay`
    // and an overflowing one to `compute_cell_chunk_overlay`'s
    // horizontal scroll.  (Multi-sub rows whose raw wraps beyond the
    // rendered height scroll vertically below — falling back there
    // would replace only one sub-line and leave the cell's other
    // rendered wrap rows painted as a stale tail.)
    if row_height <= 1 {
        return None;
    }

    // Locate cursor: which chunk + col within that chunk.  Offsets are
    // relative to the trimmed content; a cursor on the leading pad
    // clamps to the first content char.
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

    // The raw text can wrap to MORE chunks than the row has rendered
    // sub-lines (backticks and other marker bytes make raw wider than
    // rendered).  Scroll vertically: map a `row_height`-chunk window
    // containing the cursor's chunk onto the row's sub-lines.
    // Bottom-anchored minimal scroll — the click mapper resolves a
    // click on sub-line `s` to chunk `s`, so any chunk below
    // `row_height` must stay on its own sub-line or the text jumps
    // upward the moment the reveal fires.
    let window_start = cursor_sub.saturating_sub(row_height - 1);
    let window = &wrapped[window_start..(window_start + row_height).min(wrapped.len())];
    let cursor_sub = cursor_sub - window_start;

    // raw_row char index → byte offset.  +1 sentinel so we can index
    // past the last char without panicking.
    let raw_row_byte_at: Vec<usize> = raw_row
        .char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(raw_row.len()))
        .collect();

    let mut subs: Vec<CellOverlay> = Vec::with_capacity(window.len());
    for (i, (start_in_cell, chunk_text)) in window.iter().enumerate() {
        // Chunks carry trimmed content only, so every chunk paints one
        // column right of the cell edge — the rendered ' ' the renderer
        // already drew in the leading-pad column shows through.
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

    // The raw cell can wrap to FEWER chunks than the row has rendered
    // sub-lines — the styled wrap and the raw wrap break differently,
    // and an in-line edit can shrink the raw text while the rendered
    // row height is still the pre-edit parse's.  Pad with blank
    // overlays so `overlay_raw_cell`
    // wipes the cell's area on those leftover sub-lines; without this,
    // the de-rendered cell's stale rendered wrap tail stays on screen
    // below the raw chunks.
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

    let visual_col = subs[cursor_sub].rendered_start + cursor_col_in_chunk;

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

    /// Raw cell text is wider than its rendered form (the backticks are
    /// dropped on render), so it can wrap to more chunks than the row's
    /// rendered height.  The overlay must still take the multi-sub path
    /// and scroll vertically — returning `None` would drop to the
    /// single-sub chunk overlay, which replaces only the cursor's
    /// sub-line and leaves the row's other rendered wrap rows on screen
    /// as an orphaned stale tail.
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

        // Every sub-line of the row carries a raw chunk — no blank or
        // stale rendered tail — and the backtick delimiters are visible.
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

    /// Clicking the SECOND wrap sub-line of a code span must keep the
    /// cursor's chunk on that sub-line and the span's start visible on
    /// the first.  Pre-fix, the overlay wrapped the raw cell text
    /// untrimmed (the pipe-padding spaces inflated the chunk count and
    /// left a lone-space chunk) and top-anchored the scroll window, so
    /// a click on the second line yanked the cursor's chunk up to the
    /// first sub-line with the span's start scrolled invisibly away.
    #[test]
    fn click_on_second_wrap_line_keeps_span_start_visible() {
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let src = "| a | b |\n|---|---|\n| x | `tracing-appender` |\n";
        let mut state = crate::editor::EditorState::new(Buffer::from_str(src), theme);
        state.set_viewport_width(17);

        let lines_range = 0..state.parsed.lines.len();
        let raw_row = "| x | `tracing-appender` |";
        // Cursor on the 'a' of "appender" — rendered on the row's
        // second wrap sub-line ("tracing-" / "appender").
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
