//! Schema creation and versioning (`PRAGMA user_version`).

use rusqlite::{Connection, OptionalExtension, params};
use ulid::Ulid;

use crate::{Result, StoreError};

/// What the tier-1 contradiction check says in `remember`'s warning.
pub const HEURISTIC_EXPLANATION: &str = "These memories discuss the same topic but contain opposing language (negation patterns detected).";

/// What the tier-1 scan writes as a link's explanation when the model
/// wasn't asked. The engine reuses both texts, so the v5 migration can tell
/// a heuristic link from one the model confirmed.
pub const HEURISTIC_LINK_EXPLANATION: &str = "Opposing language patterns detected.";

pub(crate) const SCHEMA_VERSION: i64 = 5;

/// The trust a record with no `trust` field has: an extracted fact or
/// preference (it carries `enriched_from`) is OmniMem's own conclusion and
/// starts `candidate`; anything else, `accepted`. The generated column and
/// [`crate::trust_of`] both spell this rule, so a filter in SQL and a check
/// in Rust agree, and a 6.x backup or an older 7.x record needs no rewrite.
pub(crate) const TRUST_EXPR: &str = "COALESCE(NULLIF(json_extract(fields, '$.trust'), ''), \
     CASE WHEN COALESCE(json_extract(fields, '$.enriched_from'), '') <> '' \
     THEN 'candidate' ELSE 'accepted' END)";

/// Version 5: `trust`, whether OmniMem believes a memory (#23), beside
/// `state`, which only says whether it is visible. A generated column like
/// `state`, so `MemoryFilter::trusts` runs in SQL. The data part writes the
/// two values the default can't work out: `superseded` where a resolution
/// named a winner, and `disputed` where an unresolved contradiction link
/// was confirmed by the model (a heuristic link, recognised by the fixed
/// explanation it always writes, isn't evidence enough to rank a memory
/// down). `trust_prior` keeps what a disputed memory goes back to.
fn v5() -> String {
    let quote = |text: &str| format!("'{}'", text.replace('\'', "''"));
    let heuristic = format!(
        "{}, {}",
        quote(HEURISTIC_EXPLANATION),
        quote(HEURISTIC_LINK_EXPLANATION)
    );
    format!(
        r"
ALTER TABLE memories ADD COLUMN trust TEXT GENERATED ALWAYS AS ({TRUST_EXPR}) VIRTUAL;
CREATE INDEX memories_namespace_trust ON memories (namespace, trust);

UPDATE memories SET fields = json_set(fields, '$.trust', 'superseded')
 WHERE COALESCE(json_extract(fields, '$.superseded_by'), '') <> ''
   AND trust IN ('accepted', 'candidate');

UPDATE memories
   SET fields = json_set(fields, '$.trust_prior', trust, '$.trust', 'disputed')
 WHERE trust IN ('accepted', 'candidate')
   AND json_valid(json_extract(fields, '$.contradictions'))
   AND EXISTS (
       SELECT 1 FROM json_each(json_extract(fields, '$.contradictions')) AS link
        WHERE COALESCE(json_extract(link.value, '$.resolution.outcome'), 'reopened') = 'reopened'
          AND (json_extract(link.value, '$.verified') = 1
               OR COALESCE(json_extract(link.value, '$.explanation'), '') NOT IN ('', {heuristic}))
   );
"
    )
}

/// Version 4: the audit log, one row per change (see `audit.rs`). `key` is
/// the memory, project or skill key, or the topic for a suppression;
/// `changed_from` and `changed_to` are JSON objects of the fields that moved.
/// Empty until `AUDIT_LOG` is switched on.
const V4: &str = r"
CREATE TABLE audit (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    at           REAL NOT NULL,
    actor        TEXT NOT NULL,
    client       TEXT NOT NULL DEFAULT '',
    action       TEXT NOT NULL,
    key          TEXT NOT NULL,
    changed_from TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(changed_from)),
    changed_to   TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(changed_to)),
    reason       TEXT NOT NULL DEFAULT ''
) STRICT;

CREATE INDEX audit_at  ON audit (at);
CREATE INDEX audit_key ON audit (key, at);
";

/// Version 3: OAuth clients, authorisation codes and tokens, which were
/// `oauth:*` Valkey keys. Codes and tokens are keyed by the SHA-256 of their
/// value, never the value itself.
const V3: &str = r"
CREATE TABLE oauth_clients (
    client_id  TEXT PRIMARY KEY NOT NULL,
    info       TEXT NOT NULL CHECK (json_valid(info)),
    created_at REAL NOT NULL
) STRICT;

CREATE TABLE oauth_codes (
    code_hash  TEXT PRIMARY KEY NOT NULL,
    grant_info TEXT NOT NULL CHECK (json_valid(grant_info)),
    expires_at REAL NOT NULL
) STRICT;

CREATE TABLE oauth_tokens (
    token_hash TEXT NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('access', 'refresh')),
    record     TEXT NOT NULL CHECK (json_valid(record)),
    expires_at REAL NOT NULL,
    PRIMARY KEY (kind, token_hash)
) STRICT;

CREATE INDEX oauth_codes_expires  ON oauth_codes (expires_at);
CREATE INDEX oauth_tokens_expires ON oauth_tokens (expires_at);
";

/// Version 2: the enrichment queue, durable where the Valkey list was not.
const V2: &str = r"
CREATE TABLE enrich_queue (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    payload    TEXT NOT NULL CHECK (json_valid(payload)),
    created_at REAL NOT NULL
) STRICT;
";

/// Version 1.
///
/// `memories.fields` is the record, a JSON object of string fields. Every
/// other memory column is generated from it: SQLite keeps them in step on
/// every write, and the indexes on them are what filtered search and the
/// list views use.
const V1: &str = r"
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
";

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
    if version < 3 {
        conn.execute_batch(&format!("BEGIN; {V3} PRAGMA user_version = 3; COMMIT;"))?;
    }
    if version < 4 {
        conn.execute_batch(&format!("BEGIN; {V4} PRAGMA user_version = 4; COMMIT;"))?;
    }
    if version < 5 {
        let v5 = v5();
        conn.execute_batch(&format!("BEGIN; {v5} PRAGMA user_version = 5; COMMIT;"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_3_database_upgrades_to_4_and_keeps_its_data() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "BEGIN; {V1} {V2} {V3} PRAGMA user_version = 3; COMMIT;"
        ))
        .unwrap();
        conn.execute(
            "INSERT INTO memories (key, namespace, fields) VALUES ('mem:episodic:a', 'episodic', '{\"content\": \"kept\"}')",
            [],
        )
        .unwrap();
        let has_audit = |conn: &Connection| -> bool {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'audit'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
                == 1
        };
        assert!(!has_audit(&conn));

        migrate(&conn).unwrap();

        assert!(has_audit(&conn));
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let kept: String = conn
            .query_row(
                "SELECT json_extract(fields, '$.content') FROM memories WHERE key = 'mem:episodic:a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, "kept");
        // Running it again is a no-op.
        migrate(&conn).unwrap();
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA user_version = 99").unwrap();
        assert!(matches!(
            migrate(&conn),
            Err(StoreError::SchemaTooNew { found: 99, .. })
        ));
    }

    /// #23: a version 4 store gains `trust`, with each memory given the
    /// value its fields call for, and running the migration again changes
    /// nothing.
    #[test]
    fn version_5_gives_each_memory_its_trust() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "BEGIN; {V1} {V2} {V3} {V4} PRAGMA user_version = 4; COMMIT;"
        ))
        .unwrap();
        let link = |explanation: &str, resolved: Option<&str>| {
            let mut link =
                serde_json::json!({"key": "mem:episodic:other", "explanation": explanation});
            if let Some(outcome) = resolved {
                link["resolution"] = serde_json::json!({"outcome": outcome});
            }
            serde_json::json!([link]).to_string()
        };
        let rows = [
            ("plain", serde_json::json!({"content": "x"})),
            (
                "fact",
                serde_json::json!({"content": "x", "enriched_from": "mem:episodic:plain"}),
            ),
            (
                "loser",
                serde_json::json!({"content": "x", "superseded_by": "mem:episodic:plain"}),
            ),
            (
                "confirmed",
                serde_json::json!({"content": "x", "contradictions": link("the model says they conflict", None)}),
            ),
            (
                "factconfirmed",
                serde_json::json!({"content": "x", "enriched_from": "mem:episodic:plain", "contradictions": link("they conflict", None)}),
            ),
            (
                "heuristic",
                serde_json::json!({"content": "x", "contradictions": link(HEURISTIC_LINK_EXPLANATION, None)}),
            ),
            (
                "heuristic2",
                serde_json::json!({"content": "x", "contradictions": link(HEURISTIC_EXPLANATION, None)}),
            ),
            (
                "settled",
                serde_json::json!({"content": "x", "contradictions": link("they conflict", Some("both_valid"))}),
            ),
            (
                "reopened",
                serde_json::json!({"content": "x", "contradictions": link("they conflict", Some("reopened"))}),
            ),
            (
                "explicit",
                serde_json::json!({"content": "x", "trust": "rejected", "superseded_by": "mem:episodic:plain"}),
            ),
        ];
        for (id, fields) in &rows {
            conn.execute(
                "INSERT INTO memories (key, namespace, fields) VALUES (?1, 'episodic', ?2)",
                params![format!("mem:episodic:{id}"), fields.to_string()],
            )
            .unwrap();
        }
        migrate(&conn).unwrap();
        let read = |conn: &Connection| -> Vec<(String, String, Option<String>)> {
            let mut stmt = conn
                .prepare("SELECT key, trust, json_extract(fields, '$.trust_prior') FROM memories ORDER BY key")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let trusts: std::collections::HashMap<String, (String, Option<String>)> = read(&conn)
            .into_iter()
            .map(|(k, t, p)| (k.trim_start_matches("mem:episodic:").to_owned(), (t, p)))
            .collect();
        let expect = [
            ("plain", "accepted", None),
            ("fact", "candidate", None),
            ("loser", "superseded", None),
            ("confirmed", "disputed", Some("accepted")),
            ("factconfirmed", "disputed", Some("candidate")),
            ("heuristic", "accepted", None),
            ("heuristic2", "accepted", None),
            ("settled", "accepted", None),
            ("reopened", "disputed", Some("accepted")),
            ("explicit", "rejected", None),
        ];
        for (id, trust, prior) in expect {
            assert_eq!(
                trusts[id],
                (trust.to_owned(), prior.map(str::to_owned)),
                "{id}"
            );
        }
        // The Rust reading agrees with the column for what it can see.
        let fact: crate::Fields = serde_json::from_value(rows[1].1.clone()).unwrap();
        assert_eq!(crate::trust_of(&fact), "candidate");
        let plain: crate::Fields = serde_json::from_value(rows[0].1.clone()).unwrap();
        assert_eq!(crate::trust_of(&plain), "accepted");

        // Again: the version is current, so nothing runs, and even the data
        // statements on their own would change nothing.
        let before = read(&conn);
        migrate(&conn).unwrap();
        conn.execute_batch(&format!(
            "BEGIN; {} COMMIT;",
            v5().split("UPDATE")
                .skip(1)
                .map(|u| format!("UPDATE{u}"))
                .collect::<String>()
        ))
        .unwrap();
        assert_eq!(read(&conn), before);
    }
}
