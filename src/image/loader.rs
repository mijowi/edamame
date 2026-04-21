//! Resolve image URLs to decoded bytes.
//!
//! Local paths are read from disk relative to the document path (or
//! absolute if the URL starts with `/` or uses the `file://` scheme).
//! `http://` and `https://` URLs are fetched via `ureq` when allowed by
//! the configured `RemoteImagePolicy` plus the per-session "allow remote"
//! flag.
//!
//! This function is **blocking**; the intended call site is a background
//! thread that reports completion via `AppEvent::ImageReady`.  Keeping it
//! blocking (rather than async) avoids an async runtime dependency and
//! keeps the rest of the code-base free of `async fn`s.

use std::path::{Path, PathBuf};
use std::time::Duration;

use image::DynamicImage;

use crate::config::RemoteImagePolicy;

/// Decoded image paired with its origin info — sufficient for
/// `ImageCache::decoded` to key the entry and for debugging.
#[derive(Debug)]
pub struct LoadedImage {
    pub url: String,
    pub image: DynamicImage,
}

/// Errors reported by [`resolve`].  The UI falls back to the
/// `[Image: alt]` placeholder on any variant.
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
    #[error("unsupported url scheme: {0}")]
    UnsupportedScheme(String),
}

/// HTTP timeout for remote image fetches.  Long enough for a busy
/// connection, short enough that the UI doesn't appear hung for minutes
/// if a server ghosts us.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(10);

/// Resolve `url` to a decoded `DynamicImage`.
///
/// * `doc_path` — path of the currently-open document (used as the base
///   for relative image paths).  Pass `None` for an unsaved buffer;
///   relative URLs then resolve against the current working directory.
/// * `remote_policy` — `Ask` / `Always` / `Never` from config.  `Ask` is
///   interpreted as "blocked unless the per-session flag is set" — the
///   App shows the prompt on document load and flips the flag based on
///   the user's choice.
/// * `session_allow_remote` — set by the remote-load prompt's
///   `This time only` / `Always` buttons.  Takes precedence over a
///   `Never` persisted policy only if the caller has already verified
///   its provenance (the prompt itself defers to policy).
/// * `max_cells` / `font_size` — the target ceiling for pre-resizing
///   the decoded image so the main thread never has to resize on a
///   render path.  Passing `None` disables pre-resize (the main thread
///   will do it in the protocol, incurring a one-time cost on first
///   paint — acceptable for tests that don't exercise rendering).
///
/// Pre-resize runs on the worker thread **before** the image reaches
/// the main thread.  This means by the time `paint_images` builds the
/// `StatefulProtocol` the image already has `cells <= max_cells`, and
/// `Resize::Fit` is a no-op on every subsequent frame — no re-encoding
/// on scroll.
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
        decode(url, &bytes)?
    } else {
        let path = resolve_local_path(url, doc_path)?;
        let bytes = std::fs::read(&path).map_err(|source| ImageLoadError::Io {
            path: path.clone(),
            source,
        })?;
        decode(url, &bytes)?
    };

    let image = match (max_cells, font_size) {
        (Some(cells), Some(font)) => pre_resize(image, cells, font),
        _ => image,
    };

    Ok(LoadedImage {
        url: url.to_owned(),
        image,
    })
}

/// Downscale `image` to fit within `max_cells × font_size` pixels while
/// preserving aspect ratio.  Returns the original image unmodified when
/// it already fits, so small images don't pay the resize cost.
fn pre_resize(image: DynamicImage, max_cells: (u16, u16), font_size: (u16, u16)) -> DynamicImage {
    let max_w_px = u32::from(max_cells.0) * u32::from(font_size.0);
    let max_h_px = u32::from(max_cells.1) * u32::from(font_size.1);
    if max_w_px == 0 || max_h_px == 0 {
        return image;
    }
    if image.width() <= max_w_px && image.height() <= max_h_px {
        return image;
    }
    // `Triangle` (bilinear) is ~3–5× faster than `Lanczos3` and
    // visually indistinguishable at thumbnail sizes.  The image is
    // subsequently re-encoded by `ratatui-image`'s protocol layer
    // (halfblocks averages 2 pixels per cell, native graphics just
    // transmits the pre-scaled pixels), so the extra Lanczos quality
    // would never reach the terminal anyway.
    image.resize(max_w_px, max_h_px, image::imageops::FilterType::Triangle)
}

/// True when `url` is an `http://` or `https://` URL.  Other schemes
/// (`data:`, `file:`, `ftp:`, …) are not considered remote: `file:` is
/// handled as a local path, everything else is rejected in
/// [`resolve_local_path`].
pub fn is_remote(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn resolve_local_path(url: &str, doc_path: Option<&Path>) -> Result<PathBuf, ImageLoadError> {
    // Accept `file:///abs/path` and bare paths.  Reject any other scheme.
    if let Some(stripped) = url.strip_prefix("file://") {
        return Ok(PathBuf::from(stripped));
    }
    if let Some((scheme, _)) = url.split_once(':') {
        // A bare Windows path ("C:/…") has a single-char scheme — treat
        // any single-char prefix as not-a-scheme so we don't reject those.
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
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(REMOTE_TIMEOUT)
        .timeout_read(REMOTE_TIMEOUT)
        .build();
    let response = agent
        .get(url)
        .call()
        .map_err(|source| ImageLoadError::Http {
            url: url.to_owned(),
            source: Box::new(source),
        })?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|source| ImageLoadError::Io {
            path: PathBuf::from(url),
            source,
        })?;
    Ok(bytes)
}

fn decode(url: &str, bytes: &[u8]) -> Result<DynamicImage, ImageLoadError> {
    image::load_from_memory(bytes).map_err(|source| ImageLoadError::Decode {
        url: url.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a tiny valid PNG in-memory using the `image` crate so the
    /// test doesn't depend on a hand-crafted byte string (which is easy
    /// to get wrong — CRCs, IDAT compression, etc.).
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
        // 40×40 pixels > 20×10 cells × 2×2 font = 40×20 pixels? hmm
        // Use big-enough numbers to ensure the image is downscaled.
        let big = DynamicImage::new_rgba8(500, 500);
        let resized = pre_resize(big, (20, 10), (10, 20));
        // max_w_px = 200, max_h_px = 200 → fit preserves aspect, so
        // 500×500 → 200×200.
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
