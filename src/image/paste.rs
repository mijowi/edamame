//! Turning a bitmap on the clipboard (a screenshot) into a PNG relative to the document.
//!
//! The OS half lives in [`crate::clipboard`], which hands over a [`Bitmap`] and knows nothing
//! else.  This module is the policy: where the image goes, whether a path the user confirmed is
//! acceptable, and the two steps that turn pixels into a file.
//!
//! ```text
//! encode_png()             pixels → PNG bytes                    (pure; runs on a worker)
//! default_destination()    the path the confirm prompt proposes  (reads the fs)
//! resolve_destination()    check the path the user confirmed     (reads the fs)
//! write()                  put the PNG there                     (the one write)
//! ```
//!
//! An image is always stored *relative to the document* and referenced by
//! that relative path, so a document and its images move, sync and render
//! elsewhere together.  An absolute path is refused; one through `..` is
//! allowed, for the common layout of documents in subfolders sharing an
//! image folder beside them.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use crate::clipboard::Bitmap;

/// Where images go when the document references none yet.
pub const DEFAULT_IMAGE_DIR: &str = "images";

// ── Policy ────────────────────────────────────────────────────────────────

/// The directory the document already keeps its images in: the most
/// common parent among `urls` (its local image references), the first seen
/// winning a tie, or [`DEFAULT_IMAGE_DIR`] when there are none.  An image
/// beside the document yields `""`.
///
/// A destination is taken as a literal path, `\` and `/` alike separating
/// its segments — percent-escapes are *not* decoded, because the image
/// loader does not decode them either: `my%20pics/a.png` is loaded from a
/// directory literally named `my%20pics`, so that is where a new image
/// belongs too.
pub fn infer_dir(urls: &[&str]) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for url in urls {
        let dir = parent_dir(url);
        match counts.iter_mut().find(|(d, _)| *d == dir) {
            Some((_, n)) => *n += 1,
            None => counts.push((dir, 1)),
        }
    }
    // `max_by_key` keeps the *last* maximum, so reverse to let the first seen win.
    counts
        .into_iter()
        .rev()
        .max_by_key(|(_, n)| *n)
        .map_or_else(|| DEFAULT_IMAGE_DIR.to_owned(), |(dir, _)| dir)
}

/// `a/b/c.png` (or `a\b\c.png`) → `a/b`; `./c.png` and `c.png` → `""`.
fn parent_dir(url: &str) -> String {
    let parts: Vec<&str> = url
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    parts[..parts.len().saturating_sub(1)].join("/")
}

/// The document-relative path the confirm prompt proposes, `/`-separated: `dir` plus a
/// `YYYYMMDD-HHMMSS.png` timestamp (local time), with `-1`, `-2`, … added when the name is taken.
pub fn default_destination(dir: &str, doc_dir: &Path) -> String {
    let stem = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let join = |name: &str| {
        if dir.is_empty() {
            name.to_owned()
        } else {
            format!("{dir}/{name}")
        }
    };
    let mut candidate = join(&format!("{stem}.png"));
    let mut n = 1u32;
    while doc_dir.join(&candidate).exists() {
        candidate = join(&format!("{stem}-{n}.png"));
        n += 1;
    }
    candidate
}

/// A destination the user confirmed, checked and resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    /// The Markdown link destination: relative, `/`-separated, unescaped.
    pub link: String,
    /// Where the file goes on disk.
    pub target: PathBuf,
}

/// Check the path the user confirmed, resolving it against `doc_dir`.
///
/// `Err` carries the message the prompt shows.  Refused: an empty path, an absolute one, one
/// holding a `:` or a control character, a name not ending in `.png`, and a file that already
/// exists.  A `..` segment is kept, not refused: documents organized into subfolders commonly
/// share an image folder beside them, and the user sees and confirms the full path.  An
/// absolute path is refused because the link written into the document must stay relative.
/// A `:` has no portable meaning in a relative path — on Windows it would name a drive or an
/// NTFS alternate data stream rather than a file — and a control character (a newline above all)
/// cannot be written into a Markdown link destination.
pub fn resolve_destination(input: &str, doc_dir: &Path) -> Result<Destination, String> {
    let input = input.trim().replace('\\', "/");
    if input.is_empty() {
        return Err("Enter a path for the image".to_owned());
    }
    if is_rooted(&input) {
        return Err("Use a path relative to the document's folder".to_owned());
    }
    if input.chars().any(|c| c == ':' || c.is_control()) {
        return Err("The path can't contain ':' or control characters".to_owned());
    }
    // With the refusals above, only `Normal`, `CurDir` and `ParentDir` components remain.
    let parts: Vec<String> = Path::new(&input)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_owned()),
            _ => None,
        })
        .collect();
    let Some(name) = parts.last() else {
        return Err("Enter a file name for the image".to_owned());
    };
    let is_png = Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("png"));
    if !is_png {
        return Err("The file name must end in .png".to_owned());
    }
    let link = parts.join("/");
    let target = parts
        .iter()
        .fold(doc_dir.to_path_buf(), |p, part| p.join(part));
    if target.exists() {
        return Err(format!("{link} already exists"));
    }
    Ok(Destination { link, target })
}

/// Whether `path` is anchored somewhere other than the directory it would be resolved against:
/// it starts at a root (`/`, `\`), or its first segment holds a colon — a scheme (`https:`) or a
/// Windows drive (`C:`, including the drive-relative `C:foo`).  `/` and `\` both separate.
///
/// The one rule for "relative to the document", shared by [`resolve_destination`] and
/// [`crate::markdown::local_image_urls`].
pub fn is_rooted(path: &str) -> bool {
    let first = path.split(['/', '\\']).next().unwrap_or_default();
    path.starts_with(['/', '\\']) || first.contains(':')
}

// ── Encoding and writing ──────────────────────────────────────────────────

/// Encode RGBA pixels as PNG.  Slow for a large screenshot, so the paste flow runs it on the
/// worker that read the bitmap.  A bitmap whose length doesn't match its size is refused rather
/// than handed to the encoder, which panics on the mismatch.
pub fn encode_png(bitmap: &Bitmap) -> Result<Vec<u8>, String> {
    use image::ImageEncoder as _;
    let expected = (bitmap.width as usize)
        .checked_mul(bitmap.height as usize)
        .and_then(|n| n.checked_mul(4));
    if expected != Some(bitmap.rgba.len()) {
        return Err("The clipboard image is malformed".to_owned());
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &bitmap.rgba,
            bitmap.width,
            bitmap.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("PNG encode failed: {e}"))?;
    Ok(png)
}

/// Write `png` to `target`, creating its directory.  Never overwrites: a file that appeared
/// since [`resolve_destination`] checked is an error, and so is a symlink at `target`.  A write
/// that fails part-way deletes what it wrote, which would otherwise make the retry's name
/// "already exist".
pub fn write(png: &[u8], target: &Path) -> Result<(), String> {
    let write_err = |e: std::io::Error| format!("Cannot write {}: {e}", target.display());
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(write_err)?;
    let result = file.write_all(png).map_err(write_err);
    if result.is_err() {
        drop(file);
        let _ = std::fs::remove_file(target);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap() -> Bitmap {
        Bitmap {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
        }
    }

    #[test]
    fn infer_dir_picks_the_most_common_parent() {
        let urls = ["assets/a.png", "images/b.png", "assets/c.png"];
        assert_eq!(infer_dir(&urls), "assets");
    }

    #[test]
    fn infer_dir_breaks_a_tie_by_first_seen() {
        assert_eq!(infer_dir(&["b/x.png", "a/y.png"]), "b");
    }

    #[test]
    fn infer_dir_normalizes_dot_segments_and_keeps_nesting() {
        assert_eq!(infer_dir(&["./media/2024/x.png"]), "media/2024");
        assert_eq!(infer_dir(&["x.png", "./y.png"]), "");
    }

    #[test]
    fn infer_dir_accepts_backslash_separators() {
        assert_eq!(infer_dir(&[r"assets\2024\x.png"]), "assets/2024");
    }

    #[test]
    fn infer_dir_keeps_percent_escapes_literal_like_the_loader() {
        assert_eq!(infer_dir(&["my%20pics/x.png"]), "my%20pics");
    }

    #[test]
    fn infer_dir_falls_back_to_images() {
        assert_eq!(infer_dir(&[]), DEFAULT_IMAGE_DIR);
    }

    #[test]
    fn default_name_is_a_local_timestamp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = default_destination("images", dir.path());
        let name = dest.strip_prefix("images/").expect("under the dir");
        let stem = name.strip_suffix(".png").expect("png");
        assert!(
            chrono::NaiveDateTime::parse_from_str(stem, "%Y%m%d-%H%M%S").is_ok(),
            "{name}"
        );
    }

    #[test]
    fn default_destination_skips_taken_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = default_destination("", dir.path());
        std::fs::write(dir.path().join(&first), b"x").unwrap();
        let second = default_destination("", dir.path());
        // The clock may tick between the two calls, giving a fresh stem instead of a suffix.
        assert_ne!(first, second);
        let stem = first.strip_suffix(".png").unwrap();
        if second.starts_with(stem) {
            assert_eq!(second, format!("{stem}-1.png"));
        }
    }

    #[test]
    fn resolve_rejects_rooted_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        for bad in ["/tmp/x.png", "C:/x.png", r"\x.png"] {
            assert!(
                resolve_destination(bad, dir.path()).is_err(),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn resolve_rejects_colons_and_control_characters_anywhere() {
        let dir = tempfile::tempdir().expect("tempdir");
        for bad in ["a/b:c.png", "a\nb.png", "a/b\tc.png", "x\u{7f}.png"] {
            let err = resolve_destination(bad, dir.path()).unwrap_err();
            assert!(err.contains("control characters"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn the_relative_to_the_document_rule() {
        for rooted in [
            "/a.png",
            r"\a.png",
            r"\\server\share\a.png",
            "C:/a.png",
            "C:a.png",
            "https://x/a.png",
        ] {
            assert!(is_rooted(rooted), "{rooted}");
        }
        for relative in ["a.png", "./a/b.png", r"a\b.png", "a/b:c.png"] {
            assert!(!is_rooted(relative), "{relative}");
        }
    }

    /// A shared image folder beside the document's is a common layout, so `..` is kept in both
    /// the link and the target rather than refused.
    #[test]
    fn resolve_keeps_parent_segments() {
        let root = tempfile::tempdir().expect("tempdir");
        let doc_dir = root.path().join("docs");
        std::fs::create_dir(&doc_dir).unwrap();
        let dest = resolve_destination(r"..\assets\shot.png", &doc_dir).unwrap();
        assert_eq!(dest.link, "../assets/shot.png");
        assert_eq!(
            dest.target,
            doc_dir.join("..").join("assets").join("shot.png")
        );

        std::fs::write(root.path().join("taken.png"), b"x").unwrap();
        let err = resolve_destination("../taken.png", &doc_dir).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert!(resolve_destination("..", &doc_dir).is_err(), "no file name");
    }

    #[test]
    fn infer_dir_keeps_parent_segments() {
        let urls = ["../assets/a.png", "../assets/b.png", "local/c.png"];
        assert_eq!(infer_dir(&urls), "../assets");
    }

    #[test]
    fn resolve_requires_a_png_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(resolve_destination("images/x.jpg", dir.path()).is_err());
        assert!(resolve_destination("images/x", dir.path()).is_err());
        assert!(resolve_destination("images/X.PNG", dir.path()).is_ok());
    }

    #[test]
    fn resolve_refuses_an_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("x.png"), b"x").unwrap();
        let err = resolve_destination("x.png", dir.path()).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn resolve_normalizes_separators_and_dot_segments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = resolve_destination(r".\images\my shot.png", dir.path()).unwrap();
        assert_eq!(dest.link, "images/my shot.png");
        assert_eq!(dest.target, dir.path().join("images").join("my shot.png"));
    }

    #[test]
    fn encode_png_round_trips_through_the_decoder() {
        let png = encode_png(&bitmap()).expect("encode");
        let decoded = image::load_from_memory(&png).expect("decodes");
        assert_eq!((decoded.width(), decoded.height()), (2, 1));
    }

    #[test]
    fn encode_png_refuses_a_bitmap_whose_length_does_not_match_its_size() {
        let short = Bitmap {
            width: 2,
            height: 2,
            rgba: vec![0; 4],
        };
        assert!(encode_png(&short).is_err());
    }

    #[test]
    fn write_creates_the_directory_and_never_overwrites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("a/b/x.png");
        write(b"png", &target).expect("first write");
        assert_eq!(std::fs::read(&target).unwrap(), b"png");
        assert!(
            write(b"again", &target).is_err(),
            "second write must refuse"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"png");
    }
}
