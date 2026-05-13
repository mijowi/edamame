//! Built-in palette constructors.
//!
//! Each submodule defines one shipped theme as a `pub fn palette() ->
//! Palette` returning the fully-populated palette.  The registry in
//! [`super::theme::BUILTIN_THEMES`] references these functions
//! directly; the load path in [`super::readers::read_theme_named`]
//! resolves a theme name to a built-in before falling back to disk.
//!
//! Adding a new theme:
//! 1. Add `pub mod <name>;` below.
//! 2. Create `src/config/themes/<name>.rs` with `pub fn palette() ->
//!    Palette { … }`.
//! 3. Add an entry to `BUILTIN_THEMES` in `theme.rs`.

pub mod util;

pub mod dark_256;
pub mod light_256;
pub mod monochrome_dark;

pub mod ayu;
pub mod catppuccin;
pub mod dracula;
pub mod edamame;
pub mod github_dark;
pub mod gruvbox;
pub mod monokai;
pub mod one_dark;
pub mod orng;
pub mod rainbow;
pub mod rose_pine;
pub mod synthwave84;
pub mod tokyo_night;
