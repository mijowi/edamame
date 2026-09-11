use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ─── Action ──────────────────────────────────────────────────────────────────

/// Every command the editor can execute.
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
    // Each wraps the selection in its markers, or unwraps it if it is exactly that already.
    // All are no-ops without a non-empty, single-line selection.
    /// `**…**`
    BoldSelection,
    /// `*…*`
    ItalicizeSelection,
    /// `` `…` ``
    InlineCodeSelection,
    /// `~~…~~`
    StrikethroughSelection,
    /// `==…==`
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
    /// Write the buffer to a chosen path and adopt it as the buffer's home, so subsequent
    /// `Save`s target it.  The vim `:w <path>` command instead writes a detached copy, leaving
    /// the buffer's path unchanged.
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
    // Cell navigation.  Tab/Shift+Tab/Enter keep their normal behavior outside a table;
    // edit_ops redirects them when the cursor is inside one.
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
    // Shift+Enter inside a cell inserts a literal `<br>` (GFM's way to get multi-line cells);
    // outside a table it falls through to `Newline`.
    TableInsertBreak,
    // ── Link navigation ────────────────────────────────────────────
    /// Follow the link at the cursor's rope offset.  Handled by the `App`, not `edit_ops`,
    /// because the dispatch needs UI state (nav stack, in-flight worker threads).
    FollowLinkUnderCursor,
    /// Pop the navigation history: push the current (path, scroll, cursor, mode) onto the
    /// forward stack and restore the most recent back-entry.
    NavigateBack,
    /// Mirror of [`Action::NavigateBack`] on the forward stack.
    NavigateForward,
    /// Open the fuzzy-searchable command palette; the chosen action goes through the normal
    /// `edit_ops::apply` path.
    ShowCommandPalette,
    /// Show the static Markdown syntax cheat sheet.
    ShowMarkdownCheatSheet,
    /// Open a page of the built-in manual (`crate::docs`) read-only.
    ///
    /// Payload-bearing, hence excluded from `action_variants!` and unnameable in
    /// `keybindings.toml` — there is no keystroke-sized way to say *which* page, so the palette
    /// lists each one by name.
    OpenDoc(crate::docs::DocId),
    /// Open the settings overlay — edits `[editor] / [modal] / [table]
    /// / [images] / [export]` keys in `config.toml` in place.
    OpenSettings,
    /// Reopen the welcome modal, rebuilt from the *live* terminal capabilities.  Ignores
    /// `editor.show_welcome`, so it is the way back in after capabilities change.
    OpenWelcome,
    /// Open the keybinds overlay — edits `keybindings.toml` with
    /// conflict detection.
    OpenKeybinds,
    /// Open the theme picker; a selection writes `config.theme` and reapplies the palette live.
    SwitchTheme,
    /// Open the export-theme modal: copy an existing theme to a new `<name>.toml` in the
    /// user's `themes/` directory and make it active.
    CreateCustomTheme,
    /// Reveal the active config directory in the OS file manager.
    OpenConfigFolder,
    /// Open the export modal.  Its Format list offers HTML plus every configured
    /// `[[export.custom]]` converter, so this one action reaches every target.
    ExportHtml,
    /// Save the buffer and open it in `$VISUAL` / `$EDITOR` (or the OS handler), reloading
    /// from disk when the editor exits.
    OpenInExternalEditor,
    /// Toggle `config.table.show_buttons`.  Gated on mouse capability — the handles are inert
    /// without mouse reporting.
    ToggleTableButtons,
    // ── Setting toggles (palette) ──────────────────────────────────
    // Each flips the same `config` field its settings-overlay row writes, persists
    // `config.toml`, and pushes the change through the shared `apply_live_update`.
    /// Toggle `config.editor.big_h1` (big block-character H1 titles).
    ToggleBigH1,
    /// Toggle `config.editor.show_line_numbers` (gutter line numbers).
    ToggleLineNumbers,
    /// Toggle `config.editor.cursor_blink` (blinking editor cursor).
    ToggleBlinkCursor,
    /// Toggle `config.editor.autosave_enabled` (idle autosave).
    ToggleAutosave,
    /// Toggle `config.editor.visual_line_nav` (visual vs. logical Up/Down movement).
    ToggleVisualLineNav,
    /// Toggle Vim modal editing (`config.modal.handler`); rebuilds the live `VimState`.
    ToggleVimMode,
    /// Toggle `config.editor.max_width_enabled` (content-width limit).
    ToggleLimitWidth,
    /// Toggle `config.editor.diff_on_change` (hunk-by-hunk review vs. silent reload).
    ToggleDiffOnChange,
    /// Open the rows/columns modal that inserts a fresh GFM pipe table.  Requires the cursor
    /// on a blank line; the App-level handler flashes when that pre-flight fails.
    InsertTable,
    /// Insert an inline image snippet at the cursor, or wrap the selection as the alt text.
    /// Denied in literal-content blocks (code, HTML, an existing image).
    InsertImage,
    /// Insert an inline link snippet, or wrap the selection as the link text.  Same
    /// literal-block pre-flight as [`Action::InsertImage`].
    InsertLink,
    /// Paste an image from the OS clipboard: a screenshot is saved to
    /// `[images].save_dir` (or `EDAMAME_IMAGES_DIR`) and referenced as
    /// `![](path)`; a copied image-file path is referenced directly.
    PasteImage,
    /// Insert an auto-numbered `[^N]` footnote reference at the cursor
    /// (the next integer past the highest existing numeric footnote).
    /// The user writes the matching definition wherever they want.
    /// Insert an auto-numbered `[^N]` footnote reference (next integer past the highest
    /// existing numeric footnote); the user writes the definition.
    InsertFootnote,
    /// Delete the footnote at the cursor — every reference plus the definition — and renumber
    /// the rest.
    DeleteFootnote,
    /// Re-sequence numeric footnotes into order of first reference; named labels untouched.
    RenumberFootnotes,
    /// Renumber the ordered list under the cursor so its source numbering matches what is
    /// rendered (nesting-aware, spanning loose-list blank gaps), as one undoable edit.
    FixListNumbering,
    /// Show the About popover.  Opening it performs no network request — see
    /// [`Action::CheckForUpdates`].
    ShowAbout,
    /// Check GitHub for a newer release now.  Bypasses the daily throttle on the automatic
    /// startup check: that gate bounds unattended chatter, and this is an explicit request.
    CheckForUpdates,
    /// Open the fuzzy-searchable heading list ("Go to section").  The pick is live-previewed
    /// (debounced, so holding ↓ doesn't thrash the scroll); Esc reverts, Enter confirms.
    GoToSection,

    // ── Search and replace ─────────────────────────────────────────
    /// Open the search-and-replace modal.  Pressed during an active flow it re-opens the modal
    /// pre-filled with the current terms.
    OpenSearch,
    /// Advance focus to the next match, wrapping.  Hard-bound to `Tab` during the flow.
    SearchNext,
    /// Retreat focus to the previous match, wrapping.  Hard-bound to `Shift+Tab`.
    SearchPrev,
    /// Replace the focused match, then auto-advance after a short reveal delay.  No-op in a
    /// navigate-only flow (empty replace field).
    SearchReplace,
    /// Replace every match as one undo step, then exit the flow.
    SearchReplaceAll,
    /// Exit the search flow, leaving the cursor on the current match (search is a motion).
    SearchExit,

    // ── Diff review ────────────────────────────────────────────────
    /// Advance focus to the next hunk, leaving the current one `Pending`.
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
    /// Reset the focused hunk's decision to `Pending`.  Bound to `Backspace` in Review.
    DiffResetHunk,
    /// Request to exit diff mode.  A no-op while any hunk is pending; otherwise opens the
    /// apply-confirm modal before the merged result is written.
    DiffExit,
}

/// Classification used by the run loop to coalesce a burst of autorepeat keystrokes into one
/// buffer edit + history entry.  Only these three are coalescable: they share an `EditDelta`
/// shape (one offset, one removed range, one inserted range) so a run collapses cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoalesceKind {
    Insert,
    BackDelete,
    ForwardDelete,
}

impl Action {
    /// Classify this action for keystroke coalescing.  Equal `Some` kinds extend the current
    /// run; `None` and any mismatch break it.
    pub fn coalesce_kind(&self) -> Option<CoalesceKind> {
        match self {
            Action::InsertChar(_) => Some(CoalesceKind::Insert),
            Action::DeleteCharBack => Some(CoalesceKind::BackDelete),
            Action::DeleteCharForward => Some(CoalesceKind::ForwardDelete),
            _ => None,
        }
    }
}

/// Drive `Display` and `FromStr` for [`Action`] from one list of unit variant names.
/// Payload-bearing variants are named explicitly in the `Display` match only — `FromStr` can't
/// reconstruct them without their payload.
macro_rules! action_variants {
    ($( $variant:ident ),* $(,)?) => {
        /// Every unit variant of [`Action`], in declaration order.
        ///
        /// The exhaustive `Display` match means a new unit variant cannot compile without
        /// joining this list, so a caller can sweep the whole action surface without keeping a
        /// second list.  `InsertChar` and `OpenDoc` are absent — they need a value to
        /// construct.  `#[cfg(test)]` because its only use is the `app::actions` gate sweeps.
        #[cfg(test)]
        pub(crate) const EVERY_UNIT_ACTION: &[Action] = &[ $( Action::$variant, )* ];

        impl fmt::Display for Action {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let s: &str = match self {
                    $( Action::$variant => stringify!($variant), )*
                    Action::InsertChar(_) => "InsertChar",
                    Action::OpenDoc(_) => "OpenDoc",
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
    ShowCommandPalette, ShowMarkdownCheatSheet, ShowAbout, CheckForUpdates,
    OpenSettings, OpenWelcome, OpenKeybinds, OpenConfigFolder, SwitchTheme, CreateCustomTheme,
    ExportHtml, OpenInExternalEditor,
    ToggleTableButtons, InsertTable, InsertImage, InsertLink, PasteImage,
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

/// Parse a token as a `KeyModifiers` flag, or `None` if it is not a modifier name.
fn parse_modifier(part: &str) -> Option<KeyModifiers> {
    match part {
        "ctrl" => Some(KeyModifiers::CONTROL),
        "alt" => Some(KeyModifiers::ALT),
        "shift" => Some(KeyModifiers::SHIFT),
        _ => None,
    }
}

/// Parse the token after the modifiers into a `KeyCode`, or `None` if unrecognized.
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

/// Glyph-style label for a non-character `KeyCode` (compact form for the hint line).  `None`
/// for `KeyCode::Char` — callers handle character keys themselves.
fn keycode_glyph(code: KeyCode) -> Option<&'static str> {
    Some(match code {
        KeyCode::Up => "↑",
        KeyCode::Down => "↓",
        KeyCode::Left => "←",
        KeyCode::Right => "→",
        KeyCode::Enter => "↵",
        // BackTab is a terminal's Shift+Tab; collapse to one glyph so both forms read alike.
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

/// Word-style label for a non-character `KeyCode` (long form for the overlay and cheat sheet).
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

/// Render the key-code portion of a chord, using `lookup` for the named (non-Char) keys.
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

/// Render `ev` as a compact glyph chord for the hint line, where space is at a premium:
/// modifiers collapse to `^` / `⌥` / `⇧` and non-printable keys use Unicode glyphs.  Going
/// through this formatter keeps the displayed chord tracking the live `KeyMap`.
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

/// Render `ev` in the lowercase `+`-separated form [`parse_key`] accepts — use this whenever
/// the result must round-trip, as when the keybinds overlay writes to `keybindings.toml`.
/// `format_key` + `replace('-', '+')` would mangle keys whose own glyph is `-` or `+`.
///
/// `None` for `KeyCode` variants with no parseable spelling (`Modifier(_)`, `Null`, the
/// lock/print/pause cluster, `Media(_)`, `KeypadBegin`); callers should surface those as
/// "unsupported key" rather than write an un-parseable string to disk.
pub fn format_key_parseable(ev: &KeyEvent) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".into());
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt".into());
    }
    // BackTab implies Shift even without the modifier set; mirror `action_for`'s
    // canonicalization so the serialized form is `shift+tab`.
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

/// Render `ev` for display (`Ctrl-C`).  Deliberately not round-tripping — use
/// [`format_key_parseable`] when the result must parse back.
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

/// The `[keybindings]` section of config.toml: action name → key string.  An unknown action
/// name is an error at startup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyBindingOverrides(pub HashMap<String, String>);

impl KeyBindingOverrides {
    /// Persist the overrides to `path` as TOML.  Failure is not fatal — callers log and flash.
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

/// Alias the readline word-motion escapes `ESC b` / `ESC f` — which crossterm decodes as
/// `Alt+b` / `Alt+f` — onto `Alt+Left` / `Alt+Right`.
///
/// Every mainstream macOS terminal emits those escapes for Option+←/→ instead of a
/// modified-arrow CSI, so without this the `Alt-←` / `Alt-→` chords are silently inert there:
/// the event carries `ALT`, so it matches no binding and is not printable enough to become an
/// `InsertChar` (issue #29).
///
/// Not gated on `cfg(target_os = "macos")` — the escape comes from the terminal emulator, so a
/// Linux edamame reached over SSH from a Mac needs it too.  Consulted only *after* the primary
/// lookup, so an explicit `alt+b` binding wins.  Only the bare `ALT` chord aliases; no terminal
/// rewrites `Alt-Shift-←`/`→`.
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

/// Maps `KeyEvent`s to `Action`s: compiled-in defaults, then the user's `[keybindings]`.
#[derive(Debug, Clone)]
pub struct KeyMap {
    bindings: HashMap<KeyEvent, Action>,
}

impl KeyMap {
    /// Compiled-in defaults plus the config overrides.  Errors on an unknown action name or an
    /// unparseable key string.
    pub fn build(overrides: &KeyBindingOverrides) -> Result<Self, KeyMapError> {
        let mut map = Self::default_bindings();

        for (action_str, key_str) in &overrides.0 {
            let action = Action::from_str(action_str)?;
            let key = parse_key(key_str)?;
            map.bindings.insert(key, action);
        }

        Ok(map)
    }

    /// One key bound to `action`, formatted for display.  Which one is unspecified when
    /// several are bound — iterate `bindings()` for the full set.
    pub fn first_key_for(&self, action: &Action) -> Option<String> {
        self.bindings
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(k, _)| format_key(k))
    }

    /// [`KeyMap::first_key_for`] returning the raw `KeyEvent`, for callers with their own
    /// formatter.
    pub fn first_key_event_for(&self, action: &Action) -> Option<KeyEvent> {
        self.bindings
            .iter()
            .find(|(_, a)| *a == action)
            .map(|(k, _)| *k)
    }

    /// Rebind `action` to `new_key`.  A key already bound to a *different* action is an `Err`
    /// leaving the keymap unchanged — the keybinds overlay's conflict-detection contract.  On
    /// success `overrides` is updated too, and any prior key for `action` is dropped.
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
            // Already bound to this same key.
            return Ok(());
        }
        // Drop any prior chord for this action rather than leaving two.
        self.bindings.retain(|_, a| a != action);
        self.bindings.insert(parsed, action.clone());
        overrides.0.insert(action.to_string(), new_key.to_owned());
        Ok(())
    }

    /// Look up the action bound to a key event, if any.
    pub fn action_for(&self, event: &KeyEvent) -> Option<&Action> {
        // Strip `state` and force `kind: Press`: `KeyEvent`'s Hash covers all four fields, and
        // the kitty protocol's KEYPAD / CAPS_LOCK flags would otherwise defeat the lookup.
        let normalized = KeyEvent::new(event.code, event.modifiers);
        if let Some(action) = self.bindings.get(&normalized) {
            return Some(action);
        }
        // Some terminals report Shift+Tab as `BackTab`, with or without SHIFT set; normalize
        // to the `Tab + SHIFT` form `parse_key("shift+tab")` produces.
        if event.code == KeyCode::BackTab {
            let fallback = KeyEvent::new(KeyCode::Tab, event.modifiers | KeyModifiers::SHIFT);
            return self.bindings.get(&fallback);
        }
        // See `alt_word_motion_alias`.
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

        // Ctrl-Q only; Ctrl-C is Copy.
        bind!("ctrl+q", Action::Quit);

        // Arrows move the cursor in all modes; the app turns MoveUp/Down into scrolling in
        // Preview mode.
        bind!("up", Action::MoveUp);
        bind!("down", Action::MoveDown);
        bind!("left", Action::MoveLeft);
        bind!("right", Action::MoveRight);
        bind!("ctrl+left", Action::MoveWordLeft);
        bind!("ctrl+right", Action::MoveWordRight);
        // Ctrl+A is SelectAll (GUI convention); Home still moves to line start.
        bind!("ctrl+a", Action::SelectAll);
        bind!("ctrl+e", Action::MoveLineEnd);
        bind!("ctrl+home", Action::MoveDocStart);
        bind!("ctrl+end", Action::MoveDocEnd);

        // Explicit scrolling
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
        // Ctrl-R is vim's Redo; bound for everyone so vim Redo works by plain passthrough.
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
        // `Action::Open` is deliberately unbound while it is a stub: a default chord would
        // only surface "not implemented".  Restore `ctrl+o` (and the palette entry in
        // `ui::command_palette::actions`) when real in-app file opening lands.

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

        // Table editing — org-mode-style Alt+Arrow: arrow direction is the operation
        // direction, Shift promotes "reorder" to "insert" on that side.
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
        // Tab / Enter stay bound to InsertTab / Newline; edit_ops dispatches on context to
        // decide between inserting text and moving between cells.
        bind!("shift+tab", Action::TablePrevCell);
        // Only inside a table cell; elsewhere Shift+Enter falls back to Enter.
        bind!("shift+enter", Action::TableInsertBreak);

        // Alt+Left / Alt+Right stay bound to the table column-reorder actions; the `App`
        // redirects them to NavigateBack / NavigateForward when the cursor is outside a table.
        bind!("ctrl+enter", Action::FollowLinkUnderCursor);

        // The other overlay actions (cheat sheet, About, settings, keybinds, config folder,
        // export) are intentionally unbound — the palette reaches them all.
        bind!("ctrl+p", Action::ShowCommandPalette);

        // Ctrl-G is unclaimed by terminals: ASCII BEL is generated, never consumed as input.
        bind!("ctrl+g", Action::GoToSection);

        // The in-flow keys (Tab / Shift-Tab / r / a / Esc) are hard-bound in
        // `search::search_keys`, not here.
        bind!("ctrl+f", Action::OpenSearch);

        // Tables can't be authored from Rendered mode without this flow, so it gets a
        // discoverable chord alongside its palette entry.
        bind!("ctrl+shift+t", Action::InsertTable);

        // `InsertLink` / `InsertImage` and the code / strikethrough / highlight wraps ship
        // unbound: palette-reachable, and rebindable in keybindings.toml.

        Self { bindings: b }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full default binding table, pinned.  `docs/keybindings.md` is written by hand from
    /// it, so **accepting this snapshot is the reminder to update that page** (and
    /// `config/keybindings.toml`).  Sorted by chord, so the diff is independent of `HashMap`
    /// iteration order.
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

    /// `Action::Open` is a stub, so it must stay unbound — `docs/keybindings.md` says there is
    /// no in-app file open, and this keeps that true.
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
        // Some terminals emit `BackTab` instead of `Tab + SHIFT`; both must match.
        let km = KeyMap::build(&KeyBindingOverrides::default()).unwrap();
        let backtab_no_mod = KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(km.action_for(&backtab_no_mod), Some(&Action::TablePrevCell));
        let backtab_shift = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(km.action_for(&backtab_shift), Some(&Action::TablePrevCell));
    }

    /// macOS terminals send `ESC b` / `ESC f` for Option+←/→; both must reach whatever
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

    /// The alias follows the *live* binding, so a user who rebinds `alt+left` keeps Option+←.
    #[test]
    fn alt_arrow_alias_follows_a_rebound_alt_left() {
        let mut overrides = KeyBindingOverrides::default();
        overrides.0.insert("NavigateBack".into(), "alt+left".into());
        let km = KeyMap::build(&overrides).unwrap();
        let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(km.action_for(&alt_b), Some(&Action::NavigateBack));
    }

    /// A fallback, not an override: an explicit `alt+b` wins, and modified variants never alias.
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
        // The kitty protocol attaches `state` flags (e.g. KEYPAD); `action_for` looks past them.
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
        // The `+` separator collides with `+` as a key glyph, and `-` was once mangled by a
        // dash-to-plus normalization; both must round-trip.
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
