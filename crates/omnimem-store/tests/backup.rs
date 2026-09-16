//! Backup import and export against 6.x-shaped data.

use omnimem_core::{EmbeddingError, Namespace, TextEmbedder, VECTOR_DIM};
use omnimem_store::{
    BackupFile, Fields, SearchFilter, Store, StoreError, discovery_text, read_backup, write_backup,
};
use serde_json::json;

/// Deterministic bag-of-words vectors, so tests can predict nearest neighbours.
struct Fake;

fn fake_vector(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; VECTOR_DIM];
    for word in text.split_whitespace() {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in word.to_lowercase().bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        v[(h % VECTOR_DIM as u64) as usize] += 1.0;
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    } else {
        v[0] = 1.0;
    }
    v
}

impl TextEmbedder for Fake {
    fn dimension(&self) -> usize {
        VECTOR_DIM
    }
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|t| fake_vector(t)).collect())
    }
}

struct WrongSize;

impl TextEmbedder for WrongSize {
    fn dimension(&self) -> usize {
        768
    }
    fn embed_texts(&self, _: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        unreachable!()
    }
}

fn fields(pairs: &[(&str, &str)]) -> Fields {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

/// A backup exactly as 6.5 would have written it: strings everywhere, no
/// licence, provenance or v7 fields.
fn legacy_backup() -> BackupFile {
    let recent = now() - 3600.0;
    BackupFile::from_value(json!({
        "metadata": {"exported_at": "2026-07-01T00:00:00Z", "total_keys": 8, "version": "6.5.1"},
        "data": {
            "mem:episodic:01A": {"content": "valkey search tag filters", "state": "active", "updated_at": "100.0", "tags": "[\"valkey\"]"},
            "mem:knowledge:art": {"content": "an article about rust", "feed_name": "This Week in Rust", "updated_at": "100.0"},
            "mem:knowledge:fact": {"content": "a fact", "enriched_from": "mem:episodic:01A", "updated_at": "100.0"},
            "mem:skill:gen:python-local": {"name": "python-local", "description": "Python work", "domain": "python", "body": "---", "updated_at": "100.0"},
            "topics:suppressed": {"_type": "set", "members": ["alpine"]},
            "meta:tool_metrics:recall": {"call_count": "12"},
            (format!("log:recall:{recent}")): {"query": "q", "timestamp": recent.to_string()},
            "log:recall:1000.0": {"query": "ancient", "timestamp": "1000.0"},
            "secret:nope": {"x": "y"}
        }
    }))
    .unwrap()
}

#[test]
fn a_legacy_backup_imports_migrates_and_embeds() {
    let store = Store::open_in_memory().unwrap();
    let mut calls = Vec::new();
    let report = store
        .restore_backup(&legacy_backup(), Some(&Fake), &mut |done, total| {
            calls.push((done, total));
        })
        .unwrap();

    assert_eq!(report.memories, 4);
    assert_eq!(report.sets, 1);
    assert_eq!(
        report.other_records, 2,
        "tool metrics and the recent recall log"
    );
    assert_eq!(report.skipped_expired, 1);
    assert_eq!(report.skipped_invalid, 1);
    assert_eq!(report.embedded, 4, "skills embed their discovery text");
    assert_eq!(calls.last(), Some(&(4, 4)));

    let episodic = store.get("mem:episodic:01A").unwrap().unwrap();
    assert_eq!(episodic["licence"], "own");
    assert_eq!(episodic["provenance"], "concluded");
    assert_eq!(episodic["epoch"], "1");
    let article = store.get("mem:knowledge:art").unwrap().unwrap();
    assert_eq!(article["licence"], "unknown");
    assert_eq!(article["provenance"], "retrieved");
    assert_eq!(article["project"], "RSS");
    assert_eq!(
        store.get("mem:knowledge:fact").unwrap().unwrap()["licence"],
        "own"
    );

    assert_eq!(store.set_members("topics:suppressed").unwrap(), ["alpine"]);
    assert_eq!(
        store
            .hash_get_all("meta:tool_metrics:recall")
            .unwrap()
            .unwrap()["call_count"],
        "12"
    );

    let skill_vector = store
        .get_vectors_multi(&["mem:skill:gen:python-local".to_owned()])
        .remove(0)
        .unwrap();
    assert_eq!(
        skill_vector,
        fake_vector(&discovery_text("python-local", "Python work", "python"))
    );

    let hits = store
        .search(
            Namespace::Episodic,
            &fake_vector("valkey search tag filters"),
            3,
            &SearchFilter::default(),
            None,
        )
        .unwrap();
    assert_eq!(hits[0].key, "mem:episodic:01A");
    assert!(hits[0].similarity() > 0.999);
}

#[test]
fn newer_records_in_the_store_win() {
    let store = Store::open_in_memory().unwrap();
    store
        .set_fields(
            "mem:episodic:01A",
            &fields(&[("content", "edited since"), ("updated_at", "500.0")]),
        )
        .unwrap();
    store
        .set_fields(
            "mem:knowledge:art",
            &fields(&[("content", "stale"), ("updated_at", "50.0")]),
        )
        .unwrap();
    let report = store
        .restore_backup(&legacy_backup(), None, &mut |_, _| {})
        .unwrap();
    assert_eq!(report.skipped_older, 1);
    assert_eq!(
        store.get("mem:episodic:01A").unwrap().unwrap()["content"],
        "edited since"
    );
    assert_eq!(
        store.get("mem:knowledge:art").unwrap().unwrap()["content"],
        "an article about rust"
    );
    assert_eq!(report.embedded, 0, "no embedder, no vectors");
}

#[test]
fn export_then_import_round_trips() {
    let source = Store::open_in_memory().unwrap();
    source
        .restore_backup(&legacy_backup(), Some(&Fake), &mut |_, _| {})
        .unwrap();
    let dump = source.dump().unwrap();
    assert_eq!(dump.metadata["namespaces"]["episodic"], 1);
    assert_eq!(dump.metadata["namespaces"]["skill"], 1);
    assert_eq!(
        dump.data["topics:suppressed"],
        json!({"_type": "set", "members": ["alpine"]})
    );
    assert!(dump.data.keys().all(|k| !k.starts_with("secret:")));

    let path = std::env::temp_dir().join(format!(
        "omnimem-backup-roundtrip-{}.json",
        std::process::id()
    ));
    write_backup(&path, &dump).unwrap();
    let read = read_backup(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(read.data, dump.data);

    let target = Store::open_in_memory().unwrap();
    target
        .restore_backup(&read, Some(&Fake), &mut |_, _| {})
        .unwrap();
    for key in source.scan_prefix("mem:").unwrap() {
        let mut expected = source.get(&key).unwrap().unwrap();
        let mut actual = target.get(&key).unwrap().unwrap();
        // Identity travels with the record; nothing else may change either.
        assert_eq!(
            expected.remove("origin_id"),
            actual.remove("origin_id"),
            "{key}"
        );
        assert_eq!(expected, actual, "{key}");
    }
    assert_eq!(target.vector_count(Namespace::Knowledge), 2);
}

#[test]
fn an_embedder_of_the_wrong_size_is_refused_before_writing() {
    let store = Store::open_in_memory().unwrap();
    assert!(matches!(
        store.restore_backup(&legacy_backup(), Some(&WrongSize), &mut |_, _| {}),
        Err(StoreError::DimensionMismatch {
            expected: 384,
            got: 768
        })
    ));
    assert!(store.scan_prefix("mem:").unwrap().is_empty());
}

#[test]
fn malformed_backups_are_reported() {
    assert!(matches!(
        BackupFile::from_value(json!([1, 2])),
        Err(StoreError::Backup(_))
    ));
    assert!(matches!(
        BackupFile::from_value(json!({"metadata": {}})),
        Err(StoreError::Backup(_))
    ));
}
