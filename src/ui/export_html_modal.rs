//! Options form and phase machine for `Action::ExportHtml`.
//!
//! A single modal that walks through several phases without ever leaving
//! the stack, so the async export, the overwrite confirmation, and the
//! "open the result" buttons all live in one dismissable place:
//!
//! * **Options** — a `Title` text field, two toggles (`Inline images`,
//!   `Inline diagrams`), and a `Stylesheet` pill, above a lone
//!   `[ Export ]` button (Esc dismisses; there is no Cancel button).
//!   Each setting is separated by a spacer; each toggle carries a muted
//!   note describing its current (On/Off) state.  Enter exports only from
//!   the focused button — on any other field it advances focus.
//! * **ConfirmOverwrite** — shown only when the target already exists.
//! * **Exporting** — a static "Exporting…" notice while the worker runs.
//! * **Success** — the written path plus `[ Open in browser ]` /
//!   `[ Open folder ]`.
//! * **Error** — the failure message plus `[ Back ]` to the form.
//!
//! The widget is UI-only.  All side effects (persisting the chosen options
//! to config, spawning the export worker, opening the result) happen in the
//! App-layer adapter `crate::app::modal::export_html`, which reads the
//! values off this state when [`ExportHtmlResponse`] fires and drives the
//! phase transitions via the `enter_*` / `set_*` helpers.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
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
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind, MAX_PAD_H,
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
const OVERWRITE_BUTTONS: &[&str] = &["Overwrite", "Cancel"];
const SUCCESS_BUTTONS: &[&str] = &["Open in browser", "Open folder"];
const ERROR_BUTTONS: &[&str] = &["Back"];

/// Which step of the export flow the modal is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportPhase {
    Options,
    ConfirmOverwrite,
    Exporting,
    Success,
    Error,
}

/// Focus targets within the Options form, in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptFocus {
    Title,
    Images,
    Diagrams,
    Stylesheet,
    Export,
}

impl OptFocus {
    const ORDER: [OptFocus; 5] = [
        OptFocus::Title,
        OptFocus::Images,
        OptFocus::Diagrams,
        OptFocus::Stylesheet,
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

/// Outcome of dispatching a key to [`ExportHtmlState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportHtmlResponse {
    /// Stay open; the caller just redraws.
    Continue,
    /// Dismiss the modal (Esc or a Cancel button, at any phase).
    Cancelled,
    /// Options `[ Export ]` activated with these choices.
    Submit(ExportChoices),
    /// Overwrite confirmed — proceed with the export.
    ProceedOverwrite,
    /// Success-phase `[ Open in browser ]`.
    OpenInBrowser,
    /// Success-phase `[ Open folder ]`.
    OpenFolder,
}

/// Mutable state for an open Export HTML modal.
pub struct ExportHtmlState {
    pub phase: ExportPhase,
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
    // ── Click hit-rects, captured each render ──
    /// Options-form control rects (None until the form has rendered once).
    title_rect: Option<Rect>,
    images_rect: Option<Rect>,
    diagrams_rect: Option<Rect>,
    stylesheet_rect: Option<Rect>,
    export_button_rect: Option<Rect>,
    /// Button-row rects for the current message phase (overwrite / success /
    /// error), in button order.
    msg_button_rects: Vec<Rect>,
}

impl ExportHtmlState {
    /// Build the form, seeded from config plus a discovered stylesheet list.
    pub fn new(
        title: String,
        inline_images: bool,
        render_diagrams: bool,
        stylesheets: Vec<(String, String)>,
        stylesheet_idx: usize,
    ) -> Self {
        Self {
            phase: ExportPhase::Options,
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
            title_rect: None,
            images_rect: None,
            diagrams_rect: None,
            stylesheet_rect: None,
            export_button_rect: None,
            msg_button_rects: Vec::new(),
        }
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

    pub fn handle_key(&mut self, key: &KeyEvent) -> ExportHtmlResponse {
        // Ignore modifier chords so the user can press Ctrl-S etc. without
        // polluting the title field — Esc (no modifier) still gets through.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return ExportHtmlResponse::Continue;
        }
        match self.phase {
            ExportPhase::Options => self.handle_options_key(key),
            ExportPhase::ConfirmOverwrite => self.handle_button_key(key, 2, |idx| {
                if idx == 0 {
                    ExportHtmlResponse::ProceedOverwrite
                } else {
                    ExportHtmlResponse::Cancelled
                }
            }),
            ExportPhase::Exporting => match key.code {
                KeyCode::Esc => ExportHtmlResponse::Cancelled,
                _ => ExportHtmlResponse::Continue,
            },
            ExportPhase::Success => self.handle_button_key(key, 2, |idx| {
                if idx == 0 {
                    ExportHtmlResponse::OpenInBrowser
                } else {
                    ExportHtmlResponse::OpenFolder
                }
            }),
            ExportPhase::Error => match key.code {
                KeyCode::Esc => ExportHtmlResponse::Cancelled,
                KeyCode::Enter | KeyCode::Char(' ') => {
                    // The single `[ Back ]` button returns to the form; no
                    // App interaction needed, so handle it in place.
                    self.phase = ExportPhase::Options;
                    ExportHtmlResponse::Continue
                }
                _ => ExportHtmlResponse::Continue,
            },
        }
    }

    /// Hit-test a click at terminal `(col, row)` against the rects cached by
    /// the last render and route it through the same [`ExportHtmlResponse`]
    /// surface as the keyboard.  An `esc` close-hint click cancels in every
    /// phase.  In the Options form a control click focuses the field and
    /// applies an `Activate` (flip a toggle / advance the pill); the title
    /// click only focuses; the `[ Export ]` button submits.  The message
    /// phases mirror their button keys.
    pub fn handle_click(&mut self, col: u16, row: u16) -> ExportHtmlResponse {
        if rect_contains(self.esc_button_rect, col, row) {
            return ExportHtmlResponse::Cancelled;
        }
        match self.phase {
            ExportPhase::Options => self.handle_options_click(col, row),
            ExportPhase::ConfirmOverwrite => self.handle_message_click(col, row, |idx| {
                if idx == 0 {
                    ExportHtmlResponse::ProceedOverwrite
                } else {
                    ExportHtmlResponse::Cancelled
                }
            }),
            ExportPhase::Exporting => ExportHtmlResponse::Continue,
            ExportPhase::Success => self.handle_message_click(col, row, |idx| {
                if idx == 0 {
                    ExportHtmlResponse::OpenInBrowser
                } else {
                    ExportHtmlResponse::OpenFolder
                }
            }),
            ExportPhase::Error => {
                // The single `[ Back ]` button returns to the form in place,
                // exactly like its Enter arm.
                if rect_contains(self.msg_button_rects.first().copied(), col, row) {
                    self.phase = ExportPhase::Options;
                }
                ExportHtmlResponse::Continue
            }
        }
    }

    /// Click routing for the Options form: focus the clicked field, and for
    /// a control apply an `Activate`; the `[ Export ]` button submits.
    fn handle_options_click(&mut self, col: u16, row: u16) -> ExportHtmlResponse {
        if rect_contains(self.title_rect, col, row) {
            self.focus = OptFocus::Title;
            return ExportHtmlResponse::Continue;
        }
        if rect_contains(self.images_rect, col, row) {
            self.focus = OptFocus::Images;
            self.apply_input(ControlInput::Activate);
            return ExportHtmlResponse::Continue;
        }
        if rect_contains(self.diagrams_rect, col, row) {
            self.focus = OptFocus::Diagrams;
            self.apply_input(ControlInput::Activate);
            return ExportHtmlResponse::Continue;
        }
        if rect_contains(self.stylesheet_rect, col, row) {
            self.focus = OptFocus::Stylesheet;
            self.apply_input(ControlInput::Activate);
            return ExportHtmlResponse::Continue;
        }
        if rect_contains(self.export_button_rect, col, row) {
            self.focus = OptFocus::Export;
            return self.submit();
        }
        ExportHtmlResponse::Continue
    }

    /// Click routing for a message phase's button row: focus and activate the
    /// clicked button via `activate`, mapping its index to a response.
    fn handle_message_click(
        &mut self,
        col: u16,
        row: u16,
        activate: impl Fn(usize) -> ExportHtmlResponse,
    ) -> ExportHtmlResponse {
        for (i, r) in self.msg_button_rects.iter().enumerate() {
            if rect_contains(Some(*r), col, row) {
                self.btn_focus = i;
                return activate(i);
            }
        }
        ExportHtmlResponse::Continue
    }

    /// Shared key handling for the two-or-one-button message phases.
    /// `count` is the button count; `activate` maps the focused index to a
    /// response when Enter / Space fires.
    fn handle_button_key(
        &mut self,
        key: &KeyEvent,
        count: usize,
        activate: impl Fn(usize) -> ExportHtmlResponse,
    ) -> ExportHtmlResponse {
        match key.code {
            KeyCode::Esc => ExportHtmlResponse::Cancelled,
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab if count > 1 => {
                let delta = if matches!(key.code, KeyCode::Left | KeyCode::BackTab) {
                    count - 1
                } else {
                    1
                };
                self.btn_focus = (self.btn_focus + delta) % count;
                ExportHtmlResponse::Continue
            }
            KeyCode::Enter | KeyCode::Char(' ') => activate(self.btn_focus),
            _ => ExportHtmlResponse::Continue,
        }
    }

    fn handle_options_key(&mut self, key: &KeyEvent) -> ExportHtmlResponse {
        match key.code {
            KeyCode::Esc => ExportHtmlResponse::Cancelled,
            KeyCode::Tab | KeyCode::Down => {
                self.focus = self.focus.next();
                ExportHtmlResponse::Continue
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.focus = self.focus.prev();
                ExportHtmlResponse::Continue
            }
            KeyCode::Backspace if self.focus == OptFocus::Title => {
                self.title.pop();
                ExportHtmlResponse::Continue
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
                    ExportHtmlResponse::Continue
                }
            }
            // Space submits from the button and types into the title; on an
            // option control it falls through to `control_input_for` (Activate).
            KeyCode::Char(' ') if self.focus == OptFocus::Export => self.submit(),
            KeyCode::Char(c) if self.focus == OptFocus::Title => {
                self.push_title_char(c);
                ExportHtmlResponse::Continue
            }
            // Left / Right (any field) and Space (option controls) route
            // through the shared control-input mapping → `Control::apply` /
            // `cycle_index`.  Other keys map to `None` and no-op.
            _ => {
                if let Some(input) = control_input_for(key.code) {
                    self.apply_input(input);
                }
                ExportHtmlResponse::Continue
            }
        }
    }

    /// Apply a control input to the focused option field via the shared
    /// transition layer.  Toggles go through [`Control::apply`] (direction-
    /// bound arrows + Activate-flip); the stylesheet pill cycles its index
    /// with [`cycle_index`] because its labels are dynamic (not `'static`),
    /// so it can't be a [`Control::Pill`].  Title / Export ignore this.
    fn apply_input(&mut self, input: ControlInput) {
        match self.focus {
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

    fn submit(&mut self) -> ExportHtmlResponse {
        let choices = self.choices();
        self.submitted_title = choices.title.clone();
        ExportHtmlResponse::Submit(choices)
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
pub struct ExportHtmlView<'a> {
    pub theme: &'a Theme,
    pub cursor_visible: bool,
}

impl<'a> StatefulWidget for ExportHtmlView<'a> {
    type State = ExportHtmlState;

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
                    "Export HTML",
                    ModalKind::Warning,
                    lines,
                    OVERWRITE_BUTTONS,
                );
            }
            ExportPhase::Exporting => {
                let lines = vec![owned_line("Exporting…".to_owned(), self.theme)];
                self.render_message(
                    area,
                    buf,
                    state,
                    "Export HTML",
                    ModalKind::Normal,
                    lines,
                    &[],
                );
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
                self.render_message(
                    area,
                    buf,
                    state,
                    "Export complete",
                    ModalKind::Normal,
                    lines,
                    SUCCESS_BUTTONS,
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
                    "Export HTML",
                    ModalKind::Error,
                    lines,
                    ERROR_BUTTONS,
                );
            }
        }
    }
}

impl<'a> ExportHtmlView<'a> {
    fn render_options(&self, area: Rect, buf: &mut Buffer, state: &mut ExportHtmlState) {
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
        let content_width = row_w.max(note_w) as u16;
        // 4 settings + a note under each toggle (2) + a spacer between
        // settings (3) + the pre-button spacer (1) + the button row (1).
        let total_rows = 4 + 2 + 3 + 1 + 1;
        let content = ContentSize {
            width: content_width.max(button_row_width(OPTION_BUTTONS)),
            height: 0,
            pinned_top: total_rows,
            pinned_bottom: 0,
            ..Default::default()
        };
        let modal_area = centered_rect_for_content(content, area);
        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title: "Export HTML",
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

        let mut y = inner.y;
        let bottom = inner.y + inner.height;

        // Each row's hit-rect spans the full `label + control` run from the
        // body's left edge, so a click on the label operates the control too
        // (matching the settings overlay).  `label_w + 2` is the styled label
        // column (see `render_row`); `control_w` the uniform control width.
        let row_x = inner.x;
        let row_hit_w = ((label_w + 2 + control_w) as u16).min(inner.width);
        // A fresh form render owns no message-phase buttons.
        state.msg_button_rects.clear();

        // Title row.
        let title_y = y;
        let title_focused = state.focus == OptFocus::Title;
        let value_style = controls::text_value_style(title_focused, self.theme);
        let mut title_control = vec![Span::styled(" ", value_style)];
        title_control.extend(text_field_spans(
            &state.title,
            state.title.chars().count(),
            title_focused && self.cursor_visible,
            value_style,
            self.theme.cursor,
        ));
        self.render_row(
            buf,
            inner,
            &mut y,
            bottom,
            "Title",
            label_w,
            title_focused,
            title_control,
        );
        state.title_rect = Some(control_rect(row_x, title_y, row_hit_w));
        y = y.saturating_add(1); // spacer between settings

        // Inline images toggle, with a note describing its current state.
        let images_y = y;
        let images_focused = state.focus == OptFocus::Images;
        self.render_row(
            buf,
            inner,
            &mut y,
            bottom,
            "Inline images",
            label_w,
            images_focused,
            toggle_spans(state.inline_images, images_focused, false, self.theme),
        );
        state.images_rect = Some(control_rect(row_x, images_y, row_hit_w));
        self.render_note(buf, inner, &mut y, bottom, images_note(state.inline_images));
        y = y.saturating_add(1); // spacer between settings

        // Inline diagrams toggle, likewise annotated.
        let diagrams_y = y;
        let diagrams_focused = state.focus == OptFocus::Diagrams;
        self.render_row(
            buf,
            inner,
            &mut y,
            bottom,
            "Inline diagrams",
            label_w,
            diagrams_focused,
            toggle_spans(state.render_diagrams, diagrams_focused, false, self.theme),
        );
        state.diagrams_rect = Some(control_rect(row_x, diagrams_y, row_hit_w));
        self.render_note(
            buf,
            inner,
            &mut y,
            bottom,
            diagrams_note(state.render_diagrams),
        );
        y = y.saturating_add(1); // spacer between settings

        // Stylesheet pill row.
        let stylesheet_y = y;
        let style_focused = state.focus == OptFocus::Stylesheet;
        self.render_row(
            buf,
            inner,
            &mut y,
            bottom,
            "Stylesheet",
            label_w,
            style_focused,
            pill_spans(
                &style_label_refs,
                state.stylesheet_idx,
                style_focused,
                false,
                self.theme,
            ),
        );
        state.stylesheet_rect = Some(control_rect(row_x, stylesheet_y, row_hit_w));

        // Spacer, then the button row.
        y = y.saturating_add(1);
        if y >= bottom {
            return;
        }
        let button_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        };
        let focused_idx = match state.focus {
            OptFocus::Export => 0,
            _ => usize::MAX,
        };
        let rects = render_button_row(button_area, buf, OPTION_BUTTONS, focused_idx, self.theme);
        state.export_button_rect = rects.into_iter().next();
    }

    /// Render one `label  <control>` row at `*y`, advancing `*y` past it.
    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &self,
        buf: &mut Buffer,
        inner: Rect,
        y: &mut u16,
        bottom: u16,
        label: &str,
        label_w: usize,
        focused: bool,
        control: Vec<Span<'static>>,
    ) {
        if *y >= bottom {
            return;
        }
        let area = Rect {
            x: inner.x,
            y: *y,
            width: inner.width,
            height: 1,
        };
        // `label_w + 2` reserves the 2-cell gap between label and control as
        // part of the (styled) label column, so a focused row's fill spans
        // label → widget — the unified control-row composition.
        let spans = control_row_spans(label, label_w + 2, control, focused, false, self.theme);
        Paragraph::new(Line::from(spans))
            .style(self.theme.modal_bg)
            .render(area, buf);
        *y = y.saturating_add(1);
    }

    /// Render a muted, indented explanatory note for the row above, at `*y`,
    /// advancing `*y` past it.  Styled like the settings overlay's
    /// descriptions (`modal_description`).
    fn render_note(&self, buf: &mut Buffer, inner: Rect, y: &mut u16, bottom: u16, text: &str) {
        if *y >= bottom {
            return;
        }
        let area = Rect {
            x: inner.x,
            y: *y,
            width: inner.width,
            height: 1,
        };
        Paragraph::new(Line::from(Span::styled(
            format!("{NOTE_INDENT}{text}"),
            self.theme.modal_description,
        )))
        .style(self.theme.modal_bg)
        .render(area, buf);
        *y = y.saturating_add(1);
    }

    /// Render a centered message body plus an optional button row.  Shared
    /// by every non-form phase.
    #[allow(clippy::too_many_arguments)]
    fn render_message(
        &self,
        area: Rect,
        buf: &mut Buffer,
        state: &mut ExportHtmlState,
        title: &str,
        kind: ModalKind,
        lines: Vec<Line<'static>>,
        buttons: &[&str],
    ) {
        let has_buttons = !buttons.is_empty();
        let line_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
        let buttons_w = if has_buttons {
            button_row_width(buttons)
        } else {
            0
        };
        let content_w = line_w.max(buttons_w);
        // Rows the footer needs once it has wrapped, asked at the width
        // the frame will actually give it: `[ Open in browser ]  [ Open
        // folder ]` is 38 columns, so a terminal under about 40 puts the
        // pair on two rows and the modal has to be a row taller for it.
        let footer_rows = if has_buttons {
            footer_row_count(buttons, content_w, area.width, MAX_PAD_H)
        } else {
            0
        };
        let body_h = lines.len() as u16 + if has_buttons { 1 + footer_rows } else { 0 };
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
            let row = Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            };
            Paragraph::new(line)
                .alignment(Alignment::Center)
                .style(self.theme.modal_bg)
                .render(row, buf);
            y = y.saturating_add(1);
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

    fn state() -> ExportHtmlState {
        ExportHtmlState::new(
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
        assert_eq!(s.focus, OptFocus::Title);
        for expected in [
            OptFocus::Images,
            OptFocus::Diagrams,
            OptFocus::Stylesheet,
            OptFocus::Export,
            OptFocus::Title,
        ] {
            s.handle_key(&key(KeyCode::Tab));
            assert_eq!(s.focus, expected);
        }
    }

    #[test]
    fn typing_appends_to_title_only_when_focused() {
        let mut s = state();
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

        // Enter off the button advances focus instead of exporting.
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            ExportHtmlResponse::Continue
        );
        assert_eq!(s.focus, OptFocus::Export);
        // Now on the button, Enter exports.
        let resp = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(
            resp,
            ExportHtmlResponse::Submit(ExportChoices {
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
            ExportHtmlResponse::Submit(c) => assert_eq!(c.title, None),
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
                ExportHtmlResponse::Cancelled,
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
            ExportHtmlResponse::ProceedOverwrite
        );
        // Move to Cancel and activate.
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            ExportHtmlResponse::Cancelled
        );
    }

    #[test]
    fn success_phase_buttons_open_browser_and_folder() {
        let mut s = state();
        s.set_success(PathBuf::from("/docs/guide.html"));
        assert_eq!(s.phase, ExportPhase::Success);
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            ExportHtmlResponse::OpenInBrowser
        );
        s.handle_key(&key(KeyCode::Right));
        assert_eq!(
            s.handle_key(&key(KeyCode::Enter)),
            ExportHtmlResponse::OpenFolder
        );
    }

    #[test]
    fn error_back_returns_to_options() {
        let mut s = state();
        s.set_error("boom".to_owned());
        assert_eq!(s.phase, ExportPhase::Error);
        let resp = s.handle_key(&key(KeyCode::Enter));
        assert_eq!(resp, ExportHtmlResponse::Continue);
        assert_eq!(s.phase, ExportPhase::Options);
    }

    #[test]
    fn paste_only_lands_in_focused_title() {
        let mut s = state();
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
                let view = ExportHtmlView {
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
        assert!(content.contains("Export HTML"), "title: {content}");
        assert!(content.contains("Inline images"), "images row: {content}");
        assert!(content.contains("Stylesheet"), "stylesheet row: {content}");
        assert!(content.contains("Export"), "export button: {content}");
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
    fn render_modal(s: &mut ExportHtmlState, w: u16, h: u16) {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let view = ExportHtmlView {
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
        assert_eq!(resp, ExportHtmlResponse::Continue);
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
        assert!(matches!(resp, ExportHtmlResponse::Submit(_)));
        assert_eq!(s.focus, OptFocus::Export);
    }

    #[test]
    fn click_on_esc_hint_cancels() {
        let mut s = state();
        render_modal(&mut s, 70, 22);
        let r = s.esc_button_rect.expect("esc hint rect captured at render");
        assert_eq!(s.handle_click(r.x, r.y), ExportHtmlResponse::Cancelled);
    }

    #[test]
    fn click_on_success_buttons_opens_browser_and_folder() {
        let mut s = state();
        s.set_success(PathBuf::from("/docs/guide.html"));
        render_modal(&mut s, 70, 14);
        let browser = s.msg_button_rects[0];
        assert_eq!(
            s.handle_click(browser.x, browser.y),
            ExportHtmlResponse::OpenInBrowser
        );
        let folder = s.msg_button_rects[1];
        assert_eq!(
            s.handle_click(folder.x, folder.y),
            ExportHtmlResponse::OpenFolder
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
                let view = ExportHtmlView {
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
}
