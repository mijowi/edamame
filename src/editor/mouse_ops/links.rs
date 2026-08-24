use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::config::Theme;
use crate::editor::link::LinkTarget;
use crate::editor::EditorState;
use crate::ui::LinkRun;

use super::coord::{click_to_char_offset, rendered_click_to_line_col_on_text};

/// Does `style` belong to a rendered link?
///
/// The discriminator is the *foreground*, not `Modifier::UNDERLINED`.
/// The underline is what a link is decorated with, but it is not
/// exclusive to links: the default theme underlines H2-H6 as well, so
/// the marker test classified every heading as a link — and the run
/// coalescing then swallowed a real link sitting inside one.  Every
/// theme paints link text in the link color (`link_text` / `link_file`
/// for web and file links, the deliberately quieter, underline-free
/// `link_heading` for an in-document anchor), inside a heading and
/// inside a blockquote alike, so the color is the honest signal.
///
/// This is a *candidate* test, not a verdict: an unrelated slot may
/// share the link color (`syntax_function` is derived from it), so
/// callers go through [`link_at_rendered_pos`], which only believes a
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
    // A colorless theme (`Monochrome Dark`) spends UNDERLINED on its link
    // slots and has no foreground to tell them apart with, so the marker
    // is all that is left.  It over-matches there — that theme underlines
    // H2-H6 too — which is exactly why every consumer resolves the run
    // against the block's AST links before believing it (see
    // [`link_at_rendered_pos`]).
    style.add_modifier.contains(Modifier::UNDERLINED)
}

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
    link_at_rendered_pos(state, col as usize, row as usize, viewport_width).map(|(url, _)| url)
}

/// If `(col, row)` lands on a Markdown link, set
/// `state.pending_link_follow` to the classified target and return
/// `true`.  Otherwise return `false` so the caller falls through to
/// normal cursor placement.
///
/// Walks the rendered line's link spans first (the AST-backed
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
    // Footnote reference / definition leader.  The rendered marker
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

/// Resolve the Markdown link under rendered position `(col, row)`, if
/// any, returning its raw URL and optional title.
///
/// The single derivation behind the hand pointer, the hint-line hover
/// tooltip and the click-to-follow path, so all three answer the same
/// question the same way.
///
/// Two things make it exact where the older span-index walk was not:
///
/// * **The position is resolved with [`rendered_click_to_line_col_on_text`]**,
///   which is scroll- *and* wrap-aware.  The previous walk counted one
///   rendered line per screen row, so any wrapped line above the pointer
///   (a long HTML block, a wrapped list item) shifted the lookup down by
///   one line per extra row and the click resolved against a different
///   block entirely.
/// * **Link runs are counted per *block*, not per line.**  A block's
///   N-th link-styled run pairs with the block's N-th
///   `ui::link_view::LinkRun` — the same list `link_view::build_snapshots`
///   pairs its own runs against, though the two find their runs by
///   different tests (see [`link_run_ranges`]).  Counting the run index
///   within the clicked *line* instead made every line of a multi-line
///   block start again at zero, so every item of a link list (the
///   generated manual index, a README's documentation list) resolved to
///   the list's first URL.
///
/// A run that pairs with a `LinkRun::ImagePlaceholder` resolves to
/// `None`: an inline image is painted in the link foreground but is not
/// a link, and the caller falls through to the raw-source scan.
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

    // Every link run on the block's earlier rendered lines comes first in
    // the AST's link order.
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

    // Slice the block out of the rope directly rather than materializing
    // the whole document: this runs on every mouse-move event over a
    // link-styled run, and `Buffer::contents()` there is O(document) per
    // pointer report.  Char-boundary defensive fallback — see
    // `rendered_sub_line_to_offset` for the rationale; the App flushes the
    // parse before mouse dispatch so the `unwrap_or_default` path is for
    // safety, not correctness.
    let block_src = state
        .buffer
        .byte_slice_to_string(
            block_range.start,
            block_range.end.min(state.buffer.len_bytes()),
        )
        .unwrap_or_default();
    let mut runs_in_block: Vec<LinkRun> = Vec::new();
    for block in &crate::markdown::parse(&block_src) {
        crate::ui::link_view::collect_link_runs_from_block_public(block, &mut runs_in_block);
    }
    match runs_in_block.into_iter().nth(preceding + run_in_line)? {
        LinkRun::Link { url, title } => Some((url, title)),
        LinkRun::ImagePlaceholder => None,
    }
}

/// `[(start_col, end_col)]` char-column ranges for every run of
/// consecutive link-styled spans in `line`.  Adjacent runs coalesce so a
/// link whose text carries bold / italic substyling still counts once.
///
/// This is the *foreground*-keyed counterpart of
/// `link_view::underlined_char_ranges`, which keys on
/// `Modifier::UNDERLINED`: both walk the renderer's spans in emission
/// order and both consume one `link_view::LinkRun` per run, but they do
/// not agree run for run, and the difference is deliberate.  An
/// in-document heading anchor (`link_heading`) carries the link colour
/// and no underline, so it is a run here and not one there — which is
/// why this path resolves anchors and the snapshot path does not.  Any
/// change to either predicate has to be argued against both.
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
