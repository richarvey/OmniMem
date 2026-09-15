//! The panel through `Panel::handle`, as the desktop window calls it.

use std::sync::Arc;

use axum::http::{Request, Response};
use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_settings::Panel;
use omnimem_store::Store;

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

async fn get(panel: &Panel, uri: &str) -> Response<Vec<u8>> {
    panel
        .handle(
            Request::get(format!("omnimem://localhost{uri}"))
                .body(Vec::new())
                .unwrap(),
        )
        .await
}

fn text(response: &Response<Vec<u8>>) -> String {
    String::from_utf8_lossy(response.body()).into_owned()
}

fn engine() -> Arc<Engine> {
    Arc::new(Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Flat),
        EngineConfig::default(),
    ))
}

#[tokio::test]
async fn pages_wait_for_the_engine_then_show_the_dashboard() {
    let panel = Panel::new();
    let starting = get(&panel, "/").await;
    assert_eq!(starting.status(), 200);
    assert!(text(&starting).contains("opening its store"));

    panel.set_failure("could not bind 127.0.0.1:8765".into());
    assert!(text(&get(&panel, "/").await).contains("could not bind 127.0.0.1:8765"));

    let engine = engine();
    let tags = vec!["rust".to_owned()];
    engine
        .remember(
            "The panel renders the dashboard in-process",
            Some("omnimem"),
            Some(&tags),
            "episodic",
            true,
            Some("raw"),
            None,
            None,
        )
        .unwrap();
    engine
        .set_project_context(
            "omnimem",
            "Memory for agents",
            "rust",
            "ship 7.0",
            "porting",
            None,
            None,
        )
        .unwrap();
    panel.set_engine(engine);

    let dashboard = get(&panel, "/?refresh=1").await;
    let html = text(&dashboard);
    assert_eq!(dashboard.status(), 200, "{html}");
    assert!(html.contains("memories across all namespaces"), "{html}");
    assert!(html.contains("The panel renders the dashboard in-process"));
    assert!(html.contains("<title>Dashboard — OmniMem</title>"));
    assert!(
        html.contains("class=\"nav-link active\">Dashboard"),
        "the sidebar marks the page"
    );
    assert!(
        html.contains("width: 50"),
        "two memories split the composition bar evenly: {html}"
    );
}

#[tokio::test]
async fn static_files_are_embedded_and_nothing_else_is_served() {
    let panel = Panel::new();
    let css = get(&panel, "/static/style.css").await;
    assert_eq!(css.status(), 200);
    assert_eq!(css.headers()["content-type"], "text/css; charset=utf-8");
    assert!(css.body().len() > 40_000);
    assert_eq!(
        get(&panel, "/static/fonts/ubuntu-400-latin.woff2")
            .await
            .headers()["content-type"],
        "font/woff2"
    );
    assert_eq!(get(&panel, "/static/htmx.min.js").await.status(), 200);
    assert_eq!(get(&panel, "/static/../Cargo.toml").await.status(), 404);
}

#[tokio::test]
async fn unported_pages_say_so_and_post_bodies_arrive() {
    let panel = Panel::new();
    let pending = get(&panel, "/metrics?format=prometheus").await;
    assert_eq!(pending.status(), 200);
    assert!(text(&pending).contains("<code>/metrics</code> isn't in the panel yet"));

    let echo = panel
        .handle(
            Request::post("omnimem://localhost/_panel/echo")
                .body(b"panel-post-ok".to_vec())
                .unwrap(),
        )
        .await;
    assert_eq!(text(&echo), "panel-post-ok");

    let redirect = panel
        .handle(
            Request::post("omnimem://localhost/_panel/redirect")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(b"echo=panel-post-ok&css=42265".to_vec())
                .unwrap(),
        )
        .await;
    assert_eq!(redirect.status(), 200);
    assert_eq!(
        redirect.headers()["x-omnimem-redirect"],
        "/_panel/landed?echo=panel-post-ok&css=42265"
    );
    assert!(text(&redirect).contains(
        r#"<script>location.replace("/_panel/landed?echo=panel-post-ok&css=42265")</script>"#
    ));
    let landed = get(&panel, "/_panel/landed?echo=panel-post-ok").await;
    assert!(text(&landed).contains("window.ipc.postMessage"));
}
