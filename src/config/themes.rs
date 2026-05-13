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
pub mod catppuccin_latte;
pub mod dracula;
pub mod edamame;
pub mod everforest;
pub mod github_dark;
pub mod github_light;
pub mod gruvbox;
pub mod gruvbox_light;
pub mod kanagawa;
pub mod monokai;
pub mod nord;
pub mod one_dark;
pub mod orng;
pub mod rainbow;
pub mod rose_pine;
pub mod rose_pine_dawn;
pub mod solarized_dark;
pub mod solarized_light;
pub mod synthwave84;
pub mod tokyo_night;
pub mod tokyo_night_day;
pub mod zenburn;
