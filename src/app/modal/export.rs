//! Export modal adapter, covering the built-in HTML exporter and every user-configured
//! `[[export.custom]]` entry through one modal.
//!
//! Bridges the UI-only [`ExportState`] form to the App: persisting options, the preflight, the
//! background worker, and the success-phase buttons.  The phase machine lives on the state; this
//! only supplies the side effects each [`ExportResponse`] implies.
//!
//! The format is chosen inside the modal, so this adapter holds one [`ExportJob`] per format, in
//! the *same order* as the state's `formats`, selected by `state.format_idx`.  A custom export
//! renders the same HTML from the same form and merely hands the file to the user's converter.
//!
//! **The jobs own *clones* of their `CustomExportEntry`, not indices into
//! `config.export.custom`.**  Returning from the external editor reloads config wholesale, so an
//! index captured at open time could name a different entry — or none — by the time the user
//! presses `[ Export ]`.
//!
//! The completion handshake is asynchronous: `begin_export` claims a generation id and the worker
//! sends [`AppEvent::ExportDone`] carrying it.  [`App::handle_export_done`] advances the modal only
//! when the id still matches; a superseded result is flashed on the hint line instead.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use super::types::{Modal, ModalOutcome, ModalRenderCtx};
use crate::app::{App, AppEvent};
use crate::config::{Config, CustomExportEntry};
use crate::export::{self, HtmlExportOptions, PreflightError, Stylesheet};
use crate::ui::{ExportChoices, ExportFormat, ExportResponse, ExportState, ExportView, ModalKind};

/// Monotonic export-generation ids, so a completion event from a dismissed-then-reopened export
/// can be told from the live one — see [`App::handle_export_done`].
static EXPORT_SEQ: AtomicU64 = AtomicU64::new(1);

/// One exporter the modal can run, at the same index as its [`ExportFormat`] row.
#[derive(Debug, Clone)]
pub enum ExportJob {
    /// The built-in exporter — the rendered HTML *is* the output.
    Html,
    /// Render HTML to a temp file, then run the entry's command over it.  Owned by value from open
    /// time onward; see the module docs.
    Custom(CustomExportEntry),
}

impl ExportJob {
    /// Extension this job writes, without a leading dot.  The custom arm normalizes through
    /// [`CustomExportEntry::output_extension`] because the config validator only *reports* a
    /// malformed `" pdf "` / `".pdf"`, it does not rewrite the stored value.
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
    /// One job per Format-list row, in [`ExportState::formats`] order.
    jobs: Vec<ExportJob>,
    /// Generation id of the in-flight export; `0` before any export starts.
    export_id: u64,
    /// Out-of-folder images the user agreed to embed in this submission; cleared on each submit.
    approved_outside: Vec<PathBuf>,
}

impl ExportModal {
    pub fn new(state: ExportState, jobs: Vec<ExportJob>) -> Self {
        Self {
            state,
            jobs,
            export_id: 0,
            approved_outside: Vec::new(),
        }
    }

    /// The selected format's job.  `format_idx` is always valid; the HTML fallback is defensive.
    fn selected_job(&self) -> ExportJob {
        self.jobs
            .get(self.state.format_idx)
            .cloned()
            .unwrap_or(ExportJob::Html)
    }

    /// Advance the modal once the background export finishes.
    pub fn on_export_done(&mut self, outcome: export::ExportOutcome) {
        match outcome {
            Ok(path) => self.state.set_success(path),
            Err(message) => self.state.set_error(message),
        }
    }

    /// Map an [`ExportResponse`] to a [`ModalOutcome`], running its App-side effects.  Shared by
    /// the key and click paths so a click behaves exactly like its keystroke.
    fn resolve(&mut self, app: &mut App, response: ExportResponse) -> ModalOutcome {
        match response {
            ExportResponse::Continue => ModalOutcome::Continue,
            ExportResponse::Cancelled => ModalOutcome::Close,
            ExportResponse::Submit(choices) => {
                self.submit(app, choices);
                ModalOutcome::Continue
            }
            ExportResponse::EmbedOutsideImages(embed) => {
                self.approved_outside = if embed {
                    std::mem::take(&mut self.state.outside_images)
                } else {
                    Vec::new()
                };
                match self.state.target.clone() {
                    Some(target) => self.preflight_then_export(app, target),
                    None => self
                        .state
                        .set_error("Internal error: no export target.".to_owned()),
                }
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

    /// Persist the chosen options, then start the export or pivot to a confirmation: first for
    /// images outside the document's folder, then for an existing target.  The title is
    /// per-document and stays on the state; the rest becomes next time's defaults.
    fn submit(&mut self, app: &mut App, choices: ExportChoices) {
        app.config.export.html.inline_images = choices.inline_images;
        app.config.export.html.figures = choices.render_figures;
        app.config.export.html.stylesheet = choices.stylesheet;
        app.save_config_with_flash("failed to persist export options");

        let Some(source) = app.file_path.clone() else {
            self.state
                .set_error("Save the document to a file before exporting.".to_owned());
            return;
        };
        let job = self.selected_job();
        let target = export::target_for_source(&source, job.extension());
        // A target that *is* the open document is refused outright, never offered as an
        // overwrite: `target_for_source` only swaps the extension, so a converter configured with
        // `extension = "md"` resolves to the source, and the ordinary overwrite prompt would then
        // destroy the document being edited.  `config_problem` can't catch this — it sees the
        // entry, not the file that happens to be open.
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
        self.approved_outside.clear();
        // Inlining reads every referenced image into a file the user will likely share, so one
        // from outside the document's folder is embedded only once the user has seen its path.
        if choices.inline_images {
            let outside = source_dir_for(&target)
                .map(|dir| export::outside_images(&app.editor.buffer.contents(), &dir))
                .unwrap_or_default();
            if !outside.is_empty() {
                self.state.enter_confirm_outside_images(target, outside);
                return;
            }
        }
        self.preflight_then_export(app, target);
    }

    /// Start the export, or pivot to overwrite-confirm when `target` already exists.
    fn preflight_then_export(&mut self, app: &mut App, target: PathBuf) {
        match export::preflight(&target, false) {
            Ok(()) => self.begin_export(app, target),
            Err(PreflightError::TargetExists(_)) => self.state.enter_confirm_overwrite(target),
            Err(e) => self.state.set_error(e.to_string()),
        }
    }

    /// Spawn the selected format's worker and enter the Exporting phase.  The title comes from
    /// the state so it survives an overwrite-confirm detour.  A custom job gets the *same*
    /// [`HtmlExportOptions`] the HTML exporter would: that HTML is its input.
    fn begin_export(&mut self, app: &mut App, target: PathBuf) {
        let Some(tx) = app.app_tx.clone() else {
            self.state
                .set_error("Internal error: no event channel.".to_owned());
            return;
        };
        let html = &app.config.export.html;
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::from_config_value(&html.stylesheet),
            inline_images: html.inline_images,
            source_dir: source_dir_for(&target),
            approved_outside: self.approved_outside.clone(),
            title: self.state.submitted_title.clone(),
            render_figures: html.figures,
        };
        let markdown = app.editor.buffer.contents();
        let id = EXPORT_SEQ.fetch_add(1, Ordering::Relaxed);
        self.export_id = id;
        self.state.enter_exporting(target.clone());
        // Both workers report through the same `ExportDone`, so the handshake is blind to which
        // one ran.
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

    /// "Overwrite" pressed: re-run the export against the stashed target, forcing the write.
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
    /// Builds one [`ExportJob`] per format — HTML first, then every custom converter
    /// [`CustomExportEntry::config_problem`] did not flag — cloned from config *now*, so a later
    /// reload can't change what the chosen format runs.
    ///
    /// Seeded from `[export.html]`: the title from the document's first H1 (else the file stem),
    /// and the stylesheet pill from `Default` plus every `.css` in `<config_dir>/export/`.  Those
    /// options apply to every format, since the HTML is what a custom command converts.
    pub fn open_export_modal(&mut self) {
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
        // `builtin` is the sentinel `Stylesheet::from_config_value` maps to the compiled-in sheet.
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

        // A configured path outside the export folder is appended, so the current setting is
        // always representable in the list.
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
            html.figures,
            stylesheets,
            idx.unwrap_or(0),
        );
        self.modal_stack
            .push(Box::new(ExportModal::new(state, jobs)));
        self.needs_draw = true;
    }

    /// Surface an [`AppEvent::ExportDone`] outcome: to the modal that spawned it when the id
    /// still matches, otherwise to the hint line rather than hijacking a newly-opened modal.
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

/// The directory image paths resolve against: the export target's, which is the document's.
/// Absolutized first: a bare `target.parent()` is the *empty* path for a file opened by a relative
/// name (`edamame README.md`), breaking image inlining and the custom-export working directory
/// alike.
fn source_dir_for(target: &Path) -> Option<PathBuf> {
    std::path::absolute(target)
        .ok()
        .and_then(|t| t.parent().map(Path::to_path_buf))
}

/// Whether `a` and `b` name the same file on disk.
///
/// Plain equality answers the common case (an extension swap yields a byte-identical path); the
/// canonicalized comparison behind it covers case-insensitive filesystems, where `Guide.MD` and a
/// `"md"` extension differ as strings but are one file.  It needs both paths to resolve, which in
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

/// First ATX H1 in `markdown`, trimmed; `## ` and deeper don't match.
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
        // Guards the dismiss-then-reopen race: id 0 can never match a claimed id (>= 1).
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

    /// The extension must be the job's: a hardcoded `"html"` would make a custom export silently
    /// overwrite the HTML one.
    #[test]
    fn the_output_extension_comes_from_the_job() {
        assert_eq!(ExportJob::Html.extension(), "html");
        assert_eq!(
            ExportJob::Custom(entry("PDF (weasyprint)", "pdf")).extension(),
            "pdf"
        );
    }

    /// The row names the configured entry, so two converters are distinguishable.
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

    /// Jobs are carried by *value*, so a config reload landing mid-modal (returning from
    /// `$EDITOR` replaces `config` wholesale) can't change what the chosen format runs.
    #[test]
    fn open_builds_a_job_per_format_by_value() {
        let mut app = make_app();
        app.config.export.custom = vec![entry("PDF", "pdf"), entry("DOCX", "docx")];
        app.open_export_modal();

        let modal = app
            .modal_stack
            .find_first_mut::<ExportModal>()
            .expect("the export modal should be open");
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

    /// A flagged entry is left out of both lists, so a broken converter is never selectable.
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

    /// A converter whose extension matches the document must be refused at submit, not offered as
    /// an overwrite: the usual confirm would hand the user's live Markdown to the converter.
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
                render_figures: true,
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
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "# Guide\n");
        assert!(export.state.target.is_none());
    }

    /// With inlining on, an image outside the document's folder stops the flow at a prompt listing
    /// it; the answer decides whether it is embedded, and the flow then carries on to the
    /// overwrite check.  With inlining off there is nothing to ask.
    #[test]
    fn an_outside_image_is_confirmed_before_the_export_runs() {
        let _guard = crate::test_env::config_isolation();
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join("docs");
        std::fs::create_dir(&docs).unwrap();
        let shared = root.path().join("shared.png");
        image::RgbaImage::new(1, 1).save(&shared).unwrap();
        let source = docs.join("guide.md");
        std::fs::write(&source, "![x](../shared.png)\n").unwrap();
        // Pre-create the target so the flow stops at ConfirmOverwrite instead of spawning.
        std::fs::write(docs.join("guide.html"), b"old").unwrap();

        let submit = |inline_images| {
            ExportResponse::Submit(ExportChoices {
                title: None,
                inline_images,
                render_figures: false,
                stylesheet: "builtin".to_owned(),
            })
        };
        for (embed, expected) in [
            (true, vec![shared.canonicalize().unwrap()]),
            (false, vec![]),
        ] {
            let mut app = make_app();
            app.file_path = Some(source.clone());
            app.editor.buffer = crate::document::Buffer::from_str("![x](../shared.png)\n");
            app.open_export_modal();
            let mut modal = app.modal_stack.pop().expect("modal was pushed");
            let export = modal.as_any_mut().downcast_mut::<ExportModal>().unwrap();

            export.resolve(&mut app, submit(true));
            assert_eq!(
                export.state.phase,
                crate::ui::ExportPhase::ConfirmOutsideImages
            );
            assert_eq!(
                export.state.outside_images,
                vec![shared.canonicalize().unwrap()]
            );

            export.resolve(&mut app, ExportResponse::EmbedOutsideImages(embed));
            assert_eq!(export.state.phase, crate::ui::ExportPhase::ConfirmOverwrite);
            assert_eq!(export.approved_outside, expected, "embed = {embed}");

            // A fresh submit forgets the earlier answer, and inlining off never asks.
            export.state.phase = crate::ui::ExportPhase::Options;
            export.resolve(&mut app, submit(false));
            assert_eq!(export.state.phase, crate::ui::ExportPhase::ConfirmOverwrite);
            assert!(export.approved_outside.is_empty());
        }
    }

    /// The guard sits behind `output_extension`, so `" .md "` collides exactly like `"md"`.
    #[test]
    fn the_collision_guard_sees_through_extension_normalization() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("guide.md");
        let job = ExportJob::Custom(entry("Markdown", " .md "));
        let target = crate::export::target_for_source(&source, job.extension());
        assert!(is_same_file(&target, &source));
    }

    /// End-to-end: a selected custom format resolves a target under its own extension.  Observed
    /// at the overwrite-confirm pivot, the last step that stashes a target before the worker
    /// spawns — a headless `App` has no event channel for `begin_export`.
    #[test]
    fn a_selected_custom_format_resolves_a_target_with_its_own_extension() {
        let _guard = crate::test_env::config_isolation();
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("guide.md");
        std::fs::write(&source, "# Guide\n").unwrap();
        // Pre-create the target so the flow stops at ConfirmOverwrite instead of spawning.
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
        export.state.format_idx = 1;
        export.resolve(
            &mut app,
            ExportResponse::Submit(ExportChoices {
                title: None,
                inline_images: false,
                render_figures: true,
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
