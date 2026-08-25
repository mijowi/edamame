//! Options form and phase machine for the export flow (`Action::ExportHtml`,
//! shown in the palette as `Export…`).  Its Format list covers HTML and
//! every configured `[[export.custom]]` converter.
//!
//! A single modal that walks through several phases without ever leaving
//! the stack, so the async export, the overwrite confirmation, and the
//! "open the result" buttons all live in one dismissable place:
//!
//! * **Options** — a `Title` text field, two toggles (`Inline images`,
//!   `Inline diagrams`), a `Stylesheet` pill and the `Format` list, above
//!   a lone `[ Export ]` button (Esc dismisses; there is no Cancel
//!   button).  Each setting is separated by a spacer; each toggle carries
//!   a muted note describing its current (On/Off) state.  Enter exports
//!   only from the focused button — on any other field it advances focus.
//!   The body **scrolls** (see [`form_rows`]): the Format list is as long
//!   as the user's converter list, so it goes last and `[ Export ]` is
//!   pinned below the scroll window rather than being pushed off a short
//!   terminal.
//! * **ConfirmOverwrite** — shown only when the target already exists.
//! * **Exporting** — a static "Exporting…" notice while the worker runs.
//! * **Success** — the written path plus `[ Open … ]` / `[ Open folder ]`.
//! * **Error** — the failure message plus `[ Back ]` to the form.
//!
//! **The format is a field, and the rest of the form is the same for
//! every one of them — that is the point.**  The Format list chooses
//! HTML or a configured converter; a custom export renders the
//! document to HTML first and pipes *that* through the converter, so the
//! stylesheet, the inline-images toggle and the diagrams toggle shape a
//! PDF exactly as they shape an HTML file.  The only per-format string is
//! the success phase's primary button, taken from the selected
//! [`ExportFormat`] rather than branched on here, so the state never
//! learns which backend will run.
//!
//! The widget is UI-only.  All side effects (persisting the chosen options
//! to config, spawning the export worker, opening the result) happen in the
//! App-layer adapter `crate::app::modal::export`, which reads the
//! values off this state when [`ExportResponse`] fires and drives the
//! phase transitions via the `enter_*` / `set_*` helpers.

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
use crate::ui::cursor::text_field_spans;
use crate::ui::overlay_nav::next_focusable_wrapping;
use crate::ui::sanitize_paste;
use crate::ui::scroll_container::{
    centered_rect_for_content, compute_pad_h, draw_frame, wrapped_rows, ContentSize, FrameOpts,
    ModalKind, ScrollContainerState, MAX_PAD_H, PROSE_CONTENT_WIDTH, VERTICAL_CHROME_ROWS,
};

/// Reserved cell width of the title input column.
const TITLE_FIELD_WIDTH: usize = 30;
/// Maximum title length, in characters.
const TITLE_CHAR_CAP: usize = 120;
/// Indent applied to a toggle's explanatory note, under its label.
const NOTE_INDENT: &str = "  ";

// Current-state explanations for the two toggles.  Each pair reads as
// "what this setting does right now"; the active one is shown beneath the
// toggle and swaps as the value flips.
const IMAGES_NOTE_ON: &str = "Inline images as data:URIs";
const IMAGES_NOTE_OFF: &str = "Leave images as links";
const DIAGRAMS_NOTE_ON: &str = "Inline diagrams as SVG";
const DIAGRAMS_NOTE_OFF: &str = "Leave diagrams as code";

const OPTION_BUTTONS: &[&str] = &["Export"];

/// Rows the options form pins below its scroll window: a spacer and the
/// `[ Export ]` button row.
const FOOTER_ROWS: u16 = 2;
const OVERWRITE_BUTTONS: &[&str] = &["Overwrite", "Cancel"];
const ERROR_BUTTONS: &[&str] = &["Back"];
/// Success-phase second button.  The first is per-format
/// ([`ExportFormat::open_result`]); this one never varies — every export
/// writes a file into a folder.
const OPEN_FOLDER_BUTTON: &str = "Open folder";
/// Frame title for every phase of the export flow.  The specific format is
/// a field *inside* the form now (the Format list), so the title no longer
/// names it.
const FRAME_TITLE: &str = "Export";
/// Indent for each row of the Format list, under its "Format" label.
const LIST_INDENT: &str = "  ";
/// Radio markers for the Format list: filled for the selected format.
const MARKER_SELECTED: &str = "● ";
const MARKER_UNSELECTED: &str = "○ ";

/// One selectable export format, shown as a row in the modal's Format list.
///
/// Everything else about the flow — the rest of the options form, the
/// phases, the overwrite confirmation, the button geometry — is identical
/// whichever format is chosen, because a custom export *is* an HTML export
/// piped through a converter.  So the format contributes only its list
/// label and the one word the success button needs; the App adapter holds
/// the matching `ExportJob` (what actually runs) in a parallel list, keyed
/// by the same index, so the widget never learns which backend will run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportFormat {
    /// List-row label — the format's name (`HTML`, `PDF (weasyprint)`).
    pub label: String,
    /// Success-phase primary button.  HTML has a browser to open into;
    /// a custom target is handed to whatever the OS associates with its
    /// extension, so it says `Open file`.
    pub open_result: String,
}

impl ExportFormat {
    /// The built-in HTML exporter.
    pub fn html() -> Self {
        Self {
            label: "HTML".to_owned(),
            open_result: "Open in browser".to_owned(),
        }
    }

    /// A `[[export.custom]]` entry called `name`.
    pub fn custom(name: &str) -> Self {
        Self {
            label: name.trim().to_owned(),
            open_result: "Open file".to_owned(),
        }
    }
}

/// Which step of the export flow the modal is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportPhase {
    Options,
    ConfirmOverwrite,
    Exporting,
    Success,
    Error,
}

/// Focus targets within the Options form, in Tab order.  `Format` is the
/// whole Format list treated as a single focus stop; Up/Down move the
/// selection within it (see [`ExportState::move_focus_down`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptFocus {
    Title,
    Images,
    Diagrams,
    Stylesheet,
    Format,
    Export,
}

impl OptFocus {
    /// Tab order, matching the painted order top to bottom — the Format
    /// list sits last because it is the only variable-length part of the
    /// form (see [`form_rows`]).
    const ORDER: [OptFocus; 6] = [
        OptFocus::Title,
        OptFocus::Images,
        OptFocus::Diagrams,
        OptFocus::Stylesheet,
        OptFocus::Format,
        OptFocus::Export,
    ];

    fn step(self, delta: i32) -> Self {
        // Every form field is focusable, so the predicate is always true;
        // the shared wrapping stepper keeps welcome and export on one focus
        // ring.  Wrapping always yields a slot here, but fall back to `self`
        // defensively if the ring were ever empty.
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

/// One row of the scrolling options body, in painting order.  The form is
/// laid out as this skeleton first and painted second, so the scroll window
/// can drop a row without the click-rect pass having to guess which rows
/// made it onto the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormRow {
    /// Blank separator row; paints nothing and takes no click.
    Spacer,
    Title,
    Images,
    /// The muted note under the images toggle.
    ImagesNote,
    Diagrams,
    /// The muted note under the diagrams toggle.
    DiagramsNote,
    Stylesheet,
    /// The `Format` header above the radio rows.
    FormatLabel,
    /// One radio row, carrying its index into [`ExportState::formats`].
    Format(usize),
}

/// The options body, top to bottom.
///
/// **The Format list is last, and that placement is what makes the scroll
/// usable.**  It is the only variable-length part of the form — one row per
/// configured converter — so ending with it keeps every fixed control above
/// the fold: a user with a dozen converters still opens the modal looking
/// at Title, both toggles and the stylesheet, and scrolls only to reach
/// further down the list.  Leading with it would push those controls off a
/// short terminal instead.
///
/// `[ Export ]` is deliberately absent: it is pinned below the scroll
/// window, so the form is always completable however long the list grows.
fn form_rows(format_count: usize) -> Vec<FormRow> {
    let mut rows = vec![
        FormRow::Title,
        FormRow::Spacer,
        FormRow::Images,
        FormRow::ImagesNote,
        FormRow::Spacer,
        FormRow::Diagrams,
        FormRow::DiagramsNote,
        FormRow::Spacer,
        FormRow::Stylesheet,
        FormRow::Spacer,
        FormRow::FormatLabel,
    ];
    rows.extend((0..format_count).map(FormRow::Format));
    rows
}

/// Body rows to bring into view for the current focus, in ascending order
/// of importance — the caller reveals them in order, so the last one wins
/// when the window cannot hold them all.
///
/// A toggle is listed *after* its note so that at the fold the control
/// itself is what stays on screen.  `Export` yields nothing: it lives in
/// the pinned footer and is always visible.
fn focus_reveal_rows(rows: &[FormRow], focus: OptFocus, format_idx: usize) -> Vec<u16> {
    let row_of = |want: FormRow| rows.iter().position(|r| *r == want).map(|i| i as u16);
    let wanted: [Option<FormRow>; 2] = match focus {
        OptFocus::Title => [None, Some(FormRow::Title)],
        OptFocus::Images => [Some(FormRow::ImagesNote), Some(FormRow::Images)],
        OptFocus::Diagrams => [Some(FormRow::DiagramsNote), Some(FormRow::Diagrams)],
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
    pub render_diagrams: bool,
    /// `"builtin"` or a stylesheet path, ready for
    /// `Stylesheet::from_config_value`.
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
    /// Selectable formats, shown as the Format list; index 0 is always
    /// HTML.  Built by the App adapter, which holds the matching
    /// `ExportJob`s in the same order.
    pub formats: Vec<ExportFormat>,
    /// Index into `formats` of the chosen format.  Doubles as the Format
    /// list's highlighted row while that field is focused.
    pub format_idx: usize,
    /// Title field buffer (append-only, like the insert-table fields).
    pub title: String,
    pub inline_images: bool,
    pub render_diagrams: bool,
    /// `(display label, config value)` pairs; index 0 is always the
    /// compiled-in default stylesheet (labelled `Default`, value `builtin`).
    pub stylesheets: Vec<(String, String)>,
    pub stylesheet_idx: usize,
    /// Title captured at submit time so it survives an overwrite-confirm
    /// detour (the title is per-document and never persisted to config).
    pub submitted_title: Option<String>,
    /// Resolved export target; set once the form is submitted.  Reused by
    /// the overwrite-confirm phase and the export worker.
    pub target: Option<PathBuf>,
    /// Written file path, shown in the Success phase.
    pub result_path: Option<PathBuf>,
    /// Failure message, shown in the Error phase.
    pub error_message: Option<String>,
    /// Form focus (meaningful in the Options phase).
    focus: OptFocus,
    /// Button focus 0/1 for the non-form phases.
    btn_focus: usize,
    /// Absolute rect of the rendered `esc` close hint, for click hit-testing.
    pub esc_button_rect: Option<Rect>,
    /// Vertical scroll of the options body.  The form scrolls because the
    /// Format list is as long as the user's converter list; focus drives
    /// it (`ensure_visible` each render), and the wheel / PgUp / PgDn move
    /// it directly.
    pub scroll_state: ScrollContainerState,
    // ── Click hit-rects, captured each render ──
    /// Options-form control rects (None until the form has rendered once,
    /// and None again for any row the scroll window left off screen).
    /// `(index into `formats`, rect)` per *painted* format row — the index
    /// is carried rather than implied by position, because a scrolled list
    /// paints a window that does not start at format 0.
    format_rects: Vec<(usize, Rect)>,
    title_rect: Option<Rect>,
    images_rect: Option<Rect>,
    diagrams_rect: Option<Rect>,
    stylesheet_rect: Option<Rect>,
    export_button_rect: Option<Rect>,
    /// Button-row rects for the current message phase (overwrite / success /
    /// error), in button order.
    msg_button_rects: Vec<Rect>,
}

impl ExportState {
    /// Build the form, seeded from config plus a discovered stylesheet list.
    /// `formats` is non-empty (HTML is always present).
    ///
    /// Focus starts on `Title`, the first row.  It used to start on the
    /// Format list, which now sits at the *bottom* of the form — starting
    /// there would open the modal already scrolled past every other
    /// control.  HTML is preselected, so the common case needs no visit to
    /// the list at all.
    pub fn new(
        formats: Vec<ExportFormat>,
        title: String,
        inline_images: bool,
        render_diagrams: bool,
        stylesheets: Vec<(String, String)>,
        stylesheet_idx: usize,
    ) -> Self {
        Self {
            phase: ExportPhase::Options,
            formats,
            format_idx: 0,
            title,
            inline_images,
            render_diagrams,
            stylesheets,
            stylesheet_idx,
            submitted_title: None,
            target: None,
            result_path: None,
            error_message: None,
            focus: OptFocus::Title,
            btn_focus: 0,
            esc_button_rect: None,
            scroll_state: ScrollContainerState::default(),
            format_rects: Vec::new(),
            title_rect: None,
            images_rect: None,
            diagrams_rect: None,
            stylesheet_rect: None,
            export_button_rect: None,
            msg_button_rects: Vec::new(),
        }
    }

    /// Drop every cached click hit-rect.  Called at the top of each render
    /// so that only rows the *current* frame actually painted are
    /// clickable: a control scrolled out of the window, or dropped because
    /// the terminal is too short, must not keep answering clicks at a
    /// position where something else is now drawn.
    fn clear_hit_rects(&mut self) {
        self.esc_button_rect = None;
        self.format_rects.clear();
        self.title_rect = None;
        self.images_rect = None;
        self.diagrams_rect = None;
        self.stylesheet_rect = None;
        self.export_button_rect = None;
        self.msg_button_rects.clear();
    }

    /// Scroll the options body by `delta` rows (mouse wheel).  A no-op in
    /// the message phases, which do not scroll.
    pub fn handle_wheel(&mut self, delta: i32) {
        if self.phase == ExportPhase::Options {
            self.scroll_state.scroll_by(delta);
        }
    }

    /// The success-phase primary button label for the chosen format.
    fn open_result_label(&self) -> String {
        self.formats
            .get(self.format_idx)
            .map(|f| f.open_result.clone())
            .unwrap_or_else(|| "Open file".to_owned())
    }

    // ── Phase transitions (driven by the adapter) ──────────────────────────

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

    pub fn set_success(&mut self, path: PathBuf) {
        self.result_path = Some(path);
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
        // Ignore modifier chords so the user can press Ctrl-S etc. without
        // polluting the title field — Esc (no modifier) still gets through.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return ExportResponse::Continue;
        }
        match self.phase {
            ExportPhase::Options => self.handle_options_key(key),
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
                    // The single `[ Back ]` button returns to the form; no
                    // App interaction needed, so handle it in place.
                    self.phase = ExportPhase::Options;
                    ExportResponse::Continue
                }
                _ => ExportResponse::Continue,
            },
        }
    }

    /// Hit-test a click at terminal `(col, row)` against the rects cached by
    /// the last render and route it through the same [`ExportResponse`]
    /// surface as the keyboard.  An `esc` close-hint click cancels in every
    /// phase.  In the Options form a control click focuses the field and
    /// applies an `Activate` (flip a toggle / advance the pill); the title
    /// click only focuses; the `[ Export ]` button submits.  The message
    /// phases mirror their button keys.
    pub fn handle_click(&mut self, col: u16, row: u16) -> ExportResponse {
        if rect_contains(self.esc_button_rect, col, row) {
            return ExportResponse::Cancelled;
        }
        match self.phase {
            ExportPhase::Options => self.handle_options_click(col, row),
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
                // The single `[ Back ]` button returns to the form in place,
                // exactly like its Enter arm.
                if rect_contains(self.msg_button_rects.first().copied(), col, row) {
                    self.phase = ExportPhase::Options;
                }
                ExportResponse::Continue
            }
        }
    }

    /// Click routing for the Options form: focus the clicked field, and for
    /// a control apply an `Activate`; the `[ Export ]` button submits.
    fn handle_options_click(&mut self, col: u16, row: u16) -> ExportResponse {
        // A click on a Format row focuses the list and selects that format.
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
        if rect_contains(self.diagrams_rect, col, row) {
            self.focus = OptFocus::Diagrams;
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

    /// Click routing for a message phase's button row: focus and activate the
    /// clicked button via `activate`, mapping its index to a response.
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

    /// Shared key handling for the two-or-one-button message phases.
    /// `count` is the button count; `activate` maps the focused index to a
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
        // PgUp / PgDn / Home / End scroll the body.  Up / Down are
        // deliberately *not* consumed here — they move focus, and the
        // window follows focus at render time.
        if self.scroll_state.handle_paging_key(key) {
            return ExportResponse::Continue;
        }
        match key.code {
            KeyCode::Esc => ExportResponse::Cancelled,
            // Tab / BackTab always move *between* fields, whatever is
            // focused — the Format list is one field to Tab.
            KeyCode::Tab => {
                self.focus = self.focus.next();
                ExportResponse::Continue
            }
            KeyCode::BackTab => {
                self.focus = self.focus.prev();
                ExportResponse::Continue
            }
            // Up / Down move the selection *within* the Format list when it
            // is focused, spilling to the neighbouring field at the ends;
            // plain field navigation everywhere else.
            KeyCode::Down => {
                self.move_focus_down();
                ExportResponse::Continue
            }
            KeyCode::Up => {
                self.move_focus_up();
                ExportResponse::Continue
            }
            KeyCode::Backspace if self.focus == OptFocus::Title => {
                self.title.pop();
                ExportResponse::Continue
            }
            // Enter only exports from the focused `[ Export ]` button; on any
            // other field it advances focus (so a run of Enters walks down to
            // the button) rather than firing the export early or activating a
            // control — the export Enter exception (see docs/controls-refactor).
            KeyCode::Enter => {
                if self.focus == OptFocus::Export {
                    self.submit()
                } else {
                    self.focus = self.focus.next();
                    ExportResponse::Continue
                }
            }
            // Space submits from the button and types into the title; on an
            // option control it falls through to `control_input_for` (Activate).
            KeyCode::Char(' ') if self.focus == OptFocus::Export => self.submit(),
            KeyCode::Char(c) if self.focus == OptFocus::Title => {
                self.push_title_char(c);
                ExportResponse::Continue
            }
            // Left / Right (any field) and Space (option controls) route
            // through the shared control-input mapping → `Control::apply` /
            // `cycle_index`.  Other keys map to `None` and no-op.
            _ => {
                if let Some(input) = control_input_for(key.code) {
                    self.apply_input(input);
                }
                ExportResponse::Continue
            }
        }
    }

    /// Down-arrow behaviour in the Options form: within the Format list when
    /// it is focused (advancing the selection), otherwise to the next field.
    /// At the bottom of the list it spills to the next field, so the list
    /// never feels like a trap.
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

    /// Apply a control input to the focused option field via the shared
    /// transition layer.  Toggles go through [`Control::apply`] (direction-
    /// bound arrows + Activate-flip); the stylesheet pill cycles its index
    /// with [`cycle_index`] because its labels are dynamic (not `'static`),
    /// so it can't be a [`Control::Pill`].  The Format list cycles on
    /// Left/Right (its up/down is the spilling navigation above), ignoring
    /// Activate.  Title / Export ignore this.
    fn apply_input(&mut self, input: ControlInput) {
        match self.focus {
            OptFocus::Format => {
                if !matches!(input, ControlInput::Activate) {
                    self.format_idx =
                        cycle_index(self.format_idx, self.formats.len(), input_delta(input));
                }
            }
            OptFocus::Images => {
                if let ControlEvent::Changed(ControlValue::Toggle(v)) =
                    Control::Toggle.apply(ControlValue::Toggle(self.inline_images), input)
                {
                    self.inline_images = v;
                }
            }
            OptFocus::Diagrams => {
                if let ControlEvent::Changed(ControlValue::Toggle(v)) =
                    Control::Toggle.apply(ControlValue::Toggle(self.render_diagrams), input)
                {
                    self.render_diagrams = v;
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

    /// Append a typed character to the title field, mirroring the paste path:
    /// control chars are dropped and the length is capped at [`TITLE_CHAR_CAP`].
    fn push_title_char(&mut self, c: char) {
        if !c.is_control() && self.title.chars().count() < TITLE_CHAR_CAP {
            self.title.push(c);
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
            render_diagrams: self.render_diagrams,
            stylesheet,
        }
    }

    /// Insert a bracketed paste into the title field.  No-op outside the
    /// Options phase or when the title field isn't focused.  Mirrors the
    /// typing path: control chars stripped, capped at [`TITLE_CHAR_CAP`].
    pub fn paste(&mut self, text: &str) {
        if self.phase != ExportPhase::Options || self.focus != OptFocus::Title {
            return;
        }
        let clean = sanitize_paste(text);
        for c in clean.chars() {
            if self.title.chars().count() >= TITLE_CHAR_CAP {
                break;
            }
            self.title.push(c);
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
                let lines = vec![
                    owned_line("Exported to".to_owned(), self.theme),
                    owned_line(path, self.theme),
                ];
                // The primary button is per-format; the folder one never is.
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
        let labels: [&str; 4] = ["Title", "Inline images", "Inline diagrams", "Stylesheet"];
        let label_w = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        // Own the pill labels so the later `pill_spans` borrow doesn't pin
        // `state` across the `state.esc_button_rect` assignment below.
        let style_labels: Vec<String> = state.stylesheets.iter().map(|(l, _)| l.clone()).collect();
        let style_label_refs: Vec<&str> = style_labels.iter().map(String::as_str).collect();
        let control_w = TITLE_FIELD_WIDTH
            .max(toggle_width())
            .max(pill_width(&style_label_refs));
        let row_w = label_w + 2 + control_w;
        // A toggle's note may be wider than the control rows; size to
        // whichever is widest so an explanation never clips.
        let note_w = [
            IMAGES_NOTE_ON,
            IMAGES_NOTE_OFF,
            DIAGRAMS_NOTE_ON,
            DIAGRAMS_NOTE_OFF,
        ]
        .iter()
        .map(|n| NOTE_INDENT.len() + n.chars().count())
        .max()
        .unwrap_or(0);
        // The Format list: a "Format" label row, then one row per format,
        // indented with a radio marker.  Its width can exceed the control
        // rows when a converter has a long name.
        let format_w = state
            .formats
            .iter()
            .map(|f| LIST_INDENT.len() + MARKER_SELECTED.chars().count() + f.label.chars().count())
            .max()
            .unwrap_or(0);
        let content_width = row_w.max(note_w).max(format_w) as u16;

        // Every hit-rect is re-derived below, from the rows that actually
        // paint.  Clearing first is what keeps a row scrolled out of the
        // window — or dropped by a short terminal — from leaving a
        // clickable ghost at a position nothing is drawn at.
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

        // Resolve the scroll window *before* `draw_frame`, so the frame's
        // chrome sees the post-clamp scroll — the settings-overlay shape.
        // `ensure_visible` is what makes keyboard focus drive the scroll:
        // there is no separate scroll cursor, Tab and the arrows move
        // focus and the window follows.
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
        // Each row's hit-rect spans the full `label + control` run from the
        // body's left edge, so a click on the label operates the control too
        // (matching the settings overlay).  `label_w + 2` is the styled label
        // column (see `render_row`); `control_w` the uniform control width.
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
                    let mut control = vec![Span::styled(" ", value_style)];
                    control.extend(text_field_spans(
                        &state.title,
                        state.title.chars().count(),
                        focused && self.cursor_visible,
                        value_style,
                        self.theme.cursor,
                    ));
                    self.render_row(buf, row_area, "Title", label_w, focused, control);
                    state.title_rect = Some(control_rect(row_area.x, row_area.y, hit_w));
                }
                FormRow::Images => {
                    let focused = state.focus == OptFocus::Images;
                    let control = toggle_spans(state.inline_images, focused, false, self.theme);
                    self.render_row(buf, row_area, "Inline images", label_w, focused, control);
                    state.images_rect = Some(control_rect(row_area.x, row_area.y, hit_w));
                }
                FormRow::ImagesNote => {
                    self.render_note(buf, row_area, images_note(state.inline_images));
                }
                FormRow::Diagrams => {
                    let focused = state.focus == OptFocus::Diagrams;
                    let control = toggle_spans(state.render_diagrams, focused, false, self.theme);
                    self.render_row(buf, row_area, "Inline diagrams", label_w, focused, control);
                    state.diagrams_rect = Some(control_rect(row_area.x, row_area.y, hit_w));
                }
                FormRow::DiagramsNote => {
                    self.render_note(buf, row_area, diagrams_note(state.render_diagrams));
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
                    // Clone the label out before touching `format_rects`:
                    // the paint borrows `state.formats` immutably and the
                    // rect push needs it mutably.
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

        // Pinned footer: a spacer, then the button row.  `[ Export ]` sits
        // *outside* the scroll window on purpose — it is the one control
        // the form cannot be completed without, and the list above it is
        // as long as the user's converter list.
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
            // Focused row: a filled block spanning marker + label, padded
            // so the fill reads as one affordance.
            let text = format!("{LIST_INDENT}{marker}{label}");
            let pad = (area.width as usize).saturating_sub(text.chars().count());
            vec![Span::styled(
                format!("{text}{}", " ".repeat(pad)),
                controls::focused_style(self.theme),
            )]
        } else if selected {
            // Selected but unfocused: emphasise the marker glyph only.
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
        // `label_w + 2` reserves the 2-cell gap between label and control as
        // part of the (styled) label column, so a focused row's fill spans
        // label → widget — the unified control-row composition.
        let spans = control_row_spans(label, label_w + 2, control, focused, false, self.theme);
        Paragraph::new(Line::from(spans))
            .style(self.theme.modal_bg)
            .render(area, buf);
    }

    /// Render a muted, indented explanatory note for the row above, into
    /// `area`.  Styled like the settings overlay's descriptions
    /// (`modal_description`).
    fn render_note(&self, buf: &mut Buffer, area: Rect, text: &str) {
        Paragraph::new(Line::from(Span::styled(
            format!("{NOTE_INDENT}{text}"),
            self.theme.modal_description,
        )))
        .style(self.theme.modal_bg)
        .render(area, buf);
    }

    /// Render a centered message body plus an optional button row.  Shared
    /// by every non-form phase.
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
        // A message phase owns none of the form's rects; clearing here
        // keeps a click after a phase change from landing on a control the
        // form painted before the flow moved on.
        state.clear_hit_rects();
        let has_buttons = !buttons.is_empty();
        let natural_line_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
        let buttons_w = if has_buttons {
            button_row_width(buttons)
        } else {
            0
        };
        // Cap the body width so a long message wraps instead of being
        // truncated at the modal edge — a converter's error text (a
        // weasyprint CSS complaint, a pandoc stderr dump) is unbounded and
        // used to run straight off the frame.  Bounded by the button row
        // below (so a short message never squeezes the buttons) and by the
        // prose cap above, exactly like a prose `ModalView`.
        let content_w = natural_line_w
            .min(PROSE_CONTENT_WIDTH.max(buttons_w))
            .max(buttons_w);
        // Reserve the body's *wrapped* height, measured at the inner width
        // the frame will actually hand back.  Mirror `draw_frame`'s
        // padding rule (`compute_pad_h` at the prospective modal width) so
        // the rows reserved here match the rows painted below; a flat
        // `lines.len()` left a wrapped error clipped off the bottom.
        let prospective_modal_w = content_w.saturating_add(2 * MAX_PAD_H).min(area.width);
        let prospective_pad_h = compute_pad_h(prospective_modal_w, content_w, MAX_PAD_H);
        let body_render_w = prospective_modal_w
            .saturating_sub(2 * prospective_pad_h)
            .max(1);
        let body_rows = wrapped_rows(&lines, body_render_w);
        // Rows the footer needs once it has wrapped, asked at the width
        // the frame will actually give it: `[ Open in browser ]  [ Open
        // folder ]` is 38 columns, so a terminal under about 40 puts the
        // pair on two rows and the modal has to be a row taller for it.
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
            // Each message line is a logical paragraph that may wrap over
            // several rows (a long error, a deep path), so advance `y` by
            // how many rows it actually occupies at the inner width — and
            // render it with the same `Wrap { trim: false }` the height
            // was measured against.
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

fn owned_line(text: String, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text, theme.modal_item))
}

/// Current-state note for the "Inline images" toggle.
fn images_note(on: bool) -> &'static str {
    if on {
        IMAGES_NOTE_ON
    } else {
        IMAGES_NOTE_OFF
    }
}

/// Current-state note for the "Inline diagrams" toggle.
fn diagrams_note(on: bool) -> &'static str {
    if on {
        DIAGRAMS_NOTE_ON
    } else {
        DIAGRAMS_NOTE_OFF
    }
}

/// One-cell-high control hit-rect at `(x, y)` spanning `width` cells.
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
        // Two formats so the Format list is exercised: HTML and one custom.
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
            OptFocus::Diagrams,
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
    /// focused, spilling to the neighbouring field at the ends.
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

        // Enter off the button advances focus instead of exporting —
        // Stylesheet → Format → Export.
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
                render_diagrams: true,
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
    fn success_phase_buttons_open_browser_and_folder() {
        let mut s = state();
        s.set_success(PathBuf::from("/docs/guide.html"));
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
        let mut s = state(); // inline_images off, render_diagrams on
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
        // Each toggle carries a note reflecting its current state: images is
        // off, diagrams on.
        assert!(
            content.contains(IMAGES_NOTE_OFF),
            "images-off note: {content}"
        );
        assert!(
            content.contains(DIAGRAMS_NOTE_ON),
            "diagrams-on note: {content}"
        );
    }

    #[test]
    fn toggle_note_swaps_with_state() {
        assert_eq!(images_note(true), IMAGES_NOTE_ON);
        assert_eq!(images_note(false), IMAGES_NOTE_OFF);
        assert_eq!(diagrams_note(true), DIAGRAMS_NOTE_ON);
        assert_eq!(diagrams_note(false), DIAGRAMS_NOTE_OFF);
    }

    /// Render the modal once into a headless backend so the click hit-rects
    /// are populated on `s`.
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
        s.set_success(PathBuf::from("/docs/guide.html"));
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

    /// A converter's error text is unbounded (a weasyprint CSS complaint,
    /// a pandoc stderr dump), so the Error phase must *wrap* it rather than
    /// run it off the modal edge.  The regression: a long message rendered
    /// on one un-wrapped row, so everything past the frame was truncated —
    /// exactly the part a user needs to diagnose the failure.  Assert both
    /// the head and a sentinel at the very tail are on screen.
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
        s.set_success(PathBuf::from("/docs/guide.html"));
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

    /// The Format list shows every configured format, and the rest of the
    /// form is shared across all of them — the intermediate HTML is what a
    /// converter reads, so every option still applies.  If a format ever
    /// grew its own fields it would mean per-target branching had crept
    /// into the widget, which the shared form exists to prevent.
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
        for expected in ["Title", "Inline images", "Inline diagrams", "Stylesheet"] {
            assert!(content.contains(expected), "{expected} missing: {content}");
        }
    }

    /// A state with the given formats and `format_idx` selected.
    fn state_with_selected(formats: Vec<ExportFormat>, idx: usize) -> ExportState {
        let mut s = ExportState::new(formats, String::new(), false, true, vec![], 0);
        s.format_idx = idx;
        s
    }

    /// The success phase names what it will actually open, per the chosen
    /// format.  HTML goes to a browser; a `.pdf` goes to whatever the OS
    /// associates with it, so with the custom format selected the button
    /// must not promise a browser.
    #[test]
    fn the_success_button_names_the_selected_format() {
        let mut s = state_with_selected(vec![ExportFormat::html(), ExportFormat::custom("PDF")], 1);
        s.set_success(PathBuf::from("/docs/guide.pdf"));
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

    /// Both success buttons keep working whichever format is selected: the
    /// response surface is shared, so a click must not depend on the label.
    #[test]
    fn custom_success_buttons_still_resolve_to_open_and_folder() {
        let mut s = state_with_selected(vec![ExportFormat::html(), ExportFormat::custom("PDF")], 1);
        s.set_success(PathBuf::from("/docs/guide.pdf"));
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

    /// The variable-length part of the form is last, so every fixed control
    /// sits above it and a long converter list can only push *itself* out of
    /// view.  `[ Export ]` is not a body row at all — it is pinned below the
    /// scroll window.
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
            FormRow::Diagrams,
            FormRow::Stylesheet,
        ] {
            assert!(rows.iter().position(|r| *r == control).unwrap() < list_start);
        }
    }

    /// A toggle is revealed *after* its note, so when only one of the two
    /// fits it is the control that stays on screen.
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

    /// The regression this scroll exists for: with more converters than the
    /// terminal has rows, `[ Export ]` must still be painted and clickable.
    /// Before the pinned footer it was simply dropped off the bottom, while
    /// its stale hit-rect stayed live.
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

    /// A control the scroll window left off screen leaves **no** hit-rect
    /// behind.  A stale rect is the one failure a clipped control can
    /// produce that lies: the row is invisible but still answers clicks, at
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

    /// A click on a scrolled list resolves to the format actually under the
    /// pointer.  The rects carry their own index precisely because the
    /// painted window does not start at format 0.
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

    /// PgDn / PgUp page the body; Up / Down are left alone so they can keep
    /// moving focus (and the window follows focus at render time).
    #[test]
    fn paging_keys_scroll_but_arrows_still_move_focus() {
        let mut s = state_with_formats(12);
        render_modal(&mut s, 70, 16);

        s.handle_key(&key(KeyCode::PageDown));
        assert!(s.scroll_state.scroll > 0, "PgDn pages the body");
        s.handle_key(&key(KeyCode::Home));
        assert_eq!(s.scroll_state.scroll, 0);

        // Down still moves focus off the title, not the scroll.
        assert_eq!(s.focus, OptFocus::Title);
        s.handle_key(&key(KeyCode::Down));
        assert_eq!(s.focus, OptFocus::Images);
        assert_eq!(s.scroll_state.scroll, 0);
    }

    /// Moving focus back up from the list scrolls the fixed controls back
    /// into view — the window follows focus in both directions.
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
