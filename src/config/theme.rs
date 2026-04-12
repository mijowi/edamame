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
    pub code_span: Style,
    pub link_text: Style,
    pub link_url: Style,
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
    pub task_checked: Style,

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

    // ── General text ──────────────────────────────────────────────
    pub normal: Style,
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
            code_span: Style::default().fg(Color::Yellow).bg(Color::Indexed(236)),
            link_text: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::UNDERLINED),
            link_url: Style::default().fg(Color::DarkGray),
            image_placeholder: Style::default().fg(Color::Blue).add_modifier(Modifier::ITALIC),

            // Code block
            code_block_border: Style::default().fg(Color::Indexed(240)),
            code_block_lang: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
            code_block_text: Style::default().fg(Color::White).bg(Color::Indexed(234)),

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
            task_checked: Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::CROSSED_OUT),

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

            // General
            normal: Style::default(),
        }
    }
}

impl Theme {
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
}
