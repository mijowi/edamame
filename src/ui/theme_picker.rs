//! Fuzzy-searchable theme picker.
//!
//! Centred modal with an appearance toggle (Dark/Light) and a single-line
//! search input on top of a scrollable list of available themes (the
//! compiled-in [`BUILTIN_THEMES`](crate::config::theme) plus any user-authored
//! `themes/*.toml`).  Built on the shared [`SearchableList`] component; this
//! module supplies the theme rows (with a `current` suffix), the appearance
//! toggle chrome, and the modal framing.
//!
//! UI-only: the adapter in `src/app/modal/theme_picker.rs` wires the
//! component's [`ListEvent`](crate::ui::searchable_list::ListEvent) outcomes into live preview / selection.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::config::{AppearanceMode, Theme};
use crate::ui::content_width::max_row_width;
use crate::ui::controls;
use crate::ui::modal_row::{format_modal_row, RowLayout};
use crate::ui::scroll_container::{draw_frame, FrameOpts, ModalKind};
use crate::ui::searchable_list::{
    anchor_searchable_modal, FocusPolicy, ListChrome, RowCtx, SearchableList,
};

/// Placeholder shown in the empty search field.
const PLACEHOLDER: &str = "Type to filter themes…";

const NO_MATCHES_WIDTH: u16 = 12;
const MAX_LIST_ROWS: u16 = 20;
const CURRENT_SUFFIX_W: usize = "current".len();
/// Label preceding the Dark-mode toggle slider on the appearance row.
const MODE_LABEL: &str = "Dark mode";
/// Gap (in cells) between the label and the toggle slider.
const MODE_LABEL_GAP_W: u16 = 1;
/// Muted, centred hint shown directly under the toggle.
const MODE_HINT_LABEL: &str = "← →  to toggle";
/// Pinned rows above the list: toggle, hint, spacer, input, divider.
const PINNED_TOP: u16 = 5;

/// Rects captured during render for click hit-testing.
pub struct ThemePickerLayout {
    pub esc_rect: Option<Rect>,
    pub toggle_rect: Option<Rect>,
}

/// Build the theme list component, pre-focused on `current` and keeping focus
/// on the same theme as the query is broadened (so the live preview is
/// stable).
pub fn build_theme_list(themes: Vec<String>, current: &str) -> SearchableList<String> {
    let mut list = SearchableList::new(themes, |s: &String| s.as_str())
        .with_focus_policy(FocusPolicy::PreserveByIdentity);
    let current = current.to_owned();
    list.focus_matching(|t| *t == current);
    list
}

/// Render the theme-picker modal.  `current` is the theme active when the
/// picker opened (drives the `current` suffix); `mode` is the live appearance
/// mode.  Returns the cached esc / toggle hit-rects.
pub fn render_theme_picker(
    list: &mut SearchableList<String>,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    cursor_visible: bool,
    mode: AppearanceMode,
    current: &str,
) -> ThemePickerLayout {
    let content_width = theme_picker_content_width(list.items())
        .max(NO_MATCHES_WIDTH)
        .max(toggle_row_width())
        .max(MODE_HINT_LABEL.chars().count() as u16)
        .max(PLACEHOLDER.chars().count() as u16 + 2);
    let total_rows = list.visible_len() as u16;
    let geom = anchor_searchable_modal(
        area,
        content_width,
        total_rows,
        MAX_LIST_ROWS,
        PINNED_TOP,
        0,
        0,
    );
    let frame = draw_frame(
        geom.modal_area,
        buf,
        FrameOpts {
            title: "Switch theme",
            kind: ModalKind::Normal,
            show_close_hint: true,
            content: geom.content,
            theme,
        },
    );
    let inner = frame.body;
    if inner.height < PINNED_TOP + 1 || inner.width == 0 {
        return ThemePickerLayout {
            esc_rect: frame.esc_hit_rect,
            toggle_rect: None,
        };
    }

    // Toggle row — `Dark mode  ‹slider›`, centred.
    let toggle_y = inner.y;
    let dark_on = mode == AppearanceMode::Dark;
    let label_w = MODE_LABEL.chars().count() as u16;
    let slider_w = controls::toggle_width() as u16;
    let total_w = label_w + MODE_LABEL_GAP_W + slider_w;
    let toggle_x = inner
        .x
        .saturating_add(inner.width.saturating_sub(total_w) / 2);
    let mut toggle_spans = vec![Span::styled(
        format!("{MODE_LABEL}{}", " ".repeat(MODE_LABEL_GAP_W as usize)),
        theme.modal_item,
    )];
    toggle_spans.extend(controls::toggle_spans(dark_on, false, false, theme));
    fill_row(buf, inner, toggle_y, theme);
    Paragraph::new(Line::from(toggle_spans))
        .style(theme.modal_bg)
        .render(
            Rect {
                x: toggle_x,
                y: toggle_y,
                width: total_w,
                height: 1,
            },
            buf,
        );
    let toggle_rect = Some(Rect {
        x: toggle_x + label_w + MODE_LABEL_GAP_W,
        y: toggle_y,
        width: slider_w,
        height: 1,
    });

    // Hint row.
    let hint_y = inner.y + 1;
    let hint_len = MODE_HINT_LABEL.chars().count() as u16;
    let hint_x = inner
        .x
        .saturating_add(inner.width.saturating_sub(hint_len) / 2);
    fill_row(buf, inner, hint_y, theme);
    let hint_style = Style::default()
        .fg(theme.palette.text_muted)
        .bg(theme.palette.surface_elevated);
    Paragraph::new(Line::from(Span::styled(MODE_HINT_LABEL, hint_style)))
        .style(theme.modal_bg)
        .render(
            Rect {
                x: hint_x,
                y: hint_y,
                width: hint_len,
                height: 1,
            },
            buf,
        );

    // Blank spacer separating the appearance affordance from the search field.
    fill_row(buf, inner, inner.y + 2, theme);

    // Input + divider + list, rendered by the shared component starting at the
    // input row.
    let list_area = Rect {
        x: inner.x,
        y: inner.y + 3,
        width: inner.width,
        height: inner.height - 3,
    };
    let current = current.to_owned();
    list.render(
        list_area,
        buf,
        ListChrome {
            theme,
            cursor_visible,
            field_focused: true,
            placeholder: PLACEHOLDER,
            empty_text: "(no matches)",
            scrollbar_col: frame.scrollbar_col,
        },
        |ctx| match ctx {
            RowCtx::Item {
                item,
                focused,
                width,
            } => {
                let suffix = if *item == current { "current" } else { "" };
                format_modal_row(
                    item,
                    suffix,
                    focused,
                    false,
                    theme,
                    RowLayout::RightAlign(width),
                )
            }
            RowCtx::Header { title, .. } => {
                Line::from(Span::styled(title.to_owned(), theme.modal_item))
            }
        },
    );

    ThemePickerLayout {
        esc_rect: frame.esc_hit_rect,
        toggle_rect,
    }
}

/// Fill a full modal row with the elevated surface so centred controls read as
/// part of one continuous chrome strip.
fn fill_row(buf: &mut Buffer, inner: Rect, y: u16, theme: &Theme) {
    Paragraph::new("").style(theme.modal_bg).render(
        Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        },
        buf,
    );
}

/// Total rendered width of the centred appearance row: label + gap + slider.
fn toggle_row_width() -> u16 {
    MODE_LABEL.chars().count() as u16 + MODE_LABEL_GAP_W + controls::toggle_width() as u16
}

fn theme_picker_content_width(themes: &[String]) -> u16 {
    max_row_width(themes, |name| {
        2 + name.chars().count() + 1 + CURRENT_SUFFIX_W
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::searchable_list::ListEvent;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn themes() -> Vec<String> {
        vec![
            "256 Dark".to_owned(),
            "256 Light".to_owned(),
            "Ayu".to_owned(),
            "Catppuccin".to_owned(),
            "Dracula".to_owned(),
            "Tokyo Night".to_owned(),
        ]
    }

    #[test]
    fn opens_focused_on_current_theme() {
        let list = build_theme_list(themes(), "Catppuccin");
        assert_eq!(list.focused_item().map(String::as_str), Some("Catppuccin"));
    }

    #[test]
    fn typing_filters_themes() {
        let mut list = build_theme_list(themes(), "Ayu");
        for c in "drac".chars() {
            list.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(list.match_count(), 1);
        assert_eq!(list.focused_item().map(String::as_str), Some("Dracula"));
    }

    #[test]
    fn enter_submits_focused_theme() {
        let mut list = build_theme_list(themes(), "Ayu");
        for c in "tokyo".chars() {
            list.handle_key(&key(KeyCode::Char(c)));
        }
        match list.handle_key(&key(KeyCode::Enter)) {
            ListEvent::Submitted(i) => assert_eq!(list.items()[i], "Tokyo Night"),
            other => panic!("expected Submitted, got {other:?}"),
        }
    }

    #[test]
    fn down_emits_focus_changed_preview() {
        let mut list = build_theme_list(themes(), "256 Dark");
        assert_eq!(
            list.handle_key(&key(KeyCode::Down)),
            ListEvent::FocusChanged(1)
        );
    }

    #[test]
    fn broadening_query_preserves_focus_identity() {
        // Focus a theme, broaden the query so it still matches: focus should
        // stay on the same theme (PreserveByIdentity) and emit no preview.
        let mut list = build_theme_list(themes(), "Ayu");
        for c in "drac".chars() {
            list.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(list.focused_item().map(String::as_str), Some("Dracula"));
        let resp = list.handle_key(&key(KeyCode::Backspace));
        assert_eq!(
            resp,
            ListEvent::Continue,
            "same theme stays focused → no preview"
        );
        assert_eq!(list.focused_item().map(String::as_str), Some("Dracula"));
    }

    #[test]
    fn escape_cancels() {
        let mut list = build_theme_list(themes(), "Ayu");
        assert_eq!(list.handle_key(&key(KeyCode::Esc)), ListEvent::Cancelled);
    }
}
