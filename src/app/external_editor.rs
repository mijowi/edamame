//! External-editor lifecycle extracted from `app.rs` in Step 2 of
//! `refactor-app.md`.
//!
//! Owns:
//! - [`ExternalEditorOutcome`] reporting whether the editor process
//!   actually ran.
//! - The shared suspend/resume dance in [`App::run_external_editor`].
//! - Two App entry points, one for `config.toml` and one for the
//!   current buffer's file, that wrap the shared helper with
//!   pre-launch save and post-exit reload semantics.
//! - [`App::spawn_open_worker`] for OS-handler fallbacks (URLs and
//!   non-Markdown local files).

use std::io::Stdout;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::modal;
use crate::config::{Config, KeyMap, Theme};
use crate::terminal::ColorDepth;

use super::flash::MessageKind;
use super::{App, AppEvent};

/// Result of [`App::run_external_editor`].  Tells the caller whether
/// the editor actually ran so it can decide whether a post-exit
/// reload is appropriate.
pub(super) enum ExternalEditorOutcome {
    /// `$VISUAL` / `$EDITOR` was unset; the path was handed to the
    /// OS handler via `open::that`.  No suspend happened.
    OsHandler,
    /// The TUI couldn't be suspended.  An error was already flashed.
    SuspendFailed,
    /// The editor process ran (or failed to launch) — here's the
    /// outcome.
    Exited(std::io::Result<std::process::ExitStatus>),
}

impl App {
    /// Open `config.toml` in the user's text editor and reload the
    /// config when the editor exits.  Prefers `$VISUAL` over
    /// `$EDITOR` (the modern shell convention); falls back to
    /// `open::that` (which delegates to the OS GUI handler) when
    /// neither variable is set.
    ///
    /// When a shell editor is invoked we need to surrender the
    /// terminal entirely: leave the alternate screen, drop raw mode,
    /// disable mouse capture, etc., so the editor can talk to the
    /// real TTY.  Once the editor exits we re-enter the TUI and
    /// force a full redraw.
    ///
    /// `terminal` is borrowed mutably so we can call
    /// [`Terminal::clear`] after re-entry — without this, ratatui's
    /// in-memory buffer thinks the screen still holds whatever it
    /// drew before suspension and skips redrawing unchanged cells.
    pub(super) fn open_config_in_editor(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        let Some(path) = Config::config_path() else {
            self.flash("No config directory available", MessageKind::Error);
            return;
        };
        // Make sure the file exists before we hand it to an editor
        // that might fail on a missing path.  `Config::save`
        // serialises the in-memory config — same content the user
        // would see if they navigated to the file via the file
        // manager.
        if !path.exists() {
            if let Err(e) = self.config.save() {
                tracing::warn!(error = %e, "failed to seed config.toml before editor launch");
                self.flash(format!("Config save failed: {e}"), MessageKind::Error);
                return;
            }
        }

        let outcome = self.run_external_editor(&path, terminal, rx);

        // Reload the config from disk so any edits the user made
        // are reflected in the running session.  Failures fall back
        // to the in-memory state with a warning — the user can
        // restart edamame to retry.  Run the reload regardless of
        // whether the editor actually launched: if we fell back to
        // the OS handler the user might still have edited the file.
        //
        // Any non-fatal warnings (parse error, unknown keys, invalid
        // keybinding entries) returned by `Config::load` are routed
        // into the same `ConfigWarningModal` we use at startup so the
        // user sees their typo as soon as they exit the editor.
        match Config::load() {
            Ok(loaded) => {
                self.config = loaded.config;
                self.keybindings = loaded.keybindings;
                // Rebuild the keymap so any keybinding edits take
                // effect for the next keystroke.
                match KeyMap::build(&self.keybindings) {
                    Ok(km) => self.keymap = Some(km),
                    Err(e) => {
                        tracing::warn!(error = %e, "rebuilt KeyMap failed after editor exit");
                    }
                }
                // Live-apply the theme so a `theme = "..."` edit in
                // the external editor takes effect without a
                // restart.  Uses the already-loaded `ThemeFile` so
                // we don't read the theme TOML twice.
                let monochrome = self.capabilities.color_depth == ColorDepth::NoColor;
                let new_theme: &'static Theme =
                    Box::leak(Box::new(Theme::from_file(&loaded.theme, monochrome)));
                self.theme = new_theme;
                self.editor.set_theme(new_theme);
                if let Some(modal) = modal::ConfigWarningModal::from_warnings(&loaded.warnings) {
                    self.modal_stack.push(Box::new(modal));
                    self.needs_draw = true;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to reload config after editor exit");
            }
        }

        match outcome {
            ExternalEditorOutcome::Exited(Ok(s)) if s.success() => {
                self.flash("Configuration updated", MessageKind::Warning);
            }
            ExternalEditorOutcome::Exited(Ok(s)) => {
                self.flash(format!("Editor exited {s}"), MessageKind::Warning);
            }
            ExternalEditorOutcome::Exited(Err(e)) => {
                self.flash(format!("Editor failed: {e}"), MessageKind::Error);
            }
            // Suspend failure / OS-handler fallback already flashed
            // their own status — no extra message here.
            ExternalEditorOutcome::SuspendFailed | ExternalEditorOutcome::OsHandler => {}
        }
    }

    /// Save the current buffer (best-effort) and open it in the
    /// user's `$VISUAL` / `$EDITOR`.  After the editor exits the
    /// buffer is reloaded from disk so external edits are picked up
    /// — without this, subsequent saves from edamame would silently
    /// overwrite work done in the other editor.  Falls back to the
    /// OS handler when no shell editor is set; same flow the
    /// settings overlay uses for `config.toml`.
    pub(super) fn open_current_file_in_editor(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        let Some(path) = self.editor.buffer.path().map(|p| p.to_path_buf()) else {
            self.flash("No file path for buffer", MessageKind::Error);
            return;
        };

        // Save first so the external editor sees the in-memory state.
        if self.editor.dirty {
            if let Err(e) = self.editor.buffer.save_file() {
                tracing::warn!(error = %e, "failed to save buffer before editor launch");
                self.flash(format!("Save failed: {e}"), MessageKind::Error);
                return;
            }
            self.editor.dirty = false;
        }

        let outcome = self.run_external_editor(&path, terminal, rx);

        // Reload the buffer from disk so any external edits are
        // reflected.  Skipped on suspend failure (terminal is in a
        // degraded state already) and on the OS-handler fallback
        // (the OS handler returns immediately and the user may not
        // have closed the file yet — reloading prematurely would
        // discard their in-edamame edits while they're still
        // working).
        if matches!(outcome, ExternalEditorOutcome::Exited(_)) {
            if let Err(e) = self.load_file_into_editor(path) {
                tracing::warn!(error = %e, "failed to reload buffer after editor exit");
                self.flash(format!("Reload failed: {e}"), MessageKind::Error);
                return;
            }
        }

        match outcome {
            ExternalEditorOutcome::Exited(Ok(s)) if s.success() => {
                self.flash("File reloaded", MessageKind::Success);
            }
            ExternalEditorOutcome::Exited(Ok(s)) => {
                self.flash(format!("Editor exited {s}"), MessageKind::Warning);
            }
            ExternalEditorOutcome::Exited(Err(e)) => {
                self.flash(format!("Editor failed: {e}"), MessageKind::Error);
            }
            ExternalEditorOutcome::SuspendFailed | ExternalEditorOutcome::OsHandler => {}
        }
    }

    /// Open a theme `.toml` in the user's `$VISUAL` / `$EDITOR`, then
    /// reload the active theme so any edits take effect immediately.
    /// Mirrors [`Self::open_config_in_editor`] but scoped to a single
    /// theme file — the success modal pushed after
    /// `Action::CreateCustomTheme` routes here.
    pub(super) fn open_theme_in_editor(
        &mut self,
        path: &Path,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) {
        if !path.exists() {
            self.flash(
                format!("Theme file no longer exists: {}", path.display()),
                MessageKind::Error,
            );
            return;
        }
        let outcome = self.run_external_editor(path, terminal, rx);

        match outcome {
            ExternalEditorOutcome::Exited(Ok(s)) if s.success() => {
                self.apply_active_theme();
                self.flash("Theme reloaded", MessageKind::Success);
            }
            ExternalEditorOutcome::Exited(Ok(s)) => {
                self.apply_active_theme();
                self.flash(format!("Editor exited {s}"), MessageKind::Warning);
            }
            ExternalEditorOutcome::Exited(Err(e)) => {
                self.flash(format!("Editor failed: {e}"), MessageKind::Error);
            }
            ExternalEditorOutcome::SuspendFailed | ExternalEditorOutcome::OsHandler => {}
        }
    }

    /// Suspend the TUI, run an external editor on `path`, and resume.
    /// Shared between the settings-overlay "Open config.toml" flow
    /// and the palette "Open current file in system editor" flow:
    /// both need the same read-thread / terminal dance around
    /// `Command::status()`.  The caller is responsible for any
    /// pre-launch save / post-exit reload — this helper only owns
    /// the suspend / resume window.
    pub(super) fn run_external_editor(
        &mut self,
        path: &Path,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        rx: &mpsc::Receiver<AppEvent>,
    ) -> ExternalEditorOutcome {
        let editor = std::env::var("VISUAL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                std::env::var("EDITOR")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            });

        let Some(editor) = editor else {
            // No shell editor — fall back to the OS handler.  This
            // is the same path Phase 8 link-following uses, so the
            // user sees consistent behaviour whether $EDITOR is set
            // or not.
            self.spawn_open_worker(path.display().to_string());
            self.flash("Opening with system default", MessageKind::Info);
            return ExternalEditorOutcome::OsHandler;
        };

        // Pause our crossterm read thread so the editor has
        // uncontested access to stdin.  Without this, our thread
        // and the editor both call `read()` on the same fd: bytes
        // get split between them, the editor sees corrupted input
        // (the `1;rgb:...` artifact users reported was the OSC 11
        // background-color response that neovim queried for, with
        // some bytes stolen by us), and keystrokes feel laggy
        // because half of them never reach the editor.
        if let Some(p) = self.read_paused.as_ref() {
            p.store(true, Ordering::Release);
        }
        // The poll loop wakes every 100 ms; sleep slightly longer
        // so the read thread is guaranteed to have entered the
        // paused branch before we hand stdin to the editor.
        std::thread::sleep(Duration::from_millis(120));
        // Discard any events that were already parsed during the
        // overlap window so they don't reach the editor (or
        // re-emerge in our channel after resume).
        while rx.try_recv().is_ok() {}

        // Suspend the TUI.  Best-effort: a failure here means the
        // editor would launch into a confused terminal state, so
        // bail out and tell the user.
        if let Err(e) = crate::terminal::restore() {
            tracing::warn!(error = %e, "failed to suspend terminal for editor");
            self.flash(format!("Editor failed: {e}"), MessageKind::Error);
            if let Some(p) = self.read_paused.as_ref() {
                p.store(false, Ordering::Release);
            }
            return ExternalEditorOutcome::SuspendFailed;
        }

        let status = std::process::Command::new(&editor).arg(path).status();

        // Always try to restore the TUI, even if the editor failed —
        // otherwise we strand the user in a half-suspended state.
        let mouse = self.capabilities.mouse;
        let kbd = self.capabilities.keyboard_enhancement;
        let restore_result = crate::terminal::re_enter(mouse, kbd);
        if let Err(e) = restore_result {
            tracing::error!(error = %e, "failed to re-enter TUI after editor");
            // We can still draw something, but the terminal is in
            // a degraded state.  Surface it loudly.
            self.flash(format!("Terminal restore failed: {e}"), MessageKind::Error);
        }
        // Some terminals emit acknowledgements for the re-enter
        // sequences (kitty keyboard, mouse mode).  Pause stays on
        // here so any such bytes flow into the kernel buffer
        // rather than racing with the read thread that's about to
        // resume.  After this short wait, drain the channel and
        // resume — the read thread will pick up anything still
        // pending on its first post-resume poll.
        std::thread::sleep(Duration::from_millis(30));
        while rx.try_recv().is_ok() {}
        if let Some(p) = self.read_paused.as_ref() {
            p.store(false, Ordering::Release);
        }

        // Ratatui caches the previous frame; clearing forces it to
        // redraw every cell on the next `terminal.draw` call.
        let _ = terminal.clear();
        self.needs_draw = true;

        ExternalEditorOutcome::Exited(status)
    }

    /// Spawn a worker thread that calls `open::that` and reports the
    /// outcome via `AppEvent::LinkOpenResult`.  Keeps the UI thread
    /// responsive — `xdg-open` can take several hundred milliseconds
    /// on some desktops.
    pub(super) fn spawn_open_worker(&self, target: String) {
        let Some(tx) = self.app_tx.clone() else {
            return;
        };
        std::thread::spawn(move || {
            let result = open::that(&target).map_err(|e| e.to_string());
            let _ = tx.send(AppEvent::LinkOpenResult(result));
        });
    }
}
