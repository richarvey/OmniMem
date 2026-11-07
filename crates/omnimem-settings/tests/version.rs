//! `/version-check` through `Panel::handle`: the marker appears only for a
//! stored release newer than this build, and `UPDATE_CHECK=false` hides it.

use std::sync::Arc;

use axum::http::Request;
use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::release::{parse_version, record_latest_release};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_settings::Panel;
use omnimem_store::Store;

struct Flat;

impl TextEmbedder for Flat {
    fn dimension(&self) -> usize {
        VECTOR_DIM
    }
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|_| vec![0.0f32; VECTOR_DIM]).collect())
    }
}

async fn check(panel: &Panel) -> String {
    let response = panel
        .handle(
            Request::get("omnimem://localhost/version-check")
                .body(Vec::new())
                .unwrap(),
        )
        .await;
    String::from_utf8_lossy(response.body()).into_owned()
}

/// A development build (`7.0.7-dev`) is never told about a release, so
/// between releases the marker stays hidden even for a newer one.
fn release_build() -> bool {
    parse_version(env!("CARGO_PKG_VERSION")).is_some()
}

/// The marker for v999.1.2, or nothing on a development build.
fn assert_marker(html: &str) {
    if release_build() {
        assert!(html.contains("v999.1.2 available"), "{html}");
        assert!(html.contains("class=\"version-update\""), "{html}");
        assert!(html.contains("/releases/tag/v999.1.2"), "{html}");
    } else {
        assert_eq!(html.trim(), "", "a development build shows no marker");
    }
}

// One test: it changes UPDATE_CHECK, which the whole process shares.
#[tokio::test]
async fn the_marker_shows_only_for_a_newer_release() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(Engine::new(
        store.clone(),
        Arc::new(Flat),
        EngineConfig::default(),
    ));
    let panel = Panel::new();

    // No engine yet, then nothing recorded.
    assert_eq!(check(&panel).await.trim(), "");
    panel.set_engine(engine);
    assert_eq!(check(&panel).await.trim(), "");

    record_latest_release(&store, "999.1.2").unwrap();
    assert_marker(&check(&panel).await);

    record_latest_release(&store, env!("CARGO_PKG_VERSION")).unwrap();
    assert_eq!(check(&panel).await.trim(), "");
    record_latest_release(&store, "0.0.1").unwrap();
    assert_eq!(check(&panel).await.trim(), "");

    store
        .string_set("meta:latest_release", "not json", None)
        .unwrap();
    assert_eq!(check(&panel).await.trim(), "");
    store
        .string_set("meta:latest_release", r#"{"version": "7.0.0-rc1"}"#, None)
        .unwrap();
    assert_eq!(check(&panel).await.trim(), "");

    record_latest_release(&store, "999.1.2").unwrap();
    unsafe { std::env::set_var("UPDATE_CHECK", "false") };
    assert_eq!(check(&panel).await.trim(), "");
    unsafe { std::env::remove_var("UPDATE_CHECK") };
    assert_marker(&check(&panel).await);
}
