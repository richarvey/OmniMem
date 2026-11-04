//! Negative recall: what every read channel must NOT return.
//!
//! Most recall tests assert that the right memory comes back. These assert the
//! opposite, because a leak (an archived, forgotten, suppressed or
//! out-of-project memory surfacing in a read) is the failure that costs
//! trust, and a test that only looks for the good result cannot see it.
//!
//! # How the tests are built so they can fail
//!
//! * Every excluded memory is a near-duplicate of the query (cosine
//!   similarity well above the weak band, asserted with the test's own
//!   embedder), so the only thing keeping it out of a result is the exclusion
//!   under test. Each test also asserts the active control IS returned, so an
//!   empty result cannot pass by accident.
//! * Fixed bag-of-words vectors and an in-memory store: no embedding model, so
//!   plain `cargo test` runs this file.
//! * Mutation check: break the exclusion predicate in the engine (for example
//!   `matches!(state, "archived" | "deleted")` in `recall.rs::score_hits`) and
//!   the matching test must fail. Do this by hand when adding a case.
//!
//! # Adding a channel or a condition
//!
//! * A new condition (issue #24 adds `rejected` and `superseded`): add a field
//!   to `Fixture`, create the memory in `Fixture::new` with
//!   `near_duplicate`, put the condition on it, and add it to
//!   `Fixture::excluded`. Every recall test then checks it for free. For the
//!   briefing, extend `stale_fixture` the same way, since a briefing can only
//!   show a memory that is old enough to be stale.
//! * A new channel: add a function that returns the keys the channel
//!   surfaced, then a test that calls it on a `Fixture` and uses
//!   `assert_clean(&keys, &fixture)`.
//! * A gap that is known and scheduled is written as the desired behaviour and
//!   marked `#[ignore = "known gap: ..."]`; run with `--ignored` to see it fail,
//!   and remove the attribute in the release that fixes it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use omnimem_core::{EmbeddingError, LanguageModel, LlmError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::trust::TrustAction;
use omnimem_engine::{DomainFilter, Engine, EngineConfig, GraveyardScope, ResolveRequest, Retire};
use omnimem_store::{Fields, Store};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Embedder and model

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

fn cosine(a: &str, b: &str) -> f32 {
    vector(a).iter().zip(vector(b)).map(|(x, y)| x * y).sum()
}

impl TextEmbedder for Words {
    fn dimension(&self) -> usize {
        VECTOR_DIM
    }
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|t| vector(t)).collect())
    }
}

/// Replies in order; the only thing asked of it here is query expansion.
#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<String>>,
    calls: Mutex<usize>,
}

impl LanguageModel for Scripted {
    fn complete(&self, _model: &str, _prompt: &str, _max: u32) -> Result<String, LlmError> {
        *self.calls.lock().unwrap() += 1;
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "no reply scripted".into())
    }
}

fn engine_with(config: EngineConfig) -> Engine {
    Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Words),
        config,
    )
}

/// Briefing maintenance is off: it would link fixture memories on its own.
fn quiet() -> EngineConfig {
    EngineConfig {
        auto_maintenance_interval: 0,
        ..EngineConfig::default()
    }
}

fn remember(e: &Engine, content: &str, project: &str) -> String {
    e.remember(
        content,
        Some(project),
        None,
        "episodic",
        true, // force: skip dedup, these are near-duplicates on purpose
        Some("raw"),
        None,
        None,
    )
    .unwrap()["key"]
        .as_str()
        .unwrap()
        .to_owned()
}

// ---------------------------------------------------------------------------
// The recall fixture

const QUERY: &str = "deploy the staging cluster with helm charts nightly";
/// The expansion variant: still on topic, still nowhere near `below_floor`.
const VARIANT: &str = "deploy staging cluster helm charts every night";
const TOPIC: &str = "zebrafish";
const FLOOR: f32 = 0.15;
/// Every excluded target must beat this, which is well above the weak band (0.35).
const NEAR_DUPLICATE_MIN: f32 = 0.5;

/// A target is the query plus one distinguishing word: cosine about 0.94.
fn near_duplicate(extra: &str) -> String {
    format!("{QUERY} {extra}")
}

struct Fixture {
    engine: Engine,
    control: String,
    archived: String,
    forgotten: String,
    /// A row still in the store with `state=deleted`, as an import or an
    /// older version could leave; `forget` itself removes the row.
    deleted_state: String,
    suppressed: String,
    other_project: String,
    below_floor: String,
    /// Judged wrong by a person (#24).
    rejected: String,
    /// Replaced by `control`, which a person said is current (#24).
    superseded: String,
}

impl Fixture {
    fn new(config: EngineConfig) -> Self {
        let engine = engine_with(config);
        let control = remember(&engine, &near_duplicate("control"), "alpha");
        let archived = remember(&engine, &near_duplicate("archived"), "alpha");
        let forgotten = remember(&engine, &near_duplicate("forgotten"), "alpha");
        let deleted_state = remember(&engine, &near_duplicate("tombstone"), "alpha");
        let suppressed = remember(&engine, &near_duplicate(TOPIC), "alpha");
        let other_project = remember(&engine, &near_duplicate("elsewhere"), "beta");
        let rejected = remember(&engine, &near_duplicate("rejected"), "alpha");
        let superseded = remember(&engine, &near_duplicate("superseded"), "alpha");
        // Shares one word with the query and the variant ("staging"): a real,
        // nonzero similarity that sits under the floor.
        let below_floor = remember(
            &engine,
            "staging tomatoes watering schedule greenhouse compost pruning mulch harvest basil rosemary lavender soil",
            "alpha",
        );

        engine.archive(&archived, Some("test")).unwrap();
        assert_eq!(
            engine.forget(&forgotten, true).unwrap()["status"],
            "deleted"
        );
        engine
            .store()
            .set_fields(
                &deleted_state,
                &Fields::from([("state".to_owned(), "deleted".to_owned())]),
            )
            .unwrap();
        engine.suppress_topic(TOPIC, Some("test")).unwrap();
        engine
            .set_trust(&rejected, TrustAction::Reject, Some("test"))
            .unwrap();
        // What a person's "the control replaces this" resolution leaves.
        engine
            .store()
            .set_fields(
                &superseded,
                &Fields::from([
                    ("trust".to_owned(), "superseded".to_owned()),
                    ("superseded_by".to_owned(), control.clone()),
                ]),
            )
            .unwrap();

        let fixture = Self {
            engine,
            control,
            archived,
            forgotten,
            deleted_state,
            suppressed,
            other_project,
            below_floor,
            rejected,
            superseded,
        };
        fixture.assert_close_enough();
        fixture
    }

    /// The point of the file: every excluded memory scores well above the
    /// floor against the query, so only its exclusion keeps it out.
    fn assert_close_enough(&self) {
        // A forgotten memory's row is gone, so its text is rebuilt, not read.
        let content = |key: &str| {
            if key == self.forgotten {
                near_duplicate("forgotten")
            } else {
                self.engine.store().get(key).unwrap().unwrap()["content"].clone()
            }
        };
        for key in [
            &self.control,
            &self.archived,
            &self.forgotten,
            &self.deleted_state,
            &self.suppressed,
            &self.other_project,
            &self.rejected,
            &self.superseded,
        ] {
            for q in [QUERY, VARIANT] {
                let sim = cosine(q, &content(key));
                assert!(sim > NEAR_DUPLICATE_MIN, "{key} vs {q:?}: {sim}");
            }
        }
        for q in [QUERY, VARIANT] {
            let sim = cosine(q, &content(&self.below_floor));
            assert!(sim > 0.0 && sim < FLOOR, "below_floor vs {q:?}: {sim}");
        }
    }

    /// Keys that no unscoped read may return.
    fn excluded(&self) -> Vec<(&'static str, &str)> {
        vec![
            ("archived", &self.archived),
            ("forgotten", &self.forgotten),
            ("state=deleted", &self.deleted_state),
            ("suppressed topic", &self.suppressed),
            ("below the floor", &self.below_floor),
            ("rejected", &self.rejected),
            ("superseded (as a hit of its own)", &self.superseded),
        ]
    }
}

/// The control is present and nothing in `excluded()` is.
fn assert_clean(keys: &[String], f: &Fixture, channel: &str) {
    assert!(
        keys.contains(&f.control),
        "{channel}: the active control must come back, got {keys:?}"
    );
    for (why, key) in f.excluded() {
        assert!(
            !keys.iter().any(|k| k == key),
            "{channel}: {why} memory {key} leaked into {keys:?}"
        );
    }
}

fn keys_of(results: &Value) -> Vec<String> {
    results
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["key"].as_str().map(str::to_owned))
        .collect()
}

fn index_keys(index: &Value) -> Vec<String> {
    keys_of(&index["results"])
}

// ---------------------------------------------------------------------------
// recall and recall_index

#[test]
fn recall_never_returns_excluded_memories() {
    let f = Fixture::new(EngineConfig::default());
    let all = keys_of(&f.engine.recall(QUERY, 20, None, None, None, None).unwrap());
    assert_clean(&all, &f, "recall");
    // Unscoped, the other project's memory is legitimately a hit: this is
    // what makes the scoped assertion below meaningful.
    assert!(all.contains(&f.other_project), "{all:?}");

    let scoped = keys_of(
        &f.engine
            .recall(QUERY, 20, None, Some("alpha"), None, None)
            .unwrap(),
    );
    assert_clean(&scoped, &f, "recall scoped");
    assert!(!scoped.contains(&f.other_project), "{scoped:?}");
}

/// #24: a superseded memory appears in recall only inside the hit of the
/// memory that replaced it, never as a hit of its own, and a rejected one
/// not at all. If the winner is gone, the superseded memory comes back
/// rather than both being lost.
#[test]
fn a_superseded_memory_appears_only_inside_its_winner() {
    let f = Fixture::new(EngineConfig::default());
    for (channel, results) in [
        (
            "recall",
            f.engine.recall(QUERY, 20, None, None, None, None).unwrap(),
        ),
        (
            "recall_index",
            f.engine
                .recall_index(QUERY, 20, None, None, 200, None, None)
                .unwrap()["results"]
                .clone(),
        ),
    ] {
        let hits = results.as_array().unwrap();
        let control = hits
            .iter()
            .find(|h| h["key"] == f.control.as_str())
            .unwrap_or_else(|| panic!("{channel}: control missing"));
        let replaces = control["replaces"].to_string();
        assert!(replaces.contains(&f.superseded), "{channel}: {control}");
        assert!(!replaces.contains(&f.rejected), "{channel}: {control}");
        assert_eq!(control["trust"], "accepted", "{channel}");
    }
    // The winner archived: the superseded memory is the best there is.
    f.engine.archive(&f.control, Some("test")).unwrap();
    let keys = keys_of(&f.engine.recall(QUERY, 20, None, None, None, None).unwrap());
    assert!(keys.contains(&f.superseded), "{keys:?}");
    assert!(!keys.contains(&f.rejected), "{keys:?}");
}

#[test]
fn recall_index_never_returns_excluded_memories() {
    let f = Fixture::new(EngineConfig::default());
    let all = index_keys(
        &f.engine
            .recall_index(QUERY, 20, None, None, 100, None, None)
            .unwrap(),
    );
    assert_clean(&all, &f, "recall_index");
    assert!(all.contains(&f.other_project), "{all:?}");

    let scoped = index_keys(
        &f.engine
            .recall_index(QUERY, 20, None, Some("alpha"), 100, None, None)
            .unwrap(),
    );
    assert_clean(&scoped, &f, "recall_index scoped");
    assert!(!scoped.contains(&f.other_project), "{scoped:?}");
}

#[test]
fn recall_with_expansion_never_returns_excluded_memories() {
    let model = Arc::new(Scripted::default());
    model
        .replies
        .lock()
        .unwrap()
        .push_back(serde_json::to_string(&[VARIANT]).unwrap());
    let mut f = Fixture::new(EngineConfig::default());
    f.engine = f.engine.with_llm(model.clone());

    let all = keys_of(
        &f.engine
            .recall(QUERY, 20, None, None, Some(true), None)
            .unwrap(),
    );
    assert_eq!(
        *model.calls.lock().unwrap(),
        1,
        "the expansion (RRF) path must actually have run"
    );
    assert_clean(&all, &f, "recall with expansion");
    assert!(all.contains(&f.other_project), "{all:?}");

    // The variant is cached now, so the scoped and index calls take the same path.
    let scoped = keys_of(
        &f.engine
            .recall(QUERY, 20, None, Some("alpha"), Some(true), None)
            .unwrap(),
    );
    assert_clean(&scoped, &f, "recall with expansion, scoped");
    assert!(!scoped.contains(&f.other_project), "{scoped:?}");

    let index = index_keys(
        &f.engine
            .recall_index(QUERY, 20, None, Some("alpha"), 100, Some(true), None)
            .unwrap(),
    );
    assert_clean(&index, &f, "recall_index with expansion");
    assert!(!index.contains(&f.other_project), "{index:?}");
}

// ---------------------------------------------------------------------------
// recall_detail reports state (it fetches by key, so it cannot exclude)

#[test]
fn recall_detail_reports_the_state_of_an_archived_memory() {
    let f = Fixture::new(EngineConfig::default());
    let detail = f
        .engine
        .recall_detail(&[f.archived.clone(), f.control.clone()])
        .unwrap();
    assert_eq!(detail[0]["key"], f.archived.as_str());
    assert_eq!(detail[0]["state"], "archived");
    assert_eq!(detail[1]["state"], "active");
}

// ---------------------------------------------------------------------------
// briefing

struct BriefingFixture {
    engine: Engine,
    control: String,
    archived: String,
    forgotten: String,
    other_project: String,
    rejected: String,
    superseded: String,
    candidate: String,
    resolved_pair: (String, String),
    open_pair: (String, String),
}

/// A briefing only shows what is stale or in dispute, so these are aged.
fn stale_fixture() -> BriefingFixture {
    let e = engine_with(quiet());
    let control = remember(&e, "old note about the staging cluster", "alpha");
    let archived = remember(&e, "old archived note about the staging cluster", "alpha");
    let forgotten = remember(&e, "old forgotten note about the staging cluster", "alpha");
    let other_project = remember(&e, "old note about the staging cluster", "beta");
    let rejected = remember(&e, "old rejected note about the staging cluster", "alpha");
    let superseded = remember(&e, "old superseded note about the staging cluster", "alpha");
    // An extracted fact no person has confirmed (#24).
    let candidate = remember(
        &e,
        "old unconfirmed note about the staging cluster",
        "alpha",
    );
    e.store()
        .set_fields(
            &candidate,
            &Fields::from([("enriched_from".to_owned(), control.clone())]),
        )
        .unwrap();
    e.set_trust(&rejected, TrustAction::Reject, Some("test"))
        .unwrap();
    e.store()
        .set_fields(
            &superseded,
            &Fields::from([
                ("trust".to_owned(), "superseded".to_owned()),
                ("superseded_by".to_owned(), control.clone()),
            ]),
        )
        .unwrap();

    // Two contradicting pairs, found by the real scan, then one is settled.
    let a = remember(
        &e,
        "always use postgres for the job queue in this service",
        "alpha",
    );
    let b = remember(
        &e,
        "never use postgres for the job queue in this service",
        "alpha",
    );
    let c = remember(
        &e,
        "always enable tls on the metrics endpoint of this service",
        "alpha",
    );
    let d = remember(
        &e,
        "never enable tls on the metrics endpoint of this service",
        "alpha",
    );
    let found = e
        .check_contradictions(None, "episodic", Some("alpha"), false)
        .unwrap();
    assert_eq!(
        found["contradictions"].as_array().unwrap().len(),
        2,
        "{found}"
    );
    let request =
        ResolveRequest::new(&a, &b, "false_positive", None, Some("settled"), Retire::No).unwrap();
    e.resolve_contradiction(&request, "test").unwrap();

    e.archive(&archived, Some("test")).unwrap();
    e.forget(&forgotten, true).unwrap();
    // Age the stale candidates last: archiving and forgetting bump updated_at.
    let long_ago = (omnimem_now() - 400.0 * 86_400.0).to_string();
    for key in [
        &control,
        &archived,
        &forgotten,
        &other_project,
        &rejected,
        &superseded,
        &candidate,
    ] {
        e.store()
            .set_fields(
                key,
                &Fields::from([("updated_at".to_owned(), long_ago.clone())]),
            )
            .unwrap();
    }
    BriefingFixture {
        engine: e,
        control,
        archived,
        forgotten,
        other_project,
        rejected,
        superseded,
        candidate,
        resolved_pair: (a, b),
        open_pair: (c, d),
    }
}

fn omnimem_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

/// Every key a briefing names in its memory-bearing sections.
fn briefing_keys(b: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    for section in [
        "stale_memories",
        "contradiction_warnings",
        "reinstate_candidates",
    ] {
        for item in b[section].as_array().into_iter().flatten() {
            keys.extend(item["key"].as_str().map(str::to_owned));
            for other in item["contradicts"].as_array().into_iter().flatten() {
                keys.extend(other.as_str().map(str::to_owned));
            }
        }
    }
    keys
}

#[test]
fn briefing_never_surfaces_excluded_memories() {
    let f = stale_fixture();
    let briefing = f.engine.briefing(Some("alpha"), false).unwrap();
    let keys = briefing_keys(&briefing);

    assert!(
        keys.contains(&f.control),
        "stale control missing: {briefing}"
    );
    assert!(
        keys.contains(&f.open_pair.0) && keys.contains(&f.open_pair.1),
        "the unresolved pair must still warn: {briefing}"
    );
    for (why, key) in [
        ("archived", &f.archived),
        ("forgotten", &f.forgotten),
        ("other project", &f.other_project),
        ("rejected", &f.rejected),
        ("superseded", &f.superseded),
        ("unconfirmed candidate", &f.candidate),
        ("resolved contradiction", &f.resolved_pair.0),
        ("resolved contradiction", &f.resolved_pair.1),
    ] {
        assert!(!keys.contains(key), "{why} memory {key} leaked: {briefing}");
    }

    // Unscoped, the other project's stale memory is a legitimate entry.
    let all = briefing_keys(&f.engine.briefing(None, false).unwrap());
    assert!(all.contains(&f.other_project), "{all:?}");
}

// ---------------------------------------------------------------------------
// Graveyard warnings: scoped recall and the PreToolUse hook path

struct Graveyard {
    engine: Engine,
    own: String,
    foreign: String,
}

const DEAD_END_QUERY: &str = "should we use celery or kafka for the job queue";

fn graveyard(config: EngineConfig) -> Graveyard {
    let e = engine_with(config);
    // The memories' own text avoids the query's words, so only the warning
    // (keyed by the memory) can bring them into a result.
    let own = remember(&e, "notes from the alpha queue spike", "alpha");
    let foreign = remember(&e, "notes from the beta queue spike", "beta");
    e.record_experience(
        &own,
        4,
        "abandoned",
        3,
        Some(vec![
            json!({"name": "Celery", "type": "library", "reason": "too heavy"})
                .as_object()
                .unwrap()
                .clone(),
        ]),
        None,
        None,
        None,
    )
    .unwrap();
    e.record_experience(
        &foreign,
        4,
        "abandoned",
        3,
        Some(vec![
            json!({"name": "Kafka", "type": "service", "reason": "operational cost"})
                .as_object()
                .unwrap()
                .clone(),
        ]),
        None,
        None,
        None,
    )
    .unwrap();
    Graveyard {
        engine: e,
        own,
        foreign,
    }
}

fn warning_keys(results: &Value) -> Vec<String> {
    results
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["result_type"] == "abandoned_warning")
        .filter_map(|r| r["key"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn scoped_recall_warns_only_about_this_projects_dead_ends() {
    let g = graveyard(EngineConfig::default());
    let results = g
        .engine
        .recall(DEAD_END_QUERY, 10, None, Some("alpha"), None, None)
        .unwrap();
    let warnings = warning_keys(&results);
    assert!(warnings.contains(&g.own), "control missing: {results}");
    assert!(
        !keys_of(&results).contains(&g.foreign),
        "another project's dead end leaked: {results}"
    );

    // GRAVEYARD_SCOPE=all is the switch that restores the 6.x behaviour, and
    // it proves the foreign dead end would have matched.
    let wide = graveyard(EngineConfig {
        graveyard_scope: GraveyardScope::All,
        ..EngineConfig::default()
    });
    let results = wide
        .engine
        .recall(DEAD_END_QUERY, 10, None, Some("alpha"), None, None)
        .unwrap();
    assert!(warning_keys(&results).contains(&wide.foreign), "{results}");
}

#[test]
fn the_hook_path_warns_only_about_this_projects_dead_ends() {
    let g = graveyard(EngineConfig::default());
    let warnings = g
        .engine
        .abandoned_warnings(DEAD_END_QUERY, &["alpha".to_owned()])
        .unwrap();
    assert!(
        warnings.iter().any(|w| w.text.contains("Celery")),
        "control missing: {warnings:?}"
    );
    assert!(
        warnings.iter().all(|w| !w.text.contains("Kafka")),
        "another project's dead end leaked: {warnings:?}"
    );
    assert!(warnings.iter().all(|w| w.elsewhere.is_none()));

    let wide = graveyard(EngineConfig {
        graveyard_scope: GraveyardScope::All,
        ..EngineConfig::default()
    });
    let warnings = wide
        .engine
        .abandoned_warnings(DEAD_END_QUERY, &["alpha".to_owned()])
        .unwrap();
    assert!(
        warnings.iter().any(|w| w.text.contains("Kafka")),
        "scope=all should reach it: {warnings:?}"
    );
}

// ---------------------------------------------------------------------------
// Domain routing

#[test]
fn an_empty_domain_and_project_intersection_returns_nothing() {
    let e = engine_with(EngineConfig::default());
    for (name, domain) in [("alpha", "python"), ("beta", "rust")] {
        e.set_project_context(
            name,
            "a project",
            "stack",
            "goals",
            "state",
            None,
            Some(&DomainFilter::One(domain.to_owned())),
        )
        .unwrap();
    }
    let a = remember(&e, &near_duplicate("alpha"), "alpha");
    let b = remember(&e, &near_duplicate("beta"), "beta");
    let rust = DomainFilter::One("rust".to_owned());
    let python = DomainFilter::One("python".to_owned());

    // Alpha does not declare rust: both filters cannot hold, so nothing at
    // all comes back, not even the near-duplicate sitting in alpha.
    let none = e
        .recall(QUERY, 10, None, Some("alpha"), None, Some(&rust))
        .unwrap();
    assert!(
        keys_of(&none).is_empty(),
        "only the notice may come back: {none}"
    );
    assert_eq!(none[0]["result_type"], "domain_filter_notice");
    let none = e
        .recall_index(QUERY, 10, None, Some("alpha"), 100, None, Some(&rust))
        .unwrap();
    assert!(index_keys(&none).is_empty(), "{none}");

    // Controls: a non-empty intersection returns the project's memory only.
    let hit = keys_of(
        &e.recall(QUERY, 10, None, Some("alpha"), None, Some(&python))
            .unwrap(),
    );
    assert!(hit.contains(&a) && !hit.contains(&b), "{hit:?}");
    let hit = keys_of(&e.recall(QUERY, 10, None, None, None, Some(&rust)).unwrap());
    assert!(hit.contains(&b) && !hit.contains(&a), "{hit:?}");
}

// ---------------------------------------------------------------------------
// Known gaps, to be fixed in 7.1.1 (the Atlas review). Written as the
// behaviour we want; run with `--ignored` to see them fail today.

#[test]
#[ignore = "known gap: briefing ignores suppressed topics; fixed in 7.1.1"]
fn briefing_skips_memories_on_a_suppressed_topic() {
    let e = engine_with(quiet());
    let control = remember(&e, "old note about the staging cluster", "alpha");
    let suppressed = remember(&e, &format!("old note about {TOPIC} tanks"), "alpha");
    e.suppress_topic(TOPIC, Some("test")).unwrap();
    let long_ago = (omnimem_now() - 400.0 * 86_400.0).to_string();
    for key in [&control, &suppressed] {
        e.store()
            .set_fields(
                key,
                &Fields::from([("updated_at".to_owned(), long_ago.clone())]),
            )
            .unwrap();
    }
    let keys = briefing_keys(&e.briefing(Some("alpha"), false).unwrap());
    assert!(keys.contains(&control), "{keys:?}");
    assert!(
        !keys.contains(&suppressed),
        "a suppressed topic's memory surfaced in the briefing: {keys:?}"
    );
}

#[test]
#[ignore = "known gap: graveyard warnings ignore suppressed topics; fixed in 7.1.1"]
fn graveyard_warnings_skip_a_suppressed_topic() {
    let g = graveyard(EngineConfig::default());
    g.engine.suppress_topic("celery", Some("test")).unwrap();
    let results = g
        .engine
        .recall(DEAD_END_QUERY, 10, None, Some("alpha"), None, None)
        .unwrap();
    assert!(
        !warning_keys(&results).contains(&g.own),
        "a suppressed topic still warned in recall: {results}"
    );
    let warnings = g
        .engine
        .abandoned_warnings(DEAD_END_QUERY, &["alpha".to_owned()])
        .unwrap();
    assert!(
        warnings.iter().all(|w| !w.text.contains("Celery")),
        "a suppressed topic still warned on the hook path: {warnings:?}"
    );
}
