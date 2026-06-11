use crate::editor::link::LinkTarget;
use crate::editor::EditorState;
use crate::ui::line_render;

use super::coord::{click_to_char_offset, rendered_line_at_row, span_at_col_has_modifier};

/// If `(col, row)` falls on a Markdown link, return its raw URL string
/// exactly as written in the source.  Used by the App to stash the
/// currently hovered link on `App::hovered_link` so the hint line can
/// surface it while the pointer rests on the link.  The raw string is
/// kept (rather than a classified [`LinkTarget`]) because the hint line
/// shows what the author wrote — `./notes.md`, not the base-dir-resolved
/// absolute path.
///
/// Deliberately has no raw-scan fallback (unlike `follow_link_at_click`):
/// during the raw-reveal window the cursor block shows the literal
/// `[text](url)` source, so the URL is already on screen and a hint-line
/// echo would be redundant.
pub fn hovered_link_url(
    state: &EditorState,
    col: u16,
    row: u16,
    viewport_width: usize,
) -> Option<String> {
    let (line, _) = rendered_line_at_row(state, row as usize)?;
    if !span_at_col_has_modifier(&line, col as usize, ratatui::style::Modifier::UNDERLINED) {
        return None;
    }
    link_url_for_click(state, col as usize, row as usize, viewport_width)
}

/// If `(col, row)` lands on a Markdown link, set
/// `state.pending_link_follow` to the classified target and return
/// `true`.  Otherwise return `false` so the caller falls through to
/// normal cursor placement.
///
/// Walks the rendered line's UNDERLINED spans first (the AST-backed
/// path, matching what `link_view::build_snapshots` exposes), falling
/// back to a raw-source scan via `link_at_offset` so the raw-reveal
/// window of a cursor block still detects `[text](url)` clicks.
pub(super) fn follow_link_at_click(
    state: &mut EditorState,
    col: u16,
    row: u16,
    viewport_width: usize,
) -> bool {
    // Try AST-backed path via underlined-span hit-test on the rendered line
    // directly.  Works for Preview and Rendered when the line isn't being
    // revealed as raw.  We intentionally do NOT consult an external
    // snapshot slice here — `hit_test_clickable` already shows the span
    // marker is sufficient, and this keeps `mouse_ops::apply`'s signature
    // small.
    if let Some((line, _)) = rendered_line_at_row(state, row as usize) {
        if span_at_col_has_modifier(&line, col as usize, ratatui::style::Modifier::UNDERLINED) {
            // The rendered line has a link span at this col — resolve the
            // URL by matching the N-th link in the block's AST with the
            // N-th underlined span on this line.
            if let Some(url) = link_url_for_click(state, col as usize, row as usize, viewport_width)
            {
                let base_dir = state
                    .buffer
                    .path()
                    .and_then(|p| p.parent())
                    .map(|p| p.to_owned());
                state.pending_link_follow = Some(LinkTarget::parse(&url, base_dir.as_deref()));
                return true;
            }
        }
    }

    // Raw fallback — click on the revealed raw `[text](url)` syntax of the
    // cursor block, or a raw-mode click, also triggers link-follow.
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
    // Footnote reference / definition leader.  The rendered superscript
    // and the `  N.  ` leader both map (via the 1:1 raw-column coordinate)
    // onto the `[^label]` / `[^label]:` source bytes, so the raw scan
    // resolves both without rendered-span bookkeeping.
    if let Some(target) = super::footnotes::footnote_at_offset(&source, click_byte) {
        state.pending_link_follow = Some(target);
        return true;
    }
    // The definition's trailing `↩` glyph is appended chrome with no raw
    // byte, so it needs the rendered-line hit-test rather than the scan.
    if let Some(target) = super::footnotes::back_link_glyph_at_click(state, col, row) {
        state.pending_link_follow = Some(target);
        return true;
    }
    false
}

/// Like [`follow_link_at_click`] but for footnotes only.  Used by the
/// Rendered-mode plain-click path so a click on a footnote marker or a
/// definition back-link follows it without requiring Ctrl (matching
/// Preview), while plain clicks elsewhere still place the cursor.  Links
/// are deliberately NOT handled here — a plain click on a link in an
/// editing mode places the cursor; only Ctrl-click opens it.
pub(super) fn follow_footnote_at_click(
    state: &mut EditorState,
    col: u16,
    row: u16,
    viewport_width: usize,
) -> bool {
    // Trailing `↩` glyph first — it has no raw byte, so the offset-based
    // scan below would miss it (mapping past the body text).
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

/// Best-effort: determine which URL was clicked by matching the
/// underlined-span index at `(col, row)` against the N-th
/// `Inline::Link` in the clicked rendered line's block.
///
/// Returns `None` when the click doesn't land on an underlined span or
/// we can't associate it with an AST link (which falls back to the raw
/// scan).
fn link_url_for_click(
    state: &EditorState,
    col: usize,
    row: usize,
    _viewport_width: usize,
) -> Option<String> {
    let (line, _sub_row) = rendered_line_at_row(state, row)?;
    // Index of the underlined run at `col` within this line.
    let mut walk = 0usize;
    let mut run_index: Option<usize> = None;
    let mut link_count = 0usize;
    let mut in_run = false;
    for span in &line.spans {
        let span_len = span.content.chars().count();
        let under = span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::UNDERLINED);
        if under {
            if !in_run {
                // Entering a new underlined run — record its index.
                if col >= walk && col < walk + span_len {
                    run_index = Some(link_count);
                }
                link_count += 1;
                in_run = true;
            } else if col >= walk && col < walk + span_len {
                // Still in the same run — run_index already set.
                run_index.get_or_insert(link_count - 1);
            }
        } else {
            in_run = false;
        }
        walk += span_len;
    }
    let target_idx = run_index?;

    // Walk the block's AST to find the `target_idx`-th link.
    let cursor_byte = state
        .parsed
        .source_map
        .original_byte_for_rendered_line(index_for_row(state, row)?)?;
    let block_range = state
        .parsed
        .source_map
        .original_range_for_byte(cursor_byte)?;
    let source = state.buffer.contents();
    // Char-boundary defensive fallback — see `rendered_sub_line_to_offset`
    // for the rationale; the App flushes the parse before mouse dispatch
    // so the unwrap_or path is for safety, not correctness.
    let block_src = source
        .get(block_range.start..block_range.end.min(source.len()))
        .unwrap_or("");
    let blocks = crate::markdown::parse(block_src);
    let mut urls: Vec<(String, Option<String>)> = Vec::new();
    for block in &blocks {
        crate::ui::link_view::collect_links_from_block_public(block, &mut urls);
    }
    urls.into_iter().nth(target_idx).map(|(u, _)| u)
}

/// Resolve the rendered-line index that corresponds to document-area
/// row `row`, accounting for scroll.  Mirrors the inner loop of
/// `rendered_line_at_row` but returns the index rather than the line.
fn index_for_row(state: &EditorState, row: usize) -> Option<usize> {
    let lines = &state.parsed.lines;
    let mut y = 0usize;
    for (idx, line) in lines.iter().enumerate().skip(state.scroll) {
        let rows_used = line_render::visual_rows_for_line(line, usize::MAX).max(1);
        if row < y + rows_used {
            return Some(idx);
        }
        y += rows_used;
    }
    None
}

/// Scan the source line containing `click_byte` for Markdown link syntax
/// `[text](url)` and return the URL when the click falls inside such a span.
///
/// Kept deliberately simple: operates on the raw line (no AST re-parse), so
/// autolinks (`<url>`), reference links (`[text][id]`), and nested link
/// constructs are not detected.  A later change may upgrade this to a proper
/// per-block hit-test registry once link opening is implemented.
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
        // A backslash-escaped `\[` is literal text, not a link opener.
        if bytes[i] == b'[' && !crate::editor::footnote_edit::is_escaped(bytes, i) {
            // Find matching `]`.  Brackets are balanced to support nested
            // `[text containing [inner]]` constructs.
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
