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

pub(crate) const SCHEMA_VERSION: i64 = 6;

/// The trust a record with no `trust` field has: an extracted fact or
/// preference (it carries `enriched_from`) is OmniMem's own conclusion and
/// starts `candidate`; anything else, `accepted`. The generated column and
/// [`crate::trust_of`] both spell this rule, so a filter in SQL and a check
/// in Rust agree, and a 6.x backup or an older 7.x record needs no rewrite.
pub(crate) const TRUST_EXPR: &str = "COALESCE(NULLIF(json_extract(fields, '$.trust'), ''), \
     CASE WHEN COALESCE(json_extract(fields, '$.enriched_from'), '') <> '' \
     THEN 'candidate' ELSE 'accepted' END)";

/// A memory field read as a time: a number of seconds, with an empty or
/// missing value as NULL.
fn time_field(name: &str) -> String {
    format!("CAST(NULLIF(json_extract(fields, '$.{name}'), '') AS REAL)")
}

/// When OmniMem stopped believing a memory (#26): its `retired_at` field,
/// or for a superseded or rejected record with none (a 6.x import, a 7.0
/// backup), its `updated_at`, which a resolution bumps as it supersedes.
/// [`crate::retired_at_of`] spells the same rule.
pub(crate) fn retired_at_expr() -> String {
    format!(
        "COALESCE({}, CASE WHEN {TRUST_EXPR} IN ('superseded', 'rejected') \
         THEN CAST(json_extract(fields, '$.updated_at') AS REAL) END)",
        time_field("retired_at")
    )
}

/// When a memory stopped being true (#26): its `valid_to` field, or for a
/// superseded record with none, the moment it was superseded.
/// [`crate::valid_to_of`] spells the same rule.
pub(crate) fn valid_to_expr() -> String {
    format!(
        "COALESCE({}, CASE WHEN {TRUST_EXPR} = 'superseded' \
         THEN CAST(json_extract(fields, '$.updated_at') AS REAL) END)",
        time_field("valid_to")
    )
}

/// The memories that carry contradiction links, so listing them is a walk
/// of the links rather than of every memory. Shared with 7.1, whose
/// version 6 was this index alone.
pub(crate) const LINKED_CONDITION: &str = "json_extract(fields, '$.contradictions') IS NOT NULL AND json_extract(fields, '$.contradictions') <> ''";

/// SQL that is true when a memory's fields hold `field`, with the path
/// written in, or `None` for a name that can't be one. The same spelling
/// builds a partial index's condition and the query that must match it.
pub(crate) fn present_condition(field: &str) -> Option<String> {
    if field.is_empty() || field.contains(['"', '\\', '\'', '\0']) {
        return None;
    }
    Some(format!("json_extract(fields, '$.\"{field}\"') IS NOT NULL"))
}

/// Indexes every open makes sure of, beside the versioned steps. They add
/// no column and change no row, so a store that has them still opens in a
/// release that doesn't know them (7.0.x), and one without them gets them
/// on its next open. Building them reads every row once, a second or two
/// at 50,000 memories, the first time only.
///
/// - `memories_search` covers everything a search filters on, so the set of
///   keys a recall may return is read from the index instead of from every
///   row's JSON: about 27 ms down to 2 ms per namespace at 50,000 memories.
/// - `memories_abandoned` holds the memories that logged a dead end, so the
///   graveyard behind recall's warnings and the PreToolUse hook walks those
///   rather than every episodic memory.
fn ensured_indexes() -> String {
    let abandoned = present_condition("abandoned_approaches").expect("a plain field name");
    format!(
        "CREATE INDEX IF NOT EXISTS memories_search
             ON memories (namespace, trust, state, project, project_name, key);
         CREATE INDEX IF NOT EXISTS memories_abandoned ON memories (namespace, key) WHERE {abandoned};"
    )
}

/// Version 6, the same on 7.0.11 and 7.1 (#26): bi-temporal validity.
/// `valid_from` and `valid_to` are when a memory was true in the world,
/// `created_at` and `retired_at` when OmniMem believed it. Generated
/// columns, so `SearchFilter::valid_at` and `known_at` run in SQL, plus 7.1's
/// `memories_linked` index (with `IF NOT EXISTS`, since a 7.1 development
/// store already has it).
///
/// The data part writes the exact time a superseded loser stopped being
/// believed and stopped being true, from its resolution's `resolved_at`,
/// and a rejected memory's from its `both_wrong` resolution where it has
/// one. Everything else keeps the defaults above, and `valid_from` stays
/// empty: nobody knows it, and a made-up date would be worse than none.
fn v6_bitemporal() -> String {
    let valid_from = time_field("valid_from");
    let valid_to = valid_to_expr();
    let retired_at = retired_at_expr();
    let resolved = |outcome: &str, also: &str| {
        format!(
            "(SELECT CAST(json_extract(link.value, '$.resolution.resolved_at') AS TEXT)
                FROM json_each(CASE WHEN json_valid(json_extract(fields, '$.contradictions'))
                              THEN json_extract(fields, '$.contradictions') ELSE '[]' END) AS link
               WHERE json_extract(link.value, '$.resolution.outcome') = '{outcome}' {also}
               ORDER BY CAST(json_extract(link.value, '$.resolution.resolved_at') AS REAL) DESC
               LIMIT 1)"
        )
    };
    let superseded_at = resolved(
        "supersedes",
        "AND json_extract(link.value, '$.key') = json_extract(fields, '$.superseded_by')",
    );
    let wrong_at = resolved("both_wrong", "");
    format!(
        r"
ALTER TABLE memories ADD COLUMN valid_from REAL GENERATED ALWAYS AS ({valid_from}) VIRTUAL;
ALTER TABLE memories ADD COLUMN valid_to REAL GENERATED ALWAYS AS ({valid_to}) VIRTUAL;
ALTER TABLE memories ADD COLUMN retired_at REAL GENERATED ALWAYS AS ({retired_at}) VIRTUAL;
CREATE INDEX memories_namespace_valid_to ON memories (namespace, valid_to);
CREATE INDEX memories_namespace_retired ON memories (namespace, retired_at);

UPDATE memories
   SET fields = json_set(fields, '$.retired_at', {superseded_at}, '$.valid_to', {superseded_at})
 WHERE trust = 'superseded'
   AND COALESCE(json_extract(fields, '$.retired_at'), '') = ''
   AND json_valid(json_extract(fields, '$.contradictions'))
   AND {superseded_at} IS NOT NULL;

UPDATE memories
   SET fields = json_set(fields, '$.retired_at', {wrong_at})
 WHERE trust = 'rejected'
   AND COALESCE(json_extract(fields, '$.retired_at'), '') = ''
   AND json_valid(json_extract(fields, '$.contradictions'))
   AND {wrong_at} IS NOT NULL;
"
    )
}

fn v6() -> String {
    format!(
        "{}\nCREATE INDEX IF NOT EXISTS memories_linked ON memories (key) WHERE {LINKED_CONDITION};",
        v6_bitemporal()
    )
}

/// Whether `memories` has the bi-temporal columns. A store a `dev-v7.1.x`
/// build opened is at version 6 with only the `memories_linked` index, and
/// `ADD COLUMN` has no `IF NOT EXISTS`, so the repair reads the table rather
/// than the version. It can go once 7.1.0 is out.
fn has_bitemporal_columns(conn: &Connection) -> Result<bool> {
    let mut stmt =
        conn.prepare("SELECT 1 FROM pragma_table_xinfo('memories') WHERE name = 'valid_from'")?;
    Ok(stmt.exists([])?)
}

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
    if version < 6 {
        let v6 = v6();
        conn.execute_batch(&format!("BEGIN; {v6} PRAGMA user_version = 6; COMMIT;"))?;
    } else if !has_bitemporal_columns(conn)? {
        // A 7.1 development store: version 6 with the index and nothing else.
        let repair = v6_bitemporal();
        conn.execute_batch(&format!("BEGIN; {repair} COMMIT;"))?;
    }
    conn.execute_batch(&ensured_indexes())?;
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

    fn v5_store() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        let v5 = v5();
        conn.execute_batch(&format!(
            "BEGIN; {V1} {V2} {V3} {V4} {v5} PRAGMA user_version = 5; COMMIT;"
        ))
        .unwrap();
        conn
    }

    fn insert(conn: &Connection, id: &str, fields: &serde_json::Value) {
        conn.execute(
            "INSERT INTO memories (key, namespace, fields) VALUES (?1, 'episodic', ?2)",
            params![format!("mem:episodic:{id}"), fields.to_string()],
        )
        .unwrap();
    }

    type Times = (Option<f64>, Option<f64>, Option<f64>);

    fn times(conn: &Connection, id: &str) -> Times {
        conn.query_row(
            "SELECT valid_from, valid_to, retired_at FROM memories WHERE key = ?1",
            params![format!("mem:episodic:{id}")],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }

    fn fields_of(conn: &Connection, id: &str) -> crate::Fields {
        let raw: String = conn
            .query_row(
                "SELECT fields FROM memories WHERE key = ?1",
                params![format!("mem:episodic:{id}")],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn bitemporal_rows(conn: &Connection) {
        let resolved = |other: &str, outcome: &str, at: &str| {
            serde_json::json!([{"key": format!("mem:episodic:{other}"),
                "resolution": {"outcome": outcome, "resolved_at": at}}])
            .to_string()
        };
        insert(
            conn,
            "plain",
            &serde_json::json!({"content": "x", "created_at": "100.0", "updated_at": "150.0"}),
        );
        insert(
            conn,
            "winner",
            &serde_json::json!({"content": "x", "created_at": "200.0", "updated_at": "300.0",
            "contradictions": resolved("loser", "supersedes", "300.0")}),
        );
        insert(
            conn,
            "loser",
            &serde_json::json!({"content": "x", "created_at": "100.0", "updated_at": "310.0",
            "trust": "superseded", "superseded_by": "mem:episodic:winner",
            "contradictions": resolved("winner", "supersedes", "300.0")}),
        );
        insert(
            conn,
            "wrong1",
            &serde_json::json!({"content": "x", "created_at": "100.0", "updated_at": "410.0",
            "trust": "rejected", "contradictions": resolved("wrong2", "both_wrong", "400.0")}),
        );
        insert(
            conn,
            "wrong2",
            &serde_json::json!({"content": "x", "created_at": "100.0", "updated_at": "410.0",
            "trust": "rejected", "contradictions": resolved("wrong1", "both_wrong", "400.0")}),
        );
        insert(
            conn,
            "fact",
            &serde_json::json!({"content": "x", "created_at": "100.0", "updated_at": "500.0",
            "trust": "rejected", "enriched_from": "mem:episodic:plain"}),
        );
        insert(
            conn,
            "broken",
            &serde_json::json!({"content": "x", "created_at": "100.0", "updated_at": "600.0",
            "trust": "superseded", "superseded_by": "mem:episodic:gone", "contradictions": "not json"}),
        );
    }

    /// #26: version 6 gives every memory its two intervals. A superseded
    /// loser stops being true and believed when it was superseded, both
    /// sides of `both_wrong` stop being believed but were never true to
    /// stop, a rejected fact falls back to `updated_at`, and a plain memory
    /// keeps both intervals open. The Rust readers agree with the columns.
    #[test]
    fn version_6_gives_each_memory_its_intervals() {
        let conn = v5_store();
        bitemporal_rows(&conn);
        migrate(&conn).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 6);

        let expect: [(&str, Times); 7] = [
            ("plain", (None, None, None)),
            ("winner", (None, None, None)),
            ("loser", (None, Some(300.0), Some(300.0))),
            ("wrong1", (None, None, Some(400.0))),
            ("wrong2", (None, None, Some(400.0))),
            ("fact", (None, None, Some(500.0))),
            ("broken", (None, Some(600.0), Some(600.0))),
        ];
        for (id, want) in expect {
            assert_eq!(times(&conn, id), want, "{id}");
            let fields = fields_of(&conn, id);
            assert_eq!(crate::valid_to_of(&fields), want.1, "{id} valid_to in Rust");
            assert_eq!(
                crate::retired_at_of(&fields),
                want.2,
                "{id} retired_at in Rust"
            );
            assert_eq!(
                crate::valid_from_of(&fields),
                Some(if id == "winner" { 200.0 } else { 100.0 })
            );
        }
        // The exact times are written into the record, not only derived.
        assert_eq!(fields_of(&conn, "loser")["retired_at"], "300.0");
        assert_eq!(fields_of(&conn, "loser")["valid_to"], "300.0");
        assert!(!fields_of(&conn, "fact").contains_key("retired_at"));
        let linked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memories_linked'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, 1);

        // The readers in both languages agree on what was true and believed.
        let loser = fields_of(&conn, "loser");
        assert!(crate::valid_at(&loser, 250.0) && !crate::valid_at(&loser, 300.0));
        assert!(crate::known_at(&loser, 250.0) && !crate::known_at(&loser, 350.0));
        assert!(!crate::known_at(&loser, 50.0), "not yet recorded");

        // Again changes nothing.
        let before: Vec<Times> = expect.iter().map(|(id, _)| times(&conn, id)).collect();
        migrate(&conn).unwrap();
        let after: Vec<Times> = expect.iter().map(|(id, _)| times(&conn, id)).collect();
        assert_eq!(before, after);
    }

    /// A store a `dev-v7.1.x` build opened is at version 6 with only the
    /// `memories_linked` index. It gets the columns and the data step, once.
    #[test]
    fn a_71_development_store_at_version_6_is_repaired() {
        let conn = v5_store();
        bitemporal_rows(&conn);
        conn.execute_batch(&format!(
            "BEGIN; CREATE INDEX memories_linked ON memories (key) WHERE {LINKED_CONDITION}; PRAGMA user_version = 6; COMMIT;"
        ))
        .unwrap();
        assert!(!has_bitemporal_columns(&conn).unwrap());
        migrate(&conn).unwrap();
        assert!(has_bitemporal_columns(&conn).unwrap());
        assert_eq!(times(&conn, "loser"), (None, Some(300.0), Some(300.0)));
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 6);
        migrate(&conn).unwrap();
        assert_eq!(times(&conn, "loser"), (None, Some(300.0), Some(300.0)));
    }

    /// A 7.0.10 build supports version 5 and refuses this one, as this build
    /// refuses anything past 6.
    #[test]
    fn version_6_is_past_what_7_0_10_supports() {
        assert_eq!(SCHEMA_VERSION, 6);
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA user_version = 7").unwrap();
        assert!(matches!(
            migrate(&conn),
            Err(StoreError::SchemaTooNew {
                found: 7,
                supported: 6
            })
        ));
    }
}
