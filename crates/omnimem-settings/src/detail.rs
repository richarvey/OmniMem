//! `/memory/{key}`: one memory in full, with its tag, licence and provenance
//! forms (`web_ui/routes/detail.py`). The forms go through the same engine
//! calls as the `retag`, `set_licence` and `set_provenance` tools, so facts
//! extracted from a memory follow its classification and `updated_at` is left
//! alone.

use std::collections::HashMap;

use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{Local, TimeZone};
use minijinja::context;
use omnimem_engine::EngineError;
use omnimem_engine::classification::{
    is_classifiable_key, note_for_reclassification, resolve_licence, resolve_provenance,
    validate_licence_note,
};
use serde_json::{Value, json};

use crate::PanelState;
use crate::choices::{LICENCE_CHOICES, PROVENANCE_CHOICES, licence_label, provenance_label};
use crate::pages::{blocking, quote, see_other, starting};
use crate::render::page;

type FormData = HashMap<String, String>;

fn json_list(raw: Option<&String>) -> Value {
    raw.filter(|r| !r.is_empty())
        .and_then(|r| serde_json::from_str::<Value>(r).ok())
        .unwrap_or_else(|| json!([]))
}

fn fmt_ts(raw: Option<&String>) -> String {
    raw.and_then(|r| r.trim().parse::<f64>().ok())
        .and_then(|ts| Local.timestamp_opt(ts as i64, 0).single())
        .map_or_else(
            || "—".to_owned(),
            |t| t.format("%Y-%m-%d %H:%M:%S").to_string(),
        )
}

pub(crate) async fn handler(
    State(state): State<PanelState>,
    Path(key): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let lookup = key.clone();
    let data = match blocking(move || Ok(engine.store().get(&lookup)?)).await {
        Ok(data) => data,
        Err(failure) => return failure,
    };
    let Some(data) = data else {
        let mut response = page(
            state.templates(),
            "not_found.html",
            context! { current_page => "memories", message => "Memory not found." },
        );
        *response.status_mut() = StatusCode::NOT_FOUND;
        return response;
    };

    let text = |field: &str| data.get(field).cloned().unwrap_or_default();
    let present = |field: &str| data.get(field).cloned();
    let namespace = key.split(':').nth(1).unwrap_or("unknown").to_owned();
    let licence = text("licence");
    let provenance = text("provenance");
    let memory = json!({
        "key": key,
        "namespace": namespace,
        "content": text("content"),
        "state": data.get("state").cloned().unwrap_or_else(|| "active".to_owned()),
        "project": data.get("project").filter(|p| !p.is_empty()).or_else(|| data.get("project_name")).cloned().unwrap_or_default(),
        "tags": json_list(data.get("tags")),
        "surface_score": data.get("surface_score").cloned().unwrap_or_else(|| "1.0".to_owned()),
        "experience_weight": data.get("experience_weight").cloned().unwrap_or_else(|| "1.0".to_owned()),
        "effort_score": present("effort_score"),
        "outcome": present("outcome"),
        "iterations": present("iterations"),
        "breakthrough": present("breakthrough"),
        "lesson": present("lesson"),
        "gotchas": present("gotchas"),
        "abandoned_approaches": json_list(data.get("abandoned_approaches")),
        "contradictions": json_list(data.get("contradictions")),
        "reinstate_hints": json_list(data.get("reinstate_hints")),
        "deprioritised_reason": text("deprioritised_reason"),
        "source_url": text("source_url"),
        "feed_name": text("feed_name"),
        "licence_label": licence_label(&licence),
        "licence": licence,
        "licence_note": text("licence_note"),
        "provenance_label": provenance_label(&provenance),
        "provenance": provenance,
        "recall_count": data.get("recall_count").and_then(|c| c.trim().parse::<i64>().ok()).unwrap_or(0),
        "last_recalled": match data.get("last_recalled").filter(|l| !l.is_empty()) {
            Some(raw) => fmt_ts(Some(raw)),
            None => "Never".to_owned(),
        },
        "created_at": fmt_ts(data.get("created_at")),
        "updated_at": fmt_ts(data.get("updated_at")),
    });
    let param = |name: &str| query.get(name).cloned().unwrap_or_default();
    page(
        state.templates(),
        "detail.html",
        context! {
            memory,
            tag_error => param("tag_error"),
            licence_error => param("licence_error"),
            licence_classes => LICENCE_CHOICES,
            provenance_error => param("provenance_error"),
            provenance_classes => PROVENANCE_CHOICES,
            current_page => "memories",
        },
    )
}

/// Where a detail form lands: the memory, with the problem when there was one.
enum Outcome {
    Done,
    Refused(String),
}

async fn run_form(
    state: PanelState,
    key: String,
    error_param: &'static str,
    work: impl FnOnce(&omnimem_engine::Engine, &str) -> omnimem_engine::Result<Outcome> + Send + 'static,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let worker_key = key.clone();
    let outcome = blocking(move || match work(&engine, &worker_key) {
        Err(EngineError::Invalid(message)) => Ok(Outcome::Refused(message)),
        other => other,
    })
    .await;
    match outcome {
        Ok(Outcome::Done) => see_other(&format!("/memory/{key}")),
        Ok(Outcome::Refused(message)) => {
            see_other(&format!("/memory/{key}?{error_param}={}", quote(&message)))
        }
        Err(failure) => failure,
    }
}

/// A tool result carrying `error` is a refusal, as a `ValueError` was.
fn refused(result: &Value) -> Option<String> {
    result["error"].as_str().map(str::to_owned)
}

pub(crate) async fn retag(
    State(state): State<PanelState>,
    Path(key): Path<String>,
    Form(form): Form<FormData>,
) -> Response {
    let tags: Vec<String> = form
        .get("tags")
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    run_form(state, key, "tag_error", move |engine, key| {
        engine.retag(key, Some(tags), None, None)?;
        Ok(Outcome::Done)
    })
    .await
}

pub(crate) async fn licence(
    State(state): State<PanelState>,
    Path(key): Path<String>,
    Form(form): Form<FormData>,
) -> Response {
    run_form(state, key, "licence_error", move |engine, key| {
        // created_at proves the record exists: every writer sets it, and an
        // unclassified record must still be classifiable here.
        if !is_classifiable_key(key) {
            return Ok(Outcome::Done);
        }
        let Some(current) = engine
            .store()
            .get_fields_multi(
                &[key.to_owned()],
                &["created_at", "licence", "licence_note"],
            )?
            .pop()
            .flatten()
        else {
            return Ok(Outcome::Done);
        };
        let field = |name: &str| form.get(name).map_or("", String::as_str);
        let (class, derived) = resolve_licence(field("licence"))?;
        let submitted = validate_licence_note(Some(field("licence_note")))?;
        // The form pre-fills the current note: switching class keeps it only
        // when the human changed it.
        let note = note_for_reclassification(
            current.get("licence").map_or("", String::as_str),
            current.get("licence_note").map(String::as_str),
            class,
            submitted,
        )
        .or_else(|| derived.map(str::to_owned));
        let result = engine.set_licence(class, Some(&[key.to_owned()]), None, note.as_deref())?;
        Ok(refused(&result).map_or(Outcome::Done, Outcome::Refused))
    })
    .await
}

pub(crate) async fn provenance(
    State(state): State<PanelState>,
    Path(key): Path<String>,
    Form(form): Form<FormData>,
) -> Response {
    run_form(state, key, "provenance_error", move |engine, key| {
        if !is_classifiable_key(key)
            || engine
                .store()
                .get_fields_multi(&[key.to_owned()], &["created_at"])?
                .pop()
                .flatten()
                .is_none()
        {
            return Ok(Outcome::Done);
        }
        let class = resolve_provenance(form.get("provenance").map_or("", String::as_str))?;
        let result = engine.set_provenance(class, &[key.to_owned()])?;
        Ok(refused(&result).map_or(Outcome::Done, Outcome::Refused))
    })
    .await
}
