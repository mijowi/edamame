//! Static category table for the keybindings overlay.

use crate::config::Action;

/// Categories the overlay surfaces, in display order.  Each entry is
/// `(category_label, &[(action, action_label)])`.
///
/// Order is curated to put the most-used categories first.  Within a
/// category the order is also curated — broadly: file ops, then
/// editing, then mode/state.
///
/// * `PgUp` / `PgDown` (`ScrollPageUp` / `ScrollPageDown`) are
///   intentionally absent — they're discovered by trying the obvious
///   keys and don't need a row in the overlay.
/// * `Toggle raw/render` lives under Editor (not a separate `View`
///   section) because the user thinks of mode-switching as part of
///   the editing surface.
/// * Table cell-navigation actions (Tab / Shift-Tab / Enter / etc.)
///   appear in the Table section so the row/column reorder chords
///   sit alongside the navigation chords that complement them.
pub(super) const CATEGORIES: &[(&str, &[(Action, &str)])] = &[
    (
        "Editor",
        &[
            (Action::Save, "Save file"),
            (Action::Copy, "Copy"),
            (Action::Cut, "Cut"),
            (Action::Paste, "Paste"),
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
        ],
    ),
    ("Links", &[(Action::FollowLinkUnderCursor, "Follow link")]),
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
            (Action::DiffEnterEdit, "Edit hunk"),
            (Action::DiffExitEdit, "Exit edit"),
            (Action::DiffExit, "Exit diff"),
        ],
    ),
];
