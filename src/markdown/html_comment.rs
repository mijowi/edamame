//! HTML comments in raw HTML, by the HTML standard's tokenizer rules: what the parser hides
//! (a block or inline run made only of comments, [`is_comment_only`]) and what the renderer dims
//! (a comment inside other HTML, [`comments`]).  One scanner serves both, so a string hidden as
//! a comment is exactly one the renderer would dim as one.
//!
//! The rules (HTML Living Standard, §13.2.5 "Tokenization"):
//! - `<!--` opens a comment;
//! - `<!-->` and `<!--->` are complete, empty comments (abrupt closing);
//! - otherwise the first `-->` or `--!>` after the opener closes it;
//! - a comment still open at the end of the input runs to the end.
//!
//! Context-free: a `<!--` inside a quoted attribute value or a `<script>` / `<textarea>` body is
//! not a comment to a browser, but is matched here.  Those are rare in Markdown, and the only
//! effect is dimmed text.

use std::ops::Range;

/// One comment found by [`comments`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlComment {
    /// Byte range in the scanned string, delimiters included.
    pub range: Range<usize>,
    /// `false` for a comment still open at the end of the input, which runs to the end.
    pub closed: bool,
}

/// Every comment in `html`, in order and non-overlapping.
pub fn comments(html: &str) -> Vec<HtmlComment> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = html[from..].find("<!--") {
        let open = from + rel;
        let body_start = open + 4;
        let (end, closed) = match close_end(&html[body_start..]) {
            Some(len) => (body_start + len, true),
            None => (html.len(), false),
        };
        out.push(HtmlComment {
            range: open..end,
            closed,
        });
        from = end;
    }
    out
}

/// Whether `body` is one or more closed comments and whitespace, nothing else.  An unclosed
/// comment is `false`: hiding it would hide everything after it, so it stays visible instead.
pub fn is_comment_only(body: &str) -> bool {
    let found = comments(body);
    let mut at = 0usize;
    for c in &found {
        if !c.closed || !body[at..c.range.start].trim().is_empty() {
            return false;
        }
        at = c.range.end;
    }
    !found.is_empty() && body[at..].trim().is_empty()
}

/// The length of a comment's body up to and including its closing delimiter, or `None` when it
/// never closes.  `body` is the text right after `<!--`.
fn close_end(body: &str) -> Option<usize> {
    if body.starts_with('>') {
        return Some(1);
    }
    if body.starts_with("->") {
        return Some(2);
    }
    let mut from = 0usize;
    while let Some(rel) = body[from..].find("--") {
        let dashes = from + rel;
        let after = &body[dashes + 2..];
        if after.starts_with('>') {
            return Some(dashes + 3);
        }
        if after.starts_with("!>") {
            return Some(dashes + 4);
        }
        // One char on, not two, so the second dash of `--->` can start the closer.
        from = dashes + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(html: &str) -> Vec<&str> {
        comments(html).into_iter().map(|c| &html[c.range]).collect()
    }

    #[test]
    fn finds_each_comment_with_its_delimiters() {
        let html = "<div><!-- a --></div>\n<!-- b\nc -->";
        assert_eq!(texts(html), ["<!-- a -->", "<!-- b\nc -->"]);
    }

    #[test]
    fn abrupt_empty_comments_close_immediately() {
        assert_eq!(
            texts("<!--> x <!---> y <!---->"),
            ["<!-->", "<!--->", "<!---->"]
        );
    }

    #[test]
    fn bang_closer_and_extra_dashes_close() {
        assert_eq!(
            texts("<!-- a --!> b <!-- c ---> d"),
            ["<!-- a --!>", "<!-- c --->"]
        );
    }

    #[test]
    fn unclosed_comment_runs_to_the_end() {
        let html = "<p>x</p>\n<!-- never closed\n<p>y</p>";
        assert_eq!(
            comments(html),
            [HtmlComment {
                range: 9..html.len(),
                closed: false
            }]
        );
    }

    #[test]
    fn no_comments_without_an_opener() {
        assert!(comments("<div>a -- b --> c</div>").is_empty());
    }

    #[test]
    fn comment_only_accepts_comments_and_whitespace() {
        for body in [
            "<!-- hi -->",
            "   <!-- hi -->   ",
            "<!---->",
            "<!-->",
            "<!--->",
            "<!-- a --!>",
            "<!-- a --> <!-- b -->",
            "<!-- a --><!-- b -->",
            "<!-- a -->\n\n<!-- b -->\n",
        ] {
            assert!(is_comment_only(body), "{body:?}");
        }
    }

    #[test]
    fn comment_only_rejects_anything_else() {
        for body in [
            "",
            "   ",
            "<div>foo</div>",
            "<!-- a --> tail",
            "lead <!-- a -->",
            "<!-- a",
            "<!-- a --> <!-- b",
            // `<!-->` is a whole comment, so the text after it is not.
            "<!--> note -->",
        ] {
            assert!(!is_comment_only(body), "{body:?}");
        }
    }
}
