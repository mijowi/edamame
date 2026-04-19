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
    use ratatui::style::Modifier;

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

    // Cursor indicator: REVERSED at (8, 1), NOT on the other two 'b' cells.
    let cell_at = |x: u16, y: u16| buf.cell((x, y)).expect("cell in bounds");
    assert!(
        cell_at(8, 1).modifier.contains(Modifier::REVERSED),
        "cursor cell at (8, 1) should carry REVERSED modifier"
    );
    for x in [9u16, 10] {
        assert!(
            !cell_at(x, 1).modifier.contains(Modifier::REVERSED),
            "non-cursor cell at ({x}, 1) must not carry REVERSED modifier"
        );
    }
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
