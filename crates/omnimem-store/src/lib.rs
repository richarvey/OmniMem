//! OmniMem's store.
//!
//! One SQLite file holds every record. A memory is kept in the shape every
//! 6.x caller already expects, a map of string fields (what a Valkey hash
//! was), in a JSON column; the columns that get filtered, sorted and counted
//! are generated from that JSON inside SQLite, so they can never drift from
//! the record. Vectors are stored beside the records and loaded into one
//! matrix per namespace, where search is exact: at OmniMem's scale that is a
//! few milliseconds, and deterministic, which the v7 clustering rules need.
//!
//! What goes away with Valkey: the valkey-search tag query quirks, index
//! migrations that can silently fail, and phantom index entries. There is no
//! index to drift from the data.

mod backup;
mod error;
mod kv;
mod migrations;
mod oauth;
mod queue;
mod reindex;
mod schema;
mod store;
mod time;
mod vectors;

pub use backup::{BACKUP_KEY_PREFIXES, BackupFile, ImportReport, read_backup, write_backup};
pub use error::StoreError;
pub use migrations::MigrationReport;
pub use oauth::{OAuthStore, TokenKind, secret_hash};
pub use store::{
    Fields, MAX_KEY_BYTES, MemoryFilter, SearchFilter, SearchHit, Store, VALID_KEY_PREFIXES,
    discovery_text,
};

pub type Result<T, E = StoreError> = std::result::Result<T, E>;
