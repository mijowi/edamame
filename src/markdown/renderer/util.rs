//! Shared helpers used across the renderer's block / inline pipelines.
//! Free functions only — none of these depend on `Renderer`.

use std::path::Path;

use ratatui::style::Style;
use ratatui::text::Span;

use crate::config::Theme;
use crate::markdown::table_layout::{self, char_cells};

/// One character tagged with its source span's style, so the table renderer's
/// inline-aware wrap keeps styling across a cell's row breaks.
#[derive(Debug, Clone, Copy)]
pub(super) struct StyledChar {
    pub(super) ch: char,
    pub(super) style: Style,
}

/// Terminal columns a styled run occupies — the unit every table-cell width decision uses, so a
/// CJK glyph costs two.
pub(super) fn styled_cells(chars: &[StyledChar]) -> usize {
    chars.iter().map(|c| char_cells(c.ch)).sum()
}

/// Wrap styled chars into rows of width ≤ `width` cells through [`table_layout::wrap_ranges`],
/// keeping each char's style.  Returns at least one (possibly empty) row.
pub(super) fn wrap_styled_chars(chars: &[StyledChar], width: usize) -> Vec<Vec<StyledChar>> {
    let plain: Vec<char> = chars.iter().map(|c| c.ch).collect();
    table_layout::wrap_ranges(&plain, width)
        .into_iter()
        .map(|r| chars[r].to_vec())
        .collect()
}

/// Append a `StyledChar` slice as `Span`s, coalescing same-style runs.
pub(super) fn extend_with_styled_chars(out: &mut Vec<Span<'static>>, chars: &[StyledChar]) {
    if chars.is_empty() {
        return;
    }
    let mut current_style = chars[0].style;
    let mut buf = String::new();
    for c in chars {
        if c.style != current_style {
            if !buf.is_empty() {
                out.push(Span::styled(std::mem::take(&mut buf), current_style));
            }
            current_style = c.style;
        }
        buf.push(c.ch);
    }
    if !buf.is_empty() {
        out.push(Span::styled(buf, current_style));
    }
}

/// Truncate `text` to at most `width` character cells.  The table renderer's
/// single-line path uses this rather than overflow the trailing border when an
/// inline-formatted cell exceeds its column allocation.  A wide glyph that would
/// straddle the limit is dropped rather than half-drawn.
pub(super) fn truncate_to_width(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let cw = char_cells(ch);
        if used + cw > width {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out
}

/// Display text for a link/image with empty bracket content: the full URL for
/// web-style targets (a scheme or a `#` fragment), else the file name.
pub(super) fn link_fallback(url: &str) -> String {
    if has_url_scheme(url) || url.starts_with('#') {
        return url.to_string();
    }
    Path::new(url)
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| url.to_string())
}

/// Link style by URL kind; anchors and local paths take the dim variants.
/// See docs/dev/theming.md.
pub(super) fn link_style_for(url: &str, theme: &Theme) -> Style {
    if url.starts_with('#') {
        theme.link_heading
    } else if has_url_scheme(url) {
        theme.link_text
    } else {
        theme.link_file
    }
}

fn has_url_scheme(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};

    use super::*;

    fn styled(s: &str, style: Style) -> Vec<StyledChar> {
        s.chars().map(|ch| StyledChar { ch, style }).collect()
    }

    fn text_of(row: &[StyledChar]) -> String {
        row.iter().map(|c| c.ch).collect()
    }

    #[test]
    fn truncate_to_width_drops_a_wide_glyph_straddling_the_limit() {
        assert_eq!(truncate_to_width("日本語", 5), "日本");
        assert_eq!(truncate_to_width("日本語", 1), "");
        assert_eq!(truncate_to_width("ab日", 3), "ab");
    }

    #[test]
    fn wrap_styled_chars_breaks_cjk_between_glyphs_and_keeps_styles() {
        let bold = Style::default().fg(Color::Red);
        let mut chars = styled("ab ", Style::default());
        chars.extend(styled("日本語", bold));
        let rows = wrap_styled_chars(&chars, 5);
        let texts: Vec<String> = rows.iter().map(|r| text_of(r)).collect();
        assert_eq!(texts, vec!["ab 日", "本語"]);
        assert!(rows[1].iter().all(|c| c.style == bold));
        assert!(rows.iter().all(|r| styled_cells(r) <= 5));
    }

    #[test]
    fn wrap_styled_chars_returns_one_empty_row_for_empty_input() {
        assert_eq!(wrap_styled_chars(&[], 5).len(), 1);
    }
}
