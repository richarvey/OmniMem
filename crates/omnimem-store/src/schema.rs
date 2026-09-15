//! Schema creation and versioning (`PRAGMA user_version`).

use rusqlite::{Connection, OptionalExtension, params};
use ulid::Ulid;

use crate::{Result, StoreError};

pub(crate) const SCHEMA_VERSION: i64 = 2;

/// Version 2: the enrichment queue, durable where the Valkey list was not.
const V2: &str = r#"
CREATE TABLE enrich_queue (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    payload    TEXT NOT NULL CHECK (json_valid(payload)),
    created_at REAL NOT NULL
) STRICT;
"#;

/// Version 1.
///
/// `memories.fields` is the record, a JSON object of string fields. Every
/// other memory column is generated from it: SQLite keeps them in step on
/// every write, and the indexes on them are what filtered search and the
/// list views use.
const V1: &str = r#"
CREATE TABLE memories (
    key          TEXT PRIMARY KEY NOT NULL,
    namespace    TEXT NOT NULL,
    fields       TEXT NOT NULL CHECK (json_valid(fields)),
    state        TEXT GENERATED ALWAYS AS (json_extract(fields, '$.state')) VIRTUAL,
    project      TEXT GENERATED ALWAYS AS (json_extract(fields, '$.project')) VIRTUAL,
    project_name TEXT GENERATED ALWAYS AS (json_extract(fields, '$.project_name')) VIRTUAL,
    feed_name    TEXT GENERATED ALWAYS AS (json_extract(fields, '$.feed_name')) VIRTUAL,
    created_at   REAL GENERATED ALWAYS AS (CAST(json_extract(fields, '$.created_at') AS REAL)) VIRTUAL,
    updated_at   REAL GENERATED ALWAYS AS (CAST(json_extract(fields, '$.updated_at') AS REAL)) VIRTUAL,
    content_hash TEXT GENERATED ALWAYS AS (json_extract(fields, '$.content_hash')) VIRTUAL
) STRICT;

CREATE INDEX memories_namespace_state   ON memories (namespace, state);
CREATE INDEX memories_namespace_project ON memories (namespace, project);
CREATE INDEX memories_namespace_pname   ON memories (namespace, project_name);
CREATE INDEX memories_namespace_updated ON memories (namespace, updated_at);
CREATE INDEX memories_feed              ON memories (feed_name);
CREATE INDEX memories_content_hash      ON memories (content_hash);

CREATE TABLE vectors (
    key  TEXT PRIMARY KEY NOT NULL REFERENCES memories (key) ON DELETE CASCADE,
    data BLOB NOT NULL
) STRICT;

-- Everything that was a non-memory Valkey key: meta:* hashes and strings,
-- log:recall:* hashes, topics:suppressed, caches. `value` is JSON: an object
-- of strings for a hash, a sorted array for a set, a string for a string.
CREATE TABLE kv (
    key        TEXT PRIMARY KEY NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('hash', 'set', 'string')),
    value      TEXT NOT NULL CHECK (json_valid(value)),
    expires_at REAL
) STRICT;

CREATE INDEX kv_expires ON kv (expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE store_meta (
    name  TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;
"#;

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if version < 1 {
        conn.execute_batch(&format!("BEGIN; {V1} PRAGMA user_version = 1; COMMIT;"))?;
    }
    if version < 2 {
        conn.execute_batch(&format!("BEGIN; {V2} PRAGMA user_version = 2; COMMIT;"))?;
    }
    Ok(())
}

/// This store's node identity, the v7 `origin_id` for memories written here.
/// Created once and never changed.
pub(crate) fn origin_id(conn: &Connection) -> Result<String> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT value FROM store_meta WHERE name = 'origin_id'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = Ulid::generate().to_string();
    conn.execute(
        "INSERT INTO store_meta (name, value) VALUES ('origin_id', ?1)",
        params![id],
    )?;
    Ok(id)
}
