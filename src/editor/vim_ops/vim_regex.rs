//! Vim-pattern → `fancy-regex` translation for `:s` / `:%s` (CP9 follow-up).
//!
//! Vim's regex *syntax* is its own dialect — magic levels (`\m \v \M \V`),
//! backslashed grouping/quantifiers (`\( \) \+ \|`), `\<`/`\>` word
//! boundaries, `\a`/`\x`/… character classes — that no Rust regex engine
//! speaks natively.  So a vim user's pattern must be translated before it
//! reaches the engine.  This module is that translator, plus the matching
//! replacement expander (`\1`, `&`, `\U…\E` case modifiers — applied by us,
//! since the engine's `$1` replacement syntax can't do vim case folding).
//!
//! We compile the translated pattern with `fancy-regex` (not the `regex`
//! crate) so backreferences and lookaround survive the round-trip — `\<`/`\>`
//! become lookaround, and `\1` in a pattern passes straight through.
//!
//! Coverage is the common-to-moderately-advanced surface; a handful of rare
//! atoms (`\zs \ze`, postfix `\@=` lookaround, `\%[…]`/`\%^`/…) are rejected
//! with an explanatory [`ExError::UnsupportedPattern`] rather than
//! mistranslated.  See `docs/vim-implementation-plan.md` §1, CP9.

use fancy_regex::Captures;

use super::ex::ExError;

/// Vim's four "magic" levels, switchable mid-pattern via `\v \m \M \V`.
/// They decide whether grouping / quantifier / class metacharacters are
/// special *bare* or only when backslash-escaped.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MagicLevel {
    /// `\v` — almost everything special bare (closest to PCRE).
    Very,
    /// `\m` — vim default: `. * [ ] ^ $` special bare; grouping needs `\`.
    Magic,
    /// `\M` — only `^ $` special bare.
    No,
    /// `\V` — only `\` special; everything else literal.
    VeryNo,
}

/// Translate a vim regex pattern into `fancy-regex` syntax.
///
/// Returns [`ExError::UnsupportedPattern`] for the rare atoms we decline to
/// translate (so the user gets a clear message instead of a silent
/// mismatch).  A genuinely malformed regex surfaces later, when the
/// translated string fails to compile.
pub fn translate_pattern(input: &str) -> Result<String, ExError> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut magic = MagicLevel::Magic;
    // True at the start of a branch (pattern start, after `(` or `|`), where a
    // leading quantifier (`*`/`+`/`?`) is a literal, not an operator — vim's
    // rule, and also what keeps `regex` from erroring on a leading `*`.
    let mut branch_start = true;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            i += 1;
            let Some(&d) = chars.get(i) else {
                out.push_str("\\\\");
                break;
            };
            i += 1;
            translate_escape(d, &chars, &mut i, &mut magic, &mut out, &mut branch_start)?;
            continue;
        }
        match magic {
            MagicLevel::Very => emit_very_magic(&chars, &mut i, &mut out, &mut branch_start)?,
            MagicLevel::Magic => emit_magic(&chars, &mut i, &mut out, &mut branch_start)?,
            MagicLevel::No => emit_nomagic(&chars, &mut i, &mut out, &mut branch_start),
            MagicLevel::VeryNo => {
                escape_literal(chars[i], &mut out);
                i += 1;
                branch_start = false;
            }
        }
    }
    Ok(out)
}

/// Handle a `\<x>` escape (the char after the backslash is already consumed,
/// `*i` points at the next input char).  Mode switches, character-class
/// escapes, backreferences, and the literal `\t`/`\n`/`\r` are
/// mode-independent; the grouping / quantifier / boundary set flips meaning
/// between very-magic (where the backslash makes it *literal*) and the other
/// modes (where the backslash makes it *special*).
fn translate_escape(
    d: char,
    chars: &[char],
    i: &mut usize,
    magic: &mut MagicLevel,
    out: &mut String,
    branch_start: &mut bool,
) -> Result<(), ExError> {
    // ── Mode switches — no output, branch state unchanged. ──
    match d {
        'v' => {
            *magic = MagicLevel::Very;
            return Ok(());
        }
        'm' => {
            *magic = MagicLevel::Magic;
            return Ok(());
        }
        'M' => {
            *magic = MagicLevel::No;
            return Ok(());
        }
        'V' => {
            *magic = MagicLevel::VeryNo;
            return Ok(());
        }
        _ => {}
    }

    // ── The grouping / quantifier / boundary set: special unless very-magic. ──
    if matches!(d, '(' | ')' | '+' | '?' | '=' | '|' | '{' | '<' | '>') {
        if *magic == MagicLevel::Very {
            escape_literal(d, out);
            *branch_start = false;
        } else {
            emit_group_atom(d, chars, i, out, branch_start)?;
        }
        return Ok(());
    }

    // ── Mode-independent escapes. ──
    match d {
        // Non-capturing group `\%(` — magic-mode spelling.
        '%' => {
            if chars.get(*i) == Some(&'(') {
                *i += 1;
                out.push_str("(?:");
                *branch_start = true;
            } else {
                return Err(ExError::UnsupportedPattern(format!(
                    "\\%{}",
                    chars.get(*i).copied().unwrap_or(' ')
                )));
            }
        }
        // Match-start / match-end reset — needs whole-pattern restructuring.
        'z' => {
            return Err(ExError::UnsupportedPattern(format!(
                "\\z{}",
                chars.get(*i).copied().unwrap_or(' ')
            )));
        }
        // Postfix lookaround (`\(…\)\@=`).
        '@' => return Err(ExError::UnsupportedPattern("\\@".to_owned())),
        // Backreferences pass straight through (fancy-regex supports them).
        '1'..='9' => {
            out.push('\\');
            out.push(d);
            *branch_start = false;
        }
        // Character-class escapes that match `regex` spelling.
        'd' | 'D' | 'w' | 'W' | 's' | 'S' => {
            out.push('\\');
            out.push(d);
            *branch_start = false;
        }
        // Vim character classes with no direct equivalent → bracket expansions.
        'a' => push_class(out, "[A-Za-z]", branch_start),
        'A' => push_class(out, "[^A-Za-z]", branch_start),
        'l' => push_class(out, "[a-z]", branch_start),
        'L' => push_class(out, "[^a-z]", branch_start),
        'u' => push_class(out, "[A-Z]", branch_start),
        'U' => push_class(out, "[^A-Z]", branch_start),
        'x' => push_class(out, "[0-9A-Fa-f]", branch_start),
        'X' => push_class(out, "[^0-9A-Fa-f]", branch_start),
        'o' => push_class(out, "[0-7]", branch_start),
        'O' => push_class(out, "[^0-7]", branch_start),
        'h' => push_class(out, "[A-Za-z_]", branch_start),
        'H' => push_class(out, "[^A-Za-z_]", branch_start),
        // Keyword / identifier chars are iskeyword-dependent; approximate.
        'k' | 'i' => push_class(out, "\\w", branch_start),
        't' => push_class(out, "\\t", branch_start),
        // Vim's pattern `\n` matches a newline, and so does ours: the
        // substitution runs over the whole range at once (see
        // `ex::region_haystack`), so `:%s/  \n/ /g` really does join lines.
        'n' => push_class(out, "\\n", branch_start),
        'r' => push_class(out, "\\r", branch_start),
        // Any other escaped char is a literal (`\.`, `\*`, `\/`, `\~`, …).
        _ => {
            escape_literal(d, out);
            *branch_start = false;
        }
    }
    Ok(())
}

/// Emit the *special* form of a grouping / quantifier / boundary atom
/// (`( ) + ? = | { < >`), updating `branch_start`.  Used for the backslashed
/// form in magic modes and the bare form in very-magic.
fn emit_group_atom(
    d: char,
    chars: &[char],
    i: &mut usize,
    out: &mut String,
    branch_start: &mut bool,
) -> Result<(), ExError> {
    match d {
        '(' => {
            out.push('(');
            *branch_start = true;
        }
        ')' => {
            out.push(')');
            *branch_start = false;
        }
        '|' => {
            out.push('|');
            *branch_start = true;
        }
        '+' => {
            out.push('+');
            *branch_start = false;
        }
        '?' | '=' => {
            out.push('?');
            *branch_start = false;
        }
        '{' => {
            out.push_str(&take_brace(chars, i));
            *branch_start = false;
        }
        '<' => {
            // Start of word: a boundary with a word char ahead.
            out.push_str("\\b(?=\\w)");
            *branch_start = false;
        }
        '>' => {
            // End of word: a boundary with a word char behind.
            out.push_str("(?<=\\w)\\b");
            *branch_start = false;
        }
        _ => unreachable!("emit_group_atom only handles ( ) + ? = | {{ < >"),
    }
    Ok(())
}

/// Bare-char dispatch in very-magic (`\v`): grouping / quantifiers are special
/// without a backslash; only word chars and whitespace are literal.
fn emit_very_magic(
    chars: &[char],
    i: &mut usize,
    out: &mut String,
    branch_start: &mut bool,
) -> Result<(), ExError> {
    let c = chars[*i];
    match c {
        '(' | ')' | '|' | '{' | '<' | '>' => {
            *i += 1;
            return emit_group_atom(c, chars, i, out, branch_start);
        }
        '+' | '?' | '=' | '*' if *branch_start => {
            // A leading quantifier is a literal (nothing to repeat).
            escape_literal(c, out);
            *branch_start = false;
        }
        '+' => {
            out.push('+');
            *branch_start = false;
        }
        '?' | '=' => {
            out.push('?');
            *branch_start = false;
        }
        '*' => {
            out.push('*');
            *branch_start = false;
        }
        '.' => {
            out.push('.');
            *branch_start = false;
        }
        '^' => out.push('^'), // anchor; leaves branch_start as-is
        '$' => {
            push_dollar(chars, *i, true, out);
            *branch_start = false;
        }
        '[' => copy_class(chars, i, out, branch_start)?,
        '%' => {
            if chars.get(*i + 1) == Some(&'(') {
                *i += 2;
                out.push_str("(?:");
                *branch_start = true;
                return Ok(());
            }
            return Err(ExError::UnsupportedPattern(format!(
                "%{}",
                chars.get(*i + 1).copied().unwrap_or(' ')
            )));
        }
        '@' => return Err(ExError::UnsupportedPattern("@".to_owned())),
        '&' => return Err(ExError::UnsupportedPattern("&".to_owned())),
        _ => {
            escape_literal(c, out);
            *branch_start = false;
        }
    }
    // The `[` and `%(` arms already advanced `*i`.
    if !matches!(c, '[' | '%') {
        *i += 1;
    }
    Ok(())
}

/// Bare-char dispatch in magic (`\m`, the default): `. * [ ] ^ $` are special;
/// grouping / quantifiers need a backslash (so bare ones are literal).
fn emit_magic(
    chars: &[char],
    i: &mut usize,
    out: &mut String,
    branch_start: &mut bool,
) -> Result<(), ExError> {
    let c = chars[*i];
    match c {
        '.' => {
            out.push('.');
            *branch_start = false;
        }
        '*' => {
            if *branch_start {
                out.push_str("\\*");
            } else {
                out.push('*');
            }
            *branch_start = false;
        }
        '^' => {
            if *branch_start {
                out.push('^'); // leaves branch_start true
            } else {
                out.push_str("\\^");
                *branch_start = false;
            }
        }
        '$' => {
            push_dollar(chars, *i, false, out);
            *branch_start = false;
        }
        '[' => {
            copy_class(chars, i, out, branch_start)?;
            return Ok(()); // copy_class advanced `*i`
        }
        '~' => return Err(ExError::UnsupportedPattern("~".to_owned())),
        _ => {
            escape_literal(c, out);
            *branch_start = false;
        }
    }
    *i += 1;
    Ok(())
}

/// Bare-char dispatch in nomagic (`\M`): only `^ $` special; everything else
/// (`. * [` …) literal.  Backslash forms (`\(`, `\+`, …) still work.
fn emit_nomagic(chars: &[char], i: &mut usize, out: &mut String, branch_start: &mut bool) {
    let c = chars[*i];
    match c {
        '^' if *branch_start => out.push('^'), // leaves branch_start true
        '$' => {
            push_dollar(chars, *i, false, out);
            *branch_start = false;
        }
        _ => {
            escape_literal(c, out);
            *branch_start = false;
        }
    }
    *i += 1;
}

/// Push `$` as an anchor when it ends the pattern (or precedes a branch close
/// `\)` / `\|`, or bare `)` / `|` in very-magic); otherwise as a literal.
fn push_dollar(chars: &[char], i: usize, very: bool, out: &mut String) {
    let rest = &chars[i + 1..];
    let anchor = match rest.first() {
        None => true,
        Some(&')') | Some(&'|') if very => true,
        Some(&'\\') if !very => matches!(rest.get(1), Some(')') | Some('|')),
        _ => false,
    };
    if anchor {
        out.push('$');
    } else {
        out.push_str("\\$");
    }
}

/// Append a pre-formed class / escape string and clear `branch_start`.
fn push_class(out: &mut String, s: &str, branch_start: &mut bool) {
    out.push_str(s);
    *branch_start = false;
}

/// Copy a bracket expression `[…]` verbatim (POSIX `[:alpha:]` classes and
/// most ranges are identical between vim and `regex`).  `*i` points at the
/// opening `[`; on return it points just past the closing `]`.
fn copy_class(
    chars: &[char],
    i: &mut usize,
    out: &mut String,
    branch_start: &mut bool,
) -> Result<(), ExError> {
    out.push('[');
    *i += 1; // consume '['
    if chars.get(*i) == Some(&'^') {
        out.push('^');
        *i += 1;
    }
    // A `]` immediately after `[` (or `[^`) is a literal member.
    if chars.get(*i) == Some(&']') {
        out.push_str("\\]");
        *i += 1;
    }
    while let Some(&c) = chars.get(*i) {
        if c == ']' {
            out.push(']');
            *i += 1;
            *branch_start = false;
            return Ok(());
        }
        if c == '\\' {
            if let Some(&n) = chars.get(*i + 1) {
                out.push('\\');
                out.push(n);
                *i += 2;
                continue;
            }
        }
        out.push(c);
        *i += 1;
    }
    Err(ExError::UnsupportedPattern("unterminated [ ]".to_owned()))
}

/// Read a `{…}` quantifier body (`*i` points just after the `{`) and emit the
/// `regex` form.  Handles vim's lazy `\{-…}`, open-ended `\{n,}` / `\{,m}`,
/// and `\{}` / `\{-}` (= `*` / `*?`).  `*i` ends just past the `}`.
fn take_brace(chars: &[char], i: &mut usize) -> String {
    let mut inner = String::new();
    while let Some(&c) = chars.get(*i) {
        *i += 1;
        if c == '}' {
            break;
        }
        inner.push(c);
    }
    let (lazy, body) = match inner.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, inner.as_str()),
    };
    if body.is_empty() {
        return if lazy {
            "*?".to_owned()
        } else {
            "*".to_owned()
        };
    }
    // Vim's `{,m}` means `{0,m}`.
    let body = if let Some(stripped) = body.strip_prefix(',') {
        format!("0,{stripped}")
    } else {
        body.to_owned()
    };
    if lazy {
        format!("{{{body}}}?")
    } else {
        format!("{{{body}}}")
    }
}

/// Push `c` to `out`, backslash-escaping it when it is a `regex`
/// metacharacter so it matches literally.
fn escape_literal(c: char, out: &mut String) {
    if matches!(
        c,
        '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '\\'
    ) {
        out.push('\\');
    }
    out.push(c);
}

// ── Replacement expansion ───────────────────────────────────────────────────

/// Expand a vim replacement `template` against the match `caps`, applying
/// backreferences (`\1`–`\9`), the whole match (`&` / `\0`), and the case
/// modifiers (`\u \U \l \L \e \E`).  Done by hand rather than via the engine's
/// `$1` syntax because no Rust regex engine implements vim's case folding.
pub fn expand_replacement(template: &str, caps: &Captures) -> String {
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::new();
    // `one` upper/lowercases the next single output char (`\u` / `\l`);
    // `region` does so until `\e` / `\E` (`\U` / `\L`).
    let mut one: Option<bool> = None;
    let mut region: Option<bool> = None;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c == '&' {
            push_group(caps, 0, &mut out, &mut one, region);
            i += 1;
            continue;
        }
        if c != '\\' {
            push_cased(c, &mut out, &mut one, region);
            i += 1;
            continue;
        }
        // Backslash escape.
        i += 1;
        let Some(&d) = chars.get(i) else {
            push_cased('\\', &mut out, &mut one, region);
            break;
        };
        i += 1;
        match d {
            '0'..='9' => push_group(caps, d as usize - '0' as usize, &mut out, &mut one, region),
            '&' => push_cased('&', &mut out, &mut one, region),
            '\\' => push_cased('\\', &mut out, &mut one, region),
            'u' => one = Some(true),
            'l' => one = Some(false),
            'U' => region = Some(true),
            'L' => region = Some(false),
            'e' | 'E' => region = None,
            't' => push_cased('\t', &mut out, &mut one, region),
            'r' | 'n' => push_cased('\n', &mut out, &mut one, region),
            other => push_cased(other, &mut out, &mut one, region),
        }
    }
    out
}

/// Append capture group `n` (empty when it did not participate), applying the
/// active case state to each character.
fn push_group(
    caps: &Captures,
    n: usize,
    out: &mut String,
    one: &mut Option<bool>,
    region: Option<bool>,
) {
    if let Some(m) = caps.get(n) {
        for ch in m.as_str().chars() {
            push_cased(ch, out, one, region);
        }
    }
}

/// Append `ch`, applying a pending one-shot case (`\u`/`\l`, consumed) or the
/// active region case (`\U`/`\L`).  An uppercase/lowercase mapping may expand
/// to several chars (e.g. `ß` → `SS`).
fn push_cased(ch: char, out: &mut String, one: &mut Option<bool>, region: Option<bool>) {
    match one.take().or(region) {
        Some(true) => out.extend(ch.to_uppercase()),
        Some(false) => out.extend(ch.to_lowercase()),
        None => out.push(ch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fancy_regex::Regex;

    fn tr(p: &str) -> String {
        translate_pattern(p).expect("translatable")
    }

    #[test]
    fn literal_text_is_unchanged() {
        assert_eq!(tr("foo"), "foo");
    }

    #[test]
    fn magic_grouping_and_quantifiers() {
        assert_eq!(tr(r"\(foo\)\+"), "(foo)+");
        assert_eq!(tr(r"a\|b"), "a|b");
        assert_eq!(tr(r"\(ab\)\?"), "(ab)?");
        assert_eq!(tr(r"\%(ab\)"), "(?:ab)");
    }

    #[test]
    fn bare_metachars_are_literal_in_magic() {
        // `(`, `+`, `|`, `?` are literals in default magic mode.
        assert_eq!(tr("a+b"), r"a\+b");
        assert_eq!(tr("(x)"), r"\(x\)");
        assert_eq!(tr("a|b"), r"a\|b");
    }

    #[test]
    fn dot_star_anchors_classes() {
        assert_eq!(tr("f.o"), "f.o");
        assert_eq!(tr(r"f\.o"), r"f\.o");
        assert_eq!(tr("ab*"), "ab*");
        assert_eq!(tr("^foo$"), "^foo$");
        assert_eq!(tr("[a-z0-9]"), "[a-z0-9]");
    }

    #[test]
    fn leading_star_is_literal() {
        // Nothing precedes `*`, so it is a literal (and `regex` would error
        // on a bare leading `*`).
        assert_eq!(tr("*x"), r"\*x");
    }

    #[test]
    fn dollar_anchor_vs_literal() {
        assert_eq!(tr("a$"), "a$"); // end → anchor
        assert_eq!(tr(r"a$b"), r"a\$b"); // mid → literal
        assert_eq!(tr(r"\(a$\)"), r"(a$)"); // before `\)` → anchor
    }

    #[test]
    fn word_boundaries_use_lookaround() {
        assert_eq!(tr(r"\<word\>"), r"\b(?=\w)word(?<=\w)\b");
    }

    #[test]
    fn vim_character_classes_expand() {
        assert_eq!(tr(r"\d\+"), r"\d+");
        assert_eq!(tr(r"\a"), "[A-Za-z]");
        assert_eq!(tr(r"\x"), "[0-9A-Fa-f]");
        assert_eq!(tr(r"\l\u"), "[a-z][A-Z]");
    }

    #[test]
    fn very_magic_mode() {
        assert_eq!(tr(r"\v(foo|bar)+"), "(foo|bar)+");
        assert_eq!(tr(r"\va{2,3}"), "a{2,3}");
        // In very-magic, a backslash makes a metachar literal.
        assert_eq!(tr(r"\v\(lit\)"), r"\(lit\)");
    }

    #[test]
    fn nomagic_and_verynomagic() {
        // `\M`: `.` is literal, `\(` still groups.
        assert_eq!(tr(r"\Mfoo.bar"), r"foo\.bar");
        assert_eq!(tr(r"\M\(x\)\+"), "(x)+");
        // `\V`: everything literal.
        assert_eq!(tr(r"\Va.b"), r"a\.b");
    }

    #[test]
    fn lazy_and_bounded_quantifiers() {
        assert_eq!(tr(r"a\{-}"), "a*?");
        assert_eq!(tr(r"a\{2,4}"), "a{2,4}");
        assert_eq!(tr(r"a\{-1,3}"), "a{1,3}?");
        assert_eq!(tr(r"a\{,3}"), "a{0,3}");
        assert_eq!(tr(r"a\{2}"), "a{2}");
    }

    #[test]
    fn pattern_backreference_passes_through() {
        assert_eq!(tr(r"\(.\)\1"), r"(.)\1");
    }

    #[test]
    fn unsupported_atoms_are_rejected() {
        assert!(matches!(
            translate_pattern(r"foo\zsbar"),
            Err(ExError::UnsupportedPattern(_))
        ));
        assert!(matches!(
            translate_pattern(r"\(x\)\@="),
            Err(ExError::UnsupportedPattern(_))
        ));
        assert!(matches!(
            translate_pattern(r"\%[ab]"),
            Err(ExError::UnsupportedPattern(_))
        ));
    }

    #[test]
    fn translated_patterns_compile_and_match() {
        let re = Regex::new(&tr(r"\(\w\+\)\s\+\(\w\+\)")).unwrap();
        assert!(re.is_match("hello   world").unwrap());
        // Backreference round-trips through fancy-regex.
        let dbl = Regex::new(&tr(r"\(.\)\1")).unwrap();
        assert!(dbl.is_match("aa").unwrap());
        assert!(!dbl.is_match("ab").unwrap());
    }

    // ── Replacement expansion ──

    fn caps<'t>(pat: &str, hay: &'t str) -> Captures<'t> {
        Regex::new(pat)
            .unwrap()
            .captures(hay)
            .unwrap()
            .expect("a match")
    }

    #[test]
    fn replacement_backreferences_and_whole_match() {
        let c = caps(r"(\w+) (\w+)", "hello world");
        assert_eq!(expand_replacement(r"\2 \1", &c), "world hello");
        assert_eq!(expand_replacement(r"[&]", &c), "[hello world]");
        assert_eq!(expand_replacement(r"\0!", &c), "hello world!");
    }

    #[test]
    fn replacement_case_modifiers() {
        let c = caps(r"(\w+)", "hello");
        assert_eq!(expand_replacement(r"\U\1", &c), "HELLO");
        assert_eq!(expand_replacement(r"\u\1", &c), "Hello"); // first char only
        assert_eq!(expand_replacement(r"\U\1\E!", &c), "HELLO!");
        let c2 = caps(r"(\w+)", "HELLO");
        assert_eq!(expand_replacement(r"\l\1", &c2), "hELLO");
        assert_eq!(expand_replacement(r"\L\1", &c2), "hello");
    }

    #[test]
    fn replacement_literals_and_escapes() {
        let c = caps(r"(x)", "x");
        assert_eq!(expand_replacement(r"\&", &c), "&"); // literal ampersand
        assert_eq!(expand_replacement(r"a\tb", &c), "a\tb"); // tab
        assert_eq!(expand_replacement(r"\\", &c), "\\"); // literal backslash
    }
}
