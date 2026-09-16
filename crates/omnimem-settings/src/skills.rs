//! `/skills`: compiled skills, the gated New Skill flow, delete, and
//! transfer bundles (`web_ui/routes/skills.py`).
//!
//! Skills are build output, so there is no edit path: creating one runs the
//! same propose-and-accept gate as `compile_skill`, and changing one means
//! changing its memories and recompiling. Export writes a checksummed zip of
//! the skill and its sources into the Downloads folder (the window can't hand
//! a download to the person the same way on every platform). Import validates
//! an uploaded bundle, previews exactly what it would add under a one-shot
//! token, and writes only on confirm, never overwriting anything.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use axum::Form;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use minijinja::context;
use omnimem_engine::EngineError;
use omnimem_engine::domains::{is_valid_domain, resolve_domain};
use omnimem_engine::skills::{INVALID_DOMAIN, SKILL_KEY_PREFIX, generated_skill_key};
use omnimem_engine::transfer::{ValidatedBundle, merge_feed_influences, validate_skill_import};
use omnimem_store::Fields;
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use crate::PanelState;
use crate::feeds_file;
use crate::files::{read_upload, save_download};
use crate::format::{minutes, timestamp};
use crate::pages::{blocking, quote, see_other, starting};
use crate::render::page;

const PROPOSAL_PREFIX: &str = "meta:skill:proposal:";
const IMPORT_STASH_PREFIX: &str = "meta:skill:import:";
const IMPORT_STASH_TTL: Duration = Duration::from_secs(1800);
/// The compile defaults `compile_skill` and the 6.x page use.
const MIN_REINFORCEMENT: i64 = 2;
const INCLUDE_GRAVEYARD: bool = true;
/// Discovery metadata only: the body is read by the detail page alone.
const LIST_FIELDS: &[&str] = &[
    "name",
    "description",
    "domain",
    "user",
    "state",
    "generated",
    "compiled_at",
    "contract_version",
    "recall_count",
    "last_recalled",
    "created_at",
    "updated_at",
];

type FormData = HashMap<String, String>;

fn json_list(raw: Option<&String>) -> Vec<Value> {
    raw.filter(|r| !r.is_empty())
        .and_then(|r| serde_json::from_str::<Value>(r).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

fn short_name(key: &str) -> String {
    key.rsplit(':').next().unwrap_or(key).to_owned()
}

fn skill_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Html(r#"<p class="empty-state">Skill not found.</p>"#),
    )
        .into_response()
}

/// An htmx answer that sends the window to another page.
fn hx_redirect(location: &str) -> Response {
    ([("HX-Redirect", location)], Html("")).into_response()
}

pub(crate) async fn list(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let gathered = blocking(move || {
        let store = engine.store();
        let keys = store.scan_prefix(SKILL_KEY_PREFIX)?;
        let mut states: BTreeMap<&str, usize> =
            [("active", 0), ("deprioritised", 0), ("archived", 0)].into();
        let mut skills: Vec<Value> = Vec::new();
        for (key, row) in keys.iter().zip(store.get_fields_multi(&keys, LIST_FIELDS)?) {
            let row = row.unwrap_or_default();
            let state = row
                .get("state")
                .cloned()
                .unwrap_or_else(|| "active".to_owned());
            if let Some(n) = states.get_mut(state.as_str()) {
                *n += 1;
            }
            skills.push(json!({
                "key": key,
                "name": row.get("name").filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| short_name(key)),
                "description": row.get("description").cloned().unwrap_or_default(),
                "domain": row.get("domain").cloned().unwrap_or_default(),
                "state": state,
                "generated": row.get("generated").map(String::as_str) == Some("true"),
                "compiled_at": minutes(row.get("compiled_at")),
                "recall_count": row.get("recall_count").and_then(|c| c.trim().parse::<i64>().ok()).unwrap_or(0),
            }));
        }
        skills.sort_by_key(|s| s["name"].as_str().unwrap_or("").to_lowercase());

        // Proposals live under a TTL, so anything listed is still committable.
        let mut proposals: Vec<Value> = Vec::new();
        for key in store.scan_prefix(PROPOSAL_PREFIX)? {
            let row = store.hash_get_all(&key)?.unwrap_or_default();
            proposals.push(json!({
                "domain": row.get("domain").filter(|d| !d.is_empty()).cloned().unwrap_or_else(|| short_name(&key)),
                "created_at": minutes(row.get("created_at")),
                "key": key,
            }));
        }
        proposals.sort_by_key(|p| p["domain"].as_str().unwrap_or("").to_owned());
        Ok((skills, states, proposals))
    })
    .await;
    let (skills, states, proposals) = match gathered {
        Ok(found) => found,
        Err(failure) => return failure,
    };
    page(
        state.templates(),
        "skills/list.html",
        context! {
            current_page => "skills",
            message => query.get("message"),
            error => query.get("error"),
            total => skills.len(),
            skills,
            states,
            proposals,
        },
    )
}

pub(crate) async fn detail(State(state): State<PanelState>, Path(key): Path<String>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    if !key.starts_with(SKILL_KEY_PREFIX) {
        return skill_not_found();
    }
    let lookup = key.clone();
    let data = match blocking(move || Ok(engine.store().get(&lookup)?)).await {
        Ok(Some(data)) => data,
        Ok(None) => return skill_not_found(),
        Err(failure) => return failure,
    };
    let rules: Vec<Value> = json_list(data.get("rule_manifest"))
        .into_iter()
        .filter(Value::is_object)
        .collect();
    let mut rule_counts: Map<String, Value> = ["do", "watch", "dont", "ref", "feed"]
        .iter()
        .map(|k| ((*k).to_owned(), json!(0)))
        .collect();
    for rule in &rules {
        if let Some(n) = rule["kind"].as_str().and_then(|k| rule_counts.get_mut(k)) {
            *n = json!(n.as_i64().unwrap_or(0) + 1);
        }
    }
    let text = |field: &str| data.get(field).cloned().unwrap_or_default();
    let skill = json!({
        "key": key,
        "name": data.get("name").filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| short_name(&key)),
        "description": text("description"),
        "domain": text("domain"),
        "user": text("user"),
        "state": data.get("state").cloned().unwrap_or_else(|| "active".to_owned()),
        "generated": data.get("generated").map(String::as_str) == Some("true"),
        "contract_version": text("contract_version"),
        "compiled_at": timestamp(data.get("compiled_at")),
        "created_at": timestamp(data.get("created_at")),
        "updated_at": timestamp(data.get("updated_at")),
        "recall_count": data.get("recall_count").and_then(|c| c.trim().parse::<i64>().ok()).unwrap_or(0),
        "last_recalled": match data.get("last_recalled").filter(|l| !l.is_empty()) {
            Some(raw) => timestamp(Some(raw)),
            None => "Never".to_owned(),
        },
        "body": text("body"),
        "sources": json_list(data.get("source_manifest")).into_iter().filter(Value::is_string).collect::<Vec<_>>(),
        "rules": rules,
        "rule_counts": rule_counts,
    });
    page(
        state.templates(),
        "skills/detail.html",
        context! { skill, current_page => "skills" },
    )
}

fn compile_result(state: &PanelState, result: Value) -> Response {
    page(
        state.templates(),
        "skills/_compile_result.html",
        context! { result },
    )
}

fn compile_error(state: &PanelState, reason: &str) -> Response {
    compile_result(state, json!({"status": "error", "reason": reason}))
}

/// The submitted domain, canonical and valid, or the reason it isn't.
fn domain_from(form: &FormData, missing: &str) -> Result<String, String> {
    let domain = form.get("domain").map_or("", |d| d.trim());
    if domain.is_empty() {
        return Err(missing.to_owned());
    }
    let (canonical, _) = resolve_domain(domain);
    if is_valid_domain(&canonical) {
        Ok(canonical)
    } else {
        Err(INVALID_DOMAIN.to_owned())
    }
}

/// An engine refusal becomes the partial's error, anything else the error page.
fn refusal(result: omnimem_engine::Result<Value>) -> omnimem_engine::Result<Value> {
    match result {
        Err(EngineError::Invalid(reason)) => Ok(json!({"status": "error", "reason": reason})),
        other => other,
    }
}

/// POST `/skills/compile`: propose a draft for a new skill. An existing skill
/// is refused, so the modal can't turn into a recompile without its diff
/// review; recompiles stay on the MCP flow.
pub(crate) async fn compile(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let domain = match domain_from(&form, "Enter a domain, e.g. python.") {
        Ok(domain) => domain,
        Err(reason) => return compile_error(&state, &reason),
    };
    let result = blocking(move || {
        let skill_id = generated_skill_key(&domain, &engine.config().skill_user);
        if let Some(existing) = engine.store().get(&skill_id)? {
            return Ok(json!({
                "status": "exists",
                "name": existing.get("name").filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| short_name(&skill_id)),
                "skill_id": skill_id,
                "domain": domain,
            }));
        }
        refusal(engine.compile_skill(&domain, "propose", MIN_REINFORCEMENT, INCLUDE_GRAVEYARD, None, None))
    })
    .await;
    match result {
        Ok(result) => compile_result(&state, result),
        Err(failure) => failure,
    }
}

/// POST `/skills/commit`: write the reviewed proposal, then open the skill.
pub(crate) async fn commit(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let domain = match domain_from(&form, "Missing domain.") {
        Ok(domain) => domain,
        Err(reason) => return compile_error(&state, &reason),
    };
    let result = blocking(move || {
        refusal(engine.compile_skill(
            &domain,
            "write",
            MIN_REINFORCEMENT,
            INCLUDE_GRAVEYARD,
            None,
            None,
        ))
    })
    .await;
    match result {
        Ok(result) if result["status"] == "written" => hx_redirect(&format!(
            "/skills/{}",
            result["skill_id"].as_str().unwrap_or("")
        )),
        Ok(result) => compile_result(&state, result),
        Err(failure) => failure,
    }
}

pub(crate) async fn delete(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let key = form.get("key").map_or("", |k| k.trim()).to_owned();
    if !key.starts_with(SKILL_KEY_PREFIX) {
        return skill_not_found();
    }
    let deleted = blocking(move || {
        let found = engine.store().delete(&key)?;
        if found {
            info!(skill = key, "deleted a skill from the settings panel");
        }
        Ok(found)
    })
    .await;
    match deleted {
        Ok(true) => see_other("/skills"),
        Ok(false) => skill_not_found(),
        Err(failure) => failure,
    }
}

enum Exported {
    Saved(PathBuf, usize, usize),
    NotExportable(String),
    NotSaved(String),
}

/// GET `/skills/export/{key}`: save the bundle and say where it went.
pub(crate) async fn export(State(state): State<PanelState>, Path(key): Path<String>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let downloads = state.downloads_dir();
    let saved = blocking(move || {
        let bundle = match engine.build_skill_export(&key)? {
            Ok(bundle) => bundle,
            Err(reason) => return Ok(Exported::NotExportable(reason)),
        };
        Ok(
            match save_download(downloads, &bundle.filename, &bundle.data) {
                Ok(path) => {
                    info!(skill = key, path = %path.display(), memories = bundle.memory_count, "exported a skill bundle");
                    Exported::Saved(path, bundle.memory_count, bundle.missing_sources.len())
                }
                Err(problem) => Exported::NotSaved(problem),
            },
        )
    })
    .await;
    match saved {
        Ok(Exported::NotSaved(problem)) => see_other(&format!("/skills?error={}", quote(&problem))),
        Ok(Exported::Saved(path, memories, missing)) => {
            let mut message = format!(
                "Exported to {} with {memories} source memor{}.",
                path.display(),
                if memories == 1 { "y" } else { "ies" }
            );
            if missing > 0 {
                message.push_str(&format!(
                    " {missing} source{} no longer stored here and left out.",
                    if missing == 1 { " is" } else { "s are" }
                ));
            }
            see_other(&format!("/skills?message={}", quote(&message)))
        }
        Ok(Exported::NotExportable(reason)) => (
            StatusCode::NOT_FOUND,
            Html(format!(
                r#"<p class="empty-state">{}.</p>"#,
                reason.replace('&', "&amp;").replace('<', "&lt;")
            )),
        )
            .into_response(),
        Err(failure) => failure,
    }
}

fn import_result(state: &PanelState, result: Value) -> Response {
    page(
        state.templates(),
        "skills/_import_result.html",
        context! { result },
    )
}

fn import_error(state: &PanelState, reason: &str) -> Response {
    import_result(state, json!({"status": "error", "reason": reason}))
}

fn current_feeds(state: &PanelState) -> Result<Vec<Map<String, Value>>, String> {
    match state.feeds_path() {
        Some(path) => feeds_file::load(&path),
        None => Ok(Vec::new()),
    }
}

fn stash_bundle(bundle: &ValidatedBundle) -> Value {
    json!({
        "skill_key": bundle.skill_key,
        "skill_fields": bundle.skill_fields,
        "memories": bundle.memories,
        "feeds": bundle.feeds,
    })
}

fn unstash_bundle(raw: &str) -> Option<ValidatedBundle> {
    let value: Value = serde_json::from_str(raw).ok()?;
    Some(ValidatedBundle {
        skill_key: value["skill_key"].as_str()?.to_owned(),
        skill_fields: serde_json::from_value::<Fields>(value["skill_fields"].clone()).ok()?,
        memories: serde_json::from_value::<Vec<(String, Fields)>>(value["memories"].clone())
            .ok()?,
        feeds: serde_json::from_value::<Vec<Map<String, Value>>>(value["feeds"].clone()).ok()?,
        manifest: Value::Null,
        warnings: Vec::new(),
    })
}

/// POST `/skills/import`: validate an uploaded bundle and preview the plan.
/// Nothing is written but the one-shot stash the confirm step reads.
pub(crate) async fn import(State(state): State<PanelState>, mut multipart: Multipart) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let (filename, data) = match read_upload(&mut multipart, "file").await {
        Ok(Some(upload)) if !upload.0.is_empty() => upload,
        Ok(_) => return import_error(&state, "Choose a .zip bundle to upload."),
        Err(problem) => return import_error(&state, &problem),
    };
    if !filename.to_lowercase().ends_with(".zip") {
        return import_error(
            &state,
            "Only .zip bundles exported from the skills page are accepted.",
        );
    }
    let feeds = match current_feeds(&state) {
        Ok(feeds) => feeds,
        Err(reason) => return import_error(&state, &reason),
    };
    let previewed = blocking(move || {
        let bundle = match validate_skill_import(&data) {
            Ok(bundle) => bundle,
            Err(reason) => return Ok(Err(reason)),
        };
        let plan = engine.plan_skill_import(&bundle, Some(&feeds))?;
        let token = ulid::Ulid::generate().to_string();
        engine.store().string_set(
            &format!("{IMPORT_STASH_PREFIX}{token}"),
            &stash_bundle(&bundle).to_string(),
            Some(IMPORT_STASH_TTL),
        )?;
        let empty = |field: &str| plan[field].as_array().is_none_or(Vec::is_empty);
        let nothing_to_do = plan["skill_exists"] == true
            && empty("new_memories")
            && empty("new_feeds")
            && empty("updated_feeds");
        Ok(Ok(json!({
            "status": "preview",
            "token": token,
            "manifest": bundle.manifest,
            "warnings": bundle.warnings,
            "plan": plan,
            "nothing_to_do": nothing_to_do,
        })))
    })
    .await;
    match previewed {
        Ok(Ok(result)) => import_result(&state, result),
        Ok(Err(reason)) => import_error(&state, &reason),
        Err(failure) => failure,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// POST `/skills/import/confirm`: write exactly what was previewed.
pub(crate) async fn import_confirm(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let token = form.get("token").map_or("", |t| t.trim()).to_owned();
    let token_ok = (8..=64).contains(&token.len())
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !token_ok {
        return import_error(&state, "Invalid import token.");
    }
    let feeds_path = state.feeds_path();
    let outcome = blocking(move || {
        let stash_key = format!("{IMPORT_STASH_PREFIX}{token}");
        let Some(raw) = engine.store().string_get(&stash_key)? else {
            return Ok(Err(
                "This import preview has expired. Upload the bundle again.".to_owned(),
            ));
        };
        engine.store().kv_delete(&stash_key)?;
        let Some(bundle) = unstash_bundle(&raw) else {
            return Ok(Err(
                "The stored import bundle is unreadable. Upload the bundle again.".to_owned(),
            ));
        };
        let summary = engine.apply_skill_import(&bundle)?;
        // Imported episodic memories can carry abandoned approaches.
        engine.invalidate_abandoned_cache();

        // Bundled influences fold into the reading list additively: a feed
        // already there (by URL) at most gains the entry it lacked.
        let (mut added, mut updated) = (Vec::new(), Vec::new());
        if !bundle.feeds.is_empty() {
            if let Some(path) = &feeds_path {
                let current = feeds_file::load(path).map_err(EngineError::Io)?;
                let merged;
                (merged, added, updated, _) = merge_feed_influences(&current, &bundle.feeds);
                if !added.is_empty() || !updated.is_empty() {
                    feeds_file::save(path, &merged).map_err(EngineError::Io)?;
                    let mirrored: Vec<Value> = merged.into_iter().map(Value::Object).collect();
                    engine.sync_feed_influences(&mirrored)?;
                }
            } else {
                warn!("no reading list is configured, so the bundle's feeds were left out")
            }
        }

        let count = |field: &str| summary[field].as_array().map_or(0, Vec::len);
        let (written, skipped) = (count("memories_written"), count("memories_skipped"));
        let mut parts = vec![
            if summary["skill_written"] == true {
                "Skill imported".to_owned()
            } else {
                "Skill already existed (left untouched)".to_owned()
            },
            format!("{} added", plural(written, "memory", "memories")),
        ];
        if skipped > 0 {
            parts.push(format!("{skipped} already present (skipped)"));
        }
        if !added.is_empty() {
            parts.push(format!(
                "{} added to the reading list",
                plural(added.len(), "RSS feed", "RSS feeds")
            ));
        }
        if !updated.is_empty() {
            parts.push(format!(
                "{} gained influence",
                plural(updated.len(), "existing feed", "existing feeds")
            ));
        }
        Ok(Ok(format!("{}.", parts.join("; "))))
    })
    .await;
    match outcome {
        Ok(Ok(message)) => hx_redirect(&format!("/skills?message={}", quote(&message))),
        Ok(Err(reason)) => import_error(&state, &reason),
        Err(failure) => failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stashed_bundle_reads_back() {
        let bundle = ValidatedBundle {
            skill_key: "mem:skill:gen:rust-local".into(),
            skill_fields: Fields::from([("name".to_owned(), "rust-local".to_owned())]),
            memories: vec![(
                "mem:episodic:1".into(),
                Fields::from([("content".to_owned(), "x".to_owned())]),
            )],
            feeds: vec![
                json!({"url": "https://example.com"})
                    .as_object()
                    .cloned()
                    .unwrap(),
            ],
            manifest: json!({}),
            warnings: vec![],
        };
        let back = unstash_bundle(&stash_bundle(&bundle).to_string()).unwrap();
        assert_eq!(back.skill_key, bundle.skill_key);
        assert_eq!(back.skill_fields, bundle.skill_fields);
        assert_eq!(back.memories, bundle.memories);
        assert_eq!(back.feeds, bundle.feeds);
        assert!(unstash_bundle("{}").is_none());
    }
}
