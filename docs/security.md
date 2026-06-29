# Security

This document describes edamame's security posture: the threat model it is
built against, the hardening that is already in place, and the areas a
contributor should be careful around when adding features. It is both
user-facing (what protects you when you open an untrusted document) and
developer-facing (what invariants to preserve).

## Threat model

The primary adversary is **the author of a Markdown document you open**.
edamame is a viewer/editor, so the realistic attack is: someone sends you a
`.md` file (or you open one from an AI agent, a repo, a download), and the
file — together with any local or remote resources it references — tries to
do something you didn't intend.

Concretely, document content reaches several non-trivial subsystems:

- **Image decoding** — embedded/referenced PNG/JPEG/GIF/BMP/WEBP and SVG.
- **Remote fetches** — `http(s)` image URLs.
- **Mermaid diagrams** — fenced ` ```mermaid ` blocks rendered to images.
- **Link opening** — clicking a link hands a URL/path to the OS.
- **HTML export** — the document is serialized to a shareable HTML file.
- **Subprocess spawning** — `$EDITOR`, custom export commands.

What is **out of scope**: a malicious *local config* (`config.toml`,
`keybindings.toml`, custom export commands, `$EDITOR`) is trusted — if an
attacker can write your config they already own your account. The terminal
emulator itself is also trusted.

Two principles run through the design:

1. **Document content never reaches a shell.** Subprocesses are always
   spawned with an explicit argv vector (`open::that`, `Command::new(...)
   .arg(...)`), never via a shell string. No attacker-controlled bytes are
   interpolated into a command line.
2. **Cost and capability are bounded before content is trusted.** Decodes
   are size-limited, network access is consent-gated, and the export path
   strips or flattens anything executable.

## Hardening in place

### Image decoding is bounded (decompression bombs)

A highly compressed raster expands enormously on decode — PNG zlib ratios
routinely exceed 1000:1 — so a small file can ask the decoder to allocate
gigabytes. The decode worker runs under `catch_unwind`, which contains a
*panic* but **not** an OOM; a large enough allocation aborts the whole
process.

Mitigations (`src/image/loader.rs`):

- Raster decoding goes through `image::ImageReader` with `image::Limits` in
  force: `max_image_width`/`max_image_height` capped at 50 000 px and
  `max_alloc` at 256 MB. A decode bomb returns a `Decode` error instead of
  exhausting memory. **Do not** revert to `image::load_from_memory`, which
  allocates the full pixel buffer with no ceiling — and note the cap has to
  be enforced *at decode time*, because the loader's `pre_resize`
  downscale runs only *after* the full buffer already exists.
- Local image files are size-capped (64 MB) via a `metadata` check before
  `std::fs::read`. Remote bodies are already capped at 10 MB by ureq.
- SVG rasterization (`src/image/svg.rs`) clamps the output pixmap to the
  cell envelope, so a giant declared SVG size cannot force an oversized
  allocation.

### Remote access is consent-gated and IP-filtered

Remote image fetching is off unless the user allows it. The
`images.enabled` / remote-image policy defaults to **Ask**: edamame prompts
on document load and only fetches `http(s)` URLs after the user opts in
(`src/image/loader.rs`, `RemoteImagePolicy`). Fetches are bounded by a 10 s
timeout across connect / receive-response / receive-body and a 10 MB body
cap. ureq uses rustls (no system OpenSSL).

This is equivalent to an email client's "load remote images" gate: it
prevents a zero-interaction tracking pixel.

Even after consent, fetches are filtered against internal address ranges to
prevent SSRF. `PublicOnlyResolver` (`src/image/loader.rs`) wraps ureq's
default resolver and drops any *resolved* address that is loopback /
RFC1918 private / link-local (including the `169.254.169.254` cloud-metadata
endpoint) / carrier-grade NAT / unique-local or link-local IPv6 /
unspecified / broadcast / documentation; a host that resolves only to
internal addresses is reported `HostNotFound`. Filtering the resolved IP
(not the hostname) defeats literal-IP URLs, DNS names pointed at private
space (DNS rebinding), and `3xx` redirects uniformly — ureq re-resolves
every hop through the same resolver. IPv4-mapped IPv6 addresses are
unwrapped and re-checked so `::ffff:127.0.0.1` cannot slip past.

### HTML export strips and flattens executable content

The exported HTML is the one artifact a user typically **shares**, so the
exporter (`src/export/html.rs`) is the most safety-critical path. Three
layers protect it:

- **Raw HTML is stripped.** Block-level (`Event::Html`) and inline
  (`Event::InlineHtml`) events are filtered out before serialization, so a
  `<script>` tag (or any raw markup) in the source never reaches the
  output.
- **Link schemes are allowlisted.** pulldown-cmark's HTML writer performs
  no URL sanitization, so a `[x](javascript:…)` or `[x](data:text/html,…)`
  link would otherwise survive verbatim into the `<a href>` and run on
  click in a browser. `sanitize_link_urls` rewrites any link whose scheme
  is not in `SAFE_LINK_SCHEMES` (`http`, `https`, `mailto`, `tel`) to a
  harmless `#`. Relative paths, anchors, and `?query` targets carry no
  scheme and are preserved; scheme detection follows RFC 3986 (a colon
  after a `/`, `?`, or `#` is part of the path, not a scheme).
- **Mermaid diagrams are rasterized, not inlined as SVG.** Inline SVG can
  carry `<script>`, `foreignObject`, and `on*=` handlers that execute when
  the file is opened in a browser. `render_mermaid_png_data_uri` renders
  the diagram, rasterizes it to a PNG, and embeds it as a `data:image/png`
  `<img>` — flattening any executable payload to pixels. On render failure
  it falls back to an HTML-escaped code block, so the source is never lost.

### HTML export confines local-file reads

A self-contained export base64-embeds referenced images into the output.
Because that output is shared, an embedded file *leaves the trust
boundary* — unlike on-screen rendering, where the bytes never leave the
victim's machine. A hostile `![x](/home/victim/private/diagram.svg)` could
otherwise exfiltrate arbitrary on-disk files (text-based `.svg` especially)
into an artifact the victim sends back.

`resolve_relative` (`src/export/html.rs`) now confines inlining to the
source tree: absolute paths and `../` traversal are rejected up front, and
the resolved path is `canonicalize()`d and required to stay under
`source_dir` (which also defeats symlink escapes). An out-of-tree reference
is simply left non-inlined rather than embedded. Inlining is additionally
off by default.

### Subprocesses take no document-controlled input

- **External editor / custom export** (`src/app/external_editor.rs`,
  `src/export/custom.rs`): commands come from `$EDITOR`/`$VISUAL` and user
  config — both trusted — and are exec'd as an argv vector with no shell.
  No document data reaches argv.
- **Link/file opening** (`src/app/nav.rs`, `src/editor/link.rs`): the URL
  or path is passed to `open::that`, which spawns `xdg-open`/`open`/`start`
  with the target as a single argument — again, no shell interpolation.

### Mermaid rendering cannot execute code or touch the network

`mermaid-rs-renderer` is pure Rust: its dependency set contains no JS
engine, no headless browser, and no HTTP client. Mermaid source cannot
execute code, read files, or make network requests through the renderer.
It is pinned exactly (`=0.2.2`) because it is pre-1.0 with known panic
bugs; those panics are contained by `catch_unwind` at two layers
(`src/diagram/mermaid.rs`).

The renderer has no internal length, node-count, or timeout bound, so a
pathological diagram could otherwise drive unbounded CPU/RAM on the decode
worker. `render_mermaid_svg` rejects any source over 64 KiB before
dispatch (`MAX_MERMAID_SOURCE_BYTES`) — a single choke point covering both
the TUI raster path and the HTML exporter — and an over-cap block falls
back to the plain code block.

### SVG parsing reads no external entities, network, or local files

`usvg`/`roxmltree` (`src/image/svg.rs`) does not expand external DTD
entities (no XXE), performs no network requests, and ignores `http(s)`
`<image href>` (no SSRF). usvg's default string resolver *would* read
local files referenced by `<image href="/abs/path">`, so the rasterizer
installs a custom `image_href_resolver.resolve_string` that refuses every
path/URL href (embedded `data:` images still resolve). This removes
local-file reads from untrusted SVGs entirely — render-only on the TUI
path, but the change also closes the exfiltration channel that arises when
such an SVG is inlined into an HTML export.

### Other bounded surfaces

- **Update check** (`src/app/update_check.rs`): a hardcoded
  `https://api.github.com` URL (not influenceable by file/config), 10 MB
  body cap, 10 s timeout, detached worker; the response is parsed only to
  extract a `tag_name` string. Nothing is downloaded or executed.
- **Image cache** (`src/image/cache.rs`): keyed by literal URL (raster) or
  content `sha256` (diagrams); per-`EditorState`, GC'd to the live URL set
  each reparse (bounded growth), failed decodes memoized (no retry storm).
- **File watcher** (`src/watcher/file_watcher.rs`): reads only the user's
  own opened file; never writes; no exploitable check-then-act gap.
- **Atomic export writes** (`src/export/runner.rs`): `write_atomically`
  creates its temp file with a random, `O_EXCL` name via `NamedTempFile`
  in the target directory, then `persist`s (same-dir rename) over the
  output. No predictable sibling name a local attacker could pre-plant as
  a symlink to redirect the write.

## Things to watch out for (future hardening)

These are not zero-interaction vulnerabilities today — each is gated by
consent, a deliberate click, or trusted config — but each would raise the
floor, and a careless change nearby could turn one into a real hole.

### No confirmation before opening risky link targets

Following a link auto-opens with no prompt. Any URI scheme is handed to the
OS handler (`vscode://`, `tel:`, custom schemes), and a non-Markdown local
file — including a `file://`-rerouted `.desktop` file or executable — is
passed to `xdg-open`, which may *launch* rather than view it. This requires
a deliberate click on a visible target (and involves no shell), so it
mirrors standard Markdown-viewer behavior.

> **Future work:** a confirmation modal showing the resolved destination
> for non-`http(s)` schemes and non-Markdown local files would close the
> "bundled malicious sibling file" scenario. Scope it narrowly — open
> `http(s)` and `.md`/image targets directly, prompt only for other
> schemes and local non-Markdown/non-image files — so it doesn't become a
> nag. See `src/app/nav.rs`, `src/editor/link.rs`.

## Invariants for contributors

When touching any content-handling path, keep these from regressing:

- **Decode through `ImageReader` + `Limits`.** A new raster decode site
  must keep the limits. Never call `image::load_from_memory` directly.
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
- **Bound new external input.** Size, timeout, and allocation caps before
  the input is trusted — mirror the image-loader, Mermaid-source, and
  update-check patterns.

When you fix or add one of the "watch out" items above, move it from that
section into "Hardening in place" and add the regression test alongside.
