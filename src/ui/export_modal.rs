//! Options form and phase machine for the export flow.  One modal walks every phase without
//! leaving the stack — Options, ConfirmOutsideImages, ConfirmOverwrite, Exporting, Success, Error —
//! so the async export, both confirmations, and the "open the result" buttons all live in one
//! dismissable place.
//!
//! **The format is a field, and the rest of the form is the same for every one of them — that is
//! the point.**  A custom export renders to HTML first and pipes *that* through the converter, so
//! the stylesheet and both toggles shape a PDF exactly as they shape an HTML file.  The only
//! per-format string is the success phase's primary button, read off the selected [`ExportFormat`]
//! rather than branched on, so the widget never learns which backend will run.
//!
//! The widget is UI-only: persisting options, spawning the worker, and opening the result all
//! happen in the App-layer adapter `crate::app::modal::export`, which drives the phase transitions
//! through the `enter_*` / `set_*` helpers.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget, Wrap},
};

use crate::config::Theme;
use crate::ui::button_row::{button_row_width, footer_row_count, render_button_row};
use crate::ui::controls::{
    self, control_input_for, control_row_spans, cycle_index, input_delta, pill_spans, pill_width,
    toggle_spans, toggle_width, Control, ControlEvent, ControlInput, ControlValue,
};
use crate::ui::cursor::{insert_char_at, remove_char_at, scrolled_field_spans};
use crate::ui::overlay_nav::next_focusable_wrapping;
use crate::ui::sanitize_paste;
use crate::ui::scroll_container::{
    centered_rect_for_content, compute_pad_h, draw_frame, wrapped_rows, ContentSize, FrameOpts,
    ModalKind, ScrollContainerState, MAX_PAD_H, PROSE_CONTENT_WIDTH, VERTICAL_CHROME_ROWS,
};

/// The title row's label.  It sits outside the aligned label column of the other rows, so the
/// field gets the whole rest of the row.
const TITLE_LABEL: &str = "Title: ";
/// Minimum cell width of the title field; the field widens with the modal when another row is
/// wider.
const TITLE_FIELD_WIDTH: usize = 39;
/// Maximum title length, in characters.
const TITLE_CHAR_CAP: usize = 120;
/// Indent applied to a toggle's explanatory note, under its label.
const NOTE_INDENT: &str = "  ";

// Current-state explanations for the two toggles; the active one shows beneath its toggle.
const IMAGES_NOTE_ON: &str = "Inline images as data:URIs";
const IMAGES_NOTE_OFF: &str = "Leave images as links";
const IMAGES_NOTE_SEALED: &str = "Always inlined for converters";
const FIGURES_NOTE_ON: &str = "Render diagrams and math as images";
const FIGURES_NOTE_OFF: &str = "Leave diagrams and math as source";

const OPTION_BUTTONS: &[&str] = &["Export"];

/// Rows pinned below the scroll window: a spacer and the `[ Export ]` button row.
const FOOTER_ROWS: u16 = 2;
const OVERWRITE_BUTTONS: &[&str] = &["Overwrite", "Cancel"];
const OUTSIDE_BUTTONS: &[&str] = &["Embed", "Don't embed", "Cancel"];
/// Most out-of-folder image paths the confirmation lists before summarizing the rest.
const OUTSIDE_LIST_CAP: usize = 8;
const ERROR_BUTTONS: &[&str] = &["Back"];
/// Success-phase second button; the first is per-format ([`ExportFormat::open_result`]).
const OPEN_FOLDER_BUTTON: &str = "Open folder";
/// Frame title for every phase.  The format is a field inside the form, so the title omits it.
const FRAME_TITLE: &str = "Export";
/// Indent for each row of the Format list, under its "Format" label.
const LIST_INDENT: &str = "  ";
/// Radio markers for the Format list: filled for the selected format.
const MARKER_SELECTED: &str = "● ";
const MARKER_UNSELECTED: &str = "○ ";

/// One selectable export format, shown as a row in the Format list.  It contributes only its label
/// and the success button's wording; the App adapter holds the matching `ExportJob` in a parallel
/// list keyed by the same index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportFormat {
    /// List-row label — the format's name (`HTML`, `PDF (weasyprint)`).
    pub label: String,
    /// Success-phase primary button.  HTML opens a browser; a custom target goes to whatever the
    /// OS associates with its extension.
    pub open_result: String,
    /// Images are always inlined (`export::ImageHandling::Sealed`), so the toggle shows on and
    /// disabled.  `false` from both constructors; the App sets it from `ExportJob::seals_images`
    /// (see `docs/dev/media-export.md`).
    pub seals_images: bool,
}

impl ExportFormat {
    /// The built-in HTML exporter.
    pub fn html() -> Self {
        Self {
            label: "HTML".to_owned(),
            open_result: "Open in browser".to_owned(),
            seals_images: false,
        }
    }

    /// A `[[export.custom]]` entry called `name`.
    pub fn custom(name: &str) -> Self {
        Self {
            label: name.trim().to_owned(),
            open_result: "Open file".to_owned(),
            seals_images: false,
        }
    }
}

/// Which step of the export flow the modal is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportPhase {
    Options,
    /// Inlining would embed images from outside the document's folder; list them and ask.
    ConfirmOutsideImages,
    ConfirmOverwrite,
    Exporting,
    Success,
    Error,
}

/// Focus targets within the Options form, in Tab order.  `Format` is the whole list treated as one
/// stop; Up/Down move the selection within it (see [`ExportState::move_focus_down`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptFocus {
    Title,
    Images,
    Figures,
    Stylesheet,
    Format,
    Export,
}

impl OptFocus {
    /// Tab order, matching the painted order top to bottom.
    const ORDER: [OptFocus; 6] = [
        OptFocus::Title,
        OptFocus::Images,
        OptFocus::Figures,
        OptFocus::Stylesheet,
        OptFocus::Format,
        OptFocus::Export,
    ];

    fn step(self, delta: i32) -> Self {
        // Every field is focusable, so the predicate is always true; the shared stepper keeps
        // welcome and export on one focus ring.
        let cur = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        next_focusable_wrapping(&Self::ORDER, cur, delta, |_| true)
            .map(|i| Self::ORDER[i])
            .unwrap_or(self)
    }

    fn next(self) -> Self {
        self.step(1)
    }

    fn prev(self) -> Self {
        self.step(-1)
    }
}

/// One row of the scrolling options body.  The form is laid out as this skeleton first and painted
/// second, so the click-rect pass never has to guess which rows reached the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormRow {
    /// Blank separator row; paints nothing and takes no click.
    Spacer,
    Title,
    Images,
    /// The muted note under the images toggle.
    ImagesNote,
    Figures,
    /// The muted note under the figures toggle.
    FiguresNote,
    Stylesheet,
    /// The `Format` header above the radio rows.
    FormatLabel,
    /// One radio row, carrying its index into [`ExportState::formats`].
    Format(usize),
}

/// The options body, top to bottom.
///
/// **The Format list is last, and that placement is what makes the scroll usable**: it is the only
/// variable-length part of the form, so ending with it keeps every fixed control above the fold.
/// `[ Export ]` is absent because it is pinned below the scroll window, keeping the form
/// completable however long the list grows.
fn form_rows(format_count: usize) -> Vec<FormRow> {
    let mut rows = vec![
        FormRow::Title,
        FormRow::Spacer,
        FormRow::Images,
        FormRow::ImagesNote,
        FormRow::Spacer,
        FormRow::Figures,
        FormRow::FiguresNote,
        FormRow::Spacer,
        FormRow::Stylesheet,
        FormRow::Spacer,
        FormRow::FormatLabel,
    ];
    rows.extend((0..format_count).map(FormRow::Format));
    rows
}

/// Body rows to reveal for the current focus, in ascending order of importance — the caller
/// reveals them in order, so the last wins when the window can't hold them all.  A toggle is listed
/// *after* its note so the control is what survives at the fold.
fn focus_reveal_rows(rows: &[FormRow], focus: OptFocus, format_idx: usize) -> Vec<u16> {
    let row_of = |want: FormRow| rows.iter().position(|r| *r == want).map(|i| i as u16);
    let wanted: [Option<FormRow>; 2] = match focus {
        OptFocus::Title => [None, Some(FormRow::Title)],
        OptFocus::Images => [Some(FormRow::ImagesNote), Some(FormRow::Images)],
        OptFocus::Figures => [Some(FormRow::FiguresNote), Some(FormRow::Figures)],
        OptFocus::Stylesheet => [None, Some(FormRow::Stylesheet)],
        OptFocus::Format => [None, Some(FormRow::Format(format_idx))],
        OptFocus::Export => [None, None],
    };
    wanted.into_iter().flatten().filter_map(row_of).collect()
}

/// The user-chosen export options, handed to the adapter on submit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportChoices {
    /// `<title>` text; `None` when the field was left blank.
    pub title: Option<String>,
    pub inline_images: bool,
    pub render_figures: bool,
    /// `"builtin"` or a stylesheet path, ready for `Stylesheet::from_config_value`.
    pub stylesheet: String,
}

/// Outcome of dispatching a key to [`ExportState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportResponse {
    /// Stay open; the caller just redraws.
    Continue,
    /// Dismiss the modal (Esc or a Cancel button, at any phase).
    Cancelled,
    /// Options `[ Export ]` activated with these choices.
    Submit(ExportChoices),
    /// Out-of-folder images answered: `true` embeds the listed files, `false` leaves them as links.
    /// Either way the export proceeds.
    EmbedOutsideImages(bool),
    /// Overwrite confirmed — proceed with the export.
    ProceedOverwrite,
    /// Success-phase primary button — open the written file.
    OpenResult,
    /// Success-phase `[ Open folder ]`.
    OpenFolder,
}

/// Mutable state for an open export modal.
pub struct ExportState {
    pub phase: ExportPhase,
    /// Index 0 is always HTML.  Built by the App adapter, which holds the matching `ExportJob`s
    /// in the same order.
    pub formats: Vec<ExportFormat>,
    /// Chosen format; doubles as the list's highlighted row while the field is focused.
    pub format_idx: usize,
    /// Title field buffer, edited at [`Self::title_cursor`].
    pub title: String,
    /// Title cursor as a char index; clamped to the title's length wherever it is read.
    title_cursor: usize,
    /// First visible title char, kept between frames so the field scrolls only as far as the
    /// cursor forces it (see [`scrolled_field_spans`]).
    title_scroll: usize,
    pub inline_images: bool,
    pub render_figures: bool,
    /// `(display label, config value)` pairs; index 0 is the compiled-in default stylesheet.
    pub stylesheets: Vec<(String, String)>,
    pub stylesheet_idx: usize,
    /// Title captured at submit time so it survives an overwrite-confirm detour; it is
    /// per-document and never persisted to config.
    pub submitted_title: Option<String>,
    /// Resolved export target, set at submit and reused by the confirm phases and the worker.
    pub target: Option<PathBuf>,
    /// Canonical paths of the out-of-folder images the ConfirmOutsideImages phase lists.
    pub outside_images: Vec<PathBuf>,
    /// Written file path, shown in the Success phase.
    pub result_path: Option<PathBuf>,
    /// Images the written file is missing ([`left_out_note`]), shown in the Success phase.
    pub images_left_out: usize,
    /// Failure message, shown in the Error phase.
    pub error_message: Option<String>,
    /// Form focus (meaningful in the Options phase).
    focus: OptFocus,
    /// Button focus 0/1 for the non-form phases.
    btn_focus: usize,
    /// Absolute rect of the rendered `esc` close hint, for click hit-testing.
    pub esc_button_rect: Option<Rect>,
    /// Vertical scroll of the options body.  Focus drives it (`ensure_visible` each render); the
    /// wheel and PgUp / PgDn move it directly.
    pub scroll_state: ScrollContainerState,
    // ── Click hit-rects, captured each render ──
    /// `(index into `formats`, rect)` per *painted* format row.  The index is carried rather than
    /// implied by position, because a scrolled list paints a window not starting at format 0.
    format_rects: Vec<(usize, Rect)>,
    title_rect: Option<Rect>,
    images_rect: Option<Rect>,
    figures_rect: Option<Rect>,
    stylesheet_rect: Option<Rect>,
    export_button_rect: Option<Rect>,
    /// Button-row rects for the current message phase (overwrite / success /
    /// error), in button order.
    msg_button_rects: Vec<Rect>,
}

impl ExportState {
    /// Build the form, seeded from config plus a discovered stylesheet list.  `formats` is
    /// non-empty (HTML is always present).
    ///
    /// Focus starts on `Title`: starting on the Format list, which sits at the bottom, would open
    /// the modal already scrolled past every other control.
    pub fn new(
        formats: Vec<ExportFormat>,
        title: String,
        inline_images: bool,
        render_figures: bool,
        stylesheets: Vec<(String, String)>,
        stylesheet_idx: usize,
    ) -> Self {
        Self {
            phase: ExportPhase::Options,
            formats,
            format_idx: 0,
            title_cursor: title.chars().count(),
            title_scroll: 0,
            title,
            inline_images,
            render_figures,
            stylesheets,
            stylesheet_idx,
            submitted_title: None,
            target: None,
            outside_images: Vec::new(),
            result_path: None,
            images_left_out: 0,
            error_message: None,
            focus: OptFocus::Title,
            btn_focus: 0,
            esc_button_rect: None,
            scroll_state: ScrollContainerState::default(),
            format_rects: Vec::new(),
            title_rect: None,
            images_rect: None,
            figures_rect: None,
            stylesheet_rect: None,
            export_button_rect: None,
            msg_button_rects: Vec::new(),
        }
    }

    /// Drop every cached click hit-rect, at the top of each render, so only rows this frame
    /// painted are clickable — a scrolled-away control must not keep answering clicks where
    /// something else is now drawn.
    fn clear_hit_rects(&mut self) {
        self.esc_button_rect = None;
        self.format_rects.clear();
        self.title_rect = None;
        self.images_rect = None;
        self.figures_rect = None;
        self.stylesheet_rect = None;
        self.export_button_rect = None;
        self.msg_button_rects.clear();
    }

    /// Scroll the options body by `delta` rows.  No-op in the non-scrolling message phases.
    pub fn handle_wheel(&mut self, delta: i32) {
        if self.phase == ExportPhase::Options {
            self.scroll_state.scroll_by(delta);
        }
    }

    /// Whether the selected format ignores the Inline images toggle and always inlines.
    fn seals_images(&self) -> bool {
        self.formats
            .get(self.format_idx)
            .is_some_and(|f| f.seals_images)
    }

    /// The success-phase primary button label for the chosen format.
    fn open_result_label(&self) -> String {
        self.formats
            .get(self.format_idx)
            .map(|f| f.open_result.clone())
            .unwrap_or_else(|| "Open file".to_owned())
    }

    // ── Phase transitions (driven by the adapter) ──────────────────────────

    /// Stash the submitted target and ask whether to embed `images`, which lie outside the
    /// document's folder.  Focus starts on `[ Embed ]`, as the overwrite prompt's does on
    /// `[ Overwrite ]`: the list above it is what the user is confirming.
    pub fn enter_confirm_outside_images(&mut self, target: PathBuf, images: Vec<PathBuf>) {
        self.target = Some(target);
        self.outside_images = images;
        self.phase = ExportPhase::ConfirmOutsideImages;
        self.btn_focus = 0;
    }

    /// Stash the submitted target and switch to the overwrite-confirm phase.
    pub fn enter_confirm_overwrite(&mut self, target: PathBuf) {
        self.target = Some(target);
        self.phase = ExportPhase::ConfirmOverwrite;
        self.btn_focus = 0;
    }

    /// Set the resolved target and enter the in-progress phase.
    pub fn enter_exporting(&mut self, target: PathBuf) {
        self.target = Some(target);
        self.phase = ExportPhase::Exporting;
    }

    pub fn set_success(&mut self, path: PathBuf, images_left_out: usize) {
        self.result_path = Some(path);
        self.images_left_out = images_left_out;
        self.phase = ExportPhase::Success;
        self.btn_focus = 0;
    }

    pub fn set_error(&mut self, message: String) {
        self.error_message = Some(message);
        self.phase = ExportPhase::Error;
        self.btn_focus = 0;
    }

    // ── Input ──────────────────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: &KeyEvent) -> ExportResponse {
        // Ignore modifier chords so Ctrl-S etc. don't pollute the title field.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return ExportResponse::Continue;
        }
        match self.phase {
            ExportPhase::Options => self.handle_options_key(key),
            ExportPhase::ConfirmOutsideImages => {
                self.handle_button_key(key, OUTSIDE_BUTTONS.len(), outside_response)
            }
            ExportPhase::ConfirmOverwrite => self.handle_button_key(key, 2, |idx| {
                if idx == 0 {
                    ExportResponse::ProceedOverwrite
                } else {
                    ExportResponse::Cancelled
                }
            }),
            ExportPhase::Exporting => match key.code {
                KeyCode::Esc => ExportResponse::Cancelled,
                _ => ExportResponse::Continue,
            },
            ExportPhase::Success => self.handle_button_key(key, 2, |idx| {
                if idx == 0 {
                    ExportResponse::OpenResult
                } else {
                    ExportResponse::OpenFolder
                }
            }),
            ExportPhase::Error => match key.code {
                KeyCode::Esc => ExportResponse::Cancelled,
                KeyCode::Enter | KeyCode::Char(' ') => {
                    // `[ Back ]` needs no App interaction, so handle it in place.
                    self.phase = ExportPhase::Options;
                    ExportResponse::Continue
                }
                _ => ExportResponse::Continue,
            },
        }
    }

    /// Hit-test a click against the last render's rects, routing it through the same
    /// [`ExportResponse`] surface as the keyboard.  A control click focuses the field and applies
    /// an `Activate`; a title click only focuses.  An `esc` hit cancels in every phase.
    pub fn handle_click(&mut self, col: u16, row: u16) -> ExportResponse {
        if rect_contains(self.esc_button_rect, col, row) {
            return ExportResponse::Cancelled;
        }
        match self.phase {
            ExportPhase::Options => self.handle_options_click(col, row),
            ExportPhase::ConfirmOutsideImages => {
                self.handle_message_click(col, row, outside_response)
            }
            ExportPhase::ConfirmOverwrite => self.handle_message_click(col, row, |idx| {
                if idx == 0 {
                    ExportResponse::ProceedOverwrite
                } else {
                    ExportResponse::Cancelled
                }
            }),
            ExportPhase::Exporting => ExportResponse::Continue,
            ExportPhase::Success => self.handle_message_click(col, row, |idx| {
                if idx == 0 {
                    ExportResponse::OpenResult
                } else {
                    ExportResponse::OpenFolder
                }
            }),
            ExportPhase::Error => {
                if rect_contains(self.msg_button_rects.first().copied(), col, row) {
                    self.phase = ExportPhase::Options;
                }
                ExportResponse::Continue
            }
        }
    }

    /// Click routing for the Options form: focus the clicked field, applying an `Activate` to a
    /// control; the `[ Export ]` button submits.
    fn handle_options_click(&mut self, col: u16, row: u16) -> ExportResponse {
        // The rect carries its own index, so a scrolled list maps correctly.
        let clicked_format = self
            .format_rects
            .iter()
            .find(|(_, r)| rect_contains(Some(*r), col, row))
            .map(|(idx, _)| *idx);
        if let Some(idx) = clicked_format {
            self.focus = OptFocus::Format;
            self.format_idx = idx;
            return ExportResponse::Continue;
        }
        if rect_contains(self.title_rect, col, row) {
            self.focus = OptFocus::Title;
            return ExportResponse::Continue;
        }
        if rect_contains(self.images_rect, col, row) {
            self.focus = OptFocus::Images;
            self.apply_input(ControlInput::Activate);
            return ExportResponse::Continue;
        }
        if rect_contains(self.figures_rect, col, row) {
            self.focus = OptFocus::Figures;
            self.apply_input(ControlInput::Activate);
            return ExportResponse::Continue;
        }
        if rect_contains(self.stylesheet_rect, col, row) {
            self.focus = OptFocus::Stylesheet;
            self.apply_input(ControlInput::Activate);
            return ExportResponse::Continue;
        }
        if rect_contains(self.export_button_rect, col, row) {
            self.focus = OptFocus::Export;
            return self.submit();
        }
        ExportResponse::Continue
    }

    /// Click routing for a message phase's button row.
    fn handle_message_click(
        &mut self,
        col: u16,
        row: u16,
        activate: impl Fn(usize) -> ExportResponse,
    ) -> ExportResponse {
        for (i, r) in self.msg_button_rects.iter().enumerate() {
            if rect_contains(Some(*r), col, row) {
                self.btn_focus = i;
                return activate(i);
            }
        }
        ExportResponse::Continue
    }

    /// Shared key handling for the message phases; `activate` maps the focused button index to a
    /// response when Enter / Space fires.
    fn handle_button_key(
        &mut self,
        key: &KeyEvent,
        count: usize,
        activate: impl Fn(usize) -> ExportResponse,
    ) -> ExportResponse {
        match key.code {
            KeyCode::Esc => ExportResponse::Cancelled,
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab if count > 1 => {
                let delta = if matches!(key.code, KeyCode::Left | KeyCode::BackTab) {
                    count - 1
                } else {
                    1
                };
                self.btn_focus = (self.btn_focus + delta) % count;
                ExportResponse::Continue
            }
            KeyCode::Enter | KeyCode::Char(' ') => activate(self.btn_focus),
            _ => ExportResponse::Continue,
        }
    }

    fn handle_options_key(&mut self, key: &KeyEvent) -> ExportResponse {
        // The title field claims its editing keys first, Home / End included.
        if self.focus == OptFocus::Title && self.handle_title_key(key.code) {
            return ExportResponse::Continue;
        }
        // PgUp / PgDn / Home / End scroll the body.  Up / Down are deliberately not consumed:
        // they move focus, and the window follows focus at render time.
        if self.scroll_state.handle_paging_key(key) {
            return ExportResponse::Continue;
        }
        match key.code {
            KeyCode::Esc => ExportResponse::Cancelled,
            // Tab always moves *between* fields — the Format list is one field to Tab.
            KeyCode::Tab => {
                self.focus = self.focus.next();
                ExportResponse::Continue
            }
            KeyCode::BackTab => {
                self.focus = self.focus.prev();
                ExportResponse::Continue
            }
            // Up / Down move within the focused Format list, spilling at its ends.
            KeyCode::Down => {
                self.move_focus_down();
                ExportResponse::Continue
            }
            KeyCode::Up => {
                self.move_focus_up();
                ExportResponse::Continue
            }
            // Enter exports only from the focused button; elsewhere it advances focus, so a run
            // of Enters walks down to the button rather than exporting early.
            KeyCode::Enter => {
                if self.focus == OptFocus::Export {
                    self.submit()
                } else {
                    self.focus = self.focus.next();
                    ExportResponse::Continue
                }
            }
            // Space submits from the button; on a control it falls through to
            // `control_input_for`.  The title field consumed its own keys above.
            KeyCode::Char(' ') if self.focus == OptFocus::Export => self.submit(),
            // Everything else routes through the shared control-input mapping, or no-ops.
            _ => {
                if let Some(input) = control_input_for(key.code) {
                    self.apply_input(input);
                }
                ExportResponse::Continue
            }
        }
    }

    /// Down-arrow in the Options form: advances the selection inside a focused Format list, and
    /// spills to the next field at its bottom so the list is never a trap.
    fn move_focus_down(&mut self) {
        if self.focus == OptFocus::Format && self.format_idx + 1 < self.formats.len() {
            self.format_idx += 1;
        } else {
            self.focus = self.focus.next();
        }
    }

    /// Up-arrow mirror of [`Self::move_focus_down`].
    fn move_focus_up(&mut self) {
        if self.focus == OptFocus::Format && self.format_idx > 0 {
            self.format_idx -= 1;
        } else {
            self.focus = self.focus.prev();
        }
    }

    /// Apply a control input to the focused option field.  Toggles go through [`Control::apply`];
    /// the stylesheet pill uses [`cycle_index`] because its labels are dynamic, not `'static`, so
    /// it can't be a [`Control::Pill`].  The Format list cycles on Left/Right only.
    fn apply_input(&mut self, input: ControlInput) {
        match self.focus {
            OptFocus::Format => {
                if !matches!(input, ControlInput::Activate) {
                    self.format_idx =
                        cycle_index(self.format_idx, self.formats.len(), input_delta(input));
                }
            }
            OptFocus::Images if self.seals_images() => {}
            OptFocus::Images => {
                if let ControlEvent::Changed(ControlValue::Toggle(v)) =
                    Control::Toggle.apply(ControlValue::Toggle(self.inline_images), input)
                {
                    self.inline_images = v;
                }
            }
            OptFocus::Figures => {
                if let ControlEvent::Changed(ControlValue::Toggle(v)) =
                    Control::Toggle.apply(ControlValue::Toggle(self.render_figures), input)
                {
                    self.render_figures = v;
                }
            }
            OptFocus::Stylesheet => {
                self.stylesheet_idx = cycle_index(
                    self.stylesheet_idx,
                    self.stylesheets.len(),
                    input_delta(input),
                );
            }
            OptFocus::Title | OptFocus::Export => {}
        }
    }

    /// Apply an editing key to the focused title field; `false` leaves the key to the form
    /// (Tab, Enter, Esc, the arrows that move focus, paging).
    fn handle_title_key(&mut self, code: KeyCode) -> bool {
        let len = self.title.chars().count();
        let cursor = self.title_cursor.min(len);
        match code {
            KeyCode::Left => self.title_cursor = cursor.saturating_sub(1),
            KeyCode::Right => self.title_cursor = (cursor + 1).min(len),
            KeyCode::Home => self.title_cursor = 0,
            KeyCode::End => self.title_cursor = len,
            KeyCode::Backspace => {
                if cursor > 0 {
                    remove_char_at(&mut self.title, cursor - 1);
                    self.title_cursor = cursor - 1;
                }
            }
            KeyCode::Delete => remove_char_at(&mut self.title, cursor),
            KeyCode::Char(c) => self.insert_title_char(c),
            _ => return false,
        }
        true
    }

    /// Insert a character at the title cursor, mirroring the paste path: control chars dropped,
    /// length capped at [`TITLE_CHAR_CAP`].
    fn insert_title_char(&mut self, c: char) {
        let len = self.title.chars().count();
        if !c.is_control() && len < TITLE_CHAR_CAP {
            let cursor = self.title_cursor.min(len);
            insert_char_at(&mut self.title, cursor, c);
            self.title_cursor = cursor + 1;
        }
    }

    fn submit(&mut self) -> ExportResponse {
        let choices = self.choices();
        self.submitted_title = choices.title.clone();
        ExportResponse::Submit(choices)
    }

    fn choices(&self) -> ExportChoices {
        let title = {
            let t = self.title.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_owned())
            }
        };
        let stylesheet = self
            .stylesheets
            .get(self.stylesheet_idx)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "builtin".to_owned());
        ExportChoices {
            title,
            inline_images: self.inline_images,
            render_figures: self.render_figures,
            stylesheet,
        }
    }

    /// Insert a bracketed paste into the title field, on the same terms as the typing path.
    /// No-op unless the title field is focused.
    pub fn paste(&mut self, text: &str) {
        if self.phase != ExportPhase::Options || self.focus != OptFocus::Title {
            return;
        }
        let clean = sanitize_paste(text);
        for c in clean.chars() {
            if self.title.chars().count() >= TITLE_CHAR_CAP {
                break;
            }
            self.insert_title_char(c);
        }
    }
}

/// View-only widget that renders the modal over the editor.
pub struct ExportView<'a> {
    pub theme: &'a Theme,
    pub cursor_visible: bool,
}

impl<'a> StatefulWidget for ExportView<'a> {
    type State = ExportState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        match state.phase {
            ExportPhase::Options => self.render_options(area, buf, state),
            ExportPhase::ConfirmOutsideImages => {
                let lines =
                    outside_images_lines(&state.outside_images, state.seals_images(), self.theme);
                self.render_message(
                    area,
                    buf,
                    state,
                    FRAME_TITLE,
                    ModalKind::Warning,
                    lines,
                    OUTSIDE_BUTTONS,
                );
            }
            ExportPhase::ConfirmOverwrite => {
                let target = state
                    .target
                    .as_deref()
                    .map(display_name)
                    .unwrap_or_else(|| "the file".to_owned());
                let lines = vec![
                    owned_line(format!("{target} already exists."), self.theme),
                    owned_line("Overwrite it?".to_owned(), self.theme),
                ];
                self.render_message(
                    area,
                    buf,
                    state,
                    FRAME_TITLE,
                    ModalKind::Warning,
                    lines,
                    OVERWRITE_BUTTONS,
                );
            }
            ExportPhase::Exporting => {
                let lines = vec![owned_line("Exporting…".to_owned(), self.theme)];
                self.render_message(area, buf, state, FRAME_TITLE, ModalKind::Normal, lines, &[]);
            }
            ExportPhase::Success => {
                let path = state
                    .result_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let mut lines = vec![
                    owned_line("Exported to".to_owned(), self.theme),
                    owned_line(path, self.theme),
                ];
                if let Some(note) = left_out_note(state.images_left_out) {
                    lines.push(Line::default());
                    lines.push(owned_line(note, self.theme));
                }
                let buttons = [state.open_result_label(), OPEN_FOLDER_BUTTON.to_owned()];
                let button_refs: Vec<&str> = buttons.iter().map(String::as_str).collect();
                self.render_message(
                    area,
                    buf,
                    state,
                    "Export complete",
                    ModalKind::Normal,
                    lines,
                    &button_refs,
                );
            }
            ExportPhase::Error => {
                let msg = state.error_message.clone().unwrap_or_default();
                let lines = vec![
                    owned_line("Export failed".to_owned(), self.theme),
                    owned_line(msg, self.theme),
                ];
                self.render_message(
                    area,
                    buf,
                    state,
                    FRAME_TITLE,
                    ModalKind::Error,
                    lines,
                    ERROR_BUTTONS,
                );
            }
        }
    }
}

impl<'a> ExportView<'a> {
    fn render_options(&self, area: Rect, buf: &mut Buffer, state: &mut ExportState) {
        let labels: [&str; 3] = ["Inline images", "Inline figures", "Stylesheet"];
        let label_w = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        // Own the pill labels so the later `pill_spans` borrow doesn't pin `state` across the
        // `state.esc_button_rect` assignment below.
        let style_labels: Vec<String> = state.stylesheets.iter().map(|(l, _)| l.clone()).collect();
        let style_label_refs: Vec<&str> = style_labels.iter().map(String::as_str).collect();
        let control_w = toggle_width().max(pill_width(&style_label_refs));
        let row_w = (label_w + 2 + control_w).max(TITLE_LABEL.len() + TITLE_FIELD_WIDTH);
        // A note may be wider than the control rows; size to whichever is widest.
        let note_w = [
            IMAGES_NOTE_ON,
            IMAGES_NOTE_OFF,
            FIGURES_NOTE_ON,
            FIGURES_NOTE_OFF,
        ]
        .iter()
        .map(|n| NOTE_INDENT.len() + n.chars().count())
        .max()
        .unwrap_or(0);
        // The Format list can exceed the control rows when a converter has a long name.
        let format_w = state
            .formats
            .iter()
            .map(|f| LIST_INDENT.len() + MARKER_SELECTED.chars().count() + f.label.chars().count())
            .max()
            .unwrap_or(0);
        let content_width = row_w.max(note_w).max(format_w) as u16;

        // Clearing first is what keeps a row scrolled out of the window from leaving a clickable
        // ghost where nothing is drawn.
        state.clear_hit_rects();

        let rows = form_rows(state.formats.len());
        let content = ContentSize {
            width: content_width.max(button_row_width(OPTION_BUTTONS)),
            height: rows.len() as u16,
            pinned_top: 0,
            pinned_bottom: FOOTER_ROWS,
            ..Default::default()
        };
        let modal_area = centered_rect_for_content(content, area);

        // Resolve the scroll window *before* `draw_frame` so the chrome sees the post-clamp
        // scroll.  `ensure_visible` is what makes focus drive the scroll: there is no separate
        // scroll cursor.
        let inner_h = modal_area.height.saturating_sub(VERTICAL_CHROME_ROWS);
        let list_height = inner_h.saturating_sub(FOOTER_ROWS);
        state.scroll_state.observe(rows.len() as u16, list_height);
        for row in focus_reveal_rows(&rows, state.focus, state.format_idx) {
            state.scroll_state.ensure_visible(row);
        }

        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title: FRAME_TITLE,
                kind: ModalKind::Normal,
                show_close_hint: true,
                content,
                theme: self.theme,
            },
        );
        state.esc_button_rect = layout.esc_hit_rect;
        let inner = layout.body;
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let viewport = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: list_height.min(inner.height),
        };
        // A row's hit-rect spans the whole `label + control` run, so a click on the label
        // operates the control too (matching the settings overlay).
        let hit_w = ((label_w + 2 + control_w) as u16).min(inner.width);
        let scroll = state.scroll_state.scroll as usize;

        for (i, row) in rows
            .iter()
            .enumerate()
            .skip(scroll)
            .take(viewport.height as usize)
        {
            let row_area = Rect {
                x: viewport.x,
                y: viewport.y + (i - scroll) as u16,
                width: viewport.width,
                height: 1,
            };
            match *row {
                FormRow::Spacer => {}
                FormRow::Title => {
                    let focused = state.focus == OptFocus::Title;
                    let value_style = controls::text_value_style(focused, self.theme);
                    // The field fills the rest of the row, so its background spans the modal's
                    // inner width rather than the title's length.
                    let field_w = (row_area.width as usize).saturating_sub(TITLE_LABEL.len());
                    let control = scrolled_field_spans(
                        &state.title,
                        state.title_cursor,
                        &mut state.title_scroll,
                        field_w,
                        focused && self.cursor_visible,
                        value_style,
                        self.theme.cursor,
                    );
                    let spans = control_row_spans(
                        TITLE_LABEL.trim_end(),
                        TITLE_LABEL.len(),
                        control,
                        focused,
                        false,
                        self.theme,
                    );
                    Paragraph::new(Line::from(spans))
                        .style(self.theme.modal_bg)
                        .render(row_area, buf);
                    state.title_rect = Some(control_rect(row_area.x, row_area.y, row_area.width));
                }
                FormRow::Images => {
                    let focused = state.focus == OptFocus::Images;
                    let sealed = state.seals_images();
                    let control =
                        toggle_spans(state.inline_images || sealed, focused, sealed, self.theme);
                    self.render_row(buf, row_area, "Inline images", label_w, focused, control);
                    state.images_rect = Some(control_rect(row_area.x, row_area.y, hit_w));
                }
                FormRow::ImagesNote => {
                    let note = if state.seals_images() {
                        IMAGES_NOTE_SEALED
                    } else {
                        images_note(state.inline_images)
                    };
                    self.render_note(buf, row_area, note);
                }
                FormRow::Figures => {
                    let focused = state.focus == OptFocus::Figures;
                    let control = toggle_spans(state.render_figures, focused, false, self.theme);
                    self.render_row(buf, row_area, "Inline figures", label_w, focused, control);
                    state.figures_rect = Some(control_rect(row_area.x, row_area.y, hit_w));
                }
                FormRow::FiguresNote => {
                    self.render_note(buf, row_area, figures_note(state.render_figures));
                }
                FormRow::Stylesheet => {
                    let focused = state.focus == OptFocus::Stylesheet;
                    let control = pill_spans(
                        &style_label_refs,
                        state.stylesheet_idx,
                        focused,
                        false,
                        self.theme,
                    );
                    self.render_row(buf, row_area, "Stylesheet", label_w, focused, control);
                    state.stylesheet_rect = Some(control_rect(row_area.x, row_area.y, hit_w));
                }
                FormRow::FormatLabel => {
                    let style = controls::control_label_style(
                        state.focus == OptFocus::Format,
                        false,
                        self.theme,
                    );
                    Paragraph::new(Line::from(Span::styled("Format", style)))
                        .style(self.theme.modal_bg)
                        .render(row_area, buf);
                }
                FormRow::Format(idx) => {
                    // Clone the label out first: the paint borrows `state.formats` immutably and
                    // the rect push needs it mutably.
                    let Some(label) = state.formats.get(idx).map(|f| f.label.clone()) else {
                        continue;
                    };
                    let selected = idx == state.format_idx;
                    let focused = selected && state.focus == OptFocus::Format;
                    self.render_format_row(buf, row_area, &label, selected, focused);
                    state.format_rects.push((idx, row_area));
                }
            }
        }

        if state.scroll_state.max_scroll() > 0 {
            let bar_area = Rect {
                x: layout.scrollbar_col,
                y: viewport.y,
                width: 1,
                height: viewport.height,
            };
            crate::ui::scrollbar::render_for_scroll_state(
                bar_area,
                &state.scroll_state,
                self.theme,
                buf,
            );
        }

        // `[ Export ]` sits outside the scroll window on purpose: it is the one control the form
        // cannot be completed without.
        let button_y = viewport.y + viewport.height + 1;
        if button_y < inner.y + inner.height {
            let button_area = Rect {
                x: inner.x,
                y: button_y,
                width: inner.width,
                height: 1,
            };
            let focused_idx = match state.focus {
                OptFocus::Export => 0,
                _ => usize::MAX,
            };
            let rects =
                render_button_row(button_area, buf, OPTION_BUTTONS, focused_idx, self.theme);
            state.export_button_rect = rects.into_iter().next();
        }
    }

    /// Render one Format-list radio row into `area`.
    fn render_format_row(
        &self,
        buf: &mut Buffer,
        area: Rect,
        label: &str,
        selected: bool,
        focused: bool,
    ) {
        let marker = if selected {
            MARKER_SELECTED
        } else {
            MARKER_UNSELECTED
        };
        let spans = if focused {
            // Pad the fill so marker + label read as one affordance.
            let text = format!("{LIST_INDENT}{marker}{label}");
            let pad = (area.width as usize).saturating_sub(text.chars().count());
            vec![Span::styled(
                format!("{text}{}", " ".repeat(pad)),
                controls::focused_style(self.theme),
            )]
        } else if selected {
            // Selected but unfocused: emphasize the marker glyph only.
            vec![
                Span::styled(LIST_INDENT, self.theme.modal_item),
                Span::styled(marker, self.theme.modal_item_selected_unfocused),
                Span::styled(label.to_owned(), self.theme.modal_item),
            ]
        } else {
            vec![Span::styled(
                format!("{LIST_INDENT}{marker}{label}"),
                self.theme.modal_item,
            )]
        };
        Paragraph::new(Line::from(spans))
            .style(self.theme.modal_bg)
            .render(area, buf);
    }

    /// Render one `label  <control>` row into `area`.
    fn render_row(
        &self,
        buf: &mut Buffer,
        area: Rect,
        label: &str,
        label_w: usize,
        focused: bool,
        control: Vec<Span<'static>>,
    ) {
        // `label_w + 2` folds the gap into the styled label column, so a focused row's fill spans
        // label → widget.
        let spans = control_row_spans(label, label_w + 2, control, focused, false, self.theme);
        Paragraph::new(Line::from(spans))
            .style(self.theme.modal_bg)
            .render(area, buf);
    }

    /// Render a muted, indented note for the row above, styled like the settings descriptions.
    fn render_note(&self, buf: &mut Buffer, area: Rect, text: &str) {
        Paragraph::new(Line::from(Span::styled(
            format!("{NOTE_INDENT}{text}"),
            self.theme.modal_description,
        )))
        .style(self.theme.modal_bg)
        .render(area, buf);
    }

    /// Render a centered message body plus an optional button row, for every non-form phase.
    #[allow(clippy::too_many_arguments)]
    fn render_message(
        &self,
        area: Rect,
        buf: &mut Buffer,
        state: &mut ExportState,
        title: &str,
        kind: ModalKind,
        lines: Vec<Line<'static>>,
        buttons: &[&str],
    ) {
        // A message phase owns none of the form's rects, so a click after a phase change must not
        // land on a control the form painted earlier.
        state.clear_hit_rects();
        let has_buttons = !buttons.is_empty();
        let natural_line_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
        let buttons_w = if has_buttons {
            button_row_width(buttons)
        } else {
            0
        };
        // Cap the body width so an unbounded converter error wraps rather than running off the
        // frame.  Bounded below by the button row and above by the prose cap.
        let content_w = natural_line_w
            .min(PROSE_CONTENT_WIDTH.max(buttons_w))
            .max(buttons_w);
        // Reserve the body's *wrapped* height at the inner width the frame will hand back,
        // mirroring `draw_frame`'s padding rule — a flat `lines.len()` clipped a wrapped error.
        let prospective_modal_w = content_w.saturating_add(2 * MAX_PAD_H).min(area.width);
        let prospective_pad_h = compute_pad_h(prospective_modal_w, content_w, MAX_PAD_H);
        let body_render_w = prospective_modal_w
            .saturating_sub(2 * prospective_pad_h)
            .max(1);
        let body_rows = wrapped_rows(&lines, body_render_w);
        // The footer's wrapped height: the success pair is 38 columns, so a terminal under about
        // 40 splits it across two rows and the modal must be a row taller.
        let footer_rows = if has_buttons {
            footer_row_count(buttons, content_w, area.width, MAX_PAD_H)
        } else {
            0
        };
        let body_h = body_rows + if has_buttons { 1 + footer_rows } else { 0 };
        let content = ContentSize {
            width: content_w,
            height: 0,
            pinned_top: body_h,
            pinned_bottom: 0,
            ..Default::default()
        };
        let modal_area = centered_rect_for_content(content, area);
        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title,
                kind,
                show_close_hint: true,
                content,
                theme: self.theme,
            },
        );
        state.esc_button_rect = layout.esc_hit_rect;
        let inner = layout.body;
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let mut y = inner.y;
        let bottom = inner.y + inner.height;
        for line in lines {
            if y >= bottom {
                return;
            }
            // Each line is a paragraph that may wrap, so advance `y` by the rows it actually
            // occupies, rendered with the same `Wrap { trim: false }` the height was measured at.
            let rows = wrapped_rows(std::slice::from_ref(&line), inner.width);
            let height = rows.min(bottom.saturating_sub(y));
            let row = Rect {
                x: inner.x,
                y,
                width: inner.width,
                height,
            };
            Paragraph::new(line)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: false })
                .style(self.theme.modal_bg)
                .render(row, buf);
            y = y.saturating_add(height);
        }

        if has_buttons {
            y = y.saturating_add(1); // spacer
            if y >= bottom {
                return;
            }
            let button_area = Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: bottom.saturating_sub(y),
            };
            state.msg_button_rects =
                render_button_row(button_area, buf, buttons, state.btn_focus, self.theme);
        } else {
            state.msg_button_rects.clear();
        }
    }
}

/// Why a finished export is missing images, or `None` when it isn't.  Only a converter export
/// removes images ([`crate::export::ImageHandling::Sealed`]), so this is its one report of them.
pub fn left_out_note(count: usize) -> Option<String> {
    let noun = if count == 1 {
        "image was"
    } else {
        "images were"
    };
    (count > 0).then(|| {
        format!(
            "{count} {noun} left out: not approved for embedding, \
             remote images not allowed, or unreadable."
        )
    })
}

fn owned_line(text: String, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text, theme.modal_item))
}

/// The ConfirmOutsideImages phase's buttons, in [`OUTSIDE_BUTTONS`] order.
fn outside_response(idx: usize) -> ExportResponse {
    match idx {
        0 => ExportResponse::EmbedOutsideImages(true),
        1 => ExportResponse::EmbedOutsideImages(false),
        _ => ExportResponse::Cancelled,
    }
}

/// Body of the ConfirmOutsideImages phase: every path in full (the point is to show *where* each
/// file is), up to [`OUTSIDE_LIST_CAP`], then a count of the rest.  `sealed` says what "Don't
/// embed" means: a link for HTML, but for a converter, which would follow the link, nothing.
fn outside_images_lines(images: &[PathBuf], sealed: bool, theme: &Theme) -> Vec<Line<'static>> {
    let count = images.len();
    let noun = if count == 1 { "image" } else { "images" };
    let mut lines = vec![
        owned_line(
            format!("This document uses {count} {noun} from outside its folder:"),
            theme,
        ),
        Line::default(),
    ];
    lines.extend(
        images
            .iter()
            .take(OUTSIDE_LIST_CAP)
            .map(|p| owned_line(p.display().to_string(), theme)),
    );
    if count > OUTSIDE_LIST_CAP {
        lines.push(owned_line(
            format!("…and {} more", count - OUTSIDE_LIST_CAP),
            theme,
        ));
    }
    lines.push(Line::default());
    lines.push(owned_line(
        "Embedding copies them into the exported file.".to_owned(),
        theme,
    ));
    if sealed {
        lines.push(owned_line(
            "Otherwise they are left out of it.".to_owned(),
            theme,
        ));
    }
    lines
}

/// Current-state note for the "Inline images" toggle.
fn images_note(on: bool) -> &'static str {
    if on {
        IMAGES_NOTE_ON
    } else {
        IMAGES_NOTE_OFF
    }
}

/// Current-state note for the "Inline figures" toggle.
fn figures_note(on: bool) -> &'static str {
    if on {
        FIGURES_NOTE_ON
    } else {
        FIGURES_NOTE_OFF
    }
}

/// One-cell-high control hit-rect at `(x, y)`.
fn control_rect(x: u16, y: u16, width: u16) -> Rect {
    Rect {
        x,
        y,
        width,
        height: 1,
    }
}

/// True when `(col, row)` falls inside `rect` (a miss when `rect` is `None`).
fn rect_contains(rect: Option<Rect>, col: u16, row: u16) -> bool {
    match rect {
        Some(r) => col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height,
        None => false,
    }
}

/// File name (with extension) of a path, for the overwrite prompt.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> &'static Theme {
        Box::leak(Box::new(Theme::default()))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn state() -> ExportState {
        ExportState::new(
            vec![
                ExportFormat::html(),
                ExportFormat::custom("PDF (weasyprint)"),
            ],
            "My Doc".to_owned(),
            false,
            true,
            vec![
                ("Default".to_owned(), "builtin".to_owned()),
                ("paper.css".to_owned(), "/cfg/export/paper.css".to_owned()),
            ],
            0,
        )
    }

    #[test]
    fn tab_cycles_through_all_focus_targets() {
        let mut s = state();
        assert_eq!(s.focus, OptFocus::Title, "focus starts on the first row");
        for expected in [
            OptFocus::Images,
            OptFocus::Figures,
            OptFocus::Stylesheet,
            OptFocus::Format,
            OptFocus::Export,
            OptFocus::Title,
        ] {
            s.handle_key(&key(KeyCode::Tab));
            assert_eq!(s.focus, expected);
        }
    }

    /// Up/Down move the selection within the Format list when it is
    /// focused, spilling to the neighboring field at the ends.
    #[test]
    fn arrows_move_within_the_format_list_then_spill() {
        let mut s = state(); // 2 formats, idx 0
        s.focus = OptFocus::Format;
        assert_eq!(s.format_idx, 0);
        s.handle_key(&key(KeyCode::Down)); // → format 1
        assert_eq!(s.focus, OptFocus::Format);
        assert_eq!(s.format_idx, 1);
        s.handle_key(&key(KeyCode::Down)); // at the end → spill to Export
        assert_eq!(s.focus, OptFocus::Export);
        assert_eq!(s.format_idx, 1, "selection is unchanged when spilling");
        // Back up into the list, then off the top spills to Stylesheet.
        s.handle_key(&key(KeyCode::Up)); // → Format (idx 1)
        assert_eq!(s.focus, OptFocus::Format);
        s.handle_key(&key(KeyCode::Up)); // idx 1 → 0
        assert_eq!(s.format_idx, 0);
        s.handle_key(&key(KeyCode::Up)); // at the top → prev field
        assert_eq!(s.focus, OptFocus::Stylesheet);
    }

    /// Left/Right cycle the Format selection (with wrap) while it's focused.
    #[test]
    fn left_right_cycle_the_format_selection() {
        let mut s = state();
        s.focus = OptFocus::Format;
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(s.format_idx, 1);
        s.handle_key(&key(KeyCode::Right)); // wraps
        assert_eq!(s.format_idx, 0);
        s.handle_key(&key(KeyCode::Left)); // wraps back
        assert_eq!(s.format_idx, 1);
    }

    #[test]
    fn typing_appends_to_title_only_when_focused() {
        let mut s = state();
        s.focus = OptFocus::Title;
        s.title.clear();
        s.handle_key(&key(KeyCode::Char('H')));
        s.handle_key(&key(KeyCode::Char('i')));
        assert_eq!(s.title, "Hi");
        // Move focus off the title; characters no longer land there.
        s.handle_key(&key(KeyCode::Tab));
        s.handle_key(&key(KeyCode::Char('x')));
        assert_eq!(s.title, "Hi");
    }

    #[test]
    fn title_cursor_starts_at_the_end_and_edits_in_place() {
        let mut s = state();
        assert_eq!(s.title_cursor, "My Doc".chars().count());
        s.handle_key(&key(KeyCode::Left));
        s.handle_key(&key(KeyCode::Left));
        s.handle_key(&key(KeyCode::Left));
        s.handle_key(&key(KeyCode::Char('!')));
        assert_eq!(s.title, "My !Doc");
        s.handle_key(&key(KeyCode::Backspace));
        assert_eq!(s.title, "My Doc");
        s.handle_key(&key(KeyCode::Delete));
        assert_eq!(s.title, "My oc");
        s.handle_key(&key(KeyCode::Home));
        s.handle_key(&key(KeyCode::Left));
        assert_eq!(s.title_cursor, 0, "Left stops at the start");
        s.handle_key(&key(KeyCode::Backspace));
        assert_eq!(s.title, "My oc", "Backspace at the start is a no-op");
        s.handle_key(&key(KeyCode::End));
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(s.title_cursor, 5, "Right stops at the end");
        assert_eq!(
            s.focus,
            OptFocus::Title,
            "Left / Right never leave the field"
        );
    }

    #[test]
    fn paste_lands_at_the_title_cursor() {
        let mut s = state();
        s.handle_key(&key(KeyCode::Home));
        s.paste("The ");
        assert_eq!(s.title, "The My Doc");
        assert_eq!(s.title_cursor, 4);
    }

    #[test]
    fn a_long_title_scrolls_to_keep_the_cursor_visible() {
        let mut s = state();
        s.title = format!("{}END", "a".repeat(100));
        s.title_cursor = s.title.chars().count();
        let content = rendered(&mut s, 30);
        assert!(
            content.contains("Title: "),
            "label breaks alignment: {content}"
        );
        assert!(content.contains("aEND"), "the tail is on screen: {content}");
        assert!(s.title_scroll > 0);

        // Back to the start: the window follows the cursor left.
        s.handle_key(&key(KeyCode::Home));
        rendered(&mut s, 30);
        assert_eq!(s.title_scroll, 0);
    }

    #[test]
    fn the_focused_title_fill_spans_the_row_whatever_the_title_length() {
        let filled = |s: &mut ExportState| -> usize {
            let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
            terminal
                .draw(|f| {
                    let view = ExportView {
                        theme: theme(),
                        cursor_visible: false,
                    };
                    f.render_stateful_widget(view, f.area(), s);
                })
                .unwrap();
            let buf = terminal.backend().buffer();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| &buf[(x, y)])
                        .collect::<Vec<_>>()
                })
                .find(|row| {
                    row.iter()
                        .map(|c| c.symbol())
                        .collect::<String>()
                        .contains("Title:")
                })
                .expect("a Title row")
                .iter()
                .filter(|c| c.modifier.contains(ratatui::style::Modifier::REVERSED))
                .count()
        };
        let mut s = state();
        let short = filled(&mut s);
        assert!(
            short >= TITLE_FIELD_WIDTH,
            "the reversed fill spans the field: {short}"
        );
        s.title = "x".repeat(100);
        assert_eq!(filled(&mut s), short, "a long title fills the same width");
    }

    #[test]
    fn arrows_set_toggle_off_and_on() {
        let mut s = state();
        s.focus = OptFocus::Images;
        s.handle_key(&key(KeyCode::Right));
        assert!(s.inline_images);
        s.handle_key(&key(KeyCode::Left));
        assert!(!s.inline_images);
        // Space flips regardless of current value.
        s.handle_key(&key(KeyCode::Char(' ')));
        assert!(s.inline_images);
    }

    #[test]
    fn stylesheet_pill_cycles_and_wraps() {
        let mut s = state();
        s.focus = OptFocus::Stylesheet;
        assert_eq!(s.stylesheet_idx, 0);
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(s.stylesheet_idx, 1);
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(s.stylesheet_idx, 0, "wraps back to Default");
        s.handle_key(&key(KeyCode::Left));
        assert_eq!(s.stylesheet_idx, 1, "wraps backwards");
    }

    #[test]
    fn enter_submits_only_from_the_export_button() {
        let mut s = state();
        s.inline_images = true;
        s.focus = OptFocus::Stylesheet;
        s.handle_key(&key(KeyCode::Right)); // paper.css

        // Enter off the button advances focus instead of exporting.
        assert_eq!(s.handle_key(&key(KeyCode::Enter)), ExportResponse::Continue);
        assert_eq!(s.focus, OptFocus::Format);
        assert_eq!(s.handle_key(&key(KeyCode::Enter)), ExportResponse::Continue);
        assert_eq!(s.focus, OptFocus::Export);
        // Now on the button, Enter exports.
        let resp = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(
            resp,
            ExportResponse::Submit(ExportChoices {
                title: Some("My Doc".to_owned()),
                inline_images: true,
                render_figures: true,
                stylesheet: "/cfg/export/paper.css".to_owned(),
            })
        );
        assert_eq!(s.submitted_title, Some("My Doc".to_owned()));
    }

    #[test]
    fn blank_title_submits_as_none() {
        let mut s = state();
        s.title = "   ".to_owned();
        s.focus = OptFocus::Export;
        let resp = s.handle_key(&key(KeyCode::Enter));
        match resp {
            ExportResponse::Submit(c) => assert_eq!(c.title, None),
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn escape_cancels_in_every_phase() {
        for setup in [
            ExportPhase::Options,
            ExportPhase::ConfirmOutsideImages,
            ExportPhase::ConfirmOverwrite,
            ExportPhase::Exporting,
            ExportPhase::Success,
            ExportPhase::Error,
        ] {
            let mut s = state();
            s.phase = setup;
            assert_eq!(
                s.handle_key(&key(KeyCode::Esc)),
                ExportResponse::Cancelled,
                "phase {setup:?}"
            );
        }
    }

    #[test]
    fn overwrite_phase_confirms_and_cancels() {
        let mut s = state();
        s.enter_confirm_overwrite(PathBuf::from("/docs/guide.html"));
        assert_eq!(s.phase, ExportPhase::ConfirmOverwrite);
        // Default focus is Overwrite (index 0).
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            ExportResponse::ProceedOverwrite
        );
        // Move to Cancel and activate.
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            ExportResponse::Cancelled
        );
    }

    #[test]
    fn outside_images_phase_embeds_skips_and_cancels() {
        let mut s = state();
        let images = vec![PathBuf::from("/shared/a.png")];
        s.enter_confirm_outside_images(PathBuf::from("/docs/guide.html"), images.clone());
        assert_eq!(s.phase, ExportPhase::ConfirmOutsideImages);
        assert_eq!(s.outside_images, images);
        assert_eq!(s.target.as_deref(), Some(Path::new("/docs/guide.html")));
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            ExportResponse::EmbedOutsideImages(true)
        );
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            ExportResponse::EmbedOutsideImages(false)
        );
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            ExportResponse::Cancelled
        );
    }

    #[test]
    fn outside_images_prompt_lists_full_paths_and_caps_the_list() {
        let images: Vec<PathBuf> = (0..OUTSIDE_LIST_CAP + 2)
            .map(|i| PathBuf::from(format!("/shared/img{i}.png")))
            .collect();
        let mut s = state();
        s.enter_confirm_outside_images(PathBuf::from("/docs/guide.html"), images);
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let view = ExportView {
                    theme: theme(),
                    cursor_visible: false,
                };
                f.render_stateful_widget(view, f.area(), &mut s);
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            content.contains("10 images from outside its folder"),
            "{content}"
        );
        assert!(content.contains("/shared/img0.png"), "{content}");
        assert!(content.contains("/shared/img7.png"), "{content}");
        assert!(!content.contains("/shared/img8.png"), "{content}");
        assert!(content.contains("and 2 more"), "{content}");
        assert!(content.contains("Don't embed"), "{content}");
    }

    #[test]
    fn success_phase_buttons_open_browser_and_folder() {
        let mut s = state();
        s.set_success(PathBuf::from("/docs/guide.html"), 0);
        assert_eq!(s.phase, ExportPhase::Success);
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            ExportResponse::OpenResult
        );
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            ExportResponse::OpenFolder
        );
    }

    #[test]
    fn error_back_returns_to_options() {
        let mut s = state();
        s.set_error("boom".to_owned());
        assert_eq!(s.phase, ExportPhase::Error);
        let resp = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(resp, ExportResponse::Continue);
        assert_eq!(s.phase, ExportPhase::Options);
    }

    #[test]
    fn paste_only_lands_in_focused_title() {
        let mut s = state();
        s.focus = OptFocus::Title;
        s.title.clear();
        s.paste("a\nb\tc");
        assert_eq!(s.title, "abc", "control chars flattened away");
        s.focus = OptFocus::Images;
        s.paste("nope");
        assert_eq!(s.title, "abc", "paste ignored off the title field");
    }

    #[test]
    fn ctrl_chords_do_not_pollute_title() {
        let mut s = state();
        s.title.clear();
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        s.handle_key(&ctrl_s);
        assert_eq!(s.title, "");
    }

    #[test]
    fn renders_options_form() {
        let backend = TestBackend::new(70, 22);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut s = state(); // inline_images off, render_figures on
        terminal
            .draw(|frame| {
                let view = ExportView {
                    theme: theme(),
                    cursor_visible: true,
                };
                frame.render_stateful_widget(view, frame.area(), &mut s);
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("Export"), "frame title: {content}");
        assert!(content.contains("Format"), "format list: {content}");
        assert!(content.contains("HTML"), "HTML format row: {content}");
        assert!(content.contains("Inline images"), "images row: {content}");
        assert!(content.contains("Stylesheet"), "stylesheet row: {content}");
        // Each toggle's note reflects its current state: images off, diagrams on.
        assert!(
            content.contains(IMAGES_NOTE_OFF),
            "images-off note: {content}"
        );
        assert!(
            content.contains(FIGURES_NOTE_ON),
            "diagrams-on note: {content}"
        );
    }

    #[test]
    fn toggle_note_swaps_with_state() {
        assert_eq!(images_note(true), IMAGES_NOTE_ON);
        assert_eq!(images_note(false), IMAGES_NOTE_OFF);
        assert_eq!(figures_note(true), FIGURES_NOTE_ON);
        assert_eq!(figures_note(false), FIGURES_NOTE_OFF);
    }

    /// Render once into a headless backend so `s`'s click hit-rects are populated.
    fn render_modal(s: &mut ExportState, w: u16, h: u16) {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = ExportView {
                    theme: theme(),
                    cursor_visible: true,
                };
                frame.render_stateful_widget(view, frame.area(), s);
            })
            .unwrap();
    }

    #[test]
    fn click_flips_a_toggle_and_focuses_its_row() {
        let mut s = state(); // inline_images starts off
        render_modal(&mut s, 70, 22);
        let r = s.images_rect.expect("images rect captured at render");
        let resp = s.handle_click(r.x, r.y);
        assert_eq!(resp, ExportResponse::Continue);
        assert!(s.inline_images, "clicking the toggle flips it on");
        assert_eq!(s.focus, OptFocus::Images);
    }

    #[test]
    fn click_cycles_the_stylesheet_pill() {
        let mut s = state(); // idx 0, two stylesheets
        render_modal(&mut s, 70, 22);
        let r = s
            .stylesheet_rect
            .expect("stylesheet rect captured at render");
        s.handle_click(r.x, r.y);
        assert_eq!(
            s.stylesheet_idx, 1,
            "click advances the pill like Right/Space"
        );
    }

    #[test]
    fn click_on_export_button_submits() {
        let mut s = state();
        render_modal(&mut s, 70, 22);
        let r = s
            .export_button_rect
            .expect("export button rect captured at render");
        let resp = s.handle_click(r.x, r.y);
        assert!(matches!(resp, ExportResponse::Submit(_)));
        assert_eq!(s.focus, OptFocus::Export);
    }

    #[test]
    fn click_on_esc_hint_cancels() {
        let mut s = state();
        render_modal(&mut s, 70, 22);
        let r = s.esc_button_rect.expect("esc hint rect captured at render");
        assert_eq!(s.handle_click(r.x, r.y), ExportResponse::Cancelled);
    }

    #[test]
    fn click_on_success_buttons_opens_browser_and_folder() {
        let mut s = state();
        s.set_success(PathBuf::from("/docs/guide.html"), 0);
        render_modal(&mut s, 70, 14);
        let browser = s.msg_button_rects[0];
        assert_eq!(
            s.handle_click(browser.x, browser.y),
            ExportResponse::OpenResult
        );
        let folder = s.msg_button_rects[1];
        assert_eq!(
            s.handle_click(folder.x, folder.y),
            ExportResponse::OpenFolder
        );
    }

    /// Regression: an unbounded converter error rendered on one un-wrapped row, truncating
    /// exactly the tail a user needs to diagnose the failure.
    #[test]
    fn a_long_error_message_wraps_instead_of_truncating() {
        let (w, h) = (70u16, 24u16);
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut s = state();
        s.set_error(
            "export command exited with status 1: WARNING: Expected a media type, \
             got '(prefers-color-scheme: dark)' which is not valid; the offending \
             rule was ignored and the document rendered without it ERRORTAIL"
                .to_owned(),
        );
        terminal
            .draw(|frame| {
                let view = ExportView {
                    theme: theme(),
                    cursor_visible: true,
                };
                frame.render_stateful_widget(view, frame.area(), &mut s);
            })
            .unwrap();

        // Rebuild the screen row by row so wrapped text reads across lines.
        let buf = terminal.backend().buffer();
        let screen: String = (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf.content[(y * w + x) as usize].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(screen.contains("Export failed"), "head missing:\n{screen}");
        assert!(
            screen.contains("ERRORTAIL"),
            "the tail of a long error must be visible, not truncated:\n{screen}"
        );
    }

    #[test]
    fn renders_success_phase() {
        let backend = TestBackend::new(70, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut s = state();
        s.set_success(PathBuf::from("/docs/guide.html"), 0);
        terminal
            .draw(|frame| {
                let view = ExportView {
                    theme: theme(),
                    cursor_visible: true,
                };
                frame.render_stateful_widget(view, frame.area(), &mut s);
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("Export complete"), "{content}");
        assert!(content.contains("Open in browser"), "{content}");
        assert!(content.contains("Open folder"), "{content}");
    }

    /// Every option applies to every format, since a converter reads the intermediate HTML.  A
    /// format growing its own fields would mean per-target branching crept into the widget.
    #[test]
    fn the_format_list_shows_every_format_and_shares_the_form() {
        let mut s = ExportState::new(
            vec![
                ExportFormat::html(),
                ExportFormat::custom("PDF (weasyprint)"),
            ],
            "My Doc".to_owned(),
            false,
            true,
            vec![("Default".to_owned(), "builtin".to_owned())],
            0,
        );
        let backend = TestBackend::new(70, 22);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = ExportView {
                    theme: theme(),
                    cursor_visible: true,
                };
                frame.render_stateful_widget(view, frame.area(), &mut s);
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        // One neutral frame title, the Format list, and both formats.
        assert!(content.contains("Export"), "frame title: {content}");
        assert!(content.contains("Format"), "format label: {content}");
        assert!(content.contains("HTML"), "HTML row: {content}");
        assert!(
            content.contains("PDF (weasyprint)"),
            "custom row: {content}"
        );
        // Every shared option is present regardless of format.
        for expected in ["Title", "Inline images", "Inline figures", "Stylesheet"] {
            assert!(content.contains(expected), "{expected} missing: {content}");
        }
    }

    /// A state with the given formats and `format_idx` selected.
    fn state_with_selected(formats: Vec<ExportFormat>, idx: usize) -> ExportState {
        let mut s = ExportState::new(formats, String::new(), false, true, vec![], 0);
        s.format_idx = idx;
        s
    }

    fn rendered(s: &mut ExportState, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, h)).unwrap();
        terminal
            .draw(|f| {
                let view = ExportView {
                    theme: theme(),
                    cursor_visible: false,
                };
                f.render_stateful_widget(view, f.area(), s);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// A converter row as the App builds it (`ExportJob::format`).
    fn sealed(name: &str) -> ExportFormat {
        ExportFormat {
            seals_images: true,
            ..ExportFormat::custom(name)
        }
    }

    /// A converter format always inlines, so its toggle says so and ignores input — keeping the
    /// user's own value for when HTML is selected again.
    #[test]
    fn a_converter_format_pins_the_images_toggle_on() {
        let mut s = state_with_selected(vec![ExportFormat::html(), sealed("PDF")], 1);
        s.focus = OptFocus::Images;
        s.apply_input(ControlInput::Activate);
        assert!(!s.inline_images, "the toggle ignores input while sealed");
        assert!(rendered(&mut s, 30).contains(IMAGES_NOTE_SEALED));

        s.format_idx = 0;
        assert!(rendered(&mut s, 30).contains(IMAGES_NOTE_OFF));
        s.apply_input(ControlInput::Activate);
        assert!(s.inline_images, "HTML honors the toggle");
    }

    /// A file missing images says so; one that isn't says nothing about images.
    #[test]
    fn the_success_phase_reports_images_left_out() {
        let mut s = state_with_selected(vec![ExportFormat::html(), sealed("PDF")], 1);
        s.set_success(PathBuf::from("/docs/guide.pdf"), 2);
        assert!(rendered(&mut s, 30).contains("2 images were left out"));
        s.set_success(PathBuf::from("/docs/guide.pdf"), 0);
        assert!(!rendered(&mut s, 30).contains("left out"));
        assert_eq!(left_out_note(0), None);
        assert!(left_out_note(1)
            .unwrap()
            .starts_with("1 image was left out"));
    }

    /// For a converter, "Don't embed" can't mean "leave a link" — it would follow the link.
    #[test]
    fn the_outside_prompt_says_a_converter_leaves_unembedded_images_out() {
        let images = vec![PathBuf::from("/shared/a.png")];
        let mut s = state_with_selected(vec![ExportFormat::html(), sealed("PDF")], 1);
        s.enter_confirm_outside_images(PathBuf::from("/docs/guide.pdf"), images.clone());
        assert!(rendered(&mut s, 30).contains("left out"));

        let mut s = state_with_selected(vec![ExportFormat::html(), sealed("PDF")], 0);
        s.enter_confirm_outside_images(PathBuf::from("/docs/guide.html"), images);
        assert!(!rendered(&mut s, 30).contains("left out"));
    }

    /// The success button names what will actually open, so a custom format must not promise a
    /// browser.
    #[test]
    fn the_success_button_names_the_selected_format() {
        let mut s = state_with_selected(vec![ExportFormat::html(), sealed("PDF")], 1);
        s.set_success(PathBuf::from("/docs/guide.pdf"), 0);
        let backend = TestBackend::new(70, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = ExportView {
                    theme: theme(),
                    cursor_visible: true,
                };
                frame.render_stateful_widget(view, frame.area(), &mut s);
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(content.contains("Open file"), "{content}");
        assert!(!content.contains("Open in browser"), "{content}");
        assert!(content.contains("Open folder"), "{content}");
    }

    /// The response surface is shared, so a success click must not depend on the button label.
    #[test]
    fn custom_success_buttons_still_resolve_to_open_and_folder() {
        let mut s = state_with_selected(vec![ExportFormat::html(), sealed("PDF")], 1);
        s.set_success(PathBuf::from("/docs/guide.pdf"), 0);
        render_modal(&mut s, 70, 14);
        let open = s.msg_button_rects[0];
        let folder = s.msg_button_rects[1];
        assert_eq!(
            s.handle_click(open.x, open.y),
            ExportResponse::OpenResult,
            "primary button"
        );
        assert_eq!(
            s.handle_click(folder.x, folder.y),
            ExportResponse::OpenFolder,
            "folder button"
        );
    }

    // ── Scrolling options body ────────────────────────────────────────────

    /// The variable-length list is last, so a long converter list can only push *itself* out of
    /// view; `[ Export ]` is not a body row at all.
    #[test]
    fn the_form_rows_end_with_the_format_list() {
        let rows = form_rows(3);
        assert_eq!(rows[0], FormRow::Title);
        assert_eq!(
            &rows[rows.len() - 4..],
            &[
                FormRow::FormatLabel,
                FormRow::Format(0),
                FormRow::Format(1),
                FormRow::Format(2)
            ]
        );
        // Every fixed control precedes the list.
        let list_start = rows
            .iter()
            .position(|r| *r == FormRow::FormatLabel)
            .unwrap();
        for control in [
            FormRow::Title,
            FormRow::Images,
            FormRow::Figures,
            FormRow::Stylesheet,
        ] {
            assert!(rows.iter().position(|r| *r == control).unwrap() < list_start);
        }
    }

    /// A toggle is revealed *after* its note, so the control is what survives at the fold.
    #[test]
    fn focus_reveal_puts_the_control_last() {
        let rows = form_rows(2);
        let reveal = focus_reveal_rows(&rows, OptFocus::Images, 0);
        let images = rows.iter().position(|r| *r == FormRow::Images).unwrap() as u16;
        let note = rows.iter().position(|r| *r == FormRow::ImagesNote).unwrap() as u16;
        assert_eq!(reveal, vec![note, images], "note first, control last");
        // The pinned button is never scrolled to.
        assert!(focus_reveal_rows(&rows, OptFocus::Export, 0).is_empty());
        // The Format list reveals the *selected* row, not the label.
        assert_eq!(
            focus_reveal_rows(&rows, OptFocus::Format, 1),
            vec![rows.iter().position(|r| *r == FormRow::Format(1)).unwrap() as u16]
        );
    }

    /// Build a state with `n` formats (HTML plus `n - 1` converters).
    fn state_with_formats(n: usize) -> ExportState {
        let formats: Vec<ExportFormat> = std::iter::once(ExportFormat::html())
            .chain((1..n).map(|i| ExportFormat::custom(&format!("Converter {i}"))))
            .collect();
        ExportState::new(
            formats,
            "My Doc".to_owned(),
            false,
            true,
            vec![("Default".to_owned(), "builtin".to_owned())],
            0,
        )
    }

    /// Regression: with more converters than terminal rows, `[ Export ]` used to drop off the
    /// bottom while its stale hit-rect stayed live.
    #[test]
    fn a_long_format_list_keeps_the_export_button_on_screen() {
        let mut s = state_with_formats(12);
        s.focus = OptFocus::Format;
        s.format_idx = 11;
        render_modal(&mut s, 70, 16);

        let rect = s
            .export_button_rect
            .expect("the button is pinned, not scrolled");
        assert!(rect.y < 16, "painted inside the terminal: {rect:?}");
        assert!(
            s.scroll_state.max_scroll() > 0,
            "the body should be overflowing for this to mean anything"
        );
        // The focused format scrolled into view, so it takes clicks.
        assert!(
            s.format_rects.iter().any(|(idx, _)| *idx == 11),
            "the focused format should be painted: {:?}",
            s.format_rects
        );
    }

    /// A control scrolled off screen must leave **no** hit-rect: a stale one answers clicks at
    /// coordinates now showing something else.
    #[test]
    fn a_row_scrolled_out_of_view_leaves_no_click_rect() {
        let mut s = state_with_formats(12);
        // Tall enough for everything: every control is clickable.
        render_modal(&mut s, 70, 40);
        assert!(s.title_rect.is_some());
        assert!(s.images_rect.is_some());
        assert_eq!(s.scroll_state.max_scroll(), 0, "nothing to scroll");

        // Now short, with focus at the far end of the list.
        s.focus = OptFocus::Format;
        s.format_idx = 11;
        render_modal(&mut s, 70, 16);
        assert!(s.scroll_state.scroll > 0);
        assert!(s.title_rect.is_none(), "scrolled-away title keeps no rect");
        assert!(s.images_rect.is_none());
        assert!(s.stylesheet_rect.is_none());
    }

    /// A click on a scrolled list resolves to the format under the pointer — the reason the rects
    /// carry their own index.
    #[test]
    fn a_click_on_a_scrolled_list_selects_the_row_under_the_pointer() {
        let mut s = state_with_formats(12);
        s.focus = OptFocus::Format;
        s.format_idx = 11;
        render_modal(&mut s, 70, 16);

        // Take the topmost painted format row and click it.
        let (idx, rect) = *s
            .format_rects
            .first()
            .expect("some format rows are painted");
        assert!(idx > 0, "the list is scrolled past HTML: {idx}");
        s.handle_click(rect.x + 3, rect.y);
        assert_eq!(s.format_idx, idx, "clicked row wins");
        assert_eq!(s.focus, OptFocus::Format);
    }

    /// The wheel scrolls the options body, and stops at both ends.
    #[test]
    fn the_wheel_scrolls_the_options_body_within_bounds() {
        let mut s = state_with_formats(12);
        render_modal(&mut s, 70, 16);
        assert_eq!(s.scroll_state.scroll, 0, "focus starts on the first row");

        s.handle_wheel(3);
        assert_eq!(s.scroll_state.scroll, 3);
        s.handle_wheel(1000);
        assert_eq!(
            s.scroll_state.scroll,
            s.scroll_state.max_scroll(),
            "clamped"
        );
        s.handle_wheel(-1000);
        assert_eq!(s.scroll_state.scroll, 0, "clamped at the top");

        // The message phases don't scroll.
        s.set_error("boom".to_owned());
        s.handle_wheel(5);
        assert_eq!(s.scroll_state.scroll, 0);
    }

    /// PgDn / PgUp page the body; Up / Down are left alone so they keep moving focus.
    #[test]
    fn paging_keys_scroll_but_arrows_still_move_focus() {
        let mut s = state_with_formats(12);
        render_modal(&mut s, 70, 16);

        s.handle_key(&key(KeyCode::PageDown));
        assert!(
            s.scroll_state.scroll > 0,
            "PgDn pages the body, even from the title"
        );
        // On the title, Home belongs to the field cursor.
        s.handle_key(&key(KeyCode::Home));
        assert!(
            s.scroll_state.scroll > 0,
            "Home on the title leaves the body alone"
        );
        assert_eq!(s.title_cursor, 0);
        // Off the title, Home scrolls the body.
        s.focus = OptFocus::Images;
        s.handle_key(&key(KeyCode::Home));
        assert_eq!(s.scroll_state.scroll, 0);

        // Down still moves focus off the title, not the scroll.
        s.focus = OptFocus::Title;
        s.handle_key(&key(KeyCode::Down));
        assert_eq!(s.focus, OptFocus::Images);
        assert_eq!(s.scroll_state.scroll, 0);
    }

    /// The window follows focus in both directions.
    #[test]
    fn focus_moving_back_up_scrolls_the_controls_into_view() {
        let mut s = state_with_formats(12);
        s.focus = OptFocus::Format;
        s.format_idx = 11;
        render_modal(&mut s, 70, 16);
        assert!(s.title_rect.is_none(), "scrolled past the title");

        s.focus = OptFocus::Title;
        render_modal(&mut s, 70, 16);
        assert_eq!(s.scroll_state.scroll, 0);
        assert!(s.title_rect.is_some(), "the title is back on screen");
    }
}
