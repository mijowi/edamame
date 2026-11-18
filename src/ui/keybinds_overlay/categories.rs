//! Static category table for the keybindings overlay.

use crate::config::Action;

/// Categories the overlay surfaces, as `(category_label, &[(action, action_label)])`.
/// Both levels are in curated display order (most-used first).
/// `ScrollPageUp` / `ScrollPageDown` are intentionally absent: the obvious keys
/// find them without a row.
pub(super) const CATEGORIES: &[(&str, &[(Action, &str)])] = &[
    (
        "Editor",
        &[
            (Action::Save, "Save file"),
            (Action::Copy, "Copy"),
            (Action::Cut, "Cut"),
            (Action::Paste, "Paste"),
            (Action::BoldSelection, "Bold selection"),
            (Action::ItalicizeSelection, "Italicize selection"),
            (Action::InlineCodeSelection, "Inline code selection"),
            (Action::StrikethroughSelection, "Strikethrough selection"),
            (Action::HighlightSelection, "Highlight selection"),
            (Action::InsertImage, "Insert image"),
            (Action::Undo, "Undo"),
            (Action::Redo, "Redo"),
            (Action::Quit, "Quit"),
            (Action::ExitToPreview, "Preview mode"),
            (Action::ToggleRawMode, "Toggle raw/render"),
        ],
    ),
    (
        "Navigation",
        &[
            (Action::MoveWordLeft, "Word left"),
            (Action::MoveWordRight, "Word right"),
            (Action::MoveLineEnd, "Line end"),
            (Action::MoveDocStart, "Doc start"),
            (Action::MoveDocEnd, "Doc end"),
            (Action::SelectAll, "Select all"),
            (Action::GoToSection, "Go to section"),
            (Action::ToggleOutline, "Toggle outline"),
            (Action::FocusOutline, "Focus outline / editor"),
        ],
    ),
    (
        "Links",
        &[
            (Action::FollowLinkUnderCursor, "Follow link"),
            (Action::InsertLink, "Insert link"),
        ],
    ),
    (
        "Search",
        &[
            (Action::OpenSearch, "Search / replace"),
            (Action::SearchNext, "Next match"),
            (Action::SearchPrev, "Prev match"),
            (Action::SearchReplace, "Replace match"),
            (Action::SearchReplaceAll, "Replace all"),
            (Action::SearchExit, "Exit search"),
        ],
    ),
    ("List", &[(Action::ToggleCheckbox, "Toggle checkbox")]),
    (
        "Table",
        &[
            (Action::TableNextCell, "Next cell"),
            (Action::TablePrevCell, "Prev cell"),
            (Action::TableNextRow, "Next row"),
            (Action::TablePrevRow, "Prev row"),
            (Action::TableMoveRowUp, "Move row up"),
            (Action::TableMoveRowDown, "Move row down"),
            (Action::TableMoveColumnLeft, "Move col left"),
            (Action::TableMoveColumnRight, "Move col right"),
            (Action::TableInsertRowAbove, "Insert row above"),
            (Action::TableInsertRowBelow, "Insert row below"),
            (Action::TableInsertColumnLeft, "Insert col left"),
            (Action::TableInsertColumnRight, "Insert col right"),
            (Action::TableDeleteRow, "Delete row"),
            (Action::TableDeleteColumn, "Delete column"),
            (Action::TableInsertBreak, "Cell line break"),
        ],
    ),
    (
        "Diff Review",
        &[
            (Action::DiffNext, "Next hunk"),
            (Action::DiffPrev, "Prev hunk"),
            (Action::DiffAcceptHunk, "Accept hunk"),
            (Action::DiffRejectHunk, "Reject hunk"),
            (Action::DiffAcceptAll, "Accept all"),
            (Action::DiffRejectAll, "Reject all"),
            (Action::DiffResetHunk, "Reset hunk"),
            (Action::DiffExit, "Exit diff"),
        ],
    ),
];
