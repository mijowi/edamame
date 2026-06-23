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
    Action::ShowAbout,
    Action::OpenSettings,
    Action::SwitchTheme,
    Action::CreateCustomTheme,
    Action::OpenKeybinds,
    Action::ExportHtml,
    Action::OpenInExternalEditor,
    Action::ToggleTableButtons,
    Action::InsertTable,
    Action::OpenSearch,
    Action::InsertFootnote,
    Action::DeleteFootnote,
    Action::RenumberFootnotes,
    // File ops.
    Action::Save,
    Action::SaveAs,
    Action::Open,
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
        Action::OpenSettings => "Open settings",
        Action::SwitchTheme => "Switch theme",
        Action::CreateCustomTheme => "Create custom theme",
        Action::OpenKeybinds => "Open keybindings",
        Action::ExportHtml => "Export HTML",
        Action::OpenInExternalEditor => "Open current file in system editor",
        Action::ToggleTableButtons => "Toggle table buttons",
        Action::InsertTable => "Insert table",
        Action::OpenSearch => "Search and replace",
        Action::InsertFootnote => "Insert footnote",
        Action::DeleteFootnote => "Delete footnote at cursor",
        Action::RenumberFootnotes => "Renumber footnotes",
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
        Action::SelectAll => "Select all",
        Action::ExitToPreview => "Exit to preview",
        Action::ToggleRawMode => "Toggle raw mode",
        Action::EnterEditMode => "Enter edit mode",
        Action::Quit => "Quit",
        Action::ToggleCheckbox => "Toggle checkbox",
        Action::FollowLinkUnderCursor => "Follow link under cursor",
        Action::ShowAbout => "About edamame",
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
