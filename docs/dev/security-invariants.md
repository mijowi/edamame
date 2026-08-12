# Security invariants for contributors

The user-facing description of edamame's threat model and the hardening that
is already in place lives in [`../security.md`](../security.md). **Read it
first** — this page is only the checklist of things a change must not
regress, and it won't make sense without the reasoning behind it.

When touching any content-handling path — image/SVG decode, remote fetch,
Mermaid, link opening, HTML export, subprocess spawning — keep these from
regressing:

- **Decode through `ImageReader` + `Limits`.** A new raster decode site
  must keep the limits. Never call `image::load_from_memory` on bytes that
  came from outside the process — a file, a socket, a document. (The one
  in-tree call, at the end of `image::svg::rasterize_svg`, decodes the PNG
  it encoded two lines earlier from an already-bounded pixmap; that is the
  only shape of exemption there is.)
- **Never spawn a shell with document data.** Use argv vectors. If you add
  a new subprocess, audit every argument's provenance.
- **The HTML exporter's three filters are load-bearing.** If you add a new
  way for content to reach the serialized body (a new `Event::Html` push, a
  new attribute, a new embedded resource), it bypasses the raw-HTML strip —
  re-derive its safety explicitly and add a regression test. The existing
  tests in `src/export/html.rs` (`neutralizes_javascript_link_scheme`,
  `mermaid_export_never_emits_raw_svg_or_script`,
  `inline_images_rejects_absolute_path`, …) are the template.
- **Keep network access consent-gated *and* IP-filtered.** A new fetch
  path must respect the remote-image policy (or introduce its own explicit
  gate) and route through `PublicOnlyResolver` (or an equivalent
  resolved-IP filter); don't add a silent or unfiltered `http` request.
- **Keep the SVG pixmap budget between the caller and the allocation.**
  `src/image/svg.rs` clamps to the cell envelope only when the caller
  supplies one (HTML export deliberately doesn't), so `MAX_RASTER_DIM` /
  `MAX_RASTER_PIXELS` are what bound every other path. They are applied to
  the *rounded* dimensions and folded back into the render transform — a
  refactor that clamps the pixmap without the transform crops the drawing,
  and one that clamps before `ceil` lets the budget be exceeded by a pixel
  per axis.
- **Bound new external input.** Size, timeout, and allocation caps before
  the input is trusted — mirror the image-loader, Mermaid-source, and
  update-check patterns.

When you fix or add one of the "Things to watch out for" items in
[`../security.md`](../security.md), move it from that section into
"Hardening in place" and add the regression test alongside.
