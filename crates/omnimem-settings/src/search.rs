//! `/search`: semantic search through the recall pipeline
//! (`web_ui/routes/search.py`).
//!
//! No relevance floor here, deliberately. The floor exists because recall
//! output is spent as an agent's context, where a plausible irrelevant result
//! sends it chasing a connection that isn't there. A person reading this page
//! pays neither cost, and an empty page for a memory they know is stored is a
//! worse answer than a weak match they can dismiss, so weak matches are shown
//! and marked (issue #30).

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Response};
use minijinja::context;
use omnimem_engine::pyfmt::round_to;
use serde_json::json;

use crate::PanelState;
use crate::pages::{blocking, starting};
use crate::render::page;

pub(crate) async fn form(State(state): State<PanelState>) -> Response {
    page(
        state.templates(),
        "search.html",
        context! {
            results => (),
            query => "",
            namespace => "",
            project => "",
            top_k => 10,
            current_page => "search",
        },
    )
}

pub(crate) async fn results(
    State(state): State<PanelState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let query = params
        .get("query")
        .map(|q| q.trim().to_owned())
        .unwrap_or_default();
    if query.is_empty() {
        return Html(r#"<p class="empty-state">Enter a search query.</p>"#).into_response();
    }
    let namespace = params.get("namespace").cloned().unwrap_or_default();
    let project = params.get("project").cloned().unwrap_or_default();
    let top_k = params
        .get("top_k")
        .and_then(|k| k.trim().parse::<i64>().ok())
        .unwrap_or(10)
        .clamp(1, 50);

    let worker_query = query.clone();
    let found = blocking(move || {
        let namespaces = (!namespace.is_empty()).then(|| vec![namespace]);
        let projects = if project.is_empty() {
            Vec::new()
        } else {
            vec![project]
        };
        let floor = engine.config().recall_min_score;
        let results = engine.recall_results(
            &worker_query,
            namespaces.as_deref(),
            Some(top_k),
            &projects,
            Some(0.0),
            None,
        )?;
        Ok(results
            .into_iter()
            .map(|r| {
                json!({
                    "key": r.key,
                    "namespace": r.namespace,
                    "weak_match": r.weak_match || r.score < floor,
                    "content": r.content.chars().take(300).collect::<String>(),
                    "score": round_to(r.adjusted_score, 4),
                    "state": r.state,
                    "project": r.project.unwrap_or_default(),
                    "result_type": r.result_type,
                    "tags": r.tags,
                    "reinstate_candidate": r.reinstate_candidate,
                    "effort_score": r.effort_score,
                    "outcome": r.outcome,
                    "breakthrough": r.breakthrough,
                })
            })
            .collect::<Vec<_>>())
    })
    .await;
    match found {
        Ok(results) => page(
            state.templates(),
            "search_results.html",
            context! { results, query },
        ),
        Err(failure) => failure,
    }
}
