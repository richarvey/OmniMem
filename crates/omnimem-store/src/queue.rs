//! The enrichment queue and per-memory recall counters.
//!
//! 6.x kept enrichment jobs in a Valkey list the worker popped before
//! processing, so a crash mid-job lost it. Here the queue is a table: a job
//! is only removed once the worker says it is done.

use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::store::{Fields, load_fields, memory_namespace, merge_memory};
use crate::time::now;
use crate::{Result, Store};

impl Store {
    /// Append a job. Returns its id.
    pub fn enqueue_enrichment(&self, payload: &Value) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO enrich_queue (payload, created_at) VALUES (?1, ?2)",
            params![payload.to_string(), now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Jobs waiting to be processed.
    pub fn enrichment_pending(&self) -> Result<usize> {
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM enrich_queue", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// The oldest job, left in the queue until [`Store::complete_enrichment`].
    /// A payload that isn't valid JSON comes back as `null`, so the worker can
    /// drop it instead of stalling on it.
    pub fn next_enrichment(&self) -> Result<Option<(i64, Value)>> {
        let row: Option<(i64, String)> = self
            .conn()
            .query_row(
                "SELECT id, payload FROM enrich_queue ORDER BY id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(id, raw)| (id, serde_json::from_str(&raw).unwrap_or(Value::Null))))
    }

    pub fn complete_enrichment(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM enrich_queue WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// After a recall: `recall_count` +1 and `last_recalled` on each memory,
    /// in one transaction. Keys that no longer exist are skipped.
    pub fn bump_recall_counts(&self, keys: &[String], timestamp: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for key in keys {
            let Ok(namespace) = memory_namespace(key) else {
                continue;
            };
            let Some(fields) = load_fields(&tx, key)? else {
                continue;
            };
            let count = fields
                .get("recall_count")
                .and_then(|c| c.trim().parse::<i64>().ok())
                .unwrap_or(0);
            let updates = Fields::from([
                (
                    "recall_count".to_owned(),
                    count.saturating_add(1).to_string(),
                ),
                ("last_recalled".to_owned(), timestamp.to_owned()),
            ]);
            merge_memory(&tx, key, namespace, &updates, &self.origin_id)?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn jobs_stay_until_completed() {
        let store = Store::open_in_memory().unwrap();
        let first = store
            .enqueue_enrichment(&json!({"key": "mem:episodic:a"}))
            .unwrap();
        store
            .enqueue_enrichment(&json!({"key": "mem:episodic:b"}))
            .unwrap();
        assert_eq!(store.enrichment_pending().unwrap(), 2);
        let (id, payload) = store.next_enrichment().unwrap().unwrap();
        assert_eq!(id, first);
        assert_eq!(payload["key"], "mem:episodic:a");
        assert_eq!(
            store.next_enrichment().unwrap().unwrap().0,
            first,
            "not removed by peeking"
        );
        store.complete_enrichment(id).unwrap();
        assert_eq!(
            store.next_enrichment().unwrap().unwrap().1["key"],
            "mem:episodic:b"
        );
    }

    #[test]
    fn recall_counts_increment() {
        let store = Store::open_in_memory().unwrap();
        store.set_field("mem:episodic:a", "content", "x").unwrap();
        let keys = vec!["mem:episodic:a".to_owned(), "mem:episodic:gone".to_owned()];
        store.bump_recall_counts(&keys, "100.5").unwrap();
        store.bump_recall_counts(&keys, "200.5").unwrap();
        let f = store.get("mem:episodic:a").unwrap().unwrap();
        assert_eq!(f["recall_count"], "2");
        assert_eq!(f["last_recalled"], "200.5");
        assert!(store.get("mem:episodic:gone").unwrap().is_none());
    }
}
