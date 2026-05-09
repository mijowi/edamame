//! Modal subsystem: trait, dispatch stack, and individual modal
//! implementations.
//!
//! The App's only handle to a modal is a [`Box<dyn Modal>`] on the
//! [`ModalStack`]; pushing a modal makes it the topmost overlay,
//! popping returns it.  Each modal implementation lives in its own
//! file under `src/app/modal/`.

pub mod stack;
pub mod types;

pub mod cheat_sheet;
pub mod command_palette;
pub mod config_warning;
pub mod dirty_guard;
pub mod images_enabled;
pub mod insert_table;
pub mod keybinds;
pub mod quit_confirm;
pub mod remote_image;
pub mod save_copy;
pub mod settings;
pub mod startup_notice;
pub mod width_injection;

pub use stack::ModalStack;
#[allow(unused_imports)]
pub use types::ModalKind;
pub use types::{Modal, ModalOutcome, ModalRenderCtx};

pub use cheat_sheet::CheatSheetModal;
pub use command_palette::CommandPaletteModal;
pub use config_warning::ConfigWarningModal;
pub use dirty_guard::DirtyGuardModal;
pub use images_enabled::ImagesEnabledPromptModal;
pub use insert_table::InsertTableModal;
pub use keybinds::KeybindsOverlayModal;
pub use quit_confirm::QuitConfirmModal;
pub use remote_image::RemoteImagePromptModal;
pub use save_copy::SaveCopyModal;
pub use settings::SettingsOverlayModal;
pub use startup_notice::StartupNoticeModal;
pub use width_injection::WidthInjectionWarning;
