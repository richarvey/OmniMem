//! Phase 3 tools (experience, projects, audit, classification, knowledge,
//! briefing) on a real in-memory store with a bag-of-words embedder.

use std::sync::Arc;

use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{DomainFilter, Engine, EngineConfig, EngineError};
use omnimem_store::{Fields, Store};
use serde_json::{Map, Value, json};

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

/// A memory written straight into the store; `extra` overrides the defaults.
fn put(e: &Engine, key: &str, content: &str, extra: &[(&str, &str)]) {
    let now = omnimem_engine::pyfmt::now_str();
    let mut f: Fields = [
        ("content", content),
        ("state", "active"),
        ("surface_score", "1.0"),
        ("created_at", now.as_str()),
        ("updated_at", now.as_str()),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    f.extend(
        extra
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
    );
    e.store().upsert(key, &f, Some(&vector(content))).unwrap();
}

fn field(e: &Engine, key: &str, name: &str) -> Option<String> {
    e.store()
        .get(key)
        .unwrap()
        .and_then(|f| f.get(name).cloned())
}

fn invalid(result: Result<Value, EngineError>) -> String {
    match result {
        Err(EngineError::Invalid(message)) => message,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

fn approach(name: &str) -> Map<String, Value> {
    json!({"name": name, "type": "library", "reason": "too slow"})
        .as_object()
        .unwrap()
        .clone()
}

#[test]
fn record_experience_validates_and_auto_suppresses() {
    let e = engine();
    put(
        &e,
        "mem:episodic:01A",
        "queue work for the worker",
        &[("project", "p")],
    );

    let message = invalid(e.record_experience(
        "mem:episodic:01A",
        6,
        "succeeded",
        1,
        None,
        None,
        None,
        None,
    ));
    assert_eq!(message, "effort_score must be 1-5, got 6");
    let message =
        invalid(e.record_experience("mem:episodic:01A", 3, "won", 1, None, None, None, None));
    assert!(
        message.starts_with("outcome must be one of {'succeeded', 'pivoted', 'abandoned'}"),
        "{message}"
    );

    let result = e
        .record_experience(
            "mem:episodic:01A",
            5,
            "abandoned",
            4,
            Some(vec![approach("Celery")]),
            None,
            Some("watch the broker"),
            Some("pick boring queues"),
        )
        .unwrap();
    assert_eq!(result["auto_suppressed"], json!(["Celery"]));
    assert_eq!(result["experience_weight"], json!(0.1));
    assert_eq!(
        e.store().set_members("topics:suppressed").unwrap(),
        vec!["celery".to_owned()]
    );

    let experience = e.get_experience("mem:episodic:01A").unwrap();
    assert_eq!(experience["status"], "found");
    assert_eq!(experience["effort_score"], 5);
    assert_eq!(experience["iterations"], 4);
    assert_eq!(experience["lesson"], "pick boring queues");
    assert_eq!(experience["abandoned_approaches"][0]["name"], "Celery");

    let warning = e.warn_if_abandoned("should we try celery here").unwrap();
    assert_eq!(warning["status"], "warning");
    assert_eq!(warning["matches"][0]["memory_key"], "mem:episodic:01A");
    assert_eq!(
        e.warn_if_abandoned("postgres").unwrap(),
        json!({"status": "clear"})
    );
}

#[test]
fn log_abandoned_appends_and_rejects_unknown_types() {
    let e = engine();
    put(&e, "mem:episodic:01A", "tried a thing", &[]);
    let message = invalid(e.log_abandoned("mem:episodic:01A", "x", "framework", "no"));
    assert!(message.starts_with("type must be one of"), "{message}");
    e.log_abandoned("mem:episodic:01A", "Redis", "service", "licence")
        .unwrap();
    let result = e
        .log_abandoned("mem:episodic:01A", "Kafka", "service", "overkill")
        .unwrap();
    assert_eq!(result["abandoned_count"], 2);
    assert_eq!(result["latest_entry"]["name"], "Kafka");
    assert!(
        result["latest_entry"]["attempted_at"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert_eq!(
        e.get_experience("mem:episodic:02B").unwrap()["status"],
        "not_found"
    );
    assert_eq!(
        e.get_experience("mem:episodic:01A").unwrap()["status"],
        "no_experience"
    );
}

#[test]
fn experience_summary_dedupes_the_graveyard_and_filters_by_project() {
    let e = engine();
    put(&e, "mem:episodic:01A", "one", &[("project", "p")]);
    put(&e, "mem:episodic:01B", "two", &[("project", "p")]);
    put(&e, "mem:episodic:01C", "three", &[("project", "q")]);
    e.record_experience(
        "mem:episodic:01A",
        4,
        "succeeded",
        1,
        Some(vec![approach("Celery")]),
        Some("used RQ"),
        None,
        None,
    )
    .unwrap();
    e.record_experience(
        "mem:episodic:01B",
        2,
        "pivoted",
        1,
        Some(vec![approach("celery")]),
        None,
        None,
        None,
    )
    .unwrap();
    e.record_experience(
        "mem:episodic:01C",
        1,
        "succeeded",
        1,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let summary = e.experience_summary(Some("p")).unwrap();
    assert_eq!(summary["memories_with_experience"], 2);
    assert_eq!(summary["average_effort_score"], json!(3.0));
    assert_eq!(
        summary["outcome_breakdown"],
        json!({"succeeded": 1, "pivoted": 1, "abandoned": 0})
    );
    assert_eq!(summary["graveyard"].as_array().unwrap().len(), 1);
    assert_eq!(
        summary["top_5_most_effortful"][0]["key"],
        "mem:episodic:01A"
    );
    assert_eq!(summary["top_3_breakthroughs"][0]["breakthrough"], "used RQ");
    assert_eq!(
        e.experience_summary(None).unwrap()["memories_with_experience"],
        3
    );
}

#[test]
fn project_context_round_trips_with_domains() {
    let e = engine();
    let domains = DomainFilter::Many(vec!["py".to_owned(), "Docker".to_owned()]);
    let saved = e
        .set_project_context(
            "alpha",
            "a memory server",
            "python",
            "ship v7",
            "porting",
            None,
            Some(&domains),
        )
        .unwrap();
    assert_eq!(saved["domains"], json!(["python", "docker"]));

    let context = e.get_project_context("alpha").unwrap();
    assert_eq!(context["status"], "found");
    assert_eq!(context["goals"], "ship v7");
    assert_eq!(context["domains"], json!(["python", "docker"]));

    // Omitting domains keeps them; an empty list clears them.
    e.set_project_context(
        "alpha",
        "a memory server",
        "python",
        "ship v7",
        "still porting",
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        field(&e, "mem:project:alpha", "domains").as_deref(),
        Some("python,docker")
    );

    e.update_project_state("alpha", "phase 3", Some("tools next"))
        .unwrap();
    assert_eq!(
        field(&e, "mem:project:alpha", "current_state").as_deref(),
        Some("phase 3")
    );
    assert_eq!(
        e.update_project_state("nope", "x", None).unwrap()["status"],
        "not_found"
    );

    put(
        &e,
        "mem:episodic:01A",
        "loose memory",
        &[("project", "beta")],
    );
    let all = e.list_projects(None).unwrap();
    let names: Vec<&str> = all["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["project_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alpha"], "only mem:project: keys count as projects");
    assert_eq!(
        e.list_projects(Some("python")).unwrap()["projects"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let none = e.list_projects(Some("rust")).unwrap();
    assert!(none["note"].as_str().unwrap().contains("'rust'"));

    let cleared = DomainFilter::Many(Vec::new());
    e.set_project_context(
        "alpha",
        "a memory server",
        "python",
        "ship v7",
        "done",
        None,
        Some(&cleared),
    )
    .unwrap();
    assert_eq!(
        e.get_project_context("alpha").unwrap()["domains"],
        Value::Null
    );
}

#[test]
fn domain_migration_seeds_from_stack_once() {
    let e = engine();
    put(
        &e,
        "mem:project:alpha",
        "desc",
        &[("project_name", "alpha"), ("stack", "python, docker, and")],
    );
    put(
        &e,
        "mem:project:bare",
        "desc",
        &[("project_name", "bare"), ("goals", "something")],
    );
    assert_eq!(
        omnimem_engine::migrate_project_domains(e.store()).unwrap(),
        (1, 1)
    );
    assert_eq!(
        field(&e, "mem:project:alpha", "domains").as_deref(),
        Some("python,docker")
    );
    assert_eq!(
        field(&e, "mem:project:bare", "domains").as_deref(),
        Some("")
    );
    assert_eq!(
        omnimem_engine::migrate_project_domains(e.store()).unwrap(),
        (0, 0)
    );
}

#[test]
fn compile_project_domains_suggests_from_recurring_tags() {
    let e = engine();
    put(
        &e,
        "mem:project:beta",
        "desc",
        &[("project_name", "beta"), ("stack", "go"), ("domains", "")],
    );
    put(
        &e,
        "mem:episodic:01A",
        "one",
        &[("project", "beta"), ("tags", r#"["rust", "once"]"#)],
    );
    put(
        &e,
        "mem:episodic:01B",
        "two",
        &[("project", "beta"), ("tags", r#"["rust"]"#)],
    );

    let draft = e.compile_project_domains("beta", false).unwrap();
    assert_eq!(draft["suggested_domains"], json!(["go", "rust"]));
    assert_eq!(draft["evidence"]["rust"], json!(["tagged on 2 memories"]));
    assert_eq!(
        field(&e, "mem:project:beta", "domains").as_deref(),
        Some("")
    );

    let saved = e.compile_project_domains("beta", true).unwrap();
    assert_eq!(saved["auto_saved"], true);
    assert_eq!(
        field(&e, "mem:project:beta", "domains").as_deref(),
        Some("go,rust")
    );
    assert_eq!(
        e.compile_project_domains("nope", false).unwrap()["status"],
        "not_found"
    );
}

#[test]
fn bulk_project_tools_preview_then_apply() {
    let e = engine();
    put(
        &e,
        "mem:project:gamma",
        "desc",
        &[("project_name", "gamma"), ("goals", "g")],
    );
    put(&e, "mem:episodic:01A", "one", &[("project", "gamma")]);
    put(
        &e,
        "mem:episodic:01B",
        "two",
        &[("project", "gamma"), ("state", "archived")],
    );
    put(&e, "mem:knowledge:01C", "three", &[("project", "gamma")]);

    let preview = e.deprioritise_project("gamma", false, None, false).unwrap();
    assert_eq!(preview["status"], "preview");
    assert_eq!(preview["total"], 2);
    assert_eq!(
        field(&e, "mem:episodic:01A", "state").as_deref(),
        Some("active")
    );

    let applied = e
        .deprioritise_project("gamma", true, Some("paused"), false)
        .unwrap();
    assert_eq!(applied["total"], 2);
    assert_eq!(
        field(&e, "mem:episodic:01A", "state").as_deref(),
        Some("deprioritised")
    );
    assert_eq!(
        field(&e, "mem:episodic:01A", "deprioritised_reason").as_deref(),
        Some("paused")
    );
    assert_eq!(
        field(&e, "mem:project:gamma", "state").as_deref(),
        Some("active")
    );

    let back = e.reinstate_project("gamma", true, false).unwrap();
    assert_eq!(back["total"], 3, "the archived memory comes back too");

    let preview = e.delete_project("gamma", false, false).unwrap();
    assert_eq!(preview["total"], 3);
    let deleted = e.delete_project("gamma", true, false).unwrap();
    assert_eq!(deleted["total"], 3);
    assert!(e.store().get("mem:episodic:01A").unwrap().is_none());
    assert!(e.store().get("mem:project:gamma").unwrap().is_some());
    assert_eq!(
        e.delete_project("gamma", false, false).unwrap()["status"],
        "not_found"
    );
}

#[test]
fn compile_project_context_drafts_and_saves() {
    let e = engine();
    put(
        &e,
        "mem:episodic:01A",
        "moved the store to sqlite",
        &[
            ("project", "delta"),
            ("tags", r#"["sqlite"]"#),
            ("breakthrough", "WAL mode"),
        ],
    );
    e.log_abandoned("mem:episodic:01A", "sled", "library", "unmaintained")
        .unwrap();

    let draft = e.compile_project_context("delta", false).unwrap();
    assert_eq!(draft["memory_count"], 1);
    assert_eq!(draft["draft"]["stack"], "sqlite");
    assert!(
        draft["draft"]["notes"]
            .as_str()
            .unwrap()
            .contains("Abandoned approaches: sled (unmaintained)")
    );
    assert!(e.store().get("mem:project:delta").unwrap().is_none());

    let saved = e.compile_project_context("delta", true).unwrap();
    assert_eq!(saved["auto_saved"], true);
    assert_eq!(
        field(&e, "mem:project:delta", "provenance").as_deref(),
        Some("concluded")
    );
}

#[test]
fn memory_audit_counts_everything_and_paginates() {
    let e = engine();
    put(&e, "mem:episodic:01A", "one", &[("project", "p")]);
    put(&e, "mem:episodic:01B", "two", &[("project", "p")]);
    put(
        &e,
        "mem:episodic:01C",
        "three",
        &[("project", "p"), ("state", "archived")],
    );
    put(&e, "mem:preference:01D", "four", &[]);

    let audit = e.memory_audit(None, None, false, 1, 0).unwrap();
    assert_eq!(
        audit["summary"],
        json!({"active": 3, "deprioritised": 0, "archived": 1, "deleted": 0})
    );
    assert_eq!(audit["matching_total"], 3);
    assert_eq!(audit["returned"], 1);
    assert_eq!(audit["has_more"], true);

    let scoped = e
        .memory_audit(Some("p"), Some("episodic"), true, 100, 1)
        .unwrap();
    assert_eq!(scoped["matching_total"], 3);
    assert_eq!(scoped["returned"], 2);
    assert!(
        invalid(e.memory_audit(None, Some("skill"), false, 10, 0))
            .starts_with("Invalid namespace 'skill'")
    );
}

#[test]
fn explain_memory_reports_identity_and_classification() {
    let e = engine();
    put(&e, "mem:episodic:01A", "a fact", &[]);
    let explained = e.explain_memory("mem:episodic:01A").unwrap();
    assert_eq!(explained["status"], "found");
    assert!(
        explained["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(explained["licence"], "own");
    assert!(
        explained.get("tags").is_none(),
        "empty values are compacted away, as in 6.x"
    );
    assert_eq!(
        e.explain_memory("mem:episodic:02Z").unwrap()["status"],
        "not_found"
    );
    assert!(invalid(e.explain_memory("topics:suppressed")).starts_with("Key must start with"));
}

#[test]
fn why_did_you_mention_matches_keywords_in_recall_logs() {
    let e = engine();
    put(&e, "mem:episodic:01A", "sqlite write ahead logging", &[]);
    e.recall("sqlite logging", 5, None, None, None, None)
        .unwrap();
    let why = e.why_did_you_mention("SQLITE").unwrap();
    assert_eq!(why["status"], "found");
    assert_eq!(why["match_type"], "keyword");
    assert_eq!(why["log_query"], "sqlite logging");
}

#[test]
fn set_licence_cascades_to_facts_without_touching_updated_at() {
    let e = engine();
    put(&e, "mem:episodic:01A", "source", &[("updated_at", "100.0")]);
    put(
        &e,
        "mem:knowledge:01B",
        "fact",
        &[
            ("enriched_from", "mem:episodic:01A"),
            ("updated_at", "100.0"),
        ],
    );
    put(&e, "mem:knowledge:01C", "unrelated", &[]);

    let result = e
        .set_licence(
            "cc-by-4.0",
            Some(&["mem:episodic:01A".to_owned(), "mem:episodic:09Z".to_owned()]),
            None,
            None,
        )
        .unwrap();
    assert_eq!(result["licence"], "open");
    assert_eq!(result["cascaded_facts"], json!(["mem:knowledge:01B"]));
    assert_eq!(result["not_found"], json!(["mem:episodic:09Z"]));
    assert_eq!(
        field(&e, "mem:knowledge:01B", "licence").as_deref(),
        Some("open")
    );
    assert_eq!(
        field(&e, "mem:knowledge:01B", "updated_at").as_deref(),
        Some("100.0")
    );
    assert_eq!(field(&e, "mem:knowledge:01C", "licence"), None);

    let both = e
        .set_licence(
            "own",
            Some(&["mem:episodic:01A".to_owned()]),
            Some("feed"),
            None,
        )
        .unwrap();
    assert!(
        both["error"]
            .as_str()
            .unwrap()
            .starts_with("Give either keys or feed_name")
    );
}

#[test]
fn set_licence_by_feed_and_set_provenance_refuse_skills() {
    let e = engine();
    put(&e, "mem:knowledge:01A", "article", &[("feed_name", "news")]);
    let result = e
        .set_licence("restricted", None, Some("news"), Some("paywalled"))
        .unwrap();
    assert_eq!(result["classified"], 1);
    assert_eq!(
        field(&e, "mem:knowledge:01A", "licence_note").as_deref(),
        Some("paywalled")
    );

    let refused = e
        .set_provenance("asserted", &["mem:skill:gen:python-local".to_owned()])
        .unwrap();
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .starts_with("Compiled skills carry no provenance")
    );
    let ok = e
        .set_provenance("asserted", &["mem:knowledge:01A".to_owned()])
        .unwrap();
    assert_eq!(ok["classified"], 1);
    assert_eq!(
        field(&e, "mem:knowledge:01A", "provenance").as_deref(),
        Some("asserted")
    );
}

#[test]
fn recent_knowledge_filters_by_feed_topic_and_licence() {
    let e = engine();
    put(
        &e,
        "mem:knowledge:01A",
        "rust news",
        &[
            ("feed_name", "rss"),
            ("topics", r#"["rust"]"#),
            ("licence", "open"),
        ],
    );
    put(
        &e,
        "mem:knowledge:01B",
        "old news",
        &[("feed_name", "rss"), ("created_at", "1000.0")],
    );

    let recent = e.recent_knowledge(7, None, None, 20, None).unwrap();
    assert_eq!(recent.as_array().unwrap().len(), 1);
    assert_eq!(recent[0]["topics"], json!(["rust"]));
    assert_eq!(
        e.recent_knowledge(7, None, None, 20, Some("unknown"))
            .unwrap(),
        json!([])
    );
    assert_eq!(
        e.recent_knowledge(7, None, Some(&["go".to_owned()]), 20, None)
            .unwrap(),
        json!([])
    );
    assert!(
        invalid(e.recent_knowledge(7, None, None, 20, Some("free")))
            .starts_with("Invalid licence class 'free'")
    );
}

#[test]
fn reindex_reloads_vectors_and_keeps_the_report_shape() {
    let e = engine();
    put(&e, "mem:episodic:01A", "one", &[]);
    let report = e.reindex(None).unwrap();
    assert_eq!(report["status"], "ok");
    assert_eq!(report["reindexed"].as_array().unwrap().len(), 5);
    assert_eq!(
        report["reindexed"][0],
        json!({
            "namespace": "episodic", "before_num_docs": 1, "after_num_docs": 1,
            "actual_records": 1, "removed_phantoms": 0,
        })
    );
    assert!(invalid(e.reindex(Some("logs"))).starts_with("Invalid namespace 'logs'"));
}

#[test]
fn briefing_lists_stale_memories_and_runs_maintenance_on_its_interval() {
    let e = engine_with(EngineConfig {
        auto_maintenance_interval: 2,
        ..EngineConfig::default()
    });
    put(&e, "mem:project:p", "the project", &[("project_name", "p")]);
    put(
        &e,
        "mem:episodic:01A",
        "stale note",
        &[("project", "p"), ("updated_at", "1000.0")],
    );
    put(
        &e,
        "mem:episodic:01B",
        "compiled note",
        &[("project", "p"), ("updated_at", "1000.0")],
    );
    put(
        &e,
        "mem:skill:gen:notes-local",
        "skill",
        &[("source_manifest", r#"["mem:episodic:01B"]"#)],
    );
    put(
        &e,
        "mem:episodic:01C",
        "duplicate words here",
        &[("project", "p"), ("created_at", "10.0")],
    );
    put(
        &e,
        "mem:episodic:01D",
        "duplicate words here",
        &[("project", "p"), ("created_at", "20.0")],
    );

    let first = e.briefing(Some("p"), true).unwrap();
    assert_eq!(first["project_context"]["name"], "p");
    let stale: Vec<&str> = first["stale_memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["key"].as_str().unwrap())
        .collect();
    assert_eq!(stale, ["mem:episodic:01A"], "skill sources are exempt");
    assert!(first.get("auto_maintenance").is_none());

    let second = e.briefing(Some("p"), true).unwrap();
    assert_eq!(
        second["auto_maintenance"]["duplicates_archived"],
        json!(["mem:episodic:01C"])
    );
    assert_eq!(
        field(&e, "mem:episodic:01C", "state").as_deref(),
        Some("archived")
    );
    let meta = e
        .store()
        .hash_get_all("meta:maintenance:p")
        .unwrap()
        .unwrap();
    assert_eq!(meta.get("briefing_count").map(String::as_str), Some("0"));
}

#[test]
fn check_contradictions_links_both_sides_once() {
    let e = engine();
    put(
        &e,
        "mem:episodic:01A",
        "always use sqlite for local storage",
        &[("project", "p")],
    );
    put(
        &e,
        "mem:episodic:01B",
        "never use sqlite for local storage",
        &[("project", "p")],
    );
    put(
        &e,
        "mem:episodic:01C",
        "the deploy pipeline runs nightly",
        &[("project", "p")],
    );

    let found = e
        .check_contradictions(None, "episodic", Some("p"), false)
        .unwrap();
    let pairs = found["contradictions"].as_array().unwrap();
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0]["method"], "heuristic");
    let links: Value =
        serde_json::from_str(&field(&e, "mem:episodic:01A", "contradictions").unwrap()).unwrap();
    assert_eq!(links[0]["key"], "mem:episodic:01B");

    e.check_contradictions(Some("sqlite storage"), "episodic", None, false)
        .unwrap();
    let links: Value =
        serde_json::from_str(&field(&e, "mem:episodic:01B", "contradictions").unwrap()).unwrap();
    assert_eq!(links.as_array().unwrap().len(), 1, "a pair is linked once");

    assert_eq!(
        e.check_contradictions(None, "episodic", Some("q"), false)
            .unwrap(),
        json!({"contradictions": []})
    );
    assert!(
        invalid(e.check_contradictions(None, "logs", None, false)).starts_with("Invalid namespace")
    );
}
