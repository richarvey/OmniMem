//! The Unconfirmed page and a memory's trust buttons through `Panel::handle`
//! (#23).

use std::sync::Arc;

use axum::http::{Request, Response};
use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{AuditLevel, Engine, EngineConfig};
use omnimem_settings::Panel;
use omnimem_store::{Actor, AuditQuery, Fields, Store, enter};

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

const SRC_ONE: &str = "mem:knowledge:01SRCONE";
const SRC_TWO: &str = "mem:knowledge:01SRCTWO";

struct Fixture {
    panel: Panel,
    engine: Arc<Engine>,
}

fn setup() -> Fixture {
    let engine = Arc::new(Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Flat),
        EngineConfig {
            audit_log: AuditLevel::Lifecycle,
            ..EngineConfig::default()
        },
    ));
    let panel = Panel::new();
    panel.set_engine(engine.clone());
    Fixture { panel, engine }
}

fn write(engine: &Engine, key: &str, content: &str, from: Option<&str>) {
    let _who = enter(Actor::Mcp, "test");
    let mut fields: Fields = [
        ("content", content),
        ("state", "active"),
        ("created_at", "1.0"),
        ("updated_at", "1.0"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    if let Some(from) = from {
        fields.insert("enriched_from".to_owned(), from.to_owned());
    }
    engine.store().upsert(key, &fields, None).unwrap();
}

/// Two sources, each with candidates extracted from it.
fn seeded() -> Fixture {
    let f = setup();
    write(&f.engine, SRC_ONE, "first article", None);
    write(&f.engine, SRC_TWO, "second article", None);
    write(&f.engine, "mem:knowledge:01A1", "fact a1", Some(SRC_ONE));
    write(&f.engine, "mem:knowledge:01A2", "fact a2", Some(SRC_ONE));
    write(&f.engine, "mem:knowledge:01B1", "fact b1", Some(SRC_TWO));
    f
}

fn trust(f: &Fixture, key: &str) -> String {
    omnimem_store::trust_of(&f.engine.store().get(key).unwrap().unwrap()).to_owned()
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

#[tokio::test]
async fn the_list_shows_candidates_under_their_source() {
    let f = seeded();
    let page = text(&get(&f.panel, "/unconfirmed").await);
    assert!(page.contains("Unconfirmed"), "{page}");
    for needle in ["fact a1", "fact a2", "fact b1", SRC_ONE, SRC_TWO] {
        assert!(page.contains(needle), "missing {needle}: {page}");
    }
    assert!(page.contains("href=\"/admin/memory/mem:knowledge:01SRCONE\""));
    assert!(page.contains("Confirm all 2 from this source"));
    assert!(
        page.contains("/unconfirmed\" class=\"nav-link active"),
        "the sidebar link"
    );

    let one = text(&get(&f.panel, &format!("/unconfirmed?source={SRC_TWO}")).await);
    assert!(one.contains("fact b1") && !one.contains("fact a1"), "{one}");

    let badge = text(&get(&f.panel, "/unconfirmed/badge").await);
    assert!(
        badge.contains("nav-count") && badge.contains('3'),
        "{badge}"
    );
}

#[tokio::test]
async fn confirm_and_reject_from_the_list_are_the_panels_in_the_log() {
    let f = seeded();
    let done = post(
        &f.panel,
        "/unconfirmed/act",
        "key=mem:knowledge:01A1&action=confirm",
    )
    .await;
    assert_eq!(done.status(), 303);
    assert_eq!(trust(&f, "mem:knowledge:01A1"), "accepted");
    assert!(!text(&get(&f.panel, "/unconfirmed").await).contains("fact a1"));

    post(
        &f.panel,
        "/unconfirmed/act",
        "key=mem:knowledge:01A2&action=reject&reason=not+true",
    )
    .await;
    assert_eq!(trust(&f, "mem:knowledge:01A2"), "rejected");

    let rows = f
        .engine
        .store()
        .audit_entries(&AuditQuery {
            action: Some("trust".to_owned()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert!(rows.iter().all(|r| r.actor == "panel"), "{rows:#?}");
    assert!(rows.iter().any(|r| r.reason == "not true"), "{rows:#?}");
}

#[tokio::test]
async fn a_refusal_is_shown_not_hidden() {
    let f = seeded();
    post(
        &f.panel,
        "/unconfirmed/act",
        "key=mem:knowledge:01A1&action=confirm",
    )
    .await;
    let again = post(
        &f.panel,
        "/unconfirmed/act",
        "key=mem:knowledge:01A1&action=confirm",
    )
    .await;
    assert_eq!(again.status(), 303);
    assert!(location(&again).contains("error="), "{}", location(&again));
}

#[tokio::test]
async fn the_memory_page_confirms_rejects_and_restores() {
    let f = seeded();
    let key = "mem:knowledge:01A1";
    let page = text(&get(&f.panel, &format!("/memory/{key}")).await);
    assert!(page.contains("trust: candidate"), "{page}");
    assert!(page.contains("value=\"confirm\""));

    post(
        &f.panel,
        &format!("/memory/{key}/trust"),
        "action=reject&reason=wrong+year",
    )
    .await;
    assert_eq!(trust(&f, key), "rejected");
    let page = text(&get(&f.panel, &format!("/memory/{key}")).await);
    assert!(page.contains("trust: rejected"), "{page}");
    assert!(page.contains("value=\"restore\"") && !page.contains("value=\"reject\""));
    assert!(
        page.contains("wrong year"),
        "the History section shows the reason: {page}"
    );

    post(&f.panel, &format!("/memory/{key}/trust"), "action=restore").await;
    assert_eq!(trust(&f, key), "candidate");

    post(&f.panel, &format!("/memory/{key}/trust"), "action=confirm").await;
    assert_eq!(trust(&f, key), "accepted");
    let page = text(&get(&f.panel, &format!("/memory/{key}")).await);
    assert!(page.contains("trust: accepted") && !page.contains("value=\"confirm\""));

    // Confirming again is refused with the engine's words, on the page.
    let again = post(&f.panel, &format!("/memory/{key}/trust"), "action=confirm").await;
    assert!(
        location(&again).contains("trust_error="),
        "{}",
        location(&again)
    );
}

#[tokio::test]
async fn confirm_all_moves_one_sources_candidates_only() {
    let f = seeded();
    post(
        &f.panel,
        "/unconfirmed/source",
        &format!("source={SRC_ONE}"),
    )
    .await;
    assert_eq!(trust(&f, "mem:knowledge:01A1"), "accepted");
    assert_eq!(trust(&f, "mem:knowledge:01A2"), "accepted");
    assert_eq!(trust(&f, "mem:knowledge:01B1"), "candidate");
    let page = text(&get(&f.panel, "/unconfirmed").await);
    assert!(page.contains("fact b1") && !page.contains("fact a1"));
}

fn location(response: &Response<Vec<u8>>) -> String {
    response.headers()["location"].to_str().unwrap().to_owned()
}
