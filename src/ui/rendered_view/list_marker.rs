/// Map a raw-column on a list-item line to its rendered column.  Returns
/// `None` when `raw_text` isn't recognized as a list-item line — callers
/// fall back to treating raw-col as visual-col.
///
/// Needed because the rendered marker width can differ from the raw
/// marker width:
///
///   - task items: raw `- [ ] foo` → rendered `[ ] foo` (the `- ` prefix
///     is dropped; the checkbox is the visual anchor instead).
///   - ordered items with 10+ items: raw `1. foo` → rendered ` 1. foo`
///     (numbers are right-aligned in a max-digit-wide slot).
///
/// Both cases shift the content column, so the jitter-delay cursor
/// indicator in Rendered mode must be drawn at the correct rendered
/// column — not the raw column.
pub(super) fn list_raw_col_to_rendered_col(
    raw_text: &str,
    line: &ratatui::text::Line<'_>,
    raw_col: usize,
) -> Option<usize> {
    let raw_total = raw_list_marker_char_width(raw_text)?;
    let rendered_total = rendered_list_marker_char_width(line)?;
    if raw_col <= raw_total {
        Some(rendered_total)
    } else {
        Some(raw_col - raw_total + rendered_total)
    }
}

/// Width (in chars) of the raw list-item prefix — leading whitespace +
/// marker (`- ` / `N. ` / `N) `) + optional task-prefix (`[ ] ` etc.).
/// Returns `None` when `raw_text` doesn't start with a list marker.
pub(super) fn raw_list_marker_char_width(raw_text: &str) -> Option<usize> {
    let indent_chars = raw_text
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    let after_indent: String = raw_text.chars().skip(indent_chars).collect();
    let rb = after_indent.as_bytes();
    let marker_len = match rb.first() {
        Some(b'-') | Some(b'*') | Some(b'+') if rb.get(1) == Some(&b' ') => 2,
        _ => {
            let digits = rb.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0
                && matches!(rb.get(digits), Some(b'.') | Some(b')'))
                && rb.get(digits + 1) == Some(&b' ')
            {
                digits + 2
            } else {
                return None;
            }
        }
    };
    let after_marker = &after_indent[marker_len..];
    let task_len = if after_marker.starts_with("[ ] ")
        || after_marker.starts_with("[x] ")
        || after_marker.starts_with("[X] ")
    {
        4
    } else {
        0
    };
    Some(indent_chars + marker_len + task_len)
}

/// Width (in chars) of the rendered list-item marker: leading whitespace,
/// then `• ` or padded digits with `. `, plus an optional trailing
/// `[ ] ` task prefix.  Returns `None` when the rendered line doesn't
/// start with a recognizable list marker.
pub(super) fn rendered_list_marker_char_width(line: &ratatui::text::Line<'_>) -> Option<usize> {
    let text: String = line.spans.iter().flat_map(|s| s.content.chars()).collect();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() && chars[i] == ' ' {
        i += 1;
    }
    let after_bullet = if chars.get(i) == Some(&'•') && chars.get(i + 1) == Some(&' ') {
        Some(i + 2)
    } else {
        let digits = chars[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0
            && matches!(chars.get(i + digits), Some('.') | Some(')'))
            && chars.get(i + digits + 1) == Some(&' ')
        {
            Some(i + digits + 2)
        } else {
            None
        }
    }?;
    // Tasks are decorated bullets — `• ` (or the ordered marker) is followed
    // by a `[ ] ` / `[✓] ` checkbox.  Include those four cells in the marker
    // width so cursor / selection mapping covers the whole forbidden zone.
    if chars.get(after_bullet) == Some(&'[')
        && matches!(chars.get(after_bullet + 1), Some(' ') | Some('✓'))
        && chars.get(after_bullet + 2) == Some(&']')
        && chars.get(after_bullet + 3) == Some(&' ')
    {
        Some(after_bullet + 4)
    } else {
        Some(after_bullet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::style::Style;
    use ratatui::text::{Line, Span};

    #[test]
    fn raw_list_marker_width_bullet() {
        assert_eq!(raw_list_marker_char_width("- foo"), Some(2));
        assert_eq!(raw_list_marker_char_width("  - foo"), Some(4));
    }

    #[test]
    fn raw_list_marker_width_ordered() {
        assert_eq!(raw_list_marker_char_width("1. foo"), Some(3));
        assert_eq!(raw_list_marker_char_width("10. foo"), Some(4));
    }

    #[test]
    fn raw_list_marker_width_task() {
        assert_eq!(raw_list_marker_char_width("- [ ] foo"), Some(6));
        assert_eq!(raw_list_marker_char_width("- [x] foo"), Some(6));
    }

    #[test]
    fn rendered_marker_width_bullet() {
        let line = Line::from(vec![Span::styled("• ", Style::default()), Span::raw("foo")]);
        assert_eq!(rendered_list_marker_char_width(&line), Some(2));
    }

    #[test]
    fn rendered_marker_width_ordered_padded() {
        let line = Line::from(vec![
            Span::styled(" 1. ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(rendered_list_marker_char_width(&line), Some(4));
    }

    #[test]
    fn rendered_marker_width_task() {
        // Tasks now render with the bullet kept — `• [ ] foo` — so the
        // full marker width is 6 (bullet + space + checkbox + space).
        let line = Line::from(vec![
            Span::styled("• ", Style::default()),
            Span::styled("[ ] ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(rendered_list_marker_char_width(&line), Some(6));
    }

    #[test]
    fn rendered_marker_width_task_checked() {
        // Checked rendered as `• [✓] foo` — same 6-cell marker.
        let line = Line::from(vec![
            Span::styled("• ", Style::default()),
            Span::styled("[✓] ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(rendered_list_marker_char_width(&line), Some(6));
    }

    #[test]
    fn list_col_map_bullet_unchanged() {
        // Raw `- foo`, rendered `• foo`.  Both have 2-char markers, so
        // raw col 2 (start of 'foo') maps to rendered col 2.
        let line = Line::from(vec![Span::styled("• ", Style::default()), Span::raw("foo")]);
        assert_eq!(list_raw_col_to_rendered_col("- foo", &line, 2), Some(2));
        assert_eq!(list_raw_col_to_rendered_col("- foo", &line, 4), Some(4));
    }

    #[test]
    fn list_col_map_task_aligns_one_to_one() {
        // Raw `- [ ] foo` (6-char marker), rendered `• [ ] foo` (also 6).
        // Cursor at raw col 6 ('f') stays at rendered col 6.
        let line = Line::from(vec![
            Span::styled("• ", Style::default()),
            Span::styled("[ ] ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(list_raw_col_to_rendered_col("- [ ] foo", &line, 6), Some(6));
        assert_eq!(list_raw_col_to_rendered_col("- [ ] foo", &line, 7), Some(7));
    }

    #[test]
    fn list_col_map_ordered_padded_shifts_right() {
        // Raw `1. foo` (3-char marker), rendered ` 1. foo` (4-char marker).
        // Raw col 3 ('f') maps to rendered col 4.
        let line = Line::from(vec![
            Span::styled(" 1. ", Style::default()),
            Span::raw("foo"),
        ]);
        assert_eq!(list_raw_col_to_rendered_col("1. foo", &line, 3), Some(4));
        assert_eq!(list_raw_col_to_rendered_col("1. foo", &line, 5), Some(6));
    }
}
