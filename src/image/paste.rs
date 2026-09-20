//! Turning what is on the clipboard into a Markdown image reference.
//!
//! The OS half lives in [`crate::clipboard`], which hands over a
//! [`ClipboardData`] snapshot and knows nothing else.  This module is the
//! policy that reads that snapshot, plus the one side effect it needs:
//! writing a bitmap somewhere Markdown can point at.
//!
//! ```text
//! select()        which payload wins           (pure)
//! destination()   select() + save a bitmap     (one write, when needed)
//! ```
//!
//! The order `select` encodes is the order that copies least:
//!
//! - a **file** the user copied in a file manager is referenced where it
//!   lies — nothing is read, nothing is written;
//! - a **bitmap** (a screenshot) has to be written first, into the
//!   configured directory;
//! - **text** that merely names a path is the weakest source and is used
//!   only when nothing better is present.
//!
//! The save directory comes from `EDAMAME_IMAGES_DIR` when set, then
//! `ImagesConfig::save_dir` — empty meaning the platform directory beside
//! edamame's logs — resolved relative to the open document.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::clipboard::{Bitmap, ClipboardData};
use crate::config::Config;

/// Environment variable that overrides the configured image save directory.
pub const IMAGES_DIR_ENV: &str = "EDAMAME_IMAGES_DIR";

/// Extensions edamame's image loader accepts, kept in step with the `image`
/// crate features in `Cargo.toml` plus SVG.
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "svg"];

// ── Policy ────────────────────────────────────────────────────────────────

/// What the clipboard offers, best-first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection<'a> {
    /// A file the user copied in a file manager: reference it in place.
    /// Never copied, never read.
    File(String),
    /// A bitmap.  Markdown cannot point at pixels, so this one has to be
    /// written before it can be referenced.
    Bitmap(&'a Bitmap),
    /// Text that names an image file.
    Path(String),
}

/// What a paste should do, once the snapshot has been interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Insert `![](destination)` at the cursor.
    Insert(String),
    /// The clipboard holds no image.  The caller decides what that
    /// means: the plain paste falls through to text, the palette command
    /// reports it.
    NoImage,
    /// The clipboard held an image that could not be used.  Carries the
    /// user-facing reason.
    Failed(String),
}

/// Where a bitmap would be written, and what a relative directory
/// resolves against.
pub struct SaveTarget<'a> {
    /// The configured directory: absolute, or relative to the document.
    pub dir: &'a str,
    /// The open document; `None` for a buffer that has never been saved.
    pub doc_path: Option<&'a Path>,
}

/// The best image the snapshot offers, or `None` when it holds none.
///
/// This function *is* the priority policy — there is no second place
/// where the order is written down.
///
/// Only the first entry naming an image is used: a multi-file selection
/// has no single obvious answer, and inserting three references is not
/// what the chord promised.  Directories and non-image files are skipped
/// rather than treated as a refusal, so a mixed selection still finds its
/// image.
pub fn select(data: &ClipboardData) -> Option<Selection<'_>> {
    if let Some(path) = first_image_path(&data.files) {
        return Some(Selection::File(path));
    }
    if let Some(bitmap) = &data.bitmap {
        return Some(Selection::Bitmap(bitmap));
    }
    data.text
        .as_deref()
        .and_then(normalize_image_path)
        .map(Selection::Path)
}

/// The Markdown destination for whatever image the clipboard holds — the
/// whole clipboard-to-Markdown policy, in one call.
pub fn destination(data: &ClipboardData, target: &SaveTarget<'_>) -> Outcome {
    match select(data) {
        None => Outcome::NoImage,
        Some(Selection::File(path)) | Some(Selection::Path(path)) => Outcome::Insert(path),
        Some(Selection::Bitmap(bitmap)) => match save_image(bitmap, target.dir, target.doc_path) {
            Ok(link) => Outcome::Insert(link),
            Err(e) => Outcome::Failed(e),
        },
    }
}

/// Whether a *plain* paste should stay an ordinary text paste.
///
/// `Ctrl-V` is the everyday chord: text on the clipboard means the user
/// copied text, whatever else is on there with it, and quietly embedding
/// an image instead would be the surprising behaviour.  The palette's
/// "paste image" command skips this check — it is the explicit request.
pub fn plain_paste_wants_text(data: &ClipboardData) -> bool {
    data.text.as_deref().is_some_and(|text| !text.is_empty())
}

/// The first entry that names an image file, as a Markdown destination.
fn first_image_path(paths: &[PathBuf]) -> Option<String> {
    paths
        .iter()
        .find_map(|path| normalize_image_path(&path.to_string_lossy()))
}

/// The image save directory from the environment, if set and non-empty.
pub fn images_dir_from_env() -> Option<String> {
    std::env::var(IMAGES_DIR_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// Normalize a copied path into a Markdown-safe form, returning `None`
/// when it does not look like an image file.
///
/// Handles the Windows "Copy as path" surrounding quotes, `file://` URIs
/// (including the `file:///C:/…` drive form), and backslash separators.
/// Percent-decoding of `file://` URIs is intentionally not done — the
/// dominant source here is Windows "Copy as path", which emits raw paths.
pub fn normalize_image_path(raw: &str) -> Option<String> {
    let mut s = raw.trim().to_owned();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s = s[1..s.len() - 1].to_owned();
    }
    if let Some(rest) = s.strip_prefix("file://") {
        s = rest.to_owned();
        // `file:///C:/…` → `C:/…`: strip the leading slash before a drive.
        let bytes = s.as_bytes();
        if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' {
            s = s[1..].to_owned();
        }
    }
    s = s.replace('\\', "/");
    if !is_image_ext(&s) {
        return None;
    }
    Some(s)
}

fn is_image_ext(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

// ── Saving a bitmap ───────────────────────────────────────────────────────

/// Encode RGBA pixels to PNG bytes.
fn encode_png(bitmap: &Bitmap) -> Result<Vec<u8>, String> {
    let img = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.rgba.clone())
        .ok_or_else(|| "invalid RGBA buffer".to_owned())?;
    let dynamic = image::DynamicImage::ImageRgba8(img);
    let mut buf = Vec::new();
    dynamic
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| format!("PNG encode failed: {e}"))?;
    Ok(buf)
}

/// Resolve a save-directory string to a concrete directory.
///
/// An empty `dir` — the shipped default — means [`images_dir_default`].
/// Absolute paths pass through; relative paths (a leading `./` is
/// dropped) resolve against the open document's parent.  An unsaved
/// buffer has no parent, so relative resolution fails.
pub fn resolve_save_dir(dir: &str, doc_path: Option<&Path>) -> Result<PathBuf, String> {
    let configured = if dir.trim().is_empty() {
        images_dir_default()
    } else {
        dir.to_owned()
    };
    let dir = configured.strip_prefix("./").unwrap_or(configured.as_str());
    let dir = Path::new(dir);
    if dir.is_absolute() {
        return Ok(dir.to_path_buf());
    }
    let parent = doc_path.and_then(|p| p.parent()).ok_or_else(|| {
        "save the file first — there is no document path to resolve the image directory against"
            .to_owned()
    })?;
    Ok(parent.join(dir))
}

/// The platform image directory: `<data dir>/edamame/images`, matching
/// where edamame writes its logs.  Falls back to `./images` when no data
/// directory is available.
fn images_dir_default() -> String {
    Config::log_dir()
        .map(|dir| dir.join("images").to_string_lossy().into_owned())
        .unwrap_or_else(|| "./images".to_owned())
}

/// A non-colliding `image-<millis>.png` path inside `dir`.
fn unique_image_path(dir: &Path) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut path = dir.join(format!("image-{millis}.png"));
    let mut n = 1u32;
    while path.exists() {
        path = dir.join(format!("image-{millis}-{n}.png"));
        n += 1;
    }
    path
}

/// Save `bitmap` into `dir` and return the Markdown link path to reference
/// it — always the absolute path, forward-slash separated, so the link
/// stays valid regardless of the terminal's working directory.
pub fn save_image(bitmap: &Bitmap, dir: &str, doc_path: Option<&Path>) -> Result<String, String> {
    let dir = resolve_save_dir(dir, doc_path)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create image directory {}: {e}", dir.display()))?;
    let path = unique_image_path(&dir);
    let bytes = encode_png(bitmap)?;
    std::fs::write(&path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let abs = std::fs::canonicalize(&path)
        .map_err(|e| format!("cannot resolve {}: {e}", path.display()))?;
    Ok(to_forward_slash(&abs))
}

/// Absolute path as a forward-slash string, dropping the `\\?\` verbatim
/// prefix `std::fs::canonicalize` can emit on Windows.
fn to_forward_slash(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    s.strip_prefix("//?/").unwrap_or(&s).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_handles_quotes_file_uri_and_backslashes() {
        assert_eq!(
            normalize_image_path("\"C:\\Users\\me\\pic.PNG\"").as_deref(),
            Some("C:/Users/me/pic.PNG")
        );
        assert_eq!(
            normalize_image_path("file:///C:/Users/me/pic.png").as_deref(),
            Some("C:/Users/me/pic.png")
        );
        assert_eq!(
            normalize_image_path("file:///home/me/pic.jpg").as_deref(),
            Some("/home/me/pic.jpg")
        );
    }

    #[test]
    fn normalize_rejects_non_image_paths() {
        assert_eq!(normalize_image_path("/tmp/notes.txt"), None);
        assert_eq!(normalize_image_path("just some text"), None);
    }

    #[test]
    fn resolve_absolute_relative_and_unsaved() {
        let abs_input = if cfg!(windows) { "C:\\img" } else { "/tmp/img" };
        let abs = resolve_save_dir(abs_input, None).unwrap();
        assert!(abs.is_absolute());

        let doc = Path::new("docs").join("guide.md");
        let rel = resolve_save_dir("./images", Some(&doc)).unwrap();
        assert_eq!(rel, Path::new("docs").join("images"));

        assert!(resolve_save_dir("./images", None).is_err());
    }

    #[test]
    fn empty_save_dir_means_the_platform_directory() {
        // The shipped default leaves `save_dir` unset; it means the platform directory
        // (beside edamame's logs), never a path relative to the open document.
        let doc = Path::new("docs").join("guide.md");
        assert_eq!(
            resolve_save_dir("", Some(&doc)).unwrap(),
            resolve_save_dir("", None).unwrap()
        );
        assert!(resolve_save_dir("", None).unwrap().ends_with("images"));
    }

    #[test]
    fn png_roundtrip() {
        let bitmap = Bitmap {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
        };
        let bytes = encode_png(&bitmap).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(), 2);
        assert_eq!(decoded.height(), 1);
    }
}
