//! Resolving contradictions: what is written to both rows, who stops
//! warning, and what comes back when a pair is reopened.

use std::sync::Arc;

use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{AuditLevel, Engine, EngineConfig, EngineError, ResolveRequest, Retire};
use omnimem_store::{Actor, AuditQuery, Fields, Store, enter};
use serde_json::{Value, json};

/// The same bag-of-words embedder the other engine tests use.
struct Words;

impl TextEmbedder for Words {
    fn dimension(&self) -> usize {
        VECTOR_DIM
    }
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; VECTOR_DIM];
                for w in t.split_whitespace() {
                    let h = w
                        .to_lowercase()
                        .bytes()
                        .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
                    v[(h % VECTOR_DIM as u64) as usize] += 1.0;
                }
                let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                v.iter().map(|x| x / n).collect()
            })
            .collect())
    }
}

fn engine() -> Engine {
    Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Words),
        EngineConfig {
            auto_maintenance_interval: 1,
            audit_log: AuditLevel::Lifecycle,
            ..EngineConfig::default()
        },
    )
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

/// Two memories the scan links as contradicting each other.
fn linked(e: &Engine) -> (String, String) {
    let a = remember(e, "always use postgres for the job queue in this service");
    let b = remember(e, "never use postgres for the job queue in this service");
    let found = e
        .check_contradictions(None, "episodic", Some("alpha"), false)
        .unwrap();
    assert_eq!(
        found["contradictions"].as_array().unwrap().len(),
        1,
        "{found}"
    );
    (a, b)
}

fn links(e: &Engine, key: &str) -> Vec<Value> {
    serde_json::from_str(&e.store().get(key).unwrap().unwrap()["contradictions"]).unwrap()
}

fn request(
    a: &str,
    b: &str,
    outcome: &str,
    winner: Option<&str>,
    retire: Retire,
) -> ResolveRequest {
    ResolveRequest::new(a, b, outcome, winner, Some("we moved on"), retire).unwrap()
}

fn state(e: &Engine, key: &str) -> String {
    e.store().get(key).unwrap().unwrap()["state"].clone()
}

#[test]
fn each_outcome_writes_the_resolution_to_both_rows() {
    for (outcome, winner) in [
        ("supersedes", Some(1)),
        ("both_valid", None),
        ("both_wrong", None),
        ("false_positive", None),
    ] {
        let e = engine();
        let (a, b) = linked(&e);
        let keys = [a.as_str(), b.as_str()];
        let winner = winner.map(|i| keys[i]);
        let done = e
            .resolve_contradiction(&request(&a, &b, outcome, winner, Retire::No), "cli")
            .unwrap();
        assert_eq!(done["status"], "resolved", "{done}");
        for (key, other) in [(&a, &b), (&b, &a)] {
            let link = &links(&e, key)[0];
            assert_eq!(link["key"], other.as_str(), "{outcome}");
            let r = &link["resolution"];
            assert_eq!(r["outcome"], outcome);
            assert_eq!(r["reason"], "we moved on");
            assert_eq!(r["resolved_by"], "cli");
            assert!(r["resolved_at"].is_string());
            assert_eq!(r.get("winner").and_then(Value::as_str), winner, "{outcome}");
            // The detection is kept, not replaced.
            assert!(link["detected_at"].is_string());
        }
        let row = |k: &str| e.store().get(k).unwrap().unwrap();
        if outcome == "supersedes" {
            // b is current, so a was superseded by it.
            assert_eq!(row(&a)["superseded_by"], b);
            assert_eq!(
                row(&b)["supersedes"],
                json!([a]).to_string().replace(',', ", ")
            );
            let why = e.explain_memory(&a).unwrap();
            assert_eq!(why["superseded_by"], b.as_str());
        } else {
            assert!(row(&a).get("superseded_by").is_none_or(String::is_empty));
        }
    }
}

#[test]
fn a_resolution_needs_a_linked_pair_and_a_coherent_request() {
    let e = engine();
    let (a, b) = linked(&e);
    let c = remember(&e, "something else entirely about gardening");
    let refused = |r: ResolveRequest| match e.resolve_contradiction(&r, "cli") {
        Err(EngineError::Invalid(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert!(refused(request(&a, &c, "both_valid", None, Retire::No)).contains("aren't linked"));
    assert!(refused(request(&a, &b, "supersedes", None, Retire::No)).contains("needs a winner"));
    assert!(
        refused(request(&a, &b, "supersedes", Some(&c), Retire::No)).contains("key_a or key_b")
    );
    assert!(
        refused(request(&a, &b, "both_valid", Some(&a), Retire::No)).contains("Only a supersedes")
    );
    assert!(refused(request(&a, &b, "both_valid", None, Retire::Archive)).contains("retire_loser"));
    assert!(refused(request(&a, &b, "reopened", None, Retire::No)).contains("isn't resolved"));
    assert!(ResolveRequest::new(&a, &b, "won", None, None, Retire::No).is_err());
    // Nothing was written by any of them.
    assert!(links(&e, &a)[0].get("resolution").is_none());
}

/// Nothing finds the pair in dispute: not the scan (with or without a query),
/// not the maintenance pass a briefing runs.
fn assert_not_reported(e: &Engine) {
    let found = e
        .check_contradictions(None, "episodic", Some("alpha"), false)
        .unwrap();
    assert!(
        found["contradictions"].as_array().unwrap().is_empty(),
        "{found}"
    );
    let by_query = e
        .check_contradictions(Some("postgres job queue"), "episodic", None, false)
        .unwrap();
    assert!(by_query["contradictions"].as_array().unwrap().is_empty());
    let briefing = e.briefing(Some("alpha"), false).unwrap();
    assert_eq!(
        briefing["auto_maintenance"]["contradictions_found"],
        json!([])
    );
}

#[test]
fn a_resolved_pair_is_not_relinked_or_reported_again() {
    let e = engine();
    let (a, b) = linked(&e);
    // Unresolved, the scan reports it again as it always did.
    let again = e
        .check_contradictions(None, "episodic", Some("alpha"), false)
        .unwrap();
    assert_eq!(again["contradictions"].as_array().unwrap().len(), 1);

    e.resolve_contradiction(
        &request(&a, &b, "false_positive", None, Retire::No),
        "panel",
    )
    .unwrap();
    assert_not_reported(&e);
    // Still one link per row, resolved.
    assert_eq!(links(&e, &a).len(), 1);
    assert_eq!(links(&e, &b).len(), 1);

    // In either direction: with only one row carrying the resolution (a
    // half-restored backup, say) the pair stays settled.
    for lone in [&a, &b] {
        let other = if lone == &a { &b } else { &a };
        let mut l = links(&e, other);
        let kept = l[0]["resolution"].clone();
        l[0].as_object_mut().unwrap().remove("resolution");
        e.store()
            .set_field(
                other,
                "contradictions",
                &Value::Array(l.clone()).to_string(),
            )
            .unwrap();
        assert_not_reported(&e);
        l[0]["resolution"] = kept;
        e.store()
            .set_field(other, "contradictions", &Value::Array(l).to_string())
            .unwrap();
    }
}

#[test]
fn the_briefing_skips_resolved_links_and_reopening_restores_them() {
    let e = engine();
    let (a, b) = linked(&e);
    let warned = e.session_briefing(Some("alpha"), false).unwrap();
    assert_eq!(
        warned["contradiction_warnings"].as_array().unwrap().len(),
        2
    );

    e.resolve_contradiction(&request(&a, &b, "both_valid", None, Retire::No), "mcp")
        .unwrap();
    let settled = e.session_briefing(Some("alpha"), false).unwrap();
    assert!(settled.get("contradiction_warnings").is_none(), "{settled}");
    assert!(e.open_contradictions().unwrap().is_empty());

    // Reopening puts the warning back, and the history keeps the first call.
    e.resolve_contradiction(&request(&a, &b, "reopened", None, Retire::No), "panel")
        .unwrap();
    let reopened = e.session_briefing(Some("alpha"), false).unwrap();
    assert_eq!(
        reopened["contradiction_warnings"].as_array().unwrap().len(),
        2
    );
    let r = &links(&e, &a)[0]["resolution"];
    assert_eq!(r["outcome"], "reopened");
    assert_eq!(r["history"][0]["outcome"], "both_valid");
    assert_eq!(e.open_contradictions().unwrap().len(), 1);

    // Resolving again appends to the same history.
    e.resolve_contradiction(&request(&a, &b, "both_valid", None, Retire::No), "panel")
        .unwrap();
    let history = links(&e, &b)[0]["resolution"]["history"].clone();
    assert_eq!(history.as_array().unwrap().len(), 2);
}

#[test]
fn recall_shows_the_resolution_instead_of_a_dispute() {
    let e = engine();
    let (a, b) = linked(&e);
    let recalled = |e: &Engine| -> Vec<Value> {
        e.recall(
            "postgres job queue",
            5,
            None,
            Some("alpha"),
            Some(false),
            None,
        )
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
    };
    let hit = |hits: &[Value], key: &str| hits.iter().find(|h| h["key"] == key).unwrap().clone();

    let before = recalled(&e);
    assert_eq!(hit(&before, &a)["contradictions"], 1);

    e.resolve_contradiction(&request(&a, &b, "supersedes", Some(&b), Retire::No), "mcp")
        .unwrap();
    let after = recalled(&e);
    // #24: the superseded memory is no longer a hit of its own. It is
    // folded into its winner, which says it replaces it.
    assert!(after.iter().all(|h| h["key"] != a.as_str()), "{after:?}");
    let current = hit(&after, &b);
    assert!(current.get("superseded_by").is_none());
    assert_eq!(current["replaces"][0]["key"], a.as_str());
    assert_eq!(current["trust"], "accepted");
    assert!(current.get("contradictions").is_none(), "{current}");
    assert_eq!(current["resolved_contradictions"][0]["key"], a.as_str());
    assert_eq!(
        current["resolved_contradictions"][0]["outcome"],
        "supersedes"
    );
    assert_eq!(current["resolved_contradictions"][0]["winner"], b.as_str());
    assert_eq!(
        current["resolved_contradictions"][0]["reason"],
        "we moved on"
    );
    // The loser still says so when read by key.
    let old = e.store().get(&a).unwrap().unwrap();
    assert_eq!(old["superseded_by"], b.as_str());
    assert_eq!(old["trust"], "superseded");
    assert_eq!(current["resolved_contradictions"][0]["key"], a.as_str());
}

#[test]
fn retiring_the_loser_goes_through_the_transition_and_takes_its_facts() {
    for (retire, expected) in [
        (Retire::Deprioritise, "deprioritised"),
        (Retire::Archive, "archived"),
    ] {
        let e = engine();
        let (a, b) = linked(&e);
        // A fact drawn from the memory that is about to lose.
        let fact = Fields::from([
            (
                "content".to_owned(),
                "the service uses postgres queues".to_owned(),
            ),
            ("state".to_owned(), "active".to_owned()),
            ("enriched_from".to_owned(), a.clone()),
        ]);
        e.store()
            .upsert("mem:knowledge:01FACT", &fact, None)
            .unwrap();

        let done = e
            .resolve_contradiction(&request(&a, &b, "supersedes", Some(&b), retire), "cli")
            .unwrap();
        assert_eq!(done["retired"][0]["key"], a.as_str(), "{done}");
        assert_eq!(done["retired"][0]["new_state"], expected);
        assert_eq!(done["retired"][0]["derived_facts"], 1);
        assert_eq!(state(&e, &a), expected);
        assert_eq!(state(&e, "mem:knowledge:01FACT"), expected);
        assert_eq!(state(&e, &b), "active", "the winner is untouched");
        let row = e.store().get(&a).unwrap().unwrap();
        let reason = format!("superseded by {b}");
        let kept = if retire == Retire::Archive {
            &row["archived_reason"]
        } else {
            &row["deprioritised_reason"]
        };
        assert_eq!(kept, &reason);
    }

    // Both wrong retires both; one already past that state is reported.
    let e = engine();
    let (a, b) = linked(&e);
    e.deprioritise(&a, "stale", None).unwrap();
    let done = e
        .resolve_contradiction(
            &request(&a, &b, "both_wrong", None, Retire::Deprioritise),
            "cli",
        )
        .unwrap();
    assert_eq!(done["retired"].as_array().unwrap().len(), 1);
    assert_eq!(done["retire_skipped"][0]["key"], a.as_str());
    assert_eq!(state(&e, &b), "deprioritised");
}

#[test]
fn forgetting_one_side_settles_the_survivor() {
    let e = engine();
    let (a, b) = linked(&e);
    e.forget(&a, true).unwrap();
    let r = &links(&e, &b)[0]["resolution"];
    assert_eq!(r["outcome"], "other_forgotten");
    assert_eq!(r["resolved_by"], "system");
    let briefing = e.session_briefing(Some("alpha"), false).unwrap();
    assert!(
        briefing.get("contradiction_warnings").is_none(),
        "{briefing}"
    );
    assert!(e.open_contradictions().unwrap().is_empty());

    // A pair already settled keeps its own resolution, and a supersession
    // that named the forgotten memory is cleared.
    let e = engine();
    let (a, b) = linked(&e);
    e.resolve_contradiction(&request(&a, &b, "supersedes", Some(&b), Retire::No), "cli")
        .unwrap();
    e.forget(&b, true).unwrap();
    let row = e.store().get(&a).unwrap().unwrap();
    assert!(
        row.get("superseded_by").is_none_or(String::is_empty),
        "{row:?}"
    );
    assert_eq!(links(&e, &a)[0]["resolution"]["outcome"], "supersedes");
}

/// A pair whose other memory was deleted without its links being settled,
/// as 6.x deletions and anything before `settle_links_to_deleted` left them.
fn dangling(e: &Engine) -> (String, String) {
    let (a, b) = linked(e);
    e.store().delete_many(std::slice::from_ref(&b)).unwrap();
    assert_eq!(e.open_contradictions().unwrap().len(), 1);
    (a, b)
}

#[test]
fn a_pair_whose_other_memory_is_gone_can_still_be_cleared() {
    for outcome in ["false_positive", "both_valid"] {
        let e = engine();
        let (a, b) = dangling(&e);
        let done = e
            .resolve_contradiction(&request(&a, &b, outcome, None, Retire::No), "panel")
            .unwrap();
        assert_eq!(done["status"], "resolved");
        assert_eq!(links(&e, &a)[0]["resolution"]["outcome"], outcome);
        assert!(e.store().get(&b).unwrap().is_none_or(|r| r.is_empty()));
        assert!(e.open_contradictions().unwrap().is_empty());
        // Either way round.
        let e = engine();
        let (a, b) = dangling(&e);
        e.resolve_contradiction(&request(&b, &a, outcome, None, Retire::No), "cli")
            .unwrap();
        assert!(e.open_contradictions().unwrap().is_empty());
    }

    // Nothing is left to supersede, reject or reopen.
    let e = engine();
    let (a, b) = dangling(&e);
    for (outcome, winner) in [("supersedes", Some(a.as_str())), ("both_wrong", None)] {
        let err = e
            .resolve_contradiction(&request(&a, &b, outcome, winner, Retire::No), "panel")
            .unwrap_err();
        assert!(matches!(err, EngineError::Invalid(_)), "{err}");
        assert!(err.to_string().contains("no longer exists"), "{err}");
    }
    assert_eq!(e.open_contradictions().unwrap().len(), 1);

    // A missing key the survivor doesn't point at is still a mistake.
    let c = remember(&e, "the deploy runs on fridays");
    let err = e
        .resolve_contradiction(
            &request(
                &c,
                "mem:episodic:01NOTAREALKEY000000000000",
                "false_positive",
                None,
                Retire::No,
            ),
            "panel",
        )
        .unwrap_err();
    assert!(err.to_string().contains("Memory key not found"), "{err}");
}

#[test]
fn maintenance_settles_links_to_memories_that_are_gone() {
    let e = engine();
    let (a, b) = dangling(&e);
    assert_eq!(e.settle_dangling_links().unwrap(), 1);
    let r = &links(&e, &a)[0]["resolution"];
    assert_eq!(r["outcome"], "other_forgotten");
    assert_eq!(r["resolved_by"], "system");
    assert!(e.open_contradictions().unwrap().is_empty());
    assert!(e.store().get(&b).unwrap().is_none_or(|r| r.is_empty()));
    // Once settled there is nothing more to do, and a live pair is left alone.
    assert_eq!(e.settle_dangling_links().unwrap(), 0);
    let e = engine();
    linked(&e);
    assert_eq!(e.settle_dangling_links().unwrap(), 0);
    assert_eq!(e.open_contradictions().unwrap().len(), 1);
}

#[test]
fn a_chain_of_supersessions_keeps_each_link() {
    let e = engine();
    let (a, b) = linked(&e);
    let c = remember(
        &e,
        "always use redis, not postgres, for the job queue in this service",
    );
    e.check_contradictions(None, "episodic", Some("alpha"), false)
        .unwrap();
    e.resolve_contradiction(&request(&a, &b, "supersedes", Some(&b), Retire::No), "cli")
        .unwrap();
    e.resolve_contradiction(&request(&b, &c, "supersedes", Some(&c), Retire::No), "cli")
        .unwrap();
    let row = |k: &str| e.store().get(k).unwrap().unwrap();
    assert_eq!(row(&a)["superseded_by"], b);
    assert_eq!(row(&b)["superseded_by"], c);
    assert_eq!(row(&b)["supersedes"], format!("[\"{a}\"]"));
    assert_eq!(row(&c)["supersedes"], format!("[\"{b}\"]"));
}

#[test]
fn a_proposal_waits_for_a_person_and_a_second_one_replaces_it() {
    let e = engine();
    let (a, b) = linked(&e);
    let first = request(&a, &b, "supersedes", Some(&a), Retire::No);
    let sent = e.propose_resolution(&first, "mcp").unwrap();
    assert_eq!(sent["status"], "awaiting_approval");
    // Nothing is written by proposing.
    assert!(links(&e, &a)[0].get("resolution").is_none());
    assert_eq!(e.pending_resolutions().unwrap().len(), 1);

    // The same pair named the other way round replaces it.
    let second = ResolveRequest::new(
        &b,
        &a,
        "both_valid",
        None,
        Some("different projects"),
        Retire::No,
    )
    .unwrap();
    e.propose_resolution(&second, "mcp").unwrap();
    let pending = e.pending_resolutions().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["outcome"], "both_valid");
    assert_eq!(pending[0]["winner"], "");
    assert_eq!(pending[0]["reason"], "different projects");
    assert!(
        pending[0]["content_a"]
            .as_str()
            .unwrap()
            .contains("postgres")
    );

    // Accepting writes exactly the proposal, and clears it.
    let done = e
        .accept_pending_resolution(&a, &b, "panel")
        .unwrap()
        .unwrap();
    assert_eq!(done["outcome"], "both_valid");
    let r = &links(&e, &a)[0]["resolution"];
    assert_eq!(r["outcome"], "both_valid");
    assert_eq!(r["reason"], "different projects");
    assert_eq!(r["resolved_by"], "panel");
    assert!(e.pending_resolutions().unwrap().is_empty());
    assert!(
        e.accept_pending_resolution(&a, &b, "panel")
            .unwrap()
            .is_none()
    );

    // Discarding writes nothing; resolving by hand clears a stale proposal.
    e.resolve_contradiction(&request(&a, &b, "reopened", None, Retire::No), "cli")
        .unwrap();
    e.propose_resolution(&first, "mcp").unwrap();
    assert!(e.discard_pending_resolution(&b, &a).unwrap());
    assert!(!e.discard_pending_resolution(&b, &a).unwrap());
    e.propose_resolution(&first, "mcp").unwrap();
    e.resolve_contradiction(&request(&a, &b, "both_valid", None, Retire::No), "cli")
        .unwrap();
    assert!(e.pending_resolutions().unwrap().is_empty());
}

#[test]
fn the_audit_log_names_who_resolved_what_and_why() {
    let e = engine();
    let (a, b) = linked(&e);
    {
        let _who = enter(Actor::Cli, "");
        e.resolve_contradiction(&request(&a, &b, "supersedes", Some(&b), Retire::No), "cli")
            .unwrap();
    }
    let rows = e
        .store()
        .audit_entries(&AuditQuery {
            action: Some("contradiction_resolve".into()),
            limit: 10,
            ..AuditQuery::default()
        })
        .unwrap();
    let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
    assert!(
        keys.contains(&a.as_str()) && keys.contains(&b.as_str()),
        "{rows:?}"
    );
    for row in &rows {
        assert_eq!(row.actor, "cli");
        assert_eq!(row.reason, "we moved on");
        assert_eq!(row.to["outcome"], "supersedes");
        assert_eq!(row.to["winner"], b.as_str());
        assert_eq!(row.to["resolved_by"], "cli");
        // The memories' text is never in a row below `full`.
        assert!(!row.to.to_string().contains("postgres"), "{}", row.to);
    }
    let loser = rows.iter().find(|r| r.key == a).unwrap();
    assert_eq!(loser.to["superseded_by"], b.as_str());
}

#[test]
fn a_backup_carries_resolutions_and_a_restore_keeps_the_newer_row() {
    let e = engine();
    let (a, b) = linked(&e);
    let before = e.store().dump().unwrap();
    // The resolution bumps `updated_at`, so the restore merge can tell it
    // from the unresolved row it replaces.
    std::thread::sleep(std::time::Duration::from_millis(5));
    e.resolve_contradiction(&request(&a, &b, "supersedes", Some(&b), Retire::No), "cli")
        .unwrap();
    let after = e.store().dump().unwrap();

    // An older backup doesn't undo it.
    e.store()
        .restore_backup(&before, None, &mut |_, _| {})
        .unwrap();
    assert_eq!(links(&e, &a)[0]["resolution"]["outcome"], "supersedes");
    assert_eq!(e.store().get(&a).unwrap().unwrap()["superseded_by"], b);

    // A newer one brings it into a store that never had it.
    let other = engine();
    other
        .store()
        .restore_backup(&before, None, &mut |_, _| {})
        .unwrap();
    assert!(links(&other, &a)[0].get("resolution").is_none());
    other
        .store()
        .restore_backup(&after, None, &mut |_, _| {})
        .unwrap();
    assert_eq!(links(&other, &b)[0]["resolution"]["winner"], b.as_str());
    assert_eq!(
        other.store().get(&b).unwrap().unwrap()["supersedes"],
        format!("[\"{a}\"]")
    );
}

fn remember_in(e: &Engine, content: &str, project: &str) -> String {
    e.remember(
        content,
        Some(project),
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

/// 7.0.9 (F1): the scan's cap used to keep the store's 200 oldest keys and
/// filter by project afterwards, so a busy store never compared new pairs.
#[test]
fn a_project_scan_sees_new_pairs_behind_a_full_store() {
    let e = engine();
    for i in 0..250 {
        remember_in(
            &e,
            &format!("old note {i} about gardening topic {i}"),
            "old",
        );
    }
    let a = remember_in(&e, "always run the migrations before each deploy", "p");
    let b = remember_in(&e, "never run the migrations before each deploy", "p");
    let found = e
        .check_contradictions(None, "episodic", Some("p"), false)
        .unwrap();
    let pairs = found["contradictions"].as_array().unwrap();
    assert!(
        pairs.iter().any(|c| {
            let ks = [c["key_a"].as_str().unwrap(), c["key_b"].as_str().unwrap()];
            ks.contains(&a.as_str()) && ks.contains(&b.as_str())
        }),
        "{found}"
    );
}

/// 7.0.9 (F3): a change of value is linked by the keyless heuristic.
#[test]
fn the_scan_links_a_changed_value() {
    let e = engine();
    remember(&e, "the user lives in Berlin near the river");
    remember(
        &e,
        "the user moved from Berlin and lives in Lisbon near the river",
    );
    let found = e
        .check_contradictions(None, "episodic", Some("alpha"), false)
        .unwrap();
    assert_eq!(
        found["contradictions"].as_array().unwrap().len(),
        1,
        "{found}"
    );
}

/// 7.0.9 (F3): a person can settle a pair no scan linked; the link is
/// recorded as theirs, and only supersedes and both_wrong may do that.
#[test]
fn a_person_can_supersede_an_unlinked_pair() {
    let e = engine();
    let old = remember(&e, "Sam's favourite colour is blue");
    let new = remember(&e, "something about gardening tools and sheds");
    assert!(links_or_empty(&e, &old).is_empty());
    let r = e
        .resolve_contradiction(
            &request(&old, &new, "supersedes", Some(&new), Retire::No),
            "cli",
        )
        .unwrap();
    assert_eq!(r["status"], "resolved");
    let link = &links(&e, &old)[0];
    assert_eq!(link["key"], new.as_str());
    assert_eq!(link["explanation"], "Recorded by a person.");
    assert_eq!(e.store().get(&old).unwrap().unwrap()["superseded_by"], new);
}

fn links_or_empty(e: &Engine, key: &str) -> Vec<Value> {
    e.store()
        .get(key)
        .unwrap()
        .unwrap()
        .get("contradictions")
        .and_then(|c| serde_json::from_str(c).ok())
        .unwrap_or_default()
}

/// 7.0.9 (F2): a forget takes the recall log entries that found the memory.
#[test]
fn forget_scrubs_the_recall_log() {
    let e = engine();
    let secret = remember(&e, "the locker code is 4471 for the gym");
    let other = remember(&e, "the bike shed opens at nine");
    e.recall("locker code 4471", 5, None, None, None, None)
        .unwrap();
    e.recall("bike shed opens", 5, None, None, None, None)
        .unwrap();
    let logged = |e: &Engine| -> Vec<String> {
        e.store()
            .scan_prefix("log:recall:")
            .unwrap()
            .into_iter()
            .filter_map(|k| e.store().get(&k).unwrap())
            .map(|f| f.get("query").cloned().unwrap_or_default())
            .collect()
    };
    assert!(logged(&e).iter().any(|q| q.contains("4471")));
    let out = e.forget(&secret, true).unwrap();
    assert!(out.to_string().contains(&secret), "{out}");
    let left = logged(&e);
    assert!(!left.iter().any(|q| q.contains("4471")), "{left:?}");
    assert!(left.iter().any(|q| q.contains("bike shed")), "{left:?}");
    assert!(e.store().get(&other).unwrap().is_some());
}

/// 7.0.9 (F4): a query that matches nothing closely names the nearest memory.
#[test]
fn a_query_miss_names_the_nearest_memory() {
    let e = engine();
    let key = remember(&e, "temporary verification code 12345 for the bank login");
    for reply in [
        e.forget("verification code bank", true).unwrap(),
        e.archive("verification code bank", None).unwrap(),
        e.deprioritise("verification code bank", "test", None)
            .unwrap(),
    ] {
        assert_eq!(reply["nearest"]["key"], key.as_str(), "{reply}");
        assert!(reply["nearest"]["similarity"].as_f64().unwrap() < 0.85);
    }
    assert_eq!(state(&e, &key), "active", "nothing was acted on");
}

/// 7.0.9: an exact identifier in a query reaches every memory naming it,
/// and nothing that only shares a prefix.
#[test]
fn forget_by_identifier_takes_every_mention_and_no_lookalike() {
    let e = engine();
    let upper = remember(&e, "Customer ALICE@ACME.IO subscribed to Plan A.");
    let lower = remember(&e, "Customer alice@acme.io is the same person.");
    let other = remember(&e, "Customer alice.smith@acme.io subscribed in 2023.");
    let uk = remember(&e, "Customer alice@acme.io.uk filed an NDA.");
    let out = e.forget("alice@acme.io", true).unwrap();
    let text = out.to_string();
    assert!(text.contains(&upper) && text.contains(&lower), "{out}");
    for kept in [&other, &uk] {
        assert!(
            e.store().get(kept).unwrap().is_some(),
            "{kept} survived: {out}"
        );
    }
}
