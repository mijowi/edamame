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
    pub modal_title: Style,
    pub modal_button_focused: Style,

    // ── General text ──────────────────────────────────────────────
    pub normal: Style,

    /// Background style applied to characters inside an active text selection.
    /// Renders on top of the character's own style so colour-coded content
    /// stays legible.
    pub selection: Style,

    /// Style for the block cursor.  Default is `REVERSED` only, which
    /// swaps fg/bg of whatever's underneath — themable so users can
    /// pick a concrete colour (e.g. a bright fill) if they prefer.
    pub cursor: Style,
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

            // Code block — background matches inline code spans (Indexed(236))
            // across the border, language label, and body so the whole block
            // reads as one unit.
            code_block_border: Style::default()
                .fg(Color::Indexed(240))
                .bg(Color::Indexed(236)),
            code_block_lang: Style::default()
                .fg(Color::Yellow)
                .bg(Color::Indexed(236))
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
            // Inner status spans share the status_bar background (Indexed(236))
            // so the whole row reads as a single chrome block — set
            // explicitly rather than relying on Paragraph-level inheritance.
            status_filename: Style::default().fg(Color::White).bg(Color::Indexed(236)),
            status_info: Style::default().fg(Color::Gray).bg(Color::Indexed(236)),
            status_modified: Style::default()
                .fg(Color::Yellow)
                .bg(Color::Indexed(236))
                .add_modifier(Modifier::BOLD),
            status_selection: Style::default()
                .fg(Color::Cyan)
                .bg(Color::Indexed(236))
                .add_modifier(Modifier::BOLD),

            // Hint line — the bar itself uses the same bg family as
            // the status bar so both rows read as a single chrome
            // block, but one step lighter (237) so they remain
            // visually distinguishable.  Chord badges contrast
            // strongly against it, nano-style: light-grey bg with
            // dark text so the chord "badge" jumps off the bar.
            hint_bar: Style::default().bg(Color::Indexed(237)).fg(Color::Gray),
            hint_chord: Style::default()
                .bg(Color::Indexed(250))
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
            hint_label: Style::default().bg(Color::Indexed(237)).fg(Color::White),

            // Transient messages — kinds escalate in salience.  All
            // share the hint-bar background so they layer cleanly over
            // the chord row.
            transient_info: Style::default().bg(Color::Indexed(237)).fg(Color::White),
            transient_success: Style::default()
                .bg(Color::Indexed(237))
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            transient_warning: Style::default()
                .bg(Color::Indexed(237))
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            transient_error: Style::default()
                .bg(Color::Red)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),

            // Modal popups
            modal_title: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            modal_button_focused: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),

            // General — Color::Reset explicitly emits an SGR reset for fg
            // and bg, producing the same visual result as leaving both unset
            // while still exposing the field as a user-configurable knob.
            normal: Style::default().fg(Color::Reset).bg(Color::Reset),

            // Selection: muted blue background, distinguishable from cursor
            // which uses REVERSED.  Falls back to REVERSED in monochrome mode.
            selection: Style::default().bg(Color::Indexed(24)),

            // Cursor: REVERSED only, so it inverts whatever colour the
            // underlying character already has.  Users can override to pin
            // the cursor to a specific fill colour.
            cursor: Style::default().add_modifier(Modifier::REVERSED),
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
            status_selection: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),

            hint_bar: Style::default(),
            hint_chord: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            hint_label: Style::default(),

            transient_info: Style::default().add_modifier(Modifier::REVERSED),
            transient_success: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            transient_warning: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            transient_error: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),

            modal_title: Style::default().add_modifier(Modifier::BOLD),
            modal_button_focused: Style::default()
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),

            normal: Style::default(),
            selection: Style::default().add_modifier(Modifier::REVERSED),
            cursor: Style::default().add_modifier(Modifier::REVERSED),
        }
    }
}
