//! Phase 4: the skill compiler's flows on a real in-memory store with a
//! bag-of-words embedder (identical text scores 1.0).

use std::io::{Cursor, Read, Write};
use std::sync::Arc;

use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::transfer::{merge_feed_influences, validate_skill_import};
use omnimem_engine::{Engine, EngineConfig, EngineError};
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

fn config() -> EngineConfig {
    EngineConfig {
        skill_scan_interval_hours: 0.0,
        ..EngineConfig::default()
    }
}

fn engine_with(config: EngineConfig) -> Engine {
    Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Words),
        config,
    )
}

fn engine() -> Engine {
    engine_with(config())
}

fn put(e: &Engine, key: &str, content: &str, extra: &[(&str, &str)]) {
    let now = omnimem_engine::pyfmt::now_str();
    let mut f: Fields = [
        ("content", content),
        ("state", "active"),
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

/// A succeeded experience memory carrying a lesson, tagged with a domain.
fn lesson(e: &Engine, key: &str, domain: &str, project: &str, text: &str) {
    let tags = format!(r#"["{domain}"]"#);
    put(
        e,
        key,
        &format!("work on {project}"),
        &[
            ("tags", &tags),
            ("project", project),
            ("outcome", "succeeded"),
            ("effort_score", "3"),
            ("lesson", text),
        ],
    );
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

fn propose(e: &Engine, domain: &str) -> Value {
    e.compile_skill(domain, "propose", 2, true, None, None)
        .unwrap()
}

fn write(e: &Engine, domain: &str) -> Value {
    e.compile_skill(domain, "write", 2, true, None, None)
        .unwrap()
}

const QUEUES: &str = "always use boring queues for python background work";

#[test]
fn propose_write_then_unchanged() {
    let dir = std::env::temp_dir().join(format!("omnimem-skill-export-{}", std::process::id()));
    let e = engine_with(EngineConfig {
        skill_export_dir: dir.clone(),
        ..config()
    });
    lesson(&e, "mem:episodic:01A", "python", "alpha", QUEUES);
    lesson(&e, "mem:episodic:01B", "python", "beta", QUEUES);

    let proposal = propose(&e, "py");
    assert_eq!(proposal["status"], "proposal");
    assert_eq!(proposal["skill_id"], "mem:skill:gen:python-local");
    assert_eq!(proposal["domain_resolved_from"], "py");
    assert_eq!(proposal["new_skill"], true);
    assert_eq!(proposal["rules"], json!({"do": 1}));
    let draft = proposal["draft"].as_str().unwrap();
    assert!(draft.contains("## Do\n\n- always use boring queues for python background work. (reinforced x2) [mem:episodic:01B]"), "{draft}");
    assert!(draft.contains("  - mem:episodic:01A   # reinforced x2"));
    assert_eq!(proposal["changes"][0]["change"], "added");

    let written = e
        .compile_skill(
            "python",
            "write",
            2,
            true,
            Some("python-local/SKILL.md"),
            None,
        )
        .unwrap();
    assert_eq!(written["status"], "written");
    let exported = std::fs::read_to_string(written["exported_to"].as_str().unwrap()).unwrap();
    assert_eq!(
        exported,
        field(&e, "mem:skill:gen:python-local", "body").unwrap()
    );
    assert_eq!(
        field(&e, "mem:skill:gen:python-local", "source_manifest").as_deref(),
        Some(r#"["mem:episodic:01A", "mem:episodic:01B"]"#)
    );
    assert_eq!(
        field(&e, "mem:skill:gen:python-local", "tags").as_deref(),
        Some(r#"["python"]"#)
    );
    assert!(
        e.store()
            .hash_get_all("meta:skill:proposal:python-local")
            .unwrap()
            .is_none()
    );
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(propose(&e, "python")["status"], "unchanged");
    assert_eq!(write(&e, "python")["status"], "no_proposal");

    let escape = e
        .compile_skill("python", "write", 2, true, Some("../x.md"), None)
        .unwrap();
    assert_eq!(escape["status"], "no_proposal");
}

#[test]
fn recompiles_show_a_diff_and_refuse_stale_proposals() {
    let e = engine();
    lesson(&e, "mem:episodic:01A", "python", "alpha", QUEUES);
    lesson(&e, "mem:episodic:01B", "python", "beta", QUEUES);
    propose(&e, "python");
    write(&e, "python");

    lesson(&e, "mem:episodic:01C", "python", "gamma", QUEUES);
    let recompile = propose(&e, "python");
    assert_eq!(recompile["status"], "proposal");
    assert_eq!(recompile["new_skill"], false);
    assert_eq!(recompile["description_pinned"], true);
    assert_eq!(
        recompile["changes"],
        json!([{
            "change": "reinforced", "risk": "low", "rule_kind": "do",
            "rule": "always use boring queues for python background work",
        }])
    );
    let diff = recompile["diff"].as_str().unwrap();
    assert!(
        diff.starts_with(
            "--- mem:skill:gen:python-local (stored)\n+++ mem:skill:gen:python-local (proposed)\n"
        ),
        "{diff}"
    );
    assert!(diff.contains(
        "+- always use boring queues for python background work. (reinforced x3) [mem:episodic:01C]"
    ));

    e.store()
        .set_field("mem:skill:gen:python-local", "body", "edited underneath")
        .unwrap();
    assert_eq!(write(&e, "python")["status"], "stale_proposal");
}

#[test]
fn the_compiler_never_overwrites_authored_work() {
    let e = engine();
    lesson(&e, "mem:episodic:01A", "python", "alpha", QUEUES);
    put(
        &e,
        "mem:skill:gen:python-local",
        "authored",
        &[("generated", "false"), ("domain", "python")],
    );
    assert_eq!(propose(&e, "python")["status"], "refused");
}

#[test]
fn empty_and_weak_pools_explain_themselves() {
    let e = engine();
    assert!(
        invalid(e.compile_skill("python", "commit", 2, true, None, None))
            .starts_with("mode must be")
    );
    assert!(
        invalid(e.compile_skill("!!", "propose", 2, true, None, None))
            .starts_with("Invalid domain.")
    );

    lesson(
        &e,
        "mem:episodic:01A",
        "python",
        "alpha",
        "use uv for environments",
    );
    lesson(
        &e,
        "mem:episodic:01B",
        "python",
        "alpha",
        "type hints on public functions",
    );
    put(
        &e,
        "mem:episodic:01C",
        "no lessons here",
        &[("tags", r#"["rust"]"#)],
    );

    let none = propose(&e, "pyth");
    assert_eq!(none["status"], "no_candidates");
    assert_eq!(
        none["did_you_mean"],
        json!({"domain": "python", "similarity": 0.9})
    );
    assert_eq!(
        none["known_domains"][0],
        json!({"domain": "python", "memories": 2})
    );

    assert_eq!(propose(&e, "rust")["status"], "no_lessons");

    let weak = propose(&e, "python");
    assert_eq!(weak["status"], "insufficient_reinforcement");
    assert_eq!(
        weak["pool_concentration"],
        json!({"projects": 1, "top_project": "alpha", "top_project_share": "2/2"})
    );
    assert_eq!(weak["held_back"].as_array().unwrap().len(), 2);
    assert!(
        weak["note"]
            .as_str()
            .unwrap()
            .contains("Every candidate comes from one project (alpha)")
    );

    let blessed = e.bless("mem:episodic:01A").unwrap();
    assert_eq!(
        blessed,
        json!({
            "status": "blessed", "key": "mem:episodic:01A", "domains": ["python"],
            "note": "Its lessons now clear the reinforcement gate on the next compile_skill() for its tagged domains. Compiling still requires the propose-and-accept flow.",
        })
    );
    assert_eq!(
        e.bless("mem:episodic:01A").unwrap()["status"],
        "already_blessed"
    );
    assert_eq!(e.bless("mem:episodic:09Z").unwrap()["status"], "not_found");
    assert!(
        invalid(e.bless("mem:knowledge:01A")).starts_with("bless() takes an episodic memory key")
    );

    let now = propose(&e, "python");
    assert_eq!(now["status"], "proposal");
    assert!(
        now["draft"]
            .as_str()
            .unwrap()
            .contains("- use uv for environments. (blessed) [mem:episodic:01A]")
    );
    assert_eq!(now["held_back"].as_array().unwrap().len(), 1);
}

#[test]
fn promoted_knowledge_and_influenced_feeds_reach_the_skill() {
    let e = engine();
    put(
        &e,
        "mem:knowledge:01A",
        "Pin every dependency",
        &[
            ("title", "Pinning"),
            ("source_url", "https://example.com/pin"),
            ("expires_at", "99"),
        ],
    );

    assert_eq!(
        e.promote_knowledge("mem:episodic:01A", Some("python"), false, None)
            .unwrap()["error"],
        "Key must be in the knowledge namespace: mem:episodic:01A"
    );
    assert_eq!(
        e.promote_knowledge("mem:knowledge:01A", None, true, None)
            .unwrap()["error"],
        "demote requires a domain to remove"
    );
    assert_eq!(
        e.promote_knowledge("mem:knowledge:01A", None, false, Some(&json!([])))
            .unwrap()["error"],
        "rules only apply when promoting to a domain"
    );
    assert_eq!(
        e.promote_knowledge("mem:knowledge:01A", Some("python"), true, None)
            .unwrap()["error"],
        "mem:knowledge:01A is not promoted to domain 'python'"
    );
    assert_eq!(
        e.promote_knowledge(
            "mem:knowledge:01A",
            Some("python"),
            false,
            Some(&json!([{"kind": "maybe", "text": "x"}]))
        )
        .unwrap()["error"],
        "rules kind must be one of do/watch/dont/note, got 'maybe'"
    );

    let promoted = e
        .promote_knowledge(
            "mem:knowledge:01A",
            Some("py"),
            false,
            Some(&json!([{"kind": "dont", "text": "float versions"}])),
        )
        .unwrap();
    assert_eq!(promoted["skill_domains"], json!(["python"]));
    assert_eq!(
        promoted["reference_rules"],
        json!([{"kind": "dont", "text": "float versions"}])
    );
    assert_eq!(
        field(&e, "mem:knowledge:01A", "expires_at").as_deref(),
        Some("")
    );

    e.store()
        .hash_set(
            "meta:feed:influence",
            &Fields::from([(
                "news".to_owned(),
                r#"{"url": "https://news.example", "skills": {"python": 2}, "topics": ["python"]}"#
                    .to_owned(),
            )]),
        )
        .unwrap();
    for (key, title) in [
        ("mem:knowledge:02A", "Oldest"),
        ("mem:knowledge:02B", "Middle"),
        ("mem:knowledge:02C", "Newest"),
    ] {
        let created = match title {
            "Oldest" => "100.0",
            "Middle" => "200.0",
            _ => "300.0",
        };
        put(
            &e,
            key,
            &format!("{title} release"),
            &[
                ("feed_name", "news"),
                ("title", title),
                ("created_at", created),
            ],
        );
    }

    let proposal = propose(&e, "python");
    assert_eq!(proposal["status"], "proposal");
    assert_eq!(proposal["rules"], json!({"ref": 1, "feed": 2}));
    let draft = proposal["draft"].as_str().unwrap();
    assert!(
        draft.contains("- Avoid: float versions. (https://example.com/pin) [mem:knowledge:01A]"),
        "{draft}"
    );
    assert!(
        draft.contains("- Newest release. (via news, influence 2/10) [mem:knowledge:02C]"),
        "{draft}"
    );
    assert!(draft.contains("- Middle release. (via news, influence 2/10) [mem:knowledge:02B]"));
    assert!(!draft.contains("Oldest"));

    let demoted = e
        .promote_knowledge("mem:knowledge:01A", Some("python"), true, None)
        .unwrap();
    assert_eq!(demoted["demoted_from"], "python");
    assert!(
        demoted.get("skill_domains").is_none(),
        "an empty list is compacted away"
    );
    assert_eq!(
        e.promote_knowledge("mem:knowledge:01A", None, false, None)
            .unwrap(),
        json!({"key": "mem:knowledge:01A", "promoted": true})
    );
}

#[test]
fn find_and_get_skills() {
    let e = engine();
    assert_eq!(
        e.find_skills("python").unwrap()["note"],
        "No skills stored yet. compile_skill(domain=...) creates one."
    );
    lesson(&e, "mem:episodic:01A", "python", "alpha", QUEUES);
    lesson(&e, "mem:episodic:01B", "python", "beta", QUEUES);
    propose(&e, "python");
    write(&e, "python");

    let found = e.find_skills("py").unwrap();
    assert_eq!(found["skills"][0]["skill_id"], "mem:skill:gen:python-local");
    assert_eq!(found["skills"][0]["match"], "domain");
    assert_eq!(found["skills"][0]["confidence"], "high");
    let nothing = e.find_skills("kubernetes ingress certificates").unwrap();
    assert_eq!(nothing["skills"], json!([]));
    assert!(
        nothing["note"]
            .as_str()
            .unwrap()
            .starts_with("No skill cleared the relevance floor (0.25)")
    );
    assert!(invalid(e.find_skills("  ")).contains("cannot be empty"));

    let loaded = e.get_skill("python").unwrap();
    assert_eq!(loaded["status"], "found");
    assert_eq!(loaded["contract_version"], "1");
    assert_eq!(
        loaded["source_manifest"],
        json!(["mem:episodic:01A", "mem:episodic:01B"])
    );
    assert_eq!(
        field(&e, "mem:skill:gen:python-local", "recall_count").as_deref(),
        Some("1")
    );
    let missing = e.get_skill("rust").unwrap();
    assert_eq!(missing["status"], "not_found");
    assert_eq!(
        missing["tried"],
        json!([
            "mem:skill:rust",
            "mem:skill:gen:rust",
            "mem:skill:gen:rust-local"
        ])
    );
    assert_eq!(
        missing["available"][0]["skill_id"],
        "mem:skill:gen:python-local"
    );
}

#[test]
fn briefing_surfaces_suggestions_updates_and_the_knowledge_watch() {
    let e = engine();
    lesson(&e, "mem:episodic:01A", "python", "alpha", QUEUES);
    lesson(&e, "mem:episodic:01B", "python", "beta", QUEUES);
    e.compile_skill(
        "python",
        "propose",
        2,
        true,
        None,
        Some("python background work queues"),
    )
    .unwrap();
    write(&e, "python");

    let greenfield = e.briefing(Some("newproj"), true).unwrap();
    let first = greenfield
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        first, "skill_suggestions",
        "a greenfield briefing leads with skills"
    );
    assert_eq!(
        greenfield["skill_suggestions"]["skills"][0]["load_with"],
        "get_skill('mem:skill:gen:python-local')"
    );

    let later = format!("{}", omnimem_engine::pyfmt::now_secs() + 100.0);
    lesson(
        &e,
        "mem:episodic:01C",
        "python",
        "gamma",
        "prefer small functions",
    );
    e.store()
        .set_field("mem:episodic:01C", "created_at", &later)
        .unwrap();
    put(
        &e,
        "mem:knowledge:01K",
        "never use boring queues for python background work",
        &[("feed_name", "news")],
    );

    let briefing = e.briefing(Some("alpha"), true).unwrap();
    let update = &briefing["skill_updates"][0];
    assert_eq!(update["changes"][0]["change"], "new_source");
    assert_eq!(update["batch_accept_eligible"], true);
    let watch = &briefing["skill_knowledge_watch"][0];
    assert_eq!(watch["articles"][0]["key"], "mem:knowledge:01K");
    assert_eq!(watch["articles"][0]["possible_contradiction"], true);
}

#[test]
fn the_auto_scan_proposes_once_and_respects_an_ignored_draft() {
    let e = engine_with(EngineConfig {
        skill_scan_interval_hours: 24.0,
        skill_scan_min_pool: 2,
        ..EngineConfig::default()
    });
    lesson(
        &e,
        "mem:episodic:01A",
        "rust",
        "alpha",
        "borrow instead of cloning in hot loops",
    );
    lesson(
        &e,
        "mem:episodic:01B",
        "rust",
        "beta",
        "borrow instead of cloning in hot loops",
    );

    let first = e.briefing(Some("alpha"), false).unwrap();
    let proposals = &first["auto_proposed_skills"]["proposals"];
    assert_eq!(proposals[0]["domain"], "rust");
    assert_eq!(proposals[0]["new_skill"], true);
    assert!(
        e.store()
            .hash_get_all("meta:skill:proposal:rust-local")
            .unwrap()
            .is_some()
    );

    let second = e.briefing(Some("alpha"), false).unwrap();
    assert!(
        second.get("auto_proposed_skills").is_none(),
        "the time gate holds"
    );

    // The draft expires unreviewed and the gate reopens: the same draft is withdrawn, not re-proposed.
    e.store()
        .kv_delete("meta:skill:proposal:rust-local")
        .unwrap();
    e.store().kv_delete("meta:skill_scan:last_run").unwrap();
    let third = e.briefing(Some("alpha"), false).unwrap();
    assert!(third.get("auto_proposed_skills").is_none());
    assert!(
        e.store()
            .hash_get_all("meta:skill:proposal:rust-local")
            .unwrap()
            .is_none()
    );
}

fn rezip(data: &[u8], replace: &str, with: &[u8]) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(Cursor::new(data)).unwrap();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let name = file.name().to_owned();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        writer
            .start_file(name.as_str(), zip::write::SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(if name == replace { with } else { &contents })
            .unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn skill_bundles_round_trip_additively() {
    let a = engine();
    lesson(&a, "mem:episodic:01A", "python", "alpha", QUEUES);
    lesson(&a, "mem:episodic:01B", "python", "beta", QUEUES);
    a.store()
        .set_field("mem:episodic:01A", "recall_count", "9")
        .unwrap();
    a.store()
        .hash_set(
            "meta:feed:influence",
            &Fields::from([("news".to_owned(), r#"{"url": "https://news.example", "skills": {"python": 4, "go": 1}, "licence": "cc-by-4.0"}"#.to_owned())]),
        )
        .unwrap();
    propose(&a, "python");
    write(&a, "python");

    let export = a
        .build_skill_export("mem:skill:gen:python-local")
        .unwrap()
        .unwrap();
    assert_eq!(export.memory_count, 2);
    assert!(export.filename.starts_with("omnimem_skill_python-local_"));
    assert_eq!(
        a.build_skill_export("mem:episodic:01A")
            .unwrap()
            .unwrap_err(),
        "Not a skill key"
    );

    let bundle = validate_skill_import(&export.data).unwrap();
    assert_eq!(bundle.skill_key, "mem:skill:gen:python-local");
    assert_eq!(bundle.feeds.len(), 1);
    assert_eq!(
        bundle.feeds[0]["skills"],
        json!({"python": 4}),
        "only the exported domain's score travels"
    );
    assert!(bundle.warnings.is_empty());
    assert!(
        bundle
            .memories
            .iter()
            .all(|(_, f)| !f.contains_key("recall_count"))
    );

    let b = engine();
    let current: Vec<Map<String, Value>> = vec![
        json!({"name": "news", "url": "https://news.example", "skills": {}})
            .as_object()
            .unwrap()
            .clone(),
    ];
    let plan = b.plan_skill_import(&bundle, Some(&current)).unwrap();
    assert_eq!(plan["skill_exists"], false);
    assert_eq!(plan["new_memories"].as_array().unwrap().len(), 2);
    assert_eq!(plan["updated_feeds"], json!(["news"]));

    let applied = b.apply_skill_import(&bundle).unwrap();
    assert_eq!(applied["skill_written"], true);
    assert_eq!(applied["memories_written"].as_array().unwrap().len(), 2);
    assert_eq!(
        field(&b, "mem:episodic:01A", "licence").as_deref(),
        Some("unknown")
    );
    assert!(field(&b, "mem:episodic:01A", "imported_at").is_some());
    assert_eq!(b.store().vector_count(omnimem_core::Namespace::Skill), 1);
    let replay = b.apply_skill_import(&bundle).unwrap();
    assert_eq!(replay["skill_written"], false);
    assert_eq!(replay["memories_skipped"].as_array().unwrap().len(), 2);

    assert_eq!(
        validate_skill_import(b"").unwrap_err(),
        "The uploaded file is empty"
    );
    assert_eq!(
        validate_skill_import(b"not a zip").unwrap_err(),
        "Not a valid zip file"
    );
    let tampered = rezip(&export.data, "SKILL.md", b"hand edited");
    assert_eq!(
        validate_skill_import(&tampered).unwrap_err(),
        "Checksum mismatch on SKILL.md — the bundle is corrupt or was modified after export"
    );
}

#[test]
fn feed_merges_only_ever_add() {
    let feed = |v: Value| v.as_object().unwrap().clone();
    let current = vec![feed(
        json!({"name": "news", "url": "https://a", "skills": {"python": 9}}),
    )];
    let bundle = vec![
        feed(json!({"name": "news", "url": "https://a", "skills": {"python": 2}})),
        feed(json!({"name": "news", "url": "https://b", "skills": {"python": 3}})),
    ];
    let (merged, added, updated, skipped) = merge_feed_influences(&current, &bundle);
    assert_eq!(added, ["news (imported)"]);
    assert!(updated.is_empty());
    assert_eq!(skipped, ["news"]);
    assert_eq!(
        merged[0]["skills"],
        json!({"python": 9}),
        "an existing score is never rewritten"
    );
    assert_eq!(merged.len(), 2);
}
