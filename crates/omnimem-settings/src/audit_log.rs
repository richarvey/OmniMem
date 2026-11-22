//! `/audit`: the audit log, newest first, and the rows the memory detail
//! page's History section shows. Reading only; the log is written by the
//! store, in the transaction of each change.

use std::collections::HashMap;
use std::fmt::Write as _;

use axum::extract::{Query, State};
use axum::response::Response;
use chrono::{Local, TimeZone};
use omnimem_engine::{AuditLevel, change_summary, parse_when};
use omnimem_store::{AuditQuery, AuditRow};
use serde_json::{Value, json};

use crate::PanelState;
use crate::pages::{blocking, now_seconds, quote, starting};
use crate::render::page;

const PAGE_SIZE: usize = 50;
/// A `full` row's text is cut to this many characters in the table.
const CLIP: usize = 80;

/// The actors a row can have, in the order the filter lists them.
const ACTORS: [&str; 8] = [
    "mcp",
    "panel",
    "hook",
    "maintenance",
    "enrichment",
    "rss",
    "cli",
    "import",
];

/// A row as the templates show it.
pub(crate) fn view(row: &AuditRow) -> Value {
    let when = Local
        .timestamp_opt(row.at as i64, 0)
        .single()
        .map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M:%S").to_string());
    json!({
        "when": when,
        "actor": row.actor,
        "client": row.client,
        "action": row.action,
        "key": row.key,
        "summary": change_summary(&row.from, &row.to, CLIP),
        "reason": row.reason,
    })
}

/// A date filter's end is the end of the day it names.
fn until(raw: &str, now: f64) -> Option<f64> {
    let at = parse_when(raw, now)?;
    Some(if raw.trim().len() == 10 {
        at + 86_400.0
    } else {
        at
    })
}

pub(crate) async fn list(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let param = |name: &str| query.get(name).map_or("", |v| v.trim()).to_owned();
    let (key, actor, action, from, to) = (
        param("key"),
        param("actor"),
        param("action"),
        param("from"),
        param("to"),
    );
    let requested_page = param("page").parse::<usize>().unwrap_or(1).max(1);
    let now = now_seconds();

    let mut problems = Vec::new();
    let since = if from.is_empty() {
        None
    } else {
        let at = parse_when(&from, now);
        if at.is_none() {
            problems.push(format!("Can't read the From date {from:?}."));
        }
        at
    };
    let before = if to.is_empty() {
        None
    } else {
        let at = until(&to, now);
        if at.is_none() {
            problems.push(format!("Can't read the To date {to:?}."));
        }
        at
    };

    let level = engine.audit_level();
    let audit_query = AuditQuery {
        key_contains: Some(key.clone()),
        actor: Some(actor.clone()),
        action: Some(action.clone()),
        since,
        until: before,
        // One more than a page, to know whether there is a next one.
        limit: PAGE_SIZE + 1,
        offset: (requested_page - 1) * PAGE_SIZE,
        ..AuditQuery::default()
    };
    let rows = match blocking(move || {
        let rows = engine.store().audit_entries(&audit_query)?;
        let total = engine.store().audit_count()?;
        Ok((rows, total))
    })
    .await
    {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    let (mut rows, total) = rows;
    let has_next = rows.len() > PAGE_SIZE;
    rows.truncate(PAGE_SIZE);
    let entries: Vec<Value> = rows.iter().map(view).collect();

    // Writing into a String cannot fail.
    let mut extra_params = String::new();
    for (name, value) in [
        ("key", &key),
        ("actor", &actor),
        ("action", &action),
        ("from", &from),
        ("to", &to),
    ] {
        if !value.is_empty() {
            let _ = write!(extra_params, "&{name}={}", quote(value));
        }
    }
    page(
        state.templates(),
        "audit.html",
        context! {
            current_page => "audit",
            entries,
            total,
            level => level.as_str(),
            is_off => level == AuditLevel::Off,
            actors => ACTORS,
            key,
            actor,
            action,
            from,
            to,
            problems,
            page => requested_page,
            has_next,
            extra_params,
        },
    )
}
