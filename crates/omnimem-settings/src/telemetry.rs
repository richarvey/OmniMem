//! `/telemetry` and `/token-overhead`: recall counters, what has gone cold,
//! and what OmniMem costs an agent in context (`web_ui/routes/telemetry.py`
//! and `token_overhead.py`).
//!
//! The static overhead is measured from the MCP server the app runs (6.x
//! hardcoded it), and the tool usage comes from the `meta:tool_metrics:*`
//! counters the server keeps per call.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::Response;
use minijinja::Value as Context;
use omnimem_engine::Engine;
use omnimem_engine::pyfmt::{now_str, round_to};
use serde_json::{Map, Value, json};

use crate::format::{minutes, number};
use crate::pages::{blocking, now_seconds, see_other, starting};
use crate::render::page;
use crate::{PanelState, StaticOverhead};

/// Skills are included: `get_skill` bumps the same counters recall does.
const TELEMETRY_PREFIXES: [&str; 5] = [
    "mem:episodic:",
    "mem:project:",
    "mem:knowledge:",
    "mem:preference:",
    "mem:skill:",
];
const MEMORY_NAMESPACES: [&str; 4] = ["episodic", "project", "knowledge", "preference"];
const METRICS_PREFIX: &str = "meta:tool_metrics:";
/// No trailing colon, so a scan of the counters never includes it.
const METRICS_RESET_KEY: &str = "meta:tool_metrics_reset";
const CHARS_PER_TOKEN: usize = 4;

fn cold_days() -> i64 {
    std::env::var("TELEMETRY_COLD_DAYS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(60)
}

/// A stored counter: a whole number, however it was written.
fn count(raw: Option<&String>) -> i64 {
    let raw = raw.map_or("", |r| r.trim());
    raw.parse::<i64>()
        .ok()
        .or_else(|| {
            raw.parse::<f64>()
                .ok()
                .filter(|f| f.is_finite())
                .map(|f| f as i64)
        })
        .unwrap_or(0)
}

fn project_of(row: &omnimem_store::Fields) -> String {
    row.get("project")
        .filter(|p| !p.is_empty())
        .or_else(|| row.get("project_name").filter(|p| !p.is_empty()))
        .cloned()
        .unwrap_or_default()
}

fn is_live(row: &omnimem_store::Fields) -> bool {
    !matches!(
        row.get("state").map(String::as_str),
        Some("archived" | "deleted")
    )
}

fn telemetry_data(engine: &Engine, project: &str) -> omnimem_engine::Result<Value> {
    let cold_days = cold_days();
    let cold_threshold = now_seconds() - (cold_days * 86_400) as f64;
    let (mut total_memories, mut total_recalls, mut unique_recalled, mut never_recalled) =
        (0usize, 0i64, 0usize, 0usize);
    let (mut most_recalled, mut gone_cold, mut never_list): (Vec<Value>, Vec<Value>, Vec<Value>) =
        (Vec::new(), Vec::new(), Vec::new());

    for prefix in TELEMETRY_PREFIXES {
        let namespace = prefix.split(':').nth(1).unwrap_or("");
        let keys = engine.store().scan_prefix(prefix)?;
        let rows = engine.store().get_fields_multi(
            &keys,
            &[
                "state",
                "project",
                "project_name",
                "recall_count",
                "last_recalled",
                "content",
                "created_at",
                "name",
                "description",
            ],
        )?;
        for (key, row) in keys.iter().zip(rows) {
            let Some(row) = row else { continue };
            let mem_project = project_of(&row);
            if !is_live(&row) || (!project.is_empty() && mem_project != project) {
                continue;
            }
            total_memories += 1;
            let recall_count = count(row.get("recall_count"));
            total_recalls += recall_count;
            let mut content = row.get("content").cloned().unwrap_or_default();
            if content.is_empty() && namespace == "skill" {
                // Skills carry no content: show their discovery metadata.
                content = ["name", "description"]
                    .iter()
                    .filter_map(|f| row.get(*f).filter(|v| !v.is_empty()))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" — ");
            }
            let mut snippet: String = content.chars().take(80).collect();
            if content.chars().count() > 80 {
                snippet.push_str("...");
            }
            let last_raw = row.get("last_recalled").filter(|l| !l.is_empty());
            let last_recalled_at = number(last_raw);
            let url = if namespace == "skill" {
                format!("/skills/{key}")
            } else {
                format!("/memory/{key}")
            };
            let entry = json!({
                "key": key,
                "namespace": namespace,
                "url": url,
                "content": snippet,
                "project": mem_project,
                "recall_count": recall_count,
                "last_recalled_raw": last_recalled_at,
                "last_recalled": last_raw.map_or_else(|| "Never".to_owned(), |raw| minutes(Some(raw))),
                "created_at_raw": number(row.get("created_at")),
            });
            if recall_count > 0 {
                unique_recalled += 1;
                if last_raw.is_some() && last_recalled_at < cold_threshold {
                    gone_cold.push(entry.clone());
                }
                most_recalled.push(entry);
            } else {
                never_recalled += 1;
                never_list.push(entry);
            }
        }
    }
    let field = |v: &Value, name: &str| v[name].as_f64().unwrap_or(0.0);
    most_recalled.sort_by(|a, b| field(b, "recall_count").total_cmp(&field(a, "recall_count")));
    gone_cold
        .sort_by(|a, b| field(a, "last_recalled_raw").total_cmp(&field(b, "last_recalled_raw")));
    never_list.sort_by(|a, b| field(a, "created_at_raw").total_cmp(&field(b, "created_at_raw")));
    most_recalled.truncate(15);
    gone_cold.truncate(15);
    never_list.truncate(20);
    Ok(json!({
        "total_memories": total_memories,
        "total_recalls": total_recalls,
        "unique_recalled": unique_recalled,
        "never_recalled": never_recalled,
        "most_recalled": most_recalled,
        "gone_cold": gone_cold,
        "never_recalled_list": never_list,
        "cold_days": cold_days,
        "project_filter": project,
    }))
}

fn with_page(mut data: Value, current_page: &str) -> Context {
    if let Some(map) = data.as_object_mut() {
        map.insert("current_page".into(), current_page.into());
    }
    Context::from_serialize(&data)
}

fn project_param(query: &HashMap<String, String>) -> String {
    query.get("project").cloned().unwrap_or_default()
}

async fn render_telemetry(state: PanelState, project: String, template: &'static str) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    match blocking(move || telemetry_data(&engine, &project)).await {
        Ok(data) => page(state.templates(), template, with_page(data, "telemetry")),
        Err(failure) => failure,
    }
}

pub(crate) async fn telemetry(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    render_telemetry(state, project_param(&query), "telemetry.html").await
}

pub(crate) async fn telemetry_refresh(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    render_telemetry(
        state,
        project_param(&query),
        "partials/telemetry_content.html",
    )
    .await
}

/// Per-tool counters, most called first.
fn tool_metrics(engine: &Engine) -> omnimem_engine::Result<Vec<Value>> {
    let mut metrics = Vec::new();
    for key in engine.store().scan_prefix(METRICS_PREFIX)? {
        let Some(row) = engine.store().hash_get_all(&key)? else {
            continue;
        };
        let calls = count(row.get("call_count"));
        if calls <= 0 {
            continue;
        }
        let duration = count(row.get("total_duration_ms"));
        let chars = count(row.get("total_response_chars")).max(0) as usize;
        let avg_chars = (chars as f64 / calls as f64).round_ties_even() as usize;
        metrics.push(json!({
            "name": key.strip_prefix(METRICS_PREFIX).unwrap_or(&key),
            "call_count": calls,
            "total_duration_ms": duration,
            "total_response_chars": chars,
            "total_response_tokens": chars / CHARS_PER_TOKEN,
            "avg_duration_ms": round_to(duration as f64 / calls as f64, 1),
            "avg_response_chars": avg_chars,
            "avg_response_tokens": avg_chars / CHARS_PER_TOKEN,
            "error_count": count(row.get("error_count")),
            "last_called_at": minutes(row.get("last_called_at")),
        }));
    }
    metrics.sort_by(|a, b| b["call_count"].as_i64().cmp(&a["call_count"].as_i64()));
    Ok(metrics)
}

fn token_data(
    engine: &Engine,
    overhead: StaticOverhead,
    project: &str,
) -> omnimem_engine::Result<Value> {
    let tokens = |chars: usize| chars / CHARS_PER_TOKEN;
    let deferred = overhead.deferred_names_chars;
    let static_chars = overhead.instructions_chars + overhead.tool_schemas_chars + deferred;
    let static_tokens = tokens(overhead.instructions_chars)
        + tokens(overhead.tool_schemas_chars)
        + tokens(deferred);

    let (mut total_memories, mut total_recalls, mut content_chars) = (0usize, 0i64, 0usize);
    let mut namespace_counts: Map<String, Value> = MEMORY_NAMESPACES
        .iter()
        .map(|ns| ((*ns).to_owned(), json!(0)))
        .collect();
    for namespace in MEMORY_NAMESPACES {
        let keys = engine.store().scan_prefix(&format!("mem:{namespace}:"))?;
        let rows = engine.store().get_fields_multi(
            &keys,
            &[
                "state",
                "project",
                "project_name",
                "recall_count",
                "content",
            ],
        )?;
        for row in rows.into_iter().flatten() {
            if !is_live(&row) || (!project.is_empty() && project_of(&row) != project) {
                continue;
            }
            total_memories += 1;
            if let Some(n) = namespace_counts.get_mut(namespace) {
                *n = json!(n.as_u64().unwrap_or(0) + 1);
            }
            total_recalls += count(row.get("recall_count"));
            content_chars += row.get("content").map_or(0, |c| c.chars().count());
        }
    }
    let avg_content_chars = content_chars.checked_div(total_memories).unwrap_or(0);

    let metrics = tool_metrics(engine)?;
    let sum = |field: &str| {
        metrics
            .iter()
            .map(|m| m[field].as_i64().unwrap_or(0))
            .sum::<i64>()
    };
    let metrics_since = engine
        .store()
        .hash_get_all(METRICS_RESET_KEY)?
        .filter(|row| !row.is_empty())
        .map(|row| minutes(row.get("reset_at")))
        .unwrap_or_default();

    Ok(json!({
        "instructions_chars": overhead.instructions_chars,
        "instructions_tokens": tokens(overhead.instructions_chars),
        "tool_count": overhead.tool_count,
        "tool_schemas_chars": overhead.tool_schemas_chars,
        "tool_schemas_tokens": tokens(overhead.tool_schemas_chars),
        "deferred_names_chars": deferred,
        "deferred_names_tokens": tokens(deferred),
        "static_total_chars": static_chars,
        "static_total_tokens": static_tokens,
        "total_memories": total_memories,
        "total_recalls": total_recalls,
        "total_content_chars": content_chars,
        "total_content_tokens": tokens(content_chars),
        "avg_content_chars": avg_content_chars,
        "avg_content_tokens": tokens(avg_content_chars),
        "namespace_counts": namespace_counts,
        "has_measured_data": !metrics.is_empty(),
        "total_tool_calls": sum("call_count"),
        "total_tool_tokens": sum("total_response_tokens"),
        "total_tool_errors": sum("error_count"),
        "tool_metrics": metrics,
        "metrics_since": metrics_since,
        "project_filter": project,
    }))
}

async fn render_tokens(state: PanelState, project: String, template: &'static str) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let overhead = state.static_overhead();
    match blocking(move || token_data(&engine, overhead, &project)).await {
        Ok(data) => page(
            state.templates(),
            template,
            with_page(data, "token_overhead"),
        ),
        Err(failure) => failure,
    }
}

pub(crate) async fn token_overhead(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    render_tokens(state, project_param(&query), "token_overhead.html").await
}

pub(crate) async fn token_overhead_refresh(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    render_tokens(
        state,
        project_param(&query),
        "partials/token_overhead_content.html",
    )
    .await
}

/// POST `/token-overhead/reset`: clear the measured tool counters.
pub(crate) async fn token_overhead_reset(State(state): State<PanelState>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let reset = blocking(move || {
        let keys = engine.store().scan_prefix(METRICS_PREFIX)?;
        engine.store().delete_many(&keys)?;
        engine.store().hash_set(
            METRICS_RESET_KEY,
            &omnimem_store::Fields::from([("reset_at".to_owned(), now_str())]),
        )?;
        Ok(())
    })
    .await;
    match reset {
        Ok(()) => see_other("/token-overhead"),
        Err(failure) => failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_read_however_they_were_written() {
        assert_eq!(count(Some(&"3".to_owned())), 3);
        assert_eq!(count(Some(&"3.0".to_owned())), 3);
        assert_eq!(count(Some(&"lots".to_owned())), 0);
        assert_eq!(count(None), 0);
    }
}
