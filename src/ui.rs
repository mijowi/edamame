pub mod about;
pub mod bottom_region;
pub mod button_row;
pub mod cap_summary;
pub mod command_palette;
pub mod content_width;
pub mod controls;
pub mod cursor;
pub mod diff_intro_modal;
pub mod diff_view;
pub mod dim;
pub mod editor_view;
pub mod export_html_modal;
pub mod export_theme_modal;
pub mod gutter;
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
pub mod search_modal;
pub mod searchable_list;
pub mod section_picker;
pub mod settings_overlay;
pub mod status_bar;
pub mod table_view;
pub mod text_input;
pub mod theme_picker;
pub mod welcome;

pub use bottom_region::{hint_line_for, BottomRegion, HintChord, HintContent, HintSet};
pub use cap_summary::{build_cap_lines, CapSummary};
pub use diff_intro_modal::{DiffIntroResponse, DiffIntroState, DiffIntroView};
#[allow(unused_imports)]
pub use diff_view::{DiffView, DiffViewState};
pub use editor_view::{EditorView, EditorViewState};
pub use export_html_modal::{ExportChoices, ExportHtmlResponse, ExportHtmlState, ExportHtmlView};
pub use export_theme_modal::{ExportThemeResponse, ExportThemeState, ExportThemeView};
pub use gutter::split_gutter;
pub use image_view::ImageLayoutSnapshot;
pub use insert_table_modal::{InsertTableResponse, InsertTableState, InsertTableView};
pub use keybinds_overlay::KeybindsResponse;
pub use keybinds_overlay::{KeybindsState, KeybindsView};
pub use link_view::LinkLayoutSnapshot;
pub use markdown_cheat_sheet::body_lines as markdown_cheat_sheet_body;
pub use modal::{ModalButton, ModalResponse, ModalState, ModalView};
pub use scroll_container::ModalKind;
// Used by integration tests in tests/ui.rs.
#[allow(unused_imports)]
pub use rendered_view::{RenderedView, RenderedViewState};
pub use save_copy_modal::{default_save_as_path, SaveCopyResponse, SaveCopyState, SaveCopyView};
#[allow(unused_imports)]
pub use scrollbar::{
    position_for_click, position_for_drag, thumb_range, Scrollbar, ScrollbarMetrics,
};
pub use search_modal::{SearchModalResponse, SearchModalState, SearchModalView};
pub use section_picker::HeadingEntry;
pub use settings_overlay::SettingsResponse;
pub use settings_overlay::{SettingsState, SettingsView};
pub use table_view::DropIndicator;
pub use text_input::sanitize_paste;
pub use welcome::{WelcomeResponse, WelcomeState, WelcomeView};
