use std::collections::HashSet;

use pulldown_cmark::{BrokenLink, CowStr, Event, Options, Parser, Tag, TagEnd};

use super::parser::TextRun;
use super::renderer::util::link_fallback;
use super::segment_break;

/// The labels a document defines, so a map built over one line of it resolves `[text][label]`
/// and `[^label]` the way the document's own parse did: a line parsed alone has no definitions
/// in scope, so every reference on it would otherwise read as literal brackets.
#[derive(Debug, Clone, Default)]
pub struct RefLabels {
    links: HashSet<String>,
    footnotes: HashSet<String>,
}

impl RefLabels {
    pub fn new<'a>(
        links: impl IntoIterator<Item = &'a str>,
        footnotes: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        Self {
            links: links.into_iter().map(normalize_label).collect(),
            footnotes: footnotes.into_iter().map(normalize_label).collect(),
        }
    }

    fn has_link(&self, label: &str) -> bool {
        self.links.contains(&normalize_label(label))
    }

    fn has_footnote(&self, label: &str) -> bool {
        self.footnotes.contains(&normalize_label(label))
    }
}

/// A label as CommonMark matches it: case-insensitive, whitespace runs collapsed.  (Lowercasing
/// stands in for Unicode case folding; a label differing only in a fold-specific letter such as
/// `ß` misses, and its row just has no exact map.)
fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// An ATX heading's content without its optional closing sequence (`## Title ##` → `Title `):
/// a run of `#` at the end, after a space or tab or making up the whole content, and trailing
/// spaces.  The renderer shows neither, so a heading row's map is built over what this leaves.
pub fn strip_atx_closing(content: &str) -> &str {
    let trimmed = content.trim_end_matches([' ', '\t']);
    let before = trimmed.trim_end_matches('#');
    if before.len() == trimmed.len() {
        return content;
    }
    if before.is_empty() || before.ends_with([' ', '\t']) {
        before
    } else {
        content
    }
}

/// Bidirectional character-column map between raw Markdown source and its rendered
/// (inline-markup-collapsed) form for a single line.
///
/// Built by re-parsing `raw_line` and recording the raw position of every rendered character
/// emitted by inline `Text`, `Code`, and break events.  Marker bytes (asterisks, brackets, backtick
/// delimiters, a link's URL) sit in the gaps between events and are skipped.
#[derive(Debug, Clone)]
pub struct InlineColMap {
    rendered_to_raw: Vec<usize>,
    /// The raw char of each soft or hard break that renders as a space, ascending: where the
    /// rendered char that stands for it maps.  No text char maps there (a break's raw run, `\n`,
    /// `\` or trailing spaces, is never text), so these tell the breaks apart in the forward map.
    breaks: Vec<usize>,
    /// Each break that renders as nothing, and so has no entry in the forward map: a soft one
    /// between CJK glyphs ([`segment_break::vanishes`]), or any inside a link that shows its URL
    /// fallback.  For each, the rendered char it sits before, and its raw char.
    vanished: Vec<(usize, usize)>,
    raw_to_rendered: Vec<usize>,
    rendered_len: usize,
    raw_len: usize,
}

/// A walk's output: each rendered char with its raw char, before any soft break is dropped,
/// and the raw chars of the breaks among them, all of them and the soft ones.  An entry with no
/// char is a break inside a link that shows its URL fallback: it renders nothing, but still ends
/// its line.  `one_paragraph`: whether the text parsed as a single paragraph and nothing else.
struct Walked {
    entries: Vec<(usize, Option<char>)>,
    breaks: Vec<usize>,
    soft: Vec<usize>,
    one_paragraph: bool,
}

impl InlineColMap {
    pub fn build(raw_line: &str) -> Self {
        let raw_len = raw_line.chars().count();
        Self::from_walk(Self::walk(raw_line, None), raw_len)
    }

    /// The rendered chars of `raw_line`, resolving references against `labels` when given.
    /// Without them every `[^label]` collapses to a marker and no `[text][label]` resolves.
    fn walk(raw_line: &str, labels: Option<&RefLabels>) -> Walked {
        let mut walk = CharMapWalk::new(raw_line);

        // A reference the document defines resolves to an empty destination: only its text
        // renders, so the destination never matters.
        let resolve = |link: BrokenLink<'_>| {
            labels
                .filter(|l| l.has_link(&link.reference))
                .map(|_| (CowStr::Borrowed(""), CowStr::Borrowed("")))
        };
        let parser =
            Parser::new_with_broken_link_callback(raw_line, parse_options(), Some(resolve));
        // The open link: where the walk stood at its start, its destination and its range.
        let mut link = None;
        // Text pieces joined as the parser joins them, so `==` pairs the same way: see
        // [`TextRun`].  Every other event ends the run.
        let mut text: Option<TextRun<'_>> = None;
        let mut one_paragraph = OneParagraph::default();
        for (event, range) in parser.into_offset_iter() {
            one_paragraph.see(&event);
            if !matches!(event, Event::Text(_)) {
                if let Some(run) = text.take() {
                    walk.push_text(raw_line, run.range.clone(), run.text());
                }
            }
            match event {
                Event::Start(Tag::Link { dest_url, .. }) => {
                    link = Some((walk.mark(), dest_url, range));
                }
                Event::End(TagEnd::Link) => {
                    if let Some((mark, url, range)) = link.take() {
                        walk.end_link(raw_line, mark, &url, range);
                    }
                }
                Event::Text(piece) => {
                    let raw = raw_line.get(range.clone()).unwrap_or("");
                    if let Some(run) = TextRun::feed(&mut text, piece, range, raw) {
                        walk.push_text(raw_line, run.range.clone(), run.text());
                    }
                }
                Event::Code(s) => walk.push_code(raw_line, &s, range),
                // Math renders as its delimited source.  Parsed as math, not text, so it splits
                // the text around it as the parser does: `y== $a$ ==x` holds no highlight.
                Event::InlineMath(s) => walk.push_math(&s, "$", range),
                Event::DisplayMath(s) => walk.push_math(&s, "$$", range),
                Event::SoftBreak => walk.push_break(range.start, true),
                Event::HardBreak => walk.push_break(range.start, false),
                // The renderer shows inline HTML as its text, except a lone comment.
                Event::InlineHtml(s)
                    if !crate::markdown::parser::post_pass::is_html_comment_only(&s) =>
                {
                    let raw = raw_line.get(range.clone()).unwrap_or("");
                    walk.push_text(raw_line, range, raw)
                }
                // No `FootnoteReference` arm: built per line, there is no definition in scope, so
                // pulldown emits `[^label]` as literal `Text`.  `collapse_footnote_refs` then
                // narrows the defined ones' entries to the renderer's `[label]` marker width.
                _ => {}
            }
        }
        if let Some(run) = text {
            walk.push_text(raw_line, run.range.clone(), run.text());
        }

        let mut entries = walk.map;
        collapse_footnote_refs(raw_line, labels, &mut entries);
        Walked {
            entries,
            breaks: walk.breaks,
            soft: walk.soft,
            one_paragraph: one_paragraph.holds(),
        }
    }

    /// A map over `text` read as inline content *inside* a paragraph: a line's text past its
    /// content column, which on its own could parse as block syntax it wasn't in the document
    /// (`2. a` as a list item, `# b` as a heading, an indent as code).  A leading word pins it to
    /// paragraph text, and its entries are dropped again.  Several lines joined, a continuation
    /// can still read as block syntax (issue #96: `---` ending the paragraph as a setext
    /// underline, though in the document it was the paragraph's text behind a lazy line); only
    /// then is each such line pinned too, past its indent, which a continuation drops either
    /// way.  Only those lines, since a pin inside an inline spanning the break changes it: a
    /// reference label no longer matches its definition, a code span keeps its padding.
    /// References resolve against the document's `labels`.
    pub fn build_inline(text: &str, labels: &RefLabels) -> Self {
        let raw_len = text.chars().count();
        let walked = Self::walk_pinned(text, labels, |_| false);
        let walked = if walked.one_paragraph {
            walked
        } else {
            let lines: Vec<&str> = text.split('\n').collect();
            Self::walk_pinned(text, labels, |i| ends_paragraph(lines[i - 1], lines[i]))
        };
        Self::from_walk(walked, raw_len)
    }

    /// A walk over `text` with a pin in front of its first line, and of each later line `i`
    /// past its indent where `pin_line(i)`, the pins' entries dropped and the rest mapped back to
    /// `text`'s raw chars.
    fn walk_pinned(text: &str, labels: &RefLabels, pin_line: impl Fn(usize) -> bool) -> Walked {
        let mut pinned = String::with_capacity(text.len() + PIN.len());
        // Each char of `pinned`, then its end: the raw char of `text` it is, `None` in a pin.
        let mut raws = Vec::with_capacity(text.len() + PIN.len() + 1);
        let mut at = 0usize;
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                pinned.push('\n');
                raws.push(Some(at));
                at += 1;
            }
            let pin = i == 0 || pin_line(i);
            let indent = if i > 0 && pin {
                line.len() - line.trim_start_matches([' ', '\t']).len()
            } else {
                0
            };
            let (lead, rest) = line.split_at(indent);
            pinned.push_str(lead);
            raws.extend((at..at + indent).map(Some));
            at += indent;
            if pin {
                pinned.push_str(PIN);
                raws.extend(PIN.chars().map(|_| None));
            }
            pinned.push_str(rest);
            let len = rest.chars().count();
            raws.extend((at..at + len).map(Some));
            at += len;
        }
        raws.push(Some(at));
        let walked = Self::walk(&pinned, Some(labels));
        let unpin = |raws_in: Vec<usize>| raws_in.into_iter().filter_map(|r| raws[r]).collect();
        Walked {
            entries: walked
                .entries
                .into_iter()
                .filter_map(|(raw, ch)| raws[raw].map(|raw| (raw, ch)))
                .collect(),
            breaks: unpin(walked.breaks),
            soft: unpin(walked.soft),
            one_paragraph: walked.one_paragraph,
        }
    }

    /// The map from a walk over `raw_len` raw chars, less the breaks that render as nothing: those
    /// inside a link showing its URL fallback, and the soft ones between two rendered chars
    /// [`segment_break::vanishes`] drops a break between, as the renderer does when a paragraph
    /// reflows (`renderer::collapse_soft_breaks`).  A break beside another break sits beside a
    /// space.
    fn from_walk(walked: Walked, raw_len: usize) -> Self {
        let Walked {
            entries,
            mut breaks,
            soft,
            ..
        } = walked;
        let chars: Vec<char> = entries.iter().filter_map(|&(_, c)| c).collect();
        let mut rendered_to_raw = Vec::with_capacity(entries.len() + 1);
        let mut vanished = Vec::new();
        // Index into `chars` of the next entry with a char.
        let mut k = 0usize;
        for &(raw, ch) in &entries {
            if ch.is_none() {
                vanished.push((rendered_to_raw.len(), raw));
                continue;
            }
            let neighbors = k
                .checked_sub(1)
                .and_then(|p| chars.get(p))
                .zip(chars.get(k + 1));
            k += 1;
            let vanishes = soft.binary_search(&raw).is_ok()
                && neighbors
                    .is_some_and(|(&before, &after)| segment_break::vanishes(before, after));
            if vanishes {
                vanished.push((rendered_to_raw.len(), raw));
            } else {
                rendered_to_raw.push(raw);
            }
        }
        // Ascending in raw char, as the entries are.
        breaks.retain(|&raw| vanished.binary_search_by_key(&raw, |&(_, v)| v).is_err());
        rendered_to_raw.push(raw_len);
        let rendered_len = rendered_to_raw.len() - 1;

        // Inverse map: raw char idx -> rendered char idx.
        let mut raw_to_rendered = vec![usize::MAX; raw_len + 1];

        for (rendered_idx, &raw_char_idx) in rendered_to_raw.iter().enumerate() {
            if raw_char_idx <= raw_len && raw_to_rendered[raw_char_idx] == usize::MAX {
                raw_to_rendered[raw_char_idx] = rendered_idx;
            }
        }

        raw_to_rendered[raw_len] = rendered_len;

        // Backward-fill: a marker byte takes the rendered index of the next visible character.
        for i in (0..raw_len).rev() {
            if raw_to_rendered[i] == usize::MAX {
                raw_to_rendered[i] = raw_to_rendered[i + 1];
            }
        }

        Self {
            rendered_to_raw,
            breaks,
            vanished,
            raw_to_rendered,
            rendered_len,
            raw_len,
        }
    }

    /// Raw char index for a rendered char column.  Clamps to `raw_len`.
    #[cfg(test)]
    pub fn rendered_to_raw(&self, rendered_char: usize) -> usize {
        let idx = rendered_char.min(self.rendered_len);
        self.rendered_to_raw[idx]
    }

    /// Rendered char index for a raw char column.  A `raw_char` landing on a marker byte yields
    /// the rendered index immediately after the marker.
    pub fn raw_to_rendered(&self, raw_char: usize) -> usize {
        let idx = raw_char.min(self.raw_len);
        self.raw_to_rendered[idx]
    }

    /// Same as [`Self::raw_to_rendered`], but `None` when `rendered_len` disagrees with
    /// `actual_rendered_count` — headings, blockquotes, and list markers add rendered glyphs the
    /// walker can't see, and the caller then falls back to a 1:1 mapping.
    pub fn raw_to_rendered_checked(
        &self,
        raw_char: usize,
        actual_rendered_count: usize,
    ) -> Option<usize> {
        if self.rendered_len != actual_rendered_count {
            return None;
        }
        Some(self.raw_to_rendered(raw_char))
    }

    pub fn rendered_len(&self) -> usize {
        self.rendered_len
    }

    pub fn raw_len(&self) -> usize {
        self.raw_len
    }

    /// Direct access to the forward map, for callers needing the whole vector.
    pub fn rendered_to_raw_vec(&self) -> &[usize] {
        &self.rendered_to_raw
    }

    /// Whether rendered char `rendered_char` stands for a soft or hard break.
    pub fn is_break(&self, rendered_char: usize) -> bool {
        self.rendered_to_raw
            .get(rendered_char)
            .is_some_and(|raw| self.breaks.binary_search(raw).is_ok())
    }

    /// The breaks that render as nothing (see the field), ascending: for each, the rendered char
    /// it sits before and its own raw char.
    pub fn vanished_breaks(&self) -> &[(usize, usize)] {
        &self.vanished
    }
}

// ── Walker ──────────────────────────────────────────────────────────────────

struct CharMapWalk {
    byte_to_char: Vec<usize>,
    total_chars: usize,
    /// Each rendered char, with its raw char; see [`Walked::entries`].
    map: Vec<(usize, Option<char>)>,
    /// The raw char of each break pushed, and of each soft one.
    breaks: Vec<usize>,
    soft: Vec<usize>,
}

impl CharMapWalk {
    fn new(raw_line: &str) -> Self {
        let mut byte_to_char = vec![0usize; raw_line.len() + 1];
        let mut char_idx = 0usize;
        for (byte_idx, _) in raw_line.char_indices() {
            byte_to_char[byte_idx] = char_idx;
            char_idx += 1;
        }
        byte_to_char[raw_line.len()] = char_idx;
        Self {
            byte_to_char,
            total_chars: char_idx,
            map: Vec::new(),
            breaks: Vec::new(),
            soft: Vec::new(),
        }
    }

    /// Where the walk stands, for [`end_link`](Self::end_link) to roll back to.
    fn mark(&self) -> (usize, usize, usize) {
        (self.map.len(), self.breaks.len(), self.soft.len())
    }

    /// The end of the link read from `raw_line[range]`, the walk at `mark` when it started.  A
    /// link whose text rendered blank shows its URL fallback instead, as the renderer does
    /// (`renderer::util::link_fallback`): its chars map onto the destination's, where it shows
    /// past the `](`, else all onto the link's first char (an escaped or reference destination).
    /// The breaks in its text render nothing, but each still ends its line, so each stays as an
    /// entry with no char, before the fallback: the renderer shows the fallback on the link's last
    /// line, where its destination is, when its lines are rows of their own
    /// (`renderer::split_at_breaks`).
    fn end_link(
        &mut self,
        raw_line: &str,
        mark: (usize, usize, usize),
        url: &str,
        range: std::ops::Range<usize>,
    ) {
        let (map_len, breaks_len, soft_len) = mark;
        if !self.map[map_len..]
            .iter()
            .all(|&(_, c)| c.is_none_or(char::is_whitespace))
        {
            return;
        }
        self.map.truncate(map_len);
        self.map
            .extend(self.breaks.drain(breaks_len..).map(|raw| (raw, None)));
        self.soft.truncate(soft_len);
        let fallback = link_fallback(url);
        let raw = raw_line.get(range.clone()).unwrap_or("");
        let dest = raw.find("](").map(|open| open + 2);
        match dest.and_then(|d| raw[d..].find(fallback.as_str()).map(|at| d + at)) {
            Some(at) => {
                self.push_chars(&fallback, range.start + at);
            }
            None => {
                let first = self.lookup(range.start);
                self.map.extend(fallback.chars().map(|c| (first, Some(c))));
            }
        }
    }

    fn lookup(&self, byte: usize) -> usize {
        self.byte_to_char
            .get(byte.min(self.byte_to_char.len().saturating_sub(1)))
            .copied()
            .unwrap_or(self.total_chars)
    }

    fn push_chars(&mut self, text: &str, mut byte: usize) -> usize {
        for c in text.chars() {
            self.map.push((self.lookup(byte), Some(c)));
            byte += c.len_utf8();
        }
        byte
    }

    /// A text run `text` that pulldown-cmark read from `raw_line[range]`, walked in lockstep with
    /// the raw slice.  The two differ where smart punctuation collapsed a run (`...` → `…`,
    /// `--` → `–`, `---` → `—`): each glyph maps to its run's first char, and the rest of the run
    /// is skipped like a marker.  A `==highlight==` pair's markers, which pulldown-cmark leaves
    /// in the text and the renderer drops, are skipped on both sides.
    fn push_text(&mut self, raw_line: &str, range: std::ops::Range<usize>, text: &str) {
        let slice_end = range.end.min(raw_line.len());
        let raw_slice = raw_line.get(range.start..slice_end).unwrap_or("");
        let mut marks = highlight_marks(raw_slice).into_iter().peekable();
        let mut at = 0usize;
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            if marks.next_if_eq(&at).is_some() {
                // `ch` is the marker's first `=`; drop its second too.
                chars.next();
                at += 2;
                continue;
            }
            let rest = raw_slice.get(at..).unwrap_or("");
            let run = match ch {
                '…' if rest.starts_with("...") => 3,
                '—' if rest.starts_with("---") => 3,
                '–' if rest.starts_with("--") => 2,
                _ => rest.chars().next().map_or(0, char::len_utf8),
            };
            self.map.push((self.lookup(range.start + at), Some(ch)));
            at += run;
        }
    }

    fn push_code(&mut self, raw_line: &str, inner: &str, range: std::ops::Range<usize>) {
        // `range` spans the backtick delimiters too, which are skipped like other markers.
        // pulldown also strips one space from each end when both ends are spaces
        // (`` ` x ` `` → "x") — detect that to keep each content char on its true raw position.
        // A line break counts as a space, as pulldown turns it into one before stripping.
        let slice_end = range.end.min(raw_line.len());
        let slice = &raw_line[range.start..slice_end];
        let delim = slice.bytes().take_while(|&b| b == b'`').count();
        let body_end = slice.len().saturating_sub(delim).max(delim);
        let body = &slice[delim..body_end];
        let ends = [' ', '\n'];
        let stripped = body != inner
            && body
                .strip_prefix(ends)
                .and_then(|b| b.strip_suffix(ends))
                .is_some_and(|b| b.replace('\n', " ") == inner);
        let content_start = range.start + delim + usize::from(stripped);
        self.push_chars(inner, content_start);
    }

    /// A math span read from `range`, rendered as `delim`, its `source`, then `delim` again: the
    /// delimiters onto the raw ones at the range's ends, the source onto the raw chars between.
    fn push_math(&mut self, source: &str, delim: &str, range: std::ops::Range<usize>) {
        self.push_chars(delim, range.start);
        self.push_chars(source, range.start + delim.len());
        self.push_chars(delim, range.end.saturating_sub(delim.len()));
    }

    /// A break, rendered as a space unless [`InlineColMap::from_walk`] drops a `soft` one.
    fn push_break(&mut self, byte: usize, soft: bool) {
        let raw = self.lookup(byte);
        self.map.push((raw, Some(' ')));
        self.breaks.push(raw);
        if soft {
            self.soft.push(raw);
        }
    }
}

/// The pin [`InlineColMap::build_inline`] puts in front of a line to keep it paragraph text.
const PIN: &str = "a ";

/// The extensions the document is parsed with, as far as they change what a line renders.
fn parse_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION
        | Options::ENABLE_MATH
}

/// Whether a parse, fed its events in order, is a single paragraph and nothing else.
#[derive(Default)]
struct OneParagraph {
    paragraphs: usize,
    in_paragraph: bool,
    outside: bool,
}

impl OneParagraph {
    fn see(&mut self, event: &Event<'_>) {
        match event {
            Event::Start(Tag::Paragraph) => {
                self.paragraphs += 1;
                self.in_paragraph = true;
            }
            Event::End(TagEnd::Paragraph) => self.in_paragraph = false,
            _ => self.outside |= !self.in_paragraph,
        }
    }

    fn holds(&self) -> bool {
        self.paragraphs <= 1 && !self.outside
    }
}

/// Whether `line`, following `prev` in a paragraph, reads as block syntax that ends it: a setext
/// underline, a list item, a fence, a table's delimiter row under `prev`.  Block structure is
/// read line by line, before any inline, so the two lines alone decide it.
fn ends_paragraph(prev: &str, line: &str) -> bool {
    let mut check = OneParagraph::default();
    Parser::new_ext(&format!("{PIN}{prev}\n{line}"), parse_options()).for_each(|e| check.see(&e));
    !check.holds()
}

/// Byte offsets in `raw` of each `==` that opens or closes a highlight: pairs, each opener
/// taking the next `==` past it as its closer; an unpaired `==` is literal text.
fn highlight_marks(raw: &str) -> Vec<usize> {
    let mut marks = Vec::new();
    let mut from = 0usize;
    while let Some(open) = raw[from..].find("==").map(|i| from + i) {
        let Some(close) = raw[open + 2..].find("==").map(|i| open + 2 + i) else {
            break;
        };
        marks.extend([open, close]);
        from = close + 2;
    }
    marks
}

// ── Footnote-reference collapse ───────────────────────────────────────────────

/// Collapse every literal `[^label]` in `raw_line` down to the renderer's `[label]` marker, in
/// place on the walk's rendered chars.
///
/// The literal form maps 1:1 and the renderer emits the same characters *minus the `^`*, so simply
/// dropping the `^`'s entry leaves every survivor pointing at the correct raw char and shrinks the
/// rendered count to match `renderer::reference_marker`.
///
/// Adjacent references fuse (`[^1][^2]` → `[1,2]`, one marker), so each abutting reference also
/// loses its `[`.  The entry surviving at that position is the previous reference's `]`, which is
/// what the rendered comma points at.
fn collapse_footnote_refs(
    raw_line: &str,
    labels: Option<&RefLabels>,
    entries: &mut Vec<(usize, Option<char>)>,
) {
    let dropped = footnote_collapse_char_indices(raw_line, labels);
    if dropped.is_empty() {
        return;
    }
    entries.retain(|(raw_char, _)| !dropped.contains(raw_char));
}

/// Char indices the renderer's marker drops, for every `[^label]` on `raw_line`: each `^`, plus
/// the `[` of each reference abutting the previous one.  Definition leaders (`[^label]:`) collapse
/// the same way, mirroring `footnote_edit::scan`'s recognition rule.
///
/// With `labels`, only a reference the document defines collapses; an undefined `[^x]` renders
/// literally, as pulldown leaves it.  Without them the scan is definition-blind, so on a line
/// holding an undefined reference the map runs short, `rendered_len` disagrees with the renderer,
/// and `raw_to_rendered_checked` declines so the caller falls back to 1:1 — degraded precision,
/// never a panic.  `undefined_reference_falls_back_to_1_1` pins that fallback.
fn footnote_collapse_char_indices(raw_line: &str, labels: Option<&RefLabels>) -> Vec<usize> {
    let bytes = raw_line.as_bytes();
    let mut dropped = Vec::new();
    // Byte just past the previous reference's `]`, so an abutting `[` is found by equality.
    let mut prev_end: Option<usize> = None;
    let mut char_idx = 0usize; // char index of byte `i`
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'[' && bytes.get(i + 1) == Some(&b'^') {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] != b']' && bytes[j] != b'\n' {
                j += 1;
            }
            let defined = || labels.is_none_or(|l| l.has_footnote(&raw_line[i + 2..j]));
            if j < bytes.len() && bytes[j] == b']' && j > i + 2 && defined() {
                dropped.push(char_idx + 1);
                if prev_end == Some(i) {
                    dropped.push(char_idx);
                }
                prev_end = Some(j + 1);
            }
        }
        i += 1;
        // Advance the char counter only on UTF-8 char boundaries.
        if raw_line.is_char_boundary(i) {
            char_idx += 1;
        }
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slice read on its own as block syntax (`2. a`, `# b`, an indent) maps as the paragraph
    /// text it was in the document.
    #[test]
    fn build_inline_reads_a_slice_as_paragraph_text() {
        for (text, rendered) in [
            ("2. a", 4),
            ("# b *c*", 5),
            ("    x", 5),
            ("> q", 3),
            ("", 0),
        ] {
            let map = InlineColMap::build_inline(text, &RefLabels::default());
            assert_eq!(map.rendered_len(), rendered, "{text:?}");
            assert_eq!(map.raw_len(), text.chars().count(), "{text:?}");
        }
        let map = InlineColMap::build_inline("# b *c*", &RefLabels::default());
        assert_eq!(map.rendered_to_raw(4), 5, "the `c` past its `*`");
        assert_eq!(map.raw_to_rendered(0), 0);
    }

    /// Joined, a continuation line reading as block syntax (`---`, `- a`) stays paragraph text,
    /// as it was in the document (issue #96), and an inline spanning the break still pairs.
    #[test]
    fn build_inline_pins_a_continuation_reading_as_block_syntax() {
        let labels = RefLabels::default();
        let text = "a *x\ny* a\n  ---\n- b";
        let map = InlineColMap::build_inline(text, &labels);
        // a ␠ x ␠ y ␠ a ␠ — ␠ - ␠ b
        assert_eq!(map.rendered_len(), 13);
        assert_eq!(map.raw_len(), text.chars().count());
        assert_eq!(map.rendered_to_raw(2), 3, "the `x` past its `*`");
        assert_eq!(
            map.rendered_to_raw(8),
            12,
            "the `—` on its run, past the indent"
        );
        assert_eq!(map.rendered_to_raw(12), 18, "the `b` after `- `");
        assert!(map.is_break(7) && map.is_break(9));
    }

    /// Only the line that would end the paragraph takes a pin: a code span closing on the line
    /// before it keeps the padding its break gave it.
    #[test]
    fn build_inline_pins_only_the_lines_that_end_the_paragraph() {
        let labels = RefLabels::default();
        let map = InlineColMap::build_inline("x ` foo\n` y\n---", &labels);
        // x ␠ f o o ␠ y ␠ —
        assert_eq!(map.rendered_len(), 9);
        assert_eq!(map.rendered_to_raw(2), 4, "the `f` past the padding");
        assert_eq!(map.rendered_to_raw(6), 10, "the `y`");
        assert_eq!(map.rendered_to_raw(8), 12, "the `—` on its run");
        assert!(map.is_break(7));
    }

    /// A continuation that stays paragraph text joined takes no pin, so an inline spanning into
    /// it is the document's own: a reference label still matches its definition, and a code
    /// span still strips its padding.
    #[test]
    fn build_inline_leaves_an_inline_spanning_a_break_intact() {
        let labels = RefLabels::new(["foo *bar*"], []);
        let map = InlineColMap::build_inline("x [foo\n*bar*] y", &labels);
        // x ␠ f o o ␠ b a r ␠ y
        assert_eq!(map.rendered_len(), 11);
        assert_eq!(map.rendered_to_raw(6), 8, "the `b` past its `*`");

        let map = InlineColMap::build_inline("x ` foo\n` y", &labels);
        // x ␠ f o o ␠ y
        assert_eq!(map.rendered_len(), 7);
        assert_eq!(map.rendered_to_raw(2), 4, "the `f` past the padding");
    }

    /// A reference the document defines renders as its text, as in the document; an undefined
    /// one stays literal.  Labels match case-insensitively, whitespace collapsed.
    #[test]
    fn build_inline_resolves_the_documents_references() {
        let labels = RefLabels::new(["Ref  One"], ["n"]);
        for (text, rendered) in [
            ("see [it][ref one] now", 10),
            ("see [ref one] now", 15),
            ("see [it][nope] now", 18),
            ("x[^N] y", 6),
            ("x[^z] y", 7),
        ] {
            let map = InlineColMap::build_inline(text, &labels);
            assert_eq!(map.rendered_len(), rendered, "{text:?}");
        }
        let map = InlineColMap::build_inline("see [it][ref one] now", &labels);
        assert_eq!(map.rendered_to_raw(4), 5, "the `i` past the `[`");
        assert_eq!(map.rendered_to_raw(7), 18, "the `n` past the label");
    }

    /// A soft break between two CJK glyphs renders as nothing (issue #79), so it has no rendered
    /// char: it is reported as vanished, before the char after it, and its raw newline maps to
    /// that char.  Beside Latin text it is a space, as is a hard break.
    #[test]
    fn a_soft_break_between_cjk_glyphs_vanishes_from_the_map() {
        let labels = RefLabels::default();
        let map = InlineColMap::build_inline("日本\n*語*\nabc", &labels);
        // 日 本 語 ␠ a b c: the break after `本` vanishes, the one after `語` stays.
        assert_eq!(map.rendered_len(), 7);
        assert_eq!(map.vanished_breaks(), [(2, 2)]);
        assert_eq!(map.rendered_to_raw(2), 4, "`語` past its `*`");
        assert!(map.is_break(3) && !map.is_break(2));
        assert_eq!(
            map.raw_to_rendered(2),
            2,
            "the vanished newline maps to `語`"
        );

        let map = InlineColMap::build_inline("日本\\\n語", &labels);
        assert_eq!(map.rendered_len(), 4, "a hard break stays");
        assert!(map.vanished_breaks().is_empty());
        let map = InlineColMap::build_inline("한국\n어", &labels);
        assert_eq!(map.rendered_len(), 4, "Hangul keeps its space");
    }

    #[test]
    fn strip_atx_closing_drops_only_a_closing_sequence() {
        for (content, stripped) in [
            ("Title ##", "Title "),
            ("Title #  ", "Title "),
            ("Title\t#", "Title\t"),
            ("##", ""),
            ("C#", "C#"),
            ("foo \\#", "foo \\#"),
            ("Title", "Title"),
            ("Title  ", "Title  "),
        ] {
            assert_eq!(strip_atx_closing(content), stripped, "{content:?}");
        }
    }

    /// Inline HTML renders as its text, so it maps like text; a lone comment renders nothing.
    #[test]
    fn inline_html_maps_like_text_and_a_comment_like_nothing() {
        let map = InlineColMap::build("a <b>x</b>");
        assert_eq!(map.rendered_len(), 10);
        assert_eq!(map.rendered_to_raw(2), 2);
        let map = InlineColMap::build("a <!-- c --> x");
        assert_eq!(map.rendered_len(), 4);
    }

    #[test]
    fn plain_text_maps_one_to_one() {
        let map = InlineColMap::build("hello world");
        assert_eq!(map.rendered_len(), 11);
        assert_eq!(map.raw_len(), 11);
        for i in 0..=11 {
            assert_eq!(map.rendered_to_raw(i), i);
            assert_eq!(map.raw_to_rendered(i), i);
        }
    }

    #[test]
    fn bold_text_skips_markers() {
        let map = InlineColMap::build("**Bold text**");
        assert_eq!(map.rendered_len(), 9);
        assert_eq!(map.raw_len(), 13);
        assert_eq!(map.rendered_to_raw(0), 2); // B
        assert_eq!(map.rendered_to_raw(8), 10); // t
        assert_eq!(map.rendered_to_raw(9), 13); // sentinel
        assert_eq!(map.raw_to_rendered(0), 0); // marker `*` forward-fills to the first content char
        assert_eq!(map.raw_to_rendered(1), 0);
        assert_eq!(map.raw_to_rendered(2), 0); // B
        assert_eq!(map.raw_to_rendered(10), 8); // t
        assert_eq!(map.raw_to_rendered(11), 9); // closing *
        assert_eq!(map.raw_to_rendered(12), 9);
        assert_eq!(map.raw_to_rendered(13), 9); // past end
    }

    #[test]
    fn italic_text_skips_markers() {
        let map = InlineColMap::build("*Italic*");
        assert_eq!(map.rendered_len(), 6);
        assert_eq!(map.rendered_to_raw(0), 1);
        assert_eq!(map.rendered_to_raw(5), 6);
    }

    #[test]
    fn underscore_emphasis_skips_markers() {
        let map = InlineColMap::build("_under_");
        assert_eq!(map.rendered_len(), 5);
        assert_eq!(map.rendered_to_raw(0), 1);
    }

    #[test]
    fn strikethrough_skips_markers() {
        let map = InlineColMap::build("~~strike~~");
        assert_eq!(map.rendered_len(), 6);
        assert_eq!(map.rendered_to_raw(0), 2);
        assert_eq!(map.rendered_to_raw(5), 7);
    }

    #[test]
    fn highlight_skips_markers() {
        let map = InlineColMap::build("alpha ==beta== gamma");
        // Rendered: "alpha beta gamma"
        assert_eq!(map.rendered_len(), 16);
        assert_eq!(&map.rendered_to_raw_vec()[..6], &[0, 1, 2, 3, 4, 5]);
        assert_eq!(&map.rendered_to_raw_vec()[6..10], &[8, 9, 10, 11]);
        assert_eq!(
            &map.rendered_to_raw_vec()[10..16],
            &[14, 15, 16, 17, 18, 19]
        );
        assert_eq!(map.rendered_to_raw_vec()[16], 20);
    }

    /// Math splits the text around it, as in the parser, so a `==` on each side is literal, not
    /// a highlight; the math itself renders as its delimited source, one char for one.
    #[test]
    fn math_splits_the_text_around_it() {
        for text in ["y== $a$ ==x", "y== $$a$$ ==x"] {
            let map = InlineColMap::build(text);
            let identity: Vec<usize> = (0..=text.len()).collect();
            assert_eq!(map.rendered_to_raw_vec(), identity, "{text:?}");
        }
    }

    #[test]
    fn nested_bold_italic() {
        let map = InlineColMap::build("**_Bold and italic_**");
        assert_eq!(map.rendered_len(), 15);
        assert_eq!(map.rendered_to_raw(0), 3);
    }

    #[test]
    fn code_span_skips_backticks() {
        let map = InlineColMap::build("`code`");
        assert_eq!(map.rendered_len(), 4);
        assert_eq!(map.rendered_to_raw(0), 1);
        assert_eq!(map.rendered_to_raw(3), 4);

        // Backticks forward-fill like other markers.
        assert_eq!(map.raw_to_rendered(0), 0);
        assert_eq!(map.raw_to_rendered(5), 4);
    }

    #[test]
    fn double_backtick_code_span_skips_both_delimiters() {
        let map = InlineColMap::build("``a`b``");
        // Rendered: "a`b".
        assert_eq!(map.rendered_len(), 3);
        assert_eq!(map.rendered_to_raw(0), 2);
        assert_eq!(map.rendered_to_raw(1), 3); // inner `
        assert_eq!(map.rendered_to_raw(2), 4);
    }

    #[test]
    fn code_span_with_stripped_spaces_stays_aligned() {
        // pulldown strips one space from each end: `` ` x ` `` → "x".
        let map = InlineColMap::build("` x `");
        assert_eq!(map.rendered_len(), 1);
        assert_eq!(map.rendered_to_raw(0), 2);
    }

    #[test]
    fn code_span_round_trips() {
        let map = InlineColMap::build("see `fn main()` here");
        for rendered_col in 0..map.rendered_len() {
            let raw_col = map.rendered_to_raw(rendered_col);
            assert_eq!(
                map.raw_to_rendered(raw_col),
                rendered_col,
                "round-trip failed at rendered col {rendered_col}"
            );
        }
    }

    #[test]
    fn link_collapses_url() {
        let map = InlineColMap::build("[File link](./plan.md)");
        assert_eq!(map.rendered_len(), 9);
        assert_eq!(
            &map.rendered_to_raw_vec()[..9],
            &[1, 2, 3, 4, 5, 6, 7, 8, 9]
        );
        assert_eq!(map.rendered_to_raw_vec()[9], 22); // sentinel
    }

    /// A link with blank text renders its URL fallback (the URL, or a path's file name), so its
    /// map does too, onto the destination's chars and not a title's.
    #[test]
    fn blank_link_text_maps_its_url_fallback() {
        let map = InlineColMap::build("a [](u) b");
        assert_eq!(map.rendered_len(), 5, "a ␠ u ␠ b");
        assert_eq!(map.rendered_to_raw(2), 5, "`u` onto the destination");
        let map = InlineColMap::build("[ ](./dir/plan.md \"plan.md\")");
        assert_eq!(
            map.rendered_len(),
            7,
            "the file name, the blank text dropped"
        );
        assert_eq!(
            map.rendered_to_raw(0),
            10,
            "`p` of the destination's file name"
        );
        let map = InlineColMap::build("[](a\\_b)");
        assert_eq!(
            map.rendered_len(),
            3,
            "an escaped destination renders unescaped"
        );
        assert!(
            (0..3).all(|k| map.rendered_to_raw(k) == 0),
            "onto the link's `[`"
        );
    }

    /// The breaks in a link that shows its URL fallback render nothing, but each still ends its
    /// line: reported as vanished, before the fallback, so a map of the lines can cut there.
    #[test]
    fn a_blank_links_breaks_vanish_before_its_fallback() {
        // 日 本 ␠ h t t p s : / / x ␠ a b c; the link's own `\n` is raw char 4.
        let map = InlineColMap::build("日本\n[\n](https://x)\nabc");
        assert_eq!(map.rendered_len(), 16);
        assert_eq!(map.vanished_breaks(), [(3, 4)]);
        assert!(map.is_break(2) && map.is_break(12) && !map.is_break(3));
        assert_eq!(map.rendered_to_raw(3), 7, "`h` onto the destination");
    }

    #[test]
    fn link_round_trip() {
        let map = InlineColMap::build("[File link](./plan.md)");
        for rendered_col in 0..9 {
            let raw_col = map.rendered_to_raw(rendered_col);
            let back = map.raw_to_rendered(raw_col);
            assert_eq!(
                back, rendered_col,
                "round-trip failed at rendered col {rendered_col}"
            );
        }
    }

    #[test]
    fn highlight_round_trip() {
        let map = InlineColMap::build("alpha ==beta== gamma");
        for rendered_col in 0..map.rendered_len() {
            let raw_col = map.rendered_to_raw(rendered_col);
            let back = map.raw_to_rendered(raw_col);
            assert_eq!(
                back, rendered_col,
                "round-trip failed at rendered col {rendered_col}"
            );
        }
    }

    /// A highlight whose text holds smart punctuation: its markers are skipped as the parser
    /// pairs them, across the glyph that splits pulldown-cmark's text.
    #[test]
    fn highlight_across_smart_punctuation() {
        // Renders "it’s x": six chars, the `’` one char for its one-byte `'`.
        let map = InlineColMap::build("==it's== x");
        assert_eq!(map.rendered_len(), 6);
        assert_eq!(map.rendered_to_raw(0), 2, "the `i` past the opening marker");
        assert_eq!(map.rendered_to_raw(2), 4, "the `’` on its `'`");
        assert_eq!(
            map.rendered_to_raw(4),
            8,
            "the space past the closing marker"
        );
    }

    /// A dash run of four or more hyphens is one piece of several glyphs, each on its hyphens.
    #[test]
    fn highlight_across_dash_run() {
        // Renders "a –– b".
        let map = InlineColMap::build("==a ---- b==");
        assert_eq!(map.rendered_len(), 6);
        assert_eq!(map.rendered_to_raw(2), 4, "the first `–` on its `--`");
        assert_eq!(map.rendered_to_raw(3), 6, "the second `–` on its `--`");
        assert_eq!(
            map.rendered_to_raw(5),
            9,
            "the `b` before the closing marker"
        );
    }

    #[test]
    fn heading_well_formedness_mismatch() {
        // The walker sees only "Heading" (7 chars); the renderer's styled prefix makes the real
        // line longer, so the checked lookup declines.
        let map = InlineColMap::build("## Heading");
        assert_eq!(map.rendered_len(), 7);
        assert_eq!(map.raw_to_rendered_checked(5, 9), None);
        assert!(map.raw_to_rendered(5) <= map.rendered_len());
    }

    #[test]
    fn blockquote_well_formedness_mismatch() {
        let map = InlineColMap::build("> blockquoted text");
        // The rendered line carries an extra "▎ " prefix.
        assert_eq!(map.raw_to_rendered_checked(3, 18), None);
    }

    #[test]
    fn marker_byte_maps_to_next_visible() {
        let map = InlineColMap::build("[link](url)");
        assert_eq!(map.raw_to_rendered(0), 0); // `[` → the `l` after it
        assert_eq!(map.raw_to_rendered(5), 4);
    }

    #[test]
    fn list_prefix_backward_fills() {
        let map = InlineColMap::build("- **bold** item");
        // pulldown never emits Text for the "- " list marker, so those raw cols backward-fill.
        assert_eq!(map.raw_to_rendered(0), 0);
        assert_eq!(map.raw_to_rendered(1), 0);
        assert_eq!(map.raw_to_rendered(4), 0);
    }

    /// A multi-char smart-punctuation substitution (`...` → `…`) renders one glyph for its run, so
    /// the counts agree with the renderer's.
    #[test]
    fn multi_char_smart_punct_maps_each_glyph_to_its_run() {
        for (raw, rendered, glyph_raw) in [
            ("hello...", 6, 5), // "hello…"
            ("a---b", 3, 1),    // "a—b"
            ("a--b", 3, 1),     // "a–b"
        ] {
            let map = InlineColMap::build(raw);
            assert_eq!(map.rendered_len(), rendered, "{raw:?}");
            assert_eq!(
                map.raw_to_rendered(glyph_raw),
                glyph_raw,
                "{raw:?}: the run's first char"
            );
            // The rest of the run is skipped like a marker.
            assert_eq!(map.raw_to_rendered(glyph_raw + 1), glyph_raw + 1, "{raw:?}");
        }
        let map = InlineColMap::build("a--b");
        assert_eq!(map.rendered_to_raw(2), 3, "the `b` past the dash");
    }

    /// Smart punctuation and `==` in one text run: a lone `==` stays literal, a pair's markers
    /// render nothing, and the run still collapses either way.
    #[test]
    fn smart_punct_collapses_beside_highlight_markers() {
        // "x == y…"
        let map = InlineColMap::build("x == y...");
        assert_eq!(map.rendered_len(), 7);
        assert_eq!(map.rendered_to_raw(6), 6, "the `…` on its run's first `.`");
        // "a hi b…"
        let map = InlineColMap::build("a ==hi== b...");
        assert_eq!(map.rendered_len(), 7);
        assert_eq!(map.rendered_to_raw(2), 4, "the `h` past the opening marker");
        assert_eq!(map.rendered_to_raw(5), 9, "the `b` past the closing marker");
        assert_eq!(map.rendered_to_raw(6), 10);
    }

    /// Curly quotes substitute one char for one, so counts agree and `checked()` accepts the line.
    #[test]
    fn curly_quote_substitution_round_trips() {
        let map = InlineColMap::build("\"hi\"");
        assert_eq!(map.rendered_len(), 4);
        assert_eq!(map.raw_len(), 4);
        for rendered_col in 0..map.rendered_len() {
            let raw_col = map.rendered_to_raw(rendered_col);
            assert_eq!(map.raw_to_rendered(raw_col), rendered_col);
        }
        assert_eq!(map.raw_to_rendered_checked(0, 4), Some(0));
    }

    /// Both maps must index by char, not byte, or round-trip drifts after the first non-ASCII.
    #[test]
    fn unicode_text_round_trip() {
        let map = InlineColMap::build("café résumé");
        assert_eq!(map.rendered_len(), map.raw_len());
        for rendered_col in 0..map.rendered_len() {
            let raw_col = map.rendered_to_raw(rendered_col);
            assert_eq!(
                map.raw_to_rendered(raw_col),
                rendered_col,
                "round-trip failed at rendered col {rendered_col}",
            );
        }
    }

    /// Only the past-end sentinel exists: guards the inverse-map fill against underflow and
    /// zero-length indexing.
    #[test]
    fn empty_line() {
        let map = InlineColMap::build("");
        assert_eq!(map.rendered_len(), 0);
        assert_eq!(map.raw_len(), 0);
        assert_eq!(map.raw_to_rendered(0), 0);
        assert_eq!(map.raw_to_rendered_checked(0, 0), Some(0));
        assert_eq!(map.raw_to_rendered_checked(0, 2), None);
    }

    /// A query past `raw_len` must clamp to the sentinel rather than index out of bounds; real
    /// callers already pass `end_raw_col == raw_len`.
    #[test]
    fn raw_to_rendered_clamps_past_end() {
        let map = InlineColMap::build("hi");
        assert_eq!(map.raw_to_rendered(2), 2);
        assert_eq!(map.raw_to_rendered(999), 2);
        assert_eq!(map.raw_to_rendered_checked(999, 2), Some(2));
    }

    /// Pins the caller contract: any count mismatch, not just a large one, must decline.
    #[test]
    fn checked_accepts_plain_rejects_prefix_mismatch() {
        let map = InlineColMap::build("plain text");
        assert_eq!(map.raw_to_rendered_checked(0, 10), Some(0));
        assert_eq!(map.raw_to_rendered_checked(5, 10), Some(5));
        assert_eq!(map.raw_to_rendered_checked(0, 12), None);
        assert_eq!(map.raw_to_rendered_checked(0, 11), None);
        assert_eq!(map.raw_to_rendered_checked(0, 9), None);
    }

    #[test]
    fn footnote_reference_line_matches_renderer_and_projects_exactly() {
        // The collapse makes `rendered_len` match the renderer, so projection stays exact.
        let map = InlineColMap::build("see[^1] here");
        let actual_rendered = "see[1] here".chars().count(); // 11
        assert_eq!(map.rendered_len(), actual_rendered);
        assert_eq!(map.raw_to_rendered_checked(0, actual_rendered), Some(0));

        // Raw "see[^1] here": s0 e1 e2 [3 ^4 15 ]6 ' '7 h8 …
        // Rendered "see[1] here": s0 e1 e2 [3 14 ]5 ' '6 h7 …
        assert_eq!(map.raw_to_rendered(3), 3);
        assert_eq!(map.raw_to_rendered(5), 4);
        assert_eq!(map.raw_to_rendered(8), 7);
        // The skipped `^` (raw 4) forward-fills to the digit's rendered idx.
        assert_eq!(map.raw_to_rendered(4), 4);
    }

    #[test]
    fn footnote_reference_round_trips() {
        let map = InlineColMap::build("a[^12]b"); // renders as "a[12]b"
        assert_eq!(map.rendered_len(), 6);
        for rendered_col in 0..map.rendered_len() {
            let raw_col = map.rendered_to_raw(rendered_col);
            assert_eq!(
                map.raw_to_rendered(raw_col),
                rendered_col,
                "round-trip failed at rendered col {rendered_col}",
            );
        }
    }

    #[test]
    fn named_footnote_reference_collapses_the_caret_only() {
        // Only the `^` collapses: `[^note]` → `[note]`.
        let map = InlineColMap::build("x[^note]y");
        assert_eq!(map.rendered_len(), "x[note]y".chars().count());
    }

    /// Adjacent references fuse into one marker, so the collapse must also drop the second `[`;
    /// otherwise every selection past an abutting pair projects one column short.
    #[test]
    fn adjacent_footnote_references_collapse_into_one_marker() {
        let map = InlineColMap::build("Two.[^1][^2] more");
        let actual_rendered = "Two.[1,2] more".chars().count();
        assert_eq!(map.rendered_len(), actual_rendered);
        assert_eq!(map.raw_to_rendered_checked(0, actual_rendered), Some(0));

        // Raw "Two.[^1][^2] more": T0 w1 o2 .3 [4 ^5 16 ]7 [8 ^9 2:10 ]11 ' '12 m13 …
        // Rendered "Two.[1,2] more":   T0 w1 o2 .3 [4 15 ,6 27 ]8 ' '9 m10 …
        assert_eq!(map.raw_to_rendered(4), 4); // opening `[`
        assert_eq!(map.raw_to_rendered(6), 5); // first label
        assert_eq!(map.raw_to_rendered(7), 6); // first `]` → the comma
        assert_eq!(map.raw_to_rendered(10), 7); // second label
        assert_eq!(map.raw_to_rendered(11), 8); // second `]` → closing `]`
        assert_eq!(map.raw_to_rendered(13), 10); // text after the marker
        assert_eq!(map.raw_to_rendered(8), 7); // the fused-away `[` fills onto the second label

        for rendered_col in 0..map.rendered_len() {
            let raw_col = map.rendered_to_raw(rendered_col);
            assert_eq!(
                map.raw_to_rendered(raw_col),
                rendered_col,
                "round-trip failed at rendered col {rendered_col}",
            );
        }
    }

    /// A space keeps the references separate, so neither `[` collapses.
    #[test]
    fn spaced_footnote_references_stay_separate() {
        let map = InlineColMap::build("Two.[^1] [^2]");
        assert_eq!(map.rendered_len(), "Two.[1] [2]".chars().count());
    }

    /// A run of three fuses into one `[1,2,3]` marker — two `[` drops, not one.
    #[test]
    fn three_adjacent_footnote_references_fuse() {
        let map = InlineColMap::build("x[^1][^2][^3]y");
        assert_eq!(map.rendered_len(), "x[1,2,3]y".chars().count());
    }

    /// The definition-blind scan collapses an undefined reference the renderer prints verbatim, so
    /// the map runs short.  That is safe only because the length check catches it and the caller
    /// falls back to 1:1 — pinned so a later change can't promote the mismatch into an
    /// exact-looking projection.
    #[test]
    fn undefined_reference_falls_back_to_1_1() {
        // `[^1]` defined, `[^2]` not: the renderer emits `Two.[1][^2] more`.
        let map = InlineColMap::build("Two.[^1][^2] more");
        let actual_rendered = "Two.[1][^2] more".chars().count();
        assert_ne!(
            map.rendered_len(),
            actual_rendered,
            "the definition-blind scan is expected to disagree here",
        );
        assert_eq!(
            map.raw_to_rendered_checked(0, actual_rendered),
            None,
            "the length check must decline so the caller falls back to 1:1",
        );
    }

    #[test]
    fn mixed_formatting() {
        let raw = "**bold** *italic* `code` [link](url)";
        let map = InlineColMap::build(raw);
        for rendered_col in 0..map.rendered_len() {
            let raw_col = map.rendered_to_raw(rendered_col);
            let back = map.raw_to_rendered(raw_col);
            assert_eq!(
                back, rendered_col,
                "round-trip failed at rendered col {rendered_col} (raw {raw_col})"
            );
        }
    }
}
