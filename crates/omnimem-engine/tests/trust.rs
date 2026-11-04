//! Trust (#23 and #24): who sets a memory's trust, what moves it, and what
//! recall does with it. What every read must not return lives in
//! `negative_recall.rs`; this file is the rest.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use omnimem_core::{EmbeddingError, LanguageModel, LlmError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::trust::TrustAction;
use omnimem_engine::{AuditLevel, Engine, EngineConfig, ResolveRequest, Retire};
use omnimem_store::{Actor, AuditQuery, Fields, Store, enter, trust_of};
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

/// Replies in order.
#[derive(Default)]
struct Scripted(Mutex<VecDeque<String>>);

impl LanguageModel for Scripted {
    fn complete(&self, _: &str, _: &str, _: u32) -> Result<String, LlmError> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "no reply scripted".into())
    }
}

fn config() -> EngineConfig {
    EngineConfig {
        auto_maintenance_interval: 0,
        ..EngineConfig::default()
    }
}

fn engine_with(config: EngineConfig) -> (Engine, Arc<Scripted>) {
    let model = Arc::new(Scripted::default());
    let engine = Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Words),
        config,
    )
    .with_llm(model.clone());
    (engine, model)
}

fn engine() -> (Engine, Arc<Scripted>) {
    engine_with(config())
}

fn remember(e: &Engine, content: &str) -> String {
    e.remember(
        content,
        Some("alpha"),
        None,
        "episodic",
        true,
        Some("raw"),
        None,
        None,
    )
    .unwrap()["key"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn trust(e: &Engine, key: &str) -> String {
    trust_of(&e.store().get(key).unwrap().unwrap()).to_owned()
}

/// A fact the enrichment worker would have extracted from `source`.
fn fact(e: &Engine, key: &str, content: &str, source: &str) {
    e.store()
        .upsert(
            key,
            &Fields::from([
                ("content".to_owned(), content.to_owned()),
                ("enriched_from".to_owned(), source.to_owned()),
                ("project".to_owned(), "alpha".to_owned()),
                ("created_at".to_owned(), "1800000000".to_owned()),
            ]),
            Some(&vector(content)),
        )
        .unwrap();
}

const A: &str = "always use postgres for the job queue in this service";
const B: &str = "never use postgres for the job queue in this service";

/// Two linked memories. With `confirmed`, the model is asked and agrees,
/// which is what disputes them; without, the heuristic link only warns.
fn linked(e: &Engine, model: &Scripted, confirmed: bool) -> (String, String) {
    let (a, b) = (remember(e, A), remember(e, B));
    if confirmed {
        model.0.lock().unwrap().push_back(
            r#"{"is_contradiction": true, "confidence": 0.9, "explanation": "opposite advice"}"#
                .to_owned(),
        );
    }
    let found = e
        .check_contradictions(None, "episodic", Some("alpha"), confirmed)
        .unwrap();
    assert_eq!(
        found["contradictions"].as_array().unwrap().len(),
        1,
        "{found}"
    );
    (a, b)
}

#[test]
fn what_a_memory_starts_as() {
    let (e, _) = engine();
    let own = remember(&e, "the build server is at 10.0.0.5");
    assert_eq!(trust(&e, &own), "accepted");
    fact(&e, "mem:knowledge:f1", "build server address", &own);
    assert_eq!(trust(&e, "mem:knowledge:f1"), "candidate");
    // Nothing was written to give them those: the default does it, so an
    // older backup or a 6.x import gets the same.
    assert!(!e.store().get(&own).unwrap().unwrap().contains_key("trust"));
}

#[test]
fn a_heuristic_link_warns_but_disputes_nothing() {
    let (e, model) = engine();
    let (a, b) = linked(&e, &model, false);
    assert_eq!(
        (trust(&e, &a), trust(&e, &b)),
        ("accepted".into(), "accepted".into())
    );
}

#[test]
fn each_outcome_moves_trust_on() {
    let cases: [(&str, bool, [&str; 2]); 5] = [
        ("supersedes", true, ["superseded", "accepted"]),
        ("both_valid", false, ["accepted", "accepted"]),
        ("false_positive", false, ["accepted", "accepted"]),
        ("both_wrong", false, ["rejected", "rejected"]),
        ("reopened", false, ["disputed", "disputed"]),
    ];
    for (outcome, has_winner, [want_a, want_b]) in cases {
        let (e, model) = engine();
        let (a, b) = linked(&e, &model, true);
        assert_eq!(
            (trust(&e, &a), trust(&e, &b)),
            ("disputed".into(), "disputed".into())
        );
        if outcome == "reopened" {
            // Reopening needs something settled to reopen.
            let settle = ResolveRequest::new(&a, &b, "both_valid", None, None, Retire::No).unwrap();
            e.resolve_contradiction(&settle, "test").unwrap();
        }
        let winner = has_winner.then_some(b.as_str());
        let request =
            ResolveRequest::new(&a, &b, outcome, winner, Some("why"), Retire::No).unwrap();
        e.resolve_contradiction(&request, "test").unwrap();
        assert_eq!(
            (trust(&e, &a), trust(&e, &b)),
            (want_a.to_owned(), want_b.to_owned()),
            "{outcome}"
        );
    }
}

#[test]
fn a_disputed_fact_goes_back_to_being_a_candidate() {
    let (e, model) = engine();
    let source = remember(&e, "notes on the queue");
    fact(&e, "mem:episodic:fact1", A, &source);
    let b = remember(&e, B);
    model.0.lock().unwrap().push_back(
        r#"{"is_contradiction": true, "confidence": 0.9, "explanation": "opposite"}"#.to_owned(),
    );
    e.check_contradictions(None, "episodic", Some("alpha"), true)
        .unwrap();
    assert_eq!(trust(&e, "mem:episodic:fact1"), "disputed");
    let request = ResolveRequest::new(
        "mem:episodic:fact1",
        &b,
        "false_positive",
        None,
        None,
        Retire::No,
    )
    .unwrap();
    e.resolve_contradiction(&request, "test").unwrap();
    assert_eq!(
        trust(&e, "mem:episodic:fact1"),
        "candidate",
        "not promoted by the dispute"
    );
}

#[test]
fn forgetting_one_side_ends_the_other_sides_dispute_or_supersession() {
    let (e, model) = engine();
    let (a, b) = linked(&e, &model, true);
    e.forget(&b, true).unwrap();
    assert_eq!(trust(&e, &a), "accepted");

    let (e, model) = engine();
    let (a, b) = linked(&e, &model, true);
    let request = ResolveRequest::new(&a, &b, "supersedes", Some(&b), None, Retire::No).unwrap();
    e.resolve_contradiction(&request, "test").unwrap();
    assert_eq!(trust(&e, &a), "superseded");
    e.forget(&b, true).unwrap();
    assert_eq!(
        trust(&e, &a),
        "accepted",
        "its winner is gone, so it stands again"
    );
}

#[test]
fn a_person_confirms_rejects_and_restores_and_the_log_says_so() {
    let (e, _) = engine();
    let own = remember(&e, "the build server is at 10.0.0.5");
    fact(&e, "mem:knowledge:f1", "build server address", &own);
    fact(&e, "mem:knowledge:f2", "build server owner", &own);
    let _who = enter(Actor::Panel, "");

    let err = e.set_trust(&own, TrustAction::Confirm, None).unwrap_err();
    assert!(err.to_string().contains("only a candidate"), "{err}");
    e.set_trust("mem:knowledge:f1", TrustAction::Confirm, None)
        .unwrap();
    assert_eq!(trust(&e, "mem:knowledge:f1"), "accepted");

    e.set_trust(&own, TrustAction::Reject, Some("out of date"))
        .unwrap();
    assert_eq!(trust(&e, &own), "rejected");
    e.set_trust(&own, TrustAction::Restore, None).unwrap();
    assert_eq!(trust(&e, &own), "accepted");

    let rows = e
        .store()
        .audit_entries(&AuditQuery {
            key: Some(own.clone()),
            limit: 10,
            ..AuditQuery::default()
        })
        .unwrap();
    let reject = rows.iter().find(|r| r.to["trust"] == "rejected").unwrap();
    assert_eq!(
        (reject.actor.as_str(), reject.action.as_str()),
        ("panel", "trust")
    );
    assert_eq!(reject.reason, "out of date");
    assert_eq!(
        e.audit_level(),
        AuditLevel::Lifecycle,
        "recorded at the default level"
    );

    // Confirm all from one source moves only that source's candidates.
    let other = remember(&e, "a different source");
    fact(&e, "mem:knowledge:f3", "something else", &other);
    assert_eq!(e.confirm_from_source(&own, None).unwrap(), 1);
    assert_eq!(trust(&e, "mem:knowledge:f2"), "accepted");
    assert_eq!(trust(&e, "mem:knowledge:f3"), "candidate");
}

/// No call an agent can make over MCP moves a candidate to accepted: only
/// `set_trust` does, and no tool reaches it.
#[test]
fn nothing_an_agent_can_call_confirms_a_candidate() {
    let (e, _) = engine();
    let own = remember(&e, "the build server is at 10.0.0.5");
    let key = "mem:knowledge:f1";
    fact(&e, key, "build server address", &own);
    let keys = vec![key.to_owned()];
    let _who = enter(Actor::Mcp, "agent");
    e.retag(key, None, Some(vec!["infra".into()]), None)
        .unwrap();
    e.set_licence("open", Some(&keys), None, None).unwrap();
    e.set_provenance("asserted", &keys).unwrap();
    e.promote_knowledge(key, Some("infra"), false, None)
        .unwrap();
    e.deprioritise(key, "later", None).unwrap();
    e.reinstate(key).unwrap();
    e.recall("build server address", 5, None, None, None, None)
        .unwrap();
    assert_eq!(trust(&e, key), "candidate");
}

#[test]
fn a_candidate_ranks_below_an_accepted_twin_and_a_weight_of_one_ties_them() {
    let text = "staging cluster helm chart deploy notes";
    let score_of = |config: EngineConfig| {
        let (e, _) = engine_with(config);
        let own = remember(&e, text);
        // A twin of the accepted memory's text, as a fact whose source isn't
        // among the hits (when it is, the fact folds into it, which is a
        // different rule, tested in tools.rs).
        fact(&e, "mem:knowledge:twin", text, "mem:episodic:01SOURCEGONE");
        let hits = e.recall(text, 5, None, None, None, None).unwrap();
        let hit = |key: &str| -> Value {
            hits.as_array()
                .unwrap()
                .iter()
                .find(|h| h["key"] == key)
                .unwrap_or_else(|| panic!("{key} missing: {hits}"))
                .clone()
        };
        assert_eq!(hit("mem:knowledge:twin")["trust"], "candidate");
        (
            hit(&own)["score"].as_f64().unwrap(),
            hit("mem:knowledge:twin")["score"].as_f64().unwrap(),
        )
    };
    let (own, twin) = score_of(config());
    assert!(
        twin < own,
        "candidate {twin} should rank below accepted {own}"
    );
    assert!(
        (twin / own - 0.9).abs() < 0.02,
        "about the 0.9 weight: {twin} / {own}"
    );
    let (own, twin) = score_of(EngineConfig {
        trust_candidate_weight: 1.0,
        ..config()
    });
    assert!((own - twin).abs() < 1e-6, "{own} vs {twin}");
}

/// A store where everything is accepted scores exactly as one where the
/// weights do nothing: trust never touches a believed memory's score.
#[test]
fn an_all_accepted_store_scores_as_before() {
    let scores = |config: EngineConfig| -> Vec<(String, f64)> {
        let (e, _) = engine_with(config);
        for text in [
            "deploy the staging cluster with helm",
            "rotate the database credentials monthly",
            "staging cluster has three nodes",
        ] {
            remember(&e, text);
        }
        e.recall("staging cluster helm deploy", 10, None, None, None, None)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["content"].as_str().unwrap().to_owned(),
                    h["score"].as_f64().unwrap(),
                )
            })
            .collect()
    };
    let with_trust = scores(config());
    let neutral = scores(EngineConfig {
        trust_candidate_weight: 1.0,
        trust_disputed_weight: 1.0,
        ..config()
    });
    assert!(!with_trust.is_empty());
    assert_eq!(with_trust.len(), neutral.len());
    for ((text, a), (other, b)) in with_trust.iter().zip(&neutral) {
        assert_eq!(text, other);
        // Equal but for recency moving on between the two runs.
        assert!((a - b).abs() < 1e-6, "{text}: {a} vs {b}");
    }
}

#[test]
fn memory_audit_counts_each_trust() {
    let (e, _) = engine();
    let own = remember(&e, "the build server is at 10.0.0.5");
    fact(&e, "mem:knowledge:f1", "build server address", &own);
    let _who = enter(Actor::Panel, "");
    let gone = remember(&e, "a wrong memory");
    e.set_trust(&gone, TrustAction::Reject, None).unwrap();
    let audit = e.memory_audit(None, None, false, 10, 0).unwrap();
    assert_eq!(
        audit["trust"],
        json!({"accepted": 1, "candidate": 1, "rejected": 1})
    );
}

#[test]
fn a_backup_round_trips_trust() {
    let dir = std::env::temp_dir().join(format!("omnimem-trust-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let in_dir = || EngineConfig {
        backup_dir: dir.clone(),
        ..config()
    };
    let (e, _) = engine_with(in_dir());
    let own = remember(&e, "the build server is at 10.0.0.5");
    fact(&e, "mem:knowledge:f1", "build server address", &own);
    let _who = enter(Actor::Panel, "");
    e.set_trust(&own, TrustAction::Reject, Some("wrong"))
        .unwrap();
    let dumped = e.dump_to_file(Some("trust.json")).unwrap();
    assert_ne!(dumped["status"], "error", "{dumped}");

    let (fresh, _) = engine_with(in_dir());
    let restored: Value = fresh.restore_from_file("trust.json", false).unwrap();
    assert_ne!(restored["status"], "error", "{restored}");
    assert_eq!(trust(&fresh, &own), "rejected");
    assert_eq!(trust(&fresh, "mem:knowledge:f1"), "candidate");
    let _ = std::fs::remove_dir_all(&dir);
}
