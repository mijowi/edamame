//! A pasted screenshot, from the outside: pixels encoded, stored where the prompt resolves, and
//! loaded back through the real image loader the way the document's reference is.  No test here
//! touches the OS clipboard; the flow around it is covered in `app::clipboard::tests`.

use edamame::clipboard::Bitmap;
use edamame::config::RemoteImagePolicy;
use edamame::image::paste::{encode_png, resolve_destination, write};

#[test]
fn a_stored_screenshot_decodes_through_the_loader() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = dir.path().join("notes.md");
    let bitmap = Bitmap {
        width: 2,
        height: 1,
        rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
    };
    let png = encode_png(&bitmap).expect("encode");
    let dest = resolve_destination("images/shot.png", dir.path()).expect("valid");
    write(&png, &dest.target).expect("write");

    // Resolved the way a document's own reference is: relative to the document.
    let loaded = edamame::image::resolve(
        &dest.link,
        Some(&doc),
        RemoteImagePolicy::Never,
        false,
        None,
        None,
    )
    .expect("decode the stored image");
    assert_eq!((loaded.image.width(), loaded.image.height()), (2, 1));
}
