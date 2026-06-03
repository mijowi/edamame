//! Monochrome built-in theme — no color escapes, only text attribute
//! modifiers (bold / italic / underline / reversed / dim).  Recommended
//! for terminals reporting `ColorDepth::Ansi16` or `NoColor`; selected
//! automatically on first launch when color support is limited.
//!
//! Every palette slot resolves to [`Color::Reset`] so any site that
//! reads `theme.palette.<x>` directly (big-H1 background, table border
//! bg, command-palette / theme-picker rows, …) still produces a
//! color-free escape: the terminal's own default fg/bg shows through.
//! Text attributes are preserved because they work over SGR regardless
//! of color depth.

use ratatui::style::{Color, Modifier, Style};

use crate::config::theme::{Palette, Theme};

/// Colorless palette — every slot resolves to the terminal's own
/// default fg/bg via [`Color::Reset`].  `light` is `false` only
/// because the field is non-optional; for the appearance-mode filter
/// monochrome is classified as dark (the registry name pins this).
pub fn palette() -> Palette {
    let r = Color::Reset;
    Palette {
        text: r,
        text_muted: r,
        bg: r,
        bg_muted: r,
        surface: r,
        surface_elevated: r,
        primary: r,
        secondary: r,
        accent: r,
        link: r,
        success: r,
        warning: r,
        error: r,
        code: r,
        diff_add: r,
        diff_delete: r,
        light: false,
    }
}

pub fn theme() -> Theme {
    Theme {
        palette: palette(),

        h1: Style::default().add_modifier(Modifier::BOLD),
        h1_rule: Style::default(),
        h2: Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        h3: Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        h4: Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        h5: Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        h6: Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),

        bold: Style::default().add_modifier(Modifier::BOLD),
        italic: Style::default().add_modifier(Modifier::ITALIC),
        strikethrough: Style::default().add_modifier(Modifier::CROSSED_OUT),
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
        table_drop_indicator: Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        table_drop_target: Style::default().add_modifier(Modifier::REVERSED),
        table_handle: Style::default(),
        table_handle_delete: Style::default().add_modifier(Modifier::BOLD),

        status_bar: Style::default().add_modifier(Modifier::REVERSED),
        status_mode_preview: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        status_mode_rendered: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
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
        // Monochrome can't color-code urgency; warning/error fall
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
        // Monochrome can't color-code distinction; `DIM` reads as
        // "marked but quiet" — distinct from BOLD (focused selection)
        // and plain (unselected) without using REVERSED (which is
        // already the unselected `modal_item` state in monochrome).
        modal_item_selected_unfocused: Style::default().add_modifier(Modifier::DIM),
        modal_item_selected_hint: Style::default().add_modifier(Modifier::BOLD),
        modal_description: Style::default().add_modifier(Modifier::REVERSED),
        modal_section_heading: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        // Focused input matches `modal_button_focused` (REVERSED|BOLD)
        // — filled.  Unfocused input is plain BOLD (no fill), so it
        // reads as "an input here" without competing with the focused
        // element, mirroring the colored theme's filled-vs-outlined
        // pattern.
        modal_input_unfocused: Style::default().add_modifier(Modifier::BOLD),
        modal_input_focused: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        modal_button_focused: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),

        normal: Style::default(),
        selection: Style::default().add_modifier(Modifier::REVERSED),
        search_highlight: Style::default().add_modifier(Modifier::REVERSED),
        active_line: Style::default(),
        cursor_preview: Style::default().add_modifier(Modifier::REVERSED),
        cursor_rendered: Style::default().add_modifier(Modifier::REVERSED),
        cursor_raw: Style::default().add_modifier(Modifier::REVERSED),
        cursor: Style::default().add_modifier(Modifier::REVERSED),

        line_number: Style::default().add_modifier(Modifier::DIM),

        // Scrollbar — glyphs alone disambiguate track from thumb;
        // active state inverts so monochrome users still see it.
        scrollbar_track: Style::default(),
        scrollbar_thumb: Style::default(),
        scrollbar_thumb_active: Style::default().add_modifier(Modifier::REVERSED),

        // Diff mode (Phase 1) — monochrome fallback per §7.  Line bg
        // can't be a saturated mix, so we use REVERSED on the whole
        // line; inline highlights add BOLD on top to stand out from
        // the line bg.  Status / hint bars in diff mode become
        // REVERSED + BOLD so the mode shift is visible without color.
        diff_add_line: Style::default().add_modifier(Modifier::REVERSED),
        diff_delete_line: Style::default().add_modifier(Modifier::REVERSED),
        // Non-focused hunks dim instead of inverting, so the three
        // tiers (context plain / unfocused dim / focused reversed)
        // stay distinct without color.
        diff_add_line_unfocused: Style::default().add_modifier(Modifier::DIM),
        diff_delete_line_unfocused: Style::default().add_modifier(Modifier::DIM),
        diff_add_inline: Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        diff_delete_inline: Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        // No color: the checkbox label ("Accepted" / "Rejected") plus
        // bold/dim distinguishes the decision states.
        diff_decision_pending: Style::default().add_modifier(Modifier::DIM),
        diff_decision_accepted: Style::default().add_modifier(Modifier::BOLD),
        diff_decision_rejected: Style::default().add_modifier(Modifier::BOLD),
        status_mode_diff: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
        status_bar_diff: Style::default().add_modifier(Modifier::REVERSED),
        hint_bar_diff: Style::default().add_modifier(Modifier::REVERSED),
    }
}
