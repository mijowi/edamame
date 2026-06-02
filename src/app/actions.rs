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
use crate::config::{Action, Config, KeyBindingOverrides, KeyMap, Theme};
use crate::editor::{edit_ops, EditorState};
use crate::input::mode_handler::default::DefaultHandler;
use crate::input::ModeHandler;
use crate::terminal::ColorDepth;
use crate::ui::ModalKind;

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
pub(super) const NOT_YET_IMPLEMENTED: &[Action] =
    &[Action::Open, Action::ExportHtml, Action::ReloadFromDisk];

/// Default-deny gate over [`Action`]s in diff mode (Phase 1 §10).
/// Returns `Some(action)` when the action is allowed in Review
/// sub-mode (CP3 only ships Review); `None` for everything else.
/// CP5 will add a `(action, sub_mode)` signature so Edit-only and
/// Review-only actions can refine the gate.
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
            | DiffEnterEdit
            | DiffExitEdit
            | DiffExit
            | ScrollUp
            | ScrollDown
            | ScrollPageUp
            | ScrollPageDown
            | ScrollToTop
            | ScrollToBottom
            | SaveCopy
            | Quit
            | ShowCommandPalette
            | ShowMarkdownCheatSheet
            | OpenSettings
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
            Action::GoToSection => {
                self.open_section_picker(doc_width);
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
                    self.notify("Mouse not supported on this terminal", ModalKind::Error);
                }
                self.needs_draw = true;
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
            Action::SaveCopy => {
                self.open_save_copy_modal();
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

    /// Dispatch a resolved [`Action`] through the unified pipeline.
    /// Both the run-loop keystroke arm
    /// ([`super::App::dispatch_single_key`]) and the command palette
    /// (`CommandPaletteModal`) funnel through here so there is exactly
    /// one place where `handle_app_action`, the dirty-buffer quit
    /// guard, `edit_ops::apply`, scroll tracking, post-action flashes,
    /// and pending link-follow draining are sequenced.
    pub fn dispatch_action(&mut self, action: Action, doc_height: usize, doc_width: usize) {
        let handled = self.handle_app_action(&action, doc_height, doc_width);
        if !handled {
            if matches!(action, Action::Quit) && self.editor.dirty {
                self.open_quit_confirm();
                return;
            }
            // Diff mode owns its own dispatch.  `diff_safe_action`
            // filters everything that isn't on the diff-mode
            // allowlist (default-deny per §10); diff-control actions
            // and decision keys are dispatched through
            // `App::dispatch_diff_action`.
            if self.editor.mode == crate::editor::Mode::Diff {
                let Some(safe) = diff_safe_action(&action) else {
                    return;
                };
                self.dispatch_diff_action(safe, doc_height, doc_width);
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
        match action {
            Action::DiffNext => {
                if let Some(d) = self.editor.diff.as_mut() {
                    d.advance_focus();
                    self.needs_draw = true;
                }
            }
            Action::DiffPrev => {
                if let Some(d) = self.editor.diff.as_mut() {
                    d.retreat_focus();
                    self.needs_draw = true;
                }
            }
            Action::DiffAcceptHunk => {
                if let Some(d) = self.editor.diff.as_mut() {
                    d.set_focused_decision(Decision::Accepted);
                    self.needs_draw = true;
                }
                self.check_diff_resolution();
            }
            Action::DiffRejectHunk => {
                if let Some(d) = self.editor.diff.as_mut() {
                    d.set_focused_decision(Decision::Rejected);
                    self.needs_draw = true;
                }
                self.check_diff_resolution();
            }
            Action::DiffAcceptAll => {
                if let Some(d) = self.editor.diff.as_mut() {
                    d.bulk_decide_pending(Decision::Accepted);
                    self.needs_draw = true;
                }
                self.check_diff_resolution();
            }
            Action::DiffRejectAll => {
                if let Some(d) = self.editor.diff.as_mut() {
                    d.bulk_decide_pending(Decision::Rejected);
                    self.needs_draw = true;
                }
                self.check_diff_resolution();
            }
            Action::DiffEnterEdit | Action::DiffExitEdit => {
                // Edit sub-mode lands in CP5; until then `i` / Enter
                // and `Esc` (Edit→Review) are explicit no-ops.
                self.flash("Diff edit mode coming soon", MessageKind::Info);
            }
            Action::DiffExit => {
                // CP4 wires the proper exit-confirm flow.  CP3
                // discards in-progress decisions immediately so the
                // user can always escape diff mode.
                self.exit_diff_mode_discarding();
            }
            Action::ScrollUp => {
                if self.editor.scroll > 0 {
                    self.editor.scroll -= 1;
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
                let max = total.saturating_sub(doc_height.max(1));
                self.editor.scroll = max;
                self.mark_scrolling();
                self.needs_draw = true;
            }
            Action::SaveCopy => {
                self.open_save_copy_modal();
                self.needs_draw = true;
            }
            Action::Quit => {
                // CP3 simplification: quit while reviewing discards
                // the review.  CP4 adds the exit-confirm guard.
                self.exit_diff_mode_discarding();
                self.should_quit = true;
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
            Action::OpenSettings => {
                self.open_settings_overlay();
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

    /// If every hunk has been decided, push the resolve-confirm
    /// modal.  Called after every accept / reject / bulk action.
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
        if self.config.editor.show_diff_intro {
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

    /// Apply the merged result to the editor buffer and exit diff
    /// mode.  Called from the `[Apply]` button of
    /// [`crate::app::modal::DiffResolveConfirmModal`].  CP3 swaps the
    /// rope in place but does NOT record a merge-revert undo entry —
    /// that lands in CP4 alongside [`History::reset_with`].
    pub(crate) fn apply_diff_resolution(&mut self) {
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
        self.editor.buffer.set_rope(resolved);
        self.editor.cursor.offset = 0;
        self.editor.cursor.preferred_col = 0;
        self.editor.history = crate::document::History::new();
        self.editor.dirty = differs_from_disk;
        self.editor.refresh_parsed();
        self.editor.update_cursor_block();
        self.editor.exit_diff_mode();
        self.flash("Diff resolved", MessageKind::Success);
        self.needs_draw = true;
    }

    /// Exit diff mode without applying the merge.  Restores the
    /// pre-diff buffer state (the diff's `old_rope` already equals
    /// the editor's current buffer, so this is just a clean-up).
    pub(crate) fn exit_diff_mode_discarding(&mut self) {
        self.editor.exit_diff_mode();
        // Pop the resolve-confirm modal if it's on the stack.
        self.modal_stack
            .remove_first::<crate::app::modal::DiffResolveConfirmModal>();
        self.modal_stack
            .remove_first::<crate::app::modal::DiffIntroModal>();
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
    /// - future post-merge diff resolution (Phase 1 §6)
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
        self.modal_stack
            .push(Box::new(modal::KeybindsOverlayModal::new(
                &keymap, &overrides,
            )));
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
    use crossterm::event::{
        KeyModifiers as CtKeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    use super::modal_wheel_delta;
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
        let mut app = make_app();
        app.editor.buffer.insert(0, "alpha\nbeta\ngamma\n");
        app.enter_diff_mode("alpha\nBETA\ngamma\n".to_owned());
        // Dismiss the intro modal if it stacked.
        app.modal_stack
            .remove_first::<crate::app::modal::DiffIntroModal>();
        // Accept everything, then apply.
        app.dispatch_diff_action(crate::config::Action::DiffAcceptAll, 24, 80);
        app.apply_diff_resolution();
        assert_eq!(app.editor.mode, crate::editor::Mode::Rendered);
        assert!(app.editor.diff.is_none());
        assert_eq!(app.editor.buffer.contents(), "alpha\nBETA\ngamma\n");
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
