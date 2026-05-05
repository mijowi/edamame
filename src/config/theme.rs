use ratatui::style::{Color, Modifier, Style};

/// Edamame's two-tier theming model.
///
/// 1. [`Palette`] — a small named set of brand colours (orange, yellow,
///    purple, blue, green, red, plus muted greys and surface tones).  Each
///    colour has a `bright_*` and `dim_*` variant.  Authoring a new theme
///    boils down to picking shades for these slots.
/// 2. [`Theme`] — every styled element in the UI gets a precomputed
///    [`Style`].  `Theme::default()` derives every style from the default
///    palette, so any change to the palette ripples through the whole UI.
///
/// User theme files (`themes/<name>.toml`) may override the palette,
/// individual styles, or both.  See [`super::theme_file`] for the on-disk
/// format and the merge order.
///
/// No hardcoded colours exist outside [`Palette::default`] — every UI site
/// reads from `theme.<field>`.
#[derive(Debug, Clone)]
pub struct Theme {
    /// The named brand-colour palette every style is derived from.
    /// Stored on the theme so user code (e.g. modal selection rendering)
    /// can reach for `default_bg` as a fg colour against a coloured bg.
    pub palette: Palette,

    // ── Headings ──────────────────────────────────────────────────
    pub h1: Style,
    pub h1_rule: Style,
    pub h2: Style,
    pub h3: Style,
    pub h4: Style,
    pub h5: Style,
    pub h6: Style,

    // ── Inline formatting ─────────────────────────────────────────
    pub bold: Style,
    pub italic: Style,
    pub strikethrough: Style,
    pub highlight: Style,
    pub code_span: Style,
    /// Web link (`http://`, `https://`, `mailto:`, etc.) — bright
    /// foreground + underline so the URL reads as actionable.
    pub link_text: Style,
    /// File link (relative or absolute path) — dim variant so local
    /// links read as more peripheral than web links.
    pub link_file: Style,
    /// In-document heading link (`#section`) — dim, no underline.
    pub link_heading: Style,
    pub image_placeholder: Style,
    /// Footnote / reference marker (deferred renderer feature).  Field
    /// is in place so themes can already style it ahead of the
    /// implementation.
    pub footnote: Style,

    // ── Block elements ────────────────────────────────────────────
    pub code_block_border: Style,
    pub code_block_lang: Style,
    pub code_block_text: Style,
    pub blockquote_bar: Style,
    pub blockquote_text: Style,
    pub rule: Style,

    // ── List markers ──────────────────────────────────────────────
    pub list_bullet: Style,
    pub list_number: Style,

    // ── Task list ─────────────────────────────────────────────────
    pub task_unchecked: Style,
    /// Style applied to the `[x]` marker for checked items.
    pub task_checked: Style,
    /// Style applied to the *text* of completed tasks (the part after
    /// the checkbox).  Distinct from `task_checked` so the marker can
    /// stay green while the text fades to muted grey.
    pub task_complete_text: Style,
    /// Whether to render checked item text with strikethrough (default: true).
    pub task_strikethrough: bool,

    // ── Table ─────────────────────────────────────────────────────
    pub table_border: Style,
    pub table_header: Style,
    pub table_header_border: Style,
    pub table_cell: Style,
    /// Background fill for even-numbered data rows (0-indexed: row 0 = first
    /// data row).  Only applied when `config.table.row_striping` is true; the
    /// default is `Style::default()` so no visible change is produced for
    /// users who haven't opted in.
    pub table_row_even: Style,
    /// Background fill for odd-numbered data rows.  See `table_row_even`.
    pub table_row_odd: Style,
    /// Highlight applied during a row / column drag to mark the destination
    /// separator the cursor is currently hovering.  Painted as a post-pass
    /// over the table border so no buffer mutation is required.
    pub table_drop_indicator: Style,
    /// Style for the *inert* drop-target separators painted during a
    /// row / column drag — every separator that's a valid drop site
    /// gets this style; the one the pointer is currently over upgrades
    /// to `table_drop_indicator`.  Themes can use a dimmer shade than
    /// the active indicator so the affordance reads as a set of
    /// possibilities with one pointer-tracked highlight.
    pub table_drop_target: Style,

    // ── Status bar ────────────────────────────────────────────────
    pub status_bar: Style,
    /// Mode badge in Preview mode.  See also the Rendered/Raw variants.
    pub status_mode_preview: Style,
    /// Mode badge in Rendered mode.
    pub status_mode_rendered: Style,
    /// Mode badge in Raw mode.
    pub status_mode_raw: Style,
    pub status_filename: Style,
    pub status_info: Style,
    pub status_modified: Style,
    /// Style for the selection-size indicator (e.g. ` Sel 42 ch · 3 ln `).
    pub status_selection: Style,

    // ── Hint line (Phase 9) ───────────────────────────────────────
    /// Base background/foreground for the contextual hint line.
    pub hint_bar: Style,
    /// Chord glyph style (e.g. the `^C` in `^C Copy`).  Contrasting
    /// background distinguishes the keybind from its label.
    pub hint_chord: Style,
    /// Label style (e.g. the `Copy` in `^C Copy`).  Blends into the
    /// surrounding hint_bar fill.
    pub hint_label: Style,

    // ── Transient messages (Phase 9) ──────────────────────────────
    /// Neutral notification style — e.g. `Copied`, `Saved`.
    pub transient_info: Style,
    /// Success notification style — e.g. `Autosaved`.
    pub transient_success: Style,
    /// Warning notification style — e.g. `Configuration updated`.
    pub transient_warning: Style,
    /// Error notification style — sticky, dismissed with Escape.
    pub transient_error: Style,

    // ── Modal popups ──────────────────────────────────────────────
    /// Background fill for modal bodies (palette, settings, keybinds,
    /// Save-Copy, Insert-Table, …).  Distinct field so themes can give
    /// modals a different surface from the status bar even when the
    /// default palette uses the same shade for both.
    pub modal_bg: Style,
    /// Border / chrome of modal frames.
    pub modal_border: Style,
    pub modal_title: Style,
    /// Default style for an unfocused row in a list-style modal.
    pub modal_item: Style,
    /// Right-aligned hint / sub-label on an *unfocused* row (e.g. the
    /// chord shown next to a palette entry, or the value column in
    /// settings / keybinds rows).  Mirrors `modal_item_selected_hint`
    /// for the unfocused state.
    pub modal_item_hint: Style,
    /// Selected row in a list-style modal (palette / settings /
    /// keybinds).  Filled background so the row reads as the focus.
    pub modal_item_selected: Style,
    /// Right-aligned hint / sub-label on the focused row (e.g. the
    /// chord shown next to a palette entry, or the value column on
    /// settings / keybinds rows).
    pub modal_item_selected_hint: Style,
    /// Pinned-footer description for the focused row (e.g. the
    /// settings overlay's bottom line that explains the focused
    /// setting).  Sits on the modal body's `surface_elevated` rather
    /// than on the row's selection bg, so it gets its own field.
    pub modal_description: Style,
    /// Section heading inside a modal (e.g. `— Editor —` in the
    /// keybinds overlay).  Slightly distinct from a document H2.
    pub modal_section_heading: Style,
    /// Inline editor / text input in an unfocused state.
    pub modal_input_unfocused: Style,
    /// Inline editor / text input while focused for typing.
    pub modal_input_focused: Style,
    /// Modal button when focused for activation.
    pub modal_button_focused: Style,

    // ── General text ──────────────────────────────────────────────
    pub normal: Style,

    /// Background style applied to characters inside an active text selection.
    /// Renders on top of the character's own style so colour-coded content
    /// stays legible.
    pub selection: Style,

    /// Find-in-document highlight — distinct from `selection` so search
    /// hits remain visible even when one of them is currently selected.
    pub search_highlight: Style,

    /// Background style applied to the cursor's current line.  Default
    /// is `Style::default()` (no tint) — the active-line highlight is a
    /// deferred feature; the field exists so themes can opt in early.
    pub active_line: Style,

    /// Block cursor when the editor is in Preview mode.  Mirrors the
    /// status-bar mode chip so the cursor reads as the same affordance
    /// in both places.  Preview is read-only so this primarily
    /// applies to ad-hoc cursor indicators (e.g. tooling overlays).
    pub cursor_preview: Style,
    /// Block cursor when the editor is in Rendered mode.  Mirrors the
    /// Rendered mode chip's bg.
    pub cursor_rendered: Style,
    /// Block cursor when the editor is in Raw mode.  Mirrors the Raw
    /// mode chip's bg.
    pub cursor_raw: Style,
    /// Generic input-line cursor (`▏` glyph) used inside modal text
    /// inputs.  Default is `REVERSED` only, which swaps fg/bg of
    /// whatever's underneath — kept distinct from the editor cursor
    /// because modal inputs aren't tied to editor mode.
    pub cursor: Style,
}

/// Edamame's brand-colour palette.  Every theme is built from these
/// nineteen colours.
///
/// Each conceptual colour has a `bright_*` and `dim_*` variant so the
/// renderer can use the bright shade for emphasis (headings, primary
/// links) and the dim shade for peripheral material (heading anchors,
/// borders).  `default_text` / `default_bg` are concrete colours rather
/// than terminal defaults because they're used as foregrounds in
/// inverse contexts (e.g. selected modal item: `interactive` bg with
/// `default_bg` fg), where `Color::Reset` would not produce the right
/// contrast.
///
/// `surface_elevated` is the chrome surface (status bar, modal body,
/// inline-code background); `surface_dim` is the elevated surface used
/// for secondary chrome (hint line, unfocused text inputs) — by
/// convention slightly *lighter* than `surface_elevated`, so an input
/// reads as recessed against the modal but still distinct from the
/// document area.
#[derive(Debug, Clone)]
pub struct Palette {
    pub default_text: Color,
    pub default_bg: Color,

    pub bright_primary: Color,
    pub dim_primary: Color,
    pub bright_emphasis: Color,
    pub dim_emphasis: Color,
    pub bright_structural: Color,
    pub dim_structural: Color,
    pub bright_interactive: Color,
    pub dim_interactive: Color,
    pub bright_success: Color,
    pub dim_success: Color,
    pub bright_warning: Color,
    pub dim_warning: Color,
    pub bright_error: Color,
    pub dim_error: Color,
    pub text_muted: Color,
    pub muted: Color,
    pub surface_elevated: Color,
    pub surface: Color,

    pub h1: Color,
    pub h2: Color,
    pub h3: Color,
    pub h4: Color,
    pub h5: Color,
    pub h6: Color,

    pub code: Color,
}

impl Default for Palette {
    fn default() -> Self {
        // Edamame's default palette: warm orange brand on a near-black
        // background, with a fresh edamame-bean green for success and
        // a complementary lavender for chrome.  Bright/dim pairs are
        // tuned for ~30% lightness contrast so both variants read on a
        // dark surface.
        Self {
            default_text: Color::Indexed(253),
            default_bg: Color::Indexed(233),

            // Orange — brand identity, headings, mode chip.
            bright_primary: Color::Indexed(208),
            dim_primary: Color::Indexed(172),

            // Blue — emphasis
            bright_emphasis: Color::Indexed(117),
            dim_emphasis: Color::Indexed(45),

            // Purple — structural chrome (frames, dividers, asides).
            bright_structural: Color::Indexed(136),
            dim_structural: Color::Indexed(94),

            // Blue — links, focus, selection.
            bright_interactive: Color::Indexed(39),
            dim_interactive: Color::Indexed(25),

            // Green — success, completed tasks, edamame.
            bright_success: Color::Indexed(76),
            dim_success: Color::Indexed(28),

            // Yellow — warnings.
            bright_warning: Color::Indexed(220),
            dim_warning: Color::Indexed(178),

            // Red — errors.
            bright_error: Color::Indexed(196),
            dim_error: Color::Indexed(124),

            // Greys — UI chrome and muted items
            text_muted: Color::Indexed(245), // Muted text, e.g. strikethrough
            muted: Color::Indexed(235),      // Muted background, e.g. table row stripes
            surface_elevated: Color::Indexed(237), // Elevated surface, e.g. dialogs
            surface: Color::Indexed(236),    // Surface, e.g. panels, dialogs

            // Headings — bright color 1/2/3, dim color 1/2/3
            h1: Color::Indexed(220),
            h2: Color::Indexed(208),
            h3: Color::Indexed(135),
            h4: Color::Indexed(136),
            h5: Color::Indexed(172),
            h6: Color::Indexed(140),

            // Inline code and code block language line
            code: Color::Indexed(140),
        }
    }
}

impl Theme {
    /// Build a fully-populated [`Theme`] from `palette`.  Every style
    /// is derived from a palette entry per the rules in `theming.md`.
    /// Used both by [`Theme::default`] and by the on-disk theme loader
    /// after applying user palette overrides.
    pub fn from_palette(palette: &Palette) -> Self {
        let bold = Modifier::BOLD;
        let italic = Modifier::ITALIC;
        let underline = Modifier::UNDERLINED;
        let p = palette.clone();

        Self {
            palette: p.clone(),

            // Headings: bold + underline + a per-level palette colour.
            h1: Style::default().fg(p.h1).add_modifier(bold),
            h1_rule: Style::default().fg(p.h1), // H1 has a rule instead of an underline
            h2: Style::default()
                .fg(p.h2)
                .add_modifier(bold)
                .add_modifier(underline),
            h3: Style::default()
                .fg(p.h3)
                .add_modifier(bold)
                .add_modifier(underline),
            h4: Style::default()
                .fg(p.h4)
                .add_modifier(bold)
                .add_modifier(underline),
            h5: Style::default()
                .fg(p.h5)
                .add_modifier(bold)
                .add_modifier(underline),
            h6: Style::default()
                .fg(p.h6)
                .add_modifier(bold)
                .add_modifier(underline),

            // Inline
            bold: Style::default().add_modifier(bold),
            italic: Style::default().add_modifier(italic),
            strikethrough: Style::default()
                .fg(p.text_muted)
                .add_modifier(Modifier::CROSSED_OUT),
            highlight: Style::default().bg(p.dim_warning).fg(p.default_bg),
            code_span: Style::default().fg(p.code).bg(p.surface),
            link_text: Style::default()
                .fg(p.bright_interactive)
                .add_modifier(underline),
            link_file: Style::default()
                .fg(p.bright_interactive)
                .add_modifier(underline),
            link_heading: Style::default().fg(p.bright_interactive),
            image_placeholder: Style::default().fg(p.dim_interactive).add_modifier(italic),
            footnote: Style::default().fg(p.dim_structural),

            // Code block — surface_elevated background reads as a single
            // unit across border, language label, and body.
            code_block_border: Style::default().fg(p.default_text).bg(p.surface),
            code_block_lang: Style::default()
                .fg(p.code)
                .bg(p.surface_elevated)
                .add_modifier(italic),
            code_block_text: Style::default().fg(p.default_text).bg(p.surface),

            // Blockquote
            blockquote_bar: Style::default().fg(p.dim_structural),
            blockquote_text: Style::default().add_modifier(italic),

            // Horizontal rule
            rule: Style::default().fg(p.dim_structural),

            // List markers — bright_structural so bullets read as
            // chrome rather than content.
            list_bullet: Style::default().fg(p.dim_emphasis),
            list_number: Style::default().fg(p.bright_emphasis),

            // Task list
            task_unchecked: Style::default().fg(p.bright_warning),
            task_checked: Style::default().fg(p.dim_success),
            task_complete_text: Style::default()
                .fg(p.text_muted)
                .add_modifier(Modifier::CROSSED_OUT),
            task_strikethrough: true,

            // Table
            table_border: Style::default().fg(p.surface_elevated),
            table_header: Style::default().add_modifier(bold).fg(p.bright_emphasis),
            table_header_border: Style::default().fg(p.surface_elevated),
            table_cell: Style::default(),
            table_row_even: Style::default(),
            table_row_odd: Style::default().bg(p.muted),
            table_drop_indicator: Style::default().fg(p.bright_interactive),
            table_drop_target: Style::default().fg(p.dim_interactive),

            // Status bar — surface fill.  Mode chip swaps fg/bg
            // depending on Mode so each mode reads at a glance.
            status_bar: Style::default().bg(p.surface).fg(p.default_text),
            status_mode_preview: Style::default()
                .bg(p.text_muted)
                .fg(p.surface)
                .add_modifier(bold),
            status_mode_rendered: Style::default()
                .bg(p.bright_primary)
                .fg(p.default_bg)
                .add_modifier(bold),
            status_mode_raw: Style::default()
                .bg(p.bright_warning)
                .fg(p.default_bg)
                .add_modifier(bold),
            status_filename: Style::default().fg(p.default_text).bg(p.surface),
            status_info: Style::default().fg(p.bright_primary).bg(p.surface),
            status_modified: Style::default()
                .fg(p.bright_warning)
                .bg(p.surface)
                .add_modifier(bold),
            status_selection: Style::default()
                .fg(p.bright_interactive)
                .bg(p.surface)
                .add_modifier(bold),

            // Hint line — surface_elevated background. Chord badges are
            // bright_interactive on the hint surface for readability.
            hint_bar: Style::default().bg(p.surface_elevated).fg(p.default_text),
            hint_chord: Style::default()
                .fg(p.bright_interactive)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            hint_label: Style::default().fg(p.default_text).bg(p.surface_elevated),

            // Transient messages — escalate in salience.  All sit on
            // the hint_bar surface so they layer cleanly over the
            // chord row.
            transient_info: Style::default().fg(p.default_text).bg(p.surface_elevated),
            transient_success: Style::default()
                .fg(p.bright_success)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            transient_warning: Style::default()
                .fg(p.bright_warning)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            transient_error: Style::default()
                .fg(p.bright_error)
                .bg(p.surface_elevated)
                .add_modifier(bold),

            // Modal popups
            modal_bg: Style::default().bg(p.surface_elevated).fg(p.default_text),
            modal_border: Style::default().fg(p.dim_structural).bg(p.surface_elevated),
            modal_title: Style::default()
                .fg(p.bright_primary)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            modal_item: Style::default().fg(p.default_text).bg(p.surface_elevated),
            modal_item_hint: Style::default()
                .fg(p.bright_interactive)
                .bg(p.surface_elevated),
            modal_item_selected: Style::default()
                .bg(p.dim_interactive)
                .fg(p.default_text)
                .add_modifier(bold),
            modal_item_selected_hint: Style::default().fg(p.bright_emphasis).bg(p.dim_interactive),
            modal_description: Style::default()
                .fg(p.bright_emphasis)
                .bg(p.surface_elevated),
            modal_section_heading: Style::default()
                .fg(p.bright_structural)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            modal_input_unfocused: Style::default().fg(p.default_bg).bg(p.dim_interactive),
            modal_input_focused: Style::default().fg(p.default_bg).bg(p.bright_interactive),
            modal_button_focused: Style::default()
                .fg(p.bright_interactive)
                .add_modifier(Modifier::REVERSED | bold),

            // General — concrete `default_text` / `default_bg` so the
            // document area renders with the theme's "blank page"
            // colours rather than letting the terminal's defaults show
            // through.  Themes that prefer the terminal's own bg can
            // set `[normal] fg = "Reset"` and `bg = "Reset"` in their
            // TOML.
            normal: Style::default().fg(p.default_text).bg(p.default_bg),

            // Selection: dim_interactive bg, text colour preserved so
            // colour-coded content stays legible inside the selection.
            selection: Style::default().bg(p.dim_interactive).fg(p.default_text),

            // Search highlight: bright_structural bg, default_bg fg —
            // distinct from selection so a search hit inside the
            // selection still reads as a hit.
            search_highlight: Style::default().bg(p.bright_structural).fg(p.default_bg),

            // Active-line highlight is deferred — leave the field in
            // place so themes can opt in.
            active_line: Style::default(),

            // Editor block cursor — bg mirrors the status-bar mode chip
            // so the cursor reads as the same affordance in both
            // places (per theming.md).  Each variant pairs the chip's
            // bg with a contrasting fg so the underlying character
            // stays legible.
            cursor_preview: Style::default().bg(p.muted).fg(p.surface_elevated),
            cursor_rendered: Style::default().bg(p.bright_primary).fg(p.default_bg),
            cursor_raw: Style::default().bg(p.bright_warning).fg(p.default_bg),
            // Generic input cursor — REVERSED so the `▏` glyph inside
            // a modal text input inverts whatever's underneath without
            // needing to know the surrounding bg.
            cursor: Style::default().add_modifier(Modifier::REVERSED),
        }
    }

    /// Return the appropriate heading style for a heading level (1–6).
    pub fn heading_style(&self, level: pulldown_cmark::HeadingLevel) -> Style {
        use pulldown_cmark::HeadingLevel::*;
        match level {
            H1 => self.h1,
            H2 => self.h2,
            H3 => self.h3,
            H4 => self.h4,
            H5 => self.h5,
            H6 => self.h6,
        }
    }

    /// Pick the Mode-specific status mode chip style.
    pub fn status_mode_style(&self, mode: crate::editor::Mode) -> Style {
        use crate::editor::Mode::*;
        match mode {
            Preview => self.status_mode_preview,
            Rendered => self.status_mode_rendered,
            Raw => self.status_mode_raw,
        }
    }

    /// Pick the Mode-specific editor cursor style.  Mirrors the
    /// status-bar mode chip's bg so the cursor reads as the same
    /// affordance in both places.

    /// Build a `Theme` from a user-authored [`ThemeFile`].
    ///
    /// When `monochrome` is true the file is ignored and the compiled-in
    /// monochrome fallback is returned — preserves the contract that
    /// `ColourDepth::NoColour` terminals never emit colour escapes, even if
    /// a colourful theme file is installed.
    pub fn from_file(file: &super::theme_file::ThemeFile, monochrome: bool) -> Self {
        if monochrome {
            Self::monochrome()
        } else {
            file.into()
        }
    }

    /// Monochrome fallback theme — used when the terminal reports no colour
    /// support (e.g. `TERM=dumb`).  All colours are stripped; text attributes
    /// (bold, italic, underline, strikethrough) are preserved because they
    /// work over SGR regardless of colour depth.
    pub fn monochrome() -> Self {
        let default = Self::default();
        let strip = |s: Style| -> Style {
            // Keep only the modifier bits (bold/italic/underline/etc).
            Style::default().add_modifier(s.add_modifier)
        };

        Self {
            // Palette is preserved verbatim so user code that reads it
            // (e.g. `default_bg` as a fg) still compiles, but no style
            // actually consumes it.
            palette: default.palette.clone(),

            h1: strip(default.h1),
            h1_rule: Style::default(),
            h2: strip(default.h2),
            h3: strip(default.h3),
            h4: strip(default.h4),
            h5: strip(default.h5),
            h6: strip(default.h6),

            bold: strip(default.bold),
            italic: strip(default.italic),
            strikethrough: strip(default.strikethrough),
            highlight: Style::default().add_modifier(Modifier::REVERSED),
            code_span: Style::default().add_modifier(Modifier::REVERSED),
            link_text: Style::default().add_modifier(Modifier::UNDERLINED),
            link_file: Style::default().add_modifier(Modifier::UNDERLINED),
            link_heading: Style::default().add_modifier(Modifier::UNDERLINED),
            image_placeholder: Style::default().add_modifier(Modifier::ITALIC),
            footnote: Style::default(),

            code_block_border: Style::default(),
            code_block_lang: Style::default().add_modifier(Modifier::ITALIC),
            code_block_text: Style::default(),
            blockquote_bar: Style::default(),
            blockquote_text: Style::default().add_modifier(Modifier::ITALIC),
            rule: Style::default(),

            list_bullet: Style::default(),
            list_number: Style::default(),

            task_unchecked: Style::default(),
            task_checked: Style::default().add_modifier(Modifier::BOLD),
            task_complete_text: Style::default().add_modifier(Modifier::CROSSED_OUT),
            task_strikethrough: true,

            table_border: Style::default(),
            table_header: Style::default().add_modifier(Modifier::BOLD),
            table_header_border: Style::default(),
            table_cell: Style::default(),
            table_row_even: Style::default(),
            table_row_odd: Style::default().add_modifier(Modifier::DIM),
            table_drop_indicator: Style::default()
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            table_drop_target: Style::default().add_modifier(Modifier::REVERSED),

            status_bar: Style::default().add_modifier(Modifier::REVERSED),
            status_mode_preview: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            status_mode_rendered: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            status_mode_raw: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            status_filename: Style::default().add_modifier(Modifier::REVERSED),
            status_info: Style::default().add_modifier(Modifier::REVERSED),
            status_modified: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            status_selection: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),

            hint_bar: Style::default(),
            hint_chord: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            hint_label: Style::default(),

            transient_info: Style::default().add_modifier(Modifier::REVERSED),
            transient_success: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            transient_warning: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            transient_error: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),

            modal_bg: Style::default().add_modifier(Modifier::REVERSED),
            modal_border: Style::default().add_modifier(Modifier::REVERSED),
            modal_title: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            modal_item: Style::default().add_modifier(Modifier::REVERSED),
            modal_item_hint: Style::default().add_modifier(Modifier::REVERSED),
            modal_item_selected: Style::default().add_modifier(Modifier::BOLD),
            modal_item_selected_hint: Style::default().add_modifier(Modifier::BOLD),
            modal_description: Style::default().add_modifier(Modifier::REVERSED),
            modal_section_heading: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            modal_input_unfocused: Style::default().add_modifier(Modifier::REVERSED),
            modal_input_focused: Style::default().add_modifier(Modifier::BOLD),
            modal_button_focused: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),

            normal: Style::default(),
            selection: Style::default().add_modifier(Modifier::REVERSED),
            search_highlight: Style::default().add_modifier(Modifier::REVERSED),
            active_line: Style::default(),
            cursor_preview: Style::default().add_modifier(Modifier::REVERSED),
            cursor_rendered: Style::default().add_modifier(Modifier::REVERSED),
            cursor_raw: Style::default().add_modifier(Modifier::REVERSED),
            cursor: Style::default().add_modifier(Modifier::REVERSED),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_palette(&Palette::default())
    }
}
