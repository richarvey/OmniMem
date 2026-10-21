//! Core tool behaviours on a real store (SQLite in memory) with a
//! deterministic bag-of-words embedder, so similarities are predictable:
//! identical text scores 1.0, and overlap scores by shared words.

use std::sync::Arc;

use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{DomainFilter, Engine, EngineConfig, EngineError};
use omnimem_store::{Fields, Store};
use serde_json::{Value, json};

struct Words;

fn vector(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; VECTOR_DIM];
    for word in text.split_whitespace() {
        let word: String = word
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        if word.is_empty() {
            continue;
        }
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in word.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        v[(h % VECTOR_DIM as u64) as usize] += 1.0;
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in &mut v {
            *x /= n;
        }
    } else {
        v[0] = 1.0;
    }
    v
}

impl TextEmbedder for Words {
    fn dimension(&self) -> usize {
        VECTOR_DIM
    }
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|t| vector(t)).collect())
    }
}

fn engine() -> Engine {
    engine_with(EngineConfig::default())
}

fn engine_with(config: EngineConfig) -> Engine {
    Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Words),
        config,
    )
}

fn fields(pairs: &[(&str, &str)]) -> Fields {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// A memory written straight into the store, as 6.x data would arrive.
fn put(e: &Engine, key: &str, content: &str, extra: &[(&str, &str)]) {
    let now = omnimem_engine::pyfmt::now_str();
    let mut f = fields(&[
        ("content", content),
        ("state", "active"),
        ("surface_score", "1.0"),
        ("experience_weight", "1.0"),
        ("created_at", &now),
        ("updated_at", &now),
    ]);
    f.extend(fields(extra));
    e.store().upsert(key, &f, Some(&vector(content))).unwrap();
}

fn keys(results: &Value) -> Vec<String> {
    results
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r.get("key").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

// -- remember ----------------------------------------------------------------

#[test]
fn remember_stores_classifies_and_queues_enrichment() {
    let e = engine();
    let tags = vec!["docker".to_owned(), "arm64".to_owned()];
    let out = e
        .remember(
            "tonistiigi binfmt fixed the arm64 build",
            Some("omnimem"),
            Some(&tags),
            "episodic",
            false,
            None,
            None,
            None,
        )
        .unwrap();
    let key = out["key"].as_str().unwrap();
    assert!(key.starts_with("mem:episodic:"));
    assert_eq!(out["licence"], "own");
    assert_eq!(out["provenance"], "concluded");
    assert_eq!(out["enrichment"], "queued");
    assert_eq!(
        out.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["key", "namespace", "licence", "provenance", "enrichment"]
    );
    let stored = e.store().get(key).unwrap().unwrap();
    assert_eq!(
        stored["tags"], r#"["docker", "arm64"]"#,
        "json.dumps spacing"
    );
    assert_eq!(stored["project"], "omnimem");
    assert_eq!(e.queue_status().unwrap(), json!({"pending": 1}));
}

#[test]
fn remember_finds_duplicates_unless_forced() {
    let e = engine();
    e.remember(
        "use sqlite for the store",
        None,
        None,
        "episodic",
        false,
        Some("raw"),
        None,
        None,
    )
    .unwrap();
    let dup = e
        .remember(
            "use sqlite for the store",
            None,
            None,
            "episodic",
            false,
            Some("raw"),
            None,
            None,
        )
        .unwrap();
    assert_eq!(dup["status"], "duplicate_found");
    assert_eq!(dup["similarity"], json!(1.0));
    let forced = e
        .remember(
            "use sqlite for the store",
            None,
            None,
            "episodic",
            true,
            Some("raw"),
            None,
            None,
        )
        .unwrap();
    assert!(forced.get("key").is_some());
    assert!(
        forced.get("enrichment").is_none(),
        "force is a raw bypass write"
    );
}

#[test]
fn remember_warns_on_contradiction() {
    let e = engine();
    e.remember(
        "always use alpine images",
        None,
        None,
        "episodic",
        false,
        Some("raw"),
        None,
        None,
    )
    .unwrap();
    let out = e
        .remember(
            "never use alpine images",
            None,
            None,
            "episodic",
            false,
            Some("raw"),
            None,
            None,
        )
        .unwrap();
    let warning = &out["contradiction_warning"];
    assert_eq!(warning["existing_content"], "always use alpine images");
    assert_eq!(warning["similarity"], json!(0.75));
}

#[test]
fn remember_validates_like_6x() {
    let e = engine();
    let err = |r: Result<Value, EngineError>| match r {
        Err(EngineError::Invalid(m)) => m,
        other => panic!("expected a validation error, got {other:?}"),
    };
    assert_eq!(
        err(e.remember("  ", None, None, "episodic", false, None, None, None)),
        "Content cannot be empty"
    );
    assert_eq!(
        err(e.remember("x", None, None, "skill", false, None, None, None)),
        "Invalid namespace 'skill'. Must be one of: episodic, knowledge, preference, project"
    );
    assert!(
        err(e.remember(
            "x",
            Some("bad/name"),
            None,
            "episodic",
            false,
            None,
            None,
            None
        ))
        .contains("invalid characters")
    );
    assert!(
        err(e.remember("x", None, None, "episodic", false, Some("fast"), None, None))
            .starts_with("Invalid mode")
    );
    assert!(
        err(e.remember(
            "x",
            None,
            None,
            "episodic",
            false,
            None,
            Some("gpl-banana"),
            None
        ))
        .starts_with("Unrecognised licence")
    );
}

#[test]
fn knowledge_defaults_to_unknown_licence_and_no_enrichment() {
    let e = engine();
    let out = e
        .remember(
            "an article about rust",
            None,
            None,
            "knowledge",
            false,
            None,
            Some("cc-by-4.0"),
            None,
        )
        .unwrap();
    assert_eq!(out["licence"], "open");
    assert_eq!(out["provenance"], "retrieved");
    assert!(out.get("enrichment").is_none());
    let stored = e
        .store()
        .get(out["key"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(stored["licence_note"], "CC BY 4.0");
}

// -- recall ------------------------------------------------------------------

#[test]
fn recall_ranks_marks_and_floors() {
    let e = engine();
    put(
        &e,
        "mem:episodic:exact",
        "valkey search tag filter quirks",
        &[],
    );
    put(
        &e,
        "mem:episodic:partial",
        "valkey tag filters need raw values and clause level alternation everywhere",
        &[],
    );
    put(&e, "mem:episodic:unrelated", "zebra", &[]);
    // The fixture's own premise: the toy embedder hashes words into slots, so
    // check "unrelated" really shares none with the query.
    let premise: f32 = vector("valkey search tag filter quirks")
        .iter()
        .zip(vector("zebra"))
        .map(|(a, b)| a * b)
        .sum();
    assert!(
        premise < 0.15,
        "fixture words collide in the toy embedder ({premise})"
    );
    let out = e
        .recall("valkey search tag filter quirks", 5, None, None, None, None)
        .unwrap();
    let got = keys(&out);
    assert_eq!(got[0], "mem:episodic:exact");
    assert!(got.contains(&"mem:episodic:partial".to_owned()));
    assert!(
        !got.contains(&"mem:episodic:unrelated".to_owned()),
        "below the floor"
    );
    let partial = out
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "mem:episodic:partial")
        .unwrap();
    assert_eq!(partial["weak_match"], true);
    let exact = &out[0];
    assert!(exact.get("weak_match").is_none());
    assert_eq!(exact["licence"], "own");
    assert_eq!(exact["provenance"], "concluded");
    let stored = e.store().get("mem:episodic:exact").unwrap().unwrap();
    assert_eq!(stored["recall_count"], "1");
    assert_eq!(e.store().scan_prefix("log:recall:").unwrap().len(), 1);
}

#[test]
fn abandoned_warnings_come_first_and_skip_the_floor() {
    let e = engine();
    put(
        &e,
        "mem:episodic:alpine",
        "tried a small base image",
        &[(
            "abandoned_approaches",
            r#"[{"name": "Alpine", "type": "approach", "reason": "no musl wheels"}]"#,
        )],
    );
    let out = e
        .recall("should we use alpine", 5, None, None, None, None)
        .unwrap();
    assert_eq!(out[0]["result_type"], "abandoned_warning");
    // No breakthrough and no lesson, and it was written moments ago, so no
    // age clause either. created_at is always present, so the age is omitted
    // by being under a day rather than by being absent. The warning must read
    // exactly as it always did.
    assert_eq!(
        out[0]["content"],
        "Abandoned approach: Alpine — no musl wheels"
    );
}

#[test]
fn an_abandoned_warning_carries_the_way_out() {
    let e = engine();
    put(
        &e,
        "mem:episodic:kestrel",
        "tried a process-wide limiter",
        &[
            (
                "abandoned_approaches",
                r#"[{"name": "kestrel-rs", "type": "library", "reason": "global governor."}]"#,
            ),
            ("breakthrough", "pellham: one limiter per partition"),
            ("lesson", "limiters assuming one runtime do not fit shards"),
        ],
    );
    let out = e.recall("kestrel-rs", 5, None, None, None, None).unwrap();
    assert_eq!(out[0]["result_type"], "abandoned_warning");
    let content = out[0]["content"].as_str().unwrap();
    // A warning that says only "not that" leaves the agent to re-derive the
    // answer, which is the work the graveyard exists to save.
    assert!(
        content.contains("What worked instead: pellham"),
        "{content}"
    );
    assert!(content.contains("Lesson: limiters assuming"), "{content}");
    // The reason carries its own full stop; joining clauses must not double it.
    assert!(!content.contains(".."), "{content}");
}

#[test]
fn a_fact_and_its_source_collapse_to_the_source() {
    let e = engine();
    put(
        &e,
        "mem:episodic:source",
        "alpha beta gamma delta epsilon zeta",
        &[],
    );
    put(
        &e,
        "mem:knowledge:fact",
        "alpha beta",
        &[("enriched_from", "mem:episodic:source")],
    );
    let out = e.recall("alpha beta", 5, None, None, None, None).unwrap();
    assert_eq!(keys(&out), ["mem:episodic:source"]);
    let score = out[0]["score"].as_f64().unwrap();
    assert!(
        (score - 1.0).abs() < 1e-6,
        "promoted to the fact's score, got {score}"
    );
}

#[test]
fn deprioritised_memories_with_matching_hints_are_reinstate_candidates() {
    let e = engine();
    put(&e, "mem:episodic:old", "file watching with inotify", &[]);
    e.deprioritise(
        "mem:episodic:old",
        "docker mounts",
        Some(&["file watching".to_owned()]),
    )
    .unwrap();
    let out = e
        .recall("file watching again", 5, None, None, None, None)
        .unwrap();
    let hit = out
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "mem:episodic:old")
        .unwrap();
    assert_eq!(hit["reinstate_candidate"], true);
    assert_eq!(hit["score"], json!(0.6));
    assert_eq!(hit["deprioritised_reason"], "docker mounts");
}

#[test]
fn project_filter_scopes_and_unknown_licences_are_noticed() {
    let e = engine();
    put(
        &e,
        "mem:knowledge:ours",
        "sqlite wal mode notes",
        &[("project", "omnimem"), ("feed_name", "Feed")],
    );
    put(
        &e,
        "mem:knowledge:theirs",
        "sqlite wal mode notes too",
        &[("project", "other")],
    );
    let out = e
        .recall(
            "sqlite wal mode notes",
            5,
            None,
            Some("omnimem"),
            None,
            None,
        )
        .unwrap();
    assert_eq!(keys(&out)[0], "mem:knowledge:ours");
    assert!(!keys(&out).contains(&"mem:knowledge:theirs".to_owned()));
    let last = out.as_array().unwrap().last().unwrap();
    assert_eq!(last["result_type"], "licence_notice");
    assert_eq!(last["unclassified"], json!(["mem:knowledge:ours"]));
}

#[test]
fn domain_filters_route_report_and_intersect() {
    let e = engine();
    put(
        &e,
        "mem:project:omnimem",
        "semantic memory server",
        &[("project_name", "omnimem"), ("domains", "python,docker")],
    );
    put(
        &e,
        "mem:episodic:py",
        "python packaging with uv",
        &[("project", "omnimem")],
    );
    put(
        &e,
        "mem:episodic:elsewhere",
        "python packaging with uv tips",
        &[("project", "other")],
    );

    let scoped = e
        .recall(
            "python packaging with uv",
            5,
            None,
            None,
            None,
            Some(&DomainFilter::One("py".into())),
        )
        .unwrap();
    assert!(keys(&scoped).contains(&"mem:episodic:py".to_owned()));
    assert!(!keys(&scoped).contains(&"mem:episodic:elsewhere".to_owned()));

    let unmatched = e
        .recall(
            "python packaging with uv",
            5,
            None,
            None,
            None,
            Some(&DomainFilter::Many(vec!["rust".into()])),
        )
        .unwrap();
    assert_eq!(unmatched[0]["result_type"], "domain_filter_notice");
    assert_eq!(unmatched[0]["applied"], false);
    assert!(
        keys(&unmatched).contains(&"mem:episodic:elsewhere".to_owned()),
        "degrades to unscoped"
    );

    let empty = e
        .recall(
            "python",
            5,
            None,
            Some("other"),
            None,
            Some(&DomainFilter::One("python".into())),
        )
        .unwrap();
    assert_eq!(
        empty.as_array().unwrap().len(),
        1,
        "an empty intersection returns only the notice"
    );
    assert_eq!(empty[0]["projects"], json!([]));
}

#[test]
fn recall_index_and_detail() {
    let e = engine();
    let long = "word ".repeat(100);
    put(
        &e,
        "mem:episodic:long",
        long.trim(),
        &[
            ("tags", r#"["a"]"#),
            ("breakthrough", "it worked"),
            ("effort_score", "4"),
        ],
    );
    let index = e
        .recall_index("word word word", 10, None, None, 60, None, None)
        .unwrap();
    let first = &index["results"][0];
    assert_eq!(
        first["snippet"].as_str().unwrap().chars().count(),
        63,
        "60 chars plus ..."
    );
    assert_eq!(first["estimated_tokens"], json!(499 / 4));
    assert_eq!(index["token_estimate"]["full"], json!(499 / 4));

    let detail = e
        .recall_detail(&[
            "mem:episodic:long".into(),
            "mem:episodic:nope".into(),
            "bogus".into(),
        ])
        .unwrap();
    assert_eq!(detail.as_array().unwrap().len(), 2);
    assert_eq!(detail[0]["tags"], json!(["a"]));
    assert_eq!(detail[0]["breakthrough"], "it worked");
    assert_eq!(detail[0]["effort_score"], json!(4));
    assert_eq!(
        detail[1],
        json!({"key": "mem:episodic:nope", "status": "not_found"})
    );
}

// -- lifecycle and suppression -------------------------------------------------

#[test]
fn lifecycle_transitions_by_key() {
    let e = engine();
    put(
        &e,
        "mem:episodic:m",
        "hard won lesson",
        &[("effort_score", "5")],
    );
    let out = e.deprioritise("mem:episodic:m", "outdated", None).unwrap();
    let t = &out["affected"][0];
    assert_eq!(t["previous_state"], "active");
    assert_eq!(t["surface_score"], json!(0.2));
    assert!(t["warning"].as_str().unwrap().contains("5/5"));
    assert_eq!(
        e.store().get("mem:episodic:m").unwrap().unwrap()["surface_score"],
        "0.2"
    );

    e.archive("mem:episodic:m", None).unwrap();
    let err = e.deprioritise("mem:episodic:m", "again", None).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Invalid transition: archived -> deprioritised. Allowed: ['active', 'deleted']"
    );
    let back = e.reinstate("mem:episodic:m").unwrap();
    assert_eq!(back["affected"][0]["new_state"], "active");
    let stored = e.store().get("mem:episodic:m").unwrap().unwrap();
    assert_eq!(stored["deprioritised_reason"], "");
    assert_eq!(stored["surface_score"], "1.0");
}

#[test]
fn forget_previews_then_deletes() {
    let e = engine();
    put(&e, "mem:episodic:gone", "delete me please", &[]);
    assert_eq!(
        e.forget("mem:episodic:gone", false).unwrap()["status"],
        "preview"
    );
    assert!(e.store().get("mem:episodic:gone").unwrap().is_some());
    assert_eq!(
        e.forget("mem:episodic:gone", true).unwrap()["deleted_keys"],
        json!(["mem:episodic:gone"])
    );
    assert!(e.store().get("mem:episodic:gone").unwrap().is_none());
    assert_eq!(
        e.forget("mem:episodic:gone", true).unwrap(),
        json!({"status": "not_found"})
    );
}

#[test]
fn suppressed_topics_drop_out_of_recall() {
    let e = engine();
    put(&e, "mem:episodic:s", "kubernetes operators everywhere", &[]);
    assert_eq!(
        e.suppress_topic("Kubernetes", None).unwrap(),
        json!({"topic": "Kubernetes"})
    );
    assert!(
        keys(
            &e.recall("kubernetes operators everywhere", 5, None, None, None, None)
                .unwrap()
        )
        .is_empty()
    );
    assert_eq!(
        e.list_suppressions().unwrap(),
        json!({"suppressed_topics": ["kubernetes"]})
    );
    e.unsuppress_topic("KUBERNETES").unwrap();
    assert_eq!(
        keys(
            &e.recall("kubernetes operators everywhere", 5, None, None, None, None)
                .unwrap()
        )
        .len(),
        1
    );
}

#[test]
fn retag_replaces_adds_and_removes() {
    let e = engine();
    put(&e, "mem:episodic:t", "tagged", &[("tags", r#"["a", "b"]"#)]);
    let out = e
        .retag(
            "mem:episodic:t",
            None,
            Some(vec!["c".into(), "a".into()]),
            Some(vec!["b".into()]),
        )
        .unwrap();
    assert_eq!(out["tags"], json!(["a", "c"]));
    assert_eq!(out["previous_tags"], json!(["a", "b"]));
    assert_eq!(
        e.retag(
            "mem:episodic:t",
            Some(vec!["a".into(), "c".into()]),
            None,
            None
        )
        .unwrap()["status"],
        "unchanged"
    );
    assert!(
        e.retag("mem:skill:gen:x-local", Some(vec![]), None, None)
            .is_err()
    );
}

#[test]
fn duplicates_cluster() {
    let e = engine();
    put(&e, "mem:episodic:1", "the same sentence", &[]);
    put(&e, "mem:episodic:2", "the same sentence", &[]);
    put(&e, "mem:episodic:3", "something else entirely", &[]);
    let out = e.find_duplicates("episodic", None, None).unwrap();
    let clusters = out["clusters"].as_array().unwrap();
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0]["memories"].as_array().unwrap().len(), 2);
    assert!((clusters[0]["max_similarity"].as_f64().unwrap() - 1.0).abs() < 1e-6);
}

// -- documents and backups ---------------------------------------------------------

#[test]
fn documents_chunk_and_skip_duplicate_chunks() {
    let e = engine();
    let doc = "First paragraph about sqlite.\n\nSecond paragraph about vectors.\n\nFirst paragraph about sqlite.";
    let out = e
        .remember_document(
            doc,
            "paragraphs",
            Some("omnimem"),
            None,
            "episodic",
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(out["chunks_total"], json!(3));
    assert_eq!(out["chunks_stored"], json!(2));
    assert_eq!(out["duplicates_skipped"], json!(1));
    assert_eq!(out["enrichment"], "queued");
    let first = e
        .store()
        .get(out["keys"][0].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(first["chunk_index"], "0");
    assert_eq!(first["doc_id"], out["doc_id"].as_str().unwrap());
}

#[test]
fn backups_dump_list_and_restore() {
    let dir = std::env::temp_dir().join(format!("omnimem-engine-backups-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let config = EngineConfig {
        backup_dir: dir.clone(),
        ..EngineConfig::default()
    };
    let source = engine_with(config.clone());
    put(&source, "mem:episodic:b", "backed up memory", &[]);
    let dumped = source.dump_to_file(Some("test_backup.json")).unwrap();
    assert_eq!(dumped["filename"], "test_backup.json");
    assert_eq!(
        source.list_backups().unwrap()["backups"][0]["filename"],
        "test_backup.json"
    );
    assert_eq!(
        source.dump_to_file(Some("../escape.json")).unwrap()["status"],
        "error"
    );

    let target = engine_with(config);
    let dry = target.restore_from_file("test_backup.json", true).unwrap();
    assert_eq!(dry["status"], "dry_run");
    assert!(target.store().get("mem:episodic:b").unwrap().is_none());
    let restored = target.restore_from_file("test_backup.json", false).unwrap();
    assert_eq!(restored["status"], "restored");
    assert_eq!(restored["re_embedded"], json!(1));
    assert_eq!(
        keys(
            &target
                .recall("backed up memory", 5, None, None, None, None)
                .unwrap()
        ),
        ["mem:episodic:b"]
    );
    assert_eq!(
        target.restore_from_file("missing.json", false).unwrap()["message"],
        "Backup file not found"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn health_and_version() {
    let e = engine();
    put(&e, "mem:preference:p", "british spelling", &[]);
    let h = e.health().unwrap();
    assert_eq!(h["records"]["preference"], json!(1));
    assert_eq!(h["vectors"]["preference"], json!(1));
    assert!(e.version()["version"].as_str().unwrap().starts_with('7'));
}
