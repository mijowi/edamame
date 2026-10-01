//! What an export does with each `<img src>`: leave it, embed it, or remove it.
//!
//! Resolution runs as the sanitizer's `img src` hook ([`super::sanitize_body`]), on the
//! *serialized* body, so a Markdown image and a raw-HTML `<img>` take exactly one path.  The value
//! it sees is the attribute as a browser reads it — entities decoded, but percent-escapes intact
//! (pulldown's writer escapes a Markdown destination), so a local path is percent-decoded before
//! it touches the filesystem, the way a browser resolving a `file:` URL would.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use percent_encoding::percent_decode_str;

use super::has_data_scheme;
use crate::image::{fetch_remote, normalize_svg};

/// Largest local file an export reads to embed, matching the image loader's own cap: without it
/// one reference to a multi-gigabyte file is read whole into memory.
const MAX_EMBED_BYTES: u64 = 64 * 1024 * 1024;

/// How an export treats image sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageHandling {
    /// Leave every source as written, for the browser to resolve wherever the file is opened.
    #[default]
    Link,
    /// Embed local images inside the document's folder, plus out-of-folder ones the user
    /// approved; leave everything else as written.  The self-contained HTML export.
    Embed,
    /// Leave nothing for the file's consumer to resolve: every image is embedded or removed.
    ///
    /// For a custom export, whose converter (weasyprint, pandoc, …) reads every `<img src>`
    /// *itself* — local paths, `file:` URLs, and remote URLs alike — and writes what it finds
    /// into the output.  Left alone, it would embed an out-of-folder image nobody approved, and
    /// fetch remote ones without consent or the SSRF guard.  So: approved local images are
    /// embedded, remote ones are fetched by edamame (through that guard) only when
    /// `fetch_remote`, `data:` URIs whose *content* is raster are kept, and everything else loses
    /// its `src`.
    Sealed { fetch_remote: bool },
}

/// One export's image policy, owned so the sanitizer hook can be `'static`.
pub(super) struct ImageResolver {
    handling: ImageHandling,
    source_dir: Option<PathBuf>,
    canon_dir: Option<PathBuf>,
    approved_outside: Vec<PathBuf>,
    /// Remote URL → embedded form, so an image used twice is fetched once.
    fetched: Mutex<HashMap<String, Option<String>>>,
    /// `src`s removed so far ([`Self::left_out`]).
    left_out: AtomicUsize,
}

impl ImageResolver {
    pub(super) fn new(
        handling: ImageHandling,
        source_dir: Option<&Path>,
        approved_outside: &[PathBuf],
    ) -> Self {
        Self {
            handling,
            source_dir: source_dir.map(Path::to_path_buf),
            canon_dir: source_dir.and_then(|dir| dir.canonicalize().ok()),
            approved_outside: approved_outside.to_vec(),
            fetched: Mutex::new(HashMap::new()),
            left_out: AtomicUsize::new(0),
        }
    }

    /// How many `src`s [`Self::resolve`] has removed: images the output will be missing, which
    /// only [`ImageHandling::Sealed`] produces.
    pub(super) fn left_out(&self) -> usize {
        self.left_out.load(Ordering::Relaxed)
    }

    /// The `src` to write in place of `src`, or `None` to remove the attribute.
    pub(super) fn resolve<'u>(&self, src: &'u str) -> Option<Cow<'u, str>> {
        match self.handling {
            ImageHandling::Link => Some(Cow::Borrowed(src)),
            ImageHandling::Embed => {
                Some(self.embed_local(src).map_or(Cow::Borrowed(src), Cow::Owned))
            }
            ImageHandling::Sealed { fetch_remote } => {
                let sealed = if has_data_scheme(src) {
                    // A raster can reference nothing; an SVG could name local files for the
                    // converter to read, so it goes — whatever type the URI declares.
                    raster_data_uri(src).map(Cow::Owned)
                } else if is_http_url(src) {
                    fetch_remote
                        .then(|| self.fetch(src.trim()))
                        .flatten()
                        .map(Cow::Owned)
                } else {
                    self.embed_local(src).map(Cow::Owned)
                };
                if sealed.is_none() {
                    self.left_out.fetch_add(1, Ordering::Relaxed);
                }
                sealed
            }
        }
    }

    /// `src` as a `data:` URI, if it is a local image this export may embed: inside the
    /// document's folder, or outside it and approved.
    fn embed_local(&self, src: &str) -> Option<String> {
        let dir = self.source_dir.as_deref()?;
        let image = local_image(src, dir, self.canon_dir.as_deref())?;
        if !image.inside && !self.approved_outside.contains(&image.canonical) {
            return None;
        }
        file_data_uri(&image.canonical)
    }

    fn fetch(&self, url: &str) -> Option<String> {
        let mut fetched = self.fetched.lock().unwrap_or_else(|e| e.into_inner());
        fetched
            .entry(url.to_owned())
            .or_insert_with(|| fetch_remote(url).ok().and_then(|b| bytes_data_uri(&b)))
            .clone()
    }
}

/// A local image an export could embed, resolved.
pub(super) struct LocalImage {
    pub(super) canonical: PathBuf,
    /// Whether `canonical` lies under the canonicalized `source_dir`.
    pub(super) inside: bool,
}

/// Resolve an image `src` to an existing local file with an image extension, noting whether it
/// stays inside `source_dir`.  `None` means "never embed": remote and `data:` URLs, other
/// schemes, and anything missing or unclassifiable.  Containment is checked *after*
/// `canonicalize`, so a symlink leading out of the folder counts as outside, exactly like a `..`
/// path.
///
/// `canon_dir` is `source_dir` canonicalized once by the caller; `None` (the folder itself
/// failed to resolve) counts every image as outside, so each one is asked about.
pub(super) fn local_image(
    src: &str,
    source_dir: &Path,
    canon_dir: Option<&Path>,
) -> Option<LocalImage> {
    let canonical = source_dir.join(local_path(src)?).canonicalize().ok()?;
    if !canonical.is_file() {
        return None;
    }
    mime_from_extension(&canonical)?;
    let inside = canon_dir.is_some_and(|dir| canonical.starts_with(dir));
    Some(LocalImage { canonical, inside })
}

/// The filesystem path `src` names: a relative or absolute path, or a `file:` URL.  The query and
/// fragment are dropped and the rest percent-decoded, as a browser resolving it would.  `None`
/// for any other scheme, and for a reference naming another host: a `file:` URL with a host
/// other than `localhost`, or a network-path `//host/x` / `\\host\x`, which on Windows is a UNC
/// path that `canonicalize` would open an SMB connection to.
fn local_path(src: &str) -> Option<PathBuf> {
    let src = src.trim();
    let path = match src.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("file://") => {
            let rest = &src[7..];
            let rest = rest.strip_prefix("localhost").unwrap_or(rest);
            match rest.as_bytes() {
                // On Windows `file:///C:/x` names `C:/x`, not `/C:/x`.
                [b'/', drive, b':', ..] if cfg!(windows) && drive.is_ascii_alphabetic() => {
                    &rest[1..]
                }
                [b'/', ..] => rest,
                // `file://host/x`: `host` is a server, not a folder.
                _ => return None,
            }
        }
        _ => {
            let scheme_end = src.find([':', '/', '?', '#']);
            // A single letter before `:` is a Windows drive (`C:/x`), not a scheme.
            if scheme_end.is_some_and(|i| i > 1 && src.as_bytes()[i] == b':') {
                return None;
            }
            src
        }
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let decoded = percent_decode_str(path).decode_utf8().ok()?;
    // Checked after decoding, so `%2F%2Fhost` can't become a UNC path either.
    let slashes = ['/', '\\'];
    if decoded.is_empty()
        || decoded
            .strip_prefix(slashes)
            .is_some_and(|rest| rest.starts_with(slashes))
    {
        return None;
    }
    Some(PathBuf::from(decoded.as_ref()))
}

/// `path`'s bytes as a `data:` URI, typed by content ([`bytes_data_uri`]); `None` if unreadable,
/// over [`MAX_EMBED_BYTES`], or no image edamame accepts.  The extension decides only whether
/// the file is a candidate at all ([`local_image`]).
fn file_data_uri(path: &Path) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > MAX_EMBED_BYTES {
        return None;
    }
    bytes_data_uri(&std::fs::read(path).ok()?)
}

/// `bytes` as a `data:` URI, typed by their content, never by a declared type: a file's
/// extension, a server's `Content-Type` and a `data:` URI's media type are all
/// document-controlled, and a converter handed an SVG labeled `image/png` may still parse it as
/// SVG (WeasyPrint falls back to SVG when its raster decoder fails) and follow its references.
/// An SVG is normalized ([`svg_data_uri`]); `None` if the bytes are no image edamame accepts.
fn bytes_data_uri(bytes: &[u8]) -> Option<String> {
    match raster_mime(bytes) {
        Some(mime) => Some(data_uri(mime, bytes)),
        None => svg_data_uri(std::str::from_utf8(bytes).ok()?),
    }
}

/// The MIME type of `bytes` if their content is a raster format edamame accepts.
fn raster_mime(bytes: &[u8]) -> Option<&'static str> {
    use image::ImageFormat;
    Some(match image::guess_format(bytes).ok()? {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::WebP => "image/webp",
        ImageFormat::Bmp => "image/bmp",
        _ => return None,
    })
}

/// A `data:` URI re-encoded under the type its *content* has, if that is raster; `None` for
/// anything else, including an SVG, whatever the URI declares.
fn raster_data_uri(src: &str) -> Option<String> {
    let bytes = decode_data_uri(src)?;
    Some(data_uri(raster_mime(&bytes)?, &bytes))
}

/// The payload of a `data:[<type>][;base64],<data>` URI: base64-decoded (ASCII whitespace
/// ignored, as a browser does) or percent-decoded.
fn decode_data_uri(src: &str) -> Option<Vec<u8>> {
    let (header, payload) = src.trim().split_once(',')?;
    if header.to_ascii_lowercase().ends_with(";base64") {
        let compact: String = payload
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        BASE64.decode(compact).ok()
    } else {
        Some(percent_decode_str(payload).collect())
    }
}

const SVG_MIME: &str = "image/svg+xml";

/// An SVG embedded only after [`normalize_svg`]: a converter rendering it would otherwise follow
/// an `<image href>` to a local file or the network.  The re-serialized tree has no such
/// references, and is equally harmless in a browser, which loads nothing from an `<img>` SVG.
fn svg_data_uri(svg: &str) -> Option<String> {
    Some(data_uri(SVG_MIME, normalize_svg(svg).ok()?.as_bytes()))
}

fn data_uri(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", BASE64.encode(bytes))
}

fn mime_from_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => SVG_MIME,
        _ => return None,
    })
}

fn is_http_url(src: &str) -> bool {
    let src = src.trim_start().to_ascii_lowercase();
    src.starts_with("http://") || src.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_path_reads_paths_and_file_urls_as_a_browser_does() {
        let p = |s: &str| local_path(s).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(p("img/a%20b.png").as_deref(), Some("img/a b.png"));
        assert_eq!(p("../x.png?v=2#top").as_deref(), Some("../x.png"));
        assert_eq!(p("/abs/x.png").as_deref(), Some("/abs/x.png"));
        assert_eq!(p("file:///abs/x.png").as_deref(), Some("/abs/x.png"));
        assert_eq!(
            p("FILE://localhost/abs/x.png").as_deref(),
            Some("/abs/x.png")
        );
        let drive_url = if cfg!(windows) {
            "C:/x.png"
        } else {
            "/C:/x.png"
        };
        assert_eq!(p("file:///C:/x.png").as_deref(), Some(drive_url));
        assert_eq!(p("C:/x.png").as_deref(), Some("C:/x.png"));
        assert_eq!(p("https://example.com/x.png"), None);
        assert_eq!(p("ftp://example.com/x.png"), None);
        assert_eq!(p("#frag"), None);
    }

    /// A reference to another host is never read as a local path: on Windows `//host/x` and
    /// `\\host\x` are UNC paths, which `canonicalize` would open an SMB connection to.
    #[test]
    fn local_path_refuses_references_to_another_host() {
        for src in [
            "//host/share/x.png",
            r"\\host\share\x.png",
            r"/\host/x.png",
            "%2F%2Fhost/x.png",
            "file://host/share/x.png",
            "file:////host/share/x.png",
        ] {
            assert_eq!(local_path(src), None, "{src}");
        }
    }

    const PNG_SIGNATURE: [u8; 10] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];

    /// A `data:` URI is kept only for raster *content*, re-labeled by it: the declared type is
    /// the document's to choose.
    #[test]
    fn data_uris_are_typed_by_content() {
        let png = BASE64.encode(PNG_SIGNATURE);
        assert_eq!(
            raster_data_uri(&format!("data:image/gif;base64,{png}")),
            Some(format!("data:image/png;base64,{png}"))
        );
        assert!(raster_data_uri(&format!(" DATA:image/png;BASE64,{png}")).is_some());
        let svg = BASE64.encode(r#"<svg xmlns="http://www.w3.org/2000/svg"/>"#);
        assert!(raster_data_uri(&format!("data:image/png;base64,{svg}")).is_none());
        assert!(raster_data_uri("data:image/png,%3Csvg%3E").is_none());
        assert!(raster_data_uri("data:image/png;base64,not base64!").is_none());
    }

    #[test]
    fn remote_bytes_are_typed_by_content() {
        assert!(bytes_data_uri(&PNG_SIGNATURE)
            .unwrap()
            .starts_with("data:image/png;base64,"));
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4"/></svg>"#;
        assert!(bytes_data_uri(svg)
            .unwrap()
            .starts_with("data:image/svg+xml;base64,"));
        assert!(bytes_data_uri(b"<html>not an image</html>").is_none());
    }
}
