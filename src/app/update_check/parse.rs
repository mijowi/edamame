//! Parsing of GitHub's `releases/latest` response, and the sanitizing
//! that bounds the release notes before they ever leave the worker.
//!
//! Two fields are read out of the body — `tag_name` and `body` — with
//! hand-rolled scans rather than a JSON crate.  That is a deliberate
//! continuation of the policy `parse_tag_name` was written under: the
//! response shape is stable, both fields are plain strings at the top
//! level of a flat object, and a malformed body degrades to "no
//! release" / "no notes" rather than erroring.  A general JSON *value*
//! parser earns its keep on open-ended or nested shapes; all this needs
//! is a JSON *string-literal* decoder, which is what
//! [`decode_json_string`] is.
//!
//! The two fields are not equally easy, though, and the difference is
//! the whole reason this module exists.  A tag name never contains a
//! quote or a backslash, so `parse_tag_name` can scan to the next `"`.
//! The `body` is prose — release notes carrying quotes, backslashes,
//! newlines and emoji — so it needs a real escape-aware walk, or an
//! embedded `\"` terminates the value early and the notes are cut at
//! the first quotation mark the author used.
//!
//! Both scans are anchored by [`top_level_value`] rather than by a bare
//! `find` for the quoted key, because the key text can occur inside
//! another field's *value*: a release `name` is free text somebody
//! types, and `"body"` written in one would otherwise redirect the
//! notes scan to whatever followed it.  Depth tracking excludes the
//! nested `author` / `assets` objects for the same reason.
//!
//! Sanitizing happens **here**, on the worker thread, not at render
//! time: what crosses the channel is already truncated at cargo-dist's
//! install boilerplate, stripped of control characters, and capped in
//! both lines and bytes.  The main thread therefore never holds
//! unbounded remote text, and the modal's layout math cannot be
//! perturbed by a pathological release body.

use super::status::ReleaseInfo;

/// Heading cargo-dist emits directly after the changelog section it
/// prepends.  Everything from here on is install snippets and a
/// download table — machine-generated boilerplate, not release notes.
const INSTALL_HEADING: &str = "## Install";

/// Caps on what reaches the modal.  Generous enough for a normal
/// changelog entry, small enough that a runaway body is a non-event.
const MAX_NOTES_LINES: usize = 30;
const MAX_NOTES_BYTES: usize = 2_000;

/// Cap on an accepted `tag_name`.  Real tags are a handful of bytes;
/// this is loose enough never to reject one and tight enough that the
/// tag can't perturb the modal's layout or bloat the release URL.
const MAX_TAG_BYTES: usize = 64;

/// Appended as its own line when either cap trimmed content, so a
/// clipped summary never reads as a complete one.
const TRUNCATION_MARKER: &str = "…";

/// Parse a release response into the tag and its sanitized notes.
/// `None` when `tag_name` is absent or empty — without a version there
/// is nothing to report.  Missing or unparseable notes are *not* a
/// failure: the tag alone still makes a useful notice.
pub(crate) fn parse_release(body: &str) -> Option<ReleaseInfo> {
    let tag = parse_tag_name(body)?;
    let notes = parse_release_body(body)
        .map(|raw| sanitize_notes(&raw))
        .unwrap_or_default();
    Some(ReleaseInfo { tag, notes })
}

/// The text immediately after `"<key>":` for a key of the *top-level*
/// object, or `None` if there is no such key.
///
/// The depth tracking and the string skipping are the point.  A plain
/// `find("\"body\"")` matches that text anywhere — including inside
/// another field's *value*, and a release `name` is free text a
/// maintainer types, so `x "body":"…"` in one would hand the scan a
/// value the release never had.  Nested objects (`author`, `assets`)
/// are excluded for the same reason: only depth 1 is the release
/// object itself.
///
/// Still not a JSON parser — it identifies one key and hands back the
/// rest of the text; the caller does the reading.  Escapes are skipped
/// rather than decoded, which is all that is needed to know where a
/// string ends.
fn top_level_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let bytes = json.as_bytes();
    let mut i = 0;
    let mut depth: i32 = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            // A string: consume it whole, so no brace or quote inside
            // one is ever read as structure.
            b'"' => {
                let start = i + 1;
                let mut j = start;
                // A `"` byte cannot occur inside a multi-byte UTF-8
                // sequence (continuation bytes are >= 0x80), so `j`
                // lands on a char boundary even if a malformed escape
                // skipped into the middle of one.
                while j < bytes.len() && bytes[j] != b'"' {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                if j >= bytes.len() {
                    return None; // unterminated string: stop reading
                }
                let after = json[j + 1..].trim_start();
                if depth == 1 && &json[start..j] == key {
                    if let Some(value) = after.strip_prefix(':') {
                        return Some(value.trim_start());
                    }
                }
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Characters a tag may contain: the semver alphabet and nothing else.
/// Everything excluded is either meaningless in a version or a problem
/// downstream — whitespace and control characters would break the
/// `Latest release:` row's layout, and `/ ? # % &` and friends would
/// change what [`super::fetch::release_url`] resolves to once the tag is
/// interpolated into it.
fn is_tag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+')
}

/// Extract the `"tag_name": "…"` value from a GitHub release JSON
/// body.  Returns `None` when the field is absent, empty, longer than
/// [`MAX_TAG_BYTES`], or carries a character outside [`is_tag_char`].
/// GitHub tag names never contain escaped quotes, so a plain scan to
/// the next `"` is sufficient once the field itself has been located.
///
/// The validation is the tag's share of the bounding
/// [`sanitize_notes`] does for the release body, and it is owed for the
/// same reason: this is remote text, and it reaches both a rendered
/// line and — via [`super::fetch::release_url`] — a URL handed to the
/// system browser.  Rejecting outright rather than sanitizing is right
/// here because, unlike prose, a tag that isn't a plain tag is not a
/// tag we should be reporting at all.
pub(crate) fn parse_tag_name(body: &str) -> Option<String> {
    let rest = top_level_value(body, "tag_name")?.strip_prefix('"')?;
    let tag = &rest[..rest.find('"')?];
    (!tag.is_empty() && tag.len() <= MAX_TAG_BYTES && tag.chars().all(is_tag_char))
        .then(|| tag.to_owned())
}

/// Extract and unescape the `"body"` field — the release notes as
/// GitHub stores them.  `None` for an absent field, an explicit
/// `"body": null`, or a string that doesn't decode; every one of those
/// means "no notes", never an error.
pub(crate) fn parse_release_body(body: &str) -> Option<String> {
    // `null` (and anything else that isn't a string) fails here.
    let rest = top_level_value(body, "body")?.strip_prefix('"')?;
    decode_json_string(rest)
}

/// Decode a JSON string literal, given the text immediately after its
/// opening quote.  Stops at the first *unescaped* `"`.
///
/// Returns `None` on an unterminated string or an escape JSON does not
/// define — both mean the scan is not reading what it thinks it is, and
/// guessing past that point would put arbitrary bytes in the modal.
fn decode_json_string(rest: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{0008}'),
                'f' => out.push('\u{000C}'),
                'u' => out.push(decode_unicode_escape(&mut chars)?),
                _ => return None,
            },
            _ => out.push(c),
        }
    }
    None
}

/// Decode the four hex digits following a `\u`, combining a surrogate
/// pair when a low surrogate follows as its own escape.
///
/// An unpaired or malformed surrogate yields U+FFFD rather than failing
/// the whole parse: release notes routinely carry emoji, and one
/// mangled code unit should cost a glyph, not the entire summary.
fn decode_unicode_escape(chars: &mut std::str::Chars<'_>) -> Option<char> {
    let first = take_hex4(chars)?;
    if !(0xD800..=0xDBFF).contains(&first) {
        return Some(char::from_u32(u32::from(first)).unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    // High surrogate: only a `\uXXXX` low surrogate completes it.
    // Probe on a clone so a non-match leaves the real iterator put.
    if let Some(after) = chars.as_str().strip_prefix("\\u") {
        let mut probe = after.chars();
        if let Some(low) = take_hex4(&mut probe) {
            if (0xDC00..=0xDFFF).contains(&low) {
                let cp = 0x1_0000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
                *chars = probe;
                return Some(char::from_u32(cp).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
        }
    }
    Some(char::REPLACEMENT_CHARACTER)
}

/// Read exactly four hex digits as a UTF-16 code unit.
fn take_hex4(chars: &mut std::str::Chars<'_>) -> Option<u16> {
    let mut value: u32 = 0;
    for _ in 0..4 {
        value = value * 16 + chars.next()?.to_digit(16)?;
    }
    u16::try_from(value).ok()
}

/// Characters that paint no glyph of their own but change how the text
/// around them reads — bidi overrides and isolates, zero-width spaces
/// and joiners, the BOM.
///
/// `char::is_control` does not cover them (they are `Cf`, not `Cc`), so
/// stripping only control characters would leave a release body able to
/// reverse or hide part of a line in the modal — the one thing left
/// that remote text could still do to a surface that never re-parses
/// it as Markdown.  Dropping them costs nothing: release notes are
/// left-to-right prose here, already rendered as literal lines.
fn is_invisible_formatting(c: char) -> bool {
    matches!(c,
        '\u{00AD}'                 // soft hyphen
        | '\u{061C}'               // Arabic letter mark
        | '\u{200B}'..='\u{200F}'  // zero-width space/joiners, LRM/RLM
        | '\u{202A}'..='\u{202E}'  // bidi embeddings and overrides
        | '\u{2060}'..='\u{2064}'  // word joiner, invisible operators
        | '\u{2066}'..='\u{2069}'  // bidi isolates
        | '\u{FEFF}'               // BOM / zero-width no-break space
    )
}

/// Bound raw release notes for display: cut at cargo-dist's install
/// boilerplate, drop control characters, trim surrounding blank lines,
/// and cap both the line count and the total size.
///
/// The result is plain text.  It is deliberately *not* Markdown-parsed
/// anywhere downstream — this is remote text, and rendering it as
/// literal lines is what keeps a release body from injecting styling,
/// links, or layout into the modal.
pub(crate) fn sanitize_notes(raw: &str) -> Vec<String> {
    let head = match raw.find(INSTALL_HEADING) {
        Some(i) => &raw[..i],
        None => raw,
    };

    let mut lines: Vec<String> = head
        .lines()
        .map(|line| {
            // A tab becomes a space rather than vanishing, so indented
            // list items keep their shape; every other control
            // character — and every invisible formatting one — is
            // dropped.
            line.chars()
                .map(|c| if c == '\t' { ' ' } else { c })
                .filter(|c| !c.is_control() && !is_invisible_formatting(*c))
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();

    let first_content = lines
        .iter()
        .position(|l| !l.is_empty())
        .unwrap_or(lines.len());
    lines.drain(..first_content);
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }

    let mut truncated = lines.len() > MAX_NOTES_LINES;
    lines.truncate(MAX_NOTES_LINES);

    let mut bytes = 0usize;
    let mut kept: Vec<String> = Vec::with_capacity(lines.len());
    for line in lines {
        bytes += line.len() + 1;
        if bytes > MAX_NOTES_BYTES {
            truncated = true;
            break;
        }
        kept.push(line);
    }
    if truncated {
        kept.push(TRUNCATION_MARKER.to_owned());
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape cargo-dist produces today: no `CHANGELOG.md` in
    /// the repo, so the body is install boilerplate start to finish.
    const CARGO_DIST_BOILERPLATE: &str = "## Install edamame 0.1.0\n\n\
         ### Install prebuilt binaries via shell script\n\n\
         ```sh\ncurl -LsSf https://example/edamame-installer.sh | sh\n```\n\n\
         ## Download edamame 0.1.0\n\n| File | Platform |\n";

    #[test]
    fn parse_tag_name_extracts_from_release_json() {
        let body = r#"{"url":"https://api.github.com/…","tag_name":"v0.2.1","name":"v0.2.1"}"#;
        assert_eq!(parse_tag_name(body), Some("v0.2.1".to_owned()));
    }

    #[test]
    fn parse_tag_name_tolerates_whitespace_around_colon() {
        let body = "{\n  \"tag_name\" :  \"v1.0.0\"\n}";
        assert_eq!(parse_tag_name(body), Some("v1.0.0".to_owned()));
    }

    #[test]
    fn parse_tag_name_rejects_missing_or_empty_field() {
        assert_eq!(parse_tag_name(r#"{"message":"Not Found"}"#), None);
        assert_eq!(parse_tag_name(r#"{"tag_name":""}"#), None);
        assert_eq!(parse_tag_name(""), None);
        assert_eq!(parse_tag_name(r#"{"tag_name":42}"#), None);
    }

    #[test]
    fn parse_tag_name_accepts_the_semver_alphabet() {
        for tag in [
            "v0.2.1",
            "0.2.1",
            "v1.0.0-rc.1",
            "v1.0.0+build.7",
            "release_2",
        ] {
            let body = format!(r#"{{"tag_name":"{tag}"}}"#);
            assert_eq!(parse_tag_name(&body).as_deref(), Some(tag));
        }
    }

    #[test]
    fn parse_tag_name_rejects_a_tag_carrying_url_or_layout_characters() {
        // Every one of these reaches `release_url` and a rendered line,
        // so a tag that isn't a plain tag is refused rather than
        // patched up.
        for tag in [
            r"v1.0/../../evil",
            "v1.0 0",
            "v1.0?x=1",
            "v1.0#frag",
            "v1.0%2e%2e",
            r"v1.0\u0000",
        ] {
            let body = format!(r#"{{"tag_name":"{tag}"}}"#);
            assert_eq!(parse_tag_name(&body), None, "should reject {tag:?}");
        }
    }

    #[test]
    fn parse_tag_name_rejects_an_overlong_tag() {
        let long = "v".to_owned() + &"1".repeat(MAX_TAG_BYTES);
        let body = format!(r#"{{"tag_name":"{long}"}}"#);
        assert_eq!(parse_tag_name(&body), None);
        // One byte under the cap still parses.
        let ok = "v".to_owned() + &"1".repeat(MAX_TAG_BYTES - 1);
        let body = format!(r#"{{"tag_name":"{ok}"}}"#);
        assert_eq!(parse_tag_name(&body).as_deref(), Some(ok.as_str()));
    }

    #[test]
    fn a_rejected_tag_drops_the_whole_release() {
        // No tag, no release — the notes alone are not a finding.
        let body = r#"{"tag_name":"v1.0 beta","body":"- a thing"}"#;
        assert_eq!(parse_release(body), None);
    }

    #[test]
    fn parse_release_body_unescapes_newlines_quotes_and_backslashes() {
        let json = r#"{"body":"- a \"quoted\" thing\n- a back\\slash\n- done"}"#;
        assert_eq!(
            parse_release_body(json).as_deref(),
            Some("- a \"quoted\" thing\n- a back\\slash\n- done"),
        );
    }

    #[test]
    fn parse_release_body_does_not_stop_at_an_escaped_quote() {
        // The naive `find('"')` scan `parse_tag_name` uses would cut
        // this at `the `, losing the rest of the notes.
        let json = r#"{"body":"say \"hi\" now","name":"x"}"#;
        assert_eq!(parse_release_body(json).as_deref(), Some("say \"hi\" now"));
    }

    #[test]
    fn parse_release_body_decodes_unicode_and_surrogate_pairs() {
        let json = r#"{"body":"café 😀 done"}"#;
        assert_eq!(parse_release_body(json).as_deref(), Some("café 😀 done"));
    }

    #[test]
    fn parse_release_body_replaces_an_unpaired_surrogate() {
        let json = r#"{"body":"broken \ud83d end"}"#;
        assert_eq!(
            parse_release_body(json).as_deref(),
            Some("broken \u{FFFD} end")
        );
    }

    #[test]
    fn a_field_key_written_inside_another_value_is_not_mistaken_for_it() {
        // A release `name` is free text, and it precedes `body` in
        // GitHub's response — a bare `find("\"body\"")` would read the
        // notes out of the middle of it.
        let json =
            r#"{"tag_name":"v0.2.0","name":"the \"body\":\"decoy\" release","body":"real notes"}"#;
        assert_eq!(parse_release_body(json).as_deref(), Some("real notes"));
        assert_eq!(parse_tag_name(json), Some("v0.2.0".to_owned()));
    }

    #[test]
    fn a_nested_object_does_not_supply_the_top_level_fields() {
        // `author` and each `assets` entry are objects of their own;
        // only the release object itself is depth 1.
        let json = r#"{"author":{"body":"nested","tag_name":"v9.9.9"},"assets":[{"body":"asset"}],"tag_name":"v0.2.0","body":"real notes"}"#;
        assert_eq!(parse_tag_name(json), Some("v0.2.0".to_owned()));
        assert_eq!(parse_release_body(json).as_deref(), Some("real notes"));
    }

    #[test]
    fn a_key_that_is_only_ever_a_value_is_not_a_match() {
        // "body" as somebody's *value*, with no top-level key of that
        // name anywhere: nothing to report, rather than the string
        // that happened to follow it.
        assert_eq!(parse_release_body(r#"{"name":"body","x":"y"}"#), None);
    }

    #[test]
    fn parse_release_body_returns_none_for_null_missing_or_unterminated() {
        assert_eq!(parse_release_body(r#"{"body":null}"#), None);
        assert_eq!(parse_release_body(r#"{"tag_name":"v1"}"#), None);
        assert_eq!(parse_release_body(r#"{"body":"unterminated"#), None);
        // `\q` is not a JSON escape.
        assert_eq!(parse_release_body(r#"{"body":"bad \q escape"}"#), None);
    }

    #[test]
    fn sanitize_notes_cuts_at_the_install_heading() {
        let raw = "### Added\n- startup update check\n\n## Install edamame 0.2.0\n\ncurl …";
        assert_eq!(
            sanitize_notes(raw),
            vec!["### Added".to_owned(), "- startup update check".to_owned()],
        );
    }

    #[test]
    fn sanitize_notes_is_empty_for_a_release_with_no_changelog_section() {
        // Today's real release body: boilerplate from the first byte,
        // so there is nothing before the heading to show.
        assert!(sanitize_notes(CARGO_DIST_BOILERPLATE).is_empty());
    }

    #[test]
    fn sanitize_notes_keeps_the_whole_body_when_the_heading_is_absent() {
        assert_eq!(
            sanitize_notes("### Fixed\n- a bug"),
            vec!["### Fixed", "- a bug"]
        );
    }

    #[test]
    fn sanitize_notes_strips_control_chars_and_trims_blank_edges() {
        let raw = "\n\n### Added\r\n- tab\there\n\u{7}bell\n\n\n";
        assert_eq!(
            sanitize_notes(raw),
            vec![
                "### Added".to_owned(),
                "- tab here".to_owned(),
                "bell".to_owned(),
            ],
        );
    }

    #[test]
    fn sanitize_notes_strips_invisible_formatting_characters() {
        // Cf characters are not `is_control`, so they survive a
        // control-only filter — and a bidi override can reverse the
        // rest of the line in the modal.
        let raw = "- safe\u{202E}reversed\u{202C}\n- zero\u{200B}width\u{FEFF}";
        assert_eq!(
            sanitize_notes(raw),
            vec!["- safereversed".to_owned(), "- zerowidth".to_owned()],
        );
    }

    #[test]
    fn sanitize_notes_caps_the_line_count_and_marks_the_cut() {
        let raw = (0..MAX_NOTES_LINES + 10)
            .map(|i| format!("- line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = sanitize_notes(&raw);
        assert_eq!(
            out.len(),
            MAX_NOTES_LINES + 1,
            "capped lines plus the marker"
        );
        assert_eq!(out.last().map(String::as_str), Some(TRUNCATION_MARKER));
    }

    #[test]
    fn sanitize_notes_caps_total_bytes_and_marks_the_cut() {
        let raw = (0..MAX_NOTES_LINES)
            .map(|_| "x".repeat(300))
            .collect::<Vec<_>>()
            .join("\n");
        let out = sanitize_notes(&raw);
        assert_eq!(out.last().map(String::as_str), Some(TRUNCATION_MARKER));
        let bytes: usize = out.iter().map(|l| l.len() + 1).sum();
        assert!(
            bytes <= MAX_NOTES_BYTES + TRUNCATION_MARKER.len() + 1,
            "kept {bytes} bytes"
        );
    }

    #[test]
    fn parse_release_pairs_the_tag_with_its_notes() {
        let json =
            r#"{"tag_name":"v0.2.0","body":"Added:\n- a thing\n\n## Install edamame 0.2.0\ncurl"}"#;
        let info = parse_release(json).expect("release");
        assert_eq!(info.tag, "v0.2.0");
        assert_eq!(info.notes, vec!["Added:", "- a thing"]);
    }

    #[test]
    fn parse_release_survives_notes_it_cannot_read() {
        // A tag with an unreadable body is still a usable notice.
        let json = r#"{"tag_name":"v0.2.0","body":null}"#;
        let info = parse_release(json).expect("release");
        assert_eq!(info.tag, "v0.2.0");
        assert!(info.notes.is_empty());
    }

    #[test]
    fn parse_release_needs_a_tag() {
        assert!(parse_release(r#"{"body":"notes"}"#).is_none());
    }
}
