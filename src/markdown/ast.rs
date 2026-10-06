use std::ops::Range;

use pulldown_cmark::HeadingLevel;

// ─── Source positions ─────────────────────────────────────────────────────────

/// Where a leaf block's content sits in the source, relative to its top-level block's first line.
///
/// Block-relative rather than absolute so `RenderCache`, which keys on `Block` by value, still
/// hits for a block that moved: two identical blocks hash identically wherever they sit.
/// Recorded by the parser from pulldown-cmark's own offsets, in the one block parse; see
/// `docs/dev/plans/row-provenance.md` §1.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct SrcLines {
    /// Block-relative index of the leaf's first source line.
    pub first: u32,
    /// Per source line of the leaf, the char column where its content starts: past container
    /// prefixes (`> `, list indent), its own marker, and any stripped code indent.
    ///
    /// `None` for a line that is all chrome (a fence, a setext underline, a table's delimiter
    /// row, a metadata delimiter), and for a line that only continues an atomic inline begun on
    /// an earlier line (the tail of a multi-line code span, math span, or inline HTML), which
    /// renders on the row of the line it began on and has no content start of its own.
    ///
    /// Columns are `u32`, not `u16`: a column is bounded by line length, not by terminal
    /// width, and the document is untrusted.  A wider one saturates at `u32::MAX`.
    cols: Cols,
}

/// [`SrcLines`]' columns, stored compactly.  Nearly every leaf starts its content at one column
/// on every line but at most two chrome lines (a fence pair, a setext underline, a table's
/// delimiter row), which [`Cols::Uniform`] holds without a heap allocation, and which hashes in
/// a few words on every render-cache lookup.  Only [`SrcLines::new`] builds one, always in the
/// canonical form, so the derived `Eq` and `Hash` compare content.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Cols {
    /// `len` lines, all at `col` but the (at most two, ascending) `chrome` lines, which are
    /// `None`; an unused `chrome` slot is [`Cols::NO_LINE`].  `col` is 0 when no line has one.
    Uniform {
        col: u32,
        len: u32,
        chrome: [u32; 2],
    },
    /// Any other shape, one entry per line.
    Varied(Box<[Option<u32>]>),
}

impl Cols {
    const NO_LINE: u32 = u32::MAX;
}

impl Default for Cols {
    fn default() -> Self {
        Cols::Uniform {
            col: 0,
            len: 0,
            chrome: [Cols::NO_LINE; 2],
        }
    }
}

impl SrcLines {
    /// A leaf starting on block-relative line `first`, with `cols[k]` the content column of its
    /// `k`th line (see the field docs).
    pub fn new(first: u32, cols: &[Option<u32>]) -> Self {
        let mut col = None;
        let mut chrome = [Cols::NO_LINE; 2];
        let mut n_chrome = 0;
        let mut uniform = true;
        for (k, c) in cols.iter().enumerate() {
            match *c {
                None if n_chrome < 2 => {
                    chrome[n_chrome] = to_u32(k);
                    n_chrome += 1;
                }
                Some(c) if col.is_none_or(|col| col == c) => col = Some(c),
                _ => {
                    uniform = false;
                    break;
                }
            }
        }
        let cols = if uniform {
            Cols::Uniform {
                col: col.unwrap_or(0),
                len: to_u32(cols.len()),
                chrome,
            }
        } else {
            Cols::Varied(cols.into())
        };
        Self { first, cols }
    }

    /// How many source lines the leaf covers.
    pub fn len(&self) -> usize {
        match &self.cols {
            Cols::Uniform { len, .. } => *len as usize,
            Cols::Varied(cols) => cols.len(),
        }
    }

    /// Whether the leaf covers no line at all (a promotion's placeholder).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The content column of the leaf's `k`th line; `None` for a chrome line or past the end.
    pub fn col(&self, k: usize) -> Option<u32> {
        match &self.cols {
            Cols::Uniform { col, len, chrome } => {
                let k = u32::try_from(k).ok().filter(|&k| k < *len)?;
                (!chrome.contains(&k)).then_some(*col)
            }
            Cols::Varied(cols) => cols.get(k).copied().flatten(),
        }
    }

    /// Every line's content column, in order.
    pub fn cols(&self) -> impl ExactSizeIterator<Item = Option<u32>> + '_ {
        (0..self.len()).map(|k| self.col(k))
    }

    /// The block-relative source lines this leaf covers.
    pub fn span(&self) -> Range<u32> {
        self.first..self.first.saturating_add(to_u32(self.len()))
    }
}

/// Reads as the per-line list it stands for, whichever form stores it.
impl std::fmt::Debug for SrcLines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SrcLines")
            .field("first", &self.first)
            .field("content_col", &self.cols().collect::<Vec<_>>())
            .finish()
    }
}

/// Narrow a line, column or cell count to the `u32` positions use, saturating at `u32::MAX`
/// (see [`SrcLines`]).
pub(crate) fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Block-relative source lines a container (`BlockQuote`, `List`, `FootnoteDefinition`) or a
/// [`ListItem`] covers, end exclusive.  The renderer reads the gaps between its children's spans
/// as the source's blank lines.
pub type LineSpan = Range<u32>;

// ─── Block-level nodes ────────────────────────────────────────────────────────

// `CodeBlock` / `BlockQuote` are Markdown terminology, not stuttering.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Block {
    Heading {
        level: HeadingLevel,
        inlines: Vec<Inline>,
        src: SrcLines,
    },
    Paragraph {
        inlines: Vec<Inline>,
        src: SrcLines,
    },
    CodeBlock {
        language: Option<String>,
        content: String,
        fenced: bool,
        src: SrcLines,
    },
    BlockQuote {
        blocks: Vec<Block>,
        span: LineSpan,
        /// Lines of `span` no child covers that aren't bare `>` lines: link reference
        /// definitions, which render nothing.  Usually empty.
        hidden: Vec<u32>,
    },
    List {
        ordered: bool,
        start: Option<u64>,
        items: Vec<ListItem>,
        span: LineSpan,
    },
    /// `src.col(0)` is the column of the rule's first `-`/`*`/`_`.
    HorizontalRule {
        src: SrcLines,
    },
    Table {
        /// Column count (from the GFM table alignment row).
        col_count: usize,
        headers: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
        /// Column widths from a trailing `<!-- tui-columns: [..] -->` comment (stripped from the
        /// AST by the parser). Outer `None` = no comment; inner `None` (`_` in the comment) =
        /// auto-size that column.
        user_widths: Option<Vec<Option<usize>>>,
        src: SrcLines,
    },
    /// Raw HTML, rendered as a plain fenced block.
    Html(String, SrcLines),
    /// An HTML comment promoted out of `Block::Html` by the parser post-pass. Stores the full
    /// source including delimiters (same convention as `Html`, so comment helpers accept either).
    /// Renders zero lines; its bytes are still covered by the `SourceMap`.
    HtmlComment(String, SrcLines),
    /// A paragraph whose sole content is an image, promoted so the renderer can reserve a
    /// multi-row region for the graphics overlay. Mixed paragraphs keep `Inline::Image`
    /// placeholders since graphics can't sit mid-wrap.  `src` is the replaced block's.
    ImageBlock {
        alt: String,
        url: String,
        src: SrcLines,
    },
    /// YAML (`---`) or TOML (`+++`) frontmatter, recognized only where CommonMark's metadata
    /// extension accepts one. `content` is the raw text between the delimiter lines, never
    /// re-flowed; the delimiters are reproduced from `kind` (see `docs/dev/frontmatter.md`).
    MetadataBlock {
        kind: MetadataKind,
        content: String,
        src: SrcLines,
    },
    /// A footnote definition, rendered in place at its source position with the raw `label` as
    /// marker so the rendered number never diverges from the source. Renumbering is the
    /// `RenumberFootnotes` action's job, not the renderer's.
    FootnoteDefinition {
        label: String,
        blocks: Vec<Block>,
        span: LineSpan,
    },
}

impl Block {
    /// A leaf's [`SrcLines`]; `None` for a container.
    pub fn src(&self) -> Option<&SrcLines> {
        match self {
            Block::Heading { src, .. }
            | Block::Paragraph { src, .. }
            | Block::CodeBlock { src, .. }
            | Block::HorizontalRule { src }
            | Block::Table { src, .. }
            | Block::Html(_, src)
            | Block::HtmlComment(_, src)
            | Block::ImageBlock { src, .. }
            | Block::MetadataBlock { src, .. } => Some(src),
            Block::BlockQuote { .. } | Block::List { .. } | Block::FootnoteDefinition { .. } => {
                None
            }
        }
    }

    /// Whether this is a setext heading: its source lines end in an underline, where an ATX
    /// heading is one line.
    pub fn is_setext_heading(&self) -> bool {
        matches!(self, Block::Heading { src, .. } if src.len() >= 2)
    }

    /// The block-relative source lines this block covers, leaf or container.
    pub fn span(&self) -> LineSpan {
        match self {
            Block::BlockQuote { span, .. }
            | Block::List { span, .. }
            | Block::FootnoteDefinition { span, .. } => span.clone(),
            leaf => leaf.src().map(SrcLines::span).unwrap_or_default(),
        }
    }
}

/// Which delimiter style opened a [`Block::MetadataBlock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataKind {
    Yaml,
    Toml,
}

impl MetadataKind {
    pub fn delimiter(self) -> &'static str {
        match self {
            MetadataKind::Yaml => "---",
            MetadataKind::Toml => "+++",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ListItem {
    pub blocks: Vec<Block>,
    /// `Some(true)` = checked, `Some(false)` = unchecked, `None` = not a task item.
    pub task: Option<bool>,
    /// From the marker's line through the item's last content line: never the blank lines
    /// after it, which pulldown-cmark's item range absorbs.  A loose list's spacing is the gap
    /// between one item's span and the next's.
    pub span: LineSpan,
}

// ─── Inline nodes ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Code(String),
    Link {
        text: Vec<Inline>,
        url: String,
        title: Option<String>,
    },
    Image {
        alt: String,
        url: String,
    },
    Highlight(Vec<Inline>),
    /// Mid-paragraph HTML comment, stored with delimiters; renders as zero spans.
    HtmlComment(String),
    /// A footnote reference (`[^label]`). Always has a definition: pulldown-cmark leaves an
    /// undefined `[^x]` as literal text. See `docs/dev/footnotes.md` for marker rendering.
    FootnoteReference {
        label: String,
    },
    /// `$...$` inline or `$$...$$` display math, raw LaTeX source.
    /// Rendered as source-equivalent text in phase 1 (inline beautification
    /// is phase 2); a paragraph holding exactly one `display: true` Math is
    /// promoted to a `Block::ImageBlock` by the post-pass.
    Math {
        source: String,
        display: bool,
    },
    SoftBreak,
    HardBreak,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// `inlines_to_plain` with hard breaks collapsed to spaces, for single-row uses.
pub fn heading_plain_text(inlines: &[Inline]) -> String {
    inlines_to_plain(inlines).replace('\n', " ")
}

/// Flatten inlines to a plain text string (no styling).
pub fn inlines_to_plain(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(t) => out.push_str(t),
            Inline::Bold(inner)
            | Inline::Italic(inner)
            | Inline::Strikethrough(inner)
            | Inline::Highlight(inner) => {
                out.push_str(&inlines_to_plain(inner));
            }
            Inline::Code(c) => out.push_str(c),
            Inline::Link { text, .. } => out.push_str(&inlines_to_plain(text)),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::HtmlComment(_) => {}
            // Footnote markers are chrome, not prose: keep them out of heading slugs.
            Inline::FootnoteReference { .. } => {}
            // Math renders as its source in phase 1 (delimiters included,
            // width-equivalent to the source text).
            Inline::Math { source, display } => {
                let delim = if *display { "$$" } else { "$" };
                out.push_str(delim);
                out.push_str(source);
                out.push_str(delim);
            }
            Inline::SoftBreak => out.push(' '),
            Inline::HardBreak => out.push('\n'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line of each shape, read back through the accessors.
    fn round_trip(cols: &[Option<u32>]) -> SrcLines {
        let src = SrcLines::new(3, cols);
        assert_eq!(src.len(), cols.len());
        assert_eq!(src.cols().collect::<Vec<_>>(), cols);
        assert_eq!(src.col(cols.len()), None, "past the end");
        assert_eq!(src.span(), 3..3 + cols.len() as u32);
        src
    }

    #[test]
    fn common_shapes_store_uniformly() {
        for cols in [
            &[][..],
            &[Some(2)],
            &[Some(0), Some(0), Some(0)],
            &[Some(0), None],                   // setext heading
            &[None, Some(4), Some(4), None],    // fenced code
            &[Some(2), None, Some(2), Some(2)], // table
            &[None],                            // an empty ATX heading
            &[None, None],                      // an empty fence
        ] {
            let src = round_trip(cols);
            assert!(matches!(src.cols, Cols::Uniform { .. }), "{cols:?}");
        }
    }

    #[test]
    fn other_shapes_store_every_line() {
        for cols in [
            &[Some(2), Some(1)][..],
            &[None, None, None],
            &[Some(0), None, None, Some(0), None],
            &[Some(2), Some(70_000)],
        ] {
            let src = round_trip(cols);
            assert!(matches!(src.cols, Cols::Varied(_)), "{cols:?}");
        }
    }

    /// Equal content is an equal value, whichever way it was built, so the render cache's
    /// `Block` keys still compare by content.
    #[test]
    fn equal_columns_build_equal_values() {
        let a = SrcLines::new(0, &[None, Some(1), None]);
        let b = SrcLines::new(0, &a.cols().collect::<Vec<_>>());
        assert_eq!(a, b);
        assert_ne!(a, SrcLines::new(0, &[None, Some(2), None]));
        assert_ne!(a, SrcLines::new(0, &[Some(1), None, None]));
        assert_eq!(SrcLines::new(0, &[]), SrcLines::default());
    }

    /// AST snapshots read the per-line list, not the storage form.
    #[test]
    fn debug_shows_the_per_line_columns() {
        assert_eq!(
            format!("{:?}", SrcLines::new(1, &[None, Some(2)])),
            "SrcLines { first: 1, content_col: [None, Some(2)] }"
        );
    }
}
