//! 6.x backup files: `dump_to_file` JSON in, and out again in the same shape.
//!
//! Import follows `restore_from_file`: hashes merge, and a record already in
//! the store with an `updated_at` at least as new as the backup's is left
//! alone. Then the migrations run, and every restored memory is re-embedded,
//! because backups never carried vectors.
//!
//! Two deliberate differences from 6.x. Recall logs keep their 30-day expiry
//! (Python restored them with none, so an old dump resurrected them forever),
//! and ones already past it are skipped. And every migration runs, not only
//! licence and provenance, because importing is how a 6.x store arrives.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use omnimem_core::TextEmbedder;
use serde::Serialize;
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use crate::kv::{hash_merge_expiring, set_union};
use crate::migrations::MigrationReport;
use crate::store::{
    discovery_text, load_fields, memory_namespace, merge_memory, validate_key, write_vector,
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

#[derive(Debug, Clone, PartialEq)]
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
    pub embedded: usize,
    /// Restored memories with no text to embed.
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

/// Written beside the target and renamed into place.
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
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(io)?;
    }
    let partial = path.with_extension("json.partial");
    fs::write(&partial, body).map_err(io)?;
    fs::rename(&partial, path).map_err(io)?;
    Ok(())
}

impl BackupFile {
    pub fn from_value(value: Value) -> Result<Self> {
        let Value::Object(mut top) = value else {
            return Err(StoreError::Backup("a backup is a JSON object".into()));
        };
        let data = match top.remove("data") {
            Some(Value::Object(data)) => data,
            _ => return Err(StoreError::Backup("the backup has no `data` object".into())),
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
                    warn!(key = %key.chars().take(50).collect::<String>(), "skipping a key with an unknown prefix");
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
                        set_union(&tx, key, &members)?;
                        report.sets += 1;
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
                    let at = fields
                        .get("timestamp")
                        .and_then(|t| t.parse::<f64>().ok())
                        .or_else(|| stamp.parse::<f64>().ok());
                    if let Some(at) = at {
                        if at + RECALL_LOG_TTL <= now() {
                            report.skipped_expired += 1;
                            continue;
                        }
                        expires_at = Some(at + RECALL_LOG_TTL);
                    }
                }

                if key.starts_with("mem:") {
                    let incoming = fields
                        .get("updated_at")
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    let stored = load_fields(&tx, key)?
                        .and_then(|f| f.get("updated_at").and_then(|v| v.parse::<f64>().ok()));
                    if stored.is_some_and(|stored| stored >= incoming) {
                        report.skipped_older += 1;
                        continue;
                    }
                    merge_memory(&tx, key, memory_namespace(key)?, &fields, &self.origin_id)?;
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
            let (embedded, not_embedded) = self.embed_memories(&restored, embedder, progress)?;
            report.embedded = embedded;
            report.not_embedded = not_embedded;
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
                tx.commit()?;
            }
            {
                let mut index = self.vectors_write();
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
