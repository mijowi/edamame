use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use pulldown_cmark::{
    html as cmark_html, CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd,
};

use super::runner::{write_atomically, ExportOutcome};
use crate::diagram;
use crate::image::{rasterize_svg, SvgScaleMode, SvgSizing};

/// The compiled-in stylesheet bundled with edamame.  Used when
/// [`HtmlExportOptions::stylesheet`] is [`Stylesheet::Builtin`].
pub const BUILTIN_STYLESHEET: &str = include_str!("../../config/export/default.css");

/// Source of the CSS embedded in the generated HTML document.
#[derive(Debug, Clone)]
pub enum Stylesheet {
    /// Use the edamame-bundled stylesheet (`config/export/default.css`).
    Builtin,
    /// Read a user CSS file at export time.
    Path(PathBuf),
    /// Use the supplied CSS verbatim.  Primarily for tests and embeddings —
    /// the binary only ever builds `Builtin` / `Path` (via
    /// `from_config_value`), so this is lib-only surface in the bin build.
    #[allow(dead_code)]
    Inline(String),
}

impl Stylesheet {
    /// Parse the string form of `[export.html].stylesheet` from the
    /// config.  The sentinel `"builtin"` maps to [`Stylesheet::Builtin`];
    /// every other value is treated as a filesystem path.
    pub fn from_config_value(value: &str) -> Self {
        if value.eq_ignore_ascii_case("builtin") {
            Self::Builtin
        } else {
            Self::Path(PathBuf::from(value))
        }
    }

    fn load(&self) -> Result<String> {
        match self {
            Self::Builtin => Ok(BUILTIN_STYLESHEET.to_owned()),
            Self::Path(p) => std::fs::read_to_string(p)
                .with_context(|| format!("Failed to read stylesheet: {}", p.display())),
            Self::Inline(s) => Ok(s.clone()),
        }
    }
}

/// Options passed to [`render_html`] / [`spawn_html_export`].
#[derive(Debug, Clone)]
pub struct HtmlExportOptions {
    /// Source of the embedded CSS.
    pub stylesheet: Stylesheet,
    /// When true, relative `![alt](path.png)` references are read from
    /// disk and base64-embedded as `data:` URIs so the generated HTML
    /// is self-contained.  Requires `source_dir` to be set.
    ///
    /// Remote URLs (`http://`, `https://`, `data:`) are left untouched
    /// regardless of this flag.
    pub inline_images: bool,
    /// Directory used to resolve relative image paths when
    /// `inline_images` is true.  Typically the directory containing the
    /// source `.md` file.  `None` disables the rewrite even if
    /// `inline_images` is true.
    pub source_dir: Option<PathBuf>,
    /// Value inserted into the `<title>` element.  When `None`, a
    /// sensible fallback (`"Document"`) is used.
    pub title: Option<String>,
    /// When true (the default), fenced ```mermaid code blocks
    /// are rendered to inline SVG and wrapped in
    /// `<figure class="mermaid-diagram">`.  Falls back to the usual
    /// `<pre><code class="language-mermaid">` on render failure so the
    /// source is never lost.
    pub render_diagrams: bool,
}

impl Default for HtmlExportOptions {
    fn default() -> Self {
        Self {
            stylesheet: Stylesheet::Builtin,
            inline_images: false,
            source_dir: None,
            title: None,
            render_diagrams: true,
        }
    }
}

/// Render `markdown` to a standalone HTML document.
///
/// Mirrors the parser options used by the in-app renderer (tables, task
/// lists, strikethrough, footnotes, smart punctuation, and — when this
/// document opens with one — frontmatter) so exported documents look the
/// same as the terminal preview.  Raw HTML events —
/// both block-level (`Event::Html`) and inline (`Event::InlineHtml`) —
/// are filtered out before serialization so attacker-controlled Markdown
/// cannot inject `<script>` tags or other executable content into the
/// exported file.
pub fn render_html(markdown: &str, opts: &HtmlExportOptions) -> Result<String> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);
    // Frontmatter must be recognised here for the same reason it is in the
    // renderer: without it, a `---` block parses as a thematic break plus
    // a setext H2 and the exported file opens with the file's YAML keys
    // as its loudest heading.  pulldown-cmark's HTML writer emits nothing
    // for a metadata block, which is the wanted behavior — the
    // frontmatter is data *about* the document, not part of its body.
    //
    // The extension is enabled only when *this* document opens with the
    // matching delimiter, and the decision comes from the shared
    // `metadata_options_for` rather than a second copy of the rule: the
    // extensions are not anchored to the start of the document on their
    // own, so leaving them on unconditionally would let a mid-document
    // `---` separator claim the section under it — and, because the
    // writer emits nothing for a metadata block, drop that section from
    // the export without a word.
    options |= crate::markdown::parse_offsets::metadata_options_for(markdown);

    let parser = Parser::new_ext(markdown, options);

    // Collect so the optional image-rewrite pass can mutate events in
    // place.  The event stream for a document of any realistic size is
    // small relative to the rope we start from, so this is fine.
    let mut events: Vec<Event> = parser
        .filter(|e| !matches!(e, Event::Html(_) | Event::InlineHtml(_)))
        .collect();

    if opts.inline_images {
        if let Some(dir) = opts.source_dir.as_deref() {
            rewrite_images_to_data_uris(&mut events, dir);
        }
    }

    if opts.render_diagrams {
        events = replace_mermaid_with_image(events);
    }

    // Neutralize dangerous link schemes (`javascript:`, `vbscript:`,
    // non-image `data:`, …) before serialization.  pulldown-cmark's HTML
    // writer performs no URL sanitization, so without this a
    // `[x](javascript:…)` link survives verbatim into the exported `<a
    // href>` and runs on click in a browser.
    sanitize_link_urls(&mut events);

    let mut body = String::new();
    cmark_html::push_html(&mut body, events.into_iter());

    let css = opts.stylesheet.load()?;
    let title = opts.title.as_deref().unwrap_or("Document");

    Ok(format!(
        "<!doctype html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n\
         <style>\n{css}\n</style>\n\
         </head>\n\
         <body>\n\
         <main class=\"markdown-body\">\n\
         {body}\n\
         </main>\n\
         </body>\n\
         </html>\n",
        title = html_escape(title),
    ))
}

/// Spawn a worker thread that renders `markdown` to `target`.  The
/// provided closure is invoked on the worker thread once the write
/// completes (or fails); callers typically forward the outcome to the
/// App's mpsc channel so the UI thread can surface a transient message.
///
/// The caller is responsible for running [`crate::export::preflight`]
/// first — this function will clobber `target` if it exists.
pub fn spawn_html_export(
    markdown: String,
    target: PathBuf,
    opts: HtmlExportOptions,
    on_done: impl FnOnce(ExportOutcome) + Send + 'static,
) {
    std::thread::spawn(move || {
        let result = render_and_write(&markdown, &target, &opts).map(|()| target.clone());
        on_done(result.map_err(|e| format!("{e:#}")));
    });
}

fn render_and_write(markdown: &str, target: &Path, opts: &HtmlExportOptions) -> Result<()> {
    let html = render_html(markdown, opts)?;
    write_atomically(target, html.as_bytes())
        .with_context(|| format!("Failed to write export: {}", target.display()))?;
    Ok(())
}

// ── Link URL sanitization ─────────────────────────────────────────────────

/// Schemes permitted on an exported link destination.  Everything else —
/// notably `javascript:`, `vbscript:`, and `data:` — is neutralized.
const SAFE_LINK_SCHEMES: &[&str] = &["http", "https", "mailto", "tel"];

/// Rewrite the destination of every `Tag::Link` whose URL carries a scheme
/// outside [`SAFE_LINK_SCHEMES`] to a harmless `#`.  Relative paths,
/// anchors, and fragment targets carry no scheme and are left untouched.
fn sanitize_link_urls(events: &mut [Event<'_>]) {
    for event in events.iter_mut() {
        if let Event::Start(Tag::Link { dest_url, .. }) = event {
            if !is_safe_link_url(dest_url.as_ref()) {
                *dest_url = CowStr::Borrowed("#");
            }
        }
    }
}

/// True when `url` is safe to emit verbatim into an `<a href>`: either it
/// has no URL scheme (relative path, `#anchor`, `?query`) or its scheme is
/// on the allowlist.  A "scheme" is an RFC-3986 token — `alpha *( alpha /
/// digit / "+" / "-" / "." )` — terminated by `:` *before* any `/`, `?`,
/// or `#`; a colon that appears after one of those is part of the path
/// (e.g. `foo/bar:baz`) and does not make a scheme.
fn is_safe_link_url(url: &str) -> bool {
    let url = url.trim();
    let Some(idx) = url.find([':', '/', '?', '#']) else {
        return true; // no delimiter at all → relative
    };
    if url.as_bytes()[idx] != b':' {
        return true; // a path/query/fragment delimiter came first → relative
    }
    let scheme = &url[..idx];
    let scheme_shaped = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !scheme_shaped {
        // The colon isn't part of a real scheme (e.g. a port-looking
        // path segment) → treat as relative.
        return true;
    }
    SAFE_LINK_SCHEMES
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
}

// ── Mermaid diagrams ──────────────────────────────────────────────────────

/// Walk the event stream; for every `Start(CodeBlock(Fenced("mermaid")))`
/// ... `End(CodeBlock)` triple, try to render the enclosed text as a
/// mermaid diagram and substitute a single `Event::Html` carrying
/// `<figure class="mermaid-diagram"><img …></figure>`.  On render failure
/// (or on non-mermaid code blocks) the original events are preserved so
/// pulldown-cmark emits the usual `<pre><code class="language-mermaid">`
/// — the diagram source is never lost.
///
/// The diagram is **rasterized to a PNG** and embedded as a `data:` image
/// rather than inlined as raw `<svg>`.  Inline SVG can carry `<script>`,
/// `foreignObject`, and `on*=` event handlers that execute when the
/// exported file is opened in a browser; rasterizing flattens the diagram
/// to pixels, so no executable markup from the (document-controlled,
/// third-party-rendered) SVG can survive into the export.
///
/// Matching is case-insensitive on the language tag, same as the in-app
/// `promote_diagram_code_blocks` pass, so round-tripping between the
/// editor and the exported HTML is consistent.
fn replace_mermaid_with_image(events: Vec<Event<'_>>) -> Vec<Event<'_>> {
    let mut out: Vec<Event<'_>> = Vec::with_capacity(events.len());
    let mut iter = events.into_iter();
    while let Some(event) = iter.next() {
        let lang = match &event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang)))
                if lang.as_ref().eq_ignore_ascii_case("mermaid") =>
            {
                Some(lang.clone())
            }
            _ => None,
        };
        if lang.is_none() {
            out.push(event);
            continue;
        }
        // Collect the Text events until the matching CodeBlock end, then
        // decide — render succeeded → emit a single Event::Html, render
        // failed → replay the original Start + Texts + End so the
        // fallback `<pre><code>` is emitted by the default serialiser.
        let mut buffered: Vec<Event<'_>> = vec![event];
        let mut source = String::new();
        for inner in iter.by_ref() {
            match inner {
                Event::End(TagEnd::CodeBlock) => {
                    buffered.push(Event::End(TagEnd::CodeBlock));
                    break;
                }
                Event::Text(ref t) => {
                    source.push_str(t);
                    buffered.push(inner);
                }
                other => {
                    // pulldown-cmark should never emit other events
                    // inside a fenced code block, but if it does we
                    // treat it like text for the renderer and preserve
                    // it for the fallback.
                    buffered.push(other);
                }
            }
        }
        match render_mermaid_png_data_uri(&source) {
            Some(data_uri) => {
                let html = format!(
                    "<figure class=\"mermaid-diagram\">\
                     <img alt=\"mermaid diagram\" src=\"{data_uri}\">\
                     </figure>"
                );
                out.push(Event::Html(CowStr::Boxed(html.into_boxed_str())));
            }
            None => {
                // Falls back to the default code-block serialisation.
                // The mermaid source is preserved verbatim so the user
                // (or a downstream mermaid.js) can still see / render
                // it.  We deliberately swallow the error here — the
                // per-diagram failure is not fatal to the document
                // export.
                out.extend(buffered);
            }
        }
    }
    out
}

/// Render mermaid `source` to a PNG `data:` URI, or `None` on any failure
/// (so the caller falls back to the escaped code block).  The SVG produced
/// by the renderer never reaches the HTML — it is rasterized to pixels
/// first (white background, like the TUI path), which strips any script /
/// `foreignObject` / event-handler payload a hostile node label might have
/// smuggled through the renderer's escaping.
fn render_mermaid_png_data_uri(source: &str) -> Option<String> {
    let svg = diagram::render_mermaid_svg(source).ok()?;
    let image = rasterize_svg(
        &svg,
        SvgSizing {
            envelope: None,
            font_size: None,
            mode: SvgScaleMode::Natural,
        },
        Some([255, 255, 255, 255]),
    )
    .ok()?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        BASE64.encode(png.into_inner())
    ))
}

// ── Image inlining ────────────────────────────────────────────────────────

fn rewrite_images_to_data_uris(events: &mut [Event<'_>], source_dir: &Path) {
    for event in events.iter_mut() {
        if let Event::Start(Tag::Image { dest_url, .. }) = event {
            if let Some(new_url) = inline_image_data_uri(dest_url.as_ref(), source_dir) {
                *dest_url = CowStr::Boxed(new_url.into_boxed_str());
            }
        }
    }
}

/// Return a `data:` URI for `url` if it resolves to a readable local
/// image file.  `None` signals "leave as-is" — covers remote URLs
/// (`http(s)://`), URIs already in `data:` form, and any path we cannot
/// read or classify.
fn inline_image_data_uri(url: &str, source_dir: &Path) -> Option<String> {
    if is_remote_url(url) {
        return None;
    }
    let path = resolve_relative(url, source_dir)?;
    let bytes = std::fs::read(&path).ok()?;
    let mime = mime_from_extension(&path)?;
    let mut encoded = String::from("data:");
    encoded.push_str(mime);
    encoded.push_str(";base64,");
    encoded.push_str(&BASE64.encode(&bytes));
    Some(encoded)
}

fn is_remote_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("data:")
        || lower.starts_with("file://")
}

/// Resolve a relative image `url` against `source_dir`, returning the path
/// **only if it stays within `source_dir`**.  A self-contained HTML export
/// is an artifact the victim typically shares, so an out-of-tree path
/// (absolute, `../` traversal, or a symlink escape) would let a hostile
/// document exfiltrate arbitrary on-disk files by riding them base64-
/// encoded into the shared output.  Absolute paths and explicit `..`
/// components are rejected up front; the post-`canonicalize` containment
/// check additionally defeats symlinks that point outside the tree.
///
/// An out-of-tree reference returns `None` → the caller leaves the
/// original (non-inlined) reference in place, so the export simply doesn't
/// embed it rather than leaking it.
fn resolve_relative(url: &str, source_dir: &Path) -> Option<PathBuf> {
    let p = Path::new(url);
    if p.is_absolute() {
        return None;
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let joined = source_dir.join(p);
    let canon_dir = source_dir.canonicalize().ok()?;
    let canon = joined.canonicalize().ok()?;
    canon.starts_with(&canon_dir).then_some(canon)
}

fn mime_from_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

// ── HTML escaping ─────────────────────────────────────────────────────────

/// Escape the five XML metacharacters.  Used only for the `<title>`
/// element; the document body is escaped by `pulldown_cmark::html`.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn opts_inline_css() -> HtmlExportOptions {
        HtmlExportOptions {
            stylesheet: Stylesheet::Inline("body { color: red; }".into()),
            ..HtmlExportOptions::default()
        }
    }

    #[test]
    fn renders_basic_markdown() {
        let html = render_html("# Hello\n\nWorld", &opts_inline_css()).unwrap();
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(html.contains("<p>World</p>"));
    }

    /// Frontmatter is data about the document, not part of its body:
    /// pulldown-cmark's writer suppresses a metadata block entirely.  The
    /// options have to be enabled here too, or the export would reproduce
    /// the rule-plus-setext-H2 misparse the renderer no longer has.
    #[test]
    fn frontmatter_is_omitted_from_the_export() {
        let md = "---\ntitle: Foo\ndate: 2026-01-01\n---\n\n# Heading\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("<h1>Heading</h1>"));
        assert!(!html.contains("title: Foo"), "got: {html}");
        assert!(!html.contains("<h2>"), "got: {html}");
    }

    /// The export must not drop a section a mid-document `---` separator
    /// happens to bracket.  pulldown-cmark's writer emits *nothing* for a
    /// metadata block, so an unanchored extension here loses content the
    /// user wrote — silently, and only in the exported file.
    #[test]
    fn a_mid_document_rule_pair_is_not_dropped_from_the_export() {
        let md = "Intro.\n\n---\n## Section 2\n\nText.\n\n---\n## Section 3\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("Section 2"), "got: {html}");
        assert!(html.contains("Text."), "got: {html}");
        assert!(html.contains("Section 3"), "got: {html}");
    }

    /// The export's gate must be the same one the renderer uses, or the
    /// two disagree about whether a block is frontmatter at all.
    #[test]
    fn a_toml_opening_file_does_not_drop_a_later_dash_pair() {
        let md = "+++\na = 1\n+++\n\n---\nSection\n---\n\nEnd.\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(!html.contains("a = 1"), "got: {html}");
        assert!(html.contains("Section"), "got: {html}");
    }

    #[test]
    fn renders_gfm_table() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("<table>"));
        assert!(html.contains("<th>a</th>"));
        assert!(html.contains("<td>1</td>"));
    }

    #[test]
    fn renders_task_list_and_strikethrough() {
        let md = "- [x] done\n- [ ] todo\n\n~~gone~~";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("type=\"checkbox\""));
        assert!(html.contains("<del>gone</del>"));
    }

    #[test]
    fn strips_raw_html_block() {
        let md = "text\n\n<script>alert('x')</script>\n\nmore";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            !html.contains("<script>"),
            "raw <script> must be stripped — got:\n{html}"
        );
    }

    #[test]
    fn strips_raw_html_inline() {
        let md = "a <b onclick=\"x\">inline</b> c";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            !html.contains("onclick"),
            "inline HTML event handlers must be stripped — got:\n{html}"
        );
    }

    #[test]
    fn escapes_title() {
        let mut opts = opts_inline_css();
        opts.title = Some("A <script>x</script> & B".into());
        let html = render_html("", &opts).unwrap();
        assert!(html.contains("<title>A &lt;script&gt;x&lt;/script&gt; &amp; B</title>"));
        assert!(!html.contains("<title>A <script>"));
    }

    #[test]
    fn embeds_builtin_stylesheet() {
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Builtin,
            ..HtmlExportOptions::default()
        };
        let html = render_html("hi", &opts).unwrap();
        assert!(html.contains("markdown-body"));
        assert!(html.contains("<style>"));
    }

    #[test]
    fn footnotes_render_with_bracket_convention() {
        // The reference markup is the `<sup class="footnote-reference">…`
        // that the bundled CSS targets to add `[ ]` brackets, and the
        // bracket pseudo-element rules ship in the builtin stylesheet.
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Builtin,
            ..HtmlExportOptions::default()
        };
        let html = render_html("Claim.[^1]\n\n[^1]: Source.\n", &opts).unwrap();
        assert!(
            html.contains("<sup class=\"footnote-reference\"><a href=\"#1\">1</a></sup>"),
            "expected footnote-reference markup, got:\n{html}"
        );
        assert!(
            html.contains("sup.footnote-reference a::before { content: \"[\"; }"),
            "builtin CSS must add the opening bracket"
        );
        assert!(
            html.contains("sup.footnote-reference a::after { content: \"]\"; }"),
            "builtin CSS must add the closing bracket"
        );
    }

    #[test]
    fn stylesheet_from_config_value_parses() {
        assert!(matches!(
            Stylesheet::from_config_value("builtin"),
            Stylesheet::Builtin
        ));
        assert!(matches!(
            Stylesheet::from_config_value("BUILTIN"),
            Stylesheet::Builtin
        ));
        match Stylesheet::from_config_value("/etc/custom.css") {
            Stylesheet::Path(p) => assert_eq!(p, PathBuf::from("/etc/custom.css")),
            other => panic!("expected Path, got {other:?}"),
        }
    }

    #[test]
    fn stylesheet_path_read_failure_surfaces_error() {
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Path(PathBuf::from("/this/does/not/exist.css")),
            ..HtmlExportOptions::default()
        };
        let err = render_html("hi", &opts).unwrap_err();
        assert!(format!("{err:#}").contains("stylesheet"));
    }

    #[test]
    fn inline_images_embeds_local_png() {
        // 1x1 transparent PNG
        const ONE_PX_PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let dir = tempdir().unwrap();
        let img_path = dir.path().join("pixel.png");
        let mut f = std::fs::File::create(&img_path).unwrap();
        f.write_all(ONE_PX_PNG).unwrap();
        drop(f);

        let md = "![pixel](pixel.png)";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            inline_images: true,
            source_dir: Some(dir.path().to_path_buf()),
            title: None,
            render_diagrams: false,
        };
        let html = render_html(md, &opts).unwrap();
        assert!(
            html.contains("src=\"data:image/png;base64,"),
            "expected base64 data URI, got:\n{html}"
        );
        // The original relative reference must be gone.
        assert!(!html.contains("src=\"pixel.png\""));
    }

    #[test]
    fn inline_images_leaves_remote_urls_untouched() {
        let md = "![cat](https://example.com/cat.png)";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            inline_images: true,
            source_dir: Some(PathBuf::from("/tmp")),
            title: None,
            render_diagrams: false,
        };
        let html = render_html(md, &opts).unwrap();
        assert!(html.contains("src=\"https://example.com/cat.png\""));
    }

    #[test]
    fn inline_images_disabled_by_default() {
        let md = "![x](local.png)";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("src=\"local.png\""));
    }

    // ── Vuln 2: link-scheme sanitization ──────────────────────────────

    #[test]
    fn neutralizes_javascript_link_scheme() {
        let md = "[click](javascript:alert(document.cookie))";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            !html.contains("javascript:"),
            "javascript: href must be neutralized — got:\n{html}"
        );
        assert!(html.contains("href=\"#\""));
    }

    #[test]
    fn neutralizes_data_html_link_scheme() {
        let md = "[x](data:text/html;base64,PHNjcmlwdD4=)";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            !html.contains("data:text/html"),
            "data: link must be neutralized:\n{html}"
        );
    }

    #[test]
    fn preserves_safe_link_schemes_and_relative_targets() {
        let md = "[a](https://example.com) [b](mailto:x@y.z) [c](./page.md) [d](#anchor) [e](foo/bar:baz)";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("href=\"https://example.com\""));
        assert!(html.contains("href=\"mailto:x@y.z\""));
        assert!(html.contains("href=\"./page.md\""));
        assert!(html.contains("href=\"#anchor\""));
        // A colon after a path segment is not a scheme → left intact.
        assert!(html.contains("href=\"foo/bar:baz\""));
    }

    #[test]
    fn is_safe_link_url_classifies_schemes() {
        assert!(is_safe_link_url("https://example.com"));
        assert!(is_safe_link_url("HTTP://EXAMPLE.COM"));
        assert!(is_safe_link_url("mailto:a@b.c"));
        assert!(is_safe_link_url("/abs/path"));
        assert!(is_safe_link_url("./rel"));
        assert!(is_safe_link_url("#frag"));
        assert!(is_safe_link_url("?q=1"));
        assert!(is_safe_link_url("path/to:thing"));
        assert!(!is_safe_link_url("javascript:alert(1)"));
        assert!(!is_safe_link_url("  javascript:alert(1)"));
        assert!(!is_safe_link_url("vbscript:msgbox"));
        assert!(!is_safe_link_url("data:text/html,x"));
        assert!(!is_safe_link_url("file:///etc/passwd"));
    }

    // ── Vuln 3: mermaid export carries no raw SVG / script ─────────────

    #[test]
    fn mermaid_export_never_emits_raw_svg_or_script() {
        // Whether the live renderer is available or not, a hostile node
        // label must never produce inline SVG or executable markup: a
        // successful render is rasterized to a PNG data URI; a failed one
        // falls back to an HTML-escaped code block.
        let md = "```mermaid\nflowchart TD\n  A[\"<script>alert(1)</script>\"] --> B\n```";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            render_diagrams: true,
            ..HtmlExportOptions::default()
        };
        let html = render_html(md, &opts).unwrap();
        assert!(
            !html.contains("<svg"),
            "no raw SVG may reach the export:\n{html}"
        );
        assert!(!html.contains("foreignObject"));
        assert!(
            !html.contains("<script>"),
            "no executable <script> may reach the export:\n{html}"
        );
    }

    // ── Vuln 4: image inlining stays within the source tree ────────────

    fn write_one_px_png(path: &Path) {
        const ONE_PX_PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(path, ONE_PX_PNG).unwrap();
    }

    #[test]
    fn inline_images_rejects_absolute_path() {
        let outside = tempdir().unwrap();
        let secret = outside.path().join("secret.png");
        write_one_px_png(&secret);
        let source = tempdir().unwrap();

        let md = format!("![x]({})", secret.display());
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            inline_images: true,
            source_dir: Some(source.path().to_path_buf()),
            render_diagrams: false,
            ..HtmlExportOptions::default()
        };
        let html = render_html(&md, &opts).unwrap();
        assert!(
            !html.contains("data:image/png"),
            "absolute out-of-tree path must not be inlined:\n{html}"
        );
    }

    #[test]
    fn inline_images_rejects_parent_traversal() {
        let root = tempdir().unwrap();
        let secret = root.path().join("secret.png");
        write_one_px_png(&secret);
        let source = root.path().join("docs");
        std::fs::create_dir(&source).unwrap();

        let md = "![x](../secret.png)";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            inline_images: true,
            source_dir: Some(source.clone()),
            render_diagrams: false,
            ..HtmlExportOptions::default()
        };
        let html = render_html(md, &opts).unwrap();
        assert!(
            !html.contains("data:image/png"),
            "../ traversal must not be inlined:\n{html}"
        );
    }

    #[test]
    fn spawn_html_export_writes_file_and_reports_success() {
        use std::sync::mpsc;
        let dir = tempdir().unwrap();
        let target = dir.path().join("out.html");
        let (tx, rx) = mpsc::channel();
        spawn_html_export(
            "# hi\n".into(),
            target.clone(),
            HtmlExportOptions {
                stylesheet: Stylesheet::Inline("body{}".into()),
                ..HtmlExportOptions::default()
            },
            move |outcome| {
                tx.send(outcome).unwrap();
            },
        );
        let outcome = rx.recv().unwrap();
        assert_eq!(outcome.unwrap(), target);
        let written = std::fs::read_to_string(&target).unwrap();
        assert!(written.contains("<h1>hi</h1>"));
    }
}
