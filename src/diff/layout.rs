//! Visual-line model and layout cache for the diff view.
//!
//! `DiffState` owns the diff *content* (hunks, ropes, decisions); the
//! *layout* — how that content maps to a flat sequence of stacked
//! old-above-new lines, and how many wrapped visual rows each line
//! occupies at a given width — is derived data that both the renderer
//! ([`crate::ui::diff_view`]) and the scroll arithmetic
//! ([`crate::editor::EditorState::total_visual_rows_for_mode`],
//! scroll-into-view) need.
//!
//! Building the flat sequence and wrapping every line is `O(total
//! lines)`.  It was previously recomputed on *every* event-loop
//! iteration (the per-frame `compute_doc_dims` calls
//! `total_visual_rows_for_mode`, and the renderer rebuilt the list
//! again), which made scrolling and hunk navigation visibly laggy on
//! large diffs.
//!
//! The layout is invariant for the lifetime of a review — decisions
//! and focus changes don't alter the line set or its wrapping — so we
//! cache it on `DiffState` behind a `RefCell`.  (The focused decision
//! divider *renders* a longer prompt than the others, but the divider
//! is pinned to a single row in the row cache — see `with_layout` — so
//! its row count, and thus all scroll math, stays focus-independent.)
//! The flat line list is
//! built once, and a small LRU of per-width prefix-sum caches
//! ([`VisualRowCache`]) answers row-count / scroll-position queries in
//! `O(1)` / `O(log N)`.  Anything that mutates the hunk list (e.g. a
//! reconcile, or a future Edit mode) calls
//! [`DiffState::invalidate_layout`] to force a rebuild.

use ratatui::text::Line;
use ropey::Rope;

use crate::document::visual_cache::VisualRowCache;
use crate::ui::line_render::visual_rows_for_line;

use super::hunk::Decision;
use super::state::DiffState;

/// One *logical* line in the diff view.  Expanded into one or more
/// visual rows at paint time according to word-wrap; the scroll offset
/// indexes visual rows, not `DiffVisualLine` entries.
#[derive(Debug, Clone)]
pub struct DiffVisualLine {
    pub source: DiffLineSource,
    /// Line index into the originating rope (`new_rope` for `Context`
    /// / `NewAdd`, `old_rope` for `OldDelete`).
    pub rope_line: usize,
    /// Index into `DiffState::hunks`, when this line belongs to a
    /// hunk.  `None` for `Context` lines.
    pub hunk_idx: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineSource {
    /// Unchanged line, borrowed from `new_rope`.
    Context,
    /// Delete-side line, borrowed from `old_rope`.
    OldDelete,
    /// Add-side line, borrowed from `new_rope`.
    NewAdd,
    /// Synthetic divider carrying the accept/reject checkbox, emitted
    /// between a hunk's delete and add lines (so it sits below a
    /// delete-only hunk and above an add-only one — always at the
    /// old/new boundary).  Has no backing rope line; its text is
    /// derived from the hunk's live `Decision`.
    Decision,
}

/// Lazily-built layout cache stored on [`DiffState`].
#[derive(Default)]
pub struct DiffLayoutCache {
    /// Flat visual-line sequence; built once per layout version.
    lines: Option<Vec<DiffVisualLine>>,
    /// LRU (cap [`ROW_CACHE_CAP`]) of per-width prefix-sum caches over
    /// `lines`.  Two distinct widths are queried per frame (the
    /// scrollbar-decide width and the post-scrollbar display width),
    /// matching the raw-mode cache rationale, so a single slot would
    /// thrash.
    row_caches: Vec<VisualRowCache>,
}

/// Distinct widths kept warm — must be ≥ 2 for the per-frame
/// "decide bar / display bar" two-width query pattern.
const ROW_CACHE_CAP: usize = 2;

impl DiffState {
    /// Build the flat visual-line sequence.  Walks hunks in document
    /// order; between hunks emits `Context` lines from `new_rope`,
    /// then per hunk emits `OldDelete` lines (from `old_rope`)
    /// followed by `NewAdd` lines (from `new_rope`).
    fn build_visual_lines(&self) -> Vec<DiffVisualLine> {
        let mut out: Vec<DiffVisualLine> = Vec::new();
        let new_rope = self.new_buffer.rope();
        let new_lines = new_rope.len_lines();
        let mut new_cursor: usize = 0;

        for (i, h) in self.hunks.iter().enumerate() {
            // Emit context up to the hunk's new-side start.
            while new_cursor < h.new_lines.start && new_cursor < new_lines {
                out.push(DiffVisualLine {
                    source: DiffLineSource::Context,
                    rope_line: new_cursor,
                    hunk_idx: None,
                });
                new_cursor += 1;
            }
            // Skip over the new-side range so we don't double-emit it
            // as context.  Even for `Delete` (new_lines empty) this is
            // a no-op.
            new_cursor = h.new_lines.end;

            // Stacked order: deletes above, the decision divider, then
            // adds below.  The divider always sits at the old/new
            // boundary, so it lands below a delete-only hunk and above
            // an add-only one.
            for l in h.old_lines.clone() {
                out.push(DiffVisualLine {
                    source: DiffLineSource::OldDelete,
                    rope_line: l,
                    hunk_idx: Some(i),
                });
            }
            out.push(DiffVisualLine {
                source: DiffLineSource::Decision,
                rope_line: 0,
                hunk_idx: Some(i),
            });
            for l in h.new_lines.clone() {
                out.push(DiffVisualLine {
                    source: DiffLineSource::NewAdd,
                    rope_line: l,
                    hunk_idx: Some(i),
                });
            }
        }

        // Trailing context.
        while new_cursor < new_lines {
            out.push(DiffVisualLine {
                source: DiffLineSource::Context,
                rope_line: new_cursor,
                hunk_idx: None,
            });
            new_cursor += 1;
        }

        out
    }

    /// Run `f` with the cached flat visual-line list and the
    /// prefix-sum row cache for `width`, building or refreshing either
    /// as needed.  All scroll / total-row / scroll-into-view queries
    /// route through here so the expensive build + per-line wrap runs
    /// at most once per (layout version, width).
    ///
    /// `pub(crate)` because it hands out a borrow of the crate-private
    /// [`VisualRowCache`]; the public surface is [`Self::total_visual_rows`]
    /// and [`Self::focused_hunk_visual_row`].
    pub(crate) fn with_layout<R>(
        &self,
        width: usize,
        f: impl FnOnce(&[DiffVisualLine], &VisualRowCache) -> R,
    ) -> R {
        let width = width.max(1);
        let mut cache = self.layout.borrow_mut();
        if cache.lines.is_none() {
            cache.lines = Some(self.build_visual_lines());
        }
        // Promote-or-build the width entry (LRU, cap ROW_CACHE_CAP).
        if let Some(pos) = cache.row_caches.iter().position(|c| c.width() == width) {
            let entry = cache.row_caches.remove(pos);
            cache.row_caches.insert(0, entry);
        } else {
            let built = {
                let lines = cache.lines.as_ref().expect("lines built above");
                VisualRowCache::build(lines.len(), width, |i| {
                    // The decision divider is a single-row status strip
                    // that never wraps (the renderer paints it with
                    // `wrap = false`).  Pinning it to one row here keeps
                    // the wrap cache — and therefore every scroll
                    // computation — independent of which hunk is focused,
                    // even though the focused divider's text is longer
                    // (it spells out the accept/reject prompt).  Without
                    // the pin, focusing a hunk on a very narrow terminal
                    // could change a divider's wrapped height and
                    // silently desync the cached total.
                    if lines[i].source == DiffLineSource::Decision {
                        1
                    } else {
                        // Measure the marker *with* the text, and through
                        // `visual_rows_for_line` rather than
                        // `visual_rows_of_str`: `render_line` derives a
                        // hanging indent from a line's leading marker, and
                        // `- ` / `+ ` / a two-space context prefix all match
                        // its recognized shapes (raw bullet, indented
                        // continuation).  Measuring flat here while the
                        // painter wrapped at indent 2 would desync every
                        // scroll computation on any line that wraps.
                        let text = format!(
                            "{}{}",
                            line_marker(lines[i].source),
                            line_text(self, &lines[i])
                        );
                        visual_rows_for_line(&Line::from(text), width)
                    }
                })
            };
            cache.row_caches.insert(0, built);
            cache.row_caches.truncate(ROW_CACHE_CAP);
        }
        let lines = cache.lines.as_ref().expect("lines built above");
        let rc = cache.row_caches.first().expect("row cache built above");
        f(lines, rc)
    }

    /// Total wrapped visual rows for the diff at `width`.  `O(1)` after
    /// the first build at that width.  Used by the bottom scrollbar and
    /// by viewport / scroll clamping.
    pub fn total_visual_rows(&self, width: usize) -> usize {
        self.with_layout(width, |_, rc| rc.total())
    }

    /// Visual-row offset (at `width`) of the first row of the focused
    /// hunk, so the caller can scroll it into view.  Returns 0 when the
    /// focused id is stale or the hunk has no rendered line.
    pub fn focused_hunk_visual_row(&self, width: usize) -> usize {
        let Some(focused_idx) = self.focused_idx() else {
            return 0;
        };
        self.with_layout(width, |lines, rc| {
            match lines.iter().position(|l| l.hunk_idx == Some(focused_idx)) {
                Some(pos) => rc.before(pos),
                None => 0,
            }
        })
    }

    /// Drop the cached layout so the next query rebuilds it.  Called
    /// after any reshape of the hunk list (e.g. a reconcile, or a
    /// future Edit mode); a cheap safety valve while the list is fixed.
    pub fn invalidate_layout(&self) {
        let mut cache = self.layout.borrow_mut();
        cache.lines = None;
        cache.row_caches.clear();
    }
}

/// Raw text of a diff visual line, stripped of its trailing `\n`.  For
/// the synthetic `Decision` divider this is the checkbox plus a resolved
/// label (`[ ]` while pending, `[Y] Accepted`, `[N] Rejected`).
pub fn line_text(diff: &DiffState, dvl: &DiffVisualLine) -> String {
    if dvl.source == DiffLineSource::Decision {
        let dec = dvl
            .hunk_idx
            .and_then(|hi| diff.decisions.get(hi).copied())
            .unwrap_or(Decision::Pending);
        return decision_line_text(dec).to_owned();
    }
    let rope: &Rope = match dvl.source {
        DiffLineSource::Context | DiffLineSource::NewAdd => diff.new_buffer.rope(),
        DiffLineSource::OldDelete => &diff.old_rope,
        DiffLineSource::Decision => unreachable!("handled above"),
    };
    if dvl.rope_line >= rope.len_lines() {
        return String::new();
    }
    let raw = rope.line(dvl.rope_line).to_string();
    raw.trim_end_matches('\n').to_owned()
}

/// Leading side marker for a diff visual line — the unified-diff `+ ` /
/// `- ` convention, with a matching two-space prefix on context lines so
/// every body column lines up.
///
/// Unlike the add/delete background washes, the marker is correct for
/// degenerate hunks (a `HunkKind::Delete` hunk visibly has only `- `
/// rows) and survives monochrome themes, where every `diff_*` palette
/// slot is `Color::Reset` and color carries nothing.
///
/// The marker is *not* part of [`line_text`]: the inline highlight
/// ranges on a hunk index into the raw line's chars, so the renderer
/// paints this as a separate leading span rather than concatenating it.
/// Anything that measures a diff line must include it — see
/// [`DiffState::with_layout`].
pub fn line_marker(source: DiffLineSource) -> &'static str {
    match source {
        DiffLineSource::OldDelete => "- ",
        DiffLineSource::NewAdd => "+ ",
        DiffLineSource::Context => "  ",
        // The divider is chrome, not a body line; it spans the full row.
        DiffLineSource::Decision => "",
    }
}

/// Text shown on a hunk's decision divider for a given `Decision`.
/// Pending shows only the checkbox; resolved states append a label.
/// The resolved glyphs spell out the yes/no answer (`[Y]` = accept,
/// `[N]` = reject) so the checkbox reads as the decision itself.
pub fn decision_line_text(decision: Decision) -> &'static str {
    match decision {
        Decision::Pending => "[ ]",
        Decision::Accepted => "[Y] Accepted",
        Decision::Rejected => "[N] Rejected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Diff with `n` leading context lines, a single-line replace, and
    /// a trailing context line.  No line wraps at width 80.
    fn diff_with_leading_context(n: usize) -> DiffState {
        let mut old = String::new();
        for i in 0..n {
            old.push_str(&format!("ctx{i}\n"));
        }
        let mut new = old.clone();
        old.push_str("before\n");
        new.push_str("AFTER\n");
        old.push_str("tail\n");
        new.push_str("tail\n");
        DiffState::new(&old, &new).expect("non-empty diff")
    }

    #[test]
    fn focused_hunk_row_skips_leading_context() {
        // 5 context lines precede the change, so the focused hunk's
        // first visual row is row 5 at a non-wrapping width.
        let st = diff_with_leading_context(5);
        assert_eq!(st.focused_hunk_visual_row(80), 5);
    }

    #[test]
    fn total_visual_rows_counts_every_stacked_line() {
        // 5 context + 1 deleted (`before`) + 1 decision divider + 1
        // added (`AFTER`) + 1 trailing context (`tail`) + 1 empty
        // trailing line = 10 rows, none wrapping at width 80.
        let st = diff_with_leading_context(5);
        assert_eq!(st.total_visual_rows(80), 10);
    }

    #[test]
    fn cached_total_survives_repeated_and_multi_width_queries() {
        // Two widths exercised repeatedly must stay within the LRU and
        // return stable, correct totals (regression: the pre-cache code
        // rebuilt + rewrapped on every call).
        let st = diff_with_leading_context(3);
        let a1 = st.total_visual_rows(80);
        let b1 = st.total_visual_rows(40);
        let a2 = st.total_visual_rows(80);
        let b2 = st.total_visual_rows(40);
        assert_eq!(a1, a2);
        assert_eq!(b1, b2);
        // Narrower width never yields fewer rows (lines may wrap).
        assert!(b1 >= a1);
    }

    #[test]
    fn invalidate_layout_forces_rebuild() {
        let st = diff_with_leading_context(4);
        let before = st.total_visual_rows(80);
        st.invalidate_layout();
        // Same content → same answer after a forced rebuild.
        assert_eq!(st.total_visual_rows(80), before);
    }
}
