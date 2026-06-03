//! Raw stacked diff view (Phase 1 §5).  Renders a flat sequence of
//! [`DiffVisualLine`] entries — interleaving unchanged context with
//! per-hunk old-above-new pairs — to ratatui via the shared
//! [`crate::ui::line_render`] helper so the trailing-cell bg fill and
//! word-aware wrap match the other modes.
//!
//! Phase 2 (`docs/diff-mode-plan.md` §16) will upgrade this to a
//! hybrid rendered view; CP3 keeps it raw-only so we can ship the
//! review-and-decide flow without touching `ParsedDoc` /
//! `SourceMap`.

use ratatui::{
    buffer::Buffer as TuiBuf,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::Theme;
use crate::diff::hunk::InlineSide;
use crate::diff::layout::{line_text, DiffLineSource, DiffVisualLine};
use crate::diff::{Decision, DiffState};
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
                let text = line_text(self.diff, dvl);
                let line = build_line(self.diff, self.theme, dvl, &text);
                let painted =
                    render_line_from_visual(&line, area, buf, visual_y, true, skip_first_subrow);
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

fn build_line(diff: &DiffState, theme: &Theme, dvl: &DiffVisualLine, text: &str) -> Line<'static> {
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
        // Unfocused dividers collapse to a single muted style that
        // recedes with the rest of the hunk; the focused one keeps its
        // per-state hue and is bolded so the actionable checkbox draws
        // the eye.
        let style = if focused {
            let base = match dec {
                Decision::Pending => theme.diff_decision_pending,
                Decision::Accepted => theme.diff_decision_accepted,
                Decision::Rejected => theme.diff_decision_rejected,
            };
            base.add_modifier(Modifier::BOLD)
        } else {
            theme.diff_decision_unfocused
        };
        // Set the same style as the line base so the trailing-cell fill
        // extends the background across the full row width.
        return Line::from(Span::styled(text.to_owned(), style)).style(style);
    }

    // Delete / add / context lines.  No gutter: all start at column 0
    // and are distinguished by background color alone.  Focus selects
    // both the full-line wash and the within-line highlight: a
    // non-focused hunk uses the muted `_unfocused` variants of both so
    // its changed words recede with its background instead of popping at
    // full saturation.
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
