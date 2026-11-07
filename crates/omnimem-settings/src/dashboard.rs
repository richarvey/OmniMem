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
use crate::pages::{off_runtime, starting};
use crate::render::page;

const NAMESPACES: [&str; 4] = ["episodic", "project", "knowledge", "preference"];
const STATES: [&str; 3] = ["active", "deprioritised", "archived"];

fn ttl() -> Duration {
    let seconds = omnimem_core::env::var("DASHBOARD_STATS_TTL")
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

pub(crate) fn compute(engine: &Engine) -> omnimem_engine::Result<Value> {
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
                let index = if let Some(i) = projects.iter().position(|(n, _, _)| *n == name) {
                    i
                } else {
                    projects.push((name.clone(), None, Vec::new()));
                    projects.len() - 1
                };
                if key.strip_prefix("mem:project:") == Some(name.as_str()) {
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

    let now = Local::now().timestamp() as f64;
    let charts = charts(engine, now)?;
    let attention = attention(engine)?;
    Ok(json!({
        "ns_stats": ns_stats,
        "total": total,
        "skills": {"total": skill_keys.len(), "states": skill_states, "proposals": proposals},
        "recent": recent,
        "charts": charts,
        "attention": attention,
    }))
}

/// Weeks of growth the dashboard draws, and the window "recently active"
/// projects are judged over.
const GROWTH_WEEKS: i64 = 26;
const RECENT_DAYS: i64 = 30;
/// The namespaces the charts stack, bottom first, each in its own colour.
const CHART_NAMESPACES: [&str; 4] = ["episodic", "knowledge", "preference", "project"];

fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 * 1000.0 / whole as f64).round() / 10.0
    }
}

/// Everything the dashboard's charts and attention strip need, worked out
/// here so the template only places shapes.
fn charts(engine: &Engine, now: f64) -> omnimem_engine::Result<Value> {
    let counts = engine
        .store()
        .dashboard_counts(now, GROWTH_WEEKS, RECENT_DAYS)?;

    // Namespaces by state, as 100% bars.
    let mut states: Vec<Value> = Vec::new();
    for ns in ["episodic", "project", "knowledge", "preference", "skill"] {
        let n = |state: &str| {
            counts
                .by_namespace_state
                .iter()
                .filter(|(n, s, _)| n == ns && s == state)
                .map(|r| r.2)
                .sum::<u64>()
        };
        let (active, depri, archived) = (n("active"), n("deprioritised"), n("archived"));
        let total = counts
            .by_namespace_state
            .iter()
            .filter(|(n, _, _)| n == ns)
            .map(|r| r.2)
            .sum::<u64>();
        if total == 0 {
            continue;
        }
        states.push(json!({
            "namespace": ns,
            "total": total,
            "active": active, "deprioritised": depri, "archived": archived,
            "active_pct": pct(active, total),
            "deprioritised_pct": pct(depri, total),
            "archived_pct": pct(archived, total),
        }));
    }

    // Growth: stacked weekly columns in a 720 x 180 box.
    let (width, height, gap) = (720.0_f64, 180.0_f64, 4.0_f64);
    let mut weeks = vec![[0u64; 4]; GROWTH_WEEKS as usize];
    for (ns, week, n) in &counts.created_weekly {
        if let (Some(i), Some(slot)) = (
            CHART_NAMESPACES.iter().position(|c| c == ns),
            usize::try_from(*week).ok().and_then(|w| weeks.get_mut(w)),
        ) {
            slot[i] += n;
        }
    }
    let peak = weeks
        .iter()
        .map(|w| w.iter().sum::<u64>())
        .max()
        .unwrap_or(0)
        .max(1);
    let column = width / GROWTH_WEEKS as f64;
    let week_start = |i: usize| now - (GROWTH_WEEKS - i as i64) as f64 * 604_800.0;
    let label = |ts: f64| {
        Local
            .timestamp_opt(ts as i64, 0)
            .single()
            .map(|t| t.format("%-d %b").to_string())
            .unwrap_or_default()
    };
    let mut columns = Vec::new();
    let mut growth_total = 0u64;
    for (i, week) in weeks.iter().enumerate() {
        let mut y = height;
        let mut segments = Vec::new();
        for (j, n) in week.iter().enumerate() {
            if *n == 0 {
                continue;
            }
            let h = (*n as f64 / peak as f64 * height).max(1.0);
            y -= h;
            segments.push(json!({
                "namespace": CHART_NAMESPACES[j],
                "y": (y * 10.0).round() / 10.0,
                "h": (h * 10.0).round() / 10.0,
                "count": n,
            }));
        }
        let total: u64 = week.iter().sum();
        growth_total += total;
        columns.push(json!({
            "x": (i as f64 * column + gap / 2.0),
            "w": column - gap,
            "segments": segments,
            "total": total,
            "label": label(week_start(i)),
            "tick": i % 4 == 0,
            "by_namespace": week,
        }));
    }

    // Busiest projects as horizontal bars.
    let busiest = counts
        .projects
        .iter()
        .map(|p| p.1)
        .max()
        .unwrap_or(0)
        .max(1);
    let projects: Vec<Value> = counts
        .projects
        .iter()
        .map(|(name, total, recent)| {
            json!({"name": name, "total": total, "recent": recent, "pct": pct(*total, busiest)})
        })
        .collect();

    // Experience: outcomes as one split bar, efforts as five columns.
    let outcome = |name: &str| {
        counts
            .outcomes
            .iter()
            .filter(|o| o.0 == name)
            .map(|o| o.1)
            .sum::<u64>()
    };
    let (succeeded, pivoted, abandoned) = (
        outcome("succeeded"),
        outcome("pivoted"),
        outcome("abandoned"),
    );
    let outcomes_total = succeeded + pivoted + abandoned;
    let effort_peak = counts.efforts.iter().map(|e| e.1).max().unwrap_or(0).max(1);
    let efforts: Vec<Value> = (1..=5)
        .map(|score| {
            let n = counts
                .efforts
                .iter()
                .find(|e| e.0 == score)
                .map_or(0, |e| e.1);
            json!({"score": score, "count": n, "pct": pct(n, effort_peak)})
        })
        .collect();
    let effort_n: u64 = counts.efforts.iter().map(|e| e.1).sum();
    let effort_avg = if effort_n == 0 {
        0.0
    } else {
        let sum: u64 = counts.efforts.iter().map(|(s, n)| *s as u64 * n).sum();
        (sum as f64 / effort_n as f64 * 10.0).round() / 10.0
    };

    // Article intake: a 30-day sparkline in a 240 x 40 box.
    let mut days = [0u64; 30];
    for (day, n) in &counts.articles_daily {
        if let Some(slot) = usize::try_from(*day).ok().and_then(|d| days.get_mut(d)) {
            *slot += n;
        }
    }
    let day_peak = days.iter().copied().max().unwrap_or(0).max(1);
    let spark: Vec<String> = days
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "{:.1},{:.1}",
                i as f64 * 240.0 / 29.0,
                38.0 - *n as f64 / day_peak as f64 * 36.0
            )
        })
        .collect();
    let articles_month: u64 = days.iter().sum();

    let trust = |name: &str| {
        counts
            .trust
            .iter()
            .filter(|t| t.0 == name)
            .map(|t| t.1)
            .sum::<u64>()
    };
    Ok(json!({
        "states": states,
        "growth": {"columns": columns, "peak": peak, "total": growth_total, "weeks": GROWTH_WEEKS, "width": width, "height": height},
        "projects": projects,
        "recent_days": RECENT_DAYS,
        "experience": {
            "succeeded": succeeded, "pivoted": pivoted, "abandoned": abandoned, "total": outcomes_total,
            "succeeded_pct": pct(succeeded, outcomes_total),
            "pivoted_pct": pct(pivoted, outcomes_total),
            "abandoned_pct": pct(abandoned, outcomes_total),
            "efforts": efforts, "effort_avg": effort_avg, "effort_n": effort_n,
        },
        "articles": {"month": articles_month, "spark": spark.join(" ")},
        "disputed": trust("disputed"),
    }))
}

/// What waits on a person, read live: these are the dashboard's first row.
fn attention(engine: &Engine) -> omnimem_engine::Result<Value> {
    let unconfirmed = engine.unconfirmed_from(None, usize::MAX)?.len();
    let contradictions = engine.open_contradictions()?.len();
    let resolutions = engine.pending_resolutions()?.len();
    let proposals = engine
        .pending_skill_proposals()?
        .iter()
        .filter(|p| !p.stale)
        .count();
    Ok(json!({
        "unconfirmed": unconfirmed,
        "contradictions": contradictions,
        "resolutions": resolutions,
        "proposals": proposals,
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
    let (computed_at, counts) = if let Some(hit) = cached {
        hit
    } else {
        let worker = engine.clone();
        let computed = tokio::task::spawn_blocking(move || compute(&worker)).await;
        match computed {
            Ok(Ok(counts)) => {
                let fresh = (Instant::now(), counts);
                if !ttl.is_zero() {
                    state.caches().dashboard = Some(fresh.clone());
                }
                fresh
            }
            // A failure is shown, not cached: the next load tries again.
            Ok(Err(e)) => return crate::render::failure(&format!("counting memories: {e}")),
            Err(e) => return crate::render::failure(&format!("counting memories: {e}")),
        }
    };
    // The queue depth is a store read, so it goes off the workers too.
    let enrichment_pending =
        off_runtime(move || engine.store().enrichment_pending().map_or(-1, |n| n as i64))
            .await
            .unwrap_or(-1);
    page(
        state.templates(),
        "dashboard.html",
        context! {
            current_page => "dashboard",
            ns_stats => counts["ns_stats"],
            total => counts["total"],
            skills => counts["skills"],
            recent => counts["recent"],
            charts => counts["charts"],
            attention => counts["attention"],
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
