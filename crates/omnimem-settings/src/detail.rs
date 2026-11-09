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
use crate::pages::{back_of, blocking, is_web_url, memory_url, quote, see_other, starting};
use crate::render::page;

type FormData = HashMap<String, String>;

/// How many of a memory's changes its page lists.
const HISTORY_ROWS: usize = 25;

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

fn fmt_secs(ts: f64) -> String {
    fmt_ts(Some(&ts.to_string()))
}

/// A time as the local date a date field shows.
fn day(ts: f64) -> String {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
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
        "trust": omnimem_store::trust_of(&data),
        "trust_prior": text("trust_prior"),
        "superseded_by": text("superseded_by"),
        "supersedes": json_list(data.get("supersedes")),
        "reinstate_hints": json_list(data.get("reinstate_hints")),
        "deprioritised_reason": text("deprioritised_reason"),
        "archived_reason": text("archived_reason"),
        "source_url": text("source_url"),
        // Linked only when it is a web address; anything else is shown as text.
        "source_is_web": is_web_url(&text("source_url")),
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
        // #36: both intervals. The form's fields want a local date.
        "valid_from": omnimem_store::time_of(&data, "valid_from").map(fmt_secs),
        "valid_to": omnimem_store::valid_to_of(&data).map(fmt_secs),
        "retired_at": omnimem_store::retired_at_of(&data).map(fmt_secs),
        "valid_from_date": omnimem_store::time_of(&data, "valid_from").map(day).unwrap_or_default(),
        "valid_to_date": omnimem_store::time_of(&data, "valid_to").map(day).unwrap_or_default(),
        "expired": omnimem_store::valid_to_of(&data).is_some_and(|to| to <= chrono::Utc::now().timestamp() as f64),
    });
    // The memory's history, once the log is being kept. Rows from before it
    // was switched off still show, so the section stays while any exist.
    let engine = state.engine();
    let history_key = key.clone();
    let history = match engine {
        Some(engine) => match blocking(move || {
            let rows = engine.store().audit_entries(&omnimem_store::AuditQuery {
                key: Some(history_key),
                limit: HISTORY_ROWS,
                ..Default::default()
            })?;
            let on = engine.audit_level() != omnimem_engine::AuditLevel::Off;
            Ok((on || !rows.is_empty(), rows))
        })
        .await
        {
            Ok(found) => found,
            Err(failure) => return failure,
        },
        None => (false, Vec::new()),
    };
    let (history_on, history_rows) = history;
    let history: Vec<Value> = history_rows.iter().map(crate::audit_log::view).collect();
    let param = |name: &str| query.get(name).cloned().unwrap_or_default();
    let came_from = back_of(&query);
    let back = came_from
        .clone()
        .unwrap_or_else(|| default_back(&namespace).to_owned());
    let self_url = memory_url(&key, came_from.as_deref());
    page(
        state.templates(),
        "detail.html",
        context! {
            memory,
            history_on,
            history,
            trust_error => param("trust_error"),
            validity_error => param("validity_error"),
            tag_error => param("tag_error"),
            licence_error => param("licence_error"),
            licence_classes => LICENCE_CHOICES,
            provenance_error => param("provenance_error"),
            provenance_classes => PROVENANCE_CHOICES,
            current_page => section_of(&back),
            back_label => back_label(&back),
            back,
            self_url,
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
    back: Option<String>,
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
    let here = memory_url(&key, back.as_deref());
    match outcome {
        Ok(Outcome::Done) => see_other(&here),
        Ok(Outcome::Refused(message)) => see_other(&with_param(&here, error_param, &message)),
        Err(failure) => failure,
    }
}

/// Where a memory's page goes back to when nothing said where it came from.
fn default_back(namespace: &str) -> &'static str {
    match namespace {
        "preference" => "/memories?namespace=preference",
        "knowledge" => "/memories?namespace=knowledge",
        "project" => "/projects",
        "skill" => "/skills",
        _ => "/memories",
    }
}

/// The menu item a return path belongs to, so the menu still says where
/// the person is working while they look at one memory.
pub(crate) fn section_of(path: &str) -> &'static str {
    let route = path.split(['?', '#']).next().unwrap_or(path);
    let query = path.split_once('?').map_or("", |(_, q)| q);
    let has = |pair: &str| query.split('&').any(|p| p == pair);
    match route.trim_end_matches('/') {
        "" => "dashboard",
        "/search" => "search",
        "/unconfirmed" => "unconfirmed",
        "/contradictions" => "contradictions",
        "/duplicates" => "duplicates",
        "/experience" => "experience",
        "/experience/graveyard" => "graveyard",
        "/audit" => "audit",
        "/skills" => "skills",
        r if r.starts_with("/skills/") => "skills",
        r if r.starts_with("/projects") => "projects",
        "/memories" if has("source=rss") => "articles",
        "/memories" if has("source=learned") => "learned",
        "/memories" if has("namespace=preference") => "preferences",
        _ => "memories",
    }
}

/// What the Back link says: the page it returns to, in the menu's words.
pub(crate) fn back_label(path: &str) -> &'static str {
    match section_of(path) {
        "dashboard" => "Dashboard",
        "search" => "Search",
        "unconfirmed" => "Unconfirmed",
        "contradictions" => "Contradictions",
        "duplicates" => "Duplicates",
        "experience" => "Experience",
        "graveyard" => "Graveyard",
        "audit" => "Audit log",
        "skills" => "Skills",
        "projects" => "Projects",
        "articles" => "Articles",
        "learned" => "Learned knowledge",
        "preferences" => "Preferences",
        _ => "Memories",
    }
}

/// `url` with one more query parameter.
fn with_param(url: &str, name: &str, value: &str) -> String {
    let joiner = if url.contains('?') { '&' } else { '?' };
    format!("{url}{joiner}{name}={}", quote(value))
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
    run_form(
        state,
        key,
        back_of(&form),
        "tag_error",
        move |engine, key| {
            engine.retag(key, Some(tags), None, None)?;
            Ok(Outcome::Done)
        },
    )
    .await
}

pub(crate) async fn licence(
    State(state): State<PanelState>,
    Path(key): Path<String>,
    Form(form): Form<FormData>,
) -> Response {
    run_form(
        state,
        key,
        back_of(&form),
        "licence_error",
        move |engine, key| {
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
            let result =
                engine.set_licence(class, Some(&[key.to_owned()]), None, note.as_deref())?;
            Ok(refused(&result).map_or(Outcome::Done, Outcome::Refused))
        },
    )
    .await
}

pub(crate) async fn provenance(
    State(state): State<PanelState>,
    Path(key): Path<String>,
    Form(form): Form<FormData>,
) -> Response {
    run_form(
        state,
        key,
        back_of(&form),
        "provenance_error",
        move |engine, key| {
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
        },
    )
    .await
}

/// POST `/memory/{key}/validity`: a person sets or clears when the memory was
/// true (#36). An empty field clears that end.
pub(crate) async fn validity(
    State(state): State<PanelState>,
    Path(key): Path<String>,
    Form(form): Form<FormData>,
) -> Response {
    let back = back_of(&form);
    run_form(state, key, back, "validity_error", move |engine, key| {
        let current = engine.store().get(key)?.unwrap_or_default();
        let field = |name: &str| form.get(name).map(String::as_str).map(str::trim);
        // A date field shows the stored time as a local date; sent back
        // unchanged, the stored time stays rather than becoming midnight.
        let changed = |name: &str| {
            field(name).filter(|sent| {
                omnimem_store::time_of(&current, name).map(day).as_deref() != Some(*sent)
            })
        };
        let reason = field("reason").filter(|r| !r.is_empty());
        engine.set_validity(key, changed("valid_from"), changed("valid_to"), reason)?;
        Ok(Outcome::Done)
    })
    .await
}
