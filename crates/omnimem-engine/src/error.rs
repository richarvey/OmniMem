use omnimem_store::StoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    /// Bad input. The message is shown to the caller, as a 6.x `ValueError`
    /// raised from a tool was.
    #[error("{0}")]
    Invalid(String),

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error("embedding failed: {0}")]
    Embedding(String),

    #[error("{0}")]
    Io(String),
}

pub(crate) fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::Invalid(message.into())
}
