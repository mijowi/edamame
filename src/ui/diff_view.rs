//! Stacked diff view: paints the flat [`DiffVisualLine`] sequence (context interleaved with
//! per-hunk old-above-new pairs) through [`crate::ui::line_render`] so wrap and trailing-cell
//! fill match the other modes.
//!
//! Unchanged regions are painted as *rendered* Markdown when `DiffState::parsed_new` is
//! installed (`DiffLineSource::ContextRendered` rows are finished lines from that parse, no
//! marker, no wash); changed regions stay raw with markers, washes, inline highlights, and the
//! decision divider.  Without a parse every line is raw.  See `docs/dev/diff-review.md`.

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
    decision_line_text, line_marker, line_text, marker_indent, DiffLineSource, DiffVisualLine,
};
use crate::diff::{Decision, DiffState};
use crate::document::wrap::Indent;
use crate::input::diff_hint;
use crate::ui::line_render::render_line_from_visual;

/// Per-frame state for [`DiffView`].  The line sequence and row counts are cached on
/// [`DiffState`] (see [`crate::diff::layout`]); only image-snapshot geometry lives here.
#[derive(Debug, Default)]
pub struct DiffViewState {
    /// Geometry of images visible in clean (rendered) regions; consumed by `EditorView`'s
    /// `paint_images` pass.
    pub image_snapshots: Vec<crate::ui::ImageLayoutSnapshot>,
    /// Cache key: `(scroll, area, DiffState::layout_version)`.  The editor's `parsed_version`
    /// tracks a different document and is wrong here.
    pub image_snapshots_key: Option<(usize, Rect, u64)>,
}

pub struct DiffView<'a> {
    pub diff: &'a DiffState,
    pub theme: &'a Theme,
    /// Visual-row scroll offset ([`crate::editor::EditorState::scroll`]).
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
            let (start_idx, mut skip_first_subrow) = rc.find_visual_row(scroll);

            let mut idx = start_idx;
            let mut visual_y: u16 = 0;
            while idx < lines.len() && visual_y < area.height {
                let dvl = &lines[idx];
                let line = build_line(self.diff, self.theme, dvl);
                // The decision divider is pinned to one row in the layout cache, so no wrap.
                let wrap = dvl.source != DiffLineSource::Decision;
                let indent = if dvl.source == DiffLineSource::ContextRendered {
                    self.diff
                        .parsed_new
                        .as_ref()
                        .map_or(Indent::NONE, |p| p.row_indent(dvl.rope_line))
                } else {
                    marker_indent(dvl.source)
                };
                let painted = render_line_from_visual(
                    &line,
                    indent,
                    area,
                    buf,
                    visual_y,
                    wrap,
                    skip_first_subrow,
                );
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
    // Clean-region line: the renderer's row as-is, no marker, no wash.
    if dvl.source == DiffLineSource::ContextRendered {
        return diff
            .parsed_new
            .as_ref()
            .and_then(|p| p.lines.get(dvl.rope_line))
            .cloned()
            .unwrap_or_default();
    }

    // Decision divider.  Its style goes on the line base so the trailing-cell fill paints the
    // whole row.
    if dvl.source == DiffLineSource::Decision {
        let focused = dvl
            .hunk_idx
            .is_some_and(|hi| diff.hunks[hi].id == diff.focused_id);
        let dec = dvl
            .hunk_idx
            .and_then(|hi| diff.decisions.get(hi).copied())
            .unwrap_or(Decision::Pending);
        // A resolved unfocused divider borrows the focused state's fg hue over the muted strip
        // and adds DIM.  Taking the hue from the focused style (not the palette) keeps
        // monochrome themes correct: no fg there, so it stays a plain DIM strip.
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
        // The `(i/n)` counter dims and drops the inherited bold; DIM rather than a muted color
        // so it stays recessive in monochrome themes.
        let position = dvl.hunk_idx.map_or(0, |hi| hi + 1);
        let total = diff.hunks.len();
        let counter_style = Style::default()
            .add_modifier(Modifier::DIM)
            .remove_modifier(Modifier::BOLD);
        let mut spans = decision_divider_spans(theme, dec, focused, diff.read_only);
        // On a read-only unfocused divider the counter is the whole row.
        let counter = if spans.is_empty() {
            format!("({position}/{total})")
        } else {
            format!(" ({position}/{total})")
        };
        spans.push(Span::styled(counter, counter_style));
        return Line::from(spans).style(style);
    }

    // Focus selects both the full-line wash and the inline highlight, so an unfocused hunk's
    // changed words recede with its background.
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

    // The marker is its own span (not folded into `text`) because the inline ranges index the
    // raw line's chars.
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(body_spans.len() + 1);
    spans.push(Span::raw(line_marker(dvl.source)));
    spans.extend(body_spans);

    Line::from(spans).style(line_style)
}

/// Chip style for one side of the focused pending prompt.
///
/// Reuses the add/delete row wash so each label is painted in the color of the text it acts
/// on.  Only the wash's `bg` and modifiers are taken; the fg is pinned from `theme.normal`
/// (`Color::Reset` if none) because inheriting the divider's fg, or a user-set wash fg, would
/// mis-color the label.  In monochrome themes both chips come out identical, and the mapping
/// is carried by the reject-then-accept order and the `- ` / `+ ` markers instead.
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

/// Spans for a hunk's decision divider.
///
/// Unfocused: the bare checkbox / label from [`decision_line_text`].  Focused: a leading `>`
/// caret, plus (while pending) inline accept/reject chips whose keys come from [`diff_hint`]
/// so they can never disagree with the input handler.  Reject leads and Accept follows to
/// mirror the old-above/new-below stacking; order, unlike a directional glyph, stays true on
/// insert-only and delete-only hunks.
///
/// A read-only review gets no checkbox and no prompt (it has no decision vocabulary), gated by
/// the same `DiffState::read_only` flag that shortens the hint row, so the two surfaces agree.
fn decision_divider_spans(
    theme: &Theme,
    decision: Decision,
    focused: bool,
    read_only: bool,
) -> Vec<Span<'static>> {
    if read_only {
        return if focused {
            vec![Span::raw(">")]
        } else {
            Vec::new()
        };
    }
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
