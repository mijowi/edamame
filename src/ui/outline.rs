//! Docked document outline, drawn only when the remaining document stays readable.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    widgets::{Block, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::config::Theme;
use crate::ui::modal_row::truncate_to_cells;
use crate::ui::HeadingEntry;

/// Split a document region into outline and document, reserving a one-cell separator.
/// `estimated_gutter` counts the prospective line-number gutter; one more cell is
/// reserved for a potential scrollbar before deciding whether the outline fits.
pub fn split_outline_area(
    full: Rect,
    requested: bool,
    estimated_gutter: u16,
) -> (Option<Rect>, Rect) {
    let width = (full.width / 4).clamp(22, 30);
    let used = width.saturating_add(1);
    if !requested
        || full.height < 3
        || full
            .width
            .saturating_sub(used)
            .saturating_sub(estimated_gutter)
            .saturating_sub(1)
            < 60
    {
        return (None, full);
    }
    (
        Some(Rect::new(full.x, full.y, width, full.height)),
        Rect::new(full.x + used, full.y, full.width - used, full.height),
    )
}

/// Paints visible headings starting at `scroll`; current-section and keyboard
/// selection have independent glyphs, including in monochrome themes.
pub struct OutlineView<'a> {
    pub entries: &'a [HeadingEntry],
    pub selected: Option<usize>,
    pub current: Option<usize>,
    pub scroll: usize,
    pub focused: bool,
    pub theme: &'a Theme,
}

impl Widget for OutlineView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        Block::default().style(self.theme.normal).render(area, buf);
        buf.set_stringn(
            area.x,
            area.y,
            "Outline · F6/F8",
            area.width as usize,
            self.theme.normal.add_modifier(Modifier::BOLD),
        );
        if area.height == 1 {
            return;
        }
        if self.entries.is_empty() {
            buf.set_stringn(
                area.x,
                area.y + 1,
                "(no headings)",
                area.width as usize,
                self.theme.modal_item_hint,
            );
            return;
        }
        for (row, entry) in self
            .entries
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(area.height.saturating_sub(1) as usize)
        {
            let y = area.y + 1 + (row - self.scroll) as u16;
            let selected = self.focused && self.selected == Some(row);
            let style = if selected {
                self.theme.modal_item_selected
            } else {
                self.theme
                    .heading_style(entry.level)
                    .remove_modifier(Modifier::UNDERLINED)
                    .add_modifier(Modifier::BOLD)
            };
            if selected {
                buf.set_style(Rect::new(area.x, y, area.width, 1), style);
            }
            if self.current == Some(row) {
                buf.set_stringn(
                    area.x,
                    y,
                    "●",
                    area.width as usize,
                    if selected {
                        style
                    } else {
                        self.theme.status_breadcrumb_current
                    },
                );
            }
            if selected && area.width > 1 {
                buf.set_stringn(area.x + 1, y, "›", 1, style);
            }
            let indent = entry.level as usize;
            let prefix = 3 + indent;
            let budget = (area.width as usize).saturating_sub(prefix);
            if budget == 0 {
                continue;
            }
            buf.set_stringn(area.x + 3, y, &"      "[..indent], indent, style);
            let label = if UnicodeWidthStr::width(entry.text.as_str()) > budget {
                std::borrow::Cow::Owned(truncate_to_cells(&entry.text, budget))
            } else {
                std::borrow::Cow::Borrowed(entry.text.as_str())
            };
            buf.set_stringn(area.x + prefix as u16, y, label.as_ref(), budget, style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulldown_cmark::HeadingLevel;
    use ratatui::{backend::TestBackend, Terminal};

    fn entry(level: HeadingLevel, text: &str) -> HeadingEntry {
        HeadingEntry {
            level,
            text: text.to_owned(),
            buffer_line: 0,
            target_scroll: 0,
        }
    }

    #[test]
    fn split_only_when_document_retains_sixty_content_cells() {
        let full = Rect::new(5, 2, 120, 8);
        assert_eq!(split_outline_area(full, false, 0), (None, full));
        assert_eq!(
            split_outline_area(full, true, 0),
            (Some(Rect::new(5, 2, 30, 8)), Rect::new(36, 2, 89, 8))
        );
        let minimum = Rect::new(5, 2, 84, 3);
        assert_eq!(
            split_outline_area(minimum, true, 0),
            (Some(Rect::new(5, 2, 22, 3)), Rect::new(28, 2, 61, 3))
        );
        for (full, gutter) in [
            (Rect::new(5, 2, 83, 3), 0),
            (Rect::new(5, 2, 87, 3), 4),
            (Rect::new(5, 2, 120, 2), 0),
        ] {
            assert_eq!(split_outline_area(full, true, gutter), (None, full));
        }
        let with_gutter = Rect::new(5, 2, 88, 3);
        assert!(split_outline_area(with_gutter, true, 4).0.is_some());
    }

    #[test]
    fn painted_rows_distinguish_current_from_focused_selection() {
        let entries = [
            entry(HeadingLevel::H1, "First"),
            entry(HeadingLevel::H2, "第二节标题"),
        ];
        let theme = Theme::default();
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    OutlineView {
                        entries: &entries,
                        selected: Some(1),
                        current: Some(0),
                        scroll: 0,
                        focused: true,
                        theme: &theme,
                    },
                    Rect::new(2, 1, 22, 3),
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf.cell((2, 1)).unwrap().symbol(), "O");
        assert_eq!(buf.cell((2, 2)).unwrap().symbol(), "●");
        assert_eq!(buf.cell((3, 2)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((2, 3)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((3, 3)).unwrap().symbol(), "›");
        assert_eq!(buf.cell((7, 3)).unwrap().symbol(), "第");
        assert_eq!(
            buf.cell((3, 3)).unwrap().style().bg,
            theme.modal_item_selected.bg
        );
    }

    #[test]
    fn scrolling_and_cell_truncation_do_not_overwrite_neighbors() {
        let entries = [
            entry(HeadingLevel::H1, "Hidden"),
            entry(HeadingLevel::H2, "标题很长很长很长"),
        ];
        let theme = Theme::default();
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    OutlineView {
                        entries: &entries,
                        selected: None,
                        current: Some(1),
                        scroll: 1,
                        focused: false,
                        theme: &theme,
                    },
                    Rect::new(1, 1, 10, 2),
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "O");
        assert_eq!(buf.cell((1, 2)).unwrap().symbol(), "●");
        assert_eq!(buf.cell((10, 2)).unwrap().symbol(), "…");
        assert_eq!(buf.cell((11, 2)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((1, 3)).unwrap().symbol(), " ");
    }

    #[test]
    fn empty_outline_draws_placeholder() {
        let theme = Theme::default();
        let mut terminal = Terminal::new(TestBackend::new(20, 3)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    OutlineView {
                        entries: &[],
                        selected: None,
                        current: None,
                        scroll: 0,
                        focused: false,
                        theme: &theme,
                    },
                    Rect::new(0, 0, 18, 3),
                );
            })
            .unwrap();
        assert_eq!(
            terminal.backend().buffer().cell((0, 0)).unwrap().symbol(),
            "O"
        );
        assert_eq!(
            terminal.backend().buffer().cell((0, 1)).unwrap().symbol(),
            "("
        );
    }
}
