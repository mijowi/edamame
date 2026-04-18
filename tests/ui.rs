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
