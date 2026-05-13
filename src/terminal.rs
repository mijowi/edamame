pub mod capabilities;
pub mod setup;

pub use capabilities::{Capabilities, ColorDepth, ImageProtocol};
pub use setup::{
    enable_mouse, re_enter, restore, set_pointer_shape, setup, PointerShape, TerminalSetup,
};
