use pulldown_cmark::HeadingLevel;

// ─── Block-level nodes ────────────────────────────────────────────────────────

// `Block::CodeBlock` and `Block::BlockQuote` are intentional Markdown
// terminology, not stuttering.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Block {
    Heading {
        level: HeadingLevel,
        inlines: Vec<Inline>,
    },
    Paragraph {
        inlines: Vec<Inline>,
    },
    CodeBlock {
        language: Option<String>,
        content: String,
        fenced: bool,
    },
    BlockQuote {
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    HorizontalRule,
    Table {
        /// Column count (from the GFM table alignment row).
        col_count: usize,
        headers: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
        /// Column widths persisted by the user via a trailing
        /// `<!-- tui-columns: [..] -->` HTML comment.  The outer `Option`
        /// distinguishes "no comment present" from "comment with one or
        /// more user-set columns".  Each inner `Option<usize>` is `Some(w)`
        /// for columns the user has pinned to a specific width, and `None`
        /// (represented in the comment as `_`) for columns that should
        /// auto-size.  The parser strips the comment from the AST so the
        /// rendered output never shows it as an HTML block.
        user_widths: Option<Vec<Option<usize>>>,
    },
    /// Raw HTML — rendered as a plain fenced block for now.
    Html(String),
    /// HTML comment (`<!-- ... -->`) promoted out of `Block::Html` by the
    /// parser's post-pass when the block's body is a single comment.  The
    /// stored string is the full source text including the `<!--` / `-->`
    /// delimiters — matching `Block::Html`'s convention — so that helpers
    /// like `parse_column_widths_comment` keep working on either variant.
    /// The renderer emits zero lines for this block; the source bytes are
    /// still covered by the `SourceMap` via the block's recorded byte range,
    /// so navigation and selection over the raw bytes remain well-defined.
    HtmlComment(String),
    /// A paragraph whose sole inline content is an image, promoted to a
    /// block so the renderer can reserve a multi-row region for the
    /// terminal-graphics overlay.  Paragraphs with mixed inline content
    /// keep their `Inline::Image` placeholders — terminal graphics can't
    /// sit mid-paragraph without breaking wrap.
    ImageBlock {
        alt: String,
        url: String,
    },
    /// A document-metadata block — YAML frontmatter delimited by `---`
    /// lines, or TOML frontmatter delimited by `+++` lines.  Recognised
    /// only where CommonMark's metadata-block extensions accept one: the
    /// delimiter run is exactly three characters, the first line is
    /// non-blank, and a closing delimiter exists (otherwise the `---` is
    /// still an ordinary thematic break).
    ///
    /// `content` is the raw text *between* the delimiter lines, newlines
    /// included, exactly as pulldown-cmark emits it — the block is data
    /// the user edits, so it is never re-flowed.  The delimiter lines
    /// themselves are not stored: they are reproduced from `kind`, which
    /// is faithful except for a YAML block closed with `...` or a
    /// delimiter line carrying trailing spaces.  Both keep the same
    /// three-column width, so the raw↔rendered column mapping holds
    /// regardless, and entering the block reveals the true source.
    MetadataBlock {
        kind: MetadataKind,
        content: String,
    },
    /// A footnote definition (`[^label]: body`).  Rendered in place
    /// wherever it appears in the source (pulldown-cmark emits it at its
    /// source position, not reordered to the document end).  The renderer
    /// shows the raw `label` as the definition's leading marker plus a
    /// back-link affordance — the rendered number never diverges from the
    /// source.  Sequencing the *raw* labels is the job of the
    /// `RenumberFootnotes` action, not the renderer.
    FootnoteDefinition {
        label: String,
        blocks: Vec<Block>,
    },
}

/// Which delimiter style opened a [`Block::MetadataBlock`] — `---` for
/// YAML frontmatter (Hugo, Jekyll, Zola, Astro, Obsidian), `+++` for the
/// TOML flavor Hugo and Zola also accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataKind {
    Yaml,
    Toml,
}

impl MetadataKind {
    /// The three-character delimiter line this flavor opens and closes with.
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
    /// Number of blank source lines directly preceding this item's marker
    /// line, outside any fenced code block.  Set by
    /// [`crate::markdown::parser::post_pass::annotate_list_blanks`] from the
    /// list's source range; always `0` for the first item.  The renderer
    /// emits this many blank lines before the item so a loose list keeps its
    /// legibility spacing while staying a single `Block::List` — see the
    /// module docs on `post_pass` for why this replaced the old
    /// split-into-separate-lists approach.
    pub blank_lines_before: usize,
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
    /// Inline HTML comment (`<!-- ... -->`) detected mid-paragraph.  Rendered
    /// as zero spans in Preview/Rendered modes; the surrounding paragraph's
    /// other inlines render normally.  Stored with delimiters included so
    /// callers can round-trip the raw text if needed.
    HtmlComment(String),
    /// An inline footnote reference (`[^label]`).  The renderer shows the
    /// raw `label` in a bracketed marker (`[1]`), fusing a run of adjacent
    /// references into one (`[1,2]`), so the rendered marker never
    /// diverges from the source labels.
    /// pulldown-cmark only emits a reference when a matching definition
    /// exists — an undefined `[^x]` stays literal text, so this variant
    /// always has a definition.
    FootnoteReference {
        label: String,
    },
    SoftBreak,
    HardBreak,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Flatten a heading's inlines to a single-line plain text string —
/// `inlines_to_plain` followed by collapsing hard-break `\n`s to spaces
/// so the result fits on one row (section picker, status-bar
/// breadcrumb).
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
            // Footnote markers are chrome, not prose — omit them from plain
            // text so they don't pollute heading slugs or breadcrumbs.
            Inline::FootnoteReference { .. } => {}
            Inline::SoftBreak => out.push(' '),
            Inline::HardBreak => out.push('\n'),
        }
    }
    out
}
