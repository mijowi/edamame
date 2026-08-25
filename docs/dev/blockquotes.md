# Blockquotes

> Part of the edamame contributor deep-dives. Index and project-wide conventions: [`AGENTS.md`](../../AGENTS.md). Sibling docs live in [`docs/dev/`](.).


- **The quote's style is a *base*, never a replacement.** `render_blockquote` renders inner blocks first, then prefixes each line with the `▎ ` bar, so every span arriving there has resolved its own style; overwriting them with `blockquote_text` silenced bold, italic, code spans, highlights, strikethrough and link color inside any quote (issue #33). Each span is now `base.patch(span.style)`, with the inner block's own line style (a nested code block's surface) layered into `base` first so it still wins. A new decoration wrapped around already-rendered lines owes the same shape.
- **`blockquote_text` is a background wash, not a text attribute** — a blanket ITALIC leaves `*emphasis*` inside a quote with nothing to say. It is `secondary` mixed almost all the way to `bg` (`QUOTE_BG_MIX_TOWARD_BG`), the same `blend` that is a no-op for non-RGB palettes, so `dark_256` / `light_256` pin it by hand and `Monochrome Dark` uses DIM.
- **The wash is carried by the *line* style, not by padding.** `line_render` fills a row's trailing cells with `Line::style`, extending the wash to the viewport edge and filling a wrapped continuation's indent zone (the bar is repainted there by `leading_bar_prefix`). Don't pad quote rows out to `viewport_width` the way `render_code_block` does — it would break the 1:1 raw↔rendered column relation.
- **The revealed row keeps the wash.** `RenderedView` resolves a `reveal_base` from the cursor block's AST kind and hands it to `paint::make_raw_line_over`; without it the row being edited drops out of the block it visibly belongs to (same reason a revealed mermaid body row gets `make_code_styled_body_line`). A new block kind painting its own surface owes an arm there.

