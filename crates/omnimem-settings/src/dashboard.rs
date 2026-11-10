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

pub(crate) fn compute(engine: &Engine) -> omnimem_engine::Result<Value> {
    let store = engine.store();
    let now = Local::now().timestamp() as f64;
    let counts = store.dashboard_counts(now, GROWTH_WEEKS, RECENT_DAYS)?;
    let count = |ns: &str, state: Option<&str>| {
        counts
            .by_namespace_state
            .iter()
            .filter(|(n, s, _)| n == ns && state.is_none_or(|st| s == st))
            .map(|r| r.2)
            .sum::<u64>()
    };
    let states_of = |ns: &str| {
        let mut states = zero_states();
        for state in STATES {
            states.insert(state.to_owned(), json!(count(ns, Some(state))));
        }
        states
    };
    let total: u64 = NAMESPACES.iter().map(|ns| count(ns, None)).sum();
    let mut projects = zero_states();
    for (state, n) in &counts.project_contexts {
        if let Some(slot) = projects.get_mut(state) {
            *slot = json!(n);
        }
    }
    let distinct: u64 = counts.project_contexts.iter().map(|p| p.1).sum();

    let rows = store.get_fields_multi(
        &counts.recent,
        &[
            "content",
            "project",
            "project_name",
            "name",
            "description",
            "state",
            "updated_at",
        ],
    )?;
    let recent: Vec<Value> = counts
        .recent
        .iter()
        .zip(rows)
        .map(|(key, row)| {
            let row: HashMap<String, String> = row.unwrap_or_default().into_iter().collect();
            let namespace = key.split(':').nth(1).unwrap_or("").to_owned();
            let content = if namespace == "skill" {
                ["name", "description"]
                    .iter()
                    .filter_map(|f| row.get(*f).filter(|v| !v.is_empty()))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" — ")
            } else {
                row.get("content").cloned().unwrap_or_default()
            };
            let updated = row
                .get("updated_at")
                .and_then(|v| v.trim().parse::<f64>().ok())
                .unwrap_or(0.0);
            let (date, time) = match Local.timestamp_opt(updated as i64, 0).single() {
                Some(t) if updated > 0.0 => (
                    t.format("%-d %b %Y").to_string(),
                    t.format("%H:%M").to_string(),
                ),
                _ => ("—".to_owned(), String::new()),
            };
            json!({
                "key": key,
                "namespace": namespace,
                "state": row.get("state").cloned().unwrap_or_else(|| "active".to_owned()),
                "content": crate::render::clip(&content, crate::render::preview::DASHBOARD),
                "project": row.get("project").filter(|p| !p.is_empty()).or_else(|| row.get("project_name")).cloned().unwrap_or_default(),
                "updated_date": date,
                "updated_time": time,
            })
        })
        .collect();

    let attention = attention(engine)?;
    let charts = charts(&counts, now);
    Ok(json!({
        "total": total,
        "projects": {"distinct": distinct, "states": projects},
        "skills": {
            "total": count("skill", None),
            "states": states_of("skill"),
            "proposals": attention["proposals"],
        },
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
fn charts(counts: &omnimem_store::DashboardCounts, now: f64) -> Value {
    // Namespaces by state, as 100% bars.
    let mut states: Vec<Value> = Vec::new();
    for ns in ["episodic", "project", "knowledge", "preference"] {
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
            "left": (i as f64 * 1000.0 / GROWTH_WEEKS as f64).round() / 10.0,
            "x": (i as f64 * column + gap / 2.0),
            "w": column - gap,
            "segments": segments,
            "total": total,
            "label": label(week_start(i)),
            "tick": i % 4 == 0,
            "tick_minor": i % 8 == 4,
            "by_namespace": week,
        }));
    }

    // Busiest projects: the bars are the last month's updates, which is
    // what the list is ordered by; the total is shown beside them.
    let recent_peak = counts.projects.iter().map(|p| p.2).max().unwrap_or(0);
    let total_peak = counts
        .projects
        .iter()
        .map(|p| p.1)
        .max()
        .unwrap_or(0)
        .max(1);
    let by_recent = recent_peak > 0;
    let projects: Vec<Value> = counts
        .projects
        .iter()
        .map(|(name, total, recent)| {
            let pct_of = if by_recent {
                pct(*recent, recent_peak)
            } else {
                pct(*total, total_peak)
            };
            json!({"name": name, "total": total, "recent": recent, "pct": pct_of})
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
        (sum as f64 / effort_n as f64 * 100.0).round() / 100.0
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
    json!({
        "states": states,
        "growth": {"columns": columns, "peak": peak, "half": peak.div_ceil(2), "half_y": height - (peak.div_ceil(2) as f64 / peak as f64 * height), "total": growth_total, "weeks": GROWTH_WEEKS, "width": width, "height": height},
        "projects": projects,
        "projects_by_recent": by_recent,
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
    })
}

/// What waits on a person, read live: these are the dashboard's first row.
fn attention(engine: &Engine) -> omnimem_engine::Result<Value> {
    let (contradictions, wording) = crate::management::contradiction_counts(engine)?;
    let proposals = engine
        .pending_skill_proposals()?
        .iter()
        .filter(|p| !p.stale)
        .count();
    Ok(json!({
        "unconfirmed": engine.store().count_unconfirmed()?,
        "contradictions": contradictions,
        "wording": wording,
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
    // Duplicates a scan found, still unsettled: a scan is too costly to run
    // for the dashboard, so it reports the last one that was run, however
    // old, with its age. The tile's link rescans once the scan has expired.
    // Copies archived since, from anywhere but the scan page, drop out of
    // the cached clusters first, so the count is of what is still waiting.
    let cached_scans: Vec<_> = state
        .caches()
        .duplicates
        .iter()
        .map(|(slot, found)| (slot.clone(), found.clone()))
        .collect();
    if !cached_scans.is_empty() {
        let worker = engine.clone();
        let pruned = off_runtime(move || {
            cached_scans
                .into_iter()
                .map(|(slot, (at, clusters))| {
                    let kept = crate::management::prune(&worker, &clusters).unwrap_or(clusters);
                    (slot, (at, kept))
                })
                .collect::<Vec<_>>()
        })
        .await;
        if let Ok(pruned) = pruned {
            let mut caches = state.caches();
            for (slot, found) in pruned {
                // A scan run meanwhile is newer than what was pruned.
                if caches
                    .duplicates
                    .get(&slot)
                    .is_some_and(|(at, _)| *at == found.0)
                {
                    caches.duplicates.insert(slot, found);
                }
            }
        }
    }
    let duplicates = crate::management::cached_duplicates(&state.caches());
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
            projects => counts["projects"],
            total => counts["total"],
            skills => counts["skills"],
            recent => counts["recent"],
            charts => counts["charts"],
            attention => counts["attention"],
            duplicates_found => duplicates.0,
            duplicates_in => duplicates.1.iter().map(|(ns, _)| ns.clone()).collect::<Vec<_>>(),
            duplicates_project => duplicates.1.first().map(|(_, p)| p.clone()).unwrap_or_default(),
            duplicates_age => duplicates.2,
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
