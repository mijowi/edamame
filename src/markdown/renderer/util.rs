//! Shared helpers used across the renderer's block / inline pipelines.
//!
//! Free functions only — none of these depend on `Renderer`.  Living
//! together in one file keeps the table and list submodules focused on
//! their own layout logic.

use std::path::Path;

use ratatui::style::Style;
use ratatui::text::Span;

use crate::config::Theme;
use crate::markdown::table_layout::preferred_cut;

/// One character from a styled sequence, tagged with the style its
/// source span carried.  Used by the table renderer's inline-aware
/// wrap pipeline so bold / italic / code-span styling survives a cell
/// breaking across multiple rendered rows.
#[derive(Debug, Clone, Copy)]
pub(super) struct StyledChar {
    pub(super) ch: char,
    pub(super) style: Style,
}

/// Whitespace that wrapping may break at or drop — i.e. everything
/// `char::is_whitespace` matches EXCEPT NBSP (U+00A0).  Table cells use
/// NBSP for the code-span pad cells (the rendered stand-ins for the raw
/// backticks; see `Renderer::cell_styled_chars`), and those must travel
/// with the code token across wrap breaks instead of being trimmed like
/// an inter-word space.
pub(super) fn is_soft_break_space(ch: char) -> bool {
    ch.is_whitespace() && ch != '\u{00A0}'
}

/// Tokenize a styled char sequence into runs of leading-whitespace +
/// non-whitespace, mirroring `split_soft`.  A token always begins with
/// any whitespace that preceded its non-whitespace tail; chained
/// whitespace continues the same token until the next word boundary.
/// NBSP counts as a word char (`is_soft_break_space`), so code-span
/// pads bind to their code token.
fn tokenize_styled(chars: &[StyledChar]) -> Vec<Vec<StyledChar>> {
    let mut tokens: Vec<Vec<StyledChar>> = Vec::new();
    let mut tok: Vec<StyledChar> = Vec::new();
    let mut in_ws = true;
    for c in chars {
        if is_soft_break_space(c.ch) {
            if !in_ws && !tok.is_empty() {
                tokens.push(std::mem::take(&mut tok));
            }
            tok.push(*c);
            in_ws = true;
        } else {
            tok.push(*c);
            in_ws = false;
        }
    }
    if !tok.is_empty() {
        tokens.push(tok);
    }
    tokens
}

/// Wrap a sequence of styled chars into rows of width ≤ `width`,
/// breaking on whitespace where possible.  A token whose width
/// exceeds `width` is hard-split at character boundaries.  Mirrors
/// the algorithm in `table_layout::wrap_cell` but operates on
/// `StyledChar` so per-char styles are preserved across breaks.
///
/// Returns at least one (possibly empty) row.
pub(super) fn wrap_styled_chars(chars: &[StyledChar], width: usize) -> Vec<Vec<StyledChar>> {
    if width == 0 {
        return vec![chars.to_vec()];
    }
    if chars.is_empty() {
        return vec![Vec::new()];
    }

    let tokens = tokenize_styled(chars);

    let mut rows: Vec<Vec<StyledChar>> = Vec::new();
    let mut current: Vec<StyledChar> = Vec::new();
    let mut current_w = 0usize;

    for token in tokens {
        let w = token.len();
        if current.is_empty() {
            if w <= width {
                current.extend(&token);
                current_w = w;
            } else {
                for chunk in hard_split_styled(&token, width) {
                    rows.push(chunk);
                }
                current.clear();
                current_w = 0;
            }
        } else if current_w + w <= width {
            current.extend(&token);
            current_w += w;
        } else {
            rows.push(std::mem::take(&mut current));
            // Drop leading whitespace of the wrapped token before
            // placing it on the new row — matches `wrap_cell`'s
            // `trim_start` behaviour.  NBSP pads survive the trim so a
            // code span starting the new row keeps its leading pad cell.
            let trimmed: Vec<StyledChar> = token
                .iter()
                .skip_while(|c| is_soft_break_space(c.ch))
                .copied()
                .collect();
            let tw = trimmed.len();
            if tw <= width {
                current.extend(&trimmed);
                current_w = tw;
            } else {
                for chunk in hard_split_styled(&trimmed, width) {
                    rows.push(chunk);
                }
                current_w = 0;
            }
        }
    }
    if !current.is_empty() {
        rows.push(current);
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
}

/// Hard-split a token whose char-count exceeds `width` into chunks
/// of size ≤ `width`, preferring to break just after a punctuation
/// character (`table_layout::is_break_after`) in the trailing half of
/// each chunk.  Counterpart of `table_layout::hard_split` for styled
/// sequences.
fn hard_split_styled(token: &[StyledChar], width: usize) -> Vec<Vec<StyledChar>> {
    if width == 0 || token.is_empty() {
        return vec![token.to_vec()];
    }
    let mut rows = Vec::new();
    let mut rest = token;
    while rest.len() > width {
        let cut = preferred_cut(width, |i| rest[i].ch);
        rows.push(rest[..cut].to_vec());
        rest = &rest[cut..];
    }
    rows.push(rest.to_vec());
    rows
}

/// Append a `StyledChar` slice as a sequence of `Span`s, coalescing
/// runs of consecutive chars that share the same style.  Keeps the
/// output line tight without losing any style transitions.
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

/// Truncate `text` to at most `width` character cells.  Used by the table
/// renderer's single-line path when an inline-formatted cell's rendered
/// width exceeds the column allocation: rather than overflowing the
/// trailing border we fall back to plain text and append a `…` to signal
/// the truncation.
pub(super) fn truncate_to_width(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

/// Fallback display text for a link/image whose bracket content is empty:
/// the full URL for web-style targets (anything with a scheme or a `#` fragment),
/// otherwise the final path component of the file path.
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

/// Pick the right link style for `url`.  Heading anchors (`#section`)
/// and local file paths read as more peripheral than full web links
/// per theming.md, so they get the dim variants.
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
