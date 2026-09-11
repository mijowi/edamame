//! Integration coverage for the clipboard image paste pipeline: a raw
//! RGBA screenshot is encoded, saved into a document-relative image
//! directory, and the written file decodes through the real loader.

use edamame::clipboard::Bitmap;
use edamame::config::RemoteImagePolicy;
use edamame::image::paste::save_image;
use edamame::image::resolve;

#[test]
fn saved_screenshot_decodes_through_the_loader() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = dir.path().join("notes.md");

    let bitmap = Bitmap {
        width: 2,
        height: 1,
        rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
    };

    // The save dir resolves against the document's parent directory, and
    // the returned link is the absolute path to the written file.
    let link = save_image(&bitmap, "./images", Some(&doc)).expect("save");
    assert!(link.ends_with(".png"), "unexpected link: {link}");
    assert!(link.contains("images/image-"), "unexpected link: {link}");
    assert!(
        std::path::Path::new(&link).is_absolute(),
        "link must be absolute: {link}"
    );

    // The written file decodes through the real image loader.
    let loaded = resolve(&link, None, RemoteImagePolicy::Never, false, None, None)
        .expect("decode saved image");
    assert_eq!(loaded.image.width(), 2);
    assert_eq!(loaded.image.height(), 1);
}
