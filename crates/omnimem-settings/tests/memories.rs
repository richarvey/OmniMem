//! Memories, detail, lifecycle, create and search through `Panel::handle`.

use std::sync::Arc;

use axum::http::{Request, Response};
use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_settings::Panel;
use omnimem_store::Store;

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

fn remember(engine: &Engine, content: &str, namespace: &str, project: Option<&str>) -> String {
    engine
        .remember(
            content,
            project,
            None,
            namespace,
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

fn location(response: &Response<Vec<u8>>) -> String {
    assert_eq!(response.status(), 200, "{}", text(response));
    response.headers()["x-omnimem-redirect"]
        .to_str()
        .unwrap()
        .to_owned()
}

fn field(engine: &Engine, key: &str, name: &str) -> Option<String> {
    engine
        .store()
        .get(key)
        .unwrap()
        .and_then(|f| f.get(name).cloned())
}

#[tokio::test]
async fn the_list_filters_pages_and_answers_htmx_with_rows() {
    let (panel, engine) = setup();
    for i in 0..30 {
        remember(
            &engine,
            &format!("episodic note number {i}"),
            "episodic",
            Some("omnimem"),
        );
    }
    remember(
        &engine,
        "An article nobody has classified",
        "knowledge",
        None,
    );

    let all = text(&get(&panel, "/memories").await);
    assert!(
        all.contains(r#"<span class="badge" style="font-size:0.75rem">31</span>"#),
        "{all}"
    );
    assert!(all.contains("page=2"), "31 memories need a second page");
    assert!(all.contains("class=\"nav-link active\">Memories"));
    assert!(all.contains(r#"<option value="omnimem">omnimem</option>"#));

    let queue = text(&get(&panel, "/memories?namespace=knowledge&licence=unknown").await);
    assert!(
        queue.contains("An article nobody has classified"),
        "{queue}"
    );
    assert!(queue.contains("unclassified"));
    assert!(!queue.contains("episodic note number"));
    assert!(queue.contains("class=\"nav-link active\">Articles"));
    assert!(
        queue.contains("&amp;namespace=knowledge&amp;licence=unknown") || !queue.contains("page=2"),
        "pagination keeps the filters"
    );

    let rows = panel
        .handle(
            Request::get("omnimem://localhost/memories?page=2")
                .header("HX-Request", "true")
                .body(Vec::new())
                .unwrap(),
        )
        .await;
    let rows = text(&rows);
    assert!(
        rows.starts_with("<table") || rows.trim_start().starts_with("<table"),
        "{rows}"
    );
    assert!(!rows.contains("<html"));
    assert!(rows.contains(r#"name="next" value="/memories?page=2""#));
}

#[tokio::test]
async fn the_detail_page_edits_tags_licence_and_provenance() {
    let (panel, engine) = setup();
    let key = remember(&engine, "A memory to classify", "episodic", None);

    let page = text(&get(&panel, &format!("/memory/{key}")).await);
    assert!(page.contains("A memory to classify"), "{page}");
    assert!(page.contains("Own work"));
    assert!(page.contains("Concluded (system reasoning)"));
    assert_eq!(
        get(&panel, "/memory/mem:episodic:missing").await.status(),
        404
    );

    let back = format!("/memory/{key}");
    assert_eq!(
        location(
            &post(
                &panel,
                &format!("/memory/{key}/tags"),
                "tags=rust%2C+panel%2C+"
            )
            .await
        ),
        back
    );
    assert_eq!(
        field(&engine, &key, "tags").as_deref(),
        Some(r#"["rust", "panel"]"#)
    );

    let too_many: Vec<String> = (0..21).map(|i| format!("t{i}")).collect();
    let refused = location(
        &post(
            &panel,
            &format!("/memory/{key}/tags"),
            &format!("tags={}", too_many.join("%2C")),
        )
        .await,
    );
    assert!(
        refused.starts_with(&format!("{back}?tag_error=")),
        "{refused}"
    );
    let shown = text(&get(&panel, &refused).await);
    assert!(shown.contains(r#"<p role="alert""#), "{shown}");

    let licence = format!("/memory/{key}/licence");
    assert_eq!(
        location(&post(&panel, &licence, "licence=open&licence_note=CC+BY+4.0").await),
        back
    );
    assert_eq!(field(&engine, &key, "licence").as_deref(), Some("open"));
    assert_eq!(
        field(&engine, &key, "licence_note").as_deref(),
        Some("CC BY 4.0")
    );
    // Switching class with the pre-filled note left alone drops the note.
    location(
        &post(
            &panel,
            &licence,
            "licence=restricted&licence_note=CC+BY+4.0",
        )
        .await,
    );
    assert_eq!(
        field(&engine, &key, "licence").as_deref(),
        Some("restricted")
    );
    assert_eq!(field(&engine, &key, "licence_note").as_deref(), Some(""));
    let bad = location(&post(&panel, &licence, "licence=whatever&licence_note=").await);
    assert!(bad.starts_with(&format!("{back}?licence_error=")), "{bad}");

    location(
        &post(
            &panel,
            &format!("/memory/{key}/provenance"),
            "provenance=asserted",
        )
        .await,
    );
    assert_eq!(
        field(&engine, &key, "provenance").as_deref(),
        Some("asserted")
    );
    let bad = location(
        &post(
            &panel,
            &format!("/memory/{key}/provenance"),
            "provenance=hearsay",
        )
        .await,
    );
    assert!(bad.contains("provenance_error="), "{bad}");
}

#[tokio::test]
async fn lifecycle_actions_move_state_and_land_where_asked() {
    let (panel, engine) = setup();
    let key = remember(&engine, "A memory with a life", "episodic", None);
    let form = |extra: &str| format!("key={key}{extra}");

    assert_eq!(
        location(
            &post(
                &panel,
                "/lifecycle/deprioritise",
                &form("&next=%2Fmemories%3Fpage%3D1")
            )
            .await
        ),
        "/memories?page=1"
    );
    assert_eq!(
        field(&engine, &key, "state").as_deref(),
        Some("deprioritised")
    );
    assert_eq!(
        field(&engine, &key, "deprioritised_reason").as_deref(),
        Some("Deprioritised via web UI")
    );

    assert_eq!(
        location(
            &post(
                &panel,
                "/lifecycle/reinstate",
                &form("&next=%2F%2Fevil.example")
            )
            .await
        ),
        format!("/memory/{key}"),
        "an off-site next is ignored"
    );
    assert_eq!(field(&engine, &key, "state").as_deref(), Some("active"));
    assert_eq!(
        field(&engine, &key, "deprioritised_reason").as_deref(),
        Some("")
    );

    location(&post(&panel, "/lifecycle/archive", &form("")).await);
    assert_eq!(field(&engine, &key, "state").as_deref(), Some("archived"));

    assert_eq!(
        location(&post(&panel, "/lifecycle/delete", &form("")).await),
        "/memories"
    );
    assert!(engine.store().get(&key).unwrap().is_none());
}

#[tokio::test]
async fn create_stores_or_shows_the_duplicate() {
    let (panel, engine) = setup();
    assert!(text(&get(&panel, "/create").await).contains("class=\"nav-link active\">Create"));
    assert!(
        text(&post(&panel, "/create", "content=++").await).contains("Content cannot be empty.")
    );
    assert!(
        text(&post(&panel, "/create", "content=hello&licence=nonsense").await)
            .contains("flash-error")
    );

    let stored = location(
        &post(
            &panel,
            "/create",
            "content=Hello+panel&namespace=preference&project=omnimem&tags=a%2C+b",
        )
        .await,
    );
    let key = stored.strip_prefix("/memory/").unwrap();
    assert!(key.starts_with("mem:preference:"), "{key}");
    assert_eq!(
        field(&engine, key, "provenance").as_deref(),
        Some("asserted")
    );
    assert_eq!(field(&engine, key, "licence").as_deref(), Some("own"));
    assert_eq!(
        field(&engine, key, "tags").as_deref(),
        Some(r#"["a", "b"]"#)
    );
    assert_eq!(field(&engine, key, "project").as_deref(), Some("omnimem"));

    let again = text(
        &post(
            &panel,
            "/create",
            "content=Hello+again&namespace=preference",
        )
        .await,
    );
    assert!(
        again.contains("Near-duplicate found (1.0 similarity)"),
        "{again}"
    );
    assert!(again.contains(key));
    let forced = location(
        &post(
            &panel,
            "/create",
            "content=Hello+again&namespace=preference&force=on",
        )
        .await,
    );
    assert_ne!(forced, stored);
}

#[tokio::test]
async fn search_shows_results_with_weak_matches_marked() {
    let (panel, engine) = setup();
    remember(&engine, "Searchable memory", "episodic", None);
    assert!(text(&get(&panel, "/search").await).contains("Semantic Search"));
    assert!(text(&get(&panel, "/search/results?query=+").await).contains("Enter a search query."));
    let found = text(&get(&panel, "/search/results?query=searchable+memor&top_k=5").await);
    assert!(found.contains("Searchable memory"), "{found}");
    assert!(
        found.contains(r#"1 result for "searchable memor""#),
        "{found}"
    );
}
