/// Integration tests for UI widgets using `ratatui::backend::TestBackend`.

use ratatui::{backend::TestBackend, Terminal};

use markdown_tui::config::Theme;
use markdown_tui::editor::Mode;
use markdown_tui::ui::status_bar::{StatusBar, StatusBarState};

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
