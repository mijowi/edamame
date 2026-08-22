# Security

This document describes edamame's security posture: the threat model it is built against, the hardening that is already in place, and the areas a contributor should be careful around when adding features. It is both user-facing (what protects you when you open an untrusted document) and developer-facing (what invariants to preserve).

## Threat model

The primary adversary is **the author of a Markdown document you open**. edamame is a viewer/editor, so the realistic attack is: someone sends you a `.md` file (or you open one from an AI agent, a repo, a download), and the file — together with any local or remote resources it references — tries to do something you didn't intend.

Concretely, document content reaches several non-trivial subsystems:

- **Image decoding** — embedded/referenced PNG/JPEG/GIF/BMP/WEBP and SVG.
- **Remote fetches** — `http(s)` image URLs.
- **Mermaid diagrams** — fenced ` ```mermaid ` blocks rendered to images.
- **Syntax highlighting** — fenced code-block bodies parsed by TextMate grammars.
- **Link opening** — clicking a link hands a URL/path to the OS.
- **HTML export** — the document is serialized to a shareable HTML file.
- **Subprocess spawning** — `$EDITOR`, custom export commands.

What is **out of scope**: a malicious *local config* (`config.toml`, `keybindings.toml`, custom export commands, `$EDITOR`) is trusted — if an attacker can write your config they already own your account. The terminal emulator itself is also trusted.

Two principles run through the design:

1. **Document content never reaches a shell.** Subprocesses are always spawned with an explicit argv vector (`open::that`, `Command::new(...) .arg(...)`), never via a shell string. No attacker-controlled bytes are interpolated into a command line.
2. **Cost and capability are bounded before content is trusted.** Decodes are size-limited, network access is consent-gated, and the export path strips or flattens anything executable.

## Hardening in place

### Image decoding is bounded (decompression bombs)

A highly compressed raster expands enormously on decode — PNG zlib ratios routinely exceed 1000:1 — so a small file can ask the decoder to allocate gigabytes. The decode worker runs under `catch_unwind`, which contains a *panic* but **not** an OOM  a large enough allocation aborts the whole process.

Mitigations (`src/image/loader.rs`):

- Raster decoding goes through `image::ImageReader` with `image::Limits` in force: `max_image_width`/`max_image_height` capped at 50 000 px and `max_alloc` at 256 MB. A decode bomb returns a `Decode` error instead of exhausting memory. **Do not** revert to `image::load_from_memory`, which allocates the full pixel buffer with no ceiling — and note the cap has to be enforced *at decode time*, because the loader's `pre_resize` downscale runs only *after* the full buffer already exists.
- Local image files are size-capped (64 MB) via a `metadata` check before `std::fs::read`. Remote bodies are already capped at 10 MB by ureq.
- SVG rasterization (`src/image/svg.rs`) clamps the output pixmap to the cell envelope when the caller supplies one, and to an absolute ceiling (8192 px per side, 4 M pixels total) in every case — including HTML export, which rasterizes Mermaid diagrams at natural size because an exported PNG isn't sized in terminal cells. An over-size SVG is scaled down to fit rather than refused, so a giant declared size costs resolution, not an unbounded allocation.

### Remote access is consent-gated and IP-filtered

Remote image fetching is off unless the user allows it. The `images.enabled` / remote-image policy defaults to **Ask**: edamame prompts on document load and only fetches `http(s)` URLs after the user opts in (`src/image/loader.rs`, `RemoteImagePolicy`). Fetches are bounded by a 10 s timeout across connect / receive-response / receive-body and a 10 MB body cap. ureq uses rustls (no system OpenSSL).

This is equivalent to an email client's "load remote images" gate: it prevents a zero-interaction tracking pixel.

Even after consent, fetches are filtered against internal address ranges to prevent SSRF. `PublicOnlyResolver` (`src/image/loader.rs`) wraps ureq's default resolver and drops any *resolved* address that is loopback / RFC1918 private / link-local (including the `169.254.169.254` cloud-metadata endpoint) / carrier-grade NAT / unique-local or link-local IPv6 / unspecified / broadcast / documentation; a host that resolves only to internal addresses is reported `HostNotFound`. Filtering the resolved IP (not the hostname) defeats literal-IP URLs, DNS names pointed at private space (DNS rebinding), and `3xx` redirects uniformly — ureq re-resolves every hop through the same resolver. IPv4-mapped IPv6 addresses are unwrapped and re-checked so `::ffff:127.0.0.1` cannot slip past.

### HTML export strips and flattens executable content

The exported HTML is the one artifact a user typically **shares**, so the exporter (`src/export/html.rs`) is the most safety-critical path. Three layers protect it:

- **Raw HTML is stripped.** Block-level (`Event::Html`) and inline (`Event::InlineHtml`) events are filtered out before serialization, so a `<script>` tag (or any raw markup) in the source never reaches the output.
- **Link schemes are allowlisted.** pulldown-cmark's HTML writer performs no URL sanitization, so a `[x](javascript:…)` or `[x](data:text/html,…)` link would otherwise survive verbatim into the `<a href>` and run on click in a browser. `sanitize_link_urls` rewrites any link whose scheme is not in `SAFE_LINK_SCHEMES` (`http`, `https`, `mailto`, `tel`) to a harmless `#`. Relative paths, anchors, and `?query` targets carry no scheme and are preserved; scheme detection follows RFC 3986 (a colon after a `/`, `?`, or `#` is part of the path, not a scheme).
- **Mermaid diagrams are rasterized, not inlined as SVG.** Inline SVG can carry `<script>`, `foreignObject`, and `on*=` handlers that execute when the file is opened in a browser. `render_mermaid_png_data_uri` renders the diagram, rasterizes it to a PNG, and embeds it as a `data:image/png` `<img>` — flattening any executable payload to pixels. On render failure it falls back to an HTML-escaped code block, so the source is never lost.

### HTML export confines local-file reads

A self-contained export base64-embeds referenced images into the output. Because that output is shared, an embedded file *leaves the trust boundary* — unlike on-screen rendering, where the bytes never leave the victim's machine. A hostile `![x](/home/victim/private/diagram.svg)` could otherwise exfiltrate arbitrary on-disk files (text-based `.svg` especially) into an artifact the victim sends back.

`resolve_relative` (`src/export/html.rs`) now confines inlining to the source tree: absolute paths and `../` traversal are rejected up front, and the resolved path is `canonicalize()`d and required to stay under `source_dir` (which also defeats symlink escapes). An out-of-tree reference is simply left non-inlined rather than embedded. Inlining is additionally off by default.

### Subprocesses take no document-controlled input

- **External editor / custom export** (`src/app/external_editor.rs`, `src/export/custom.rs`): commands come from `$EDITOR`/`$VISUAL` and user config — both trusted — and are exec'd as an argv vector with no shell. No document data reaches argv.
- **Link/file opening** (`src/app/nav.rs`, `src/editor/link.rs`): the URL or path is passed to `open::that`, which spawns `xdg-open`/`open`/`start` with the target as a single argument — again, no shell interpolation.

### Mermaid rendering cannot execute code or touch the network

`mermaid-rs-renderer` is pure Rust: its dependency set contains no JS engine, no headless browser, and no HTTP client. Mermaid source cannot execute code, read files, or make network requests through the renderer. It is pinned exactly (`=0.2.2`) because it is pre-1.0 with known panic bugs; those panics are contained by `catch_unwind` at two layers (`src/diagram/mermaid.rs`).

The renderer has no internal length, node-count, or timeout bound, so a pathological diagram could otherwise drive unbounded CPU/RAM on the decode worker. `render_mermaid_svg` rejects any source over 64 KiB before dispatch (`MAX_MERMAID_SOURCE_BYTES`) — a single choke point covering both the TUI raster path and the HTML exporter — and an over-cap block falls back to the plain code block.

### Syntax highlighting is bounded and cannot execute code

Fenced code-block bodies are attacker-controlled text, and syntax highlighting hands them to `syntect`'s TextMate grammars — regex programs running on a backtracking engine, which is a classic denial-of-service shape. The grammars are data, not code: they cannot execute anything, read files, or make network requests, and syntect is built with `default-features = false` so neither its `.tmTheme`/`.sublime-syntax` *runtime* loaders (`plist-load`, `yaml-load`) nor its HTML writer are compiled in. The only dumps it deserializes are `include_bytes!` blobs fixed at build time.

Three caps bound the cost before content is trusted (`src/markdown/highlight.rs`), and all three bound **color, never content** — over any of them the block still renders every byte, just without highlighting, because a user must always be able to read their own code:

- `MAX_HIGHLIGHT_SOURCE_BYTES` (64 KiB, matching the Mermaid cap) bounds the *cold* parse. No parser is constructed at all for a larger block.
- `MAX_HIGHLIGHT_LINE_CHARS` (2 000) bounds the *per-keystroke* cost, which the byte cap cannot: highlighting reuses an unchanged prefix between edits, and a one-line block has none, so each keystroke re-parses the whole line. A minified bundle or a pasted base64 blob is exactly that shape.
- `MAX_HIGHLIGHT_GRAMMARS` (24) bounds *grammar compilation*, which neither of the others can, because it scales with how many languages a document names rather than with how big any block is. syntect compiles a grammar's regexes lazily, on first use of that language — around 9 ms each, ~18 ms for a large one. A document of fifty one-line fences in fifty different languages sits comfortably inside both other caps and still costs roughly 430 ms; with all 213 bundled grammars that reaches several seconds. That work runs on the warm worker rather than the render thread, so the cap bounds how much background CPU and queue memory one document can claim. It bounds a *burst* rather than the process lifetime: one slot returns every second, and a grammar already compiled is free forever (syntect keeps the regexes in a thread-safe cell in the shared set), so a long session that legitimately visits many languages keeps working while no single document can queue more than two dozen compiles at once. Past the budget a language renders plain exactly as an unknown one does.

The caps were sized against measurement rather than guessed — see the `#[ignore]`d `throughput` test in that module, which reports the parse figures for deliberately pathological inputs. *Parsing* runs synchronously on the render thread (deferring it would make colors flicker on the line being typed, which is worse than the cost), so there is no worker to absorb a slow parse; the tokenizer is wrapped in `catch_unwind`, so a grammar bug degrades one block to plain text rather than taking the process down. That recovery is real rather than nominal: the process panic hook restores the terminal and prints to stderr, which would wreck a running TUI, so each guarded section marks its thread via `terminal::panic_guard` and the hook stands down for a panic that is about to be caught.

*Compiling* a grammar is the one cost that scales with languages rather than text, and it happens on a worker thread (`highlight::spawn_warm_worker`), which also absorbs the ~2 ms dump deserialization. A block whose language is not yet compiled renders plain and picks up its color once the worker lands — so the ~430 ms fifty-language case above never reaches a frame at all, and `MAX_HIGHLIGHT_GRAMMARS` now bounds queued background work and queue memory rather than a stall.

### SVG parsing reads no external entities, network, or local files

`usvg`/`roxmltree` (`src/image/svg.rs`) does not expand external DTD entities (no XXE), performs no network requests, and ignores `http(s)` `<image href>` (no SSRF). usvg's default string resolver *would* read local files referenced by `<image href="/abs/path">`, so the rasterizer installs a custom `image_href_resolver.resolve_string` that refuses every path/URL href (embedded `data:` images still resolve). This removes local-file reads from untrusted SVGs entirely — render-only on the TUI path, but the change also closes the exfiltration channel that arises when such an SVG is inlined into an HTML export.

### Other bounded surfaces

- **Update check** (`src/app/update_check/`): a hardcoded `https://api.github.com` URL (not influenceable by the open document or by config), a 256 KB body cap, a 10 s timeout across connect / receive-response / receive-body, and a detached worker. Nothing is downloaded or executed, and the request carries no identity beyond an `edamame/<version>` User-Agent — no token, no telemetry, no document data.

  It runs **automatically at startup**, at most once per 24 h, and can be turned off entirely with `editor.check_for_updates` (default on) — asked on the welcome screen and available in the settings overlay. On a first run it waits for the welcome screen to be answered, so declining there means no request is made at all, not one already sent. This is the one network request edamame makes without a per-use prompt; it is treated differently from remote images because the endpoint is fixed and nothing about it is derived from the file you opened, so there is no tracking-pixel equivalent to gate. An explicit "Check for updates" (About page, command palette) always runs regardless of the setting, and ignores the 24 h throttle.

  Two fields are read out of the response: `tag_name`, and the release `body` shown as "what's new". Both are remote text and both are bounded before anything else sees them. The tag is capped at 64 bytes and must consist only of the semver alphabet (letters, digits, `.`, `-`, `_`, `+`); anything else is refused outright and the release is discarded, because the tag is both displayed and interpolated into the release URL that the "View on GitHub" button hands to your browser. The body is bounded more gently, since it is prose — the worker cuts it at the first `## Install` heading, strips control characters along with invisible formatting ones (bidi overrides, zero-width joiners), and caps it at 30 lines / 2 000 bytes, so the main thread never holds an unbounded string. It is then rendered **a line at a time, and never re-parsed as Markdown or HTML**: a heading line is bolded and a list marker becomes a bullet, and nothing else about a line is interpreted, so a release body cannot inject emphasis, links, images, or layout into the modal. Both fields are extracted with bounded hand-rolled scans anchored to genuine top-level keys (no JSON dependency); any malformed body degrades to "no release" or "no notes" rather than erroring.
- **Image cache** (`src/image/cache.rs`): keyed by literal URL (raster) or content `sha256` (diagrams); per-`EditorState`, GC'd to the live URL set each reparse (bounded growth), failed decodes memoized (no retry storm).
- **File watcher** (`src/watcher/file_watcher.rs`): reads only the user's own opened file; never writes; no exploitable check-then-act gap.
- **Atomic export writes** (`src/export/runner.rs`): `write_atomically` creates its temp file with a random, `O_EXCL` name via `NamedTempFile` in the target directory, then `persist`s (same-dir rename) over the output. No predictable sibling name a local attacker could pre-plant as a symlink to redirect the write.

## Things to watch out for (future hardening)

These are not zero-interaction vulnerabilities today — each is gated by consent, a deliberate click, or trusted config — but each would raise the floor, and a careless change nearby could turn one into a real hole.

### No confirmation before opening risky link targets

Following a link auto-opens with no prompt. Any URI scheme is handed to the OS handler (`vscode://`, `tel:`, custom schemes), and a non-Markdown local file — including a `file://`-rerouted `.desktop` file or executable — is passed to `xdg-open`, which may *launch* rather than view it. This requires a deliberate click on a visible target (and involves no shell), so it mirrors standard Markdown-viewer behavior.

> **Future work:** a confirmation modal showing the resolved destination for non-`http(s)` schemes and non-Markdown local files would close the "bundled malicious sibling file" scenario. Scope it narrowly — open `http(s)` and `.md`/image targets directly, prompt only for other schemes and local non-Markdown/non-image files — so it doesn't become a nag. See `src/app/nav.rs`, `src/editor/link.rs`.

## Reporting a problem

Please report privately rather than opening a public issue: [**Report a vulnerability**](https://github.com/mijowi/edamame/security/advisories/new).

The reporting policy — what's in scope, what to include, expected response time — lives in [`SECURITY.md`](../SECURITY.md) at the repository root, which is where GitHub surfaces it. This page is the technical detail behind it.

---

*Contributors: the checklist of invariants a change must not regress is in [`dev/security-invariants.md`](dev/security-invariants.md).*
