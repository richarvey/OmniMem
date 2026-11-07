//! `/version-check`: a marker in the sidebar when a newer stable release is
//! out (`web_ui/routes/version_check.py`). The services' background check
//! files the latest stable release in the store (`omnimem_app::run_update_checks`),
//! so this only reads it back: the panel itself never goes to the network. A
//! development build, whose version isn't plain numbers, never shows the
//! marker, and `UPDATE_CHECK=false` turns it off.

use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use minijinja::context;
use omnimem_engine::release::newer_release;

use crate::PanelState;
use crate::render::page;

pub(crate) async fn check(State(state): State<PanelState>) -> Response {
    let Some(engine) = state.engine() else {
        return Html("").into_response();
    };
    let store = engine.store().clone();
    // A kv read, but SQLite all the same, so off the async workers.
    let latest =
        tokio::task::spawn_blocking(move || newer_release(&store, env!("CARGO_PKG_VERSION")))
            .await
            .ok()
            .flatten();
    match latest {
        Some(latest) => page(
            state.templates(),
            "partials/version_update.html",
            context! {
                release_url => omnimem_engine::release::release_url(&latest),
                latest_version => latest,
            },
        ),
        None => Html("").into_response(),
    }
}
