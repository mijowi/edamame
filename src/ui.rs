pub mod bottom_region;
pub mod button_row;
pub mod command_palette;
pub mod content_width;
pub mod editor_view;
pub mod image_view;
pub mod insert_table_modal;
pub mod keybinds_overlay;
pub mod line_render;
pub mod link_view;
pub mod markdown_cheat_sheet;
pub mod modal;
pub mod modal_row;
pub mod overlay_nav;
pub mod preview;
pub mod raw_view;
pub mod rendered_view;
pub mod save_copy_modal;
pub mod scroll_container;
pub mod scrollbar;
pub mod settings_overlay;
pub mod status_bar;
pub mod table_view;

pub use bottom_region::{hint_line_for, BottomRegion, HintChord, HintContent};
pub use command_palette::{PaletteResponse, PaletteState, PaletteView};
pub use editor_view::{EditorView, EditorViewState};
pub use image_view::ImageLayoutSnapshot;
pub use insert_table_modal::{InsertTableResponse, InsertTableState, InsertTableView};
pub use keybinds_overlay::KeybindsResponse;
pub use keybinds_overlay::{KeybindsState, KeybindsView};
pub use link_view::LinkLayoutSnapshot;
pub use markdown_cheat_sheet::body_lines as markdown_cheat_sheet_body;
pub use modal::{ModalButton, ModalResponse, ModalState, ModalView};
// Used by integration tests in tests/ui.rs.
#[allow(unused_imports)]
pub use rendered_view::{RenderedView, RenderedViewState};
pub use save_copy_modal::{default_copy_path, SaveCopyResponse, SaveCopyState, SaveCopyView};
#[allow(unused_imports)]
pub use scrollbar::{
    position_for_click, position_for_drag, thumb_range, Scrollbar, ScrollbarMetrics,
};
pub use settings_overlay::SettingsResponse;
pub use settings_overlay::{SettingsState, SettingsView};
pub use table_view::DropIndicator;
