//! Integration tests for mermaid diagram support.
//!
//! These exercise the end-to-end plumbing that turns a ```mermaid
//! fenced block into a `Block::ImageBlock` in `ParsedDoc::image_blocks`,
//! complete with the synthetic URL and the `DiagramSource::Mermaid`
//! marker that drives the decode dispatcher.  Tests that would hit the
//! real renderer are `#[ignore]`'d and guarded so CI without system
//! fonts still passes cleanly.

use edamame::config::Theme;
use edamame::diagram::{synthetic_url, DiagramSource};
use edamame::document::{Buffer, ParsedDoc};
use edamame::editor::{EditorState, Mode};
use edamame::export::{render_html, HtmlExportOptions, Stylesheet};
use edamame::markdown::{promote_diagram_code_blocks, Block};

/// Parse a small document via `ParsedDoc::build_with_overrides` without
/// any overrides.  Produces the same structure the live App would see.
fn build_parsed(source: &str) -> ParsedDoc {
    let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
    ParsedDoc::build_with_overrides(
        source, theme, false, 12, None, None, false, 80, false, false, true, None,
    )
}

// ── Parser promotion ──────────────────────────────────────────────────────

#[test]
fn promote_diagram_only_matches_mermaid_tag() {
    // Post-parse pass (library-level) should accept `mermaid` only.
    // Other tags (`mermaidjs`, `diagram`, `rust`) must be left untouched
    // so the code block renders as usual.
    let mut blocks = vec![
        Block::CodeBlock {
            language: Some("mermaid".into()),
            content: "flowchart TD\nA-->B".into(),
            fenced: true,
        },
        Block::CodeBlock {
            language: Some("Mermaid".into()),
            content: "pie\n\"A\": 50\n\"B\": 50".into(),
            fenced: true,
        },
        Block::CodeBlock {
            language: Some("mermaidjs".into()),
            content: "flowchart TD\nA-->B".into(),
            fenced: true,
        },
        Block::CodeBlock {
            language: Some("diagram".into()),
            content: "flowchart TD\nA-->B".into(),
            fenced: true,
        },
        Block::CodeBlock {
            language: Some("rust".into()),
            content: "fn main() {}".into(),
            fenced: true,
        },
    ];
    let sources = promote_diagram_code_blocks(&mut blocks);
    assert!(matches!(blocks[0], Block::ImageBlock { .. }));
    assert!(matches!(blocks[1], Block::ImageBlock { .. }));
    assert!(matches!(blocks[2], Block::CodeBlock { .. }));
    assert!(matches!(blocks[3], Block::CodeBlock { .. }));
    assert!(matches!(blocks[4], Block::CodeBlock { .. }));
    assert_eq!(sources.len(), 2);
}

#[test]
fn promote_diagram_preserves_alt_text() {
    let mut blocks = vec![Block::CodeBlock {
        language: Some("mermaid".into()),
        content: "flowchart TD\nA-->B".into(),
        fenced: true,
    }];
    promote_diagram_code_blocks(&mut blocks);
    if let Block::ImageBlock { alt, .. } = &blocks[0] {
        assert_eq!(alt, "mermaid diagram");
    } else {
        panic!("expected ImageBlock after promotion");
    }
}

#[test]
fn promote_diagram_url_matches_synthetic_url() {
    let src = "flowchart TD\nA-->B";
    let mut blocks = vec![Block::CodeBlock {
        language: Some("mermaid".into()),
        content: src.into(),
        fenced: true,
    }];
    let sources = promote_diagram_code_blocks(&mut blocks);
    let Block::ImageBlock { url, .. } = &blocks[0] else {
        panic!("expected ImageBlock");
    };
    assert_eq!(url, &synthetic_url(&DiagramSource::Mermaid(src.into())));
    assert!(sources.contains_key(url));
}

// ── ParsedDoc integration ─────────────────────────────────────────────────

#[test]
fn parsed_doc_populates_image_block_for_mermaid() {
    let doc = "# Title\n\n```mermaid\nflowchart TD\nA-->B\n```\n\nAfter.\n";
    let parsed = build_parsed(doc);
    assert_eq!(parsed.image_blocks.len(), 1);
    let info = &parsed.image_blocks[0];
    assert_eq!(info.alt, "mermaid diagram");
    assert!(info.url.starts_with("diagram-mermaid-"));
    match &info.source {
        Some(DiagramSource::Mermaid(src)) => {
            assert!(src.contains("flowchart TD"));
            assert!(src.contains("A-->B"));
        }
        _ => panic!("expected DiagramSource::Mermaid"),
    }
}

#[test]
fn parsed_doc_leaves_regular_code_blocks_alone() {
    // A non-mermaid fenced code block stays a CodeBlock — no image
    // block reservation, no decode request.
    let doc = "```rust\nfn main() {}\n```\n";
    let parsed = build_parsed(doc);
    assert!(parsed.image_blocks.is_empty());
}

#[test]
fn parsed_doc_keeps_real_image_blocks_without_a_diagram_source() {
    // Regular `![alt](path)` image blocks get `source == None` so the
    // App dispatcher routes them through `image::resolve`, not the
    // mermaid path.
    let doc = "![pic](cat.png)\n";
    let parsed = build_parsed(doc);
    assert_eq!(parsed.image_blocks.len(), 1);
    assert!(parsed.image_blocks[0].source.is_none());
    assert_eq!(parsed.image_blocks[0].url, "cat.png");
}

#[test]
fn synthetic_url_stays_stable_across_reparses_of_identical_doc() {
    // Editing elsewhere in the document must not invalidate the
    // diagram cache — the synthetic URL is content-addressed by the
    // mermaid source only.
    let diagram = "```mermaid\nflowchart TD\nA-->B\n```";
    let a = build_parsed(&format!("# Title\n\n{}\n\nTrailer.\n", diagram));
    let b = build_parsed(&format!("# Title\n\n{}\n\nDifferent trailer.\n", diagram));
    assert_eq!(a.image_blocks[0].url, b.image_blocks[0].url);
}

#[test]
fn synthetic_url_changes_when_diagram_source_changes() {
    // Editing the diagram itself must mint a new URL so the cache
    // evicts the stale render.
    let a = build_parsed("```mermaid\nflowchart TD\nA-->B\n```\n");
    let b = build_parsed("```mermaid\nflowchart TD\nA-->C\n```\n");
    assert_ne!(a.image_blocks[0].url, b.image_blocks[0].url);
}

// ── Raw-source reveal reflow ──────────────────────────────────────────────

/// Document with a diagram whose source is longer than the rows a
/// (still-undecoded) image reserves, plus a trailer block so the reflow
/// below it is observable.
const REVEAL_DOC: &str = "\
# Title

```mermaid
flowchart LR
    A --> B
    B --> C
    C --> D
```

After.
";

/// Byte offset of the diagram's first content line (`flowchart LR`) — a
/// cursor position squarely inside the mermaid block.
fn diagram_cursor_offset() -> usize {
    REVEAL_DOC.find("flowchart LR").expect("diagram in fixture")
}

fn reveal_state(image_rows: usize) -> EditorState {
    let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
    let mut state =
        EditorState::new_with_config(Buffer::from_str(REVEAL_DOC), theme, true, true, image_rows);
    state.mode = Mode::Rendered;
    state
}

/// Index of the block holding `byte`, and how many rendered rows it owns.
fn block_rows_at(state: &EditorState, byte: usize) -> usize {
    let idx = state
        .parsed
        .source_map
        .block_for_byte(byte)
        .expect("byte inside a block");
    state.parsed.block_own_line_count(idx)
}

#[test]
fn revealed_diagram_reserves_one_row_per_source_line() {
    // The mermaid fence is 6 raw lines (```mermaid, four body lines,
    // ```) against an image reservation of 12, so before the reveal the
    // block is padded out and after it the raw source fits exactly.
    let mut state = reveal_state(12);
    let cursor = diagram_cursor_offset();
    assert_eq!(block_rows_at(&state, cursor), 12);

    state.cursor.offset = state.buffer.rope().byte_to_char(cursor);
    assert!(state.sync_image_reveal(), "entering the block re-lays out");
    assert_eq!(block_rows_at(&state, cursor), 6);
}

#[test]
fn revealed_diagram_grows_past_a_short_image() {
    // The reported bug: a wide, short diagram reserves fewer rows than
    // its source has lines, clipping the reveal.  The block must grow.
    let mut state = reveal_state(2);
    let cursor = diagram_cursor_offset();
    assert_eq!(block_rows_at(&state, cursor), 2);

    state.cursor.offset = state.buffer.rope().byte_to_char(cursor);
    state.sync_image_reveal();
    assert_eq!(block_rows_at(&state, cursor), 6);
}

#[test]
fn leaving_a_diagram_restores_the_image_reservation() {
    // The expansion is scoped to the reveal: moving the cursor out puts
    // the image's rows back and reflows the document again.
    let mut state = reveal_state(12);
    let cursor = diagram_cursor_offset();
    state.cursor.offset = state.buffer.rope().byte_to_char(cursor);
    state.sync_image_reveal();
    assert_eq!(block_rows_at(&state, cursor), 6);

    let trailer = REVEAL_DOC.find("After.").expect("trailer in fixture");
    state.cursor.offset = state.buffer.rope().byte_to_char(trailer);
    assert!(state.sync_image_reveal(), "leaving the block re-lays out");
    assert_eq!(block_rows_at(&state, cursor), 12);
}

#[test]
fn image_reveal_sync_is_idempotent() {
    // Called once per frame, so a no-op frame must neither re-parse nor
    // change the layout.
    let mut state = reveal_state(12);
    state.cursor.offset = state.buffer.rope().byte_to_char(diagram_cursor_offset());
    assert!(state.sync_image_reveal());
    assert!(!state.sync_image_reveal());
    assert!(!state.sync_image_reveal());
}

#[test]
fn preview_mode_never_expands_a_diagram() {
    // Preview is browse-only: no reveal, so no reflow.
    let mut state = reveal_state(12);
    state.mode = Mode::Preview;
    let cursor = diagram_cursor_offset();
    state.cursor.offset = state.buffer.rope().byte_to_char(cursor);
    assert!(!state.sync_image_reveal());
    assert_eq!(block_rows_at(&state, cursor), 12);
}

/// Regression: the reveal re-lays the document out under a cursor that
/// never moved, so nothing on the ordinary edit/motion paths would scroll
/// it back — entering a fence from *below* near the bottom of the viewport
/// grows the block downward and takes the cursor off screen with it.
/// `App::prepare_viewport` owes the `ensure_cursor_visible` call after a
/// sync that returned `true`; this pins that it resolves the case.
#[test]
fn revealing_a_diagram_at_the_fold_keeps_the_cursor_on_screen() {
    let theme: &'static Theme = Box::leak(Box::new(Theme::default()));
    let mut doc = String::new();
    for _ in 0..10 {
        doc.push_str("filler paragraph\n\n");
    }
    doc.push_str(
        "```mermaid\nflowchart LR\n    A --> B\n    B --> C\n    C --> D\n    D --> E\n    E --> F\n```\n\nAfter.\n",
    );
    let (vh, vw) = (10usize, 40usize);
    // Two reserved image rows against an eight-line fence: the reveal
    // grows the block by six rows.
    let mut state = EditorState::new_with_config(Buffer::from_str(&doc), theme, true, true, 2);
    state.mode = Mode::Rendered;
    state.set_viewport_width(vw);

    // Park the cursor on the fence's last source line — where a cursor
    // arriving from the line below lands — and scroll it into view as the
    // motion that put it there would have.
    let byte = doc.find("    E --> F").expect("fixture");
    state.cursor.offset = state.buffer.rope().byte_to_char(byte);
    state.update_cursor_block();
    state.ensure_cursor_visible(vh, vw);
    let block_idx = state
        .parsed
        .source_map
        .block_for_byte(byte)
        .expect("byte inside a block");

    // `update_cursor_block` armed the reveal timer; the reflow is what
    // happens once it elapses, so skip the delay rather than sleep it.
    state.cursor_block_entered_at = None;
    assert!(state.sync_image_reveal(), "entering the block re-lays out");
    state.ensure_cursor_visible(vh, vw);

    // The revealed block reserves one row per source line, so the cursor's
    // row is the block's first row plus its line index within the fence
    // (`    E --> F` is the seventh line).
    let rows = state.parsed.source_map.rendered_lines_for_block(block_idx);
    assert_eq!(rows.len(), 8, "the whole fence reserves a row per line");
    let cursor_row = rows.start + 6;
    assert!(
        (state.scroll..state.scroll + vh).contains(&cursor_row),
        "the cursor's row ({cursor_row}) must stay on screen after the \
         reflow (scroll {}, height {vh}, fence {rows:?})",
        state.scroll
    );
}

// ── HTML export ───────────────────────────────────────────────────────────

fn opts(render_diagrams: bool) -> HtmlExportOptions {
    HtmlExportOptions {
        stylesheet: Stylesheet::Inline(String::new()),
        inline_images: false,
        source_dir: None,
        title: None,
        render_diagrams,
    }
}

#[test]
fn html_export_falls_back_when_render_diagrams_is_false() {
    // With `render_diagrams = false`, mermaid blocks must emit the
    // standard `<pre><code class="language-mermaid">` so downstream
    // toolchains (e.g. Docusaurus + mermaid.js plugin) can render
    // them client-side.  The source must be preserved verbatim.
    let md = "```mermaid\nflowchart TD\nA-->B\n```\n";
    let html = render_html(md, &opts(false)).expect("render");
    assert!(
        html.contains("<code class=\"language-mermaid\">"),
        "expected fallback code-block, got:\n{html}"
    );
    assert!(html.contains("flowchart TD"));
    assert!(!html.contains("<figure class=\"mermaid-diagram\">"));
}

#[test]
fn html_export_ignores_non_mermaid_code_blocks() {
    // A non-mermaid fenced block renders the same with or without
    // diagrams enabled.
    let md = "```rust\nfn main() {}\n```\n";
    let html_on = render_html(md, &opts(true)).expect("render");
    let html_off = render_html(md, &opts(false)).expect("render");
    assert_eq!(html_on, html_off);
    assert!(html_on.contains("<code class=\"language-rust\">"));
    assert!(html_on.contains("fn main"));
}

// The live-render tests below hit the real `mermaid-rs-renderer` crate,
// which (a) requires system fonts on the CI host, (b) has known panic
// bugs in v0.2.1.  Run locally with
// `cargo test --test diagrams -- --ignored mermaid_live`.
#[test]
#[ignore = "requires system fonts; exercises live mermaid-rs-renderer"]
fn mermaid_live_html_export_emits_inline_svg() {
    let md = "```mermaid\nflowchart TD\nA-->B\n```\n";
    let html = render_html(md, &opts(true)).expect("render");
    assert!(
        html.contains("<figure class=\"mermaid-diagram\">"),
        "expected figure wrapper, got:\n{html}"
    );
    assert!(html.contains("<svg"));
    assert!(
        !html.contains("<code class=\"language-mermaid\">"),
        "diagram should not also emit the fallback code block"
    );
}

#[test]
#[ignore = "requires system fonts; exercises live mermaid-rs-renderer"]
fn mermaid_live_html_export_falls_back_on_unparseable_input() {
    // Malformed mermaid source should gracefully fall back to the code
    // block form — the HTML export must never fail a whole document
    // over one bad diagram.
    let md = "```mermaid\n~~~not~~a~~~valid~~mermaid~~source~~~\n```\n";
    let html = render_html(md, &opts(true)).expect("render");
    // Either the fallback is used, or the renderer's best-effort
    // output is embedded; either way the document export succeeds.
    assert!(
        html.contains("<code class=\"language-mermaid\">")
            || html.contains("<figure class=\"mermaid-diagram\">"),
        "export should have emitted either fallback or figure:\n{html}"
    );
}
