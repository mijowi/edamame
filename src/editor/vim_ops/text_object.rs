//! Vim text-object resolution — `iw aw iW aW`, quote pairs (`i" a" i' a'
//! i\` a\``), and bracket pairs (`i( a( i[ a[ i{ a{`, plus the closing
//! variants and the `b`/`B` aliases).
//!
//! `vim_feed` (input layer) maps the `i`/`a` prefix + the object char to a
//! [`TextObject`]; [`resolve_text_object_range`] (editor layer) turns that
//! into a char range against the buffer.  Every offset is a rope **char**
//! offset, the same space as the rest of `vim_ops`.  All in-scope text
//! objects are charwise, so the range is returned bare (no linewise flag).
//! `None` means the object could not be resolved (cursor not inside a pair,
//! no quote on the line, empty buffer) — the caller then does nothing.  See
//! `docs/vim-implementation-plan.md` §2.4.

use std::ops::Range;

use crate::document::Buffer;
use crate::editor::vim_ops::motion::{class, line_end_offset, Class};

/// A resolved text object, carrying the inner (`i`) / around (`a`) flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObject {
    /// `iw aw iW aW` — the word (or whitespace run) under the cursor;
    /// `big` collapses punctuation into the word class (`W` semantics).
    Word { inner: bool, big: bool },
    /// `i" a" i' a' i\` a\`` — the quoted span on the cursor's line.
    Quote { inner: bool, quote: char },
    /// `i( a( i[ a[ i{ a{` (and `)`/`]`/`}` / `b`/`B`) — the balanced
    /// bracket pair surrounding (or under) the cursor.
    Pair {
        inner: bool,
        open: char,
        close: char,
    },
}

/// Resolve `obj` to the char range it covers, or `None` when it cannot be
/// found.  An *inner* object can legitimately be empty (`di(` on `()`):
/// `start == end`, which the operator layer treats as a no-op delete / an
/// in-place Insert for change.
pub fn resolve_text_object_range(
    obj: TextObject,
    cursor: usize,
    buf: &Buffer,
) -> Option<Range<usize>> {
    match obj {
        TextObject::Word { inner, big } => word_object(buf, cursor, inner, big),
        TextObject::Quote { inner, quote } => quote_object(buf, cursor, inner, quote),
        TextObject::Pair { inner, open, close } => pair_object(buf, cursor, inner, open, close),
    }
}

fn ch(buf: &Buffer, i: usize) -> char {
    buf.rope().char(i)
}

// ── Word objects ────────────────────────────────────────────────────────────

/// `iw`/`aw`/`iW`/`aW`.  The inner object is the run of one character class
/// (word / punctuation / whitespace) under the cursor.  The around object
/// adds the trailing whitespace (or, when there is none, the leading
/// whitespace) for a word run, or the following word for a whitespace run —
/// vim's rules.  A newline is always a hard boundary: a word object never
/// spans lines.
fn word_object(buf: &Buffer, cursor: usize, inner: bool, big: bool) -> Option<Range<usize>> {
    let len = buf.len_chars();
    if len == 0 {
        return None;
    }
    let cursor = cursor.min(len - 1);
    if ch(buf, cursor) == '\n' {
        return None;
    }
    let cls = class(ch(buf, cursor), big);

    let mut start = cursor;
    while start > 0 {
        let p = start - 1;
        if ch(buf, p) == '\n' || class(ch(buf, p), big) != cls {
            break;
        }
        start = p;
    }
    let mut end = cursor + 1;
    while end < len {
        if ch(buf, end) == '\n' || class(ch(buf, end), big) != cls {
            break;
        }
        end += 1;
    }

    if inner {
        return Some(start..end);
    }

    if cls != Class::Blank {
        // Around a word: extend over trailing whitespace; if there is none,
        // extend over leading whitespace instead (vim's `aw`).
        let mut e = end;
        while e < len && ch(buf, e) != '\n' && ch(buf, e).is_whitespace() {
            e += 1;
        }
        if e > end {
            return Some(start..e);
        }
        let mut s = start;
        while s > 0 && ch(buf, s - 1) != '\n' && ch(buf, s - 1).is_whitespace() {
            s -= 1;
        }
        Some(s..end)
    } else {
        // Around whitespace: extend over the following word.
        let mut e = end;
        while e < len && ch(buf, e) != '\n' && class(ch(buf, e), big) != Class::Blank {
            e += 1;
        }
        Some(start..e)
    }
}

// ── Quote objects ────────────────────────────────────────────────────────────

/// `i"`/`a"` (and `'` / `` ` ``).  Quotes on the cursor's line pair up
/// left-to-right; the chosen pair is the first whose closing quote is at or
/// after the cursor (so a cursor between two strings selects the next one).
/// The inner object is the span between the quotes; the around object adds
/// the quotes plus trailing whitespace (or leading whitespace when there is
/// none).  Bounded to the cursor's line.
fn quote_object(buf: &Buffer, cursor: usize, inner: bool, q: char) -> Option<Range<usize>> {
    let line = buf.char_to_line(cursor);
    let lstart = buf.line_to_char(line);
    let lend = line_end_offset(buf, line);
    let quotes: Vec<usize> = (lstart..lend).filter(|&i| ch(buf, i) == q).collect();
    for pair in quotes.chunks(2) {
        if pair.len() < 2 {
            break;
        }
        let (open, close) = (pair[0], pair[1]);
        if cursor <= close {
            if inner {
                return Some(open + 1..close);
            }
            let mut e = close + 1;
            let mut t = e;
            while t < lend && ch(buf, t).is_whitespace() {
                t += 1;
            }
            if t > e {
                e = t;
                return Some(open..e);
            }
            let mut s = open;
            while s > lstart && ch(buf, s - 1).is_whitespace() {
                s -= 1;
            }
            return Some(s..e);
        }
    }
    None
}

// ── Bracket-pair objects ──────────────────────────────────────────────────────

/// `i(`/`a(` (and `[]` / `{}`).  Finds the balanced bracket pair the cursor
/// sits inside (or on); the inner object is the span between the brackets,
/// the around object includes them.  Spans multiple lines.  Returns `None`
/// when the cursor is not within a matched pair.
fn pair_object(
    buf: &Buffer,
    cursor: usize,
    inner: bool,
    open: char,
    close: char,
) -> Option<Range<usize>> {
    let len = buf.len_chars();
    if len == 0 {
        return None;
    }
    let cursor = cursor.min(len - 1);
    let open_pos = find_enclosing_open(buf, cursor, open, close)?;
    let close_pos = find_matching_close(buf, open_pos, open, close)?;
    if inner {
        Some(open_pos + 1..close_pos)
    } else {
        Some(open_pos..close_pos + 1)
    }
}

/// Scan left (and the cursor itself) for the `open` bracket enclosing the
/// cursor, respecting nesting.  A cursor sitting *on* an `open` is its own
/// answer; a cursor on a `close` resolves to that close's matching open.
fn find_enclosing_open(buf: &Buffer, cursor: usize, open: char, close: char) -> Option<usize> {
    if ch(buf, cursor) == open {
        return Some(cursor);
    }
    let mut depth = 0i32;
    let mut i = cursor;
    loop {
        let c = ch(buf, i);
        if c == close && i != cursor {
            depth += 1;
        } else if c == open {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// Scan right from `open_pos` for its matching `close`, respecting nesting.
fn find_matching_close(buf: &Buffer, open_pos: usize, open: char, close: char) -> Option<usize> {
    let len = buf.len_chars();
    let mut depth = 0i32;
    let mut i = open_pos;
    while i < len {
        let c = ch(buf, i);
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    fn word(cursor: usize, inner: bool, big: bool, text: &str) -> Option<Range<usize>> {
        word_object(&buf(text), cursor, inner, big)
    }

    #[test]
    fn inner_word_is_the_run_under_the_cursor() {
        // "foo bar": iw from any char of "foo" → 0..3.
        assert_eq!(word(0, true, false, "foo bar"), Some(0..3));
        assert_eq!(word(2, true, false, "foo bar"), Some(0..3));
        // From a char of "bar" → 4..7.
        assert_eq!(word(5, true, false, "foo bar"), Some(4..7));
    }

    #[test]
    fn inner_word_splits_punctuation() {
        // "foo.bar": iw on 'f' → "foo" (0..3); on '.' → "." (3..4).
        assert_eq!(word(0, true, false, "foo.bar"), Some(0..3));
        assert_eq!(word(3, true, false, "foo.bar"), Some(3..4));
        // iW collapses the whole "foo.bar".
        assert_eq!(word(0, true, true, "foo.bar"), Some(0..7));
    }

    #[test]
    fn around_word_takes_trailing_whitespace() {
        // "foo bar": aw on "foo" → "foo " (0..4).
        assert_eq!(word(1, false, false, "foo bar"), Some(0..4));
    }

    #[test]
    fn around_word_falls_back_to_leading_whitespace() {
        // "foo bar": aw on "bar" (no trailing ws) → " bar" (3..7).
        assert_eq!(word(5, false, false, "foo bar"), Some(3..7));
    }

    #[test]
    fn around_whitespace_takes_following_word() {
        // "foo   bar": aw on a space (offset 4) → "   bar" (3..9).
        assert_eq!(word(4, false, false, "foo   bar"), Some(3..9));
    }

    #[test]
    fn word_object_does_not_cross_newlines() {
        let t = "foo\nbar";
        assert_eq!(word(0, true, false, t), Some(0..3));
        // aw at end of line has no trailing space → no leading space → "foo".
        assert_eq!(word(0, false, false, t), Some(0..3));
    }

    fn quote(cursor: usize, inner: bool, q: char, text: &str) -> Option<Range<usize>> {
        quote_object(&buf(text), cursor, inner, q)
    }

    #[test]
    fn inner_and_around_quote() {
        // 'say "hi" now' — quotes at 4 and 7.
        let t = "say \"hi\" now";
        assert_eq!(quote(5, true, '"', t), Some(5..7)); // inner "hi"
        assert_eq!(quote(5, false, '"', t), Some(4..9)); // around: quotes + trailing space
                                                         // Cursor before the opening quote still selects the pair.
        assert_eq!(quote(0, true, '"', t), Some(5..7));
    }

    #[test]
    fn around_quote_falls_back_to_leading_whitespace() {
        // No trailing whitespace after the close → include the leading space.
        let t = "a \"x\"";
        assert_eq!(quote(3, false, '"', t), Some(1..5));
    }

    #[test]
    fn single_quotes_and_backticks() {
        assert_eq!(quote(1, true, '\'', "'q'"), Some(1..2));
        assert_eq!(quote(1, true, '`', "`q`"), Some(1..2));
    }

    fn pair(cursor: usize, inner: bool, o: char, c: char, text: &str) -> Option<Range<usize>> {
        pair_object(&buf(text), cursor, inner, o, c)
    }

    #[test]
    fn inner_and_around_parens() {
        // "(abc)": inner 1..4, around 0..5, from anywhere inside or on a bracket.
        let t = "(abc)";
        assert_eq!(pair(2, true, '(', ')', t), Some(1..4));
        assert_eq!(pair(2, false, '(', ')', t), Some(0..5));
        assert_eq!(pair(0, true, '(', ')', t), Some(1..4)); // on '('
        assert_eq!(pair(4, true, '(', ')', t), Some(1..4)); // on ')'
    }

    #[test]
    fn nested_parens_pick_the_innermost() {
        // "(a(b)c)": from 'b' (3) the inner pair is 2..4 → inner 3..4.
        let t = "(a(b)c)";
        assert_eq!(pair(3, true, '(', ')', t), Some(3..4));
        assert_eq!(pair(3, false, '(', ')', t), Some(2..5));
        // From 'a' (1) the enclosing pair is the outer one.
        assert_eq!(pair(1, false, '(', ')', t), Some(0..7));
    }

    #[test]
    fn brackets_and_braces() {
        assert_eq!(pair(2, true, '[', ']', "[ab]"), Some(1..3));
        assert_eq!(pair(2, true, '{', '}', "{ab}"), Some(1..3));
    }

    #[test]
    fn empty_inner_pair_is_an_empty_range() {
        // "()" inner → 1..1 (empty, but found).
        assert_eq!(pair(0, true, '(', ')', "()"), Some(1..1));
        assert_eq!(pair(0, false, '(', ')', "()"), Some(0..2));
    }

    #[test]
    fn cursor_outside_any_pair_is_none() {
        assert_eq!(pair(0, true, '(', ')', "abc"), None);
        // After a closed pair, not inside it.
        assert_eq!(pair(3, true, '(', ')', "()x"), None);
    }

    #[test]
    fn pair_spans_lines() {
        let t = "(\nab\n)";
        // '(' at 0, ')' at 5; inner 1..5, around 0..6.
        assert_eq!(pair(2, true, '(', ')', t), Some(1..5));
        assert_eq!(pair(2, false, '(', ')', t), Some(0..6));
    }
}
