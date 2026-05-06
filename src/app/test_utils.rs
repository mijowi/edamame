//! Shared fixtures for binary unit tests under `src/app/`.
//!
//! Step 4 of `refactor-app.md`: the `phase9_flash_tests` and
//! `phase15_insert_table_tests` blocks that previously lived inline in
//! `app.rs` were relocated into the modules they exercise.  Both share
//! a `make_app()` constructor (and the table tests share an
//! `app_with_buffer` seed) — keeping the helpers in one place avoids
//! duplicating the `App::new` boilerplate across the per-module test
//! blocks.

use crate::config::{Config, KeyBindingOverrides, Theme};
use crate::document::Buffer;
use crate::terminal::Capabilities;

use super::App;

/// Build a default-config `App` with no file loaded.  Uses
/// [`Capabilities::default`] so no terminal probing is performed —
/// safe to call from any test thread.
pub(crate) fn make_app() -> App {
    let caps = Capabilities::default();
    let theme_file = (&Theme::default()).into();
    App::new(
        Config::default(),
        KeyBindingOverrides::default(),
        theme_file,
        None,
        caps,
        Vec::new(),
    )
    .expect("build app")
}

/// Build an `App` seeded with `text` and the cursor at byte
/// `cursor_byte` (clamped to the buffer length).
pub(crate) fn app_with_buffer(text: &str, cursor_byte: usize) -> App {
    let mut app = make_app();
    app.editor.buffer = Buffer::from_str(text);
    app.editor.refresh_parsed();
    let total = app.editor.buffer.len_chars();
    let char_off = app
        .editor
        .buffer
        .rope()
        .byte_to_char(cursor_byte.min(app.editor.buffer.contents().len()));
    app.editor.cursor.offset = char_off.min(total);
    app.editor.update_cursor_block();
    app
}
