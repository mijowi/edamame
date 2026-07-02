//! `Block::List` rendering.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::markdown::ast::{Block, ListItem};
use crate::markdown::renderer::Renderer;

impl<'t> Renderer<'t> {
    pub(super) fn render_list(
        &self,
        ordered: bool,
        start: Option<u64>,
        items: &[ListItem],
        out: &mut Vec<Line<'static>>,
        indent_prefix: &str,
    ) {
        // Nested blocks (sub-lists, continuation paragraphs, …) inside this
        // list's items are indented by exactly `INDENT_WIDTH` cells relative to
        // the item's own line.  That mirrors the raw source, which indents a
        // nested item by the same fixed step (list indent / Tab both insert
        // `INDENT_WIDTH` spaces), so a nested marker sits at the same column in
        // the rendered and de-rendered (raw) views and de-rendering causes no
        // horizontal jump.  `digit_width` still right-aligns multi-digit
        // ordered markers so their item text stays in one column.
        let first_num = start.unwrap_or(1);
        let last_num = first_num + items.len().saturating_sub(1) as u64;
        let digit_width = last_num.to_string().len().max(1);
        let child_indent_prefix = format!(
            "{indent_prefix}{}",
            " ".repeat(crate::constants::INDENT_WIDTH)
        );

        let mut counter = first_num;
        for item in items {
            // Marker is the bullet / number prefix.  Tasks are decorated
            // bullets — they render the same `• ` (or ordered marker)
            // followed by the `[ ] ` checkbox span emitted just below.
            // This lets task items and plain bullets coexist in one list.
            let (marker, marker_style) = if ordered {
                // Right-align the number inside a `digit_width`-wide slot so
                // multi-digit numbers (10+) don't push their item's text out
                // of alignment with the single-digit items above.
                let s = format!(
                    "{indent_prefix}{counter:>digit_width$}. ",
                    digit_width = digit_width
                );
                counter += 1;
                (s, self.theme.list_number)
            } else {
                let bullet_style = match item.task {
                    Some(true) => self.theme.task_checked,
                    Some(false) => self.theme.task_unchecked,
                    None => self.theme.list_bullet,
                };
                (format!("{indent_prefix}• "), bullet_style)
            };

            // Task list prefix (checkbox).
            let task_prefix: Option<Span<'static>> = item.task.map(|checked| {
                if checked {
                    Span::styled("[✓] ", self.theme.task_checked)
                } else {
                    Span::styled("[ ] ", self.theme.task_unchecked)
                }
            });

            // Checked-item text style.  `task_complete_text` is the
            // theme's "muted text" style; `task_strikethrough` keeps
            // the CROSSED_OUT modifier opt-in so themes can ship the
            // muted color without the strikethrough.
            let checked_text_style = if item.task == Some(true) {
                if self.theme.task_strikethrough {
                    self.theme
                        .task_complete_text
                        .add_modifier(ratatui::style::Modifier::CROSSED_OUT)
                } else {
                    self.theme.task_complete_text
                }
            } else {
                Style::default()
            };

            // Empty list item: render the marker (and the task checkbox, if any)
            // so the block produces ≥1 line.  Without the checkbox branch, an
            // empty task item collapses to an invisible line because the "marker"
            // for task items is just indentation.
            if item.blocks.is_empty() {
                let mut spans = vec![Span::styled(marker.clone(), marker_style)];
                if let Some(tp) = task_prefix.clone() {
                    spans.push(tp);
                }
                out.push(Line::from(spans));
                continue;
            }

            // Render each block in the item.
            for (i, block) in item.blocks.iter().enumerate() {
                if i == 0 {
                    // First block: prepend the marker (and task prefix if any).
                    match block {
                        Block::Paragraph { inlines } => {
                            let mut spans = vec![Span::styled(marker.clone(), marker_style)];
                            if let Some(tp) = task_prefix.clone() {
                                spans.push(tp);
                            }
                            spans.extend(self.render_inlines(inlines, checked_text_style));
                            out.push(Line::from(spans));
                            // No blank line after list items (tight-list style).
                        }
                        other => {
                            // Non-paragraph first block: render the marker (and
                            // task prefix, if any) alone, then the block below.
                            let mut spans = vec![Span::styled(marker.clone(), marker_style)];
                            if let Some(tp) = task_prefix.clone() {
                                spans.push(tp);
                            }
                            out.push(Line::from(spans));
                            self.render_block(other, out, &child_indent_prefix);
                        }
                    }
                } else {
                    // Subsequent blocks in the same item: render with the
                    // child indent prefix so their text aligns with this
                    // item's text column (hanging-indent layout).
                    self.render_block(block, out, &child_indent_prefix);
                }
            }
        }
    }
}
