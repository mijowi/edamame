//! Per-frame visual-row view with the raw-reveal patch applied.
//!
//! Part of paragraph reflow (`docs/dev/plans/paragraph-reflow.md`).  Once a paragraph
//! reflows, its rendered form can be *shorter* than its raw source (six wrapped-to-two rows),
//! so revealing the cursor's block as raw makes the document taller exactly while the cursor
//! rests inside it.  The old reveal is height-neutral and cannot express that.
//!
//! `EffectiveRows` presents the document's visual rows as if the revealed paragraph's one flow
//! row were replaced by its `M` raw source lines, each wrapped at the current width as the
//! painter wraps it — a cheap delta over the base
//! [`VisualRowCache`](crate::document::visual_cache::VisualRowCache), never a rebuild.  The
//! paragraph can sit at any depth, so the replaced row may be one of many in its block (an item's
//! paragraph in a list); [`row_map::stacked_lines`](crate::document::row_map::stacked_lines)
//! picks it.  When no paragraph is revealed (Preview, the pre-reveal-delay window, or any other
//! row, whose reveal stays height-neutral), it is the identity over the base cache.
//!
//! The base cache clamps every wrapped row count to `>= 1`; the raw wrap counts here must too, or
//! a blank raw line reports zero rows and every prefix sum past it drifts.

use std::ops::Range;
use std::rc::Rc;

use crate::document::wrap::revealed_row_count;
use crate::document::ParsedDoc;

/// What a visual row resolves to under the reveal patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowHit {
    /// A rendered line (index into `parsed.lines`) and its wrap sub-row.
    Rendered { line: usize, sub: usize },
    /// A raw source line of the revealed paragraph — `raw_line` is the block-relative source line
    /// (the line's offset from its block's first line) — and its wrap sub-row.
    Raw { raw_line: usize, sub: usize },
}

/// The reveal patch: the revealed row's rendered span and its raw lines' wrap geometry.
/// Opaque to callers (built by [`EffectiveRows::with_reveal`], shared via [`EffectiveRowsCache`]).
#[derive(Debug, Clone)]
pub struct Patch {
    /// Rendered-line range `[start, end)` of the revealed row.
    rendered: Range<usize>,
    /// Block-relative source line of the first raw line.
    first_line: usize,
    /// Base visual rows in lines `[0, start)`.
    base_before: usize,
    /// Base visual rows in `[start, end)` — the rows the patch replaces.
    base_block_rows: usize,
    /// Wrap count (>= 1) of each raw line.
    raw_wrap: Vec<usize>,
    /// Prefix sums of `raw_wrap`; `len == raw_wrap.len() + 1`, last entry is the total.
    raw_prefix: Vec<usize>,
}

/// A per-frame view over one `ParsedDoc`'s visual rows.  Cheap to construct; holds only a
/// borrow plus a shared handle on the small patch.
///
/// The patch is `Rc`-shared so [`EditorState::effective_rows`](crate::editor::EditorState) can
/// cache it (see [`EffectiveRowsCache`]) and hand out many views per frame without re-allocating
/// the revealed block's source or recomputing its raw-line wrap counts — the queries here run
/// several times a frame (scroll, scrollbar, cursor row, gutter).
pub struct EffectiveRows<'a> {
    parsed: &'a ParsedDoc,
    width: usize,
    base_total: usize,
    patch: Option<Rc<Patch>>,
}

/// [`EffectiveRowsCache`]'s key: `(parsed_version, width, reveal)`, the reveal being the
/// revealed row's rendered start and the block-relative source lines it stacks (`start, end`).
pub type EffectiveRowsKey = (u64, usize, Option<(usize, u32, u32)>);

/// Per-`EditorState` memo for the reveal patch, so a frame's repeated `effective_rows` calls build
/// it once.  Keyed by [`EffectiveRowsKey`] — the only inputs that change the patch.  The lines
/// are part of it: a reflowed paragraph stacks the same lines wherever the cursor is in it, so
/// moves inside one stay a cache hit, but a row a line with no row of its own shares stacks that
/// line only while the cursor is on it.  `None` reveal = identity.
#[derive(Debug, Clone, Default)]
pub struct EffectiveRowsCache {
    key: Option<EffectiveRowsKey>,
    base_total: usize,
    patch: Option<Rc<Patch>>,
}

impl EffectiveRowsCache {
    /// The cached patch when the key matches, else `None` (caller rebuilds).
    pub fn get(&self, key: EffectiveRowsKey) -> Option<(usize, Option<Rc<Patch>>)> {
        (self.key == Some(key)).then(|| (self.base_total, self.patch.clone()))
    }

    /// Store a freshly built patch under `key`.
    pub fn store(&mut self, key: EffectiveRowsKey, base_total: usize, patch: Option<Rc<Patch>>) {
        self.key = Some(key);
        self.base_total = base_total;
        self.patch = patch;
    }
}

impl<'a> EffectiveRows<'a> {
    /// Identity view: every query delegates straight to the base cache.
    pub fn identity(parsed: &'a ParsedDoc, width: usize) -> Self {
        let width = width.max(1);
        Self {
            parsed,
            width,
            base_total: parsed.total_visual_rows(width),
            patch: None,
        }
    }

    /// View with the row at rendered range `rendered` revealed to `raw_lines`: the paragraph's raw
    /// source lines (soft breaks split back out), the first of them block-relative line
    /// `first_line`.  Each is wrapped at `width`.
    pub fn with_reveal(
        parsed: &'a ParsedDoc,
        width: usize,
        rendered: Range<usize>,
        first_line: usize,
        raw_lines: &[&str],
    ) -> Self {
        let width = width.max(1);
        Self {
            parsed,
            width,
            base_total: parsed.total_visual_rows(width),
            patch: Some(Rc::new(Patch::build(
                parsed, width, rendered, first_line, raw_lines,
            ))),
        }
    }

    /// Reconstruct a view from cached parts (see [`EffectiveRowsCache`]) — no allocation beyond an
    /// `Rc` clone.  `base_total` and `patch` must have been built at this same `width`.
    pub fn from_cached(
        parsed: &'a ParsedDoc,
        width: usize,
        base_total: usize,
        patch: Option<Rc<Patch>>,
    ) -> Self {
        Self {
            parsed,
            width: width.max(1),
            base_total,
            patch,
        }
    }

    /// Whether a reveal patch is in effect (i.e. this is not the identity view).
    pub fn has_reveal(&self) -> bool {
        self.patch.is_some()
    }

    /// The parts [`EffectiveRowsCache`] memoizes: the base total and a shared handle on the patch.
    pub fn cache_parts(&self) -> (usize, Option<Rc<Patch>>) {
        (self.base_total, self.patch.clone())
    }

    /// Rendered-line range of the revealed row, or `None` on the identity view.  The reveal
    /// loop uses `.start` to know where the raw-line unit is spliced in.
    pub fn block_rendered(&self) -> Option<Range<usize>> {
        self.patch.as_ref().map(|p| p.rendered.clone())
    }

    /// Wrap count (>= 1) of block-relative raw line `raw_line`, or 1 out of range / on the
    /// identity view.
    pub fn raw_wrap_at(&self, raw_line: usize) -> usize {
        self.patch
            .as_ref()
            .and_then(|p| p.raw_wrap.get(raw_line.checked_sub(p.first_line)?).copied())
            .unwrap_or(1)
    }

    /// The block-relative source lines the revealed row expands to (empty on the identity view).
    pub fn raw_lines(&self) -> Range<usize> {
        self.patch
            .as_ref()
            .map_or(0..0, |p| p.first_line..p.first_line + p.raw_wrap.len())
    }

    /// Total visual rows with the patch applied.
    pub fn total_visual_rows(&self) -> usize {
        match &self.patch {
            None => self.base_total,
            Some(p) => self.base_total - p.base_block_rows + p.raw_rows_total(),
        }
    }

    /// Visual row block-relative raw line `raw_line` of the revealed row starts on (absolute,
    /// document coordinates), clamped into the stack.  Panics only if called on the identity
    /// view — callers gate on [`Self::has_reveal`].
    pub fn raw_line_visual_row(&self, raw_line: usize) -> usize {
        let p = self
            .patch
            .as_ref()
            .expect("raw_line_visual_row on identity view");
        let idx = raw_line.saturating_sub(p.first_line).min(p.raw_wrap.len());
        p.base_before + p.raw_prefix[idx]
    }

    /// What visual row `v` resolves to under the patch.
    pub fn line_at_visual_row(&self, v: usize) -> RowHit {
        let Some(p) = &self.patch else {
            let (line, sub) = self.parsed.line_at_visual_row(v, self.width);
            return RowHit::Rendered { line, sub };
        };

        if v < p.base_before {
            // Before the block: base coordinates coincide.
            let (line, sub) = self.parsed.line_at_visual_row(v, self.width);
            return RowHit::Rendered { line, sub };
        }
        let raw_total = p.raw_rows_total();
        if v < p.base_before + raw_total {
            // Inside the revealed block: locate within the raw wrap prefix sums.
            let local = v - p.base_before;
            let raw_line = p
                .raw_prefix
                .partition_point(|&s| s <= local)
                .saturating_sub(1);
            let sub = local - p.raw_prefix[raw_line];
            return RowHit::Raw {
                raw_line: p.first_line + raw_line,
                sub,
            };
        }
        // After the block: shift back into base coordinates by the height delta.
        let base_coord = v + p.base_block_rows - raw_total;
        let (line, sub) = self.parsed.line_at_visual_row(base_coord, self.width);
        RowHit::Rendered { line, sub }
    }
}

impl Patch {
    /// Build the patch: measure each raw line's wrap at `width` and its prefix sums, plus the base
    /// rows the row spans (which the raw expansion replaces).
    fn build(
        parsed: &ParsedDoc,
        width: usize,
        rendered: Range<usize>,
        first_line: usize,
        raw_lines: &[&str],
    ) -> Self {
        let base_before = parsed.visual_rows_before(rendered.start, width);
        let base_end = parsed.visual_rows_before(rendered.end, width);
        let base_block_rows = base_end.saturating_sub(base_before);

        let mut raw_wrap = Vec::with_capacity(raw_lines.len());
        let mut raw_prefix = Vec::with_capacity(raw_lines.len() + 1);
        raw_prefix.push(0usize);
        let mut acc = 0usize;
        for line in raw_lines {
            let rows = revealed_row_count(line, width);
            raw_wrap.push(rows);
            acc += rows;
            raw_prefix.push(acc);
        }

        Self {
            rendered,
            first_line,
            base_before,
            base_block_rows,
            raw_wrap,
            raw_prefix,
        }
    }

    fn raw_rows_total(&self) -> usize {
        *self.raw_prefix.last().unwrap_or(&0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::Theme;
    use crate::document::wrap::visual_rows_for_line;
    use crate::document::ParsedDoc;
    use ratatui::text::Line;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    /// Build a reflowed parse (soft breaks joined) for a source string.
    fn reflowed(src: &str) -> ParsedDoc {
        ParsedDoc::build_with_overrides(
            src,
            theme(),
            false,
            24,
            None,
            None,
            false,
            80,
            false,
            false,
            true,
            true, // reflow on
            None,
        )
    }

    /// Brute-force expansion of the effective visual-row sequence: one `RowHit` per visual row.
    /// Raw lines count as the painter wraps them (a `Line` of the raw text, its hanging indent
    /// detected from its marker), independently of the patch's own measure.
    fn expand(
        parsed: &ParsedDoc,
        width: usize,
        reveal: Option<(Range<usize>, &[&str])>,
    ) -> Vec<RowHit> {
        expand_from(parsed, width, reveal, 0)
    }

    fn expand_from(
        parsed: &ParsedDoc,
        width: usize,
        reveal: Option<(Range<usize>, &[&str])>,
        first_line: usize,
    ) -> Vec<RowHit> {
        let mut out = Vec::new();
        let n = parsed.lines.len();
        let (start, end, raw): (usize, usize, &[&str]) = match &reveal {
            Some((r, raw)) => (r.start, r.end, raw),
            None => (n, n, &[]),
        };
        for line in 0..n {
            if line == start && !raw.is_empty() {
                for (i, text) in raw.iter().enumerate() {
                    let rows = visual_rows_for_line(&Line::raw(*text), width).max(1);
                    for sub in 0..rows {
                        out.push(RowHit::Raw {
                            raw_line: first_line + i,
                            sub,
                        });
                    }
                }
            }
            if line >= start && line < end {
                continue; // replaced by the raw lines above
            }
            let rows = visual_rows_for_line(&parsed.lines[line], width).max(1);
            for sub in 0..rows {
                out.push(RowHit::Rendered { line, sub });
            }
        }
        out
    }

    fn check_against_brute_force(
        parsed: &ParsedDoc,
        width: usize,
        reveal: Option<(Range<usize>, Vec<&str>)>,
    ) {
        let er = match &reveal {
            None => EffectiveRows::identity(parsed, width),
            Some((r, raw)) => EffectiveRows::with_reveal(parsed, width, r.clone(), 0, raw),
        };
        let expected = expand(
            parsed,
            width,
            reveal.as_ref().map(|(r, raw)| (r.clone(), raw.as_slice())),
        );
        assert_eq!(
            er.total_visual_rows(),
            expected.len(),
            "total_visual_rows disagrees with brute force at width {width}"
        );
        for (v, want) in expected.iter().enumerate() {
            assert_eq!(
                er.line_at_visual_row(v),
                *want,
                "line_at_visual_row({v}) disagrees at width {width}"
            );
        }
    }

    #[test]
    fn identity_matches_base_at_several_widths() {
        let parsed = reflowed("# Title\n\nalpha beta gamma delta epsilon\n\nlast\n");
        for width in [80, 20, 12, 8] {
            check_against_brute_force(&parsed, width, None);
        }
    }

    #[test]
    fn reveal_taller_than_rendered_expands() {
        // A soft-broken paragraph: rendered as one (wrapping) line, revealed as 3 raw lines.
        let parsed = reflowed("intro\n\none\ntwo\nthree\n\nafter\n");
        // Locate the paragraph's rendered range: it renders "one two three" as a single line.
        let block = parsed
            .lines
            .iter()
            .position(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("one two three")
            })
            .expect("reflowed paragraph must render as one line");
        let raw = vec!["one", "two", "three"];
        for width in [80, 20, 6] {
            check_against_brute_force(&parsed, width, Some((block..block + 1, raw.clone())));
        }
    }

    #[test]
    fn reveal_with_wrapping_raw_lines() {
        // Raw lines that themselves wrap at a narrow width.
        let parsed = reflowed("head\n\nalpha bravo\ncharlie delta echo\n\ntail\n");
        let block = parsed
            .lines
            .iter()
            .position(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("alpha bravo charlie")
            })
            .expect("reflowed paragraph must render as one line");
        let raw = vec!["alpha bravo", "charlie delta echo"];
        for width in [80, 10, 6] {
            check_against_brute_force(&parsed, width, Some((block..block + 1, raw.clone())));
        }
    }

    /// A nested paragraph's patch replaces its one flow row inside a multi-row block (a list),
    /// names its lines block-relative, and wraps them as the painter does: a raw `- ` marker and a
    /// continuation's indent hang their wrapped rows.
    #[test]
    fn a_nested_reveal_splices_one_row_inside_its_block() {
        let parsed = reflowed("- one\n- alpha bravo charlie\n  delta echo foxtrot\n- four\n");
        let row = parsed
            .lines
            .iter()
            .position(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("alpha bravo charlie delta")
            })
            .expect("the item's paragraph must render as one flow row");
        let raw = ["- alpha bravo charlie", "  delta echo foxtrot"];
        for width in [80, 12, 9] {
            let er = EffectiveRows::with_reveal(&parsed, width, row..row + 1, 1, &raw);
            let expected = expand_from(&parsed, width, Some((row..row + 1, &raw)), 1);
            assert_eq!(er.total_visual_rows(), expected.len(), "width {width}");
            for (v, want) in expected.iter().enumerate() {
                assert_eq!(er.line_at_visual_row(v), *want, "row {v} at width {width}");
            }
            assert_eq!(er.raw_lines(), 1..3);
            for line in er.raw_lines() {
                let vr = er.raw_line_visual_row(line);
                assert_eq!(
                    expected[vr],
                    RowHit::Raw {
                        raw_line: line,
                        sub: 0
                    }
                );
            }
        }
    }

    #[test]
    fn raw_line_visual_row_matches_expansion() {
        let parsed = reflowed("intro\n\none\ntwo\nthree\n\nafter\n");
        let block = parsed
            .lines
            .iter()
            .position(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains("one two three")
            })
            .unwrap();
        let raw = vec!["one", "two", "three"];
        let width = 8;
        let er = EffectiveRows::with_reveal(&parsed, width, block..block + 1, 0, &raw);
        let expected = expand(&parsed, width, Some((block..block + 1, &raw)));
        // The first visual row of each raw line must be a `Raw { sub: 0 }` at the reported row.
        for raw_line in 0..raw.len() {
            let vr = er.raw_line_visual_row(raw_line);
            assert_eq!(expected[vr], RowHit::Raw { raw_line, sub: 0 });
        }
    }
}
