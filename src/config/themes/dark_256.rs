use ratatui::style::{Color, Modifier, Style};

use crate::config::theme::{Palette, Theme};

/// `256 Dark`'s palette, also `Palette::default()`: the Edamame look in the 256-color cube — warm
/// orange brand on near-black, edamame-bean green for success, and a cool purple for chrome so
/// the alternating heading ramp reads as two hue families.
pub fn palette() -> Palette {
    Palette {
        text: Color::Indexed(253),
        text_muted: Color::Indexed(245),
        bg: Color::Indexed(233),
        bg_muted: Color::Indexed(235),
        surface: Color::Indexed(236),
        surface_elevated: Color::Indexed(237),

        primary: Color::Indexed(208),
        secondary: Color::Indexed(97),
        // Dark enough to carry `text` (253) when used as selection bg, saturated enough to read as
        // a foreground on the document surface.
        accent: Color::Indexed(25),
        link: Color::Indexed(39),

        success: Color::Indexed(76),
        warning: Color::Indexed(220),
        error: Color::Indexed(196),

        // Lighter purple than `secondary`, so inline code reads distinct from chrome.
        code: Color::Indexed(140),

        // Distinct from `success` / `error` so no two palette slots share a value.
        diff_add: Color::Indexed(34),
        diff_delete: Color::Indexed(160),

        light: false,
    }
}

/// [`Theme::from_palette`] plus a curated `h1`–`h6` ramp: its darkening only works for RGB colors,
/// and stepping indexed colors through the 6×6×6 cube shifts hue, so 256-color built-ins pin their
/// own shades.
pub fn theme() -> Theme {
    let mut t = Theme::from_palette(&palette());
    let bold = Modifier::BOLD;
    let underline = Modifier::UNDERLINED;
    // Alternates primary (orange) and secondary (purple), dulling with each level.
    let h1 = Color::Indexed(208); // primary, bright
    let h2 = Color::Indexed(99); // secondary, bright violet
    let h3 = Color::Indexed(172); // primary, medium
    let h4 = Color::Indexed(97); // secondary, medium
    let h5 = Color::Indexed(130); // primary, dull
    let h6 = Color::Indexed(60); // secondary, dull
    t.h1 = Style::default().fg(h1).add_modifier(bold);
    t.h1_rule = Style::default().fg(h1);
    t.h2 = Style::default().fg(h2).add_modifier(bold | underline);
    t.h3 = Style::default().fg(h3).add_modifier(bold | underline);
    t.h4 = Style::default().fg(h4).add_modifier(bold | underline);
    t.h5 = Style::default().fg(h5).add_modifier(bold | underline);
    t.h6 = Style::default().fg(h6).add_modifier(bold | underline);

    // A lifted neutral grey, distinct from the striped-row bg (235) so a code span inside a stripe
    // still reads as code.  Hand-picked because cube-stepping `code` (140) would shift hue.
    let code_bg = Color::Indexed(238);
    t.code_span = Style::default().fg(palette().code).bg(code_bg);
    t.code_span_dim = Style::default()
        .fg(palette().code)
        .bg(code_bg)
        .add_modifier(Modifier::DIM);
    t.code_block_border = Style::default().fg(palette().text).bg(code_bg);
    t.code_block_text = Style::default().fg(palette().text).bg(code_bg);

    // Hand-picked against `code_bg` (238) because the derived path lifts dark tokens with `blend`,
    // a no-op for indexed colors.  Every entry clears 4.5:1 there
    // (`syntax_contrast_clears_the_floor_for_every_builtin_theme`), each the bright-tint sibling of
    // the slot the RGB derivation would use — those mid shades are picked to read on `bg` (233).
    t.syntax_keyword = Style::default()
        .fg(Color::Indexed(214)) // orange, bright sibling of primary 208
        .add_modifier(bold);
    t.syntax_string = Style::default().fg(Color::Indexed(76)); // success green
    t.syntax_comment = Style::default()
        .fg(Color::Indexed(249)) // the most recessive grey still legible here
        .add_modifier(Modifier::ITALIC);
    t.syntax_number = Style::default().fg(Color::Indexed(220)); // warning amber
    t.syntax_type = Style::default().fg(Color::Indexed(183)); // light purple, cf. code 140
    t.syntax_function = Style::default().fg(Color::Indexed(117)); // light blue, cf. link 39
    t.syntax_attribute = Style::default().fg(Color::Indexed(217)); // light red, cf. error 196

    // The faintest greyscale lift over `bg`, below both the stripe (235) and code (238) surfaces.
    // The derived version blends `secondary` and would leave a full-strength purple wash.
    t.blockquote_text = Style::default().bg(Color::Indexed(234));

    // The derived blend would leave the bare `surface` grey (236), barely separable from `bg`.
    // A dark navy reads as a highlight while staying recessive against the focused match's 25.
    let selection_muted_bg = Color::Indexed(18);
    t.selection_muted = Style::default().bg(selection_muted_bg).fg(palette().text);

    // Diff washes.  The `blend` no-op is worse here than for `selection_muted`: every `diff_*`
    // style would collapse to the bare `surface` grey, and `diff_view` paints add / delete rows
    // with no gutter, so an addition and a deletion would render identically.
    //
    // Two constraints. (1) A wash sits behind whatever the markdown painted, so heading orange and
    // code purple must stay legible on it — only the darkest tints leave room. (2) The cube's
    // greens carry far more luminance than its reds: against `text` (253), 22 measures 5.7:1 but 28
    // only 3.4:1 and 34 just 2.1:1, while 52 / 88 / 124 all stay above 5:1.
    //
    // Hence 22 / 52, the faintest (and darkest expressible) shade of each hue.  That leaves no
    // second green level, so focused and non-focused rows share a wash and focus is carried by the
    // inline highlights below and the decision divider instead; the alternative is 28 behind body
    // text.
    let add_wash = Color::Indexed(22); // #005f00
    let delete_wash = Color::Indexed(52); // #5f0000
    t.diff_add_line = Style::default().bg(add_wash);
    t.diff_add_line_unfocused = Style::default().bg(add_wash);
    t.diff_delete_line = Style::default().bg(delete_wash);
    t.diff_delete_line_unfocused = Style::default().bg(delete_wash);

    // Inline highlights sit *on* the wash, so they step a shade deeper.  The focused pair is a
    // saturated fill pinning the foreground `best_contrast` would pick if it could measure indexed
    // colors — near-black on the bright green (6.4:1), `text` on the dark red (5.3:1).
    t.diff_add_inline_unfocused = Style::default().bg(Color::Indexed(28));
    t.diff_delete_inline_unfocused = Style::default().bg(Color::Indexed(88));
    t.diff_add_inline = Style::default()
        .bg(Color::Indexed(34))
        .fg(palette().bg)
        .add_modifier(Modifier::BOLD);
    t.diff_delete_inline = Style::default()
        .bg(Color::Indexed(124))
        .fg(palette().text)
        .add_modifier(Modifier::BOLD);

    // Green on the status line, red on the hint line, mirroring the adds-below / deletes-above
    // stacking, in the same wash shades as the document rows.
    t.status_bar_diff = Style::default().bg(add_wash).fg(palette().text);
    t.hint_bar_diff = Style::default().bg(delete_wash).fg(palette().text);
    t
}
