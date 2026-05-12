//! Action-routing layer extracted from `app.rs` in Step 2 of
//! `refactor-app.md`.
//!
//! Owns:
//! - [`App::handle_app_action`] — App-level actions intercepted before
//!   the generic `edit_ops::apply` fallthrough (link follow, palette,
//!   overlays, table buttons toggle, insert table, save copy, …).
//! - [`App::dispatch_palette_action`] — palette-pick re-entry into the
//!   same dispatch pipeline as a direct keystroke.
//! - The thin `open_X` modal-push helpers + [`App::ensure_keymap_clone`].
//! - Modal-key dispatch and the quit-confirm / column-widths flows.
//! - The [`HandleEvent`] adapter trait used by the run loop.
//! - Live-theme reapply.
//! - Pure helpers `cursor_in_table` and `modal_wheel_delta`.

use crossterm::event::{Event, KeyEventKind, MouseEvent, MouseEventKind};

use crate::app::modal;
use crate::config::{Action, Config, KeyBindingOverrides, KeyMap, Theme};
use crate::editor::{edit_ops, EditorState};
use crate::input::mode_handler::default::DefaultHandler;
use crate::input::ModeHandler;
use crate::terminal::ColourDepth;

use super::flash::MessageKind;
use super::modal::ModalOutcome;
use super::App;

/// True when the editor's cursor sits inside a table block.  Mirrors
/// the check used by `edit_ops::cursor_in_table`; re-implemented here
/// to keep the App free of a cross-module private dep.
pub(super) fn cursor_in_table(state: &EditorState) -> bool {
    let cursor_byte = state.buffer.rope().char_to_byte(state.cursor.offset);
    let source = state.buffer.contents();
    crate::editor::table_edit::find_table_at(&source, cursor_byte).is_some()
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
            Action::OpenGitHub => {
                const EDAMAME_GITHUB_URL: &str = "https://github.com/gorgonian/edamame";
                self.spawn_open_worker(EDAMAME_GITHUB_URL.to_string());
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
            // Phase 10 — palette + configuration overlays.
            Action::ShowCommandPalette => {
                self.open_command_palette();
                true
            }
            Action::ShowMarkdownCheatSheet => {
                self.open_markdown_cheat_sheet();
                true
            }
            // Phase 10 review — ShowCheatSheet is no longer a
            // separate flow.  We accept it as an alias for
            // OpenKeybinds so users with a custom keybinding to it
            // (the action is configurable per `keybindings.toml`)
            // still see the combined view+edit overlay.
            Action::ShowCheatSheet => {
                self.open_keybinds_overlay();
                true
            }
            Action::OpenSettings => {
                self.open_settings_overlay();
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
            Action::OpenConfigFolder => {
                if let Some(dir) = Config::config_dir() {
                    self.spawn_open_worker(dir.display().to_string());
                } else {
                    self.flash("No config directory available", MessageKind::Error);
                }
                true
            }
            // Phase 16 / Phase 11 — these overlays are wired up in their
            // own phases.  Until then, surface a flash so users hitting
            // them in the palette get explicit feedback rather than
            // silent failure.
            Action::ExportHtml => {
                self.flash("HTML export — see Phase 16", MessageKind::Info);
                true
            }
            Action::ReloadFromDisk => {
                self.flash("Reload from disk — see Phase 11", MessageKind::Info);
                true
            }
            Action::OpenInExternalEditor => {
                if self.editor.buffer.path().is_none() {
                    self.flash("No file path for buffer", MessageKind::Error);
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
                // In-memory only — never write the change back to
                // `config.toml`.  Settings the user wants to keep
                // belong in the settings overlay.  Skip the toggle on
                // terminals where mouse reporting is unavailable: the
                // gutter glyphs would be inert and confusing.
                if self.capabilities.mouse {
                    self.config.table.show_buttons = !self.config.table.show_buttons;
                    let state = if self.config.table.show_buttons {
                        "on"
                    } else {
                        "off"
                    };
                    self.flash(format!("Table buttons {state}"), MessageKind::Info);
                } else {
                    self.flash("Mouse not supported on this terminal", MessageKind::Error);
                }
                self.needs_draw = true;
                true
            }
            Action::InsertTable => {
                // Pre-flight the blank-line guard before
                // opening the modal so a non-blank cursor surfaces an
                // immediate sticky error.  The same guard subsumes
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
                    self.flash("Insert Table requires a blank line", MessageKind::Warning);
                }
                self.needs_draw = true;
                true
            }
            Action::SaveCopy => {
                self.open_save_copy_modal();
                self.needs_draw = true;
                true
            }
            _ => false,
        }
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
        let outcome = top.handle_click(col, row);
        match outcome {
            ModalOutcome::Continue => self.modal_stack.push(top),
            ModalOutcome::Close => {}
            ModalOutcome::CloseAnd(cb) => cb(self),
        }
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
            .push(Box::new(modal::CheatSheetModal::new(self.theme)));
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

    /// Dispatch an `Action` chosen from the palette through the same
    /// path as a direct keystroke.  Mirrors the dispatch arm in
    /// [`App::run`].
    pub fn dispatch_palette_action(&mut self, action: Action, doc_height: usize, doc_width: usize) {
        let handled = self.handle_app_action(&action, doc_height, doc_width);
        if !handled {
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

    /// Open the settings overlay.
    pub fn open_settings_overlay(&mut self) {
        self.modal_stack
            .push(Box::new(modal::SettingsOverlayModal::new()));
    }

    /// Open the fuzzy-searchable theme picker.  Replaces the Theme
    /// row that the settings overlay used to carry — selecting a row
    /// writes `config.theme`, saves, and reapplies the palette live.
    pub fn open_theme_picker(&mut self) {
        let themes = crate::config::theme::list_theme_names();
        let current = self.config.theme.clone();
        self.modal_stack
            .push(Box::new(modal::ThemePickerModal::new(themes, current)));
    }

    /// Open the keybinds overlay.  Builds a live `KeyMap` if one
    /// hasn't been kept around yet.
    pub fn open_keybinds_overlay(&mut self) {
        let keymap = self.ensure_keymap_clone();
        self.modal_stack
            .push(Box::new(modal::KeybindsOverlayModal::new(&keymap)));
    }

    /// Open the rows/columns prompt.  Caller is expected to have
    /// already verified the cursor sits on a blank line via
    /// [`editor::table_edit::cursor_line_is_blank`]; this method just
    /// seeds the modal state.
    pub fn open_insert_table_modal(&mut self) {
        self.modal_stack
            .push(Box::new(modal::InsertTableModal::new()));
    }

    /// Open the path-input prompt seeded with a sensible default
    /// derived from the current buffer's filename (e.g. `notes.md`
    /// becomes `notes copy.md`).
    pub fn open_save_copy_modal(&mut self) {
        self.modal_stack
            .push(Box::new(modal::SaveCopyModal::for_buffer_path(
                self.editor.buffer.path(),
            )));
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
        let (theme_file, warnings) = Config::load_theme(&self.config.theme);
        let monochrome = self.capabilities.colour_depth == ColourDepth::NoColour;
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
    use crossterm::event::{
        KeyModifiers as CtKeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    use super::modal_wheel_delta;

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
