//! `/experience` and `/experience/graveyard`: effort, outcomes, breakthroughs
//! and abandoned approaches across episodic memories
//! (`web_ui/routes/experience.py`).

use std::collections::HashMap;
use std::fmt::Write as _;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use minijinja::context;
use omnimem_engine::Engine;
use omnimem_engine::pyfmt::round_to;
use omnimem_store::Fields;
use serde_json::{Value, json};

use crate::PanelState;
use crate::pages::{blocking, quote, starting};
use crate::render::page;

const EFFORTFUL_PAGE_SIZE: usize = 10;
const OUTCOMES: [&str; 3] = ["succeeded", "pivoted", "abandoned"];

/// `int(float(raw))`: a whole effort score, when there is a usable one.
fn effort(raw: Option<&String>) -> Option<i64> {
    raw.and_then(|r| r.trim().parse::<f64>().ok())
        .filter(|e| e.is_finite())
        .map(|e| e.trunc() as i64)
}

/// Episodic memories, optionally for one project.
fn episodic(engine: &Engine, project: &str) -> omnimem_engine::Result<Vec<(String, Fields)>> {
    let keys = engine.store().scan_prefix("mem:episodic:")?;
    let rows = engine.store().get_multi(&keys)?;
    Ok(keys
        .into_iter()
        .zip(rows)
        .filter_map(|(key, row)| row.map(|r| (key, r)))
        .filter(|(_, row)| {
            project.is_empty() || row.get("project").map(String::as_str) == Some(project)
        })
        .collect())
}

fn abandoned_approaches(row: &Fields) -> Vec<Value> {
    row.get("abandoned_approaches")
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| serde_json::from_str::<Vec<Value>>(raw).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(Value::is_object)
        .collect()
}

pub(crate) async fn summary(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let project = query.get("project").cloned().unwrap_or_default();
    let outcome_filter = query
        .get("outcome")
        .filter(|o| OUTCOMES.contains(&o.as_str()))
        .cloned()
        .unwrap_or_default();
    let requested_page = query
        .get("page")
        .and_then(|p| p.trim().parse::<i64>().ok())
        .unwrap_or(1)
        .max(1) as usize;

    let scan_project = project.clone();
    let rows = match blocking(move || episodic(&engine, &scan_project)).await {
        Ok(rows) => rows,
        Err(failure) => return failure,
    };

    let mut count = 0usize;
    let mut total_effort = 0i64;
    let mut outcomes: HashMap<&str, usize> = OUTCOMES.iter().map(|o| (*o, 0)).collect();
    let mut effortful: Vec<Value> = Vec::new();
    let mut breakthroughs: Vec<Value> = Vec::new();
    for (key, row) in &rows {
        let Some(effort) = effort(row.get("effort_score")) else {
            continue;
        };
        let outcome = row
            .get("outcome")
            .cloned()
            .unwrap_or_else(|| "unknown".to_owned());
        count += 1;
        total_effort += effort;
        if let Some(n) = outcomes.get_mut(outcome.as_str()) {
            *n += 1;
        }
        let content: String = row
            .get("content")
            .map(|c| c.chars().take(100).collect())
            .unwrap_or_default();
        effortful.push(json!({
            "key": key, "content": content, "effort_score": effort, "outcome": outcome,
        }));
        if let Some(breakthrough) = row.get("breakthrough").filter(|b| !b.is_empty()) {
            breakthroughs.push(json!({
                "key": key, "content": content, "effort_score": effort,
                "outcome": outcome, "breakthrough": breakthrough,
            }));
        }
    }
    let by_effort =
        |a: &Value, b: &Value| b["effort_score"].as_i64().cmp(&a["effort_score"].as_i64());
    effortful.sort_by(by_effort);
    breakthroughs.sort_by(by_effort);
    breakthroughs.truncate(5);

    let avg_effort = if count > 0 {
        json!(round_to(total_effort as f64 / count as f64, 2))
    } else {
        json!(0)
    };
    if !outcome_filter.is_empty() {
        effortful.retain(|m| m["outcome"] == outcome_filter.as_str());
    }
    let total_pages = effortful.len().div_ceil(EFFORTFUL_PAGE_SIZE).max(1);
    let current = requested_page.min(total_pages);
    let effortful: Vec<Value> = effortful
        .into_iter()
        .skip((current - 1) * EFFORTFUL_PAGE_SIZE)
        .take(EFFORTFUL_PAGE_SIZE)
        .collect();

    // Writing into a String cannot fail.
    let mut extra_params = String::new();
    if !outcome_filter.is_empty() {
        let _ = write!(extra_params, "&outcome={}", quote(&outcome_filter));
    }
    if !project.is_empty() {
        let _ = write!(extra_params, "&project={}", quote(&project));
    }
    let template = if headers.get("HX-Request").is_some_and(|v| v == "true") {
        "experience/_effortful.html"
    } else {
        "experience/summary.html"
    };
    page(
        state.templates(),
        template,
        context! {
            current_page => "experience",
            count,
            avg_effort,
            outcomes,
            effortful,
            outcome_filter,
            page => current,
            total_pages,
            base_url => "/experience",
            extra_params,
            breakthroughs,
            project,
        },
    )
}

pub(crate) async fn graveyard(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let project = query.get("project").cloned().unwrap_or_default();
    let scan_project = project.clone();
    let rows = match blocking(move || episodic(&engine, &scan_project)).await {
        Ok(rows) => rows,
        Err(failure) => return failure,
    };

    // One entry per approach name (case-insensitive), keeping the most
    // effortful memory that recorded it.
    let mut unique: Vec<(String, Value)> = Vec::new();
    for (key, row) in &rows {
        let effort = effort(row.get("effort_score").filter(|e| !e.is_empty()));
        for approach in abandoned_approaches(row) {
            let get = |field: &str, default: &str| {
                approach
                    .get(field)
                    .cloned()
                    .unwrap_or_else(|| json!(default))
            };
            let name = get("name", "?");
            let folded = name
                .as_str()
                .map_or_else(|| name.to_string(), str::to_owned)
                .to_lowercase();
            let item = json!({
                "name": name,
                "type": get("type", "?"),
                "reason": get("reason", ""),
                "attempted_at": get("attempted_at", ""),
                "effort_score": effort,
                "memory_key": key,
            });
            match unique.iter_mut().find(|(n, _)| *n == folded) {
                Some((_, kept)) => {
                    if effort.unwrap_or(0) > kept["effort_score"].as_i64().unwrap_or(0) {
                        *kept = item;
                    }
                }
                None => unique.push((folded, item)),
            }
        }
    }
    let mut abandoned: Vec<Value> = unique.into_iter().map(|(_, item)| item).collect();
    abandoned.sort_by(|a, b| {
        b["effort_score"]
            .as_i64()
            .unwrap_or(0)
            .cmp(&a["effort_score"].as_i64().unwrap_or(0))
    });
    page(
        state.templates(),
        "experience/graveyard.html",
        context! { current_page => "graveyard", abandoned, project },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_scores_truncate_like_int_of_float() {
        assert_eq!(effort(Some(&"4.9".to_owned())), Some(4));
        assert_eq!(effort(Some(&"3".to_owned())), Some(3));
        assert_eq!(effort(Some(&"lots".to_owned())), None);
        assert_eq!(effort(None), None);
    }
}
