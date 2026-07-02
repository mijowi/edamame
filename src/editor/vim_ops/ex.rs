//! Ex-command parsing and substitution — CP9.
//!
//! [`parse_ex`] is a *pure* parser from the command-line text (the part
//! after the leading `:`) to an [`ExCommand`].  `:w` / `:q` / `:wq` map to
//! App effects the reducer bubbles up as outcomes (so the dirty-buffer
//! confirm fires exactly as for `Ctrl-Q`); `:s` / `:%s` are executed *here*
//! against `&mut EditorState` by [`execute_substitute`].  This is the only
//! place a regex engine is used — the `/` search path stays literal substring
//! + smartcase, never regex (see `docs/vim-implementation-plan.md` §1, CP9).
//!
//! The substitution is applied as a **single** [`EditDelta`], so an entire
//! `:%s/…/…/g` is one undo unit.
//!
//! **Vim syntax in, vim syntax out.** The pattern is written in vim's regex
//! dialect and translated to `fancy-regex` by
//! [`vim_regex::translate_pattern`](super::vim_regex::translate_pattern); the
//! replacement is written with vim's `\1` / `&` / `\U…\E` and applied per
//! match by [`vim_regex::expand_replacement`](super::vim_regex::expand_replacement).
//! `fancy-regex` (not the `regex` crate) is the engine, so pattern
//! backreferences and the lookaround that `\<`/`\>` translate to are
//! available.  An escaped delimiter (`\/`) is reduced to a literal `/` during
//! parsing, before the pattern reaches the translator.

use fancy_regex::{Regex, RegexBuilder};

use crate::document::EditDelta;
use crate::editor::vim_ops::vim_regex::{expand_replacement, translate_pattern};
use crate::editor::EditorState;

/// A parsed ex command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExCommand {
    /// `:w` — write the buffer.
    Write,
    /// `:w <path>` / `:write <path>` — write a snapshot to the given path
    /// *without* changing the buffer's own path (real-vim `:w {file}`
    /// semantics; the user keeps editing the current file).  `force` is a
    /// trailing `!` (`:w! <path>`), which skips the overwrite-confirmation
    /// prompt.
    WriteCopy { path: String, force: bool },
    /// `:saveas <path>` — write the buffer to the given path and *adopt*
    /// it as the buffer's home (subsequent `:w` target the new path).
    /// `force` is a trailing `!` (`:saveas! <path>`).
    WriteAs { path: String, force: bool },
    /// `:saveas` with no argument — prompt for a path (the user always
    /// wants the path-entry modal here, even on an already-named buffer).
    SaveAsPrompt,
    /// `:q` — quit (dirty-guarded).
    Quit,
    /// `:wq` — write then quit.
    WriteQuit,
    /// `:wq <path>` — write a snapshot to the given path (copy semantics,
    /// like `:w <path>`), then quit.  `force` is a trailing `!`
    /// (`:wq! <path>`).
    WriteQuitCopy { path: String, force: bool },
    /// `:x` — write then quit, but only write when the buffer is modified
    /// (the canonical vim behavior; `:wq` always writes).
    WriteQuitIfModified,
    /// `:s/…` (current line), `:%s/…` (whole file), or `:'<,'>s/…` (the
    /// last visual selection's line span).
    Substitute(Substitution),
}

/// Which lines a substitution runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstituteRange {
    /// `:s` — the current line only.
    CurrentLine,
    /// `:%s` — every line in the buffer.
    AllLines,
    /// `:'<,'>s` — the line span of the last visual selection, resolved
    /// against the concrete bounds threaded into [`execute_substitute`].
    VisualRange,
}

/// A parsed `:s` / `:%s` / `:'<,'>s` substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitution {
    /// The line span the substitution runs over.
    pub range: SubstituteRange,
    /// The vim regex pattern (escaped delimiters already reduced); translated
    /// to `fancy-regex` syntax at execution time.
    pub pattern: String,
    /// The vim replacement text (`\1` / `&` / `\U…\E`), expanded per match.
    pub replacement: String,
    /// Whether the second delimiter was typed (`:s/foo/` vs `:s/foo`).
    /// Both parse to an empty `replacement`, but the live preview must
    /// distinguish "still typing the pattern" (highlight matches only)
    /// from "replace with nothing" (preview the deletion).  The execute
    /// path ignores this — an absent field and an empty one both delete.
    pub replacement_present: bool,
    /// `g` flag — replace every match on a line, not just the first.
    pub global: bool,
    /// `i` flag — case-insensitive matching.
    pub ignore_case: bool,
}

/// A parse- or execution-time ex error.  Its `Display` is the text the
/// reducer flashes on the hint line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExError {
    #[error("Not an editor command: {0}")]
    UnknownCommand(String),
    #[error("Unknown flag: {0}")]
    UnknownFlag(char),
    #[error("Empty search pattern")]
    EmptyPattern,
    #[error("Unsupported vim pattern: {0}")]
    UnsupportedPattern(String),
    #[error("Invalid pattern: {0}")]
    InvalidRegex(String),
}

/// Parse the text after the leading `:` into an [`ExCommand`].
pub fn parse_ex(input: &str) -> Result<ExCommand, ExError> {
    let s = input.trim();

    // Optional `'<,'>` visual-range prefix — the marks vim inserts when `:`
    // is pressed in Visual / Visual-Line.  It only qualifies a `:s`; on any
    // other command the whole line falls through to `UnknownCommand` below.
    let (visual_range, s) = match s.strip_prefix("'<,'>") {
        Some(rest) => (true, rest.trim_start()),
        None => (false, s),
    };

    // Substitution: `%s/…` (all lines), `s/…` (current line), or `'<,'>s/…`
    // (the visual selection).  A bare `:s` (vim's "repeat last substitution")
    // is out of scope, so a delimiter must follow.  A `%` overrides any
    // `'<,'>` prefix, matching vim's last-range-wins rule.
    if let Some(rest) = s.strip_prefix("%s") {
        return parse_substitute(SubstituteRange::AllLines, rest);
    }
    if let Some(rest) = s.strip_prefix('s') {
        if rest.starts_with('/') {
            let range = if visual_range {
                SubstituteRange::VisualRange
            } else {
                SubstituteRange::CurrentLine
            };
            return parse_substitute(range, rest);
        }
    }

    // Beyond `:s`, edamame has no ranged ex commands, but `:` in Visual
    // auto-inserts `'<,'>` — so the write / quit family simply ignores the
    // range and acts on the whole buffer (a Visual `:w` / `:wq` / `:q` does
    // what the user means rather than erroring on the prefix they never typed).
    //
    // The write / save-as family may carry a path argument, so it can't go
    // through the exact-match table below (`:w foo.md` must not be an
    // "unknown command").  A bare `:w` / `:wq` resolves here too.
    if let Some(cmd) = parse_write_forms(s) {
        return Ok(cmd);
    }

    match s {
        "q" | "quit" => Ok(ExCommand::Quit),
        "x" | "xit" => Ok(ExCommand::WriteQuitIfModified),
        // An unknown command keeps any `'<,'>` prefix in the message so the
        // user sees exactly what failed to parse.
        _ if visual_range => Err(ExError::UnknownCommand(input.trim().to_owned())),
        other => Err(ExError::UnknownCommand(other.to_owned())),
    }
}

/// Parse the write / save-as command family, every member of which may
/// carry a path argument: `:w[!] [path]`, `:write[!] [path]`,
/// `:saveas[!] [path]`, and `:wq[!] [path]`.  A trailing `!` (force) is
/// accepted and ignored — edamame has no read-only buffer concept.
/// Returns `None` for anything outside this family so the caller's
/// exact-match table can handle `:q`, `:x`, …
fn parse_write_forms(s: &str) -> Option<ExCommand> {
    // Split the command word (up to the first whitespace) from its
    // argument; `s` is already outer-trimmed, so the remainder is the
    // path verbatim (internal spaces preserved, no surrounding blanks).
    let (head, rest) = match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    };
    let force = head.ends_with('!');
    let word = head.strip_suffix('!').unwrap_or(head);
    let path = (!rest.is_empty()).then(|| rest.to_owned());
    match (word, path) {
        // `:saveas <path>` re-points; a bare `:saveas` prompts for a name.
        ("saveas", Some(path)) => Some(ExCommand::WriteAs { path, force }),
        ("saveas", None) => Some(ExCommand::SaveAsPrompt),
        // `:w <path>` writes a copy and keeps the current file (real vim).
        ("w" | "write", Some(path)) => Some(ExCommand::WriteCopy { path, force }),
        ("w" | "write", None) => Some(ExCommand::Write),
        ("wq", Some(path)) => Some(ExCommand::WriteQuitCopy { path, force }),
        ("wq", None) => Some(ExCommand::WriteQuit),
        _ => None,
    }
}

/// Parse the `/pat/rep/flags` tail of a substitution.  `rest` is everything
/// after the `s` / `%s` prefix and must begin with the `/` delimiter.
fn parse_substitute(range: SubstituteRange, rest: &str) -> Result<ExCommand, ExError> {
    const DELIM: char = '/';
    let Some(body) = rest.strip_prefix(DELIM) else {
        let prefix = match range {
            SubstituteRange::AllLines => "%s",
            SubstituteRange::VisualRange => "'<,'>s",
            SubstituteRange::CurrentLine => "s",
        };
        return Err(ExError::UnknownCommand(format!("{prefix}{rest}")));
    };

    let (pattern, after_pattern) = take_field(body, DELIM);
    let replacement_present = after_pattern.is_some();
    // No second delimiter (`:s/foo`) → empty replacement, no flags.
    let (replacement, flags) = match after_pattern {
        None => (String::new(), ""),
        Some(after) => {
            let (rep, after_rep) = take_field(after, DELIM);
            (rep, after_rep.unwrap_or(""))
        }
    };

    let mut global = false;
    let mut ignore_case = false;
    for c in flags.chars() {
        match c {
            'g' => global = true,
            'i' => ignore_case = true,
            c if c.is_whitespace() => {}
            c => return Err(ExError::UnknownFlag(c)),
        }
    }

    Ok(ExCommand::Substitute(Substitution {
        range,
        pattern,
        replacement,
        replacement_present,
        global,
        ignore_case,
    }))
}

/// Take one delimiter-terminated field from `s`.  Returns the field text —
/// with `\<delim>` reduced to a literal `<delim>`, every other escape kept
/// for the regex engine — and the slice *after* the terminating delimiter,
/// or `None` when no unescaped delimiter remains (the field runs to the end).
fn take_field(s: &str, delim: char) -> (String, Option<&str>) {
    let mut out = String::new();
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            if c == delim {
                out.push(delim);
            } else {
                out.push('\\');
                out.push(c);
            }
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == delim {
            return (out, Some(&s[i + c.len_utf8()..]));
        } else {
            out.push(c);
        }
    }
    // A trailing backslash with no following char is kept literally.
    if escaped {
        out.push('\\');
    }
    (out, None)
}

/// Execute a substitution against the editor and return the number of
/// matches replaced (`Ok(0)` when the pattern never matched — a no-op that
/// records no edit).  The vim pattern is translated to `fancy-regex` syntax
/// first (`translate_pattern`), and the replacement is expanded per match by
/// `expand_replacement` (so vim's `\1` / `&` / `\U…\E` all work).  The whole
/// substitution is applied as one [`EditDelta`] so it undoes in a single step.
/// Each affected line is processed independently (first match only, or every
/// match with the `g` flag), matching vim's per-line semantics.
///
/// `visual_range` supplies the inclusive `(first, last)` buffer-line span for a
/// [`SubstituteRange::VisualRange`] substitution (the marks vim carries from
/// the last Visual selection).  It is ignored for the other ranges, and a
/// `VisualRange` with no bounds falls back to the current line.
pub fn execute_substitute(
    editor: &mut EditorState,
    sub: &Substitution,
    visual_range: Option<(usize, usize)>,
) -> Result<usize, ExError> {
    if sub.pattern.is_empty() {
        return Err(ExError::EmptyPattern);
    }
    let translated = translate_pattern(&sub.pattern)?;
    let re = RegexBuilder::new(&translated)
        .case_insensitive(sub.ignore_case)
        .build()
        .map_err(|e| ExError::InvalidRegex(e.to_string()))?;

    let cursor_line = editor.buffer.char_to_line(editor.cursor.offset);
    let Some(edit) = build_substitution(&editor.buffer, cursor_line, &re, sub, visual_range, None)?
    else {
        return Ok(0);
    };
    let count = edit.count;
    let range_first = edit.range_first;
    editor.apply_delta(edit.delta);
    // Park the cursor at the start of the first affected line rather than at
    // the end of the inserted region (`apply_delta`'s default), which for
    // `:%s` would jump to end-of-document.
    let target = editor
        .buffer
        .line_to_char(range_first.min(editor.buffer.line_count().saturating_sub(1)));
    editor.place_cursor(target);
    Ok(count)
}

/// Resolve a substitution's inclusive `(first, last)` buffer-line span.
/// `None` only for an empty buffer (`line_count == 0`).  `cursor_line` is
/// the line the cursor sits on (for [`SubstituteRange::CurrentLine`] and a
/// [`SubstituteRange::VisualRange`] with no recorded bounds).
pub(crate) fn resolve_substitute_lines(
    buffer: &crate::document::Buffer,
    cursor_line: usize,
    range: SubstituteRange,
    visual_range: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    let line_count = buffer.line_count();
    if line_count == 0 {
        return None;
    }
    Some(match range {
        SubstituteRange::AllLines => (0, line_count - 1),
        SubstituteRange::CurrentLine => (cursor_line, cursor_line),
        SubstituteRange::VisualRange => match visual_range {
            Some((f, l)) => (f.min(line_count - 1), l.min(line_count - 1)),
            None => (cursor_line, cursor_line),
        },
    })
}

/// The fully-computed edit for one substitution, produced by
/// [`build_substitution`] against an unmodified buffer.  Pure data — the
/// shared seam between the commit path ([`execute_substitute`]) and the
/// live preview (`vim_ops::preview`).
pub(crate) struct SubstitutionEdit {
    /// The single char-offset delta rewriting lines `range_first..=` the
    /// last scanned line.
    pub delta: EditDelta,
    /// Total matches replaced.
    pub count: usize,
    /// Post-apply byte ranges of each inserted replacement segment,
    /// absolute in the rewritten buffer (preceding text is untouched, so
    /// offsets before the rewritten region are identical pre/post).
    pub replaced_ranges: Vec<std::ops::Range<usize>>,
    /// First line of the resolved range (where the commit path parks the
    /// cursor).
    pub range_first: usize,
    /// First line that actually matched (where the preview scrolls to).
    pub first_match_line: usize,
}

/// Walk the substitution's line range and build the combined edit without
/// applying anything.  Returns `Ok(None)` when the pattern never matched
/// (or the buffer is empty) — the commit path turns that into "Pattern not
/// found".  `match_cap` bounds the walk for the live preview: once the
/// total match count reaches the cap the remaining lines are left
/// untouched (the delta still only spans scanned lines, so it stays
/// correct).  The commit path passes `None` — a real `:%s` is never
/// truncated.
pub(crate) fn build_substitution(
    buffer: &crate::document::Buffer,
    cursor_line: usize,
    re: &Regex,
    sub: &Substitution,
    visual_range: Option<(usize, usize)>,
    match_cap: Option<usize>,
) -> Result<Option<SubstitutionEdit>, ExError> {
    let Some((first, last)) =
        resolve_substitute_lines(buffer, cursor_line, sub.range, visual_range)
    else {
        return Ok(None);
    };

    let start_char = buffer.line_to_char(first);
    // Text before the rewritten region is untouched, so its byte length is
    // the same pre- and post-apply — replacement spans offset from here are
    // valid absolute positions in the rewritten buffer.
    let region_start_byte = buffer.rope().char_to_byte(start_char);
    let mut old = String::new();
    let mut new = String::new();
    let mut total = 0usize;
    let mut replaced_ranges = Vec::new();
    let mut first_match_line = None;
    for li in first..=last {
        let line = buffer.rope().line(li).to_string();
        // Process the line content without its trailing newline so `^`/`$`
        // anchor per line and the newline is never consumed.
        let (content, nl) = match line.strip_suffix('\n') {
            Some(c) => (c, "\n"),
            None => (line.as_str(), ""),
        };
        let (replaced, n, spans) = substitute_line(re, content, &sub.replacement, sub.global)?;
        if n > 0 && first_match_line.is_none() {
            first_match_line = Some(li);
        }
        total += n;
        let line_base = region_start_byte + new.len();
        replaced_ranges.extend(
            spans
                .into_iter()
                .map(|s| line_base + s.start..line_base + s.end),
        );
        old.push_str(content);
        old.push_str(nl);
        new.push_str(&replaced);
        new.push_str(nl);
        if match_cap.is_some_and(|cap| total >= cap) {
            break;
        }
    }

    if total == 0 {
        return Ok(None);
    }
    Ok(Some(SubstitutionEdit {
        delta: EditDelta {
            offset: start_char,
            removed: old,
            inserted: new,
        },
        count: total,
        replaced_ranges,
        range_first: first,
        first_match_line: first_match_line.unwrap_or(first),
    }))
}

/// Apply `re` to a single line's `content`, expanding `template` per match
/// (vim `\1` / `&` / case modifiers via `expand_replacement`).  Returns the
/// rewritten line, the match count, and the byte span of each expanded
/// replacement *within the rewritten line* (the preview highlights these).
/// `global` replaces every match; otherwise only the first.  A match-time
/// engine error (e.g. a backtrack limit) surfaces as
/// [`ExError::InvalidRegex`].
fn substitute_line(
    re: &Regex,
    content: &str,
    template: &str,
    global: bool,
) -> Result<(String, usize, Vec<std::ops::Range<usize>>), ExError> {
    let mut out = String::new();
    let mut last = 0;
    let mut count = 0;
    let mut spans = Vec::new();
    for cap in re.captures_iter(content) {
        let caps = cap.map_err(|e| ExError::InvalidRegex(e.to_string()))?;
        let whole = caps.get(0).expect("group 0 is always present");
        out.push_str(&content[last..whole.start()]);
        let span_start = out.len();
        out.push_str(&expand_replacement(template, &caps));
        spans.push(span_start..out.len());
        last = whole.end();
        count += 1;
        if !global {
            break;
        }
    }
    if count == 0 {
        return Ok((content.to_owned(), 0, spans));
    }
    out.push_str(&content[last..]);
    Ok((out, count, spans))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `rep: None` models a missing replacement field (`:s/foo`, no second
    /// delimiter) — `replacement_present` false, empty replacement.
    fn sub(
        range: SubstituteRange,
        pat: &str,
        rep: Option<&str>,
        global: bool,
        ignore_case: bool,
    ) -> ExCommand {
        ExCommand::Substitute(Substitution {
            range,
            pattern: pat.to_owned(),
            replacement: rep.unwrap_or("").to_owned(),
            replacement_present: rep.is_some(),
            global,
            ignore_case,
        })
    }

    use SubstituteRange::{AllLines, CurrentLine, VisualRange};

    #[test]
    fn parses_write_quit_variants() {
        assert_eq!(parse_ex("w"), Ok(ExCommand::Write));
        assert_eq!(parse_ex("write"), Ok(ExCommand::Write));
        assert_eq!(parse_ex("q"), Ok(ExCommand::Quit));
        assert_eq!(parse_ex("wq"), Ok(ExCommand::WriteQuit));
        assert_eq!(parse_ex("x"), Ok(ExCommand::WriteQuitIfModified));
        assert_eq!(parse_ex("xit"), Ok(ExCommand::WriteQuitIfModified));
        // Leading / trailing whitespace is tolerated.
        assert_eq!(parse_ex("  w  "), Ok(ExCommand::Write));
    }

    #[test]
    fn parses_write_as_forms() {
        // `:w <path>` / `:write <path>` write a copy (keep the current file).
        assert_eq!(
            parse_ex("w notes.md"),
            Ok(ExCommand::WriteCopy {
                path: "notes.md".to_owned(),
                force: false,
            })
        );
        assert_eq!(
            parse_ex("write notes.md"),
            Ok(ExCommand::WriteCopy {
                path: "notes.md".to_owned(),
                force: false,
            })
        );
        // `:saveas <path>` re-points the buffer at the new path.
        assert_eq!(
            parse_ex("saveas notes.md"),
            Ok(ExCommand::WriteAs {
                path: "notes.md".to_owned(),
                force: false,
            })
        );
        // A bare `:saveas` prompts for a path.
        assert_eq!(parse_ex("saveas"), Ok(ExCommand::SaveAsPrompt));
        // `:wq <path>` writes a copy, then quits.
        assert_eq!(
            parse_ex("wq out.md"),
            Ok(ExCommand::WriteQuitCopy {
                path: "out.md".to_owned(),
                force: false,
            })
        );
        // A bare `:w!` writes to the current path (force needs no path).
        assert_eq!(parse_ex("w!"), Ok(ExCommand::Write));
        // `!` on a named destination sets force (skips the overwrite prompt).
        assert_eq!(
            parse_ex("w! notes.md"),
            Ok(ExCommand::WriteCopy {
                path: "notes.md".to_owned(),
                force: true,
            })
        );
        assert_eq!(
            parse_ex("saveas! out.md"),
            Ok(ExCommand::WriteAs {
                path: "out.md".to_owned(),
                force: true,
            })
        );
        assert_eq!(
            parse_ex("wq! out.md"),
            Ok(ExCommand::WriteQuitCopy {
                path: "out.md".to_owned(),
                force: true,
            })
        );
        // Internal spaces in the path are preserved.
        assert_eq!(
            parse_ex("w my file.md"),
            Ok(ExCommand::WriteCopy {
                path: "my file.md".to_owned(),
                force: false,
            })
        );
    }

    #[test]
    fn unknown_command_errors() {
        assert_eq!(
            parse_ex("nope"),
            Err(ExError::UnknownCommand("nope".to_owned()))
        );
        // A bare `s` with no delimiter is not a known command.
        assert_eq!(parse_ex("s"), Err(ExError::UnknownCommand("s".to_owned())));
    }

    #[test]
    fn parses_line_substitution() {
        assert_eq!(
            parse_ex("s/foo/bar/"),
            Ok(sub(CurrentLine, "foo", Some("bar"), false, false))
        );
        // Trailing delimiter optional.
        assert_eq!(
            parse_ex("s/foo/bar"),
            Ok(sub(CurrentLine, "foo", Some("bar"), false, false))
        );
        // Missing replacement deletes the match.
        assert_eq!(
            parse_ex("s/foo"),
            Ok(sub(CurrentLine, "foo", None, false, false))
        );
    }

    #[test]
    fn replacement_present_tracks_the_second_delimiter() {
        // `s/foo/` and `s/foo` both parse to an empty replacement, but only
        // the former typed the second delimiter — the live preview keys the
        // highlight-only vs. deletion-preview distinction off this bit.
        assert_eq!(
            parse_ex("s/foo/"),
            Ok(sub(CurrentLine, "foo", Some(""), false, false))
        );
        // An escaped delimiter is field content, not a terminator.
        assert_eq!(
            parse_ex(r"s/foo\/bar"),
            Ok(sub(CurrentLine, "foo/bar", None, false, false))
        );
    }

    #[test]
    fn parses_global_substitution_and_flags() {
        assert_eq!(
            parse_ex("%s/a/b/g"),
            Ok(sub(AllLines, "a", Some("b"), true, false))
        );
        assert_eq!(
            parse_ex("%s/a/b/gi"),
            Ok(sub(AllLines, "a", Some("b"), true, true))
        );
        assert_eq!(
            parse_ex("s/a/b/i"),
            Ok(sub(CurrentLine, "a", Some("b"), false, true))
        );
    }

    #[test]
    fn parses_visual_range_substitution() {
        // The `'<,'>` marks vim inserts when `:` is pressed in Visual mode.
        assert_eq!(
            parse_ex("'<,'>s/foo/bar/g"),
            Ok(sub(VisualRange, "foo", Some("bar"), true, false))
        );
        // A `%` after the range wins (last range specifier wins, as in vim).
        assert_eq!(
            parse_ex("'<,'>%s/a/b/"),
            Ok(sub(AllLines, "a", Some("b"), false, false))
        );
        // The write / quit family ignores a `'<,'>` prefix (Visual `:` inserts
        // it) and acts on the whole buffer.
        assert_eq!(parse_ex("'<,'>w"), Ok(ExCommand::Write));
        assert_eq!(parse_ex("'<,'>wq"), Ok(ExCommand::WriteQuit));
        assert_eq!(parse_ex("'<,'>q"), Ok(ExCommand::Quit));
        assert_eq!(parse_ex("'<,'>x"), Ok(ExCommand::WriteQuitIfModified));
        // A genuinely unknown command keeps the prefix in the error.
        assert_eq!(
            parse_ex("'<,'>nope"),
            Err(ExError::UnknownCommand("'<,'>nope".to_owned()))
        );
    }

    #[test]
    fn unknown_flag_errors() {
        assert_eq!(parse_ex("s/a/b/z"), Err(ExError::UnknownFlag('z')));
    }

    #[test]
    fn escaped_delimiter_is_literal() {
        // `\/` in the pattern is a literal slash; regex escapes survive.
        assert_eq!(
            parse_ex(r"s/a\/b/c\.d/"),
            Ok(sub(CurrentLine, "a/b", Some(r"c\.d"), false, false))
        );
    }
}
