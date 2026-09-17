//! Link destinations, both directions: collecting the document's local image destinations
//! (where the paste prompt infers a directory from) and escaping one for writing back.

use crate::markdown::ast::{Block, Inline};

/// The destination of every image the document references *beside itself*: relative, without a
/// scheme, and not through `..`, in document order.  Remote, absolute and escaping references are
/// skipped, as are the synthetic URLs of promoted diagrams and anything inside code, HTML or
/// frontmatter — literal text, not references.
pub fn local_image_urls(blocks: &[Block]) -> Vec<&str> {
    let mut out = Vec::new();
    collect_blocks(blocks, &mut out);
    out.retain(|url| is_local_relative(url));
    out
}

fn collect_blocks<'a>(blocks: &'a [Block], out: &mut Vec<&'a str>) {
    for block in blocks {
        match block {
            Block::Heading { inlines, .. } | Block::Paragraph { inlines } => {
                collect_inlines(inlines, out);
            }
            Block::BlockQuote { blocks } | Block::FootnoteDefinition { blocks, .. } => {
                collect_blocks(blocks, out);
            }
            Block::List { items, .. } => {
                for item in items {
                    collect_blocks(&item.blocks, out);
                }
            }
            Block::Table { headers, rows, .. } => {
                for cell in headers.iter().chain(rows.iter().flatten()) {
                    collect_inlines(cell, out);
                }
            }
            Block::ImageBlock { url, .. } if !crate::diagram::is_diagram_url(url) => {
                out.push(url);
            }
            Block::ImageBlock { .. }
            | Block::CodeBlock { .. }
            | Block::HorizontalRule
            | Block::Html(_)
            | Block::HtmlComment(_)
            | Block::MetadataBlock { .. } => {}
        }
    }
}

fn collect_inlines<'a>(inlines: &'a [Inline], out: &mut Vec<&'a str>) {
    for inline in inlines {
        match inline {
            Inline::Image { url, .. } => out.push(url),
            Inline::Bold(inner)
            | Inline::Italic(inner)
            | Inline::Strikethrough(inner)
            | Inline::Highlight(inner)
            | Inline::Link { text: inner, .. } => collect_inlines(inner, out),
            Inline::Text(_)
            | Inline::Code(_)
            | Inline::HtmlComment(_)
            | Inline::FootnoteReference { .. }
            | Inline::Math { .. }
            | Inline::SoftBreak
            | Inline::HardBreak => {}
        }
    }
}

/// Relative, scheme-less, and staying inside the document's directory — the rule
/// [`image::paste`](crate::image::paste) also checks a pasted image's path against.
fn is_local_relative(url: &str) -> bool {
    use crate::image::paste::{climbs_out, is_rooted};
    !url.is_empty() && !url.starts_with('#') && !is_rooted(url) && !climbs_out(url)
}

/// Write `dest` as a CommonMark link destination.  A plain destination may not contain spaces
/// and needs its parentheses balanced, so one containing whitespace, `(`, `)`, `<` or `>` takes
/// the angle-bracket form, inside which `<`, `>` and `\` are backslash-escaped.  Anything else
/// is written as-is — the form a reader expects.
pub fn escape_destination(dest: &str) -> String {
    let needs_brackets = dest
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>'));
    if !needs_brackets {
        return dest.to_owned();
    }
    let mut out = String::with_capacity(dest.len() + 2);
    out.push('<');
    for c in dest.chars() {
        if matches!(c, '<' | '>' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('>');
    out
}

/// [`escape_destination`] for a destination written inside a GFM table cell, where an unescaped
/// `|` would end the cell.  Every `|` becomes `\|`: the table splits its cells before inline
/// parsing and turns `\|` back into `|` first, so this holds in both the plain and the
/// angle-bracket form.  `\|` is an ordinary backslash escape outside a table, so this is also safe
/// where a table *might* be — inside a container whose nested blocks the caller can't see.
pub fn escape_destination_in_table(dest: &str) -> String {
    escape_destination(dest).replace('|', r"\|")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::parse;

    fn urls(src: &str) -> Vec<String> {
        local_image_urls(&parse(src))
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn collects_images_from_every_markup_container() {
        let src = "\
![a](top.png)

Text with ![b](inline.png) inside.

- item ![c](list.png)

> ![d](quote.png)

| h |
|---|
| ![e](cell.png) |

[![f](linked.png)](https://example.com)

Note[^1].

[^1]: ![g](footnote.png)
";
        assert_eq!(
            urls(src),
            [
                "top.png",
                "inline.png",
                "list.png",
                "quote.png",
                "cell.png",
                "linked.png",
                "footnote.png"
            ]
        );
    }

    #[test]
    fn skips_code_html_and_non_local_references() {
        let src = "\
```
![a](code.png)
```

<img src=\"html.png\">

![b](https://example.com/remote.png)

![c](/abs/path.png)

![d](../outside.png)

![e](C:/drive.png)

```mermaid
graph TD; A-->B
```
";
        assert!(urls(src).is_empty(), "{:?}", urls(src));
    }

    #[test]
    fn plain_destinations_are_left_alone() {
        assert_eq!(escape_destination("images/a-b_c.png"), "images/a-b_c.png");
    }

    #[test]
    fn escaped_destinations_round_trip_through_the_parser() {
        for dest in [
            "images/my shot.png",
            "images/image (1).png",
            "images/a)b.png",
            "images/<odd>.png",
            r"images/back\slash.png",
            "images/tab\there.png",
        ] {
            let src = format!("![]({})\n", escape_destination(dest));
            let blocks = parse(&src);
            let url = match blocks.as_slice() {
                [Block::ImageBlock { url, .. }] => url.as_str(),
                other => panic!("{dest:?} did not parse as one image: {other:?}"),
            };
            assert_eq!(url, dest, "source was {src:?}");
        }
    }

    #[test]
    fn table_destinations_round_trip_outside_a_table_too() {
        // The paste escapes `|` in every container, so `\|` must also unescape where no table
        // strips it first.
        for dest in ["images/a|b.png", "images/my shot|1.png"] {
            let src = format!("- ![]({})\n", escape_destination_in_table(dest));
            let blocks = parse(&src);
            let [Block::List { items, .. }] = blocks.as_slice() else {
                panic!("{dest:?} did not parse as one list: {blocks:?}");
            };
            match items[0].blocks.as_slice() {
                [Block::Paragraph { inlines }] => match inlines.as_slice() {
                    [Inline::Image { url, .. }] => assert_eq!(url, dest, "source was {src:?}"),
                    other => panic!("{dest:?} is not one image: {other:?}"),
                },
                other => panic!("{dest:?} is not one paragraph: {other:?}"),
            }
        }
    }

    #[test]
    fn table_destinations_round_trip_through_a_table_cell() {
        for dest in ["images/a.png", "images/a|b.png", "images/my shot|1.png"] {
            let src = format!(
                "| h | i |\n|---|---|\n| ![]({}) | x |\n",
                escape_destination_in_table(dest)
            );
            let blocks = parse(&src);
            let [Block::Table { rows, .. }] = blocks.as_slice() else {
                panic!("{dest:?} did not parse as one table: {blocks:?}");
            };
            assert_eq!(
                rows[0].len(),
                2,
                "the pipe must not split the cell: {src:?}"
            );
            match rows[0][0].as_slice() {
                [Inline::Image { url, .. }] => assert_eq!(url, dest, "source was {src:?}"),
                other => panic!("{dest:?} is not one image in its cell: {other:?}"),
            }
        }
    }
}
