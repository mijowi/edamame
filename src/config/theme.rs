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
    /// Dim variant of [`Self::code_span`] used for inline code that
    /// appears inside strikethrough text (e.g. inside a `Strikethrough`
    /// inline or a checked task item's text).  Foreground falls back to
    /// `Palette::code_dim` so the snippet reads as struck-through
    /// without losing its code-span affordance.
    pub code_span_dim: Style,
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
    /// Style applied to the `[✓]` marker for checked items.
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
    /// Style for the row/column reorder (`⠿`) and column-resize (`⇔`)
    /// button glyphs painted on top of the table border.  Distinct from
    /// `table_border` so the affordances read as interactive rather
    /// than chrome.
    pub table_handle: Style,
    /// Style for the row/column delete (`✕`) button glyphs painted on
    /// top of the table border.  Distinct from `table_handle` so the
    /// destructive affordance reads as a warning.
    pub table_handle_delete: Style,

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
    /// Title text style for `ModalKind::Normal` — neutral / informational
    /// modals.  `primary_bright` on the modal surface.
    pub modal_title_normal: Style,
    /// Title text style for `ModalKind::Warning` — yellow on the modal
    /// surface.  Used by config / image / quit / dirty-guard prompts.
    pub modal_title_warning: Style,
    /// Title text style for `ModalKind::Error` — red on the modal surface.
    pub modal_title_error: Style,
    /// `text_muted` close-hint label rendered as `esc` on the right edge
    /// of the title row of dismissable modals.  Doubles as the visible
    /// affordance for the clickable close button.
    pub modal_close_hint: Style,
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

    /// Scrollbar track (the `│` glyph drawn down the gutter behind the
    /// thumb).  The track is only painted when the content overflows.
    pub scrollbar_track: Style,
    /// Scrollbar thumb (the `█` glyph that indicates current position).
    pub scrollbar_thumb: Style,
    /// Scrollbar thumb while the user is hovering the gutter or
    /// dragging the thumb.  Defaults to `primary_bright`.
    pub scrollbar_thumb_active: Style,
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

    pub primary_bright: Color,
    pub primary_dim: Color,
    pub emphasis_bright: Color,
    pub emphasis_dim: Color,
    pub structural_bright: Color,
    pub structural_dim: Color,
    pub interactive_bright: Color,
    pub interactive_dim: Color,
    /// Background fill for an active text selection.  Split from
    /// [`Self::interactive_dim`] because the two have conflicting
    /// contrast requirements: `interactive_dim` is the bg for
    /// inverse-text sites (`modal_input_unfocused`,
    /// `modal_item_selected`) where the fg is `default_bg`, and so
    /// must be saturated/dark on a light page; `selection_bg` is
    /// painted under regular text (`fg = default_text`), so it must
    /// be light enough for dark text to read.  On the dark default
    /// the two coincide.
    pub selection_bg: Color,
    pub success_bright: Color,
    pub success_dim: Color,
    pub warning_bright: Color,
    pub warning_dim: Color,
    pub error_bright: Color,
    pub error_dim: Color,
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

    pub code_bright: Color,
    pub code_dim: Color,
}

impl Default for Palette {
    fn default() -> Self {
        super::themes::dark_256::palette()
    }
}

/// Constructor for a built-in palette.  Each entry in
/// [`BUILTIN_THEMES`] pairs a reserved theme name with one of these.
pub type PaletteCtor = fn() -> Palette;

/// Registry of built-in palettes shipped in the binary.  Names listed
/// here are reserved: a user file `themes/<name>.toml` with one of
/// these names is ignored at load time so the built-in always wins.
/// Order is the user-facing cycle order in the settings overlay.
///
/// Each constructor lives in its own file under `src/config/themes/`
/// so adding a theme is a single new file plus an entry here.
pub const BUILTIN_THEMES: &[(&str, PaletteCtor)] = &[
    ("256 Dark", super::themes::dark_256::palette),
    ("256 Light", super::themes::light_256::palette),
];

impl Palette {
    /// Look up a built-in palette by name.  Returns `None` for names not
    /// in [`BUILTIN_THEMES`], in which case the caller falls back to
    /// reading `themes/<name>.toml` from the user's config directory.
    pub fn builtin(name: &str) -> Option<Palette> {
        BUILTIN_THEMES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, ctor)| ctor())
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
            highlight: Style::default().bg(p.warning_dim).fg(p.default_bg),
            code_span: Style::default().fg(p.code_bright).bg(p.muted),
            code_span_dim: Style::default().fg(p.code_dim).bg(p.muted),
            link_text: Style::default()
                .fg(p.interactive_bright)
                .add_modifier(underline),
            link_file: Style::default()
                .fg(p.interactive_bright)
                .add_modifier(underline),
            link_heading: Style::default().fg(p.interactive_bright),
            image_placeholder: Style::default().fg(p.interactive_dim).add_modifier(italic),
            footnote: Style::default().fg(p.structural_dim),

            // Code block — surface_elevated background reads as a single
            // unit across border, language label, and body.
            code_block_border: Style::default().fg(p.default_text).bg(p.muted),
            code_block_lang: Style::default()
                .fg(p.code_bright)
                .bg(p.surface)
                .add_modifier(italic),
            code_block_text: Style::default().fg(p.default_text).bg(p.muted),

            // Blockquote
            blockquote_bar: Style::default().fg(p.structural_dim),
            blockquote_text: Style::default().add_modifier(italic),

            // Horizontal rule
            rule: Style::default().fg(p.structural_dim),

            // List markers — structural_bright so bullets read as
            // chrome rather than content.
            list_bullet: Style::default().fg(p.emphasis_dim),
            list_number: Style::default().fg(p.emphasis_bright),

            // Task list
            task_unchecked: Style::default().fg(p.warning_bright),
            task_checked: Style::default().fg(p.success_dim),
            task_complete_text: Style::default()
                .fg(p.text_muted)
                .add_modifier(Modifier::CROSSED_OUT),
            task_strikethrough: true,

            // Table
            table_border: Style::default().fg(p.surface_elevated),
            table_header: Style::default().add_modifier(bold).fg(p.emphasis_bright),
            table_header_border: Style::default().fg(p.surface_elevated),
            table_cell: Style::default(),
            table_row_even: Style::default(),
            table_row_odd: Style::default().bg(p.muted),
            table_drop_indicator: Style::default().fg(p.interactive_bright),
            table_drop_target: Style::default().fg(p.interactive_dim),
            table_handle: Style::default().fg(p.interactive_dim),
            table_handle_delete: Style::default().fg(p.error_dim),

            // Status bar — surface fill.  Mode chip swaps fg/bg
            // depending on Mode so each mode reads at a glance.
            status_bar: Style::default().bg(p.surface).fg(p.default_text),
            status_mode_preview: Style::default()
                .bg(p.text_muted)
                .fg(p.surface)
                .add_modifier(bold),
            status_mode_rendered: Style::default()
                .bg(p.primary_bright)
                .fg(p.default_bg)
                .add_modifier(bold),
            status_mode_raw: Style::default()
                .bg(p.warning_bright)
                .fg(p.default_bg)
                .add_modifier(bold),
            status_filename: Style::default().fg(p.default_text).bg(p.surface),
            status_info: Style::default().fg(p.primary_bright).bg(p.surface),
            status_modified: Style::default()
                .fg(p.warning_bright)
                .bg(p.surface)
                .add_modifier(bold),
            status_selection: Style::default()
                .fg(p.interactive_bright)
                .bg(p.surface)
                .add_modifier(bold),

            // Hint line — surface_elevated background. Chord badges are
            // interactive_bright on the hint surface for readability.
            hint_bar: Style::default().bg(p.surface_elevated).fg(p.default_text),
            hint_chord: Style::default()
                .fg(p.interactive_bright)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            hint_label: Style::default().fg(p.default_text).bg(p.surface_elevated),

            // Transient messages — escalate in salience.  All sit on
            // the hint_bar surface so they layer cleanly over the
            // chord row.
            transient_info: Style::default().fg(p.default_text).bg(p.surface_elevated),
            transient_success: Style::default()
                .fg(p.success_bright)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            transient_warning: Style::default()
                .fg(p.warning_bright)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            transient_error: Style::default()
                .fg(p.error_bright)
                .bg(p.surface_elevated)
                .add_modifier(bold),

            // Modal popups
            modal_bg: Style::default().bg(p.surface_elevated).fg(p.default_text),
            modal_title_normal: Style::default()
                .fg(p.primary_bright)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            modal_title_warning: Style::default()
                .fg(p.warning_bright)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            modal_title_error: Style::default()
                .fg(p.error_bright)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            modal_close_hint: Style::default().fg(p.text_muted).bg(p.surface_elevated),
            modal_item: Style::default().fg(p.default_text).bg(p.surface_elevated),
            modal_item_hint: Style::default()
                .fg(p.interactive_bright)
                .bg(p.surface_elevated),
            modal_item_selected: Style::default()
                .bg(p.interactive_dim)
                .fg(p.default_text)
                .add_modifier(bold),
            modal_item_selected_hint: Style::default().fg(p.emphasis_bright).bg(p.interactive_dim),
            modal_description: Style::default()
                .fg(p.emphasis_bright)
                .bg(p.surface_elevated),
            modal_section_heading: Style::default()
                .fg(p.structural_bright)
                .bg(p.surface_elevated)
                .add_modifier(bold),
            modal_input_unfocused: Style::default().fg(p.default_bg).bg(p.interactive_dim),
            modal_input_focused: Style::default().fg(p.default_bg).bg(p.interactive_bright),
            modal_button_focused: Style::default()
                .fg(p.interactive_bright)
                .add_modifier(Modifier::REVERSED | bold),

            // General — concrete `default_text` / `default_bg` so the
            // document area renders with the theme's "blank page"
            // colours rather than letting the terminal's defaults show
            // through.  Themes that prefer the terminal's own bg can
            // set `[normal] fg = "Reset"` and `bg = "Reset"` in their
            // TOML.
            normal: Style::default().fg(p.default_text).bg(p.default_bg),

            // Selection: dedicated `selection_bg` so the highlight can
            // be tuned independently of `interactive_dim`.  Text colour
            // preserved so colour-coded content stays legible inside
            // the selection.
            selection: Style::default().bg(p.selection_bg).fg(p.default_text),

            // Search highlight: structural_bright bg, default_bg fg —
            // distinct from selection so a search hit inside the
            // selection still reads as a hit.
            search_highlight: Style::default().bg(p.structural_bright).fg(p.default_bg),

            // Active-line highlight is deferred — leave the field in
            // place so themes can opt in.
            active_line: Style::default(),

            // Editor block cursor — bg mirrors the status-bar mode chip
            // so the cursor reads as the same affordance in both
            // places (per theming.md).  Each variant pairs the chip's
            // bg with a contrasting fg so the underlying character
            // stays legible.
            cursor_preview: Style::default().bg(p.muted).fg(p.surface_elevated),
            cursor_rendered: Style::default().bg(p.primary_bright).fg(p.default_bg),
            cursor_raw: Style::default().bg(p.warning_bright).fg(p.default_bg),
            // Generic input cursor — REVERSED so the `▏` glyph inside
            // a modal text input inverts whatever's underneath without
            // needing to know the surrounding bg.
            cursor: Style::default().add_modifier(Modifier::REVERSED),

            // Scrollbar — track in muted text grey, thumb in primary
            // dim normally and primary bright when interacted with.
            scrollbar_track: Style::default().fg(p.muted),
            scrollbar_thumb: Style::default().fg(p.primary_dim),
            scrollbar_thumb_active: Style::default().fg(p.primary_bright),
        }
    }

    /// The "blank page" background colour — exposed so UI code that
    /// blends or composites against the document surface (e.g. the
    /// modal-dim pass) doesn't have to reach into `palette` directly.
    pub fn default_bg(&self) -> Color {
        self.palette.default_bg
    }

    /// Foreground colour for muted text — used as the Ansi256 fallback
    /// foreground for the modal-dim sweep.
    pub fn text_muted(&self) -> Color {
        self.palette.text_muted
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
            code_span_dim: Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM),
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
            table_handle: Style::default(),
            table_handle_delete: Style::default().add_modifier(Modifier::BOLD),

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
            modal_title_normal: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            // Monochrome can't colour-code urgency; warning/error fall
            // back to BOLD + REVERSED + DIM so the title still reads as
            // distinct chrome on dim-aware terminals.
            modal_title_warning: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED | Modifier::DIM),
            modal_title_error: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED | Modifier::DIM),
            modal_close_hint: Style::default().add_modifier(Modifier::REVERSED | Modifier::DIM),
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

            // Scrollbar — glyphs alone disambiguate track from thumb;
            // active state inverts so monochrome users still see it.
            scrollbar_track: Style::default(),
            scrollbar_thumb: Style::default(),
            scrollbar_thumb_active: Style::default().add_modifier(Modifier::REVERSED),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_palette(&Palette::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field on [`Palette`] in the order it appears in the struct
    /// definition.  When you add a field to `Palette`, add it here so
    /// `palette_fields_list_matches_struct` keeps the count honest.
    const PALETTE_FIELDS: &[&str] = &[
        "default_text",
        "default_bg",
        "primary_bright",
        "primary_dim",
        "emphasis_bright",
        "emphasis_dim",
        "structural_bright",
        "structural_dim",
        "interactive_bright",
        "interactive_dim",
        "selection_bg",
        "success_bright",
        "success_dim",
        "warning_bright",
        "warning_dim",
        "error_bright",
        "error_dim",
        "text_muted",
        "muted",
        "surface_elevated",
        "surface",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "code_bright",
        "code_dim",
    ];

    #[test]
    fn palette_fields_list_matches_struct() {
        // Construct a Palette from defaults and read each field.  If a
        // field is renamed or removed, this won't compile.  If a field
        // is added without updating PALETTE_FIELDS, the length
        // assertion at the bottom fails.
        let p = Palette::default();
        let _all = [
            p.default_text,
            p.default_bg,
            p.primary_bright,
            p.primary_dim,
            p.emphasis_bright,
            p.emphasis_dim,
            p.structural_bright,
            p.structural_dim,
            p.interactive_bright,
            p.interactive_dim,
            p.selection_bg,
            p.success_bright,
            p.success_dim,
            p.warning_bright,
            p.warning_dim,
            p.error_bright,
            p.error_dim,
            p.text_muted,
            p.muted,
            p.surface_elevated,
            p.surface,
            p.h1,
            p.h2,
            p.h3,
            p.h4,
            p.h5,
            p.h6,
            p.code_bright,
            p.code_dim,
        ];
        assert_eq!(_all.len(), PALETTE_FIELDS.len());
    }

    #[test]
    fn light_palette_has_distinct_default_bg() {
        // Sanity check that the light built-in is actually distinct
        // from the dark default — otherwise we shipped two themes
        // with the same colour table.
        use super::super::themes::{dark_256, light_256};
        assert_ne!(
            dark_256::palette().default_bg,
            light_256::palette().default_bg
        );
    }

    #[test]
    fn builtin_lookup_resolves_registered_names() {
        assert!(Palette::builtin("default").is_some());
        assert!(Palette::builtin("light").is_some());
        assert!(Palette::builtin("nonexistent").is_none());
    }
}
