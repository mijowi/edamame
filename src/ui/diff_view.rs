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
    style::Style,
    text::{Line, Span},
    widgets::StatefulWidget,
};

use crate::config::Theme;
use crate::diff::hunk::{HunkKind, InlineSide};
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
    let (line_style, focused) = match (dvl.source, dvl.hunk_idx) {
        (DiffLineSource::Context, _) => (Style::default(), false),
        (DiffLineSource::OldDelete, Some(hi)) => {
            let focused = diff.hunks[hi].id == diff.focused_id;
            (theme.diff_delete_line, focused)
        }
        (DiffLineSource::NewAdd, Some(hi)) => {
            let focused = diff.hunks[hi].id == diff.focused_id;
            (theme.diff_add_line, focused)
        }
        _ => (Style::default(), false),
    };

    // Build the body spans with optional inline highlights.
    let mut body_spans: Vec<Span<'static>> = Vec::new();
    let inline_bg = match dvl.source {
        DiffLineSource::OldDelete => Some(theme.diff_delete_inline),
        DiffLineSource::NewAdd => Some(theme.diff_add_inline),
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

    // Gutter: focused-hunk indicator + decision glyph on the first
    // line of the hunk's "indicator" side.
    let mut spans: Vec<Span<'static>> = Vec::new();
    if dvl.first_of_hunk {
        let glyph = if focused { "> " } else { "  " };
        spans.push(Span::styled(glyph.to_owned(), theme.diff_cursor_gutter));
        if let Some(hi) = dvl.hunk_idx {
            let dec = diff.decisions[hi];
            // Show the decision indicator on the first new-side line
            // for Insert/Replace, and on the first old-side line for
            // Delete hunks.
            let h = &diff.hunks[hi];
            let want_here = match h.kind {
                HunkKind::Delete => dvl.source == DiffLineSource::OldDelete,
                HunkKind::Insert | HunkKind::Replace => dvl.source == DiffLineSource::NewAdd,
            };
            if want_here {
                let glyph = match dec {
                    Decision::Pending => "[ ] ",
                    Decision::Accepted => "[✓] ",
                    Decision::Rejected => "[x] ",
                };
                spans.push(Span::raw(glyph.to_owned()));
            } else {
                spans.push(Span::raw("    ".to_owned()));
            }
        }
    } else if dvl.hunk_idx.is_some() {
        spans.push(Span::raw("      ".to_owned()));
    }
    spans.extend(body_spans);

    Line::from(spans).style(line_style)
}
