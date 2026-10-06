//! The event stream the block parser consumes: pulldown-cmark's `(Event, Range)` pairs behind a
//! peek/next interface, recording source positions as a side effect.
//!
//! The AST builder asks for events exactly as it did when it consumed bare `Event`s; ranges stay
//! here.  While a leaf block is open ([`EventStream::begin_leaf`] … [`EventStream::end_leaf`])
//! every event it consumes is noted against the source line it starts on, which is what
//! a [`SrcLines`]' columns are made of.  Container parsers ask for their own line spans through
//! [`EventStream::container_span`].  The same walk feeds the [`RangeTracker`], so the parse, its
//! top-level ranges and its positions stay one pass over one stream.
//!
//! Line numbers are block-relative ([`EventStream::set_base`]; see [`SrcLines`] for why).

use std::ops::Range;

use pulldown_cmark::Event;

use crate::markdown::ast::{to_u32, LineSpan, SrcLines};
use crate::markdown::parse_offsets::{BlockKind, RangeTracker};

/// Byte offset → (line, char column) over one source text.
pub(super) struct LineIndex<'s> {
    src: &'s str,
    /// Byte offset of every line start; `starts[0] == 0`.
    starts: Vec<usize>,
    /// Line of the last [`line_at`](Self::line_at) answer: events arrive in source order, so the
    /// next answer is almost always this line or a few after it.
    cursor: usize,
}

impl<'s> LineIndex<'s> {
    pub(super) fn new(src: &'s str) -> Self {
        // Sized for typical prose line lengths, so the table rarely regrows.
        let mut starts = Vec::with_capacity(src.len() / 32 + 1);
        starts.push(0);
        starts.extend(memchr::memchr_iter(b'\n', src.as_bytes()).map(|i| i + 1));
        Self {
            src,
            starts,
            cursor: 0,
        }
    }

    /// 0-based line containing `byte`; past the end answers the last line.
    fn line_of(&self, byte: usize) -> usize {
        self.starts
            .partition_point(|&s| s <= byte)
            .saturating_sub(1)
    }

    /// [`line_of`](Self::line_of), stepping forward from the last answer when `byte` is at or
    /// past it, which in-order events nearly always are.
    fn line_at(&mut self, byte: usize) -> usize {
        let at_or_past_cursor = self.starts.get(self.cursor).is_some_and(|&s| s <= byte);
        if !at_or_past_cursor {
            self.cursor = self.line_of(byte);
            return self.cursor;
        }
        while self.starts.get(self.cursor + 1).is_some_and(|&s| s <= byte) {
            self.cursor += 1;
            // A long jump (a big code block, a skipped container) is cheaper searched.
            if self.starts.get(self.cursor + 8).is_some_and(|&s| s <= byte) {
                self.cursor = self.line_of(byte);
                break;
            }
        }
        self.cursor
    }

    /// The line `range`'s last byte is on, given `line`, the one its first is on.
    fn end_line(&self, line: usize, range: &Range<usize>) -> usize {
        let end = range.end.min(self.src.len());
        // Ending no further than the next line's start, its last byte is at most this line's
        // newline: nearly every event, answered without a scan.
        if end <= range.start || self.starts.get(line + 1).is_none_or(|&next| end <= next) {
            return line;
        }
        // Bytes before the last one: a newline ending the range is still its last line's.  Most
        // events hold no newline at all, so a memchr scan answers before any counting.
        let body = &self.src.as_bytes()[range.start..end - 1];
        if memchr::memchr(b'\n', body).is_none() {
            return line;
        }
        line + memchr::memchr_iter(b'\n', body).count()
    }

    /// Char column of `byte` on `line`.  The recorder asks once per line of a leaf at most (its
    /// first content event's), so counting from the line start is never repeated work.  A
    /// CRLF line's terminator is one: `byte` at its `\n` answers the `\r`'s column, so a blank
    /// CRLF code line's text starts at column 0 as an LF one's does.
    fn col_of(&self, line: usize, byte: usize) -> usize {
        let mut byte = byte.min(self.src.len());
        let line_start = self.starts.get(line).copied().unwrap_or(0).min(byte);
        if byte > line_start && self.src.as_bytes()[byte - 1..].starts_with(b"\r\n") {
            byte -= 1;
        }
        self.src[line_start..byte].chars().count()
    }

    /// `line`'s text, without its newline; empty past the end.
    fn line_text(&self, line: usize) -> &'s str {
        let Some(&start) = self.starts.get(line) else {
            return "";
        };
        let end = self
            .starts
            .get(line + 1)
            .map_or(self.src.len(), |&next| next - 1);
        self.src.get(start..end).unwrap_or("")
    }

    /// The line of the last byte of `range`, ignoring trailing newlines — the last line the
    /// range has content on — given `line`, the one its first byte is on.  An empty range
    /// answers `line`.
    fn last_line(&self, line: usize, range: &Range<usize>) -> usize {
        let bytes = self.src.as_bytes();
        let mut end = range.end.min(bytes.len());
        while end > range.start && bytes[end - 1] == b'\n' {
            end -= 1;
        }
        self.end_line(line, &(range.start..end))
    }

    /// Whether `range` stops partway through a line that continues past it — pulldown-cmark
    /// ends some container ranges inside the next line's prefix (`> `).
    fn ends_mid_line(&self, range: &Range<usize>) -> bool {
        let bytes = self.src.as_bytes();
        let end = range.end;
        let at_line_end = |rest: &[u8]| rest.starts_with(b"\n") || rest.starts_with(b"\r\n");
        end > range.start
            && end < bytes.len()
            && bytes[end - 1] != b'\n'
            && !at_line_end(&bytes[end..])
    }
}

/// Which of a leaf's events can begin one of its source lines, so the recorder looks at those
/// and no others: noting every inline event of a prose paragraph is a large share of the parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LeafMode {
    /// A paragraph or heading: a new line's content begins only with the leaf's first event and
    /// the first one after a soft or hard break.  A code span, math span, inline HTML or image
    /// is looked at too, since one running across lines makes its later lines start nothing.
    Prose,
    /// Code, frontmatter, raw HTML, a rule: a few events per line, every one looked at.
    Verbatim,
    /// A table: each row's line begins with its `TableHead` / `TableRow`.
    Table,
}

/// Positions noted while one leaf block is open.  Absolute lines.
struct LeafRec {
    mode: LeafMode,
    /// [`LeafMode::Prose`]: the next event begins a line (the leaf's first, or one past a
    /// break).
    line_pending: bool,
    /// [`LeafMode::Prose`]: where the line past the pending break starts, while
    /// [`line_pending`](Self::line_pending) is owed to one.
    break_end: Option<usize>,
    /// First line, once known: the leaf's `Start` event's, or else its first content event's.
    first: Option<usize>,
    last: usize,
    /// Per line from `first`, the earliest content column seen.  The stream's one buffer, lent
    /// to each leaf in turn, so recording allocates nothing per leaf.
    cols: Vec<Option<u32>>,
    /// `(start, until)`: an atomic inline (code span, math, inline HTML, image) begun on line `start`
    /// runs through line `until`, so lines `start + 1 ..= until` have no content start of their
    /// own.
    atomic: Option<(usize, usize)>,
}

impl LeafRec {
    /// Whether `event` can begin a line's content and so must be noted; tracks the breaks that
    /// make the next prose event one that does.
    fn wants(&mut self, event: &Event<'_>, range: &Range<usize>) -> bool {
        match self.mode {
            LeafMode::Verbatim => true,
            LeafMode::Table => matches!(
                event,
                Event::Start(pulldown_cmark::Tag::TableHead | pulldown_cmark::Tag::TableRow)
            ),
            LeafMode::Prose => match event {
                Event::SoftBreak | Event::HardBreak => {
                    self.line_pending = true;
                    self.break_end = Some(range.end);
                    false
                }
                Event::Code(_)
                | Event::InlineMath(_)
                | Event::DisplayMath(_)
                | Event::InlineHtml(_)
                | Event::Html(_)
                | Event::Start(pulldown_cmark::Tag::Image { .. }) => {
                    self.take_pending();
                    true
                }
                _ => self.take_pending(),
            },
        }
    }

    /// Whether a line start was pending, clearing it.
    fn take_pending(&mut self) -> bool {
        self.break_end = None;
        std::mem::replace(&mut self.line_pending, false)
    }

    fn continues_atomic(&self, line: usize) -> bool {
        self.atomic
            .is_some_and(|(start, until)| line > start && line <= until)
    }

    /// Whether `line` already has a content column.
    fn has_col(&self, line: usize) -> bool {
        self.first
            .and_then(|first| line.checked_sub(first))
            .and_then(|i| self.cols.get(i))
            .is_some_and(Option::is_some)
    }

    /// Note content at `col` on `line`.  Events arrive in source order, so `line` is never
    /// above `first`; one that were would be dropped.
    fn note(&mut self, line: usize, col: usize) {
        let first = *self.first.get_or_insert(line);
        let Some(i) = line.checked_sub(first) else {
            return;
        };
        let col = to_u32(col);
        if self.cols.len() <= i {
            self.cols.resize(i + 1, None);
        }
        let slot = &mut self.cols[i];
        *slot = Some(slot.map_or(col, |c| c.min(col)));
    }
}

/// A container's `Start`: its range, and the line it opens on, found while the stream's line
/// cursor is still there (see [`EventStream::open_container`]).
pub(super) struct ContainerStart {
    line: usize,
    range: Range<usize>,
}

pub(super) struct EventStream<'s, I>
where
    I: Iterator<Item = (Event<'s>, Range<usize>)>,
{
    inner: I,
    /// One event of lookahead, held apart from its range, so the `(Event, Range)` pair isn't
    /// moved through a `Peekable` slot on every event.
    peeked: Option<Event<'s>>,
    /// Range of the event most recently pulled from `inner`: the peeked one while there is one,
    /// else the one [`next`](Self::next) just returned.
    pulled_range: Range<usize>,
    lines: LineIndex<'s>,
    /// Collects the top-level blocks' byte ranges from the same events.
    ranges: RangeTracker<fn(BlockKind) -> bool>,
    /// The column buffer [`LeafRec::cols`] borrows while a leaf is open.
    spare_cols: Vec<Option<u32>>,
    /// First line of the top-level block being parsed; every line handed out is relative to it.
    base_line: usize,
    leaf: Option<LeafRec>,
}

impl<'s, I> EventStream<'s, I>
where
    I: Iterator<Item = (Event<'s>, Range<usize>)>,
{
    pub(super) fn new(src: &'s str, events: I) -> Self {
        Self {
            inner: events,
            peeked: None,
            pulled_range: 0..0,
            lines: LineIndex::new(src),
            ranges: RangeTracker::new(|_| true),
            spare_cols: Vec::new(),
            base_line: 0,
            leaf: None,
        }
    }

    // ── Events ────────────────────────────────────────────────────────────
    //
    // `pull` / `peek` / `next` are force-inlined: benched out of line (or behind a `Map`
    // closure), every `Event` was copied through several frames on the parser's hottest path.

    /// The next event from pulldown-cmark, shown to the range tracker as it arrives.
    #[inline(always)]
    fn pull(&mut self) -> Option<Event<'s>> {
        let (event, range) = self.inner.next()?;
        self.ranges.observe(self.lines.src, &event, &range);
        self.pulled_range = range;
        Some(event)
    }

    #[inline(always)]
    pub(super) fn peek(&mut self) -> Option<&Event<'s>> {
        if self.peeked.is_none() {
            self.peeked = self.pull();
        }
        self.peeked.as_ref()
    }

    #[inline(always)]
    pub(super) fn next(&mut self) -> Option<Event<'s>> {
        let event = match self.peeked.take() {
            Some(event) => event,
            None => self.pull()?,
        };
        // Only the leaf's own events are noted, and an `End` repeats its `Start`'s range, so it
        // adds nothing (a leaf's own `End` would otherwise note its opening line, a fence, as
        // content) — except one a break left owing a line start.  Checked here so the common
        // case never leaves this hot loop.
        let range = &self.pulled_range;
        let wanted = match event {
            Event::End(_) => {
                let owed = self.leaf.as_ref().and_then(|leaf| leaf.break_end);
                if let Some(break_end) = owed.filter(|&b| b < range.end) {
                    self.record_closing_line(break_end);
                }
                false
            }
            _ => self
                .leaf
                .as_mut()
                .is_some_and(|leaf| leaf.wants(&event, range)),
        };
        if wanted {
            self.record(&event, &self.pulled_range.clone());
        }
        Some(event)
    }

    /// Drain the events the parser left unread, so the tracker sees every one, and hand back
    /// the top-level blocks' byte ranges.
    pub(super) fn into_ranges(mut self) -> Vec<Range<usize>> {
        while self.next().is_some() {}
        self.ranges.into_ranges()
    }

    /// Range of the event [`next`](Self::next) just returned.  Ask before peeking again: a peek
    /// pulls the next event and its range, and only debug builds catch a call after one.
    pub(super) fn last_range(&self) -> Range<usize> {
        debug_assert!(self.peeked.is_none(), "last_range after a peek");
        self.pulled_range.clone()
    }

    /// Source text of the event [`next`](Self::next) just returned.
    pub(super) fn last_text(&self) -> &'s str {
        self.lines.src.get(self.last_range()).unwrap_or("")
    }

    /// Note that the container whose `Start` event [`next`](Self::next) just returned opens
    /// here, for [`container_span`](Self::container_span) to close.
    pub(super) fn open_container(&mut self) -> ContainerStart {
        let range = self.last_range();
        ContainerStart {
            line: self.lines.line_at(range.start),
            range,
        }
    }

    // ── Positions ─────────────────────────────────────────────────────────

    /// Anchor relative lines at the block the next event starts.  Called by the top-level
    /// block loop before each block, so it matches the `RangeTracker`'s range for it.
    pub(super) fn set_base(&mut self) {
        if self.peek().is_some() {
            self.base_line = self.lines.line_at(self.pulled_range.start);
        }
    }

    fn rel(&self, line: usize) -> u32 {
        to_u32(line.saturating_sub(self.base_line))
    }

    /// Start noting positions for a leaf.  `start` is its `Start` event's range when it has
    /// one (it is not itself content: a fence's opening line stays `None`); a tight list
    /// item's paragraph, a bare `Html` event and a rule have none and begin at their first
    /// content event.
    pub(super) fn begin_leaf(&mut self, start: Option<Range<usize>>, mode: LeafMode) {
        let (first, last) = match &start {
            Some(range) => {
                let first = self.lines.line_at(range.start);
                (Some(first), self.lines.last_line(first, range))
            }
            None => (None, 0),
        };
        let mut cols = std::mem::take(&mut self.spare_cols);
        cols.clear();
        self.leaf = Some(LeafRec {
            mode,
            line_pending: true,
            break_end: None,
            first,
            last,
            cols,
            atomic: None,
        });
    }

    /// Stop noting positions and hand back the leaf's [`SrcLines`].
    pub(super) fn end_leaf(&mut self) -> SrcLines {
        let Some(mut leaf) = self.leaf.take() else {
            return SrcLines::default();
        };
        let src = match leaf.first {
            Some(first) => {
                leaf.cols.resize(leaf.last.max(first) - first + 1, None);
                SrcLines::new(self.rel(first), &leaf.cols)
            }
            None => SrcLines::default(),
        };
        self.spare_cols = leaf.cols;
        src
    }

    /// Note `event`'s position against the open leaf, if any.
    ///
    /// Kept out of line so it doesn't bloat [`next`](Self::next), the parser's hottest loop.
    #[inline(never)]
    fn record(&mut self, event: &Event<'s>, range: &Range<usize>) {
        let line = self.lines.line_at(range.start);
        let end_line = self.lines.end_line(line, range);
        // Events arrive left to right, so the first one noted on a line already holds its
        // smallest column: a later one needn't count its own (most of a prose line's events).
        let needs_col = self
            .leaf
            .as_ref()
            .is_some_and(|leaf| !leaf.continues_atomic(line) && !leaf.has_col(line));
        let col = match event {
            _ if !needs_col => None,
            // Indentation pulldown-cmark synthesizes as an empty-range `Text` of spaces, placed
            // *past* the whitespace it stands for: an indented HTML block's leading spaces, or
            // the unconsumed columns of a tab content starts partway into.  The content starts
            // at that whitespace — at the tab itself, since a raw char column can't point inside
            // one.
            Event::Text(t) if range.is_empty() && !t.is_empty() => {
                let before = &self.lines.src.as_bytes()[..range.start];
                let back = if before.last() == Some(&b'\t') {
                    1
                } else {
                    before
                        .iter()
                        .rev()
                        .take(t.len())
                        .take_while(|&&b| b == b' ')
                        .count()
                };
                Some(self.lines.col_of(line, range.start - back))
            }
            _ => Some(self.lines.col_of(line, range.start)),
        };
        let Some(leaf) = self.leaf.as_mut() else {
            return;
        };
        leaf.last = leaf.last.max(end_line);
        if let Some(col) = col {
            leaf.note(line, col);
        }
        match event {
            // Text is a verbatim slice of the source, and pulldown-cmark splits it wherever a
            // container prefix intervenes, so text running onto a later line (a code block's
            // blank line) holds that line from its first column.
            Event::Text(_) | Event::Html(_) => {
                for l in line + 1..=end_line {
                    if !leaf.continues_atomic(l) {
                        leaf.note(l, 0);
                    }
                }
            }
            // A multi-line code span, math span, inline HTML or image spans a container prefix
            // it doesn't hold, and renders on its first line's row: its later lines start no
            // content.  An image's alt can hold a code span of its own, which must not cut the
            // image's run short.
            Event::Code(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::InlineHtml(_)
            | Event::Start(pulldown_cmark::Tag::Image { .. })
                if end_line > line =>
            {
                leaf.atomic = Some(match leaf.atomic {
                    Some((start, until)) if line >= start && line <= until => {
                        (start, until.max(end_line))
                    }
                    _ => (line, end_line),
                });
            }
            _ => {}
        }
    }

    /// Note the line starting at `line_start` byte, which a link's or image's closing `](…)`
    /// begins: its `End` event, which repeats the whole link's range, is the only event there.
    /// Its column is the first char past the container prefix (`>`, spaces, tabs), since a
    /// paragraph line can't begin with `>` itself.
    #[inline(never)]
    fn record_closing_line(&mut self, line_start: usize) {
        let line = self.lines.line_at(line_start);
        let Some(leaf) = self.leaf.as_mut() else {
            return;
        };
        leaf.take_pending();
        if leaf.continues_atomic(line) || leaf.has_col(line) {
            return;
        }
        let col = self
            .lines
            .line_text(line)
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t' | '>'))
            .count();
        leaf.last = leaf.last.max(line);
        leaf.note(line, col);
    }

    /// Whether block-relative `line` holds nothing but container prefix (`>`, spaces, tabs).
    pub(super) fn is_bare_line(&self, line: u32) -> bool {
        self.lines
            .line_text(self.base_line + line as usize)
            .chars()
            .all(|c| matches!(c, '>' | ' ' | '\t' | '\r'))
    }

    /// The span of a container opened at `start`: from its first line through
    /// the later of its children's last line and, when `count_range_end`, the last line its own
    /// range covers in full.  A blockquote counts its range (a trailing bare `>` belongs to
    /// it); a list, item or footnote definition doesn't (their ranges absorb the blank lines
    /// that follow them).
    pub(super) fn container_span(
        &self,
        start: &ContainerStart,
        children_end: Option<u32>,
        count_range_end: bool,
    ) -> LineSpan {
        let first = start.line;
        let rel_first = self.rel(first);
        let mut end = rel_first.saturating_add(1);
        if let Some(children_end) = children_end {
            end = end.max(children_end);
        }
        if count_range_end {
            let mut last = self.lines.last_line(first, &start.range);
            if self.lines.ends_mid_line(&start.range) && last > first {
                last -= 1;
            }
            end = end.max(self.rel(last).saturating_add(1));
        }
        rel_first..end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_index_answers_lines_and_char_columns() {
        let src = "ab\nçd\n\nx";
        let idx = LineIndex::new(src);
        assert_eq!(idx.line_of(0), 0);
        assert_eq!(idx.line_of(2), 0); // the '\n'
        assert_eq!(idx.line_of(3), 1);
        assert_eq!(idx.line_of(7), 2);
        assert_eq!(idx.line_of(8), 3);
        assert_eq!(idx.line_of(99), 3);
        // `ç` is two bytes but one char.
        assert_eq!(idx.col_of(1, 5), 1);
        assert_eq!(idx.col_of(1, 6), 2);
        assert_eq!(idx.col_of(1, 3), 0);
        // A CRLF terminator counts from its `\r`.
        let idx = LineIndex::new("a\r\n\r\nb");
        assert_eq!(idx.col_of(1, 4), 0);
        assert_eq!(idx.col_of(0, 2), 1);
    }

    #[test]
    fn end_line_counts_the_newlines_before_the_last_byte() {
        let idx = LineIndex::new("é\nb\n\nç");
        assert_eq!(idx.end_line(0, &(0..2)), 0); // `é` alone, two bytes
        assert_eq!(idx.end_line(0, &(0..3)), 0); // `é\n`: the newline is still line 0's
        assert_eq!(idx.end_line(0, &(0..6)), 2);
        assert_eq!(idx.end_line(3, &(6..8)), 3); // a trailing multi-byte char
    }

    #[test]
    fn last_line_ignores_trailing_newlines() {
        let idx = LineIndex::new("a\nb\n\n\nc\n");
        assert_eq!(idx.last_line(0, &(0..5)), 1);
        assert_eq!(idx.last_line(1, &(2..2)), 1);
    }
}
