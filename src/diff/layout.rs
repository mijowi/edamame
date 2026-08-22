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
//! The layout is invariant for a given (hunk list, new-side parse)
//! pair — decisions and focus changes don't alter the line set or its
//! wrapping — so we cache it on `DiffState` behind a `RefCell`.  The
//! line *set* depends on the parse as well as on the hunks (which
//! blocks are clean, and how many rendered rows each contributes), so
//! installing or dropping a parse invalidates too — which
//! [`DiffState::set_rendered_parse`] does for every caller.  (The focused decision
//! divider *renders* a longer prompt than the others, but the divider
//! is pinned to a single row in the row cache — see `with_layout` — so
//! its row count, and thus all scroll math, stays focus-independent.)
//! The flat line list is
//! built once, and a small LRU of per-width prefix-sum caches
//! ([`VisualRowCache`]) answers row-count / scroll-position queries in
//! `O(1)` / `O(log N)`.  Anything that mutates the hunk list (e.g. a
//! reconcile, or a future Edit mode) calls
//! [`DiffState::invalidate_layout`] to force a rebuild.

use std::collections::HashMap;
use std::ops::Range;

use ratatui::text::Line;
use ropey::Rope;

use crate::document::visual_cache::VisualRowCache;
use crate::document::ParsedDoc;
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
    /// / `NewAdd`, `old_rope` for `OldDelete`).  For
    /// [`DiffLineSource::ContextRendered`] it is instead an index into
    /// `DiffState::parsed_new`'s rendered `lines` — the row is already a
    /// laid-out `ratatui::Line`, not source text.
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
    /// Unchanged line shown as *rendered* Markdown, taken from
    /// `DiffState::parsed_new`.  `DiffVisualLine::rope_line` is an index
    /// into `parsed_new.lines`, **not** into a rope (see its doc).
    ///
    /// Deliberately fieldless, like every other variant: `DiffLineSource`
    /// is `Copy`, compared with `==` in the renderer and the row cache,
    /// and passed by value to [`line_marker`] — a payload variant would
    /// churn all of that to carry an index the struct already has a slot
    /// for.
    ContextRendered,
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
    /// Memoised [`rendered_row_index`] over `lines` — width-independent,
    /// so it sits beside `lines` rather than inside a row cache, and is
    /// dropped with them.
    ///
    /// Built on first request, not alongside `lines`: its only consumers
    /// are the two image paths, so a review of a document with no images
    /// never pays for it.  Memoising matters because the diff-side
    /// decode dispatch runs from `App::prepare_viewport` — once per
    /// event-loop iteration for the whole length of the review — and a
    /// fresh scan there is O(rendered rows) plus a `HashMap` allocation
    /// at the frame cadence, against an editor-mode counterpart that is
    /// O(images).
    rendered_index: Option<HashMap<usize, usize>>,
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

    /// Build the flat visual-line sequence for a *rendered* review:
    /// unchanged blocks emit their pre-rendered rows, changed regions
    /// keep exactly the raw stacked presentation [`Self::build_visual_lines`]
    /// produces.
    ///
    /// The document is partitioned by block ([`block_spans`]); a maximal
    /// run of consecutive *touched* blocks is a raw region, and the hunk
    /// list itself is never reshaped.  Snapping hunk ranges out to block
    /// boundaries instead would collapse the per-row table hunks
    /// `engine::split_table_hunk` produces back into one whole-table
    /// hunk; a display-only partition leaves `hunks`, `decisions`,
    /// `HunkId` stability, `reconcile_with_disk` and `resolved_rope`
    /// untouched.
    fn build_visual_lines_rendered(&self, parsed: &ParsedDoc) -> Vec<DiffVisualLine> {
        let total_lines = self.new_buffer.rope().len_lines();
        let (spans, owners) = block_spans(self, parsed, total_lines);
        // A new side with *no blocks at all* — reachable, and only, when
        // the file was truncated to empty on disk (`> notes.md`, a failed
        // save, a partial sync), which `App::enter_diff_mode` hands
        // straight through from the watcher.  There is nothing to render,
        // and no span for the whole-document delete's boundary line 0 to
        // be emitted against: the loop below never runs, the trailing
        // `emit_boundary_hunks(total_lines)` looks at line 1, and the
        // hunk is dropped.  In release that is a *blank* review of a real
        // change, with `all_resolved()` false so `Esc` refuses to finish.
        // Fall back to the raw walk, which has no such gap.
        if spans.is_empty() {
            return self.build_visual_lines();
        }
        let mut out: Vec<DiffVisualLine> = Vec::new();

        let mut i = 0usize;
        while i < spans.len() {
            if spans[i].touched {
                // Maximal run of touched blocks = one raw region.
                let start = i;
                let mut j = i;
                while j < spans.len() && spans[j].touched {
                    j += 1;
                }
                let region = spans[start].lines.start..spans[j - 1].lines.end;
                self.emit_raw_region(&region, &owners, &mut out);
                i = j;
            } else {
                // A delete-only hunk whose insertion point is exactly
                // this block's first line touches no block at all: its
                // rows are emitted *between* two rendered runs, at the
                // boundary it names.
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

        // Exactly once, not merely at least once: a hunk emitted twice
        // paints two decision dividers for one decision, and the second
        // would silently disagree with the first the moment the user
        // presses `y`.  The divider is the countable marker because
        // `emit_hunk` always emits exactly one per call.
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

    /// Emit one raw region: today's stacked walk, restricted to the line
    /// range `region` and to the hunks the partition assigned to it.
    ///
    /// **The ordering assumption is stated rather than assumed.**  The
    /// whole-document walk is a monotone `new_cursor` loop that would
    /// silently drop or double-emit a hunk under the overlapping or
    /// unsorted hunk lists the table split can produce (a straddling
    /// hunk and a contained one can share table lines).  Here context is
    /// emitted only when the hunk actually starts ahead of the cursor,
    /// the hunk's own rows are emitted unconditionally, and the cursor
    /// only ever moves forward.  A hunk may end up with no context ahead
    /// of it; it is never dropped.
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
                // A boundary delete landing *inside* a raw region is part
                // of it; one landing on its far edge belongs to the clean
                // block that starts there.
                HunkOwner::Boundary { line } => line >= region.start && line < region.end,
            };
            if !in_region {
                continue;
            }
            let h = &self.hunks[hi];
            // Context runs up to the hunk's *own* new-side start — the
            // owner's `start_line` names the block that put it in this
            // region, which is at or before it.
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

    /// The stacked old-above-new rows for one hunk: deletes, the
    /// decision divider, then adds — byte-identical to what
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
        self.ensure_layout(&mut cache, width);
        let lines = cache.lines.as_ref().expect("lines built above");
        let rc = cache.row_caches.first().expect("row cache built above");
        f(lines, rc)
    }

    /// As [`Self::with_layout`], plus the memoised map from a rendered
    /// line index to its position in the flat line list (see
    /// [`DiffLayoutCache::rendered_index`]).
    ///
    /// The single door to that map, so the diff-side decode dispatch and
    /// the diff image-snapshot builder place images against exactly the
    /// rows the painter walks — and neither rebuilds it per frame.
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

    /// Populate `cache.lines` and promote-or-build the row cache for
    /// `width`.  Shared by [`Self::with_layout`] and
    /// [`Self::with_layout_index`] so the two can never disagree about
    /// what a layout version contains.
    fn ensure_layout(&self, cache: &mut DiffLayoutCache, width: usize) {
        if cache.lines.is_none() {
            cache.lines = Some(match self.parsed_new.as_ref() {
                Some(parsed) => self.build_visual_lines_rendered(parsed),
                // No parse installed — the first frame of a review, whose
                // deferred build `prepare_viewport` has not resolved yet
                // → the whole review stays raw, exactly as before.
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
                    } else if lines[i].source == DiffLineSource::ContextRendered {
                        // A rendered row is measured as the `Line` the
                        // painter will hand to `render_line_from_visual`
                        // — the same call `PreviewView` makes — so its
                        // wrap and the diff's scroll math agree by
                        // construction.  Placed ahead of the `line_text`
                        // call below, which has nothing to say about it.
                        self.parsed_new
                            .as_ref()
                            .and_then(|p| p.lines.get(lines[i].rope_line))
                            .map_or(1, |l| visual_rows_for_line(l, width))
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
        cache.rendered_index = None;
        cache.row_caches.clear();
        self.bump_layout_version();
    }
}

/// Map each rendered-line index shown as `ContextRendered` to its
/// position in the diff's flat line list.
///
/// One scan of the layout's own `lines` — the same slice the painter
/// walks — so the rows the decode dispatch reasons about are exactly the
/// rows the snapshot builder places.
///
/// Private: the memo in [`DiffLayoutCache::rendered_index`] is the only
/// caller, and [`DiffState::with_layout_index`] the only door, so the
/// map cannot be rebuilt per frame by a new consumer or outlive the
/// `lines` it was scanned from.
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
    /// True when a hunk's change lands in this block, so its lines are
    /// shown as raw source instead of rendered rows.
    touched: bool,
}

/// Where one hunk's rows get emitted.
#[derive(Debug, Clone, Copy)]
enum HunkOwner {
    /// The hunk touches at least one block; it is emitted inside the raw
    /// region containing the block whose span starts at `start_line`.
    Region { start_line: usize },
    /// A delete-only hunk whose insertion point falls exactly on a block
    /// boundary, so it touches no block: its rows are emitted at `line`,
    /// between two rendered runs.
    Boundary { line: usize },
}

/// Partition the new side by block, and decide where each hunk's rows go.
///
/// **Every source-map block gets a span, zero-row blocks included.**  A
/// block that renders nothing is still real and still carries source
/// lines — a standalone HTML comment, a `<!-- tui-columns: [...] -->`
/// hint, a blank run collapsed by `preserve_blank_lines = false`.  If
/// such a block had no span, a hunk confined to it would mark nothing
/// touched, would fall inside no raw region, and its delete / decision /
/// add rows would never be emitted: the user would review a diff that
/// does not contain the change, while `all_resolved()` still let them
/// resolve it.  So zero rendered rows is a property of a clean block's
/// *emission*, never of its *existence*.
///
/// The walk shape is [`crate::editor::state_source_lines`]'s, which
/// solves the same problem for the line-number gutter: source-map block
/// space (not `ParsedDoc::blocks`, which misses the blank-line virtual
/// blocks and the phantom trailing one), byte ranges read out of
/// `ParsedDoc::source` rather than the live buffer, and a *running*
/// newline count — `byte_to_line` is O(byte) and would make this
/// quadratic.
///
/// Each span runs from its own first line to the *next* block's first
/// line, rather than being derived from its byte range: pulldown-cmark
/// ranges absorb trailing blank lines that already have virtual blocks
/// of their own, so range-derived spans would overlap.  The last span
/// runs to `len_lines()`, which includes ropey's phantom line past the
/// trailing newline — that is what keeps the spans a total partition,
/// and it is the same line the raw walk emits as a trailing empty
/// context row.
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
        // Advance the running count before anything else, so it stays
        // honest even for a block we then record as row-less.
        if start > scanned {
            block_line += contents.as_bytes()[scanned..start]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
            scanned = start;
        }
        // A row-less block's `rendered_lines_for_block` is its
        // *neighbour's* range (the map's documented fallback), so the
        // `own` count — not the range — is what decides whether it has
        // rows at all.  Keep the entry either way.
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
        // A document whose first block starts past byte 0 would otherwise
        // leave its opening lines outside the partition.
        first.lines.start = 0;
    }
    debug_assert!(
        spans.first().is_none_or(|s| s.lines.start == 0)
            && spans.windows(2).all(|w| w[0].lines.end == w[1].lines.start)
            && spans.last().is_none_or(|s| s.lines.end == total_lines),
        "block spans must partition every source line of the new side"
    );

    // Mark touched blocks.  Hunks are *not* assumed disjoint or sorted:
    // after the table split a straddling hunk and a contained one can
    // share table lines, so this accumulates into a flag per block and
    // overlap is harmless.
    let mut owners: Vec<HunkOwner> = Vec::with_capacity(diff.hunks.len());
    for h in &diff.hunks {
        let mut first_touched: Option<usize> = None;
        if h.new_lines.is_empty() {
            // Delete-only: the block that *strictly* contains the
            // insertion point.  Landing exactly on a boundary touches
            // nothing, which is the good case — the deleted lines show
            // between two rendered blocks.
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
                // Should be unreachable: a non-empty new-side range
                // always intersects some span, because the spans cover
                // every line.  Losing a hunk is worse than showing one
                // extra block raw, so widen the nearest block rather
                // than dropping it.
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
    // Second pass: flag the blocks each owning hunk covers.  Done after
    // the owner decision so an overlapping pair can't disturb it.
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
    // A rendered row has no source text — its content is a finished
    // `Line` in `parsed_new`.  A real arm rather than an `unreachable!`:
    // this is a `pub fn` reached from `with_layout`'s cache-fill closure,
    // whose own `ContextRendered` arm means the call never actually
    // happens, and a panic there is not worth the assertion.
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
        // A rendered row is the document, painted at column 0: a marker
        // would overflow table grids and code-block padding, both of
        // which the renderer laid out at the full viewport width.  The
        // markers' alignment reference is the raw context *inside* a
        // changed region, which is unaffected.
        DiffLineSource::ContextRendered => "",
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

    use crate::config::Theme;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// A review of `old` → `new` with the rendered new-side parse
    /// installed, as `EditorState::refresh_diff_parse` installs it.
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
        // The untouched heading and the two untouched paragraphs render.
        assert!(rows.iter().any(|r| r.contains("Title")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("Alpha.")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("Charlie.")), "{rows:?}");
        // The heading renders *styled* — its `#` marker is gone.
        assert!(!rows.iter().any(|r| r.contains('#')), "{rows:?}");
        // The changed paragraph keeps both raw sides.
        assert_eq!(raw_rows(&st, DiffLineSource::OldDelete), vec!["Bravo."]);
        assert_eq!(raw_rows(&st, DiffLineSource::NewAdd), vec!["BRAVO!"]);
    }

    #[test]
    fn a_delete_on_a_block_boundary_lands_between_two_rendered_runs() {
        // Removing a whole paragraph (and its trailing blank) deletes
        // lines that start exactly on a block boundary, so no block is
        // touched and every surviving block still renders.
        let old = "Alpha.\n\nBravo.\n\nCharlie.\n";
        let new = "Alpha.\n\nCharlie.\n";
        let st = rendered(old, new);
        let lines = lines_of(&st);
        assert!(!raw_rows(&st, DiffLineSource::OldDelete).is_empty());
        assert!(lines.iter().any(|l| l.source == DiffLineSource::Decision));
        let rows = rendered_rows(&st);
        assert!(rows.iter().any(|r| r.contains("Alpha.")), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains("Charlie.")), "{rows:?}");
        // Nothing survives as a *raw* context line: the deleted lines are
        // emitted between two fully-rendered runs.
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
        // The row split is untouched by the display partition.
        assert_eq!(st.hunks.len(), 1);
        // The table's block is touched, so its untouched rows stay raw
        // context rather than being painted as a grid.
        let ctx = raw_rows(&st, DiffLineSource::Context);
        assert!(ctx.iter().any(|r| r.contains("| 3 | 4 |")), "{ctx:?}");
        assert_eq!(raw_rows(&st, DiffLineSource::NewAdd), vec!["| 1 | X |"]);
    }

    #[test]
    fn a_change_confined_to_an_html_comment_is_still_reviewable() {
        // A standalone HTML comment renders no rows at all.  Its lines
        // must still belong to a block span, or the hunk inside it would
        // touch nothing, fall in no raw region, and vanish from the
        // review while `all_resolved()` still let the user resolve it.
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
        // The other zero-row case: with `preserve_blank_lines` off, the
        // extra blanks in a run render nothing.
        let old = "Alpha.\n\n\n\nBravo.\n";
        let new = "Alpha.\n\n\n\nBRAVO!\n";
        let mut st = DiffState::new(old, new).expect("non-empty diff");
        st.set_rendered_parse(Some(ParsedDoc::build(new, theme(), false, 20)));
        assert_eq!(raw_rows(&st, DiffLineSource::OldDelete), vec!["Bravo."]);
        assert_eq!(raw_rows(&st, DiffLineSource::NewAdd), vec!["BRAVO!"]);
    }

    /// Fixture with a heading, a table, a hidden comment and trailing
    /// blanks — every block flavour the partition has to cover.
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

    /// A file truncated to empty on disk parses to *zero* blocks, so the
    /// block partition is empty and the whole-document delete has no span
    /// to be emitted against.  Left unhandled the review is blank: no
    /// lines, no rows, and an unresolvable hunk that `Esc` refuses to
    /// finish on.  The raw walk has no such gap, so that is the fallback.
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

    /// The rendered-row memo is dropped with the lines it was scanned
    /// from.  Kept stale it would name flat-line positions from the
    /// previous layout, and the image snapshots built off it would place
    /// pictures on rows belonging to other blocks.
    #[test]
    fn the_rendered_row_memo_follows_the_layout_it_was_built_from() {
        let old = "# Title\n\nAlpha.\n\nBravo.\n";
        let new = "# Title\n\nAlpha.\n\nBRAVO!\n";
        let mut st = rendered(old, new);

        let before = st.with_layout_index(80, |lines, _, index| {
            // Every `ContextRendered` row is in the map, and each entry
            // points at a line that really is one.
            for (&row, &pos) in index {
                assert_eq!(lines[pos].source, DiffLineSource::ContextRendered);
                assert_eq!(lines[pos].rope_line, row);
            }
            index.len()
        });
        assert!(before > 0, "the clean blocks must contribute rows");
        // Same answer on a second query — the memo is a cache, not a
        // one-shot.
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
