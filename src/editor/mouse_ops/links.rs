use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::config::Theme;
use crate::editor::link::LinkTarget;
use crate::editor::EditorState;
use crate::ui::LinkRun;

use super::coord::{click_to_char_offset, rendered_click_to_line_col_on_text};

/// Does `style` belong to a rendered link?
///
/// Keyed on the *foreground*, not `Modifier::UNDERLINED`: the default theme underlines H2-H6
/// too, which classified every heading as a link and let run coalescing swallow a real link
/// inside one.  Every theme paints link text in a link color (`link_heading` is the
/// underline-free anchor slot).  A candidate test, not a verdict — `syntax_function` is derived
/// from the link color — so callers go through [`link_at_rendered_pos`], which only believes a
/// run it can pair with a real `Inline::Link` in the block's AST.
pub(super) fn is_link_style(style: Style, theme: &Theme) -> bool {
    let link_fgs = [
        theme.link_text.fg,
        theme.link_file.fg,
        theme.link_heading.fg,
    ];
    if link_fgs.iter().any(Option::is_some) {
        return link_fgs.iter().any(|fg| fg.is_some() && *fg == style.fg);
    }
    // A colorless theme (`Monochrome Dark`) leaves only the underline; it over-matches on
    // headings, which the AST pairing in `link_at_rendered_pos` filters out.
    style.add_modifier.contains(Modifier::UNDERLINED)
}

/// The raw URL string of the link under `(col, row)`, as written, for the hint line's hover
/// display (`./notes.md`, not the resolved path).  No raw-scan fallback, unlike
/// `follow_link_at_click`: during raw reveal the URL is already on screen.
pub fn hovered_link_url(
    state: &EditorState,
    col: u16,
    row: u16,
    viewport_width: usize,
) -> Option<String> {
    link_at_rendered_pos(state, col as usize, row as usize, viewport_width).map(|(url, _)| url)
}

/// If `(col, row)` lands on a link or footnote, set `state.pending_link_follow` and return
/// `true`.  The AST-backed rendered-line path comes first; the raw-source scan fallback covers
/// the raw-reveal window of the cursor block and Raw mode.
pub(super) fn follow_link_at_click(
    state: &mut EditorState,
    col: u16,
    row: u16,
    viewport_width: usize,
) -> bool {
    if let Some((url, _)) = link_at_rendered_pos(state, col as usize, row as usize, viewport_width)
    {
        let base_dir = state
            .buffer
            .path()
            .and_then(|p| p.parent())
            .map(|p| p.to_owned());
        state.pending_link_follow = Some(LinkTarget::parse(&url, base_dir.as_deref()));
        return true;
    }

    let Some(offset) = click_to_char_offset(state, col as usize, row as usize, viewport_width)
    else {
        return false;
    };
    let source = state.buffer.contents();
    let click_byte = state.buffer.rope().char_to_byte(offset);
    if let Some(url) = link_at_offset(&source, click_byte) {
        let base_dir = state
            .buffer
            .path()
            .and_then(|p| p.parent())
            .map(|p| p.to_owned());
        state.pending_link_follow = Some(LinkTarget::parse(&url, base_dir.as_deref()));
        return true;
    }
    if let Some(target) = super::footnotes::footnote_at_offset(&source, click_byte) {
        state.pending_link_follow = Some(target);
        return true;
    }
    if let Some(target) = super::footnotes::back_link_glyph_at_click(state, col, row) {
        state.pending_link_follow = Some(target);
        return true;
    }
    false
}

/// [`follow_link_at_click`] for footnotes only: the Rendered-mode plain-click path, where a
/// plain click on a link places the cursor (only Ctrl-click opens it).
pub(super) fn follow_footnote_at_click(
    state: &mut EditorState,
    col: u16,
    row: u16,
    viewport_width: usize,
) -> bool {
    // The `↩` glyph has no raw byte, so the offset scan below would map past it.
    if let Some(target) = super::footnotes::back_link_glyph_at_click(state, col, row) {
        state.pending_link_follow = Some(target);
        return true;
    }
    let Some(offset) = click_to_char_offset(state, col as usize, row as usize, viewport_width)
    else {
        return false;
    };
    let source = state.buffer.contents();
    let click_byte = state.buffer.rope().char_to_byte(offset);
    if let Some(target) = super::footnotes::footnote_at_offset(&source, click_byte) {
        state.pending_link_follow = Some(target);
        return true;
    }
    false
}

/// The link under rendered `(col, row)`: raw URL and optional title.  The single derivation
/// behind the hand pointer, the hint-line hover, and click-to-follow.
///
/// Position resolution is scroll- and wrap-aware, and link runs are counted per *block*, not
/// per line: the block's N-th link-styled run pairs with its N-th `ui::link_view::LinkRun`
/// (the same list `link_view::build_snapshots` pairs against, though by a different run test —
/// see [`link_run_ranges`]).  A per-line count made every item of a link list resolve to the
/// first URL.  A run pairing with `LinkRun::ImagePlaceholder` resolves to `None` (painted in
/// the link color, but not a link).
pub(super) fn link_at_rendered_pos(
    state: &EditorState,
    col: usize,
    row: usize,
    viewport_width: usize,
) -> Option<(String, Option<String>)> {
    let (line_idx, char_col) = rendered_click_to_line_col_on_text(state, col, row, viewport_width)?;
    let theme = state.theme();
    let line = state.parsed.lines.get(line_idx)?;
    let runs = link_run_ranges(line, theme);
    let run_in_line = runs
        .iter()
        .position(|(start, end)| char_col >= *start && char_col < *end)?;

    let block_byte = state
        .parsed
        .source_map
        .original_byte_for_rendered_line(line_idx)?;
    let block_range = state
        .parsed
        .source_map
        .original_range_for_byte(block_byte)?;
    let rendered_range = state
        .parsed
        .source_map
        .rendered_lines_for_byte(block_range.start);
    let preceding: usize = (rendered_range.start..line_idx.min(rendered_range.end))
        .filter_map(|idx| state.parsed.lines.get(idx))
        .map(|earlier| link_run_ranges(earlier, theme).len())
        .sum();

    // Slice the block out of the rope rather than `Buffer::contents()`: this runs on every
    // mouse-move over a link run, and materializing the document is O(document) per report.
    // The App flushes the parse before mouse dispatch, so `unwrap_or_default` is defensive.
    let block_src = state
        .buffer
        .byte_slice_to_string(
            block_range.start,
            block_range.end.min(state.buffer.len_bytes()),
        )
        .unwrap_or_default();
    let mut runs_in_block: Vec<LinkRun> = Vec::new();
    for block in &crate::markdown::parse(&block_src) {
        crate::ui::link_view::collect_link_runs_from_block_public(
            block,
            state.parsed.reflow_paragraphs,
            &mut runs_in_block,
        );
    }
    match runs_in_block.into_iter().nth(preceding + run_in_line)? {
        LinkRun::Link { url, title } => Some((url, title)),
        LinkRun::ImagePlaceholder => None,
    }
}

/// Char-column ranges of every run of consecutive link-styled spans in `line`; adjacent spans
/// coalesce so a link with bold/italic substyling counts once.
///
/// The foreground-keyed counterpart of `link_view::underlined_char_ranges`.  Both consume one
/// `link_view::LinkRun` per run but deliberately disagree: a heading anchor (`link_heading`)
/// has the link color and no underline, so it is a run here and not there — which is why this
/// path resolves anchors and the snapshot path does not.  A change to either predicate must be
/// argued against both.
fn link_run_ranges(line: &Line<'_>, theme: &Theme) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut col = 0usize;
    let mut run_start: Option<usize> = None;
    for span in &line.spans {
        let span_len = span.content.chars().count();
        if is_link_style(span.style, theme) {
            if run_start.is_none() {
                run_start = Some(col);
            }
        } else if let Some(start) = run_start.take() {
            out.push((start, col));
        }
        col += span_len;
    }
    if let Some(start) = run_start {
        out.push((start, col));
    }
    out
}

/// Scan the raw source line containing `click_byte` for `[text](url)` and return the URL when
/// the click falls inside it.  No AST: autolinks and reference links are not detected.
pub fn link_at_offset(source: &str, click_byte: usize) -> Option<String> {
    let click_byte = click_byte.min(source.len());
    let line_start = source[..click_byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let rel_after = source[click_byte..]
        .find('\n')
        .map(|i| click_byte + i)
        .unwrap_or(source.len());
    let line = &source[line_start..rel_after];
    let col = click_byte.saturating_sub(line_start);

    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' && !crate::editor::footnote_edit::is_escaped(bytes, i) {
            // Brackets are balanced to support `[text containing [inner]]`.
            let mut depth = 1usize;
            let mut j = i + 1;
            while j < bytes.len() {
                match bytes[j] {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    b'\\' => {
                        j += 1;
                    }
                    _ => {}
                }
                j += 1;
            }
            if depth != 0 || j >= bytes.len() {
                return None;
            }
            let close_bracket = j;
            if close_bracket + 1 >= bytes.len() || bytes[close_bracket + 1] != b'(' {
                i = close_bracket + 1;
                continue;
            }
            let url_start = close_bracket + 2;
            let mut pdepth = 1usize;
            let mut k = url_start;
            while k < bytes.len() {
                match bytes[k] {
                    b'(' => pdepth += 1,
                    b')' => {
                        pdepth -= 1;
                        if pdepth == 0 {
                            break;
                        }
                    }
                    b'\\' => {
                        k += 1;
                    }
                    _ => {}
                }
                k += 1;
            }
            if pdepth != 0 || k >= bytes.len() {
                return None;
            }
            let url_end = k;
            if col >= i && col <= url_end {
                let url_bytes = &bytes[url_start..url_end];
                let url = String::from_utf8_lossy(url_bytes).trim().to_owned();
                return if url.is_empty() { None } else { Some(url) };
            }
            i = url_end + 1;
        } else {
            i += 1;
        }
    }
    None
}
