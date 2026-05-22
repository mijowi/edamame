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
use ropey::Rope;

use crate::config::Theme;
use crate::diff::hunk::{HunkKind, InlineSide};
use crate::diff::{Decision, DiffState};
use crate::ui::line_render::{render_line_from_visual, visual_rows_of_str};

/// One *logical* line in the diff view.  Expanded into one or more
/// visual rows at paint time according to word-wrap; `scroll` indexes
/// visual rows, not `DiffVisualLine` entries.
#[derive(Debug, Clone)]
pub struct DiffVisualLine {
    pub source: DiffLineSource,
    /// Line index into the originating rope (`new_rope` for `Context`
    /// / `NewAdd`, `old_rope` for `OldDelete`).
    pub rope_line: usize,
    /// Index into `DiffState::hunks`, when this line belongs to a
    /// hunk.  `None` for `Context` lines.
    pub hunk_idx: Option<usize>,
    /// `true` for the first line of the hunk's old-side range (for
    /// `OldDelete`) or new-side range (for `NewAdd`).  Used to paint
    /// the gutter glyph and decision indicator on the right cells.
    pub first_of_hunk: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineSource {
    /// Unchanged line, borrowed from `new_rope`.
    Context,
    /// Delete-side line, borrowed from `old_rope`.
    OldDelete,
    /// Add-side line, borrowed from `new_rope`.
    NewAdd,
}

#[derive(Debug, Default)]
pub struct DiffViewState {
    /// Cached materialised line sequence for the active diff state.
    /// Rebuilt whenever the hunk list changes; CP3 rebuilds every
    /// frame for simplicity (the cost is `O(total lines)` and
    /// negligible for typical markdown files).
    pub visual_lines: Vec<DiffVisualLine>,
}

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

    fn render(self, area: Rect, buf: &mut TuiBuf, view_state: &mut Self::State) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let width = area.width as usize;

        view_state.visual_lines = build_visual_lines(self.diff);

        // Pre-compute the wrapped visual-row offset of each
        // `DiffVisualLine` so the scroll index can walk into the
        // middle of a wrapped line cleanly.  We walk the list once
        // until we exhaust `area.height`.
        let scroll = self.scroll;
        let mut consumed_rows: usize = 0;
        let mut idx: usize = 0;
        let mut skip_first_subrow: usize = 0;
        // Skip non-rendered visual rows up to `scroll`.
        while idx < view_state.visual_lines.len() {
            let dvl = &view_state.visual_lines[idx];
            let text = line_text(self.diff, dvl);
            let rows = visual_rows_of_str(&text, width.max(1)).len().max(1);
            if consumed_rows + rows > scroll {
                skip_first_subrow = scroll - consumed_rows;
                break;
            }
            consumed_rows += rows;
            idx += 1;
        }

        let mut visual_y: u16 = 0;
        while idx < view_state.visual_lines.len() && visual_y < area.height {
            let dvl = view_state.visual_lines[idx].clone();
            let text = line_text(self.diff, &dvl);
            let line = build_line(self.diff, self.theme, &dvl, &text);
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
    }
}

/// Build the flat visual-line sequence for a diff state.  Walks
/// hunks in document order; between hunks emits `Context` lines
/// from `new_rope`, then per hunk emits `OldDelete` lines (from
/// `old_rope`) followed by `NewAdd` lines (from `new_rope`).
pub fn build_visual_lines(diff: &DiffState) -> Vec<DiffVisualLine> {
    let mut out: Vec<DiffVisualLine> = Vec::new();
    let new_rope = diff.new_buffer.rope();
    let new_lines = new_rope.len_lines();
    let mut new_cursor: usize = 0;

    for (i, h) in diff.hunks.iter().enumerate() {
        // Emit context up to the hunk's new-side start.
        while new_cursor < h.new_lines.start && new_cursor < new_lines {
            out.push(DiffVisualLine {
                source: DiffLineSource::Context,
                rope_line: new_cursor,
                hunk_idx: None,
                first_of_hunk: false,
            });
            new_cursor += 1;
        }
        // Skip over the new-side range so we don't double-emit it
        // as context.  Even for `Delete` (new_lines empty) this is
        // a no-op.
        new_cursor = h.new_lines.end;

        // Stacked order: deletes above, adds below.
        let mut first = true;
        for l in h.old_lines.clone() {
            out.push(DiffVisualLine {
                source: DiffLineSource::OldDelete,
                rope_line: l,
                hunk_idx: Some(i),
                first_of_hunk: first,
            });
            first = false;
        }
        let mut first = true;
        for l in h.new_lines.clone() {
            out.push(DiffVisualLine {
                source: DiffLineSource::NewAdd,
                rope_line: l,
                hunk_idx: Some(i),
                first_of_hunk: first,
            });
            first = false;
        }
    }

    // Trailing context.
    while new_cursor < new_lines {
        out.push(DiffVisualLine {
            source: DiffLineSource::Context,
            rope_line: new_cursor,
            hunk_idx: None,
            first_of_hunk: false,
        });
        new_cursor += 1;
    }

    out
}

/// Get the raw text of a diff visual line, stripped of trailing `\n`.
fn line_text(diff: &DiffState, dvl: &DiffVisualLine) -> String {
    let rope: &Rope = match dvl.source {
        DiffLineSource::Context | DiffLineSource::NewAdd => diff.new_buffer.rope(),
        DiffLineSource::OldDelete => &diff.old_rope,
    };
    if dvl.rope_line >= rope.len_lines() {
        return String::new();
    }
    let raw = rope.line(dvl.rope_line).to_string();
    raw.trim_end_matches('\n').to_owned()
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

/// Compute the total number of wrapped visual rows for the diff
/// state at `width`.  Used by the bottom scrollbar and by viewport
/// clamping.
pub fn total_visual_rows(diff: &DiffState, width: usize) -> usize {
    let lines = build_visual_lines(diff);
    if width == 0 {
        return lines.len();
    }
    let mut total = 0usize;
    for dvl in &lines {
        let text = line_text(diff, dvl);
        total += visual_rows_of_str(&text, width).len().max(1);
    }
    total
}
