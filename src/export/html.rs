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

/// The compiled-in stylesheet, used for [`Stylesheet::Builtin`].
pub const BUILTIN_STYLESHEET: &str = include_str!("../../config/export/default.css");

/// Source of the CSS embedded in the generated HTML document.
#[derive(Debug, Clone)]
pub enum Stylesheet {
    /// The bundled `config/export/default.css`.
    Builtin,
    /// Read a user CSS file at export time.
    Path(PathBuf),
    /// CSS verbatim.  Tests and embeddings only — the binary builds `Builtin` / `Path`.
    #[allow(dead_code)]
    Inline(String),
}

impl Stylesheet {
    /// Parse `[export.html].stylesheet`: the sentinel `"builtin"`, or a filesystem path.
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
    /// Embed relative image references as `data:` URIs so the HTML is self-contained.  Requires
    /// `source_dir`; remote and already-`data:` URLs are untouched either way.
    pub inline_images: bool,
    /// Resolves relative image paths, and bounds them: see [`embeddable_image`].  `None`
    /// disables the rewrite even when `inline_images` is true.
    pub source_dir: Option<PathBuf>,
    /// Canonical paths *outside* `source_dir` the user agreed to embed, as listed by
    /// [`outside_images`].  An out-of-folder image not on this list stays a plain link.
    pub approved_outside: Vec<PathBuf>,
    /// `<title>` text; `None` falls back to `"Document"`.
    pub title: Option<String>,
    /// Render *figures* — fenced ```mermaid code blocks and `$$...$$` display math — to PNG
    /// embedded in a `<figure>` (`mermaid-diagram` / `math-formula`), each falling back to its
    /// source form on failure so the source is never lost.  Independent of this flag, inline `$…$`
    /// is always emitted as literal source, matching the terminal preview.
    pub render_figures: bool,
}

impl Default for HtmlExportOptions {
    fn default() -> Self {
        Self {
            stylesheet: Stylesheet::Builtin,
            inline_images: false,
            source_dir: None,
            approved_outside: Vec::new(),
            title: None,
            render_figures: true,
        }
    }
}

/// Render `markdown` to a standalone HTML document, mirroring the in-app renderer's parser
/// options so an export looks like the terminal preview.
///
/// **Raw HTML events are filtered out before serialization** — block *and* inline — so
/// attacker-controlled Markdown cannot inject `<script>` or other executable content.
pub fn render_html(markdown: &str, opts: &HtmlExportOptions) -> Result<String> {
    let parser = Parser::new_ext(markdown, parser_options(markdown));

    // Collected so the image-rewrite pass can mutate events in place.
    let mut events: Vec<Event> = parser
        .filter(|e| !matches!(e, Event::Html(_) | Event::InlineHtml(_)))
        .collect();

    if opts.inline_images {
        if let Some(dir) = opts.source_dir.as_deref() {
            rewrite_images_to_data_uris(&mut events, dir, &opts.approved_outside);
        }
    }

    if opts.render_figures {
        events = replace_mermaid_with_image(events);
    }
    // Always run, so inline `$…$` and (with figures off) display math
    // collapse to literal source rather than a bare `<span class="math">`.
    events = replace_math(events, opts.render_figures);

    // pulldown-cmark's HTML writer performs no URL sanitization, so without this a
    // `[x](javascript:…)` link survives into the exported `<a href>` and runs on click.
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

/// Render `markdown` to `target` on a worker thread, invoking the closure there with the outcome.
///
/// **The caller must run [`crate::export::preflight`] first** — this clobbers an existing
/// `target`.
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

/// The parser options every export pass uses, so [`outside_images`] sees exactly the images
/// [`render_html`] would.
fn parser_options(markdown: &str) -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);
    // Recognize math so `$$…$$` reaches `replace_math` as `Event::DisplayMath` (rasterized to a
    // figure) rather than surviving as literal text.  `replace_math` always runs when this is on —
    // even with figures disabled — so inline `$…$` and un-rasterized display math collapse back to
    // their literal source instead of pulldown's `<span class="math">` wrapper.
    options.insert(Options::ENABLE_MATH);
    // Without the frontmatter extension a `---` block parses as a thematic break plus a setext
    // H2, and the export opens with the YAML keys as its loudest heading.  It is gated on *this*
    // document's opening delimiter, through the shared `metadata_options_for`: the extensions are
    // not anchored to the document start on their own, so leaving them on unconditionally would
    // let a mid-document `---` claim the section under it — and the writer emits nothing for a
    // metadata block, so that section would vanish from the export silently.
    options |= crate::markdown::parse_offsets::metadata_options_for(markdown);

    options
}

// ── Link URL sanitization ─────────────────────────────────────────────────

/// Schemes permitted on an exported link destination; everything else is neutralized.
const SAFE_LINK_SCHEMES: &[&str] = &["http", "https", "mailto", "tel"];

/// Rewrite every link destination outside [`SAFE_LINK_SCHEMES`] to a harmless `#`.  Relative
/// paths and anchors carry no scheme and are untouched.
fn sanitize_link_urls(events: &mut [Event<'_>]) {
    for event in events.iter_mut() {
        if let Event::Start(Tag::Link { dest_url, .. }) = event {
            if !is_safe_link_url(dest_url.as_ref()) {
                *dest_url = CowStr::Borrowed("#");
            }
        }
    }
}

/// True when `url` has no scheme at all or an allowlisted one.  A "scheme" is an RFC-3986 token
/// terminated by `:` *before* any `/`, `?`, or `#`; a later colon is part of the path
/// (`foo/bar:baz`) and makes no scheme.
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
        // Not a real scheme (a port-looking path segment) → relative.
        return true;
    }
    SAFE_LINK_SCHEMES
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
}

// ── Mermaid diagrams ──────────────────────────────────────────────────────

/// Replace each mermaid fence with a single `Event::Html` figure, preserving the original events
/// on render failure so the diagram source is never lost.
///
/// **The diagram is rasterized to a PNG `data:` image, never inlined as `<svg>`.**  Inline SVG can
/// carry `<script>`, `foreignObject`, and `on*=` handlers that execute when the export is opened
/// in a browser; flattening to pixels means no executable markup from the document-controlled,
/// third-party-rendered SVG can survive.
///
/// Language matching is case-insensitive, like the in-app `promote_diagram_code_blocks`.
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
        // Collect Text events to the matching end, then either emit one `Event::Html` or replay
        // the originals for the default serializer's fallback.
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
                    // Shouldn't occur inside a fenced code block; treat as text and preserve.
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
                // Fall back to the code block; a per-diagram failure is not fatal to the export.
                out.extend(buffered);
            }
        }
    }
    out
}

/// Render mermaid `source` to a PNG `data:` URI, or `None` on any failure.  The intermediate SVG
/// never reaches the HTML — rasterizing strips any script / `foreignObject` / event-handler
/// payload a hostile node label smuggled through the renderer's escaping.
fn render_mermaid_png_data_uri(source: &str) -> Option<String> {
    let svg = diagram::render_mermaid_svg(source).ok()?;
    svg_to_png_data_uri(&svg)
}

/// Rasterize an already-rendered diagram/math SVG to a PNG `data:` URI on
/// a white background (`None` on any failure).  Shared by the mermaid and
/// display-math passes: both flatten their SVG to pixels — never inlining
/// raw `<svg>`, which could carry `<script>` / `foreignObject` / `on*=`
/// payloads — and embed the PNG as an `<img>`.  Natural sizing keeps the
/// figure's own dimensions; `MAX_RASTER_*` in `image::svg` bounds them.
fn svg_to_png_data_uri(svg: &str) -> Option<String> {
    let image = rasterize_svg(
        svg,
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

// ── Display math ──────────────────────────────────────────────────────────

/// Rewrite math events in the stream, mirroring the terminal's promotion
/// rules (`markdown::parser::post_pass::promote_display_math_paragraphs`):
///
/// * A paragraph whose body is **only** display math (one or more
///   `$$…$$`, plus whitespace and breaks) is a *figure* paragraph: the
///   enclosing `<p>` is dropped (a block-level figure/code block can't nest
///   in `<p>`) and each formula becomes its own block.  With figures on it
///   rasterizes to a PNG `<figure class="math-formula">`; with figures off,
///   or on a render failure, it becomes a fenced `math` code block
///   (`push_display_math_source_block`) — the styled, padded box mermaid's
///   non-inlined fallback gets, delimiters removed — never loose `$$…$$`
///   text.
/// * Everywhere else — inline `$…$`, display math mixed with other
///   inlines, or math in a heading / list item — the math collapses to
///   its literal `$…$` / `$$…$$` source, exactly as the terminal shows
///   un-promoted math.
///
/// Enabling `Options::ENABLE_MATH` is what makes these events exist, so
/// this pass must run whenever that option is set — otherwise pulldown's
/// HTML writer would emit a bare `<span class="math">` wrapper (no KaTeX /
/// MathJax ships with the export, so it would render as raw source anyway,
/// only less predictably).
///
/// Rasterizing to PNG rather than inlining SVG is the same defence the
/// mermaid pass relies on: no executable markup from RaTeX's output can
/// survive into the exported file.
fn replace_math(events: Vec<Event<'_>>, render_figures: bool) -> Vec<Event<'_>> {
    let mut out: Vec<Event<'_>> = Vec::with_capacity(events.len());
    let mut iter = events.into_iter();
    while let Some(event) = iter.next() {
        match event {
            Event::Start(Tag::Paragraph) => {
                // Buffer the paragraph body up to its close (paragraphs
                // never nest in CommonMark, so the first End wins).
                let mut body: Vec<Event<'_>> = Vec::new();
                for inner in iter.by_ref() {
                    if matches!(inner, Event::End(TagEnd::Paragraph)) {
                        break;
                    }
                    body.push(inner);
                }
                if is_display_math_only(&body) {
                    // A figure paragraph: drop the enclosing `<p>` (a
                    // block-level `<figure>` / `<pre>` can't nest in `<p>`)
                    // and emit one block per formula — a rasterized
                    // `<figure>` when figures are on and the render
                    // succeeds, otherwise a fenced `math` code block (the
                    // export peer of the in-app figures-off `math` block,
                    // and the parallel of mermaid's non-inlined code-block
                    // fallback).  Whitespace text and breaks were only
                    // separators between formulas — drop them with the `<p>`.
                    for inner in body {
                        if let Event::DisplayMath(source) = inner {
                            if render_figures {
                                push_display_math_figure(&mut out, &source);
                            } else {
                                push_display_math_source_block(&mut out, &source);
                            }
                        }
                    }
                } else {
                    out.push(Event::Start(Tag::Paragraph));
                    for inner in body {
                        push_math_as_literal(&mut out, inner);
                    }
                    out.push(Event::End(TagEnd::Paragraph));
                }
            }
            other => push_math_as_literal(&mut out, other),
        }
    }
    out
}

/// True when `body` (a buffered paragraph's inner events) holds at least
/// one display formula and nothing but display math, whitespace text, and
/// line breaks — the same shape `collect_display_math_only` recognises in
/// the terminal promotion pass.
fn is_display_math_only(body: &[Event<'_>]) -> bool {
    let mut saw_display = false;
    for ev in body {
        match ev {
            Event::DisplayMath(_) => saw_display = true,
            Event::Text(t) if t.trim().is_empty() => {}
            Event::SoftBreak | Event::HardBreak => {}
            _ => return false,
        }
    }
    saw_display
}

/// Push `event`, converting any math to its literal source text
/// (`$…$` / `$$…$$`) and passing everything else through untouched.
fn push_math_as_literal<'a>(out: &mut Vec<Event<'a>>, event: Event<'a>) {
    match event {
        Event::InlineMath(source) => out.push(Event::Text(literal_math(&source, false))),
        Event::DisplayMath(source) => out.push(Event::Text(literal_math(&source, true))),
        other => out.push(other),
    }
}

/// Emit one display formula as a `<figure class="math-formula">` PNG, or
/// fall back to a fenced `math` code block ([`push_display_math_source_block`])
/// on render failure — the same styled, padded box a non-inlined mermaid
/// diagram gets, not loose `$$…$$` text.
fn push_display_math_figure(out: &mut Vec<Event<'_>>, source: &str) {
    match render_latex_png_data_uri(source) {
        Some(data_uri) => {
            let html = format!(
                "<figure class=\"math-formula\">\
                 <img alt=\"math formula\" src=\"{data_uri}\">\
                 </figure>"
            );
            out.push(Event::Html(CowStr::Boxed(html.into_boxed_str())));
        }
        None => push_display_math_source_block(out, source),
    }
}

/// Emit one display formula as a fenced `math` code block — the styled,
/// padded box mermaid's non-inlined fallback produces (`<pre><code
/// class="language-math">`, painted by the existing code-block rules), with
/// the `$$` delimiters removed: pulldown already strips them from
/// `Event::DisplayMath`, and the one surrounding newline on each side (the
/// `$$` sitting on their own lines) is trimmed the way the in-app
/// figures-off `math` block does.  Used whenever a display formula is *not*
/// rasterized — figures off, or a render failure — so it reads as a
/// formula rather than as source text stranded in a paragraph.
///
/// Emitted as real code-block events, not raw `Event::Html`, so pulldown's
/// writer HTML-escapes the body: no LaTeX can inject markup into the
/// exported file, the same guarantee `literal_math` gives.
fn push_display_math_source_block(out: &mut Vec<Event<'_>>, source: &str) {
    let trimmed = source.strip_prefix('\n').unwrap_or(source);
    let body = trimmed.strip_suffix('\n').unwrap_or(trimmed);
    out.push(Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(
        CowStr::Borrowed("math"),
    ))));
    out.push(Event::Text(CowStr::Boxed(
        body.to_string().into_boxed_str(),
    )));
    out.push(Event::End(TagEnd::CodeBlock));
}

/// The literal source form of a math span — `$source$` (inline) or
/// `$$source$$` (display) — as an owned `CowStr`.  Emitted as an
/// `Event::Text`, so pulldown's writer HTML-escapes it: the delimiters and
/// LaTeX read back verbatim, exactly as the terminal shows un-rendered
/// math.
fn literal_math(source: &str, display: bool) -> CowStr<'static> {
    let delim = if display { "$$" } else { "$" };
    CowStr::Boxed(format!("{delim}{source}{delim}").into_boxed_str())
}

/// Reference cell height (px) the exporter renders display math at.  The
/// in-app raster sizes off the *terminal's* real cell height; the exporter
/// has none, so it passes this instead — larger than the 16 px terminal
/// default so a formula reads at a comfortable display size in the browser
/// (and stays crisp) rather than the cramped ~1-line PNG a 16 px cell gave.
/// `diagram::render_latex_svg` scales the formula from it exactly as the
/// TUI path does, so the export tracks the in-app look, only bigger.
const HTML_EXPORT_MATH_CELL_PX: u16 = 24;

/// Render display-math `source` to a PNG `data:` URI, or `None` on any
/// failure (so the caller falls back to the literal source text).  Glyphs
/// are drawn opaque black for a light document background; the SVG is
/// rasterized to pixels, never inlined.
fn render_latex_png_data_uri(source: &str) -> Option<String> {
    let svg = diagram::render_latex_svg(
        source,
        [0, 0, 0, 255],
        Some((HTML_EXPORT_MATH_CELL_PX, HTML_EXPORT_MATH_CELL_PX)),
    )
    .ok()?;
    svg_to_png_data_uri(&svg)
}

// ── Image inlining ────────────────────────────────────────────────────────

fn rewrite_images_to_data_uris(
    events: &mut [Event<'_>],
    source_dir: &Path,
    approved_outside: &[PathBuf],
) {
    let canon_dir = source_dir.canonicalize().ok();
    for event in events.iter_mut() {
        if let Event::Start(Tag::Image { dest_url, .. }) = event {
            let Some(path) = embeddable_image(dest_url.as_ref(), source_dir, canon_dir.as_deref())
            else {
                continue;
            };
            if !path.inside && !approved_outside.contains(&path.canonical) {
                continue;
            }
            if let Some(new_url) = data_uri(&path.canonical) {
                *dest_url = CowStr::Boxed(new_url.into_boxed_str());
            }
        }
    }
}

/// The local images `markdown` references that a self-contained export would embed from *outside*
/// `source_dir`: through `..`, by absolute path, or via a symlink leading out.  Canonical, deduped,
/// in document order.  The export modal lists these and asks before embedding any of them, since
/// the exported file is typically shared and an out-of-folder image may be one the document's
/// author has no business seeing — see `docs/dev/security-invariants.md`.
pub fn outside_images(markdown: &str, source_dir: &Path) -> Vec<PathBuf> {
    let canon_dir = source_dir.canonicalize().ok();
    let mut out: Vec<PathBuf> = Vec::new();
    for event in Parser::new_ext(markdown, parser_options(markdown)) {
        if let Event::Start(Tag::Image { dest_url, .. }) = event {
            if let Some(path) = embeddable_image(&dest_url, source_dir, canon_dir.as_deref()) {
                if !path.inside && !out.contains(&path.canonical) {
                    out.push(path.canonical);
                }
            }
        }
    }
    out
}

/// A local image an export could embed, resolved.
struct ResolvedImage {
    canonical: PathBuf,
    /// Whether `canonical` lies under the canonicalized `source_dir`.
    inside: bool,
}

/// Resolve an image `url` to an existing local file with an image extension, noting whether it
/// stays inside `source_dir`.  `None` means "never embed": remote URLs, existing `data:` URIs,
/// and anything missing or unclassifiable.  Containment is checked *after* `canonicalize`, so a
/// symlink leading out of the folder counts as outside, exactly like a `..` path.
///
/// `canon_dir` is `source_dir` canonicalized once by the caller; `None` (the folder itself
/// failed to resolve) counts every image as outside, so each one is asked about.
fn embeddable_image(
    url: &str,
    source_dir: &Path,
    canon_dir: Option<&Path>,
) -> Option<ResolvedImage> {
    if is_remote_url(url) {
        return None;
    }
    let canonical = source_dir.join(url).canonicalize().ok()?;
    if !canonical.is_file() {
        return None;
    }
    mime_from_extension(&canonical)?;
    let inside = canon_dir.is_some_and(|dir| canonical.starts_with(dir));
    Some(ResolvedImage { canonical, inside })
}

/// `path`'s bytes as a `data:` URI; `None` if unreadable or of no known image type.
fn data_uri(path: &Path) -> Option<String> {
    let mime = mime_from_extension(path)?;
    let bytes = std::fs::read(path).ok()?;
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

/// Escape the five XML metacharacters.  Only for `<title>`; the body is escaped by
/// `pulldown_cmark::html`.
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

    /// Without the extension the export reproduces the rule-plus-setext-H2 misparse.
    #[test]
    fn frontmatter_is_omitted_from_the_export() {
        let md = "---\ntitle: Foo\ndate: 2026-01-01\n---\n\n# Heading\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("<h1>Heading</h1>"));
        assert!(!html.contains("title: Foo"), "got: {html}");
        assert!(!html.contains("<h2>"), "got: {html}");
    }

    /// The writer emits *nothing* for a metadata block, so an unanchored extension would drop a
    /// section a mid-document `---` pair brackets — silently, and only in the export.
    #[test]
    fn a_mid_document_rule_pair_is_not_dropped_from_the_export() {
        let md = "Intro.\n\n---\n## Section 2\n\nText.\n\n---\n## Section 3\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("Section 2"), "got: {html}");
        assert!(html.contains("Text."), "got: {html}");
        assert!(html.contains("Section 3"), "got: {html}");
    }

    /// The export's gate must be the renderer's, or the two disagree about what frontmatter is.
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
        // The bundled CSS adds the `[ ]` brackets by targeting this exact markup.
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
            approved_outside: Vec::new(),
            title: None,
            render_figures: false,
        };
        let html = render_html(md, &opts).unwrap();
        assert!(
            html.contains("src=\"data:image/png;base64,"),
            "expected base64 data URI, got:\n{html}"
        );
        assert!(!html.contains("src=\"pixel.png\""));
    }

    #[test]
    fn inline_images_leaves_remote_urls_untouched() {
        let md = "![cat](https://example.com/cat.png)";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            inline_images: true,
            source_dir: Some(PathBuf::from("/tmp")),
            approved_outside: Vec::new(),
            title: None,
            render_figures: false,
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
        // A colon after a path segment is not a scheme.
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
        // Holds whether or not the live renderer is available: a success rasterizes to PNG, a
        // failure falls back to an escaped code block.
        let md = "```mermaid\nflowchart TD\n  A[\"<script>alert(1)</script>\"] --> B\n```";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            render_figures: true,
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

    // ── Display math ───────────────────────────────────────────────────

    /// A `$$...$$` paragraph exports as a rasterized `math-formula` figure
    /// (PNG data URI) — the same treatment mermaid gets — when figures are
    /// on.  The KaTeX faces are bundled into the shared fontdb, so this
    /// renders in CI without system fonts.
    #[test]
    fn display_math_exports_as_a_png_figure() {
        let md = "$$\nx^2 + y^2 = z^2\n$$\n";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            render_figures: true,
            ..HtmlExportOptions::default()
        };
        let html = render_html(md, &opts).unwrap();
        assert!(
            html.contains("<figure class=\"math-formula\">"),
            "expected a math-formula figure:\n{html}"
        );
        assert!(
            html.contains("src=\"data:image/png;base64,"),
            "formula must be a rasterized PNG:\n{html}"
        );
        // Rasterized to pixels, never inlined as SVG / math markup.
        assert!(!html.contains("<svg"), "no raw SVG:\n{html}");
        assert!(
            !html.contains("class=\"math math-"),
            "pulldown's math span must not survive:\n{html}"
        );
    }

    /// The exported formula is rasterized at `HTML_EXPORT_MATH_CELL_PX`,
    /// not the bare 16 px terminal-cell fallback, so a display equation
    /// reads at a comfortable size in the browser instead of a cramped
    /// ~1-line PNG.  Guards the export-sizing fix by decoding the figure
    /// and asserting its pixel height clears what a 16 px cell produced.
    #[test]
    fn exported_display_math_is_rendered_large_enough_to_read() {
        use image::GenericImageView;
        let md = "$$\nx^2 + y^2 = z^2\n$$\n";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            render_figures: true,
            ..HtmlExportOptions::default()
        };
        let html = render_html(md, &opts).unwrap();
        let marker = "data:image/png;base64,";
        let start = html.find(marker).expect("png data uri present") + marker.len();
        let end = start + html[start..].find('"').expect("data uri is quoted");
        let bytes = BASE64
            .decode(&html.as_bytes()[start..end])
            .expect("valid base64 payload");
        let (w, h) = image::load_from_memory(&bytes)
            .expect("valid png")
            .dimensions();
        // A single-line display formula at the 24 px reference cell
        // (`HTML_EXPORT_MATH_CELL_PX`) rendered tens of pixels tall —
        // comfortably past the ~18 px a 16 px-cell fallback gave, and
        // nowhere near runaway.
        assert!(
            (28..=160).contains(&h),
            "exported formula height {h}px outside expected range (w={w})"
        );
    }

    /// With figures disabled, a display-math paragraph renders as a
    /// fenced `math` code block — the same styled, padded box mermaid's
    /// non-inlined fallback gets — with the `$$` delimiters removed, never
    /// loose `$$...$$` text in a paragraph and never a bare math span.
    #[test]
    fn display_math_off_renders_as_a_math_code_block() {
        let md = "$$\na + b\n$$\n";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            render_figures: false,
            ..HtmlExportOptions::default()
        };
        let html = render_html(md, &opts).unwrap();
        // Same box as a non-inlined mermaid diagram: a `<pre><code
        // class="language-math">` block, painted by the existing code-block
        // CSS — not a `<figure>`, not a math span, not literal `$$`.
        assert!(
            html.contains("<pre><code class=\"language-math\">"),
            "expected a math code block:\n{html}"
        );
        assert!(html.contains("a + b"), "formula body kept:\n{html}");
        assert!(!html.contains("$$"), "delimiters must be stripped:\n{html}");
        assert!(!html.contains("<figure"), "no figure when off:\n{html}");
        assert!(
            !html.contains("class=\"math math-"),
            "no math span:\n{html}"
        );
    }

    /// A display formula that can't be rasterized (here: over the
    /// `MAX_LATEX_SOURCE_BYTES` cap, so `render_latex_svg` refuses it)
    /// falls back to the same `math` code block, not loose `$$...$$` text —
    /// figures on, but the render fails.
    #[test]
    fn oversized_display_math_falls_back_to_a_code_block() {
        let huge = "1+".repeat(64 * 1024); // well past MAX_LATEX_SOURCE_BYTES
        let md = format!("$$\n{huge}1\n$$\n");
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            render_figures: true,
            ..HtmlExportOptions::default()
        };
        let html = render_html(&md, &opts).unwrap();
        assert!(
            html.contains("<pre><code class=\"language-math\">"),
            "render failure must fall back to a math code block, not a figure or literal text"
        );
        assert!(
            !html.contains("data:image/png"),
            "no PNG when the render failed:\n{}",
            &html[..html.len().min(400)]
        );
    }

    /// Inline `$...$` math always stays literal source (delimiters kept),
    /// matching the terminal preview — regardless of the figures toggle.
    #[test]
    fn inline_math_stays_literal_source() {
        let md = "Solve $a^2 + b^2$ please.\n";
        for render_figures in [true, false] {
            let opts = HtmlExportOptions {
                stylesheet: Stylesheet::Inline(String::new()),
                render_figures,
                ..HtmlExportOptions::default()
            };
            let html = render_html(md, &opts).unwrap();
            assert!(
                html.contains("$a^2 + b^2$"),
                "inline math must read back as literal source (figures={render_figures}):\n{html}"
            );
            assert!(
                !html.contains("class=\"math math-"),
                "no math span (figures={render_figures}):\n{html}"
            );
        }
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

    fn inline_opts(source: &Path, approved_outside: Vec<PathBuf>) -> HtmlExportOptions {
        HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            inline_images: true,
            source_dir: Some(source.to_path_buf()),
            approved_outside,
            render_figures: false,
            ..HtmlExportOptions::default()
        }
    }

    /// A shared root holding `secret.png` beside a `docs/` source folder.
    fn root_with_docs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempdir().unwrap();
        let secret = root.path().join("secret.png");
        write_one_px_png(&secret);
        let source = root.path().join("docs");
        std::fs::create_dir(&source).unwrap();
        (root, secret.canonicalize().unwrap(), source)
    }

    #[test]
    fn inline_images_skips_an_unapproved_absolute_path() {
        let (_root, secret, source) = root_with_docs();
        let md = format!("![x]({})", secret.display());
        let html = render_html(&md, &inline_opts(&source, Vec::new())).unwrap();
        assert!(
            !html.contains("data:image/png"),
            "an unapproved out-of-folder path must not be inlined:\n{html}"
        );
    }

    #[test]
    fn inline_images_skips_unapproved_parent_traversal() {
        let (_root, _secret, source) = root_with_docs();
        let html = render_html("![x](../secret.png)", &inline_opts(&source, Vec::new())).unwrap();
        assert!(
            !html.contains("data:image/png"),
            "unapproved ../ traversal must not be inlined:\n{html}"
        );
        assert!(
            html.contains("src=\"../secret.png\""),
            "left as a link:\n{html}"
        );
    }

    #[test]
    fn inline_images_embeds_an_approved_outside_image() {
        let (_root, secret, source) = root_with_docs();
        let opts = inline_opts(&source, vec![secret]);
        let html = render_html("![x](../secret.png)", &opts).unwrap();
        assert!(html.contains("src=\"data:image/png;base64,"), "{html}");
    }

    /// Approval covers the listed file only, not everything outside the folder.
    #[test]
    fn approval_does_not_extend_to_an_unlisted_outside_image() {
        let (root, secret, source) = root_with_docs();
        write_one_px_png(&root.path().join("other.png"));
        let md = "![a](../secret.png) ![b](../other.png)";
        let html = render_html(md, &inline_opts(&source, vec![secret])).unwrap();
        assert_eq!(html.matches("data:image/png").count(), 1, "{html}");
        assert!(html.contains("src=\"../other.png\""), "{html}");
    }

    #[test]
    fn outside_images_lists_parent_and_absolute_references_once() {
        let (_root, secret, source) = root_with_docs();
        write_one_px_png(&source.join("inside.png"));
        let md = format!(
            "![a](../secret.png) ![b]({}) ![c](inside.png) ![d](../docs/inside.png) \
             ![e](../missing.png) ![f](https://example.com/x.png)",
            secret.display()
        );
        assert_eq!(outside_images(&md, &source), vec![secret]);
    }

    #[test]
    fn outside_images_ignores_non_image_files() {
        let root = tempdir().unwrap();
        std::fs::write(root.path().join("notes.txt"), "private").unwrap();
        let source = root.path().join("docs");
        std::fs::create_dir(&source).unwrap();
        assert!(outside_images("![x](../notes.txt)", &source).is_empty());
    }

    /// A symlink inside the folder that leads out counts as outside, so it is listed for approval
    /// rather than embedded silently.
    #[cfg(unix)]
    #[test]
    fn outside_images_sees_through_a_symlink_leading_out() {
        let (_root, secret, source) = root_with_docs();
        std::os::unix::fs::symlink(&secret, source.join("link.png")).unwrap();
        assert_eq!(outside_images("![x](link.png)", &source), vec![secret]);
        let html = render_html("![x](link.png)", &inline_opts(&source, Vec::new())).unwrap();
        assert!(!html.contains("data:image/png"), "{html}");
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
