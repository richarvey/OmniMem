//! Bi-temporal validity (#26 to #29): who writes when a memory was true and
//! when OmniMem believed it, and what recall does with both.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use omnimem_core::{EmbeddingError, LanguageModel, LlmError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::trust::TrustAction;
use omnimem_engine::{Engine, EngineConfig, ResolveRequest, Retire, Validity};
use omnimem_store::{AuditQuery, Fields, Store, retired_at_of, time_of, trust_of, valid_to_of};
use serde_json::Value;

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

fn get(e: &Engine, key: &str) -> Fields {
    e.store().get(key).unwrap().unwrap()
}

fn trust(e: &Engine, key: &str) -> String {
    trust_of(&get(e, key)).to_owned()
}

fn remember_valid(e: &Engine, content: &str, from: Option<&str>, to: Option<&str>) -> Value {
    e.remember_valid(
        content,
        Some("alpha"),
        None,
        "episodic",
        true,
        Some("raw"),
        None,
        None,
        &Validity {
            valid_from: from.map(str::to_owned),
            valid_to: to.map(str::to_owned),
        },
    )
    .unwrap()
}

fn resolve(e: &Engine, a: &str, b: &str, outcome: &str, winner: Option<&str>) -> Value {
    let request = ResolveRequest::new(a, b, outcome, winner, Some("test"), Retire::No).unwrap();
    e.resolve_contradiction(&request, "test").unwrap()
}

/// A fact the enrichment worker would have extracted from `source`.
fn fact(e: &Engine, key: &str, content: &str, source: &str, extra: &[(&str, &str)]) {
    let mut fields = Fields::from([
        ("content".to_owned(), content.to_owned()),
        ("enriched_from".to_owned(), source.to_owned()),
        ("project".to_owned(), "alpha".to_owned()),
        ("created_at".to_owned(), "1700000000".to_owned()),
    ]);
    for (k, v) in extra {
        fields.insert((*k).to_owned(), (*v).to_owned());
    }
    e.store()
        .upsert(key, &fields, Some(&vector(content)))
        .unwrap();
}

const BERLIN: &str = "Sam lives in Berlin";
const LISBON: &str = "Sam moved to Lisbon";

// ---------------------------------------------------------------------------
// #27: the writers

#[test]
fn remember_takes_a_validity() {
    let (e, _) = engine();
    let stored = remember_valid(&e, LISBON, Some("2026-04-01T00:00:00Z"), None);
    let key = stored["key"].as_str().unwrap();
    assert_eq!(stored["valid_from"], "1775001600.0", "{stored}");
    let row = get(&e, key);
    assert_eq!(time_of(&row, "valid_from"), Some(1_775_001_600.0));
    assert!(!row.contains_key("valid_to"));

    // Prose is read too.
    let stored = remember_valid(&e, "Sam worked at Acme", None, Some("3 weeks ago"));
    assert!(stored["valid_to"].as_str().is_some(), "{stored}");

    // Neither: remember exactly, no interval fields.
    let plain = remember_valid(&e, "Sam likes tea", None, None);
    let row = get(&e, plain["key"].as_str().unwrap());
    assert!(!row.contains_key("valid_from") && !row.contains_key("valid_to"));

    // What can't be read, or ends before it starts, is refused.
    let refuse = |from: Option<&str>, to: Option<&str>| {
        e.remember_valid(
            "Sam owns a bike",
            None,
            None,
            "episodic",
            true,
            Some("raw"),
            None,
            None,
            &Validity {
                valid_from: from.map(str::to_owned),
                valid_to: to.map(str::to_owned),
            },
        )
        .unwrap_err()
        .to_string()
    };
    assert!(refuse(Some("whenever"), None).contains("valid_from"));
    assert!(refuse(Some("2026-05-01"), Some("2026-04-01")).contains("after"));
}

#[test]
fn supersedes_closes_the_losers_validity_and_retires_it() {
    let (e, _) = engine();
    let berlin = remember_valid(&e, BERLIN, None, None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    let lisbon = remember_valid(&e, LISBON, Some("2026-04-01T00:00:00Z"), None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    fact(&e, "mem:knowledge:f1", "Sam's city is Berlin", &berlin, &[]);
    fact(
        &e,
        "mem:knowledge:f2",
        "Sam rents in Berlin",
        &berlin,
        &[("valid_to", "1700000500")],
    );

    resolve(&e, &berlin, &lisbon, "supersedes", Some(&lisbon));
    let loser = get(&e, &berlin);
    assert_eq!(trust_of(&loser), "superseded");
    // It stopped being true when the winner says it became true...
    assert_eq!(time_of(&loser, "valid_to"), Some(1_775_001_600.0));
    // ...and stopped being believed now.
    let retired = time_of(&loser, "retired_at").expect("retired");
    assert!(retired > 1_775_001_600.0);
    let winner = get(&e, &lisbon);
    assert_eq!(time_of(&winner, "retired_at"), None);
    assert_eq!(time_of(&winner, "valid_to"), None);

    // Its facts take both, except an end of their own.
    let f1 = get(&e, "mem:knowledge:f1");
    assert_eq!(valid_to_of(&f1), Some(1_775_001_600.0));
    assert_eq!(retired_at_of(&f1), Some(retired));
    assert_eq!(f1["valid_to_from"], berlin);
    assert_eq!(f1["retired_at_from"], berlin);
    let f2 = get(&e, "mem:knowledge:f2");
    assert_eq!(
        time_of(&f2, "valid_to"),
        Some(1_700_000_500.0),
        "its own end stands"
    );

    // Reopening undoes exactly that: in dispute again, open intervals.
    resolve(&e, &berlin, &lisbon, "reopened", None);
    let loser = get(&e, &berlin);
    assert_eq!(trust_of(&loser), "disputed");
    assert_eq!(time_of(&loser, "valid_to"), None);
    assert_eq!(time_of(&loser, "retired_at"), None);
    let f1 = get(&e, "mem:knowledge:f1");
    assert_eq!(
        (time_of(&f1, "valid_to"), time_of(&f1, "retired_at")),
        (None, None)
    );
    assert_eq!(
        time_of(&get(&e, "mem:knowledge:f2"), "valid_to"),
        Some(1_700_000_500.0)
    );
    // The resolution's history keeps what the supersession wrote.
    let links: Vec<Value> = serde_json::from_str(&get(&e, &berlin)["contradictions"]).unwrap();
    let history = &links[0]["resolution"]["history"];
    assert_eq!(history[0]["outcome"], "supersedes");
    assert_eq!(history[0]["closed_valid_to"], "1775001600.0");
}

#[test]
fn a_winner_with_no_valid_from_closes_the_loser_now() {
    let (e, _) = engine();
    let a = remember_valid(&e, BERLIN, None, None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    let b = remember_valid(&e, LISBON, None, None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    resolve(&e, &a, &b, "supersedes", Some(&b));
    let loser = get(&e, &a);
    assert_eq!(time_of(&loser, "valid_to"), time_of(&loser, "retired_at"));
    assert!(time_of(&loser, "valid_to").is_some());
}

#[test]
fn both_wrong_retires_both_and_ends_no_validity() {
    let (e, _) = engine();
    let a = remember_valid(&e, BERLIN, None, None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    let b = remember_valid(&e, LISBON, None, None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    resolve(&e, &a, &b, "both_wrong", None);
    for key in [&a, &b] {
        let row = get(&e, key);
        assert_eq!(trust_of(&row), "rejected");
        assert!(time_of(&row, "retired_at").is_some(), "{key}");
        assert_eq!(
            time_of(&row, "valid_to"),
            None,
            "never true, so nothing to end"
        );
    }
    resolve(&e, &a, &b, "reopened", None);
    for key in [&a, &b] {
        let row = get(&e, key);
        assert_eq!(trust_of(&row), "disputed");
        assert_eq!(time_of(&row, "retired_at"), None);
    }
}

#[test]
fn a_persons_reject_retires_and_a_restore_unretires() {
    let (e, _) = engine();
    let key = remember_valid(&e, BERLIN, None, None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    fact(&e, "mem:knowledge:f1", "Sam's city is Berlin", &key, &[]);
    e.set_trust(&key, TrustAction::Reject, Some("wrong"))
        .unwrap();
    let at = time_of(&get(&e, &key), "retired_at").expect("retired");
    assert_eq!(retired_at_of(&get(&e, "mem:knowledge:f1")), Some(at));

    let rows = e
        .store()
        .audit_entries(&AuditQuery {
            key: Some(key.clone()),
            limit: 10,
            ..AuditQuery::default()
        })
        .unwrap();
    let reject = rows.iter().find(|r| r.to["trust"] == "rejected").unwrap();
    assert!(
        reject.to["retired_at"].as_str().is_some(),
        "audited at lifecycle: {:?}",
        reject.to
    );

    e.set_trust(&key, TrustAction::Restore, None).unwrap();
    assert_eq!(time_of(&get(&e, &key), "retired_at"), None);
    assert_eq!(trust(&e, &key), "accepted");
    assert_eq!(time_of(&get(&e, "mem:knowledge:f1"), "retired_at"), None);
}

#[test]
fn an_extracted_fact_takes_its_validity_from_the_text_or_its_source() {
    let (e, model) = engine();
    let stored = e
        .remember_valid(
            "Sam moved to Lisbon in April and works at Acme",
            Some("alpha"),
            None,
            "episodic",
            false,
            Some("full"),
            None,
            None,
            &Validity {
                valid_from: Some("2026-04-01T00:00:00Z".to_owned()),
                valid_to: None,
            },
        )
        .unwrap();
    assert_eq!(stored["enrichment"], "queued", "{stored}");
    model.0.lock().unwrap().push_back(
        r#"[{"text": "Sam lives in Lisbon"},
            {"text": "Sam works at Acme", "valid_from": "2026-05-01", "valid_to": "2026-09-01"},
            {"text": "Sam had a backwards interval", "valid_from": "2026-09-01", "valid_to": "2026-05-01"}]"#
            .to_owned(),
    );
    assert!(e.process_next_enrichment().unwrap());
    let facts = e
        .store()
        .list_memories(
            omnimem_core::Namespace::Knowledge,
            &omnimem_store::MemoryFilter::default(),
            &["content", "valid_from", "valid_to"],
        )
        .unwrap();
    let by = |text: &str| {
        facts
            .iter()
            .find(|(_, f)| f["content"] == text)
            .map(|(_, f)| f.clone())
            .unwrap()
    };
    assert_eq!(
        time_of(&by("Sam lives in Lisbon"), "valid_from"),
        Some(1_775_001_600.0)
    );
    let acme = by("Sam works at Acme");
    assert!(time_of(&acme, "valid_from").is_some() && time_of(&acme, "valid_to").is_some());
    let odd = by("Sam had a backwards interval");
    assert_eq!(
        time_of(&odd, "valid_to"),
        None,
        "a backwards interval is dropped"
    );
}

// ---------------------------------------------------------------------------
// #28: the reads

fn utc(day: &str) -> f64 {
    omnimem_engine::validity::parse_instant(&format!("{day}T00:00:00Z")).unwrap()
}

fn set_times(e: &Engine, key: &str, created: &str) {
    let at = format!("{:.1}", utc(created));
    e.store()
        .set_fields(
            key,
            &Fields::from([
                ("created_at".to_owned(), at.clone()),
                ("updated_at".to_owned(), at),
            ]),
        )
        .unwrap();
}

/// Berlin is recorded in January. In May, "moved to Lisbon" is recorded,
/// true from 1 April, and supersedes Berlin.
fn march() -> (Engine, String, String) {
    let (e, _) = engine();
    let berlin = remember_valid(&e, BERLIN, None, None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    set_times(&e, &berlin, "2026-01-10");
    let lisbon = remember_valid(&e, LISBON, Some("2026-04-01T00:00:00Z"), None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    set_times(&e, &lisbon, "2026-05-10");
    resolve(&e, &berlin, &lisbon, "supersedes", Some(&lisbon));
    // The resolution ran now; the scenario has it in May, as Lisbon was
    // recorded.
    let may = format!("{:.1}", utc("2026-05-10"));
    e.store()
        .set_fields(
            &berlin,
            &Fields::from([
                ("retired_at".to_owned(), may.clone()),
                ("valid_to_recorded_at".to_owned(), may),
            ]),
        )
        .unwrap();
    (e, berlin, lisbon)
}

const WHERE: &str = "where does Sam live";

fn ask(e: &Engine, valid: Option<&str>, known: Option<&str>) -> Vec<Value> {
    let as_of = omnimem_engine::AsOf::parse(valid, known).unwrap();
    let out = e
        .recall_at(WHERE, 10, None, Some("alpha"), Some(false), None, as_of)
        .unwrap();
    out.as_array().unwrap().clone()
}

fn keys(results: &[Value]) -> Vec<String> {
    results
        .iter()
        .filter_map(|r| r["key"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn the_march_scenario() {
    let (e, berlin, lisbon) = march();

    // What was true then.
    assert_eq!(
        keys(&ask(&e, Some("2026-03-01"), None)),
        vec![berlin.clone()]
    );
    assert_eq!(
        keys(&ask(&e, Some("2026-05-01"), None)),
        vec![lisbon.clone()]
    );

    // What we believed then: the move wasn't recorded yet, so Berlin, as
    // believed, with what happened to it afterwards attached.
    let then = ask(&e, None, Some("2026-04-15"));
    assert_eq!(keys(&then), vec![berlin.clone()]);
    assert_eq!(then[0]["trust"], "accepted", "{then:?}");
    assert_eq!(then[0]["superseded_by"], lisbon.as_str());
    assert!(then[0]["retired_at"].as_str().is_some());

    // What we believed then about then: the end of Berlin's validity was
    // written in May, so in April it was still true as far as we knew.
    assert_eq!(
        keys(&ask(&e, Some("2026-04-15"), Some("2026-04-15"))),
        vec![berlin.clone()]
    );
    // Asked from June about mid-April, the move is known.
    assert_eq!(
        keys(&ask(&e, Some("2026-04-15"), Some("2026-06-01"))),
        vec![lisbon.clone()]
    );

    // An ordinary recall: Lisbon, with Berlin folded in.
    let now = ask(&e, None, None);
    assert_eq!(keys(&now), vec![lisbon.clone()]);
    assert_eq!(now[0]["replaces"][0]["key"], berlin.as_str());
    assert_eq!(now[0]["valid_from"], "2026-04-01T00:00:00Z");

    // recall_index takes the same times.
    let index = e
        .recall_index_at(
            WHERE,
            10,
            None,
            Some("alpha"),
            100,
            Some(false),
            None,
            omnimem_engine::AsOf::parse(Some("2026-03-01"), None).unwrap(),
        )
        .unwrap();
    let index_keys: Vec<&str> = index["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["key"].as_str())
        .collect();
    assert_eq!(index_keys, [berlin.as_str()]);

    // A time that can't be read is refused, not ignored.
    assert!(omnimem_engine::AsOf::parse(Some("whenever"), None).is_err());
}

/// Under `known_at`, a memory rejected later was believed then, and one
/// rejected earlier wasn't.
#[test]
fn known_at_judges_trust_as_it_stood() {
    let (e, _) = engine();
    let key = remember_valid(&e, BERLIN, None, None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    set_times(&e, &key, "2026-01-10");
    e.set_trust(&key, TrustAction::Reject, None).unwrap();
    assert!(
        keys(&ask(&e, None, None)).is_empty(),
        "rejected: never a hit now"
    );
    let then = ask(&e, None, Some("2026-02-01"));
    assert_eq!(keys(&then), vec![key.clone()]);
    assert_eq!(then[0]["trust"], "accepted");
    assert!(
        keys(&ask(&e, None, Some("2025-12-01"))).is_empty(),
        "not yet recorded"
    );
}

// ---------------------------------------------------------------------------
// #34: end_validity

#[test]
fn end_validity_closes_a_memory_with_no_successor() {
    let (e, _) = engine();
    let traefik = remember_valid(&e, "We route traffic through Traefik", None, None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    set_times(&e, &traefik, "2026-01-10");
    fact(
        &e,
        "mem:knowledge:t1",
        "The proxy is Traefik",
        &traefik,
        &[],
    );
    let before = get(&e, &traefik);

    let ended = e
        .end_validity(
            &traefik,
            Some("2026-06-01T00:00:00Z"),
            Some("moved to Caddy"),
        )
        .unwrap();
    assert_eq!(ended["status"], "ended");
    assert_eq!(ended["valid_to"], "2026-06-01T00:00:00Z");
    assert_eq!(ended["derived_facts"], 1);
    let row = get(&e, &traefik);
    assert_eq!(time_of(&row, "valid_to"), Some(utc("2026-06-01")));
    assert!(time_of(&row, "valid_to_recorded_at").is_some());
    // Still believed: validity isn't a verdict on belief.
    assert_eq!(trust_of(&row), "accepted");
    assert_eq!(time_of(&row, "retired_at"), None);
    // updated_at moves, so a restore and the skill compiler see it.
    assert!(time_of(&row, "updated_at") > time_of(&before, "updated_at"));
    // The fact follows.
    assert_eq!(
        valid_to_of(&get(&e, "mem:knowledge:t1")),
        Some(utc("2026-06-01"))
    );

    // Audited at lifecycle, with the reason and the new value.
    let rows = e
        .store()
        .audit_entries(&AuditQuery {
            key: Some(traefik.clone()),
            limit: 10,
            ..AuditQuery::default()
        })
        .unwrap();
    let end = rows
        .iter()
        .find(|r| r.action == "end_validity")
        .expect("audited");
    assert_eq!(end.reason, "moved to Caddy");
    assert!(end.to["valid_to"].as_str().is_some(), "{:?}", end.to);

    // It still comes back for the time it held, and not after.
    let q = "how do we route traffic";
    let at = |day: &str| {
        let as_of = omnimem_engine::AsOf::parse(Some(day), None).unwrap();
        keys(
            e.recall_at(q, 5, None, Some("alpha"), Some(false), None, as_of)
                .unwrap()
                .as_array()
                .unwrap(),
        )
    };
    assert_eq!(at("2026-03-01"), vec![traefik.clone()]);
    assert!(at("2026-07-01").is_empty());

    // Again, later: the earlier end is reported.
    let again = e.end_validity(&traefik, Some("2026-05-01"), None).unwrap();
    assert_eq!(again["previous_valid_to"], "2026-06-01T00:00:00Z");
}

#[test]
fn end_validity_refuses_what_it_cant_end() {
    let (e, _) = engine();
    let key = remember_valid(&e, LISBON, Some("2026-04-01T00:00:00Z"), None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    let refused =
        |key: &str, to: Option<&str>| e.end_validity(key, to, None).unwrap_err().to_string();
    assert!(refused(&key, Some("2026-03-01")).contains("after"));
    assert!(refused(&key, Some("whenever")).contains("valid_to"));
    assert!(refused("mem:episodic:nope", None).contains("not found"));
    assert!(refused("mem:skill:gen:x", None).contains("Cannot"));
    // With no date, it ends now.
    let now = e.end_validity(&key, None, None).unwrap();
    assert!(now["valid_to"].as_str().is_some());
}

// ---------------------------------------------------------------------------
// #35: remember hints may_supersede

#[test]
fn remember_hints_that_a_changed_fact_may_supersede() {
    // The word-count embedder scores these pairs low, so the bar is lowered
    // to where the real model puts "lives in Berlin" against "moved to
    // Lisbon" (0.70) relative to an unrelated pair.
    let (e, _) = engine_with(EngineConfig {
        contradiction_threshold: 0.2,
        ..config()
    });
    let soft = |content: &str| {
        e.remember(
            content,
            Some("alpha"),
            None,
            "episodic",
            false,
            Some("raw"),
            None,
            None,
        )
        .unwrap()
    };
    let berlin = soft(BERLIN)["key"].as_str().unwrap().to_owned();

    let lisbon = soft(LISBON);
    let hint = &lisbon["may_supersede"];
    assert_eq!(hint["key"], berlin.as_str(), "{lisbon}");
    assert!(hint["similarity"].as_f64().unwrap() >= 0.2);
    // A hint only: nothing about either memory changed.
    assert_eq!(trust(&e, &berlin), "accepted");
    assert_eq!(time_of(&get(&e, &berlin), "valid_to"), None);
    assert_eq!(trust(&e, lisbon["key"].as_str().unwrap()), "accepted");

    // An unrelated neighbour, even one that says "now", gets no hint.
    let unrelated = soft("We now deploy with Helm charts");
    assert!(unrelated.get("may_supersede").is_none(), "{unrelated}");

    // The marker has to be in the new statement, not the old one.
    let (e2, _) = engine_with(EngineConfig {
        contradiction_threshold: 0.2,
        ..config()
    });
    let soft2 = |content: &str| {
        e2.remember(
            content,
            Some("alpha"),
            None,
            "episodic",
            false,
            Some("raw"),
            None,
            None,
        )
        .unwrap()
    };
    soft2("Sam moved to Lisbon last spring");
    let plain = soft2("Sam lives in Lisbon");
    assert!(plain.get("may_supersede").is_none(), "{plain}");

    // A neighbour already superseded isn't offered again.
    let (e3, _) = engine_with(EngineConfig {
        contradiction_threshold: 0.2,
        ..config()
    });
    let old = remember_valid(&e3, BERLIN, None, None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    let mid = remember_valid(&e3, "Sam lived in Porto", None, None)["key"]
        .as_str()
        .unwrap()
        .to_owned();
    resolve(&e3, &old, &mid, "supersedes", Some(&mid));
    let again = e3
        .remember(
            "Sam moved to Berlin again",
            Some("alpha"),
            None,
            "episodic",
            false,
            Some("raw"),
            None,
            None,
        )
        .unwrap();
    assert_ne!(again["may_supersede"]["key"], old.as_str(), "{again}");
}
