//! Static catalogue of the `Action`s the command palette exposes, plus their labels.

use crate::config::Action;

/// Every Action exposed in the palette; `build_entries` re-sorts, so order is irrelevant.
/// Cursor-movement and selection actions are excluded: they are meaningless from a modal.
/// `OpenConfigFolder` lives on the settings overlay instead.
pub(super) const ALL_ACTIONS: &[Action] = &[
    Action::ShowMarkdownCheatSheet,
    // One entry per manual page, written out because this is a `const`; held in step with
    // `docs::ALL_DOCS` by `tests::the_palette_lists_every_embedded_page_exactly_once`.
    Action::OpenDoc(crate::docs::DocId::Index),
    Action::OpenDoc(crate::docs::DocId::GettingStarted),
    Action::OpenDoc(crate::docs::DocId::Editing),
    Action::OpenDoc(crate::docs::DocId::Keybindings),
    Action::OpenDoc(crate::docs::DocId::TerminalCompatibility),
    Action::OpenDoc(crate::docs::DocId::Configuration),
    Action::OpenDoc(crate::docs::DocId::Themes),
    Action::OpenDoc(crate::docs::DocId::VimMode),
    Action::OpenDoc(crate::docs::DocId::Security),
    Action::ShowAbout,
    Action::CheckForUpdates,
    Action::OpenSettings,
    Action::OpenWelcome,
    Action::SwitchTheme,
    Action::CreateCustomTheme,
    Action::OpenKeybinds,
    Action::ExportHtml,
    Action::OpenInExternalEditor,
    Action::ToggleTableButtons,
    // The settings-overlay booleans.
    Action::ToggleBigH1,
    Action::ToggleLineNumbers,
    Action::ToggleBlinkCursor,
    Action::ToggleAutosave,
    Action::ToggleVisualLineNav,
    Action::ToggleVimMode,
    Action::ToggleLimitWidth,
    Action::ToggleDiffOnChange,
    Action::InsertTable,
    Action::InsertImage,
    Action::InsertLink,
    Action::PasteImage,
    Action::OpenSearch,
    Action::InsertFootnote,
    Action::DeleteFootnote,
    Action::RenumberFootnotes,
    Action::FixListNumbering,
    // `Action::Open` is omitted while it remains a `NOT_YET_IMPLEMENTED` stub.
    Action::Save,
    Action::SaveAs,
    Action::Undo,
    Action::Redo,
    Action::Copy,
    Action::Cut,
    Action::Paste,
    Action::BoldSelection,
    Action::ItalicizeSelection,
    Action::InlineCodeSelection,
    Action::StrikethroughSelection,
    Action::HighlightSelection,
    Action::SelectAll,
    Action::ExitToPreview,
    Action::ToggleRawMode,
    Action::EnterEditMode,
    Action::Quit,
    Action::ToggleCheckbox,
    Action::FollowLinkUnderCursor,
    Action::NavigateBack,
    Action::NavigateForward,
    Action::GoToSection,
    // Tables: structural ops only; cell navigation makes no sense from a palette.
    Action::TableMoveRowUp,
    Action::TableMoveRowDown,
    Action::TableMoveColumnLeft,
    Action::TableMoveColumnRight,
    Action::TableInsertRowAbove,
    Action::TableInsertRowBelow,
    Action::TableInsertColumnLeft,
    Action::TableInsertColumnRight,
    Action::TableDeleteRow,
    Action::TableDeleteColumn,
];

/// User-facing label for an [`Action`]; `None` excludes it from the palette.
pub(super) fn label_for(action: &Action) -> Option<&'static str> {
    Some(match action {
        Action::ShowMarkdownCheatSheet => "Show Markdown cheat sheet",
        Action::OpenDoc(id) => id.palette_label(),
        Action::OpenSettings => "Open settings",
        Action::OpenWelcome => "Open welcome / terminal setup",
        Action::SwitchTheme => "Switch theme",
        Action::CreateCustomTheme => "Create custom theme",
        Action::OpenKeybinds => "Open keybindings",
        Action::ExportHtml => "Export…",
        Action::OpenInExternalEditor => "Open current file in system editor",
        Action::ToggleTableButtons => "Toggle table buttons",
        Action::ToggleBigH1 => "Toggle big H1 headings",
        Action::ToggleLineNumbers => "Toggle line numbers",
        Action::ToggleBlinkCursor => "Toggle cursor blink",
        Action::ToggleAutosave => "Toggle autosave",
        Action::ToggleVisualLineNav => "Toggle visual line navigation",
        Action::ToggleVimMode => "Toggle Vim mode",
        Action::ToggleLimitWidth => "Toggle editor width limit",
        Action::ToggleDiffOnChange => "Toggle diff on external change",
        Action::InsertTable => "Insert table",
        Action::InsertImage => "Insert image",
        Action::InsertLink => "Insert link",
        Action::PasteImage => "Paste image from clipboard",
        Action::OpenSearch => "Search and replace",
        Action::InsertFootnote => "Insert footnote",
        Action::DeleteFootnote => "Delete footnote at cursor",
        Action::RenumberFootnotes => "Renumber footnotes",
        Action::FixListNumbering => "Fix list numbering",
        Action::Save => "Save file",
        Action::SaveAs => "Save as…",
        Action::Open => "Open file",
        Action::Undo => "Undo",
        Action::Redo => "Redo",
        Action::Copy => "Copy",
        Action::Cut => "Cut",
        Action::Paste => "Paste",
        Action::BoldSelection => "Bold selection",
        Action::ItalicizeSelection => "Italicize selection",
        Action::InlineCodeSelection => "Inline code selection",
        Action::StrikethroughSelection => "Strikethrough selection",
        Action::HighlightSelection => "Highlight selection",
        Action::SelectAll => "Select all",
        Action::ExitToPreview => "Exit to preview",
        Action::ToggleRawMode => "Toggle raw mode",
        Action::EnterEditMode => "Enter edit mode",
        Action::Quit => "Quit",
        Action::ToggleCheckbox => "Toggle checkbox",
        Action::FollowLinkUnderCursor => "Follow link under cursor",
        Action::ShowAbout => "About edamame",
        Action::CheckForUpdates => "Check for updates",
        Action::NavigateBack => "Navigate back",
        Action::NavigateForward => "Navigate forward",
        Action::GoToSection => "Go to section",
        Action::TableMoveRowUp => "Table: Move row up",
        Action::TableMoveRowDown => "Table: Move row down",
        Action::TableMoveColumnLeft => "Table: Move column left",
        Action::TableMoveColumnRight => "Table: Move column right",
        Action::TableInsertRowAbove => "Table: Insert row above",
        Action::TableInsertRowBelow => "Table: Insert row below",
        Action::TableInsertColumnLeft => "Table: Insert column left",
        Action::TableInsertColumnRight => "Table: Insert column right",
        Action::TableDeleteRow => "Table: Delete row",
        Action::TableDeleteColumn => "Table: Delete column",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::{DocId, ALL_DOCS};

    #[test]
    fn the_palette_lists_every_embedded_page_exactly_once() {
        // `ALL_ACTIONS` is hand-written: a page added to `docs::ALL_DOCS` but not here would be
        // silently unreachable except via links from another page.
        let listed: Vec<DocId> = ALL_ACTIONS
            .iter()
            .filter_map(|a| match a {
                Action::OpenDoc(id) => Some(*id),
                _ => None,
            })
            .collect();

        for page in ALL_DOCS {
            let n = listed.iter().filter(|id| **id == page.id).count();
            assert_eq!(n, 1, "{} appears {n} times in the palette", page.slug);
        }
        assert_eq!(
            listed.iter().filter(|id| **id == DocId::Index).count(),
            1,
            "the index needs exactly one palette entry"
        );
        assert_eq!(
            listed.len(),
            ALL_DOCS.len() + 1,
            "the palette lists a page that is not in ALL_DOCS"
        );
    }

    #[test]
    fn every_palette_documentation_entry_has_a_label() {
        for action in ALL_ACTIONS
            .iter()
            .filter(|a| matches!(a, Action::OpenDoc(_)))
        {
            assert!(
                label_for(action).is_some_and(|l| !l.is_empty()),
                "{action} has no palette label"
            );
        }
    }
}
