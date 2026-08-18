use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ─── Action ──────────────────────────────────────────────────────────────────

/// Every command the editor can execute. The full enum is defined upfront so
/// keybindings are stable across phases; unimplemented variants are simply
/// no-ops until their phase is implemented.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    // ── Navigation / scrolling ─────────────────────────────────────
    ScrollUp,
    ScrollDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToTop,
    ScrollToBottom,
    // ── Cursor movement ─────────────────────────────────────────────
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveWordLeft,
    MoveWordRight,
    MoveLineStart,
    MoveLineEnd,
    MoveDocStart,
    MoveDocEnd,
    // ── Editing ────────────────────────────────────────────────────
    InsertChar(char),
    InsertTab,
    Newline,
    DeleteCharBack,
    DeleteCharForward,
    DeleteWordBack,
    DeleteWordForward,
    DeleteLine,
    // ── Clipboard ──────────────────────────────────────────────────
    Cut,
    Copy,
    Paste,
    // ── Formatting ─────────────────────────────────────────────────
    /// Wrap the selection in `**…**` (or unwrap it if it is exactly
    /// bold already).  No-op without a non-empty, single-line selection.
    BoldSelection,
    /// Wrap the selection in `*…*` (or unwrap it if it is exactly italic
    /// already).  No-op without a non-empty, single-line selection.
    ItalicizeSelection,
    /// Wrap the selection in `` ` `` backticks (or unwrap it if it is
    /// exactly an inline code span already).  No-op without a
    /// non-empty, single-line selection.
    InlineCodeSelection,
    /// Wrap the selection in `~~…~~` (or unwrap it if it is exactly
    /// struck-through already).  No-op without a non-empty,
    /// single-line selection.
    StrikethroughSelection,
    /// Wrap the selection in `==…==` (or unwrap it if it is exactly
    /// highlighted already).  No-op without a non-empty, single-line
    /// selection.
    HighlightSelection,
    // ── Selection ──────────────────────────────────────────────────
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    SelectAll,
    // ── History ────────────────────────────────────────────────────
    Undo,
    Redo,
    // ── File operations ────────────────────────────────────────────
    Save,
    /// Write the buffer to a chosen path and adopt it as the buffer's
    /// home — subsequent `Save`s target the new path.  Opens a path-entry
    /// modal seeded with the current path (or a default for an unnamed
    /// buffer).  The vim `:w <path>` command writes a detached copy
    /// instead, leaving the buffer's path unchanged.
    SaveAs,
    Open,
    // ── Mode transitions ───────────────────────────────────────────
    EnterEditMode,
    ExitToPreview,
    ToggleRawMode,
    // ── App control ────────────────────────────────────────────────
    Quit,
    // ── List editing ───────────────────────────────────────────────
    ToggleCheckbox,
    // ── Table editing ──────────────────────────────────────────────
    // Cell navigation. Tab/Shift+Tab/Enter outside a table retain their
    // normal behaviour; edit_ops redirects them when the cursor is inside
    // a table.
    TableNextCell,
    TablePrevCell,
    TableNextRow,
    TablePrevRow,
    // Row/column reorder (Alt+Arrow).
    TableMoveRowUp,
    TableMoveRowDown,
    TableMoveColumnLeft,
    TableMoveColumnRight,
    // Row/column insertion (Alt+Shift+Arrow).
    TableInsertRowAbove,
    TableInsertRowBelow,
    TableInsertColumnLeft,
    TableInsertColumnRight,
    // Row/column deletion.
    TableDeleteRow,
    TableDeleteColumn,
    // Shift+Enter inside a cell inserts a literal `<br>` (GFM supports this
    // as the canonical way to get multi-line cells).  Outside a table it
    // falls through to `Newline`.
    TableInsertBreak,
    // ── Link navigation ────────────────────────────────────────────
    /// Follow the link at the cursor's rope offset (if any).  In
    /// Preview mode users reach links via mouse click; in Rendered /
    /// Raw mode this action is the keyboard equivalent.  Handled by
    /// the `App`, not `edit_ops`, so the dispatch happens against UI
    /// state (nav stack, in-flight worker threads).
    FollowLinkUnderCursor,
    /// Pop the navigation history: move the current (path, scroll,
    /// cursor, mode) onto the forward stack and restore the most
    /// recent back-entry.  App-level.
    NavigateBack,
    /// Mirror of [`Action::NavigateBack`] operating on the forward
    /// stack.
    NavigateForward,
    /// Open the fuzzy-searchable command palette (Ctrl-P).  Lists
    /// every bound action and routes the chosen one through the
    /// normal `edit_ops::apply` path.
    ShowCommandPalette,
    /// Show the static Markdown syntax cheat sheet (CommonMark + GFM
    /// tables / task lists / strikethrough / footnotes).
    ShowMarkdownCheatSheet,
    /// Open the settings overlay — edits `[editor] / [modal] / [table]
    /// / [images] / [export]` keys in `config.toml` in place.
    OpenSettings,
    /// Reopen the welcome modal: the capability-aware settings surface
    /// (theme / images / diagrams / vim), rebuilt from the *live*
    /// terminal capabilities.  Unlike the first-run path this ignores
    /// `editor.show_welcome`, so it is the way back in after the
    /// terminal's capabilities change.
    OpenWelcome,
    /// Open the keybinds overlay — edits `keybindings.toml` with
    /// conflict detection.
    OpenKeybinds,
    /// Open the fuzzy-searchable theme picker.  Selecting a theme
    /// writes `config.theme` to disk and reapplies the palette live.
    SwitchTheme,
    /// Open the export-theme modal: choose an existing theme to copy,
    /// pick a new name, write the resulting `<name>.toml` into the
    /// user's `themes/` directory, and apply it as the active theme.
    CreateCustomTheme,
    /// Reveal the active config directory in the OS file manager / open
    /// it via `open::that`.
    OpenConfigFolder,
    /// Palette entry for the built-in HTML exporter.  Wired
    /// up here so the palette's `Suggested` list can reference it.
    ExportHtml,
    /// Save the current buffer and open it in `$VISUAL` / `$EDITOR`
    /// (falling back to the OS handler).  Reuses the same suspend /
    /// resume flow the settings overlay uses for `config.toml`.  The
    /// buffer is reloaded from disk after the editor exits so any
    /// external edits are picked up.
    OpenInExternalEditor,
    /// Toggle `config.table.show_buttons` and persist it to
    /// `config.toml`, mirroring the settings-overlay row.  Gated on
    /// mouse capability — the handles are inert without mouse reporting.
    ToggleTableButtons,
    // ── Setting toggles (palette) ──────────────────────────────────
    // Persisted boolean settings surfaced in the command palette so a
    // user who prefers the search-for-a-thing flow can flip them
    // without opening the settings overlay.  Each flips the same
    // `config` field its overlay row writes, persists `config.toml`,
    // and pushes the change through the shared `apply_live_update`.
    /// Toggle `config.editor.big_h1` (big block-character H1 titles).
    ToggleBigH1,
    /// Toggle `config.editor.show_line_numbers` (gutter line numbers).
    ToggleLineNumbers,
    /// Toggle `config.editor.cursor_blink` (blinking editor cursor).
    ToggleBlinkCursor,
    /// Toggle `config.editor.autosave_enabled` (idle autosave).
    ToggleAutosave,
    /// Toggle `config.editor.visual_line_nav` (visual vs. logical
    /// Up/Down movement).
    ToggleVisualLineNav,
    /// Toggle Vim modal editing by swapping `config.modal.handler`
    /// between `vim` and `default`; rebuilds the live `VimState`.
    ToggleVimMode,
    /// Toggle `config.editor.max_width_enabled` (content-width limit).
    ToggleLimitWidth,
    /// Toggle `config.editor.diff_on_change` (review external changes
    /// hunk-by-hunk vs. silent reload).
    ToggleDiffOnChange,
    /// Open the rows/columns modal that inserts a fresh
    /// GFM pipe table at the cursor.  Requires the cursor to be on
    /// a blank line; the App-level handler flashes an error
    /// when that pre-flight fails.
    InsertTable,
    /// Insert an inline image snippet (`![alt text](file path or URL)`)
    /// at the cursor, or wrap the selection as the alt text.  Denied in
    /// blocks whose content is literal (code, HTML, an existing image);
    /// the App-level handler flashes a warning when that pre-flight
    /// fails.
    InsertImage,
    /// Insert an inline link snippet (`[link text](file path or URL)`)
    /// at the cursor, or wrap the selection as the link text.  Same
    /// literal-block pre-flight as [`Action::InsertImage`].
    InsertLink,
    /// Insert an auto-numbered `[^N]` footnote reference at the cursor
    /// (the next integer past the highest existing numeric footnote).
    /// The user writes the matching definition wherever they want.
    InsertFootnote,
    /// Delete the footnote at the cursor — all of its references plus the
    /// definition — and renumber the remaining numeric footnotes.
    DeleteFootnote,
    /// Re-sequence every numeric footnote into order of first reference
    /// (GFM); named labels are left untouched.
    RenumberFootnotes,
    /// Renumber the ordered list under the cursor so its source numbering
    /// matches what is rendered (sequential from the first item's number,
    /// nesting-aware, spanning loose-list blank gaps), as one undoable edit.
    /// Flashes when the cursor is not in an ordered list or it is already
    /// sequential.
    FixListNumbering,
    /// Show the About edamame popover: bean art, rotating acronym
    /// tagline, version info (installed + latest GitHub release),
    /// author credit, and a button that opens the project homepage.
    ShowAbout,
    /// Open the fuzzy-searchable heading list ("Go to section").  Lets
    /// the user jump the viewport to any heading in the document; the
    /// pick is live-previewed (debounced) so holding ↓ doesn't thrash
    /// the scroll, Esc reverts to the original position, Enter
    /// confirms and places the cursor at the end of the heading line.
    GoToSection,

    // ── Search and replace ─────────────────────────────────────────
    /// Open the search-and-replace modal (search term + optional
    /// replacement).  Confirming with a non-empty search term starts
    /// the search flow.  Pressed during an active flow, it re-opens
    /// the modal pre-filled with the current terms.
    OpenSearch,
    /// Advance focus to the next match, wrapping at the end of the
    /// document.  Hard-bound to `Tab` while the flow is active.
    SearchNext,
    /// Retreat focus to the previous match, wrapping at the start.
    /// Hard-bound to `Shift+Tab` while the flow is active.
    SearchPrev,
    /// Replace the focused match with the replacement text, then
    /// auto-advance to the next match after a short reveal delay.
    /// No-op in a navigate-only flow (empty replace field).
    SearchReplace,
    /// Replace every match in one shot — a single undo step — then
    /// exit the flow.  No-op in a navigate-only flow.
    SearchReplaceAll,
    /// Exit the search flow, leaving the cursor and viewport on the
    /// current match (search is a motion — no scroll-back to origin).
    SearchExit,

    // ── Diff review ────────────────────────────────────────────────
    /// Advance focus to the next hunk in document order.  No decision
    /// implied — pressing this leaves the current hunk as `Pending`.
    DiffNext,
    /// Retreat focus to the previous hunk in document order.
    DiffPrev,
    /// Accept the focused hunk (`Decision::Accepted`) and advance.
    DiffAcceptHunk,
    /// Reject the focused hunk (`Decision::Rejected`) and advance.
    DiffRejectHunk,
    /// Bulk-accept every still-`Pending` hunk in one shot.
    DiffAcceptAll,
    /// Bulk-reject every still-`Pending` hunk in one shot.
    DiffRejectAll,
    /// Reset the focused hunk's decision back to `Pending`
    /// ("undecide").  No-op when the hunk is already `Pending`.  Bound
    /// to `Backspace` in Review sub-mode.
    DiffResetHunk,
    /// Request to exit diff mode.  Gated on full resolution: a no-op
    /// while any hunk is still pending, and otherwise opens the
    /// apply-confirm modal before the merged result is written.
    DiffExit,
}

/// Classification used by the run loop to coalesce a burst of
/// autorepeat keystrokes into a single buffer edit + history entry.
/// Only the three highest-frequency hot-path edits are coalescable:
/// they share an `EditDelta` shape (one offset, one removed-range,
/// one inserted-range) so a run of them collapses cleanly.  Any
/// action that returns `None` from [`Action::coalesce_kind`] ends the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoalesceKind {
    Insert,
    BackDelete,
    ForwardDelete,
}

impl Action {
    /// Classify this action for keystroke coalescing.  `None` means
    /// the action cannot be merged with its neighbours (cursor moves,
    /// mode switches, table/list special handling, etc.).  Run-
    /// membership is `Some(kind1) == Some(kind2)` — equal `Some`
    /// kinds extend the current run; everything else breaks it.
    pub fn coalesce_kind(&self) -> Option<CoalesceKind> {
        match self {
            Action::InsertChar(_) => Some(CoalesceKind::Insert),
            Action::DeleteCharBack => Some(CoalesceKind::BackDelete),
            Action::DeleteCharForward => Some(CoalesceKind::ForwardDelete),
            _ => None,
        }
    }
}

/// Drive `Display for Action` and `FromStr for Action` from a single
/// list of unit variant names.  Every payload-bearing variant has to
/// be named explicitly outside the macro: those go into the `Display`
/// `match` only (FromStr can't reconstruct them without their
/// payload).
macro_rules! action_variants {
    ($( $variant:ident ),* $(,)?) => {
        impl fmt::Display for Action {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let s: &str = match self {
                    $( Action::$variant => stringify!($variant), )*
                    Action::InsertChar(_) => "InsertChar",
                };
                f.write_str(s)
            }
        }

        impl FromStr for Action {
            type Err = KeyMapError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( stringify!($variant) => Ok(Action::$variant), )*
                    other => Err(KeyMapError::UnknownAction(other.to_owned())),
                }
            }
        }
    };
}

action_variants! {
    ScrollUp, ScrollDown, ScrollPageUp, ScrollPageDown, ScrollToTop, ScrollToBottom,
    MoveLeft, MoveRight, MoveUp, MoveDown,
    MoveWordLeft, MoveWordRight,
    MoveLineStart, MoveLineEnd,
    MoveDocStart, MoveDocEnd,
    InsertTab, Newline,
    DeleteCharBack, DeleteCharForward, DeleteWordBack, DeleteWordForward, DeleteLine,
    Cut, Copy, Paste,
    BoldSelection, ItalicizeSelection,
    InlineCodeSelection, StrikethroughSelection, HighlightSelection,
    SelectLeft, SelectRight, SelectUp, SelectDown, SelectAll,
    Undo, Redo,
    Save, SaveAs, Open,
    EnterEditMode, ExitToPreview, ToggleRawMode, Quit,
    ToggleCheckbox,
    TableNextCell, TablePrevCell, TableNextRow, TablePrevRow,
    TableMoveRowUp, TableMoveRowDown, TableMoveColumnLeft, TableMoveColumnRight,
    TableInsertRowAbove, TableInsertRowBelow,
    TableInsertColumnLeft, TableInsertColumnRight,
    TableDeleteRow, TableDeleteColumn,
    TableInsertBreak,
    FollowLinkUnderCursor,
    NavigateBack, NavigateForward,
    ShowCommandPalette, ShowMarkdownCheatSheet, ShowAbout,
    OpenSettings, OpenWelcome, OpenKeybinds, OpenConfigFolder, SwitchTheme, CreateCustomTheme,
    ExportHtml, OpenInExternalEditor,
    ToggleTableButtons, InsertTable, InsertImage, InsertLink,
    ToggleBigH1, ToggleLineNumbers, ToggleBlinkCursor, ToggleAutosave,
    ToggleVisualLineNav, ToggleVimMode, ToggleLimitWidth, ToggleDiffOnChange,
    InsertFootnote, DeleteFootnote, RenumberFootnotes,
    FixListNumbering,
    GoToSection,
    OpenSearch, SearchNext, SearchPrev,
    SearchReplace, SearchReplaceAll, SearchExit,
    DiffNext, DiffPrev,
    DiffAcceptHunk, DiffRejectHunk,
    DiffAcceptAll, DiffRejectAll, DiffResetHunk,
    DiffExit,
}

// ─── Key parsing ─────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum KeyMapError {
    #[error("unknown action name: '{0}'")]
    UnknownAction(String),
    #[error("unparseable key string: '{0}'")]
    UnparseableKey(String),
    #[error("'{key}' is already bound to {action}")]
    ConflictingBinding { key: String, action: String },
}

/// Parse a single token as a `KeyModifiers` flag, or return `None` if it
/// is not a modifier name.
fn parse_modifier(part: &str) -> Option<KeyModifiers> {
    match part {
        "ctrl" => Some(KeyModifiers::CONTROL),
        "alt" => Some(KeyModifiers::ALT),
        "shift" => Some(KeyModifiers::SHIFT),
        _ => None,
    }
}

/// Parse a `key_part` token (everything after the modifiers) into a
/// `KeyCode`, or return `None` if the token is not a recognized key.
fn parse_key_code(key_part: &str) -> Option<KeyCode> {
    let code = match key_part {
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "page_up" | "pageup" | "pgup" => KeyCode::PageUp,
        "page_down" | "pagedown" | "pgdn" => KeyCode::PageDown,
        "enter" | "return" => KeyCode::Enter,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "escape" | "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "insert" => KeyCode::Insert,
        "space" => KeyCode::Char(' '),
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "f4" => KeyCode::F(4),
        "f5" => KeyCode::F(5),
        "f6" => KeyCode::F(6),
        "f7" => KeyCode::F(7),
        "f8" => KeyCode::F(8),
        "f9" => KeyCode::F(9),
        "f10" => KeyCode::F(10),
        "f11" => KeyCode::F(11),
        "f12" => KeyCode::F(12),
        _ => {
            // Single Unicode scalar value — anything else is unparseable.
            let mut chars = key_part.chars();
            let first = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(first)
        }
    };
    Some(code)
}

/// Parse a human-readable key string such as `"ctrl+q"`, `"up"`, `"page_up"`,
/// `"ctrl+shift+z"` into a crossterm `KeyEvent`.
///
/// The literal `+` key collides with the modifier separator; it is
/// spelled `"+"` on its own and `"<mods>++"` with modifiers (e.g.
/// `"ctrl++"`).  All other keys split cleanly on `+`.
pub fn parse_key(s: &str) -> Result<KeyEvent, KeyMapError> {
    let lower = s.to_lowercase();

    let (modifier_parts, key_part): (Vec<&str>, &str) = if lower == "+" {
        (Vec::new(), "+")
    } else if let Some(prefix) = lower.strip_suffix("++") {
        (prefix.split('+').collect(), "+")
    } else {
        let mut parts: Vec<&str> = lower.split('+').collect();
        let key = parts.pop().unwrap_or("");
        (parts, key)
    };

    if key_part.is_empty() {
        return Err(KeyMapError::UnparseableKey(s.to_owned()));
    }

    let mut modifiers = KeyModifiers::NONE;
    for part in modifier_parts {
        let m = parse_modifier(part).ok_or_else(|| KeyMapError::UnparseableKey(s.to_owned()))?;
        modifiers |= m;
    }

    let code = parse_key_code(key_part).ok_or_else(|| KeyMapError::UnparseableKey(s.to_owned()))?;
    Ok(KeyEvent::new(code, modifiers))
}

/// Glyph-style label for a non-character `KeyCode` (compact form used in
/// the bottom-region hint line).  Returns `None` for `KeyCode::Char` —
/// callers handle character keys directly.
fn keycode_glyph(code: KeyCode) -> Option<&'static str> {
    Some(match code {
        KeyCode::Up => "↑",
        KeyCode::Down => "↓",
        KeyCode::Left => "←",
        KeyCode::Right => "→",
        KeyCode::Enter => "↵",
        // BackTab is the terminal's representation of Shift+Tab — collapse
        // to the canonical `⇧⇥` glyph so it reads the same regardless of
        // which form the source `KeyEvent` used.
        KeyCode::Tab | KeyCode::BackTab => "⇥",
        KeyCode::Backspace => "⌫",
        KeyCode::Delete => "Del",
        KeyCode::Esc => "Esc",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PgUp",
        KeyCode::PageDown => "PgDn",
        KeyCode::Insert => "Ins",
        _ => return None,
    })
}

/// Word-style label for a non-character `KeyCode` (long form used in the
/// keybinds overlay and cheat sheet).
fn keycode_word(code: KeyCode) -> Option<&'static str> {
    Some(match code {
        KeyCode::Up => "Up",
        KeyCode::Down => "Down",
        KeyCode::Left => "Left",
        KeyCode::Right => "Right",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PgUp",
        KeyCode::PageDown => "PgDn",
        KeyCode::Enter => "Enter",
        KeyCode::Backspace => "Backspace",
        KeyCode::Delete => "Delete",
        KeyCode::Esc => "Esc",
        KeyCode::Tab => "Tab",
        KeyCode::BackTab => "Shift-Tab",
        KeyCode::Insert => "Insert",
        _ => return None,
    })
}

/// Render the key-code portion of a chord using `lookup` for the named
/// (non-Char) keys.  `KeyCode::Char` always renders as its uppercase form
/// for ASCII letters or as itself otherwise, with `' '` shown as `Space`.
fn format_keycode(code: KeyCode, lookup: fn(KeyCode) -> Option<&'static str>) -> String {
    if let Some(s) = lookup(code) {
        return s.to_owned();
    }
    match code {
        KeyCode::Char(' ') => "Space".to_owned(),
        KeyCode::Char(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase().to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{:?}", other),
    }
}

/// Render `ev` as a compact glyph-based chord string suitable for the
/// bottom-region hint line, where horizontal space is at a premium.
/// Modifiers collapse to single characters (`^` / `⌥` / `⇧`) and
/// non-printable keys use Unicode glyphs (`↑` / `↓` / `←` / `→` / `↵`
/// / `⇥` / `⌫`).  Mirrors [`format_key`] for everything else.
///
/// This is the inverse of how the hint line *used* to hardcode chord
/// glyphs — by going through this formatter, the displayed chord
/// always tracks whatever the live `KeyMap` has bound for the action.
pub fn format_key_compact(ev: &KeyEvent) -> String {
    let mut out = String::new();
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        out.push('^');
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        out.push('⌥');
    }
    let shift = ev.modifiers.contains(KeyModifiers::SHIFT) || ev.code == KeyCode::BackTab;
    if shift {
        out.push('⇧');
    }
    out.push_str(&format_keycode(ev.code, keycode_glyph));
    out
}

/// Render `ev` in the lowercase `+`-separated form accepted by
/// [`parse_key`].  Use this when the result needs to round-trip back
/// through `parse_key` (e.g. the keybinds overlay writes the captured
/// chord to `keybindings.toml`).  Going via `format_key` + `replace('-',
/// '+')` instead would mangle keys whose own glyph is `-` or `+`.
///
/// Returns `None` for `KeyCode` variants that have no parseable
/// spelling — e.g. `KeyCode::Modifier(_)` (bare modifier presses,
/// emitted only with keyboard-enhancement flags), `Null`, the
/// lock/print/pause cluster, `Media(_)`, `KeypadBegin`.  Callers
/// should surface these as "unsupported key" rather than silently
/// writing an un-parseable string to disk.
pub fn format_key_parseable(ev: &KeyEvent) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".into());
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt".into());
    }
    // BackTab implies Shift even when the SHIFT modifier isn't set —
    // mirror the canonicalisation `action_for` does on lookup so the
    // serialized form is the canonical `shift+tab`.
    if ev.modifiers.contains(KeyModifiers::SHIFT) || ev.code == KeyCode::BackTab {
        parts.push("shift".into());
    }
    let code_str: String = match ev.code {
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(c) => c.to_ascii_lowercase().to_string(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "page_up".into(),
        KeyCode::PageDown => "page_down".into(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Esc => "escape".into(),
        KeyCode::Tab | KeyCode::BackTab => "tab".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::F(n) => format!("f{n}"),
        _ => return None,
    };
    parts.push(code_str);
    Some(parts.join("+"))
}

/// Render `ev` as a human-readable key string roughly matching what
/// [`parse_key`] accepts.  Used by the cheat-sheet popover to
/// display bindings; the inverse of `parse_key` is good enough here
/// even if it's not strictly round-tripping (e.g. we emit `Ctrl-C`
/// rather than `ctrl+c` for readability).
pub fn format_key(ev: &KeyEvent) -> String {
    let mut parts: Vec<String> = Vec::new();
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("Ctrl".into());
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        parts.push("Alt".into());
    }
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("Shift".into());
    }
    parts.push(format_keycode(ev.code, keycode_word));
    parts.join("-")
}

// ─── KeyBindingOverrides ──────────────────────────────────────────────────────

/// The `[keybindings]` section of config.toml. Maps action name strings to key
/// strings. Unknown action names are an error at startup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyBindingOverrides(pub HashMap<String, String>);

impl KeyBindingOverrides {
    /// Persist the overrides to `path` as TOML.  Used by the
    /// keybinds overlay so a rebind takes effect immediately and
    /// survives the next startup.  Returns the underlying I/O / TOML
    /// error verbatim — callers typically log + flash on failure
    /// rather than treating it as fatal.
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        use anyhow::Context;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory: {}", parent.display())
            })?;
        }
        let toml_str =
            toml::to_string_pretty(self).context("Failed to serialize keybindings to TOML")?;
        std::fs::write(path, toml_str)
            .with_context(|| format!("Failed to write keybindings file: {}", path.display()))?;
        Ok(())
    }
}

// ─── KeyMap ───────────────────────────────────────────────────────────────────

/// Map the readline word-motion escapes `ESC b` / `ESC f` — which
/// crossterm decodes as `Alt+b` / `Alt+f` — onto `Alt+Left` /
/// `Alt+Right`, or `None` for any other event.
///
/// Every mainstream macOS terminal emits those two escapes for
/// Option+←/→ rather than the modified-arrow CSI the chord would
/// otherwise produce: it is Apple Terminal's and iTerm2's default key
/// mapping, and Ghostty ships `keybind = alt+arrow_left=esc:b` (and
/// `…right=esc:f`) as a compiled-in default.  Without this alias the
/// column-reorder and navigation-history chords (`Alt-←` / `Alt-→`,
/// which the App redirects to `NavigateBack` / `NavigateForward`
/// outside a table) are silently inert on macOS — the event carries
/// `ALT`, so it matches no binding *and* is not printable enough to
/// become an `InsertChar`.  See issue #29.
///
/// This is deliberately not gated on `cfg(target_os = "macos")`: the
/// escape originates in the terminal emulator, not the host, so an
/// edamame running on Linux over SSH from a Mac needs it too.  It is a
/// *fallback*, consulted only after the primary lookup, so a user who
/// binds `alt+b` to something of their own keeps it — at the cost of
/// Option+← on macOS, which is the correct trade for an explicit
/// binding.
///
/// Only the bare `ALT` chord aliases.  `Alt-Shift-←`/`→` are left
/// alone because no terminal rewrites them: they still arrive as real
/// modified arrows, and mapping `Alt-Shift-B` onto
/// `TableInsertColumnLeft` would be a chord nobody asked for.
fn alt_word_motion_alias(event: &KeyEvent) -> Option<KeyEvent> {
    if event.modifiers != KeyModifiers::ALT {
        return None;
    }
    let code = match event.code {
        KeyCode::Char('b') => KeyCode::Left,
        KeyCode::Char('f') => KeyCode::Right,
        _ => return None,
    };
    Some(KeyEvent::new(code, KeyModifiers::ALT))
}

/// Maps `KeyEvent`s to `Action`s. Built from compiled-in defaults, then
/// overridden by the user's `[keybindings]` config.
#[derive(Debug, Clone)]
pub struct KeyMap {
    /// Primary map: key event → action.
    bindings: HashMap<KeyEvent, Action>,
}

impl KeyMap {
    /// Build a `KeyMap` with compiled-in defaults, then apply any overrides
    /// from config. Returns an error if any override contains an unknown action
    /// name or an unparseable key string.
    pub fn build(overrides: &KeyBindingOverrides) -> Result<Self, KeyMapError> {
        let mut map = Self::default_bindings();

        for (action_str, key_str) in &overrides.0 {
            let action = Action::from_str(action_str)?;
            let key = parse_key(key_str)?;
            map.bindings.insert(key, action);
        }

        Ok(map)
    }

    /// Return the first key (in insertion order, which HashMap does not
    /// guarantee but is fine for an approximate lookup) bound to
    /// `action`, formatted as a human-readable string.  Used by the
    /// cheat-sheet popover to surface the current binding for
    /// each known action.  When multiple keys are bound, only one is
    /// returned — callers that need every binding should iterate
    /// `bindings()` themselves.
    pub fn first_key_for(&self, action: &Action) -> Option<String> {
        self.bindings
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(k, _)| format_key(k))
    }

    /// Like [`KeyMap::first_key_for`] but returns the raw `KeyEvent` so
    /// callers can apply their own formatter (e.g. the bottom-region
    /// hint line uses `format_key_compact` instead of `format_key`).
    pub fn first_key_event_for(&self, action: &Action) -> Option<KeyEvent> {
        self.bindings
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(k, _)| *k)
    }

    /// Rebind `action` to `new_key` (parsed from `parse_key`-style
    /// syntax).  If `new_key` is already bound to a *different*
    /// action, returns `Err` and leaves the keymap unchanged — that's
    /// the conflict-detection contract from the keybinds
    /// overlay.  On success, the overrides table is updated to keep
    /// the on-disk shape in sync with the in-memory keymap.
    ///
    /// Existing bindings of the same `action` are removed so the
    /// caller doesn't have to track stale mappings.
    pub fn rebind(
        &mut self,
        action: &Action,
        new_key: &str,
        overrides: &mut KeyBindingOverrides,
    ) -> Result<(), KeyMapError> {
        let parsed = parse_key(new_key)?;
        if let Some(existing) = self.bindings.get(&parsed) {
            if existing != action {
                return Err(KeyMapError::ConflictingBinding {
                    key: new_key.to_owned(),
                    action: existing.to_string(),
                });
            }
            // Same action already bound to the same key — no-op.
            return Ok(());
        }
        // Drop any prior key bound to this action so we don't end up
        // with two chords for the same action sticking around.
        self.bindings.retain(|_, a| a != action);
        self.bindings.insert(parsed, action.clone());
        overrides.0.insert(action.to_string(), new_key.to_owned());
        Ok(())
    }

    /// Look up the action bound to a key event, if any.
    pub fn action_for(&self, event: &KeyEvent) -> Option<&Action> {
        // Normalize: strip `state` and force `kind: Press` so the kitty
        // keyboard protocol (which reports KEYPAD / CAPS_LOCK state flags)
        // does not prevent HashMap lookup. `KeyEvent`'s PartialEq/Hash
        // compare all four fields, and `parse_key` always produces events
        // with `state: EMPTY, kind: Press`.
        let normalized = KeyEvent::new(event.code, event.modifiers);
        if let Some(action) = self.bindings.get(&normalized) {
            return Some(action);
        }
        // Some terminals report Shift+Tab as `KeyCode::BackTab` (with or
        // without the SHIFT modifier set).  Normalize it to the canonical
        // `Tab + SHIFT` form produced by `parse_key("shift+tab")` so bindings
        // match regardless of which representation the terminal emits.
        if event.code == KeyCode::BackTab {
            let fallback = KeyEvent::new(KeyCode::Tab, event.modifiers | KeyModifiers::SHIFT);
            return self.bindings.get(&fallback);
        }
        // macOS terminals send the readline word-motion escapes for
        // Option+←/→ instead of a modified-arrow CSI (see
        // `alt_word_motion_alias`).
        if let Some(alias) = alt_word_motion_alias(&normalized) {
            return self.bindings.get(&alias);
        }
        None
    }

    /// Build the compiled-in default bindings.
    fn default_bindings() -> Self {
        let mut b: HashMap<KeyEvent, Action> = HashMap::new();

        macro_rules! bind {
            ($key:expr, $action:expr) => {
                if let Ok(k) = parse_key($key) {
                    b.insert(k, $action);
                }
            };
        }

        // Quit — Ctrl-Q only. Ctrl-C is Copy (see below).
        bind!("ctrl+q", Action::Quit);

        // Scrolling / cursor movement
        // Arrow keys → cursor movement in all modes; MoveUp/Down act as
        // ScrollUp/ScrollDown when in Preview mode (handled in app).
        bind!("up", Action::MoveUp);
        bind!("down", Action::MoveDown);
        bind!("left", Action::MoveLeft);
        bind!("right", Action::MoveRight);
        bind!("ctrl+left", Action::MoveWordLeft);
        bind!("ctrl+right", Action::MoveWordRight);
        // Ctrl+A is SelectAll (typical GUI editor convention).  Unix shell
        // users who want move-line-start can still use Home.
        bind!("ctrl+a", Action::SelectAll);
        bind!("ctrl+e", Action::MoveLineEnd);
        bind!("ctrl+home", Action::MoveDocStart);
        bind!("ctrl+end", Action::MoveDocEnd);

        // Explicit scrolling (works in all modes)
        bind!("page_up", Action::ScrollPageUp);
        bind!("page_down", Action::ScrollPageDown);
        bind!("home", Action::ScrollToTop);
        bind!("end", Action::ScrollToBottom);

        // Editing
        bind!("enter", Action::Newline);
        bind!("tab", Action::InsertTab);
        bind!("backspace", Action::DeleteCharBack);
        bind!("delete", Action::DeleteCharForward);
        bind!("ctrl+backspace", Action::DeleteWordBack);
        bind!("ctrl+delete", Action::DeleteWordForward);
        bind!("ctrl+d", Action::DeleteLine);

        // History
        bind!("ctrl+z", Action::Undo);
        bind!("ctrl+shift+z", Action::Redo);
        // Ctrl-R is vim's Redo; bind it for everyone so vim Redo works via
        // plain passthrough (no vim-specific claim) and non-vim users gain a
        // second Redo chord.  See docs/vim-implementation-plan.md §2.7.
        bind!("ctrl+r", Action::Redo);

        // Clipboard
        // Ctrl-C → Copy (not Quit). The app intercepts Ctrl-C in crossterm
        // raw mode before it can generate SIGINT, so this is safe.
        bind!("ctrl+c", Action::Copy);
        bind!("ctrl+x", Action::Cut);
        bind!("ctrl+v", Action::Paste);

        // Formatting — wrap the selection in bold / italic markers.
        // NOTE: Ctrl-i is historically identical to Tab and Ctrl-b to
        // ASCII 0x02; both are only delivered as distinct chords when the
        // kitty keyboard protocol is active (edamame requests it in
        // `terminal::setup`).  On terminals without it, Ctrl-i inserts a
        // Tab and Ctrl-b no-ops — the command palette is the fallback.
        bind!("ctrl+b", Action::BoldSelection);
        bind!("ctrl+i", Action::ItalicizeSelection);

        // File operations
        bind!("ctrl+s", Action::Save);
        // `Action::Open` is deliberately unbound: it is still a stub (see
        // `NOT_YET_IMPLEMENTED` in `app::actions`), so a default chord would
        // only surface a "not implemented" notice.  Restore the `ctrl+o`
        // binding — and the palette entry in `ui::command_palette::actions` —
        // when real in-app file opening lands.

        // Mode transitions
        bind!("escape", Action::ExitToPreview);
        bind!("ctrl+`", Action::ToggleRawMode);

        // Selection — Shift+Arrow extends the selection.
        bind!("shift+left", Action::SelectLeft);
        bind!("shift+right", Action::SelectRight);
        bind!("shift+up", Action::SelectUp);
        bind!("shift+down", Action::SelectDown);

        // List
        bind!("ctrl+space", Action::ToggleCheckbox);

        // Table editing — org-mode-style Alt+Arrow scheme.
        // Arrow direction = operation direction; Shift promotes "reorder" to
        // "insert" on that side. Cell navigation (Tab / Shift+Tab / Enter) is
        // handled via context dispatch in edit_ops when the cursor is inside
        // a table — they remain bound to InsertTab / Newline by default.
        bind!("alt+up", Action::TableMoveRowUp);
        bind!("alt+down", Action::TableMoveRowDown);
        bind!("alt+left", Action::TableMoveColumnLeft);
        bind!("alt+right", Action::TableMoveColumnRight);
        bind!("alt+shift+up", Action::TableInsertRowAbove);
        bind!("alt+shift+down", Action::TableInsertRowBelow);
        bind!("alt+shift+left", Action::TableInsertColumnLeft);
        bind!("alt+shift+right", Action::TableInsertColumnRight);
        bind!("alt+backspace", Action::TableDeleteRow);
        bind!("alt+shift+backspace", Action::TableDeleteColumn);
        // Shift+Tab moves to the previous cell when the cursor is inside a
        // table; it is a no-op elsewhere.  Tab / Enter remain bound to
        // InsertTab / Newline so that context dispatch in edit_ops can decide
        // whether to insert text or move between cells.
        bind!("shift+tab", Action::TablePrevCell);
        // Shift+Enter inserts a literal `<br>` when the cursor is inside a
        // table cell; outside a table it has no binding and the default
        // Shift+Enter behaviour (same as Enter) applies.
        bind!("shift+enter", Action::TableInsertBreak);

        // Link navigation.  Alt+Left / Alt+Right are NOT bound to
        // NavigateBack/NavigateForward here: those keys remain bound to
        // TableMoveColumnLeft / TableMoveColumnRight so tables keep their
        // column-reorder semantics, and the `App` dispatches them to
        // NavigateBack/Forward only when the cursor is outside any table.
        // Users can still rebind NavigateBack/Forward to any key via the
        // keybindings config.
        bind!("ctrl+enter", Action::FollowLinkUnderCursor);

        // Command palette.  Ctrl-P is the primary chord
        // (also surfaced on the bottom-region hint line as `^P Menu`).
        // The other palette actions (`ShowMarkdownCheatSheet`,
        // `ShowAbout`, `OpenSettings`, `OpenKeybinds`,
        // `OpenConfigFolder`, `ExportHtml`) are intentionally unbound:
        // they are reached only via the palette, so the user can
        // search-and-execute without memorising a chord per overlay.
        bind!("ctrl+p", Action::ShowCommandPalette);

        // "Go to section" — pop open a fuzzy-searchable heading list
        // and jump the viewport to the chosen heading.  Ctrl-G is
        // otherwise unbound by terminals (ASCII BEL is generated by
        // the application, never consumed as input).
        bind!("ctrl+g", Action::GoToSection);

        // Search and replace.  Ctrl-F is unclaimed by terminals and
        // unbound elsewhere in edamame, so the conventional "find"
        // chord opens the search modal directly.  The in-flow keys
        // (Tab / Shift-Tab / r / a / Esc) are hard-bound in
        // `search::search_keys`, not here.
        bind!("ctrl+f", Action::OpenSearch);

        // Graduation chord for the Insert Table command.
        // Tables can't be authored from Rendered mode without this
        // flow, so a discoverable keybind sits next to the palette
        // entry.
        bind!("ctrl+shift+t", Action::InsertTable);

        // `InsertLink` / `InsertImage` and the code / strikethrough /
        // highlight selection wraps ship unbound: they're reachable
        // from the command palette, and a user who wants a chord can
        // bind one in keybindings.toml.

        Self { bindings: b }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full default binding table, pinned.
    ///
    /// `docs/keybindings.md` is written by hand from this table, and a
    /// user reading a chord that no longer fires is a worse bug than most
    /// code defects — it is unfalsifiable from inside the app.  So any
    /// change here has to be accepted as a snapshot, and **that review is
    /// the reminder to update `docs/keybindings.md`** (plus the
    /// `config/keybindings.toml` reference file if the action appears
    /// there).
    ///
    /// Rendered as `chord = Action` sorted by chord so the diff is
    /// readable and independent of `HashMap` iteration order.
    #[test]
    fn default_bindings_are_pinned_for_the_docs() {
        let km = KeyMap::default_bindings();
        let mut rows: Vec<String> = km
            .bindings
            .iter()
            .map(|(ev, action)| {
                let chord = format_key_parseable(ev)
                    .unwrap_or_else(|| panic!("default binding {ev:?} has no parseable spelling"));
                format!("{chord} = {action:?}")
            })
            .collect();
        rows.sort();
        insta::assert_snapshot!(rows.join("\n"));
    }

    /// `Action::Open` is still a stub (`app::actions::NOT_YET_IMPLEMENTED`).
    /// It must stay unbound so no user discovers a default chord that can
    /// only flash "not implemented" — `docs/keybindings.md` tells readers
    /// there is no in-app file open, and this is what keeps that true.
    #[test]
    fn open_stays_unbound_while_it_is_a_stub() {
        let km = KeyMap::default_bindings();
        assert!(
            !km.bindings.values().any(|a| *a == Action::Open),
            "Action::Open is a stub but has a default binding; either implement it \
             (and update docs/keybindings.md) or leave it unbound"
        );
    }

    #[test]
    fn default_keymap_has_quit() {
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let key = parse_key("ctrl+q").unwrap();
        assert_eq!(km.action_for(&key), Some(&Action::Quit));
    }

    #[test]
    fn ctrl_c_is_copy_not_quit() {
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let key = parse_key("ctrl+c").unwrap();
        assert_eq!(km.action_for(&key), Some(&Action::Copy));
    }

    #[test]
    fn override_changes_binding() {
        let mut overrides = KeyBindingOverrides::default();
        overrides.0.insert("Quit".into(), "ctrl+x".into());
        let km = KeyMap::build(&overrides).unwrap();
        let key = parse_key("ctrl+x").unwrap();
        assert_eq!(km.action_for(&key), Some(&Action::Quit));
    }

    #[test]
    fn unknown_action_is_error() {
        let mut overrides = KeyBindingOverrides::default();
        overrides.0.insert("TypoAction".into(), "ctrl+x".into());
        assert!(KeyMap::build(&overrides).is_err());
    }

    #[test]
    fn unparseable_key_is_error() {
        let mut overrides = KeyBindingOverrides::default();
        overrides.0.insert("Quit".into(), "superkey+q".into());
        assert!(KeyMap::build(&overrides).is_err());
    }

    #[test]
    fn backtab_maps_to_shift_tab_binding() {
        // Some terminals emit Shift+Tab as `KeyCode::BackTab` instead of the
        // canonical `Tab + SHIFT` form.  `action_for` must match either way.
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let backtab_no_mod = KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(km.action_for(&backtab_no_mod), Some(&Action::TablePrevCell));
        let backtab_shift = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(km.action_for(&backtab_shift), Some(&Action::TablePrevCell));
    }

    /// macOS terminals send `ESC b` / `ESC f` for Option+←/→, which
    /// crossterm decodes as `Alt+b` / `Alt+f`.  Both must reach whatever
    /// `alt+left` / `alt+right` are bound to (issue #29).
    #[test]
    fn alt_b_and_alt_f_reach_the_alt_arrow_bindings() {
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
        let alt_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(
            km.action_for(&alt_b),
            km.action_for(&parse_key("alt+left").unwrap())
        );
        assert_eq!(
            km.action_for(&alt_f),
            km.action_for(&parse_key("alt+right").unwrap())
        );
        assert_eq!(km.action_for(&alt_b), Some(&Action::TableMoveColumnLeft));
        assert_eq!(km.action_for(&alt_f), Some(&Action::TableMoveColumnRight));
    }

    /// The alias follows the *live* binding rather than hard-coding the
    /// default action, so a user who rebinds `alt+left` keeps Option+←
    /// working on macOS.
    #[test]
    fn alt_arrow_alias_follows_a_rebound_alt_left() {
        let mut overrides = KeyBindingOverrides::default();
        overrides.0.insert("NavigateBack".into(), "alt+left".into());
        let km = KeyMap::build(&overrides).unwrap();
        let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(km.action_for(&alt_b), Some(&Action::NavigateBack));
    }

    /// It is a fallback, not an override: an explicit `alt+b` binding
    /// wins, and shifted / control-laden variants never alias.
    #[test]
    fn explicit_alt_b_binding_wins_over_the_arrow_alias() {
        let mut overrides = KeyBindingOverrides::default();
        overrides.0.insert("Save".into(), "alt+b".into());
        let km = KeyMap::build(&overrides).unwrap();
        let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(km.action_for(&alt_b), Some(&Action::Save));

        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let alt_shift_b =
            KeyEvent::new(KeyCode::Char('B'), KeyModifiers::ALT | KeyModifiers::SHIFT);
        assert_eq!(km.action_for(&alt_shift_b), None);
        let plain_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE);
        assert_eq!(km.action_for(&plain_b), None);
    }

    #[test]
    fn action_lookup_ignores_kitty_state_flags() {
        // The kitty keyboard protocol attaches non-default `state` flags
        // (e.g. KEYPAD) to events. `action_for` must look past those.
        use crossterm::event::{KeyEventKind, KeyEventState};
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let ctrl_q_with_state = KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::KEYPAD,
        };
        assert_eq!(km.action_for(&ctrl_q_with_state), Some(&Action::Quit));
    }

    #[test]
    fn parse_key_variants() {
        assert!(parse_key("up").is_ok());
        assert!(parse_key("page_down").is_ok());
        assert!(parse_key("ctrl+s").is_ok());
        assert!(parse_key("ctrl+shift+z").is_ok());
        assert!(parse_key("escape").is_ok());
        assert!(parse_key("space").is_ok());
        assert!(parse_key("ctrl+space").is_ok());
    }

    #[test]
    fn literal_plus_and_hyphen_round_trip() {
        // The `+` separator collides with `+` as a key glyph; `-` is
        // unambiguous but used to be mangled by the overlay's old
        // dash-to-plus normalisation.  Both must round-trip cleanly
        // through `parse_key` / `format_key_parseable`.
        for chord in ["+", "-", "ctrl++", "ctrl+-", "ctrl+shift++"] {
            let ev = parse_key(chord).unwrap_or_else(|_| panic!("parse {chord}"));
            let re = format_key_parseable(&ev).expect("supported key");
            assert_eq!(parse_key(&re).unwrap(), ev, "round-trip {chord} → {re}");
        }
    }

    #[test]
    fn ctrl_space_maps_to_toggle_checkbox() {
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let key = parse_key("ctrl+space").unwrap();
        assert_eq!(km.action_for(&key), Some(&Action::ToggleCheckbox));
    }
}
