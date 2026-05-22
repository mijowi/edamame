//! Smoke tests for the raw DiffView (Phase 1 §5).  Renders a small
//! diff into a `TestBackend` and asserts the stacked old-above-new
//! layout, the focused-hunk gutter glyph, and the decision indicator
//! glyphs all reach the output buffer.

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use edamame::config::Theme;
use edamame::diff::{Decision, DiffState};
use edamame::ui::{DiffView, DiffViewState};

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

fn render_to_strings(state: &DiffState, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = DiffViewState::default();
    terminal
        .draw(|frame| {
            let area = frame.area();
            ratatui::widgets::StatefulWidget::render(
                DiffView {
                    diff: state,
                    theme: theme(),
                    scroll: 0,
                },
                area,
                frame.buffer_mut(),
                &mut view_state,
            );
        })
        .unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..height as usize)
        .map(|y| {
            (0..width as usize)
                .map(|x| {
                    buf.cell((x as u16, y as u16))
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect::<String>()
        })
        .collect()
}

#[test]
fn stacked_old_above_new() {
    // Replace line 1: old `bravo` → new `BRAVO`.  Verify old line
    // appears in the output above the new line.
    let state = DiffState::new("alpha\nbravo\ngamma\n", "alpha\nBRAVO\ngamma\n").unwrap();
    let lines = render_to_strings(&state, 40, 6);
    let old_idx = lines
        .iter()
        .position(|l| l.contains("bravo"))
        .expect("old line shown");
    let new_idx = lines
        .iter()
        .position(|l| l.contains("BRAVO"))
        .expect("new line shown");
    assert!(old_idx < new_idx, "old line must appear above new line");
}

#[test]
fn focused_hunk_gutter_marks_with_arrow() {
    let state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    let lines = render_to_strings(&state, 30, 6);
    // The `>` gutter glyph should appear on the focused hunk's
    // first line.
    assert!(
        lines.iter().any(|l| l.starts_with('>')),
        "expected `>` glyph in gutter: {lines:?}"
    );
}

#[test]
fn pending_decision_renders_open_checkbox() {
    let state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    let lines = render_to_strings(&state, 30, 6);
    assert!(
        lines.iter().any(|l| l.contains("[ ]")),
        "expected pending [ ] glyph: {lines:?}"
    );
}

#[test]
fn accepted_decision_renders_check_glyph() {
    let mut state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    state.decisions[0] = Decision::Accepted;
    let lines = render_to_strings(&state, 30, 6);
    assert!(
        lines.iter().any(|l| l.contains('✓')),
        "expected ✓ glyph after accepting hunk: {lines:?}",
    );
}

#[test]
fn rejected_decision_renders_x_glyph() {
    let mut state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    state.decisions[0] = Decision::Rejected;
    let lines = render_to_strings(&state, 30, 6);
    assert!(
        lines.iter().any(|l| l.contains("[x]")),
        "expected [x] glyph after rejecting hunk: {lines:?}",
    );
}
