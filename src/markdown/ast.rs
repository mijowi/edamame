use pulldown_cmark::HeadingLevel;

// ─── Block-level nodes ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
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
    /// A paragraph whose sole inline content is an image, promoted to a
    /// block so the renderer can reserve a multi-row region for the
    /// terminal-graphics overlay.  Paragraphs with mixed inline content
    /// keep their `Inline::Image` placeholders — terminal graphics can't
    /// sit mid-paragraph without breaking wrap.
    ImageBlock {
        alt: String,
        url: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub blocks: Vec<Block>,
    /// `Some(true)` = checked, `Some(false)` = unchecked, `None` = not a task item.
    pub task: Option<bool>,
}

// ─── Inline nodes ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
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
    SoftBreak,
    HardBreak,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

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
            Inline::SoftBreak => out.push(' '),
            Inline::HardBreak => out.push('\n'),
        }
    }
    out
}
