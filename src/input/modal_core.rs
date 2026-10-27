//! What every modal editing handler shares, so the App holds one value and asks it questions
//! instead of branching on which handler is live.  `ModalInput` is the exclusive session — the
//! App's `Option<ModalInput>` is `None` under the default handler.

pub mod session;

pub use session::{ModalInput, ModeBadge, ModeFace};
