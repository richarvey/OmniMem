//! 6.x backup files: `dump_to_file` JSON in, and out again in the same shape.
//!
//! Import follows `restore_from_file`: hashes merge, and a record already in
//! the store with an `updated_at` at least as new as the backup's is left
//! alone. Then the migrations run, and every restored memory is re-embedded,
//! because backups never carried vectors; so is any memory already in the
//! store without a vector, so a restore that was cut short can be re-run.
//!
//! Two deliberate differences from 6.x. Recall logs keep their 30-day expiry
//! (Python restored them with none, so an old dump resurrected them forever),
//! and ones already past it are skipped. And every migration runs, not only
//! licence and provenance, because importing is how a 6.x store arrives.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use omnimem_core::TextEmbedder;
use serde::Serialize;
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use crate::kv::{hash_merge_expiring, set_union};
use crate::migrations::MigrationReport;
use crate::store::{
    discovery_text, load_fields, memory_namespace, merge_memory_into, validate_key, write_vector,
};
use crate::time::{iso8601_utc, now};
use crate::{Fields, Result, Store, StoreError};

/// The key families a backup carries, as `dump_all` exported them.
pub const BACKUP_KEY_PREFIXES: [&str; 4] = ["mem:", "topics:", "log:recall:", "meta:"];

/// Recall logs expire after this long, as the recall pipeline sets them.
const RECALL_LOG_TTL: f64 = 30.0 * 86_400.0;

/// Texts per embedding call.
const EMBED_BATCH: usize = 32;

const NAMESPACE_COUNTS: [&str; 5] = ["episodic", "project", "knowledge", "preference", "skill"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFile {
    pub metadata: Value,
    pub data: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// Memories written (new, or newer than what the store had).
    pub memories: usize,
    /// Non-memory hashes written: tool metrics, maintenance counters, recall logs.
    pub other_records: usize,
    pub sets: usize,
    pub skipped_older: usize,
    pub skipped_invalid: usize,
    pub skipped_expired: usize,
    /// Memories given a vector: the ones restored, plus any the store
    /// already held without one.
    pub embedded: usize,
    /// Memories that needed a vector but had no text to embed.
    pub not_embedded: usize,
    pub migrations: MigrationReport,
}

pub fn read_backup(path: &Path) -> Result<BackupFile> {
    let raw = fs::read_to_string(path).map_err(|source| StoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|e| StoreError::Backup(format!("{} is not valid JSON: {e}", path.display())))?;
    BackupFile::from_value(value)
}

/// Distinguishes partial files written by this process in the same instant.
static PARTIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write a backup beside the target and rename it into place.
///
/// A backup is never overwritten: a caller naming an existing file gets
/// [`StoreError::Backup`], so a repeated name (or a request that guesses
/// one) cannot replace the copy already on disk. The partial file has a
/// unique name and is created exclusively, so two writers never share one.
pub fn write_backup(path: &Path, backup: &BackupFile) -> Result<()> {
    let body = serde_json::to_string_pretty(&json!({
        "metadata": backup.metadata,
        "data": backup.data,
    }))
    .map_err(|e| StoreError::Backup(e.to_string()))?;
    let io = |source| StoreError::Io {
        path: path.display().to_string(),
        source,
    };
    let exists = || StoreError::Backup(format!("{} already exists", path.display()));
    if path.exists() {
        return Err(exists());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(io)?;
    }
    let partial = path.with_extension(format!(
        "json.{}.{}.partial",
        std::process::id(),
        PARTIAL_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .and_then(|mut file| {
            file.write_all(body.as_bytes())
                .and_then(|()| file.sync_all())
        });
    if let Err(source) = written {
        let _ = fs::remove_file(&partial);
        return Err(io(source));
    }
    // Re-checked just before the rename; the window between this check and
    // the rename is the only one left, and it is the file system's.
    if path.exists() {
        let _ = fs::remove_file(&partial);
        return Err(exists());
    }
    if let Err(source) = fs::rename(&partial, path) {
        let _ = fs::remove_file(&partial);
        return Err(io(source));
    }
    Ok(())
}

impl BackupFile {
    pub fn from_value(value: Value) -> Result<Self> {
        let Value::Object(mut top) = value else {
            return Err(StoreError::Backup("a backup is a JSON object".into()));
        };
        let Some(Value::Object(data)) = top.remove("data") else {
            return Err(StoreError::Backup("the backup has no `data` object".into()));
        };
        Ok(Self {
            metadata: top.remove("metadata").unwrap_or(Value::Null),
            data,
        })
    }
}

/// A backup value as a stored string. 6.x only ever wrote strings; anything
/// else is kept as its JSON text rather than dropped.
fn as_field(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

impl Store {
    /// Import a backup. `embedder` re-embeds the restored memories; without
    /// one they are stored but not searchable until embedded. `progress` is
    /// called with (embedded so far, to embed).
    pub fn restore_backup(
        &self,
        backup: &BackupFile,
        embedder: Option<&dyn TextEmbedder>,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<ImportReport> {
        if let Some(e) = embedder
            && e.dimension() != self.dimension()
        {
            return Err(StoreError::DimensionMismatch {
                expected: self.dimension(),
                got: e.dimension(),
            });
        }
        let mut report = ImportReport::default();
        let mut restored = Vec::new();
        {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            for (key, value) in &backup.data {
                if validate_key(key).is_err() {
                    warn!(key = %key.chars().take(50).collect::<String>(), "skipping an invalid key");
                    report.skipped_invalid += 1;
                    continue;
                }
                let Value::Object(object) = value else {
                    report.skipped_invalid += 1;
                    continue;
                };

                if object.get("_type").and_then(Value::as_str) == Some("set") {
                    if key.starts_with("mem:") {
                        report.skipped_invalid += 1;
                        continue;
                    }
                    let members: Vec<String> = object
                        .get("members")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(as_field).collect())
                        .unwrap_or_default();
                    if !members.is_empty() {
                        // A key already holding a hash is left alone, as the
                        // hash path leaves a set alone: one odd record must
                        // not abort the whole restore.
                        match set_union(&tx, key, &members) {
                            Ok(_) => report.sets += 1,
                            Err(StoreError::WrongType { .. }) => report.skipped_invalid += 1,
                            Err(e) => return Err(e),
                        }
                    }
                    continue;
                }

                let fields: Fields = object
                    .iter()
                    .filter(|(name, _)| name.as_str() != "vector")
                    .filter_map(|(name, v)| as_field(v).map(|s| (name.clone(), s)))
                    .collect();
                if fields.is_empty() {
                    continue;
                }

                let mut expires_at = None;
                if let Some(stamp) = key.strip_prefix("log:recall:") {
                    // "NaN" or "inf" parse as f64 but make no instant: the
                    // expiry arithmetic below would keep such a log for ever.
                    let at = match fields.get("timestamp").map(|t| t.parse::<f64>()) {
                        Some(Ok(at)) if at.is_finite() => Some(at),
                        Some(Ok(_)) => {
                            report.skipped_invalid += 1;
                            continue;
                        }
                        _ => stamp.parse::<f64>().ok().filter(|at| at.is_finite()),
                    };
                    if let Some(at) = at {
                        if at + RECALL_LOG_TTL <= now() {
                            report.skipped_expired += 1;
                            continue;
                        }
                        expires_at = Some(at + RECALL_LOG_TTL);
                    }
                }

                if key.starts_with("mem:") {
                    // A non-finite `updated_at` compares as newer than
                    // anything (NaN fails every `>=`), so it is refused
                    // rather than allowed to win every merge.
                    let incoming = match fields.get("updated_at").map(|v| v.parse::<f64>()) {
                        Some(Ok(at)) if at.is_finite() => at,
                        Some(Ok(_)) => {
                            report.skipped_invalid += 1;
                            continue;
                        }
                        _ => 0.0,
                    };
                    // Read once: the same record decides whether the backup
                    // is newer and is what the merge builds on.
                    let existing = load_fields(&tx, key)?;
                    let stored = existing.as_ref().and_then(|f| {
                        f.get("updated_at")
                            .and_then(|v| v.parse::<f64>().ok())
                            .filter(|at| at.is_finite())
                    });
                    if stored.is_some_and(|stored| stored >= incoming) {
                        report.skipped_older += 1;
                        continue;
                    }
                    merge_memory_into(
                        &tx,
                        key,
                        memory_namespace(key)?,
                        existing,
                        &fields,
                        &self.origin_id,
                    )?;
                    restored.push(key.clone());
                    report.memories += 1;
                } else {
                    match hash_merge_expiring(&tx, key, &fields, expires_at) {
                        Ok(()) => report.other_records += 1,
                        Err(StoreError::WrongType { .. }) => report.skipped_invalid += 1,
                        Err(e) => return Err(e),
                    }
                }
            }
            tx.commit()?;
        }

        report.migrations = self.run_migrations()?;
        if let Some(embedder) = embedder {
            // The rows are committed before embedding starts, so a run that
            // fails part way leaves memories without vectors, and a re-run
            // would skip them as "not newer". Embedding everything that
            // lacks a vector, not only what this run wrote, makes a re-run
            // (or a later import after `--no-embed`) repair them.
            let to_embed: Vec<String> = restored
                .into_iter()
                .chain(self.memories_without_vectors()?)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let (given_vectors, without_text) =
                self.embed_memories(&to_embed, embedder, progress)?;
            report.embedded = given_vectors;
            report.not_embedded = without_text;
        }
        info!(?report, "backup imported");
        Ok(report)
    }

    /// Embed memories' text (a memory's `content`, a skill's discovery text)
    /// and store the vectors. Returns (embedded, had no text).
    pub fn embed_memories(
        &self,
        keys: &[String],
        embedder: &dyn TextEmbedder,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<(usize, usize)> {
        let rows = self.get_multi(keys)?;
        let mut work: Vec<(String, String)> = Vec::new();
        let mut no_text = 0;
        for (key, row) in keys.iter().zip(rows) {
            let text = row
                .map(|f| {
                    if key.starts_with("mem:skill:") {
                        match f.get("name").filter(|n| !n.is_empty()) {
                            Some(name) => discovery_text(
                                name,
                                f.get("description").map_or("", String::as_str),
                                f.get("domain").map_or("", String::as_str),
                            ),
                            None => String::new(),
                        }
                    } else {
                        f.get("content").cloned().unwrap_or_default()
                    }
                })
                .unwrap_or_default();
            if text.is_empty() {
                no_text += 1;
            } else {
                work.push((key.clone(), text));
            }
        }

        let total = work.len();
        let mut done = 0;
        for batch in work.chunks(EMBED_BATCH) {
            let texts: Vec<&str> = batch.iter().map(|(_, t)| t.as_str()).collect();
            let vectors = embedder
                .embed_texts(&texts)
                .map_err(|e| StoreError::Embedding(e.to_string()))?;
            if vectors.len() != batch.len() {
                return Err(StoreError::Embedding(format!(
                    "asked for {} vectors, got {}",
                    batch.len(),
                    vectors.len()
                )));
            }
            if let Some(bad) = vectors.iter().find(|v| v.len() != self.dimension()) {
                return Err(StoreError::DimensionMismatch {
                    expected: self.dimension(),
                    got: bad.len(),
                });
            }
            {
                let mut conn = self.conn();
                let tx = conn.transaction()?;
                for ((key, _), vector) in batch.iter().zip(&vectors) {
                    write_vector(&tx, key, vector)?;
                }
                // Connection then matrix, held together across the commit,
                // as every writer in the store does.
                let mut index = self.vectors_write();
                tx.commit()?;
                for ((key, _), vector) in batch.iter().zip(&vectors) {
                    index.insert(memory_namespace(key)?, key, vector);
                }
            }
            done += batch.len();
            progress(done, total);
        }
        Ok((done, no_text))
    }

    /// Everything a 6.x `dump_to_file` exported: memories, and the live
    /// hashes and sets under `topics:`, `log:recall:` and `meta:`. Strings
    /// (sessions, caches) are left out, as they were. Vectors never travel.
    pub fn dump(&self) -> Result<BackupFile> {
        let mut data = Map::new();
        let mut counts: BTreeMap<&str, usize> = NAMESPACE_COUNTS.iter().map(|n| (*n, 0)).collect();
        let conn = self.conn();

        let mut stmt = conn.prepare("SELECT key, namespace, fields FROM memories ORDER BY key")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let namespace: String = row.get(1)?;
            let raw: String = row.get(2)?;
            let fields: Value =
                serde_json::from_str(&raw).map_err(|source| StoreError::CorruptRecord {
                    key: key.clone(),
                    source,
                })?;
            if let Some(n) = counts.get_mut(namespace.as_str()) {
                *n += 1;
            }
            data.insert(key, fields);
        }
        drop(rows);
        drop(stmt);

        let mut stmt = conn.prepare(
            "SELECT key, kind, value FROM kv WHERE expires_at IS NULL OR expires_at > ?1 ORDER BY key",
        )?;
        let mut rows = stmt.query([now()])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            if key.starts_with("mem:") || !BACKUP_KEY_PREFIXES.iter().any(|p| key.starts_with(p)) {
                continue;
            }
            let kind: String = row.get(1)?;
            let raw: String = row.get(2)?;
            let value: Value =
                serde_json::from_str(&raw).map_err(|source| StoreError::CorruptRecord {
                    key: key.clone(),
                    source,
                })?;
            match kind.as_str() {
                "hash" => {
                    data.insert(key, value);
                }
                "set" => {
                    data.insert(key, json!({"_type": "set", "members": value}));
                }
                _ => {}
            }
        }

        let metadata = json!({
            "exported_at": iso8601_utc(now()),
            "total_keys": data.len(),
            "namespaces": counts,
            "version": env!("CARGO_PKG_VERSION"),
        });
        Ok(BackupFile { metadata, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnimem_core::{EmbeddingError, Namespace};

    /// One fixed unit vector per text, enough to see that a memory got one.
    struct Unit;

    impl TextEmbedder for Unit {
        fn dimension(&self) -> usize {
            2
        }
        fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
    }

    fn backup(data: &Value) -> BackupFile {
        BackupFile::from_value(json!({"metadata": {}, "data": data})).unwrap()
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "omnimem-store-backup-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_backup_is_never_overwritten() {
        let dir = scratch("overwrite");
        let path = dir.join("memory_backup.json");
        let first = backup(&json!({"meta:a": {"n": "1"}}));
        write_backup(&path, &first).unwrap();
        let second = backup(&json!({"meta:a": {"n": "2"}}));
        let refused = write_backup(&path, &second).unwrap_err();
        assert!(
            matches!(&refused, StoreError::Backup(m) if m.ends_with("already exists")),
            "{refused}"
        );
        assert_eq!(read_backup(&path).unwrap().data, first.data);
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "memory_backup.json")
            .collect();
        assert!(leftovers.is_empty(), "partial files left: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_set_over_a_hash_is_skipped_not_fatal() {
        let store = Store::open_in_memory_with_dim(2).unwrap();
        store
            .hash_set(
                "topics:suppressed",
                &Fields::from([("x".to_owned(), "1".to_owned())]),
            )
            .unwrap();
        let report = store
            .restore_backup(
                &backup(&json!({
                    "topics:suppressed": {"_type": "set", "members": ["alpine"]},
                    "topics:other": {"_type": "set", "members": ["b"]},
                })),
                None,
                &mut |_, _| {},
            )
            .unwrap();
        assert_eq!(report.skipped_invalid, 1);
        assert_eq!(report.sets, 1);
        assert_eq!(store.set_members("topics:other").unwrap(), ["b"]);
    }

    #[test]
    fn non_finite_timestamps_are_invalid() {
        let store = Store::open_in_memory_with_dim(2).unwrap();
        store
            .set_fields(
                "mem:episodic:a",
                &Fields::from([
                    ("content".to_owned(), "old".to_owned()),
                    ("updated_at".to_owned(), "100".to_owned()),
                ]),
            )
            .unwrap();
        let report = store
            .restore_backup(
                &backup(&json!({
                    "mem:episodic:a": {"content": "new", "updated_at": "NaN"},
                    "mem:episodic:b": {"content": "b", "updated_at": "inf"},
                    "log:recall:1": {"query": "q", "timestamp": "NaN"},
                    "log:recall:2": {"query": "q", "timestamp": "-inf"},
                })),
                None,
                &mut |_, _| {},
            )
            .unwrap();
        assert_eq!(report.skipped_invalid, 4);
        assert_eq!(report.memories, 0);
        assert_eq!(report.other_records, 0);
        assert_eq!(
            store.get("mem:episodic:a").unwrap().unwrap()["content"],
            "old"
        );
        assert!(store.get("mem:episodic:b").unwrap().is_none());

        // A poisoned record already in the store does not block a real one.
        store
            .set_field("mem:episodic:a", "updated_at", "NaN")
            .unwrap();
        let report = store
            .restore_backup(
                &backup(&json!({"mem:episodic:a": {"content": "real", "updated_at": "5"}})),
                None,
                &mut |_, _| {},
            )
            .unwrap();
        assert_eq!(report.memories, 1);
        assert_eq!(
            store.get("mem:episodic:a").unwrap().unwrap()["content"],
            "real"
        );
    }

    #[test]
    fn a_re_run_embeds_what_the_first_run_left_without_vectors() {
        let store = Store::open_in_memory_with_dim(2).unwrap();
        let data = json!({
            "mem:episodic:a": {"content": "alpha", "updated_at": "10"},
            "mem:knowledge:b": {"content": "beta", "updated_at": "10"},
        });
        // As `import --no-embed`, or a run that died before embedding.
        let first = store
            .restore_backup(&backup(&data), None, &mut |_, _| {})
            .unwrap();
        assert_eq!(first.memories, 2);
        assert_eq!(first.embedded, 0);
        assert_eq!(store.vector_count(Namespace::Episodic), 0);
        assert_eq!(
            store.memories_without_vectors().unwrap(),
            ["mem:episodic:a", "mem:knowledge:b"]
        );

        let second = store
            .restore_backup(&backup(&data), Some(&Unit), &mut |_, _| {})
            .unwrap();
        assert_eq!(second.skipped_older, 2, "nothing newer to write");
        assert_eq!(
            second.embedded, 2,
            "but the vectorless memories are repaired"
        );
        assert_eq!(store.vector_count(Namespace::Episodic), 1);
        assert_eq!(store.vector_count(Namespace::Knowledge), 1);
        assert!(store.memories_without_vectors().unwrap().is_empty());
    }
}
