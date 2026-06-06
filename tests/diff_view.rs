//! Smoke tests for the raw DiffView (§5).  Renders a small
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
fn decision_divider_sits_between_delete_and_add() {
    // Replace `b` → `B`: the checkbox divider must land on its own line
    // between the deleted `b` and the added `B`.
    let state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    let lines = render_to_strings(&state, 30, 6);
    let del = lines.iter().position(|l| l.contains('b')).expect("delete");
    let div = lines
        .iter()
        .position(|l| l.contains("[ ]"))
        .expect("divider");
    let add = lines.iter().position(|l| l.contains('B')).expect("add");
    assert!(
        del < div && div < add,
        "divider must sit between delete and add: {lines:?}"
    );
}

#[test]
fn delete_only_hunk_puts_divider_below() {
    // Pure deletion of `b`: divider sits below the deleted line.
    let state = DiffState::new("a\nb\nc\n", "a\nc\n").unwrap();
    let lines = render_to_strings(&state, 30, 6);
    let del = lines.iter().position(|l| l.contains('b')).expect("delete");
    let div = lines
        .iter()
        .position(|l| l.contains("[ ]"))
        .expect("divider");
    assert!(
        div > del,
        "divider must sit below a delete-only hunk: {lines:?}"
    );
}

#[test]
fn insert_only_hunk_puts_divider_above() {
    // Pure insertion of `b`: divider sits above the added line.
    let state = DiffState::new("a\nc\n", "a\nb\nc\n").unwrap();
    let lines = render_to_strings(&state, 30, 6);
    let div = lines
        .iter()
        .position(|l| l.contains("[ ]"))
        .expect("divider");
    let add = lines.iter().rposition(|l| l.contains('b')).expect("add");
    assert!(
        div < add,
        "divider must sit above an insert-only hunk: {lines:?}"
    );
}

#[test]
fn accepted_decision_shows_label() {
    let mut state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    state.decisions[0] = Decision::Accepted;
    let lines = render_to_strings(&state, 30, 6);
    assert!(
        lines.iter().any(|l| l.contains("Accepted")),
        "expected `Accepted` label on the divider: {lines:?}"
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
fn accepted_decision_renders_yes_glyph() {
    let mut state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    state.decisions[0] = Decision::Accepted;
    let lines = render_to_strings(&state, 30, 6);
    assert!(
        lines.iter().any(|l| l.contains("[Y]")),
        "expected [Y] glyph after accepting hunk: {lines:?}",
    );
}

#[test]
fn rejected_decision_renders_no_glyph() {
    let mut state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    state.decisions[0] = Decision::Rejected;
    let lines = render_to_strings(&state, 30, 6);
    assert!(
        lines.iter().any(|l| l.contains("[N]")),
        "expected [N] glyph after rejecting hunk: {lines:?}",
    );
}

#[test]
fn focused_pending_divider_shows_caret_and_inline_prompt() {
    // The single (focused) hunk is pending, so its divider carries the
    // `>` caret and spells out the accept/reject keys inline.  The
    // glyphs come from the shared `diff_keys` table — `y` / `n`.
    let state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    let lines = render_to_strings(&state, 60, 6);
    let prompt = lines
        .iter()
        .find(|l| l.contains("Accept"))
        .expect("focused-pending divider must show the inline prompt");
    assert!(
        prompt.contains('>'),
        "focused divider must carry the caret: {prompt:?}"
    );
    assert!(
        prompt.contains("Accept [y]") && prompt.contains("Reject [n]"),
        "prompt must name the y/n keys: {prompt:?}"
    );
}

#[test]
fn unfocused_pending_divider_stays_bare() {
    // Two replace hunks: only the first is focused, so the second hunk's
    // divider stays the bare `[ ]` checkbox with no caret or prompt.
    let state = DiffState::new("a\nb\nc\nd\n", "a\nB\nc\nD\n").unwrap();
    assert!(state.hunks.len() >= 2, "need two hunks for this test");
    let lines = render_to_strings(&state, 60, 12);
    // Exactly one divider (the focused one) carries the inline prompt.
    let prompts = lines.iter().filter(|l| l.contains("Accept")).count();
    assert_eq!(
        prompts, 1,
        "only the focused divider shows the prompt: {lines:?}"
    );
    // The unfocused divider is a bare checkbox: a `[ ]` line with no
    // caret and no prompt text.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("[ ]") && !l.contains("Accept") && !l.contains('>')),
        "unfocused divider must stay a bare `[ ]`: {lines:?}"
    );
}

#[test]
fn every_divider_shows_its_position_counter() {
    // Two replace hunks: each divider is numbered in document order,
    // `(1/2)` then `(2/2)`, regardless of focus.
    let state = DiffState::new("a\nb\nc\nd\n", "a\nB\nc\nD\n").unwrap();
    assert!(state.hunks.len() == 2, "need exactly two hunks");
    let lines = render_to_strings(&state, 60, 12);
    assert!(
        lines.iter().any(|l| l.contains("(1/2)")),
        "first divider must show (1/2): {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("(2/2)")),
        "second divider must show (2/2): {lines:?}"
    );
}

#[test]
fn unfocused_resolved_divider_uses_dim_resolution_hue() {
    use ratatui::style::Modifier;

    // Two hunks; focus stays on hunk 0.  Resolve the *unfocused* hunk 1
    // as Accepted: its divider must carry the green resolution hue with
    // DIM (and no bold), per the requested styling.
    let mut state = DiffState::new("a\nb\nc\nd\n", "a\nB\nc\nD\n").unwrap();
    assert!(state.hunks.len() == 2, "need exactly two hunks");
    state.decisions[1] = Decision::Accepted;

    let th = theme();
    let (width, height) = (60u16, 12u16);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut view_state = DiffViewState::default();
    terminal
        .draw(|frame| {
            let area = frame.area();
            ratatui::widgets::StatefulWidget::render(
                DiffView {
                    diff: &state,
                    theme: th,
                    scroll: 0,
                },
                area,
                frame.buffer_mut(),
                &mut view_state,
            );
        })
        .unwrap();
    let buf = terminal.backend().buffer().clone();

    let row_text = |y: u16| -> String {
        (0..width)
            .map(|x| {
                buf.cell((x, y))
                    .and_then(|c| c.symbol().chars().next())
                    .unwrap_or(' ')
            })
            .collect()
    };
    // The unfocused accepted divider: shows "Accepted" + "(2/2)", no caret.
    let row = (0..height)
        .find(|&y| {
            let t = row_text(y);
            t.contains("Accepted") && t.contains("(2/2)") && !t.contains('>')
        })
        .expect("unfocused accepted divider row");

    let want_fg = th.diff_decision_accepted.fg.expect("accepted hue is set");
    let style = buf.cell((0, row)).expect("cell in bounds").style();
    assert_eq!(
        style.fg,
        Some(want_fg),
        "resolved unfocused divider uses the hue"
    );
    assert!(style.add_modifier.contains(Modifier::DIM), "must be DIM");
    assert!(
        !style.add_modifier.contains(Modifier::BOLD),
        "must not be bold"
    );
}
