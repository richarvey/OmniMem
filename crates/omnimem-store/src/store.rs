//! Memory records: the operations the 6.x `ValkeyStore` offered, over SQLite.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use omnimem_core::classification::DEFAULT_CLASSIFICATION;
use omnimem_core::{Namespace, VECTOR_DIM, content_hash};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::vectors::{VectorIndex, to_bytes};
use crate::{Result, StoreError, schema};

/// A record's fields: what a Valkey hash held. Ordered, so a record always
/// serialises the same way.
pub type Fields = BTreeMap<String, String>;

/// Every key the store accepts, as the 6.x store's `_VALID_KEY_PREFIXES`.
pub const VALID_KEY_PREFIXES: [&str; 10] = [
    "mem:episodic:",
    "mem:project:",
    "mem:knowledge:",
    "mem:preference:",
    "mem:skill:",
    "topics:",
    "log:recall:",
    "meta:",
    "qexp:",
    "queue:",
];

/// SQLite caps bound parameters; stay well under it per statement.
pub(crate) const IN_CHUNK: usize = 500;

/// Which memories a search may return. Empty lists mean "no restriction".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilter {
    /// Allowed `state` values. A record with no state counts as `active`.
    pub states: Vec<String>,
    /// Allowed projects, matched against `project` or `project_name`.
    pub projects: Vec<String>,
}

impl SearchFilter {
    fn is_empty(&self) -> bool {
        self.states.is_empty() && self.projects.is_empty()
    }
}

/// Which memories a listing returns. Every condition is evaluated by
/// SQLite over the generated columns (see `schema.rs`), so a filtered
/// listing reads only the rows that match instead of every record in the
/// namespace. Empty lists and `None` mean "no restriction".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryFilter {
    /// Only keys starting with this prefix (which must lie inside the
    /// namespace, such as `mem:skill:gen:`).
    pub key_prefix: Option<String>,
    /// Allowed `state` values. A record with no state counts as `active`;
    /// one with an empty state matches nothing, as the engine reads it.
    pub states: Vec<String>,
    /// The project a memory belongs to: `project` when set and non-empty,
    /// else `project_name`. An empty name matches nothing.
    pub project: Option<String>,
    /// Only the `project` field, exactly; `project_name` is not consulted.
    pub project_field: Option<String>,
    /// Allowed `feed_name` values. An empty name matches records with no
    /// feed, as the engine's `unwrap_or_default` comparison does.
    pub feed_names: Vec<String>,
    /// At least one of these fields must be present (any value, even empty).
    pub present_any: Vec<String>,
    /// `created_at` at or after this instant. The check is a superset of the
    /// engine's: a value that is not a plain number (the engine's parser
    /// reads `inf` and `nan` as numbers, SQLite's does not) passes through,
    /// so a caller that must match its own parser re-checks the field.
    pub created_at_min: Option<f64>,
    /// Consider only the first `n` keys of the namespace, in key order, as
    /// the capped scans did before filtering. The other conditions then
    /// apply to those keys.
    pub key_cap: Option<usize>,
    /// At most this many rows, in key order, after every other condition.
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub key: String,
    /// Cosine distance, `1 - cosine`: what valkey-search returned as
    /// `similarity_score`.
    pub distance: f32,
    pub fields: Fields,
}

impl SearchHit {
    pub fn similarity(&self) -> f32 {
        1.0 - self.distance
    }
}

/// A skill's vector embeds its discovery metadata, not its body.
pub fn discovery_text(name: &str, description: &str, domain: &str) -> String {
    format!("{name}. {description} Domain: {domain}.")
}

pub struct Store {
    pub(crate) conn: Mutex<Connection>,
    pub(crate) vectors: RwLock<VectorIndex>,
    pub(crate) origin_id: String,
    dim: usize,
}

impl Store {
    /// Open (creating if needed) the database file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        Self::init(Connection::open(path)?, VECTOR_DIM)
    }

    /// A private in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, VECTOR_DIM)
    }

    /// In-memory with a different vector size, for small hand-written tests.
    pub fn open_in_memory_with_dim(dim: usize) -> Result<Self> {
        Self::init(Connection::open_in_memory()?, dim)
    }

    fn init(conn: Connection, dim: usize) -> Result<Self> {
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;",
        )?;
        schema::migrate(&conn)?;
        let origin_id = schema::origin_id(&conn)?;
        let vectors = VectorIndex::load(&conn, dim)?;
        Ok(Self {
            conn: Mutex::new(conn),
            vectors: RwLock::new(vectors),
            origin_id,
            dim,
        })
    }

    pub fn dimension(&self) -> usize {
        self.dim
    }

    /// The v7 `origin_id` this store stamps on memories created here.
    pub fn origin_id(&self) -> &str {
        &self.origin_id
    }

    // Lock order: the connection first, then the vector matrix, never the
    // other way round. A writer takes the matrix lock before committing and
    // keeps it until the matrix matches the rows, so a delete can't land
    // between a row's commit and its vector's insertion and leave a phantom.
    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn vectors_read(&self) -> RwLockReadGuard<'_, VectorIndex> {
        self.vectors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn vectors_write(&self) -> RwLockWriteGuard<'_, VectorIndex> {
        self.vectors
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn check_dim(&self, vector: &[f32]) -> Result<()> {
        if vector.len() == self.dim {
            Ok(())
        } else {
            Err(StoreError::DimensionMismatch {
                expected: self.dim,
                got: vector.len(),
            })
        }
    }

    // -- memories ---------------------------------------------------------

    /// Merge fields into a memory (creating it) and, when given, set its
    /// vector. Fields not mentioned are kept, as HSET did.
    pub fn upsert(&self, key: &str, fields: &Fields, vector: Option<&[f32]>) -> Result<()> {
        let namespace = memory_namespace(key)?;
        if let Some(v) = vector {
            self.check_dim(v)?;
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        merge_memory(&tx, key, namespace, fields, &self.origin_id)?;
        if let Some(v) = vector {
            write_vector(&tx, key, v)?;
        }
        // The matrix lock is taken before the commit so no delete can slip
        // between the row landing and the matrix learning of it.
        let mut index = self.vectors_write();
        tx.commit()?;
        if let Some(v) = vector {
            index.insert(namespace, key, v);
        }
        Ok(())
    }

    /// Set a memory's vector without touching its fields. The memory must exist.
    pub fn set_vector(&self, key: &str, vector: &[f32]) -> Result<bool> {
        let namespace = memory_namespace(key)?;
        self.check_dim(vector)?;
        let mut conn = self.conn();
        if load_fields(&conn, key)?.is_none() {
            return Ok(false);
        }
        let tx = conn.transaction()?;
        write_vector(&tx, key, vector)?;
        let mut index = self.vectors_write();
        tx.commit()?;
        index.insert(namespace, key, vector);
        Ok(true)
    }

    /// Memories with no stored vector: what a restore that failed part way
    /// through re-embedding, or an import run with embedding off, leaves
    /// unsearchable. Sorted by key.
    pub fn memories_without_vectors(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT m.key FROM memories m LEFT JOIN vectors v ON v.key = m.key
             WHERE v.key IS NULL ORDER BY m.key",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// All fields of a memory, or of a non-memory hash.
    pub fn get(&self, key: &str) -> Result<Option<Fields>> {
        if key.starts_with("mem:") {
            return load_fields(&self.conn(), key);
        }
        self.hash_get_all(key)
    }

    /// Every field of many records, aligned with `keys`. Memories and
    /// non-memory hashes are each fetched in one `IN (...)` query per
    /// chunk, under one lock, instead of a round trip per key.
    pub fn get_multi(&self, keys: &[String]) -> Result<Vec<Option<Fields>>> {
        let mut found: HashMap<String, Fields> = HashMap::new();
        {
            let conn = self.conn();
            let (memory_keys, other_keys) = split_keys(keys);
            for chunk in memory_keys.chunks(IN_CHUNK) {
                let sql = format!(
                    "SELECT key, fields FROM memories WHERE key IN ({})",
                    placeholders(chunk.len())
                );
                let mut stmt = conn.prepare(&sql)?;
                let mut rows = stmt.query(params_from_iter(chunk))?;
                while let Some(row) = rows.next()? {
                    let key: String = row.get(0)?;
                    let raw: String = row.get(1)?;
                    let fields = parse_fields(&key, &raw)?;
                    found.insert(key, fields);
                }
            }
            crate::kv::load_hashes(&conn, &other_keys, &mut found)?;
        }
        Ok(align(keys, found))
    }

    /// A fixed projection of fields for many keys; `None` where a key has
    /// none of them. The projection runs inside SQLite (`json_extract` on
    /// each requested field), so a record's `content` never crosses into
    /// Rust unless it was asked for: the scans behind the briefing, the
    /// abandoned cache and the skill pools read a few short fields from
    /// thousands of rows.
    pub fn get_fields_multi(
        &self,
        keys: &[String],
        fields: &[&str],
    ) -> Result<Vec<Option<Fields>>> {
        if fields.is_empty() {
            return Ok(vec![None; keys.len()]);
        }
        let Some(paths) = json_paths(fields) else {
            // A name that can't be quoted into a JSON path: read whole
            // records and project them here, as this always did.
            return Ok(self
                .get_multi(keys)?
                .into_iter()
                .map(|row| row.and_then(|all| project(&all, fields)))
                .collect());
        };
        let mut found: HashMap<String, Fields> = HashMap::new();
        {
            let conn = self.conn();
            let (memory_keys, other_keys) = split_keys(keys);
            let columns = ", json_extract(fields, ?)".repeat(fields.len());
            for chunk in memory_keys.chunks(IN_CHUNK) {
                let sql = format!(
                    "SELECT key{columns} FROM memories WHERE key IN ({})",
                    placeholders(chunk.len())
                );
                let mut stmt = conn.prepare(&sql)?;
                let args = paths
                    .iter()
                    .map(String::as_str)
                    .chain(chunk.iter().copied());
                let mut rows = stmt.query(params_from_iter(args))?;
                while let Some(row) = rows.next()? {
                    let key: String = row.get(0)?;
                    if let Some(projected) = read_projection(row, fields)? {
                        found.insert(key, projected);
                    }
                }
            }
            let mut hashes = HashMap::new();
            crate::kv::load_hashes(&conn, &other_keys, &mut hashes)?;
            for (key, all) in hashes {
                if let Some(projected) = project(&all, fields) {
                    found.insert(key, projected);
                }
            }
        }
        Ok(align(keys, found))
    }

    /// The memories of one namespace that satisfy `filter`, in key order,
    /// each with the projection of `fields` it has. A row that has none of
    /// the requested fields is left out, as [`Store::get_fields_multi`]
    /// reports it as `None`; with `fields` empty every matching key is
    /// listed with no fields.
    ///
    /// This replaces the scan-every-key-then-filter-in-Rust pattern: one
    /// statement, evaluated over the generated columns and their indexes,
    /// returns only the rows a caller would have kept.
    pub fn list_memories(
        &self,
        namespace: Namespace,
        filter: &MemoryFilter,
        fields: &[&str],
    ) -> Result<Vec<(String, Fields)>> {
        let paths = json_paths(fields).ok_or_else(|| {
            StoreError::InvalidField(fields.iter().map(ToString::to_string).collect())
        })?;
        let mut sql = String::from("SELECT key");
        let mut args: Vec<SqlValue> = Vec::new();
        for path in paths {
            sql.push_str(", json_extract(fields, ?)");
            args.push(SqlValue::Text(path));
        }
        sql.push_str(" FROM memories WHERE namespace = ?");
        args.push(SqlValue::Text(namespace.as_str().to_owned()));

        if let Some(prefix) = &filter.key_prefix {
            sql.push_str(" AND key >= ? AND key < ?");
            args.push(SqlValue::Text(prefix.clone()));
            args.push(SqlValue::Text(format!("{prefix}\u{10FFFF}")));
        }
        push_state_filter(&mut sql, &mut args, &filter.states);
        if let Some(project) = &filter.project {
            if project.is_empty() {
                // Neither field can equal a name the engine reads as "none".
                sql.push_str(" AND 0");
            } else {
                // `project` when set and non-empty, else `project_name`,
                // spelt so each branch can use its own index.
                sql.push_str(
                    " AND (project = ? OR ((project IS NULL OR project = '') AND project_name = ?))",
                );
                args.push(SqlValue::Text(project.clone()));
                args.push(SqlValue::Text(project.clone()));
            }
        }
        if let Some(project) = &filter.project_field {
            sql.push_str(" AND project = ?");
            args.push(SqlValue::Text(project.clone()));
        }
        if !filter.feed_names.is_empty() {
            let _ = write!(
                sql,
                " AND (feed_name IN ({})",
                placeholders(filter.feed_names.len())
            );
            args.extend(filter.feed_names.iter().cloned().map(SqlValue::Text));
            if filter.feed_names.iter().any(String::is_empty) {
                sql.push_str(" OR feed_name IS NULL");
            }
            sql.push(')');
        }
        if !filter.present_any.is_empty() {
            let Some(present) = json_paths_owned(&filter.present_any) else {
                return Err(StoreError::InvalidField(filter.present_any.join(", ")));
            };
            sql.push_str(" AND (");
            for (i, path) in present.into_iter().enumerate() {
                if i > 0 {
                    sql.push_str(" OR ");
                }
                sql.push_str("json_extract(fields, ?) IS NOT NULL");
                args.push(SqlValue::Text(path));
            }
            sql.push(')');
        }
        if let Some(min) = filter.created_at_min {
            // `created_at` is `CAST(... AS REAL)`, which reads `inf`, `nan`
            // and their kin as 0; the engine's parser reads them as numbers.
            // Any value carrying an `i` or `n` passes so the caller's own
            // parse decides, and no plain number does both.
            sql.push_str(
                " AND (created_at >= ? OR lower(json_extract(fields, '$.created_at')) GLOB '*[in]*')",
            );
            args.push(SqlValue::Real(min));
        }
        if let Some(cap) = filter.key_cap {
            sql.push_str(
                " AND key IN (SELECT key FROM memories WHERE namespace = ? ORDER BY key LIMIT ?)",
            );
            args.push(SqlValue::Text(namespace.as_str().to_owned()));
            args.push(SqlValue::Integer(cap as i64));
        }
        sql.push_str(" ORDER BY key");
        if let Some(limit) = filter.limit {
            sql.push_str(" LIMIT ?");
            args.push(SqlValue::Integer(limit as i64));
        }

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(args))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            if fields.is_empty() {
                out.push((key, Fields::new()));
            } else if let Some(projected) = read_projection(row, fields)? {
                out.push((key, projected));
            }
        }
        Ok(out)
    }

    pub fn set_field(&self, key: &str, field: &str, value: &str) -> Result<()> {
        self.set_fields(key, &Fields::from([(field.to_owned(), value.to_owned())]))
    }

    /// Merge fields into a record without re-embedding. Like HSET, a memory
    /// that doesn't exist yet is created.
    pub fn set_fields(&self, key: &str, fields: &Fields) -> Result<()> {
        validate_key(key)?;
        if fields.is_empty() {
            return Ok(());
        }
        if key.starts_with("mem:") {
            let namespace = memory_namespace(key)?;
            merge_memory(&self.conn(), key, namespace, fields, &self.origin_id)?;
            return Ok(());
        }
        self.hash_set(key, fields)
    }

    /// The same field updates on many memories, in one transaction.
    pub fn set_fields_multi(&self, keys: &[String], fields: &Fields) -> Result<usize> {
        if keys.is_empty() || fields.is_empty() {
            return Ok(0);
        }
        for key in keys {
            validate_key(key)?;
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for key in keys {
            if key.starts_with("mem:") {
                merge_memory(&tx, key, memory_namespace(key)?, fields, &self.origin_id)?;
            } else {
                crate::kv::hash_merge(&tx, key, fields)?;
            }
        }
        tx.commit()?;
        Ok(keys.len())
    }

    pub fn delete(&self, key: &str) -> Result<bool> {
        Ok(self.delete_many(&[key.to_owned()])? == 1)
    }

    /// Hard delete; a memory's vector goes with it. Keys are deleted in
    /// `IN (...)` chunks rather than one statement each, and `RETURNING`
    /// names the memories that existed so the matrix can drop them.
    pub fn delete_many(&self, keys: &[String]) -> Result<usize> {
        for key in keys {
            validate_key(key)?;
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (memory_keys, other_keys) = split_keys(keys);
        let mut removed_memories: Vec<String> = Vec::new();
        for chunk in memory_keys.chunks(IN_CHUNK) {
            let sql = format!(
                "DELETE FROM memories WHERE key IN ({}) RETURNING key",
                placeholders(chunk.len())
            );
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(chunk), |r| r.get::<_, String>(0))?;
            for key in rows {
                removed_memories.push(key?);
            }
        }
        let mut deleted = removed_memories.len();
        for chunk in other_keys.chunks(IN_CHUNK) {
            let sql = format!(
                "DELETE FROM kv WHERE key IN ({})",
                placeholders(chunk.len())
            );
            deleted += tx.execute(&sql, params_from_iter(chunk))?;
        }
        let mut index = self.vectors_write();
        tx.commit()?;
        for key in &removed_memories {
            if let Ok(ns) = memory_namespace(key) {
                index.remove(ns, key);
            }
        }
        Ok(deleted)
    }

    /// Every live key starting with `prefix`, memories and others, sorted.
    pub fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let upper = format!("{prefix}\u{10FFFF}");
        let conn = self.conn();
        let mut keys = Vec::new();
        let scan_memories = "mem:".starts_with(prefix) || prefix.starts_with("mem:");
        let scan_kv = !prefix.starts_with("mem:");
        if scan_memories {
            let mut stmt = conn.prepare_cached(
                "SELECT key FROM memories WHERE key >= ?1 AND key < ?2 ORDER BY key",
            )?;
            let rows = stmt.query_map(params![prefix, upper], |r| r.get::<_, String>(0))?;
            for key in rows {
                keys.push(key?);
            }
        }
        if scan_kv {
            let memories = keys.len();
            let mut stmt = conn.prepare_cached(
                "SELECT key FROM kv WHERE key >= ?1 AND key < ?2 AND (expires_at IS NULL OR expires_at > ?3) ORDER BY key",
            )?;
            let rows = stmt.query_map(params![prefix, upper, crate::time::now()], |r| {
                r.get::<_, String>(0)
            })?;
            for key in rows {
                keys.push(key?);
            }
            // Each table came back sorted; only a mix of both needs merging.
            if memories > 0 && keys.len() > memories {
                keys.sort();
            }
        }
        Ok(keys)
    }

    pub fn count_records(&self, namespace: Namespace) -> Result<usize> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM memories WHERE namespace = ?1",
            params![namespace.as_str()],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Record counts for every namespace, zero where there are none.
    pub fn count_all_records(&self) -> Result<BTreeMap<Namespace, usize>> {
        let mut counts: BTreeMap<Namespace, usize> =
            Namespace::ALL.iter().map(|ns| (*ns, 0)).collect();
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT namespace, COUNT(*) FROM memories GROUP BY namespace")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (namespace, n) = row?;
            if let Ok(ns) = namespace.parse::<Namespace>() {
                counts.insert(ns, n as usize);
            }
        }
        Ok(counts)
    }

    /// How many memories in a namespace have a vector.
    pub fn vector_count(&self, namespace: Namespace) -> usize {
        self.vectors_read().len(namespace)
    }

    /// Stored vectors, aligned with `keys`.
    pub fn get_vectors_multi(&self, keys: &[String]) -> Vec<Option<Vec<f32>>> {
        let index = self.vectors_read();
        keys.iter()
            .map(|k| memory_namespace(k).ok().and_then(|ns| index.get(ns, k)))
            .collect()
    }

    /// The nearest memories in a namespace. `top_k` is clamped to 1..=100 as
    /// in 6.x. `return_fields` projects each hit's fields; `None` returns all.
    pub fn search(
        &self,
        namespace: Namespace,
        vector: &[f32],
        top_k: usize,
        filter: &SearchFilter,
        return_fields: Option<&[&str]>,
    ) -> Result<Vec<SearchHit>> {
        self.check_dim(vector)?;
        let top_k = top_k.clamp(1, 100);
        let allowed = if filter.is_empty() {
            None
        } else {
            Some(self.filtered_keys(namespace, filter)?)
        };
        let ranked = self
            .vectors_read()
            .search(namespace, vector, top_k, allowed.as_ref());
        let keys: Vec<String> = ranked.iter().map(|(k, _)| k.clone()).collect();
        let rows = self.get_multi(&keys)?;
        Ok(ranked
            .into_iter()
            .zip(rows)
            .filter_map(|((key, distance), row)| {
                let all = row?;
                let fields = match return_fields {
                    None => all,
                    Some(names) => project(&all, names).unwrap_or_default(),
                };
                Some(SearchHit {
                    key,
                    distance,
                    fields,
                })
            })
            .collect())
    }

    fn filtered_keys(
        &self,
        namespace: Namespace,
        filter: &SearchFilter,
    ) -> Result<HashSet<String>> {
        let mut sql = String::from("SELECT key FROM memories WHERE namespace = ?");
        let mut args: Vec<SqlValue> = vec![SqlValue::Text(namespace.as_str().to_owned())];
        push_state_filter(&mut sql, &mut args, &filter.states);
        if !filter.projects.is_empty() {
            let marks = placeholders(filter.projects.len());
            let _ = write!(
                sql,
                " AND (project IN ({marks}) OR project_name IN ({marks}))"
            );
            args.extend(filter.projects.iter().cloned().map(SqlValue::Text));
            args.extend(filter.projects.iter().cloned().map(SqlValue::Text));
        }
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<HashSet<_>, _>>()
            .map_err(StoreError::from)
    }
}

// -- shared row helpers ------------------------------------------------------

/// The longest key the store accepts.
///
/// Keys are ULIDs, project names and `mem:skill:gen:<domain>-<user>` forms,
/// all far shorter; the cap keeps a backup or bundle from smuggling in a key
/// the size of a record.
pub const MAX_KEY_BYTES: usize = 512;

/// A key must carry a known prefix and a non-empty id after it, with no
/// control characters (a NUL or newline would corrupt logs and any text
/// export) and a bounded length.
pub(crate) fn validate_key(key: &str) -> Result<()> {
    let invalid = || StoreError::InvalidKey(key.chars().take(50).collect());
    let rest = VALID_KEY_PREFIXES
        .iter()
        .find_map(|p| key.strip_prefix(p))
        .ok_or_else(invalid)?;
    if rest.is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn memory_namespace(key: &str) -> Result<Namespace> {
    validate_key(key)?;
    key.strip_prefix("mem:")
        .and_then(|rest| rest.split(':').next())
        .and_then(|ns| ns.parse().ok())
        .ok_or_else(|| StoreError::InvalidKey(key.chars().take(50).collect()))
}

pub(crate) fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// Memory keys and the rest, each in the order given.
fn split_keys(keys: &[String]) -> (Vec<&str>, Vec<&str>) {
    keys.iter()
        .map(String::as_str)
        .partition(|k| k.starts_with("mem:"))
}

/// Rows back into the order of `keys`. A record found under a key is
/// handed out once per mention: keys are normally unique, and only a list
/// that repeats one pays for the clone.
fn align(keys: &[String], mut found: HashMap<String, Fields>) -> Vec<Option<Fields>> {
    let unique = keys
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>()
        .len()
        == keys.len();
    keys.iter()
        .map(|k| {
            let row = if unique {
                found.remove(k)
            } else {
                found.get(k).cloned()
            };
            row.filter(|f| !f.is_empty())
        })
        .collect()
}

/// `$."name"` for each field, or `None` when a name can't be quoted. Every
/// field the engine reads is a plain identifier; the guard is for callers
/// passing user text.
fn json_paths(fields: &[&str]) -> Option<Vec<String>> {
    fields
        .iter()
        .map(|f| (!f.contains(['"', '\\', '\0'])).then(|| format!("$.\"{f}\"")))
        .collect()
}

fn json_paths_owned(fields: &[String]) -> Option<Vec<String>> {
    let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
    json_paths(&refs)
}

/// The requested fields of a row whose columns after the key are their
/// `json_extract` values, or `None` when it has none of them. Every stored
/// value is a string; a number met in a hand-edited row is kept as its text
/// rather than refused.
fn read_projection(row: &rusqlite::Row<'_>, fields: &[&str]) -> Result<Option<Fields>> {
    let mut projected = Fields::new();
    for (i, name) in fields.iter().enumerate() {
        let value = match row.get_ref(i + 1)? {
            rusqlite::types::ValueRef::Null | rusqlite::types::ValueRef::Blob(_) => continue,
            rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
            rusqlite::types::ValueRef::Integer(i) => i.to_string(),
            rusqlite::types::ValueRef::Real(r) => r.to_string(),
        };
        projected.insert((*name).to_owned(), value);
    }
    Ok((!projected.is_empty()).then_some(projected))
}

/// The requested fields of a whole record, or `None` when it has none.
fn project(all: &Fields, fields: &[&str]) -> Option<Fields> {
    let projected: Fields = fields
        .iter()
        .filter_map(|f| all.get(*f).map(|v| ((*f).to_owned(), v.clone())))
        .collect();
    (!projected.is_empty()).then_some(projected)
}

/// `COALESCE(state, 'active') IN (states)`, written so the
/// `(namespace, state)` index still applies: a missing state matches only
/// when `active` is allowed.
fn push_state_filter(sql: &mut String, args: &mut Vec<SqlValue>, states: &[String]) {
    if states.is_empty() {
        return;
    }
    let _ = write!(sql, " AND (state IN ({})", placeholders(states.len()));
    args.extend(states.iter().cloned().map(SqlValue::Text));
    if states.iter().any(|s| s == "active") {
        sql.push_str(" OR state IS NULL");
    }
    sql.push(')');
}

pub(crate) fn parse_fields(key: &str, raw: &str) -> Result<Fields> {
    serde_json::from_str(raw).map_err(|source| StoreError::CorruptRecord {
        key: key.to_owned(),
        source,
    })
}

pub(crate) fn load_fields(conn: &Connection, key: &str) -> Result<Option<Fields>> {
    let mut stmt = conn.prepare_cached("SELECT fields FROM memories WHERE key = ?1")?;
    let raw: Option<String> = stmt.query_row(params![key], |r| r.get(0)).optional()?;
    raw.map(|raw| parse_fields(key, &raw)).transpose()
}

/// Merge `updates` into a memory, creating it if needed, and keep the v7
/// identity fields right: a new memory gets this store's `origin_id`,
/// `epoch` 1 and the default classification, and any write of `content`
/// recomputes `content_hash`. Bumping `epoch` on a meaningful edit is the
/// engine's decision (v7 spec §5), not the store's.
pub(crate) fn merge_memory(
    conn: &Connection,
    key: &str,
    namespace: Namespace,
    updates: &Fields,
    origin_id: &str,
) -> Result<Fields> {
    let existing = load_fields(conn, key)?;
    merge_memory_into(conn, key, namespace, existing, updates, origin_id)
}

/// [`merge_memory`] for a caller that has already read the record, so the
/// row is not fetched twice.
pub(crate) fn merge_memory_into(
    conn: &Connection,
    key: &str,
    namespace: Namespace,
    existing: Option<Fields>,
    updates: &Fields,
    origin_id: &str,
) -> Result<Fields> {
    let created = existing.is_none();
    let mut fields = existing.unwrap_or_default();
    for (name, value) in updates {
        if name != "vector" {
            fields.insert(name.clone(), value.clone());
        }
    }
    if updates.contains_key("content") || (created && fields.contains_key("content")) {
        match fields.get("content").filter(|c| !c.is_empty()) {
            Some(content) => {
                let hash = content_hash(content);
                fields.insert("content_hash".to_owned(), hash);
            }
            None => {
                fields.remove("content_hash");
            }
        }
    }
    if created {
        fields
            .entry("origin_id".to_owned())
            .or_insert_with(|| origin_id.to_owned());
        fields
            .entry("epoch".to_owned())
            .or_insert_with(|| "1".to_owned());
        fields
            .entry("classification".to_owned())
            .or_insert_with(|| DEFAULT_CLASSIFICATION.to_owned());
    }
    let json = serde_json::to_string(&fields).map_err(|source| StoreError::CorruptRecord {
        key: key.to_owned(),
        source,
    })?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO memories (key, namespace, fields) VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET fields = excluded.fields",
    )?;
    stmt.execute(params![key, namespace.as_str(), json])?;
    Ok(fields)
}

pub(crate) fn write_vector(conn: &Connection, key: &str, vector: &[f32]) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO vectors (key, data) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET data = excluded.data",
    )?;
    stmt.execute(params![key, to_bytes(vector)])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_need_a_prefix_an_id_and_no_control_characters() {
        for key in [
            "mem:episodic:01J8X0Q9Z4T5M6N7P8R9S0T1U2",
            "mem:skill:gen:python-local",
            "meta:tool_metrics:recall",
            "topics:suppressed",
            "log:recall:1700000000.5",
        ] {
            assert!(validate_key(key).is_ok(), "{key}");
        }
        for key in [
            "",
            "secret:x",
            "mem:episodic:",
            "meta:",
            "mem:episodic:a\0b",
            "mem:episodic:a\nb",
            "mem:episodic:a\u{7f}",
        ] {
            assert!(
                matches!(validate_key(key), Err(StoreError::InvalidKey(_))),
                "{key:?}"
            );
        }
        let long = format!("mem:episodic:{}", "a".repeat(MAX_KEY_BYTES));
        assert!(matches!(
            validate_key(&long),
            Err(StoreError::InvalidKey(_))
        ));
        let just_fits = format!("mem:episodic:{}", "a".repeat(MAX_KEY_BYTES - 13));
        assert_eq!(just_fits.len(), MAX_KEY_BYTES);
        assert!(validate_key(&just_fits).is_ok());
    }

    #[test]
    fn vectorless_memories_are_listed_until_given_a_vector() {
        let store = Store::open_in_memory_with_dim(2).unwrap();
        let fields = Fields::from([("content".to_owned(), "x".to_owned())]);
        store.upsert("mem:episodic:a", &fields, None).unwrap();
        store
            .upsert("mem:knowledge:b", &fields, Some(&[1.0, 0.0]))
            .unwrap();
        store.set_fields("mem:episodic:c", &fields).unwrap();
        assert_eq!(
            store.memories_without_vectors().unwrap(),
            ["mem:episodic:a", "mem:episodic:c"]
        );
        assert!(store.set_vector("mem:episodic:a", &[0.0, 1.0]).unwrap());
        assert_eq!(
            store.memories_without_vectors().unwrap(),
            ["mem:episodic:c"]
        );
    }

    #[test]
    fn rows_and_matrix_change_together_under_contention() {
        // Writers and deleters race on the same keys; at every quiet point
        // the matrix must hold exactly the vectors of the rows that exist.
        let store = std::sync::Arc::new(Store::open_in_memory_with_dim(2).unwrap());
        let fields = Fields::from([("content".to_owned(), "x".to_owned())]);
        let keys: Vec<String> = (0..8).map(|i| format!("mem:episodic:{i}")).collect();
        let workers: Vec<_> = (0..4)
            .map(|w| {
                let store = std::sync::Arc::clone(&store);
                let keys = keys.clone();
                let fields = fields.clone();
                std::thread::spawn(move || {
                    for round in 0..200 {
                        let key = &keys[(round + w) % keys.len()];
                        if (round + w) % 3 == 0 {
                            store.delete(key).unwrap();
                        } else {
                            store.upsert(key, &fields, Some(&[1.0, 0.0])).unwrap();
                        }
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let rows = store.scan_prefix("mem:episodic:").unwrap();
        let with_vector = store.memories_without_vectors().unwrap();
        assert!(
            with_vector.is_empty(),
            "every row was written with a vector"
        );
        assert_eq!(store.vector_count(Namespace::Episodic), rows.len());
        let vectors = store.get_vectors_multi(&rows);
        assert!(vectors.iter().all(Option::is_some));
        let report = store.reload_vectors().unwrap();
        assert_eq!(report[&Namespace::Episodic], (rows.len(), rows.len()));
    }

    #[test]
    fn json_paths_quote_names_and_refuse_the_unquotable() {
        assert_eq!(
            json_paths(&["state", "a.b", "with space"]).unwrap(),
            [r#"$."state""#, r#"$."a.b""#, r#"$."with space""#]
        );
        assert!(json_paths(&["state", "quo\"te"]).is_none());
        assert!(json_paths(&["back\\slash"]).is_none());
    }

    #[test]
    fn repeated_keys_each_get_the_record() {
        let store = Store::open_in_memory_with_dim(2).unwrap();
        let fields = Fields::from([("content".to_owned(), "x".to_owned())]);
        store.set_fields("mem:episodic:a", &fields).unwrap();
        let twice = vec!["mem:episodic:a".to_owned(), "mem:episodic:a".to_owned()];
        let rows = store.get_multi(&twice).unwrap();
        assert!(rows.iter().all(Option::is_some), "{rows:?}");
        let rows = store.get_fields_multi(&twice, &["content"]).unwrap();
        assert!(rows.iter().all(Option::is_some), "{rows:?}");
    }
}
