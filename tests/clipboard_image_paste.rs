//! The clipboard-image-paste decision table.
//!
//! These pin the whole clipboard-to-Markdown policy (`image::paste`) from
//! the outside: what wins when a clipboard holds several kinds of payload,
//! and what destination each kind produces. They were written before the
//! implementation, so the table — not the code — decided the shape, and a
//! change to the priority order has to argue with them.
//!
//! ## The simulated environment
//!
//! No test here touches the OS clipboard. The environment is a
//! [`ClipboardData`] snapshot — the payload model every platform maps
//! onto — constructed to mirror what the real clipboard holds, with each
//! case's shape measured on Windows 11 (build 26200):
//!
//! | Real action | Formats on the clipboard (measured) | Payload |
//! |---|---|---|
//! | Explorer `Ctrl+C` on a file | `CF_HDROP` + shell-private formats, **no text format at all** | `files` only |
//! | Snipping Tool / `Clipboard::SetImage` | `CF_BITMAP` + `CF_DIB` + `CF_DIBV5`, no text | `bitmap` only |
//! | `Ctrl+C` on a path in a text editor | `CF_UNICODETEXT` + `CF_TEXT` | `text` only |
//!
//! Each test names the real action it simulates, so a failure reads as a
//! user-visible behaviour rather than as a unit of code.
//!
//! Determinism: the save side is a real `tempfile::tempdir`, so the
//! "no copy" cases assert on an empty directory — a structural fact —
//! rather than on a mock remembering it was not called.

use std::path::{Path, PathBuf};

use edamame::clipboard::{Bitmap, ClipboardData};
use edamame::image::paste::{
    destination, plain_paste_wants_text, select, Outcome, SaveTarget, Selection,
};

// ── Payload builders: one per measured clipboard shape ───────────────────────

/// Explorer's `Ctrl+C` on one or more files: a file list, and nothing else.
///
/// Measured on Windows 11 26200 — the clipboard carries `CF_HDROP` plus
/// shell-private formats (`FileNameW`, `Shell IDList Array`, …) and
/// **no** `CF_UNICODETEXT`/`CF_TEXT`, which is exactly why a text-only
/// reader sees an empty clipboard here.
fn explorer_file_copy(paths: &[&str]) -> ClipboardData {
    ClipboardData {
        files: paths.iter().map(PathBuf::from).collect(),
        ..ClipboardData::default()
    }
}

/// A screenshot: pixels, and nothing else.
fn screenshot(rgba: Vec<u8>) -> ClipboardData {
    ClipboardData {
        bitmap: Some(Bitmap {
            width: 1,
            height: 1,
            rgba,
        }),
        ..ClipboardData::default()
    }
}

/// Text on the clipboard, as a text editor's `Ctrl+C` leaves it.
fn copied_text(text: &str) -> ClipboardData {
    ClipboardData {
        text: Some(text.to_owned()),
        ..ClipboardData::default()
    }
}

/// Where a screenshot would be written: the configured directory plus the
/// open document (a relative directory resolves against it).
fn target<'a>(dir: &'a str, doc: Option<&'a Path>) -> SaveTarget<'a> {
    SaveTarget { dir, doc_path: doc }
}

/// Every entry a directory holds — the observable proving whether a paste
/// copied anything.  A missing directory reads as "nothing was written".
fn entries(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

// ── File-list payloads: the file manager's Ctrl+C ───────────────────────────

#[test]
fn explorer_file_copy_references_the_original_file_in_place() {
    // The point of the feature: the copied file is referenced where it
    // lies — absolute, forward slashes, no copy anywhere.
    let dir = tempfile::tempdir().expect("tempdir");
    let data = explorer_file_copy(&[r"C:\Users\me\shot.png"]);

    assert_eq!(
        select(&data),
        Some(Selection::File("C:/Users/me/shot.png".to_owned())),
        "an Explorer file copy must select the file itself"
    );
    assert_eq!(
        destination(&data, &target(&dir.path().to_string_lossy(), None)),
        Outcome::Insert("C:/Users/me/shot.png".to_owned())
    );
    assert_eq!(
        entries(dir.path()),
        Vec::<String>::new(),
        "referencing a file must not write anything"
    );
}

#[test]
fn a_multi_file_selection_uses_the_first_entry_only() {
    let data = explorer_file_copy(&[r"C:\a.png", r"C:\b.jpg", r"C:\c.webp"]);

    assert_eq!(
        select(&data),
        Some(Selection::File("C:/a.png".to_owned())),
        "a multi-selection inserts one reference, not three"
    );
}

#[test]
fn a_non_image_entry_does_not_mask_a_later_image() {
    // A mixed selection still has an obvious answer: the extension
    // allowlist decides, not the position of the first entry.
    let data = explorer_file_copy(&[r"C:\notes.txt", r"C:\shot.png"]);

    assert_eq!(
        select(&data),
        Some(Selection::File("C:/shot.png".to_owned()))
    );
}

#[test]
fn a_copied_folder_is_not_an_image() {
    // Explorer's Ctrl+C on a directory yields a one-entry file list naming
    // the directory.  It must fall through to "no image", never insert a
    // reference to a folder.  The target is a real directory because
    // `destination` is a function that writes: a case that expects no
    // write still owes it somewhere harmless to write.
    let dir = tempfile::tempdir().expect("tempdir");
    let data = explorer_file_copy(&[r"C:\Users\me\Pictures"]);

    assert_eq!(select(&data), None);
    assert_eq!(
        destination(&data, &target(&dir.path().to_string_lossy(), None)),
        Outcome::NoImage
    );
    assert_eq!(entries(dir.path()), Vec::<String>::new());
}

// ── Precedence between payload kinds ────────────────────────────────────────

#[test]
fn a_file_list_beats_a_bitmap_so_nothing_is_copied() {
    // Some image viewers put both the pixels and the source file on the
    // clipboard.  The file wins: referencing it copies nothing, and not
    // copying is the thing this feature exists for.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut data = screenshot(vec![255, 0, 0, 255]);
    data.files = vec![PathBuf::from("C:/Users/me/shot.png")];

    assert_eq!(
        select(&data),
        Some(Selection::File("C:/Users/me/shot.png".to_owned()))
    );
    assert_eq!(
        destination(&data, &target(&dir.path().to_string_lossy(), None)),
        Outcome::Insert("C:/Users/me/shot.png".to_owned())
    );
    assert_eq!(
        entries(dir.path()),
        Vec::<String>::new(),
        "the bitmap must be ignored while a file list is present"
    );
}

#[test]
fn a_file_list_beats_copied_text() {
    // Text that happens to name an image is the weakest source; a real
    // file list is authoritative.
    let mut data = explorer_file_copy(&["C:/a.png"]);
    data.text = Some("C:/b.png".to_owned());

    assert_eq!(select(&data), Some(Selection::File("C:/a.png".to_owned())));
}

#[test]
fn a_bitmap_beats_copied_text() {
    let mut data = screenshot(vec![255, 0, 0, 255]);
    data.text = Some("C:/b.png".to_owned());

    assert_eq!(
        select(&data),
        Some(Selection::Bitmap(data.bitmap.as_ref().expect("bitmap")))
    );
}

// ── Bitmap payloads: the screenshot ─────────────────────────────────────────

#[test]
fn a_screenshot_is_saved_and_referenced_by_its_absolute_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = dir.path().join("notes.md");
    let data = ClipboardData {
        bitmap: Some(Bitmap {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
        }),
        ..ClipboardData::default()
    };

    let outcome = destination(&data, &target("./images", Some(&doc)));
    let Outcome::Insert(link) = outcome else {
        panic!("expected an inserted reference, got {outcome:?}");
    };
    assert!(
        Path::new(&link).is_absolute(),
        "the reference is absolute so it survives another working directory: {link}"
    );

    let saved = entries(&dir.path().join("images"));
    assert_eq!(
        saved.len(),
        1,
        "the screenshot is written exactly once: {saved:?}"
    );
    assert!(saved[0].ends_with(".png"), "saved as PNG: {saved:?}");
    assert!(
        link.contains(&saved[0]),
        "the reference names the file that was written: {link} vs {saved:?}"
    );
}

#[test]
fn a_screenshot_whose_directory_cannot_be_resolved_reports_a_failure() {
    // A relative `save_dir` needs a document to resolve against; an
    // unsaved buffer has none.  That is a reportable failure, not a
    // silent no-op — the user must hear why nothing appeared.
    //
    // The relative literal is the point of the case, and it never reaches
    // the filesystem: `resolve_save_dir` refuses before anything is
    // created.
    let data = screenshot(vec![255, 0, 0, 255]);

    match destination(&data, &target("./images", None)) {
        Outcome::Failed(message) => assert!(
            message.contains("save the file first"),
            "the message must say what to do: {message}"
        ),
        other => panic!("expected Failed, got {other:?}"),
    }
}

// ── Text payloads ───────────────────────────────────────────────────────────

#[test]
fn a_copied_path_in_text_is_normalized_before_it_is_inserted() {
    // Windows "Copy as path" is quoted; a copied path may carry
    // backslashes.  Neither may reach the Markdown destination verbatim.
    assert_eq!(
        select(&copied_text(r#""C:\Users\me\shot.png""#)),
        Some(Selection::Path("C:/Users/me/shot.png".to_owned()))
    );
    assert_eq!(
        select(&copied_text("file:///C:/Users/me/shot.png")),
        Some(Selection::Path("C:/Users/me/shot.png".to_owned()))
    );
}

#[test]
fn copied_text_that_is_not_an_image_path_is_not_an_image() {
    assert_eq!(select(&copied_text("just some prose")), None);
    assert_eq!(select(&copied_text(r"C:\notes.txt")), None);
}

#[test]
fn a_copied_image_url_is_taken_at_face_value() {
    // The rule is the extension, nothing more: a URL ending in an image
    // extension is an image destination like any other.  A *plain* paste
    // still keeps a copied URL as text (`plain_paste_wants_text`), so
    // only the explicit command embeds it — and edamame's remote-image
    // policy still governs whether it is ever fetched.
    assert_eq!(
        select(&copied_text("https://example.com/a.png")),
        Some(Selection::Path("https://example.com/a.png".to_owned()))
    );
}

// ── Empty payloads ──────────────────────────────────────────────────────────

#[test]
fn an_empty_clipboard_offers_no_image() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = ClipboardData::default();

    assert_eq!(select(&data), None);
    assert_eq!(
        destination(&data, &target(&dir.path().to_string_lossy(), None)),
        Outcome::NoImage
    );
    assert_eq!(entries(dir.path()), Vec::<String>::new());
}

// ── The plain (Ctrl-V) paste's text-first rule ──────────────────────────────

#[test]
fn plain_paste_lets_text_win_so_ordinary_pasting_is_untouched() {
    // `Action::Paste` is the everyday paste: non-empty text means the user
    // copied text, whatever else the clipboard also holds.
    let mut data = screenshot(vec![255, 0, 0, 255]);
    data.text = Some("alpha".to_owned());

    assert!(
        plain_paste_wants_text(&data),
        "text on the clipboard must keep the plain paste ordinary"
    );
}

#[test]
fn plain_paste_falls_through_to_the_image_when_there_is_no_text() {
    assert!(!plain_paste_wants_text(&explorer_file_copy(&["C:/a.png"])));
    assert!(!plain_paste_wants_text(&screenshot(vec![255, 0, 0, 255])));
    assert!(!plain_paste_wants_text(&ClipboardData::default()));
}
