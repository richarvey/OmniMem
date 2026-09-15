//! `/memories`: every memory, filtered and paged (`web_ui/routes/memories.py`).
//!
//! The sidebar's Preferences, Articles and Learned Knowledge entries are
//! filtered views of this page. An htmx request (a filter change or a page
//! link) gets the rows partial, anything else the whole page.

use std::collections::{BTreeSet, HashMap};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use chrono::{Local, TimeZone};
use minijinja::context;
use omnimem_engine::Engine;
use omnimem_engine::classification::classification_fields;
use serde_json::json;

use crate::PanelState;
use crate::choices::{LICENCE_CHOICES, PROVENANCE_CHOICES, is_licence_class, is_provenance_class};
use crate::pages::{blocking, now_seconds, starting};
use crate::render::page;

const PAGE_SIZE: usize = 25;
const LISTABLE_NAMESPACES: [&str; 4] = ["episodic", "project", "knowledge", "preference"];
/// Only what the list renders or filters on: no vectors, lessons or gotchas.
const LIST_FIELDS: &[&str] = &[
    "content",
    "state",
    "project",
    "project_name",
    "updated_at",
    "created_at",
    "feed_name",
    "last_recalled",
    "licence",
    "provenance",
    "enriched_from",
    "imported_at",
    "stack",
    "goals",
];

#[derive(Default)]
struct Filters {
    namespace: String,
    state: String,
    project: String,
    source: String,
    licence: String,
    provenance: String,
}

struct Row {
    key: String,
    namespace: &'static str,
    content: String,
    state: String,
    project: String,
    feed_name: String,
    licence: String,
    provenance: String,
    updated_at: f64,
    created_at: f64,
    heat: &'static str,
}

fn number(raw: Option<&String>) -> f64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}

/// Time since the last recall, bucketed for the row's fading left rule.
fn recall_heat(last_recalled: Option<&String>, now: f64) -> &'static str {
    let at = number(last_recalled);
    if at <= 0.0 {
        return "";
    }
    match (now - at) / 86_400.0 {
        days if days < 7.0 => "hot",
        days if days < 30.0 => "warm",
        days if days < 90.0 => "cool",
        _ => "",
    }
}

/// The matching memories, and every project seen (filtered or not).
fn collect(engine: &Engine, filters: &Filters) -> omnimem_engine::Result<(Vec<Row>, Vec<String>)> {
    let store = engine.store();
    let namespaces: Vec<&'static str> = match LISTABLE_NAMESPACES
        .iter()
        .find(|ns| **ns == filters.namespace)
    {
        Some(ns) => vec![*ns],
        None => LISTABLE_NAMESPACES.to_vec(),
    };
    let now = now_seconds();
    let mut rows = Vec::new();
    let mut projects = BTreeSet::new();
    for ns in namespaces {
        let keys = store.scan_prefix(&format!("mem:{ns}:"))?;
        if keys.is_empty() {
            continue;
        }
        for (key, data) in keys.iter().zip(store.get_fields_multi(&keys, LIST_FIELDS)?) {
            let Some(data) = data else { continue };
            let text = |field: &str| data.get(field).cloned().unwrap_or_default();
            let state = data
                .get("state")
                .cloned()
                .unwrap_or_else(|| "active".to_owned());
            let project = data
                .get("project")
                .filter(|p| !p.is_empty())
                .or_else(|| data.get("project_name"))
                .cloned()
                .unwrap_or_default();
            if !project.is_empty() {
                projects.insert(project.clone());
            }
            let feed_name = text("feed_name");
            if (!filters.state.is_empty() && state != filters.state)
                || (!filters.project.is_empty() && project != filters.project)
                || (filters.source == "rss" && feed_name.is_empty())
                || (filters.source == "learned" && !feed_name.is_empty())
            {
                continue;
            }
            // Filter on the value the record has or would be backfilled with,
            // as recall reports it, so ?licence=unknown is the whole queue.
            let classification = classification_fields(&data, ns, Some(key));
            let class = |field: &str| {
                classification
                    .get(field)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned()
            };
            let (licence, provenance) = (class("licence"), class("provenance"));
            if (!filters.licence.is_empty() && licence != filters.licence)
                || (!filters.provenance.is_empty() && provenance != filters.provenance)
            {
                continue;
            }
            rows.push(Row {
                key: key.clone(),
                namespace: ns,
                content: text("content").chars().take(120).collect(),
                state,
                project,
                feed_name,
                licence,
                provenance,
                updated_at: number(data.get("updated_at")),
                created_at: number(data.get("created_at")),
                heat: recall_heat(data.get("last_recalled"), now),
            });
        }
    }
    Ok((rows, projects.into_iter().collect()))
}

pub(crate) async fn handler(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let param = |name: &str| query.get(name).cloned().unwrap_or_default();
    let keep = |value: String, valid: bool| if valid { value } else { String::new() };
    let namespace = param("namespace");
    let source = param("source");
    let licence = param("licence");
    let provenance = param("provenance");
    let filters = Filters {
        namespace: keep(
            namespace.clone(),
            LISTABLE_NAMESPACES.contains(&namespace.as_str()),
        ),
        state: param("state"),
        project: param("project"),
        source: keep(source.clone(), matches!(source.as_str(), "rss" | "learned")),
        licence: keep(licence.clone(), is_licence_class(&licence)),
        provenance: keep(provenance.clone(), is_provenance_class(&provenance)),
    };
    let sort = query
        .get("sort")
        .cloned()
        .unwrap_or_else(|| "newest".to_owned());
    let requested_page = query
        .get("page")
        .and_then(|p| p.trim().parse::<i64>().ok())
        .unwrap_or(1)
        .max(1) as usize;

    let (rows, projects, filters) = match blocking(move || {
        collect(&engine, &filters).map(|(rows, projects)| (rows, projects, filters))
    })
    .await
    {
        Ok(found) => found,
        Err(failure) => return failure,
    };

    // Articles don't change after ingestion, but migrations can touch
    // updated_at, so they are ordered and dated by when they arrived.
    let articles = filters.source == "rss";
    let row_ts = |row: &Row| {
        if articles && row.created_at != 0.0 {
            row.created_at
        } else {
            row.updated_at
        }
    };
    let mut rows = rows;
    rows.sort_by(|a, b| {
        let ordering = row_ts(a).total_cmp(&row_ts(b));
        if sort == "oldest" {
            ordering
        } else {
            ordering.reverse()
        }
    });

    let total = rows.len();
    let total_pages = total.div_ceil(PAGE_SIZE).max(1);
    let current = requested_page.min(total_pages);
    let memories: Vec<_> = rows
        .iter()
        .skip((current - 1) * PAGE_SIZE)
        .take(PAGE_SIZE)
        .map(|row| {
            let ts = row_ts(row);
            let (date, time) = match Local.timestamp_opt(ts as i64, 0).single() {
                Some(t) if ts > 0.0 => (
                    t.format("%-d %b %Y").to_string(),
                    t.format("%H:%M").to_string(),
                ),
                _ => ("—".to_owned(), String::new()),
            };
            json!({
                "key": row.key,
                "namespace": row.namespace,
                "content": row.content,
                "state": row.state,
                "project": row.project,
                "feed_name": row.feed_name,
                "licence": row.licence,
                "provenance": row.provenance,
                "updated_at": row.updated_at,
                "created_at": row.created_at,
                "heat": row.heat,
                "updated_date": date,
                "updated_time": time,
            })
        })
        .collect();

    let mut extra_params = String::new();
    for (name, value) in [
        ("namespace", &filters.namespace),
        ("state", &filters.state),
        ("project", &filters.project),
        ("source", &filters.source),
        ("licence", &filters.licence),
        ("provenance", &filters.provenance),
    ] {
        if !value.is_empty() {
            extra_params.push_str(&format!("&{name}={value}"));
        }
    }
    if sort != "newest" {
        extra_params.push_str(&format!("&sort={sort}"));
    }

    let nav_page = match (filters.namespace.as_str(), filters.source.as_str()) {
        ("preference", _) => "preferences",
        ("knowledge", "learned") => "learned",
        ("knowledge", _) => "articles",
        _ => "memories",
    };
    // Row actions come back to this exact view, filters and page intact.
    let back_url = match uri.query() {
        Some(q) if !q.is_empty() => format!("{}?{q}", uri.path()),
        _ => uri.path().to_owned(),
    };
    let template = if headers.get("HX-Request").is_some_and(|v| v == "true") {
        "memories/_rows.html"
    } else {
        "memories/list.html"
    };
    page(
        state.templates(),
        template,
        context! {
            memories,
            namespace => filters.namespace,
            state => filters.state,
            project => filters.project,
            source => filters.source,
            licence => filters.licence,
            licence_classes => LICENCE_CHOICES,
            provenance => filters.provenance,
            provenance_classes => PROVENANCE_CHOICES,
            back_url,
            sort,
            projects,
            page => current,
            total_pages,
            total,
            extra_params,
            base_url => "/memories",
            current_page => nav_page,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_heat_fades_with_time() {
        let now = 100.0 * 86_400.0;
        let ago = |days: f64| Some((now - days * 86_400.0).to_string());
        assert_eq!(recall_heat(ago(1.0).as_ref(), now), "hot");
        assert_eq!(recall_heat(ago(10.0).as_ref(), now), "warm");
        assert_eq!(recall_heat(ago(60.0).as_ref(), now), "cool");
        assert_eq!(recall_heat(ago(95.0).as_ref(), now), "");
        assert_eq!(recall_heat(None, now), "");
        assert_eq!(recall_heat(Some(&"junk".to_owned()), now), "");
    }
}
