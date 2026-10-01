use std::borrow::Cow;
use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use pulldown_cmark::{
    html as cmark_html, CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd,
};

use super::runner::{write_atomically, ExportOutcome};
use crate::diagram;
use crate::document::parsed_doc::{gfm_slug, uniquify_slug};
use crate::image::normalize_svg;
use crate::markdown::parser::post_pass::is_html_comment_only;

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
    /// Render *figures* — fenced ```mermaid code blocks and `$$...$$` display math — to SVG
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
/// **The serialized body passes through [`sanitize_body`] before anything edamame generated is
/// added to it.**  Raw HTML in the document survives only as far as the allowlist lets it, so
/// attacker-controlled Markdown cannot inject `<script>`, event handlers, or a code-running link.
/// Figures are rendered as placeholders and swapped in afterwards ([`Figures`]), so the sanitizer
/// never sees — and can never be asked to permit — the markup edamame writes itself.
pub fn render_html(markdown: &str, opts: &HtmlExportOptions) -> Result<String> {
    // Collected so the rewrite passes can mutate events in place.
    let mut events: Vec<Event> = Parser::new_ext(markdown, parser_options(markdown)).collect();

    if opts.inline_images {
        if let Some(dir) = opts.source_dir.as_deref() {
            rewrite_images_to_data_uris(&mut events, dir, &opts.approved_outside);
        }
    }

    // Before `replace_math`, which turns heading math into plain text: the slug reads the
    // `InlineMath` events the in-app parser sees.
    assign_heading_ids(&mut events);

    let mut figures = Figures::new();
    if opts.render_figures {
        events = replace_mermaid_with_figure(events, &mut figures);
    }
    // Always run, so inline `$…$` and (with figures off) display math
    // collapse to literal source rather than a bare `<span class="math">`.
    events = replace_math(events, opts.render_figures, &mut figures);

    let mut body = String::new();
    cmark_html::push_html(&mut body, events.into_iter());
    let body = figures.insert_into(&sanitize_body(&body));

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
    // Recognize math so `$$…$$` reaches `replace_math` as `Event::DisplayMath` (rendered to a
    // figure) rather than surviving as literal text.  `replace_math` always runs when this is on —
    // even with figures disabled — so inline `$…$` and un-rendered display math collapse back to
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

// ── Heading anchors ───────────────────────────────────────────────────────

/// Give every heading the `id` a `[x](#fragment)` link names, so in-document links work in the
/// export.  pulldown-cmark's writer emits an `id` only when the event carries one, and without
/// `ENABLE_HEADING_ATTRIBUTES` none does — every heading came out bare and every `#anchor` link
/// went nowhere.
///
/// The slug is the one `ParsedDoc::heading_anchors` keys the in-app jump on — [`gfm_slug`] over
/// the same plain text `inlines_to_plain` builds, deduplicated by [`uniquify_slug`] — so a
/// fragment that resolves in edamame resolves in the export.  Unlike the in-app table, which
/// indexes top-level blocks only, a heading nested in a list or quote gets an id too, as on
/// GitHub.  A nested heading also takes its slug's count, so where it shares text with a later
/// top-level heading the fragment can name a different heading in each: `- # Intro` then
/// `# Intro` exports as `intro` / `intro-1`, while edamame jumps `#intro` to the top-level one.
fn assign_heading_ids(events: &mut [Event<'_>]) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut i = 0;
    while i < events.len() {
        if !matches!(events[i], Event::Start(Tag::Heading { .. })) {
            i += 1;
            continue;
        }
        let mut text = String::new();
        let mut j = i + 1;
        while let Some(event) = events.get(j) {
            match event {
                Event::End(TagEnd::Heading(_)) => break,
                Event::Text(t) | Event::Code(t) => text.push_str(t),
                Event::InlineMath(m) => text.push_str(&literal_math(m, false)),
                Event::DisplayMath(m) => text.push_str(&literal_math(m, true)),
                Event::SoftBreak => text.push(' '),
                Event::HardBreak => text.push('\n'),
                // The in-app parser keeps non-comment inline HTML as text.
                Event::InlineHtml(h) if !is_html_comment_only(h) => text.push_str(h),
                _ => {}
            }
            j += 1;
        }
        let base = gfm_slug(&text);
        if !base.is_empty() {
            let slug = uniquify_slug(&base, &mut counts);
            if let Event::Start(Tag::Heading { id, .. }) = &mut events[i] {
                *id = Some(CowStr::Boxed(slug.into_boxed_str()));
            }
        }
        i = j + 1;
    }
}

// ── Sanitization ──────────────────────────────────────────────────────────

/// URL schemes that run code when a browser follows or loads them.  The list is closed: these
/// three are the only schemes a browser executes.  `data:` is additionally allowed on `img src`,
/// where it can only ever be an image (see [`sanitize_body`]).
const BLOCKED_URL_SCHEMES: &[&str] = &["javascript", "vbscript", "data"];

/// Clean the serialized body with `ammonia`'s allowlist, extended for what Markdown and README
/// HTML produce.
///
/// * **Tags and attributes.**  ammonia's defaults (`details`/`summary`, `kbd`, `sub`/`sup`,
///   `img` with `width`/`height`, tables, …) plus: `id`, `class` and `align` anywhere (heading
///   anchors, footnotes, code-block languages, `<p align="center">`); `name` on `a`; `open` on
///   `details`; a *checkbox* `input` for task lists; and `style` on `th`/`td`, filtered down to
///   `text-align`, for table column alignment.  None of these can run anything: the export
///   carries no script for `id`/`class` to steer, and every other CSS property — the ones that
///   fetch (`url()`) or overlay the page — is dropped.  `<script>` and `<style>` are removed
///   with their contents; comments are dropped, and any other unlisted tag is removed with its
///   content kept.
/// * **URL schemes — a denylist expressed as an allowlist.**  ammonia only takes an allowlist,
///   and a fixed one would break every app link (`obsidian:`, `zotero:`, `vscode:`, `file:`)
///   it didn't anticipate.  So the list handed to it is every scheme-shaped token in the body
///   ([`url_schemes_in`]) minus [`BLOCKED_URL_SCHEMES`].  The safety argument rests only on the
///   subtraction: ammonia parses each URL as a browser would (stripping tab/newline, decoding
///   entities, lowercasing the scheme), and a parsed `javascript` is never in the set however
///   the source spelled it.  A link whose scheme is refused loses its `href` and stays as text.
/// * **`data:` on `img src` only.**  `data` is in the allowlist for the self-contained export's
///   embedded images, and the attribute filter strips it from every other URL attribute.  Inside
///   `<img>` even `data:image/svg+xml` is inert: a browser runs no script and loads no resource
///   in an SVG used as an image.
fn sanitize_body(html: &str) -> String {
    let schemes = url_schemes_in(html);
    let mut builder = ammonia::Builder::default();
    builder
        .url_schemes(schemes.iter().map(String::as_str).collect())
        .add_url_schemes(["data"])
        // ammonia's default adds `rel="noopener noreferrer"` to every `<a>`; that guards
        // `target="_blank"`, which isn't allowed here, and would clutter every footnote link.
        .link_rel(None)
        .add_generic_attributes(["id", "class", "align"])
        .add_tag_attributes("a", ["name"])
        .add_tag_attributes("details", ["open"])
        .add_tags(["input"])
        .add_tag_attributes("input", ["checked", "disabled"])
        .add_tag_attribute_values("input", "type", ["checkbox"])
        .add_tag_attributes("th", ["style"])
        .add_tag_attributes("td", ["style"])
        .filter_style_properties(HashSet::from(["text-align"]))
        .attribute_filter(|element, attribute, value| {
            let is_url_attribute = matches!(attribute, "href" | "src" | "xlink:href" | "cite");
            if is_url_attribute && has_data_scheme(value) && (element, attribute) != ("img", "src")
            {
                None
            } else {
                Some(Cow::Borrowed(value))
            }
        });
    builder.clean(html).to_string()
}

/// Every lowercased, scheme-shaped token (`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`) followed
/// by a `:` anywhere in `html`, minus [`BLOCKED_URL_SCHEMES`].  Deliberately over-inclusive —
/// words before a colon in prose land here too — because extra entries only widen what
/// [`sanitize_body`] *permits*, and the blocked schemes are the safety boundary.
fn url_schemes_in(html: &str) -> HashSet<String> {
    let is_scheme_char = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.');
    let bytes = html.as_bytes();
    let mut schemes = HashSet::new();
    for (colon, _) in html.match_indices(':') {
        let start = bytes[..colon]
            .iter()
            .rposition(|&b| !is_scheme_char(b))
            .map_or(0, |i| i + 1);
        // Leading digits / `+-.` are not part of a scheme: skip to the first letter.
        let Some(first_alpha) = bytes[start..colon].iter().position(u8::is_ascii_alphabetic) else {
            continue;
        };
        let scheme = html[start + first_alpha..colon].to_ascii_lowercase();
        if !BLOCKED_URL_SCHEMES.contains(&scheme.as_str()) {
            schemes.insert(scheme);
        }
    }
    schemes
}

/// Whether a URL attribute value is a `data:` URL as a browser parses it: ASCII tab and newline
/// removed anywhere, leading C0 controls and spaces trimmed, scheme case-insensitive.
fn has_data_scheme(value: &str) -> bool {
    let normalized: String = value
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .take(5)
        .collect();
    normalized.eq_ignore_ascii_case("data:")
}

// ── Figures ───────────────────────────────────────────────────────────────

/// Figure markup edamame generates, held out of the body until it has been sanitized.
///
/// Each figure enters the event stream as a placeholder element,
/// `<div class="edamame-figure-{nonce}-{n}"></div>`, which [`sanitize_body`] serializes back
/// verbatim, and [`insert_into`](Self::insert_into) swaps the markup in afterwards.  Two properties keep the swap in element context:
///
/// * **The nonce is random per export**, so a document cannot spell a placeholder itself.
/// * **The placeholder contains `"` and `<`.**  Where the real one lands outside element content —
///   swallowed by a document's unclosed `<p title="`, say — the sanitizer escapes or drops it
///   (`&quot;` in an attribute value, `&lt;` in text), so it no longer matches and the figure is
///   left out rather than spliced into an attribute.
struct Figures {
    /// `<div class="edamame-figure-{nonce}-`; the index and [`PLACEHOLDER_CLOSE`] follow.
    open: String,
    html: Vec<String>,
}

/// The tail of a [`Figures`] placeholder after its index.
const PLACEHOLDER_CLOSE: &str = "\"></div>";

impl Figures {
    fn new() -> Self {
        // `RandomState` is seeded from the OS RNG; two hashes give a 128-bit nonce with no
        // extra dependency.  It only has to be unguessable to a document written in advance.
        let nonce = (
            RandomState::new().hash_one(0u8),
            RandomState::new().hash_one(1u8),
        );
        Self {
            open: format!(
                "<div class=\"edamame-figure-{:016x}{:016x}-",
                nonce.0, nonce.1
            ),
            html: Vec::new(),
        }
    }

    /// Hold `html` back and return the placeholder event standing in for it.
    fn placeholder(&mut self, html: String) -> Event<'static> {
        let token = format!("{}{}{PLACEHOLDER_CLOSE}", self.open, self.html.len());
        self.html.push(html);
        Event::Html(CowStr::Boxed(token.into_boxed_str()))
    }

    /// Replace each placeholder in the sanitized `body` with its figure.
    fn insert_into(&self, body: &str) -> String {
        if self.html.is_empty() {
            return body.to_owned();
        }
        let mut out =
            String::with_capacity(body.len() + self.html.iter().map(String::len).sum::<usize>());
        let mut rest = body;
        while let Some(pos) = rest.find(&self.open) {
            out.push_str(&rest[..pos]);
            let after = &rest[pos + self.open.len()..];
            let digits = after.bytes().take_while(u8::is_ascii_digit).count();
            let figure = after[..digits]
                .parse::<usize>()
                .ok()
                .filter(|_| after[digits..].starts_with(PLACEHOLDER_CLOSE))
                .and_then(|i| self.html.get(i));
            match figure {
                Some(html) => {
                    out.push_str(html);
                    rest = &after[digits + PLACEHOLDER_CLOSE.len()..];
                }
                None => {
                    out.push_str(&self.open);
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }
}

/// `<figure class="{class}">` around an `<img>` whose source is `svg`, normalized and embedded as
/// a `data:image/svg+xml` URI, or `None` if usvg rejects it.
///
/// **The SVG goes in as an `<img>`, never inline.**  Inline `<svg>` can carry `<script>`,
/// `foreignObject`, and `on*=` handlers; an SVG loaded as an image runs no script and fetches
/// nothing, by browser design.  [`normalize_svg`] is the second layer — it re-serializes through
/// usvg's tree, which cannot express any of that — and it converts text to paths, without which
/// a formula's KaTeX glyphs would not display in a browser at all.
fn svg_figure(svg: &str, class: &str, alt: &str) -> Option<String> {
    let svg = normalize_svg(svg).ok()?;
    Some(format!(
        "<figure class=\"{class}\"><img alt=\"{alt}\" src=\"data:image/svg+xml;base64,{}\"></figure>",
        BASE64.encode(svg)
    ))
}

// ── Mermaid diagrams ──────────────────────────────────────────────────────

/// Replace each mermaid fence with a [`Figures`] placeholder for its [`svg_figure`], preserving the
/// original events on render failure so the diagram source is never lost.
///
/// Language matching is case-insensitive, like the in-app `promote_diagram_code_blocks`.
fn replace_mermaid_with_figure<'a>(
    events: Vec<Event<'a>>,
    figures: &mut Figures,
) -> Vec<Event<'a>> {
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
        // Collect Text events to the matching end, then either emit a placeholder or replay
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
        match render_mermaid_figure(&source) {
            Some(html) => out.push(figures.placeholder(html)),
            None => {
                // Fall back to the code block; a per-diagram failure is not fatal to the export.
                out.extend(buffered);
            }
        }
    }
    out
}

/// Render mermaid `source` to a `<figure class="mermaid-diagram">`, or `None` on any failure.
fn render_mermaid_figure(source: &str) -> Option<String> {
    let svg = diagram::render_mermaid_svg(source).ok()?;
    svg_figure(&svg, "mermaid-diagram", "mermaid diagram")
}

// ── Display math ──────────────────────────────────────────────────────────

/// Rewrite math events in the stream, mirroring the terminal's promotion
/// rules (`markdown::parser::post_pass::promote_display_math_paragraphs`):
///
/// * A paragraph whose body is **only** display math (one or more
///   `$$…$$`, plus whitespace and breaks) is a *figure* paragraph: the
///   enclosing `<p>` is dropped (a block-level figure/code block can't nest
///   in `<p>`) and each formula becomes its own block.  With figures on it
///   becomes an SVG `<figure class="math-formula">`; with figures off,
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
/// The figure goes through [`svg_figure`], like mermaid's, so RaTeX's
/// output reaches the export only as a normalized SVG inside an `<img>`.
fn replace_math<'a>(
    events: Vec<Event<'a>>,
    render_figures: bool,
    figures: &mut Figures,
) -> Vec<Event<'a>> {
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
                    // and emit one block per formula — an SVG
                    // `<figure>` when figures are on and the render
                    // succeeds, otherwise a fenced `math` code block (the
                    // export peer of the in-app figures-off `math` block,
                    // and the parallel of mermaid's non-inlined code-block
                    // fallback).  Whitespace text and breaks were only
                    // separators between formulas — drop them with the `<p>`.
                    for inner in body {
                        if let Event::DisplayMath(source) = inner {
                            if render_figures {
                                push_display_math_figure(&mut out, &source, figures);
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

/// Emit one display formula as a [`Figures`] placeholder for a `<figure class="math-formula">`,
/// or fall back to a fenced `math` code block ([`push_display_math_source_block`]) on render
/// failure — the same styled, padded box a non-inlined mermaid diagram gets, not loose `$$…$$`
/// text.
fn push_display_math_figure(out: &mut Vec<Event<'_>>, source: &str, figures: &mut Figures) {
    match render_math_figure(source) {
        Some(html) => out.push(figures.placeholder(html)),
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
/// rendered — figures off, or a render failure — so it reads as a
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
/// rather than at the cramped ~1-line size a 16 px cell gives.  It sets the
/// SVG's intrinsic size, which is the size the browser shows it at.
/// `diagram::render_latex_svg` scales the formula from it exactly as the
/// TUI path does, so the export tracks the in-app look, only bigger.
const HTML_EXPORT_MATH_CELL_PX: u16 = 24;

/// Render display-math `source` to a `<figure class="math-formula">`, or
/// `None` on any failure (so the caller falls back to a code block).
/// Glyphs are drawn opaque black for a light document background.
fn render_math_figure(source: &str) -> Option<String> {
    let svg = diagram::render_latex_svg(
        source,
        [0, 0, 0, 255],
        Some((HTML_EXPORT_MATH_CELL_PX, HTML_EXPORT_MATH_CELL_PX)),
    )
    .ok()?;
    svg_figure(&svg, "math-formula", "math formula")
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
        assert!(html.contains("<h1 id=\"hello\">Hello</h1>"));
        assert!(html.contains("<p>World</p>"));
    }

    /// Without the extension the export reproduces the rule-plus-setext-H2 misparse.
    #[test]
    fn frontmatter_is_omitted_from_the_export() {
        let md = "---\ntitle: Foo\ndate: 2026-01-01\n---\n\n# Heading\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("<h1 id=\"heading\">Heading</h1>"));
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
            !html.contains("<script>") && !html.contains("alert"),
            "raw <script> must be stripped with its contents — got:\n{html}"
        );
        assert!(html.contains("more"));
    }

    #[test]
    fn strips_raw_html_inline() {
        let md = "a <b onclick=\"x\">inline</b> c";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            !html.contains("onclick"),
            "inline HTML event handlers must be stripped — got:\n{html}"
        );
        assert!(
            html.contains("<b>inline</b>"),
            "the tag itself is allowed:\n{html}"
        );
    }

    /// The raw HTML READMEs lean on survives the sanitizer.
    #[test]
    fn keeps_readme_html() {
        let md = "<p align=\"center\"><img src=\"logo.png\" width=\"120\" alt=\"logo\"></p>\n\n\
                  <details open><summary>More</summary>\n\nHidden *text*.\n\n</details>\n\n\
                  Press <kbd>Ctrl</kbd>+<kbd>S</kbd>, H<sub>2</sub>O, x<sup>2</sup>,<br>next line.\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        for needle in [
            "<p align=\"center\">",
            "width=\"120\"",
            "src=\"logo.png\"",
            "<details open=\"\">",
            "<summary>More</summary>",
            "<em>text</em>",
            "<kbd>Ctrl</kbd>",
            "<sub>2</sub>",
            "<sup>2</sup>",
            "<br>",
        ] {
            assert!(html.contains(needle), "missing {needle}:\n{html}");
        }
    }

    /// `<style>` is dropped with its rules, and inline styles are dropped except the
    /// `text-align` pulldown writes for table column alignment.
    #[test]
    fn strips_css_except_table_alignment() {
        let md = "<style>body { background: url(https://t.example/x) }</style>\n\n\
                  <p style=\"position: fixed; background: url(https://t.example/y)\">p</p>\n\n\
                  | a | b |\n|:-:|--:|\n| 1 | 2 |\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            !html.contains("t.example"),
            "no CSS fetch may survive:\n{html}"
        );
        assert!(!html.contains("position"), "{html}");
        assert!(html.contains("text-align:center"), "{html}");
        assert!(html.contains("text-align:right"), "{html}");
    }

    #[test]
    fn task_list_checkboxes_survive() {
        let md = "- [x] done\n- [ ] todo\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert_eq!(html.matches("type=\"checkbox\"").count(), 2, "{html}");
        assert!(html.contains("checked"), "{html}");
        assert!(html.contains("disabled"), "{html}");
    }

    #[test]
    fn footnotes_keep_their_anchors() {
        let md = "Claim.[^1]\n\n[^1]: Source.\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("class=\"footnote-reference\""), "{html}");
        assert!(html.contains("href=\"#1\""), "{html}");
        assert!(html.contains("id=\"1\""), "{html}");
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
        // The link loses its target, not its text.
        assert!(html.contains(">click</a>"), "{html}");
        assert!(!html.contains("href"), "{html}");
    }

    /// Every spelling a browser would still parse as a code-running scheme is refused.
    #[test]
    fn neutralizes_obfuscated_script_schemes() {
        let md = "[a](JaVaScRiPt:alert(1)) <a href=\"java&#9;script:alert(1)\">b</a> [c](vbscript:msgbox) \
                  [d](&#106;avascript:alert(1)) \
                  <a href=\"&#x6A;avascript:alert(1)\">e</a> <a href=\" javascript:alert(1)\">f</a> \
                  <a href=\"java&#10;script:alert(1)\">g</a>";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(!html.contains("href"), "no href may survive:\n{html}");
        for text in ["a", "b", "c", "d", "e", "f", "g"] {
            assert!(
                html.contains(&format!(">{text}</a>")),
                "{text} lost:\n{html}"
            );
        }
    }

    #[test]
    fn neutralizes_data_html_link_scheme() {
        let md = "[x](data:text/html;base64,PHNjcmlwdD4=) \
                  <a href=\"data:text/html,<script>alert(1)</script>\">y</a> \
                  <a href=\"data:image/svg+xml,x\">z</a>";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            !html.contains("data:"),
            "data: link must be neutralized:\n{html}"
        );
    }

    /// `data:` survives on `img src`, where a browser can only treat it as an image.
    #[test]
    fn keeps_data_image_sources() {
        let md = "<img src=\"data:image/png;base64,iVBORw0KGgo=\" alt=\"a\">";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(
            html.contains("src=\"data:image/png;base64,iVBORw0KGgo=\""),
            "{html}"
        );
    }

    #[test]
    fn preserves_link_schemes_and_relative_targets() {
        let md = "[a](https://example.com) [b](mailto:x@y.z) [c](./page.md) [d](#anchor) \
                  [e](foo/bar:baz) [f](file:///home/me/notes.md) [g](obsidian://open?vault=v) \
                  [h](vscode://file/src/main.rs) [i](zotero://select/items/ABC) \
                  <a href=\"x-devonthink-item://123\">j</a>";
        let html = render_html(md, &opts_inline_css()).unwrap();
        for href in [
            "https://example.com",
            "mailto:x@y.z",
            "./page.md",
            "#anchor",
            // A colon after a path segment is not a scheme.
            "foo/bar:baz",
            "file:///home/me/notes.md",
            "obsidian://open?vault=v",
            "vscode://file/src/main.rs",
            "zotero://select/items/ABC",
            "x-devonthink-item://123",
        ] {
            assert!(
                html.contains(&format!("href=\"{href}\"")),
                "{href} lost:\n{html}"
            );
        }
    }

    #[test]
    fn url_schemes_in_collects_scheme_tokens_minus_the_blocked_ones() {
        let found = url_schemes_in(
            "<a href=\"Obsidian://x\">a</a> 12:30 x-dt.item+1:y JavaScript: vbscript: data: 9file:z",
        );
        for scheme in ["obsidian", "x-dt.item+1", "file"] {
            assert!(found.contains(scheme), "{scheme} missing from {found:?}");
        }
        for scheme in BLOCKED_URL_SCHEMES {
            assert!(!found.contains(*scheme), "{scheme} must never be allowed");
        }
        assert!(
            !found.contains("12"),
            "a scheme starts with a letter: {found:?}"
        );
    }

    #[test]
    fn has_data_scheme_reads_urls_as_a_browser_does() {
        assert!(has_data_scheme("data:text/html,x"));
        assert!(has_data_scheme("  DATA:text/html,x"));
        assert!(has_data_scheme("\u{1}da\tta:x"));
        assert!(has_data_scheme("d\na\rta:x"));
        assert!(!has_data_scheme("dat"));
        assert!(!has_data_scheme("./data:x"));
        assert!(!has_data_scheme("https://data:x"));
    }

    // ── Heading anchors ────────────────────────────────────────────────

    /// The bug this fixed: headings exported without an `id`, so every `#anchor` link was dead.
    #[test]
    fn headings_carry_the_gfm_slug_an_anchor_link_names() {
        let md = "[Go](#getting-started)\n\n## Getting Started\n\n## Getting Started\n\n\
                  ### The `--doctor` *flag*\n\n# !!!\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        assert!(html.contains("href=\"#getting-started\""), "{html}");
        assert!(html.contains("<h2 id=\"getting-started\">"), "{html}");
        assert!(html.contains("<h2 id=\"getting-started-1\">"), "{html}");
        assert!(html.contains("<h3 id=\"the---doctor-flag\">"), "{html}");
        // A heading with nothing to slug gets no id rather than an empty one.
        assert!(html.contains("<h1>!!!</h1>"), "{html}");
    }

    /// The export's slugs are the in-app jump table's, so a fragment that works in edamame
    /// works in the exported file.
    #[test]
    fn heading_ids_match_the_in_app_anchor_table() {
        let md =
            "# Intro\n\n## Math $x^2$ and <kbd>K</kbd>\n\n## Intro\n\n## Note[^1]\n\n[^1]: n\n";
        let html = render_html(md, &opts_inline_css()).unwrap();
        let theme = crate::config::Theme::default();
        let parsed = crate::document::ParsedDoc::build(md, &theme, false, 4);
        for slug in parsed.heading_anchors.keys() {
            assert!(
                html.contains(&format!("id=\"{slug}\"")),
                "{slug} missing:\n{html}"
            );
        }
        assert_eq!(parsed.heading_anchors.len(), 4);
    }

    // ── Figures ────────────────────────────────────────────────────────

    /// A document cannot forge a figure placeholder: the nonce differs per export.
    #[test]
    fn figure_placeholders_cannot_be_forged() {
        let mut figures = Figures::new();
        let Event::Html(token) = figures.placeholder("<figure>F</figure>".into()) else {
            panic!("placeholder is raw HTML");
        };
        let forged = format!("{}0{PLACEHOLDER_CLOSE}", Figures::new().open);
        let body = format!("{forged}{token}");
        assert_eq!(
            figures.insert_into(&body),
            format!("{forged}<figure>F</figure>")
        );
    }

    /// The real placeholder, swallowed by an unclosed attribute in the document, is dropped
    /// rather than spliced into the attribute value (where the figure's `"` would break out).
    #[test]
    fn figure_placeholders_stay_out_of_attribute_values() {
        let mut figures = Figures::new();
        let Event::Html(token) = figures.placeholder("<figure>F</figure>".into()) else {
            panic!("placeholder is raw HTML");
        };
        let body = format!("<div title=\"\n{token}\n<p class=\"a\">after</p>");
        let html = figures.insert_into(&sanitize_body(&body));
        assert!(!html.contains("<figure>"), "{html}");
    }

    #[test]
    fn figure_placeholders_survive_sanitization() {
        let mut figures = Figures::new();
        let Event::Html(token) = figures.placeholder("<figure>F</figure>".into()) else {
            panic!("placeholder is raw HTML");
        };
        let body = format!("<ul><li>a\n{token}</li></ul>");
        assert_eq!(
            figures.insert_into(&sanitize_body(&body)),
            "<ul><li>a\n<figure>F</figure></li></ul>"
        );
    }

    /// The `data:image/svg+xml` payload of the first figure in `html`, decoded.
    fn first_figure_svg(html: &str) -> Option<String> {
        let marker = "src=\"data:image/svg+xml;base64,";
        let start = html.find(marker)? + marker.len();
        let end = start + html[start..].find('"')?;
        let bytes = BASE64.decode(&html.as_bytes()[start..end]).ok()?;
        String::from_utf8(bytes).ok()
    }

    // ── Vuln 3: mermaid export carries no raw SVG / script ─────────────

    #[test]
    fn mermaid_export_never_emits_raw_svg_or_script() {
        // Holds whether or not the live renderer is available: a success embeds a normalized SVG
        // as an `<img>`, a failure falls back to an escaped code block.
        let md = "```mermaid\nflowchart TD\n  A[\"<script>alert(1)</script>\"] --> B\n```";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            render_figures: true,
            ..HtmlExportOptions::default()
        };
        let html = render_html(md, &opts).unwrap();
        assert!(
            !html.contains("<svg"),
            "no inline SVG may reach the export:\n{html}"
        );
        assert!(!html.contains("foreignObject"));
        assert!(
            !html.contains("<script>"),
            "no executable <script> may reach the export:\n{html}"
        );
        if let Some(svg) = first_figure_svg(&html) {
            assert!(!svg.contains("<script"), "{svg}");
            assert!(!svg.contains("foreignObject"), "{svg}");
            assert!(!svg.contains("<text"), "text must be outlined:\n{svg}");
        }
    }

    // ── Display math ───────────────────────────────────────────────────

    /// A `$$...$$` paragraph exports as a `math-formula` figure — an SVG `<img>`, the same
    /// treatment mermaid gets — when figures are on.  The KaTeX faces are bundled into the shared
    /// fontdb, so this renders in CI without system fonts.
    #[test]
    fn display_math_exports_as_an_svg_figure() {
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
        let svg = first_figure_svg(&html).expect("formula is an SVG data URI");
        // Glyphs are outlined: a browser has no KaTeX fonts to draw `<text>` with.
        assert!(!svg.contains("<text"), "text must be outlined:\n{svg}");
        assert!(svg.contains("<path"), "{svg}");
        // Embedded as an image, never inlined as SVG / math markup.
        assert!(!html.contains("<svg"), "no inline SVG:\n{html}");
        assert!(
            !html.contains("class=\"math math-"),
            "pulldown's math span must not survive:\n{html}"
        );
    }

    /// The exported formula is sized from `HTML_EXPORT_MATH_CELL_PX`, not the bare 16 px
    /// terminal-cell fallback, so a display equation reads at a comfortable size in the browser
    /// instead of a cramped ~1-line one.  The SVG's intrinsic height is what the browser shows.
    #[test]
    fn exported_display_math_is_rendered_large_enough_to_read() {
        let md = "$$\nx^2 + y^2 = z^2\n$$\n";
        let opts = HtmlExportOptions {
            stylesheet: Stylesheet::Inline(String::new()),
            render_figures: true,
            ..HtmlExportOptions::default()
        };
        let html = render_html(md, &opts).unwrap();
        let svg = first_figure_svg(&html).expect("formula is an SVG data URI");
        let attr = "height=\"";
        let start = svg.find(attr).expect("svg height") + attr.len();
        let h: f32 = svg[start..start + svg[start..].find('"').unwrap()]
            .parse()
            .expect("numeric height");
        // A single-line display formula at the 24 px reference cell
        // (`HTML_EXPORT_MATH_CELL_PX`) is tens of pixels tall —
        // comfortably past the ~18 px a 16 px-cell fallback gave, and
        // nowhere near runaway.
        assert!(
            (28.0..=160.0).contains(&h),
            "exported formula height {h}px out of range"
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

    /// A display formula that can't be rendered (here: over the
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
        assert!(written.contains("<h1 id=\"hi\">hi</h1>"));
    }
}
