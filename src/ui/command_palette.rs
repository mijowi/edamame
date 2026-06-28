//! Fuzzy-searchable command palette.
//!
//! The palette is a centred modal with a single-line input on top of a
//! scrollable list of matched actions.  When the input is empty, all actions
//! are shown organized into named sections (`Suggested`, `File`, `Edit`, …);
//! once the user types, the shared [`SearchableList`] fuzzy-ranks every entry
//! against the query and sections collapse into a flat ranked list.
//!
//! The widget is deliberately UI-only: selecting a row produces an [`Action`],
//! which the adapter dispatches through the normal `edit_ops::apply` path.

mod actions;

use self::actions::{label_for, ALL_ACTIONS};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
};

use crate::config::{Action, KeyMap, Theme};
use crate::ui::content_width::max_row_width;
use crate::ui::modal_row::{format_modal_row, RowLayout};
use crate::ui::searchable_list::{
    draw_searchable_list_modal, ListModalOpts, RowCtx, SearchableList, VisibleRow, MAX_LIST_ROWS,
};

/// One palette row: an action plus its display label and bound chord.
///
/// Showing the chord next to the label is how users learn bindings
/// organically — typing "save" surfaces `Save file  (Ctrl-S)`.
#[derive(Debug, Clone)]
pub struct PaletteEntry {
    pub action: Action,
    pub label: String,
    pub chord: Option<String>,
}

/// Placeholder shown in the empty search field.
const PLACEHOLDER: &str = "Search commands…";

/// Width of "(no matches)" copy, used as a floor so the modal doesn't snap
/// narrower than the placeholder.
const NO_MATCHES_WIDTH: u16 = 12;

/// Build the palette's list component from `keymap`.
pub fn build_palette_list(keymap: &KeyMap) -> SearchableList<PaletteEntry> {
    SearchableList::new(build_entries(keymap), |e: &PaletteEntry| e.label.as_str())
        .with_sections(palette_sections)
}

/// Render the palette modal.  Returns the `esc` close-hint rect for click
/// hit-testing.
pub fn render_palette(
    list: &mut SearchableList<PaletteEntry>,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    cursor_visible: bool,
) -> Option<Rect> {
    let content_width = palette_content_width(list.items())
        .max(NO_MATCHES_WIDTH)
        .max(PLACEHOLDER.chars().count() as u16 + 2);
    draw_searchable_list_modal(
        list,
        area,
        buf,
        ListModalOpts {
            title: "Command Palette",
            content_width,
            max_list_rows: MAX_LIST_ROWS,
            vertical_pad: 0,
            theme,
            cursor_visible,
            placeholder: PLACEHOLDER,
            empty_text: "(no matches)",
        },
        |ctx| match ctx {
            RowCtx::Header { title, width } => format_section_header(title, theme, width),
            RowCtx::Item {
                item,
                focused,
                width,
            } => format_row(item, focused, theme, width),
        },
    )
}

/// Content-aware width for the palette body: max over `entries` of
/// `marker(2) + label_w + 1 (gap) + chord_w`.  Sized on the whole entry list
/// so the modal doesn't jiggle in width as the user filters.
fn palette_content_width(entries: &[PaletteEntry]) -> u16 {
    max_row_width(entries, |e| {
        let label_w = e.label.chars().count();
        let chord_w = e.chord.as_deref().map(|c| c.chars().count()).unwrap_or(0);
        2 + label_w + 1 + chord_w
    })
}

/// Format one palette row via the shared modal-row formatter (chord
/// right-aligned).
fn format_row(entry: &PaletteEntry, focused: bool, theme: &Theme, width: u16) -> Line<'static> {
    let chord = entry.chord.as_deref().unwrap_or("");
    format_modal_row(
        &entry.label,
        chord,
        focused,
        false,
        theme,
        RowLayout::RightAlign(width),
    )
}

/// Format a section header as a thin separator: `─ Title ───────`.
fn format_section_header(title: &str, theme: &Theme, width: u16) -> Line<'static> {
    let prefix = format!("─ {} ", title);
    let prefix_w = prefix.chars().count();
    let remaining = (width as usize).saturating_sub(prefix_w);
    let text = format!("{}{}", prefix, "─".repeat(remaining));
    Line::from(Span::styled(text, theme.modal_section_heading))
}

/// Empty-query sectioned layout: `Suggested` first (curated order), then each
/// action category in [`SECTION_ORDER`].  Suggested actions also appear in
/// their category section.
fn palette_sections(entries: &[PaletteEntry]) -> Vec<VisibleRow> {
    let mut rows = Vec::new();
    rows.push(VisibleRow::Header("Suggested".to_owned()));
    let mut suggested: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| is_suggested(&e.action))
        .map(|(i, _)| i)
        .collect();
    suggested.sort_by_key(|&i| {
        SUGGESTED_ACTIONS
            .iter()
            .position(|a| a == &entries[i].action)
            .unwrap_or(usize::MAX)
    });
    for idx in suggested {
        rows.push(VisibleRow::Item(idx));
    }
    for &section in SECTION_ORDER {
        let items: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| section_of(&e.action) == section)
            .map(|(i, _)| i)
            .collect();
        if !items.is_empty() {
            rows.push(VisibleRow::Header(section.to_owned()));
            for idx in items {
                rows.push(VisibleRow::Item(idx));
            }
        }
    }
    rows
}

/// Curated "Suggested" entries shown when the palette opens with no input.
const SUGGESTED_ACTIONS: &[Action] = &[
    Action::OpenSettings,
    Action::SwitchTheme,
    Action::OpenKeybinds,
    Action::GoToSection,
    Action::InsertTable,
    Action::ToggleTableButtons,
    Action::ExportHtml,
    Action::OpenInExternalEditor,
    Action::ShowMarkdownCheatSheet,
    Action::ShowAbout,
];

/// True when `action` is part of the curated suggested list.
fn is_suggested(action: &Action) -> bool {
    SUGGESTED_ACTIONS.contains(action)
}

/// Section ordering for the empty-state view.  Each action maps to exactly one
/// section via [`section_of`]; the Suggested section is handled separately.
const SECTION_ORDER: &[&str] = &["File", "Edit", "View", "Navigate", "Table", "Tools"];

fn section_of(action: &Action) -> &'static str {
    match action {
        Action::Save | Action::SaveAs | Action::Open | Action::ExportHtml | Action::Quit => "File",
        Action::Undo
        | Action::Redo
        | Action::Copy
        | Action::Cut
        | Action::Paste
        | Action::SelectAll
        | Action::BoldSelection
        | Action::ItalicizeSelection
        | Action::ToggleCheckbox
        | Action::InsertTable
        | Action::InsertFootnote
        | Action::DeleteFootnote
        | Action::RenumberFootnotes => "Edit",
        Action::ExitToPreview
        | Action::ToggleRawMode
        | Action::EnterEditMode
        | Action::ToggleTableButtons => "View",
        Action::FollowLinkUnderCursor
        | Action::NavigateBack
        | Action::NavigateForward
        | Action::GoToSection
        | Action::OpenSearch => "Navigate",
        Action::TableMoveRowUp
        | Action::TableMoveRowDown
        | Action::TableMoveColumnLeft
        | Action::TableMoveColumnRight
        | Action::TableInsertRowAbove
        | Action::TableInsertRowBelow
        | Action::TableInsertColumnLeft
        | Action::TableInsertColumnRight
        | Action::TableDeleteRow
        | Action::TableDeleteColumn => "Table",
        Action::OpenSettings
        | Action::OpenKeybinds
        | Action::SwitchTheme
        | Action::CreateCustomTheme
        | Action::ShowMarkdownCheatSheet
        | Action::OpenInExternalEditor
        | Action::ShowAbout => "Tools",
        _ => "Other",
    }
}

/// Build the full action list shown in the palette.  Each entry has a
/// human-readable label and (optionally) the bound chord.  Sorted
/// alphabetically by label (the empty-state view re-orders via sections; a
/// typed query is sorted by fuzzy score).
fn build_entries(keymap: &KeyMap) -> Vec<PaletteEntry> {
    let mut entries: Vec<PaletteEntry> = ALL_ACTIONS
        .iter()
        .filter_map(|a| {
            let label = label_for(a)?;
            let chord = keymap.first_key_for(a);
            Some(PaletteEntry {
                action: a.clone(),
                label: label.to_owned(),
                chord,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.label.cmp(&b.label));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::KeyBindingOverrides;
    use crate::ui::searchable_list::{ListEvent, VisibleRow};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn keymap() -> KeyMap {
        KeyMap::build(&KeyBindingOverrides::default()).unwrap()
    }

    /// Render once into a TestBackend so the list observes its visible-window
    /// size (needed before scroll/paging assertions).
    fn render(list: &mut SearchableList<PaletteEntry>, w: u16, h: u16) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_palette(list, area, frame.buffer_mut(), theme, true);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect()
    }

    #[test]
    fn empty_state_has_suggested_section_first_in_curated_order() {
        let list = build_palette_list(&keymap());
        let rows = palette_sections(list.items());
        assert!(matches!(&rows[0], VisibleRow::Header(h) if h == "Suggested"));
        let second_header = rows
            .iter()
            .skip(1)
            .position(|r| matches!(r, VisibleRow::Header(_)))
            .unwrap()
            + 1;
        let labels: Vec<String> = rows[1..second_header]
            .iter()
            .filter_map(|r| match r {
                VisibleRow::Item(i) => Some(list.items()[*i].label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            labels,
            vec![
                "Open settings".to_owned(),
                "Switch theme".to_owned(),
                "Open keybindings".to_owned(),
                "Go to section".to_owned(),
                "Insert table".to_owned(),
                "Toggle table buttons".to_owned(),
                "Export HTML".to_owned(),
                "Open current file in system editor".to_owned(),
                "Show Markdown cheat sheet".to_owned(),
                "About edamame".to_owned()
            ]
        );
    }

    #[test]
    fn empty_state_shows_all_actions_in_category_sections() {
        let list = build_palette_list(&keymap());
        let rows = palette_sections(list.items());
        let second_header_pos = rows
            .iter()
            .skip(1)
            .position(|r| matches!(r, VisibleRow::Header(_)))
            .unwrap()
            + 1;
        let category_actions: Vec<Action> = rows[second_header_pos..]
            .iter()
            .filter_map(|r| match r {
                VisibleRow::Item(i) => Some(list.items()[*i].action.clone()),
                _ => None,
            })
            .collect();
        for action in ALL_ACTIONS {
            if label_for(action).is_some() {
                assert!(
                    category_actions.contains(action),
                    "action {action} missing from category sections"
                );
            }
        }
    }

    #[test]
    fn typing_save_file_finds_save_file() {
        let mut list = build_palette_list(&keymap());
        for c in "save f".chars() {
            list.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(
            list.focused_item().map(|e| e.action.clone()),
            Some(Action::Save)
        );
    }

    #[test]
    fn enter_with_no_matches_is_continue() {
        let mut list = build_palette_list(&keymap());
        for c in "zzzznotanything".chars() {
            list.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(list.match_count(), 0);
        assert_eq!(list.handle_key(&key(KeyCode::Enter)), ListEvent::Continue);
    }

    #[test]
    fn escape_cancels() {
        let mut list = build_palette_list(&keymap());
        assert_eq!(list.handle_key(&key(KeyCode::Esc)), ListEvent::Cancelled);
    }

    #[test]
    fn enter_returns_focused_action() {
        let mut list = build_palette_list(&keymap());
        for c in "markd".chars() {
            list.handle_key(&key(KeyCode::Char(c)));
        }
        match list.handle_key(&key(KeyCode::Enter)) {
            ListEvent::Submitted(i) => {
                assert_eq!(list.items()[i].action, Action::ShowMarkdownCheatSheet)
            }
            other => panic!("expected Submitted, got {other:?}"),
        }
    }

    #[test]
    fn down_advances_focus_skipping_headers() {
        let mut list = build_palette_list(&keymap());
        let count = list.match_count();
        assert!(count > 1);
        let first = list.focused_item_index();
        list.handle_key(&key(KeyCode::Down));
        assert_ne!(list.focused_item_index(), first);
        // Focus never lands on a header even after exhausting the list.
        for _ in 0..count + 5 {
            list.handle_key(&key(KeyCode::Down));
            assert!(list.focused_item_index().is_some());
        }
    }

    #[test]
    fn ctrl_chars_do_not_pollute_query() {
        let mut list = build_palette_list(&keymap());
        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        list.handle_key(&ctrl_p);
        assert!(list.query().is_empty());
    }

    #[test]
    fn entries_carry_the_chord_for_save() {
        let entries = build_entries(&keymap());
        let save = entries.iter().find(|e| e.action == Action::Save).unwrap();
        assert!(save.chord.is_some(), "Save chord should be Ctrl-S");
    }

    #[test]
    fn palette_renders_scrollbar_when_more_rows_than_visible_height() {
        let mut list = build_palette_list(&keymap());
        list.handle_key(&key(KeyCode::Char('e')));
        let contents = render(&mut list, 80, 10);
        assert!(contents.contains('█'), "expected scrollbar thumb glyph");
    }

    #[test]
    fn palette_wheel_scrolls_without_moving_focus() {
        let mut list = build_palette_list(&keymap());
        list.handle_key(&key(KeyCode::Char('e')));
        render(&mut list, 80, 10);
        let focused_before = list.focused_item_index();
        list.scroll_by(2);
        render(&mut list, 80, 10);
        assert_eq!(
            list.focused_item_index(),
            focused_before,
            "wheel must not move focus"
        );
    }
}
