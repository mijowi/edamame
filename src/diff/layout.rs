//! Visual-line model and layout cache for the diff view.
//!
//! `DiffState` owns the diff *content*; the *layout* — the flat sequence of stacked
//! old-above-new lines and each line's wrapped row count at a given width — is derived data
//! shared by the renderer ([`crate::ui::diff_view`]) and the scroll arithmetic.
//!
//! Building it is `O(total lines)` and every event-loop iteration asks for it, so it is cached on
//! `DiffState` behind a `RefCell`: the flat list is built once and a small LRU of per-width
//! prefix-sum caches ([`VisualRowCache`]) answers row queries in `O(1)` / `O(log N)`.
//!
//! The layout is invariant for a given (hunk list, new-side parse) pair — decisions and focus
//! don't alter it, since the decision divider is pinned to one row in the row cache regardless of
//! the longer prompt the focused one paints.  Installing or dropping a parse changes the line set,
//! so [`DiffState::set_rendered_parse`] invalidates; so does
//! [`DiffState::invalidate_layout`] after any reshape of the hunk list.
//!
//! See `docs/dev/diff-review.md`.

use std::collections::HashMap;
use std::ops::Range;

use ratatui::text::Line;
use ropey::Rope;

use crate::document::visual_cache::VisualRowCache;
use crate::document::wrap::{visual_rows_for_line, Indent};
use crate::document::ParsedDoc;

use super::hunk::Decision;
use super::state::DiffState;

/// One *logical* line in the diff view.  Wrapped into one or more visual rows at paint time; the
/// scroll offset indexes visual rows, not these entries.
#[derive(Debug, Clone)]
pub struct DiffVisualLine {
    pub source: DiffLineSource,
    /// Line index into the originating rope (`new_rope` for `Context` / `NewAdd`, `old_rope` for
    /// `OldDelete`).  For [`DiffLineSource::ContextRendered`] it is instead an index into
    /// `DiffState::parsed_new`'s already laid-out `lines`.
    pub rope_line: usize,
    /// Index into `DiffState::hunks`; `None` for context lines.
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
    /// Unchanged line shown as *rendered* Markdown; `rope_line` indexes `parsed_new.lines`, not a
    /// rope.  Fieldless like every other variant so `DiffLineSource` stays `Copy` and cheaply
    /// comparable — the index already has a slot on the struct.
    ContextRendered,
    /// Synthetic divider carrying the accept/reject checkbox, always emitted at the hunk's
    /// old/new boundary.  No backing rope line; its text comes from the hunk's live `Decision`.
    Decision,
}

/// Lazily-built layout cache stored on [`DiffState`].
#[derive(Default)]
pub struct DiffLayoutCache {
    /// Flat visual-line sequence; built once per layout version.
    lines: Option<Vec<DiffVisualLine>>,
    /// LRU (cap [`ROW_CACHE_CAP`]) of per-width prefix-sum caches.  Two widths are queried per
    /// frame — scrollbar-decide and post-scrollbar display — so a single slot would thrash.
    row_caches: Vec<VisualRowCache>,
    /// Memoised [`rendered_row_index`].  Width-independent, so it lives beside `lines` and is
    /// dropped with them; built on first request because only the two image paths want it.  The
    /// memo matters because the diff-side decode dispatch runs once per event-loop iteration.
    rendered_index: Option<HashMap<usize, usize>>,
}

/// Distinct widths kept warm — must be ≥ 2 for the per-frame
/// "decide bar / display bar" two-width query pattern.
const ROW_CACHE_CAP: usize = 2;

impl DiffState {
    /// Build the flat visual-line sequence for a fully raw review: context between hunks, then
    /// each hunk's deletes, divider, and adds.
    fn build_visual_lines(&self) -> Vec<DiffVisualLine> {
        let mut out: Vec<DiffVisualLine> = Vec::new();
        let new_rope = self.new_buffer.rope();
        let new_lines = new_rope.len_lines();
        let mut new_cursor: usize = 0;

        for (i, h) in self.hunks.iter().enumerate() {
            while new_cursor < h.new_lines.start && new_cursor < new_lines {
                out.push(DiffVisualLine {
                    source: DiffLineSource::Context,
                    rope_line: new_cursor,
                    hunk_idx: None,
                });
                new_cursor += 1;
            }
            // Skip the new-side range so it isn't also emitted as context.
            new_cursor = h.new_lines.end;

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

    /// Build the flat visual-line sequence for a *rendered* review: unchanged blocks emit their
    /// pre-rendered rows, changed regions keep [`Self::build_visual_lines`]'s raw stacked form.
    ///
    /// The partition is display-only ([`block_spans`]); the hunk list is never reshaped.  Snapping
    /// hunk ranges out to block boundaries would collapse the per-row table hunks
    /// `engine::split_table_hunk` produces back into one whole-table hunk.
    fn build_visual_lines_rendered(&self, parsed: &ParsedDoc) -> Vec<DiffVisualLine> {
        let total_lines = self.new_buffer.rope().len_lines();
        let (spans, owners) = block_spans(self, parsed, total_lines);
        // A new side with *no blocks at all* means the file was truncated to empty on disk.  The
        // whole-document delete would then have no span to be emitted against and the review
        // would be blank, with `all_resolved()` false so `Esc` refuses to finish.  The raw walk
        // has no such gap.
        if spans.is_empty() {
            return self.build_visual_lines();
        }
        let mut out: Vec<DiffVisualLine> = Vec::new();

        let mut i = 0usize;
        while i < spans.len() {
            if spans[i].touched {
                // A maximal run of touched blocks is one raw region.
                let start = i;
                let mut j = i;
                while j < spans.len() && spans[j].touched {
                    j += 1;
                }
                let region = spans[start].lines.start..spans[j - 1].lines.end;
                self.emit_raw_region(&region, &owners, &mut out);
                i = j;
            } else {
                // A delete-only hunk anchored exactly on this block's first line touches no
                // block, so its rows go between the two rendered runs.
                self.emit_boundary_hunks(spans[i].lines.start, &owners, &mut out);
                for row in spans[i].rows.clone() {
                    out.push(DiffVisualLine {
                        source: DiffLineSource::ContextRendered,
                        rope_line: row,
                        hunk_idx: None,
                    });
                }
                i += 1;
            }
        }
        // A boundary delete at end-of-document sits past every span.
        self.emit_boundary_hunks(total_lines, &owners, &mut out);

        // Exactly once, not merely at least once: two dividers for one decision would disagree
        // the moment the user presses `y`.
        debug_assert!(
            (0..self.hunks.len()).all(|hi| out
                .iter()
                .filter(|l| l.source == DiffLineSource::Decision && l.hunk_idx == Some(hi))
                .count()
                == 1),
            "every hunk must appear exactly once in the rendered diff line list"
        );
        out
    }

    /// Emit one raw region: the stacked walk restricted to `region` and to the hunks the
    /// partition assigned to it.
    ///
    /// **Hunks are not assumed disjoint or sorted** — the table split can produce a straddling
    /// hunk and a contained one sharing table lines, which a monotone-cursor walk would drop or
    /// double-emit.  Here context is emitted only when the hunk starts ahead of the cursor, the
    /// hunk's own rows unconditionally, and the cursor only moves forward: a hunk may end up with
    /// no context ahead of it, but is never dropped.
    fn emit_raw_region(
        &self,
        region: &Range<usize>,
        owners: &[HunkOwner],
        out: &mut Vec<DiffVisualLine>,
    ) {
        let mut new_cursor = region.start;
        for (hi, owner) in owners.iter().enumerate() {
            let in_region = match *owner {
                HunkOwner::Region { start_line } => {
                    start_line >= region.start && start_line < region.end
                }
                // A boundary delete inside a raw region is part of it; one on its far edge
                // belongs to the clean block starting there.
                HunkOwner::Boundary { line } => line >= region.start && line < region.end,
            };
            if !in_region {
                continue;
            }
            let h = &self.hunks[hi];
            // Context runs to the hunk's *own* new-side start; the owner's `start_line` names
            // the block that put it in this region, which is at or before that.
            let anchor = h.new_lines.start;
            while new_cursor < anchor.min(region.end) {
                out.push(DiffVisualLine {
                    source: DiffLineSource::Context,
                    rope_line: new_cursor,
                    hunk_idx: None,
                });
                new_cursor += 1;
            }
            self.emit_hunk(hi, out);
            new_cursor = new_cursor.max(h.new_lines.end).max(anchor);
        }
        while new_cursor < region.end {
            out.push(DiffVisualLine {
                source: DiffLineSource::Context,
                rope_line: new_cursor,
                hunk_idx: None,
            });
            new_cursor += 1;
        }
    }

    /// Emit every delete-only hunk anchored exactly at source line
    /// `line` (touching no block), in hunk-index order.
    fn emit_boundary_hunks(
        &self,
        line: usize,
        owners: &[HunkOwner],
        out: &mut Vec<DiffVisualLine>,
    ) {
        for (hi, owner) in owners.iter().enumerate() {
            if matches!(*owner, HunkOwner::Boundary { line: l } if l == line) {
                self.emit_hunk(hi, out);
            }
        }
    }

    /// The stacked rows for one hunk — deletes, divider, adds — identical to what
    /// [`Self::build_visual_lines`] emits.
    fn emit_hunk(&self, hunk_idx: usize, out: &mut Vec<DiffVisualLine>) {
        let h = &self.hunks[hunk_idx];
        for l in h.old_lines.clone() {
            out.push(DiffVisualLine {
                source: DiffLineSource::OldDelete,
                rope_line: l,
                hunk_idx: Some(hunk_idx),
            });
        }
        out.push(DiffVisualLine {
            source: DiffLineSource::Decision,
            rope_line: 0,
            hunk_idx: Some(hunk_idx),
        });
        for l in h.new_lines.clone() {
            out.push(DiffVisualLine {
                source: DiffLineSource::NewAdd,
                rope_line: l,
                hunk_idx: Some(hunk_idx),
            });
        }
    }

    /// Run `f` with the cached line list and the prefix-sum row cache for `width`, building
    /// either as needed.  Every scroll / row-count query routes through here, so the build and
    /// per-line wrap run at most once per (layout version, width).
    pub(crate) fn with_layout<R>(
        &self,
        width: usize,
        f: impl FnOnce(&[DiffVisualLine], &VisualRowCache) -> R,
    ) -> R {
        let width = width.max(1);
        let mut cache = self.layout.borrow_mut();
        self.ensure_layout(&mut cache, width);
        let lines = cache.lines.as_ref().expect("lines built above");
        let rc = cache.row_caches.first().expect("row cache built above");
        f(lines, rc)
    }

    /// As [`Self::with_layout`], plus the memoised rendered-line → flat-position map.  The single
    /// door to it, so the decode dispatch and the image-snapshot builder agree on the rows the
    /// painter walks and neither rebuilds the map per frame.
    pub(crate) fn with_layout_index<R>(
        &self,
        width: usize,
        f: impl FnOnce(&[DiffVisualLine], &VisualRowCache, &HashMap<usize, usize>) -> R,
    ) -> R {
        let width = width.max(1);
        let mut cache = self.layout.borrow_mut();
        self.ensure_layout(&mut cache, width);
        if cache.rendered_index.is_none() {
            let index = rendered_row_index(cache.lines.as_ref().expect("lines built above"));
            cache.rendered_index = Some(index);
        }
        let lines = cache.lines.as_ref().expect("lines built above");
        let rc = cache.row_caches.first().expect("row cache built above");
        let index = cache.rendered_index.as_ref().expect("index built above");
        f(lines, rc, index)
    }

    /// Populate `cache.lines` and promote-or-build the row cache for `width`.  Shared by both
    /// `with_layout*` entry points so they can't disagree about a layout version's contents.
    fn ensure_layout(&self, cache: &mut DiffLayoutCache, width: usize) {
        if cache.lines.is_none() {
            cache.lines = Some(match self.parsed_new.as_ref() {
                Some(parsed) => self.build_visual_lines_rendered(parsed),
                // No parse yet (the first frame of a review): the whole review stays raw.
                None => self.build_visual_lines(),
            });
        }
        // Promote-or-build the width entry (LRU, cap ROW_CACHE_CAP).
        if let Some(pos) = cache.row_caches.iter().position(|c| c.width() == width) {
            let entry = cache.row_caches.remove(pos);
            cache.row_caches.insert(0, entry);
        } else {
            let built = {
                let lines = cache.lines.as_ref().expect("lines built above");
                VisualRowCache::build(lines.len(), width, |i| {
                    // The divider never wraps (the renderer paints it with `wrap = false`).
                    // Pinning it to one row keeps every scroll computation independent of which
                    // hunk is focused, even though the focused divider's prompt is longer.
                    if lines[i].source == DiffLineSource::Decision {
                        1
                    } else if lines[i].source == DiffLineSource::ContextRendered {
                        // Measured as the very `Line` the painter will hand to
                        // `render_line_from_visual`, so wrap and scroll math agree by
                        // construction.
                        self.parsed_new.as_ref().map_or(1, |p| {
                            p.lines.get(lines[i].rope_line).map_or(1, |l| {
                                visual_rows_for_line(l, p.row_indent(lines[i].rope_line), width)
                            })
                        })
                    } else {
                        // Measure the marker *with* the text, behind the marker's hang, exactly
                        // as `DiffView` paints it; measuring any other way desyncs every
                        // wrapping line.
                        let text = format!(
                            "{}{}",
                            line_marker(lines[i].source),
                            line_text(self, &lines[i])
                        );
                        visual_rows_for_line(
                            &Line::from(text),
                            marker_indent(lines[i].source),
                            width,
                        )
                    }
                })
            };
            cache.row_caches.insert(0, built);
            cache.row_caches.truncate(ROW_CACHE_CAP);
        }
    }

    /// Total wrapped visual rows at `width`; `O(1)` after the first build at that width.
    pub fn total_visual_rows(&self, width: usize) -> usize {
        self.with_layout(width, |_, rc| rc.total())
    }

    /// Visual-row offset of the focused hunk's first row, for scroll-into-view.  Returns 0 when
    /// the focused id is stale or the hunk has no rendered line.
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

    /// Drop the cached layout so the next query rebuilds it.  Call after any reshape of the hunk
    /// list.
    pub fn invalidate_layout(&self) {
        let mut cache = self.layout.borrow_mut();
        cache.lines = None;
        cache.rendered_index = None;
        cache.row_caches.clear();
        self.bump_layout_version();
    }
}

/// Map each `ContextRendered` line index to its position in the flat line list, by one scan of
/// the same slice the painter walks.
///
/// Private on purpose: [`DiffLayoutCache::rendered_index`] is the only caller and
/// [`DiffState::with_layout_index`] the only door, so the map can neither be rebuilt per frame nor
/// outlive the `lines` it was scanned from.
fn rendered_row_index(lines: &[DiffVisualLine]) -> HashMap<usize, usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.source == DiffLineSource::ContextRendered)
        .map(|(i, l)| (l.rope_line, i))
        .collect()
}

/// One source-map block's share of the new-side document.
#[derive(Debug, Clone)]
struct BlockSpan {
    /// Half-open source-line range, in `new_buffer` line indices.
    lines: Range<usize>,
    /// Half-open rendered-line range into `parsed.lines`; empty for a
    /// block that renders nothing.
    rows: Range<usize>,
    /// A hunk's change lands here, so these lines show as raw source, not rendered rows.
    touched: bool,
}

/// Where one hunk's rows get emitted.
#[derive(Debug, Clone, Copy)]
enum HunkOwner {
    /// Emitted inside the raw region containing the block whose span starts at `start_line`.
    Region { start_line: usize },
    /// A delete-only hunk anchored exactly on a block boundary, so it touches no block; emitted
    /// at `line`, between two rendered runs.
    Boundary { line: usize },
}

/// Partition the new side by block, and decide where each hunk's rows go.
///
/// **Every source-map block gets a span, zero-row blocks included.**  A block that renders nothing
/// (a standalone HTML comment, a collapsed blank run) still carries source lines; without a span,
/// a hunk confined to it would touch nothing, fall in no raw region, and vanish from the review
/// while `all_resolved()` still let the user resolve it.
///
/// The walk shape is [`crate::editor::state_source_lines`]'s: source-map block space (not
/// `ParsedDoc::blocks`, which misses virtual blank blocks and the phantom trailing one), byte
/// ranges out of `ParsedDoc::source`, and a *running* newline count — `byte_to_line` is O(byte)
/// and would make this quadratic.
///
/// Each span runs to the *next* block's first line rather than being derived from its own byte
/// range, because pulldown-cmark ranges absorb trailing blank lines that have virtual blocks of
/// their own and would overlap.  The last span runs to `len_lines()`, ropey's phantom line
/// included, which is what makes the spans a total partition.
fn block_spans(
    diff: &DiffState,
    parsed: &ParsedDoc,
    total_lines: usize,
) -> (Vec<BlockSpan>, Vec<HunkOwner>) {
    let contents = parsed.source();
    let mut spans: Vec<BlockSpan> = Vec::with_capacity(parsed.source_map.block_count());
    let mut scanned = 0usize;
    let mut block_line = 0usize;

    for block_idx in 0..parsed.source_map.block_count() {
        let range = parsed
            .source_map
            .original_range_for_block(block_idx)
            .unwrap_or(scanned..scanned);
        let start = range.start.min(contents.len());
        // Advance the running count first, so it stays honest for a row-less block too.
        if start > scanned {
            block_line += contents.as_bytes()[scanned..start]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
            scanned = start;
        }
        // A row-less block's `rendered_lines_for_block` is its *neighbour's* range (the map's
        // documented fallback), so the `own` count is what decides whether it has rows.
        let rows = if parsed.block_own_line_count(block_idx) == 0 {
            0..0
        } else {
            parsed.source_map.rendered_lines_for_block(block_idx)
        };
        spans.push(BlockSpan {
            lines: block_line..block_line,
            rows,
            touched: false,
        });
    }

    // Close each span at the next one's first line; the last runs to the
    // end of the document (phantom line included).
    for i in 0..spans.len() {
        let end = spans
            .get(i + 1)
            .map_or(total_lines, |next| next.lines.start)
            .max(spans[i].lines.start);
        spans[i].lines.end = end;
    }
    if let Some(first) = spans.first_mut() {
        // A first block starting past byte 0 would leave the opening lines outside the partition.
        first.lines.start = 0;
    }
    debug_assert!(
        spans.first().is_none_or(|s| s.lines.start == 0)
            && spans.windows(2).all(|w| w[0].lines.end == w[1].lines.start)
            && spans.last().is_none_or(|s| s.lines.end == total_lines),
        "block spans must partition every source line of the new side"
    );

    // Hunks are *not* assumed disjoint or sorted (see `emit_raw_region`), so this accumulates a
    // flag per block and overlap is harmless.
    let mut owners: Vec<HunkOwner> = Vec::with_capacity(diff.hunks.len());
    for h in &diff.hunks {
        let mut first_touched: Option<usize> = None;
        if h.new_lines.is_empty() {
            // Delete-only: the block *strictly* containing the insertion point.  Landing on a
            // boundary touches nothing, which is the good case.
            let point = h.new_lines.start;
            for (bi, sp) in spans.iter().enumerate() {
                if sp.lines.start < point && point < sp.lines.end {
                    first_touched = Some(bi);
                    break;
                }
            }
        } else {
            for (bi, sp) in spans.iter().enumerate() {
                if sp.lines.start < h.new_lines.end && sp.lines.end > h.new_lines.start {
                    first_touched.get_or_insert(bi);
                }
            }
        }
        match first_touched {
            Some(bi) => owners.push(HunkOwner::Region {
                start_line: spans[bi].lines.start,
            }),
            None if h.new_lines.is_empty() => owners.push(HunkOwner::Boundary {
                line: h.new_lines.start,
            }),
            None => {
                // Unreachable — the spans cover every line.  Losing a hunk is worse than showing
                // one extra block raw, so widen the nearest block rather than dropping it.
                debug_assert!(false, "every hunk must land in a raw region");
                let bi = spans
                    .iter()
                    .rposition(|sp| sp.lines.start <= h.new_lines.start)
                    .unwrap_or(0);
                if let Some(sp) = spans.get_mut(bi) {
                    sp.touched = true;
                }
                let start_line = spans.get(bi).map_or(0, |sp| sp.lines.start);
                owners.push(HunkOwner::Region { start_line });
            }
        }
    }
    // Flagged in a second pass, after the owner decision, so an overlapping pair can't disturb it.
    for (hi, owner) in owners.iter().enumerate() {
        if !matches!(owner, HunkOwner::Region { .. }) {
            continue;
        }
        let h = &diff.hunks[hi];
        for sp in spans.iter_mut() {
            let hit = if h.new_lines.is_empty() {
                sp.lines.start < h.new_lines.start && h.new_lines.start < sp.lines.end
            } else {
                sp.lines.start < h.new_lines.end && sp.lines.end > h.new_lines.start
            };
            if hit {
                sp.touched = true;
            }
        }
    }
    (spans, owners)
}

/// Raw text of a diff visual line, stripped of its trailing `\n`.  The `Decision` divider yields
/// its checkbox text instead.
pub fn line_text(diff: &DiffState, dvl: &DiffVisualLine) -> String {
    if dvl.source == DiffLineSource::Decision {
        let dec = dvl
            .hunk_idx
            .and_then(|hi| diff.decisions.get(hi).copied())
            .unwrap_or(Decision::Pending);
        return decision_line_text(dec).to_owned();
    }
    // A rendered row has no source text.  A real arm rather than `unreachable!`: this is a
    // `pub fn`, and a panic is not worth the assertion.
    if dvl.source == DiffLineSource::ContextRendered {
        return String::new();
    }
    let rope: &Rope = match dvl.source {
        DiffLineSource::Context | DiffLineSource::NewAdd => diff.new_buffer.rope(),
        DiffLineSource::OldDelete => &diff.old_rope,
        DiffLineSource::Decision | DiffLineSource::ContextRendered => {
            unreachable!("handled above")
        }
    };
    if dvl.rope_line >= rope.len_lines() {
        return String::new();
    }
    let raw = rope.line(dvl.rope_line).to_string();
    raw.trim_end_matches('\n').to_owned()
}

/// Leading `+ ` / `- ` marker, with a matching two-space context prefix so body columns line up.
/// Unlike the background washes it survives monochrome themes, where every `diff_*` slot is
/// `Color::Reset`.
///
/// Deliberately *not* part of [`line_text`]: a hunk's inline highlight ranges index the raw line's
/// chars, so the renderer paints this as a separate leading span.  Anything that *measures* a diff
/// line must nevertheless include it — see [`DiffState::with_layout`].
pub fn line_marker(source: DiffLineSource) -> &'static str {
    match source {
        DiffLineSource::OldDelete => "- ",
        DiffLineSource::NewAdd => "+ ",
        DiffLineSource::Context => "  ",
        // A rendered row is the document painted at column 0; a marker would overflow table
        // grids and code-block padding laid out at the full viewport width.
        DiffLineSource::ContextRendered => "",
        // The divider is chrome, not a body line.
        DiffLineSource::Decision => "",
    }
}

/// The indent a marked line wraps behind: continuation rows hang past [`line_marker`], under the
/// line's text.  The diff line is raw source, so only the marker is prefix: its own Markdown
/// markers are text.  Rendered rows wrap behind their own `ParsedDoc::row_indent` instead.
pub fn marker_indent(source: DiffLineSource) -> Indent {
    Indent::hanging(line_marker(source).len())
}

/// Divider text for a `Decision`.  The resolved glyphs spell out the yes/no answer so the
/// checkbox reads as the decision itself.
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

    use crate::config::Theme;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// A review with the rendered new-side parse installed, as `refresh_diff_parse` installs it.
    fn rendered(old: &str, new: &str) -> DiffState {
        let mut st = DiffState::new(old, new).expect("non-empty diff");
        let parsed = ParsedDoc::build(new, theme(), true, 20);
        st.set_rendered_parse(Some(parsed));
        st
    }

    fn lines_of(st: &DiffState) -> Vec<DiffVisualLine> {
        st.with_layout(80, |lines, _| lines.to_vec())
    }

    fn rendered_text(st: &DiffState, dvl: &DiffVisualLine) -> String {
        st.parsed_new
            .as_ref()
            .and_then(|p| p.lines.get(dvl.rope_line))
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .unwrap_or_default()
    }

    /// Every rendered context row's text, in order.
    fn rendered_rows(st: &DiffState) -> Vec<String> {
        lines_of(st)
            .iter()
            .filter(|l| l.source == DiffLineSource::ContextRendered)
            .map(|l| rendered_text(st, l))
            .collect()
    }

    /// Raw text of every line emitted with a given source.
    fn raw_rows(st: &DiffState, want: DiffLineSource) -> Vec<String> {
        lines_of(st)
            .iter()
            .filter(|l| l.source == want)
            .map(|l| line_text(st, l))
            .collect()
    }

    #[test]
    fn a_change_in_one_paragraph_leaves_the_others_rendered() {
        let old = "# Title\n\nAlpha.\n\nBravo.\n\nCharlie.\n";
        let new = "# Title\n\nAlpha.\n\nBRAVO!\n\nCharlie.\n";
        let st = rendered(old, new);
        let rows = rendered_rows(&st);
        assert!(rows.iter().any(|r| r.contains("Title")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("Alpha.")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("Charlie.")), "{rows:?}");
        // The heading renders *styled* — its `#` marker is gone.
        assert!(!rows.iter().any(|r| r.contains('#')), "{rows:?}");
        assert_eq!(raw_rows(&st, DiffLineSource::OldDelete), vec!["Bravo."]);
        assert_eq!(raw_rows(&st, DiffLineSource::NewAdd), vec!["BRAVO!"]);
    }

    #[test]
    fn a_delete_on_a_block_boundary_lands_between_two_rendered_runs() {
        // The deleted lines start exactly on a block boundary, so no block is touched.
        let old = "Alpha.\n\nBravo.\n\nCharlie.\n";
        let new = "Alpha.\n\nCharlie.\n";
        let st = rendered(old, new);
        let lines = lines_of(&st);
        assert!(!raw_rows(&st, DiffLineSource::OldDelete).is_empty());
        assert!(lines.iter().any(|l| l.source == DiffLineSource::Decision));
        let rows = rendered_rows(&st);
        assert!(rows.iter().any(|r| r.contains("Alpha.")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("Charlie.")), "{rows:?}");
        // Nothing survives as a *raw* context line.
        assert!(
            raw_rows(&st, DiffLineSource::Context)
                .iter()
                .all(|r| r.is_empty()),
            "{:?}",
            raw_rows(&st, DiffLineSource::Context)
        );
    }

    #[test]
    fn a_changed_table_row_keeps_one_hunk_per_row_and_one_raw_region() {
        let old = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
        let new = "| a | b |\n|---|---|\n| 1 | X |\n| 3 | 4 |\n";
        let st = rendered(old, new);
        // The row split is untouched by the display partition, and the touched table's other
        // rows stay raw context rather than being painted as a grid.
        assert_eq!(st.hunks.len(), 1);
        let ctx = raw_rows(&st, DiffLineSource::Context);
        assert!(ctx.iter().any(|r| r.contains("| 3 | 4 |")), "{ctx:?}");
        assert_eq!(raw_rows(&st, DiffLineSource::NewAdd), vec!["| 1 | X |"]);
    }

    #[test]
    fn a_change_confined_to_an_html_comment_is_still_reviewable() {
        // A standalone HTML comment renders no rows, but its lines must still have a span.
        let old = "Alpha.\n\n<!-- note: one -->\n\nBravo.\n";
        let new = "Alpha.\n\n<!-- note: two -->\n\nBravo.\n";
        let st = rendered(old, new);
        assert_eq!(
            raw_rows(&st, DiffLineSource::OldDelete),
            vec!["<!-- note: one -->"]
        );
        assert_eq!(
            raw_rows(&st, DiffLineSource::NewAdd),
            vec!["<!-- note: two -->"]
        );
        assert_eq!(
            lines_of(&st)
                .iter()
                .filter(|l| l.source == DiffLineSource::Decision)
                .count(),
            1
        );
    }

    #[test]
    fn a_change_in_a_collapsed_blank_run_is_still_reviewable() {
        // The other zero-row case: with `preserve_blank_lines` off, extra blanks render nothing.
        let old = "Alpha.\n\n\n\nBravo.\n";
        let new = "Alpha.\n\n\n\nBRAVO!\n";
        let mut st = DiffState::new(old, new).expect("non-empty diff");
        st.set_rendered_parse(Some(ParsedDoc::build(new, theme(), false, 20)));
        assert_eq!(raw_rows(&st, DiffLineSource::OldDelete), vec!["Bravo."]);
        assert_eq!(raw_rows(&st, DiffLineSource::NewAdd), vec!["BRAVO!"]);
    }

    /// Every block flavor the partition has to cover.
    const MIXED_NEW: &str = "# Title\n\nAlpha text.\n\n<!-- hidden -->\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nOmega.\n\n\n";

    #[test]
    fn block_spans_partition_every_source_line() {
        let old = MIXED_NEW.replace("Alpha text.", "Alpha.");
        let st = rendered(&old, MIXED_NEW);
        let parsed = st.parsed_new.as_ref().expect("parse installed");
        let total = st.new_buffer.rope().len_lines();
        let (spans, _) = block_spans(&st, parsed, total);
        assert_eq!(spans.first().expect("blocks").lines.start, 0);
        for w in spans.windows(2) {
            assert_eq!(w[0].lines.end, w[1].lines.start, "{spans:?}");
        }
        assert_eq!(spans.last().expect("blocks").lines.end, total);
    }

    #[test]
    fn every_hunk_appears_exactly_once_in_the_line_list() {
        let old = "# Title\n\nAlpha.\n\n<!-- hidden -->\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nOmega.\n\n\n";
        let new = "# TITLE\n\nAlpha.\n\n<!-- shown -->\n\n| a | b |\n|---|---|\n| 1 | 9 |\n\nOmega!\n\n\n";
        let st = rendered(old, new);
        assert!(st.hunks.len() >= 3, "{} hunks", st.hunks.len());
        let lines = lines_of(&st);
        for hi in 0..st.hunks.len() {
            let dividers = lines
                .iter()
                .filter(|l| l.source == DiffLineSource::Decision && l.hunk_idx == Some(hi))
                .count();
            assert_eq!(dividers, 1, "hunk {hi} emitted {dividers} times");
        }
    }

    #[test]
    fn rendered_totals_are_stable_across_repeated_and_multi_width_queries() {
        let st = rendered(
            "# Title\n\nAlpha.\n\nBravo.\n",
            "# Title\n\nAlpha!\n\nBravo.\n",
        );
        let a1 = st.total_visual_rows(80);
        let b1 = st.total_visual_rows(40);
        assert_eq!(a1, st.total_visual_rows(80));
        assert_eq!(b1, st.total_visual_rows(40));
        assert!(b1 >= a1);
    }

    #[test]
    fn focused_hunk_row_lands_on_the_hunk_after_rendered_context() {
        let st = rendered(
            "# Title\n\nAlpha.\n\nBravo.\n",
            "# Title\n\nAlpha.\n\nBRAVO!\n",
        );
        let row = st.focused_hunk_visual_row(80);
        let (idx, sub) = st.with_layout(80, |_, rc| rc.find_visual_row(row));
        assert_eq!(sub, 0);
        let lines = lines_of(&st);
        assert_eq!(lines[idx].hunk_idx, st.focused_idx());
        assert!(row > 0, "rendered context precedes the hunk");
    }

    /// A file truncated to empty parses to *zero* blocks, so the whole-document delete has no
    /// span to be emitted against and the review would be blank.  The raw walk is the fallback.
    #[test]
    fn a_new_side_truncated_to_empty_still_shows_its_deletion() {
        let old = "# Title\n\nAlpha.\n";
        let st = rendered(old, "");
        assert_eq!(st.hunks.len(), 1);

        let lines = lines_of(&st);
        assert!(!lines.is_empty(), "a truncated review must not be blank");
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.source == DiffLineSource::Decision)
                .count(),
            1,
            "the deletion needs its decision divider: {lines:?}"
        );
        let deleted = raw_rows(&st, DiffLineSource::OldDelete);
        assert!(deleted.iter().any(|r| r == "# Title"), "{deleted:?}");
        assert!(deleted.iter().any(|r| r == "Alpha."), "{deleted:?}");

        // Byte for byte the raw layout, which is what the fallback is.
        let raw = DiffState::new(old, "").expect("non-empty diff");
        assert_eq!(st.total_visual_rows(80), raw.total_visual_rows(80));
        assert!(st.total_visual_rows(80) > 0);
    }

    /// The memo must be dropped with the lines it was scanned from; kept stale, image snapshots
    /// built off it would place pictures on rows belonging to other blocks.
    #[test]
    fn the_rendered_row_memo_follows_the_layout_it_was_built_from() {
        let old = "# Title\n\nAlpha.\n\nBravo.\n";
        let new = "# Title\n\nAlpha.\n\nBRAVO!\n";
        let mut st = rendered(old, new);

        let before = st.with_layout_index(80, |lines, _, index| {
            for (&row, &pos) in index {
                assert_eq!(lines[pos].source, DiffLineSource::ContextRendered);
                assert_eq!(lines[pos].rope_line, row);
            }
            index.len()
        });
        assert!(before > 0, "the clean blocks must contribute rows");
        // Same answer on a second query — the memo is a cache, not a one-shot.
        assert_eq!(st.with_layout_index(80, |_, _, index| index.len()), before);

        st.set_rendered_parse(None);
        assert_eq!(
            st.with_layout_index(80, |_, _, index| index.len()),
            0,
            "a raw layout has no rendered rows, so the memo must be rebuilt empty"
        );
    }

    #[test]
    fn dropping_the_parse_restores_the_raw_layout() {
        let old = "# Title\n\nAlpha.\n";
        let new = "# Title\n\nALPHA.\n";
        let raw = DiffState::new(old, new).expect("non-empty diff");
        let mut st = rendered(old, new);
        assert_ne!(st.total_visual_rows(80), raw.total_visual_rows(80));
        st.set_rendered_parse(None);
        assert_eq!(st.total_visual_rows(80), raw.total_visual_rows(80));
    }

    /// `n` leading context lines, a single-line replace, and a trailing context line.  Nothing
    /// wraps at width 80.
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
        // 5 context lines precede the change.
        let st = diff_with_leading_context(5);
        assert_eq!(st.focused_hunk_visual_row(80), 5);
    }

    #[test]
    fn total_visual_rows_counts_every_stacked_line() {
        // 5 context + 1 delete + 1 divider + 1 add + 1 trailing context + 1 empty = 10.
        let st = diff_with_leading_context(5);
        assert_eq!(st.total_visual_rows(80), 10);
    }

    #[test]
    fn cached_total_survives_repeated_and_multi_width_queries() {
        // Two widths must both stay within the LRU and return stable totals.
        let st = diff_with_leading_context(3);
        let a1 = st.total_visual_rows(80);
        let b1 = st.total_visual_rows(40);
        let a2 = st.total_visual_rows(80);
        let b2 = st.total_visual_rows(40);
        assert_eq!(a1, a2);
        assert_eq!(b1, b2);
        assert!(b1 >= a1);
    }

    #[test]
    fn invalidate_layout_forces_rebuild() {
        let st = diff_with_leading_context(4);
        let before = st.total_visual_rows(80);
        st.invalidate_layout();
        assert_eq!(st.total_visual_rows(80), before);
    }
}
