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
    style::Style,
    text::{Line, Span},
    widgets::{Block, Paragraph, StatefulWidget, Widget, Wrap},
};

use crate::config::{DiagramsEnabled, ImagesEnabled, RemoteImagePolicy, Theme};
use crate::terminal::Capabilities;
use crate::ui::cap_summary::{render_cap_row as shared_render_cap_row, CapSummary};
use crate::ui::controls::{self, Control, ControlEvent, ControlInput, ControlValue};
use crate::ui::overlay_nav::next_focusable_wrapping;
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
    VimMotions,
    ShowAgain,
    Save,
}

const FOCUS_ORDER: [WelcomeFocus; 7] = [
    WelcomeFocus::Theme,
    WelcomeFocus::Diagrams,
    WelcomeFocus::Images,
    WelcomeFocus::RemoteImages,
    WelcomeFocus::VimMotions,
    WelcomeFocus::ShowAgain,
    WelcomeFocus::Save,
];

impl WelcomeFocus {
    /// Position of this variant within [`FOCUS_ORDER`] — the single
    /// source of truth shared by Tab navigation and the `focus_offsets`
    /// slot each render pass writes to.  Using this instead of literal
    /// indices means inserting a new focusable row only requires editing
    /// `FOCUS_ORDER`; the offset assignments can never drift out of sync.
    /// Panics if a variant is missing from `FOCUS_ORDER`, which
    /// `every_focus_variant_is_ordered` guards against at test time.
    fn order_index(self) -> usize {
        FOCUS_ORDER
            .iter()
            .position(|f| *f == self)
            .expect("every WelcomeFocus must appear in FOCUS_ORDER")
    }
}

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
    /// "Use Vim motions" toggle.  Default `false` — when checked, Save
    /// flips `config.modal.handler` to `"vim"` and activates modal
    /// editing for the running session.
    pub use_vim: bool,
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
    pub images_rect: Option<Rect>,
    pub remote_rect: Option<Rect>,
    pub diagrams_rect: Option<Rect>,
    pub vim_rect: Option<Rect>,
    pub show_again_rect: Option<Rect>,
    pub save_button_rect: Option<Rect>,

    /// Body-relative y of each focusable row, captured each render so
    /// focus moves can scroll the focused element back into view.
    /// Indexed by position in `FOCUS_ORDER` — array length is tied to
    /// `FOCUS_ORDER.len()`, and both the render writes and the Tab reads
    /// resolve their slot through `WelcomeFocus::order_index`, so the two
    /// can't drift.
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
        use_vim: bool,
    ) -> Self {
        Self {
            focused: WelcomeFocus::Theme,
            images,
            remote,
            diagrams,
            use_vim,
            dont_show_again: true,
            image_capable: caps.image_protocol.is_some(),
            pre_cascade_remote: None,
            scroll_state: ScrollContainerState::default(),
            theme_button_rect: None,
            esc_button_rect: None,
            images_rect: None,
            remote_rect: None,
            diagrams_rect: None,
            vim_rect: None,
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
    /// body-relative y captured by the previous render.  Backed by the
    /// shared [`next_focusable_wrapping`] so welcome and export share one
    /// wrapping focus ring.
    fn step_focus(&mut self, delta: i32) {
        let cur = FOCUS_ORDER
            .iter()
            .position(|f| *f == self.focused)
            .unwrap_or(0);
        if let Some(i) =
            next_focusable_wrapping(&FOCUS_ORDER, cur, delta, |f| !self.row_disabled(*f))
        {
            self.focused = FOCUS_ORDER[i];
            self.scroll_state.ensure_visible(self.focus_offsets[i]);
        }
    }

    /// Apply a [`ControlInput`] to the focused option row through the shared
    /// transition layer ([`Control::apply`]), writing the result back.  No-op
    /// if focus isn't on an option row.  The tri-state pills (images / remote
    /// / diagrams) cycle through [`controls::ASK_ALWAYS_NEVER`]; the on/off
    /// toggles (vim motions / show-again) are direction-bound (Left=off /
    /// Right=on, Activate flips).  The images path goes through `set_images`
    /// so the remote cascade still fires.
    fn apply_input(&mut self, input: ControlInput) {
        let pill = Control::Pill(controls::ASK_ALWAYS_NEVER);
        match self.focused {
            WelcomeFocus::Images => {
                if let ControlEvent::Changed(ControlValue::Choice(i)) = pill.apply(
                    ControlValue::Choice(pill_index(&IMAGES_ORDER, self.images)),
                    input,
                ) {
                    self.set_images(pill_value(&IMAGES_ORDER, i));
                }
            }
            WelcomeFocus::RemoteImages if !self.remote_locked_by_images() => {
                if let ControlEvent::Changed(ControlValue::Choice(i)) = pill.apply(
                    ControlValue::Choice(pill_index(&REMOTE_ORDER, self.remote)),
                    input,
                ) {
                    self.remote = pill_value(&REMOTE_ORDER, i);
                }
            }
            WelcomeFocus::Diagrams => {
                if let ControlEvent::Changed(ControlValue::Choice(i)) = pill.apply(
                    ControlValue::Choice(pill_index(&DIAGRAMS_ORDER, self.diagrams)),
                    input,
                ) {
                    self.diagrams = pill_value(&DIAGRAMS_ORDER, i);
                }
            }
            WelcomeFocus::VimMotions => {
                if let ControlEvent::Changed(ControlValue::Toggle(v)) =
                    Control::Toggle.apply(ControlValue::Toggle(self.use_vim), input)
                {
                    self.use_vim = v;
                }
            }
            WelcomeFocus::ShowAgain => {
                if let ControlEvent::Changed(ControlValue::Toggle(v)) =
                    Control::Toggle.apply(ControlValue::Toggle(self.dont_show_again), input)
                {
                    self.dont_show_again = v;
                }
            }
            _ => {}
        }
    }

    fn set_images(&mut self, next: ImagesEnabled) {
        let was_never = matches!(self.images, ImagesEnabled::Never);
        self.remote = controls::apply_images_cascade(
            next,
            was_never,
            self.remote,
            &mut self.pre_cascade_remote,
        );
        self.images = next;
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> WelcomeResponse {
        // PgUp/PgDn/Home/End scroll the body without moving focus.
        // Arrow keys remain bound to focus / tri-state cycling below.
        if self.scroll_state.handle_paging_key(key) {
            return WelcomeResponse::Continue;
        }
        match key.code {
            KeyCode::Tab | KeyCode::Down => {
                self.step_focus(1);
                WelcomeResponse::Continue
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.step_focus(-1);
                WelcomeResponse::Continue
            }
            // Activate (Enter / Space) on the Theme / Save rows fires its own
            // response; on a control row it falls through to the shared
            // `control_input_for` mapping below (where it becomes Activate).
            KeyCode::Enter | KeyCode::Char(' ') if self.focused == WelcomeFocus::Theme => {
                WelcomeResponse::OpenThemePicker
            }
            KeyCode::Enter | KeyCode::Char(' ') if self.focused == WelcomeFocus::Save => {
                WelcomeResponse::Save
            }
            // No Esc dismissal — the spec replaces Cancel with the
            // explicit "Show on next launch" toggle.  Esc is consumed
            // but does nothing so the modal can't be closed without
            // pressing Save (which respects the show-again toggle).
            KeyCode::Esc => WelcomeResponse::Continue,
            // Left / Right (any control row) and Activate (Enter / Space on a
            // control row) route through the single key → ControlInput map →
            // `apply_input`.  Keys the control doesn't take map to None and
            // no-op.
            _ => {
                if let Some(input) = controls::control_input_for(key.code) {
                    self.apply_input(input);
                }
                WelcomeResponse::Continue
            }
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
            self.apply_input(ControlInput::Activate);
            return WelcomeResponse::Continue;
        }
        if rect_contains(self.vim_rect, col, row) {
            self.focused = WelcomeFocus::VimMotions;
            self.apply_input(ControlInput::Activate);
            return WelcomeResponse::Continue;
        }
        // A cycle pill shows only the current value, so a click advances
        // it by one (same as Space / Right), rather than selecting a
        // specific option.  `apply_input` applies the images cascade.
        if self.image_capable {
            if rect_contains(self.images_rect, col, row) {
                self.focused = WelcomeFocus::Images;
                self.apply_input(ControlInput::Activate);
                return WelcomeResponse::Continue;
            }
            if !self.remote_locked_by_images() && rect_contains(self.remote_rect, col, row) {
                self.focused = WelcomeFocus::RemoteImages;
                self.apply_input(ControlInput::Activate);
                return WelcomeResponse::Continue;
            }
            if rect_contains(self.diagrams_rect, col, row) {
                self.focused = WelcomeFocus::Diagrams;
                self.apply_input(ControlInput::Activate);
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
/// Left column where each row's interactive control starts.  Lines up
/// the cycle-pill rows so the user sees a coherent column.
const CONTROL_COL: u16 = 22;
/// Body text describing the editor — wraps at the body's inner width
/// at render time (see `wrapped_para_rows`).
const QUICK_START_TEXT: &str = "edamame is a Markdown editor for your terminal, featuring:\n\
• PREVIEW, hybrid EDIT, and RAW edit modes — PREVIEW is for viewing only; \
in EDIT, the cursor's line or table cell reveals its raw Markdown and \
everything else stays formatted; RAW has no formatting. \n\
• Mouse, image, and Mermaid diagram support, depending on your terminal's capabilities\n\
• GitHub Flavored Markdown, including tables, task lists, and more, plus highlights\n\
• Diff mode — review external file changes hunk by hunk\n\
• Bottom bar with status and contextual hints\n\
• Command palette for access to commands and settings (Ctrl-P)\n\
• Vim mode — optional Vim-style editing (see docs for what's supported)";
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
        //  2                 theme label+button row + spacer below
        //  3 * 3             three tri-state sections (row + explanation + spacer)
        //  3                 vim-motions toggle (row + explanation + spacer)
        //  1                 "Don't show this again" toggle row
        //  1                 spacer
        //  1                 Save button row
        let cap_rows = state.cap_summary.rows.len() as u16;
        let natural_height =
            1 + para_rows + 1 + 1 + cap_rows + hint_rows + 1 + 2 + 9 + 3 + 1 + 1 + 1;

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

        // ── Theme (standard label + button row) ──────────────────────
        // The current theme name lives *inside* the button: the bracketed
        // affordance distinguishes it without needing an accent color, and
        // the ▸ arrow signals that activating it opens the theme picker.
        let theme_focused = state.focused == WelcomeFocus::Theme;
        // Uniform row fill so the label column inherits modal_bg.
        Paragraph::new("").style(self.theme.modal_bg).render(
            Rect {
                x: body_x,
                y,
                width: body_w,
                height: 1,
            },
            &mut scratch,
        );
        // Label column — same focus treatment as the option rows below.
        let label_col_w = CONTROL_COL.min(body_w) as usize;
        Paragraph::new(Line::from(Span::styled(
            format!("{:<label_col_w$}", "Choose theme"),
            controls::control_label_style(theme_focused, false, self.theme),
        )))
        .style(self.theme.modal_bg)
        .render(
            Rect {
                x: body_x,
                y,
                width: CONTROL_COL.min(body_w),
                height: 1,
            },
            &mut scratch,
        );
        // Button carries the current theme name + the "opens a modal" arrow.
        let theme_button_label = format!("{} ▸", self.theme_name);
        state.theme_button_rect = Some(crate::ui::button_row::render_button_at(
            Rect {
                x: body_x + CONTROL_COL,
                y,
                width: body_w.saturating_sub(CONTROL_COL),
                height: 1,
            },
            &mut scratch,
            crate::ui::button_row::Button::bracketed(&theme_button_label),
            theme_focused,
            self.theme,
        ));
        state.focus_offsets[WelcomeFocus::Theme.order_index()] = y;
        y += 2; // label+button row + spacer below

        // ── Option rows (diagrams sit above the image rows) ─────────
        let pill_w = controls::pill_width(controls::ASK_ALWAYS_NEVER) as u16;

        state.focus_offsets[WelcomeFocus::Diagrams.order_index()] = y;
        state.diagrams_rect = render_control_row(
            &mut scratch,
            scratch_rect,
            y,
            "Show diagrams",
            controls::pill_spans(
                controls::ASK_ALWAYS_NEVER,
                pill_index(&DIAGRAMS_ORDER, state.diagrams),
                state.focused == WelcomeFocus::Diagrams,
                !state.image_capable,
                self.theme,
            ),
            pill_w,
            state.focused == WelcomeFocus::Diagrams,
            !state.image_capable,
            self.theme,
        );
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

        state.focus_offsets[WelcomeFocus::Images.order_index()] = y;
        state.images_rect = render_control_row(
            &mut scratch,
            scratch_rect,
            y,
            "Show images",
            controls::pill_spans(
                controls::ASK_ALWAYS_NEVER,
                pill_index(&IMAGES_ORDER, state.images),
                state.focused == WelcomeFocus::Images,
                !state.image_capable,
                self.theme,
            ),
            pill_w,
            state.focused == WelcomeFocus::Images,
            !state.image_capable,
            self.theme,
        );
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

        state.focus_offsets[WelcomeFocus::RemoteImages.order_index()] = y;
        let remote_disabled = !state.image_capable || state.remote_locked_by_images();
        state.remote_rect = render_control_row(
            &mut scratch,
            scratch_rect,
            y,
            "Show remote images",
            controls::pill_spans(
                controls::ASK_ALWAYS_NEVER,
                pill_index(&REMOTE_ORDER, state.remote),
                state.focused == WelcomeFocus::RemoteImages,
                remote_disabled,
                self.theme,
            ),
            pill_w,
            state.focused == WelcomeFocus::RemoteImages,
            remote_disabled,
            self.theme,
        );
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

        // ── Vim motions toggle ──────────────────────────────────────
        let toggle_w = controls::toggle_width() as u16;
        state.focus_offsets[WelcomeFocus::VimMotions.order_index()] = y;
        state.vim_rect = render_control_row(
            &mut scratch,
            scratch_rect,
            y,
            "Vim mode",
            controls::toggle_spans(
                state.use_vim,
                state.focused == WelcomeFocus::VimMotions,
                false,
                self.theme,
            ),
            toggle_w,
            state.focused == WelcomeFocus::VimMotions,
            false,
            self.theme,
        );
        y += 1;
        render_explanation(
            &mut scratch,
            body_x,
            y,
            body_w,
            "Enable Vim-style modal editing",
            muted_style,
            self.theme,
        );
        y += 2;

        // ── "Don't show this again" toggle (standard label-left row) ─
        state.focus_offsets[WelcomeFocus::ShowAgain.order_index()] = y;
        state.show_again_rect = render_control_row(
            &mut scratch,
            scratch_rect,
            y,
            "Don't show this again",
            controls::toggle_spans(
                state.dont_show_again,
                state.focused == WelcomeFocus::ShowAgain,
                false,
                self.theme,
            ),
            toggle_w,
            state.focused == WelcomeFocus::ShowAgain,
            false,
            self.theme,
        );
        y += 2; // toggle row + spacer

        // ── Save button row (centred on its own line) ───────────────
        let save_focused = state.focused == WelcomeFocus::Save;
        let save_area = Rect {
            x: body_x,
            y,
            width: body_w,
            height: 1,
        };
        let save_rects = crate::ui::button_row::render_button_row(
            save_area,
            &mut scratch,
            &["Save"],
            if save_focused { 0 } else { usize::MAX },
            self.theme,
        );
        state.save_button_rect = save_rects.into_iter().next();
        state.focus_offsets[WelcomeFocus::Save.order_index()] = y;

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
        state.vim_rect = translate_rect(state.vim_rect, body, scroll);
        state.images_rect = translate_rect(state.images_rect, body, scroll);
        state.remote_rect = translate_rect(state.remote_rect, body, scroll);
        state.diagrams_rect = translate_rect(state.diagrams_rect, body, scroll);

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

// One ordered table per tri-state enum, mirroring `controls::ASK_ALWAYS_NEVER`'s
// label order.  Each table is the *single source* for both directions of its
// enum's pill mapping — `pill_index` (value → `ControlValue::Choice` index) and
// `pill_value` (index → value) read the same slice, so a reordering can't drift
// the two halves apart.
const IMAGES_ORDER: [ImagesEnabled; 3] = [
    ImagesEnabled::Ask,
    ImagesEnabled::Always,
    ImagesEnabled::Never,
];
const REMOTE_ORDER: [RemoteImagePolicy; 3] = [
    RemoteImagePolicy::Ask,
    RemoteImagePolicy::Always,
    RemoteImagePolicy::Never,
];
const DIAGRAMS_ORDER: [DiagramsEnabled; 3] = [
    DiagramsEnabled::Ask,
    DiagramsEnabled::Always,
    DiagramsEnabled::Never,
];

/// Index of `value` within its pill `order` (the `ControlValue::Choice` index
/// fed into [`Control::apply`]).  Falls back to 0 for a value absent from the
/// table — unreachable for the tri-state enums, which list every variant.
fn pill_index<T: PartialEq>(order: &[T], value: T) -> usize {
    order.iter().position(|v| *v == value).unwrap_or(0)
}

/// Inverse of [`pill_index`]: the value at `i` in `order`, clamped to the last
/// entry for any out-of-range index (the pill only ever yields `0..len`).
fn pill_value<T: Copy>(order: &[T], i: usize) -> T {
    order[i.min(order.len().saturating_sub(1))]
}

/// Render a label + control (pill or toggle) row into the welcome modal's
/// scratch buffer, returning the control's body-relative hit rect (or
/// `None` when the row is disabled or the body is too narrow to fit the
/// control).  The label uses the same focus / disabled styling as the
/// surrounding rows; the caller supplies the already-built control
/// `spans` (via [`controls::pill_spans`] / [`controls::toggle_spans`]) and
/// their rendered `width`.
#[allow(clippy::too_many_arguments)]
fn render_control_row(
    buf: &mut Buffer,
    body: Rect,
    y: u16,
    label: &str,
    spans: Vec<Span<'static>>,
    width: u16,
    focused: bool,
    disabled: bool,
    theme: &Theme,
) -> Option<Rect> {
    // Row fill — uniform modal_bg across the whole row width.
    let row_rect = Rect {
        x: body.x,
        y,
        width: body.width,
        height: 1,
    };
    Paragraph::new("")
        .style(theme.modal_bg)
        .render(row_rect, buf);
    // The label column is one unit with the control: pad the label across
    // the whole column (CONTROL_COL cells) so a focused row's fill spans the
    // column → widget, then append the caller's control spans — the shared
    // [`controls::control_row_spans`] composition, exactly like the settings
    // and export modals.  The modal only reports focus / disabled.
    let label_col_w = CONTROL_COL.min(body.width) as usize;
    let row_spans =
        controls::control_row_spans(label, label_col_w, spans, focused, disabled, theme);
    Paragraph::new(Line::from(row_spans))
        .style(theme.modal_bg)
        .render(row_rect, buf);

    if body.width < CONTROL_COL + width {
        return None;
    }
    if disabled {
        None
    } else {
        Some(Rect {
            x: body.x + CONTROL_COL,
            y,
            width,
            height: 1,
        })
    }
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

fn rect_contains(rect: Option<Rect>, col: u16, row: u16) -> bool {
    let Some(r) = rect else {
        return false;
    };
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
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
            false,
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
        // Image rows are disabled (no protocol) — skip to the always-on
        // vim-motions toggle.
        assert_eq!(s.focused, WelcomeFocus::VimMotions);
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
    fn space_toggles_vim_motions() {
        let caps = caps_full();
        let mut s = make_state(&caps);
        assert!(!s.use_vim, "vim motions default off");
        s.focused = WelcomeFocus::VimMotions;
        s.handle_key(&KeyEvent::new(
            KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(s.use_vim);
        s.handle_key(&KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(!s.use_vim, "Enter toggles back off");
    }

    #[test]
    fn toggle_arrows_are_direction_bound() {
        // Phase 2 unified toggle arrows to direction-bound everywhere: Left
        // sets off, Right sets on (Space/Enter still flip).  Previously
        // welcome flipped on either arrow.
        let caps = caps_full();
        let mut s = make_state(&caps);
        s.focused = WelcomeFocus::VimMotions;
        assert!(!s.use_vim);
        // Left on an already-off toggle is a no-op (not a flip-on).
        s.handle_key(&KeyEvent::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(!s.use_vim, "Left means off, even when already off");
        // Right turns it on; a second Right is a no-op.
        s.handle_key(&KeyEvent::new(
            KeyCode::Right,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(s.use_vim, "Right means on");
        s.handle_key(&KeyEvent::new(
            KeyCode::Right,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(s.use_vim, "Right when already on is a no-op");
        // Left turns it back off.
        s.handle_key(&KeyEvent::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(!s.use_vim, "Left means off");
    }

    #[test]
    fn every_focus_variant_is_ordered() {
        // `order_index` panics if a variant is absent from `FOCUS_ORDER`,
        // so resolving each one is the guard that keeps the render writes
        // and Tab reads addressing the same `focus_offsets` slots.
        for (i, f) in FOCUS_ORDER.iter().enumerate() {
            assert_eq!(f.order_index(), i, "{f:?} resolves to its FOCUS_ORDER slot");
        }
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
