//! Vim motion resolution — pure char-offset arithmetic over the rope.
//!
//! `vim_feed` maps a key to a [`Motion`]; [`resolve_motion`] turns that
//! into a target char offset against the buffer.  Every offset here is a
//! rope **char** offset (the same space as `Cursor::offset` and
//! `Selection`) — vim introduces no byte offsets.  See
//! `docs/vim-implementation-plan.md` §2.4.
//!
//! CP2 ships the core motions (`w e b W E B 0 ^ $ gg G`).  CP3 adds the
//! count-aware [`resolve_motion_range`] operator entry point (plus the
//! `h`/`l` charwise targets `Left`/`Right`).  Character-find / paragraph /
//! search / matching-pair motions land in later checkpoints.

use std::ops::Range;

use crate::document::{next_grapheme_offset, Buffer};

/// A resolved Normal-mode motion.  The variants present so far are the
/// CP2 core set plus the CP3 charwise `h`/`l` targets and the `cw`/`cW`
/// current-word-end targets; later checkpoints extend the enum (`f/F/t/T`,
/// `{ }`, `n N`, `%`, `NG`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,              // h (operator target)
    Right,             // l (operator target)
    WordForward,       // w
    WordEnd,           // e
    WordBackward,      // b
    CurrentWordEnd,    // cw target (end of the word the cursor is *in*)
    CurrentBigWordEnd, // cW target
    BigWordForward,    // W
    BigWordEnd,        // E
    BigWordBackward,   // B
    LineStart,         // 0
    LineFirstNonBlank, // ^
    LineEnd,           // $
    DocStart,          // gg
    DocEnd,            // G
}

/// The span an operator (`d`/`c`/`y`) should act on, resolved from a
/// motion or a doubled operator.  Charwise spans carry an explicit
/// char-offset range; linewise spans carry inclusive buffer-line indices
/// so the operator layer can compute the full-line content range, the
/// delete range (which may consume a trailing/leading newline), and the
/// register text without re-deriving line boundaries from char offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpRange {
    /// `[start, end)` char offsets — `e`/`$`/`w`/… etc.
    Chars(Range<usize>),
    /// Inclusive buffer-line indices — `dd`/`dj`/`dgg`/`dG`/… .
    Lines { first: usize, last: usize },
}

/// Character class for word motions.  `w`/`e`/`b` distinguish runs of
/// "word" characters (alphanumeric + `_`) from runs of punctuation;
/// `W`/`E`/`B` treat every non-blank character as a single class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Blank,
    Word,
    Punct,
}

fn class(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn char_at(buf: &Buffer, offset: usize) -> char {
    buf.rope().char(offset)
}

/// Resolve `motion` to a target char offset, starting from `cursor`.
/// `count` is honoured for the word motions; CP2 callers pass `1`
/// (count-driven repeats arrive in CP3).
pub fn resolve_motion(motion: Motion, count: u32, cursor: usize, buf: &Buffer) -> usize {
    let count = count.max(1);
    match motion {
        Motion::Left => step_horizontal(buf, cursor, count, false),
        Motion::Right => step_horizontal(buf, cursor, count, true),
        Motion::WordForward => word_forward(buf, cursor, count, false),
        Motion::WordEnd => word_end(buf, cursor, count, false),
        Motion::WordBackward => word_backward(buf, cursor, count, false),
        Motion::CurrentWordEnd => current_word_end(buf, cursor, count, false),
        Motion::CurrentBigWordEnd => current_word_end(buf, cursor, count, true),
        Motion::BigWordForward => word_forward(buf, cursor, count, true),
        Motion::BigWordEnd => word_end(buf, cursor, count, true),
        Motion::BigWordBackward => word_backward(buf, cursor, count, true),
        Motion::LineStart => line_start(buf, cursor),
        Motion::LineFirstNonBlank => first_non_blank(buf, buf.char_to_line(cursor)),
        Motion::LineEnd => line_end(buf, cursor),
        Motion::DocStart => first_non_blank(buf, 0),
        Motion::DocEnd => first_non_blank(buf, last_content_line(buf)),
    }
}

// ── Operator ranges ───────────────────────────────────────────────────────────

/// Resolve a motion into the [`OpRange`] an operator should act on, given
/// the effective `count`.  Charwise motions become a `[start, end)` char
/// range (end-inclusive for `e`/`E`, end-exclusive otherwise); `gg`/`G`
/// become linewise spans.  A forward word motion (`w`/`W`) that would
/// spill onto a later line is clamped to the end of the cursor's line —
/// vim's rule that `dw` on the last word of a line never joins lines.
pub fn resolve_motion_range(motion: Motion, count: u32, cursor: usize, buf: &Buffer) -> OpRange {
    match motion {
        Motion::DocStart => OpRange::Lines {
            first: 0,
            last: buf.char_to_line(cursor),
        },
        Motion::DocEnd => OpRange::Lines {
            first: buf.char_to_line(cursor),
            last: last_content_line(buf),
        },
        _ => {
            let mut target = resolve_motion(motion, count, cursor, buf);
            if matches!(motion, Motion::WordForward | Motion::BigWordForward)
                && buf.char_to_line(target) > buf.char_to_line(cursor)
            {
                target = line_end(buf, cursor);
            }
            let lo = cursor.min(target);
            let mut hi = cursor.max(target);
            // `e`/`E` (and the `cw`/`cW` current-word-end targets) land
            // *on* the last char of the word; include it.
            if matches!(
                motion,
                Motion::WordEnd
                    | Motion::BigWordEnd
                    | Motion::CurrentWordEnd
                    | Motion::CurrentBigWordEnd
            ) {
                hi = next_grapheme_offset(buf, hi).min(buf.len_chars());
            }
            OpRange::Chars(lo..hi)
        }
    }
}

/// Linewise span for a vertical operator target (`dj` / `dk`): `count`
/// lines below (or above) the cursor's line, clamped to the document.
pub fn vertical_line_range(buf: &Buffer, cursor: usize, count: u32, down: bool) -> OpRange {
    let line = buf.char_to_line(cursor);
    let last = buf.line_count().saturating_sub(1);
    let step = count.max(1) as usize;
    let (first, last_line) = if down {
        (line, (line + step).min(last))
    } else {
        (line.saturating_sub(step), line)
    };
    OpRange::Lines {
        first,
        last: last_line,
    }
}

/// Linewise span for a doubled operator (`dd` / `yy` / `cc`): `count`
/// lines starting at the cursor's line, clamped to the document.
pub fn doubled_line_range(buf: &Buffer, cursor: usize, count: u32) -> OpRange {
    let line = buf.char_to_line(cursor);
    let last = buf.line_count().saturating_sub(1);
    let span = count.max(1) as usize;
    OpRange::Lines {
        first: line,
        last: (line + span - 1).min(last),
    }
}

// ── Word motions ────────────────────────────────────────────────────────────

/// Step `count` graphemes left (`false`) or right (`true`), clamped to the
/// cursor's line (vim `h`/`l` never cross a line boundary).
fn step_horizontal(buf: &Buffer, cursor: usize, count: u32, forward: bool) -> usize {
    let mut pos = cursor;
    if forward {
        let bound = line_end(buf, cursor);
        for _ in 0..count {
            if pos >= bound {
                break;
            }
            pos = next_grapheme_offset(buf, pos);
        }
    } else {
        let bound = line_start(buf, cursor);
        for _ in 0..count {
            if pos <= bound {
                break;
            }
            pos = crate::document::prev_grapheme_offset(buf, pos);
        }
    }
    pos
}

/// `w` / `W`: move to the start of the next word.
fn word_forward(buf: &Buffer, mut pos: usize, count: u32, big: bool) -> usize {
    let len = buf.len_chars();
    for _ in 0..count {
        if pos >= len {
            break;
        }
        // Skip the rest of the current word-class run, then skip blanks.
        let start = class(char_at(buf, pos), big);
        if start != Class::Blank {
            while pos < len && class(char_at(buf, pos), big) == start {
                pos += 1;
            }
        }
        while pos < len && class(char_at(buf, pos), big) == Class::Blank {
            pos += 1;
        }
    }
    pos
}

/// `e` / `E`: move to the end (last char) of the next word.
fn word_end(buf: &Buffer, mut pos: usize, count: u32, big: bool) -> usize {
    let len = buf.len_chars();
    if len == 0 {
        return 0;
    }
    for _ in 0..count {
        if pos + 1 >= len {
            return len.saturating_sub(1);
        }
        // Always advance at least one, then skip blanks to enter the word.
        pos += 1;
        while pos < len && class(char_at(buf, pos), big) == Class::Blank {
            pos += 1;
        }
        if pos >= len {
            return len.saturating_sub(1);
        }
        // Walk to the last char of this word-class run.
        let cls = class(char_at(buf, pos), big);
        while pos + 1 < len && class(char_at(buf, pos + 1), big) == cls {
            pos += 1;
        }
    }
    pos
}

/// `cw` / `cW` target: the last char of the *current* word-class run,
/// then `count - 1` further `e` steps.  Unlike `e`, it does **not** skip
/// the current word when the cursor already sits on its last char — vim's
/// rule that `cw` changes only up to the end of the word the cursor is in
/// (so `cw` on a single-char word, or on a word's final char, changes just
/// that word rather than running into the next one).  When the cursor is
/// on a blank `change_word_to_word_end` never routes here, so the blank
/// case below is defensive only.
fn current_word_end(buf: &Buffer, cursor: usize, count: u32, big: bool) -> usize {
    let len = buf.len_chars();
    if cursor >= len {
        return cursor;
    }
    let mut pos = cursor;
    let cls = class(char_at(buf, pos), big);
    if cls != Class::Blank {
        while pos + 1 < len && class(char_at(buf, pos + 1), big) == cls {
            pos += 1;
        }
    }
    // Remaining words use the normal `e` semantics (advance into the next
    // word), so the count counts the current word as the first.
    if count > 1 {
        pos = word_end(buf, pos, count - 1, big);
    }
    pos
}

/// `b` / `B`: move to the start (first char) of the current/previous word.
fn word_backward(buf: &Buffer, mut pos: usize, count: u32, big: bool) -> usize {
    for _ in 0..count {
        if pos == 0 {
            break;
        }
        pos -= 1;
        // Skip blanks backward onto the previous word's last char.
        while pos > 0 && class(char_at(buf, pos), big) == Class::Blank {
            pos -= 1;
        }
        if class(char_at(buf, pos), big) == Class::Blank {
            // Reached the start of the buffer over blanks only.
            break;
        }
        // Walk back to the first char of this word-class run.
        let cls = class(char_at(buf, pos), big);
        while pos > 0 && class(char_at(buf, pos - 1), big) == cls {
            pos -= 1;
        }
    }
    pos
}

// ── Line / document motions ───────────────────────────────────────────────────

fn line_start(buf: &Buffer, pos: usize) -> usize {
    buf.line_to_char(buf.char_to_line(pos))
}

/// End of the line containing `pos`, before any trailing newline.
fn line_end(buf: &Buffer, pos: usize) -> usize {
    line_end_offset(buf, buf.char_to_line(pos))
}

/// End of buffer line `line` (its content length from the line start,
/// excluding any trailing newline).  Shared by `$`, the `dw` line clamp,
/// and the linewise `cc` content boundary.
pub fn line_end_offset(buf: &Buffer, line: usize) -> usize {
    let start = buf.line_to_char(line);
    let content_len = buf
        .line(line)
        .map(|s| s.trim_end_matches('\n').chars().count())
        .unwrap_or(0);
    start + content_len
}

/// First non-blank char of `line` (its start when the line is blank).
pub fn first_non_blank(buf: &Buffer, line: usize) -> usize {
    let len = buf.len_chars();
    let mut pos = buf.line_to_char(line.min(buf.line_count().saturating_sub(1)));
    while pos < len {
        let c = char_at(buf, pos);
        if c == '\n' || !c.is_whitespace() {
            break;
        }
        pos += 1;
    }
    pos
}

/// The last line carrying content — the target for `G`.  When the buffer
/// ends with a newline (producing a trailing empty line), back up to the
/// line above it, matching vim's `G`.
fn last_content_line(buf: &Buffer) -> usize {
    let len = buf.len_chars();
    let mut last = buf.char_to_line(len);
    if last > 0 && buf.line_to_char(last) == len {
        last -= 1;
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Buffer;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    fn m(motion: Motion, cursor: usize, text: &str) -> usize {
        resolve_motion(motion, 1, cursor, &buf(text))
    }

    #[test]
    fn word_forward_crosses_punctuation_runs() {
        // "foo.bar baz": w from 'f' → '.', then → 'bar', then → 'baz'.
        let t = "foo.bar baz";
        assert_eq!(m(Motion::WordForward, 0, t), 3); // '.'
        assert_eq!(m(Motion::WordForward, 3, t), 4); // 'b' of bar
        assert_eq!(m(Motion::WordForward, 4, t), 8); // 'b' of baz
    }

    #[test]
    fn big_word_forward_ignores_punctuation() {
        // "foo.bar baz": W from 'f' jumps the whole "foo.bar" blob.
        let t = "foo.bar baz";
        assert_eq!(m(Motion::BigWordForward, 0, t), 8); // 'b' of baz
    }

    #[test]
    fn word_end_lands_on_last_char() {
        // "foo bar": e from 'f' → 'o' (offset 2), then → 'r' (offset 6).
        let t = "foo bar";
        assert_eq!(m(Motion::WordEnd, 0, t), 2);
        assert_eq!(m(Motion::WordEnd, 2, t), 6);
    }

    #[test]
    fn word_backward_lands_on_word_start() {
        // "foo bar baz": b from 'z' (10) → 'b' of baz (8) → 'b' of bar (4).
        let t = "foo bar baz";
        assert_eq!(m(Motion::WordBackward, 10, t), 8);
        assert_eq!(m(Motion::WordBackward, 8, t), 4);
    }

    #[test]
    fn line_motions() {
        let t = "  hello world\nsecond";
        // 0 → line start (the leading space).
        assert_eq!(m(Motion::LineStart, 5, t), 0);
        // ^ → first non-blank.
        assert_eq!(m(Motion::LineFirstNonBlank, 5, t), 2);
        // $ → end of "  hello world" before the newline (offset 13).
        assert_eq!(m(Motion::LineEnd, 0, t), 13);
    }

    #[test]
    fn doc_motions() {
        let t = "  first\nmiddle\nlast";
        // gg → first non-blank of line 0.
        assert_eq!(m(Motion::DocStart, 15, t), 2);
        // G → first non-blank of the last content line ('l' of "last").
        assert_eq!(m(Motion::DocEnd, 0, t), 15);
    }

    #[test]
    fn doc_end_skips_a_trailing_empty_line() {
        // Trailing newline must not park G on the empty final line.
        let t = "alpha\nbeta\n";
        assert_eq!(m(Motion::DocEnd, 0, t), 6); // 'b' of "beta"
    }

    #[test]
    fn word_forward_crosses_a_newline() {
        // "foo\nbar": w from 'f' skips to 'b' on the next line.
        let t = "foo\nbar";
        assert_eq!(m(Motion::WordForward, 0, t), 4);
    }

    // ── Operator ranges ───────────────────────────────────────────────

    #[test]
    fn range_word_forward_is_exclusive() {
        // dw on "foo bar": [0, 4) — includes the trailing space, not 'b'.
        let b = buf("foo bar");
        assert_eq!(
            resolve_motion_range(Motion::WordForward, 1, 0, &b),
            OpRange::Chars(0..4)
        );
    }

    #[test]
    fn range_word_end_is_inclusive() {
        // de on "foo bar": [0, 3) — 'e' lands on 'o' (offset 2), included.
        let b = buf("foo bar");
        assert_eq!(
            resolve_motion_range(Motion::WordEnd, 1, 0, &b),
            OpRange::Chars(0..3)
        );
    }

    #[test]
    fn range_word_forward_clamps_at_line_end() {
        // dw on the last word of a line stops before the newline.
        let b = buf("foo\nbar");
        assert_eq!(
            resolve_motion_range(Motion::WordForward, 1, 0, &b),
            OpRange::Chars(0..3)
        );
    }

    #[test]
    fn range_line_end_stops_before_newline() {
        // d$ from col 0 covers the line content, not the '\n'.
        let b = buf("abc\ndef");
        assert_eq!(
            resolve_motion_range(Motion::LineEnd, 1, 0, &b),
            OpRange::Chars(0..3)
        );
    }

    #[test]
    fn range_doc_start_and_end_are_linewise() {
        let b = buf("a\nb\nc\nd");
        let cursor = b.line_to_char(2); // on "c"
        assert_eq!(
            resolve_motion_range(Motion::DocStart, 1, cursor, &b),
            OpRange::Lines { first: 0, last: 2 }
        );
        assert_eq!(
            resolve_motion_range(Motion::DocEnd, 1, cursor, &b),
            OpRange::Lines { first: 2, last: 3 }
        );
    }

    #[test]
    fn doubled_range_spans_count_lines_from_cursor() {
        let b = buf("a\nb\nc\nd");
        assert_eq!(
            doubled_line_range(&b, 0, 2),
            OpRange::Lines { first: 0, last: 1 }
        );
        // Clamped to the last line.
        let last = b.line_to_char(3);
        assert_eq!(
            doubled_line_range(&b, last, 5),
            OpRange::Lines { first: 3, last: 3 }
        );
    }

    #[test]
    fn vertical_range_walks_up_and_down() {
        let b = buf("a\nb\nc\nd");
        let mid = b.line_to_char(2); // "c"
        assert_eq!(
            vertical_line_range(&b, mid, 1, true),
            OpRange::Lines { first: 2, last: 3 }
        );
        assert_eq!(
            vertical_line_range(&b, mid, 2, false),
            OpRange::Lines { first: 0, last: 2 }
        );
    }

    #[test]
    fn horizontal_step_clamps_to_the_line() {
        // l from 'a' across "abc" stops at the line-content end (offset 3).
        let b = buf("abc\ndef");
        assert_eq!(resolve_motion(Motion::Right, 9, 0, &b), 3);
        // h stops at the line start.
        assert_eq!(resolve_motion(Motion::Left, 9, 2, &b), 0);
    }
}
