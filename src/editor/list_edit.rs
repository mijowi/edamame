//! Markdown list detection, parsing, and structure-editing primitives.
//!
//! Mirrors `table_edit.rs`: byte-oriented, scans buffer text to find the list
//! surrounding the cursor, and produces `EditDelta` values for continue/exit
//! actions and checkbox toggles.  Rope/char-offset conversions happen in
//! `edit_ops`.
//!
//! A "list" here is a contiguous run of item lines at the same indent and
//! marker family (bullet or ordered).  Items may span multiple lines:
//! deeper-indented non-blank lines (continuation paragraphs, nested list
//! lines) and interior blank runs followed by one belong to the item above
//! them.  A blank run followed by a same-indent marker line, or any line at
//! or below the list's own indent that isn't a marker, terminates the run.
//! This keeps cursor detection cheap and means the cursor's list is always
//! the innermost list at the cursor's own indent level — which is what we
//! want for Enter-to-continue and ToggleCheckbox.
//!
//! The implementation is split across two submodules:
//!
//! * [`parse`] — types and primitive parsers (`find_list_at`, etc.)
//! * [`edit`]  — structure-editing primitives that produce `EditDelta`s

pub mod edit;
pub mod parse;

pub use edit::*;
pub use parse::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn info_at(source: &str, cursor_byte: usize) -> ListInfo {
        find_list_at(source, cursor_byte).expect("expected a list at cursor")
    }

    #[test]
    fn finds_simple_bullet_list() {
        let src = "- a\n- b\n- c\n";
        let info = info_at(src, 2); // inside "- a"
        assert_eq!(info.items.len(), 3);
        assert_eq!(info.kind, MarkerKind::Bullet('-'));
        assert_eq!(info.indent, "");
    }

    #[test]
    fn finds_ordered_list_with_numbers() {
        let src = "1. one\n2. two\n3. three\n";
        let info = info_at(src, 5);
        assert_eq!(info.items.len(), 3);
        assert_eq!(info.kind, MarkerKind::Ordered('.'));
        assert_eq!(info.items[0].number, Some(1));
        assert_eq!(info.items[2].number, Some(3));
    }

    #[test]
    fn detects_task_items() {
        let src = "- [ ] todo\n- [x] done\n";
        let info = info_at(src, 3);
        assert_eq!(info.items[0].task, Some(false));
        assert_eq!(info.items[1].task, Some(true));
    }

    #[test]
    fn none_outside_list() {
        let src = "just text\n";
        assert!(find_list_at(src, 5).is_none());
    }

    #[test]
    fn nested_list_scoped_to_indent() {
        let src = "- outer\n  - inner1\n  - inner2\n- outer2\n";
        // Cursor inside "  - inner1" (byte 12)
        let info = info_at(src, 12);
        assert_eq!(info.items.len(), 2);
        assert_eq!(info.indent, "  ");
    }

    #[test]
    fn continue_item_at_end_of_line() {
        let src = "- foo\n";
        let info = info_at(src, 5);
        let res = continue_item(&info, src, 5).expect("continue");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- foo\n- \n");
        assert_eq!(res.cursor_byte, 8);
    }

    #[test]
    fn continue_renumbers_subsequent_ordered_items() {
        let src = "1. a\n2. b\n3. c\n";
        let info = info_at(src, 4);
        let res = continue_item(&info, src, 4).expect("continue");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "1. a\n2. \n3. b\n4. c\n");
    }

    #[test]
    fn exit_list_removes_empty_marker() {
        let src = "- foo\n- \n";
        let info = info_at(src, 8);
        let res = exit_list(&info, src, 8).expect("exit");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- foo\n\n");
    }

    #[test]
    fn exit_list_with_ordered_trailing_renumbers_from_one() {
        // `1. a / 2. (empty cursor) / 3. b / 4. c` — calling `exit_list`
        // directly (no blank line above the empty item) inserts a
        // single newline gap and renumbers the trailing items starting
        // at 1.  The parser's blank-line list split then renders the
        // tail as a fresh ordered list.
        let src = "1. a\n2. \n3. b\n4. c\n";
        let info = info_at(src, 8);
        let res = exit_list(&info, src, 8).expect("exit");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "1. a\n\n1. b\n2. c\n");
        // Cursor lands on the inserted blank line that separates the
        // surviving head from the renumbered trailing list.
        assert_eq!(res.cursor_byte, 5);
    }

    #[test]
    fn exit_list_with_bullet_trailing_keeps_items_unchanged() {
        let src = "- a\n- \n- b\n";
        let info = info_at(src, 5);
        let res = exit_list(&info, src, 5).expect("exit");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- a\n\n- b\n");
        assert_eq!(res.cursor_byte, 4);
    }

    #[test]
    fn exit_list_no_trailing_with_blank_above_strips_only_the_marker() {
        // Triple-`Enter` end state from the dispatcher's perspective:
        // `space_out_empty_item` has already inserted the blank line
        // above the empty item, so `exit_list` only needs to strip the
        // marker.  No extra newline is added — the blank above plus the
        // cursor's now-empty line already provide the two-lines-below
        // resting state.
        let src = "- foo\n\n- ";
        let info = info_at(src, 9);
        let res = exit_list(&info, src, 9).expect("exit");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- foo\n\n");
        assert_eq!(res.cursor_byte, 7);
    }

    #[test]
    fn exit_list_with_blank_above_and_ordered_trailing_renumbers() {
        // Mid-list triple-`Enter` end state for an ordered list: a blank
        // line is already above the empty item, so `exit_list` simply
        // strips the empty marker and renumbers the trailing items from
        // 1.  The pre-existing blank line carries the parser's
        // list-splitting gap between the surviving head and the
        // renumbered tail; the cursor lands on it.
        let src = "1. a\n2. b\n\n3. \n4. c\n";
        let info = info_at(src, 12);
        let res = exit_list(&info, src, 12).expect("exit");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "1. a\n2. b\n\n1. c\n");
        assert_eq!(res.cursor_byte, 10);
    }

    #[test]
    fn space_out_empty_item_inserts_blank_line_above() {
        // Second step of the triple-`Enter` sequence: `Enter` on an empty
        // marker that has no blank line above pushes the marker (and the
        // cursor on it) one line down, leaving the empty item itself in
        // place ready for either real content or the third Enter.
        let src = "- foo\n- ";
        let info = info_at(src, 8);
        let res = space_out_empty_item(&info, src, 8).expect("space");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- foo\n\n- ");
        // Cursor moves down with the empty marker.
        assert_eq!(res.cursor_byte, 9);
    }

    #[test]
    fn space_out_empty_item_rejects_non_empty_item() {
        // The dispatcher only routes empty items here, but be defensive:
        // a non-empty item should fall through to `continue_item`.
        let src = "- foo\n";
        let info = info_at(src, 5);
        assert!(space_out_empty_item(&info, src, 5).is_none());
    }

    #[test]
    fn is_blank_line_above_recognises_blank_predecessor() {
        // First-line items, items after a blank line, items at offsets
        // that don't sit on a line boundary, and items preceded by
        // non-blank content all need to be classified correctly.
        assert!(is_blank_line_above("- foo", 0));
        assert!(is_blank_line_above("- foo\n\n- bar", 7));
        assert!(!is_blank_line_above("- foo\n- bar", 6));
        assert!(!is_blank_line_above("text\n- foo", 5));
    }

    #[test]
    fn toggle_checkbox_flips_state() {
        let src = "- [x] done\n";
        let info = info_at(src, 6);
        let res = toggle_checkbox(&info, src, 6).expect("toggle");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            &res.delta.inserted,
        );
        assert_eq!(out, "- [ ] done\n");
    }

    #[test]
    fn renumber_list_fixes_disordered_numbers() {
        let src = "1. a\n1. b\n1. c\n";
        let delta = renumber_list_block(src, 0).expect("renumber");
        let mut out = src.to_owned();
        out.replace_range(
            delta.offset..delta.offset + delta.removed.len(),
            &delta.inserted,
        );
        assert_eq!(out, "1. a\n2. b\n3. c\n");
    }

    #[test]
    fn renumber_list_noop_when_already_sequential() {
        let src = "1. a\n2. b\n3. c\n";
        assert!(renumber_list_block(src, 0).is_none());
    }

    #[test]
    fn continue_item_rejects_cursor_in_marker() {
        let src = "- foo\n";
        let info = info_at(src, 1); // between `-` and ` `
        assert!(continue_item(&info, src, 1).is_none());
    }

    // ── Multi-line items ──────────────────────────────────────────────────

    #[test]
    fn find_list_includes_continuation_lines() {
        let src = "- a\n  cont\n- b\n";
        let info = info_at(src, 2); // inside "- a"
        assert_eq!(info.items.len(), 2);
        assert_eq!(
            &src[info.items[0].start..info.items[0].end],
            "- a\n  cont\n"
        );
        assert_eq!(
            info.items[0].line_end, 3,
            "line_end stays a first-line fact"
        );
        assert_eq!(&src[info.items[1].start..info.items[1].end], "- b\n");
    }

    #[test]
    fn find_list_from_cursor_on_continuation_line() {
        let src = "- a\n  cont\n- b\n";
        let info = info_at(src, 7); // inside "  cont"
        assert_eq!(info.items.len(), 2);
        assert_eq!(cursor_item_idx(&info, 7), Some(0));
    }

    #[test]
    fn interior_blank_then_continuation_stays_one_item() {
        let src = "- a\n\n  cont\n- b\n";
        let info = info_at(src, 2);
        assert_eq!(info.items.len(), 2);
        assert_eq!(
            &src[info.items[0].start..info.items[0].end],
            "- a\n\n  cont\n",
            "the attached blank run and continuation belong to item 0"
        );
    }

    #[test]
    fn blank_before_same_level_marker_still_ends_scan() {
        let src = "- a\n\n- b\n";
        let info = info_at(src, 2);
        assert_eq!(info.items.len(), 1, "separator blank splits the lists");
        let info_b = info_at(src, 6);
        assert_eq!(info_b.items.len(), 1);
        assert_eq!(info_b.start, 5);
    }

    #[test]
    fn cursor_on_blank_separator_below_list_finds_nothing() {
        // The blank line below a list is outside it — a list edit fired
        // there must not resolve to (and mutate) the item above.
        assert!(find_list_at("- a\n\n- b\n", 4).is_none());
        // Same for the virtual empty line past a trailing final newline.
        assert!(find_list_at("- a\n", 4).is_none());
        // An attached interior blank stays owned by the item above it.
        assert!(find_list_at("- a\n\n  cont\n", 4).is_some());
    }

    #[test]
    fn content_is_empty_false_with_continuation() {
        let src = "- \n  cont\n";
        let info = info_at(src, 2);
        assert!(!info.items[0].content_is_empty(src));
    }

    #[test]
    fn deeper_nested_marker_extends_outer_item() {
        let src = "- a\n  - child\n- b\n";
        let info = info_at(src, 2); // on "- a"
        assert_eq!(info.items.len(), 2);
        assert_eq!(
            &src[info.items[0].start..info.items[0].end],
            "- a\n  - child\n"
        );
        // Anchoring on the nested marker still scopes to the nested list.
        let nested = info_at(src, 6); // on "  - child"
        assert_eq!(nested.indent, "  ");
        assert_eq!(nested.items.len(), 1);
    }

    #[test]
    fn cursor_on_flush_left_non_list_line_finds_nothing() {
        let src = "- a\npara\n";
        assert!(find_list_at(src, 5).is_none());
    }

    #[test]
    fn continuation_shaped_lines_without_marker_above_find_nothing() {
        let src = "para\n  indented\n";
        assert!(find_list_at(src, 7).is_none());
    }

    #[test]
    fn continue_item_mid_first_line_carries_continuations() {
        // Enter between "a" and "b" of the first line: "b" plus the
        // continuation lines move to the new item.
        let src = "- ab\n  cont\n- c\n";
        let info = info_at(src, 3);
        let res = continue_item(&info, src, 3).expect("continues");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "- a\n- b\n  cont\n- c\n");
        assert_eq!(res.cursor_byte, 6); // just past the new "- "
    }

    #[test]
    fn continue_item_at_item_end_appends_sibling() {
        // Enter at the very end of the continuation line appends a new
        // empty sibling after the whole item.
        let src = "- a\n  cont\n";
        let info = info_at(src, 2);
        let res = continue_item(&info, src, 10).expect("continues");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "- a\n  cont\n- \n");
        assert_eq!(res.cursor_byte, 13);
    }

    #[test]
    fn continue_item_mid_continuation_returns_none() {
        let src = "- a\n  cont\n- b\n";
        let info = info_at(src, 7);
        assert!(continue_item(&info, src, 7).is_none());
    }

    #[test]
    fn indent_item_rejects_first_item() {
        // No preceding sibling to nest under → no valid deeper position.
        let src = "- a\n- b\n";
        let info = info_at(src, 2); // on "- a"
        assert!(indent_item(&info, src, 2, 4).is_none());
        // Nested lists too: "  - x" is the first item of its own list.
        let nested = "- top\n  - x\n  - y\n";
        let info = info_at(nested, 10); // on "  - x"
        assert!(indent_item(&info, nested, 10, 4).is_none());
    }

    #[test]
    fn indent_item_shifts_all_item_lines_bullet() {
        let src = "- a\n- b\n  cont\n- c\n";
        let info = info_at(src, 5); // on "- b"
        let res = indent_item(&info, src, 5, 4).expect("indents");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "- a\n    - b\n      cont\n- c\n");
        assert_eq!(
            res.cursor_byte, 9,
            "cursor tracks its char on the marker line"
        );
    }

    #[test]
    fn indent_item_shifts_all_item_lines_ordered() {
        let src = "1. a\n2. b\n   cont\n3. c\n";
        let info = info_at(src, 6); // on "2. b"
        let res = indent_item(&info, src, 6, 4).expect("indents");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "1. a\n    1. b\n       cont\n2. c\n");
    }

    #[test]
    fn outdent_item_shifts_all_item_lines() {
        let src = "- top\n    - b\n      cont\n";
        let info = info_at(src, 11); // on "    - b"
        let res = outdent_item(&info, src, 11, 4).expect("outdents");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "- top\n- b\n  cont\n");
    }

    #[test]
    fn exit_list_preserves_trailing_multiline_items() {
        // Empty item mid-list with a blank line above; the trailing item
        // keeps its continuation line through the renumber.
        let src = "1. a\n\n2. \n3. b\n   cont\n";
        let info = info_at(src, 8); // on the empty "2. "
        let res = exit_list(&info, src, 8).expect("exits");
        let mut out = src.to_owned();
        out.replace_range(
            res.delta.offset..res.delta.offset + res.delta.removed.len(),
            "",
        );
        out.insert_str(res.delta.offset, &res.delta.inserted);
        assert_eq!(out, "1. a\n\n1. b\n   cont\n");
    }

    #[test]
    fn renumber_block_expansion_crosses_continuation_lines() {
        // Cursor on "1. b": the upward expansion must cross "   cont" to
        // reach "1. a" so the run renumbers as one list.
        let src = "1. a\n   cont\n1. b\n";
        let delta = renumber_list_block(src, 14).expect("renumbers");
        let mut out = src.to_owned();
        out.replace_range(delta.offset..delta.offset + delta.removed.len(), "");
        out.insert_str(delta.offset, &delta.inserted);
        assert_eq!(out, "1. a\n   cont\n2. b\n");
        // Cursor on the continuation line renumbers the same block.
        let delta2 = renumber_list_block(src, 6).expect("renumbers from cont line");
        assert_eq!(delta2, delta);
    }
}
