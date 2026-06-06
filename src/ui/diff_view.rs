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
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::{Action, Theme};
use crate::diff::hunk::InlineSide;
use crate::diff::layout::{decision_line_text, line_text, DiffLineSource, DiffVisualLine};
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
        let divider = decision_divider_text(dec, focused);
        let position = dvl.hunk_idx.map_or(0, |hi| hi + 1);
        let total = diff.hunks.len();
        let counter_style = Style::default()
            .add_modifier(Modifier::DIM)
            .remove_modifier(Modifier::BOLD);
        let spans = vec![
            Span::raw(divider),
            Span::styled(format!(" ({position}/{total})"), counter_style),
        ];
        return Line::from(spans).style(style);
    }

    // Delete / add / context lines pull their text from the rope; the
    // decision branch above never needs it, so we only pay the
    // allocation here.  No gutter: all start at column 0
    // and are distinguished by background color alone.  Focus selects
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

    Line::from(body_spans).style(line_style)
}

/// Text shown on a hunk's decision divider, given its decision and
/// whether it is the focused hunk.
///
/// Unfocused dividers show the bare checkbox / resolved label from
/// [`decision_line_text`].  The focused divider gains a leading `>`
/// caret so the active hunk is unmistakable even when its add/delete
/// wash has scrolled out of view, and the focused *pending* divider
/// additionally spells the accept/reject keys inline.  Those glyphs come
/// from the shared `diff_keys` table via [`diff_hint`], so the prompt
/// can never name a key the input handler doesn't actually honor.
fn decision_divider_text(decision: Decision, focused: bool) -> String {
    let base = decision_line_text(decision);
    if !focused {
        return base.to_owned();
    }
    match decision {
        Decision::Pending => format!(
            "> {base} Accept [{}] · Reject [{}]",
            diff_hint(&Action::DiffAcceptHunk),
            diff_hint(&Action::DiffRejectHunk),
        ),
        Decision::Accepted | Decision::Rejected => format!("> {base}"),
    }
}
