//! OSC 22 pointer-shape feedback extracted from `app.rs` in Step 2 of
//! `refactor-app.md`.

use crate::terminal::{set_pointer_shape, PointerShape};

use super::App;

impl App {
    /// Emit an OSC 22 escape to change the terminal pointer shape, but only
    /// if the requested shape differs from the last one we asked for.
    pub(super) fn update_pointer_shape(&mut self, shape: PointerShape) {
        if self.last_pointer_shape == shape {
            return;
        }
        // `set_pointer_shape` writes the escape straight to stdout,
        // which libtest does not capture — unit tests driving mouse
        // events would spray OSC 22 bytes into the test output.
        if !cfg!(test) {
            set_pointer_shape(shape);
        }
        self.last_pointer_shape = shape;
    }
}
