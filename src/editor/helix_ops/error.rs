use crate::editor::vim_ops::TableBreak;
use crate::search::SearchError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HelixError {
    #[error("Can't edit a read-only document")]
    ReadOnly,
    #[error("{}", .0.message())]
    TableRange(TableBreak),
    #[error("Can't paste table separators or newlines into a cell")]
    CellPaste,
    #[error("Can't change table structure with this selection action")]
    TableTransform,
    #[error("Expanded paste exceeds 64 MiB")]
    PasteTooLarge,
    #[error("Can't paste these lines into a table")]
    TablePaste,
    #[error("No search pattern")]
    NoSearchPattern,
    #[error("No matches")]
    NoMatches,
    #[error(transparent)]
    Search(#[from] SearchError),
}
