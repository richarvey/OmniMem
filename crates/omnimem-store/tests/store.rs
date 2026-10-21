//! Store behaviour through the public API, on SQLite in memory: no fakes.

use omnimem_core::classification::DEFAULT_CLASSIFICATION;
use omnimem_core::{Namespace, content_hash};
use omnimem_store::{Fields, MemoryFilter, SearchFilter, Store, StoreError};

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

/// Rows with every awkward value the filters have to agree on: states that
/// are missing, empty or unusual; a project on either field or both; feed
/// names; timestamps the engine's parser and SQLite's `CAST` read
/// differently; values with quotes, backslashes and non-ASCII text.
fn awkward_rows(s: &Store) -> Vec<String> {
    let rows: Vec<(&str, Vec<(&str, &str)>)> = vec![
        (
            "mem:episodic:01",
            vec![
                ("content", "plain \"quoted\" back\\slash caf\u{e9}"),
                ("state", "active"),
                ("project", "alpha"),
                ("created_at", "1700000000.5"),
                ("effort_score", "4"),
            ],
        ),
        (
            "mem:episodic:02",
            vec![
                ("content", "no state"),
                ("project", ""),
                ("project_name", "alpha"),
                ("created_at", "nan"),
            ],
        ),
        (
            "mem:episodic:03",
            vec![
                ("content", "empty state"),
                ("state", ""),
                ("project", "beta"),
                ("project_name", "alpha"),
                ("created_at", "inf"),
            ],
        ),
        (
            "mem:episodic:04",
            vec![
                ("state", "deprioritised"),
                ("project_name", "alpha"),
                ("created_at", " 1800000000"),
                ("effort_score", ""),
            ],
        ),
        (
            "mem:episodic:05",
            vec![
                ("content", "archived"),
                ("state", "archived"),
                ("project", "alpha"),
                ("created_at", "1800000000.0"),
            ],
        ),
        (
            "mem:episodic:06",
            vec![("tags", "[\"x\"]"), ("created_at", "abc")],
        ),
        (
            "mem:knowledge:07",
            vec![
                ("content", "article"),
                ("state", "active"),
                ("feed_name", "Feed A"),
                ("created_at", "1800000000.0"),
            ],
        ),
        (
            "mem:knowledge:08",
            vec![
                ("content", "unfed"),
                ("state", "active"),
                ("feed_name", ""),
                ("created_at", "1800000001"),
            ],
        ),
        (
            "mem:knowledge:09",
            vec![("content", "feedless"), ("created_at", "1e12")],
        ),
        (
            "mem:skill:gen:python-local",
            vec![("name", "python-local"), ("domain", "python")],
        ),
        (
            "mem:skill:hand",
            vec![("name", "hand"), ("state", "active")],
        ),
    ];
    rows.iter()
        .map(|(key, pairs)| {
            s.set_fields(key, &fields(pairs)).unwrap();
            (*key).to_owned()
        })
        .collect()
}

#[test]
fn projected_reads_match_whole_records() {
    let s = store();
    let mut keys = awkward_rows(&s);
    keys.push("mem:episodic:missing".to_owned());
    let whole = s.get_multi(&keys).unwrap();
    for projection in [
        &["content", "state", "project", "project_name", "created_at"][..],
        &["effort_score", "tags"],
        &["feed_name"],
        &["nothing_here"],
    ] {
        let projected = s.get_fields_multi(&keys, projection).unwrap();
        let expected: Vec<Option<Fields>> = whole
            .iter()
            .map(|row| {
                row.as_ref().and_then(|all| {
                    let p: Fields = projection
                        .iter()
                        .filter_map(|f| all.get(*f).map(|v| ((*f).to_owned(), v.clone())))
                        .collect();
                    (!p.is_empty()).then_some(p)
                })
            })
            .collect();
        assert_eq!(projected, expected, "projection {projection:?}");
    }
    assert_eq!(
        s.get_fields_multi(&keys, &[]).unwrap(),
        vec![None; keys.len()]
    );
}

/// `list_memories` must return exactly what a full scan filtered in Rust
/// would, because the engine's callers were doing the latter.
#[test]
fn filtered_listings_match_a_scan_filtered_by_hand() {
    let s = store();
    awkward_rows(&s);
    let all_fields = [
        "content",
        "state",
        "project",
        "project_name",
        "feed_name",
        "created_at",
        "effort_score",
        "tags",
        "name",
        "domain",
    ];
    let scan = |ns: Namespace| -> Vec<(String, Fields)> {
        let keys = s.scan_prefix(&format!("mem:{ns}:")).unwrap();
        let rows = s.get_fields_multi(&keys, &all_fields).unwrap();
        keys.into_iter()
            .zip(rows)
            .filter_map(|(k, r)| r.map(|r| (k, r)))
            .collect()
    };
    let by_hand = |ns: Namespace, keep: &dyn Fn(&str, &Fields) -> bool| -> Vec<String> {
        scan(ns)
            .into_iter()
            .filter(|(k, r)| keep(k, r))
            .map(|(k, _)| k)
            .collect()
    };
    let listed = |ns: Namespace, filter: &MemoryFilter| -> Vec<String> {
        s.list_memories(ns, filter, &all_fields)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect()
    };
    let doc_project = |r: &Fields| -> Option<String> {
        r.get("project")
            .filter(|p| !p.is_empty())
            .or_else(|| r.get("project_name").filter(|p| !p.is_empty()))
            .cloned()
    };
    let ep = Namespace::Episodic;
    let kn = Namespace::Knowledge;

    // Unfiltered: the whole namespace, in key order, with the projection.
    assert_eq!(
        s.list_memories(ep, &MemoryFilter::default(), &all_fields)
            .unwrap(),
        scan(ep)
    );

    // States: a missing state is active, an empty one is nothing.
    let active = MemoryFilter {
        states: vec!["active".into()],
        ..MemoryFilter::default()
    };
    assert_eq!(
        listed(ep, &active),
        by_hand(ep, &|_, r| r.get("state").is_none_or(|st| st == "active"))
    );
    assert_eq!(
        listed(ep, &active),
        ["mem:episodic:01", "mem:episodic:02", "mem:episodic:06"]
    );
    let live = MemoryFilter {
        states: vec!["deprioritised".into(), "archived".into()],
        ..MemoryFilter::default()
    };
    assert_eq!(
        listed(ep, &live),
        by_hand(ep, &|_, r| matches!(
            r.get("state").map(String::as_str),
            Some("deprioritised" | "archived")
        ))
    );

    // Project: `project` when non-empty, else `project_name`.
    let alpha = MemoryFilter {
        project: Some("alpha".into()),
        ..MemoryFilter::default()
    };
    assert_eq!(
        listed(ep, &alpha),
        by_hand(ep, &|_, r| doc_project(r).as_deref() == Some("alpha"))
    );
    assert_eq!(
        listed(ep, &alpha),
        [
            "mem:episodic:01",
            "mem:episodic:02",
            "mem:episodic:04",
            "mem:episodic:05"
        ]
    );
    let alpha_field = MemoryFilter {
        project_field: Some("alpha".into()),
        ..MemoryFilter::default()
    };
    assert_eq!(
        listed(ep, &alpha_field),
        by_hand(ep, &|_, r| r.get("project").map(String::as_str)
            == Some("alpha"))
    );
    let nobody = MemoryFilter {
        project: Some(String::new()),
        ..MemoryFilter::default()
    };
    assert!(listed(ep, &nobody).is_empty());

    // Feed names, with the empty name matching records without a feed.
    let feed_a = MemoryFilter {
        feed_names: vec!["Feed A".into()],
        ..MemoryFilter::default()
    };
    assert_eq!(listed(kn, &feed_a), ["mem:knowledge:07"]);
    let unfed = MemoryFilter {
        feed_names: vec![String::new()],
        ..MemoryFilter::default()
    };
    assert_eq!(
        listed(kn, &unfed),
        by_hand(kn, &|_, r| r.get("feed_name").is_none_or(String::is_empty))
    );

    // Presence of a field, whatever its value.
    let with_effort = MemoryFilter {
        present_any: vec!["effort_score".into(), "tags".into()],
        ..MemoryFilter::default()
    };
    assert_eq!(
        listed(ep, &with_effort),
        by_hand(ep, &|_, r| r.contains_key("effort_score")
            || r.contains_key("tags"))
    );

    // Timestamps: a superset of the engine's parse, which re-checks. Every
    // row the engine would keep is listed, and the extras are exactly the
    // ones whose text is not a plain number.
    let since = MemoryFilter {
        created_at_min: Some(1_750_000_000.0),
        ..MemoryFilter::default()
    };
    let parsed = |r: &Fields| r.get("created_at").and_then(|c| c.parse::<f64>().ok());
    let engine_keeps = by_hand(ep, &|_, r| parsed(r).is_some_and(|c| c >= 1_750_000_000.0));
    let sql_keeps = listed(ep, &since);
    assert!(
        engine_keeps.iter().all(|k| sql_keeps.contains(k)),
        "{sql_keeps:?}"
    );
    assert_eq!(
        sql_keeps,
        [
            "mem:episodic:02",
            "mem:episodic:03",
            "mem:episodic:04",
            "mem:episodic:05"
        ],
        "nan and inf pass through for the caller to judge; ' 1800000000' is kept by SQLite's CAST"
    );
    assert_eq!(
        listed(kn, &since),
        ["mem:knowledge:07", "mem:knowledge:08", "mem:knowledge:09"]
    );

    // Key prefix inside a namespace, key cap, and limit.
    let generated = MemoryFilter {
        key_prefix: Some("mem:skill:gen:".into()),
        ..MemoryFilter::default()
    };
    assert_eq!(
        listed(Namespace::Skill, &generated),
        ["mem:skill:gen:python-local"]
    );
    let capped = MemoryFilter {
        key_cap: Some(3),
        states: vec!["active".into()],
        ..MemoryFilter::default()
    };
    assert_eq!(
        listed(ep, &capped),
        ["mem:episodic:01", "mem:episodic:02"],
        "the cap takes the first keys of the namespace, then the filter applies"
    );
    let limited = MemoryFilter {
        limit: Some(2),
        ..MemoryFilter::default()
    };
    assert_eq!(listed(ep, &limited), ["mem:episodic:01", "mem:episodic:02"]);

    // With no projection every matching key is listed with no fields; with
    // one, a row that has none of the fields is left out.
    assert_eq!(
        s.list_memories(ep, &MemoryFilter::default(), &[])
            .unwrap()
            .len(),
        6
    );
    assert_eq!(
        s.list_memories(ep, &MemoryFilter::default(), &["effort_score"])
            .unwrap(),
        vec![
            (
                "mem:episodic:01".to_owned(),
                fields(&[("effort_score", "4")])
            ),
            (
                "mem:episodic:04".to_owned(),
                fields(&[("effort_score", "")])
            ),
        ]
    );
    assert!(matches!(
        s.list_memories(ep, &MemoryFilter::default(), &["bad\"name"]),
        Err(StoreError::InvalidField(_))
    ));
}

#[test]
fn delete_many_reports_what_existed_and_drops_vectors() {
    let s = store();
    for key in ["mem:episodic:a", "mem:episodic:b", "mem:knowledge:c"] {
        s.upsert(
            key,
            &fields(&[("content", key)]),
            Some(&unit([1.0, 0.0, 0.0])),
        )
        .unwrap();
    }
    s.hash_set("meta:x", &fields(&[("n", "1")])).unwrap();
    let keys: Vec<String> = [
        "mem:episodic:a",
        "mem:episodic:a",
        "mem:episodic:missing",
        "mem:knowledge:c",
        "meta:x",
        "meta:missing",
    ]
    .iter()
    .map(|k| (*k).to_owned())
    .collect();
    assert_eq!(s.delete_many(&keys).unwrap(), 3);
    assert_eq!(s.vector_count(Namespace::Episodic), 1);
    assert_eq!(s.vector_count(Namespace::Knowledge), 0);
    assert_eq!(s.scan_prefix("").unwrap(), ["mem:episodic:b"]);
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
