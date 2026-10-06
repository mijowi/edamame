//! A proptest generator of nested Markdown documents: paragraphs (with lazy continuation lines,
//! hard breaks, wide characters, inline math, reference links and footnote references, smart
//! punctuation, entities and escapes, and emphasis, links or images broken across lines),
//! headings (ATX ones with or without a closing sequence), fenced and indented code (mermaid
//! included), display math, raw HTML and
//! comments, rules, tables (some with cells long enough to wrap), footnote definitions,
//! blockquotes (with or without a space after `>`), and tight, loose or task lists with every
//! marker style and start number, nested in one another.  No tabs: pulldown-cmark starts content
//! mid-tab, which a raw char column cannot express, and the unit tests cover that case on its
//! own.
//!
//! Shared by the test targets that check source positions against the rendered output; include
//! it with `#[path = "support/markdown_gen.rs"] mod markdown_gen;`.

#![allow(dead_code)]

use proptest::prelude::*;

/// One generated block.  Rendered to source lines by [`Gen::lines`].
#[derive(Debug, Clone)]
pub enum Gen {
    /// Lines of prose.  `lazy` drops the container prefix from every line after the first,
    /// which CommonMark reads as a lazy continuation of the same paragraph.
    Para {
        lines: Vec<String>,
        lazy: bool,
    },
    Atx {
        level: usize,
        text: String,
        /// Whether the heading ends in a closing sequence (`## text ##`).
        closing: bool,
    },
    Setext {
        text: String,
        h1: bool,
    },
    /// A fenced block; `closed: false` leaves the fence open, as while typing it.
    Fence {
        lang: Option<String>,
        body: Vec<String>,
        closed: bool,
    },
    Indented {
        body: Vec<String>,
    },
    Rule,
    /// `wide` fills every cell with enough words to wrap.
    Table {
        rows: usize,
        wide: bool,
    },
    /// A `$$` … `$$` formula on lines of its own.
    DisplayMath,
    /// A raw HTML block, or a comment.
    Html {
        comment: bool,
    },
    /// `[^label]: text`, with an indented continuation line when `more`.
    Footnote {
        label: String,
        text: String,
        more: bool,
    },
    /// `gaps` puts a bare `>` line between children; `spaced` writes `> ` rather than `>`.
    Quote {
        children: Vec<Gen>,
        gaps: bool,
        spaced: bool,
    },
    /// `loose` puts a blank line between items.
    List {
        marker: Marker,
        items: Vec<Vec<Gen>>,
        loose: bool,
        /// A task box on every item that opens with a paragraph or setext heading, checked or not.
        task: Option<bool>,
    },
}

/// A list's marker style.
#[derive(Debug, Clone, Copy)]
pub enum Marker {
    Bullet(char),
    /// Delimiter (`.` or `)`) and start number.
    Ordered(char, u64),
}

fn word() -> impl Strategy<Value = String> {
    prop_oneof![
        8 => "[a-z]{1,6}",
        2 => "[a-z]{1,4}".prop_map(|w| format!("*{w}*")),
        2 => "[a-z]{1,4}".prop_map(|w| format!("`{w}`")),
        1 => "[日本語中文]{1,3}",
        1 => "[a-z]{1,3}".prop_map(|w| format!("${w}$")),
        // `[r]` is defined at the end of every document; a footnote label is defined only when
        // a footnote block happens to use it.
        1 => "[a-z]{1,3}".prop_map(|w| format!("[{w}][r]")),
        1 => "[a-z]{1,3}".prop_map(|w| format!("[^{w}]")),
        1 => prop::sample::select(vec!["a--b", "c...", "d---e", "&amp;", "x\\*y"])
            .prop_map(str::to_owned),
    ]
}

fn text_line() -> impl Strategy<Value = String> {
    prop::collection::vec(word(), 1..4).prop_map(|ws| ws.join(" "))
}

fn code_line() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-z]{1,6}( [a-z]{1,4})?",
        1 => "  [a-z]{1,4}",
        1 => Just(String::new()),
    ]
}

fn leaf() -> impl Strategy<Value = Gen> {
    prop_oneof![
        4 => (
            prop::collection::vec((text_line(), 0u8..10), 1..4),
            any::<bool>(),
        )
            .prop_map(|(lines, lazy)| Gen::Para {
                lines: wrap_across_breaks(lines),
                lazy,
            }),
        1 => (1usize..4, text_line(), any::<bool>())
            .prop_map(|(level, text, closing)| Gen::Atx { level, text, closing }),
        1 => (text_line(), any::<bool>()).prop_map(|(text, h1)| Gen::Setext { text, h1 }),
        2 => (
            prop::option::of(prop_oneof![4 => "[a-z]{1,4}", 1 => Just("mermaid".to_owned())]),
            prop::collection::vec(code_line(), 0..4),
            prop::bool::weighted(0.85),
        )
            .prop_map(|(lang, body, closed)| Gen::Fence { lang, body, closed }),
        1 => prop::collection::vec("[a-z]{1,6}", 1..3).prop_map(|body| Gen::Indented { body }),
        1 => Just(Gen::Rule),
        1 => (0usize..3, prop::bool::weighted(0.3)).prop_map(|(rows, wide)| Gen::Table { rows, wide }),
        1 => Just(Gen::DisplayMath),
        1 => any::<bool>().prop_map(|comment| Gen::Html { comment }),
        1 => ("[a-z]{1,3}", text_line(), any::<bool>())
            .prop_map(|(label, text, more)| Gen::Footnote { label, text, more }),
    ]
}

fn marker() -> impl Strategy<Value = Marker> {
    prop_oneof![
        2 => prop::sample::select(vec!['-', '*', '+']).prop_map(Marker::Bullet),
        2 => (prop::sample::select(vec!['.', ')']), prop_oneof![3 => Just(1u64), 1 => 0u64..12])
            .prop_map(|(delim, start)| Marker::Ordered(delim, start)),
    ]
}

/// A paragraph's lines, where `pick` sometimes ends a line in a hard break, or opens emphasis,
/// a link or an image at a line's end and closes it at the next line's start: a break nested
/// inside an inline, which doesn't split the paragraph's row segments.
fn wrap_across_breaks(lines: Vec<(String, u8)>) -> Vec<String> {
    let mut out: Vec<String> = lines.iter().map(|(l, _)| l.clone()).collect();
    for k in 0..out.len().saturating_sub(1) {
        let (open, close) = match lines[k].1 {
            0 => ("*x", "y*"),
            1 => ("**x", "y**"),
            2 => ("[x", "y](u)"),
            // The line after the break opens with the link's close.
            3 => ("[x", "](u)"),
            4 => ("![x", "y](u)"),
            5 => {
                out[k].push('\\');
                continue;
            }
            6 => {
                out[k].push_str("  ");
                continue;
            }
            _ => continue,
        };
        out[k] = format!("{} {open}", out[k]);
        out[k + 1] = format!("{close} {}", out[k + 1]);
    }
    out
}

pub fn block() -> impl Strategy<Value = Gen> {
    leaf().prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            (
                prop::collection::vec(inner.clone(), 1..4),
                any::<bool>(),
                prop::bool::weighted(0.8),
            )
                .prop_map(|(children, gaps, spaced)| Gen::Quote {
                    children,
                    gaps,
                    spaced,
                }),
            (
                marker(),
                prop::collection::vec(prop::collection::vec(inner, 1..3), 1..4),
                any::<bool>(),
                prop::option::weighted(0.2, any::<bool>()),
            )
                .prop_map(|(marker, items, loose, task)| Gen::List {
                    marker,
                    items: items
                        .into_iter()
                        .map(|item| item.into_iter().map(without_html).collect())
                        .collect(),
                    loose,
                    task,
                }),
        ]
    })
}

/// `gen` with every raw HTML block in it a rule instead.  A list item can't hold one yet: the
/// parser folds an HTML block that follows a tight item's text into that text (issue #9).
fn without_html(gen: Gen) -> Gen {
    match gen {
        Gen::Html { .. } => Gen::Rule,
        Gen::Quote {
            children,
            gaps,
            spaced,
        } => Gen::Quote {
            children: children.into_iter().map(without_html).collect(),
            gaps,
            spaced,
        },
        Gen::List {
            marker,
            items,
            loose,
            task,
        } => Gen::List {
            marker,
            items: items
                .into_iter()
                .map(|item| item.into_iter().map(without_html).collect())
                .collect(),
            loose,
            task,
        },
        other => other,
    }
}

/// A whole document: blank-separated top-level blocks.
pub fn document() -> impl Strategy<Value = String> {
    prop::collection::vec(block(), 1..5).prop_map(|blocks| {
        let mut out = String::new();
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            for line in b.lines() {
                out.push_str(&line);
                out.push('\n');
            }
        }
        out.push_str("\n[r]: /u\n");
        out
    })
}

impl Gen {
    /// The block's source lines, without a trailing newline on each.
    pub fn lines(&self) -> Vec<String> {
        match self {
            Gen::Para { lines, .. } => lines.clone(),
            Gen::Atx {
                level,
                text,
                closing,
            } => {
                let hashes = "#".repeat(*level);
                let close = if *closing {
                    format!(" {hashes}")
                } else {
                    String::new()
                };
                vec![format!("{hashes} {text}{close}")]
            }
            Gen::Setext { text, h1 } => {
                vec![text.clone(), if *h1 { "===" } else { "---" }.to_owned()]
            }
            Gen::Fence { lang, body, closed } => {
                let mut out = vec![format!("```{}", lang.as_deref().unwrap_or(""))];
                out.extend(body.iter().cloned());
                if *closed {
                    out.push("```".to_owned());
                }
                out
            }
            Gen::Indented { body } => body.iter().map(|l| format!("    {l}")).collect(),
            Gen::Rule => vec!["***".to_owned()],
            Gen::Table { rows, wide } => {
                let cell = |r: usize, c: &str| {
                    if *wide {
                        format!("{c}{r} {}", "word ".repeat(10).trim_end())
                    } else {
                        format!("{c}{r}")
                    }
                };
                let mut out = vec!["| a | b |".to_owned(), "|---|---|".to_owned()];
                out.extend((0..*rows).map(|r| format!("| {} | {} |", cell(r, "y"), cell(r, "x"))));
                out
            }
            Gen::DisplayMath => vec!["$$".to_owned(), "x^2".to_owned(), "$$".to_owned()],
            Gen::Html { comment: true } => vec!["<!-- note -->".to_owned()],
            Gen::Html { comment: false } => {
                vec!["<div>".to_owned(), "x".to_owned(), "</div>".to_owned()]
            }
            Gen::Footnote { label, text, more } => {
                let mut out = vec![format!("[^{label}]: {text}")];
                if *more {
                    out.push(format!("    {text}"));
                }
                out
            }
            Gen::Quote {
                children,
                gaps,
                spaced,
            } => {
                let prefix = if *spaced { "> " } else { ">" };
                let mut out = Vec::new();
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        // A paragraph would swallow the next child's first line without one.
                        out.push(">".to_owned());
                        if *gaps {
                            out.push(">".to_owned());
                        }
                    }
                    out.extend(prefix_lines(child, prefix, ">"));
                }
                out
            }
            Gen::List {
                marker,
                items,
                loose,
                task,
            } => {
                let mut out = Vec::new();
                for (n, item) in items.iter().enumerate() {
                    if n > 0 && *loose {
                        out.push(String::new());
                    }
                    let marker = match *marker {
                        Marker::Bullet(c) => format!("{c} "),
                        Marker::Ordered(delim, start) => format!("{}{delim} ", start + n as u64),
                    };
                    let indent = " ".repeat(marker.len());
                    // A box only before text: before a block it reads as text, which turns an
                    // HTML block into the tight-item case issue #9 tracks.  Before a setext
                    // heading it's the heading's literal text.
                    let boxed = task.filter(|_| {
                        matches!(item.first(), Some(Gen::Para { .. } | Gen::Setext { .. }))
                    });
                    let marker = match boxed {
                        Some(true) => format!("{marker}[x] "),
                        Some(false) => format!("{marker}[ ] "),
                        None => marker,
                    };
                    // `(line, indented)`: a lazy continuation line takes no indent.
                    let mut item_lines: Vec<(String, bool)> = Vec::new();
                    for (i, child) in item.iter().enumerate() {
                        if i > 0 {
                            item_lines.push((String::new(), true));
                        }
                        let lazy = matches!(child, Gen::Para { lazy: true, .. });
                        item_lines.extend(
                            child
                                .lines()
                                .into_iter()
                                .enumerate()
                                .map(|(k, line)| (line, !(lazy && k > 0))),
                        );
                    }
                    for (i, (line, indented)) in item_lines.into_iter().enumerate() {
                        if i == 0 {
                            out.push(format!("{marker}{line}"));
                        } else if !indented || line.is_empty() {
                            out.push(line);
                        } else {
                            out.push(format!("{indent}{line}"));
                        }
                    }
                }
                out
            }
        }
    }
}

/// `child`'s lines under a container prefix: `prefix` before a line with text, `bare` for an
/// empty one, and nothing on a lazy paragraph's continuation lines.
fn prefix_lines(child: &Gen, prefix: &str, bare: &str) -> Vec<String> {
    let lazy = matches!(child, Gen::Para { lazy: true, .. });
    child
        .lines()
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            if lazy && i > 0 {
                line
            } else if line.is_empty() {
                bare.to_owned()
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect()
}
