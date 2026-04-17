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
    },
    /// Raw HTML — rendered as a plain fenced block for now.
    Html(String),
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
