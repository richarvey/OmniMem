//! Duplicates, contradictions and topic suppressions
//! (`web_ui/routes/duplicates.py`, `contradictions.py` and `suppressions.py`).

use std::collections::{BTreeSet, HashMap};

use axum::Form;
use axum::extract::{Query, State};
use axum::response::Response;
use chrono::{TimeZone, Utc};
use minijinja::context;
use omnimem_engine::{Engine, EngineError};
use serde_json::{Map, Value, json};
use tracing::warn;

use crate::PanelState;
use crate::pages::{blocking, starting};
use crate::render::page;

const NAMESPACES: [&str; 4] = ["episodic", "project", "knowledge", "preference"];
const MAINTENANCE_PREFIX: &str = "meta:maintenance:";

fn clip(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

/// The latest auto-maintenance run across all projects, for the note the
/// duplicates and contradictions pages show.
fn last_maintenance(engine: &Engine) -> omnimem_engine::Result<Option<Value>> {
    let mut latest: Option<(f64, Value)> = None;
    for key in engine.store().scan_prefix(MAINTENANCE_PREFIX)? {
        let Some(row) = engine.store().hash_get_all(&key)? else {
            continue;
        };
        let Some(at) = row
            .get("last_maintenance_at")
            .and_then(|t| t.trim().parse::<f64>().ok())
            .filter(|t| t.is_finite())
        else {
            continue;
        };
        if latest.as_ref().is_some_and(|(best, _)| at <= *best) {
            continue;
        }
        let summary: Value = row
            .get("last_maintenance_summary")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| json!({}));
        let when = Utc
            .timestamp_opt(at as i64, 0)
            .single()
            .map(|t| t.format("%d %b %Y %H:%M UTC").to_string())
            .unwrap_or_default();
        latest = Some((
            at,
            json!({
                "project": key.strip_prefix(MAINTENANCE_PREFIX).unwrap_or(&key),
                "timestamp": at,
                "when": when,
                "duplicates_archived": summary.get("duplicates_archived").cloned().unwrap_or_else(|| json!(0)),
                "contradictions_found": summary.get("contradictions_found").cloned().unwrap_or_else(|| json!(0)),
            }),
        ));
    }
    Ok(latest.map(|(_, note)| note))
}

pub(crate) async fn duplicates(State(state): State<PanelState>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    match blocking(move || last_maintenance(&engine)).await {
        Ok(last) => page(
            state.templates(),
            "duplicates.html",
            context! {
                current_page => "duplicates",
                clusters => (),
                namespace => "episodic",
                scanned => false,
                last_maintenance => last,
            },
        ),
        Err(failure) => failure,
    }
}

/// GET `/duplicates/scan`: the htmx scan, reusing stored vectors.
pub(crate) async fn duplicates_scan(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let namespace = query
        .get("namespace")
        .filter(|n| NAMESPACES.contains(&n.as_str()))
        .cloned()
        .unwrap_or_else(|| "episodic".to_owned());
    let project = query.get("project").cloned().unwrap_or_default();
    let scanned = namespace.clone();
    let found = blocking(
        move || match engine.find_duplicates(&scanned, None, Some(&project)) {
            Ok(result) => Ok(result["clusters"].clone()),
            Err(EngineError::Invalid(problem)) => {
                warn!(problem, "duplicate scan refused");
                Ok(json!([]))
            }
            Err(e) => Err(e),
        },
    )
    .await;
    match found {
        Ok(clusters) => page(
            state.templates(),
            "partials/dup_results.html",
            context! { clusters, namespace },
        ),
        Err(failure) => failure,
    }
}

/// Every recorded contradiction pair once, with both sides' content.
fn contradiction_pairs(engine: &Engine) -> omnimem_engine::Result<Vec<Value>> {
    let store = engine.store();
    let keys = store.scan_prefix("mem:episodic:")?;
    let rows = store.get_fields_multi(&keys, &["content", "contradictions"])?;
    let mut seen: BTreeSet<[String; 2]> = BTreeSet::new();
    let mut others: BTreeSet<String> = BTreeSet::new();
    let mut raw_pairs: Vec<(String, String, Map<String, Value>)> = Vec::new();
    for (key, row) in keys.iter().zip(rows) {
        let Some(row) = row else { continue };
        let Some(recorded) = row
            .get("contradictions")
            .filter(|r| !r.is_empty())
            .and_then(|r| serde_json::from_str::<Value>(r).ok())
        else {
            continue;
        };
        for entry in recorded.as_array().into_iter().flatten() {
            let Some(entry) = entry.as_object() else {
                continue;
            };
            let other = entry
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let mut pair = [key.clone(), other.clone()];
            pair.sort();
            if !seen.insert(pair) {
                continue;
            }
            if !other.is_empty() {
                others.insert(other);
            }
            raw_pairs.push((
                key.clone(),
                row.get("content").cloned().unwrap_or_default(),
                entry.clone(),
            ));
        }
    }
    // One batched read for every other side.
    let other_keys: Vec<String> = others.into_iter().collect();
    let other_content: HashMap<String, String> = other_keys
        .iter()
        .cloned()
        .zip(store.get_fields_multi(&other_keys, &["content"])?)
        .map(|(key, row)| {
            (
                key,
                row.and_then(|r| r.get("content").cloned())
                    .unwrap_or_default(),
            )
        })
        .collect();
    Ok(raw_pairs
        .into_iter()
        .map(|(key, content_a, entry)| {
            let other = entry.get("key").and_then(Value::as_str).unwrap_or("");
            let content_b = other_content
                .get(other)
                .filter(|c| !c.is_empty())
                .cloned()
                .unwrap_or_else(|| {
                    entry
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                });
            json!({
                "key_a": key,
                "content_a": clip(&content_a, 150),
                "key_b": other,
                "content_b": clip(&content_b, 150),
                "explanation": entry.get("explanation").cloned().unwrap_or_else(|| json!("")),
                "similarity": entry.get("similarity").cloned().unwrap_or(Value::Null),
            })
        })
        .collect())
}

pub(crate) async fn contradictions(State(state): State<PanelState>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let found =
        blocking(move || Ok((contradiction_pairs(&engine)?, last_maintenance(&engine)?))).await;
    match found {
        Ok((pairs, last)) => page(
            state.templates(),
            "contradictions.html",
            context! {
                current_page => "contradictions",
                pairs,
                last_maintenance => last,
            },
        ),
        Err(failure) => failure,
    }
}

/// Apply a change to the suppression list, then show the whole page (the
/// forms swap the body).
async fn suppressions_page(
    state: PanelState,
    change: impl FnOnce(&Engine) -> omnimem_engine::Result<()> + Send + 'static,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let topics = blocking(move || {
        change(&engine)?;
        Ok(engine.list_suppressions()?["suppressed_topics"].clone())
    })
    .await;
    match topics {
        Ok(topics) => page(
            state.templates(),
            "suppressions.html",
            context! { current_page => "suppressions", topics },
        ),
        Err(failure) => failure,
    }
}

pub(crate) async fn suppressions(State(state): State<PanelState>) -> Response {
    suppressions_page(state, |_| Ok(())).await
}

fn topic(form: &HashMap<String, String>) -> String {
    form.get("topic").map_or("", |t| t.trim()).to_owned()
}

pub(crate) async fn suppress(
    State(state): State<PanelState>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let topic = topic(&form);
    suppressions_page(state, move |engine| {
        if topic.is_empty() {
            return Ok(());
        }
        match engine.suppress_topic(&topic, None) {
            Err(EngineError::Invalid(problem)) => {
                warn!(problem, "topic not suppressed");
                Ok(())
            }
            other => other.map(|_| ()),
        }
    })
    .await
}

pub(crate) async fn unsuppress(
    State(state): State<PanelState>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let topic = topic(&form);
    suppressions_page(state, move |engine| {
        if !topic.is_empty() {
            engine.unsuppress_topic(&topic)?;
        }
        Ok(())
    })
    .await
}
