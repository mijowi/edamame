use ratatui::style::{Color, Modifier, Style};

use crate::config::theme::{Palette, Theme};

/// Companion light palette to [`super::dark_256`].  Same color
/// families (warm orange brand, edamame-green success, cool purple
/// chrome) re-tuned for a near-white page: brand colors are darker
/// and more saturated so they pop against the light surface.
/// Inverse-text sites (`fg = bg`) on any saturated brand color rely
/// on those colors being dark enough to contrast with near-white —
/// yellows are shifted to amber/orange-gold for that reason.
pub fn palette() -> Palette {
    Palette {
        text: Color::Indexed(234),
        text_muted: Color::Indexed(244),
        bg: Color::Indexed(254),
        bg_muted: Color::Indexed(252),
        surface: Color::Indexed(251),
        surface_elevated: Color::Indexed(250),

        // Orange — brand identity.
        primary: Color::Indexed(166),
        // Mid purple — structural chrome.  Cool hue chosen to
        // contrast with the warm `primary` orange.
        secondary: Color::Indexed(97),
        // Medium blue — list markers, table header, selection bg.
        // Pale enough to let the near-black `text` (234) read
        // through when used as selection bg, dark enough to read as
        // a fg on the near-white page.
        accent: Color::Indexed(75),
        // Saturated blue — link foreground.
        link: Color::Indexed(21),

        success: Color::Indexed(28),
        // Amber — shifted darker than the dark palette's yellow so
        // inverse-text sites (highlight, raw-mode chip,
        // task_unchecked text color) stay legible against the
        // near-white page.
        warning: Color::Indexed(172),
        error: Color::Indexed(124),

        // Inline code — saturated purple foreground on the muted
        // light grey.  Distinct enough from the `secondary` mid
        // purple that inline-code reads as code rather than chrome.
        code: Color::Indexed(91),

        // Reserved for a future diff view; not consumed yet.  Picked
        // distinct from `success` / `error` so the palette has no
        // duplicate slot values.
        diff_add: Color::Indexed(22),
        diff_delete: Color::Indexed(88),

        light: true,
    }
}

/// Built-in theme: builds a [`Theme`] from [`palette`] and pins the
/// `h1`–`h6` heading ramp to curated 256-cube shades.  See the
/// `dark_256::theme` doc for the rationale.
pub fn theme() -> Theme {
    let mut t = Theme::from_palette(&palette());
    let bold = Modifier::BOLD;
    let underline = Modifier::UNDERLINED;
    // Heading ramp: alternates primary (orange) and secondary (purple),
    // each shade pushed darker than its dark-palette counterpart so
    // it reads on a light page.
    let h1 = Color::Indexed(166); // primary, bright
    let h2 = Color::Indexed(53); // secondary, bright (dark purple)
    let h3 = Color::Indexed(130); // primary, medium
    let h4 = Color::Indexed(97); // secondary, medium
    let h5 = Color::Indexed(94); // primary, dull
    let h6 = Color::Indexed(60); // secondary, dull
    t.h1 = Style::default().fg(h1).add_modifier(bold);
    t.h1_rule = Style::default().fg(h1);
    t.h2 = Style::default().fg(h2).add_modifier(bold | underline);
    t.h3 = Style::default().fg(h3).add_modifier(bold | underline);
    t.h4 = Style::default().fg(h4).add_modifier(bold | underline);
    t.h5 = Style::default().fg(h5).add_modifier(bold | underline);
    t.h6 = Style::default().fg(h6).add_modifier(bold | underline);

    // Code surface: a light shade between `bg` (254) and `bg_muted`
    // (252, the striped-row bg) so a code span inside a stripe still
    // reads as code.  Indexed-cube stepping of `code` (91, saturated
    // purple) shifts hue, so we pick the shade by hand.
    let code_bg = Color::Indexed(253);
    t.code_span = Style::default().fg(palette().code).bg(code_bg);
    t.code_span_dim = Style::default()
        .fg(palette().code)
        .bg(code_bg)
        .add_modifier(Modifier::DIM);
    t.code_block_border = Style::default().fg(palette().text).bg(code_bg);
    t.code_block_text = Style::default().fg(palette().text).bg(code_bg);

    // Syntax highlighting, hand-picked against `code_bg` (253).  Same
    // reason as `dark_256`: the derived path's contrast lift uses
    // `blend`, a no-op for indexed colours.  Every entry clears 4.5:1
    // on 253.  The cube offers no dark orange that does, so `keyword`
    // takes the deep red that light editor themes conventionally give
    // it and `attribute` moves to teal rather than crowding the reds —
    // the hue families differ from the RGB derivation's, the meanings
    // do not.
    t.syntax_keyword = Style::default()
        .fg(Color::Indexed(124)) // deep red
        .add_modifier(Modifier::BOLD);
    t.syntax_string = Style::default().fg(Color::Indexed(22)); // dark green
    t.syntax_comment = Style::default()
        .fg(Color::Indexed(59)) // the most recessive grey still legible here
        .add_modifier(Modifier::ITALIC);
    t.syntax_number = Style::default().fg(Color::Indexed(58)); // dark olive
    t.syntax_type = Style::default().fg(Color::Indexed(91)); // purple, the `code` slot
    t.syntax_function = Style::default().fg(Color::Indexed(21)); // blue, the `link` slot
    t.syntax_attribute = Style::default().fg(Color::Indexed(23)); // teal

    // Blockquote surface.  Same `blend` no-op as the code surface, so
    // the wash is picked by hand.  The greyscale ramp has no room
    // between `bg` (254) and `code_bg` (253), so unlike the dark theme
    // the quote wash sits one step *past* the code surface rather than
    // short of it — a code span inside a quote still separates, just in
    // the other direction.  252 is also the striped-row bg, which a
    // quote can never sit inside.
    t.blockquote_text = Style::default().bg(Color::Indexed(252));

    // Muted selection (non-focused search matches).  See the
    // `dark_256` counterpart: the derived blend is a no-op for indexed
    // colors and would leave the highlight as the bare `surface` grey
    // (251) against a 254 page.  Pick a pale blue instead — a washed
    // version of `accent` (75) that still reads as a highlight on the
    // near-white page while the focused match's saturated 75 stays
    // clearly stronger.
    let selection_muted_bg = Color::Indexed(153);
    t.selection_muted = Style::default().bg(selection_muted_bg).fg(palette().text);

    // Diff washes.  Same `blend` no-op as `selection_muted` — see the
    // `dark_256` counterpart for why it matters most here: `diff_view`
    // paints add / delete rows with no gutter, so collapsing both to the
    // bare `surface` grey makes an addition and a deletion identical.
    //
    // Unlike the dark theme, this palette has room for the full
    // hierarchy the derived styles intend: against near-black `text`
    // (234) every pale tint below clears 7:1, so the four levels can be
    // spent on focus and inline depth rather than on legibility.  Each
    // hue therefore ramps pale wash → stronger wash (focused) → muted
    // patch (non-focused inline) → saturated patch (focused inline).
    // The non-focused inline shades (151 / 181) are the greyer members
    // of each ramp, so a changed word inside a non-focused hunk reads as
    // deeper than its wash without competing with the focused hunk's
    // brighter fills.
    t.diff_add_line_unfocused = Style::default().bg(Color::Indexed(194)); // #d7ffd7
    t.diff_add_line = Style::default().bg(Color::Indexed(157)); // #afffaf
    t.diff_delete_line_unfocused = Style::default().bg(Color::Indexed(224)); // #ffd7d7
    t.diff_delete_line = Style::default().bg(Color::Indexed(217)); // #ffafaf

    // Inline (within-line) change highlights.  Bold on the focused pair
    // only, as in the derived styles.  No foreground is pinned here —
    // `text` clears 7:1 on all four shades, so unlike the dark theme's
    // bright green there is nothing to rescue, and leaving fg unset lets
    // the markdown's own colors show through the highlight.
    t.diff_add_inline_unfocused = Style::default().bg(Color::Indexed(151)); // #afd7af
    t.diff_delete_inline_unfocused = Style::default().bg(Color::Indexed(181)); // #d7afaf
    t.diff_add_inline = Style::default()
        .bg(Color::Indexed(114)) // #87d787
        .add_modifier(Modifier::BOLD);
    t.diff_delete_inline = Style::default()
        .bg(Color::Indexed(210)) // #ff8787
        .add_modifier(Modifier::BOLD);

    // Bottom region in diff mode — green tint on the status line, red on
    // the hint line, mirroring the adds-below / deletes-above stacking.
    // Reuses the faint wash shades so the bars read as the same language
    // as the document rows.
    t.status_bar_diff = Style::default().bg(Color::Indexed(194)).fg(palette().text);
    t.hint_bar_diff = Style::default().bg(Color::Indexed(224)).fg(palette().text);
    t
}
