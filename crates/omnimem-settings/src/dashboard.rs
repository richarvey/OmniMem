//! `/`: namespace and state counts, compiled skills and recent activity
//! (`web_ui/routes/dashboard.py`).
//!
//! The counts come from a scan of every key, so they are cached for
//! `DASHBOARD_STATS_TTL` seconds (default 60, 0 disables); `?refresh=1`
//! recomputes. The queue and model indicators are always live.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::response::Response;
use chrono::{Local, TimeZone};
use minijinja::context;
use omnimem_engine::Engine;
use serde_json::{Map, Value, json};

use crate::PanelState;
use crate::pages::starting;
use crate::render::page;

const NAMESPACES: [&str; 4] = ["episodic", "project", "knowledge", "preference"];
const STATES: [&str; 3] = ["active", "deprioritised", "archived"];

fn ttl() -> Duration {
    let seconds = std::env::var("DASHBOARD_STATS_TTL")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(60);
    Duration::from_secs(seconds)
}

fn zero_states() -> Map<String, Value> {
    STATES.iter().map(|s| ((*s).to_owned(), json!(0))).collect()
}

fn bump(counts: &mut Map<String, Value>, state: &str) {
    if let Some(n) = counts.get_mut(state) {
        *n = json!(n.as_u64().unwrap_or(0) + 1);
    }
}

/// One state per project: the context entry's when it has one, otherwise the
/// most-alive state among its memories.
fn project_state(context_state: Option<&str>, member_states: &[String]) -> String {
    if let Some(state) = context_state {
        return state.to_owned();
    }
    for state in ["active", "deprioritised"] {
        if member_states.iter().any(|s| s == state) {
            return state.to_owned();
        }
    }
    "archived".to_owned()
}

struct Candidate {
    key: String,
    namespace: String,
    state: String,
    updated_at: f64,
}

pub(crate) fn compute(engine: &Engine) -> omnimem_store::Result<Value> {
    let store = engine.store();
    let mut ns_stats = Map::new();
    let mut total = 0usize;
    let mut candidates: Vec<Candidate> = Vec::new();
    let number = |v: Option<&String>| v.and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(0.0);

    for ns in NAMESPACES {
        let keys = store.scan_prefix(&format!("mem:{ns}:"))?;
        let mut states = zero_states();
        let mut projects: Vec<(String, Option<String>, Vec<String>)> = Vec::new();
        let fields: &[&str] = if ns == "project" {
            &["state", "updated_at", "project_name", "project"]
        } else {
            &["state", "updated_at"]
        };
        for (key, row) in keys.iter().zip(store.get_fields_multi(&keys, fields)?) {
            let row = row.unwrap_or_default();
            let state = row
                .get("state")
                .cloned()
                .unwrap_or_else(|| "active".to_owned());
            bump(&mut states, &state);
            if ns == "project" {
                let name = row
                    .get("project_name")
                    .filter(|n| !n.is_empty())
                    .or_else(|| row.get("project").filter(|n| !n.is_empty()))
                    .cloned()
                    .unwrap_or_else(|| key.rsplit(':').next().unwrap_or("").to_owned());
                let index = match projects.iter().position(|(n, _, _)| *n == name) {
                    Some(i) => i,
                    None => {
                        projects.push((name.clone(), None, Vec::new()));
                        projects.len() - 1
                    }
                };
                if *key == format!("mem:project:{name}") {
                    projects[index].1 = Some(state.clone());
                } else {
                    projects[index].2.push(state.clone());
                }
            }
            candidates.push(Candidate {
                key: key.clone(),
                namespace: ns.to_owned(),
                state,
                updated_at: number(row.get("updated_at")),
            });
        }
        let mut entry = Map::new();
        entry.insert("total".into(), json!(keys.len()));
        entry.insert("states".into(), Value::Object(states));
        if ns == "project" {
            let mut by_state = zero_states();
            for (_, context_state, members) in &projects {
                bump(
                    &mut by_state,
                    &project_state(context_state.as_deref(), members),
                );
            }
            entry.insert("distinct".into(), json!(projects.len()));
            entry.insert("projects".into(), Value::Object(by_state));
        }
        ns_stats.insert(ns.to_owned(), Value::Object(entry));
        total += keys.len();
    }

    let skill_keys = store.scan_prefix("mem:skill:")?;
    let mut skill_states = zero_states();
    for (key, row) in skill_keys
        .iter()
        .zip(store.get_fields_multi(&skill_keys, &["state", "updated_at"])?)
    {
        let row = row.unwrap_or_default();
        let state = row
            .get("state")
            .cloned()
            .unwrap_or_else(|| "active".to_owned());
        bump(&mut skill_states, &state);
        candidates.push(Candidate {
            key: key.clone(),
            namespace: "skill".to_owned(),
            state,
            updated_at: number(row.get("updated_at")),
        });
    }
    let proposals = store.scan_prefix("meta:skill:proposal:")?.len();

    candidates.sort_by(|a, b| {
        b.updated_at
            .partial_cmp(&a.updated_at)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.truncate(10);
    let recent_keys: Vec<String> = candidates.iter().map(|c| c.key.clone()).collect();
    let details =
        store.get_fields_multi(&recent_keys, &["content", "project", "name", "description"])?;
    let recent: Vec<Value> = candidates
        .iter()
        .zip(details)
        .map(|(candidate, row)| {
            let row: HashMap<String, String> = row.unwrap_or_default().into_iter().collect();
            let content = if candidate.namespace == "skill" {
                ["name", "description"]
                    .iter()
                    .filter_map(|f| row.get(*f).filter(|v| !v.is_empty()))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" — ")
            } else {
                row.get("content").cloned().unwrap_or_default()
            };
            let (date, time) = match Local.timestamp_opt(candidate.updated_at as i64, 0).single() {
                Some(t) if candidate.updated_at > 0.0 => (
                    t.format("%-d %b %Y").to_string(),
                    t.format("%H:%M").to_string(),
                ),
                _ => ("—".to_owned(), String::new()),
            };
            json!({
                "key": candidate.key,
                "namespace": candidate.namespace,
                "state": candidate.state,
                "updated_at": candidate.updated_at,
                "content": content.chars().take(100).collect::<String>(),
                "project": row.get("project").cloned().unwrap_or_default(),
                "updated_date": date,
                "updated_time": time,
            })
        })
        .collect();

    Ok(json!({
        "ns_stats": ns_stats,
        "total": total,
        "skills": {"total": skill_keys.len(), "states": skill_states, "proposals": proposals},
        "recent": recent,
    }))
}

pub(crate) async fn page_handler(state: PanelState, refresh: bool) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let ttl = ttl();
    let cached = (!refresh && !ttl.is_zero())
        .then(|| state.caches().dashboard.clone())
        .flatten()
        .filter(|(at, _)| at.elapsed() < ttl);
    let (computed_at, stats) = match cached {
        Some(hit) => hit,
        None => {
            let worker = engine.clone();
            let computed = tokio::task::spawn_blocking(move || compute(&worker)).await;
            let stats = match computed {
                Ok(Ok(stats)) => stats,
                _ => {
                    json!({"ns_stats": {}, "total": 0, "skills": {"total": 0, "states": zero_states(), "proposals": 0}, "recent": []})
                }
            };
            let fresh = (Instant::now(), stats);
            if !ttl.is_zero() {
                state.caches().dashboard = Some(fresh.clone());
            }
            fresh
        }
    };
    let enrichment_pending = engine.store().enrichment_pending().map_or(-1, |n| n as i64);
    page(
        state.templates(),
        "dashboard.html",
        context! {
            current_page => "dashboard",
            ns_stats => stats["ns_stats"],
            total => stats["total"],
            skills => stats["skills"],
            recent => stats["recent"],
            stats_age => (!ttl.is_zero()).then(|| computed_at.elapsed().as_secs()),
            health => json!({"store": true, "model": true}),
            enrichment_pending => enrichment_pending,
        },
    )
}

pub(crate) async fn handler(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    page_handler(state, query.get("refresh").map(String::as_str) == Some("1")).await
}
