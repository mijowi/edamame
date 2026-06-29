//! "Export HTML" modal adapter.
//!
//! Bridges the UI-only [`ExportHtmlState`] form to the App: it persists the
//! chosen options to `config.toml`, runs the export preflight, spawns the
//! background render worker, and routes the success-phase buttons to the
//! OS opener.  The phase machine itself lives on the state; this adapter
//! only supplies the side effects each [`ExportHtmlResponse`] implies.
//!
//! The completion handshake is asynchronous: `begin_export` claims an
//! export-generation id and hands `spawn_html_export` a closure that sends
//! [`AppEvent::ExportDone`] (carrying that id) back to the run loop.
//! [`App::handle_export_done`] finds the still-open modal via
//! `ModalStack::find_first_mut` and, only when its id matches, calls
//! [`ExportHtmlModal::on_export_done`] to flip it to its Success / Error
//! phase; a result from a superseded export (dismissed-then-reopened) is
//! flashed on the hint line instead.

use std::any::Any;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, AppEvent};
use crate::config::Config;
use crate::export::{self, HtmlExportOptions, PreflightError, Stylesheet};
use crate::ui::{ExportChoices, ExportHtmlResponse, ExportHtmlState, ExportHtmlView, ModalKind};

/// Monotonic source of export-generation ids.  Each `begin_export` claims
/// the next id so a completion event from a superseded export (the modal was
/// dismissed and reopened while the worker ran) can be told apart from the
/// live one — see [`App::handle_export_done`].
static EXPORT_SEQ: AtomicU64 = AtomicU64::new(1);

pub struct ExportHtmlModal {
    state: ExportHtmlState,
    /// Generation id of this modal's in-flight export (`0` before any
    /// export starts).  Matched against the id carried by
    /// [`AppEvent::ExportDone`].
    export_id: u64,
}

impl ExportHtmlModal {
    pub fn new(state: ExportHtmlState) -> Self {
        Self {
            state,
            export_id: 0,
        }
    }

    /// Advance the modal once the background export finishes.  Called from
    /// the run loop's [`AppEvent::ExportDone`] arm.
    pub fn on_export_done(&mut self, outcome: export::ExportOutcome) {
        match outcome {
            Ok(path) => self.state.set_success(path),
            Err(message) => self.state.set_error(message),
        }
    }

    /// Map a state-level [`ExportHtmlResponse`] to a [`ModalOutcome`],
    /// running its App-side effects.  Shared by the key and click paths so a
    /// mouse click on a control / button behaves exactly like its keystroke.
    fn resolve(&mut self, app: &mut App, response: ExportHtmlResponse) -> ModalOutcome {
        match response {
            ExportHtmlResponse::Continue => ModalOutcome::Continue,
            ExportHtmlResponse::Cancelled => ModalOutcome::Close,
            ExportHtmlResponse::Submit(choices) => {
                self.submit(app, choices);
                ModalOutcome::Continue
            }
            ExportHtmlResponse::ProceedOverwrite => {
                self.proceed_overwrite(app);
                ModalOutcome::Continue
            }
            ExportHtmlResponse::OpenInBrowser => {
                if let Some(path) = &self.state.result_path {
                    app.spawn_open_worker(path.display().to_string());
                }
                ModalOutcome::Continue
            }
            ExportHtmlResponse::OpenFolder => {
                if let Some(dir) = self.state.result_path.as_deref().and_then(Path::parent) {
                    app.spawn_open_worker(dir.display().to_string());
                }
                ModalOutcome::Continue
            }
        }
    }

    /// Persist the chosen options, then either start the export or pivot to
    /// the overwrite-confirm phase.  The title is per-document and stays on
    /// the state (`submitted_title`); the rest is written to config so the
    /// choices become next time's defaults.
    fn submit(&mut self, app: &mut App, choices: ExportChoices) {
        app.config.export.html.inline_images = choices.inline_images;
        app.config.export.html.diagrams = choices.render_diagrams;
        app.config.export.html.stylesheet = choices.stylesheet;
        app.save_config_with_flash("failed to persist export options");

        let Some(source) = app.file_path.clone() else {
            self.state
                .set_error("Save the document to a file before exporting.".to_owned());
            return;
        };
        let target = export::target_for_source(&source, "html");
        match export::preflight(&target, false) {
            Ok(()) => self.begin_export(app, target),
            Err(PreflightError::TargetExists(_)) => self.state.enter_confirm_overwrite(target),
            Err(e) => self.state.set_error(e.to_string()),
        }
    }

    /// Spawn the render worker for `target` and enter the Exporting phase.
    /// Reads the just-persisted toggle / stylesheet choices off
    /// `app.config`; the title comes from the state so it survives an
    /// overwrite-confirm detour.
    fn begin_export(&mut self, app: &mut App, target: std::path::PathBuf) {
        let Some(tx) = app.app_tx.clone() else {
            self.state
                .set_error("Internal error: no event channel.".to_owned());
            return;
        };
        let html = &app.config.export.html;
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::from_config_value(&html.stylesheet),
            inline_images: html.inline_images,
            source_dir: target.parent().map(Path::to_path_buf),
            title: self.state.submitted_title.clone(),
            render_diagrams: html.diagrams,
        };
        let markdown = app.editor.buffer.contents();
        let id = EXPORT_SEQ.fetch_add(1, Ordering::Relaxed);
        self.export_id = id;
        self.state.enter_exporting(target.clone());
        export::spawn_html_export(markdown, target, opts, move |outcome| {
            let _ = tx.send(AppEvent::ExportDone(id, outcome));
        });
    }

    /// Confirm-overwrite "Overwrite" pressed: re-run the export against the
    /// stashed target, forcing the write.
    fn proceed_overwrite(&mut self, app: &mut App) {
        let Some(target) = self.state.target.clone() else {
            self.state
                .set_error("Internal error: no export target.".to_owned());
            return;
        };
        self.begin_export(app, target);
    }
}

impl Modal for ExportHtmlModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = ExportHtmlView {
            theme: ctx.theme,
            cursor_visible: ctx.cursor_visible,
        };
        frame.render_stateful_widget(view, area, &mut self.state);
    }

    fn handle_key(
        &mut self,
        key: KeyEvent,
        app: &mut App,
        _doc_height: usize,
        _doc_width: usize,
    ) -> ModalOutcome {
        let response = self.state.handle_key(&key);
        self.resolve(app, response)
    }

    fn handle_paste(&mut self, text: &str) -> ModalOutcome {
        self.state.paste(text);
        ModalOutcome::Continue
    }

    fn handle_click(&mut self, col: u16, row: u16, app: &mut App) -> ModalOutcome {
        let response = self.state.handle_click(col, row);
        self.resolve(app, response)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl App {
    /// Open the Export HTML options modal, seeded from `[export.html]`
    /// config: the title from the document's first H1 (falling back to the
    /// file stem), and the stylesheet pill from `Default` (the compiled-in
    /// `builtin` stylesheet) plus every `.css` discovered in
    /// `<config_dir>/export/`.
    pub fn open_export_html_modal(&mut self) {
        let markdown = self.editor.buffer.contents();
        let title = first_h1(&markdown)
            .or_else(|| {
                self.file_path
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .unwrap_or_default();

        let html = &self.config.export.html;
        // Display label is "Default"; the `builtin` value is the sentinel
        // `Stylesheet::from_config_value` maps to the compiled-in stylesheet.
        let mut stylesheets: Vec<(String, String)> =
            vec![("Default".to_owned(), "builtin".to_owned())];
        if let Some(dir) = Config::config_dir() {
            for path in crate::config::list_export_stylesheets(&dir) {
                let label = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                stylesheets.push((label, path.display().to_string()));
            }
        }

        // Select the configured stylesheet.  The `builtin` sentinel is the
        // first entry; a custom path that isn't in the export folder is
        // appended so the current setting is always representable.
        let current = if html.stylesheet.eq_ignore_ascii_case("builtin") {
            "builtin".to_owned()
        } else {
            html.stylesheet.clone()
        };
        let mut idx = stylesheets.iter().position(|(_, v)| *v == current);
        if idx.is_none() && current != "builtin" {
            let label = Path::new(&current)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| current.clone());
            stylesheets.push((label, current));
            idx = Some(stylesheets.len() - 1);
        }

        let state = ExportHtmlState::new(
            title,
            html.inline_images,
            html.diagrams,
            stylesheets,
            idx.unwrap_or(0),
        );
        self.modal_stack.push(Box::new(ExportHtmlModal::new(state)));
        self.needs_draw = true;
    }

    /// Surface an [`AppEvent::ExportDone`] outcome.  Routes to the modal
    /// that spawned this export (id match) when it is still open; otherwise
    /// — the user dismissed it, or reopened a fresh one while the worker ran
    /// — flashes the result on the hint line instead of hijacking the new
    /// modal.
    pub(in crate::app) fn handle_export_done(&mut self, id: u64, outcome: export::ExportOutcome) {
        match self.modal_stack.find_first_mut::<ExportHtmlModal>() {
            Some(modal) if modal.export_id == id => modal.on_export_done(outcome),
            _ => match outcome {
                Ok(path) => self.flash(
                    format!("Exported to {}", path.display()),
                    crate::app::MessageKind::Success,
                ),
                Err(message) => self.notify(format!("Export failed: {message}"), ModalKind::Error),
            },
        }
        self.needs_draw = true;
    }
}

/// First ATX H1 (`# Heading`) in `markdown`, trimmed; `None` if absent.
/// Only a real `# ` matches — `## ` and deeper are skipped.
fn first_h1(markdown: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        line.trim_start()
            .strip_prefix("# ")
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_h1_picks_the_first_level_one_heading() {
        assert_eq!(first_h1("# Title\n\nbody"), Some("Title".to_owned()));
        assert_eq!(
            first_h1("intro\n\n#  Spaced  \nmore"),
            Some("Spaced".to_owned())
        );
    }

    #[test]
    fn first_h1_ignores_deeper_headings_and_blanks() {
        assert_eq!(first_h1("## Sub\n### Deep"), None);
        assert_eq!(first_h1("#\n#   \nnope"), None);
        assert_eq!(first_h1(""), None);
    }

    #[test]
    fn fresh_modal_has_no_export_generation() {
        // A reopened modal starts at id 0, so a completion event from a
        // superseded export (id >= 1, claimed in `begin_export`) can never
        // match it — `handle_export_done` routes that result to a flash
        // instead of hijacking the new form.  Guards the dismiss-then-reopen
        // race.
        let state = ExportHtmlState::new(String::new(), false, true, vec![], 0);
        let modal = ExportHtmlModal::new(state);
        assert_eq!(modal.export_id, 0);
        assert!(EXPORT_SEQ.load(Ordering::Relaxed) >= 1);
    }
}
