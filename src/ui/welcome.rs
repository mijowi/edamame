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
//!
//! Layout is computed at render time from the available terminal area:
//! the "Getting started" paragraph and the degraded-capabilities hint
//! both wrap at the modal's body width, and the body is rendered into
//! a scratch buffer of the natural height before a `scroll`-offset
//! window is blitted into the visible body area.  This lets the modal
//! gracefully scroll on small terminals instead of clipping or
//! collapsing rows.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, StatefulWidget, Widget, Wrap},
};

use crate::config::{DiagramsEnabled, ImagesEnabled, RemoteImagePolicy, Theme};
use crate::terminal::Capabilities;
use crate::ui::cap_summary::{render_cap_row as shared_render_cap_row, CapSummary};
use crate::ui::scroll_container::{
    centered_rect_for_content, compute_pad_h, draw_frame, ContentSize, FrameOpts, ModalKind,
    ScrollContainerState, MAX_PAD_H,
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

const FOCUS_ORDER: [WelcomeFocus; 6] = [
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

    /// Vertical scroll bookkeeping.  When the natural body height
    /// exceeds the available body height, this drives the window of
    /// content that gets blitted from the scratch buffer.
    pub scroll_state: ScrollContainerState,

    // ── Hit-test rects, captured each render for click dispatch ──
    pub theme_button_rect: Option<Rect>,
    pub esc_button_rect: Option<Rect>,
    pub images_pill_rects: [Option<Rect>; 3],
    pub remote_pill_rects: [Option<Rect>; 3],
    pub diagrams_pill_rects: [Option<Rect>; 3],
    pub show_again_rect: Option<Rect>,
    pub save_button_rect: Option<Rect>,

    /// Body-relative y of each focusable row, captured each render so
    /// focus moves can scroll the focused element back into view.
    /// Indexed by position in `FOCUS_ORDER` — array length is tied to
    /// `FOCUS_ORDER.len()` so the two can't drift.
    focus_offsets: [u16; FOCUS_ORDER.len()],

    // ── Capability summary, captured at construction ──
    cap_summary: CapSummary,
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
            scroll_state: ScrollContainerState::default(),
            theme_button_rect: None,
            esc_button_rect: None,
            images_pill_rects: [None, None, None],
            remote_pill_rects: [None, None, None],
            diagrams_pill_rects: [None, None, None],
            show_again_rect: None,
            save_button_rect: None,
            focus_offsets: [0; FOCUS_ORDER.len()],
            cap_summary: CapSummary::from_caps(caps),
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
    /// pill row.  Scrolls the newly focused row into view using the
    /// body-relative y captured by the previous render.
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
                self.scroll_state.ensure_visible(self.focus_offsets[i]);
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
        // PgUp/PgDn/Home/End scroll the body without moving focus.
        // Arrow keys remain bound to focus / tri-state cycling below.
        if self.scroll_state.handle_paging_key(key) {
            return WelcomeResponse::Continue;
        }
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

    /// Forward a mouse wheel delta into the scroll state.  Mirrors the
    /// pattern used by `SettingsOverlay` / `KeybindsOverlay`.
    pub fn handle_wheel(&mut self, delta: i32) {
        self.scroll_state.scroll_by(delta);
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
/// Width of each tri-state pill cell (`[ Always ]` = 10 cols).
const PILL_W: u16 = 10;
const PILL_GAP: u16 = 2;
const PILL_ROW_W: u16 = PILL_W * 3 + PILL_GAP * 2;
/// Left column where each row's interactive control starts.  Lines up
/// the three pill rows so the user sees a coherent column.
const CONTROL_COL: u16 = 22;
/// Body text describing the editor — wraps at the body's inner width
/// at render time (see `wrapped_para_rows`).
const QUICK_START_TEXT: &str = "edamame is a Markdown editor for your terminal, featuring:\n\
• PREVIEW, hybrid EDIT, and RAW edit modes — PREVIEW is for viewing only; \
in EDIT, the cursor's line or table cell reveals its raw Markdown and \
everything else stays formatted; RAW has no formatting. \n\
• Mouse, image, and Mermaid diagram support, depending on your terminal's capabilities\n\
• GitHub Flavored Markdown, including tables, task lists, and more, plus highlights\n\
• Bottom bar with status and contextual hints\n\
• Command palette for access to commands and settings (Ctrl-P)";
/// Hint shown below the capability summary when any capability is
/// degraded.  Wrapped at body inner width at render time.
const DEGRADED_HINT: &str = "  ✗ — Consider upgrading to a modern terminal, \
such as kitty, wezterm, or ghostty, for a better experience.";

/// Number of wrapped rows a string would occupy at `width` columns
/// under `Paragraph::wrap(Wrap { trim: false })`.  Uses ratatui's own
/// line counter (gated by `unstable-rendered-line-info`) so the
/// pre-render sizing matches the actual `WordWrapper` output.
fn wrapped_para_rows(text: &str, width: u16) -> u16 {
    if width == 0 {
        // Worst-case: each \n becomes its own row.
        return text.split('\n').count().max(1) as u16;
    }
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .line_count(width)
        .min(u16::MAX as usize) as u16
}

impl<'a> StatefulWidget for WelcomeView<'a> {
    type State = WelcomeState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let degraded = !state.cap_summary.all_ok();

        // Body width is determined by the modal's outer width and its
        // horizontal padding — both derived from `area` and the fixed
        // CONTENT_WIDTH.  We need it BEFORE computing natural body
        // height because the paragraph and hint wrap at this width.
        let modal_width = CONTENT_WIDTH.saturating_add(2 * MAX_PAD_H).min(area.width);
        let pad_h = compute_pad_h(modal_width, CONTENT_WIDTH, MAX_PAD_H);
        let body_width = modal_width.saturating_sub(2 * pad_h);

        let para_inner_w = body_width.saturating_sub(2);
        let para_rows = wrapped_para_rows(QUICK_START_TEXT, para_inner_w);
        let hint_rows = if degraded {
            wrapped_para_rows(DEGRADED_HINT, para_inner_w)
        } else {
            0
        };

        // Natural body row count.  Trace:
        //  1                 "Getting started" label
        //  para_rows + 1     paragraph + spacer
        //  1                 "Terminal capabilities" label
        //  cap_rows          one row per capability in the summary
        //  hint_rows         degraded hint (0 when all OK)
        //  1                 spacer
        //  1                 current theme line
        //  2                 switch theme button + spacer below
        //  3 * 3             three tri-state sections (row + explanation + spacer)
        //  1                 footer (toggle + Save)
        let cap_rows = state.cap_summary.rows.len() as u16;
        let natural_height = 1 + para_rows + 1 + 1 + cap_rows + hint_rows + 1 + 1 + 2 + 9 + 1;

        let content = ContentSize {
            width: CONTENT_WIDTH,
            height: natural_height,
            pinned_top: 0,
            pinned_bottom: 0,
            ..Default::default()
        };
        let modal_area = centered_rect_for_content(content, area);
        let layout = draw_frame(
            modal_area,
            buf,
            FrameOpts {
                title: "Welcome to edamame",
                kind: ModalKind::Normal,
                show_close_hint: false,
                content,
                theme: self.theme,
            },
        );
        state.esc_button_rect = layout.esc_hit_rect;
        let body = layout.body;
        if body.height == 0 || body.width == 0 {
            return;
        }

        state.scroll_state.observe(natural_height, body.height);
        let scroll = state.scroll_state.scroll;

        // Render the natural-sized body into a scratch buffer whose
        // origin is (0, 0).  Subsequent rect bookkeeping is in
        // body-relative coords (matching the scratch); we translate to
        // absolute terminal coords once at the end.
        let scratch_rect = Rect {
            x: 0,
            y: 0,
            width: body.width,
            height: natural_height,
        };
        let mut scratch = Buffer::empty(scratch_rect);
        Block::default()
            .style(self.theme.modal_bg)
            .render(scratch_rect, &mut scratch);

        let normal_style = Style::default()
            .fg(self.theme.palette.text)
            .bg(self.theme.palette.surface_elevated);
        let muted_style = Style::default()
            .fg(self.theme.palette.text_muted)
            .bg(self.theme.palette.surface_elevated);
        let ok_style = Style::default()
            .fg(self.theme.palette.success)
            .bg(self.theme.palette.surface_elevated);
        let warn_style = Style::default()
            .fg(self.theme.palette.warning)
            .bg(self.theme.palette.surface_elevated);

        let body_x: u16 = 0;
        let body_w = body.width;
        let mut y: u16 = 0;

        // ── Getting started ──────────────────────────────────────────
        render_label(
            &mut scratch,
            body_x,
            y,
            body_w,
            "Getting started",
            self.theme,
        );
        y += 1;
        // Fill the band with modal_bg so unprinted cells inside the
        // wrapped paragraph inherit the modal surface color.
        for row in 0..para_rows {
            Paragraph::new("").style(self.theme.modal_bg).render(
                Rect {
                    x: body_x,
                    y: y + row,
                    width: body_w,
                    height: 1,
                },
                &mut scratch,
            );
        }
        let para_area = Rect {
            x: body_x + 2,
            y,
            width: para_inner_w,
            height: para_rows,
        };
        Paragraph::new(QUICK_START_TEXT)
            .wrap(Wrap { trim: false })
            .style(normal_style)
            .render(para_area, &mut scratch);
        y += para_rows + 1; // +1 spacer between sections

        // ── Capability summary ────────────────────────────────────────
        render_label(
            &mut scratch,
            body_x,
            y,
            body_w,
            "Terminal capabilities",
            self.theme,
        );
        y += 1;
        for row in &state.cap_summary.rows {
            shared_render_cap_row(
                &mut scratch,
                body_x,
                y,
                body_w,
                row,
                self.theme,
                ok_style,
                warn_style,
            );
            y += 1;
        }

        if degraded {
            for row in 0..hint_rows {
                Paragraph::new("").style(self.theme.modal_bg).render(
                    Rect {
                        x: body_x,
                        y: y + row,
                        width: body_w,
                        height: 1,
                    },
                    &mut scratch,
                );
            }
            Paragraph::new(DEGRADED_HINT)
                .wrap(Wrap { trim: false })
                .style(muted_style)
                .render(
                    Rect {
                        x: body_x,
                        y,
                        width: para_inner_w,
                        height: hint_rows,
                    },
                    &mut scratch,
                );
            y += hint_rows;
        }
        // Spacer between capability summary (or its wrapped hint) and
        // the theme section.
        Paragraph::new("").style(self.theme.modal_bg).render(
            Rect {
                x: body_x,
                y,
                width: body_w,
                height: 1,
            },
            &mut scratch,
        );
        y += 1;

        // ── Theme ────────────────────────────────────────────────────
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
        render_line(&mut scratch, body_x, y, body_w, current_line, self.theme);
        y += 1;
        let button_style = if theme_focused {
            self.theme.modal_button_focused
        } else {
            self.theme.modal_item
        };
        let button_label = "[ Switch theme ▸ ]";
        let button_w = button_label.chars().count() as u16;
        let button_x = body_x;
        Paragraph::new("").style(self.theme.modal_bg).render(
            Rect {
                x: body_x,
                y,
                width: body_w,
                height: 1,
            },
            &mut scratch,
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
                &mut scratch,
            );
        state.theme_button_rect = Some(Rect {
            x: button_x,
            y,
            width: button_w,
            height: 1,
        });
        state.focus_offsets[0] = y;
        y += 2;

        // ── Tri-state rows ──────────────────────────────────────────
        state.focus_offsets[1] = y;
        let images_rects = render_tristate(
            &mut scratch,
            scratch_rect,
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
            &mut scratch,
            body_x,
            y,
            body_w,
            "Render inline images using your terminal's image protocol.",
            muted_style,
            self.theme,
        );
        y += 2;

        state.focus_offsets[2] = y;
        let remote_disabled = !state.image_capable || state.remote_locked_by_images();
        let remote_rects = render_tristate(
            &mut scratch,
            scratch_rect,
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
            &mut scratch,
            body_x,
            y,
            body_w,
            "Fetch images from http(s):// URLs",
            muted_style,
            self.theme,
        );
        y += 2;

        state.focus_offsets[3] = y;
        let diagrams_rects = render_tristate(
            &mut scratch,
            scratch_rect,
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
            &mut scratch,
            body_x,
            y,
            body_w,
            "Render mermaid code blocks as inline diagrams.",
            muted_style,
            self.theme,
        );
        y += 2;

        // ── Footer row: Don't-show-again toggle + [ Save ] ──────────
        let save_focused = state.focused == WelcomeFocus::Save;
        let save_style = if save_focused {
            self.theme.modal_button_focused
        } else {
            self.theme.modal_item
        };
        let save_label = "[ Save ]";
        let save_w = save_label.chars().count() as u16;

        let sa_focused = state.focused == WelcomeFocus::ShowAgain;
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
        let start_x = body_x + body_w.saturating_sub(combined_w) / 2;
        let toggle_x = start_x;
        let save_x = toggle_x + toggle_w + gap_w;

        Paragraph::new("").style(self.theme.modal_bg).render(
            Rect {
                x: body_x,
                y,
                width: body_w,
                height: 1,
            },
            &mut scratch,
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
        .render(toggle_area, &mut scratch);
        state.show_again_rect = Some(toggle_area);
        state.focus_offsets[4] = y;

        let save_area = Rect {
            x: save_x,
            y,
            width: save_w,
            height: 1,
        };
        Paragraph::new(Line::from(Span::styled(save_label, save_style)))
            .style(self.theme.modal_bg)
            .render(save_area, &mut scratch);
        state.save_button_rect = Some(save_area);
        state.focus_offsets[5] = y;

        // ── Blit visible window of scratch into the body ────────────
        // Retarget `scratch`'s area so its (scroll..scroll+visible_h)
        // window aligns with the visible body rect, then merge into
        // `buf`.  `Buffer::merge` clips by absolute coords, but it also
        // *unions* the two areas — so we have to align the scratch top
        // with `body.y` (not `body.y - scroll`) and drop the rows above
        // `scroll` by trimming both `area.y` and `area.height`.
        let visible_h = body.height.min(natural_height.saturating_sub(scroll));
        let visible_window = Rect {
            x: body.x,
            y: body.y,
            width: body.width,
            height: visible_h,
        };
        let trimmed_len = (visible_h as usize) * (body.width as usize);
        let src_start = (scroll as usize) * (body.width as usize);
        scratch.content = scratch.content[src_start..src_start + trimmed_len].to_vec();
        scratch.area = visible_window;
        buf.merge(&scratch);

        // Translate every captured rect from body-relative scratch
        // coords to absolute terminal coords, clipping to the visible
        // body window.  Rects entirely outside the visible window
        // become `None` — clicks in those regions read as misses.
        state.theme_button_rect = translate_rect(state.theme_button_rect, body, scroll);
        state.save_button_rect = translate_rect(state.save_button_rect, body, scroll);
        state.show_again_rect = translate_rect(state.show_again_rect, body, scroll);
        for r in state.images_pill_rects.iter_mut() {
            *r = translate_rect(*r, body, scroll);
        }
        for r in state.remote_pill_rects.iter_mut() {
            *r = translate_rect(*r, body, scroll);
        }
        for r in state.diagrams_pill_rects.iter_mut() {
            *r = translate_rect(*r, body, scroll);
        }

        // Scrollbar in the right padding column, only when overflowing.
        if state.scroll_state.max_scroll() > 0 {
            let bar_area = Rect {
                x: layout.scrollbar_col,
                y: body.y,
                width: 1,
                height: body.height,
            };
            crate::ui::scrollbar::render_for_scroll_state(
                bar_area,
                &state.scroll_state,
                self.theme,
                buf,
            );
        }
    }
}

/// Translate a body-relative rect (origin at body's top-left) into
/// absolute terminal coords, clipped to the visible body window.
/// Returns `None` when the rect lies entirely outside the window.
fn translate_rect(rect: Option<Rect>, body: Rect, scroll: u16) -> Option<Rect> {
    let r = rect?;
    let src_y0 = r.y;
    let src_y1 = r.y.saturating_add(r.height);
    let vis_y0 = src_y0.max(scroll);
    let vis_y1 = src_y1.min(scroll.saturating_add(body.height));
    if vis_y0 >= vis_y1 {
        return None;
    }
    Some(Rect {
        x: body.x + r.x,
        y: body.y + (vis_y0 - scroll),
        width: r.width,
        height: vis_y1 - vis_y0,
    })
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
    use crate::terminal::{Capabilities, ColorDepth, ImageProtocol};

    fn caps_full() -> Capabilities {
        Capabilities {
            color_depth: ColorDepth::TrueColor,
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

    #[test]
    fn wheel_scrolls_body() {
        let caps = caps_full();
        let mut s = make_state(&caps);
        // Simulate a prior render observing a body shorter than the
        // natural content height so max_scroll() > 0.
        s.scroll_state.observe(40, 20);
        s.handle_wheel(3);
        assert_eq!(s.scroll_state.scroll, 3);
        s.handle_wheel(-1);
        assert_eq!(s.scroll_state.scroll, 2);
    }

    #[test]
    fn pgdown_scrolls_without_moving_focus() {
        let caps = caps_full();
        let mut s = make_state(&caps);
        s.scroll_state.observe(40, 10);
        let before = s.focused;
        s.handle_key(&KeyEvent::new(
            KeyCode::PageDown,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(s.focused, before, "PgDn must not move focus");
        assert_eq!(s.scroll_state.scroll, 10);
    }
}
