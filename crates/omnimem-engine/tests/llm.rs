//! Phase 5: fact extraction, enrichment, query expansion and contradiction
//! tier 2 against a scripted model, on a real in-memory store.

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use omnimem_core::{EmbeddingError, LanguageModel, LlmError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig};
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
        v.iter_mut().for_each(|x| *x /= n);
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

/// Replies in order; records every call.
#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<Result<String, String>>>,
    calls: Mutex<Vec<(String, String, u32)>>,
}

impl Scripted {
    fn reply(&self, text: &str) {
        self.replies.lock().unwrap().push_back(Ok(text.to_owned()));
    }
    fn fail(&self, message: &str) {
        self.replies
            .lock()
            .unwrap()
            .push_back(Err(message.to_owned()));
    }
    fn calls(&self) -> Vec<(String, String, u32)> {
        self.calls.lock().unwrap().clone()
    }
}

impl LanguageModel for Scripted {
    fn complete(&self, model: &str, prompt: &str, max_tokens: u32) -> Result<String, LlmError> {
        self.calls
            .lock()
            .unwrap()
            .push((model.to_owned(), prompt.to_owned(), max_tokens));
        match self.replies.lock().unwrap().pop_front() {
            Some(Ok(text)) => Ok(text),
            Some(Err(message)) => Err(message.into()),
            None => Err("no reply scripted".into()),
        }
    }
}

fn engine_with(config: EngineConfig, model: Option<Arc<Scripted>>) -> Engine {
    let engine = Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Words),
        config,
    );
    match model {
        Some(m) => engine.with_llm(m),
        None => engine,
    }
}

fn scripted() -> (Engine, Arc<Scripted>) {
    let model = Arc::new(Scripted::default());
    (
        engine_with(EngineConfig::default(), Some(model.clone())),
        model,
    )
}

fn memories(e: &Engine, prefix: &str) -> Vec<Fields> {
    let keys = e.store().scan_prefix(prefix).unwrap();
    e.store()
        .get_multi(&keys)
        .unwrap()
        .into_iter()
        .flatten()
        .collect()
}

fn local_midnight(y: i32, m: u32, d: u32) -> String {
    use chrono::{Local, NaiveDate, TimeZone};
    let naive = NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    omnimem_engine::pyfmt::py_float(
        Local
            .from_local_datetime(&naive)
            .earliest()
            .unwrap()
            .timestamp() as f64,
    )
}

#[test]
fn extraction_parses_fenced_replies_and_fails_open() {
    let (e, model) = scripted();
    model.reply(
        "```json\n[{\"text\": \" Uses Rust \", \"kind\": \"PREFERENCE\"}, {\"text\": \"\"}, \
         {\"text\": \"Shipped\", \"kind\": \"other\", \"event_date\": \"2026-03-15T10:00:00Z\"}, 5]\n```",
    );
    let facts = e.extract_facts("we use rust and shipped");
    assert_eq!(facts.len(), 2);
    assert_eq!(
        (facts[0].text.as_str(), facts[0].kind, facts[0].event_date),
        ("Uses Rust", "preference", None)
    );
    assert_eq!(
        (facts[1].kind, facts[1].event_date),
        ("fact", Some(1_773_568_800.0))
    );
    let (model_name, prompt, max_tokens) = &model.calls()[0];
    assert_eq!(model_name, "claude-haiku-4-5-20251001");
    assert_eq!(*max_tokens, 2048);
    assert!(prompt.starts_with("Extract discrete, atomic facts from the following text."));
    assert!(prompt.ends_with("Text:\nwe use rust and shipped\n"));

    model.fail("overloaded");
    assert!(e.extract_facts("more").is_empty());
    model.reply("{\"text\": \"not a list\"}");
    assert!(e.extract_facts("more").is_empty());
    assert!(e.extract_facts("   ").is_empty());
    assert_eq!(
        model.calls().len(),
        3,
        "blank content never reaches the model"
    );

    let offline = engine_with(EngineConfig::default(), None);
    assert!(offline.extract_facts("anything").is_empty());
}

#[test]
fn enrichment_writes_linked_facts_that_inherit_their_source() {
    let (e, model) = scripted();
    let tags = vec!["team".to_owned()];
    let stored = e
        .remember(
            "the team moved the api to v2 and prefers tabs",
            Some("alpha"),
            Some(&tags),
            "episodic",
            false,
            None,
            Some("cc-by-4.0"),
            Some("asserted"),
        )
        .unwrap();
    assert_eq!(stored["enrichment"], "queued");
    let source = stored["key"].as_str().unwrap().to_owned();
    let created_at = e.store().get(&source).unwrap().unwrap()["created_at"].clone();

    let reply = r#"[{"text": "The team prefers tabs", "kind": "preference"},
                    {"text": "The API moved to v2", "event_date": "2026-01-02"},
                    {"text": "The team has an API"}]"#;
    model.reply(reply);
    assert!(e.process_next_enrichment().unwrap());
    assert!(!e.process_next_enrichment().unwrap(), "the job was removed");

    let preferences = memories(&e, "mem:preference:");
    assert_eq!(preferences.len(), 1);
    let preference = &preferences[0];
    assert_eq!(preference["content"], "The team prefers tabs");
    assert_eq!(preference["scope"], "project");
    assert_eq!(preference["project"], "alpha");
    assert_eq!(preference["enriched_from"], source);
    assert_eq!(preference["source_doc_id"], source);
    assert_eq!(preference["surface_score"], "0.5");
    assert_eq!(preference["tags"], r#"["team"]"#);
    assert_eq!(preference["licence"], "open");
    assert_eq!(preference["licence_note"], "CC BY 4.0");
    assert_eq!(preference["provenance"], "asserted");
    assert_eq!(
        preference["event_date"], created_at,
        "no date of its own: the source's ingest time"
    );

    let knowledge = memories(&e, "mem:knowledge:");
    assert_eq!(knowledge.len(), 2);
    let dated = knowledge
        .iter()
        .find(|f| f["content"] == "The API moved to v2")
        .unwrap();
    assert_eq!(dated["event_date"], local_midnight(2026, 1, 2));

    // The same facts again are duplicates and are skipped.
    e.store()
        .enqueue_enrichment(&json!({"key": source, "project": "alpha"}))
        .unwrap();
    model.reply(reply);
    assert!(e.process_next_enrichment().unwrap());
    assert_eq!(
        memories(&e, "mem:knowledge:").len() + memories(&e, "mem:preference:").len(),
        3
    );
}

#[test]
fn batch_enrichment_links_facts_to_the_document() {
    let model = Arc::new(Scripted::default());
    let e = engine_with(
        EngineConfig {
            enrichment_batch_mode: true,
            ..EngineConfig::default()
        },
        Some(model.clone()),
    );
    let doc = e
        .remember_document(
            "first paragraph about queues\n\nsecond paragraph about workers",
            "paragraphs",
            None,
            None,
            "episodic",
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(doc["enrichment"], "batch_queued");
    assert_eq!(e.store().enrichment_pending().unwrap(), 1);
    model.reply(r#"[{"text": "Workers read from queues"}]"#);
    assert!(e.process_next_enrichment().unwrap());
    assert!(
        model.calls()[0]
            .1
            .contains("first paragraph about queues\n\nsecond paragraph about workers")
    );
    let facts = memories(&e, "mem:knowledge:");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0]["source_doc_id"], doc["doc_id"]);
    assert_eq!(facts[0]["enriched_from"], doc["keys"][0]);
    assert_eq!(facts[0]["licence"], "own");
    assert!(!facts[0].contains_key("project"));
}

#[test]
fn jobs_without_a_source_or_a_model_are_consumed_quietly() {
    let (e, model) = scripted();
    e.store()
        .enqueue_enrichment(&json!({"key": "mem:episodic:01GONE"}))
        .unwrap();
    e.store()
        .enqueue_enrichment(&json!("not an object"))
        .unwrap();
    assert!(e.process_next_enrichment().unwrap());
    assert!(e.process_next_enrichment().unwrap());
    assert!(model.calls().is_empty());

    let offline = engine_with(EngineConfig::default(), None);
    offline
        .remember(
            "some content worth keeping",
            None,
            None,
            "episodic",
            false,
            None,
            None,
            None,
        )
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let (stop, engine) = (stop.clone(), Arc::new(offline));
        let handle_engine = engine.clone();
        (
            std::thread::spawn(move || handle_engine.run_enrichment_worker(&stop)),
            engine,
        )
    };
    for _ in 0..50 {
        if worker.1.store().enrichment_pending().unwrap() == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    worker.0.join().unwrap();
    assert_eq!(worker.1.store().enrichment_pending().unwrap(), 0);
    assert!(memories(&worker.1, "mem:knowledge:").is_empty());
}

#[test]
fn expansion_is_cached_cleaned_and_unions_into_recall() {
    let (e, model) = scripted();
    e.remember(
        "I studied Business Administration at university",
        None,
        None,
        "episodic",
        true,
        Some("raw"),
        None,
        None,
    )
    .unwrap();
    let query = "which qualification was earned";

    let plain = e.recall(query, 5, None, None, None, None).unwrap();
    assert_eq!(
        plain,
        json!([]),
        "no shared words, nothing clears the floor"
    );
    assert!(model.calls().is_empty(), "expansion is off by default");

    model.reply("```\n[\"studied business administration university\", 7, null, \"\"]\n```");
    let expanded = e.recall(query, 5, None, None, Some(true), None).unwrap();
    assert_eq!(expanded.as_array().unwrap().len(), 1);
    assert_eq!(
        expanded[0]["content"],
        "I studied Business Administration at university"
    );
    let (model_name, prompt, max_tokens) = &model.calls()[0];
    assert_eq!(
        (model_name.as_str(), *max_tokens),
        ("claude-haiku-4-5-20251001", 512)
    );
    assert!(prompt.starts_with("Generate 3 alternative phrasings"));
    assert!(prompt.ends_with("Original query: which qualification was earned\n"));

    assert_eq!(
        e.expand_query(query),
        ["studied business administration university", "7"]
    );
    assert_eq!(
        model.calls().len(),
        1,
        "the second expansion came from the cache"
    );

    model.fail("timeout");
    assert!(e.expand_query("another query").is_empty());
    model.reply(r#"["a", "b", "c", "d"]"#);
    assert_eq!(
        e.expand_query("another query"),
        ["a", "b", "c"],
        "a failure isn't cached"
    );
}

#[test]
fn tier_two_confirms_or_rejects_heuristic_matches() {
    let seed = |e: &Engine| {
        e.remember(
            "always use sqlite for local storage",
            Some("p"),
            None,
            "episodic",
            true,
            Some("raw"),
            None,
            None,
        )
        .unwrap();
        e.remember(
            "never use sqlite for local storage",
            Some("p"),
            None,
            "episodic",
            true,
            Some("raw"),
            None,
            None,
        )
        .unwrap();
    };

    let offline = engine_with(EngineConfig::default(), None);
    seed(&offline);
    assert_eq!(
        offline
            .check_contradictions(None, "episodic", None, true)
            .unwrap(),
        json!({"contradictions": []})
    );

    let (e, model) = scripted();
    seed(&e);
    model.reply(
        r#"{"is_contradiction": false, "confidence": 0.2, "explanation": "different contexts"}"#,
    );
    assert_eq!(
        e.check_contradictions(None, "episodic", None, true)
            .unwrap()["contradictions"],
        json!([])
    );

    model.reply("Sure. {\"is_contradiction\": true, \"confidence\": 0.9, \"explanation\": \"Opposite advice\"} Hope that helps");
    let confirmed = e
        .check_contradictions(None, "episodic", None, true)
        .unwrap();
    let entry = &confirmed["contradictions"][0];
    assert_eq!(entry["method"], "api_confirmed");
    assert_eq!(entry["confidence"], 0.9);
    assert_eq!(entry["explanation"], "Opposite advice");
    let (model_name, prompt, max_tokens) = &model.calls()[1];
    assert_eq!(
        (model_name.as_str(), *max_tokens),
        ("claude-haiku-4-5-20251001", 256)
    );
    assert!(prompt.contains("Memory A:\n"));
    let linked: Value = serde_json::from_str(
        &e.store()
            .get(entry["key_a"].as_str().unwrap())
            .unwrap()
            .unwrap()["contradictions"],
    )
    .unwrap();
    assert_eq!(linked[0]["explanation"], "Opposite advice");

    model.fail("overloaded");
    let (e2, model2) = scripted();
    seed(&e2);
    model2.fail("overloaded");
    assert_eq!(
        e2.check_contradictions(None, "episodic", None, true)
            .unwrap()["contradictions"],
        json!([])
    );
}
