//! Vim motion resolution — pure char-offset arithmetic over the rope.
//!
//! `vim_feed` maps a key to a [`Motion`]; [`resolve_motion`] turns that
//! into a target char offset against the buffer.  Every offset here is a
//! rope **char** offset (the same space as `Cursor::offset` and
//! `Selection`) — vim introduces no byte offsets.  See
//! `docs/vim-implementation-plan.md` §2.4.
//!
//! CP2 ships the core motions (`w e b W E B 0 ^ $ gg G`).  Vertical /
//! character-find / paragraph / search / matching-pair motions, plus the
//! count-aware `resolve_motion_range` operator entry point, land in later
//! checkpoints.

use crate::document::Buffer;

/// A resolved Normal-mode motion.  The variants present so far are the
/// CP2 core set; later checkpoints extend the enum (vertical, `f/F/t/T`,
/// `{ }`, `n N`, `%`, `NG`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    WordForward,       // w
    WordEnd,           // e
    WordBackward,      // b
    BigWordForward,    // W
    BigWordEnd,        // E
    BigWordBackward,   // B
    LineStart,         // 0
    LineFirstNonBlank, // ^
    LineEnd,           // $
    DocStart,          // gg
    DocEnd,            // G
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
        Motion::WordForward => word_forward(buf, cursor, count, false),
        Motion::WordEnd => word_end(buf, cursor, count, false),
        Motion::WordBackward => word_backward(buf, cursor, count, false),
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

// ── Word motions ────────────────────────────────────────────────────────────

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
    let line = buf.char_to_line(pos);
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
}
