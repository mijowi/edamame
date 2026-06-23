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
    /// `:s/…` (current line) or `:%s/…` (whole file).
    Substitute(Substitution),
}

/// A parsed `:s` / `:%s` substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitution {
    /// `:%s` (every line) vs. `:s` (the current line only).
    pub all_lines: bool,
    /// The vim regex pattern (escaped delimiters already reduced); translated
    /// to `fancy-regex` syntax at execution time.
    pub pattern: String,
    /// The vim replacement text (`\1` / `&` / `\U…\E`), expanded per match.
    pub replacement: String,
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

    // Substitution: `%s/…` (all lines) or `s/…` (current line).  A bare
    // `:s` (vim's "repeat last substitution") is out of scope, so a
    // delimiter must follow.
    if let Some(rest) = s.strip_prefix("%s") {
        return parse_substitute(true, rest);
    }
    if let Some(rest) = s.strip_prefix('s') {
        if rest.starts_with('/') {
            return parse_substitute(false, rest);
        }
    }

    // The write / save-as family may carry a path argument, so it can't go
    // through the exact-match table below (`:w foo.md` must not be an
    // "unknown command").  A bare `:w` / `:wq` resolves here too.
    if let Some(cmd) = parse_write_forms(s) {
        return Ok(cmd);
    }

    match s {
        "q" | "quit" => Ok(ExCommand::Quit),
        "x" | "xit" => Ok(ExCommand::WriteQuitIfModified),
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
fn parse_substitute(all_lines: bool, rest: &str) -> Result<ExCommand, ExError> {
    const DELIM: char = '/';
    let Some(body) = rest.strip_prefix(DELIM) else {
        let prefix = if all_lines { "%s" } else { "s" };
        return Err(ExError::UnknownCommand(format!("{prefix}{rest}")));
    };

    let (pattern, after_pattern) = take_field(body, DELIM);
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
        all_lines,
        pattern,
        replacement,
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
pub fn execute_substitute(editor: &mut EditorState, sub: &Substitution) -> Result<usize, ExError> {
    if sub.pattern.is_empty() {
        return Err(ExError::EmptyPattern);
    }
    let translated = translate_pattern(&sub.pattern)?;
    let re = RegexBuilder::new(&translated)
        .case_insensitive(sub.ignore_case)
        .build()
        .map_err(|e| ExError::InvalidRegex(e.to_string()))?;

    let line_count = editor.buffer.line_count();
    if line_count == 0 {
        return Ok(0);
    }
    let (first, last) = if sub.all_lines {
        (0, line_count - 1)
    } else {
        let l = editor.buffer.char_to_line(editor.cursor.offset);
        (l, l)
    };

    let start_char = editor.buffer.line_to_char(first);
    let mut old = String::new();
    let mut new = String::new();
    let mut total = 0usize;
    for li in first..=last {
        let line = editor.buffer.rope().line(li).to_string();
        // Process the line content without its trailing newline so `^`/`$`
        // anchor per line and the newline is never consumed.
        let (content, nl) = match line.strip_suffix('\n') {
            Some(c) => (c, "\n"),
            None => (line.as_str(), ""),
        };
        let (replaced, n) = substitute_line(&re, content, &sub.replacement, sub.global)?;
        total += n;
        old.push_str(content);
        old.push_str(nl);
        new.push_str(&replaced);
        new.push_str(nl);
    }

    if total == 0 {
        return Ok(0);
    }

    editor.apply_delta(EditDelta {
        offset: start_char,
        removed: old,
        inserted: new,
    });
    // Park the cursor at the start of the first affected line rather than at
    // the end of the inserted region (`apply_delta`'s default), which for
    // `:%s` would jump to end-of-document.
    let target = editor
        .buffer
        .line_to_char(first.min(editor.buffer.line_count().saturating_sub(1)));
    editor.cursor.offset = target.min(editor.buffer.len_chars());
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
    Ok(total)
}

/// Apply `re` to a single line's `content`, expanding `template` per match
/// (vim `\1` / `&` / case modifiers via `expand_replacement`).  Returns the
/// rewritten line and the match count.  `global` replaces every match;
/// otherwise only the first.  A match-time engine error (e.g. a backtrack
/// limit) surfaces as [`ExError::InvalidRegex`].
fn substitute_line(
    re: &Regex,
    content: &str,
    template: &str,
    global: bool,
) -> Result<(String, usize), ExError> {
    let mut out = String::new();
    let mut last = 0;
    let mut count = 0;
    for cap in re.captures_iter(content) {
        let caps = cap.map_err(|e| ExError::InvalidRegex(e.to_string()))?;
        let whole = caps.get(0).expect("group 0 is always present");
        out.push_str(&content[last..whole.start()]);
        out.push_str(&expand_replacement(template, &caps));
        last = whole.end();
        count += 1;
        if !global {
            break;
        }
    }
    if count == 0 {
        return Ok((content.to_owned(), 0));
    }
    out.push_str(&content[last..]);
    Ok((out, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(all_lines: bool, pat: &str, rep: &str, global: bool, ignore_case: bool) -> ExCommand {
        ExCommand::Substitute(Substitution {
            all_lines,
            pattern: pat.to_owned(),
            replacement: rep.to_owned(),
            global,
            ignore_case,
        })
    }

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
            Ok(sub(false, "foo", "bar", false, false))
        );
        // Trailing delimiter optional.
        assert_eq!(
            parse_ex("s/foo/bar"),
            Ok(sub(false, "foo", "bar", false, false))
        );
        // Missing replacement deletes the match.
        assert_eq!(parse_ex("s/foo"), Ok(sub(false, "foo", "", false, false)));
    }

    #[test]
    fn parses_global_substitution_and_flags() {
        assert_eq!(parse_ex("%s/a/b/g"), Ok(sub(true, "a", "b", true, false)));
        assert_eq!(parse_ex("%s/a/b/gi"), Ok(sub(true, "a", "b", true, true)));
        assert_eq!(parse_ex("s/a/b/i"), Ok(sub(false, "a", "b", false, true)));
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
            Ok(sub(false, "a/b", r"c\.d", false, false))
        );
    }
}
