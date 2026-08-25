//! Export modal adapter, covering the built-in HTML exporter and every
//! user-configured `[[export.custom]]` entry through one modal.
//!
//! Bridges the UI-only [`ExportState`] form to the App: it persists the
//! chosen options to `config.toml`, runs the export preflight, spawns the
//! background render worker, and routes the success-phase buttons to the
//! OS opener.  The phase machine itself lives on the state; this adapter
//! only supplies the side effects each [`ExportResponse`] implies.
//!
//! **The format is chosen inside the modal, so this adapter holds one
//! [`ExportJob`] per format.**  The modal opens with a single `Export…`
//! palette entry; its Format list offers HTML plus every valid
//! `[[export.custom]]` converter.  The state carries the display side (a
//! parallel `Vec<ExportFormat>` and the selected index); this adapter
//! carries the matching `Vec<ExportJob>` in the *same order*, and reads
//! `state.format_idx` to know which one to run.  A custom export renders
//! the same HTML, from the same options form, with the same overwrite and
//! completion handshake — it just hands the file to the user's converter
//! at the end.
//!
//! **The jobs own *clones* of their `CustomExportEntry`, not indices into
//! `config.export.custom`.**  Returning from the external editor reloads
//! config wholesale, so an index captured when the modal opened can name
//! a different entry — or none — by the time the user presses
//! `[ Export ]`.  Resolving once, at open time, means the flow always
//! runs the command the chosen format promised.
//!
//! The completion handshake is asynchronous: `begin_export` claims an
//! export-generation id and hands the worker a closure that sends
//! [`AppEvent::ExportDone`] (carrying that id) back to the run loop.
//! [`App::handle_export_done`] finds the still-open modal via
//! `ModalStack::find_first_mut` and, only when its id matches, calls
//! [`ExportModal::on_export_done`] to flip it to its Success / Error
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
use crate::config::{Config, CustomExportEntry};
use crate::export::{self, HtmlExportOptions, PreflightError, Stylesheet};
use crate::ui::{ExportChoices, ExportFormat, ExportResponse, ExportState, ExportView, ModalKind};

/// Monotonic source of export-generation ids.  Each `begin_export` claims
/// the next id so a completion event from a superseded export (the modal was
/// dismissed and reopened while the worker ran) can be told apart from the
/// live one — see [`App::handle_export_done`].
static EXPORT_SEQ: AtomicU64 = AtomicU64::new(1);

/// One exporter the modal can run — parallel to a [`ExportFormat`] in the
/// state's Format list (same index).
#[derive(Debug, Clone)]
pub enum ExportJob {
    /// The built-in HTML exporter — the rendered HTML *is* the output.
    Html,
    /// A `[[export.custom]]` entry: render HTML to a temp file, then run
    /// the entry's command over it.  Resolved from config when the modal
    /// opens and owned from then on (see the module docs).
    Custom(CustomExportEntry),
}

impl ExportJob {
    /// Extension of the file this job writes, without a leading dot.  The
    /// custom arm goes through [`CustomExportEntry::output_extension`], so
    /// the configured `" pdf "` / `".pdf"` and the filename `guide.pdf`
    /// agree — the config validator only *reports* a malformed extension,
    /// it does not rewrite the stored value.
    fn extension(&self) -> &str {
        match self {
            ExportJob::Html => "html",
            ExportJob::Custom(entry) => entry.output_extension(),
        }
    }

    /// The Format-list row this job presents in the modal.
    fn format(&self) -> ExportFormat {
        match self {
            ExportJob::Html => ExportFormat::html(),
            ExportJob::Custom(entry) => ExportFormat::custom(&entry.name),
        }
    }
}

pub struct ExportModal {
    state: ExportState,
    /// One job per Format-list row, in the same order as
    /// [`ExportState::formats`]; the chosen one is `jobs[state.format_idx]`.
    jobs: Vec<ExportJob>,
    /// Generation id of this modal's in-flight export (`0` before any
    /// export starts).  Matched against the id carried by
    /// [`AppEvent::ExportDone`].
    export_id: u64,
}

impl ExportModal {
    pub fn new(state: ExportState, jobs: Vec<ExportJob>) -> Self {
        Self {
            state,
            jobs,
            export_id: 0,
        }
    }

    /// The job for the format currently selected in the Format list.
    /// `format_idx` is always a valid index (the state clamps it and the
    /// two lists are built together), but fall back to HTML defensively.
    fn selected_job(&self) -> ExportJob {
        self.jobs
            .get(self.state.format_idx)
            .cloned()
            .unwrap_or(ExportJob::Html)
    }

    /// Advance the modal once the background export finishes.  Called from
    /// the run loop's [`AppEvent::ExportDone`] arm.
    pub fn on_export_done(&mut self, outcome: export::ExportOutcome) {
        match outcome {
            Ok(path) => self.state.set_success(path),
            Err(message) => self.state.set_error(message),
        }
    }

    /// Map a state-level [`ExportResponse`] to a [`ModalOutcome`],
    /// running its App-side effects.  Shared by the key and click paths so a
    /// mouse click on a control / button behaves exactly like its keystroke.
    fn resolve(&mut self, app: &mut App, response: ExportResponse) -> ModalOutcome {
        match response {
            ExportResponse::Continue => ModalOutcome::Continue,
            ExportResponse::Cancelled => ModalOutcome::Close,
            ExportResponse::Submit(choices) => {
                self.submit(app, choices);
                ModalOutcome::Continue
            }
            ExportResponse::ProceedOverwrite => {
                self.proceed_overwrite(app);
                ModalOutcome::Continue
            }
            ExportResponse::OpenResult => {
                if let Some(path) = &self.state.result_path {
                    app.spawn_open_worker(path.display().to_string());
                }
                ModalOutcome::Continue
            }
            ExportResponse::OpenFolder => {
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
        let job = self.selected_job();
        let target = export::target_for_source(&source, job.extension());
        // An export target that *is* the open document is refused outright,
        // never offered as an overwrite.  `target_for_source` only swaps the
        // extension, so a converter configured with the document's own
        // (`extension = "md"`) resolves to the source itself — and the
        // overwrite prompt that would follow is the same one every export
        // shows, so confirming it destroys the document the user is editing.
        // `config_problem` cannot catch this: it sees the entry, not the
        // file that happens to be open.
        if is_same_file(&target, &source) {
            self.state.set_error(format!(
                "\"{}\" would overwrite the document itself. \
                 Give the {} export a different `extension` in config.toml.",
                target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| target.display().to_string()),
                job.format().label,
            ));
            return;
        }
        match export::preflight(&target, false) {
            Ok(()) => self.begin_export(app, target),
            Err(PreflightError::TargetExists(_)) => self.state.enter_confirm_overwrite(target),
            Err(e) => self.state.set_error(e.to_string()),
        }
    }

    /// Spawn the selected format's worker for `target` and enter the
    /// Exporting phase.  Reads the just-persisted toggle / stylesheet
    /// choices off `app.config`; the title comes from the state so it
    /// survives an overwrite-confirm detour.  A custom job gets the *same*
    /// [`HtmlExportOptions`] the HTML exporter would have used — that HTML
    /// is its input, so every option on the form still applies.
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
            // The document's directory, resolved to an absolute path.  A
            // bare `target.parent()` is an *empty* path for a repo-root
            // file opened by a relative name (`edamame README.md`), which
            // broke image inlining and the custom-export working directory
            // alike; absolutizing the target first gives a real folder.
            source_dir: std::path::absolute(&target)
                .ok()
                .and_then(|t| t.parent().map(Path::to_path_buf)),
            title: self.state.submitted_title.clone(),
            render_diagrams: html.diagrams,
        };
        let markdown = app.editor.buffer.contents();
        let id = EXPORT_SEQ.fetch_add(1, Ordering::Relaxed);
        self.export_id = id;
        self.state.enter_exporting(target.clone());
        // Both workers report through the same `ExportDone` event, so the
        // completion handshake below — and `handle_export_done`'s
        // superseded-result guard — is blind to which one ran.
        let done = move |outcome| {
            let _ = tx.send(AppEvent::ExportDone(id, outcome));
        };
        match self.selected_job() {
            ExportJob::Html => export::spawn_html_export(markdown, target, opts, done),
            ExportJob::Custom(entry) => {
                export::spawn_custom_export(entry, markdown, target, opts, done)
            }
        }
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

impl Modal for ExportModal {
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &ModalRenderCtx<'_>) {
        let view = ExportView {
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

    fn handle_wheel(&mut self, delta: i32) {
        self.state.handle_wheel(delta);
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
    /// Open the export options modal.
    ///
    /// Builds one [`ExportJob`] per format — HTML first, then every valid
    /// `[[export.custom]]` converter (the ones the config validator did not
    /// flag via [`CustomExportEntry::config_problem`]) — cloned from config
    /// *now*, so a later reload can't change what the chosen format runs.
    /// The state's Format list mirrors these jobs one-for-one.
    ///
    /// Seeded from `[export.html]` config: the title from the document's
    /// first H1 (falling back to the file stem), and the stylesheet pill
    /// from `Default` (the compiled-in `builtin` stylesheet) plus every
    /// `.css` discovered in `<config_dir>/export/`.  Those HTML options
    /// apply to every format, because the HTML is what a custom command
    /// converts.
    pub fn open_export_modal(&mut self) {
        // HTML always first; then each usable custom converter, cloned so
        // the modal is immune to a config reload.
        let mut jobs = vec![ExportJob::Html];
        jobs.extend(
            self.config
                .export
                .custom
                .iter()
                .filter(|e| e.config_problem().is_none())
                .cloned()
                .map(ExportJob::Custom),
        );
        let formats: Vec<ExportFormat> = jobs.iter().map(ExportJob::format).collect();

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

        let state = ExportState::new(
            formats,
            title,
            html.inline_images,
            html.diagrams,
            stylesheets,
            idx.unwrap_or(0),
        );
        self.modal_stack
            .push(Box::new(ExportModal::new(state, jobs)));
        self.needs_draw = true;
    }

    /// Surface an [`AppEvent::ExportDone`] outcome.  Routes to the modal
    /// that spawned this export (id match) when it is still open; otherwise
    /// — the user dismissed it, or reopened a fresh one while the worker ran
    /// — flashes the result on the hint line instead of hijacking the new
    /// modal.
    pub(in crate::app) fn handle_export_done(&mut self, id: u64, outcome: export::ExportOutcome) {
        match self.modal_stack.find_first_mut::<ExportModal>() {
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

/// Whether `a` and `b` name the same file on disk.
///
/// Plain equality answers it for the case this guards — `target` is
/// `source` with its extension swapped, so an extension matching the
/// document's produces a byte-identical path.  The canonicalized
/// comparison behind it covers a case-insensitive filesystem (macOS,
/// Windows), where `Guide.MD` and a `"md"` extension differ as strings and
/// are one file to the OS.  It runs only when both paths resolve, which in
/// the dangerous case they do: the target existing *is* the collision.
fn is_same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
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
    use crate::app::test_utils::make_app;

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
        let state = ExportState::new(
            vec![ExportFormat::html()],
            String::new(),
            false,
            true,
            vec![],
            0,
        );
        let modal = ExportModal::new(state, vec![ExportJob::Html]);
        assert_eq!(modal.export_id, 0);
        assert!(EXPORT_SEQ.load(Ordering::Relaxed) >= 1);
    }

    /// The extension is the job's, not a hardcoded `"html"` — it is what
    /// `target_for_source` writes to, so getting it from the wrong place
    /// means a custom export silently overwrites the HTML one.
    #[test]
    fn the_output_extension_comes_from_the_job() {
        assert_eq!(ExportJob::Html.extension(), "html");
        assert_eq!(
            ExportJob::Custom(entry("PDF (weasyprint)", "pdf")).extension(),
            "pdf"
        );
    }

    /// The Format-list row names the entry the user configured, so two
    /// converters are distinguishable in the list.
    #[test]
    fn the_format_row_names_the_entry() {
        let format = ExportJob::Custom(entry("PDF (weasyprint)", "pdf")).format();
        assert_eq!(format.label, "PDF (weasyprint)");
        assert_eq!(format.open_result, "Open file");
        assert_eq!(ExportJob::Html.format().label, "HTML");
        assert_eq!(ExportJob::Html.format().open_result, "Open in browser");
    }

    fn entry(name: &str, extension: &str) -> CustomExportEntry {
        CustomExportEntry {
            name: name.to_owned(),
            command: vec!["true".to_owned()],
            extension: extension.to_owned(),
        }
    }

    /// The modal lists HTML first, then every configured converter, and
    /// carries each job by *value* — so a config reload landing mid-modal
    /// (returning from `$EDITOR` replaces `config` wholesale) can't change
    /// what the chosen format runs.
    #[test]
    fn open_builds_a_job_per_format_by_value() {
        let mut app = make_app();
        app.config.export.custom = vec![entry("PDF", "pdf"), entry("DOCX", "docx")];
        app.open_export_modal();

        let modal = app
            .modal_stack
            .find_first_mut::<ExportModal>()
            .expect("the export modal should be open");
        // HTML first, then the two customs in order.
        assert!(matches!(modal.jobs[0], ExportJob::Html));
        let labels: Vec<String> = modal
            .state
            .formats
            .iter()
            .map(|f| f.label.clone())
            .collect();
        assert_eq!(labels, vec!["HTML", "PDF", "DOCX"]);
        assert_eq!(modal.state.format_idx, 0, "HTML selected by default");

        // Config changing underneath must not reach the open modal.
        app.config.export.custom.clear();
        let modal = app.modal_stack.find_first_mut::<ExportModal>().unwrap();
        match &modal.jobs[2] {
            ExportJob::Custom(e) => assert_eq!(e.extension, "docx"),
            other => panic!("expected the custom job, got {other:?}"),
        }
    }

    /// A custom entry the config validator flagged is left out of the
    /// Format list (and the parallel jobs), so a broken converter never
    /// becomes a selectable option that fails after submit.
    #[test]
    fn a_flagged_custom_entry_is_not_offered() {
        let mut app = make_app();
        app.config.export.custom = vec![
            entry("PDF", "pdf"),
            CustomExportEntry {
                name: "broken".to_owned(),
                command: vec![], // empty command → config_problem
                extension: "docx".to_owned(),
            },
        ];
        app.open_export_modal();
        let modal = app.modal_stack.find_first_mut::<ExportModal>().unwrap();
        let labels: Vec<String> = modal
            .state
            .formats
            .iter()
            .map(|f| f.label.clone())
            .collect();
        assert_eq!(labels, vec!["HTML", "PDF"], "the broken entry is excluded");
        assert_eq!(modal.jobs.len(), 2);
    }

    /// A converter configured with the document's own extension resolves
    /// its target to the document itself.  That must be refused at submit,
    /// not offered as an overwrite: the confirm prompt is the same one
    /// every export shows, so answering it the usual way would hand the
    /// user's live Markdown to the converter as the output file.
    #[test]
    fn a_converter_whose_extension_matches_the_document_is_refused() {
        let _guard = crate::test_env::config_isolation();
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("guide.md");
        std::fs::write(&source, "# Guide\n").unwrap();

        let mut app = make_app();
        app.file_path = Some(source.clone());
        app.config.export.custom = vec![entry("Markdown", "md")];
        app.open_export_modal();

        let mut modal = app.modal_stack.pop().expect("modal was pushed");
        let export = modal
            .as_any_mut()
            .downcast_mut::<ExportModal>()
            .expect("an ExportModal");
        export.state.format_idx = 1; // the "md" converter
        export.resolve(
            &mut app,
            ExportResponse::Submit(ExportChoices {
                title: None,
                inline_images: false,
                render_diagrams: true,
                stylesheet: "builtin".to_owned(),
            }),
        );

        assert_eq!(
            export.state.phase,
            crate::ui::ExportPhase::Error,
            "the collision must stop the flow, not reach ConfirmOverwrite"
        );
        assert!(
            export
                .state
                .error_message
                .as_deref()
                .unwrap_or_default()
                .contains("overwrite the document itself"),
            "got {:?}",
            export.state.error_message
        );
        // The document is untouched and no export target was stashed.
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "# Guide\n");
        assert!(export.state.target.is_none());
    }

    /// A leading dot / stray whitespace is normalized before the target is
    /// resolved, so `" .md "` collides exactly like `"md"` — the guard sits
    /// behind `output_extension`, not in front of it.
    #[test]
    fn the_collision_guard_sees_through_extension_normalization() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("guide.md");
        let job = ExportJob::Custom(entry("Markdown", " .md "));
        let target = crate::export::target_for_source(&source, job.extension());
        assert!(is_same_file(&target, &source));
    }

    /// End-to-end through the real submit path: with a custom format
    /// selected, the target the flow resolves carries the *format's*
    /// extension, not `"html"`.
    ///
    /// Observed at the overwrite-confirm pivot because that is the last
    /// step before the worker spawns, and it stashes the resolved target;
    /// the success path continues into `begin_export`, which a headless
    /// `App` has no event channel for.
    #[test]
    fn a_selected_custom_format_resolves_a_target_with_its_own_extension() {
        let _guard = crate::test_env::config_isolation();
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("guide.md");
        std::fs::write(&source, "# Guide\n").unwrap();
        // Pre-create the target so the flow stops at ConfirmOverwrite
        // rather than spawning a worker.
        std::fs::write(dir.path().join("guide.pdf"), b"old").unwrap();

        let mut app = make_app();
        app.file_path = Some(source);
        app.config.export.custom = vec![entry("PDF", "pdf")];
        app.open_export_modal();

        let mut modal = app.modal_stack.pop().expect("modal was pushed");
        let export = modal
            .as_any_mut()
            .downcast_mut::<ExportModal>()
            .expect("an ExportModal");
        // Select the PDF format (index 1; HTML is 0).
        export.state.format_idx = 1;
        export.resolve(
            &mut app,
            ExportResponse::Submit(ExportChoices {
                title: None,
                inline_images: false,
                render_diagrams: true,
                stylesheet: "builtin".to_owned(),
            }),
        );

        assert_eq!(
            export.state.target.as_deref(),
            Some(dir.path().join("guide.pdf").as_path()),
            "the target should sit beside the document under the format's extension"
        );
    }
}
