//! Duplicates, contradictions and topic suppressions
//! (`web_ui/routes/duplicates.py`, `contradictions.py` and `suppressions.py`).

use std::collections::HashMap;

use axum::Form;
use axum::extract::{Query, State};
use axum::response::Response;
use chrono::{TimeZone, Utc};
use minijinja::context;
use omnimem_engine::{Engine, EngineError, ResolveRequest, Retire};
use serde_json::{Value, json};
use tracing::warn;

use crate::PanelState;
use crate::pages::{blocking, quote, see_other, starting};
use crate::render::page;

const NAMESPACES: [&str; 4] = ["episodic", "project", "knowledge", "preference"];
const MAINTENANCE_PREFIX: &str = "meta:maintenance:";

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

pub(crate) async fn contradictions(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let found = blocking(move || {
        Ok((
            engine.open_contradictions()?,
            engine.pending_resolutions()?,
            last_maintenance(&engine)?,
        ))
    })
    .await;
    match found {
        Ok((pairs, pending, last)) => page(
            state.templates(),
            "contradictions.html",
            context! {
                current_page => "contradictions",
                pairs,
                pending,
                last_maintenance => last,
                message => query.get("message"),
                error => query.get("error"),
            },
        ),
        Err(failure) => failure,
    }
}

fn form_value(form: &HashMap<String, String>, name: &str) -> String {
    form.get(name).map_or("", |v| v.trim()).to_owned()
}

/// Where the contradictions page goes after an action.
fn contradictions_notice(result: Result<String, String>) -> Response {
    match result {
        Ok(message) => see_other(&format!("/contradictions?message={}", quote(&message))),
        Err(problem) => see_other(&format!("/contradictions?error={}", quote(&problem))),
    }
}

/// An engine refusal (a pair that has gone, a bad choice) is shown on the
/// page; anything else is the error page.
fn notice(
    result: omnimem_engine::Result<String>,
) -> omnimem_engine::Result<Result<String, String>> {
    match result {
        Ok(message) => Ok(Ok(message)),
        Err(EngineError::Invalid(problem)) => Ok(Err(problem)),
        Err(e) => Err(e),
    }
}

/// POST `/contradictions/resolve`: a click is the person's say-so, so it
/// writes at once. `choice` is `a` or `b` (that one is current), `both` or
/// `not`.
pub(crate) async fn resolve_contradiction(
    State(state): State<PanelState>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let (key_a, key_b) = (form_value(&form, "key_a"), form_value(&form, "key_b"));
    let choice = form_value(&form, "choice");
    let reason = form_value(&form, "reason");
    // The box retires the loser, which only picking a side gives one to retire.
    let retire = form.contains_key("retire_loser");
    let done = blocking(move || {
        notice((|| {
            let (outcome, winner) = match choice.as_str() {
                "a" => ("supersedes", Some(key_a.as_str())),
                "b" => ("supersedes", Some(key_b.as_str())),
                "both" => ("both_valid", None),
                "not" => ("false_positive", None),
                _ => {
                    return Err(EngineError::Invalid(
                        "Choose how to resolve the pair.".into(),
                    ));
                }
            };
            let retire = if retire && winner.is_some() {
                Retire::Deprioritise
            } else {
                Retire::No
            };
            let request =
                ResolveRequest::new(&key_a, &key_b, outcome, winner, Some(&reason), retire)?;
            engine.resolve_contradiction(&request, "panel")?;
            Ok("Resolved.".to_owned())
        })())
    })
    .await;
    done.map_or_else(|failure| failure, contradictions_notice)
}

/// POST `/contradictions/accept`: write a proposal an agent left for a person.
pub(crate) async fn accept_resolution(
    State(state): State<PanelState>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let (key_a, key_b) = (form_value(&form, "key_a"), form_value(&form, "key_b"));
    let done = blocking(move || {
        notice(
            engine
                .accept_pending_resolution(&key_a, &key_b, "panel")
                .map(|accepted| match accepted {
                    Some(_) => "Resolved.".to_owned(),
                    None => "That proposal had already gone.".to_owned(),
                }),
        )
    })
    .await;
    done.map_or_else(|failure| failure, contradictions_notice)
}

/// POST `/contradictions/discard`: throw a proposal away.
pub(crate) async fn discard_resolution(
    State(state): State<PanelState>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let (key_a, key_b) = (form_value(&form, "key_a"), form_value(&form, "key_b"));
    let done = blocking(move || {
        notice(
            engine
                .discard_pending_resolution(&key_a, &key_b)
                .map(|found| {
                    if found {
                        "Proposal discarded.".to_owned()
                    } else {
                        "That proposal had already gone.".to_owned()
                    }
                }),
        )
    })
    .await;
    done.map_or_else(|failure| failure, contradictions_notice)
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
    let listed = blocking(move || {
        change(&engine)?;
        engine.list_suppressions()
    })
    .await;
    match listed {
        Ok(listed) => page(
            state.templates(),
            "suppressions.html",
            context! {
                current_page => "suppressions",
                topics => listed["suppressed_topics"].clone(),
                reasons => listed["reasons"].clone(),
            },
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
