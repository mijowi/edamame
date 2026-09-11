//! Action routing.
//!
//! [`App::handle_app_action`] intercepts App-level actions before the generic `edit_ops::apply`
//! fallthrough; [`App::dispatch_action`] is the single dispatcher shared by the run-loop
//! keystroke arm and the palette-pick re-entry path, resolving `handle_app_action` → dirty-quit
//! guard → `edit_ops::apply` → scroll / flash / link-follow side effects.
//!
//! Also holds the `open_X` modal-push helpers, modal-key dispatch, the quit-confirm and
//! column-widths flows, the [`HandleEvent`] run-loop adapter, and live-theme reapply.

use crossterm::event::{Event, KeyEventKind, MouseEvent, MouseEventKind};

use crate::app::modal;
use crate::clipboard::ClipboardData;
use crate::config::sections::{DEFAULT_HANDLER, VIM_HANDLER};
use crate::config::{Action, Config, KeyBindingOverrides, KeyMap, Theme};
use crate::editor::{edit_ops, EditorState};
use crate::image::paste;
use crate::input::mode_handler::default::DefaultHandler;
use crate::input::ModeHandler;
use crate::terminal::ColorDepth;
use crate::ui::{settings_overlay, ModalKind};

use super::flash::MessageKind;
use super::modal::ModalOutcome;
use super::App;

/// Actions whose handlers are stubs; firing one pops a generic "not implemented" notice.  The
/// single source of truth for unfinished features.
///
/// Implementing one means adding an explicit `Action::Foo => …` arm above the catch-all guard
/// *and* removing it here — nothing enforces that, and a stale entry is merely misleading (the
/// explicit arm wins).
pub(super) const NOT_YET_IMPLEMENTED: &[Action] = &[Action::Open];

/// What an [`Action`] can *do*, tagged once per variant.
///
/// The three default-deny gates below — diff review, a capturing search flow, a read-only
/// document — are rules over these tags rather than hand-maintained allowlists, which used to
/// silently default every new `Action` to denied in three places at once.  The tag match is
/// exhaustive with **no wildcard arm**, so a new variant is a compile error until tagged.
///
/// The flags describe *capability*, not any gate's policy: "writes to the text", not "denied in
/// diff".  A gate wanting an exception spells it out at its own site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ActionCaps {
    /// Writes to the document's text.  Conservative: a variant that mutates only in some states
    /// (`TableNextCell` appends a row at a table's end) still carries the flag.
    pub mutates_buffer: bool,
    /// Replaces the live document, or leaves the current position for another.
    ///
    /// **No gate denies on this today.**  The capturing search flow excludes these by simply not
    /// listing them (default-deny already covers it), and the read-only gate takes the opposite
    /// view — cross-linking and section jumping are what a manual is *for*.  Tagged anyway so a
    /// future gate finds the distinction already made rather than re-deriving it.
    pub navigates_away: bool,
    /// Writes the document out, or needs it to have a path on disk.  A pathless document (an
    /// embedded manual page) refuses these rather than detour into a Save-as prompt.
    pub needs_path: bool,
    /// Reads the buffer without writing it: cursor motion, selection, copy.  Distinct from "not
    /// `mutates_buffer`", which also covers config toggles and mode transitions.
    pub read_only_nav: bool,
    /// Safe in every gated context: the scroll actions, the overlay openers, and `Quit`.
    pub always_safe: bool,
}

/// Tag `action` with what it can do.  **Do not add a wildcard arm** — the compile error a new
/// variant produces here is the whole point.
pub(super) fn action_caps(action: &Action) -> ActionCaps {
    use Action::*;
    let mutates_buffer = matches!(
        action,
        InsertChar(_)
            | InsertTab
            | Newline
            | DeleteCharBack
            | DeleteCharForward
            | DeleteWordBack
            | DeleteWordForward
            | DeleteLine
            | Cut
            | Paste
            | BoldSelection
            | ItalicizeSelection
            | InlineCodeSelection
            | StrikethroughSelection
            | HighlightSelection
            | Undo
            | Redo
            | ToggleCheckbox
            // The "motion" table commands are included on purpose: Tab off the last cell
            // appends a row, and Shift-Tab outside a table outdents a list item.
            | TableNextCell
            | TablePrevCell
            | TableNextRow
            | TablePrevRow
            | TableMoveRowUp
            | TableMoveRowDown
            | TableMoveColumnLeft
            | TableMoveColumnRight
            | TableInsertRowAbove
            | TableInsertRowBelow
            | TableInsertColumnLeft
            | TableInsertColumnRight
            | TableDeleteRow
            | TableDeleteColumn
            | TableInsertBreak
            | InsertTable
            | InsertImage
            | InsertLink
            | PasteImage
            | InsertFootnote
            | DeleteFootnote
            | RenumberFootnotes
            | FixListNumbering
            | SearchReplace
            | SearchReplaceAll
    );
    let navigates_away = matches!(
        action,
        FollowLinkUnderCursor | NavigateBack | NavigateForward | GoToSection | OpenDoc(_) | Open
    );
    let needs_path = matches!(action, Save | SaveAs | ExportHtml | OpenInExternalEditor);
    let read_only_nav = matches!(
        action,
        MoveLeft
            | MoveRight
            | MoveUp
            | MoveDown
            | MoveWordLeft
            | MoveWordRight
            | MoveLineStart
            | MoveLineEnd
            | MoveDocStart
            | MoveDocEnd
            | SelectLeft
            | SelectRight
            | SelectUp
            | SelectDown
            | SelectAll
            | Copy
    );
    let always_safe = matches!(
        action,
        ScrollUp
            | ScrollDown
            | ScrollPageUp
            | ScrollPageDown
            | ScrollToTop
            | ScrollToBottom
            | Quit
            | ShowCommandPalette
            | ShowMarkdownCheatSheet
            | ShowAbout
            | CheckForUpdates
            | OpenSettings
            | OpenWelcome
            | OpenKeybinds
            | SwitchTheme
            | CreateCustomTheme
            | OpenConfigFolder
    );
    // The exhaustiveness check: a new variant can't be added without deciding which group it
    // joins — including "none of them", the real answer for mode transitions and config toggles.
    match action {
        ScrollUp | ScrollDown | ScrollPageUp | ScrollPageDown | ScrollToTop | ScrollToBottom
        | Quit | ShowCommandPalette | ShowMarkdownCheatSheet | ShowAbout | CheckForUpdates
        | OpenSettings | OpenWelcome | OpenKeybinds | SwitchTheme | CreateCustomTheme
        | OpenConfigFolder | MoveLeft | MoveRight | MoveUp | MoveDown | MoveWordLeft
        | MoveWordRight | MoveLineStart | MoveLineEnd | MoveDocStart | MoveDocEnd | SelectLeft
        | SelectRight | SelectUp | SelectDown | SelectAll | Copy | InsertChar(_) | InsertTab
        | Newline | DeleteCharBack | DeleteCharForward | DeleteWordBack | DeleteWordForward
        | DeleteLine | Cut | Paste | BoldSelection | ItalicizeSelection | InlineCodeSelection
        | StrikethroughSelection | HighlightSelection | Undo | Redo | ToggleCheckbox
        | TableNextCell | TablePrevCell | TableNextRow | TablePrevRow | TableMoveRowUp
        | TableMoveRowDown | TableMoveColumnLeft | TableMoveColumnRight | TableInsertRowAbove
        | TableInsertRowBelow | TableInsertColumnLeft | TableInsertColumnRight | TableDeleteRow
        | TableDeleteColumn | TableInsertBreak | InsertTable | InsertImage | InsertLink
        | PasteImage
        | InsertFootnote | DeleteFootnote | RenumberFootnotes | FixListNumbering | SearchReplace
        | SearchReplaceAll | FollowLinkUnderCursor | NavigateBack | NavigateForward
        | GoToSection | OpenDoc(_) | Open | Save | SaveAs | ExportHtml
        | OpenInExternalEditor
        // Neither reads nor writes the document: mode transitions and persisted-setting flips.
        | EnterEditMode | ExitToPreview | ToggleRawMode | ToggleTableButtons | ToggleBigH1
        | ToggleLineNumbers | ToggleBlinkCursor | ToggleAutosave | ToggleVisualLineNav
        | ToggleVimMode | ToggleLimitWidth | ToggleDiffOnChange
        // The two bespoke command vocabularies, which each gate names explicitly: "is a diff
        // command" is a fact about one context, not a capability.
        | OpenSearch | SearchNext | SearchPrev | SearchExit | DiffNext | DiffPrev
        | DiffAcceptHunk | DiffRejectHunk | DiffAcceptAll | DiffRejectAll | DiffResetHunk
        | DiffExit => {}
    }
    ActionCaps {
        mutates_buffer,
        navigates_away,
        needs_path,
        read_only_nav,
        always_safe,
    }
}

/// Default-deny gate over [`Action`]s in diff mode: `Some(action)` when allowed in Review
/// sub-mode, the only one today.
///
/// The narrowest of the three gates — the review has no text-editing UI at all, so nothing beyond
/// the always-safe set and diff's own vocabulary passes, not even cursor motion, which would move
/// a cursor the stacked view does not draw.
pub(super) fn diff_safe_action(action: &Action) -> Option<Action> {
    use Action::*;
    let allowed = action_caps(action).always_safe
        || matches!(
            action,
            DiffNext
                | DiffPrev
                | DiffAcceptHunk
                | DiffRejectHunk
                | DiffAcceptAll
                | DiffRejectAll
                | DiffResetHunk
                | DiffExit
        );
    allowed.then(|| action.clone())
}

/// Default-deny gate over [`Action`]s while a *capturing* (replace) search flow is active;
/// navigate-only flows never reach it.
///
/// Allowed: the flow's own commands, read-only navigation, and the always-safe set.  Buffer
/// mutation stays unavailable, as does navigating away — that abandons a task in progress.
pub(super) fn search_safe_action(action: &Action) -> Option<Action> {
    use Action::*;
    let caps = action_caps(action);
    let allowed = caps.always_safe
        || caps.read_only_nav
        || matches!(
            action,
            OpenSearch
                | SearchNext
                | SearchPrev
                | SearchReplace
                | SearchReplaceAll
                | SearchExit
                // Undo / redo mutate but are allowed: taking back a replace is part of the flow.
                | Undo
                | Redo
                // Saving mid-flow writes the document the flow is already editing — no detour.
                | Save
                | SaveAs
        );
    allowed.then(|| action.clone())
}

/// Default-deny gate over [`Action`]s while the live document is read-only (today: a page of the
/// embedded manual).  Refuses exactly `mutates_buffer` and `needs_path`; everything else is
/// allowed, `navigates_away` emphatically included, since cross-linking is what a manual is for.
///
/// A *courtesy*, not the guarantee: that is made two layers down by `enter_edit_if_preview` and
/// `EditorState::apply_delta`.  This exists so a palette pick of `Save` doesn't open a Save-as
/// prompt for a document nobody can own, and so the refusal is silent rather than
/// half-performed.
pub(super) fn readonly_safe_action(action: &Action) -> bool {
    let caps = action_caps(action);
    !caps.mutates_buffer && !caps.needs_path
}

/// True when the editor's cursor sits inside a table block.  Mirrors `edit_ops::cursor_in_table`,
/// re-implemented to keep the App free of a cross-module private dep.
pub(super) fn cursor_in_table(state: &EditorState) -> bool {
    // A read-only document has no column to reorder, so `Alt+Left` / `Alt+Right` must stay the
    // Back / Forward chord the hint row advertises even when the cursor sits inside a table.
    if state.readonly {
        return false;
    }
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let source = state.buffer.contents();
    crate::editor::table_edit::find_table_at(&source, cursor_byte).is_some()
}

/// Reshape `clipboard` into the payload replacing a vim VisualLine selection over `range`: made
/// *linewise*, ending with a newline whenever the span it replaces does.
///
/// `visual_line_char_range` includes the last line's trailing newline, so charwise text pasted
/// over it would weld the following line onto the paste.  Text already ending in a newline is
/// returned unchanged so no blank line creeps in, and a final line without one gets nothing
/// appended.
///
/// `None` when there is nothing to paste (empty clipboard *and* kill-ring), which the caller
/// treats as a no-op rather than replacing the lines with a bare newline.
///
/// Pure in `clipboard`; the OS read stays at the call site so this is unit-testable.
fn linewise_paste_payload(
    buffer: &crate::document::Buffer,
    range: &std::ops::Range<usize>,
    clipboard: String,
) -> Option<String> {
    if clipboard.is_empty() {
        return None;
    }
    let replaced_ends_with_newline =
        range.end > range.start && buffer.slice_to_string(range.end - 1, range.end) == "\n";
    if replaced_ends_with_newline && !clipboard.ends_with('\n') {
        return Some(clipboard + "\n");
    }
    Some(clipboard)
}

/// Translate a wheel event into a `ModalState::scroll_by` delta, honoring the configured
/// `mouse_scroll_lines`.  `0` for non-wheel events, so callers can forward every `Event::Mouse`.
pub(super) fn modal_wheel_delta(event: &MouseEvent, wheel_step: usize) -> i32 {
    let step = wheel_step.max(1) as i32;
    match event.kind {
        MouseEventKind::ScrollUp => -step,
        MouseEventKind::ScrollDown => step,
        _ => 0,
    }
}

/// Private extension trait letting `DefaultHandler` process raw crossterm events (filtering for
/// KeyPress) without putting that in `ModeHandler`, which takes already-filtered `KeyEvent`s.
pub(super) trait HandleEvent {
    fn handle_event(&mut self, event: Event, state: &EditorState) -> Option<crate::config::Action>;
}

impl<'k> HandleEvent for DefaultHandler<'k> {
    fn handle_event(&mut self, event: Event, state: &EditorState) -> Option<crate::config::Action> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle(key, state),
            _ => None,
        }
    }
}

impl App {
    /// Convenience for `!self.modal_stack.is_empty()`.
    pub(super) fn any_modal_open(&self) -> bool {
        !self.modal_stack.is_empty()
    }

    /// Resolve an [`Action`] whose meaning depends on the cursor position, so every downstream
    /// gate judges what will actually run.
    ///
    /// One case today: `Alt+Left` / `Alt+Right` bind to `TableMoveColumnLeft` /
    /// `TableMoveColumnRight`, which mean "reorder a column" inside a table and "navigate back /
    /// forward" outside one.
    ///
    /// Deliberately ahead of the gates: a gate judging the pre-redirect action would deny a
    /// navigation because of what a *different* action would have done.
    fn normalize_context_action(&self, action: Action) -> Action {
        match action {
            Action::TableMoveColumnLeft if !cursor_in_table(&self.editor) => Action::NavigateBack,
            Action::TableMoveColumnRight if !cursor_in_table(&self.editor) => {
                Action::NavigateForward
            }
            other => other,
        }
    }

    /// Whatever image the clipboard snapshot offers, resolved to the
    /// Markdown destination to insert.
    ///
    /// The priority between a copied file, a screenshot and a path typed
    /// as text lives in [`paste::select`], not here — this only supplies
    /// the two things it cannot know: where screenshots go, and which
    /// document a relative directory resolves against.
    fn image_paste_outcome(&self, data: &ClipboardData) -> paste::Outcome {
        let dir =
            paste::images_dir_from_env().unwrap_or_else(|| self.config.images.save_dir.clone());
        paste::destination(
            data,
            &paste::SaveTarget {
                dir: &dir,
                doc_path: self.file_path.as_deref(),
            },
        )
    }

    /// Insert the pasted image, or tell the user why there was none.
    ///
    /// The one insert site for every clipboard image: the file the user
    /// copied, the screenshot just written, and the path they copied as
    /// text all arrive here as a [`paste::Outcome`].
    fn report_pasted_image(
        &mut self,
        outcome: paste::Outcome,
        doc_height: usize,
        doc_width: usize,
    ) {
        let destination = match outcome {
            paste::Outcome::Insert(destination) => destination,
            paste::Outcome::Failed(reason) => return self.notify(reason, ModalKind::Error),
            paste::Outcome::NoImage => {
                return self.flash("No image or image path on the clipboard", MessageKind::Info)
            }
        };
        let inserted = crate::editor::edit_ops::insert_image_reference_at_cursor(
            &mut self.editor,
            &destination,
            doc_height,
            doc_width,
        );
        if !inserted {
            self.notify("Cannot insert image inside this block", ModalKind::Warning);
        }
    }

    /// Intercept App-level actions (`FollowLinkUnderCursor`,
    /// `NavigateBack`, `NavigateForward`) before they hit `edit_ops::apply`.
    ///
    /// Returns `true` when the action was fully handled here; `false`
    /// means the caller should fall through to `edit_ops::apply`.
    /// Intercept App-level actions before they reach `edit_ops::apply`.  `true` when fully
    /// handled here; `false` means fall through.
    pub(super) fn handle_app_action(
        &mut self,
        action: &Action,
        doc_height: usize,
        doc_width: usize,
    ) -> bool {
        match action {
            Action::FollowLinkUnderCursor => {
                if let Some(target) = self.resolve_link_at_cursor() {
                    self.follow_link(target, doc_height, doc_width);
                }
                true
            }
            Action::ShowAbout => {
                self.open_about_modal();
                true
            }
            Action::CheckForUpdates => {
                self.open_update_modal();
                true
            }
            Action::NavigateBack => {
                self.navigate_back(doc_height, doc_width);
                true
            }
            Action::NavigateForward => {
                self.navigate_forward(doc_height, doc_width);
                true
            }
            // Palette + configuration overlays.
            Action::ShowCommandPalette => {
                self.open_command_palette();
                true
            }
            Action::GoToSection => {
                self.open_section_picker(doc_width);
                true
            }
            Action::OpenSearch => {
                self.open_search_modal();
                true
            }
            Action::ShowMarkdownCheatSheet => {
                self.open_markdown_cheat_sheet();
                true
            }
            Action::OpenDoc(id) => {
                // Opening a page replaces the document, so an unsaved buffer needs the same
                // guard a cross-file link gets.
                if self.editor.dirty {
                    self.open_dirty_guard(crate::app::nav::NavPending::Doc(*id), None);
                } else {
                    self.open_doc_page(*id, None, doc_height, doc_width);
                }
                true
            }
            Action::OpenSettings => {
                self.open_settings_overlay();
                true
            }
            Action::OpenWelcome => {
                self.open_welcome_modal();
                true
            }
            Action::OpenKeybinds => {
                self.open_keybinds_overlay();
                true
            }
            Action::SwitchTheme => {
                self.open_theme_picker();
                true
            }
            Action::CreateCustomTheme => {
                self.open_export_theme_modal();
                true
            }
            Action::ExportHtml => {
                self.open_export_modal();
                true
            }
            Action::OpenConfigFolder => {
                if let Some(dir) = Config::config_dir() {
                    self.spawn_open_worker(dir.display().to_string());
                } else {
                    self.notify("No config directory available", ModalKind::Error);
                }
                true
            }
            // Every `NOT_YET_IMPLEMENTED` entry lands here and surfaces the generic notice.
            a if NOT_YET_IMPLEMENTED.contains(a) => {
                self.notify_not_implemented();
                true
            }
            Action::OpenInExternalEditor => {
                if self.editor.buffer.path().is_none() {
                    self.notify("No file path for buffer", ModalKind::Error);
                } else {
                    // The invocation itself needs the live `Terminal` handle, owned by the run
                    // loop.
                    self.pending_open_file_in_editor = true;
                    self.needs_draw = true;
                }
                true
            }
            Action::ToggleTableButtons => {
                // Inert on terminals without mouse reporting, where the glyphs would confuse.
                if self.capabilities.mouse {
                    self.config.table.show_buttons = !self.config.table.show_buttons;
                    self.toggle_persisted_setting(
                        settings_overlay::LABEL_TABLE_BUTTONS,
                        self.config.table.show_buttons,
                    );
                } else {
                    self.notify("Mouse not supported on this terminal", ModalKind::Error);
                    self.needs_draw = true;
                }
                true
            }
            Action::ToggleBigH1 => {
                self.config.editor.big_h1 = !self.config.editor.big_h1;
                self.toggle_persisted_setting(
                    settings_overlay::LABEL_BIG_H1,
                    self.config.editor.big_h1,
                );
                true
            }
            Action::ToggleLineNumbers => {
                self.config.editor.show_line_numbers = !self.config.editor.show_line_numbers;
                self.toggle_persisted_setting(
                    settings_overlay::LABEL_LINE_NUMBERS,
                    self.config.editor.show_line_numbers,
                );
                true
            }
            Action::ToggleBlinkCursor => {
                self.config.editor.cursor_blink = !self.config.editor.cursor_blink;
                self.toggle_persisted_setting(
                    settings_overlay::LABEL_BLINK_CURSOR,
                    self.config.editor.cursor_blink,
                );
                true
            }
            Action::ToggleAutosave => {
                self.config.editor.autosave_enabled = !self.config.editor.autosave_enabled;
                self.toggle_persisted_setting(
                    settings_overlay::LABEL_AUTOSAVE,
                    self.config.editor.autosave_enabled,
                );
                true
            }
            Action::ToggleVisualLineNav => {
                self.config.editor.visual_line_nav = !self.config.editor.visual_line_nav;
                self.toggle_persisted_setting(
                    settings_overlay::LABEL_VISUAL_LINE_NAV,
                    self.config.editor.visual_line_nav,
                );
                true
            }
            Action::ToggleVimMode => {
                // Vim mode is stored as the modal handler name, not a bool; `apply_live_update`
                // then rebuilds the live `VimState`.
                let enabling = self.config.modal.handler != VIM_HANDLER;
                self.config.modal.handler = if enabling {
                    VIM_HANDLER
                } else {
                    DEFAULT_HANDLER
                }
                .to_owned();
                self.toggle_persisted_setting(settings_overlay::LABEL_VIM_MODE, enabling);
                true
            }
            Action::ToggleLimitWidth => {
                self.config.editor.max_width_enabled = !self.config.editor.max_width_enabled;
                self.toggle_persisted_setting(
                    settings_overlay::LABEL_LIMIT_WIDTH,
                    self.config.editor.max_width_enabled,
                );
                true
            }
            Action::ToggleDiffOnChange => {
                self.config.editor.diff_on_change = !self.config.editor.diff_on_change;
                self.toggle_persisted_setting(
                    settings_overlay::LABEL_DIFF_ON_CHANGE,
                    self.config.editor.diff_on_change,
                );
                true
            }
            Action::InsertTable => {
                // The blank-line guard runs before the modal opens, so a non-blank cursor gets
                // an immediate warning.  It subsumes every block-kind case without classifying.
                let source = self.editor.buffer.contents();
                let cursor_byte = self
                    .editor
                    .buffer
                    .rope()
                    .char_to_byte(self.editor.cursor.offset);
                if crate::editor::table_edit::cursor_line_is_blank(&source, cursor_byte) {
                    self.open_insert_table_modal();
                } else {
                    self.notify("Insert Table requires a blank line", ModalKind::Warning);
                }
                self.needs_draw = true;
                true
            }
            // Image / link snippets share one pre-flight — the target block must host inline
            // Markdown — run inside the insert functions, against the post-sync insert offset.
            Action::InsertImage | Action::InsertLink => {
                let is_image = matches!(action, Action::InsertImage);
                let inserted = if is_image {
                    crate::editor::edit_ops::insert_image_at_cursor(
                        &mut self.editor,
                        doc_height,
                        doc_width,
                    )
                } else {
                    crate::editor::edit_ops::insert_link_at_cursor(
                        &mut self.editor,
                        doc_height,
                        doc_width,
                    )
                };
                if !inserted {
                    let what = if is_image { "an image" } else { "a link" };
                    self.notify(
                        format!("Cannot insert {what} inside this block"),
                        ModalKind::Warning,
                    );
                }
                self.needs_draw = true;
                true
            }
            // One clipboard read per paste, and one place that decides
            // what the snapshot means.  The two actions differ only in
            // whether text on the clipboard keeps the chord ordinary:
            // `PasteImage` is the explicit request, so it never defers.
            Action::PasteImage | Action::Paste => {
                let data = self.clipboard.read();
                let plain_paste = matches!(*action, Action::Paste);
                if plain_paste && paste::plain_paste_wants_text(&data) {
                    return false;
                }
                let outcome = self.image_paste_outcome(&data);
                if plain_paste && outcome == paste::Outcome::NoImage {
                    // Nothing image-shaped on the clipboard: the ordinary
                    // text paste (kill-ring included) runs instead.
                    return false;
                }
                self.report_pasted_image(outcome, doc_height, doc_width);
                self.needs_draw = true;
                true
            }
            Action::InsertFootnote => {
                crate::editor::edit_ops::insert_footnote_at_cursor(
                    &mut self.editor,
                    doc_height,
                    doc_width,
                );
                self.needs_draw = true;
                true
            }
            Action::DeleteFootnote => {
                if !crate::editor::edit_ops::delete_footnote_at_cursor(
                    &mut self.editor,
                    doc_height,
                    doc_width,
                ) {
                    self.flash("Cursor is not on a footnote", MessageKind::Info);
                }
                self.needs_draw = true;
                true
            }
            Action::RenumberFootnotes => {
                if !crate::editor::edit_ops::renumber_footnotes(
                    &mut self.editor,
                    doc_height,
                    doc_width,
                ) {
                    self.flash("Footnotes already in order", MessageKind::Info);
                }
                self.needs_draw = true;
                true
            }
            Action::FixListNumbering => {
                use crate::editor::edit_ops::FixListNumbering;
                match crate::editor::edit_ops::fix_list_numbering(
                    &mut self.editor,
                    doc_height,
                    doc_width,
                ) {
                    FixListNumbering::Fixed => {}
                    FixListNumbering::AlreadyCorrect => {
                        self.flash("List numbering already correct", MessageKind::Info);
                    }
                    FixListNumbering::NotOrdered => {
                        self.flash("Cursor is not in an ordered list", MessageKind::Info);
                    }
                }
                self.needs_draw = true;
                true
            }
            Action::SaveAs => {
                self.open_save_as_modal(None);
                self.needs_draw = true;
                true
            }
            // Hoisted out of `edit_ops::apply` so every save path routes through
            // `App::save_buffer`, the single call site for `Buffer::save_file`.  The flash fires
            // here because `dispatch_action`'s only runs when this returns `false`.
            Action::Save => {
                if self.editor.mode == crate::editor::Mode::Diff {
                    self.flash("Resolve diff before saving", MessageKind::Info);
                    return true;
                }
                // A never-saved buffer has no destination: prompt rather than let `save_file`
                // fail into a generic "Save failed".
                if self.editor.buffer.path().is_none() {
                    self.open_save_as_modal(None);
                    self.needs_draw = true;
                    return true;
                }
                let dirty_before = self.editor.dirty;
                if let Err(e) = self.save_buffer() {
                    tracing::warn!(error = %e, "save failed");
                }
                self.flash_for_action(&Action::Save, dirty_before);
                true
            }
            _ => false,
        }
    }

    /// Flip-and-persist path for the command-palette setting toggles.  The caller has already
    /// mutated `config`; this writes `config.toml`, pushes the change through the settings
    /// overlay's
    /// [`apply_live_update`](crate::app::modal::settings::apply_live_update) so the two surfaces
    /// can't diverge, and flashes the setting's new value by name.
    ///
    /// The live update runs even when the save fails: `config` is already flipped, so the setting
    /// takes effect for the session, just unpersisted.
    fn toggle_persisted_setting(&mut self, label: &str, new_state: bool) {
        let saved = self.config.save();
        modal::settings::apply_live_update(label, self);
        match saved {
            Ok(()) => {
                let state = if new_state { "on" } else { "off" };
                let msg = format!(
                    "{}: {state}{}",
                    label.trim(),
                    crate::config::unpersisted_suffix()
                );
                self.flash(msg, MessageKind::Info);
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to persist palette setting toggle");
                self.notify(format!("Config save failed: {e}"), ModalKind::Error);
            }
        }
        self.needs_draw = true;
    }

    /// Pop the topmost modal, dispatch the key to it, and apply the [`ModalOutcome`].
    /// Pop-then-dispatch is what lets the handler take `&mut App` without a borrow conflict.
    pub(super) fn dispatch_modal_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        doc_height: usize,
        doc_width: usize,
    ) {
        let Some(mut top) = self.modal_stack.pop() else {
            return;
        };
        let outcome = top.handle_key(key, self, doc_height, doc_width);
        match outcome {
            ModalOutcome::Continue => self.modal_stack.push(top),
            ModalOutcome::ContinueAnd(cb) => {
                self.modal_stack.push(top);
                cb(self);
            }
            ModalOutcome::Close => {}
            ModalOutcome::CloseAnd(cb) => cb(self),
        }
    }

    /// Route a bracketed paste to the topmost modal, as [`Self::dispatch_modal_key`] does.  Only
    /// the text-input modals act on it.
    pub(super) fn dispatch_modal_paste(&mut self, text: &str) {
        let Some(mut top) = self.modal_stack.pop() else {
            return;
        };
        let outcome = top.handle_paste(text);
        match outcome {
            ModalOutcome::Continue => self.modal_stack.push(top),
            ModalOutcome::ContinueAnd(cb) => {
                self.modal_stack.push(top);
                cb(self);
            }
            ModalOutcome::Close => {}
            ModalOutcome::CloseAnd(cb) => cb(self),
        }
    }

    /// Route a left-button click at `(col, row)` to the topmost modal, as
    /// [`Self::dispatch_modal_key`] does.
    pub(super) fn dispatch_modal_click(&mut self, col: u16, row: u16) {
        let Some(mut top) = self.modal_stack.pop() else {
            return;
        };
        let outcome = top.handle_click(col, row, self);
        match outcome {
            ModalOutcome::Continue => self.modal_stack.push(top),
            ModalOutcome::ContinueAnd(cb) => {
                self.modal_stack.push(top);
                cb(self);
            }
            ModalOutcome::Close => {}
            ModalOutcome::CloseAnd(cb) => cb(self),
        }
    }

    /// Push the generic "feature not implemented yet" notice, for the [`NOT_YET_IMPLEMENTED`]
    /// dispatch arm.
    pub(super) fn notify_not_implemented(&mut self) {
        self.notify("This feature is not implemented yet.", ModalKind::Normal);
    }

    /// Open the `Save / Discard / Cancel` modal, for `Quit` on a dirty buffer.
    pub(super) fn open_quit_confirm(&mut self) {
        let display = self
            .file_path
            .as_deref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Current buffer".to_owned());
        self.modal_stack
            .push(Box::new(modal::QuitConfirmModal::new(&display)));
    }

    /// Open the Markdown syntax cheat-sheet popover.
    pub fn open_markdown_cheat_sheet(&mut self) {
        self.modal_stack
            .push(Box::new(modal::CheatSheetModal::new()));
    }

    /// Open the About page.  Touches no network — its `[ Check for updates ]` button is the only
    /// thing that reaches GitHub (see [`App::open_update_modal`]).
    pub fn open_about_modal(&mut self) {
        if self.modal_stack.contains::<modal::AboutModal>() {
            return;
        }
        self.modal_stack.push(Box::new(modal::AboutModal::new()));
        self.needs_draw = true;
    }

    /// Open the fuzzy-searchable command palette.
    pub fn open_command_palette(&mut self) {
        let keymap = self.ensure_keymap_clone();
        let vim_enabled = self.vim.is_some();
        self.modal_stack
            .push(Box::new(modal::CommandPaletteModal::new(
                &keymap,
                vim_enabled,
            )));
    }

    /// Build `self.keymap` if needed and return a clone, so callers need no borrow on `self`.
    pub(super) fn ensure_keymap_clone(&mut self) -> KeyMap {
        if self.keymap.is_none() {
            match KeyMap::build(&self.keybindings) {
                Ok(km) => self.keymap = Some(km),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to build KeyMap on demand");
                    return KeyMap::build(&KeyBindingOverrides::default())
                        .expect("default keymap always builds");
                }
            }
        }
        self.keymap.as_ref().unwrap().clone()
    }

    /// Dispatch a resolved [`Action`] through the unified pipeline.  The run-loop keystroke arm
    /// and the command palette both funnel through here, so `handle_app_action`, the dirty-quit
    /// guard, `edit_ops::apply`, scroll tracking, flashes, and link-follow draining are sequenced
    /// in exactly one place.
    pub fn dispatch_action(&mut self, action: Action, doc_height: usize, doc_width: usize) {
        // Resolve the context-dependent meaning *before* any gate judges it — otherwise the
        // read-only gate sees `TableMoveColumnLeft` and denies what is really a Back navigation.
        let action = self.normalize_context_action(action);
        // Outermost on purpose: `search_safe_action` allows `SearchReplace`, so a replace flow
        // started while reading would otherwise rewrite the in-memory page.  The gates compose by
        // narrowing.
        //
        // Silent, and keyed on `editor.readonly` rather than on `open_doc`, which leaves the door
        // open for a `--readonly` flag or a file opened without write permission.
        if self.editor.readonly && !readonly_safe_action(&action) {
            return;
        }
        // Gated *before* `handle_app_action` (unlike diff) so app-level actions that mutate or
        // navigate away can't fire mid-flow; allowed openers are re-routed inside
        // `dispatch_search_action`.  A navigate-only flow does not capture — see
        // `search_flow_captures`.
        if self.search_flow_captures() {
            let Some(safe) = search_safe_action(&action) else {
                self.flash_action_unavailable("search");
                return;
            };
            self.dispatch_search_action(safe, doc_height, doc_width);
            return;
        }
        // Non-capturing navigate flow: its own navigation actions still route to the search
        // dispatcher; everything else falls through to normal editing, highlights intact.
        if self.editor.search.is_some()
            && matches!(
                action,
                Action::SearchNext | Action::SearchPrev | Action::SearchExit
            )
        {
            self.dispatch_search_action(action, doc_height, doc_width);
            return;
        }
        // Visual `Ctrl-C` / `Ctrl-X` / `Ctrl-V` act on the *widened* range so the clipboard
        // matches the highlight, widened through the shared `vim_ops::visual_span` the render and
        // operator paths also use.  `selection` itself is never snapped: Copy restores the stored
        // span so a continued Visual session keeps its anchor.
        if matches!(action, Action::Copy | Action::Cut | Action::Paste) {
            if let Some(kind) = self.vim.as_ref().and_then(|v| v.visual_kind()) {
                self.dispatch_visual_clipboard(action, kind, doc_height, doc_width);
                return;
            }
        }
        // Denied *before* `handle_app_action`, like the search flow: that function answers
        // `true` for everything it handles, so a gate behind it never sees `InsertTable`, the
        // footnote fix-ups or `FollowLinkUnderCursor` — all off the diff allowlist, and all
        // reachable mid-review from the palette, which is on it.
        //
        // Only the refusal moves; dispatch stays below, so an allowed action still reaches
        // `handle_app_action` first and falls through to `dispatch_diff_action`.
        if self.editor.mode == crate::editor::Mode::Diff && diff_safe_action(&action).is_none() {
            self.flash_action_unavailable("diff review");
            return;
        }
        let handled = self.handle_app_action(&action, doc_height, doc_width);
        if !handled {
            // Before the generic dirty-quit guard: in diff mode the buffer still holds the
            // pre-merge text, so that guard's "Save" would persist the wrong contents.
            if self.editor.mode == crate::editor::Mode::Diff {
                self.dispatch_diff_action(action, doc_height, doc_width);
                return;
            }
            if matches!(action, Action::Quit) && self.editor.dirty {
                self.open_quit_confirm();
                return;
            }
            let dirty_before = self.editor.dirty;
            let scroll_before = self.editor.scroll;
            let quit = edit_ops::apply(&mut self.editor, action.clone(), doc_height, doc_width);
            if quit {
                self.should_quit = true;
            }
            if self.editor.scroll != scroll_before {
                self.mark_scrolling();
            }
            self.flash_for_action(&action, dirty_before);
            if let Some(target) = self.editor.pending_link_follow.take() {
                self.follow_link(target, doc_height, doc_width);
            }
        }
    }

    /// Copy, cut, or paste over a vim Visual selection, widening it to the span actually on
    /// screen (`vim_ops::visual_span`) without snapping the persistent half-open `selection`.
    /// `Copy` restores the stored span so the user can keep extending; `Cut` and `Paste` consume
    /// it and exit Visual.  The widening lives here so `edit_ops` stays vim-agnostic.
    fn dispatch_visual_clipboard(
        &mut self,
        action: Action,
        kind: crate::editor::vim_ops::VisualKind,
        doc_height: usize,
        doc_width: usize,
    ) {
        let Some(sel) = self.editor.selection else {
            return;
        };
        let range = crate::editor::vim_ops::visual_span(&sel, &self.editor.buffer, Some(kind));
        // A VisualLine paste needs a newline-terminated payload so it can't weld onto the
        // following line, and an empty one must bail before the widening rather than consume the
        // lines.  Charwise paste is an ordinary span replacement `edit_ops` handles.
        let payload = match action {
            Action::Paste if kind == crate::editor::vim_ops::VisualKind::Line => {
                let clipboard = edit_ops::clipboard_text(&self.editor);
                let Some(text) = linewise_paste_payload(&self.editor.buffer, &range, clipboard)
                else {
                    return;
                };
                Some(text)
            }
            Action::Paste => {
                // As above: nothing to paste must not consume the span or exit Visual.
                if edit_ops::clipboard_text(&self.editor).is_empty() {
                    return;
                }
                None
            }
            _ => None,
        };
        let widened = crate::document::Selection {
            anchor: range.start,
            active: range.end,
        };
        self.editor.selection = Some(widened);
        let dirty_before = self.editor.dirty;
        match &payload {
            Some(text) => edit_ops::paste_text(&mut self.editor, text, doc_height, doc_width),
            None => {
                edit_ops::apply(&mut self.editor, action.clone(), doc_height, doc_width);
            }
        }
        // The shared dispatch's `flash_for_action` is skipped by our early return.
        self.flash_for_action(&action, dirty_before);
        if matches!(action, Action::Cut | Action::Paste) {
            // The span is gone, so drop back to Normal.
            if let Some(vim) = self.vim.as_mut() {
                vim.sub_mode = crate::input::VimSubMode::Normal;
                vim.visual_anchor = None;
            }
            self.editor.selection = None;
        } else {
            // Copy left the buffer untouched, so restore the stored span.
            self.editor.selection = Some(sel);
        }
        self.needs_draw = true;
    }

    /// Shared free-scroll arms for the diff and search-flow dispatchers: the viewport moves
    /// without dragging the cursor along, unlike `edit_ops` scrolling.  `true` when handled.
    pub(super) fn dispatch_flow_scroll(
        &mut self,
        action: &Action,
        doc_height: usize,
        doc_width: usize,
    ) -> bool {
        match action {
            Action::ScrollUp => {
                if self.editor.scroll > 0 {
                    self.editor.scroll = self.editor.scroll.saturating_sub(1);
                    self.mark_scrolling();
                    self.needs_draw = true;
                }
            }
            Action::ScrollDown => {
                let total = self.editor.total_visual_rows_for_mode(doc_width);
                let max = total.saturating_sub(1);
                if self.editor.scroll < max {
                    self.editor.scroll += 1;
                    self.mark_scrolling();
                    self.needs_draw = true;
                }
            }
            Action::ScrollPageUp => {
                self.editor.scroll = self.editor.scroll.saturating_sub(doc_height.max(1));
                self.mark_scrolling();
                self.needs_draw = true;
            }
            Action::ScrollPageDown => {
                let total = self.editor.total_visual_rows_for_mode(doc_width);
                let max = total.saturating_sub(1);
                self.editor.scroll = (self.editor.scroll + doc_height.max(1)).min(max);
                self.mark_scrolling();
                self.needs_draw = true;
            }
            Action::ScrollToTop => {
                self.editor.scroll = 0;
                self.mark_scrolling();
                self.needs_draw = true;
            }
            Action::ScrollToBottom => {
                let total = self.editor.total_visual_rows_for_mode(doc_width);
                self.editor.scroll = total.saturating_sub(doc_height.max(1));
                self.mark_scrolling();
                self.needs_draw = true;
            }
            _ => return false,
        }
        true
    }

    /// Dispatch a single action while `Mode::Diff` is active; the caller has already filtered it
    /// through [`diff_safe_action`].
    pub(super) fn dispatch_diff_action(
        &mut self,
        action: Action,
        doc_height: usize,
        doc_width: usize,
    ) {
        use crate::diff::Decision;
        if self.dispatch_flow_scroll(&action, doc_height, doc_width) {
            return;
        }
        // A read-only review (`--diff`) has no decision vocabulary — the sides are paths git
        // chose.  Denied here rather than in `diff_safe_action`, which answers the
        // presentation-independent question and holds no `DiffState`; and not merely by omission
        // from the key table, since the palette reaches these too.
        if self.editor.diff.as_ref().is_some_and(|d| d.read_only)
            && matches!(
                action,
                Action::DiffAcceptHunk
                    | Action::DiffRejectHunk
                    | Action::DiffAcceptAll
                    | Action::DiffRejectAll
                    | Action::DiffResetHunk
            )
        {
            self.flash("This review is read-only", MessageKind::Info);
            return;
        }
        match action {
            Action::DiffNext => {
                // Manual navigation supersedes a deferred auto-advance and never triggers the
                // resolve-confirm flow — tabbing among decided hunks must not pop the modal.
                // See `check_diff_resolution` for the flow's two entry points.
                self.cancel_diff_advance();
                if let Some(d) = self.editor.diff.as_mut() {
                    d.advance_focus();
                    self.editor
                        .scroll_focused_hunk_into_view(doc_height, doc_width);
                    self.needs_draw = true;
                }
            }
            Action::DiffPrev => {
                self.cancel_diff_advance();
                if let Some(d) = self.editor.diff.as_mut() {
                    d.retreat_focus();
                    self.editor
                        .scroll_focused_hunk_into_view(doc_height, doc_width);
                    self.needs_draw = true;
                }
            }
            Action::DiffAcceptHunk => self.decide_focused_hunk(Decision::Accepted),
            Action::DiffRejectHunk => self.decide_focused_hunk(Decision::Rejected),
            // Decisions are not on an undo stack, so a one-keystroke override of every hunk
            // goes behind a confirm modal; `apply_diff_bulk_decision` does the work.
            Action::DiffAcceptAll => self.open_diff_bulk_confirm(Decision::Accepted),
            Action::DiffRejectAll => self.open_diff_bulk_confirm(Decision::Rejected),
            Action::DiffResetHunk => {
                // Cancel any in-flight advance first, so a freshly-reset hunk keeps focus.
                self.cancel_diff_advance();
                let reset = self.editor.diff.as_mut().is_some_and(|d| d.reset_focused());
                if reset {
                    self.needs_draw = true;
                }
            }
            Action::DiffExit => {
                // Esc cannot exit while any hunk is pending: fully resolved opens the
                // apply-confirm modal (resolve-flow entry point 2), anything pending no-ops with
                // a hint.  Apply or Quit are the two exits.
                self.cancel_diff_advance();
                // A read-only review has no editor behind it and nothing to resolve, so Esc
                // leaves the process — which is what advances a `git difftool` walk.
                if self.editor.diff.as_ref().is_some_and(|d| d.read_only) {
                    self.should_quit = true;
                } else if self.editor.diff.as_ref().is_some_and(|d| d.all_resolved()) {
                    self.check_diff_resolution();
                } else {
                    self.flash(
                        "Resolve every hunk before exiting diff mode",
                        MessageKind::Info,
                    );
                }
            }
            // `SaveAs` never reaches here: `diff_safe_action` excludes it, since re-pointing the
            // buffer path and watcher would desync the live diff.
            Action::Quit => {
                // Nothing is at stake in a read-only review, so no confirmation.  The flag tells
                // `main` to end the whole `git difftool` walk, not just this file.
                if self.editor.diff.as_ref().is_some_and(|d| d.read_only) {
                    self.diff_stop_walk = true;
                    self.should_quit = true;
                    return;
                }
                // An active review is unapplied work, so warn first, as the dirty-buffer quit
                // guard does — without stacking a second copy of the modal.
                if !self
                    .modal_stack
                    .contains::<crate::app::modal::DiffQuitConfirmModal>()
                {
                    self.modal_stack
                        .push(Box::new(crate::app::modal::DiffQuitConfirmModal::new()));
                    self.needs_draw = true;
                }
            }
            Action::ShowCommandPalette => {
                self.open_command_palette();
            }
            Action::ShowMarkdownCheatSheet => {
                self.open_markdown_cheat_sheet();
            }
            Action::ShowAbout => {
                self.open_about_modal();
            }
            Action::CheckForUpdates => {
                self.open_update_modal();
            }
            Action::OpenSettings => {
                self.open_settings_overlay();
            }
            Action::OpenWelcome => {
                self.open_welcome_modal();
            }
            Action::OpenKeybinds => {
                self.open_keybinds_overlay();
            }
            Action::SwitchTheme => {
                self.open_theme_picker();
            }
            Action::CreateCustomTheme => {
                self.open_export_theme_modal();
            }
            Action::OpenConfigFolder => {
                if let Some(dir) = Config::config_dir() {
                    self.spawn_open_worker(dir.display().to_string());
                }
            }
            // Everything else passed `diff_safe_action` but needs no specific arm here.
            _ => {}
        }
    }

    /// Record an accept/reject on the focused hunk and arm the deferred advance, so the user sees
    /// the decision land before focus moves.  A prior pending advance is flushed first, so rapid
    /// taps walk through hunks rather than re-deciding one.  On the final hunk, the deferred
    /// advance's `check_diff_resolution` opens the confirm modal — the flow is triggered by the
    /// *act* of deciding, not by landing in a resolved state.
    fn decide_focused_hunk(&mut self, decision: crate::diff::Decision) {
        if self.diff_advance_pending_since.is_some() {
            self.apply_diff_advance();
        }
        let decided = self
            .editor
            .diff
            .as_mut()
            .is_some_and(|d| d.decide_focused(decision));
        if decided {
            self.needs_draw = true;
            self.arm_diff_advance();
        }
    }

    /// Open the bulk-decision confirm modal.  No-op without a diff, or when one is already
    /// stacked (a held key must not stack duplicates).  The decision waits for `[Yes]`.
    fn open_diff_bulk_confirm(&mut self, decision: crate::diff::Decision) {
        self.cancel_diff_advance();
        if self.editor.diff.is_none() {
            return;
        }
        if self
            .modal_stack
            .contains::<crate::app::modal::DiffBulkConfirmModal>()
        {
            return;
        }
        self.modal_stack
            .push(Box::new(crate::app::modal::DiffBulkConfirmModal::new(
                decision,
            )));
        self.needs_draw = true;
    }

    /// Apply a confirmed bulk decision to every hunk, then run the normal resolution check.
    /// Invoked from the bulk-confirm modal's `[Yes]` callback.
    pub(crate) fn apply_diff_bulk_decision(&mut self, decision: crate::diff::Decision) {
        self.cancel_diff_advance();
        if let Some(d) = self.editor.diff.as_mut() {
            d.bulk_decide(decision);
            self.needs_draw = true;
        }
        self.check_diff_resolution();
    }

    /// Push the apply-confirm modal iff every hunk has been decided.  The *single* place it is
    /// opened, with exactly two callers: `apply_diff_advance` once a decision resolves the final
    /// hunk, and `Action::DiffExit` on an already-resolved diff.  Hunk navigation deliberately
    /// does not call it — tabbing through resolved hunks must not re-open the modal.
    pub(crate) fn check_diff_resolution(&mut self) {
        let Some(diff) = self.editor.diff.as_ref() else {
            return;
        };
        if !diff.all_resolved() {
            return;
        }
        if self
            .modal_stack
            .contains::<crate::app::modal::DiffResolveConfirmModal>()
        {
            return;
        }
        let accepted = diff
            .decisions
            .iter()
            .filter(|d| **d == crate::diff::Decision::Accepted)
            .count();
        let rejected = diff.decisions.len() - accepted;
        self.modal_stack
            .push(Box::new(crate::app::modal::DiffResolveConfirmModal::new(
                accepted, rejected,
            )));
        self.needs_draw = true;
    }

    /// Enter diff-review mode against the on-disk contents the `DirtyConflictModal` was carrying,
    /// pushing the intro modal first unless the user opted out.
    pub(crate) fn enter_diff_mode(&mut self, on_disk: String) {
        // A half-typed vim command line can't survive into diff review: the prompt could never
        // be completed there, and a stale `cmdline` masks the diff hint row.  End its live
        // sessions first, as Esc would — a dangling incsearch session would be reused by the next
        // `/`, and a live `:s` preview must revert its transient edit before the `old` snapshot
        // below, or the diff is taken against preview text and its gates hold forever.
        if let Some(vim) = self.vim.as_mut() {
            crate::editor::vim_ops::end_incsearch(&mut self.editor, &mut vim.incsearch);
            vim.cmdline = None;
            vim.reset_pending();
        }
        crate::editor::vim_ops::clear_substitute_preview(
            &mut self.editor,
            /*restore_view=*/ true,
        );
        // A search flow can't survive either: resolution swaps the buffer out from under the
        // match list.  After `end_incsearch`, so a restored prior hlsearch session goes too.
        self.exit_search_flow();
        let old = self.editor.buffer.contents();
        let Some(diff_state) = crate::diff::DiffState::new(&old, &on_disk) else {
            // The on-disk bytes match the buffer after all (a manual revert before Merge).
            self.flash("No differences to review", MessageKind::Info);
            return;
        };
        let uneven_table_fallback = diff_state.uneven_table_fallback;
        self.editor.enter_diff_mode(diff_state);
        // A clean buffer stays clean during review, so a second external overwrite re-enters
        // this path; without the check the user dismisses one intro modal per overwrite.
        if self.config.editor.show_diff_intro
            && !self
                .modal_stack
                .contains::<crate::app::modal::DiffIntroModal>()
        {
            self.modal_stack
                .push(Box::new(crate::app::modal::DiffIntroModal::new()));
        }
        if uneven_table_fallback {
            self.flash(
                "Table has uneven row widths — not split into per-row hunks",
                MessageKind::Info,
            );
        }
        self.needs_draw = true;
    }

    /// Install a **read-only** review of `old` vs `new` and enter diff mode — the `--diff`
    /// difftool presentation.
    ///
    /// Separate from [`Self::enter_diff_mode`] rather than a flag on it: there is no vim command
    /// line to tear down, no search flow to end, and no intro modal (it teaches an accept/reject
    /// vocabulary this review lacks).  What it shares is `DiffState::new` refusing an empty hunk
    /// list and `EditorState::enter_diff_mode` as the single door into `Mode::Diff`.
    ///
    /// The buffer is seeded with the *old* side so the review reads like every other one (buffer =
    /// before, `new_buffer` = after).  `dirty` stays false and `file_path` stays `None`, so
    /// nothing has a path to save over.
    ///
    /// `false` when the two files are byte-identical: git invokes a difftool only for paths it
    /// believes differ, but a whitespace- or mode-only change can still reach us.
    pub fn enter_read_only_diff(&mut self, old: String, new: String) -> bool {
        let Some(mut diff_state) = crate::diff::DiffState::new(&old, &new) else {
            return false;
        };
        diff_state.read_only = true;
        let uneven_table_fallback = diff_state.uneven_table_fallback;
        self.editor
            .replace_buffer(crate::document::Buffer::from_str(&old));
        self.editor.enter_diff_mode(diff_state);
        // `App::new` built the media prompts from the empty startup buffer, so this is the call
        // that asks them against the document actually under review — otherwise nothing ever
        // sets `session_*_enabled` and every diagram / remote image stays a placeholder for the
        // session.  After `enter_diff_mode`, since the prompts read the rebuilt `editor.parsed`.
        self.on_document_contents_swapped();
        if uneven_table_fallback {
            self.flash(
                "Table has uneven row widths — not split into per-row hunks",
                MessageKind::Info,
            );
        }
        self.needs_draw = true;
        true
    }

    /// Set the status-bar label for a difftool session (see [`App::diff_label`]).
    pub fn set_diff_label(&mut self, label: Option<String>) {
        self.diff_label = label;
    }

    /// Apply the merged result and exit diff mode, from the `[Apply]` button of
    /// [`crate::app::modal::DiffResolveConfirmModal`].  Records one coarse history entry, so a
    /// single `Ctrl-Z` reverts the whole merge.
    pub(crate) fn apply_diff_resolution(&mut self) {
        self.cancel_diff_advance();
        let Some(diff) = self.editor.diff.as_ref() else {
            return;
        };
        let Some(resolved) = diff.resolved_rope() else {
            self.flash("Diff is not fully resolved", MessageKind::Info);
            return;
        };
        // Only dirty when the merge actually diverges from disk.
        let new_text = diff.new_buffer.contents();
        let resolved_text = resolved.to_string();
        let differs_from_disk = resolved_text != new_text;
        // The pre-merge buffer is the diff's `old_rope` — entering diff mode never mutates
        // `editor.buffer` — so one synthetic delta covers both directions.
        let merge_delta = crate::document::EditDelta {
            offset: 0,
            removed: diff.old_rope.to_string(),
            inserted: resolved_text,
        };
        self.editor.buffer.set_rope(resolved);
        self.editor.cursor.offset = 0;
        self.editor.cursor.preferred_col = 0;
        self.editor.history.reset_with(merge_delta);
        self.editor.dirty = differs_from_disk;
        self.editor.refresh_parsed();
        self.editor.update_cursor_block();
        self.editor.exit_diff_mode();
        // A wholesale content replacement owes the same bookkeeping a file load does, media
        // prompts included.  After `refresh_parsed`, which the prompts are built from.
        self.on_document_contents_swapped();
        self.flash("Diff resolved", MessageKind::Success);
        self.needs_draw = true;
    }

    /// Exit diff mode without applying the merge.  Just clean-up: the diff's `old_rope` already
    /// equals the editor's buffer.
    pub(crate) fn exit_diff_mode_discarding(&mut self) {
        self.cancel_diff_advance();
        self.editor.exit_diff_mode();
        // The diff these refer to is gone, so a buried one (e.g. behind the file-deleted modal)
        // would later fire against no diff.  `remove_first` no-ops when absent.
        self.modal_stack
            .remove_first::<crate::app::modal::DiffResolveConfirmModal>();
        self.modal_stack
            .remove_first::<crate::app::modal::DiffIntroModal>();
        self.modal_stack
            .remove_first::<crate::app::modal::DiffBulkConfirmModal>();
        self.modal_stack
            .remove_first::<crate::app::modal::DiffQuitConfirmModal>();
        self.needs_draw = true;
    }

    /// The single call site for [`crate::document::Buffer::save_file`].  Every save path funnels
    /// through here — keystroke / palette `Save`, autosave, the dirty-link and dirty-quit guards,
    /// the external-editor flow — so follow-up state (the dirty flag, the watcher's own-write
    /// hash) has one home.
    ///
    /// Returns the raw `Result`; each caller shapes its own success / failure UX.
    pub(super) fn save_buffer(&mut self) -> anyhow::Result<()> {
        self.editor.buffer.save_file()?;
        self.editor.dirty = false;
        // Stamp the written contents so the watcher's own-write filter drops our save's own
        // echo.  Hashed from the in-memory rope's `\n`-only form rather than re-reading disk:
        // `handle_file_changed` normalizes CRLF back to `\n` before hashing, so both sides meet
        // in `\n` space and the memory read is far cheaper.
        let bytes = self.editor.buffer.contents();
        self.set_disk_hash(bytes.as_bytes());
        Ok(())
    }

    /// Save the buffer to a new path and adopt it: the buffer, the App's `file_path`, and the
    /// filesystem watcher are all re-pointed there.  Backs every "Save As" path.
    ///
    /// Mirrors [`Self::save_buffer`]'s post-write bookkeeping, plus a best-effort watcher
    /// re-point.
    pub(super) fn save_buffer_as(&mut self, path: &std::path::Path) -> anyhow::Result<()> {
        self.editor.buffer.save_as(path)?;
        self.editor.dirty = false;
        self.file_path = Some(path.to_owned());
        let bytes = self.editor.buffer.contents();
        self.set_disk_hash(bytes.as_bytes());
        if let Some(w) = self.watcher.as_mut() {
            if let Err(e) = w.watch(path) {
                tracing::warn!(
                    target: "watcher",
                    path = %path.display(),
                    error = %e,
                    "watch swap failed after save-as",
                );
            }
        }
        Ok(())
    }

    /// Stamp the watcher's own-write filter from raw bytes, for callers without an
    /// `incoming_hash` already in hand.  The accepted-`FileChanged` arm writes `last_disk_hash`
    /// directly instead, to avoid hashing the same bytes twice.
    pub(crate) fn set_disk_hash(&mut self, bytes: &[u8]) {
        self.last_disk_hash = Some(seahash::hash(bytes));
    }

    /// Open the settings overlay.
    pub fn open_settings_overlay(&mut self) {
        self.modal_stack
            .push(Box::new(modal::SettingsOverlayModal::new()));
    }

    /// Open the welcome modal on demand, ignoring `config.editor.show_welcome` and rebuilding
    /// from the live `capabilities` — so it doubles as the "my terminal changed" entry point.
    /// Guarded against stacking two copies.
    pub fn open_welcome_modal(&mut self) {
        if self.modal_stack.contains::<modal::WelcomeModal>() {
            return;
        }
        self.modal_stack.push(Box::new(modal::WelcomeModal::new(
            &self.capabilities,
            &self.config,
        )));
    }

    /// Open the fuzzy-searchable theme picker; selecting a row writes `config.theme`, saves, and
    /// reapplies the palette live.
    pub fn open_theme_picker(&mut self) {
        let current = self.config.theme.clone();
        // Open in the mode that actually contains the current theme (they can disagree after a
        // hand-edited config.toml), or it is filtered out and "(current)" never renders.
        let mode =
            crate::config::theme::theme_appearance(&current).unwrap_or(self.config.appearance);
        let themes = crate::config::theme::list_theme_names_for_mode(mode);
        self.modal_stack.push(Box::new(modal::ThemePickerModal::new(
            themes, current, mode,
        )));
    }

    /// Open the keybinds overlay, building a live `KeyMap` if there isn't one yet.
    pub fn open_keybinds_overlay(&mut self) {
        let keymap = self.ensure_keymap_clone();
        let overrides = self.keybindings.clone();
        let vim_enabled = self.vim.is_some();
        self.modal_stack
            .push(Box::new(modal::KeybindsOverlayModal::new(
                &keymap,
                &overrides,
                vim_enabled,
            )));
    }

    /// Open the rows/columns prompt.  The caller must have verified the blank-line precondition
    /// via [`crate::editor::table_edit::cursor_line_is_blank`].
    pub fn open_insert_table_modal(&mut self) {
        self.modal_stack
            .push(Box::new(modal::InsertTableModal::new()));
    }

    /// Open the "Save As" path-entry modal, seeded from the buffer's current path.  On submit
    /// the buffer is written and re-pointed via [`Self::save_buffer_as`]; `after_save` then runs,
    /// which is how the save-then-quit / save-then-navigate flows finish a deferred action.
    pub fn open_save_as_modal(&mut self, after_save: Option<modal::save_as::AfterSave>) {
        let m = modal::SaveAsModal::for_buffer_path(self.editor.buffer.path(), after_save);
        self.modal_stack.push(Box::new(m));
    }

    /// Write the buffer to a named `path` and adopt it, confirming first when the write would
    /// clobber a *different* existing file (see [`crate::document::Buffer::would_overwrite`]).
    /// `force` skips that prompt (vim `:w!` / `:saveas!`); `after` runs once the write succeeds.
    ///
    /// For the vim direct-save path, where the destination is named on the command line.  The
    /// Save As modal owns the path field, so it does its own check and pushes the same modal.
    pub(super) fn save_buffer_as_confirmed(
        &mut self,
        path: std::path::PathBuf,
        force: bool,
        after: Option<modal::save_as::AfterSave>,
    ) {
        if !force && self.editor.buffer.would_overwrite(&path) {
            self.modal_stack
                .push(Box::new(modal::OverwriteConfirmModal::new(path, after)));
            return;
        }
        match self.save_buffer_as(&path) {
            Ok(()) => {
                self.flash(format!("Saved to {}", path.display()), MessageKind::Success);
                if let Some(after) = after {
                    after(self);
                }
            }
            Err(e) => self.notify(format!("Save failed: {e}"), ModalKind::Error),
        }
    }

    /// Write a snapshot to `path` *without* re-pointing the buffer (vim `:w <path>`).  Confirms
    /// before clobbering a different existing file, as [`Self::save_buffer_as_confirmed`] does.
    pub(super) fn save_copy_confirmed(
        &mut self,
        path: std::path::PathBuf,
        force: bool,
        after: Option<modal::save_as::AfterSave>,
    ) {
        if !force && self.editor.buffer.would_overwrite(&path) {
            self.modal_stack
                .push(Box::new(modal::OverwriteConfirmModal::for_copy(
                    path, after,
                )));
            return;
        }
        match self.editor.buffer.save_copy(&path) {
            Ok(()) => {
                self.flash(
                    format!("Copy saved to {}", path.display()),
                    MessageKind::Success,
                );
                if let Some(after) = after {
                    after(self);
                }
            }
            Err(e) => self.notify(format!("Save failed: {e}"), ModalKind::Error),
        }
    }

    /// Drain `EditorState::pending_column_widths_commit`, set by a column-border drag's Release.
    /// Commits immediately when the table already carries a `<!-- tui-columns: … -->` comment or
    /// `config.table.warn_on_width_injection` is off; otherwise opens the warning modal carrying
    /// the table's `table_byte_start` so its handler can commit or cancel.
    pub(super) fn handle_pending_column_widths(&mut self) {
        let Some(table_byte_start) = self.editor.pending_column_widths_commit else {
            return;
        };
        let already_has_comment = self.editor.table_has_tui_columns_comment(table_byte_start);
        if already_has_comment || !self.config.table.warn_on_width_injection {
            self.editor.commit_pending_column_widths();
            return;
        }
        self.modal_stack
            .push(Box::new(modal::WidthInjectionWarning::new()));
    }

    /// Reload the theme named by `self.config.theme`, leak it into `'static`, and swap it onto
    /// `self.theme` and the editor.  Loader warnings surface via `ConfigWarningModal`, which
    /// renders above the settings overlay.
    ///
    /// # Leak by design
    ///
    /// `Theme` is held everywhere as `&'static Theme` (see the constructor), obtained by
    /// `Box::leak`.  Each theme change leaks one fresh allocation — a few KB, user-initiated — so
    /// even aggressive cycling accumulates at most a few MB per session.
    pub(super) fn apply_active_theme(&mut self) {
        let truecolor = self.capabilities.color_depth == ColorDepth::TrueColor;
        let (theme_file, warnings) = Config::load_theme(&self.config.theme, truecolor);
        let monochrome = self.capabilities.color_depth == ColorDepth::NoColor;
        let new_theme: &'static Theme =
            Box::leak(Box::new(Theme::from_file(&theme_file, monochrome)));
        self.theme = new_theme;
        self.editor.set_theme(new_theme);
        self.needs_draw = true;
        if let Some(modal) = modal::ConfigWarningModal::from_warnings(&warnings) {
            self.modal_stack.push(Box::new(modal));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{action_caps, readonly_safe_action, search_safe_action};

    // ── The diff-mode gate sits ahead of `handle_app_action` ───────

    /// `handle_app_action` answers `true` for everything it handles, so a gate behind it never
    /// sees `InsertTable` — off the diff allowlist, handled there, and reachable mid-review from
    /// the palette, which is on the allowlist.
    #[test]
    fn an_app_level_action_off_the_allowlist_is_refused_in_diff_review() {
        let mut app = app_with_buffer("alpha\n", 0);
        app.enter_diff_mode("bravo\n".to_owned());
        assert_eq!(app.editor.mode, crate::editor::Mode::Diff);
        assert!(
            diff_safe_action(&Action::InsertTable).is_none(),
            "precondition: InsertTable is not diff-safe"
        );

        let before = app.modal_stack.len();
        app.dispatch_action(Action::InsertTable, 20, 80);
        assert_eq!(
            app.modal_stack.len(),
            before,
            "the insert-table modal must not open over a diff review"
        );
    }

    /// The same hole let a link follow replace the document mid-review.
    #[test]
    fn following_a_link_is_refused_in_diff_review() {
        let mut app = app_with_buffer("[x](other.md)\n", 0);
        app.enter_diff_mode("bravo\n".to_owned());
        assert!(diff_safe_action(&Action::FollowLinkUnderCursor).is_none());

        app.dispatch_action(Action::FollowLinkUnderCursor, 20, 80);
        assert_eq!(
            app.editor.mode,
            crate::editor::Mode::Diff,
            "a link follow must not navigate out of a review"
        );
    }

    /// Only the *refusal* moved: an allowed app-level action still reaches `handle_app_action`,
    /// which is what opens the palette and the overlays during a review.
    #[test]
    fn an_allowed_app_level_action_still_runs_in_diff_review() {
        let mut app = app_with_buffer("alpha\n", 0);
        app.enter_diff_mode("bravo\n".to_owned());
        assert!(diff_safe_action(&Action::ShowCommandPalette).is_some());

        let before = app.modal_stack.len();
        app.dispatch_action(Action::ShowCommandPalette, 20, 80);
        assert_eq!(
            app.modal_stack.len(),
            before + 1,
            "the palette still opens during a review"
        );
    }
    use crossterm::event::{
        KeyModifiers as CtKeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    use super::{diff_safe_action, edit_ops, linewise_paste_payload, modal_wheel_delta};
    use crate::app::test_utils::{app_with_buffer, make_app};
    use crate::config::Action;
    use crate::document::Buffer;

    #[test]
    fn enter_diff_mode_with_show_intro_pushes_intro_modal() {
        use crate::app::modal::DiffIntroModal;
        let mut app = make_app();
        app.editor.buffer.insert(0, "alpha\nbeta\n");
        assert!(app.config.editor.show_diff_intro);
        app.enter_diff_mode("alpha\nGAMMA\n".to_owned());
        assert_eq!(app.editor.mode, crate::editor::Mode::Diff);
        assert!(app.editor.diff.is_some());
        assert!(
            app.modal_stack.contains::<DiffIntroModal>(),
            "first-time entry must push the intro modal",
        );
    }

    #[test]
    fn reentering_diff_mode_does_not_stack_a_second_intro_modal() {
        // A clean buffer stays clean during review, so a second external overwrite re-enters
        // `enter_diff_mode` and must not stack a second intro modal.
        use crate::app::modal::DiffIntroModal;
        let mut app = make_app();
        app.editor.buffer.insert(0, "alpha\nbeta\n");
        app.enter_diff_mode("alpha\nGAMMA\n".to_owned());
        app.enter_diff_mode("alpha\nDELTA\n".to_owned());
        assert_eq!(
            app.modal_stack.count::<DiffIntroModal>(),
            1,
            "re-entry must not stack a second intro modal",
        );
    }

    #[test]
    fn visual_line_copy_flashes_copied() {
        use crate::config::Action;
        use crate::document::Selection;
        use crate::input::vim::state::{VimState, VimSubMode};
        let mut app = make_app();
        app.editor.buffer.insert(0, "alpha\nbeta\n");
        app.editor.mode = crate::editor::Mode::Rendered;
        app.editor.refresh_parsed();
        // V-LINE on the first line: the charwise span is empty, but the whole line is selected.
        app.editor.selection = Some(Selection {
            anchor: 0,
            active: 0,
        });
        app.vim = Some(VimState {
            sub_mode: VimSubMode::VisualLine,
            visual_anchor: Some(0),
            ..Default::default()
        });
        app.dispatch_action(Action::Copy, 24, 80);
        let text = app.transient.as_ref().map(|t| t.text.clone());
        assert_eq!(
            text.as_deref(),
            Some("Copied"),
            "V-LINE copy must flash Copied like the charwise path"
        );
    }

    /// The linewise-paste payload rules, exercised directly: the OS clipboard is global and
    /// would race parallel tests.
    #[test]
    fn linewise_paste_payload_keeps_the_line_structure() {
        let buf = Buffer::from_str("alpha\nbeta\ngamma\n");
        let range = 0..11;
        assert_eq!(
            linewise_paste_payload(&buf, &range, "foo".to_owned()),
            Some("foo\n".to_owned()),
            "charwise text gets a newline so it can't weld onto the next line"
        );
        assert_eq!(
            linewise_paste_payload(&buf, &range, "x\ny\n".to_owned()),
            Some("x\ny\n".to_owned()),
            "already-linewise text is used as-is — no blank line inserted"
        );
        assert_eq!(
            linewise_paste_payload(&buf, &range, String::new()),
            None,
            "nothing to paste must not replace the lines with a bare newline"
        );
    }

    #[test]
    fn linewise_paste_payload_leaves_a_final_line_without_a_newline() {
        // The replaced span has no trailing newline either, so nothing is appended.
        let buf = Buffer::from_str("alpha\nbeta");
        let range = 6..10;
        assert_eq!(
            linewise_paste_payload(&buf, &range, "foo".to_owned()),
            Some("foo".to_owned()),
        );
    }

    /// `dispatch_visual_line_clipboard`'s widening and payload composition against a *charwise*
    /// payload — the case the end-to-end test in `app.rs` can't reach, since it must Copy first
    /// (yielding a linewise payload) to stay independent of the live OS clipboard.
    #[test]
    fn visual_line_paste_of_charwise_text_keeps_the_line_intact() {
        use crate::document::Selection;
        let mut app = make_app();
        app.editor.buffer.insert(0, "alpha\nbeta\ngamma\n");
        app.editor.mode = crate::editor::Mode::Rendered;
        app.editor.refresh_parsed();
        // V-LINE parked mid-line: the widened span is the whole line, newline included.
        let sel = Selection {
            anchor: 8,
            active: 8,
        };
        let range = crate::editor::vim_ops::visual_line_char_range(&sel, &app.editor.buffer);
        assert_eq!(range, 6..11, "V-LINE on line 1 widens to \"beta\\n\"");
        let payload = linewise_paste_payload(&app.editor.buffer, &range, "REPLACED".to_owned())
            .expect("non-empty clipboard");
        app.editor.selection = Some(Selection {
            anchor: range.start,
            active: range.end,
        });
        edit_ops::paste_text(&mut app.editor, &payload, 24, 80);
        assert_eq!(
            app.editor.buffer.contents(),
            "alpha\nREPLACED\ngamma\n",
            "the line is replaced whole and `gamma` keeps its own row"
        );
    }

    #[test]
    fn entering_diff_mode_clears_an_open_vim_command_line() {
        use crate::input::vim::state::{CmdLineKind, CmdLineState, VimState};
        use crate::ui::HintContent;
        let mut app = make_app();
        app.editor.buffer.insert(0, "alpha\nbeta\n");
        app.vim = Some(VimState {
            cmdline: Some(CmdLineState::new(CmdLineKind::Ex)),
            count: Some(3),
            ..Default::default()
        });
        app.enter_diff_mode("alpha\nGAMMA\n".to_owned());
        let vim = app.vim.as_ref().unwrap();
        assert!(vim.cmdline.is_none(), "command line must be cleared");
        assert_eq!(vim.count, None, "pending parse must be reset");
        assert!(matches!(app.hint_content(), HintContent::Chords(_)));
    }

    #[test]
    fn entering_diff_mode_ends_a_live_incsearch_session() {
        use crate::editor::vim_ops::update_incsearch;
        use crate::input::vim::state::{CmdLineKind, CmdLineState, VimState};
        let mut app = make_app();
        app.editor.buffer.insert(0, "foo bar\nfoo\n");
        app.editor.refresh_parsed();
        let mut vim = VimState {
            cmdline: Some(CmdLineState::new(CmdLineKind::SearchForward)),
            ..Default::default()
        };
        update_incsearch(&mut app.editor, &mut vim.incsearch, "foo", true, 24, 80);
        assert!(vim.incsearch.is_some(), "live session while typing");
        assert_eq!(app.editor.cursor.offset, 8, "parked on the live focus");
        app.vim = Some(vim);
        app.enter_diff_mode("foo bar\nGAMMA\n".to_owned());
        let vim = app.vim.as_ref().unwrap();
        assert!(
            vim.incsearch.is_none(),
            "session must not dangle on VimState — the next `/` would reuse its stale saved view",
        );
        assert!(vim.cmdline.is_none());
        assert_eq!(app.editor.cursor.offset, 0, "pre-prompt cursor restored");
        assert!(app.editor.search.is_none(), "transient session torn down");
    }

    #[test]
    fn entering_diff_mode_reverts_a_live_substitute_preview() {
        use crate::editor::vim_ops::update_substitute_preview;
        use crate::input::vim::state::{CmdLineKind, CmdLineState, VimState};
        let mut app = make_app();
        app.editor.buffer.insert(0, "alpha\nbeta\n");
        app.editor.refresh_parsed();
        app.vim = Some(VimState {
            cmdline: Some(CmdLineState::new(CmdLineKind::Ex)),
            ..Default::default()
        });
        update_substitute_preview(&mut app.editor, "%s/alpha/OMEGA/", None, 24, 80);
        assert!(app.editor.substitute_preview.is_some(), "preview active");
        assert!(app.editor.buffer.contents().contains("OMEGA"));
        app.enter_diff_mode("alpha\nGAMMA\n".to_owned());
        assert!(
            app.editor.substitute_preview.is_none(),
            "preview must revert on diff entry — its gates would otherwise hold forever",
        );
        assert_eq!(
            app.editor.buffer.contents(),
            "alpha\nbeta\n",
            "diff is taken against the pristine buffer, not preview text",
        );
        assert!(app.editor.diff.is_some());
    }

    // ── Read-only (difftool) review ──────────────────────────────

    fn read_only_app(old: &str, new: &str) -> crate::app::App {
        let mut app = make_app();
        assert!(app.enter_read_only_diff(old.to_owned(), new.to_owned()));
        app
    }

    #[test]
    fn enter_read_only_diff_marks_the_review_and_seeds_the_old_side() {
        let app = read_only_app("alpha\nbeta\n", "alpha\nBETA\n");
        assert_eq!(app.editor.mode, crate::editor::Mode::Diff);
        let diff = app.editor.diff.as_ref().expect("review installed");
        assert!(diff.read_only);
        assert_eq!(app.editor.buffer.contents(), "alpha\nbeta\n");
        assert!(!app.editor.dirty);
    }

    /// The intro modal teaches accept/reject, which this review does not have.
    #[test]
    fn a_read_only_review_pushes_no_intro_modal() {
        use crate::app::modal::DiffIntroModal;
        let app = read_only_app("alpha\nbeta\n", "alpha\nBETA\n");
        assert!(app.config.editor.show_diff_intro, "intro is on by default");
        assert!(!app.modal_stack.contains::<DiffIntroModal>());
    }

    /// Identical sides yield no review at all, rather than an empty one.
    #[test]
    fn enter_read_only_diff_declines_identical_files() {
        let mut app = make_app();
        assert!(!app.enter_read_only_diff("same\n".to_owned(), "same\n".to_owned()));
        assert!(app.editor.diff.is_none());
        assert_ne!(app.editor.mode, crate::editor::Mode::Diff);
    }

    /// Every decision action is refused, including via the palette: the key table is not the only
    /// way to reach them.
    #[test]
    fn a_read_only_review_refuses_every_decision_action() {
        use crate::config::Action;
        for action in [
            Action::DiffAcceptHunk,
            Action::DiffRejectHunk,
            Action::DiffAcceptAll,
            Action::DiffRejectAll,
            Action::DiffResetHunk,
        ] {
            let mut app = read_only_app("alpha\nbeta\n", "alpha\nBETA\n");
            app.dispatch_action(action.clone(), 40, 80);
            let diff = app.editor.diff.as_ref().expect("still reviewing");
            assert!(
                diff.decisions
                    .iter()
                    .all(|d| *d == crate::diff::Decision::Pending),
                "{action:?} changed a decision in a read-only review"
            );
            assert!(!app.should_quit, "{action:?} must not end the session");
        }
    }

    /// Esc ends the process: there is no editor behind the review, and git difftool runs one file
    /// per invocation.
    #[test]
    fn esc_quits_a_read_only_review_without_resolving() {
        use crate::config::Action;
        let mut app = read_only_app("alpha\nbeta\n", "alpha\nBETA\n");
        app.dispatch_action(Action::DiffExit, 40, 80);
        assert!(app.should_quit);
        assert!(!app.diff_stop_walk(), "Esc moves on to the next file");
    }

    /// Quit skips the discard-confirm modal and flags the abort so `main` exits non-zero.
    #[test]
    fn quit_aborts_a_read_only_review_without_confirmation() {
        use crate::app::modal::DiffQuitConfirmModal;
        use crate::config::Action;
        let mut app = read_only_app("alpha\nbeta\n", "alpha\nBETA\n");
        app.dispatch_action(Action::Quit, 40, 80);
        assert!(app.should_quit);
        assert!(app.diff_stop_walk());
        assert!(!app.modal_stack.contains::<DiffQuitConfirmModal>());
    }

    /// The review *is* the document for a `--diff` session, so the media prompts must be asked
    /// against it: `App::new` built them from the empty startup buffer, and nothing else would
    /// ever set `session_diagrams_enabled`.
    #[test]
    fn a_read_only_review_asks_the_media_prompts_for_its_document() {
        use crate::app::modal::FiguresEnabledPromptModal;
        let old = "# Doc\n\n```mermaid\ngraph TD;\nA-->B;\n```\n\nbefore\n";
        let new = "# Doc\n\n```mermaid\ngraph TD;\nA-->B;\n```\n\nafter\n";
        let app = read_only_app(old, new);
        assert!(
            matches!(
                app.config.figures.enabled,
                crate::config::FiguresEnabled::Ask
            ),
            "ask is the default policy this test depends on",
        );
        assert!(
            app.modal_stack.contains::<FiguresEnabledPromptModal>(),
            "the diagrams prompt must be queued for the reviewed document",
        );
    }

    #[test]
    fn enter_diff_mode_with_intro_off_skips_intro_modal() {
        use crate::app::modal::DiffIntroModal;
        let mut app = make_app();
        app.editor.buffer.insert(0, "alpha\nbeta\n");
        app.config.editor.show_diff_intro = false;
        app.enter_diff_mode("alpha\nGAMMA\n".to_owned());
        assert_eq!(app.editor.mode, crate::editor::Mode::Diff);
        assert!(!app.modal_stack.contains::<DiffIntroModal>());
    }

    #[test]
    fn diff_accept_all_then_apply_swaps_resolved_rope() {
        use crate::app::modal::DiffBulkConfirmModal;
        let mut app = make_app();
        app.editor.buffer.insert(0, "alpha\nbeta\ngamma\n");
        app.enter_diff_mode("alpha\nBETA\ngamma\n".to_owned());
        app.modal_stack
            .remove_first::<crate::app::modal::DiffIntroModal>();
        // Accept-all opens the bulk-confirm modal rather than deciding immediately.
        app.dispatch_diff_action(crate::config::Action::DiffAcceptAll, 24, 80);
        assert!(app.modal_stack.contains::<DiffBulkConfirmModal>());
        app.apply_diff_bulk_decision(crate::diff::Decision::Accepted);
        app.apply_diff_resolution();
        assert_eq!(app.editor.mode, crate::editor::Mode::Rendered);
        assert!(app.editor.diff.is_none());
        assert_eq!(app.editor.buffer.contents(), "alpha\nBETA\ngamma\n");
    }

    #[test]
    fn apply_diff_resolution_records_single_merge_revert_entry() {
        use crate::config::Action;
        let mut app = app_in_diff("alpha\nbeta\ngamma\n", "alpha\nBETA\ngamma\n");
        resolve_all(&mut app, crate::diff::Decision::Accepted);
        app.apply_diff_resolution();
        assert!(app.editor.diff.is_none());
        assert_eq!(app.editor.buffer.contents(), "alpha\nBETA\ngamma\n");
        assert_eq!(app.editor.history.undo_depth(), 1);

        crate::editor::edit_ops::apply(&mut app.editor, Action::Undo, 24, 80);
        assert_eq!(app.editor.buffer.contents(), "alpha\nbeta\ngamma\n");
        crate::editor::edit_ops::apply(&mut app.editor, Action::Redo, 24, 80);
        assert_eq!(app.editor.buffer.contents(), "alpha\nBETA\ngamma\n");
    }

    /// Enter diff mode against `disk`, dropping the intro modal so it doesn't interfere with
    /// stack assertions.
    fn app_in_diff(buffer: &str, disk: &str) -> crate::app::App {
        let mut app = make_app();
        app.editor.buffer.insert(0, buffer);
        app.enter_diff_mode(disk.to_owned());
        app.modal_stack
            .remove_first::<crate::app::modal::DiffIntroModal>();
        app
    }

    /// Resolve every hunk directly, so a subsequent action is the only thing under test.
    fn resolve_all(app: &mut crate::app::App, decision: crate::diff::Decision) {
        for d in app.editor.diff.as_mut().unwrap().decisions.iter_mut() {
            *d = decision;
        }
    }

    #[test]
    fn resolving_a_diff_that_adds_an_image_queues_the_images_prompt() {
        // A clean buffer's external change goes to diff review by default, so this — not
        // `reload_buffer_from_disk` — is the usual way a document gains its first image
        // mid-session.  Without the prompt the merged image never decodes (issue #30).
        use crate::app::modal::ImagesEnabledPromptModal;
        let mut app = app_in_diff("Just prose.\n", "Just prose.\n\n![a](img.png)\n");
        assert!(!app.modal_stack.contains::<ImagesEnabledPromptModal>());
        resolve_all(&mut app, crate::diff::Decision::Accepted);
        app.apply_diff_resolution();
        assert!(app.editor.buffer.contents().contains("![a](img.png)"));
        assert!(
            app.modal_stack.contains::<ImagesEnabledPromptModal>(),
            "the merged document's images must raise the prompt that enables them",
        );
        assert!(app.images_dirty, "the image cache needs reconciling");
    }

    #[test]
    fn resolving_a_diff_does_not_re_ask_an_answered_question() {
        use crate::app::modal::ImagesEnabledPromptModal;
        let mut app = app_in_diff("![a](img.png)\n", "![a](img.png)\n\n![b](other.png)\n");
        app.modal_stack.remove_first::<ImagesEnabledPromptModal>();
        app.session_images_enabled = Some(false);
        resolve_all(&mut app, crate::diff::Decision::Accepted);
        app.apply_diff_resolution();
        assert!(!app.modal_stack.contains::<ImagesEnabledPromptModal>());
    }

    #[test]
    fn diff_denied_action_flashes_not_available() {
        use crate::config::Action;
        let mut app = app_in_diff("old text\n", "new text\n");
        app.dispatch_action(Action::InsertChar('x'), 24, 80);
        let text = app.transient.as_ref().map(|t| t.text.clone());
        assert_eq!(text.as_deref(), Some("Not available during diff review"));
        assert_eq!(app.editor.mode, crate::editor::Mode::Diff);
    }

    #[test]
    fn diff_navigation_never_opens_confirm_modal_when_resolved() {
        use crate::app::modal::DiffResolveConfirmModal;
        use crate::config::Action;
        use crate::diff::Decision;
        let mut app = app_in_diff("a\nb\nc\nd\ne\n", "A\nb\nC\nd\nE\n");
        resolve_all(&mut app, Decision::Accepted);
        // Navigation is not a resolve trigger, even among already-resolved hunks.
        app.dispatch_diff_action(Action::DiffNext, 24, 80);
        assert!(!app.modal_stack.contains::<DiffResolveConfirmModal>());
        app.dispatch_diff_action(Action::DiffPrev, 24, 80);
        assert!(!app.modal_stack.contains::<DiffResolveConfirmModal>());
        assert!(
            app.editor.diff.is_some(),
            "navigation must not exit diff mode"
        );
    }

    #[test]
    fn diff_reject_all_gates_behind_bulk_confirm_then_overrides() {
        use crate::app::modal::{DiffBulkConfirmModal, DiffResolveConfirmModal};
        use crate::config::Action;
        use crate::diff::Decision;
        let mut app = app_in_diff("a\nb\nc\n", "A\nb\nC\n");
        resolve_all(&mut app, Decision::Accepted);
        // The bulk-confirm modal must not change any decision before the user confirms.
        app.dispatch_diff_action(Action::DiffRejectAll, 24, 80);
        assert!(app.modal_stack.contains::<DiffBulkConfirmModal>());
        assert!(
            app.editor
                .diff
                .as_ref()
                .unwrap()
                .decisions
                .iter()
                .all(|d| *d == Decision::Accepted),
            "bulk-confirm must not flip decisions before the user confirms",
        );
        app.dispatch_diff_action(Action::DiffRejectAll, 24, 80);
        assert_eq!(app.modal_stack.count::<DiffBulkConfirmModal>(), 1);

        app.apply_diff_bulk_decision(Decision::Rejected);
        assert!(
            app.editor
                .diff
                .as_ref()
                .unwrap()
                .decisions
                .iter()
                .all(|d| *d == Decision::Rejected),
            "reject-all must override prior accepted decisions on confirm",
        );
        assert!(app.modal_stack.contains::<DiffResolveConfirmModal>());
    }

    #[test]
    fn diff_bulk_confirm_dismissed_leaves_decisions_intact() {
        use crate::app::modal::DiffBulkConfirmModal;
        use crate::config::Action;
        use crate::diff::Decision;
        let mut app = app_in_diff("a\nb\nc\n", "A\nb\nC\n");
        {
            let d = app.editor.diff.as_mut().unwrap();
            d.decisions[0] = Decision::Accepted;
            d.decisions[1] = Decision::Rejected;
        }
        let before = app.editor.diff.as_ref().unwrap().decisions.clone();
        // Dismissing the gate without the [Yes] callback must leave every decision untouched.
        app.dispatch_diff_action(Action::DiffAcceptAll, 24, 80);
        assert!(app.modal_stack.contains::<DiffBulkConfirmModal>());
        app.modal_stack.remove_first::<DiffBulkConfirmModal>();
        assert_eq!(
            app.editor.diff.as_ref().unwrap().decisions,
            before,
            "dismissing the bulk-confirm must not change any decision",
        );
    }

    #[test]
    fn diff_quit_warns_instead_of_discarding_immediately() {
        use crate::app::modal::DiffQuitConfirmModal;
        use crate::config::Action;
        let mut app = app_in_diff("a\nb\nc\n", "A\nb\nC\n");
        app.dispatch_action(Action::Quit, 24, 80);
        assert!(
            app.modal_stack.contains::<DiffQuitConfirmModal>(),
            "Quit in diff mode must open the discard-confirm modal",
        );
        assert!(!app.should_quit, "Quit must not fire before confirmation");
        assert!(app.editor.diff.is_some(), "the review must stay active");

        app.dispatch_action(Action::Quit, 24, 80);
        assert_eq!(app.modal_stack.count::<DiffQuitConfirmModal>(), 1);
    }

    #[test]
    fn diff_esc_is_gated_on_full_resolution() {
        use crate::app::modal::DiffResolveConfirmModal;
        use crate::config::Action;
        use crate::diff::Decision;

        // With hunks pending, Esc must neither exit nor open the confirm modal.
        let mut app = app_in_diff("a\nb\nc\n", "A\nb\nC\n");
        app.dispatch_diff_action(Action::DiffExit, 24, 80);
        assert!(
            app.editor.diff.is_some(),
            "Esc with pending hunks must not exit diff mode",
        );
        assert!(!app.modal_stack.contains::<DiffResolveConfirmModal>());

        resolve_all(&mut app, Decision::Accepted);
        app.dispatch_diff_action(Action::DiffExit, 24, 80);
        assert!(
            app.editor.diff.is_some(),
            "Esc with all hunks resolved must stay in diff mode until applied",
        );
        assert!(app.modal_stack.contains::<DiffResolveConfirmModal>());
    }

    #[test]
    fn save_buffer_clears_dirty_on_success() {
        let mut app = make_app();
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        app.editor.buffer = Buffer::for_new_file(tmp.path());
        let len = app.editor.buffer.len_chars();
        app.editor.buffer.insert_char(len, 'z');
        app.editor.dirty = true;

        app.save_buffer().expect("save");

        assert!(!app.editor.dirty);
        let on_disk = std::fs::read_to_string(tmp.path()).expect("read back");
        assert!(on_disk.ends_with('z'));
    }

    #[test]
    fn save_buffer_returns_err_when_buffer_has_no_path() {
        let mut app = make_app();
        assert!(app.editor.buffer.path().is_none());
        app.editor.dirty = true;
        let result = app.save_buffer();
        assert!(result.is_err(), "unnamed buffer must fail to save");
        assert!(app.editor.dirty, "failed save must leave dirty set");
    }

    #[test]
    fn modal_wheel_delta_translates_scroll_direction() {
        let scroll_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: CtKeyModifiers::NONE,
        };
        let scroll_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            ..scroll_up
        };
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            ..scroll_up
        };
        assert_eq!(modal_wheel_delta(&scroll_up, 1), -1);
        assert_eq!(modal_wheel_delta(&scroll_down, 1), 1);
        assert_eq!(modal_wheel_delta(&scroll_down, 4), 4);
        // The wheel-step floor is 1, even when config asks for 0.
        assert_eq!(modal_wheel_delta(&scroll_up, 0), -1);
        assert_eq!(modal_wheel_delta(&click, 1), 0);
    }

    /// Every `Action` variant, for the gate sweeps below.  `EVERY_UNIT_ACTION` derives from the
    /// `action_variants!` list, so it cannot fall behind the enum; the two payload-bearing
    /// variants are appended by hand.
    fn every_action() -> Vec<Action> {
        let mut all = crate::config::keymap::EVERY_UNIT_ACTION.to_vec();
        all.push(Action::InsertChar('a'));
        all.push(Action::OpenDoc(crate::docs::DocId::Index));
        all
    }

    /// The always-safe set, pinned.
    #[test]
    fn always_safe_is_exactly_the_scroll_openers_and_quit() {
        use Action::*;
        let expected = [
            ScrollUp,
            ScrollDown,
            ScrollPageUp,
            ScrollPageDown,
            ScrollToTop,
            ScrollToBottom,
            Quit,
            ShowCommandPalette,
            ShowMarkdownCheatSheet,
            ShowAbout,
            CheckForUpdates,
            OpenSettings,
            OpenWelcome,
            OpenKeybinds,
            SwitchTheme,
            CreateCustomTheme,
            OpenConfigFolder,
        ];
        for action in &expected {
            assert!(action_caps(action).always_safe, "{action} should be safe");
            assert!(diff_safe_action(action).is_some(), "{action} in diff");
            assert!(search_safe_action(action).is_some(), "{action} in search");
            assert!(readonly_safe_action(action), "{action} while read-only");
        }
    }

    /// The three policy-gating flags must not overlap in ways that make a rule ambiguous.
    #[test]
    fn read_only_navigation_never_mutates_or_needs_a_path() {
        for action in every_action() {
            let caps = action_caps(&action);
            if caps.read_only_nav {
                assert!(!caps.mutates_buffer, "{action} both reads and writes");
                assert!(!caps.needs_path, "{action} both reads and needs a path");
            }
            if caps.always_safe {
                assert!(!caps.mutates_buffer, "{action} is safe yet writes");
                assert!(!caps.needs_path, "{action} is safe yet needs a path");
            }
        }
    }

    /// Diff review is the narrowest gate: its own vocabulary plus the always-safe set, and
    /// nothing else — not even cursor motion.
    #[test]
    fn diff_allows_only_its_own_commands_and_the_always_safe_set() {
        for action in every_action() {
            let allowed = diff_safe_action(&action).is_some();
            let expected = action_caps(&action).always_safe
                || matches!(
                    action,
                    Action::DiffNext
                        | Action::DiffPrev
                        | Action::DiffAcceptHunk
                        | Action::DiffRejectHunk
                        | Action::DiffAcceptAll
                        | Action::DiffRejectAll
                        | Action::DiffResetHunk
                        | Action::DiffExit
                );
            assert_eq!(allowed, expected, "{action}");
        }
    }

    /// Nothing that writes the buffer may run in a capturing replace flow, except the flow's own
    /// commands and the undo/redo that takes one back.
    #[test]
    fn a_capturing_search_flow_admits_no_other_buffer_mutation() {
        for action in every_action() {
            if !action_caps(&action).mutates_buffer {
                continue;
            }
            let allowed = search_safe_action(&action).is_some();
            let expected = matches!(
                action,
                Action::SearchReplace | Action::SearchReplaceAll | Action::Undo | Action::Redo
            );
            assert_eq!(allowed, expected, "{action}");
        }
    }

    /// The read-only rule as a property, rather than the five names it resolves to today.
    #[test]
    fn a_read_only_document_denies_every_write_and_allows_every_navigation() {
        for action in every_action() {
            let caps = action_caps(&action);
            let allowed = readonly_safe_action(&action);
            if caps.mutates_buffer || caps.needs_path {
                assert!(!allowed, "{action} must be denied while read-only");
            } else {
                assert!(allowed, "{action} must be allowed while read-only");
            }
            // Cross-linking is the whole point of a manual, so `navigates_away` never denies.
            if caps.navigates_away {
                assert!(allowed, "{action} navigates and must stay available");
            }
        }
    }

    // ── Clipboard paste, end to end through a substituted clipboard ─────
    //
    // The environment each test simulates is stated in its own comment and
    // mirrors a measured clipboard shape (Windows 11 26200):
    //
    //   Explorer `Ctrl+C` on a file → `CF_HDROP` + shell-private, no text
    //   Snipping Tool              → `CF_DIBV5` + `CF_DIB` + `CF_BITMAP`
    //   Text editor `Ctrl+C`       → `CF_UNICODETEXT` + `CF_TEXT`
    //
    // These stay in the module that owns the dispatch rather than moving
    // to `tests/`: an integration test cannot reach `App`'s crate-private
    // fields (needed to arrange the save directory and read the buffer),
    // has no `test_env::config_isolation`, and building an `App` without
    // it can rewrite the developer's own `config.toml`.

    use crate::clipboard::{Bitmap, ClipboardData, ClipboardSource};

    /// A clipboard with scripted contents: the substitution seam the
    /// production build fills with the OS adapter.  Every read serves the
    /// same payload, so a test states exactly what the OS would have
    /// handed over.
    struct StubClipboard(ClipboardData);

    impl ClipboardSource for StubClipboard {
        fn read(&mut self) -> ClipboardData {
            self.0.clone()
        }
    }

    /// Hand the app a clipboard it wrote down, instead of the OS one —
    /// the substitution the production build fills with `OsClipboard`.
    fn with_clipboard(app: &mut crate::app::App, source: Box<dyn ClipboardSource>) {
        app.clipboard = source;
    }

    /// Point the paste's screenshot directory at a scratch directory.
    ///
    /// Every paste test takes one, including those whose payload can only
    /// be referenced: `destination` is a function that *writes*, so a
    /// test that left `save_dir` alone would be one policy change away
    /// from dropping a PNG into the developer's real images directory.
    /// Dropping the guard deletes the directory.
    fn scratch_save_dir(app: &mut crate::app::App) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        app.config.images.save_dir = dir.path().to_string_lossy().into_owned();
        dir
    }

    /// Explorer's `Ctrl+C` on files: a file list, and nothing else.
    fn file_copy(paths: &[&str]) -> ClipboardData {
        ClipboardData {
            files: paths.iter().map(std::path::PathBuf::from).collect(),
            ..ClipboardData::default()
        }
    }

    /// A screenshot: pixels, and nothing else.
    fn screenshot() -> ClipboardData {
        ClipboardData {
            bitmap: Some(Bitmap {
                width: 2,
                height: 1,
                rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
            }),
            ..ClipboardData::default()
        }
    }

    /// Text on the clipboard, as a text editor's `Ctrl+C` leaves it.
    fn text_copy(text: &str) -> ClipboardData {
        ClipboardData {
            text: Some(text.to_owned()),
            ..ClipboardData::default()
        }
    }

    /// Paste at a 40x80 document area and forget whether it was handled —
    /// every assertion here is about what the user ends up seeing.
    fn paste(app: &mut crate::app::App, action: Action) {
        app.dispatch_action(action, 40, 80);
    }

    /// Whether the parse promoted the reference to an image block — the
    /// observable that says the pasted image will actually render rather
    /// than paint as a line of text.
    fn promoted(app: &crate::app::App, url: &str) -> bool {
        app.editor.parsed.image_blocks.iter().any(|i| i.url == url)
    }

    #[test]
    fn ctrl_v_on_a_file_copied_in_the_file_manager_inserts_a_reference_to_it() {
        // Explorer's copy: a file list, no text format and no bitmap — so
        // the list is the only payload that can answer, and the file is
        // referenced where it lies rather than copied.
        let mut app = app_with_buffer("prose\n\n", 7);
        let dir = scratch_save_dir(&mut app);
        with_clipboard(
            &mut app,
            Box::new(StubClipboard(file_copy(&[r"C:\Users\me\shot.png"]))),
        );

        paste(&mut app, Action::Paste);

        assert_eq!(
            app.editor.contents(),
            "prose\n\n![](C:/Users/me/shot.png)\n",
            "the copied file is referenced where it lies"
        );
        assert!(
            promoted(&app, "C:/Users/me/shot.png"),
            "the reference must parse as an image block, not paint as text"
        );
        assert!(
            std::fs::read_dir(dir.path())
                .expect("tempdir")
                .next()
                .is_none(),
            "referencing a copied file must not write a copy of it"
        );
    }

    #[test]
    fn the_paste_image_command_accepts_a_copied_file_too() {
        // The palette command and the chord are the same code path.
        let mut app = app_with_buffer("prose\n\n", 7);
        let _scratch = scratch_save_dir(&mut app);
        with_clipboard(&mut app, Box::new(StubClipboard(file_copy(&["C:/a.png"]))));

        paste(&mut app, Action::PasteImage);

        assert_eq!(app.editor.contents(), "prose\n\n![](C:/a.png)\n");
        assert!(promoted(&app, "C:/a.png"));
    }

    #[test]
    fn a_multi_file_selection_inserts_one_reference() {
        let mut app = app_with_buffer("", 0);
        let _scratch = scratch_save_dir(&mut app);
        with_clipboard(
            &mut app,
            Box::new(StubClipboard(file_copy(&[
                "C:/a.png",
                "C:/b.jpg",
                "C:/c.webp",
            ]))),
        );

        paste(&mut app, Action::Paste);

        assert_eq!(app.editor.contents(), "![](C:/a.png)\n");
    }

    #[test]
    fn a_copied_folder_reports_that_there_is_no_image() {
        // A directory has no image extension, so the paste must say so
        // rather than insert a reference to the folder.
        let mut app = app_with_buffer("prose\n", 6);
        let _scratch = scratch_save_dir(&mut app);
        with_clipboard(
            &mut app,
            Box::new(StubClipboard(file_copy(&["C:/Pictures"]))),
        );

        paste(&mut app, Action::PasteImage);

        assert_eq!(app.editor.contents(), "prose\n", "nothing may be inserted");
        let flash = app.transient.as_ref().expect("a message must be shown");
        assert_eq!(flash.text, "No image or image path on the clipboard");
    }

    #[test]
    fn a_file_list_wins_over_a_bitmap_so_the_bitmap_is_never_saved() {
        // An image viewer's copy puts both the pixels and the source file
        // on the clipboard; the file wins, and the pixels are dropped.
        let mut app = app_with_buffer("prose\n\n", 7);
        let dir = scratch_save_dir(&mut app);
        let mut data = screenshot();
        data.files = vec![std::path::PathBuf::from("C:/Users/me/shot.png")];
        with_clipboard(&mut app, Box::new(StubClipboard(data)));

        paste(&mut app, Action::Paste);

        assert_eq!(
            app.editor.contents(),
            "prose\n\n![](C:/Users/me/shot.png)\n"
        );
        assert!(
            std::fs::read_dir(dir.path())
                .expect("tempdir")
                .next()
                .is_none(),
            "the bitmap must not be written while a source file is on the clipboard"
        );
    }

    #[test]
    fn a_screenshot_with_no_file_list_is_saved_into_the_configured_directory() {
        let mut app = app_with_buffer("prose\n\n", 7);
        let dir = scratch_save_dir(&mut app);
        app.file_path = Some(dir.path().join("notes.md"));
        with_clipboard(&mut app, Box::new(StubClipboard(screenshot())));

        paste(&mut app, Action::PasteImage);

        let written: Vec<String> = std::fs::read_dir(dir.path())
            .expect("tempdir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            written.len(),
            1,
            "the screenshot is written once: {written:?}"
        );
        assert!(written[0].starts_with("image-") && written[0].ends_with(".png"));
        assert!(
            app.editor.contents().ends_with(".png)\n"),
            "the buffer references the file that was written: {:?}",
            app.editor.contents()
        );
    }

    // ── Behaviour the refactor must not break ──────────────────────────
    //
    // These pin behaviour the branch already produces, so they are the
    // regression net for replacing the free functions with the port.  They
    // still compile only once the seam exists.

    #[test]
    fn ctrl_v_with_text_on_the_clipboard_stays_an_ordinary_paste() {
        // Text means the user copied text, whatever else is on the
        // clipboard; the image path must not hijack the chord.
        //
        // Note: the *text* itself is still pasted by `edit_ops`, which
        // reads the OS clipboard directly — routing that half through the
        // same port is a follow-up, not part of this seam — so the
        // assertion here is "no image was inserted", not "the text
        // appeared".
        let mut app = app_with_buffer("prose\n", 6);
        let dir = scratch_save_dir(&mut app);
        let mut data = file_copy(&["C:/shot.png"]);
        data.text = Some("hello".to_owned());
        with_clipboard(&mut app, Box::new(StubClipboard(data)));

        paste(&mut app, Action::Paste);

        assert_eq!(app.editor.contents(), "prose\n");
        assert!(app.editor.parsed.image_blocks.is_empty());
        assert!(app.transient.is_none(), "a text copy is not an image miss");
        assert!(
            std::fs::read_dir(dir.path())
                .expect("tempdir")
                .next()
                .is_none(),
            "a text copy must not write an image either"
        );
    }

    #[test]
    fn an_empty_clipboard_reports_it_without_touching_the_buffer() {
        let mut app = app_with_buffer("prose\n", 6);
        let _scratch = scratch_save_dir(&mut app);
        with_clipboard(&mut app, Box::new(StubClipboard(ClipboardData::default())));

        paste(&mut app, Action::PasteImage);

        assert_eq!(app.editor.contents(), "prose\n");
        let flash = app.transient.as_ref().expect("a message must be shown");
        assert_eq!(flash.text, "No image or image path on the clipboard");
    }

    #[test]
    fn a_copied_image_path_in_text_is_still_referenced() {
        // The behaviour the branch already ships; the port must keep it.
        let mut app = app_with_buffer("", 0);
        let _scratch = scratch_save_dir(&mut app);
        with_clipboard(
            &mut app,
            Box::new(StubClipboard(text_copy(r#""C:\Users\me\shot.png""#))),
        );

        paste(&mut app, Action::PasteImage);

        assert_eq!(app.editor.contents(), "![](C:/Users/me/shot.png)\n");
    }
}
