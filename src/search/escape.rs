//! Backslash escapes for search queries.
//!
//! Search stays **literal substring** matching — there is deliberately no
//! regex in the `/` path (regex is confined to `:s`/`:%s`, see
//! `editor::vim_ops::vim_regex`).  But a literal matcher still needs a way
//! to express characters a single-line text field can't hold, above all the
//! line break: `/  \n` must find every line ending in two spaces.
//!
//! The convention is vim's: a backslash always introduces an escape, so a
//! **literal backslash must be written `\\`**, and an escape we don't
//! recognize is an error rather than a silently-literal `\d`.  Erroring is
//! the point — a user who types `\d` expecting a digit class should be told
//! the search is not a regex, not quietly handed zero matches.
//!
//! [`decode`] runs on text the *user typed*; [`escape`] is its inverse, for
//! text **edamame** supplies to a search (the `*` / `#` keyword under the
//! cursor, a pasted payload) which must not have its backslashes reinterpreted.

/// A malformed escape in a search query.  Its `Display` is the text shown
/// on the hint line / in the search modal's error row, matching the
/// `ExError` convention in `editor::vim_ops::ex`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EscapeError {
    #[error("Unsupported escape: \\{0} (use \\\\ for a literal backslash)")]
    Unsupported(char),
    #[error("Trailing backslash (use \\\\ for a literal backslash)")]
    Trailing,
}

/// Decode the backslash escapes in a user-typed search query into the
/// literal needle the matcher searches for.
///
/// `\n` → line feed, `\t` → tab, `\r` → carriage return, `\\` → a single
/// backslash.  Every other `\<c>` is [`EscapeError::Unsupported`], and a
/// query ending in a lone backslash is [`EscapeError::Trailing`].
pub fn decode(input: &str) -> Result<String, EscapeError> {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => return Err(EscapeError::Unsupported(other)),
            None => return Err(EscapeError::Trailing),
        }
    }
    Ok(out)
}

/// Encode a literal string so [`decode`] round-trips it back unchanged —
/// the inverse of `decode`, for text edamame supplies rather than the user
/// types.  Without it, `*` on a word containing a backslash would search
/// for something the user never wrote (or fail to parse at all).
pub fn escape(literal: &str) -> String {
    let mut out = String::with_capacity(literal.len());
    for c in literal.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_supported_escapes() {
        assert_eq!(decode(r"  \n").unwrap(), "  \n");
        assert_eq!(decode(r"a\tb").unwrap(), "a\tb");
        assert_eq!(decode(r"a\rb").unwrap(), "a\rb");
        assert_eq!(decode(r"C:\\dir").unwrap(), r"C:\dir");
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(decode("foo bar").unwrap(), "foo bar");
        assert_eq!(decode("").unwrap(), "");
    }

    #[test]
    fn an_escaped_backslash_does_not_start_a_new_escape() {
        // `\\n` is a literal backslash followed by the letter n — NOT a
        // newline.  Getting this wrong is the classic escape-decoder bug.
        assert_eq!(decode(r"\\n").unwrap(), r"\n");
        assert_eq!(decode(r"\\\n").unwrap(), "\\\n");
    }

    #[test]
    fn unknown_and_trailing_escapes_error() {
        // Search is literal, not regex: `\d` must say so rather than
        // silently matching a backslash and a d.
        assert_eq!(decode(r"\d"), Err(EscapeError::Unsupported('d')));
        assert_eq!(decode(r"a\"), Err(EscapeError::Trailing));
        assert_eq!(decode(r"\"), Err(EscapeError::Trailing));
    }

    #[test]
    fn escape_round_trips_through_decode() {
        for literal in [
            "plain",
            "back\\slash",
            "line\nbreak",
            "tab\there",
            "\\n",
            "\\",
            "mixed \\ and \n and \t",
        ] {
            assert_eq!(
                decode(&escape(literal)).as_deref(),
                Ok(literal),
                "round-trip failed for {literal:?}"
            );
        }
    }

    #[test]
    fn escape_leaves_ordinary_text_alone() {
        assert_eq!(escape("foo bar"), "foo bar");
        assert_eq!(escape("café"), "café");
    }
}
