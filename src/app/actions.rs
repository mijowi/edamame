//! Action-routing layer extracted from `app.rs` in Step 2 of
//! `refactor-app.md`.
//!
//! Owns:
//! - [`App::handle_app_action`] — App-level actions intercepted before
//!   the generic `edit_ops::apply` fallthrough (link follow, palette,
//!   overlays, table buttons toggle, insert table, save copy, …).
//! - [`App::dispatch_action`] — the single unified dispatcher used by
//!   both the run-loop keystroke arm and the palette-pick re-entry
//!   path.  Resolves `handle_app_action` → dirty-quit guard →
//!   `edit_ops::apply` → scroll / flash / link-follow side effects.
//! - The thin `open_X` modal-push helpers + [`App::ensure_keymap_clone`].
//! - Modal-key dispatch and the quit-confirm / column-widths flows.
//! - The [`HandleEvent`] adapter trait used by the run loop.
//! - Live-theme reapply.
//! - Pure helpers `cursor_in_table` and `modal_wheel_delta`.

use crossterm::event::{Event, KeyEventKind, MouseEvent, MouseEventKind};

use crate::app::modal;
use crate::config::sections::{DEFAULT_HANDLER, VIM_HANDLER};
use crate::config::{Action, Config, KeyBindingOverrides, KeyMap, Theme};
use crate::editor::{edit_ops, EditorState};
use crate::input::mode_handler::default::DefaultHandler;
use crate::input::ModeHandler;
use crate::terminal::ColorDepth;
use crate::ui::{settings_overlay, ModalKind};

use super::flash::MessageKind;
use super::modal::ModalOutcome;
use super::App;

/// Actions whose handlers are stubs.  When one of these fires (from a
/// keybinding, palette pick, or other surface) the App pops a generic
/// "not implemented" notice.  This is the single source of truth for
/// unfinished features — grep `NOT_YET_IMPLEMENTED` to enumerate them.
///
/// When you implement one of these for real: add an explicit
/// `Action::Foo => …` arm above the catch-all guard AND remove the
/// variant from this list.  The two are not enforced to stay in
/// sync — an entry left here after a real handler lands will never
/// fire (the explicit arm wins), but the stale entry is misleading
/// to future readers of this list.
pub(super) const NOT_YET_IMPLEMENTED: &[Action] = &[Action::Open];

/// Default-deny gate over [`Action`]s in diff mode.  Returns
/// `Some(action)` when the action is allowed in Review sub-mode (the
/// only sub-mode today); `None` for everything else.  When an in-diff
/// Edit mode lands, this can grow a `(action, sub_mode)` signature so
/// Edit-only and Review-only actions refine the gate.
pub(super) fn diff_safe_action(action: &Action) -> Option<Action> {
    use Action::*;
    let allowed = matches!(
        action,
        DiffNext
            | DiffPrev
            | DiffAcceptHunk
            | DiffRejectHunk
            | DiffAcceptAll
            | DiffRejectAll
            | DiffResetHunk
            | DiffExit
            | ScrollUp
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
    if allowed {
        Some(action.clone())
    } else {
        None
    }
}

/// Default-deny gate over [`Action`]s while a *capturing* search flow
/// (a replace flow) is active.  Returns `Some(action)` for the
/// search-flow actions, read-only navigation (cursor moves, selection,
/// copy), and the always-safe set (scrolling, overlay openers, save,
/// quit, undo/redo of in-flow replaces); `None` for everything that
/// mutates the buffer — those stay unavailable for the duration of the
/// flow, mirroring diff mode.  Navigate-only flows don't capture, so
/// they never reach this gate.
pub(super) fn search_safe_action(action: &Action) -> Option<Action> {
    use Action::*;
    let allowed = matches!(
        action,
        OpenSearch
            | SearchNext
            | SearchPrev
            | SearchReplace
            | SearchReplaceAll
            | SearchExit
            // Read-only navigation: the user can move the cursor, select,
            // and copy while the replace flow holds the `Tab`/`r`/`a` keys.
            | MoveLeft
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
            | ScrollUp
            | ScrollDown
            | ScrollPageUp
            | ScrollPageDown
            | ScrollToTop
            | ScrollToBottom
            | Undo
            | Redo
            | Save
            | SaveAs
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
    if allowed {
        Some(action.clone())
    } else {
        None
    }
}

/// True when the editor's cursor sits inside a table block.  Mirrors
/// the check used by `edit_ops::cursor_in_table`; re-implemented here
/// to keep the App free of a cross-module private dep.
pub(super) fn cursor_in_table(state: &EditorState) -> bool {
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let source = state.buffer.contents();
    crate::editor::table_edit::find_table_at(&source, cursor_byte).is_some()
}

/// Reshape `clipboard` into the payload that replaces a vim VisualLine
/// selection whose widened char range is `range`: the text, made *linewise*
/// — guaranteed to end with a newline whenever the span it replaces does.
///
/// `visual_line_char_range` includes the trailing newline of the last
/// highlighted line, so pasting charwise text (`"foo"`) over it would
/// otherwise weld the following line onto the paste (`alpha\nbeta\ngamma` →
/// `foogamma`).  Appending the newline puts the payload on its own line(s),
/// matching vim's `Vp` with a charwise register.  Text that already ends in a
/// newline — the common V-LINE copy → V-LINE paste round trip — is returned
/// unchanged, so no blank line creeps in.  A selection on a final line with
/// no trailing newline replaces a span that doesn't end in one either, so
/// nothing is appended there.
///
/// Returns `None` when there is nothing to paste (empty clipboard *and* empty
/// kill-ring); the caller treats that as "do nothing" rather than replacing
/// the lines with a bare newline.
///
/// Pure in `clipboard` — the OS clipboard read stays at the call site so this
/// decision can be unit-tested without a live clipboard.
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

/// Translate a wheel event into a `ModalState::scroll_by` delta.
/// Honours the user's configured `mouse_scroll_lines` so a coarser
/// wheel feel applies inside modals as well as the editor.  Returns
/// `0` for non-wheel mouse events so callers can blindly forward
/// every `Event::Mouse` without filtering.
pub(super) fn modal_wheel_delta(event: &MouseEvent, wheel_step: usize) -> i32 {
    let step = wheel_step.max(1) as i32;
    match event.kind {
        MouseEventKind::ScrollUp => -step,
        MouseEventKind::ScrollDown => step,
        _ => 0,
    }
}

/// Private extension trait so `DefaultHandler` can process raw crossterm events
/// (filtering for KeyPress) without exposing this logic in the `ModeHandler`
/// trait (which operates on already-filtered `KeyEvent`s).
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

    /// Intercept App-level actions (`FollowLinkUnderCursor`,
    /// `NavigateBack`, `NavigateForward`) before they hit `edit_ops::apply`.
    /// `TableMoveColumnLeft` / `TableMoveColumnRight` outside a table
    /// also short-circuit to navigation so the default Alt+Arrow
    /// keybinding feels natural even without an override.
    ///
    /// Returns `true` when the action was fully handled here; `false`
    /// means the caller should fall through to `edit_ops::apply`.
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
            // Default Alt+Arrow bindings land on the table actions; when
            // the cursor is outside any table, redirect them to nav.
            Action::TableMoveColumnLeft if !cursor_in_table(&self.editor) => {
                self.navigate_back(doc_height, doc_width);
                true
            }
            Action::TableMoveColumnRight if !cursor_in_table(&self.editor) => {
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
                self.open_export_html_modal();
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
            // Stub actions — every entry in `NOT_YET_IMPLEMENTED` lands
            // here and surfaces the generic notice.  Wire a real handler
            // above and drop the entry from the list to implement.
            a if NOT_YET_IMPLEMENTED.contains(a) => {
                self.notify_not_implemented();
                true
            }
            Action::OpenInExternalEditor => {
                if self.editor.buffer.path().is_none() {
                    self.notify("No file path for buffer", ModalKind::Error);
                } else {
                    // The actual editor invocation needs the live
                    // `Terminal` handle, owned by the run loop.
                    // Mirrors the settings-overlay "Open config.toml"
                    // flow.
                    self.pending_open_file_in_editor = true;
                    self.needs_draw = true;
                }
                true
            }
            Action::ToggleTableButtons => {
                // Skip the toggle on terminals where mouse reporting is
                // unavailable: the gutter glyphs would be inert and
                // confusing.  Otherwise flip and persist, matching the
                // settings-overlay row.
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
                // Vim mode is stored as the modal handler name, not a
                // bool — flip the handler, then let `apply_live_update`
                // rebuild the live `VimState`.
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
                // Pre-flight the blank-line guard before
                // opening the modal so a non-blank cursor surfaces an
                // immediate warning notice.  The same guard subsumes
                // mid-paragraph, heading, list, code-block, and
                // existing-table cases without classifying the block.
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
            // Image / link snippets share one pre-flight: the target
            // block must be able to host inline Markdown (code, HTML,
            // and image blocks hold literal content).  The insert
            // functions run it themselves — against the actual insert
            // offset, after the Preview cursor→scroll sync — and
            // return `false` when it fails.
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
            // Hoisted out of `edit_ops::apply` so all save paths
            // (keystroke, palette, autosave, post-merge resolution
            // in later checkpoints) route through `App::save_buffer`
            // — the single call site for `Buffer::save_file`.
            // `flash_for_action` is invoked here because the
            // post-dispatch flash in `dispatch_action` only fires
            // when `handle_app_action` returns `false`.
            Action::Save => {
                if self.editor.mode == crate::editor::Mode::Diff {
                    self.flash("Resolve diff before saving", MessageKind::Info);
                    return true;
                }
                // A new, never-saved buffer has no destination yet.
                // Prompt for one via the Save As modal instead of
                // letting `save_file` fail into a generic "Save failed".
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

    /// Flip-and-persist path shared by the command-palette setting
    /// toggles.  The caller has already mutated the `config` field;
    /// this writes `config.toml`, pushes the change into any App-side
    /// live cache (reusing the settings-overlay
    /// [`apply_live_update`](crate::app::modal::settings::apply_live_update)
    /// so the two surfaces can't diverge), and flashes the new state.
    /// Unlike the overlay — which flashes the generic "Configuration
    /// updated" — the palette names the setting and its new value.
    ///
    /// The live update runs even when the save fails: the `config`
    /// field is already flipped, so we push it into the live cache
    /// regardless to keep the two in sync (the setting takes effect
    /// for the session, just unpersisted).  This mirrors the overlay,
    /// whose `apply_live_update` also runs unconditionally after save.
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

    /// Pop the topmost modal off the stack, dispatch the key to it,
    /// and apply the resulting [`ModalOutcome`].  Pop-then-dispatch
    /// lets the modal handler take `&mut App` without borrow conflicts
    /// — the popped modal owns itself.  `Continue` re-pushes it,
    /// `Close` drops it, `CloseAnd` drops it then runs the supplied
    /// callback against the now-unborrowed `App`.
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

    /// Route a bracketed paste to the topmost modal.  Mirrors
    /// [`Self::dispatch_modal_key`]'s pop-dispatch-push pattern; only the
    /// text-input modals act on it (the rest inherit the `Modal`
    /// trait's no-op `handle_paste`).
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

    /// Route a left-button click at terminal coords `(col, row)` to the
    /// topmost modal.  Mirrors [`Self::dispatch_modal_key`]'s
    /// pop-dispatch-push pattern so handlers can take `&mut App`.
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

    /// Push the generic "feature not implemented yet" notice.  Called
    /// from the [`NOT_YET_IMPLEMENTED`] dispatch arm; kept as its own
    /// method so any future change to the wording lives in one place.
    pub(super) fn notify_not_implemented(&mut self) {
        self.notify("This feature is not implemented yet.", ModalKind::Normal);
    }

    /// Open the three-button `Save / Discard / Cancel` modal.  Called
    /// when the user requests `Quit` on a dirty buffer.
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

    /// Open the Markdown syntax cheat-sheet popover.  Pushes a
    /// trait-based modal onto the stack; dispatch is handled by the
    /// generic [`Self::dispatch_modal_key`] / wheel routes.
    pub fn open_markdown_cheat_sheet(&mut self) {
        self.modal_stack
            .push(Box::new(modal::CheatSheetModal::new()));
    }

    /// Open the About page.  Deliberately touches no network: the page
    /// no longer reports release information, and its
    /// `[ Check for updates ]` button is the only thing here that
    /// reaches GitHub (see [`App::open_update_modal`]).
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
        self.modal_stack
            .push(Box::new(modal::CommandPaletteModal::new(&keymap)));
    }

    /// Build a fresh copy of the keymap, populating `self.keymap` if
    /// it has not been built yet.  Returns a clone so callers can use
    /// it without holding a borrow on `self`.
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

    /// Dispatch a resolved [`Action`] through the unified pipeline.
    /// Both the run-loop keystroke arm
    /// ([`super::App::dispatch_single_key`]) and the command palette
    /// (`CommandPaletteModal`) funnel through here so there is exactly
    /// one place where `handle_app_action`, the dirty-buffer quit
    /// guard, `edit_ops::apply`, scroll tracking, post-action flashes,
    /// and pending link-follow draining are sequenced.
    pub fn dispatch_action(&mut self, action: Action, doc_height: usize, doc_width: usize) {
        // Search flow owns its own dispatch — gated *before*
        // `handle_app_action` (unlike diff) so app-level actions that
        // mutate the buffer or navigate away (insert table, footnotes,
        // link follow, …) can't fire mid-flow.  `search_safe_action`
        // default-denies everything off the allowlist; allowed
        // app-level openers are re-routed inside
        // `dispatch_search_action`.
        // A navigate-only flow does *not* capture, in vim or default mode —
        // see `search_flow_captures`.
        if self.search_flow_captures() {
            let Some(safe) = search_safe_action(&action) else {
                self.flash_action_unavailable("search");
                return;
            };
            self.dispatch_search_action(safe, doc_height, doc_width);
            return;
        }
        // Non-capturing navigate flow: its own navigation actions (intercepted
        // ahead of the keymap by `search_action_for`) still route to the
        // search dispatcher; everything else falls through to normal editing
        // with the match highlights left in place.
        if self.editor.search.is_some()
            && matches!(
                action,
                Action::SearchNext | Action::SearchPrev | Action::SearchExit
            )
        {
            self.dispatch_search_action(action, doc_height, doc_width);
            return;
        }
        // Visual `Ctrl-C` / `Ctrl-X` / `Ctrl-V`: copy, cut, or replace the
        // *widened* range so the clipboard matches the highlight (§2.6) —
        // inclusive of the char under the cursor charwise, whole rows in
        // VisualLine.  The widening goes through the one shared
        // `vim_ops::visual_span` helper that the render and operator paths
        // use, so the three can never disagree.  `selection` itself is never
        // snapped — Copy restores the stored span so a continued Visual
        // session keeps its true anchor; Cut and Paste consume the span and
        // leave Visual.
        if matches!(action, Action::Copy | Action::Cut | Action::Paste) {
            if let Some(kind) = self.vim.as_ref().and_then(|v| v.visual_kind()) {
                self.dispatch_visual_clipboard(action, kind, doc_height, doc_width);
                return;
            }
        }
        // Diff mode default-denies *before* `handle_app_action`, for the
        // same reason the search flow does: `handle_app_action` answers
        // `true` for everything it handles, so a gate placed behind it
        // never sees those actions at all.  `InsertTable`, the footnote
        // fix-ups and `FollowLinkUnderCursor` are all off the diff
        // allowlist and all handled there — and all reachable mid-review
        // from the command palette, which *is* on the allowlist.  Gated
        // here, the insert-table modal opened over a diff review and its
        // edit landed on the pre-merge buffer.
        //
        // Only the refusal moves.  Dispatch stays below, so an allowed
        // action still reaches `handle_app_action` first (that is what
        // opens the palette and the overlays) and falls through to
        // `dispatch_diff_action` only when nothing there claimed it.
        if self.editor.mode == crate::editor::Mode::Diff && diff_safe_action(&action).is_none() {
            self.flash_action_unavailable("diff review");
            return;
        }
        let handled = self.handle_app_action(&action, doc_height, doc_width);
        if !handled {
            // Diff mode owns its own dispatch — checked *before* the
            // generic dirty-buffer quit guard, because in diff mode the
            // buffer still holds the pre-merge text, so that guard's
            // "Save" path would persist the wrong contents.  `Quit` in
            // diff mode routes to `dispatch_diff_action`, which opens the
            // diff-specific `DiffQuitConfirmModal`.
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

    /// Copy, cut, or paste over a vim Visual selection: widen it first to the
    /// span actually on screen (`vim_ops::visual_span` — inclusive of the char
    /// under the cursor charwise, whole lines in VisualLine), without ever
    /// snapping the persistent half-open `selection`.  `Copy` restores the
    /// stored span afterwards (the user may keep extending in Visual); `Cut`
    /// and `Paste` consume the span and exit Visual, since the selected
    /// content is gone.  `EditorState` / `edit_ops` stay vim-agnostic — the
    /// widening lives entirely here.
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
        // A VisualLine paste needs its payload newline-terminated so it can't
        // weld onto the following line; nothing to paste must not consume the
        // lines, so bail before the widening and leave the session untouched.
        // Charwise paste is an ordinary span replacement — `edit_ops` handles
        // it (and no-ops on an empty clipboard) against the widened selection.
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
                // Same promise as the linewise arm: nothing to paste must not
                // consume the span, so bail while the session is intact rather
                // than letting `edit_ops` no-op and exiting Visual anyway.
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
            // `paste_text` replaces the (now widened) selection with the
            // linewise payload, so the highlighted rows are what gets
            // overwritten — not the charwise anchor..active span.
            Some(text) => edit_ops::paste_text(&mut self.editor, text, doc_height, doc_width),
            None => {
                edit_ops::apply(&mut self.editor, action.clone(), doc_height, doc_width);
            }
        }
        // Mirror the non-vim clipboard path's feedback — the shared dispatch's
        // `flash_for_action` is skipped by our early return, so run it here
        // (flashing "Copied" for Copy / Cut; Paste is silent there, exactly as
        // it is off this path).
        self.flash_for_action(&action, dirty_before);
        if matches!(action, Action::Cut | Action::Paste) {
            // The span is gone (deleted, or overwritten by the paste);
            // drop back to Normal.
            if let Some(vim) = self.vim.as_mut() {
                vim.sub_mode = crate::input::VimSubMode::Normal;
                vim.visual_anchor = None;
            }
            self.editor.selection = None;
        } else {
            // Copy left the buffer untouched — restore the stored span so
            // the highlight and a continued Visual session stay correct.
            self.editor.selection = Some(sel);
        }
        self.needs_draw = true;
    }

    /// Shared free-scroll arms for the diff and search flow
    /// dispatchers: the viewport moves without dragging the cursor
    /// along (unlike `edit_ops` scrolling, which keeps the cursor
    /// visible).  Returns `true` when `action` was a scroll action and
    /// was handled here.
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

    /// Dispatch a single action while `Mode::Diff` is active.  The
    /// caller has already passed `action` through
    /// [`diff_safe_action`] so unsupported actions never arrive here.
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
        // A read-only review (`--diff`) has no decision vocabulary: the
        // sides are paths git chose, so a decision has nowhere to go.
        // Denied here rather than in `diff_safe_action` because that
        // gate answers "is this action legal in diff mode at all", a
        // question with the same answer for both presentations — and it
        // has no `DiffState` in hand to ask.  The command palette can
        // reach these actions too, so a key-table omission would not be
        // enough on its own.
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
                // Manual navigation supersedes any deferred auto-advance,
                // and never triggers the resolve-confirm flow.  That flow
                // has exactly two entry points (see `check_diff_resolution`):
                // resolving the final hunk (a decision action, via the
                // deferred advance) and pressing Esc once everything is
                // resolved.  Tabbing among already-decided hunks must not
                // pop the modal.
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
            // Accept-all / reject-all override *every* hunk in one
            // keystroke, so an accidental press would wipe out a mix of
            // careful per-hunk decisions with no way to undo it
            // (decisions aren't on an undo stack).  Gate the bulk flip
            // behind a confirm modal; the actual `bulk_decide` happens
            // on confirmation via `apply_diff_bulk_decision`.
            Action::DiffAcceptAll => self.open_diff_bulk_confirm(Decision::Accepted),
            Action::DiffRejectAll => self.open_diff_bulk_confirm(Decision::Rejected),
            Action::DiffResetHunk => {
                // Undecide the focused hunk.  Cancel any in-flight
                // post-decision advance first so a freshly-reset hunk
                // keeps focus instead of being skipped past.  A no-op
                // (hunk already `Pending`) leaves everything untouched.
                self.cancel_diff_advance();
                let reset = self.editor.diff.as_mut().is_some_and(|d| d.reset_focused());
                if reset {
                    self.needs_draw = true;
                }
            }
            Action::DiffExit => {
                // Esc is gated on full resolution — diff mode cannot be
                // exited while any hunk is still pending:
                //  - every hunk resolved → open the apply-confirm modal
                //    (entry point 2 of the resolve flow), so a fully
                //    reviewed diff is applied via an explicit choice.
                //  - anything still pending → no-op + a hint; the user
                //    must decide every hunk before leaving (Apply on the
                //    confirm modal is the exit, or Quit to abandon).
                self.cancel_diff_advance();
                // A read-only review is the whole session — there is no
                // editor behind it to return to, and nothing to resolve
                // — so Esc leaves the process.  git difftool runs one
                // file per invocation, so this is what advances the loop
                // to the next file.
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
            // Note: `SaveAs` is deliberately excluded from
            // `diff_safe_action` (and so never reaches here) — it
            // re-points the buffer path and watcher, which would desync
            // the live diff.
            Action::Quit => {
                // Nothing is at stake in a read-only review — no
                // decisions, no unapplied merge — so the confirmation
                // would be a prompt with one sensible answer.  The flag
                // is what tells `main` to end the whole `git difftool`
                // walk and not just this file.
                if self.editor.diff.as_ref().is_some_and(|d| d.read_only) {
                    self.diff_stop_walk = true;
                    self.should_quit = true;
                    return;
                }
                // An active diff review is unapplied work — quitting
                // would discard the pending external change and every
                // decision.  Warn first, mirroring the dirty-buffer
                // quit guard; `DiffQuitConfirmModal` handles the actual
                // discard-and-quit on confirm.  Don't stack a second
                // copy if one is already up.
                if !self
                    .modal_stack
                    .contains::<crate::app::modal::DiffQuitConfirmModal>()
                {
                    self.modal_stack
                        .push(Box::new(crate::app::modal::DiffQuitConfirmModal::new()));
                    self.needs_draw = true;
                }
            }
            // Read-only overlay openers route through their
            // standard App-level helpers, which push the modal atop
            // the diff view.
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
            // Everything else passed `diff_safe_action` but doesn't
            // need a specific arm (e.g. NavigateBack/Forward, which
            // are app-level and handled by `handle_app_action`).
            _ => {}
        }
    }

    /// Record an accept/reject on the focused hunk and arm the deferred
    /// advance so the user sees the decision land before focus moves on
    /// (§ diff-mode UX).  A prior pending advance is flushed first so
    /// rapid taps walk through hunks rather than re-deciding one.  When
    /// this decision resolves the final hunk, the deferred advance's
    /// `check_diff_resolution` is what opens the confirm modal — so the
    /// resolve flow is triggered by the *act* of deciding, not by merely
    /// landing in a resolved state.
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

    /// Open the bulk-decision confirm modal for `DiffAcceptAll` /
    /// `DiffRejectAll`.  No-op when no diff is active or a copy of the
    /// modal is already on the stack (so a held / repeated key can't
    /// stack duplicates).  The decision is *not* applied here — that
    /// waits for the user's `[Yes]` (see `apply_diff_bulk_decision`).
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

    /// Apply a confirmed bulk decision to every hunk, then route through
    /// the normal resolution check (which opens the apply-confirm modal
    /// once everything is decided).  Invoked from the bulk-confirm
    /// modal's `[Yes]` callback.
    pub(crate) fn apply_diff_bulk_decision(&mut self, decision: crate::diff::Decision) {
        self.cancel_diff_advance();
        if let Some(d) = self.editor.diff.as_mut() {
            d.bulk_decide(decision);
            self.needs_draw = true;
        }
        self.check_diff_resolution();
    }

    /// Push the apply-confirm modal iff every hunk has been decided.
    /// This is the *single* place the modal is opened, and it has only
    /// two callers so the resolve flow has exactly two entry points:
    /// (1) `apply_diff_advance` after a decision resolves the final hunk
    /// (including bulk accept-all / reject-all), and (2) `Action::DiffExit`
    /// (Esc) when the diff is already fully resolved.  Hunk navigation
    /// deliberately does *not* call this — tabbing through resolved hunks
    /// must not re-open the modal.
    pub(crate) fn check_diff_resolution(&mut self) {
        let Some(diff) = self.editor.diff.as_ref() else {
            return;
        };
        if !diff.all_resolved() {
            return;
        }
        // Already showing the confirm modal?  Don't push a second one.
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

    /// Enter diff-review mode against the on-disk contents the user's
    /// `DirtyConflictModal` was carrying.  Push the intro modal first
    /// when the user hasn't opted out (§8).
    pub(crate) fn enter_diff_mode(&mut self, on_disk: String) {
        // A half-typed vim command line (`:`/`/`/`?`) can't survive
        // into diff review: vim key handling is deferred while in diff
        // mode (so the prompt could never be completed), and a stale
        // `cmdline` outranks the diff hint row in `hint_content`,
        // masking the diff-review chords.  Drop it — along with any
        // in-progress multi-key parse — so the hint line reads diff.
        // End the prompt's live sessions first, as Esc would: the
        // incsearch session must not dangle on `VimState` (the next `/`
        // would reuse its stale saved view), and a live `:s` preview
        // must revert its transient buffer edit before the `old`
        // snapshot below — otherwise the diff is taken against preview
        // text and the preview's app-level gates hold forever.
        if let Some(vim) = self.vim.as_mut() {
            crate::editor::vim_ops::end_incsearch(&mut self.editor, &mut vim.incsearch);
            vim.cmdline = None;
            vim.reset_pending();
        }
        crate::editor::vim_ops::clear_substitute_preview(
            &mut self.editor,
            /*restore_view=*/ true,
        );
        // A search flow can't survive into diff review either — the
        // diff view replaces the document and its resolution will swap
        // the buffer out from under the match list.  Runs after
        // `end_incsearch` so a restored prior hlsearch session is torn
        // down along with the advance timer.
        self.exit_search_flow();
        let old = self.editor.buffer.contents();
        let Some(diff_state) = crate::diff::DiffState::new(&old, &on_disk) else {
            // Edge case: the dirty-conflict modal was opened against
            // bytes that happen to match the buffer now (perhaps the
            // user reverted manually before clicking Merge).  Flash
            // a hint and return to the main editor.
            self.flash("No differences to review", MessageKind::Info);
            return;
        };
        let uneven_table_fallback = diff_state.uneven_table_fallback;
        self.editor.enter_diff_mode(diff_state);
        // Only ever show one intro modal at a time.  A clean buffer stays
        // clean while in diff review, so a second (or third) external
        // overwrite re-enters this path and would otherwise stack another
        // identical modal on top — the user then has to dismiss each one
        // in turn.  Skip the push when one is already on the stack.
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

    /// Install a **read-only** review of `old` vs `new` and enter diff
    /// mode — the `--diff` difftool presentation.
    ///
    /// Distinct from [`Self::enter_diff_mode`] rather than a flag on it,
    /// because almost none of that method's work applies: there is no
    /// vim command line to tear down, no search flow to end, and no
    /// intro modal to push (it teaches the accept/reject vocabulary this
    /// review does not have). What it shares is the one thing that
    /// matters — `DiffState::new` refusing an empty hunk list, and
    /// `EditorState::enter_diff_mode` as the single door into
    /// `Mode::Diff`.
    ///
    /// The buffer is seeded with the *old* side so the review reads the
    /// way every other one does (buffer = before, `new_buffer` = after)
    /// and so `resolved_rope`'s coordinate space stays meaningful should
    /// a later write-back mode want it. `dirty` is left false by
    /// `replace_buffer`, and `file_path` stays `None`, so nothing in the
    /// process has a path to save over.
    ///
    /// Returns `false` when the two files are byte-identical — git only
    /// invokes a difftool for paths it believes differ, but a
    /// whitespace- or mode-only change can still reach us, and opening
    /// an empty review would be worse than saying so on stderr.
    pub fn enter_read_only_diff(&mut self, old: String, new: String) -> bool {
        let Some(mut diff_state) = crate::diff::DiffState::new(&old, &new) else {
            return false;
        };
        diff_state.read_only = true;
        let uneven_table_fallback = diff_state.uneven_table_fallback;
        self.editor
            .replace_buffer(crate::document::Buffer::from_str(&old));
        self.editor.enter_diff_mode(diff_state);
        // `App::new` built the three media prompts from the editor it
        // was handed, which for a `--diff` session is the empty startup
        // buffer — so this is the call that asks them against the
        // document actually under review.  Without it a mermaid diagram
        // or a remote image in a clean region shows a placeholder for
        // the whole session under the default `ask` policy, because
        // nothing else ever sets `session_diagrams_enabled` /
        // `session_images_enabled` and `effective_*_enabled` is false
        // until something does (issue #30's shape, one path further on).
        // After `enter_diff_mode`, since the prompts read
        // `editor.parsed`, which `replace_buffer` has just rebuilt.
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

    /// Set the status-bar label for a difftool session (see
    /// [`App::diff_label`]).
    pub fn set_diff_label(&mut self, label: Option<String>) {
        self.diff_label = label;
    }

    /// Apply the merged result to the editor buffer and exit diff
    /// mode.  Called from the `[Apply]` button of
    /// [`crate::app::modal::DiffResolveConfirmModal`].  Records a single
    /// coarse "merge-revert" history entry (§6) so one `Ctrl-Z` from
    /// normal mode reverts the whole merge and one `Ctrl-Y` re-applies
    /// it; any new edit afterwards clears the redo path as usual.
    pub(crate) fn apply_diff_resolution(&mut self) {
        self.cancel_diff_advance();
        let Some(diff) = self.editor.diff.as_ref() else {
            return;
        };
        let Some(resolved) = diff.resolved_rope() else {
            self.flash("Diff is not fully resolved", MessageKind::Info);
            return;
        };
        // Compare resolved bytes against the disk contents so we
        // only set dirty when the merge actually diverges from disk.
        let new_text = diff.new_buffer.contents();
        let resolved_text = resolved.to_string();
        let differs_from_disk = resolved_text != new_text;
        // The pre-merge buffer is the diff's `old_rope` (entering diff
        // mode never mutates `editor.buffer`).  Build the single
        // synthetic delta that, on undo, restores it and, on redo,
        // re-applies the merged text.
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
        // The merge replaced the document's contents wholesale, so it
        // owes the same bookkeeping a file load does — including the
        // media prompts, since the external change may have introduced
        // this session's first image / diagram / remote URL.  Must
        // follow `refresh_parsed`: the prompts are built from
        // `editor.parsed`.
        self.on_document_contents_swapped();
        self.flash("Diff resolved", MessageKind::Success);
        self.needs_draw = true;
    }

    /// Exit diff mode without applying the merge.  Restores the
    /// pre-diff buffer state (the diff's `old_rope` already equals
    /// the editor's current buffer, so this is just a clean-up).
    pub(crate) fn exit_diff_mode_discarding(&mut self) {
        self.cancel_diff_advance();
        self.editor.exit_diff_mode();
        // Tear down every diff-specific modal: the diff they refer to
        // is gone, so leaving one buried (e.g. behind the file-deleted
        // modal when the file vanishes mid-review) would let a stale
        // confirmation fire against no diff.  `remove_first` is a no-op
        // for any not present, so this is safe from every caller.
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

    /// The single call site for [`crate::document::Buffer::save_file`]
    /// across the application.  Every save path funnels through
    /// here so that follow-up state — clearing the dirty flag today,
    /// and the watcher's own-write hash stamp in a later checkpoint —
    /// has a single place to live.  Routed callers today:
    ///
    /// - keystroke / palette-invoked `Action::Save`
    ///   ([`App::handle_app_action`])
    /// - idle autosave ([`App::tick_autosave`])
    /// - save-before-navigate from the dirty-link guard
    ///   ([`super::modal::DirtyGuardModal`])
    /// - save-and-quit from the dirty-quit confirm modal
    ///   ([`super::modal::QuitConfirmModal`])
    /// - save-before-launch in the external-editor flow
    ///   ([`App::open_current_file_in_editor`])
    /// - future post-merge diff resolution
    ///
    /// Callers are responsible for their own success / failure UX
    /// (toast vs. error modal vs. silent autosave flash); this helper
    /// only returns the underlying `Result` so each caller can shape
    /// the message it wants.
    pub(super) fn save_buffer(&mut self) -> anyhow::Result<()> {
        self.editor.buffer.save_file()?;
        self.editor.dirty = false;
        // Stamp the just-written contents so the watcher's own-write
        // filter drops the inotify echo that our own save is about
        // to generate.  Computed from the in-memory rope rather than
        // re-reading disk — they are byte-identical at this point
        // (Buffer::save_file just wrote `rope.to_string()`) and the
        // memory read is dramatically cheaper.
        let bytes = self.editor.buffer.contents();
        self.set_disk_hash(bytes.as_bytes());
        Ok(())
    }

    /// Save the buffer to a new path and adopt it as the buffer's
    /// home.  Backs every "Save As" path: the [`modal::SaveAsModal`]
    /// (palette `SaveAs`, a path-less `Save`, vim `:w <path>` /
    /// `:saveas`) and the file-deleted recovery flow.  Writing the rope
    /// elsewhere re-points the buffer, the App's `file_path`, and the
    /// filesystem watcher at the new location rather than leaving them
    /// bound to the old path.
    ///
    /// Mirrors [`Self::save_buffer`]'s post-write bookkeeping (clear
    /// dirty, stamp the own-write hash) and additionally re-points the
    /// watcher — best-effort, matching [`Self::load_file_into_editor`].
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

    /// Stamp the watcher's own-write filter from raw bytes.  Used by
    /// callers that do not already have an `incoming_hash` computed
    /// — the initial file load and the manual / autosave save paths.
    /// The accepted-`FileChanged` arm in `file_changed.rs` writes
    /// `last_disk_hash` directly to avoid hashing the same bytes
    /// twice.
    pub(crate) fn set_disk_hash(&mut self, bytes: &[u8]) {
        self.last_disk_hash = Some(seahash::hash(bytes));
    }

    /// Open the settings overlay.
    pub fn open_settings_overlay(&mut self) {
        self.modal_stack
            .push(Box::new(modal::SettingsOverlayModal::new()));
    }

    /// Open the welcome modal on demand — the capability-aware settings
    /// surface.  Unlike the startup path this ignores
    /// `config.editor.show_welcome`, and it rebuilds the state from the
    /// live `capabilities`, so it doubles as the "my terminal changed"
    /// entry point: images / diagrams / theme are re-gated against what
    /// *this* terminal can actually do.  Guarded against stacking two
    /// copies, like the About modal.
    pub fn open_welcome_modal(&mut self) {
        if self.modal_stack.contains::<modal::WelcomeModal>() {
            return;
        }
        self.modal_stack.push(Box::new(modal::WelcomeModal::new(
            &self.capabilities,
            &self.config,
        )));
    }

    /// Open the fuzzy-searchable theme picker.  Replaces the Theme
    /// row that the settings overlay used to carry — selecting a row
    /// writes `config.theme`, saves, and reapplies the palette live.
    pub fn open_theme_picker(&mut self) {
        let current = self.config.theme.clone();
        // If the configured appearance doesn't match the current theme's
        // appearance (e.g. config.toml was hand-edited), open the picker
        // in the mode that actually contains the current theme — otherwise
        // it would be filtered out of the list and the "(current)" marker
        // would never render.
        let mode =
            crate::config::theme::theme_appearance(&current).unwrap_or(self.config.appearance);
        let themes = crate::config::theme::list_theme_names_for_mode(mode);
        self.modal_stack.push(Box::new(modal::ThemePickerModal::new(
            themes, current, mode,
        )));
    }

    /// Open the keybinds overlay.  Builds a live `KeyMap` if one
    /// hasn't been kept around yet.
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

    /// Open the rows/columns prompt.  Caller is expected to have
    /// already verified the cursor sits on a blank line via
    /// [`crate::editor::table_edit::cursor_line_is_blank`]; this method just
    /// seeds the modal state.
    pub fn open_insert_table_modal(&mut self) {
        self.modal_stack
            .push(Box::new(modal::InsertTableModal::new()));
    }

    /// Open the "Save As" path-entry modal, seeded from the buffer's
    /// current path (or a default for an unnamed buffer).  On submit the
    /// buffer is written and *re-pointed* at the new path via
    /// [`Self::save_buffer_as`].  `after_save` runs once the write
    /// succeeds — used by the save-then-quit / save-then-navigate flows
    /// so a path-less buffer can finish a deferred action after the user
    /// supplies a path.
    pub fn open_save_as_modal(&mut self, after_save: Option<modal::save_as::AfterSave>) {
        let m = modal::SaveAsModal::for_buffer_path(self.editor.buffer.path(), after_save);
        self.modal_stack.push(Box::new(m));
    }

    /// Write the buffer to a named `path`, adopting it — but first prompt
    /// for confirmation when the write would clobber a *different*
    /// existing file (see [`crate::document::Buffer::would_overwrite`]).
    /// `force` skips that prompt (vim `:w!` / `:saveas!`).  `after` runs
    /// once the write succeeds (e.g. quit for `:wq <path>`).
    ///
    /// Used by the vim direct-save path, where the destination is named
    /// on the command line so no path-entry modal is involved.  The Save
    /// As modal does its own overwrite check inline (it owns the path
    /// field) and pushes the same [`modal::OverwriteConfirmModal`].
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

    /// Write a snapshot of the buffer to a named `path` *without* changing
    /// the buffer's own path (vim `:w <path>` — the user keeps editing the
    /// current file).  Like [`Self::save_buffer_as_confirmed`] it confirms
    /// before clobbering a *different* existing file; `force` (`:w!`) skips
    /// that prompt.  `after` runs once the write succeeds (e.g. quit for
    /// `:wq <path>`).
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

    /// Drain `EditorState::pending_column_widths_commit` (set by a
    /// column-border drag's Release) and decide what happens next:
    ///   * No pending commit → no-op.
    ///   * Table already has a `<!-- tui-columns: ... -->` comment, OR
    ///     `config.table.warn_on_width_injection` is false → commit
    ///     immediately.
    ///   * Otherwise → open the warning modal carrying the table's
    ///     `table_byte_start` so its handler can call back to commit /
    ///     cancel via `EditorState`.
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

    /// Reload the theme named by `self.config.theme` from disk, build
    /// a fresh `Theme`, leak it into `'static`, and swap it onto
    /// `self.theme` and the editor.  Any non-fatal warnings raised by
    /// the theme loader (parse error, unknown keys) are surfaced via
    /// the existing `ConfigWarningModal`, which renders above the
    /// settings overlay so a malformed theme is the first thing the
    /// user sees.
    ///
    /// # Leak by design
    ///
    /// `Theme` is held everywhere as `&'static Theme` — see the
    /// constructor for the rationale (every widget and `EditorState`
    /// reads it on the hot render path, and threading a lifetime or
    /// wrapping in `Arc` would touch dozens of call sites for no
    /// observable benefit).  `'static` is obtained by `Box::leak`-ing
    /// the heap allocation.
    ///
    /// Each theme change leaks one fresh `Theme` allocation: the
    /// previous one is unreachable but never freed, since `'static`
    /// references can't be invalidated.  The cost per leak is bounded
    /// — a `Theme` is a fixed-size struct of ~100 `Style` values, on
    /// the order of a few KB — and theme changes are user-initiated
    /// (Enter / Left / Right on the settings overlay's Theme row, or
    /// post-editor reload).  Even an aggressive cycler would
    /// accumulate at most a few MB across the editor's session.
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

    // ── The diff-mode gate sits ahead of `handle_app_action` ───────

    /// `handle_app_action` answers `true` for everything it handles, so
    /// a gate placed behind it never sees those actions.  `InsertTable`
    /// is off the diff allowlist *and* handled there, and the command
    /// palette — which is on the allowlist — can reach it mid-review.
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

    /// Only the *refusal* moved: an allowed app-level action still
    /// reaches `handle_app_action`, which is what opens the palette and
    /// the overlays during a review.
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

    use super::{edit_ops, linewise_paste_payload, modal_wheel_delta};
    use crate::app::test_utils::make_app;
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
        // A clean buffer stays clean during diff review, so a second
        // external overwrite re-enters `enter_diff_mode`.  Only one
        // intro modal must ever be on the stack — otherwise the user
        // has to dismiss one per overwrite.
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
        // V-LINE on the first line: the charwise span is empty (anchor ==
        // active) but the whole line is the effective selection.
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

    /// The linewise-paste payload rules, exercised directly so no live
    /// clipboard is involved (the OS clipboard is global and would race
    /// parallel tests).
    #[test]
    fn linewise_paste_payload_keeps_the_line_structure() {
        let buf = Buffer::from_str("alpha\nbeta\ngamma\n");
        // Lines 0..=1 → "alpha\nbeta\n", a span ending in a newline.
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
        // No trailing newline on the last line, so the replaced span
        // doesn't end in one either and nothing should be appended.
        let buf = Buffer::from_str("alpha\nbeta");
        let range = 6..10;
        assert_eq!(
            linewise_paste_payload(&buf, &range, "foo".to_owned()),
            Some("foo".to_owned()),
        );
    }

    /// The widening + payload composition that `dispatch_visual_line_clipboard`
    /// performs, run against a *charwise* payload — the case the end-to-end
    /// wiring test in `app.rs` can't reach (it must Copy first, which yields a
    /// linewise payload, to be independent of the live OS clipboard).
    #[test]
    fn visual_line_paste_of_charwise_text_keeps_the_line_intact() {
        use crate::document::Selection;
        let mut app = make_app();
        app.editor.buffer.insert(0, "alpha\nbeta\ngamma\n");
        app.editor.mode = crate::editor::Mode::Rendered;
        app.editor.refresh_parsed();
        // V-LINE parked mid-line-1: the charwise span is empty, the widened
        // one is the whole line including its newline.
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
        // The hint line now reads the diff-review chords, not a stale
        // command line.
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
        // Buffer holds the *before* side, as in every other review.
        assert_eq!(app.editor.buffer.contents(), "alpha\nbeta\n");
        assert!(!app.editor.dirty);
    }

    /// The intro modal teaches accept/reject, which this review does not
    /// have — pushing it would document keys that answer "read-only".
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

    /// Every decision action is refused, including via the command
    /// palette — the key table is not the only way to reach them.
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

    /// Esc ends the process: there is no editor behind the review to
    /// return to, and git difftool runs one file per invocation.
    #[test]
    fn esc_quits_a_read_only_review_without_resolving() {
        use crate::config::Action;
        let mut app = read_only_app("alpha\nbeta\n", "alpha\nBETA\n");
        app.dispatch_action(Action::DiffExit, 40, 80);
        assert!(app.should_quit);
        assert!(!app.diff_stop_walk(), "Esc moves on to the next file");
    }

    /// Quit skips the discard-confirm modal (nothing is at stake) and
    /// flags the abort so `main` can exit non-zero.
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

    /// The review *is* the document for a `--diff` session, so the media
    /// prompts have to be asked against it: `App::new` built them from
    /// the empty startup buffer, and under the default `ask` policy
    /// nothing else would ever set `session_diagrams_enabled` — the
    /// diagram would render as a placeholder for the whole review with
    /// no question asked.
    #[test]
    fn a_read_only_review_asks_the_media_prompts_for_its_document() {
        use crate::app::modal::DiagramsEnabledPromptModal;
        let old = "# Doc\n\n```mermaid\ngraph TD;\nA-->B;\n```\n\nbefore\n";
        let new = "# Doc\n\n```mermaid\ngraph TD;\nA-->B;\n```\n\nafter\n";
        let app = read_only_app(old, new);
        assert!(
            matches!(
                app.config.diagrams.enabled,
                crate::config::DiagramsEnabled::Ask
            ),
            "ask is the default policy this test depends on",
        );
        assert!(
            app.modal_stack.contains::<DiagramsEnabledPromptModal>(),
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
        // Dismiss the intro modal if it stacked.
        app.modal_stack
            .remove_first::<crate::app::modal::DiffIntroModal>();
        // Accept-all now opens the bulk-confirm modal rather than
        // deciding immediately; confirm it, then apply.
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
        // Exactly one coarse undo step: the merge-revert entry.
        assert_eq!(app.editor.history.undo_depth(), 1);

        // One Undo reverts the whole merge back to the pre-merge buffer.
        crate::editor::edit_ops::apply(&mut app.editor, Action::Undo, 24, 80);
        assert_eq!(app.editor.buffer.contents(), "alpha\nbeta\ngamma\n");
        // One Redo re-applies the merged result.
        crate::editor::edit_ops::apply(&mut app.editor, Action::Redo, 24, 80);
        assert_eq!(app.editor.buffer.contents(), "alpha\nBETA\ngamma\n");
    }

    /// Helper: enter diff mode against `disk`, dropping the intro modal
    /// so it doesn't interfere with stack assertions.
    fn app_in_diff(buffer: &str, disk: &str) -> crate::app::App {
        let mut app = make_app();
        app.editor.buffer.insert(0, buffer);
        app.enter_diff_mode(disk.to_owned());
        app.modal_stack
            .remove_first::<crate::app::modal::DiffIntroModal>();
        app
    }

    /// Resolve every hunk directly (no action), so a subsequent action
    /// is the only thing under test.
    fn resolve_all(app: &mut crate::app::App, decision: crate::diff::Decision) {
        for d in app.editor.diff.as_mut().unwrap().decisions.iter_mut() {
            *d = decision;
        }
    }

    #[test]
    fn resolving_a_diff_that_adds_an_image_queues_the_images_prompt() {
        // A clean buffer's external change goes to diff review by
        // default (`diff_on_change`), so this — not
        // `reload_buffer_from_disk` — is the usual way a document picks
        // up its first image mid-session.  Without the prompt,
        // `effective_images_enabled` stays false and the merged image
        // never decodes (issue #30).
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
        // Tab / Shift-Tab among already-resolved hunks must not pop the
        // apply-confirm modal — navigation is not a resolve trigger.
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
        // Pre-resolve everything as accepted, so nothing is `Pending`.
        resolve_all(&mut app, Decision::Accepted);
        // Reject-all opens the bulk-confirm modal *without* yet changing
        // any decision — the prior accepts must stay intact until the
        // user confirms.
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
        // A second press must not stack a duplicate modal.
        app.dispatch_diff_action(Action::DiffRejectAll, 24, 80);
        assert_eq!(app.modal_stack.count::<DiffBulkConfirmModal>(), 1);

        // Confirming applies the override and opens the resolve-confirm.
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
        // Make a deliberate mix of per-hunk decisions.
        {
            let d = app.editor.diff.as_mut().unwrap();
            d.decisions[0] = Decision::Accepted;
            d.decisions[1] = Decision::Rejected;
        }
        let before = app.editor.diff.as_ref().unwrap().decisions.clone();
        // Accept-all opens the gate; dismissing it (without the [Yes]
        // callback) must leave every prior decision untouched.
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
        // Quit mid-review must not silently discard + quit: it pushes
        // the diff-quit confirm modal and leaves the review intact.
        let mut app = app_in_diff("a\nb\nc\n", "A\nb\nC\n");
        app.dispatch_action(Action::Quit, 24, 80);
        assert!(
            app.modal_stack.contains::<DiffQuitConfirmModal>(),
            "Quit in diff mode must open the discard-confirm modal",
        );
        assert!(!app.should_quit, "Quit must not fire before confirmation");
        assert!(app.editor.diff.is_some(), "the review must stay active");

        // A second Quit while the modal is up must not stack a duplicate.
        app.dispatch_action(Action::Quit, 24, 80);
        assert_eq!(app.modal_stack.count::<DiffQuitConfirmModal>(), 1);
    }

    #[test]
    fn diff_esc_is_gated_on_full_resolution() {
        use crate::app::modal::DiffResolveConfirmModal;
        use crate::config::Action;
        use crate::diff::Decision;

        // Pending hunks: Esc must NOT exit or discard — the diff stays
        // active and no confirm modal opens.
        let mut app = app_in_diff("a\nb\nc\n", "A\nb\nC\n");
        app.dispatch_diff_action(Action::DiffExit, 24, 80);
        assert!(
            app.editor.diff.is_some(),
            "Esc with pending hunks must not exit diff mode",
        );
        assert!(!app.modal_stack.contains::<DiffResolveConfirmModal>());

        // All resolved: Esc opens the confirm modal (the only exit path).
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
        // Build minimal `MouseEvent`s with the kinds we care about;
        // crossterm requires explicit modifier + column/row fields.
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
        // Coarser wheel honoured.
        assert_eq!(modal_wheel_delta(&scroll_down, 4), 4);
        // Wheel-step floor is 1, even when config asks for 0.
        assert_eq!(modal_wheel_delta(&scroll_up, 0), -1);
        // Non-wheel events return 0 so callers can blindly forward.
        assert_eq!(modal_wheel_delta(&click, 1), 0);
    }
}
