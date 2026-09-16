use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("invalid key: {0}")]
    InvalidKey(String),

    /// A field name that can't be quoted into a JSON path for a filtered
    /// listing. Every field the engine reads is a plain identifier.
    #[error("invalid field name: {0}")]
    InvalidField(String),

    #[error("{key} holds a {actual}, not a {expected}")]
    WrongType {
        key: String,
        expected: &'static str,
        actual: &'static str,
    },

    #[error("field {field} of {key} is not an integer")]
    NotAnInteger { key: String, field: String },

    #[error("vector has {got} dimensions; the store is built for {expected}")]
    DimensionMismatch { expected: usize, got: usize },

    #[error(
        "this database was written by a newer OmniMem (schema {found}, this build supports up to {supported})"
    )]
    SchemaTooNew { found: i64, supported: i64 },

    #[error("stored JSON is corrupt for {key}: {source}")]
    CorruptRecord {
        key: String,
        source: serde_json::Error,
    },

    #[error("backup: {0}")]
    Backup(String),

    #[error("could not read or write {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },

    #[error("embedding failed: {0}")]
    Embedding(String),
}
