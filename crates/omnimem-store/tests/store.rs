//! Store behaviour through the public API, on SQLite in memory: no fakes.

use omnimem_core::classification::DEFAULT_CLASSIFICATION;
use omnimem_core::{Namespace, content_hash};
use omnimem_store::{Fields, SearchFilter, Store, StoreError};

fn fields(pairs: &[(&str, &str)]) -> Fields {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn unit(values: [f32; 3]) -> Vec<f32> {
    let n = values.iter().map(|x| x * x).sum::<f32>().sqrt();
    values.iter().map(|x| x / n).collect()
}

fn store() -> Store {
    Store::open_in_memory_with_dim(3).unwrap()
}

#[test]
fn upsert_merges_fields_and_stamps_v7_identity() {
    let s = store();
    s.upsert(
        "mem:episodic:01A",
        &fields(&[("content", "first"), ("state", "active")]),
        Some(&unit([1.0, 0.0, 0.0])),
    )
    .unwrap();
    let f = s.get("mem:episodic:01A").unwrap().unwrap();
    assert_eq!(f["content_hash"], content_hash("first"));
    assert_eq!(f["epoch"], "1");
    assert_eq!(f["origin_id"], s.origin_id());
    assert_eq!(f["classification"], DEFAULT_CLASSIFICATION);

    s.upsert("mem:episodic:01A", &fields(&[("tags", "[]")]), None)
        .unwrap();
    let f = s.get("mem:episodic:01A").unwrap().unwrap();
    assert_eq!(
        f["content"], "first",
        "fields not mentioned are kept, as HSET did"
    );
    assert_eq!(f["tags"], "[]");

    s.set_field("mem:episodic:01A", "content", "second")
        .unwrap();
    let f = s.get("mem:episodic:01A").unwrap().unwrap();
    assert_eq!(f["content_hash"], content_hash("second"));
    assert_eq!(f["epoch"], "1", "bumping epoch is the engine's decision");
}

#[test]
fn set_fields_creates_a_memory_like_hset() {
    let s = store();
    s.set_fields("mem:knowledge:x", &fields(&[("feed_name", "Feed")]))
        .unwrap();
    assert_eq!(
        s.get("mem:knowledge:x").unwrap().unwrap()["feed_name"],
        "Feed"
    );
    assert_eq!(s.count_records(Namespace::Knowledge).unwrap(), 1);
}

#[test]
fn delete_takes_the_vector_with_it() {
    let s = store();
    s.upsert(
        "mem:episodic:a",
        &fields(&[("content", "a")]),
        Some(&unit([1.0, 0.0, 0.0])),
    )
    .unwrap();
    assert!(s.delete("mem:episodic:a").unwrap());
    assert!(s.get("mem:episodic:a").unwrap().is_none());
    assert_eq!(
        s.get_vectors_multi(&["mem:episodic:a".to_owned()]),
        vec![None]
    );
    assert!(
        s.search(
            Namespace::Episodic,
            &unit([1.0, 0.0, 0.0]),
            5,
            &SearchFilter::default(),
            None
        )
        .unwrap()
        .is_empty()
    );
    assert!(!s.delete("mem:episodic:a").unwrap());
}

#[test]
fn search_orders_filters_and_projects() {
    let s = store();
    let add = |key: &str, v: [f32; 3], extra: &[(&str, &str)]| {
        let mut f = fields(&[("content", key)]);
        f.extend(fields(extra));
        s.upsert(key, &f, Some(&unit(v))).unwrap();
    };
    add(
        "mem:episodic:near",
        [1.0, 0.1, 0.0],
        &[("state", "active"), ("project", "omnimem")],
    );
    add(
        "mem:episodic:mid",
        [1.0, 1.0, 0.0],
        &[("state", "deprioritised"), ("project", "other")],
    );
    add(
        "mem:episodic:far",
        [0.0, 0.0, 1.0],
        &[("state", "archived"), ("project", "omnimem")],
    );
    add(
        "mem:episodic:nostate",
        [1.0, 0.5, 0.0],
        &[("project", "omnimem")],
    );
    add("mem:knowledge:elsewhere", [1.0, 0.0, 0.0], &[]);

    let q = unit([1.0, 0.0, 0.0]);
    let all = s
        .search(Namespace::Episodic, &q, 10, &SearchFilter::default(), None)
        .unwrap();
    let keys: Vec<&str> = all.iter().map(|h| h.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "mem:episodic:near",
            "mem:episodic:nostate",
            "mem:episodic:mid",
            "mem:episodic:far"
        ]
    );
    assert!(all[0].similarity() > all[3].similarity());

    let live = SearchFilter {
        states: vec!["active".into(), "deprioritised".into()],
        projects: vec![],
    };
    let keys: Vec<String> = s
        .search(Namespace::Episodic, &q, 10, &live, None)
        .unwrap()
        .into_iter()
        .map(|h| h.key)
        .collect();
    assert_eq!(
        keys,
        [
            "mem:episodic:near",
            "mem:episodic:nostate",
            "mem:episodic:mid"
        ],
        "no state counts as active"
    );

    let scoped = SearchFilter {
        states: vec!["active".into()],
        projects: vec!["omnimem".into()],
    };
    let keys: Vec<String> = s
        .search(Namespace::Episodic, &q, 10, &scoped, None)
        .unwrap()
        .into_iter()
        .map(|h| h.key)
        .collect();
    assert_eq!(keys, ["mem:episodic:near", "mem:episodic:nostate"]);

    let projected = s
        .search(
            Namespace::Episodic,
            &q,
            1,
            &SearchFilter::default(),
            Some(&["state"]),
        )
        .unwrap();
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].fields, fields(&[("state", "active")]));
}

#[test]
fn project_filter_matches_project_name_too() {
    let s = store();
    s.upsert(
        "mem:project:omnimem",
        &fields(&[("project_name", "omnimem"), ("content", "ctx")]),
        Some(&unit([1.0, 0.0, 0.0])),
    )
    .unwrap();
    let filter = SearchFilter {
        states: vec![],
        projects: vec!["omnimem".into()],
    };
    assert_eq!(
        s.search(Namespace::Project, &unit([1.0, 0.0, 0.0]), 5, &filter, None)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn top_k_is_clamped_and_dimensions_checked() {
    let s = store();
    s.upsert(
        "mem:episodic:a",
        &fields(&[("content", "a")]),
        Some(&unit([1.0, 0.0, 0.0])),
    )
    .unwrap();
    s.upsert(
        "mem:episodic:b",
        &fields(&[("content", "b")]),
        Some(&unit([0.0, 1.0, 0.0])),
    )
    .unwrap();
    assert_eq!(
        s.search(
            Namespace::Episodic,
            &unit([1.0, 0.0, 0.0]),
            0,
            &SearchFilter::default(),
            None
        )
        .unwrap()
        .len(),
        1
    );
    assert!(matches!(
        s.search(
            Namespace::Episodic,
            &[1.0, 0.0],
            5,
            &SearchFilter::default(),
            None
        ),
        Err(StoreError::DimensionMismatch {
            expected: 3,
            got: 2
        })
    ));
    assert!(matches!(
        s.upsert("mem:episodic:c", &fields(&[]), Some(&[1.0])),
        Err(StoreError::DimensionMismatch { .. })
    ));
}

#[test]
fn keys_are_validated() {
    let s = store();
    assert!(matches!(
        s.set_field("secret:x", "a", "b"),
        Err(StoreError::InvalidKey(_))
    ));
    assert!(matches!(
        s.upsert("mem:bogus:x", &fields(&[]), None),
        Err(StoreError::InvalidKey(_))
    ));
}

#[test]
fn scan_prefix_spans_memories_and_other_records() {
    let s = store();
    s.set_field("mem:episodic:b", "content", "b").unwrap();
    s.set_field("mem:episodic:a", "content", "a").unwrap();
    s.set_field("mem:project:p", "content", "p").unwrap();
    s.hash_set(
        "meta:maintenance:omnimem",
        &fields(&[("briefing_count", "1")]),
    )
    .unwrap();
    assert_eq!(
        s.scan_prefix("mem:episodic:").unwrap(),
        ["mem:episodic:a", "mem:episodic:b"]
    );
    assert_eq!(
        s.scan_prefix("meta:").unwrap(),
        ["meta:maintenance:omnimem"]
    );
    assert_eq!(s.scan_prefix("mem:").unwrap().len(), 3);
    assert_eq!(s.scan_prefix("").unwrap().len(), 4);
}

#[test]
fn get_fields_multi_projects_and_aligns() {
    let s = store();
    s.set_fields(
        "mem:episodic:a",
        &fields(&[("state", "active"), ("content", "x")]),
    )
    .unwrap();
    let keys = vec![
        "mem:episodic:a".to_owned(),
        "mem:episodic:missing".to_owned(),
    ];
    let rows = s.get_fields_multi(&keys, &["state", "outcome"]).unwrap();
    assert_eq!(rows, vec![Some(fields(&[("state", "active")])), None]);
}

#[test]
fn counts_cover_every_namespace() {
    let s = store();
    s.set_field("mem:preference:a", "content", "x").unwrap();
    let counts = s.count_all_records().unwrap();
    assert_eq!(counts.len(), Namespace::ALL.len());
    assert_eq!(counts[&Namespace::Preference], 1);
    assert_eq!(counts[&Namespace::Episodic], 0);
}

#[test]
fn records_and_vectors_survive_reopening() {
    let path = std::env::temp_dir().join(format!("omnimem-store-reopen-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let origin;
    {
        let s = Store::open(&path).unwrap();
        origin = s.origin_id().to_owned();
        let v = vec![1.0 / (omnimem_core::VECTOR_DIM as f32).sqrt(); omnimem_core::VECTOR_DIM];
        s.upsert(
            "mem:episodic:kept",
            &fields(&[("content", "kept")]),
            Some(&v),
        )
        .unwrap();
    }
    let s = Store::open(&path).unwrap();
    assert_eq!(s.origin_id(), origin, "the node identity is permanent");
    assert_eq!(s.vector_count(Namespace::Episodic), 1);
    let q = vec![1.0; omnimem_core::VECTOR_DIM];
    let hits = s
        .search(Namespace::Episodic, &q, 1, &SearchFilter::default(), None)
        .unwrap();
    assert_eq!(hits[0].key, "mem:episodic:kept");
    drop(s);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
