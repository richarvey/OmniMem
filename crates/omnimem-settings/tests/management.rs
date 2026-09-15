//! Duplicates, contradictions, suppressions, telemetry and token overhead
//! through `Panel::handle`.

use std::sync::Arc;

use axum::http::{Request, Response};
use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_settings::{Panel, StaticOverhead};
use omnimem_store::{Fields, Store};

/// Texts of the same length embed identically, so duplicates are easy to make.
struct Flat;

impl TextEmbedder for Flat {
    fn dimension(&self) -> usize {
        VECTOR_DIM
    }
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; VECTOR_DIM];
                v[t.len() % VECTOR_DIM] = 1.0;
                v
            })
            .collect())
    }
}

fn setup() -> (Panel, Arc<Engine>) {
    let engine = Arc::new(Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Flat),
        EngineConfig::default(),
    ));
    let panel = Panel::new();
    panel.set_engine(engine.clone());
    (panel, engine)
}

fn remember(engine: &Engine, content: &str, project: &str) -> String {
    engine
        .remember(
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

async fn get(panel: &Panel, uri: &str) -> Response<Vec<u8>> {
    panel
        .handle(
            Request::get(format!("omnimem://localhost{uri}"))
                .body(Vec::new())
                .unwrap(),
        )
        .await
}

async fn post(panel: &Panel, uri: &str, form: &str) -> Response<Vec<u8>> {
    panel
        .handle(
            Request::post(format!("omnimem://localhost{uri}"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(form.as_bytes().to_vec())
                .unwrap(),
        )
        .await
}

fn text(response: &Response<Vec<u8>>) -> String {
    String::from_utf8_lossy(response.body()).into_owned()
}

fn maintenance_ran(engine: &Engine) {
    engine
        .store()
        .hash_set(
            "meta:maintenance:omnimem",
            &Fields::from([
                ("last_maintenance_at".to_owned(), "1757000000.0".to_owned()),
                (
                    "last_maintenance_summary".to_owned(),
                    r#"{"duplicates_archived": 2, "contradictions_found": 1}"#.to_owned(),
                ),
            ]),
        )
        .unwrap();
}

#[tokio::test]
async fn duplicates_scan_and_maintenance_is_noted() {
    let (panel, engine) = setup();
    maintenance_ran(&engine);
    let first = remember(&engine, "Twins share a length: one", "omnimem");
    let second = remember(&engine, "Twins share a length: two", "omnimem");
    remember(
        &engine,
        "Something else entirely, a different length",
        "omnimem",
    );

    let page = text(&get(&panel, "/duplicates").await);
    assert!(page.contains("04 Sep 2025 15:33 UTC"), "{page}");
    assert!(page.contains("(project: omnimem, 2 duplicates archived)"));
    assert!(page.contains("class=\"nav-link active\">Duplicates"));

    let scan = text(&get(&panel, "/duplicates/scan?namespace=episodic").await);
    assert!(scan.contains("Found 1 cluster of duplicates"), "{scan}");
    assert!(scan.contains(&first) && scan.contains(&second));
    assert!(scan.contains("similarity 1.0"), "{scan}");
    assert!(
        text(&get(&panel, "/duplicates/scan?namespace=knowledge").await)
            .contains("No duplicates found in knowledge.")
    );
}

#[tokio::test]
async fn contradictions_list_each_pair_once() {
    let (panel, engine) = setup();
    maintenance_ran(&engine);
    let a = remember(&engine, "Always squash merge", "omnimem");
    let b = remember(&engine, "Never squash merge", "omnimem");
    engine
        .store()
        .set_field(
            &a,
            "contradictions",
            &format!(r#"[{{"key": "{b}", "explanation": "They disagree", "similarity": 0.81}}]"#),
        )
        .unwrap();
    engine
        .store()
        .set_field(&b, "contradictions", &format!(r#"[{{"key": "{a}"}}]"#))
        .unwrap();

    let page = text(&get(&panel, "/contradictions").await);
    assert!(page.contains("1 contradiction pair"), "{page}");
    assert!(!page.contains("pairs"));
    assert!(page.contains("They disagree"));
    assert!(page.contains("Always squash merge") && page.contains("Never squash merge"));
    assert!(page.contains("1 contradiction found)"));
}

#[tokio::test]
async fn topics_are_suppressed_and_released() {
    let (panel, engine) = setup();
    assert!(
        text(&get(&panel, "/suppressions").await).contains("No topics are currently suppressed.")
    );
    let added = text(&post(&panel, "/suppressions/add", "topic=Kubernetes").await);
    assert!(added.to_lowercase().contains("kubernetes"), "{added}");
    let listed = engine.list_suppressions().unwrap()["suppressed_topics"].to_string();
    assert!(listed.to_lowercase().contains("kubernetes"));
    assert!(text(&post(&panel, "/suppressions/add", "topic=++").await).contains("<table"));

    let removed = text(&post(&panel, "/suppressions/remove", "topic=kubernetes").await);
    assert!(
        removed.contains("No topics are currently suppressed."),
        "{removed}"
    );
}

#[tokio::test]
async fn telemetry_ranks_recalls_and_finds_what_went_cold() {
    let (panel, engine) = setup();
    let hot = remember(&engine, "Recalled often", "omnimem");
    let cold = remember(&engine, "Recalled long ago", "omnimem");
    remember(&engine, "Never recalled at all", "other");
    let now = omnimem_engine::pyfmt::now_str();
    engine
        .store()
        .bump_recall_counts(std::slice::from_ref(&hot), &now)
        .unwrap();
    engine
        .store()
        .bump_recall_counts(std::slice::from_ref(&hot), &now)
        .unwrap();
    engine
        .store()
        .bump_recall_counts(std::slice::from_ref(&cold), "1000.0")
        .unwrap();

    let page = text(&get(&panel, "/telemetry").await);
    assert!(
        page.contains(r#"<div class="stat-total">3</div>"#),
        "{page}"
    );
    let most = page.find("Most Recalled").unwrap();
    let gone = page.find("Gone Cold").unwrap();
    let never = page.find("Never Recalled <span").unwrap();
    assert!(page[most..gone].find("Recalled often") < page[most..gone].find("Recalled long ago"));
    assert!(page[gone..never].contains("Recalled long ago"));
    assert!(page[never..].contains("Never recalled at all"));
    assert!(page.contains("class=\"nav-link active\">Telemetry"));

    let filtered = text(&get(&panel, "/telemetry/refresh?project=other").await);
    assert!(!filtered.contains("<html"));
    assert!(filtered.contains("Never recalled at all"));
    assert!(!filtered.contains("Recalled often"));
}

#[tokio::test]
async fn token_overhead_measures_and_resets() {
    let (panel, engine) = setup();
    panel.set_static_overhead(StaticOverhead {
        instructions_chars: 14_162,
        tool_count: 50,
        tool_schemas_chars: 40_000,
        deferred_names_chars: 1_000,
    });
    remember(&engine, "Twelve chars", "omnimem");
    let metrics = "meta:tool_metrics:recall";
    engine.store().hash_incr(metrics, "call_count", 3).unwrap();
    engine
        .store()
        .hash_incr(metrics, "total_duration_ms", 100)
        .unwrap();
    engine
        .store()
        .hash_incr(metrics, "total_response_chars", 1000)
        .unwrap();

    let page = text(&get(&panel, "/token-overhead").await);
    assert!(page.contains("55,162"), "static characters: {page}");
    assert!(page.contains("~13,790"), "static tokens: {page}");
    assert!(page.contains("Tool schemas (50 tools)"));
    assert!(page.contains(r#"<td class="mono">recall</td>"#));
    assert!(page.contains("33.3ms"), "{page}");
    assert!(page.contains("~83"), "average tokens");
    assert!(page.contains("class=\"nav-link active\">Telemetry"));

    let reset = post(&panel, "/token-overhead/reset", "").await;
    assert_eq!(reset.headers()["x-omnimem-redirect"], "/token-overhead");
    let after = text(&get(&panel, "/token-overhead/refresh").await);
    assert!(
        after.contains("No tool calls recorded since the counters were reset"),
        "{after}"
    );
    assert!(engine.store().hash_get_all(metrics).unwrap().is_none());
}
