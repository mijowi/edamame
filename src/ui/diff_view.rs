//! Raw stacked diff view.  Renders a flat sequence of
//! [`DiffVisualLine`] entries — interleaving unchanged context with
//! per-hunk old-above-new pairs — to ratatui via the shared
//! [`crate::ui::line_render`] helper so the trailing-cell bg fill and
//! word-aware wrap match the other modes.
//!
//! A future iteration (`docs/diff-mode-plan.md` §16) will upgrade this to a
//! hybrid rendered view; for now it stays raw-only so the
//! review-and-decide flow ships without touching `ParsedDoc` /
//! `SourceMap`.

use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::{Action, Theme};
use crate::diff::hunk::InlineSide;
use crate::diff::layout::{
    decision_line_text, line_marker, line_text, DiffLineSource, DiffVisualLine,
};
use crate::diff::{Decision, DiffState};
use crate::input::diff_hint;
use crate::ui::line_render::render_line_from_visual;

/// Per-frame state for [`DiffView`].  The materialised line sequence
/// and its wrapped-row counts are cached on [`DiffState`] itself (see
/// [`crate::diff::layout`]) rather than here, so this is just the
/// marker required by `StatefulWidget`.
#[derive(Debug, Default)]
pub struct DiffViewState {}

pub struct DiffView<'a> {
    pub diff: &'a DiffState,
    pub theme: &'a Theme,
    /// Visual-row scroll offset, sourced from
    /// [`crate::editor::EditorState::scroll`] (diff mode reuses the
    /// canonical scroll field).
    pub scroll: usize,
}

impl<'a> StatefulWidget for DiffView<'a> {
    type State = DiffViewState;

    fn render(self, area: Rect, buf: &mut TuiBuf, _view_state: &mut Self::State) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let width = area.width as usize;
        let scroll = self.scroll;

        self.diff.with_layout(width, |lines, rc| {
            // O(log N) jump to the line containing visual row `scroll`,
            // plus the sub-row to skip within it — no per-line rewrap.
            let (start_idx, mut skip_first_subrow) = rc.find_visual_row(scroll);

            let mut idx = start_idx;
            let mut visual_y: u16 = 0;
            while idx < lines.len() && visual_y < area.height {
                let dvl = &lines[idx];
                let line = build_line(self.diff, self.theme, dvl);
                // The decision divider is a single-row status strip
                // (pinned to one row in the layout cache), so it renders
                // without wrapping; every other line word-wraps.
                let wrap = dvl.source != DiffLineSource::Decision;
                let painted =
                    render_line_from_visual(&line, area, buf, visual_y, wrap, skip_first_subrow);
                // After the first line, never skip sub-rows again.
                skip_first_subrow = 0;
                if painted == 0 {
                    break;
                }
                visual_y = visual_y.saturating_add(painted);
                idx += 1;
            }
        });
    }
}

fn build_line(diff: &DiffState, theme: &Theme, dvl: &DiffVisualLine) -> Line<'static> {
    // Decision divider: the accept/reject checkbox plus a resolved
    // label, on its own line between the delete and add sides.  The
    // decision style carries a background, set on the line base so the
    // trailing-cell fill paints the whole row — the strip reads as the
    // actionable divider between the two sides.
    if dvl.source == DiffLineSource::Decision {
        let focused = dvl
            .hunk_idx
            .is_some_and(|hi| diff.hunks[hi].id == diff.focused_id);
        let dec = dvl
            .hunk_idx
            .and_then(|hi| diff.decisions.get(hi).copied())
            .unwrap_or(Decision::Pending);
        // The focused divider keeps its per-state hue and is bolded so
        // the actionable checkbox draws the eye.  Unfocused dividers
        // recede onto the muted `diff_decision_unfocused` strip; a
        // *resolved* unfocused divider keeps that background but borrows
        // the focused state's foreground hue (green/red) and adds `DIM`,
        // so its decision still reads by color while staying quieter than
        // the focused one.  Deriving the hue from the focused style
        // (rather than the palette) keeps monochrome themes correct —
        // there the focused style carries no color, so the unfocused one
        // stays a plain `DIM` strip and the label text conveys the state.
        let style = if focused {
            let base = match dec {
                Decision::Pending => theme.diff_decision_pending,
                Decision::Accepted => theme.diff_decision_accepted,
                Decision::Rejected => theme.diff_decision_rejected,
            };
            base.add_modifier(Modifier::BOLD)
        } else {
            match dec {
                Decision::Pending => theme.diff_decision_unfocused,
                Decision::Accepted | Decision::Rejected => {
                    let hue = if dec == Decision::Accepted {
                        theme.diff_decision_accepted.fg
                    } else {
                        theme.diff_decision_rejected.fg
                    };
                    let mut s = theme.diff_decision_unfocused.add_modifier(Modifier::DIM);
                    if let Some(color) = hue {
                        s = s.fg(color);
                    }
                    s
                }
            }
        };
        // The focused divider carries a `>` caret and (while pending)
        // an inline accept/reject prompt; unfocused dividers stay a bare
        // checkbox / label.  A trailing `(i/n)` position counter numbers
        // every divider in document order.  Set the line base style so
        // the trailing-cell fill extends the muted band across the full
        // row; the prompt span inherits it (bold and all), while the
        // counter span dims it (and clears the inherited bold) so the
        // index reads as quiet metadata, not part of the call to action.
        // `DIM` rather than a muted color keeps the counter recessive in
        // monochrome themes too, where color can't carry the hierarchy.
        let position = dvl.hunk_idx.map_or(0, |hi| hi + 1);
        let total = diff.hunks.len();
        let counter_style = Style::default()
            .add_modifier(Modifier::DIM)
            .remove_modifier(Modifier::BOLD);
        let mut spans = decision_divider_spans(theme, dec, focused);
        spans.push(Span::styled(
            format!(" ({position}/{total})"),
            counter_style,
        ));
        return Line::from(spans).style(style);
    }

    // Delete / add / context lines pull their text from the rope; the
    // decision branch above never needs it, so we only pay the
    // allocation here.  Each carries a two-cell `line_marker` gutter
    // (`- ` / `+ ` / two spaces) ahead of its body, so the side reads
    // without color — including on delete-only and insert-only hunks,
    // where the divider's spatial "above is old, below is new" claim has
    // nothing to point at.  Focus selects
    // both the full-line wash and the within-line highlight: a
    // non-focused hunk uses the muted `_unfocused` variants of both so
    // its changed words recede with its background instead of popping at
    // full saturation.
    let text = line_text(diff, dvl);
    let focused = dvl
        .hunk_idx
        .is_some_and(|hi| diff.hunks[hi].id == diff.focused_id);
    let line_style = match dvl.source {
        DiffLineSource::OldDelete if dvl.hunk_idx.is_some() => {
            if focused {
                theme.diff_delete_line
            } else {
                theme.diff_delete_line_unfocused
            }
        }
        DiffLineSource::NewAdd if dvl.hunk_idx.is_some() => {
            if focused {
                theme.diff_add_line
            } else {
                theme.diff_add_line_unfocused
            }
        }
        _ => Style::default(),
    };

    // Build the body spans with optional inline highlights.
    let mut body_spans: Vec<Span<'static>> = Vec::new();
    let inline_bg = match dvl.source {
        DiffLineSource::OldDelete if focused => Some(theme.diff_delete_inline),
        DiffLineSource::OldDelete => Some(theme.diff_delete_inline_unfocused),
        DiffLineSource::NewAdd if focused => Some(theme.diff_add_inline),
        DiffLineSource::NewAdd => Some(theme.diff_add_inline_unfocused),
        _ => None,
    };
    if let (Some(hi), Some(inline_style)) = (dvl.hunk_idx, inline_bg) {
        let h = &diff.hunks[hi];
        let line_in_hunk = match dvl.source {
            DiffLineSource::OldDelete => dvl.rope_line.saturating_sub(h.old_lines.start),
            DiffLineSource::NewAdd => dvl.rope_line.saturating_sub(h.new_lines.start),
            _ => 0,
        };
        let want_side = match dvl.source {
            DiffLineSource::OldDelete => InlineSide::Old,
            DiffLineSource::NewAdd => InlineSide::New,
            _ => InlineSide::New,
        };
        let mut cursor = 0usize;
        let chars: Vec<char> = text.chars().collect();
        for span in h
            .inline
            .iter()
            .filter(|s| s.line_in_hunk == line_in_hunk && s.side == want_side)
        {
            let s = span.chars.start.min(chars.len());
            let e = span.chars.end.min(chars.len());
            if s >= e {
                continue;
            }
            if cursor < s {
                let body: String = chars[cursor..s].iter().collect();
                body_spans.push(Span::raw(body));
            }
            let body: String = chars[s..e].iter().collect();
            body_spans.push(Span::styled(body, inline_style));
            cursor = e;
        }
        if cursor < chars.len() {
            let body: String = chars[cursor..].iter().collect();
            body_spans.push(Span::raw(body));
        }
        if body_spans.is_empty() {
            body_spans.push(Span::raw(text.to_owned()));
        }
    } else {
        body_spans.push(Span::raw(text.to_owned()));
    }

    // The marker is prepended as its own span rather than folded into
    // `text`, because the inline highlight ranges above index into the
    // raw line's chars.  It inherits `line_style`, so the add/delete
    // wash covers the gutter and the row reads as one band.
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(body_spans.len() + 1);
    spans.push(Span::raw(line_marker(dvl.source)));
    spans.extend(body_spans);

    Line::from(spans).style(line_style)
}

/// Background chip style for one side of the focused pending prompt.
///
/// The chip reuses the *same* `diff_add_line` / `diff_delete_line` wash
/// the add and delete rows carry, so "Accept" is painted in the literal
/// color of the block below the divider and "Reject" in the color of the
/// block above it — the label, its key, and the text it acts on are one
/// color. Deriving the chip from a fresh palette hue instead would let
/// it drift from the wash it is supposed to name, and would need a new
/// theme field in every built-in and user theme.
///
/// The washes are meant to be background-only, so the chip takes only
/// their `bg` and `add_modifier` and pins the foreground from
/// `theme.normal` — inheriting the divider's `secondary` fg would put a
/// cyan-ish label on a green fill, and honoring a wash's own fg would do
/// the same for any theme that set one.  Both washes *are* user-authorable
/// (they have to be: `blend` is a no-op on non-RGB colors, so on an
/// indexed palette a hand-picked `bg` is the only way to get a focused
/// fill at all), so this drops any fg the theme set rather than assuming
/// none exists.  `Color::Reset` is the pin when `normal` carries no fg,
/// which keeps the terminal default rather than letting the divider's
/// through.
///
/// In a monochrome theme both washes are a bare `REVERSED` over that
/// reset fg, so the chips come out identical: there the mapping is
/// carried by the reject-then-accept order and the `- ` / `+ ` markers,
/// which is why those, not this, are the load-bearing half of the change.
fn prompt_chip_style(theme: &Theme, accept: bool) -> Style {
    let wash = if accept {
        theme.diff_add_line
    } else {
        theme.diff_delete_line
    };
    let mut chip = Style::default()
        .add_modifier(wash.add_modifier)
        .add_modifier(Modifier::BOLD)
        .fg(theme.normal.fg.unwrap_or(Color::Reset));
    if let Some(bg) = wash.bg {
        chip = chip.bg(bg);
    }
    chip
}

/// Spans shown on a hunk's decision divider, given its decision and
/// whether it is the focused hunk.
///
/// Unfocused dividers show the bare checkbox / resolved label from
/// [`decision_line_text`].  The focused divider gains a leading `>`
/// caret so the active hunk is unmistakable even when its add/delete
/// wash has scrolled out of view, and the focused *pending* divider
/// additionally spells the accept/reject keys inline.  Those glyphs come
/// from the shared `diff_keys` table via [`diff_hint`], so the prompt
/// can never name a key the input handler doesn't actually honor.
///
/// **Reject leads, Accept follows** — reading order mirrors the stacking
/// (`layout::build_visual_lines` puts the old side above the divider and
/// the new side below), so the prompt encodes the mapping by position.
/// Order, unlike a directional glyph, asserts nothing that goes false on
/// an insert-only or delete-only hunk. The prompt only ever renders on a
/// *pending* divider, whose base style is the neutral
/// `diff_decision_pending`, so the chips never land on the green/red
/// wash of a resolved row.
///
/// Only the divider is color-coded; the diff hint row in
/// `ui::bottom_region` deliberately stays uniform for now.
fn decision_divider_spans(theme: &Theme, decision: Decision, focused: bool) -> Vec<Span<'static>> {
    let base = decision_line_text(decision);
    if !focused {
        return vec![Span::raw(base.to_owned())];
    }
    match decision {
        Decision::Pending => vec![
            Span::raw(format!("> {base} ")),
            Span::styled(
                format!(" Reject [{}] ", diff_hint(&Action::DiffRejectHunk)),
                prompt_chip_style(theme, false),
            ),
            Span::raw(" "),
            Span::styled(
                format!(" Accept [{}] ", diff_hint(&Action::DiffAcceptHunk)),
                prompt_chip_style(theme, true),
            ),
        ],
        Decision::Accepted | Decision::Rejected => vec![Span::raw(format!("> {base}"))],
    }
}
