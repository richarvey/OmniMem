//! Projects, experience and the graveyard through `Panel::handle`.

use std::sync::Arc;

use axum::http::{Request, Response};
use omnimem_core::{EmbeddingError, MemoryKey, Namespace, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_settings::Panel;
use omnimem_store::{Fields, Store};

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

async fn get(panel: &Panel, uri: &str) -> Response<Vec<u8>> {
    panel
        .handle(
            Request::get(format!("omnimem://localhost{uri}"))
                .body(Vec::new())
                .unwrap(),
        )
        .await
}

async fn htmx(panel: &Panel, uri: &str) -> String {
    let response = panel
        .handle(
            Request::get(format!("omnimem://localhost{uri}"))
                .header("HX-Request", "true")
                .body(Vec::new())
                .unwrap(),
        )
        .await;
    text(&response)
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

fn landed(response: &Response<Vec<u8>>) -> String {
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

fn tagged(engine: &Engine, content: &str, project: &str, tag: &str) -> String {
    let tags = vec![tag.to_owned()];
    engine
        .remember(
            content,
            Some(project),
            Some(&tags),
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

#[tokio::test]
async fn projects_are_created_listed_filtered_and_edited() {
    let (panel, engine) = setup();
    assert!(text(&get(&panel, "/projects/new").await).contains("New Project"));
    assert_eq!(
        landed(&post(&panel, "/projects/new", "name=++").await),
        "/projects/new"
    );
    assert_eq!(
        landed(
            &post(
                &panel,
                "/projects/new",
                "name=omnimem&description=Memory+for+agents&stack=Rust%2C+SQLite&domains=py%2C+docker&goals=ship+7.0"
            )
            .await
        ),
        "/projects/omnimem"
    );
    let key = "mem:project:omnimem";
    assert_eq!(
        field(&engine, key, "domains").as_deref(),
        Some("python,docker")
    );
    assert_eq!(field(&engine, key, "licence").as_deref(), Some("own"));
    assert_eq!(
        field(&engine, key, "provenance").as_deref(),
        Some("asserted")
    );
    let created = field(&engine, key, "created_at").unwrap();
    // Only project-namespace memories count towards a project here, as in 6.x.
    engine
        .remember(
            "A note about another project",
            Some("other"),
            None,
            "project",
            true,
            Some("raw"),
            None,
            None,
        )
        .unwrap();

    let list = text(&get(&panel, "/projects").await);
    assert!(
        list.contains(r#"<a href="/projects/omnimem" class="project-name">omnimem</a>"#),
        "{list}"
    );
    assert!(
        list.contains(r#"<span class="project-name">other</span>"#),
        "memories without a context aren't links"
    );
    assert!(list.contains(r#"<span class="memory-count-badge">1</span>"#));
    assert!(
        list.contains(r#"class="domain-chip">python <span class="domain-chip-count">1</span>"#),
        "{list}"
    );

    let python = text(&get(&panel, "/projects?domain=py").await);
    assert!(
        python.contains(r#"<span class="header-qualifier">in python</span>"#),
        "aliases resolve"
    );
    assert!(!python.contains(r#"<span class="project-name">other</span>"#));
    assert!(
        text(&get(&panel, "/projects?domain=golang").await)
            .contains("No project declares the domain")
    );
    assert!(text(&get(&panel, "/projects?domain=%21%21").await).contains("is not a usable domain"));

    let detail = text(&get(&panel, "/projects/omnimem").await);
    assert!(detail.contains("Memory for agents"), "{detail}");
    assert!(detail.contains(r#"<a href="/projects?domain=docker" class="domain-pill">docker</a>"#));
    assert_eq!(get(&panel, "/projects/nothing").await.status(), 404);

    let edit = text(&get(&panel, "/projects/omnimem/edit").await);
    assert!(edit.contains(r#"value="python,docker""#), "{edit}");
    assert!(edit.contains(r#"<option value="docker"></option>"#));
    assert_eq!(
        landed(
            &post(
                &panel,
                "/projects/omnimem/edit",
                "description=Changed&stack=Rust&domains=rust&goals=ship"
            )
            .await
        ),
        "/projects/omnimem"
    );
    assert_eq!(
        field(&engine, key, "description").as_deref(),
        Some("Changed")
    );
    assert_eq!(field(&engine, key, "domains").as_deref(), Some("rust"));
    assert_eq!(
        field(&engine, key, "created_at"),
        Some(created),
        "an edit keeps created_at"
    );
}

#[tokio::test]
async fn domain_suggestions_show_their_evidence() {
    let (panel, engine) = setup();
    landed(
        &post(
            &panel,
            "/projects/new",
            "name=omnimem&stack=Docker&goals=ship",
        )
        .await,
    );
    tagged(&engine, "first kubernetes memory", "omnimem", "kubernetes");
    tagged(&engine, "second kubernetes memory", "omnimem", "kubernetes");

    let suggestion = text(&post(&panel, "/projects/omnimem/domains/suggest", "").await);
    assert!(
        suggestion.contains(r#"<span class="domain-pill">docker</span>"#),
        "{suggestion}"
    );
    assert!(suggestion.contains("tagged on 2 memories"));
    assert!(
        suggestion.contains(r#"data-domains="docker,kubernetes""#),
        "{suggestion}"
    );
    assert_eq!(
        post(&panel, "/projects/missing/domains/suggest", "")
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn project_state_moves_in_bulk_and_delete_keeps_memories() {
    let (panel, engine) = setup();
    landed(&post(&panel, "/projects/new", "name=omnimem&goals=ship").await);
    let memory = tagged(&engine, "work on omnimem", "omnimem", "rust");

    assert_eq!(
        landed(&post(&panel, "/projects/omnimem/deprioritise", "").await),
        "/projects"
    );
    assert_eq!(
        field(&engine, "mem:project:omnimem", "state").as_deref(),
        Some("deprioritised")
    );
    assert_eq!(
        field(&engine, &memory, "state").as_deref(),
        Some("deprioritised")
    );
    assert!(text(&get(&panel, "/projects").await).contains("/projects/omnimem/reinstate"));

    landed(&post(&panel, "/projects/omnimem/reinstate", "").await);
    assert_eq!(field(&engine, &memory, "state").as_deref(), Some("active"));

    assert_eq!(
        landed(&post(&panel, "/projects/omnimem/delete", "").await),
        "/projects"
    );
    assert!(engine.store().get("mem:project:omnimem").unwrap().is_none());
    assert!(engine.store().get(&memory).unwrap().is_some());
}

fn experience(
    engine: &Engine,
    project: &str,
    effort: &str,
    outcome: &str,
    extra: &[(&str, &str)],
) -> String {
    let key = MemoryKey::generate("episodic".parse::<Namespace>().unwrap()).to_string();
    let mut fields = Fields::from([
        (
            "content".to_owned(),
            format!("{outcome} work at effort {effort}"),
        ),
        ("project".to_owned(), project.to_owned()),
        ("effort_score".to_owned(), effort.to_owned()),
        ("outcome".to_owned(), outcome.to_owned()),
        ("created_at".to_owned(), "1757000000.0".to_owned()),
        ("updated_at".to_owned(), "1757000000.0".to_owned()),
    ]);
    for (name, value) in extra {
        fields.insert((*name).to_owned(), (*value).to_owned());
    }
    engine.store().upsert(&key, &fields, None).unwrap();
    key
}

#[tokio::test]
async fn experience_summarises_and_the_graveyard_deduplicates() {
    let (panel, engine) = setup();
    assert!(text(&get(&panel, "/experience").await).contains("No experience data recorded yet."));

    experience(
        &engine,
        "omnimem",
        "5",
        "succeeded",
        &[("breakthrough", "Share the model")],
    );
    experience(
        &engine,
        "omnimem",
        "4",
        "pivoted",
        &[(
            "abandoned_approaches",
            r#"[{"name": "Valkey", "type": "library", "reason": "one binary"}]"#,
        )],
    );
    let heavier = experience(
        &engine,
        "other",
        "3.0",
        "abandoned",
        &[(
            "abandoned_approaches",
            r#"[{"name": "valkey", "type": "library", "reason": "again"}, "junk"]"#,
        )],
    );
    experience(&engine, "omnimem", "", "succeeded", &[]);

    let page = text(&get(&panel, "/experience").await);
    assert!(
        page.contains(r#"<div class="stat-total">3</div>"#),
        "{page}"
    );
    assert!(
        page.contains(r#"<div class="stat-total">4.0</div>"#),
        "{page}"
    );
    assert!(page.contains("1 pivoted"));
    assert!(page.contains("Share the model"));
    assert!(page.contains("class=\"nav-link active\">Experience"));

    let pivoted = htmx(&panel, "/experience?outcome=pivoted").await;
    assert!(pivoted.contains("pivoted work at effort 4"), "{pivoted}");
    assert!(!pivoted.contains("succeeded work"));
    assert!(!pivoted.contains("<html"));
    assert!(
        text(&get(&panel, "/experience?project=other").await)
            .contains(r#"<div class="stat-total">1</div>"#)
    );

    let graveyard = text(&get(&panel, "/experience/graveyard").await);
    assert_eq!(
        graveyard
            .matches(r#"style="font-weight:600;color:var(--orange)""#)
            .count(),
        1,
        "{graveyard}"
    );
    assert!(
        graveyard.contains("one binary"),
        "the effort 4 record outranks effort 3"
    );
    assert!(!graveyard.contains(&heavier));
    assert!(graveyard.contains("class=\"nav-link active\">Graveyard"));
}
