//! Memory records: the operations the 6.x `ValkeyStore` offered, over SQLite.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use omnimem_core::classification::DEFAULT_CLASSIFICATION;
use omnimem_core::{Namespace, VECTOR_DIM, content_hash};
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
const IN_CHUNK: usize = 500;

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

    pub fn get_multi(&self, keys: &[String]) -> Result<Vec<Option<Fields>>> {
        let conn = self.conn();
        let mut found: HashMap<String, Fields> = HashMap::new();
        let memory_keys: Vec<&String> = keys.iter().filter(|k| k.starts_with("mem:")).collect();
        for chunk in memory_keys.chunks(IN_CHUNK) {
            let sql = format!(
                "SELECT key, fields FROM memories WHERE key IN ({})",
                placeholders(chunk.len())
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(chunk.iter()))?;
            while let Some(row) = rows.next()? {
                let key: String = row.get(0)?;
                let raw: String = row.get(1)?;
                let fields = parse_fields(&key, &raw)?;
                found.insert(key, fields);
            }
        }
        drop(conn);
        keys.iter()
            .map(|k| {
                if k.starts_with("mem:") {
                    Ok(found.remove(k).filter(|f| !f.is_empty()))
                } else {
                    self.hash_get_all(k)
                }
            })
            .collect()
    }

    /// A fixed projection of fields for many keys; `None` where a key has
    /// none of them.
    pub fn get_fields_multi(
        &self,
        keys: &[String],
        fields: &[&str],
    ) -> Result<Vec<Option<Fields>>> {
        Ok(self
            .get_multi(keys)?
            .into_iter()
            .map(|row| {
                row.map(|all| {
                    fields
                        .iter()
                        .filter_map(|f| all.get(*f).map(|v| ((*f).to_owned(), v.clone())))
                        .collect::<Fields>()
                })
                .filter(|projected| !projected.is_empty())
            })
            .collect())
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

    /// Hard delete; a memory's vector goes with it.
    pub fn delete_many(&self, keys: &[String]) -> Result<usize> {
        for key in keys {
            validate_key(key)?;
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut deleted = 0;
        let mut removed_memories = Vec::new();
        for key in keys {
            let n = if key.starts_with("mem:") {
                let n = tx.execute("DELETE FROM memories WHERE key = ?1", params![key])?;
                if n > 0 {
                    removed_memories.push(key.clone());
                }
                n
            } else {
                tx.execute("DELETE FROM kv WHERE key = ?1", params![key])?
            };
            deleted += n;
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
        if "mem:".starts_with(prefix) || prefix.starts_with("mem:") {
            let mut stmt =
                conn.prepare("SELECT key FROM memories WHERE key >= ?1 AND key < ?2 ORDER BY key")?;
            let rows = stmt.query_map(params![prefix, upper], |r| r.get::<_, String>(0))?;
            for key in rows {
                keys.push(key?);
            }
        }
        if !prefix.starts_with("mem:") {
            let mut stmt = conn.prepare(
                "SELECT key FROM kv WHERE key >= ?1 AND key < ?2 AND (expires_at IS NULL OR expires_at > ?3) ORDER BY key",
            )?;
            let rows = stmt.query_map(params![prefix, upper, crate::time::now()], |r| {
                r.get::<_, String>(0)
            })?;
            for key in rows {
                keys.push(key?);
            }
        }
        keys.sort();
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
                    Some(names) => names
                        .iter()
                        .filter_map(|f| all.get(*f).map(|v| ((*f).to_owned(), v.clone())))
                        .collect(),
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
        let mut args: Vec<&str> = vec![namespace.as_str()];
        if !filter.states.is_empty() {
            sql.push_str(&format!(
                " AND COALESCE(state, 'active') IN ({})",
                placeholders(filter.states.len())
            ));
            args.extend(filter.states.iter().map(String::as_str));
        }
        if !filter.projects.is_empty() {
            let marks = placeholders(filter.projects.len());
            sql.push_str(&format!(
                " AND (project IN ({marks}) OR project_name IN ({marks}))"
            ));
            args.extend(filter.projects.iter().map(String::as_str));
            args.extend(filter.projects.iter().map(String::as_str));
        }
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args), |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<HashSet<_>, _>>()
            .map_err(StoreError::from)
    }
}

// -- shared row helpers ------------------------------------------------------

/// The longest key the store accepts. Keys are ULIDs, project names and
/// `mem:skill:gen:<domain>-<user>` forms, all far shorter; the cap keeps a
/// backup or bundle from smuggling in a key the size of a record.
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

pub(crate) fn parse_fields(key: &str, raw: &str) -> Result<Fields> {
    serde_json::from_str(raw).map_err(|source| StoreError::CorruptRecord {
        key: key.to_owned(),
        source,
    })
}

pub(crate) fn load_fields(conn: &Connection, key: &str) -> Result<Option<Fields>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT fields FROM memories WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?;
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
    conn.execute(
        "INSERT INTO memories (key, namespace, fields) VALUES (?1, ?2, ?3)
         ON CONFLICT (key) DO UPDATE SET fields = excluded.fields",
        params![key, namespace.as_str(), json],
    )?;
    Ok(fields)
}

pub(crate) fn write_vector(conn: &Connection, key: &str, vector: &[f32]) -> Result<()> {
    conn.execute(
        "INSERT INTO vectors (key, data) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET data = excluded.data",
        params![key, to_bytes(vector)],
    )?;
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
}
