//! Shared helpers used across the renderer's block / inline pipelines.
//!
//! Free functions only — none of these depend on `Renderer`.  Living
//! together in one file keeps the table and list submodules focused on
//! their own layout logic.

use std::path::Path;

use ratatui::style::Style;
use ratatui::text::Span;

use crate::config::Theme;

/// One character from a styled sequence, tagged with the style its
/// source span carried.  Used by the table renderer's inline-aware
/// wrap pipeline so bold / italic / code-span styling survives a cell
/// breaking across multiple rendered rows.
#[derive(Debug, Clone, Copy)]
pub(super) struct StyledChar {
    pub(super) ch: char,
    pub(super) style: Style,
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

    // Tokenize into runs of whitespace+word, mirroring `split_soft`.
    let mut tokens: Vec<Vec<StyledChar>> = Vec::new();
    let mut tok: Vec<StyledChar> = Vec::new();
    let mut in_ws = true;
    for c in chars {
        if c.ch.is_whitespace() {
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
            // `trim_start` behaviour.
            let trimmed: Vec<StyledChar> = token
                .iter()
                .skip_while(|c| c.ch.is_whitespace())
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
/// of size ≤ `width`.  Counterpart of `table_layout::hard_split`
/// for styled sequences.
fn hard_split_styled(token: &[StyledChar], width: usize) -> Vec<Vec<StyledChar>> {
    if width == 0 || token.is_empty() {
        return vec![token.to_vec()];
    }
    let mut rows = Vec::new();
    for chunk in token.chunks(width) {
        rows.push(chunk.to_vec());
    }
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

/// Number of characters in the longest whitespace-delimited word in `text`.
/// Used by the table renderer to compute a column's `min` — the floor below
/// which `compute_widths` would have to break a word to fit.
pub(super) fn longest_word_chars(text: &str) -> usize {
    text.split_whitespace()
        .map(|w| w.chars().count())
        .max()
        .unwrap_or(0)
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
