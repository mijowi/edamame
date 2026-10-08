//! Rendered row ↔ source line, and a row's rendered chars ↔ source columns, answered from the
//! [`RowOrigin`]s the renderer recorded.
//!
//! Works in **logical rows** — entries of `ParsedDoc::lines`, before wrap — in rendered chars
//! within such a row, and in source lines relative to a block's first line (the line holding its
//! original range's start).  Columns are read against the parse's own text, so they agree with
//! the origins they were recorded beside.  Wrapping and the reflow reveal stay with
//! `line_render` and `EffectiveRows`.  `block` is always an index in the source map's space
//! (blank-line virtual blocks included), and a row is an offset from the block's first rendered
//! row.  The rules are in `docs/dev/editing-model.md`.

use std::cell::OnceCell;
use std::ops::Range;

use crate::document::ParsedDoc;
use crate::markdown::ast::to_u32;
use crate::markdown::table_layout::char_cells;
use crate::markdown::{strip_atx_closing, Block, ColOrigin, ContentKind, InlineColMap, RowOrigin};

/// `block`'s own rows' origins, and how many rows a `$$…$$` reveal's math-preview band holds
/// above them.  The band is editor state (the cursor's reveal), not the renderer's, so the
/// origins don't know it: a band row shows no source line, and the rows below it are the
/// origins' rows shifted down.
fn own_origins(parsed: &ParsedDoc, block: usize) -> (&[RowOrigin], usize) {
    let own = parsed.block_own_line_count(block);
    let first = parsed.source_map.rendered_lines_for_block(block).start;
    let origins = parsed
        .row_origins()
        .get(first..first.saturating_add(own))
        .unwrap_or(&[]);
    (origins, parsed.latex_source_offset(block))
}

/// The row of `block` that shows block-relative source `line`: the first row whose lines reach
/// it.  A line that renders no row of its own (a link reference definition inside a container,
/// a setext underline below a one-row heading, a bare `-`) shares the next line's row; a line
/// past every row clamps to the last row that shows a line, so never onto trailing chrome no
/// line owns (a table's bottom
/// border, an unclosed fence's placeholder).  A block with no rows answers 0.
pub fn row_for_line(parsed: &ParsedDoc, block: usize, line: usize) -> usize {
    let (origins, band) = own_origins(parsed, block);
    if origins.is_empty() {
        return 0;
    }
    let line = to_u32(line);
    let last = origins.len() - 1;
    let row = origins
        .iter()
        .position(|o| o.lines.as_ref().is_some_and(|l| l.end > line))
        .or_else(|| origins.iter().rposition(|o| o.lines.is_some()))
        .unwrap_or(last);
    (row + band).min(last)
}

/// The row of `block` showing source position `pos` (columns as [`RawPos`] counts them): the
/// cursor's row.  Usually [`row_for_line`]'s.  But a line can render on several rows, the first of
/// them chrome: a marker on a row of its own above the block its item opens (`- - a`, `- > q`,
/// `- # H`).  The line's chars show on a later row, so the cursor goes on the first one that places
/// its column exactly, which is also where a click on that char puts it back.
pub fn row_for_pos(parsed: &ParsedDoc, block: usize, pos: RawPos) -> usize {
    let row = row_for_line(parsed, block, pos.line);
    let (origins, band) = own_origins(parsed, block);
    let chrome = row
        .checked_sub(band)
        .and_then(|r| origins.get(r))
        .is_some_and(|o| o.cols == ColOrigin::Chrome);
    if !chrome {
        return row;
    }
    let line = to_u32(pos.line);
    (row + 1..)
        .take_while(|&r| lines_of_row(parsed, block, r).is_some_and(|l| l.contains(&line)))
        .find(|&r| raw_to_rendered_col(parsed, block, r, pos).is_some())
        .unwrap_or(row)
}

/// The source lines row `row` of `block` shows; `None` for a row no line owns (a table's top
/// border, an unclosed fence's placeholder) and for a math-preview band row.
pub fn lines_of_row(parsed: &ParsedDoc, block: usize, row: usize) -> Option<Range<u32>> {
    let (origins, band) = own_origins(parsed, block);
    origins.get(row.checked_sub(band)?)?.lines.clone()
}

/// The block-relative source line row `row` of `block` belongs to: the first line it shows, or,
/// for a row no line owns, the nearest owned line above it (0 when there is none).
pub fn line_for_row(parsed: &ParsedDoc, block: usize, row: usize) -> usize {
    let (origins, band) = own_origins(parsed, block);
    let Some(row) = row.checked_sub(band) else {
        return 0;
    };
    let row = row.min(origins.len().saturating_sub(1));
    origins
        .get(..=row)
        .unwrap_or(&[])
        .iter()
        .rev()
        .find_map(RowOrigin::first_line)
        .map_or(0, |l| l as usize)
}

/// The block-relative source line row `row` of diagram `block` (a mermaid fence, `$$…$$` math)
/// paints while revealed: its rows show the source lines 1:1, shifted down past any math-preview
/// band.  `None` for a band row, which paints empty behind the formula, for a reserved row past
/// the source (the origins clamp it to the last line, but it paints as padding until the
/// reservation shrinks to the source), and past the block's rows.  The one rule the click, the
/// raw row count and the view share for a diagram's rows.
pub fn revealed_diagram_line(parsed: &ParsedDoc, block: usize, row: usize) -> Option<usize> {
    let (origins, band) = own_origins(parsed, block);
    let k = row.checked_sub(band)?;
    let line = origins.get(k)?.lines.as_ref()?.start as usize;
    (line == k).then_some(line)
}

/// The source lines row `row` of `block` expands to when it is the revealed cursor row: `Some`
/// for an inline row over several lines, whose raw form is those lines stacked, `None` for
/// every other row, which reveals in place.  Such a row is a reflowed paragraph's flow (taller
/// raw than the one wrapped row it renders as), a multi-line setext heading's text, or a row
/// holding a multi-line code span, reflow on or off.  A reflowed paragraph's flow stacks even
/// over one line (each item of a list of one-liners): it reflows when reflow is on and it has
/// no hard break ([`paragraph_reflows`](crate::markdown::renderer::paragraph_reflows), the
/// renderer's rule), and is then its only row, a `Flow` origin over the paragraph's lines.
/// [`cursor_stack`] builds the cursor's stack from it.
pub fn stacked_lines(parsed: &ParsedDoc, block: usize, row: usize) -> Option<Range<u32>> {
    let (origins, band) = own_origins(parsed, block);
    let origin = origins.get(row.checked_sub(band)?)?;
    let ColOrigin::Content {
        kind: kind @ (ContentKind::Inline | ContentKind::Flow),
        ..
    } = origin.cols
    else {
        return None;
    };
    let lines = origin.lines.clone()?;
    if lines.len() > 1 {
        return Some(lines);
    }
    if !parsed.reflow_paragraphs || kind != ContentKind::Flow {
        return None;
    }
    let range_start = parsed.source_map.original_range_for_block(block)?.start;
    let ast = parsed.real_block_for_byte(range_start)?;
    match leaf_at(ast, lines.start)? {
        Block::Paragraph { inlines, .. }
            if crate::markdown::renderer::paragraph_reflows(parsed.reflow_paragraphs, inlines) =>
        {
            Some(lines)
        }
        _ => None,
    }
}

/// The row of `block` the cursor at `pos` reveals as stacked source lines, and those lines:
/// the one gate the reveal patch (`EffectiveRows`), the view, the click and the reveal timer
/// share.  Usually the cursor's row ([`row_for_pos`]) and its [`stacked_lines`].  Two cases add
/// to that:
///
/// - The cursor's line renders no row of its own (a link reference definition inside a
///   container, a bare list marker, a nested setext underline) and shares a neighbor's
///   ([`row_for_line`]: the next row, or the last for a line past every row).  The stack is
///   then that row's lines and the cursor's, so the row it shares still shows, whether or not
///   it stacks by itself.  The union is contiguous, since any line between the two has no row
///   either.  But a marker row whose line goes on to show on the rows below it (`- - a`) stacks
///   the cursor's line alone: its own line would show twice, raw and rendered.
/// - The cursor is on a setext underline with a rule row of its own: the heading's text row
///   stacks, so every text line shows while the rule reveals as the underline.
///
/// `None` in a diagram, whose rows show its lines one for one, and for a shared table row,
/// which reveals cell by cell.
pub fn cursor_stack(parsed: &ParsedDoc, block: usize, pos: RawPos) -> Option<(usize, Range<u32>)> {
    if parsed.is_diagram_reveal_block(block) {
        return None;
    }
    let line = to_u32(pos.line);
    let row = row_for_pos(parsed, block, pos);
    let own = lines_of_row(parsed, block, row)?;
    if !own.contains(&line) {
        if table_row(parsed, block, row).is_some() {
            return None;
        }
        if lines_of_row(parsed, block, row + 1).is_some_and(|next| next.contains(&own.start)) {
            return Some((row, line..line + 1));
        }
        let lines = stacked_lines(parsed, block, row).unwrap_or(own);
        return Some((row, lines.start.min(line)..lines.end.max(line + 1)));
    }
    if let Some(lines) = stacked_lines(parsed, block, row) {
        return Some((row, lines));
    }
    // A setext underline's rule row: stack the heading's text above it.
    let range_start = parsed.source_map.original_range_for_block(block)?.start;
    let leaf = leaf_at(parsed.real_block_for_byte(range_start)?, line)?;
    if !leaf.is_setext_heading() || line + 1 != leaf.span().end {
        return None;
    }
    let text = row_for_line(parsed, block, leaf.span().start as usize);
    stacked_lines(parsed, block, text).map(|lines| (text, lines))
}

// ── Columns ───────────────────────────────────────────────────────────────

/// A position in a block's source: a block-relative line and a char column on it.  Ordered
/// line first, so a row's positions ascend.
///
/// The public functions take and return columns in the space every consumer slices the block
/// in: from the block's range start on its first line (an indented code block's range starts
/// past its indent), from the line start on every other.  Internally they count from the line
/// start, as the origins do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawPos {
    pub line: usize,
    pub col: usize,
}

/// What a row's column questions read, built on the first one and cached on the parse
/// ([`ParsedDoc::row_cache_or_init`]), one per entry of `ParsedDoc::lines`.  Nothing in it
/// depends on the cursor: the math-preview band only shifts which origin a row reads, and a
/// banded formula's rows are all chrome, which reads no positions.
#[derive(Debug, Clone)]
pub(crate) struct RowCache {
    /// The row's chars.
    chars: Box<[char]>,
    /// Where its block's original range starts; `None` for a block with none.
    range_start: Option<usize>,
    /// The source line holding that start: block-relative line 0.
    first_line: usize,
    /// Chars on that line before the range starts (an indented code block's indent).
    lead: usize,
    /// The raw position per content char, then the end of the content; `None` inside when no
    /// map renders exactly what the row shows.  Built on first use.
    positions: OnceCell<Option<Box<[RawPos]>>>,
}

impl RowCache {
    /// Physical row `abs` of `block`; `None` past the last row.
    fn new(parsed: &ParsedDoc, block: usize, abs: usize) -> Option<Self> {
        let chars = parsed
            .lines
            .get(abs)?
            .spans
            .iter()
            .flat_map(|s| s.content.chars())
            .collect();
        let range_start = parsed
            .source_map
            .original_range_for_block(block)
            .map(|r| r.start);
        let first_line = range_start.map_or(0, |b| parsed.byte_to_line(b));
        let lead = range_start
            .and_then(|b| parsed.source().get(parsed.line_start_byte(first_line)..b))
            .map_or(0, |t| t.chars().count());
        Some(Self {
            chars,
            range_start,
            first_line,
            lead,
            positions: OnceCell::new(),
        })
    }
}

/// Lines' slices joined by `\n` and mapped as one text: the raw position of each rendered char,
/// and which of them are breaks.  A paragraph's is built once per parse and shared by its rows
/// ([`ParsedDoc::with_joined_map`]), each of which takes its [`part`](Self::part): mapping the
/// whole paragraph per row cost a 500-line paragraph 100 ms over a screenful.
#[derive(Debug, Clone)]
pub(crate) struct JoinedMap {
    positions: Box<[RawPos]>,
    /// Indices into `positions` of the rendered chars that stand for breaks, ascending.
    breaks: Box<[usize]>,
}

impl JoinedMap {
    /// The map over `slices`, as [`Row::slices`] cuts them.
    fn new(slices: &[(usize, usize, &str)], labels: &crate::markdown::RefLabels) -> Self {
        let joined: Vec<&str> = slices.iter().map(|&(_, _, s)| s).collect();
        let map = InlineColMap::build_inline(&joined.join("\n"), labels);
        // Joined char index → `(line, col)`: walk the slices, each one char longer for its `\n`.
        let mut starts = Vec::with_capacity(slices.len());
        let mut at = 0usize;
        for &(_, _, s) in slices {
            starts.push(at);
            at += s.chars().count() + 1;
        }
        let pos = |c: usize| {
            let i = starts.partition_point(|&s| s <= c).saturating_sub(1);
            let (line, col, _) = slices[i];
            RawPos {
                line,
                col: col + (c - starts[i]),
            }
        };
        let forward = &map.rendered_to_raw_vec()[..map.rendered_len()];
        Self {
            positions: forward.iter().map(|&c| pos(c)).collect(),
            breaks: (0..forward.len()).filter(|&k| map.is_break(k)).collect(),
        }
    }

    /// The raw positions of the rendered chars on `lines`: those between the last break on a
    /// line above them and the first break on their last line.  For a reflowed flow, `lines`
    /// are every line mapped, and its breaks are inside it.
    fn part(&self, lines: Range<u32>) -> Option<Vec<RawPos>> {
        let (first, last) = (lines.start as usize, lines.end.saturating_sub(1) as usize);
        // A break's line never falls below an earlier one's.
        let before = |line: usize| {
            self.breaks
                .partition_point(|&k| self.positions[k].line < line)
        };
        let from = before(first)
            .checked_sub(1)
            .map_or(0, |i| self.breaks[i] + 1);
        let to = self
            .breaks
            .get(before(last))
            .copied()
            .unwrap_or(self.positions.len());
        self.positions.get(from..to).map(<[RawPos]>::to_vec)
    }
}

/// One logical row, resolved for a column question.
struct Row<'a> {
    parsed: &'a ParsedDoc,
    block: usize,
    origin: &'a RowOrigin,
    cache: &'a RowCache,
    /// The row's chars.
    chars: &'a [char],
    /// The char where its content starts: the origin's `rendered_col`, a cell, as a char.
    start: usize,
    range_start: Option<usize>,
    first_line: usize,
    lead: usize,
}

impl<'a> Row<'a> {
    /// Row `row` of `block`; `None` for a math-preview band row or past the block's rows.
    fn new(parsed: &'a ParsedDoc, block: usize, row: usize) -> Option<Self> {
        let (origins, band) = own_origins(parsed, block);
        let origin = origins.get(row.checked_sub(band)?)?;
        // The row's text is physical row `abs`, its origin row `row - band`, and its cache is
        // kept by `abs`.  The two differ only under a math-preview band, and a banded formula's
        // rows are all chrome, which reads no positions.
        debug_assert!(
            band == 0 || origin.cols == ColOrigin::Chrome,
            "a math-preview band over content rows"
        );
        let abs = parsed.source_map.rendered_lines_for_block(block).start + row;
        let cache = parsed.row_cache_or_init(abs, || RowCache::new(parsed, block, abs))?;
        let chars = &cache.chars[..];
        let start = match origin.cols {
            ColOrigin::Content { rendered_col, .. } => char_at_cell(chars, rendered_col as usize),
            ColOrigin::Chrome => 0,
        };
        Some(Self {
            parsed,
            block,
            origin,
            cache,
            chars,
            start,
            range_start: cache.range_start,
            first_line: cache.first_line,
            lead: cache.lead,
        })
    }

    /// How many chars of content the row shows past its prefix.
    fn content_len(&self) -> usize {
        self.chars.len() - self.start
    }

    /// `pos` from the space callers use (columns from the block's range start) into the
    /// origins' (columns from the line start).
    fn line_space(&self, pos: RawPos) -> RawPos {
        match pos.line {
            0 => RawPos {
                line: 0,
                col: pos.col + self.lead,
            },
            _ => pos,
        }
    }

    /// [`line_space`](Self::line_space)'s inverse.
    fn caller_space(&self, pos: RawPos) -> RawPos {
        match pos.line {
            0 => RawPos {
                line: 0,
                col: pos.col.saturating_sub(self.lead),
            },
            _ => pos,
        }
    }

    /// The real block's AST, `None` for a blank line's virtual block.
    fn ast(&self) -> Option<&'a Block> {
        self.parsed.real_block_for_byte(self.range_start?)
    }

    /// Where content starts on block-relative `line`, as its leaf recorded it.
    fn content_col(&self, line: u32) -> Option<u32> {
        let src = leaf_at(self.ast()?, line)?.src()?;
        src.col((line - src.first) as usize)
    }

    /// Where a click on a chrome row showing `line` lands: the line's content column, or, on a
    /// line that is all chrome (a fence, a setext underline), the nearest content column of the
    /// same leaf on a line with text there, below first, then above.  That puts it past the
    /// container prefix (`- `, `> `), on the fence or rule itself.  0 when the leaf has none.
    fn chrome_col(&self, line: u32) -> usize {
        let Some(src) = self
            .ast()
            .and_then(|b| leaf_at(b, line))
            .and_then(Block::src)
        else {
            return 0;
        };
        let k = (line - src.first) as usize;
        let at = |i: usize| {
            let col = src.col(i)? as usize;
            (col < self.line_len(src.first as usize + i)).then_some(col)
        };
        (k..src.len())
            .find_map(at)
            .or_else(|| (0..k).rev().find_map(at))
            .unwrap_or(0)
    }

    /// The row's raw position per content char, then the end of its content; `None` when no
    /// map renders exactly what the row shows.  Cached per row.
    fn positions(&self) -> Option<&'a [RawPos]> {
        let cache = self.cache;
        cache
            .positions
            .get_or_init(|| self.build_positions().map(Vec::into_boxed_slice))
            .as_deref()
    }

    /// An inline row's (or flow's) lines, each sliced past its content column and mapped by
    /// its own [`InlineColMap`], one space for each break; failing that, a joined map's part on
    /// the row ([`JoinedMap::part`]), for an inline spanning a break
    /// (`*a⏎b*`).  Line by line first, since joined, a continuation reading `===` or `- a` turns
    /// into block syntax it wasn't in the document.  Accepted only when the map renders as many
    /// chars as the row shows (a footnote definition's trailing ` ↩` back-link aside).
    /// References resolve against the document's definitions, and an ATX heading's closing
    /// sequence is left out, as the renderer leaves them.
    fn build_positions(&self) -> Option<Vec<RawPos>> {
        let ColOrigin::Content {
            raw_col,
            kind: ContentKind::Inline | ContentKind::Flow,
            ..
        } = self.origin.cols
        else {
            return None;
        };
        let lines = self.origin.lines.clone()?;
        let leaf = self.ast().and_then(|b| leaf_at(b, lines.start));
        let labels = self.parsed.ref_labels();
        let slices = self.slices(lines.clone(), raw_col, leaf);

        let shown = self.content_len();
        let back_link = self.chars.ends_with(&[' ', '↩']);
        let fits = |len: usize| len == shown || (back_link && len + 2 == shown);

        let mut by_line = Vec::with_capacity(shown + 1);
        for (i, &(line, col, slice)) in slices.iter().enumerate() {
            if i > 0 {
                let (prev, prev_col, prev_slice) = slices[i - 1];
                by_line.push(RawPos {
                    line: prev,
                    col: prev_col + prev_slice.chars().count(),
                });
            }
            let map = InlineColMap::build_inline(slice, labels);
            let forward = map.rendered_to_raw_vec();
            by_line.extend(
                forward[..map.rendered_len()]
                    .iter()
                    .map(|&c| RawPos { line, col: col + c }),
            );
        }
        let end = slices
            .last()
            .map_or(RawPos::default(), |&(line, col, slice)| RawPos {
                line,
                col: col + slice.chars().count(),
            });
        if fits(by_line.len()) {
            by_line.push(end);
            return Some(by_line);
        }
        // A paragraph's row is one of the segments its breaks cut it into, so an inline it
        // shares with the row above or below (`a *b⏎c* d`) reads right only over every line.
        // That map is the paragraph's, so every row of it shares one.
        let mut joined = match leaf {
            Some(Block::Paragraph { src, .. }) => {
                let first_col = src.col(0)?;
                self.parsed.with_joined_map(
                    (self.block, src.first),
                    || JoinedMap::new(&self.slices(src.span(), first_col, leaf), labels),
                    |map| map.part(lines),
                )?
            }
            _ if slices.len() >= 2 => JoinedMap::new(&slices, labels).part(lines)?,
            _ => return None,
        };
        if !fits(joined.len()) {
            return None;
        }
        joined.push(end);
        Some(joined)
    }

    /// `(line, first col, slice)` per line of `lines`: its text past its content column
    /// (`first_col` on the first), less a hard break's trailing `\` and an ATX heading's
    /// closing sequence.
    fn slices(
        &self,
        lines: Range<u32>,
        first_col: u32,
        leaf: Option<&Block>,
    ) -> Vec<(usize, usize, &'a str)> {
        let leaf_end = leaf.map_or(0, |l| l.span().end);
        let atx =
            leaf.is_some_and(|l| matches!(l, Block::Heading { .. }) && !l.is_setext_heading());
        let start = lines.start;
        lines
            .map(|l| {
                let line = self.line_text(l as usize);
                let col = if l == start {
                    Some(first_col)
                } else {
                    self.content_col(l)
                };
                // No column: the line continues an atomic inline begun above (a code span,
                // math, inline HTML, an image's alt), past its container prefix.
                let col = col.map_or_else(
                    || {
                        line.chars()
                            .take_while(|c| matches!(c, '>' | ' ' | '\t'))
                            .count()
                    },
                    |c| c as usize,
                );
                let byte = line.char_indices().nth(col).map_or(line.len(), |(b, _)| b);
                let slice = &line[byte..];
                // A trailing `\` breaks the line only where the leaf continues past it, and only
                // unescaped: an odd run (`a\\\` is an escaped `\`, then the break).
                let trailing = slice.chars().rev().take_while(|&c| c == '\\').count();
                let slice = match slice.strip_suffix('\\') {
                    Some(s) if l + 1 < leaf_end && trailing % 2 == 1 => s,
                    _ => slice,
                };
                let slice = if atx { strip_atx_closing(slice) } else { slice };
                (l as usize, col, slice)
            })
            .collect()
    }

    /// The row's first source line, or the nearest owned line above it.
    fn home_line(&self, row: usize) -> usize {
        line_for_row(self.parsed, self.block, row)
    }

    /// Block-relative `line`'s text from its start, `\r` aside; `""` for a block with no range.
    fn line_text(&self, line: usize) -> &'a str {
        if self.range_start.is_none() {
            return "";
        }
        let text = self.parsed.source_line(self.first_line + line);
        text.strip_suffix('\r').unwrap_or(text)
    }

    /// Chars on block-relative `line`, `\r` aside.
    fn line_len(&self, line: usize) -> usize {
        self.line_text(line).chars().count()
    }

    /// The raw column the rendered prefix's end lines up with on `line`: the content column,
    /// less however many more spaces the raw prefix ends in than the rendered one.  A marker
    /// padded out to its content (`1.  foo`, `-   foo`) then still lines up with its glyph,
    /// and the surplus spaces, which nothing renders, land on the content start.
    fn prefix_anchor(&self, line: usize, raw_col: usize) -> usize {
        let raw: Vec<char> = self.line_text(line).chars().take(raw_col).collect();
        let raw_gap = raw
            .iter()
            .rev()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .count();
        let rendered_gap = self.chars[..self.start]
            .iter()
            .rev()
            .take_while(|&&c| c == ' ')
            .count();
        raw_col - raw_gap.saturating_sub(rendered_gap)
    }
}

/// The char of `chars` at cell `cell`, or the row's end.
fn char_at_cell(chars: &[char], cell: usize) -> usize {
    let mut at = 0usize;
    for (i, &ch) in chars.iter().enumerate() {
        if at >= cell {
            return i;
        }
        at += char_cells(ch);
    }
    chars.len()
}

/// The leaf `line` (block-relative) belongs to inside `block`.
fn leaf_at(block: &Block, line: u32) -> Option<&Block> {
    match block {
        Block::BlockQuote { blocks, .. } | Block::FootnoteDefinition { blocks, .. } => {
            blocks.iter().find_map(|b| leaf_at(b, line))
        }
        Block::List { items, .. } => items
            .iter()
            .flat_map(|i| &i.blocks)
            .find_map(|b| leaf_at(b, line)),
        leaf => leaf.span().contains(&line).then_some(leaf),
    }
}

/// Whether row `row` of `block` de-renders to raw source when it is the revealed cursor row.
/// Every row does but one showing characters verbatim (a code body, frontmatter, raw HTML),
/// which would look the same raw.  A row the origins don't hold (a band row, a blank line's
/// missing row) reveals.
pub fn reveals(parsed: &ParsedDoc, block: usize, row: usize) -> bool {
    let (origins, band) = own_origins(parsed, block);
    let origin = row.checked_sub(band).and_then(|r| origins.get(r));
    !matches!(
        origin.map(|o| o.cols),
        Some(ColOrigin::Content {
            kind: ContentKind::Verbatim,
            ..
        })
    )
}

/// The source position rendered char `rendered` of row `row` of `block` shows: where a click on
/// it lands.  A char in the row's prefix (a marker, a bar, a leader) lands on the raw prefix
/// right-aligned with it, so a task box or footnote leader is hit exactly; a code row's pad cell,
/// which stands for nothing, on the content start.  One past the content lands on its end.  A
/// chrome row lands on its line's content start, or, for a line with none (a fence), on its
/// leaf's nearest one, past any container prefix.
pub fn rendered_to_raw_col(
    parsed: &ParsedDoc,
    block: usize,
    row: usize,
    rendered: usize,
) -> RawPos {
    let Some(r) = Row::new(parsed, block, row) else {
        return RawPos {
            line: line_for_row(parsed, block, row),
            col: 0,
        };
    };
    let ColOrigin::Content { raw_col, kind, .. } = r.origin.cols else {
        let line = r.home_line(row);
        let col = r.chrome_col(to_u32(line));
        return r.caller_space(RawPos {
            line,
            col: col.min(r.line_len(line)),
        });
    };
    if rendered < r.start && kind != ContentKind::Verbatim {
        // The prefix lines up with the raw one from the right, so a click on a task box, a
        // footnote leader or a list marker lands on the raw marker it stands for whatever
        // indent, bullet or number padding the two disagree on.  Indentation stands for nothing
        // and lands past itself: on the marker, or on a continuation line's content.
        let line = r.home_line(row);
        let text = r.line_text(line);
        let indent = text.chars().take_while(|c| matches!(c, ' ' | '\t')).count();
        let anchor = r.prefix_anchor(line, raw_col as usize);
        let col = (rendered + anchor).saturating_sub(r.start).max(indent);
        // One reaching the content start lands where its first char does, past any marker
        // hiding there (`*a*`), as a click on that char would.
        if col < anchor {
            return r.caller_space(RawPos { line, col });
        }
    }
    let k = rendered.saturating_sub(r.start).min(r.content_len());
    if let Some(positions) = r.positions() {
        return r.caller_space(positions[k.min(positions.len() - 1)]);
    }
    // Verbatim, or content no map renders exactly: one char for one past the content start.
    let line = r.home_line(row);
    r.caller_space(RawPos {
        line,
        col: (raw_col as usize).saturating_add(k).min(r.line_len(line)),
    })
}

/// The rendered char of row `row` of `block` showing source position `pos`: where its cursor
/// indicator sits and where an overlay starting or ending there paints.  A position in the
/// row's raw prefix lands on the rendered prefix right-aligned with it (the inverse of
/// [`rendered_to_raw_col`]), and in a code row's indent on the content start.  `None` when the
/// row can't place it exactly: a chrome row, a line the row doesn't show, content no inline map
/// renders exactly.  An overlay then skips rather than painting off by N.
pub fn raw_to_rendered_col(
    parsed: &ParsedDoc,
    block: usize,
    row: usize,
    pos: RawPos,
) -> Option<usize> {
    let r = Row::new(parsed, block, row)?;
    let ColOrigin::Content { raw_col, kind, .. } = r.origin.cols else {
        return None;
    };
    let pos = r.line_space(pos);
    let lines = r.origin.lines.clone()?;
    if !lines.contains(&to_u32(pos.line)) {
        return None;
    }
    if pos.line == lines.start as usize
        && pos.col < raw_col as usize
        && kind != ContentKind::Verbatim
    {
        // In the raw prefix: the inverse of `rendered_to_raw_col`'s right alignment.
        let anchor = r.prefix_anchor(pos.line, raw_col as usize);
        return Some((pos.col + r.start).saturating_sub(anchor).min(r.start));
    }
    if kind == ContentKind::Verbatim {
        let k = pos
            .col
            .saturating_sub(raw_col as usize)
            .min(r.content_len());
        return Some(r.start + k);
    }
    let positions = r.positions()?;
    Some(r.start + positions.partition_point(|p| *p < pos).min(r.content_len()))
}

/// [`raw_to_rendered_col`], or its best guess where that has no exact answer: one char for one
/// past the content start on the row's own line, else the content start, and on a chrome row
/// the column itself.  For the cursor indicator, which must sit somewhere.
pub fn raw_to_rendered_col_near(
    parsed: &ParsedDoc,
    block: usize,
    row: usize,
    pos: RawPos,
) -> usize {
    if let Some(col) = raw_to_rendered_col(parsed, block, row, pos) {
        return col;
    }
    let Some(r) = Row::new(parsed, block, row) else {
        return pos.col;
    };
    let pos = r.line_space(pos);
    match r.origin.cols {
        ColOrigin::Content { raw_col, .. } if r.origin.first_line() == Some(to_u32(pos.line)) => {
            r.start
                + pos
                    .col
                    .saturating_sub(raw_col as usize)
                    .min(r.content_len())
        }
        ColOrigin::Content { .. } => r.start,
        ColOrigin::Chrome => pos.col.min(r.chars.len()),
    }
}

// ── Tables ────────────────────────────────────────────────────────────────

/// The table row a row of a block belongs to, from the origins: what a click, an overlay and
/// the cursor indicator need to find before `table_layout`'s cell geometry maps the columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRowHit {
    /// The table row's block-relative source line.
    pub line: usize,
    /// Its index in the table: 0 the header, `1 + i` data row `i`.
    pub index: u32,
    /// The wrap chunk the row shows; 0 for a border or separator snapped onto the table row.
    pub sub: usize,
    /// Whether the row shows the table row's cells, rather than a border or separator.
    pub cells: bool,
    /// The block's rows showing the table row's cells, one per wrap chunk.
    pub rows: Range<usize>,
}

/// The table row that row `row` of `block` belongs to; `None` outside a table.  A row of cells
/// is its own.  A border or separator (chrome inside a table leaf) snaps onto a table row: the
/// one showing the same line (a separator or the bottom border, onto the row above it), else the
/// one directly below (the top border onto the header, the heavy rule onto the first data row),
/// else the nearest above (the heavy rule of a table with no data rows).
pub fn table_row(parsed: &ParsedDoc, block: usize, row: usize) -> Option<TableRowHit> {
    let (origins, band) = own_origins(parsed, block);
    let r = row.checked_sub(band)?;
    let origin = origins.get(r)?;
    let cells_of = |o: &RowOrigin| match o.cols {
        ColOrigin::Content {
            kind: ContentKind::TableRow { row, sub },
            ..
        } => Some((o.first_line()? as usize, row, sub as usize)),
        _ => None,
    };
    let (at, (line, index, sub)) = if let Some(hit) = cells_of(origin) {
        (r, hit)
    } else {
        if origin.cols != ColOrigin::Chrome {
            return None;
        }
        // The top border shows no line; the header below it does.
        let line = origin
            .first_line()
            .or_else(|| origins.get(r + 1)?.first_line())?;
        let range_start = parsed.source_map.original_range_for_block(block)?.start;
        let ast = parsed.real_block_for_byte(range_start)?;
        if !matches!(leaf_at(ast, line), Some(Block::Table { .. })) {
            return None;
        }
        // A row of cells showing the same line is always above: the top border's line and the
        // delimiter line show on no row of cells.
        let shows = |o: &RowOrigin| cells_of(o).is_some_and(|(l, ..)| l == line as usize);
        let at = origins[..r]
            .iter()
            .rposition(shows)
            .or_else(|| origins.get(r + 1).and_then(cells_of).map(|_| r + 1))
            .or_else(|| origins[..r].iter().rposition(|o| cells_of(o).is_some()))?;
        let (line, index, _) = cells_of(&origins[at])?;
        (at, (line, index, 0))
    };
    let same = |o: &RowOrigin| cells_of(o).is_some_and(|(l, i, _)| l == line && i == index);
    let first = origins[..at]
        .iter()
        .rposition(|o| !same(o))
        .map_or(0, |i| i + 1);
    let end = origins[at..]
        .iter()
        .position(|o| !same(o))
        .map_or(origins.len(), |i| at + i);
    Some(TableRowHit {
        line,
        index,
        sub,
        cells: at == r,
        rows: first + band..end + band,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn doc(src: &str) -> ParsedDoc {
        ParsedDoc::build(src, theme(), true, 4)
    }

    /// Rows and lines of the block holding `byte`.
    fn block_at(doc: &ParsedDoc, byte: usize) -> usize {
        doc.source_map.block_for_byte(byte).unwrap()
    }

    #[test]
    fn a_line_with_no_row_shares_the_next_lines_row() {
        // A link reference definition inside a quote renders no row.
        let d = doc("> a\n>\n> [r]: /u\n> b\n");
        let b = block_at(&d, 0);
        assert_eq!(
            (0..4).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [0, 1, 2, 2]
        );
        assert_eq!(line_for_row(&d, b, 2), 3);
    }

    #[test]
    fn a_blank_between_an_items_blocks_has_a_row_of_its_own() {
        let d = doc("- a\n\n  b\n");
        let b = block_at(&d, 0);
        assert_eq!(
            (0..3).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        // So does a blank between a nested loose list's items.
        let d = doc("- x\n  - b\n\n  - d\n");
        let b = block_at(&d, 0);
        assert_eq!(
            (0..4).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn a_marker_on_a_row_of_its_own_keeps_the_rows_below_on_their_lines() {
        // Rows: `•`, ` bash ` label, body, closing fence, `• next`.
        let d = doc("- ```bash\n  code\n  ```\n- next\n");
        let b = block_at(&d, 0);
        assert_eq!(
            (0..4).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [0, 2, 3, 4]
        );
        assert_eq!(
            (0..5).map(|r| line_for_row(&d, b, r)).collect::<Vec<_>>(),
            [0, 0, 1, 2, 3]
        );
    }

    #[test]
    fn a_row_no_line_owns_belongs_to_the_line_above() {
        // An unclosed fence's closing placeholder.
        let d = doc("- a\n  ```\n  x\n- b\n");
        let b = block_at(&d, 0);
        assert_eq!(lines_of_row(&d, b, 3), None);
        assert_eq!(line_for_row(&d, b, 3), 2);
        assert_eq!(row_for_line(&d, b, 3), 4);
    }

    /// A line past every row (a cursor on the blank line a block's range absorbs) lands on the
    /// last row that shows a line, not on a trailing placeholder.
    #[test]
    fn a_line_past_every_row_skips_trailing_chrome() {
        let d = doc("- a\n  ```\n  x\n");
        let b = block_at(&d, 0);
        let rows = d.block_own_line_count(b);
        assert_eq!(
            lines_of_row(&d, b, rows - 1),
            None,
            "the placeholder closes the block"
        );
        assert_eq!(row_for_line(&d, b, 9), rows - 2);
    }

    #[test]
    fn a_table_line_lands_on_its_first_content_row() {
        // Rows: top border, header, heavy rule, data row, bottom border.
        let d = doc("| a | b |\n|---|---|\n| 1 | 2 |\n");
        let b = block_at(&d, 0);
        assert_eq!(
            (0..3).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(line_for_row(&d, b, 0), 0);
        assert_eq!(line_for_row(&d, b, 4), 2);
    }

    /// Every row of a table finds its table row: cells their own, a border or separator the one
    /// it snaps onto.
    #[test]
    fn a_tables_borders_snap_onto_its_rows() {
        // Rows: top border, header, heavy rule, data 1, thin separator, data 2, bottom border.
        let d = doc("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n");
        let b = block_at(&d, 0);
        let hit = |r| table_row(&d, b, r).map(|h| (h.line, h.index, h.cells));
        assert_eq!(
            (0..7).map(hit).collect::<Vec<_>>(),
            [
                Some((0, 0, false)),
                Some((0, 0, true)),
                Some((2, 1, false)),
                Some((2, 1, true)),
                Some((2, 1, false)),
                Some((3, 2, true)),
                Some((3, 2, false)),
            ]
        );
        assert_eq!(table_row(&d, b, 3).unwrap().rows, 3..4);
    }

    /// A table nested in a list item is found by its origins, behind the item's other rows; the
    /// item's own rows are no table's.
    #[test]
    fn a_nested_tables_rows_are_found_inside_their_block() {
        let d = doc("- item\n\n  | a | b |\n  |---|---|\n  | 1 | 2 |\n");
        let b = block_at(&d, 0);
        assert_eq!(table_row(&d, b, 0), None, "the item's text");
        let header = row_for_line(&d, b, 2);
        assert_eq!(
            table_row(&d, b, header - 1).map(|h| (h.line, h.cells)),
            Some((2, false)),
            "the top border"
        );
        let data = row_for_line(&d, b, 4);
        let hit = table_row(&d, b, data).unwrap();
        assert_eq!((hit.line, hit.index, hit.sub, hit.cells), (4, 1, 0, true));
        assert_eq!(hit.rows, data..data + 1);
        assert_eq!(
            table_row(&d, b, data - 1).map(|h| h.line),
            Some(4),
            "heavy rule"
        );
    }

    /// A row that wraps is one table row over several rows, each its own chunk.
    #[test]
    fn a_wrapped_table_rows_chunks_share_it() {
        let src = "| a | b |\n|---|---|\n| x | aa bb cc dd ee ff |\n";
        let d = ParsedDoc::build_with_overrides(
            src,
            theme(),
            true,
            4,
            None,
            None,
            false,
            16,
            false,
            false,
            true,
            false,
            None,
        );
        let b = block_at(&d, 0);
        let first = row_for_line(&d, b, 2);
        let hit = table_row(&d, b, first + 1).unwrap();
        assert!(hit.rows.len() > 1, "fixture: the row wraps");
        assert_eq!((hit.line, hit.sub, hit.rows.start), (2, 1, first));
    }

    /// A row showing several lines (a multi-line heading's text) is the row of each of them.
    #[test]
    fn every_line_of_a_multi_line_row_lands_on_it() {
        // Rows: the heading's text (lines 0–1), its rule (line 2).
        let d = doc("Title\nmore\n=====\n");
        let b = block_at(&d, 0);
        assert_eq!(lines_of_row(&d, b, 0), Some(0..2));
        assert_eq!(
            (0..3).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [0, 0, 1]
        );
        assert_eq!(line_for_row(&d, b, 1), 2);
    }

    /// The math-preview band sits above a revealed formula's source rows: its rows show no
    /// line, and every line moves down past it.
    #[test]
    fn a_math_preview_band_shifts_the_source_rows_down() {
        let mut d = doc("$$\nx\n$$\n");
        let b = block_at(&d, 0);
        d.math_source_offset = Some((b, 1));
        assert_eq!(lines_of_row(&d, b, 0), None);
        assert_eq!(
            line_for_row(&d, b, 0),
            0,
            "a band row resolves to the first line"
        );
        assert_eq!(
            (0..3).map(|l| row_for_line(&d, b, l)).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            (1..4).map(|r| line_for_row(&d, b, r)).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[test]
    fn an_image_pins_every_reserved_row_to_its_line() {
        let d = doc("![alt](missing.png)\n");
        let b = block_at(&d, 0);
        assert_eq!(d.block_own_line_count(b), 4);
        assert!((0..4).all(|r| line_for_row(&d, b, r) == 0));
        assert_eq!(row_for_line(&d, b, 0), 0);
    }

    /// A line whose first row is a marker of its own shows its chars on the row below, and the
    /// cursor goes there; a line no later row shows stays on its first.
    #[test]
    fn a_position_lands_on_the_row_showing_its_char() {
        // Rows: `•`, `  • a`, `    • b`, `• c`.
        let d = doc("- - a\n  - b\n- c\n");
        let b = block_at(&d, 0);
        assert_eq!(row_for_line(&d, b, 0), 0);
        for col in [0, 2, 4, 5] {
            assert_eq!(row_for_pos(&d, b, RawPos { line: 0, col }), 1, "col {col}");
        }
        assert_eq!(row_for_pos(&d, b, RawPos { line: 2, col: 2 }), 3);
        // Rows: `•`, the ` bash ` label, the body, the closing fence.
        let d = doc("- ```bash\n  x\n  ```\n");
        let b = block_at(&d, 0);
        assert_eq!(row_for_pos(&d, b, RawPos { line: 0, col: 4 }), 0);
    }

    // ── Columns ───────────────────────────────────────────────────────────

    /// The row of `block` whose text contains `needle`.
    fn row_with(d: &ParsedDoc, block: usize, needle: &str) -> usize {
        let rows = d.source_map.rendered_lines_for_block(block);
        rows.clone()
            .position(|r| {
                d.lines[r]
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .contains(needle)
            })
            .expect("row present")
    }

    fn pos(line: usize, col: usize) -> RawPos {
        RawPos { line, col }
    }

    /// A fenced block reveals its fence rows and never its body; an unclosed fence's last line
    /// is ordinary code; an indented block reveals nothing; prose reveals.
    #[test]
    fn only_verbatim_rows_never_reveal() {
        let d = doc("```rust\nlet x = 1;\n```\n\n    indented\n\npara\n\n```\nopen\n");
        let fenced = block_at(&d, 0);
        assert_eq!(
            (0..3).map(|r| reveals(&d, fenced, r)).collect::<Vec<_>>(),
            [true, false, true]
        );
        let src = d.source();
        let indented = block_at(&d, src.find("indented").unwrap());
        assert!(!reveals(&d, indented, 0));
        assert!(reveals(&d, block_at(&d, src.find("para").unwrap()), 0));
        let open = block_at(&d, src.find("open").unwrap());
        assert!(!reveals(&d, open, row_with(&d, open, "open")));
    }

    /// A figures-off `$$…$$` formula reveals its `$$` rows, never its body.
    #[test]
    fn a_figures_off_formula_reveals_only_its_delimiters() {
        let d = ParsedDoc::build_with_overrides(
            "$$\nE = mc^2\n$$\n",
            theme(),
            true,
            4,
            None,
            None,
            false,
            80,
            false,
            false,
            false,
            false,
            None,
        );
        let b = block_at(&d, 0);
        let body = row_with(&d, b, "E = mc^2");
        assert!(!reveals(&d, b, body));
        assert!(reveals(&d, b, body - 1) && reveals(&d, b, body + 1));
    }

    /// A code body renders behind one pad cell, past the indent pulldown-cmark stripped; a click
    /// on the pad lands on the first content char, one in the fill past the line on its end.
    #[test]
    fn a_code_row_maps_past_its_pad_cell() {
        // Inside a list item: the item indent is stripped too.
        let d = doc("- a\n\n  ```\n  let x = 1;\n  ```\n");
        let b = block_at(&d, 0);
        let row = row_with(&d, b, "let x");
        let eq = 2 + "let x ".len();
        assert_eq!(rendered_to_raw_col(&d, b, row, 7), pos(3, eq));
        assert_eq!(raw_to_rendered_col(&d, b, row, pos(3, eq)), Some(7));
        assert_eq!(
            rendered_to_raw_col(&d, b, row, 0),
            pos(3, 2),
            "the pad cell"
        );
        assert_eq!(rendered_to_raw_col(&d, b, row, 60), pos(3, 12), "the fill");
        // A column in the stripped indent shows on the first content cell.
        assert_eq!(raw_to_rendered_col(&d, b, row, pos(3, 0)), Some(1));
    }

    /// A click on a chrome row whose line is all chrome (a fence, a setext underline) lands past
    /// the container prefix, on the fence or rule itself, not in the indent or on the `>`.
    #[test]
    fn a_chrome_rows_click_lands_past_the_container_prefix() {
        // Rows: `• a`, the ` rust ` label, the body, the closing fence.
        let d = doc("- a\n\n  ```rust\n  x\n  ```\n");
        let b = block_at(&d, 0);
        let label = row_with(&d, b, "rust");
        assert_eq!(rendered_to_raw_col(&d, b, label, 3), pos(2, 2));
        assert_eq!(rendered_to_raw_col(&d, b, label + 2, 0), pos(4, 2));
        // The fence opens on the item's marker line, inside a quote.
        let d = doc("> - ```\n>   x\n>   ```\n");
        let b = block_at(&d, 0);
        let body = row_with(&d, b, "x");
        assert_eq!(rendered_to_raw_col(&d, b, body + 1, 0), pos(2, 4));
        assert_eq!(rendered_to_raw_col(&d, b, body - 1, 0), pos(0, 4));
        // The underline takes the heading text's column.
        let d = doc("> Foo\n> ===\n");
        let b = block_at(&d, 0);
        assert_eq!(lines_of_row(&d, b, 1), Some(1..2), "the rule row");
        assert_eq!(rendered_to_raw_col(&d, b, 1, 0), pos(1, 2));
    }

    /// An indented block's range starts past its indent, and columns on its first line count
    /// from there, as the callers slice it.
    #[test]
    fn an_indented_blocks_first_line_counts_from_its_range() {
        let d = doc("Intro.\n\n    let x = 1;\n");
        let b = block_at(&d, d.source().find("let").unwrap());
        assert_eq!(rendered_to_raw_col(&d, b, 0, 7), pos(0, 6));
        assert_eq!(raw_to_rendered_col(&d, b, 0, pos(0, 6)), Some(7));
    }

    /// A marker's cells line up with the raw marker from the right, so a task box is hit
    /// exactly; indentation lands past itself.
    #[test]
    fn a_prefix_aligns_with_its_raw_marker_from_the_right() {
        let d = doc("- a\n  - [ ] b\n");
        let b = block_at(&d, 0);
        let row = row_with(&d, b, "b");
        // Rendered `    • [ ] b` (or the bullet-less task form): the `[` is right-aligned.
        let text: String = d.lines[row]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        let bracket = text.chars().position(|c| c == '[').unwrap();
        assert_eq!(rendered_to_raw_col(&d, b, row, bracket), pos(1, 4));
        assert_eq!(raw_to_rendered_col(&d, b, row, pos(1, 4)), Some(bracket));
        assert_eq!(
            rendered_to_raw_col(&d, b, row, 0),
            pos(1, 2),
            "indent lands on the marker"
        );
    }

    /// A marker padded out to its content with more spaces than the rendered one still lines up
    /// with its glyph; the surplus spaces render nothing and show on the content start.
    #[test]
    fn a_padded_marker_aligns_with_its_glyph() {
        for (src, marker) in [("1.  foo\n", "1."), ("-   foo\n", "-"), (">   foo\n", ">")] {
            let d = doc(src);
            let b = block_at(&d, 0);
            let content = src.find('f').unwrap();
            for (i, _) in marker.char_indices() {
                assert_eq!(
                    rendered_to_raw_col(&d, b, 0, i),
                    pos(0, i),
                    "{src:?} char {i}"
                );
                assert_eq!(
                    raw_to_rendered_col(&d, b, 0, pos(0, i)),
                    Some(i),
                    "{src:?} col {i}"
                );
            }
            let start = raw_to_rendered_col(&d, b, 0, pos(0, content)).unwrap();
            assert_eq!(
                raw_to_rendered_col(&d, b, 0, pos(0, content - 1)),
                Some(start),
                "{src:?}: a surplus space"
            );
        }
    }

    /// A line ending in an escaped backslash (`a\\`) has no hard break to drop: its rendered `\`
    /// maps to the backslash that shows, not the one escaping it.
    #[test]
    fn an_escaped_trailing_backslash_maps_to_itself() {
        let d = doc("a\\\\\nb\n");
        let b = block_at(&d, 0);
        assert_eq!(rendered_to_raw_col(&d, b, 0, 1), pos(0, 2));
        assert_eq!(raw_to_rendered_col(&d, b, 0, pos(0, 2)), Some(1));
    }

    /// A flow maps across its lines, a break onto the end of the line it ends; an inline
    /// spanning a break maps through the joined text.
    #[test]
    fn a_flow_maps_across_its_lines() {
        let d = ParsedDoc::build_with_overrides(
            "one\n*two\nthree*\n",
            theme(),
            true,
            4,
            None,
            None,
            false,
            80,
            false,
            false,
            true,
            true,
            None,
        );
        let b = block_at(&d, 0);
        // Rendered `one two three`.
        assert_eq!(rendered_to_raw_col(&d, b, 0, 3), pos(0, 3), "the break");
        assert_eq!(
            rendered_to_raw_col(&d, b, 0, 4),
            pos(1, 1),
            "`t` past the `*`"
        );
        assert_eq!(rendered_to_raw_col(&d, b, 0, 8), pos(2, 0));
        assert_eq!(raw_to_rendered_col(&d, b, 0, pos(2, 2)), Some(10));
        assert_eq!(
            raw_to_rendered_col(&d, b, 0, pos(1, 0)),
            Some(4),
            "a marker shows on the next char"
        );
    }

    /// A row a break cuts out of a paragraph maps as part of the whole paragraph: its own line
    /// alone reads `c* d` with a literal `*`.
    #[test]
    fn a_row_cut_from_an_inline_maps_through_its_paragraph() {
        let d = doc("a *b\nc* d\n");
        let b = block_at(&d, 0);
        assert_eq!(lines_of_row(&d, b, 0), Some(0..1));
        assert_eq!(lines_of_row(&d, b, 1), Some(1..2));
        // Rendered `a b`, then `c d`.
        assert_eq!(
            rendered_to_raw_col(&d, b, 0, 2),
            pos(0, 3),
            "`b` past the `*`"
        );
        assert_eq!(
            raw_to_rendered_col(&d, b, 0, pos(0, 4)),
            Some(3),
            "the row's end"
        );
        assert_eq!(rendered_to_raw_col(&d, b, 1, 0), pos(1, 0));
        assert_eq!(
            rendered_to_raw_col(&d, b, 1, 2),
            pos(1, 3),
            "`d` past the `*`"
        );
        assert_eq!(raw_to_rendered_col(&d, b, 1, pos(1, 1)), Some(1), "the `*`");
    }

    /// Where no map renders exactly what the row shows, columns can't be placed exactly, and
    /// the indicator falls back to one char for one past the content start.
    #[test]
    fn a_row_no_map_matches_has_no_exact_columns() {
        // pulldown-cmark folds `ss` and `ß` to one label; the maps' lowercasing doesn't, so the
        // reference collapses in the row but stays literal in the map.
        let d = doc("x[^ss] y\n\n[^ß]: n\n");
        let b = block_at(&d, 0);
        assert_eq!(raw_to_rendered_col(&d, b, 0, pos(0, 7)), None);
        assert_eq!(raw_to_rendered_col_near(&d, b, 0, pos(0, 7)), 7);
    }

    /// A reference link or footnote the document defines renders as the document parsed it, so
    /// its row maps exactly; an undefined footnote reference stays literal and maps one for one.
    #[test]
    fn a_rows_references_resolve_against_the_documents_definitions() {
        let d = doc("see [it][r] now [^n] [^x]\n\n[r]: /u\n[^n]: note\n");
        let b = block_at(&d, 0);
        let text: String = d.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.starts_with("see it now [n] [^x]"), "{text:?}");
        assert_eq!(
            rendered_to_raw_col(&d, b, 0, 7),
            pos(0, 12),
            "the `n` of `now`"
        );
        assert_eq!(raw_to_rendered_col(&d, b, 0, pos(0, 12)), Some(7));
        assert_eq!(
            raw_to_rendered_col(&d, b, 0, pos(0, 19)),
            Some(13),
            "the `]` of `[^n]`"
        );
        assert_eq!(
            raw_to_rendered_col(&d, b, 0, pos(0, 23)),
            Some(17),
            "the `x` of `[^x]`"
        );
    }

    /// An ATX heading's closing sequence renders nothing, and its row maps exactly up to it.
    #[test]
    fn an_atx_headings_closing_sequence_is_not_content() {
        for src in ["## Title ##\n", "> ## Title ##\n"] {
            let d = doc(src);
            let b = block_at(&d, 0);
            let row = row_with(&d, b, "Title");
            let t = src.find('T').unwrap();
            let shown = raw_to_rendered_col(&d, b, row, pos(0, t)).expect("an exact map");
            assert_eq!(
                raw_to_rendered_col(&d, b, row, pos(0, t + 4)),
                Some(shown + 4),
                "{src:?}"
            );
            assert_eq!(
                rendered_to_raw_col(&d, b, row, shown + 4),
                pos(0, t + 4),
                "{src:?}"
            );
        }
    }
}
