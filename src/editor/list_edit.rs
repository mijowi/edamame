//! Markdown list detection, parsing, and structure-editing primitives.
//!
//! Mirrors `table_edit.rs`: byte-oriented, scans buffer text to find the list
//! surrounding the cursor, and produces `EditDelta` values for continue/exit
//! actions and checkbox toggles.  Rope/char-offset conversions happen in
//! `edit_ops`.
//!
//! A "list" here is a contiguous run of item lines at the same indent and
//! marker family (bullet or ordered).  Blank lines or lines at a different
//! indent terminate the run.  This keeps cursor detection cheap and means the
//! cursor's list is always the innermost list at the cursor's own indent level
//! — which is what we want for Enter-to-continue and ToggleCheckbox.
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
        let info = info_at(src, 0);
        let delta = renumber_list(&info, src).expect("renumber");
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
        let info = info_at(src, 0);
        assert!(renumber_list(&info, src).is_none());
    }

    #[test]
    fn continue_item_rejects_cursor_in_marker() {
        let src = "- foo\n";
        let info = info_at(src, 1); // between `-` and ` `
        assert!(continue_item(&info, src, 1).is_none());
    }
}
