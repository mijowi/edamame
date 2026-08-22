//! Smoke tests for the raw DiffView (§5).  Renders a small
//! diff into a `TestBackend` and asserts the stacked old-above-new
//! layout, the focused-hunk gutter glyph, and the decision indicator
//! glyphs all reach the output buffer.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer as TuiBuf;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::Terminal;

use edamame::config::Theme;
use edamame::diff::{Decision, DiffState};
use edamame::document::ParsedDoc;
use edamame::ui::{DiffView, DiffViewState};

fn theme() -> &'static Theme {
    Box::leak(Box::new(Theme::default()))
}

/// An RGB built-in, for the assertions that care about actual colors.
/// `Theme::default()` derives from the *indexed* `256 Dark` palette, and
/// `themes::util::blend` is a no-op on non-RGB colors, so several of its
/// blended fields collapse onto their base — fine as a layout fixture,
/// useless as a color one.
fn rgb_theme() -> &'static Theme {
    Box::leak(Box::new(
        Theme::builtin("Edamame").expect("Edamame is a built-in"),
    ))
}

fn render_to_buffer_with(state: &DiffState, th: &Theme, width: u16, height: u16) -> TuiBuf {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view_state = DiffViewState::default();
    terminal
        .draw(|frame| {
            let area = frame.area();
            ratatui::widgets::StatefulWidget::render(
                DiffView {
                    diff: state,
                    theme: th,
                    scroll: 0,
                },
                area,
                frame.buffer_mut(),
                &mut view_state,
            );
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn render_to_buffer(state: &DiffState, width: u16, height: u16) -> TuiBuf {
    render_to_buffer_with(state, theme(), width, height)
}

/// One `String` per terminal row, for the tests that only care about text.
fn buffer_to_strings(buf: &TuiBuf) -> Vec<String> {
    let Rect { width, height, .. } = buf.area;
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| {
                    buf.cell((x, y))
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect::<String>()
        })
        .collect()
}

fn render_to_strings(state: &DiffState, width: u16, height: u16) -> Vec<String> {
    buffer_to_strings(&render_to_buffer(state, width, height))
}

/// The style of the cell at the start of `needle` on the first row that
/// contains it — enough to assert which wash a rendered label carries.
fn style_at_substring(buf: &TuiBuf, needle: &str) -> Style {
    let rows = buffer_to_strings(buf);
    let (y, col) = rows
        .iter()
        .enumerate()
        .find_map(|(y, row)| row.find(needle).map(|byte_idx| (y, byte_idx)))
        .unwrap_or_else(|| panic!("{needle:?} not rendered: {rows:?}"));
    // The rows are built one char per cell, and every glyph the divider
    // draws is ASCII, so the byte index is the column.
    buf.cell((col as u16, y as u16))
        .expect("cell in bounds")
        .style()
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
fn prompt_orders_reject_above_accept() {
    // Reading order mirrors the stacking: the old side sits above the
    // divider and the new side below, so `Reject` must precede `Accept`
    // in the prompt.  Position is what encodes the mapping — keep the
    // two in this order.
    let state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    let lines = render_to_strings(&state, 60, 6);
    let prompt = lines
        .iter()
        .find(|l| l.contains("Accept"))
        .expect("focused-pending divider must show the inline prompt");
    let reject = prompt.find("Reject").expect("Reject label");
    let accept = prompt.find("Accept").expect("Accept label");
    assert!(
        reject < accept,
        "Reject must lead (old is above the divider): {prompt:?}"
    );
}

#[test]
fn prompt_chips_wear_the_wash_of_the_side_they_name() {
    // The point of the chips: "Accept" is painted in the literal
    // background of the add rows and "Reject" in that of the delete rows,
    // so the label, its key, and the block it acts on read as one color.
    // Pin it against the theme fields themselves — deriving the chip from
    // a fresh palette hue would leave every text assertion passing while
    // the chip silently drifted from the wash it is supposed to name.
    let th = rgb_theme();
    let state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    let buf = render_to_buffer_with(&state, th, 60, 6);

    let accept = style_at_substring(&buf, "Accept");
    let reject = style_at_substring(&buf, "Reject");
    assert_eq!(
        accept.bg, th.diff_add_line.bg,
        "Accept chip must carry the add wash"
    );
    assert_eq!(
        reject.bg, th.diff_delete_line.bg,
        "Reject chip must carry the delete wash"
    );
    assert_ne!(
        accept.bg, reject.bg,
        "the two chips must be distinguishable in a color theme"
    );

    // The washes are background-only by convention, and the built-ins
    // honor it — but the chip does not depend on that: it pins its own
    // foreground from `normal` rather than inheriting either the wash's
    // or the divider's `secondary`, which would be a cyan-ish label on a
    // green fill.  See `prompt_chip_style_ignores_a_wash_foreground`.
    assert_eq!(th.diff_add_line.fg, None, "add wash stays bg-only");
    assert_eq!(th.diff_delete_line.fg, None, "delete wash stays bg-only");
    assert_eq!(
        accept.fg, th.normal.fg,
        "chip fg is pinned from `normal`, not inherited from the divider"
    );
    assert_ne!(
        accept.fg, th.diff_decision_pending.fg,
        "chip must not inherit the divider's fg"
    );
}

#[test]
fn prompt_chip_style_ignores_a_wash_foreground() {
    // Both washes are user-authorable — they have to be, since the
    // palette blend can't derive them on an indexed palette — so a theme
    // *can* set an `fg` on them against the convention.  The chip takes
    // the wash's background and modifiers only and pins its foreground
    // from `normal`, so an authored fg can't put an unreadable label on
    // the fill.
    let mut mutated = Theme::builtin("Edamame").expect("Edamame is a built-in");
    mutated.diff_add_line = mutated.diff_add_line.fg(Color::Magenta);
    mutated.diff_delete_line = mutated.diff_delete_line.fg(Color::Magenta);
    let normal_fg = mutated.normal.fg;
    let add_bg = mutated.diff_add_line.bg;
    let th: &'static Theme = Box::leak(Box::new(mutated));

    let state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    let buf = render_to_buffer_with(&state, th, 60, 6);
    let accept = style_at_substring(&buf, "Accept");
    assert_eq!(accept.fg, normal_fg, "chip fg must ignore the wash's fg");
    assert_eq!(accept.bg, add_bg, "chip still wears the wash's bg");
}

#[test]
fn resolved_divider_renders_no_chips() {
    // The chips are only ever correct against the neutral
    // `diff_decision_pending` base.  A resolved divider's base carries the
    // green/red resolution hue, so the prompt — and with it the washes —
    // must be gone once the hunk is decided.
    let mut state = DiffState::new("a\nb\nc\n", "a\nB\nc\n").unwrap();
    state.decisions[0] = Decision::Accepted;
    let lines = render_to_strings(&state, 60, 6);
    let divider = lines
        .iter()
        .find(|l| l.contains("Accepted"))
        .expect("resolved divider");
    assert!(
        !divider.contains("Accept [") && !divider.contains("Reject ["),
        "a resolved divider must drop the prompt: {divider:?}"
    );
}

#[test]
fn side_markers_prefix_every_body_line() {
    // Unified-diff convention: `- ` on the delete side, `+ ` on the add
    // side, and a matching two-space prefix on context so every body
    // column lines up.
    let state = DiffState::new("ctx\nbee\n", "ctx\nBEE\n").unwrap();
    let lines = render_to_strings(&state, 30, 8);
    assert!(
        lines.iter().any(|l| l.starts_with("  ctx")),
        "context line needs a two-space prefix: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("- bee")),
        "delete line needs a `- ` marker: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("+ BEE")),
        "add line needs a `+ ` marker: {lines:?}"
    );
}

#[test]
fn delete_only_hunk_still_marks_its_side() {
    // The marker is the encoding that survives a degenerate hunk: with
    // no add side below the divider, `- ` is the only thing naming the
    // side the change acts on.
    let state = DiffState::new("a\nb\nc\n", "a\nc\n").unwrap();
    let lines = render_to_strings(&state, 30, 8);
    assert!(
        lines.iter().any(|l| l.starts_with("- b")),
        "delete-only hunk must still mark its side: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("+ ")),
        "delete-only hunk has no add side: {lines:?}"
    );
}

#[test]
fn wrapped_marker_line_agrees_with_the_row_cache() {
    // `render_line` derives a hanging indent from a leading marker, and
    // `- ` / `+ ` match its raw-bullet shape.  The layout row cache must
    // measure the marker *and* that indent, or the painted height and
    // the cached height diverge on any line that wraps — desyncing every
    // scroll computation.  No trailing newline, so the last visual line
    // carries text and the painted extent is measurable.
    let state = DiffState::new(
        "one two three four five six\nTAIL",
        "ONE two three four five six\nTAIL",
    )
    .unwrap();
    let width = 12u16;
    let lines = render_to_strings(&state, width, 40);
    let painted = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .expect("some content painted")
        + 1;
    assert_eq!(
        painted,
        state.total_visual_rows(width as usize),
        "painted rows must match the cached row count: {lines:?}"
    );
    // And the continuation of a wrapped delete line hangs under its
    // text, not under the marker.
    let cont = lines
        .iter()
        .position(|l| l.starts_with("- one"))
        .map(|i| lines[i + 1].clone())
        .expect("wrapped delete line");
    assert!(
        cont.starts_with("  ") && !cont.trim().is_empty(),
        "continuation must hang at the marker width: {cont:?}"
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

// ── Rendered clean regions ──────────────────────────────────────────────

/// A review with the rendered new-side parse installed, as
/// `EditorState::refresh_diff_parse` installs it.
fn rendered_state(old: &str, new: &str) -> DiffState {
    let mut state = DiffState::new(old, new).expect("non-empty diff");
    state.set_rendered_parse(Some(ParsedDoc::build(new, theme(), true, 20)));
    state
}

#[test]
fn a_heading_in_a_clean_region_paints_rendered_not_raw() {
    let old = "# Heading\n\nbee\n";
    let new = "# Heading\n\nBEE\n";
    let lines = render_to_strings(&rendered_state(old, new), 40, 12);
    assert!(
        lines.iter().any(|l| l.trim() == "Heading"),
        "the unchanged heading must render styled, without its `#`: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("# Heading")),
        "raw heading source must not appear: {lines:?}"
    );
}

#[test]
fn markers_still_prefix_every_line_inside_a_raw_region() {
    // The rendered path must not disturb the changed region's
    // presentation: markers, both sides, and the untouched context lines
    // *within* the region all stay raw.
    let old = "# Heading\n\nctx\nbee\n";
    let new = "# Heading\n\nctx\nBEE\n";
    let lines = render_to_strings(&rendered_state(old, new), 40, 12);
    assert!(
        lines.iter().any(|l| l.starts_with("- bee")),
        "delete line needs a `- ` marker: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("+ BEE")),
        "add line needs a `+ ` marker: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("  ctx")),
        "raw context inside the region keeps its two-space prefix: {lines:?}"
    );
}

/// The raw layout is still a live path — every review is queried in it
/// on its first frame, before `prepare_viewport` resolves the deferred
/// parse — so it has to keep painting exactly what it always did.
#[test]
fn without_a_parse_the_review_paints_the_raw_view() {
    // Every other test in this file constructs `parsed_new: None`
    // already, so "identical to today" proves nothing on its own.  Build
    // the same review twice: with a parse the output must differ, and
    // dropping the parse must restore the raw output byte for byte.
    let old = "# Heading\n\nbee\n";
    let new = "# Heading\n\nBEE\n";
    let raw = render_to_strings(&DiffState::new(old, new).unwrap(), 40, 12);

    let mut state = rendered_state(old, new);
    let with_parse = render_to_strings(&state, 40, 12);
    assert_ne!(with_parse, raw, "the rendered path must change the output");

    state.set_rendered_parse(None);
    assert_eq!(
        render_to_strings(&state, 40, 12),
        raw,
        "dropping the parse must restore the raw view exactly"
    );
}
