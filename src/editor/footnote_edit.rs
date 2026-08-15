//! Pure document-wide footnote edit primitives.
//!
//! Each public function takes the raw buffer text and returns an
//! [`EditDelta`] (byte-offset based, ready for
//! `edit_ops::apply_byte_delta`) or `None` when nothing changes.  No
//! `EditorState` is touched here — the layering mirrors
//! [`crate::editor::list_edit`] and [`crate::editor::table_edit`].
//!
//! Footnote numbering is numeric and follows GFM order-of-first-reference
//! semantics; named labels (`[^note]`) are recognized for navigation and
//! deletion but left untouched by auto-numbering and renumbering.

use std::collections::HashMap;
use std::ops::Range;

use crate::document::EditDelta;
use crate::markdown::parse_offsets;

/// One `[^label]` occurrence in the source.  Shared with
/// `mouse_ops::footnotes` so the click/keyboard hit-test and the edit
/// primitives scan footnote syntax through one implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Site {
    pub(crate) label: String,
    /// Byte offset of the opening `[`.
    pub(crate) start: usize,
    /// Byte offset just past the closing `]` (excludes a definition's
    /// trailing `:`).
    pub(crate) end: usize,
    /// `true` when this is a definition leader (`[^label]:` at line start).
    pub(crate) is_definition: bool,
}

/// Scan `source` for every `[^label]` reference and `[^label]:`
/// definition, in document order.  When called on a single line slice the
/// offsets are line-relative — `mouse_ops::footnotes` relies on that for
/// hit-testing.
pub(crate) fn scan(source: &str) -> Vec<Site> {
    let bytes = source.as_bytes();
    let mut sites = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'^' && !is_escaped(bytes, i) {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] != b']' && bytes[j] != b'\n' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b']' {
                let label = source[i + 2..j].to_string();
                if !label.is_empty() {
                    let end = j + 1;
                    let is_definition = bytes.get(end) == Some(&b':') && is_line_start(source, i);
                    sites.push(Site {
                        label,
                        start: i,
                        end,
                        is_definition,
                    });
                    i = end;
                    continue;
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    sites
}

/// True when the byte at `pos` is backslash-escaped: an odd number of
/// consecutive `\` immediately precede it (`\[` is literal, `\\[` is a
/// literal backslash followed by a live `[`).
///
/// Shared by the Markdown inline scanners (`scan` here and
/// [`mouse_ops::links::link_at_offset`]) so an escaped `\[^1]` / `\[x](y)`
/// is never mistaken for a live footnote or link.
///
/// [`mouse_ops::links::link_at_offset`]: crate::editor::mouse_ops::link_at_offset
pub(crate) fn is_escaped(bytes: &[u8], pos: usize) -> bool {
    let mut backslashes = 0usize;
    let mut k = pos;
    while k > 0 && bytes[k - 1] == b'\\' {
        backslashes += 1;
        k -= 1;
    }
    backslashes % 2 == 1
}

/// True when only whitespace precedes byte `pos` on its line (GFM allows
/// a small indent before a definition leader).
fn is_line_start(source: &str, pos: usize) -> bool {
    let line_start = source[..pos].rfind('\n').map(|n| n + 1).unwrap_or(0);
    source[line_start..pos]
        .chars()
        .all(|c| c == ' ' || c == '\t')
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// The next unused integer footnote number — one past the highest numeric
/// label present (references or definitions), or 1 when there are none.
pub fn next_footnote_number(source: &str) -> u32 {
    scan(source)
        .iter()
        .filter_map(|s| s.label.parse::<u32>().ok())
        .max()
        .map(|m| m + 1)
        .unwrap_or(1)
}

/// Insert an auto-numbered `[^N]` reference at `cursor_byte`.  Returns the
/// delta plus the byte offset the cursor should land on (just past the
/// inserted marker).
pub fn insert_footnote(source: &str, cursor_byte: usize) -> (EditDelta, usize) {
    let n = next_footnote_number(source);
    let inserted = format!("[^{n}]");
    let cursor_target = cursor_byte + inserted.len();
    let delta = EditDelta {
        offset: cursor_byte,
        removed: String::new(),
        inserted,
    };
    (delta, cursor_target)
}

/// Label of the footnote reference or definition at `cursor_byte`, if any.
pub fn label_at(source: &str, cursor_byte: usize) -> Option<String> {
    scan(source)
        .into_iter()
        .find(|s| {
            // The definition hit span includes the trailing `:`.
            let end = if s.is_definition { s.end + 1 } else { s.end };
            cursor_byte >= s.start && cursor_byte <= end
        })
        .map(|s| s.label)
}

/// Re-sequence all numeric footnotes into order-of-first-reference (GFM).
/// Named labels are left untouched.  Returns `None` when no numeric
/// footnote needs renumbering (already in order, or none present).
pub fn renumber_footnotes(source: &str) -> Option<EditDelta> {
    let sites = scan(source);
    let mapping = numeric_renumber_mapping(&sites);
    if mapping.is_empty() {
        return None;
    }
    let new = rewrite_labels(source, &sites, &mapping);
    EditDelta::diff(source, &new)
}

/// Remove every reference to `label` plus its definition, then renumber
/// the remaining numeric footnotes — all as one atomic delta.  Returns
/// `None` when `label` is absent.
///
/// The definition is removed by its parsed [`FootnoteDefinition`] block
/// byte range, not a single-line scan, so a multi-line definition (an
/// indented continuation body, nested blocks) is removed in full rather
/// than leaving its tail orphaned as an indented code block.
///
/// [`FootnoteDefinition`]: crate::markdown::ast::Block::FootnoteDefinition
pub fn delete_footnote(source: &str, label: &str) -> Option<EditDelta> {
    let sites = scan(source);
    if !sites.iter().any(|s| s.label == label) {
        return None;
    }

    // Collect the byte spans to remove: every reference marker for this
    // label, plus the full block range of each matching definition.
    let mut spans: Vec<Range<usize>> = sites
        .iter()
        .filter(|s| s.label == label && !s.is_definition)
        .map(|s| s.start..s.end)
        .collect();
    for (lbl, range) in parse_offsets::footnote_definition_ranges(source) {
        if lbl == label {
            spans.push(range);
        }
    }

    // Merge overlapping/adjacent spans (a reference can sit inside the
    // definition body) so the strip pass below stays monotonic.
    spans.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for sp in spans {
        match merged.last_mut() {
            Some(last) if sp.start <= last.end => last.end = last.end.max(sp.end),
            _ => merged.push(sp),
        }
    }

    // First pass: strip the merged spans.
    let mut stripped = String::with_capacity(source.len());
    let mut last = 0;
    for sp in &merged {
        stripped.push_str(&source[last..sp.start]);
        last = sp.end;
    }
    stripped.push_str(&source[last..]);

    // Second pass: renumber the remaining numeric footnotes so the
    // sequence stays contiguous.
    let renumbered = match renumber_footnotes(&stripped) {
        Some(d) => d.apply_to_string(&stripped),
        None => stripped,
    };

    EditDelta::diff(source, &renumbered)
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Build the old-label → new-number map for numeric footnotes: referenced
/// labels first, in order of first reference; then numeric definitions that
/// were never referenced, in document order.
fn numeric_renumber_mapping(sites: &[Site]) -> HashMap<String, usize> {
    fn push_if_numeric(label: &str, order: &mut Vec<String>) {
        if label.parse::<u32>().is_ok() && !order.iter().any(|l| l == label) {
            order.push(label.to_string());
        }
    }
    let mut order: Vec<String> = Vec::new();
    for s in sites.iter().filter(|s| !s.is_definition) {
        push_if_numeric(&s.label, &mut order);
    }
    for s in sites.iter().filter(|s| s.is_definition) {
        push_if_numeric(&s.label, &mut order);
    }
    order
        .into_iter()
        .enumerate()
        .map(|(i, label)| (label, i + 1))
        .collect()
}

/// Rebuild `source` with each site's label replaced per `mapping` (sites
/// whose label isn't in the map are left verbatim).  The trailing `:` of a
/// definition is preserved because it sits past `site.end`.
fn rewrite_labels(source: &str, sites: &[Site], mapping: &HashMap<String, usize>) -> String {
    let mut out = String::with_capacity(source.len());
    let mut last = 0;
    for s in sites {
        if let Some(num) = mapping.get(&s.label) {
            out.push_str(&source[last..s.start]);
            out.push_str(&format!("[^{num}]"));
            last = s.end;
        }
    }
    out.push_str(&source[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_number_continues_from_highest() {
        assert_eq!(next_footnote_number("a[^1] b[^3]\n\n[^1]: x\n[^3]: y\n"), 4);
        assert_eq!(next_footnote_number("no footnotes here\n"), 1);
        // Named labels don't affect the numeric sequence.
        assert_eq!(next_footnote_number("a[^note] b[^2]\n"), 3);
    }

    #[test]
    fn insert_places_marker_and_cursor() {
        let src = "Claim.\n\n[^1]: prior.\n";
        let cursor = "Claim.".len(); // right after "Claim."
        let (delta, target) = insert_footnote(src, cursor);
        assert_eq!(delta.inserted, "[^2]");
        assert_eq!(delta.removed, "");
        assert_eq!(delta.offset, cursor);
        assert_eq!(target, cursor + "[^2]".len());
        let out = delta.apply_to_string(src);
        assert!(out.starts_with("Claim.[^2]"));
    }

    #[test]
    fn label_at_finds_reference_and_definition() {
        let src = "see[^1] x\n\n[^1]: note\n";
        let ref_byte = src.find("[^1]").unwrap() + 1;
        assert_eq!(label_at(src, ref_byte).as_deref(), Some("1"));
        let def_byte = src.rfind("[^1]").unwrap(); // the definition `[`
        assert_eq!(label_at(src, def_byte).as_deref(), Some("1"));
        assert_eq!(label_at(src, src.find("see").unwrap()), None);
    }

    #[test]
    fn renumber_orders_by_first_reference() {
        // `2` is referenced before `1`; renumber should swap them.
        let src = "A[^2] B[^1]\n\n[^2]: two\n[^1]: one\n";
        let delta = renumber_footnotes(src).expect("renumber needed");
        let out = delta.apply_to_string(src);
        assert_eq!(out, "A[^1] B[^2]\n\n[^1]: two\n[^2]: one\n");
    }

    #[test]
    fn renumber_leaves_named_labels_untouched() {
        let src = "A[^note] B[^2]\n\n[^note]: n\n[^2]: two\n";
        let delta = renumber_footnotes(src).expect("renumber needed");
        let out = delta.apply_to_string(src);
        // Only the numeric `2` re-sequences to `1`; `note` stays.
        assert_eq!(out, "A[^note] B[^1]\n\n[^note]: n\n[^1]: two\n");
    }

    #[test]
    fn renumber_returns_none_when_already_sequential() {
        let src = "A[^1] B[^2]\n\n[^1]: one\n[^2]: two\n";
        assert_eq!(renumber_footnotes(src), None);
    }

    #[test]
    fn delete_removes_refs_and_definition_then_renumbers() {
        // Delete footnote 1; footnote 2 should renumber down to 1.
        let src = "A[^1] B[^2] C[^1]\n\n[^1]: one\n[^2]: two\n";
        let delta = delete_footnote(src, "1").expect("delete needed");
        let out = delta.apply_to_string(src);
        assert_eq!(out, "A B[^1] C\n\n[^1]: two\n");
    }

    #[test]
    fn delete_missing_label_is_noop() {
        let src = "A[^1]\n\n[^1]: one\n";
        assert_eq!(delete_footnote(src, "9"), None);
    }

    #[test]
    fn escaped_reference_is_not_a_footnote() {
        // `\[^1]` is escaped literal text — scan must skip it, so it doesn't
        // count toward numbering and isn't deletable as a footnote.
        assert!(scan(r"an \[^1] escaped marker").is_empty());
        assert_eq!(next_footnote_number(r"a \[^5] b"), 1);
        // A doubled backslash leaves the `[^2]` live (`\\` is a literal
        // backslash, the bracket is not escaped).
        let sites = scan(r"a \\[^2] b");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].label, "2");
    }

    #[test]
    fn is_escaped_counts_backslashes() {
        assert!(is_escaped(br"\[", 1)); // one backslash → escaped
        assert!(!is_escaped(br"\\[", 2)); // two → not escaped
        assert!(is_escaped(br"\\\[", 3)); // three → escaped
        assert!(!is_escaped(b"[", 0)); // none → not escaped
    }

    #[test]
    fn delete_removes_multiline_definition_in_full() {
        // A definition with an indented continuation line must be removed
        // entirely — the old single-line scan left the continuation behind
        // as an orphaned indented code block.
        let src = "A[^1] end\n\n[^1]: first line\n    continuation line\n";
        let delta = delete_footnote(src, "1").expect("delete needed");
        let out = delta.apply_to_string(src);
        assert_eq!(out, "A end\n\n");
        assert!(
            !out.contains("continuation line"),
            "continuation line should not be orphaned: {out:?}"
        );
    }

    #[test]
    fn delete_handles_reference_inside_definition_body() {
        // A self-reference inside the deleted definition's body is covered
        // by the definition's block range; the span merge avoids
        // double-removal.
        let src = "A[^1] B[^2]\n\n[^1]: see [^1] again\n[^2]: two\n";
        let delta = delete_footnote(src, "1").expect("delete needed");
        let out = delta.apply_to_string(src);
        assert_eq!(out, "A B[^1]\n\n[^1]: two\n");
    }
}
