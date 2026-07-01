//! Live `:s` substitution preview — neovim's `inccommand=nosplit`.
//!
//! While the user types a `:s` / `:%s` / `:'<,'>s` command line, the
//! document updates live: matches are highlighted, and once the second
//! delimiter is typed the buffer visually shows the substituted text.
//! Every keystroke goes through [`update_substitute_preview`], which
//! **reverts** the previous preview (restoring the pristine buffer),
//! re-parses the command, and applies a fresh preview — never diffing two
//! previews against each other.  Esc reverts via
//! [`clear_substitute_preview`]; Enter reverts first too, so the real
//! [`execute_substitute`](super::ex::execute_substitute) runs against the
//! untouched buffer and its undo / flash semantics stay byte-identical to
//! a preview-less submit.
//!
//! Preview edits go through the raw [`Buffer`] primitives — never
//! `EditorState::apply_delta` — so no undo delta is recorded and `dirty`
//! is untouched.  The stashed inverse delta is stamped with the
//! `Buffer::version()` it was applied at; a revert on a mismatched
//! version silently drops the preview instead of corrupting text (a
//! safety valve behind the App-level gates: autosave, mouse, and search
//! freshness are all suspended while a preview is active).

use fancy_regex::{Regex, RegexBuilder};

use crate::document::{Buffer, EditDelta};
use crate::editor::vim_ops::ex::{
    build_substitution, parse_ex, resolve_substitute_lines, ExCommand, ExError, Substitution,
};
use crate::editor::vim_ops::vim_regex::translate_pattern;
use crate::editor::EditorState;

/// Preview matches / replacements past this count are left untouched —
/// the delta still rewrites every *scanned* line correctly, later lines
/// simply keep their original text until the user submits.
const MAX_PREVIEW_MATCHES: usize = 1_000;

/// `fancy-regex` backtrack limit for the preview engine only (the commit
/// path keeps the crate default).  A pathological half-typed pattern
/// (`(a+)+b`) must fail fast per keystroke, not hang the UI.
const BACKTRACK_LIMIT: usize = 100_000;

/// Live `:s` preview state.  Lives on [`EditorState`] (like `search` and
/// `yank_flash`) so the overlay painters read it off `&EditorState`.
pub struct SubstitutePreview {
    /// Byte ranges to highlight, valid against the CURRENT (possibly
    /// preview-modified) buffer: match ranges while the replacement field
    /// is absent, the post-apply ranges of each inserted replacement
    /// segment once it is present.  Sorted, non-overlapping.
    pub highlights: Vec<std::ops::Range<usize>>,
    /// Inverse delta restoring the original text.  `None` while the
    /// preview is highlight-only (nothing was edited).
    revert: Option<EditDelta>,
    /// `Buffer::version()` immediately after the preview edit was applied.
    /// A revert is refused (state silently dropped) on mismatch — a
    /// mutation slipped past the gates and the original text is gone.
    applied_version: u64,
    /// Cursor char offset when the preview session started, restored on
    /// cancel (and before submit, so `:s`'s current-line resolution sees
    /// the original cursor).
    saved_cursor: usize,
    /// Viewport scroll when the preview session started, restored on
    /// cancel only (submit lets `execute_substitute` place the view).
    saved_scroll: usize,
}

/// Everything needed to apply one preview frame, computed against an
/// unmodified buffer.  Pure data — the test surface for the preview.
pub struct PreviewPlan {
    /// The combined substitution delta (char offsets).  `Some` only when
    /// the replacement field is present (`:s/foo/…`); `None` for a
    /// highlight-only preview (`:s/foo`).
    pub delta: Option<EditDelta>,
    /// Highlight byte ranges: pre-apply match ranges when `delta` is
    /// `None`, post-apply inserted-segment ranges when it is `Some`
    /// (zero-width segments — a deletion preview — are filtered out).
    pub highlights: Vec<std::ops::Range<usize>>,
    /// First buffer line that matched, for scroll-into-view.
    pub first_line: usize,
}

// ── Compute ─────────────────────────────────────────────────────────────────

/// Compute the preview for one substitution against the (pristine)
/// buffer.  `Ok(None)` means "nothing to preview": empty pattern, empty
/// buffer, or no matches.  Regex errors surface as `Err` — the caller
/// treats them the same as `Ok(None)` (a half-typed pattern must never
/// flash an error), but tests can tell them apart.
pub fn compute_preview_plan(
    buffer: &Buffer,
    cursor_line: usize,
    sub: &Substitution,
    visual_range: Option<(usize, usize)>,
) -> Result<Option<PreviewPlan>, ExError> {
    if sub.pattern.is_empty() {
        return Ok(None);
    }
    let translated = translate_pattern(&sub.pattern)?;
    let re = RegexBuilder::new(&translated)
        .case_insensitive(sub.ignore_case)
        .backtrack_limit(BACKTRACK_LIMIT)
        .build()
        .map_err(|e| ExError::InvalidRegex(e.to_string()))?;

    if sub.replacement_present {
        let Some(edit) = build_substitution(
            buffer,
            cursor_line,
            &re,
            sub,
            visual_range,
            Some(MAX_PREVIEW_MATCHES),
        )?
        else {
            return Ok(None);
        };
        return Ok(Some(PreviewPlan {
            delta: Some(edit.delta),
            // A deletion preview (`:%s/foo/`) inserts nothing — there is
            // no cell to highlight, so zero-width segments are dropped.
            highlights: edit
                .replaced_ranges
                .into_iter()
                .filter(|r| r.start < r.end)
                .collect(),
            first_line: edit.first_match_line,
        }));
    }

    // Highlight-only: the replacement field hasn't been typed yet, so
    // nothing is edited — just collect the ranges the substitution WOULD
    // touch (first match per line without `g`, mirroring what a submit
    // would actually replace).
    let Some((first, last)) =
        resolve_substitute_lines(buffer, cursor_line, sub.range, visual_range)
    else {
        return Ok(None);
    };
    let (highlights, first_match_line) = scan_matches(buffer, first, last, &re, sub.global)?;
    match first_match_line {
        Some(line) => Ok(Some(PreviewPlan {
            delta: None,
            highlights,
            first_line: line,
        })),
        None => Ok(None),
    }
}

/// Collect absolute byte ranges of every match in lines `first..=last`
/// (first match per line unless `global`), capped at
/// [`MAX_PREVIEW_MATCHES`].  Zero-width matches are skipped (nothing to
/// paint).  Returns the ranges and the first line that matched.
#[allow(clippy::type_complexity)]
fn scan_matches(
    buffer: &Buffer,
    first: usize,
    last: usize,
    re: &Regex,
    global: bool,
) -> Result<(Vec<std::ops::Range<usize>>, Option<usize>), ExError> {
    let mut out = Vec::new();
    let mut first_match_line = None;
    for li in first..=last {
        let line_start_byte = buffer.rope().line_to_byte(li);
        let line = buffer.rope().line(li).to_string();
        let content = line.strip_suffix('\n').unwrap_or(&line);
        for m in re.find_iter(content) {
            let m = m.map_err(|e| ExError::InvalidRegex(e.to_string()))?;
            if first_match_line.is_none() {
                first_match_line = Some(li);
            }
            if m.start() < m.end() {
                out.push(line_start_byte + m.start()..line_start_byte + m.end());
            }
            if !global || out.len() >= MAX_PREVIEW_MATCHES {
                break;
            }
        }
        if out.len() >= MAX_PREVIEW_MATCHES {
            break;
        }
    }
    Ok((out, first_match_line))
}

// ── Apply / revert ──────────────────────────────────────────────────────────

/// Re-derive the preview from the current command-line text.  Reverts any
/// existing preview first (so the plan is always computed against the
/// pristine buffer), then parses `input`; on a complete-enough `:s` parse
/// with at least one match, applies the new preview.  Any parse / regex
/// error, non-substitute command, or matchless pattern silently ends the
/// preview session (restoring the saved view) — no error spam mid-typing.
pub fn update_substitute_preview(
    editor: &mut EditorState,
    input: &str,
    visual_range: Option<(usize, usize)>,
    viewport_height: usize,
    viewport_width: usize,
) {
    let (saved_cursor, saved_scroll, had_prior) = match editor.substitute_preview.take() {
        Some(prior) => {
            let saved = (prior.saved_cursor, prior.saved_scroll);
            if !apply_revert(editor, prior) {
                // The revert was refused (version mismatch): a mutation
                // slipped past the gates and the pristine text is gone.
                // End the session outright — computing a "fresh" plan
                // here would stack preview edits on top of the orphaned
                // preview text and stash a revert against that.
                return;
            }
            (saved.0, saved.1, true)
        }
        None => (editor.cursor.offset, editor.scroll, false),
    };

    let plan = match parse_ex(input) {
        Ok(ExCommand::Substitute(sub)) => {
            let cursor_line = editor
                .buffer
                .char_to_line(saved_cursor.min(editor.buffer.len_chars()));
            compute_preview_plan(&editor.buffer, cursor_line, &sub, visual_range)
                .ok()
                .flatten()
        }
        _ => None,
    };
    let Some(plan) = plan else {
        if had_prior {
            restore_saved_view(editor, saved_cursor, Some(saved_scroll));
        }
        return;
    };

    let (revert, applied_version) = match plan.delta {
        Some(delta) => {
            apply_raw(editor, &delta);
            (
                Some(EditDelta {
                    offset: delta.offset,
                    removed: delta.inserted,
                    inserted: delta.removed,
                }),
                editor.buffer.version(),
            )
        }
        None => (None, editor.buffer.version()),
    };

    // Park the cursor at the first affected line and scroll it into view
    // (nvim's inccommand shows the first change).  The cursor is restored
    // from `saved_cursor` when the session ends, and every recompute uses
    // `saved_cursor` for the `:s` current-line resolution, so the park
    // never leaks into semantics.
    let target = editor.buffer.line_to_char(
        plan.first_line
            .min(editor.buffer.line_count().saturating_sub(1)),
    );
    editor.cursor.offset = target.min(editor.buffer.len_chars());
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
    scroll_cursor_into_view(editor, viewport_height, viewport_width);

    editor.substitute_preview = Some(SubstitutePreview {
        highlights: plan.highlights,
        revert,
        applied_version,
        saved_cursor,
        saved_scroll,
    });
}

/// Revert and drop the preview.  The cursor returns to its pre-preview
/// offset (submit's current-line resolution needs it); `restore_view`
/// additionally restores the scroll — `true` on cancel, `false` on the
/// submit path, where `execute_substitute` places the view itself.  When
/// the revert is refused (version mismatch), the saved view is NOT
/// restored — the buffer holds foreign text the saved positions don't
/// belong to.  Returns `true` when a preview session existed.
pub fn clear_substitute_preview(editor: &mut EditorState, restore_view: bool) -> bool {
    let Some(preview) = editor.substitute_preview.take() else {
        return false;
    };
    let saved_cursor = preview.saved_cursor;
    let saved_scroll = preview.saved_scroll;
    if apply_revert(editor, preview) {
        restore_saved_view(editor, saved_cursor, restore_view.then_some(saved_scroll));
    }
    true
}

/// Apply the preview's inverse delta through the raw buffer primitives.
/// Returns `false` when the revert is refused on a version mismatch —
/// some mutation slipped past the gates and the stashed original no
/// longer lines up, so dropping the preview (the caller already
/// `take()`d it) beats corrupting text.  A highlight-only preview
/// (`revert: None`) has nothing to undo and reports success.
fn apply_revert(editor: &mut EditorState, preview: SubstitutePreview) -> bool {
    let Some(revert) = preview.revert else {
        return true;
    };
    if editor.buffer.version() != preview.applied_version {
        return false;
    }
    apply_raw(editor, &revert);
    true
}

/// Apply `delta` via the raw [`Buffer`] edit primitives — no undo delta
/// recorded, `dirty` untouched — then re-parse so the very next frame
/// renders the new text.
fn apply_raw(editor: &mut EditorState, delta: &EditDelta) {
    if !delta.removed.is_empty() {
        let end = delta.offset + delta.removed.chars().count();
        editor
            .buffer
            .remove(delta.offset, end.min(editor.buffer.len_chars()));
    }
    if !delta.inserted.is_empty() {
        editor.buffer.insert(delta.offset, &delta.inserted);
    }
    editor.refresh_parsed();
}

/// Put the cursor (and optionally the scroll) back where the preview
/// session found them.
fn restore_saved_view(editor: &mut EditorState, saved_cursor: usize, saved_scroll: Option<usize>) {
    editor.cursor.offset = saved_cursor.min(editor.buffer.len_chars());
    editor.cursor.preferred_col = editor.cursor.cell_col(&editor.buffer);
    editor.update_cursor_block();
    if let Some(scroll) = saved_scroll {
        editor.scroll = scroll;
    }
}

/// Scroll the cursor's visual row into view with a few rows of context —
/// the same TOP_MARGIN treatment as
/// [`EditorState::scroll_focused_match_into_view`], which can't be reused
/// here because it is gated on an active search session.
fn scroll_cursor_into_view(
    editor: &mut EditorState,
    viewport_height: usize,
    viewport_width: usize,
) {
    if viewport_height == 0 || viewport_width == 0 {
        return;
    }
    const TOP_MARGIN: usize = 3;
    let row = editor.cursor_visual_row(viewport_width);
    let total = editor.total_visual_rows_for_mode(viewport_width);
    let max_scroll = total.saturating_sub(1);
    let comfortably_visible =
        row >= editor.scroll + TOP_MARGIN && row < editor.scroll + viewport_height;
    if !comfortably_visible {
        editor.scroll = row.saturating_sub(TOP_MARGIN).min(max_scroll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;
    use crate::editor::vim_ops::ex::SubstituteRange;

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn editor(text: &str) -> EditorState {
        let mut st = EditorState::new(Buffer::from_str(text), theme());
        st.update_cursor_block();
        st
    }

    /// `rep: None` = replacement field absent (`:s/pat`, highlight-only);
    /// `Some` = present (`:s/pat/rep`, inline preview).
    fn sub(range: SubstituteRange, pat: &str, rep: Option<&str>, global: bool) -> Substitution {
        Substitution {
            range,
            pattern: pat.to_owned(),
            replacement: rep.unwrap_or("").to_owned(),
            replacement_present: rep.is_some(),
            global,
            ignore_case: false,
        }
    }

    // ── compute_preview_plan ────────────────────────────────────────

    #[test]
    fn highlight_only_collects_match_ranges_without_a_delta() {
        let buf = Buffer::from_str("foo bar\nbaz foo\n");
        let plan = compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "foo", None, false),
            None,
        )
        .unwrap()
        .expect("matches exist");
        assert!(plan.delta.is_none(), "no replacement field → no edit");
        // First match per line (no `g` while typing the pattern).
        assert_eq!(plan.highlights, vec![0..3, 12..15]);
        assert_eq!(plan.first_line, 0);
    }

    #[test]
    fn replacement_plan_carries_post_apply_inserted_ranges() {
        let buf = Buffer::from_str("foo foo\nfoo");
        let plan = compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "foo", Some("xy"), true),
            None,
        )
        .unwrap()
        .expect("matches exist");
        let delta = plan.delta.expect("replacement present → delta");
        assert_eq!(delta.offset, 0);
        assert_eq!(delta.removed, "foo foo\nfoo");
        assert_eq!(delta.inserted, "xy xy\nxy");
        // Ranges are absolute byte offsets in the rewritten text.
        assert_eq!(plan.highlights, vec![0..2, 3..5, 6..8]);
    }

    #[test]
    fn deletion_preview_keeps_the_delta_but_drops_zero_width_highlights() {
        let buf = Buffer::from_str("a foo b");
        let plan = compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "foo ", Some(""), false),
            None,
        )
        .unwrap()
        .expect("matches exist");
        assert_eq!(plan.delta.expect("deletion edits").inserted, "a b");
        assert!(
            plan.highlights.is_empty(),
            "zero-width inserted segments have no cell to paint"
        );
    }

    #[test]
    fn multibyte_highlights_stay_on_char_boundaries() {
        let buf = Buffer::from_str("héllo héllo");
        let plan = compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "héllo", None, true),
            None,
        )
        .unwrap()
        .expect("matches exist");
        // "héllo" is 6 bytes; the space pushes the second match to 7.
        assert_eq!(plan.highlights, vec![0..6, 7..13]);
        let src = buf.contents();
        for r in &plan.highlights {
            assert!(src.is_char_boundary(r.start) && src.is_char_boundary(r.end));
        }
    }

    #[test]
    fn empty_pattern_and_no_match_produce_no_plan() {
        let buf = Buffer::from_str("abc");
        assert!(compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "", None, false),
            None
        )
        .unwrap()
        .is_none());
        assert!(compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "zzz", Some("x"), false),
            None
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn invalid_regex_surfaces_as_err() {
        let buf = Buffer::from_str("abc");
        assert!(compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::AllLines, "a\\v(", None, false),
            None
        )
        .is_err());
    }

    #[test]
    fn current_line_and_visual_range_scope_the_scan() {
        let buf = Buffer::from_str("foo\nfoo\nfoo");
        let plan = compute_preview_plan(
            &buf,
            1,
            &sub(SubstituteRange::CurrentLine, "foo", None, false),
            None,
        )
        .unwrap()
        .expect("current line matches");
        assert_eq!(plan.highlights, vec![4..7]);
        let plan = compute_preview_plan(
            &buf,
            0,
            &sub(SubstituteRange::VisualRange, "foo", None, false),
            Some((1, 2)),
        )
        .unwrap()
        .expect("visual range matches");
        assert_eq!(plan.highlights, vec![4..7, 8..11]);
    }

    // ── update / clear round-trip ───────────────────────────────────

    #[test]
    fn update_previews_the_replacement_without_history_or_dirty() {
        let mut st = editor("foo bar\nfoo");
        update_substitute_preview(&mut st, "%s/foo/quux/g", None, 24, 80);
        assert_eq!(st.buffer.contents(), "quux bar\nquux");
        assert!(!st.dirty, "preview must not dirty the buffer");
        let preview = st.substitute_preview.as_ref().expect("preview active");
        assert_eq!(preview.highlights, vec![0..4, 9..13]);
        assert_eq!(
            st.history.undo_depth(),
            0,
            "no undo delta may be recorded for a preview"
        );
    }

    #[test]
    fn every_keystroke_recomputes_against_the_pristine_buffer() {
        let mut st = editor("foo");
        update_substitute_preview(&mut st, "%s/foo/ba", None, 24, 80);
        assert_eq!(st.buffer.contents(), "ba");
        // Next keystroke: longer replacement, derived from the ORIGINAL
        // text, not from the previewed "ba".
        update_substitute_preview(&mut st, "%s/foo/bar", None, 24, 80);
        assert_eq!(st.buffer.contents(), "bar");
        // Backspacing across the second delimiter walks back to
        // deletion-preview, then to highlight-only on the original text.
        update_substitute_preview(&mut st, "%s/foo/", None, 24, 80);
        assert_eq!(st.buffer.contents(), "", "deletion preview");
        update_substitute_preview(&mut st, "%s/foo", None, 24, 80);
        assert_eq!(st.buffer.contents(), "foo");
        let preview = st.substitute_preview.as_ref().expect("highlight-only");
        assert_eq!(preview.highlights, vec![0..3]);
    }

    #[test]
    fn clear_restores_text_cursor_and_scroll() {
        let mut st = editor("one\n\ntwo\n\nfoo");
        st.cursor.offset = 2;
        st.scroll = 1;
        update_substitute_preview(&mut st, "%s/foo/bar/", None, 2, 80);
        assert_eq!(st.buffer.contents(), "one\n\ntwo\n\nbar");
        assert!(clear_substitute_preview(&mut st, /*restore_view=*/ true));
        assert_eq!(st.buffer.contents(), "one\n\ntwo\n\nfoo");
        assert_eq!(st.cursor.offset, 2, "cursor restored");
        assert_eq!(st.scroll, 1, "scroll restored on cancel");
        assert!(st.substitute_preview.is_none());
        assert!(!clear_substitute_preview(&mut st, true), "already cleared");
    }

    #[test]
    fn a_non_substitute_line_ends_the_session_and_restores_the_view() {
        let mut st = editor("foo");
        update_substitute_preview(&mut st, "%s/foo/bar/", None, 24, 80);
        assert_eq!(st.buffer.contents(), "bar");
        // The user backspaces the line down to `:w` — not a substitution.
        update_substitute_preview(&mut st, "w", None, 24, 80);
        assert_eq!(st.buffer.contents(), "foo", "preview reverted");
        assert!(st.substitute_preview.is_none());
    }

    #[test]
    fn version_mismatch_drops_the_preview_without_touching_the_buffer() {
        let mut st = editor("foo");
        update_substitute_preview(&mut st, "%s/foo/bar/", None, 24, 80);
        assert_eq!(st.buffer.contents(), "bar");
        // A mutation slips past the gates (nothing should allow this;
        // the version stamp is the fail-safe).
        st.buffer.insert(0, "X");
        clear_substitute_preview(&mut st, true);
        assert_eq!(
            st.buffer.contents(),
            "Xbar",
            "a mismatched revert must be refused, not misapplied"
        );
        assert!(st.substitute_preview.is_none());
    }

    #[test]
    fn update_after_a_gate_slip_ends_the_session_instead_of_compounding() {
        let mut st = editor("foo");
        update_substitute_preview(&mut st, "%s/foo/bar/", None, 24, 80);
        assert_eq!(st.buffer.contents(), "bar");
        // A mutation slips past the gates mid-session (nothing should
        // allow this; the version stamp is the fail-safe).
        st.buffer.insert(0, "X");
        // The next keystroke's recompute must end the session — a
        // "fresh" plan here would stack preview edits on top of the
        // orphaned preview text and stash a revert against that.
        update_substitute_preview(&mut st, "%s/bar/QQ/", None, 24, 80);
        assert_eq!(
            st.buffer.contents(),
            "Xbar",
            "no new preview may apply on top of orphaned preview text"
        );
        assert!(st.substitute_preview.is_none());
    }
}
