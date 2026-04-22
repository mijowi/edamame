use ratatui::style::{Color, Modifier, Style};

/// All colour and style values for rendering. No hardcoded colours exist
/// outside this struct — every rendered element derives its style from here.
///
/// The default is a dark-mode palette. Full user theming is a deferred feature;
/// the struct is wired in from Phase 0 so adding it later requires no refactoring.
#[derive(Debug, Clone)]
pub struct Theme {
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
    pub link_text: Style,
    pub image_placeholder: Style,

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
    /// Whether to render checked item text with strikethrough (default: true).
    pub task_strikethrough: bool,

    // ── Table ─────────────────────────────────────────────────────
    pub table_border: Style,
    pub table_header: Style,
    pub table_cell: Style,

    // ── Status bar ────────────────────────────────────────────────
    pub status_bar: Style,
    pub status_mode: Style,
    pub status_filename: Style,
    pub status_info: Style,
    pub status_modified: Style,

    // ── Modal popups ──────────────────────────────────────────────
    pub modal_title: Style,
    pub modal_button_focused: Style,

    // ── General text ──────────────────────────────────────────────
    pub normal: Style,

    /// Background style applied to characters inside an active text selection.
    /// Renders on top of the character's own style so colour-coded content
    /// stays legible.
    pub selection: Style,
}

impl Default for Theme {
    fn default() -> Self {
        // Dark-mode palette.
        let h_bold = Modifier::BOLD;

        Self {
            // Headings: bold + distinct colour per level, decreasing salience
            h1: Style::default().fg(Color::Magenta).add_modifier(h_bold),
            h1_rule: Style::default().fg(Color::Magenta),
            h2: Style::default().fg(Color::Cyan).add_modifier(h_bold),
            h3: Style::default().fg(Color::Yellow).add_modifier(h_bold),
            h4: Style::default().fg(Color::Green).add_modifier(h_bold),
            h5: Style::default().fg(Color::Blue).add_modifier(h_bold),
            h6: Style::default().fg(Color::Gray).add_modifier(h_bold),

            // Inline
            bold: Style::default().add_modifier(Modifier::BOLD),
            italic: Style::default().add_modifier(Modifier::ITALIC),
            strikethrough: Style::default()
                .add_modifier(Modifier::CROSSED_OUT)
                .fg(Color::DarkGray),
            highlight: Style::default().bg(Color::Yellow).fg(Color::Black),
            code_span: Style::default().fg(Color::Yellow).bg(Color::Indexed(236)),
            link_text: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::UNDERLINED),
            image_placeholder: Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::ITALIC),

            // Code block — background matches inline code spans (Indexed(236)).
            code_block_border: Style::default().fg(Color::Indexed(240)),
            code_block_lang: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
            code_block_text: Style::default().fg(Color::White).bg(Color::Indexed(236)),

            // Blockquote: muted vertical bar + slightly dim text
            blockquote_bar: Style::default().fg(Color::Blue),
            blockquote_text: Style::default().fg(Color::Gray),

            // Horizontal rule
            rule: Style::default().fg(Color::Indexed(240)),

            // List markers
            list_bullet: Style::default().fg(Color::Cyan),
            list_number: Style::default().fg(Color::Cyan),

            // Task list
            task_unchecked: Style::default().fg(Color::DarkGray),
            // task_checked styles the [x] marker; strikethrough on text is
            // controlled separately by task_strikethrough.
            task_checked: Style::default().fg(Color::Green),
            task_strikethrough: true,

            // Table
            table_border: Style::default().fg(Color::Indexed(240)),
            table_header: Style::default().add_modifier(Modifier::BOLD),
            table_cell: Style::default(),

            // Status bar: dark background, light text
            status_bar: Style::default().bg(Color::Indexed(236)).fg(Color::White),
            status_mode: Style::default()
                .bg(Color::DarkGray)
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            status_filename: Style::default().fg(Color::White),
            status_info: Style::default().fg(Color::Gray),
            status_modified: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),

            // Modal popups
            modal_title: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            modal_button_focused: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),

            // General
            normal: Style::default(),

            // Selection: muted blue background, distinguishable from cursor
            // which uses REVERSED.  Falls back to REVERSED in monochrome mode.
            selection: Style::default().bg(Color::Indexed(24)),
        }
    }
}

impl Theme {
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
            image_placeholder: Style::default().add_modifier(Modifier::ITALIC),

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
            task_strikethrough: true,

            table_border: Style::default(),
            table_header: Style::default().add_modifier(Modifier::BOLD),
            table_cell: Style::default(),

            status_bar: Style::default().add_modifier(Modifier::REVERSED),
            status_mode: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            status_filename: Style::default().add_modifier(Modifier::REVERSED),
            status_info: Style::default().add_modifier(Modifier::REVERSED),
            status_modified: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),

            modal_title: Style::default().add_modifier(Modifier::BOLD),
            modal_button_focused: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),

            normal: Style::default(),
            selection: Style::default().add_modifier(Modifier::REVERSED),
        }
    }
}
