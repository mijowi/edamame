//! Phase 9 — `?` cheat-sheet popover.
//!
//! Lists every bound keybinding grouped by category.  Shares the
//! [`ModalView`](super::modal::ModalView) scaffolding so the same
//! dismiss semantics (Escape, Enter, focused button) apply.

use crate::config::{Action, KeyMap};

/// Render a category-grouped body for the cheat-sheet modal.  Each
/// category header is a bare title line; bindings follow as
/// `<key>  <label>` rows.  Unbound actions are skipped silently.
pub fn build_cheat_sheet_body(keymap: &KeyMap) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (title, actions) in CATEGORIES {
        let mut rows: Vec<String> = Vec::new();
        for (action, label) in *actions {
            let Some(keys) = keys_bound_to(keymap, action) else {
                continue;
            };
            rows.push(format!("  {:<14}  {}", keys, label));
        }
        if rows.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push(format!("— {} —", title));
        out.extend(rows);
    }
    out
}

/// Return the primary human-readable key for `action`, if it's bound.
/// When multiple keys map to the action we show just the first.
fn keys_bound_to(keymap: &KeyMap, action: &Action) -> Option<String> {
    keymap.first_key_for(action)
}

/// Hard-coded category layout.  Titles reflect the phase boundaries
/// in the plan.  Keep the table sorted by increasing specialisation so
/// the common editing chords are visible without scrolling.
const CATEGORIES: &[(&str, &[(Action, &str)])] = &[
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
            (Action::ScrollPageUp, "Page up"),
            (Action::ScrollPageDown, "Page down"),
            (Action::SelectAll, "Select all"),
        ],
    ),
    (
        "View",
        &[
            (Action::ExitToPreview, "Preview mode"),
            (Action::ToggleRawMode, "Toggle raw"),
        ],
    ),
    ("Links", &[(Action::FollowLinkUnderCursor, "Follow link")]),
    ("List", &[(Action::ToggleCheckbox, "Toggle checkbox")]),
    (
        "Table",
        &[
            (Action::TableMoveRowUp, "Move row up"),
            (Action::TableMoveRowDown, "Move row down"),
            (Action::TableMoveColumnLeft, "Move col left"),
            (Action::TableMoveColumnRight, "Move col right"),
            (Action::TableInsertRowAbove, "Insert row above"),
            (Action::TableInsertRowBelow, "Insert row below"),
            (Action::TableDeleteRow, "Delete row"),
            (Action::TableDeleteColumn, "Delete column"),
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{KeyBindingOverrides, KeyMap};

    #[test]
    fn body_contains_common_editor_bindings() {
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let body = build_cheat_sheet_body(&km);
        let joined = body.join("\n");
        assert!(joined.contains("Editor"));
        assert!(joined.contains("Save file"));
        assert!(joined.contains("Quit"));
    }

    #[test]
    fn body_drops_empty_categories() {
        // Build a KeyMap where every binding is the default; the list
        // category still has ToggleCheckbox (`ctrl+space`) so it must
        // appear.  We assert at least the list section renders.
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let body = build_cheat_sheet_body(&km);
        assert!(body.iter().any(|l| l.contains("List")));
    }
}
