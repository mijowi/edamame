/// Integration tests for UI widgets using `ratatui::backend::TestBackend`.
use ratatui::{backend::TestBackend, Terminal};

use edamame::config::Theme;
use edamame::editor::Mode;
use edamame::ui::status_bar::{StatusBar, StatusBarState};

fn render_status_bar(
    mode: Mode,
    filename: &str,
    line_count: usize,
    modified: bool,
    width: u16,
) -> String {
    let theme = Box::leak(Box::new(Theme::default()));
    let backend = TestBackend::new(width, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|frame| {
            let bar = StatusBar {
                state: StatusBarState {
                    mode,
                    filename,
                    line_count,
                    modified,
                    scroll: 0,
                    cursor_line: None,
                    cursor_col: None,
                    selection_size: None,
                },
                theme,
            };
            frame.render_widget(bar, frame.area());
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    (0..width)
        .map(|x| {
            buf.cell((x, 0))
                .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
        })
        .collect()
}

#[test]
fn status_bar_preview_mode() {
    let out = render_status_bar(Mode::Preview, "test.md", 42, false, 60);
    assert!(out.contains("PREVIEW"), "got: {out:?}");
}

#[test]
fn status_bar_filename() {
    let out = render_status_bar(Mode::Preview, "my_notes.md", 10, false, 60);
    assert!(out.contains("my_notes.md"), "got: {out:?}");
}

#[test]
fn status_bar_line_count() {
    let out = render_status_bar(Mode::Preview, "f.md", 123, false, 80);
    assert!(out.contains("123"), "got: {out:?}");
}

#[test]
fn status_bar_modified_flag() {
    let out = render_status_bar(Mode::Preview, "f.md", 5, true, 80);
    assert!(out.contains("[modified]"), "got: {out:?}");
}

#[test]
fn status_bar_clean_no_modified_flag() {
    let out = render_status_bar(Mode::Preview, "f.md", 5, false, 80);
    assert!(!out.contains("[modified]"), "got: {out:?}");
}

#[test]
fn status_bar_raw_mode_label() {
    let out = render_status_bar(Mode::Raw, "f.md", 5, false, 60);
    assert!(out.contains("RAW"), "got: {out:?}");
}

#[test]
fn status_bar_edit_mode_label() {
    let out = render_status_bar(Mode::Rendered, "f.md", 5, false, 60);
    assert!(out.contains("EDIT"), "got: {out:?}");
}

#[test]
fn snapshot_status_bar_preview() {
    let out = render_status_bar(Mode::Preview, "readme.md", 100, false, 80);
    insta::assert_snapshot!(out);
}

#[test]
fn snapshot_status_bar_modified() {
    let out = render_status_bar(Mode::Rendered, "notes.md", 42, true, 80);
    insta::assert_snapshot!(out);
}

#[test]
fn rendered_view_paints_selection_across_multiple_rendered_blocks() {
    use edamame::document::{Buffer, Selection};
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    // Two paragraphs separated by a blank line.  Selection starts mid-first
    // paragraph and ends mid-second.
    let src = "first para here\n\nsecond para here\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    // Selection covers all of "para" in first and "second" in second.
    // Use char offsets that span multiple blocks.
    state.selection = Some(Selection {
        anchor: 6,  // start of "para" in first
        active: 23, // part-way into "second"
    });
    state.cursor.offset = 23;

    let backend = TestBackend::new(25, 5);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                show_table_buttons: false,
                state: &state,
                theme,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    // Row 0 is "first para here" — at least one cell on it must have the
    // selection bg (cells 6+ for "para here").
    let row_has_sel_bg = |y: u16| {
        (0..25u16).any(|x| {
            buf.cell((x, y))
                .map(|c| c.style().bg == theme.selection.bg)
                .unwrap_or(false)
        })
    };
    assert!(row_has_sel_bg(0), "first paragraph row must show selection");
    // Row 2 is the second paragraph.  It should also have selection bg on
    // the covered portion.
    assert!(
        row_has_sel_bg(2),
        "second paragraph row must show selection"
    );
}

#[test]
fn setext_heading_reveals_both_title_and_underline_on_cursor() {
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    // Setext H2 heading followed by a blank line + paragraph so the reveal
    // region is isolated.
    let src = "Title\n-----\n\nBody\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    // Cursor on the underline line (raw line 1).  Leaving `cursor_block_entered_at`
    // as None makes `cursor_block_revealed` return true immediately.
    state.cursor.offset = src.find('-').unwrap();

    let backend = TestBackend::new(20, 5);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                show_table_buttons: false,
                state: &state,
                theme,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let row_text = |y: u16| -> String {
        (0..20u16)
            .map(|x| {
                buf.cell((x, y))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect()
    };
    // Row 0 must show the raw title "Title", not the styled heading glyph.
    assert!(
        row_text(0).starts_with("Title"),
        "row 0 = {:?}",
        row_text(0)
    );
    // Row 1 must show the raw underline "-----", not a rendered rule.
    assert!(
        row_text(1).starts_with("-----"),
        "row 1 = {:?}",
        row_text(1)
    );
}

#[test]
fn rendered_view_selection_in_table_cell_does_not_spill_into_borders() {
    use edamame::document::{Buffer, Selection};
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    // 3-row table; select just the "yy" bytes in the data row.
    let src = "| a | bb | c |\n|---|----|---|\n| x | yy | z |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    let yy_start = src.find("yy").unwrap();
    state.selection = Some(Selection {
        anchor: yy_start,
        active: yy_start + 2,
    });
    // Place cursor elsewhere (col 0 of header) so the cursor block reveal
    // path doesn't overlap the data cell's selection highlight.
    state.cursor.offset = 0;

    let backend = TestBackend::new(30, 6);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                show_table_buttons: false,
                state: &state,
                theme,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let has_sel_bg = |x: u16, y: u16| {
        buf.cell((x, y))
            .map(|c| c.style().bg == theme.selection.bg)
            .unwrap_or(false)
    };
    // Row 0 is the top border (┌─┬─┐) — no selection bg anywhere.
    for x in 0..30u16 {
        assert!(
            !has_sel_bg(x, 0),
            "top border col {x} must not carry selection bg"
        );
    }
    // Row 2 is the separator (├─┼─┤) — no selection bg.
    for x in 0..30u16 {
        assert!(
            !has_sel_bg(x, 2),
            "separator col {x} must not carry selection bg"
        );
    }
    // Row 3 is the data row.  Some cells there MUST carry the selection bg
    // (where "yy" is rendered).  We don't pin exact cols here — the renderer
    // decides layout widths — but at least one cell must be highlighted.
    let data_row_has_sel = (0..30u16).any(|x| has_sel_bg(x, 3));
    assert!(
        data_row_has_sel,
        "data row must show selection bg on the 'yy' cell"
    );
}

#[test]
fn rendered_view_selection_inside_cursors_own_cell_survives_cell_overlay() {
    // Regression for the Phase 5 issue: after click-drag selection within a
    // table cell (or a double-/triple-click that lands the cursor inside its
    // own cell) the highlight used to vanish because the cell overlay
    // repainted the cell's rendered cells with raw text, clobbering whatever
    // `paint_selection_overlay` had drawn.
    use edamame::document::{Buffer, Selection};
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    let src = "| aaa | bbb | ccc |\n|---|---|---|\n| 1 | 2 | 3 |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;

    // Cursor on the first 'b' of the middle header cell — char 8 in `src`.
    // Leaving `cursor_block_entered_at` at None makes `cursor_block_revealed`
    // return true immediately, so the cell overlay path is taken.
    state.cursor.offset = 8;
    // Select "bbb" — raw chars [8..11).
    state.selection = Some(Selection {
        anchor: 8,
        active: 11,
    });

    let backend = TestBackend::new(25, 5);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                show_table_buttons: false,
                state: &state,
                theme,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let has_sel_bg = |x: u16, y: u16| {
        buf.cell((x, y))
            .map(|c| c.style().bg == theme.selection.bg)
            .unwrap_or(false)
    };
    // At least one cell across the active cell's rendered range (cols 7..11 on
    // row 1 — the header with "bbb") must carry the selection bg.  Before the
    // fix, all cells here were painted by `overlay_raw_cell` using only the
    // base style, so the selection bg was lost entirely on this row.
    let any_cell_highlighted = (7u16..=11).any(|x| has_sel_bg(x, 1));
    assert!(
        any_cell_highlighted,
        "selection bg must survive the cell overlay for the cursor's own cell"
    );
}

#[test]
fn rendered_view_cell_scoped_reveal_keeps_neighbouring_pipes_rendered() {
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    let src = "| aaa | bbb | ccc |\n|---|---|---|\n| 1 | 2 | 3 |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    // Place the cursor on the first 'b' of the middle header cell. Chars
    // 0..19 in src form the header row: `| aaa | bbb | ccc |`, so char 8 is
    // the first 'b'. Leaving `cursor_block_entered_at` at its default (None)
    // causes `cursor_block_revealed()` to return true immediately — bypassing
    // the RAW_REVEAL_DELAY without any sleeping in the test.
    state.cursor.offset = 8;

    let backend = TestBackend::new(25, 5);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                show_table_buttons: false,
                state: &state,
                theme,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let symbol_at = |x: u16, y: u16| {
        buf.cell((x, y))
            .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
    };

    // Rendered layout, y=0 is the top border (┌──┬──┐), y=1 is the header row.
    // Pipes (│) sit at cols 0, 6, 12, 18 on every data/header row.
    for &x in &[0u16, 6, 12, 18] {
        assert_eq!(symbol_at(x, 1), '│', "expected │ at ({x}, 1)");
    }

    // Neighbouring cells keep their rendered content — NOT replaced by raw.
    for (x, ch) in [
        (2u16, 'a'),
        (3, 'a'),
        (4, 'a'),
        (14, 'c'),
        (15, 'c'),
        (16, 'c'),
    ] {
        assert_eq!(
            symbol_at(x, 1),
            ch,
            "neighbouring cell char at ({x}, 1) should still be rendered"
        );
    }

    // Active cell shows raw text "bbb" at cols 8-10 (between pipes at 6 and 12).
    for x in 8u16..=10 {
        assert_eq!(
            symbol_at(x, 1),
            'b',
            "active cell should show raw 'b' at ({x}, 1)"
        );
    }

    // Cursor indicator: cursor_rendered bg at (8, 1), NOT on the other two
    // 'b' cells.  Mode chip → cursor parity means the rendered-mode cursor
    // wears the bright_primary fill from the status bar's mode chip.
    let cell_at = |x: u16, y: u16| buf.cell((x, y)).expect("cell in bounds");
    let cursor_bg = theme.cursor_rendered.bg;
    assert!(cursor_bg.is_some(), "cursor_rendered must carry a bg");
    assert_eq!(
        cell_at(8, 1).style().bg,
        cursor_bg,
        "cursor cell at (8, 1) should carry cursor_rendered bg"
    );
    for x in [9u16, 10] {
        assert_ne!(
            cell_at(x, 1).style().bg,
            cursor_bg,
            "non-cursor cell at ({x}, 1) must not carry cursor_rendered bg"
        );
    }
}

#[test]
fn table_view_paints_row_and_column_handles_when_enabled() {
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    let src = "| aa | bb |\n|---|---|\n| 1  | 2  |\n| 3  | 4  |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;

    let backend = TestBackend::new(30, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                state: &state,
                theme,
                show_table_buttons: true,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let symbol_at = |x: u16, y: u16| {
        buf.cell((x, y))
            .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
    };

    // The snapshot-build pass should have produced exactly one snapshot with
    // the row-reorder gutter, the top-border row (column-reorder glyph site),
    // and the header row (column-resize glyph site) all populated.
    assert_eq!(view_state.table_snapshots.len(), 1);
    let snap = &view_state.table_snapshots[0];
    assert!(
        snap.row_handle_col.is_some(),
        "row handle col must be set when handles enabled"
    );
    assert!(
        snap.top_border_row.is_some(),
        "top border row must be set when handles enabled"
    );
    assert!(
        snap.header_row.is_some(),
        "header row must be set when handles enabled"
    );

    // Row-reorder: `⠿` on the left gutter for each data row.
    let handle_col = snap.row_handle_col.unwrap();
    let mut row_handle_count = 0;
    for y_range in &snap.row_ranges {
        if symbol_at(handle_col, y_range.start) == '⠿' {
            row_handle_count += 1;
        }
    }
    assert_eq!(
        row_handle_count,
        snap.row_ranges.len(),
        "every data row should show a ⠿ reorder glyph in the gutter"
    );

    // Column-reorder: `⠿` centered on each column's top border.
    let top_y = snap.top_border_row.unwrap();
    let col_reorder_count = snap
        .col_ranges
        .iter()
        .filter(|r| {
            let mid = r.start + (r.end - r.start) / 2;
            symbol_at(mid, top_y) == '⠿'
        })
        .count();
    assert_eq!(
        col_reorder_count,
        snap.col_ranges.len(),
        "every column should show a ⠿ reorder glyph on the top border"
    );

    // Column-resize: `⇔` on every header-row `│` — interior borders AND
    // the rightmost outer border (which resizes the last column).
    let header_y = snap.header_row.unwrap();
    let resize_count = snap
        .col_ranges
        .iter()
        .filter(|r| symbol_at(r.end, header_y) == '⇔')
        .count();
    assert_eq!(
        resize_count,
        snap.col_ranges.len(),
        "every border (interior + right outer) should show a ⇔ resize glyph"
    );
}

/// Phase 13 — for a multi-row data row whose cell wrapped to two
/// rendered sub-lines, exactly ONE `⠿` row-handle glyph paints (on
/// the row's first sub-line).  Painting on every wrapped sub-line
/// reads as visual noise without making the row easier to grab.
#[test]
fn table_view_paints_one_row_handle_per_logical_row() {
    use edamame::config::Theme;
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
    // First data row's second cell wraps under a narrow viewport.
    let src = "| Name | Notes |\n|---|---|\n| a | This is a very long note that wraps |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    // Cursor in the data row so handles paint.
    let target = src.find("This").unwrap();
    state.cursor.offset = state.buffer.rope().byte_to_char(target);
    state.update_cursor_block();

    let backend = TestBackend::new(28, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                state: &state,
                theme,
                show_table_buttons: true,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let symbol_at = |x: u16, y: u16| {
        buf.cell((x, y))
            .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
    };

    assert_eq!(view_state.table_snapshots.len(), 1);
    let snap = &view_state.table_snapshots[0];
    assert_eq!(snap.row_ranges.len(), 1, "one logical data row");
    let y_range = &snap.row_ranges[0];
    // Wrapped: the row spans more than one rendered y.
    assert!(
        y_range.end - y_range.start > 1,
        "test fixture must produce a wrapped row, got {y_range:?}",
    );
    let handle_col = snap.row_handle_col.expect("row handle col is set");
    let mut painted = 0usize;
    for y in y_range.start..y_range.end {
        if symbol_at(handle_col, y) == '⠿' {
            painted += 1;
        }
    }
    assert_eq!(
        painted, 1,
        "exactly one ⠿ row handle should paint per logical row, got {painted}",
    );
}

/// Phase 13 — when the cursor enters a *wrapped* table cell, the
/// rendered table layout (including the row's wrap continuation
/// sub-lines) must stay intact.  Pre-fix behaviour collapsed the
/// row to a single line of raw markdown like
/// `| a | This is ... long note |`; the post-fix behaviour leaves
/// every rendered table line in place and paints just a cursor
/// indicator on the cursor's wrap sub-line.
#[test]
fn rendered_view_wrapped_cell_keeps_table_layout_when_cursor_inside() {
    use edamame::config::Theme;
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
    let src = "| Name | Notes |\n|---|---|\n| a | This is a very long note that wraps |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    // Cursor on a char inside the wrapped cell (the second cell of the
    // first data row).
    let target = src.find("very").unwrap();
    state.cursor.offset = state.buffer.rope().byte_to_char(target);
    state.update_cursor_block();
    // Force the reveal delay to be elapsed so we test the steady-state
    // behaviour, not the jitter-suppression path.
    state.cursor_block_entered_at =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(1));

    let backend = TestBackend::new(28, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                state: &state,
                theme,
                show_table_buttons: false,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let row_text = |y: u16| -> String {
        (0..28u16)
            .map(|x| {
                buf.cell((x, y))
                    .and_then(|c| c.symbol().chars().next())
                    .unwrap_or(' ')
            })
            .collect()
    };

    // Every visible row of the rendered table starts with a box-drawing
    // glyph (`┌`, `│`, `┝`, `└`).  If the row collapsed to a raw line
    // we'd see `|` (ASCII pipe) at column 0 — explicitly rule that out.
    for y in 0u16..8 {
        let row = row_text(y);
        if row.is_empty() {
            continue;
        }
        let first = row.chars().next().unwrap();
        if first == ' ' {
            continue; // blank rows past the table
        }
        assert!(
            !row.starts_with('|'),
            "row {y} should not collapse to raw markdown ({row:?})",
        );
    }

    // Confirm the table's box-drawing glyphs are still visible.
    let mut found_top = false;
    let mut found_bottom = false;
    for y in 0u16..12 {
        let row = row_text(y);
        if row.starts_with('┌') {
            found_top = true;
        }
        if row.starts_with('└') {
            found_bottom = true;
        }
    }
    assert!(found_top, "top border must still render");
    assert!(found_bottom, "bottom border must still render");
}

/// Phase 13 — when a cell's raw markdown is too wide for the
/// rendered cell (e.g. `**_words_**` in a column whose auto-fit is
/// keyed off the rendered "words"), the cell horizontally scrolls to
/// keep the cursor's chunk visible.  The user must see *raw* chars
/// (`*`, `_`) where the cursor is — not the rendered bold-italic
/// glyphs of the formatted output.
#[test]
fn rendered_view_wrapped_cell_shows_raw_markdown_chunk() {
    use edamame::config::Theme;
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
    // Cell content `**_words_**` renders as the 5-char word "words".
    // With a narrow viewport the column auto-fits to ~5 chars, so the
    // raw 11-char source can't sit on the rendered row.
    let src = "| a | b |\n|---|---|\n| x | **_words_** |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    let target = src.find("words").unwrap();
    state.cursor.offset = state.buffer.rope().byte_to_char(target);
    state.update_cursor_block();
    state.cursor_block_entered_at =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(1));

    let backend = TestBackend::new(20, 8);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                state: &state,
                theme,
                show_table_buttons: false,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let row_text = |y: u16| -> String {
        (0..20u16)
            .map(|x| {
                buf.cell((x, y))
                    .and_then(|c| c.symbol().chars().next())
                    .unwrap_or(' ')
            })
            .collect()
    };

    // Find the data row (starts with `│`, not `┌` / `┝` / `└`).
    let mut data_row_text = None;
    for y in 0u16..8 {
        let row = row_text(y);
        if row.starts_with('│') && !row.contains('━') {
            // The first `│`-prefixed row is the header; skip until we
            // see one whose content isn't `a` / `b`.
            if row.contains('x') || row.contains('*') || row.contains('w') {
                data_row_text = Some(row);
                break;
            }
        }
    }
    let data_row = data_row_text.expect("data row should be on screen");
    // The cursor's chunk should expose at least one `*` or `_` (raw
    // markdown markers), proving we didn't just paint rendered styled
    // text.
    assert!(
        data_row.contains('*') || data_row.contains('_'),
        "cursor's cell should show raw markdown chars; got {data_row:?}",
    );
}

/// Phase 13 — buttons paint only on the table the cursor is inside.
/// With the cursor parked in a paragraph above a table, the table's
/// buttons must be invisible; moving the cursor onto the table
/// reveals them.  Snapshots are still captured for hit-testing in
/// either case.
#[test]
fn table_view_handles_only_paint_when_cursor_in_table() {
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    // A paragraph followed by a table.  Cursor starts at offset 0 (in
    // the paragraph), then later we move it into the table.
    let src = "intro line\n\n| aa | bb |\n|---|---|\n| 1  | 2  |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;

    // Cursor in the paragraph: snapshot exists, but no glyphs.
    let backend = TestBackend::new(30, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                state: &state,
                theme,
                show_table_buttons: true,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let symbol_at = |buf: &ratatui::buffer::Buffer, x: u16, y: u16| {
        buf.cell((x, y))
            .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
    };

    assert_eq!(view_state.table_snapshots.len(), 1);
    // No `⠿` (row/column reorder) glyphs anywhere on the screen yet.
    let mut painted = 0usize;
    for y in 0u16..10 {
        for x in 0u16..30 {
            if symbol_at(&buf, x, y) == '⠿' {
                painted += 1;
            }
        }
    }
    assert_eq!(
        painted, 0,
        "no handles should paint when cursor is outside the table",
    );

    // Move cursor into the table's first cell — find a byte offset
    // inside the first data row.  The table starts at byte 12 (`| aa`)
    // so byte 35 is comfortably inside `| 1  | 2  |`.
    let table_start = src.find("| aa").unwrap();
    let data_byte = src[table_start..]
        .find("| 1")
        .map(|off| table_start + off + 2) // jump past `| ` to land on '1'
        .unwrap();
    let target_char = state.buffer.rope().byte_to_char(data_byte);
    state.cursor.offset = target_char;
    state.update_cursor_block();

    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                state: &state,
                theme,
                show_table_buttons: true,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();
    let buf = terminal.backend().buffer().clone();

    let mut painted = 0usize;
    for y in 0u16..10 {
        for x in 0u16..30 {
            if symbol_at(&buf, x, y) == '⠿' {
                painted += 1;
            }
        }
    }
    assert!(
        painted > 0,
        "cursor inside the table should reveal `⠿` handle glyphs",
    );
}

#[test]
fn table_view_snapshots_empty_when_no_table() {
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    let state = EditorState::new(Buffer::from_str("plain paragraph\n"), theme);

    let backend = TestBackend::new(30, 5);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                state: &state,
                theme,
                show_table_buttons: true,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    assert!(view_state.table_snapshots.is_empty());
}

/// Partial user widths (issue 2): columns marked `_` in the comment should
/// auto-size while numeric columns stay pinned.
#[test]
fn table_view_partial_user_widths_honours_auto_underscore_entries() {
    use edamame::document::Buffer;
    use edamame::editor::EditorState;

    let theme = Box::leak(Box::new(Theme::default()));
    // Natural widths: col 0 = 3 ("abc"), col 1 = 6 ("defghi").  With
    // `[5, _]` col 0 pins to 5 and col 1 stays at its natural 6.
    let src = "| abc | defghi |\n| --- | --- |\n| bar | baz |\n<!-- tui-columns: [5, _] -->\n";
    let state = EditorState::new(Buffer::from_str(src), theme);

    let header = state
        .parsed
        .lines
        .iter()
        .find(|l| l.spans.iter().any(|s| s.content.contains('│')))
        .expect("rendered a header row");
    let mut pipes = Vec::new();
    let mut col = 0usize;
    for span in &header.spans {
        for ch in span.content.chars() {
            if ch == '│' {
                pipes.push(col);
            }
            col += 1;
        }
    }
    assert_eq!(pipes.len(), 3);
    // Col 0 content area = pinned width (5) + 2 padding = 7.
    assert_eq!(pipes[1] - pipes[0] - 1, 7);
    // Col 1 content area = natural (6) + 2 padding = 8.
    assert_eq!(pipes[2] - pipes[1] - 1, 8);
}

#[test]
fn table_view_persists_user_widths_from_tui_columns_comment() {
    use edamame::document::Buffer;
    use edamame::editor::EditorState;

    let theme = Box::leak(Box::new(Theme::default()));
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n<!-- tui-columns: [10, 20] -->\n";
    let state = EditorState::new(Buffer::from_str(src), theme);

    // The parsed doc's first block should be a table whose rendering uses
    // the persisted widths — we can verify this by measuring the pipe
    // positions on the rendered header row.
    let first_line = state
        .parsed
        .lines
        .iter()
        .find(|l| l.spans.iter().any(|s| s.content.contains('│')))
        .expect("rendered a header row");
    let pipes: Vec<usize> = {
        let mut positions = Vec::new();
        let mut col = 0usize;
        for span in &first_line.spans {
            for ch in span.content.chars() {
                if ch == '│' {
                    positions.push(col);
                }
                col += 1;
            }
        }
        positions
    };
    assert_eq!(pipes.len(), 3, "header has two cells + three pipes");
    // Col 0 content width: widths[0]=10, plus a 1-char pad on each side = 12.
    let col0_width = pipes[1] - pipes[0] - 1;
    assert_eq!(col0_width, 12);
    // Col 1 content width: widths[1]=20, plus 1+1 pad = 22.
    let col1_width = pipes[2] - pipes[1] - 1;
    assert_eq!(col1_width, 22);
}

#[test]
fn status_bar_shows_cursor_position() {
    let theme = Box::leak(Box::new(Theme::default()));
    let backend = TestBackend::new(80, 1);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            let bar = StatusBar {
                state: StatusBarState {
                    mode: Mode::Rendered,
                    filename: "f.md",
                    line_count: 10,
                    modified: false,
                    scroll: 0,
                    cursor_line: Some(5),
                    cursor_col: Some(12),
                    selection_size: None,
                },
                theme,
            };
            frame.render_widget(bar, frame.area());
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let out: String = (0..80u16)
        .map(|x| {
            buf.cell((x, 0))
                .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
        })
        .collect();
    assert!(out.contains("5:12"), "got: {out:?}");
}

// ── Phase 13: row striping and drop indicators ──────────────────────────────

/// With `row_striping = true`, alternating data rows pick up
/// `Theme::table_row_even` / `Theme::table_row_odd` as their background.
/// Asserts that row 0's cell carries the `table_row_even` style and row
/// 1's carries `table_row_odd` (themed to a contrasting bg here so we
/// can verify the difference).
#[test]
fn table_row_striping_alternates_bg_per_data_row() {
    use edamame::config::Theme;
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};
    use ratatui::style::{Color, Style};

    // Build a custom theme whose row striping styles are visibly
    // different so the assertion has a clear signal to check.
    let theme_owned = Theme {
        table_row_even: Style::default().bg(Color::Indexed(238)),
        table_row_odd: Style::default().bg(Color::Indexed(237)),
        ..Theme::default()
    };
    let theme: &'static Theme = Box::leak(Box::new(theme_owned));

    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    state.set_row_striping(true);

    let backend = TestBackend::new(20, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                state: &state,
                theme,
                show_table_buttons: false,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let cell_at = |x: u16, y: u16| buf.cell((x, y)).cloned().unwrap_or_default();

    // Layout: y=0 top border, y=1 header, y=2 thick sep, y=3 data row 0,
    // y=4 thin sep, y=5 data row 1.  Pick a column inside the cell
    // content area (x=2 sits inside col 0's `1` / `3`).
    let row0_bg = cell_at(2, 3).bg;
    let row1_bg = cell_at(2, 5).bg;
    assert_eq!(
        row0_bg,
        Color::Indexed(238),
        "row 0 even-stripe bg mismatch"
    );
    assert_eq!(row1_bg, Color::Indexed(237), "row 1 odd-stripe bg mismatch");
}

/// With `row_striping` on, the inter-row separator between two data
/// rows is rendered as a *blank* line (NBSP-padded) instead of the
/// `├─┼─┤` rule.  The blank line carries the background of the row
/// immediately above it so each striped row reads as a 2-row band of
/// its own colour, with no horizontal rule breaking up the rhythm.
#[test]
fn table_row_striping_replaces_thin_rule_with_blank_separator() {
    use edamame::config::Theme;
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};
    use ratatui::style::{Color, Style};

    let theme_owned = Theme {
        table_row_even: Style::default().bg(Color::Indexed(238)),
        table_row_odd: Style::default().bg(Color::Indexed(237)),
        ..Theme::default()
    };
    let theme: &'static Theme = Box::leak(Box::new(theme_owned));

    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    state.set_row_striping(true);

    let backend = TestBackend::new(20, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                state: &state,
                theme,
                show_table_buttons: false,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let symbol_at = |x: u16, y: u16| {
        buf.cell((x, y))
            .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
    };
    let bg_at = |x: u16, y: u16| buf.cell((x, y)).map(|c| c.bg).unwrap_or(Color::Reset);

    // Layout when stripe is on: y=0 top, y=1 header, y=2 thick,
    // y=3 row 0, y=4 BLANK separator (bg matches row 0), y=5 row 1,
    // y=6 bottom.
    // The "thin rule" position (y=4) must NOT carry `├` / `┼` /
    // `─` glyphs — only `│` and NBSP (which renders visually as a
    // space).
    for x in 0u16..16 {
        let g = symbol_at(x, 4);
        assert!(
            g != '├' && g != '┼' && g != '┤' && g != '─',
            "stripe-on separator at ({x}, 4) should be blank, got {g:?}",
        );
    }

    // The blank separator below row 0 should pick up row 0's bg
    // (Indexed(238)) on the cell-padding NBSPs.
    assert_eq!(
        bg_at(2, 4),
        Color::Indexed(238),
        "blank separator below row 0 should carry row 0's bg",
    );
}

/// With striping disabled (the default), the renderer must NOT apply
/// either striping style — every data row falls through to
/// `Theme::table_cell` and the cell background stays at terminal
/// default.
#[test]
fn table_row_striping_off_leaves_default_bg() {
    use edamame::config::Theme;
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};
    use ratatui::style::{Color, Style};

    let theme_owned = Theme {
        table_row_even: Style::default().bg(Color::Indexed(238)),
        table_row_odd: Style::default().bg(Color::Indexed(237)),
        ..Theme::default()
    };
    let theme: &'static Theme = Box::leak(Box::new(theme_owned));

    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    // Note: row_striping is false (default).

    let backend = TestBackend::new(20, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                state: &state,
                theme,
                show_table_buttons: false,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let bg_at = |x: u16, y: u16| buf.cell((x, y)).map(|c| c.bg).unwrap_or(Color::Reset);
    // Both data rows should have the same (default) background — neither
    // stripe colour should leak through.
    assert_ne!(bg_at(2, 3), Color::Indexed(238));
    assert_ne!(bg_at(2, 3), Color::Indexed(237));
    assert_eq!(bg_at(2, 3), bg_at(2, 5));
}

/// During a row-handle drag, `paint_drop_indicator` overlays a heavy
/// horizontal rule on the destination separator.  The painter renders
/// `DROP_ROW_GLYPH` ('━') styled with `Theme::table_drop_indicator`
/// across the table's full width on the separator y-coordinate.
#[test]
fn table_view_paints_drop_indicator_on_row_drag() {
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::table_view::DropIndicator;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    let src = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;

    // Drag from data row index 2 (first data row) onto data row 3
    // (second data row).  The painter should highlight the separator
    // *below* the second data row → drop_below.
    let indicator = DropIndicator::Row {
        table_byte_start: 0,
        src_row_idx: 2,
        hover_row_idx: 3,
    };

    let backend = TestBackend::new(30, 10);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: Some(indicator),
                state: &state,
                theme,
                show_table_buttons: false,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let symbol_at = |x: u16, y: u16| {
        buf.cell((x, y))
            .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
    };

    assert_eq!(view_state.table_snapshots.len(), 1);
    let snap = &view_state.table_snapshots[0];
    assert_eq!(snap.row_ranges.len(), 2);
    // Drop is below hover row (data idx = 1 → second data row).  Its
    // separator y is row_ranges[1].end.
    let sep_y = snap.row_ranges[1].end;
    let first_x = snap.col_ranges.first().unwrap().start;
    let last_x = snap.col_ranges.last().unwrap().end;
    let mut found = 0usize;
    for x in first_x.saturating_sub(1)..=last_x {
        if symbol_at(x, sep_y) == '━' {
            found += 1;
        }
    }
    assert!(
        found > 0,
        "drop indicator should paint ━ on separator y={sep_y}, got 0 matches"
    );
}

#[test]
fn rendered_view_code_block_only_opening_fence_de_renders() {
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    // Fenced code block with a language tag.  The renderer produces:
    //   row 0: " rust " language label
    //   row 1: " fn main() {} " padded body line
    // The closing fence has no rendered row of its own.
    let src = "```rust\nfn main() {}\n```\n";

    let row_text = |term: &Terminal<TestBackend>, y: u16, w: u16| -> String {
        let buf = term.backend().buffer().clone();
        (0..w)
            .map(|x| {
                buf.cell((x, y))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect()
    };

    let render = |state: &EditorState| -> Terminal<TestBackend> {
        let backend = TestBackend::new(30, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut view_state = RenderedViewState::default();
        terminal
            .draw(|frame| {
                let view = RenderedView {
                    drop_indicator: None,
                    show_table_buttons: false,
                    state,
                    theme,
                };
                frame.render_stateful_widget(view, frame.area(), &mut view_state);
            })
            .unwrap();
        terminal
    };

    // Cursor on the opening fence (raw line 0 — the ```rust line).
    {
        let mut state = EditorState::new(Buffer::from_str(src), theme);
        state.mode = Mode::Rendered;
        state.cursor.offset = 0;
        let term = render(&state);
        // Row 0 must show the raw fence ```rust, not the styled "rust" label.
        assert!(
            row_text(&term, 0, 30).contains("```rust"),
            "row 0 should show raw fence: {:?}",
            row_text(&term, 0, 30)
        );
        // Row 1 must still show the body line rendered.
        assert!(
            row_text(&term, 1, 30).contains("fn main() {}"),
            "row 1 should show rendered body: {:?}",
            row_text(&term, 1, 30)
        );
    }

    // Cursor on the body line — must NOT de-render to raw text.  The
    // rendered body row already shows the same characters, so we instead
    // assert that the row 0 language label is the styled " rust " (no
    // backticks bleed in from a misplaced de-render).
    {
        let mut state = EditorState::new(Buffer::from_str(src), theme);
        state.mode = Mode::Rendered;
        state.cursor.offset = src.find("fn main").unwrap();
        let term = render(&state);
        assert!(
            !row_text(&term, 0, 30).contains("```"),
            "row 0 must remain the styled language label, got: {:?}",
            row_text(&term, 0, 30)
        );
        assert!(
            row_text(&term, 1, 30).contains("fn main() {}"),
            "row 1 should show rendered body: {:?}",
            row_text(&term, 1, 30)
        );
    }

    // Cursor on the closing fence — must NOT replace the body row with ```.
    {
        let mut state = EditorState::new(Buffer::from_str(src), theme);
        state.mode = Mode::Rendered;
        state.cursor.offset = src.rfind("```").unwrap();
        let term = render(&state);
        assert!(
            row_text(&term, 1, 30).contains("fn main() {}"),
            "body row must stay rendered when cursor is on closing fence: {:?}",
            row_text(&term, 1, 30)
        );
    }
}

#[test]
fn rendered_view_code_block_blank_body_line_aligns_cursor_indicator() {
    // Regression: blank lines inside code blocks DO render (as NBSP-padded
    // rows), so the raw-to-rendered mapping must NOT compress them out.
    // Previously the list-style "preceding non-blank" compression made the
    // cursor indicator paint one row higher than the body line the cursor
    // is actually editing for every blank line above it.
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    // ```rust
    // A
    //          ← blank body line
    // B
    // ```
    let src = "```rust\nA\n\nB\n```\n";
    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    // Cursor on the 'B' body line — the line BELOW the blank.
    state.cursor.offset = src.find('B').unwrap();

    // Match viewport_width to area width so the NBSP-padded blank body
    // line fits in a single visual row (without this the default 80-col
    // padding wraps to 3 rows on a 30-col area and hides the bug).
    let width: u16 = 30;
    state.set_viewport_width(width as usize);

    let backend = TestBackend::new(width, 8);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                show_table_buttons: false,
                state: &state,
                theme,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let row_text = |y: u16| -> String {
        (0..width)
            .map(|x| {
                buf.cell((x, y))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect()
    };
    // Layout: row 0 = " rust " label, row 1 = " A", row 2 = NBSP padding,
    // row 3 = " B".  The cursor indicator (theme.cursor_rendered) must be
    // on row 3, not row 2.
    let row_has_cursor = |y: u16| {
        (0..width).any(|x| {
            buf.cell((x, y))
                .map(|c| {
                    c.style().bg == theme.cursor_rendered.bg
                        && c.style().fg == theme.cursor_rendered.fg
                })
                .unwrap_or(false)
        })
    };
    assert!(
        row_text(3).contains('B'),
        "row 3 should contain the 'B' body line, got: {:?}",
        row_text(3)
    );
    assert!(
        row_has_cursor(3),
        "cursor indicator must paint on row 3 (the 'B' line) not above it"
    );
    assert!(
        !row_has_cursor(2),
        "cursor indicator must not paint on row 2 (the blank padding line)"
    );
}

#[test]
fn rendered_view_bare_code_fence_never_de_renders() {
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    // Code block with no language tag — the renderer emits NO language
    // label, so nothing in the block has a useful raw form to expose.
    let src = "```\nfn main() {}\n```\n";

    let mut state = EditorState::new(Buffer::from_str(src), theme);
    state.mode = Mode::Rendered;
    // Place cursor on the opening fence.
    state.cursor.offset = 0;

    let backend = TestBackend::new(30, 5);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                show_table_buttons: false,
                state: &state,
                theme,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let row_text = |y: u16| -> String {
        (0..30u16)
            .map(|x| {
                buf.cell((x, y))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect()
    };
    // Row 0 is the first body line "fn main() {}" — for a bare ``` fence
    // there's no language label row.  The cursor is on raw line 0 (the
    // opening fence), but since there's no language to expose, the
    // body must remain rendered.
    assert!(
        row_text(0).contains("fn main() {}"),
        "bare fence must not de-render the first body line, got: {:?}",
        row_text(0)
    );
    assert!(
        !row_text(0).contains("```"),
        "bare fence must not bleed ``` into rendered output, got: {:?}",
        row_text(0)
    );
}

#[test]
fn mermaid_block_reveals_full_raw_source_on_cursor_entry() {
    // When the cursor enters a `\`\`\`mermaid` fenced block, the entire
    // image-reservation region must paint the raw mermaid source — not
    // just the cursor's single line.  Regression guard for the "only
    // one line de-renders" bug.
    use edamame::document::Buffer;
    use edamame::editor::EditorState;
    use edamame::ui::{RenderedView, RenderedViewState};

    let theme = Box::leak(Box::new(Theme::default()));
    // Small image_max_height so the reserved region stays bounded.  The
    // mermaid source is 5 raw lines (fence, 3 content, fence), which
    // fits in 6 rows.
    let src = "```mermaid\nflowchart TD\nA-->B\nB-->C\n```\n";
    let mut state = EditorState::new_with_config(Buffer::from_str(src), theme, true, true, 6);
    state.mode = Mode::Rendered;
    // Cursor on the first content line "flowchart TD".
    state.cursor.offset = src.find("flowchart").unwrap();
    state.update_cursor_block();
    // Skip the reveal delay so raw paint runs this frame.
    state.cursor_block_entered_at = None;

    let backend = TestBackend::new(20, 7);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = RenderedViewState::default();
    terminal
        .draw(|frame| {
            let view = RenderedView {
                drop_indicator: None,
                show_table_buttons: false,
                state: &state,
                theme,
            };
            frame.render_stateful_widget(view, frame.area(), &mut view_state);
        })
        .unwrap();

    let buf = terminal.backend().buffer().clone();
    let row_text = |y: u16| -> String {
        (0..20u16)
            .map(|x| {
                buf.cell((x, y))
                    .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
            })
            .collect()
    };
    // Every raw source line must appear on its corresponding rendered
    // row — not the `[Image: …]` placeholder.
    assert!(
        row_text(0).starts_with("```mermaid"),
        "row 0 must show opening fence, got: {:?}",
        row_text(0)
    );
    assert!(
        row_text(1).starts_with("flowchart TD"),
        "row 1 must show first body line, got: {:?}",
        row_text(1)
    );
    assert!(
        row_text(2).starts_with("A-->B"),
        "row 2 must show second body line, got: {:?}",
        row_text(2)
    );
    assert!(
        row_text(3).starts_with("B-->C"),
        "row 3 must show third body line, got: {:?}",
        row_text(3)
    );
    assert!(
        row_text(4).starts_with("```"),
        "row 4 must show closing fence, got: {:?}",
        row_text(4)
    );
    // The placeholder text the renderer would otherwise emit on row 0
    // must not be visible anywhere in the reserved region.
    for y in 0..5u16 {
        assert!(
            !row_text(y).contains("[Image"),
            "row {y} must not show image placeholder, got: {:?}",
            row_text(y)
        );
    }
}
