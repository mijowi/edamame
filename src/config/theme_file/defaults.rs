use crate::config::theme::Theme;

use super::ThemeFile;

/// Build the contents of `themes/default.toml` from the compiled-in
/// [`Theme::default`] and [`crate::config::theme::Palette::default`].
///
/// The output is a header (`default_theme_header.txt`) followed by a
/// `[palette]` section in which every colour entry is *commented out* —
/// the `# field = value` lines show what the compiled defaults are without
/// actually overriding anything at load time.  Per-element style sections
/// (`[h1]`, `[modal_input_focused]`, …) follow as bare empty headers so
/// users can discover the available override slots.
///
/// Called from [`crate::config::init::ensure_default_files_in`] on first
/// run.  There is no checked-in `config/themes/default.toml` — the file
/// is generated at startup so it can never drift from the code-side
/// defaults.
pub fn default_theme_toml() -> String {
    let theme = Theme::default();

    // Build a ThemeFile carrying the default palette + default
    // task_strikethrough plus all-empty style specs.  Serializing it
    // produces the full skeleton (palette values + bare `[<element>]`
    // headers); we then comment out every line inside `[palette]`.
    let file = ThemeFile {
        palette: (&theme.palette).into(),
        task_strikethrough: theme.task_strikethrough,
        ..ThemeFile::default()
    };
    let body = toml::to_string_pretty(&file).expect("serialize default ThemeFile");

    // A blank line or the next `[...]` header ends the palette block;
    // anything else within the block is a palette entry we comment out so
    // it documents the default without overriding it.
    let mut out = String::new();
    let lines = body.lines().scan(false, |in_palette, line| {
        let was_in_palette = *in_palette;
        if was_in_palette && (line.is_empty() || line.starts_with('[')) {
            *in_palette = false;
        }
        let comment = was_in_palette && !line.is_empty() && !line.starts_with('[');
        if line == "[palette]" {
            *in_palette = true;
        }
        Some((line, comment))
    });
    for (line, comment) in lines {
        if comment {
            out.push_str("# ");
        }
        out.push_str(line);
        out.push('\n');
    }

    let header = include_str!("../default_theme_header.txt");
    format!("{header}\n{out}")
}
