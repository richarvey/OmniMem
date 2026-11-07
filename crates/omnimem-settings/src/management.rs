//! Duplicates, contradictions and topic suppressions
//! (`web_ui/routes/duplicates.py`, `contradictions.py` and `suppressions.py`).

use std::collections::HashMap;

use axum::Form;
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Response};
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

fn scan_namespace(query: &HashMap<String, String>) -> String {
    query
        .get("namespace")
        .filter(|n| NAMESPACES.contains(&n.as_str()))
        .cloned()
        .unwrap_or_else(|| "episodic".to_owned())
}

/// One namespace's clusters, from the stored vectors.
fn scan(engine: &Engine, namespace: &str, project: &str) -> omnimem_engine::Result<Value> {
    match engine.find_duplicates(namespace, None, Some(project)) {
        Ok(result) => Ok(result["clusters"].clone()),
        Err(EngineError::Invalid(problem)) => {
            warn!(problem, "duplicate scan refused");
            Ok(json!([]))
        }
        Err(e) => Err(e),
    }
}

/// GET `/duplicates`: the form, and with `scan=1` its results, so a scan has
/// an address: an Archive, or a memory's Back link, comes back to the same
/// clusters rather than an empty form.
pub(crate) async fn duplicates(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let namespace = scan_namespace(&query);
    // Only a usable project name narrows a scan, so a made-up one can't start
    // (and cache) a scan of its own.
    let project = query
        .get("project")
        .filter(|p| crate::projects::check_project_name(p).is_ok())
        .cloned()
        .unwrap_or_default();
    let scanned = query.get("scan").is_some_and(|s| s == "1");
    let fresh = query.get("fresh").is_some_and(|f| f == "1");
    let wanted = namespace.clone();
    let slot = (namespace.clone(), project.clone());
    // A scan compares every vector, so the page an Archive returns to reuses
    // the last one, minus what has been archived since. It keeps its own
    // age: archiving doesn't make an old scan new.
    let cached = state
        .caches()
        .duplicates
        .get(&slot)
        .filter(|(at, _)| at.elapsed() < SCAN_TTL && !fresh)
        .map(|(at, clusters)| (*at, clusters.clone()));
    let found = blocking(move || {
        let (at, clusters) = match (scanned, cached) {
            (false, _) => (std::time::Instant::now(), json!([])),
            (true, Some((at, clusters))) => (at, prune(&engine, &clusters)?),
            (true, None) => (std::time::Instant::now(), scan(&engine, &wanted, &project)?),
        };
        Ok((at, clusters, last_maintenance(&engine)?))
    })
    .await;
    let (at, clusters, last) = match found {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    if scanned {
        let mut caches = state.caches();
        caches
            .duplicates
            .retain(|_, (when, _)| when.elapsed() < SCAN_TTL);
        caches.duplicates.insert(slot, (at, clusters.clone()));
    }
    page(
        state.templates(),
        "duplicates.html",
        context! {
            current_page => "duplicates",
            clusters,
            namespace,
            scanned,
            scan_minutes => at.elapsed().as_secs() / 60,
            last_maintenance => last,
            message => query.get("message"),
            error => query.get("error"),
        },
    )
}

/// POST `/duplicates/keep`: keep one member of a cluster and archive the
/// rest (each `other`), then return to the scan.
pub(crate) async fn keep_duplicate(
    State(state): State<PanelState>,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let value = |name: &str| form.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let keep = value("key").unwrap_or_default();
    let target = value("next")
        .and_then(|n| crate::pages::local_path(&n))
        .unwrap_or_else(|| "/duplicates".to_owned());
    // The others are the rest of the scanned cluster this memory is in, never
    // whatever the form names: a form can't widen what gets archived.
    let others: Option<Vec<String>> = state
        .caches()
        .duplicates
        .values()
        .filter(|(at, _)| at.elapsed() < SCAN_TTL)
        .find_map(|(_, clusters)| {
            clusters.as_array()?.iter().find_map(|cluster| {
                let keys: Vec<String> = cluster["memories"]
                    .as_array()?
                    .iter()
                    .filter_map(|m| m["key"].as_str().map(str::to_owned))
                    .collect();
                keys.contains(&keep)
                    .then(|| keys.into_iter().filter(|k| *k != keep).collect())
            })
        });
    let Some(others) = others else {
        return see_other(&with_notice(
            &target,
            "error",
            "That scan has expired. Scan again, then choose which copy to keep.",
        ));
    };
    let done = blocking(move || {
        let live = engine
            .store()
            .get_fields_multi(std::slice::from_ref(&keep), &["state"])?
            .into_iter()
            .next()
            .flatten()
            .is_some_and(|row| {
                !matches!(
                    row.get("state").map(String::as_str),
                    Some("archived" | "deleted")
                )
            });
        if !live {
            return Ok(Err(
                "The copy to keep is no longer active, so nothing was archived.".to_owned(),
            ));
        }
        let reason = format!("Duplicate of {keep}, archived from the settings panel");
        let (mut archived, mut failed) = (0usize, 0usize);
        // A copy archived from its own row since the scan is already done.
        let states = engine.store().get_fields_multi(&others, &["state"])?;
        let others: Vec<&String> = others
            .iter()
            .zip(states)
            .filter(|(_, row)| {
                row.as_ref().is_some_and(|r| {
                    !matches!(
                        r.get("state").map(String::as_str),
                        Some("archived" | "deleted")
                    )
                })
            })
            .map(|(key, _)| key)
            .collect();
        for key in others {
            match engine.transition(key, omnimem_engine::MemoryState::Archived, Some(&reason)) {
                Ok(_) => archived += 1,
                Err(e) => {
                    warn!(key, error = %e, "could not archive a duplicate");
                    failed += 1;
                }
            }
        }
        Ok(if failed == 0 {
            Ok(format!("Kept one, archived {archived}."))
        } else {
            Err(format!(
                "Kept one, archived {archived}; {failed} could not be archived."
            ))
        })
    })
    .await;
    state.invalidate_counts();
    match done {
        Ok(Ok(message)) => see_other(&with_notice(&target, "message", &message)),
        Ok(Err(problem)) => see_other(&with_notice(&target, "error", &problem)),
        Err(failure) => failure,
    }
}

/// `url` with a one-off notice added to its query.
fn with_notice(url: &str, name: &str, text: &str) -> String {
    let joiner = if url.contains('?') { '&' } else { '?' };
    format!("{url}{joiner}{name}={}", quote(text))
}

/// A cached scan as it stands now: members archived or deleted since are
/// gone, and a cluster left with one member is settled.
fn prune(engine: &Engine, clusters: &Value) -> omnimem_engine::Result<Value> {
    let Some(list) = clusters.as_array() else {
        return Ok(json!([]));
    };
    let mut kept = Vec::new();
    for cluster in list {
        let members = cluster["memories"].as_array().cloned().unwrap_or_default();
        let keys: Vec<String> = members
            .iter()
            .filter_map(|m| m["key"].as_str().map(str::to_owned))
            .collect();
        let states = engine.store().get_fields_multi(&keys, &["state"])?;
        let live: Vec<Value> = members
            .into_iter()
            .zip(states)
            .filter(|(_, row)| {
                row.as_ref().is_some_and(|r| {
                    !matches!(
                        r.get("state").map(String::as_str),
                        Some("archived" | "deleted")
                    )
                })
            })
            .map(|(m, _)| m)
            .collect();
        if live.len() > 1 {
            let mut cluster = cluster.clone();
            cluster["memories"] = Value::Array(live);
            kept.push(cluster);
        }
    }
    Ok(Value::Array(kept))
}

/// GET `/duplicates/scan`: the htmx form of the same scan.
pub(crate) async fn duplicates_scan(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let namespace = scan_namespace(&query);
    let project = query.get("project").cloned().unwrap_or_default();
    let scanned = namespace.clone();
    match blocking(move || scan(&engine, &scanned, &project)).await {
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
        Ok((pairs, pending, last)) => {
            // Wording-only links come from the heuristic, which never disputes
            // anything; they are most of the list and mostly noise, so they
            // are shown apart from the ones a model confirmed.
            let (wording, pairs): (Vec<Value>, Vec<Value>) =
                pairs.into_iter().partition(is_wording_only);
            page(
                state.templates(),
                "contradictions.html",
                context! {
                    current_page => "contradictions",
                    pairs,
                    wording,
                    pending,
                    last_maintenance => last,
                    message => query.get("message"),
                    error => query.get("error"),
                },
            )
        }
        Err(failure) => failure,
    }
}

/// A pair only the heuristic linked: its explanation is one of the fixed
/// heuristic sentences.
fn is_wording_only(pair: &Value) -> bool {
    let explanation = pair["explanation"].as_str().unwrap_or("").trim();
    explanation == omnimem_store::HEURISTIC_LINK_EXPLANATION
        || explanation == omnimem_store::HEURISTIC_EXPLANATION
}

/// What waits on a person in Contradictions, counted as the menu, the
/// dashboard and the page all count it: pairs a model confirmed plus
/// resolutions an agent proposed, and, apart, the wording-only pairs.
pub(crate) fn contradiction_counts(engine: &Engine) -> omnimem_engine::Result<(usize, usize)> {
    let open = engine.open_contradictions()?;
    let wording = open.iter().filter(|p| is_wording_only(p)).count();
    Ok((
        open.len() - wording + engine.pending_resolutions()?.len(),
        wording,
    ))
}

/// POST `/contradictions/dismiss-wording`: settle every wording-only pair as
/// not a contradiction, in one click. The click is the person's say-so, as
/// for a single pair; a pair a model confirmed is never touched.
pub(crate) async fn dismiss_wording(
    State(state): State<PanelState>,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    // Only the pairs the page listed, so one linked after it was shown is
    // never dismissed unseen.
    let shown: std::collections::BTreeSet<(String, String)> = form
        .iter()
        .filter(|(k, _)| k == "pair")
        .filter_map(|(_, v)| v.split_once('|'))
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect();
    let done = blocking(move || {
        let (mut settled, mut skipped) = (0usize, 0usize);
        let open = engine.open_contradictions()?;
        for pair in &open {
            let (Some(a), Some(b)) = (pair["key_a"].as_str(), pair["key_b"].as_str()) else {
                continue;
            };
            if !shown.contains(&(a.to_owned(), b.to_owned())) {
                continue;
            }
            if !is_wording_only(pair) {
                skipped += 1;
                continue;
            }
            let request = ResolveRequest::new(
                a,
                b,
                "false_positive",
                None,
                Some("Wording only: dismissed from the settings panel"),
                Retire::No,
            )?;
            match engine.resolve_contradiction(&request, "panel") {
                Ok(_) => settled += 1,
                Err(EngineError::Invalid(problem)) => {
                    warn!(problem, a, b, "could not dismiss a pair");
                    skipped += 1;
                }
                Err(e) => return Err(e),
            }
        }
        let gone = shown.len().saturating_sub(settled + skipped);
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let mut message = format!("Dismissed {settled} wording-only pair{}.", plural(settled));
        if gone > 0 {
            message.push_str(&format!(" {gone} had already been settled."));
        }
        if skipped > 0 {
            message.push_str(&format!(
                " {skipped} changed since the page was shown and were left alone."
            ));
        }
        Ok(Ok(message))
    })
    .await;
    state.invalidate_counts();
    done.map_or_else(|failure| failure, contradictions_notice)
}

/// How long the menu's counts and a duplicate scan are reused.
const NAV_TTL: std::time::Duration = std::time::Duration::from_secs(30);
pub(crate) const SCAN_TTL: std::time::Duration = std::time::Duration::from_secs(300);

fn nav_badge(id: &str, n: usize, what: &str) -> String {
    let inner = if n > 0 {
        format!(
            r#"<span class="nav-count" title="{n} {what}{}">{n}</span>"#,
            if n == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    format!(r#"<span class="nav-badge" id="{id}" hx-swap-oob="true">{inner}</span>"#)
}

/// GET `/nav-counts`: every review count the menu shows, in one request
/// and reused for a short while, swapped into the menu out of band.
pub(crate) async fn nav_counts(State(state): State<PanelState>) -> Response {
    let Some(engine) = state.engine() else {
        return Html("").into_response();
    };
    let cached = state
        .caches()
        .nav_counts
        .clone()
        .filter(|(at, _)| at.elapsed() < NAV_TTL);
    let counts = if let Some((_, counts)) = cached {
        counts
    } else {
        let counted = blocking(move || {
            let (model, _) = contradiction_counts(&engine)?;
            Ok(json!({
                "unconfirmed": engine.store().count_unconfirmed()?,
                "contradictions": model,
                "proposals": engine
                    .pending_skill_proposals()?
                    .iter()
                    .filter(|p| !p.stale)
                    .count(),
            }))
        })
        .await;
        match counted {
            Ok(counts) => {
                state.caches().nav_counts = Some((std::time::Instant::now(), counts.clone()));
                counts
            }
            Err(_) => return Html("").into_response(),
        }
    };
    let n = |name: &str| usize::try_from(counts[name].as_u64().unwrap_or(0)).unwrap_or(0);
    Html(format!(
        "{}{}{}",
        nav_badge(
            "nav-count-unconfirmed",
            n("unconfirmed"),
            "unconfirmed fact"
        ),
        nav_badge(
            "nav-count-contradictions",
            n("contradictions"),
            "contradiction to settle"
        ),
        nav_badge("nav-count-skills", n("proposals"), "skill proposal waiting"),
    ))
    .into_response()
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
    state.invalidate_counts();
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
    state.invalidate_counts();
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
    state.invalidate_counts();
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
