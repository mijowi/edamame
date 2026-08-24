//! Static catalogue of `Action`s that the command palette exposes,
//! plus their user-facing labels.  Pulled out of `command_palette.rs`
//! so the parent file is widget plumbing without the long action list.

use crate::config::Action;

/// Every Action variant we expose in the palette.  Ordering doesn't
/// matter — `build_entries` re-sorts.  Cursor-movement and selection
/// actions are excluded: they have no meaning when dispatched from a
/// modal palette (the cursor's already moved by the time the user
/// clicks `Move Right`).
pub(super) const ALL_ACTIONS: &[Action] = &[
    // Palette-only entries.  `OpenConfigFolder` is no
    // longer surfaced here — it lives on the first row of the
    // settings overlay (the "Open Config folder" entry), which is
    // where users go to discover config-file locations.  Surfacing
    // it twice was redundant and made the palette noisier.
    Action::ShowMarkdownCheatSheet,
    // The manual, shipped inside the binary.  The index first, then one
    // entry per page so fuzzy search finds a topic by name without
    // having to go through the index.  Written out rather than derived
    // from `docs::ALL_DOCS` because this is a `const` array; the two are
    // held in step by
    // `tests::the_palette_lists_every_embedded_page_exactly_once`.
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
    // Persisted setting toggles — the settings-overlay booleans, also
    // reachable via the palette's search-for-a-thing flow.
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
    Action::OpenSearch,
    Action::InsertFootnote,
    Action::DeleteFootnote,
    Action::RenumberFootnotes,
    Action::FixListNumbering,
    // File ops.  `Action::Open` is omitted while it remains a stub
    // (`NOT_YET_IMPLEMENTED`) — listing it here would offer the user a
    // command that can only flash "not implemented".
    Action::Save,
    Action::SaveAs,
    // History.
    Action::Undo,
    Action::Redo,
    // Clipboard.
    Action::Copy,
    Action::Cut,
    Action::Paste,
    // Formatting.
    Action::BoldSelection,
    Action::ItalicizeSelection,
    Action::InlineCodeSelection,
    Action::StrikethroughSelection,
    Action::HighlightSelection,
    // Selection / mode.
    Action::SelectAll,
    Action::ExitToPreview,
    Action::ToggleRawMode,
    Action::EnterEditMode,
    Action::Quit,
    // List + checkbox.
    Action::ToggleCheckbox,
    // Navigation (link / nav stack).
    Action::FollowLinkUnderCursor,
    Action::NavigateBack,
    Action::NavigateForward,
    Action::GoToSection,
    // Tables — surface only the structural ops.  Cell navigation
    // (Tab/Shift+Tab) doesn't make sense from a palette.
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

/// User-facing label for an [`Action`].  Returning `None` excludes the
/// action from the palette entirely.
pub(super) fn label_for(action: &Action) -> Option<&'static str> {
    Some(match action {
        Action::ShowMarkdownCheatSheet => "Show Markdown cheat sheet",
        Action::OpenDoc(id) => id.palette_label(),
        Action::OpenSettings => "Open settings",
        Action::OpenWelcome => "Open welcome / terminal setup",
        Action::SwitchTheme => "Switch theme",
        Action::CreateCustomTheme => "Create custom theme",
        Action::OpenKeybinds => "Open keybindings",
        Action::ExportHtml => "Export HTML",
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
        // `ALL_ACTIONS` is hand-written, so adding a page to
        // `docs::ALL_DOCS` without adding its entry here would leave it
        // reachable only by a link from another page — silently, with
        // nothing failing to compile.  Pinned for the same reason
        // `indexed_safe_themes_are_registered` pins the theme list
        // against `BUILTIN_THEMES`.
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
