//! Modal subsystem: trait, dispatch stack, and individual modal
//! implementations.
//!
//! The App's only handle to a modal is a [`Box<dyn Modal>`] on the
//! [`ModalStack`]; pushing a modal makes it the topmost overlay,
//! popping returns it.  Each modal implementation lives in its own
//! file under `src/app/modal/`.

pub mod stack;
pub mod types;

pub mod command_palette;
pub mod config_warning;
pub mod diagrams_enabled;
pub mod diff_intro;
pub mod diff_resolve_confirm;
pub mod dirty_conflict;
pub mod dirty_conflict_discard_confirm;
pub mod dirty_conflict_save_copy;
pub mod dirty_guard;
pub mod export_success;
pub mod export_theme;
pub mod images_enabled;
pub mod insert_table;
pub mod keybinds;
pub mod markdown_cheat_sheet;
pub mod notice;
pub mod quit_confirm;
pub mod remote_image;
pub mod save_copy;
pub mod section_picker;
pub mod settings;
pub mod terminal_capabilities;
pub mod theme_picker;
pub mod welcome;
pub mod width_injection;

pub use stack::ModalStack;
#[allow(unused_imports)]
pub use types::ModalKind;
pub use types::{Modal, ModalOutcome, ModalRenderCtx};

pub use command_palette::CommandPaletteModal;
pub use config_warning::ConfigWarningModal;
pub use diagrams_enabled::DiagramsEnabledPromptModal;
pub use diff_intro::DiffIntroModal;
pub use diff_resolve_confirm::DiffResolveConfirmModal;
pub use dirty_conflict::DirtyConflictModal;
pub use dirty_guard::DirtyGuardModal;
pub use export_success::ExportSuccessModal;
pub use images_enabled::ImagesEnabledPromptModal;
pub use insert_table::InsertTableModal;
pub use keybinds::KeybindsOverlayModal;
pub use markdown_cheat_sheet::CheatSheetModal;
pub use notice::NoticeModal;
pub use quit_confirm::QuitConfirmModal;
pub use remote_image::RemoteImagePromptModal;
pub use save_copy::SaveCopyModal;
pub use section_picker::SectionPickerModal;
pub use settings::SettingsOverlayModal;
pub use terminal_capabilities::TerminalCapabilitiesModal;
pub use theme_picker::ThemePickerModal;
pub use welcome::WelcomeModal;
pub use width_injection::WidthInjectionWarning;
