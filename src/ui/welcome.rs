//! First-run welcome modal.
//!
//! Built on the `scroll_container` chrome primitives like
//! [`crate::ui::theme_picker`] rather than the simpler `ModalView`,
//! because the body contains interactive tri-state pill rows and a
//! click-through theme button that aren't expressible as a flat
//! body+button-row layout.
//!
//! The widget is UI-only.  The adapter
//! `src/app/modal/welcome.rs` wires the responses back into `App`:
//! Save persists everything; the Theme button pushes the theme picker
//! onto the modal stack so it stacks ON TOP of the welcome and pops
//! back to it on close.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::config::{DiagramsEnabled, ImagesEnabled, RemoteImagePolicy, Theme};
use crate::terminal::{Capabilities, ColourDepth, ImageProtocol};
use crate::ui::scroll_container::{
    centered_rect_for_content, draw_frame, ContentSize, FrameOpts, ModalKind,
};

/// One focusable row on the welcome modal.  Order matches the on-screen
/// vertical order; Tab cycles forward, Shift-Tab cycles backward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WelcomeFocus {
    Theme,
    Images,
    RemoteImages,
    Diagrams,
    ShowAgain,
    Save,
}

const FOCUS_ORDER: &[WelcomeFocus] = &[
    WelcomeFocus::Theme,
    WelcomeFocus::Images,
    WelcomeFocus::RemoteImages,
    WelcomeFocus::Diagrams,
    WelcomeFocus::ShowAgain,
    WelcomeFocus::Save,
];

/// Outcome of dispatching a key/click to the welcome modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WelcomeResponse {
    Continue,
    /// User clicked / activated the Theme button.  Caller should open
    /// the theme picker on top of this modal.
    OpenThemePicker,
    /// User pressed Save.  Caller should persist the choices the state
    /// exposes and dismiss the modal.
    Save,
}

/// Live state of the welcome modal — the in-flight tri-state choices
/// plus focus / hit-rect bookkeeping.  The active theme name is read
/// straight off `config.theme` at render time; the picker mutates that
/// directly so we never need to mirror it here.
pub struct WelcomeState {
    pub focused: WelcomeFocus,
    pub images: ImagesEnabled,
    pub remote: RemoteImagePolicy,
    pub diagrams: DiagramsEnabled,
    /// "Don't show this again" toggle.  Default `true` per spec — Save
    /// writes `show_welcome = false` (the modal won't reappear on next
    /// launch) unless the user opts back in by unchecking this box.
    pub dont_show_again: bool,
    /// True when `caps.image_protocol.is_some()` — image/remote/diagram
    /// rows are interactive only when this is true.  Captured at
    /// construction so the modal's behaviour doesn't drift when the
    /// underlying `Capabilities` are queried from a callback that
    /// doesn't have access to them.
    pub image_capable: bool,
    /// Cached "remote was X before cascade" so flipping Images out of
    /// Never restores the user's prior remote choice.
    pre_cascade_remote: Option<RemoteImagePolicy>,

    // ── Hit-test rects, captured each render for click dispatch ──
    pub theme_button_rect: Option<Rect>,
    pub esc_button_rect: Option<Rect>,
    pub images_pill_rects: [Option<Rect>; 3],
    pub remote_pill_rects: [Option<Rect>; 3],
    pub diagrams_pill_rects: [Option<Rect>; 3],
    pub show_again_rect: Option<Rect>,
    pub save_button_rect: Option<Rect>,

    // ── Capability summary, captured at construction ──
    cap_summary: CapSummary,
}

#[derive(Debug, Clone)]
struct CapSummary {
    colour: String,
    colour_ok: bool,
    images: String,
    images_ok: bool,
    mouse: String,
    mouse_ok: bool,
}

impl WelcomeState {
    /// Construct fresh state from detected `caps` and the current
    /// `config` tri-state values.
    pub fn new(
        caps: &Capabilities,
        images: ImagesEnabled,
        remote: RemoteImagePolicy,
        diagrams: DiagramsEnabled,
    ) -> Self {
        Self {
            focused: WelcomeFocus::Theme,
            images,
            remote,
            diagrams,
            dont_show_again: true,
            image_capable: caps.image_protocol.is_some(),
            pre_cascade_remote: None,
            theme_button_rect: None,
            esc_button_rect: None,
            images_pill_rects: [None, None, None],
            remote_pill_rects: [None, None, None],
            diagrams_pill_rects: [None, None, None],
            show_again_rect: None,
            save_button_rect: None,
            cap_summary: CapSummary::from(caps),
        }
    }

    /// True iff the cascade rule has forced remote to Never because
    /// images is Never.  Rendered greyed-out and skipped by Tab focus.
    fn remote_locked_by_images(&self) -> bool {
        matches!(self.images, ImagesEnabled::Never)
    }

    /// True iff a row is non-interactive — either capability-locked
    /// or cascade-locked.  `RemoteImages` carries both gates.
    fn row_disabled(&self, row: WelcomeFocus) -> bool {
        match row {
            WelcomeFocus::Images | WelcomeFocus::Diagrams => !self.image_capable,
            WelcomeFocus::RemoteImages => !self.image_capable || self.remote_locked_by_images(),
            _ => false,
        }
    }

    /// Step focus by `delta` (-1 for Shift-Tab, +1 for Tab).  Skips
    /// disabled rows so the user never lands on a non-interactive
    /// pill row.
    fn step_focus(&mut self, delta: isize) {
        let len = FOCUS_ORDER.len() as isize;
        let cur = FOCUS_ORDER
            .iter()
            .position(|f| *f == self.focused)
            .unwrap_or(0) as isize;
        // Walk at most `len` steps so we don't loop forever if every row
        // happens to be disabled (can't happen today; defensive).
        for offset in 1..=len {
            let i = ((cur + delta * offset).rem_euclid(len)) as usize;
            let candidate = FOCUS_ORDER[i];
            if !self.row_disabled(candidate) {
                self.focused = candidate;
                return;
            }
        }
    }

    /// Cycle the tri-state value of the focused row by `delta` (-1 / +1).
    /// No-op if focus isn't on a tri-state row.  Applies the cascade
    /// rule when images leaves / enters Never.
    fn cycle_focused(&mut self, delta: isize) {
        match self.focused {
            WelcomeFocus::Images => {
                let next = cycle_images(self.images, delta);
                self.set_images(next);
            }
            WelcomeFocus::RemoteImages if !self.remote_locked_by_images() => {
                self.remote = cycle_remote(self.remote, delta);
            }
            WelcomeFocus::Diagrams => {
                self.diagrams = cycle_diagrams(self.diagrams, delta);
            }
            _ => {}
        }
    }

    fn set_images(&mut self, next: ImagesEnabled) {
        let was_never = matches!(self.images, ImagesEnabled::Never);
        let now_never = matches!(next, ImagesEnabled::Never);
        if !was_never && now_never {
            self.pre_cascade_remote = Some(self.remote);
            self.remote = RemoteImagePolicy::Never;
        } else if was_never && !now_never {
            if let Some(prev) = self.pre_cascade_remote.take() {
                self.remote = prev;
            }
        }
        self.images = next;
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> WelcomeResponse {
        match key.code {
            KeyCode::Tab => {
                self.step_focus(1);
                WelcomeResponse::Continue
            }
            KeyCode::BackTab => {
                self.step_focus(-1);
                WelcomeResponse::Continue
            }
            KeyCode::Down => {
                self.step_focus(1);
                WelcomeResponse::Continue
            }
            KeyCode::Up => {
                self.step_focus(-1);
                WelcomeResponse::Continue
            }
            KeyCode::Left => {
                match self.focused {
                    WelcomeFocus::ShowAgain => self.focused = WelcomeFocus::Save,
                    WelcomeFocus::Save => self.focused = WelcomeFocus::ShowAgain,
                    _ => self.cycle_focused(-1),
                }
                WelcomeResponse::Continue
            }
            KeyCode::Right => {
                match self.focused {
                    WelcomeFocus::ShowAgain => self.focused = WelcomeFocus::Save,
                    WelcomeFocus::Save => self.focused = WelcomeFocus::ShowAgain,
                    _ => self.cycle_focused(1),
                }
                WelcomeResponse::Continue
            }
            KeyCode::Char(' ') => {
                match self.focused {
                    WelcomeFocus::ShowAgain => self.dont_show_again = !self.dont_show_again,
                    WelcomeFocus::Theme => return WelcomeResponse::OpenThemePicker,
                    WelcomeFocus::Save => return WelcomeResponse::Save,
                    WelcomeFocus::Images | WelcomeFocus::RemoteImages | WelcomeFocus::Diagrams => {
                        self.cycle_focused(1)
                    }
                }
                WelcomeResponse::Continue
            }
            KeyCode::Enter => match self.focused {
                WelcomeFocus::Theme => WelcomeResponse::OpenThemePicker,
                WelcomeFocus::Save => WelcomeResponse::Save,
                WelcomeFocus::ShowAgain => {
                    self.dont_show_again = !self.dont_show_again;
                    WelcomeResponse::Continue
                }
                _ => WelcomeResponse::Continue,
            },
            // No Esc dismissal — the spec replaces Cancel with the
            // explicit "Show on next launch" toggle.  Esc is consumed
            // but does nothing so the modal can't be closed without
            // pressing Save (which respects the show-again toggle).
            KeyCode::Esc => WelcomeResponse::Continue,
            _ => WelcomeResponse::Continue,
        }
    }

    /// Hit-test `(col, row)` against the cached rects from the last
    /// render.  Returns the matching response.
    pub fn handle_click(&mut self, col: u16, row: u16) -> WelcomeResponse {
        if rect_contains(self.theme_button_rect, col, row) {
            self.focused = WelcomeFocus::Theme;
            return WelcomeResponse::OpenThemePicker;
        }
        if rect_contains(self.save_button_rect, col, row) {
            self.focused = WelcomeFocus::Save;
            return WelcomeResponse::Save;
        }
        if rect_contains(self.show_again_rect, col, row) {
            self.focused = WelcomeFocus::ShowAgain;
            self.dont_show_again = !self.dont_show_again;
            return WelcomeResponse::Continue;
        }
        if self.image_capable {
            if let Some(idx) = hit_index(&self.images_pill_rects, col, row) {
                self.focused = WelcomeFocus::Images;
                let next = match idx {
                    0 => ImagesEnabled::Ask,
                    1 => ImagesEnabled::Always,
                    _ => ImagesEnabled::Never,
                };
                self.set_images(next);
                return WelcomeResponse::Continue;
            }
            if !self.remote_locked_by_images() {
                if let Some(idx) = hit_index(&self.remote_pill_rects, col, row) {
                    self.focused = WelcomeFocus::RemoteImages;
                    self.remote = match idx {
                        0 => RemoteImagePolicy::Ask,
                        1 => RemoteImagePolicy::Always,
                        _ => RemoteImagePolicy::Never,
                    };
                    return WelcomeResponse::Continue;
                }
            }
            if let Some(idx) = hit_index(&self.diagrams_pill_rects, col, row) {
                self.focused = WelcomeFocus::Diagrams;
                self.diagrams = match idx {
                    0 => DiagramsEnabled::Ask,
                    1 => DiagramsEnabled::Always,
                    _ => DiagramsEnabled::Never,
                };
                return WelcomeResponse::Continue;
            }
        }
        WelcomeResponse::Continue
    }
}

impl CapSummary {
    fn from(caps: &Capabilities) -> Self {
        let (colour, colour_ok) = match caps.colour_depth {
            ColourDepth::TrueColor => ("truecolor (24-bit)".to_owned(), true),
            ColourDepth::Ansi256 => ("256 colours".to_owned(), true),
            ColourDepth::Ansi16 => ("16 colours — themes will look muted".to_owned(), false),
            ColourDepth::NoColour => ("none — plain text only".to_owned(), false),
        };
        let (images, images_ok) = match caps.image_protocol {
            Some(ImageProtocol::KittyGraphics) => ("Kitty graphics".to_owned(), true),
            Some(ImageProtocol::Sixel) => ("Sixel".to_owned(), true),
            Some(ImageProtocol::ITerm2) => ("iTerm2 inline images".to_owned(), true),
            Some(ImageProtocol::Halfblocks) => {
                ("Unicode half-blocks (low fidelity)".to_owned(), false)
            }
            None => ("not supported — placeholders only".to_owned(), false),
        };
        let (mouse, mouse_ok) = if caps.mouse {
            ("enabled".to_owned(), true)
        } else {
            ("not supported".to_owned(), false)
        };
        Self {
            colour,
            colour_ok,
            images,
            images_ok,
            mouse,
            mouse_ok,
        }
    }
}

// ── Rendering ──────────────────────────────────────────────────────────

/// View widget — drawn each frame from fresh state.
pub struct WelcomeView<'a> {
    pub theme: &'a Theme,
    /// Currently-active theme name (read straight off `config.theme`).
    pub theme_name: &'a str,
}

/// Natural body width — fits the longest content line plus a little
/// breathing room.  Pinned so the modal width doesn't jitter when the
/// content changes (e.g. switching between truecolor/256/none).
const CONTENT_WIDTH: u16 = 64;
/// Body height when no capabilities are degraded.  Includes a single
/// blank spacer row between the capability summary and the theme
/// section.  When any capability is degraded, the body grows by
/// `DEGRADED_HINT_ROWS` to fit the wrapped hint paragraph above that
/// spacer (so the spacer is preserved and the theme section never
/// gets crowded by the hint text).
const BODY_HEIGHT_BASE: u16 = 27;
/// Extra rows the wrapped "✗ consider upgrading…" hint occupies when
/// any capability is degraded.  Added on top of `BODY_HEIGHT_BASE`
/// (and consumed inside `render`) only in that case.
const DEGRADED_HINT_ROWS: u16 = 2;
/// Rows reserved for the wrapped "Getting started" paragraph (label
/// row is counted separately).  Sized to fit the current copy at
/// `CONTENT_WIDTH - 2` columns with a safety margin.
const QUICK_START_ROWS: u16 = 7;
/// Width of each tri-state pill cell (`[ Always ]` = 10 cols).
const PILL_W: u16 = 10;
const PILL_GAP: u16 = 2;
const PILL_ROW_W: u16 = PILL_W * 3 + PILL_GAP * 2;
/// Left column where each row's interactive control starts.  Lines up
/// the three pill rows so the user sees a coherent column.
const CONTROL_COL: u16 = 22;

impl<'a> StatefulWidget for WelcomeView<'a> {
    type State = WelcomeState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let degraded = !(state.cap_summary.colour_ok
            && state.cap_summary.images_ok
            && state.cap_summary.mouse_ok);
        let hint_rows = if degraded { DEGRADED_HINT_ROWS } else { 0 };
        let body_height = BODY_HEIGHT_BASE + hint_rows;
        let content = ContentSize {
            width: CONTENT_WIDTH,
            height: body_height,
            pinned_top: 0,
            pinned_bottom: 0,
        };
        let modal_area = centered_rect_for_content(content, area);
        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title: "Welcome to edamame",
                kind: ModalKind::Normal,
                show_close_hint: false,
                content_width: CONTENT_WIDTH,
                theme: self.theme,
            },
        );
        state.esc_button_rect = layout.esc_hit_rect;
        let body = layout.body;
        if body.height < body_height || body.width == 0 {
            return;
        }

        let mut y = body.y;
        let muted_style = Style::default()
            .fg(self.theme.palette.text_muted)
            .bg(self.theme.palette.surface_elevated);
        let ok_style = Style::default()
            .fg(self.theme.palette.success)
            .bg(self.theme.palette.surface_elevated);
        let warn_style = Style::default()
            .fg(self.theme.palette.warning)
            .bg(self.theme.palette.surface_elevated);

        // ── Getting started — short paragraph describing how edamame
        // works.  Renders with `Paragraph::wrap` so the body width
        // determines the line breaks; reserved row count is
        // `QUICK_START_ROWS` and the paragraph is expected to fit.
        // Placed at the top per UX feedback so the user reads what the
        // app does before fiddling with capability gauges or toggles.
        render_label(buf, body.x, y, body.width, "Getting started", self.theme);
        y += 1;
        let para_text = "edamame is a Markdown viewer and editor for the terminal. \
            Preview mode renders the document for distraction-free reading; start \
            typing to edit in Rendered mode, where the cursor's block reveals its raw \
            Markdown while neighbouring blocks stay formatted. Press Escape to return \
            to Preview, Ctrl+` for Raw mode (plain source for the whole document), \
            Ctrl-P for the command palette.";
        let para_area = Rect {
            x: body.x + 2,
            y,
            width: body.width.saturating_sub(2),
            height: QUICK_START_ROWS,
        };
        for row in 0..QUICK_START_ROWS {
            Paragraph::new("").style(self.theme.modal_bg).render(
                Rect {
                    x: body.x,
                    y: y + row,
                    width: body.width,
                    height: 1,
                },
                buf,
            );
        }
        Paragraph::new(para_text)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .style(muted_style)
            .render(para_area, buf);
        y += QUICK_START_ROWS + 1; // +1 spacer between sections

        // ── Capability summary ────────────────────────────────────────
        render_label(
            buf,
            body.x,
            y,
            body.width,
            "Terminal capabilities",
            self.theme,
        );
        y += 1;
        render_cap_row(
            buf,
            body.x,
            y,
            body.width,
            "Colour",
            &state.cap_summary.colour,
            state.cap_summary.colour_ok,
            self.theme,
            ok_style,
            warn_style,
        );
        y += 1;
        render_cap_row(
            buf,
            body.x,
            y,
            body.width,
            "Images",
            &state.cap_summary.images,
            state.cap_summary.images_ok,
            self.theme,
            ok_style,
            warn_style,
        );
        y += 1;
        render_cap_row(
            buf,
            body.x,
            y,
            body.width,
            "Mouse",
            &state.cap_summary.mouse,
            state.cap_summary.mouse_ok,
            self.theme,
            ok_style,
            warn_style,
        );
        y += 1;

        // Wrapped "✗ consider upgrading…" hint, only when degraded.
        // Sized at `hint_rows` so the modal collapses by that many rows
        // when every capability is fine — no empty space below the
        // capability summary in the common case.  Wraps within
        // `body.width - 2` so it never gets cut off on narrow modals.
        if degraded {
            for row in 0..hint_rows {
                Paragraph::new("").style(self.theme.modal_bg).render(
                    Rect {
                        x: body.x,
                        y: y + row,
                        width: body.width,
                        height: 1,
                    },
                    buf,
                );
            }
            let hint = "  ✗ — Consider upgrading to a modern terminal, \
                such as kitty, wezterm, or ghostty, for a better experience.";
            Paragraph::new(hint)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .style(muted_style)
                .render(
                    Rect {
                        x: body.x,
                        y,
                        width: body.width.saturating_sub(2),
                        height: hint_rows,
                    },
                    buf,
                );
            y += hint_rows;
        }
        // One-row spacer between the capability summary (or its
        // wrapped hint) and the theme section — consistent regardless
        // of whether the hint is shown.
        Paragraph::new("").style(self.theme.modal_bg).render(
            Rect {
                x: body.x,
                y,
                width: body.width,
                height: 1,
            },
            buf,
        );
        y += 1;

        // ── Theme — two lines: current theme + "Switch theme" button
        // The button-only line is the focusable target; the "Current
        // theme: <name>" line is purely informational so the active
        // selection reads at a glance after the picker mutates
        // `config.theme`.
        let theme_focused = state.focused == WelcomeFocus::Theme;
        let current_line = Line::from(vec![
            Span::styled("Current theme: ", self.theme.modal_bg),
            Span::styled(
                self.theme_name.to_owned(),
                Style::default()
                    .fg(self.theme.palette.primary)
                    .bg(self.theme.palette.surface_elevated)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        render_line(buf, body.x, y, body.width, current_line, self.theme);
        y += 1;
        let button_style = if theme_focused {
            self.theme.modal_button_focused
        } else {
            self.theme.modal_item
        };
        let button_label = "[ Switch theme ▸ ]";
        let button_w = button_label.chars().count() as u16;
        let button_x = body.x;
        Paragraph::new("").style(self.theme.modal_bg).render(
            Rect {
                x: body.x,
                y,
                width: body.width,
                height: 1,
            },
            buf,
        );
        Paragraph::new(Line::from(Span::styled(button_label, button_style)))
            .style(self.theme.modal_bg)
            .render(
                Rect {
                    x: button_x,
                    y,
                    width: button_w,
                    height: 1,
                },
                buf,
            );
        state.theme_button_rect = Some(Rect {
            x: button_x,
            y,
            width: button_w,
            height: 1,
        });
        y += 2;

        // ── Tri-state rows ──────────────────────────────────────────
        let images_rects = render_tristate(
            buf,
            body,
            y,
            "Show images",
            images_pill_labels(state.images),
            state.focused == WelcomeFocus::Images,
            !state.image_capable,
            self.theme,
        );
        state.images_pill_rects = images_rects;
        y += 1;
        render_explanation(
            buf,
            body.x,
            y,
            body.width,
            "Render inline images using your terminal's image protocol.",
            muted_style,
            self.theme,
        );
        y += 2;

        let remote_disabled = !state.image_capable || state.remote_locked_by_images();
        let remote_rects = render_tristate(
            buf,
            body,
            y,
            "Show remote images",
            remote_pill_labels(state.remote, remote_disabled),
            state.focused == WelcomeFocus::RemoteImages,
            remote_disabled,
            self.theme,
        );
        state.remote_pill_rects = remote_rects;
        y += 1;
        render_explanation(
            buf,
            body.x,
            y,
            body.width,
            "Fetch images from http(s):// URLs",
            muted_style,
            self.theme,
        );
        y += 2;

        let diagrams_rects = render_tristate(
            buf,
            body,
            y,
            "Show diagrams",
            diagrams_pill_labels(state.diagrams),
            state.focused == WelcomeFocus::Diagrams,
            !state.image_capable,
            self.theme,
        );
        state.diagrams_pill_rects = diagrams_rects;
        y += 1;
        render_explanation(
            buf,
            body.x,
            y,
            body.width,
            "Render mermaid code blocks as inline diagrams.",
            muted_style,
            self.theme,
        );
        y += 2;

        // ── Footer row: Don't-show-again toggle followed by [ Save ]
        // Centred as a pair so the two related affordances read as one
        // group.  The toggle sits to the left of Save so users scan it
        // first; Tab order matches (ShowAgain → Save).
        let save_focused = state.focused == WelcomeFocus::Save;
        let save_style = if save_focused {
            self.theme.modal_button_focused
        } else {
            self.theme.modal_item
        };
        let save_label = "[ Save ]";
        let save_w = save_label.chars().count() as u16;

        let sa_focused = state.focused == WelcomeFocus::ShowAgain;
        // Label-row style: focused → primary bg, unfocused → plain.
        // The `[x]` glyph gets its own secondary-fg accent (when
        // unfocused-checked) so the persistent-selection affordance
        // lands on the checkbox itself rather than the full label.
        let sa_label_style = if sa_focused {
            self.theme.modal_button_focused
        } else {
            self.theme.modal_item
        };
        let glyph_style = if sa_focused {
            self.theme.modal_button_focused
        } else if state.dont_show_again {
            self.theme.modal_item_selected_unfocused
        } else {
            self.theme.modal_item
        };
        let glyph = if state.dont_show_again { "[x]" } else { "[ ]" };
        let suffix = " Don't show this again";
        let toggle_w = (glyph.chars().count() + suffix.chars().count()) as u16;

        let gap_w: u16 = 4;
        let combined_w = toggle_w + gap_w + save_w;
        let start_x = body.x + body.width.saturating_sub(combined_w) / 2;
        let toggle_x = start_x;
        let save_x = toggle_x + toggle_w + gap_w;

        // Fill the row with modal_bg so the surface stays uniform.
        Paragraph::new("").style(self.theme.modal_bg).render(
            Rect {
                x: body.x,
                y,
                width: body.width,
                height: 1,
            },
            buf,
        );

        let toggle_area = Rect {
            x: toggle_x,
            y,
            width: toggle_w,
            height: 1,
        };
        Paragraph::new(Line::from(vec![
            Span::styled(glyph.to_owned(), glyph_style),
            Span::styled(suffix.to_owned(), sa_label_style),
        ]))
        .style(self.theme.modal_bg)
        .render(toggle_area, buf);
        state.show_again_rect = Some(toggle_area);

        let save_area = Rect {
            x: save_x,
            y,
            width: save_w,
            height: 1,
        };
        Paragraph::new(Line::from(Span::styled(save_label, save_style)))
            .style(self.theme.modal_bg)
            .render(save_area, buf);
        state.save_button_rect = Some(save_area);
    }
}

fn images_pill_labels(value: ImagesEnabled) -> [PillCell; 3] {
    [
        PillCell::new("Ask", matches!(value, ImagesEnabled::Ask)),
        PillCell::new("Always", matches!(value, ImagesEnabled::Always)),
        PillCell::new("Never", matches!(value, ImagesEnabled::Never)),
    ]
}

fn remote_pill_labels(value: RemoteImagePolicy, disabled: bool) -> [PillCell; 3] {
    if disabled {
        // When greyed out, highlight none so the row reads as inert.
        [
            PillCell::new("Ask", false),
            PillCell::new("Always", false),
            PillCell::new("Never", false),
        ]
    } else {
        [
            PillCell::new("Ask", matches!(value, RemoteImagePolicy::Ask)),
            PillCell::new("Always", matches!(value, RemoteImagePolicy::Always)),
            PillCell::new("Never", matches!(value, RemoteImagePolicy::Never)),
        ]
    }
}

fn diagrams_pill_labels(value: DiagramsEnabled) -> [PillCell; 3] {
    [
        PillCell::new("Ask", matches!(value, DiagramsEnabled::Ask)),
        PillCell::new("Always", matches!(value, DiagramsEnabled::Always)),
        PillCell::new("Never", matches!(value, DiagramsEnabled::Never)),
    ]
}

struct PillCell {
    label: &'static str,
    selected: bool,
}

impl PillCell {
    fn new(label: &'static str, selected: bool) -> Self {
        Self { label, selected }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_tristate(
    buf: &mut Buffer,
    body: Rect,
    y: u16,
    label: &str,
    cells: [PillCell; 3],
    focused: bool,
    disabled: bool,
    theme: &Theme,
) -> [Option<Rect>; 3] {
    // Row fill — uniform modal_bg across the whole row width.
    Paragraph::new("").style(theme.modal_bg).render(
        Rect {
            x: body.x,
            y,
            width: body.width,
            height: 1,
        },
        buf,
    );
    // Row label on the left — focused rows get a bold primary-fg label
    // so the focused row's leading text matches the focused pill's
    // primary affordance.  Unfocused-but-selected pills use
    // `modal_item_selected_unfocused` (secondary) so focus location is
    // distinguishable from persistent selection at a glance.
    let label_style = if disabled {
        Style::default()
            .fg(theme.palette.text_muted)
            .bg(theme.palette.surface_elevated)
            .add_modifier(Modifier::DIM)
    } else if focused {
        Style::default()
            .fg(theme.palette.primary)
            .bg(theme.palette.surface_elevated)
            .add_modifier(Modifier::BOLD)
    } else {
        theme.modal_bg
    };
    Paragraph::new(Line::from(Span::styled(label.to_owned(), label_style)))
        .style(theme.modal_bg)
        .render(
            Rect {
                x: body.x,
                y,
                width: CONTROL_COL.min(body.width),
                height: 1,
            },
            buf,
        );

    // Pill row, drawn at `body.x + CONTROL_COL`.
    let mut rects = [None, None, None];
    if body.width < CONTROL_COL + PILL_ROW_W {
        return rects;
    }
    let pill_x0 = body.x + CONTROL_COL;
    let dim_style = Style::default()
        .fg(theme.palette.text_muted)
        .bg(theme.palette.surface_elevated)
        .add_modifier(Modifier::DIM);
    for (i, cell) in cells.iter().enumerate() {
        let x = pill_x0 + (PILL_W + PILL_GAP) * i as u16;
        let style = if disabled {
            dim_style
        } else if cell.selected && focused {
            theme.modal_button_focused
        } else if cell.selected {
            theme.modal_item_selected_unfocused
        } else {
            theme.modal_item
        };
        let text = format!("[ {} ]", center_label(cell.label, (PILL_W - 4) as usize));
        let line = Line::from(Span::styled(text, style));
        let rect = Rect {
            x,
            y,
            width: PILL_W,
            height: 1,
        };
        Paragraph::new(line).style(theme.modal_bg).render(rect, buf);
        if !disabled {
            rects[i] = Some(rect);
        }
    }
    rects
}

fn center_label(label: &str, target_chars: usize) -> String {
    let label_chars = label.chars().count();
    if label_chars >= target_chars {
        return label.to_owned();
    }
    let pad = target_chars - label_chars;
    let left = pad / 2;
    let right = pad - left;
    format!("{}{}{}", " ".repeat(left), label, " ".repeat(right))
}

fn render_explanation(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    text: &str,
    style: Style,
    theme: &Theme,
) {
    Paragraph::new("").style(theme.modal_bg).render(
        Rect {
            x,
            y,
            width,
            height: 1,
        },
        buf,
    );
    Paragraph::new(Line::from(Span::styled(format!("  {text}"), style)))
        .style(theme.modal_bg)
        .render(
            Rect {
                x,
                y,
                width,
                height: 1,
            },
            buf,
        );
}

fn render_label(buf: &mut Buffer, x: u16, y: u16, width: u16, text: &str, theme: &Theme) {
    let style = theme.modal_section_heading;
    Paragraph::new("").style(theme.modal_bg).render(
        Rect {
            x,
            y,
            width,
            height: 1,
        },
        buf,
    );
    Paragraph::new(Line::from(Span::styled(text.to_owned(), style)))
        .style(theme.modal_bg)
        .render(
            Rect {
                x,
                y,
                width,
                height: 1,
            },
            buf,
        );
}

#[allow(clippy::too_many_arguments)]
fn render_cap_row(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    label: &str,
    value: &str,
    ok: bool,
    theme: &Theme,
    ok_style: Style,
    warn_style: Style,
) {
    let value_style = if ok { ok_style } else { warn_style };
    let mark = if ok { "✓" } else { "✗" };
    let line = Line::from(vec![
        Span::raw("  • "),
        Span::styled(format!("{label}: "), theme.modal_bg),
        Span::styled(value.to_owned(), value_style),
        Span::raw(" "),
        Span::styled(mark.to_owned(), value_style),
    ]);
    Paragraph::new(line).style(theme.modal_bg).render(
        Rect {
            x,
            y,
            width,
            height: 1,
        },
        buf,
    );
}

fn render_line(buf: &mut Buffer, x: u16, y: u16, width: u16, line: Line<'_>, theme: &Theme) {
    Paragraph::new("").style(theme.modal_bg).render(
        Rect {
            x,
            y,
            width,
            height: 1,
        },
        buf,
    );
    Paragraph::new(line).style(theme.modal_bg).render(
        Rect {
            x,
            y,
            width,
            height: 1,
        },
        buf,
    );
}

fn rect_contains(rect: Option<Rect>, col: u16, row: u16) -> bool {
    let Some(r) = rect else {
        return false;
    };
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

fn hit_index(rects: &[Option<Rect>; 3], col: u16, row: u16) -> Option<usize> {
    for (i, r) in rects.iter().enumerate() {
        if rect_contains(*r, col, row) {
            return Some(i);
        }
    }
    None
}

fn cycle_images(value: ImagesEnabled, delta: isize) -> ImagesEnabled {
    let order = [
        ImagesEnabled::Ask,
        ImagesEnabled::Always,
        ImagesEnabled::Never,
    ];
    let cur = order.iter().position(|v| *v == value).unwrap_or(0) as isize;
    let len = order.len() as isize;
    let next = ((cur + delta).rem_euclid(len)) as usize;
    order[next]
}

fn cycle_remote(value: RemoteImagePolicy, delta: isize) -> RemoteImagePolicy {
    let order = [
        RemoteImagePolicy::Ask,
        RemoteImagePolicy::Always,
        RemoteImagePolicy::Never,
    ];
    let cur = order.iter().position(|v| *v == value).unwrap_or(0) as isize;
    let len = order.len() as isize;
    let next = ((cur + delta).rem_euclid(len)) as usize;
    order[next]
}

fn cycle_diagrams(value: DiagramsEnabled, delta: isize) -> DiagramsEnabled {
    let order = [
        DiagramsEnabled::Ask,
        DiagramsEnabled::Always,
        DiagramsEnabled::Never,
    ];
    let cur = order.iter().position(|v| *v == value).unwrap_or(0) as isize;
    let len = order.len() as isize;
    let next = ((cur + delta).rem_euclid(len)) as usize;
    order[next]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Capabilities;

    fn caps_full() -> Capabilities {
        Capabilities {
            colour_depth: ColourDepth::TrueColor,
            mouse: true,
            image_protocol: Some(ImageProtocol::KittyGraphics),
            image_picker: None,
            halfblocks_picker: None,
            unicode_full: true,
            keyboard_enhancement: true,
        }
    }

    fn caps_no_images() -> Capabilities {
        Capabilities {
            image_protocol: None,
            image_picker: None,
            halfblocks_picker: None,
            ..caps_full()
        }
    }

    fn make_state(caps: &Capabilities) -> WelcomeState {
        WelcomeState::new(
            caps,
            ImagesEnabled::Ask,
            RemoteImagePolicy::Ask,
            DiagramsEnabled::Ask,
        )
    }

    #[test]
    fn tab_cycles_focus_skipping_disabled_rows_when_no_images() {
        let caps = caps_no_images();
        let mut s = make_state(&caps);
        assert_eq!(s.focused, WelcomeFocus::Theme);
        s.handle_key(&KeyEvent::new(
            KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        ));
        // Image rows are disabled (no protocol) — skip to ShowAgain.
        assert_eq!(s.focused, WelcomeFocus::ShowAgain);
    }

    #[test]
    fn images_never_cascades_remote_to_never() {
        let caps = caps_full();
        let mut s = make_state(&caps);
        s.focused = WelcomeFocus::Images;
        // Ask → Always → Never via two Right presses.
        s.handle_key(&KeyEvent::new(
            KeyCode::Right,
            crossterm::event::KeyModifiers::NONE,
        ));
        s.handle_key(&KeyEvent::new(
            KeyCode::Right,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(matches!(s.images, ImagesEnabled::Never));
        assert!(matches!(s.remote, RemoteImagePolicy::Never));
    }

    #[test]
    fn flipping_images_back_restores_pre_cascade_remote() {
        let caps = caps_full();
        let mut s = make_state(&caps);
        s.remote = RemoteImagePolicy::Always;
        s.focused = WelcomeFocus::Images;
        // Cycle to Never (Ask → Always → Never) then back to Always.
        for _ in 0..2 {
            s.handle_key(&KeyEvent::new(
                KeyCode::Right,
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        assert!(matches!(s.images, ImagesEnabled::Never));
        assert!(matches!(s.remote, RemoteImagePolicy::Never));
        // Cycle once more (Never → Ask) — should restore Always.
        s.handle_key(&KeyEvent::new(
            KeyCode::Right,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(matches!(s.images, ImagesEnabled::Ask));
        assert!(matches!(s.remote, RemoteImagePolicy::Always));
    }

    #[test]
    fn save_button_enter_returns_save_response() {
        let caps = caps_full();
        let mut s = make_state(&caps);
        s.focused = WelcomeFocus::Save;
        let r = s.handle_key(&KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(r, WelcomeResponse::Save);
    }

    #[test]
    fn space_cycles_focused_tristate_row() {
        let caps = caps_full();
        let mut s = make_state(&caps);
        s.focused = WelcomeFocus::Images;
        // Ask → Always
        s.handle_key(&KeyEvent::new(
            KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(matches!(s.images, ImagesEnabled::Always));
        // Always → Never (also cascades remote → Never)
        s.handle_key(&KeyEvent::new(
            KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(matches!(s.images, ImagesEnabled::Never));
        assert!(matches!(s.remote, RemoteImagePolicy::Never));
    }

    #[test]
    fn letter_s_no_longer_saves() {
        let caps = caps_full();
        let mut s = make_state(&caps);
        let r = s.handle_key(&KeyEvent::new(
            KeyCode::Char('s'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(r, WelcomeResponse::Continue);
    }

    #[test]
    fn theme_enter_opens_picker() {
        let caps = caps_full();
        let mut s = make_state(&caps);
        s.focused = WelcomeFocus::Theme;
        let r = s.handle_key(&KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(r, WelcomeResponse::OpenThemePicker);
    }

    #[test]
    fn esc_does_not_dismiss() {
        let caps = caps_full();
        let mut s = make_state(&caps);
        let r = s.handle_key(&KeyEvent::new(
            KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(r, WelcomeResponse::Continue);
    }
}
