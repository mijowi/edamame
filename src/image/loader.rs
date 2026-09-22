//! Resolve image URLs to decoded images.  Local paths are read relative to the document (or
//! absolutely, including `file://`); `http(s)` URLs are fetched via `ureq` when the
//! `RemoteImagePolicy` plus the per-session flag allow it.
//!
//! [`resolve`] is **blocking** — the call site is the decode worker thread, which reports back via
//! `AppEvent::ImageReady`.  Blocking rather than async keeps an async runtime out of the tree.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use image::DynamicImage;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::config::RemoteImagePolicy;
use crate::image::cache::DirectPlacement;
use crate::image::svg::{rasterize_svg, SvgError, SvgScaleMode, SvgSizing};
use crate::image::SlicedProtocol;

/// A decoded image plus the URL `ImageCache` keys it by.
///
/// `scratch` is a pre-rendered halfblocks `Buffer` for a known target rect, built on the worker so
/// the first paint doesn't pay a cold sync encode.  `sliced` is the row-addressed Kitty protocol
/// for the same rect, built on the worker for the same reason — `SlicedProtocol::new_with_resize`
/// formats the transmit string synchronously, and that string is megabytes of base64.  Both are
/// `None` without image support, when the dispatcher supplied no picker and target width, or on the
/// protocol's own cold-path failure.
///
/// `Debug` is written by hand below: `SlicedProtocol` is not `Debug`, and the interesting part of
/// a prebuilt is its geometry, not megabytes of base64.
pub struct LoadedImage {
    pub url: String,
    pub image: DynamicImage,
    pub scratch: Option<(Rect, Buffer)>,
    pub sliced: Option<(Rect, SlicedProtocol)>,
    pub direct: Option<(Rect, DirectPlacement)>,
}

impl std::fmt::Debug for LoadedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedImage")
            .field("url", &self.url)
            .field("image", &self.image)
            .field("scratch", &self.scratch.as_ref().map(|(rect, _)| *rect))
            .field("sliced", &self.sliced.as_ref().map(|(rect, _)| *rect))
            .field("direct", &self.direct.as_ref().map(|(rect, _)| *rect))
            .finish()
    }
}

/// Errors from [`resolve`]; the UI falls back to the `[Image: alt]` placeholder on any of them.
#[derive(Debug, thiserror::Error)]
pub enum ImageLoadError {
    #[error("remote image blocked by policy: {0}")]
    RemoteBlocked(String),
    #[error("file read failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("http fetch failed for {url}: {source}")]
    Http {
        url: String,
        #[source]
        source: Box<ureq::Error>,
    },
    #[error("image decode failed for {url}: {source}")]
    Decode {
        url: String,
        #[source]
        source: image::ImageError,
    },
    #[error("svg render failed for {url}: {source}")]
    Svg {
        url: String,
        #[source]
        source: SvgError,
    },
    #[error("unsupported url scheme: {0}")]
    UnsupportedScheme(String),
}

/// HTTP timeout for remote image fetches, so a ghosting server can't hang the worker for
/// minutes.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on a *local* image file read off disk.  Remote fetches are bounded by ureq's own body
/// limit, but without this a multi-gigabyte file is slurped into memory by one `std::fs::read`
/// before the decode limits below can apply.
const MAX_LOCAL_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Hard ceilings on every raster decode (see [`decode`]).  PNG zlib ratios routinely exceed
/// 1000:1 and `image` allocates the full pixel buffer *before* [`pre_resize`] can shrink it, so
/// decode time via `image::Limits` is the only place to bound peak memory — a decode bomb errors
/// out instead of aborting the process.
const MAX_DECODE_DIMENSION: u32 = 50_000;
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// Resolve `url` to a decoded `DynamicImage`.
///
/// * `doc_path` — base for relative image paths; `None` resolves against the working directory.
/// * `remote_policy` — `Ask` means "blocked unless `session_allow_remote` is set".
/// * `session_allow_remote` — set by the remote-load prompt, which itself defers to policy.
/// * `max_cells` / `font_size` — pre-resize ceiling.  `None` skips it, leaving the resize to the
///   protocol layer on first paint.
///
/// Pre-resizing here, on the worker, is what makes `Resize::Fit` a no-op once the image reaches
/// `paint_images` — so scrolling never re-encodes.
pub fn resolve(
    url: &str,
    doc_path: Option<&Path>,
    remote_policy: RemoteImagePolicy,
    session_allow_remote: bool,
    max_cells: Option<(u16, u16)>,
    font_size: Option<(u16, u16)>,
) -> Result<LoadedImage, ImageLoadError> {
    let image = if is_remote(url) {
        let allow = matches!(remote_policy, RemoteImagePolicy::Always) || session_allow_remote;
        if !allow {
            return Err(ImageLoadError::RemoteBlocked(url.to_owned()));
        }
        let bytes = fetch_remote(url)?;
        decode_any(url, &bytes, max_cells, font_size)?
    } else {
        let path = resolve_local_path(url, doc_path)?;
        let meta = std::fs::metadata(&path).map_err(|source| ImageLoadError::Io {
            path: path.clone(),
            source,
        })?;
        if meta.len() > MAX_LOCAL_IMAGE_BYTES {
            return Err(ImageLoadError::Io {
                path: path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "image file too large: {} bytes (max {MAX_LOCAL_IMAGE_BYTES})",
                        meta.len()
                    ),
                ),
            });
        }
        let bytes = std::fs::read(&path).map_err(|source| ImageLoadError::Io {
            path: path.clone(),
            source,
        })?;
        decode_any(url, &bytes, max_cells, font_size)?
    };

    let image = match (max_cells, font_size) {
        (Some(cells), Some(font)) => pre_resize(image, cells, font),
        _ => image,
    };

    Ok(LoadedImage {
        url: url.to_owned(),
        image,
        // Both prebuilts are the decode dispatch's to fill in: it owns the picker and the target
        // width, and builds them on the worker thread so the first paint is a cache hit.
        scratch: None,
        sliced: None,
        direct: None,
    })
}

/// Downscale to fit `max_cells × font_size` pixels, preserving aspect ratio.  An image that
/// already fits is returned untouched.
fn pre_resize(image: DynamicImage, max_cells: (u16, u16), font_size: (u16, u16)) -> DynamicImage {
    let max_w_px = u32::from(max_cells.0) * u32::from(font_size.0);
    let max_h_px = u32::from(max_cells.1) * u32::from(font_size.1);
    if max_w_px == 0 || max_h_px == 0 {
        return image;
    }
    if image.width() <= max_w_px && image.height() <= max_h_px {
        return image;
    }
    // `Triangle` is ~3–5× faster than `Lanczos3`, and the protocol layer re-encodes afterwards,
    // so the extra Lanczos quality would never reach the terminal anyway.
    image.resize(max_w_px, max_h_px, image::imageops::FilterType::Triangle)
}

/// True for `http(s)` URLs only.  `file:` is a local path; every other scheme is rejected by
/// [`resolve_local_path`].
pub fn is_remote(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn resolve_local_path(url: &str, doc_path: Option<&Path>) -> Result<PathBuf, ImageLoadError> {
    if let Some(stripped) = url.strip_prefix("file://") {
        return Ok(PathBuf::from(stripped));
    }
    if let Some((scheme, _)) = url.split_once(':') {
        // A bare Windows path ("C:/…") looks like a single-char scheme; don't reject those.
        let looks_like_scheme = scheme.len() > 1
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if looks_like_scheme {
            return Err(ImageLoadError::UnsupportedScheme(url.to_owned()));
        }
    }
    let candidate = PathBuf::from(url);
    if candidate.is_absolute() {
        return Ok(candidate);
    }
    match doc_path.and_then(|p| p.parent()) {
        Some(parent) => Ok(parent.join(candidate)),
        None => Ok(candidate),
    }
}

fn fetch_remote(url: &str) -> Result<Vec<u8>, ImageLoadError> {
    // All three phases are bounded, so a slow server can't hang the decode worker.
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(REMOTE_TIMEOUT))
        .timeout_recv_response(Some(REMOTE_TIMEOUT))
        .timeout_recv_body(Some(REMOTE_TIMEOUT))
        .build();
    // The custom resolver is the SSRF guard — see `PublicOnlyResolver`.
    let agent = ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::default(),
        PublicOnlyResolver::default(),
    );
    let mut response = agent
        .get(url)
        .call()
        .map_err(|source| ImageLoadError::Http {
            url: url.to_owned(),
            source: Box::new(source),
        })?;
    // Body-read transport errors are `ureq::Error` too, so they route through the same variant.
    response
        .body_mut()
        .read_to_vec()
        .map_err(|source| ImageLoadError::Http {
            url: url.to_owned(),
            source: Box::new(source),
        })
}

// ── SSRF guard ──────────────────────────────────────────────────────────────

/// A resolver that drops internal addresses, wrapping ureq's `DefaultResolver`.  Once remote
/// images are allowed for a document *every* `http(s)` URL in it is fetched, so without this a
/// benign-looking URL — or a `3xx` redirect from one — could reach loopback, a LAN host, or the
/// cloud-metadata endpoint.
///
/// Filtering the *resolved* IP rather than the hostname defeats literal-IP URLs, DNS rebinding,
/// and every redirect hop uniformly, since ureq re-resolves each hop through here.
#[derive(Debug, Default)]
struct PublicOnlyResolver {
    inner: ureq::unversioned::resolver::DefaultResolver,
}

impl ureq::unversioned::resolver::Resolver for PublicOnlyResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let resolved = self.inner.resolve(uri, config, timeout)?;
        let mut allowed = self.empty();
        for addr in &resolved {
            if !is_blocked_ip(addr.ip()) {
                allowed.push(*addr);
            }
        }
        if allowed.is_empty() {
            // Every candidate was internal: unresolvable, rather than connect to it.
            return Err(ureq::Error::HostNotFound);
        }
        Ok(allowed)
    }
}

/// True for any IP a remote image fetch must not reach: loopback, RFC1918, link-local (including
/// the `169.254.169.254` metadata address), CGNAT, IPv6 unique- and link-local, and the
/// unspecified / broadcast / documentation ranges.  IPv4-mapped IPv6 is unwrapped and re-checked,
/// so `::ffff:127.0.0.1` cannot slip past.
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.octets()[0] == 0
                // 100.64.0.0/10 — carrier-grade NAT
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(mapped));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg0 & 0xfe00) == 0xfc00 // fc00::/7  unique-local
                || (seg0 & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

/// True when `url`'s path ends in `.svg`, ignoring any query or fragment.  Extension only: a URL
/// serving SVG from a non-`.svg` path falls through to the raster decoder and fails like any
/// non-image.
fn is_svg_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.to_ascii_lowercase().ends_with(".svg")
}

/// Decode `bytes`, picking the SVG rasterizer for `.svg` URLs and the raster decoder otherwise.
fn decode_any(
    url: &str,
    bytes: &[u8],
    max_cells: Option<(u16, u16)>,
    font_size: Option<(u16, u16)>,
) -> Result<DynamicImage, ImageLoadError> {
    if is_svg_url(url) {
        decode_svg(url, bytes, max_cells, font_size)
    } else {
        decode(url, bytes)
    }
}

/// Decode raster `bytes` with [`image::Limits`] in force, so a compression bomb is rejected with
/// a `Decode` error rather than an OOM `catch_unwind` cannot contain.  Never use the unbounded
/// `image::load_from_memory` here.
fn decode(url: &str, bytes: &[u8]) -> Result<DynamicImage, ImageLoadError> {
    let mut limits = image::Limits::no_limits();
    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);

    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| ImageLoadError::Decode {
            url: url.to_owned(),
            source: image::ImageError::IoError(e),
        })?;
    reader.limits(limits);
    reader.decode().map_err(|source| ImageLoadError::Decode {
        url: url.to_owned(),
        source,
    })
}

/// Rasterize SVG `bytes`.  Unlike a diagram, a user's SVG has a meaningful natural size, so it is
/// only *downscaled* to the envelope (`SvgSizing::Natural`) and its transparency is preserved.
/// The later [`pre_resize`] is a no-op — the rasterizer already capped the pixel size.
///
/// Only UTF-8 is accepted; a UTF-16 or BOM-prefixed file reports an `Svg` parse error rather than
/// failing obscurely later.
fn decode_svg(
    url: &str,
    bytes: &[u8],
    max_cells: Option<(u16, u16)>,
    font_size: Option<(u16, u16)>,
) -> Result<DynamicImage, ImageLoadError> {
    let svg = std::str::from_utf8(bytes).map_err(|e| ImageLoadError::Svg {
        url: url.to_owned(),
        source: SvgError::Parse(format!("invalid UTF-8: {e}")),
    })?;
    rasterize_svg(
        svg,
        SvgSizing {
            envelope: max_cells,
            font_size,
            mode: SvgScaleMode::Natural,
        },
        None,
    )
    .map_err(|source| ImageLoadError::Svg {
        url: url.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny valid PNG, encoded rather than hand-written (CRCs, IDAT compression).
    fn tiny_png() -> Vec<u8> {
        use image::{ImageBuffer, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(2, 2, Rgba([10, 20, 30, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
    }

    /// A minimal valid SVG with explicit dimensions.
    fn tiny_svg() -> &'static [u8] {
        br##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">
  <rect width="24" height="24" fill="#3498db"/>
</svg>"##
    }

    #[test]
    fn is_svg_url_matches_extension_ignoring_query() {
        assert!(is_svg_url("icon.svg"));
        assert!(is_svg_url("./art/Logo.SVG"));
        assert!(is_svg_url("/abs/diagram.svg"));
        assert!(is_svg_url("https://img.shields.io/badge/x.svg?style=flat"));
        assert!(is_svg_url("https://example.com/a.svg#frag"));
        assert!(!is_svg_url("photo.png"));
        assert!(!is_svg_url("https://example.com/render?format=svg"));
    }

    #[test]
    fn local_svg_file_rasterizes_at_natural_size() {
        let mut file = tempfile::Builder::new()
            .suffix(".svg")
            .tempfile()
            .expect("tempfile");
        std::io::Write::write_all(&mut file, tiny_svg()).expect("write svg");
        let path = file.path().to_str().unwrap().to_owned();

        // Natural 24×24 fits the 80×24-cell envelope, so it stays 24×24.
        let loaded = resolve(
            &path,
            None,
            RemoteImagePolicy::Never,
            false,
            Some((80, 24)),
            Some((8, 16)),
        )
        .expect("load svg");
        assert_eq!(loaded.image.width(), 24);
        assert_eq!(loaded.image.height(), 24);
    }

    #[test]
    fn invalid_svg_file_reports_svg_error() {
        let mut file = tempfile::Builder::new()
            .suffix(".svg")
            .tempfile()
            .expect("tempfile");
        std::io::Write::write_all(&mut file, b"this is not svg").expect("write");
        let path = file.path().to_str().unwrap().to_owned();

        let err = resolve(&path, None, RemoteImagePolicy::Never, false, None, None).unwrap_err();
        assert!(matches!(err, ImageLoadError::Svg { .. }));
    }

    #[test]
    fn ssrf_filter_blocks_internal_ranges() {
        use std::net::{Ipv4Addr, Ipv6Addr};
        let blocked: [IpAddr; 10] = [
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)), // cloud metadata
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),      // CGNAT
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6("fc00::1".parse().unwrap()),
            IpAddr::V6("fe80::1".parse().unwrap()),
        ];
        for ip in blocked {
            assert!(is_blocked_ip(ip), "{ip} must be blocked");
        }

        let allowed: [IpAddr; 4] = [
            IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
            IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), // example.com
            IpAddr::V6("2606:4700:4700::1111".parse().unwrap()),
        ];
        for ip in allowed {
            assert!(!is_blocked_ip(ip), "{ip} must be allowed");
        }

        // IPv4-mapped loopback must not slip past the v6 arm.
        assert!(is_blocked_ip(IpAddr::V6(
            "::ffff:127.0.0.1".parse().unwrap()
        )));
    }

    #[test]
    fn is_remote_matches_http_schemes() {
        assert!(is_remote("http://example.com/a.png"));
        assert!(is_remote("HTTPS://Example.COM/a.png"));
        assert!(!is_remote("./img/a.png"));
        assert!(!is_remote("/abs/a.png"));
        assert!(!is_remote("file:///tmp/a.png"));
    }

    #[test]
    fn remote_never_policy_blocks_even_with_session_flag_cleared() {
        let err = resolve(
            "https://example.com/a.png",
            None,
            RemoteImagePolicy::Never,
            false,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ImageLoadError::RemoteBlocked(_)));
    }

    #[test]
    fn remote_ask_policy_blocks_without_session_flag() {
        let err = resolve(
            "https://example.com/a.png",
            None,
            RemoteImagePolicy::Ask,
            false,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ImageLoadError::RemoteBlocked(_)));
    }

    #[test]
    fn local_absolute_path_is_used_verbatim() {
        let mut file = tempfile::NamedTempFile::new().expect("tempfile");
        let bytes = tiny_png();
        std::io::Write::write_all(&mut file, &bytes).expect("write png");
        let path = file.path().to_str().unwrap().to_owned();

        let loaded =
            resolve(&path, None, RemoteImagePolicy::Never, false, None, None).expect("load");
        assert_eq!(loaded.url, path);
        assert_eq!(loaded.image.width(), 2);
        assert_eq!(loaded.image.height(), 2);
    }

    #[test]
    fn local_relative_path_resolves_from_doc_parent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let img = dir.path().join("pic.png");
        std::fs::write(&img, tiny_png()).expect("write png");
        let doc = dir.path().join("doc.md");

        let loaded = resolve(
            "pic.png",
            Some(&doc),
            RemoteImagePolicy::Never,
            false,
            None,
            None,
        )
        .expect("load relative");
        assert_eq!(loaded.image.width(), 2);
    }

    #[test]
    fn file_scheme_is_treated_as_local_path() {
        let mut file = tempfile::NamedTempFile::new().expect("tempfile");
        let bytes = tiny_png();
        std::io::Write::write_all(&mut file, &bytes).expect("write png");
        let url = format!("file://{}", file.path().display());
        let loaded =
            resolve(&url, None, RemoteImagePolicy::Never, false, None, None).expect("load");
        assert_eq!(loaded.image.width(), 2);
    }

    #[test]
    fn unsupported_scheme_is_rejected() {
        let err = resolve(
            "ftp://example.com/a.png",
            None,
            RemoteImagePolicy::Always,
            false,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ImageLoadError::UnsupportedScheme(_)));
    }

    #[test]
    fn missing_local_file_reports_io_error() {
        let err = resolve(
            "/definitely/not/a/real/path/image.png",
            None,
            RemoteImagePolicy::Never,
            false,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ImageLoadError::Io { .. }));
    }

    #[test]
    fn pre_resize_downscales_oversized_images() {
        let big = DynamicImage::new_rgba8(500, 500);
        // Envelope is 200×200 px, so 500×500 fits down to 200×200.
        let resized = pre_resize(big, (20, 10), (10, 20));
        assert!(resized.width() <= 200);
        assert!(resized.height() <= 200);
    }

    #[test]
    fn pre_resize_leaves_small_images_unchanged() {
        let small = DynamicImage::new_rgba8(50, 50);
        let (before_w, before_h) = (small.width(), small.height());
        let resized = pre_resize(small, (20, 10), (10, 20));
        assert_eq!(resized.width(), before_w);
        assert_eq!(resized.height(), before_h);
    }
}
