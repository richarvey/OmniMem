//! `/skills`: compiled skills, the gated New Skill flow, delete, and
//! transfer bundles (`web_ui/routes/skills.py`).
//!
//! Skills are build output, so there is no edit path: creating one runs the
//! same propose-and-accept gate as `compile_skill`, and changing one means
//! changing its memories and recompiling. Export downloads a checksummed zip
//! of the skill and its sources. Import validates
//! an uploaded bundle, previews exactly what it would add under a one-shot
//! token, and writes only on confirm, never overwriting anything.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use axum::Form;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use omnimem_engine::domains::{is_valid_domain, resolve_domain};
use omnimem_engine::skills::{INVALID_DOMAIN, SKILL_KEY_PREFIX, generated_skill_key};
use omnimem_engine::transfer::{ValidatedBundle, merge_feed_influences, validate_skill_import};
use omnimem_engine::{EngineError, SkillReview};
use omnimem_store::Fields;
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use crate::PanelState;
use crate::feeds_file;
use crate::files::{attachment, read_upload};
use crate::format::{minutes, timestamp};
use crate::pages::{blocking, is_web_url, quote, see_other, starting};
use crate::render::page;

const IMPORT_STASH_PREFIX: &str = "meta:skill:import:";
const IMPORT_STASH_TTL: Duration = Duration::from_mins(30);
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

/// A pending proposal as the Skills page lists it.
fn proposal_row(review: &SkillReview) -> Value {
    json!({
        "domain": review.domain,
        "skill_id": review.skill_id,
        "new_skill": review.new_skill,
        "description": review.description,
        "shown": review.shown,
        "added": review.added,
        "removed": review.removed,
        "rules": review.rules,
        "created_at": minutes(Some(&review.created_at.to_string())),
        "stale": review.stale,
    })
}

fn skill_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Html(r#"<p class="empty-state">Skill not found.</p>"#),
    )
        .into_response()
}

/// An htmx answer that sends the window to another page: `location` is a
/// panel path, which the browser reaches under `/admin`, as `see_other`'s.
fn hx_redirect(location: &str) -> Response {
    let target = format!("{}{location}", crate::ADMIN_ROOT);
    ([("HX-Redirect", target)], Html("")).into_response()
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

        // Every stashed proposal, whoever made it: the agent over MCP, the
        // auto scan or this window. They live under a TTL, so what is listed
        // can still be accepted unless it has gone stale.
        let proposals: Vec<Value> = engine
            .pending_skill_proposals()?
            .iter()
            .map(proposal_row)
            .collect();
        // Recent articles close to a skill, as the briefing lists them.
        let mut suggestions = engine.knowledge_watch()?;
        // A feed's link is only a link when it's a web address.
        for article in suggestions
            .iter_mut()
            .filter_map(|w| w["articles"].as_array_mut())
            .flatten()
        {
            let web = article["source_url"].as_str().is_some_and(is_web_url);
            article["source_is_web"] = web.into();
        }
        Ok((skills, states, proposals, suggestions))
    })
    .await;
    let (skills, states, proposals, suggestions) = match gathered {
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
            suggestions,
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

fn compile_result(state: &PanelState, result: &Value) -> Response {
    page(
        state.templates(),
        "skills/_compile_result.html",
        context! { result },
    )
}

fn compile_error(state: &PanelState, reason: &str) -> Response {
    compile_result(state, &json!({"status": "error", "reason": reason}))
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
        Ok(result) => compile_result(&state, &result),
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
    // The menu's and the dashboard's proposal counts are cached.
    state.invalidate_counts();
    match result {
        Ok(result) if result["status"] == "written" => hx_redirect(&format!(
            "/skills/{}",
            result["skill_id"].as_str().unwrap_or("")
        )),
        Ok(result) => compile_result(&state, &result),
        Err(failure) => failure,
    }
}

/// POST `/skills/discard`: throw a pending proposal away. The skill it would
/// have changed is untouched.
pub(crate) async fn discard(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let domain = match domain_from(&form, "Missing domain.") {
        Ok(domain) => domain,
        Err(reason) => return see_other(&format!("/skills?error={}", quote(&reason))),
    };
    let discarded = blocking(move || {
        let found = engine.discard_skill_proposal(&domain)?;
        if found {
            info!(domain, "discarded a skill proposal from the settings panel");
        }
        Ok(found)
    })
    .await;
    state.invalidate_counts();
    match discarded {
        Ok(true) => see_other(&format!("/skills?message={}", quote("Proposal discarded."))),
        Ok(false) => see_other(&format!(
            "/skills?error={}",
            quote("That proposal had already gone.")
        )),
        Err(failure) => failure,
    }
}

/// The article a suggestion form names, if it is a knowledge memory.
fn article_from(form: &FormData) -> Result<String, String> {
    let key = form.get("key").map_or("", |k| k.trim());
    if key.starts_with("mem:knowledge:") {
        Ok(key.to_owned())
    } else {
        Err("That suggestion isn't a knowledge article.".to_owned())
    }
}

/// What accepting a suggestion did, for the Skills page's message.
fn accepted_message(domain: &str, proposed: &Value) -> Result<String, String> {
    match proposed["status"].as_str() {
        Some("proposal") => Ok(format!(
            "Added to the {domain} skill's references. Its new draft is in Pending proposals: \
             accept it to write the skill."
        )),
        Some("unchanged") => Ok(format!(
            "Added to the {domain} skill's references. The skill already says the same, so \
             there's nothing new to accept."
        )),
        _ => Err(format!(
            "Added to the {domain} skill's references, but the skill couldn't be redrafted: {}",
            proposed["note"]
                .as_str()
                .or_else(|| proposed["reason"].as_str())
                .unwrap_or("the compiler gave no reason")
        )),
    }
}

/// POST `/skills/suggestions/accept`: promote a suggested article into a
/// skill's domain and draft the skill with it. The draft waits in Pending
/// proposals like any other, so the skill itself changes only when a person
/// accepts that.
pub(crate) async fn accept_suggestion(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let (domain, key) =
        match domain_from(&form, "Missing domain.").and_then(|d| Ok((d, article_from(&form)?))) {
            Ok(found) => found,
            Err(reason) => return see_other(&format!("/skills?error={}", quote(&reason))),
        };
    let outcome = blocking(move || {
        let promoted = engine.promote_knowledge(&key, Some(&domain), false, None)?;
        if let Some(error) = promoted["error"].as_str() {
            return Ok(Err(error.to_owned()));
        }
        info!(
            domain,
            article = key,
            "promoted a suggested article from the settings panel"
        );
        let proposed = refusal(engine.compile_skill(
            &domain,
            "propose",
            MIN_REINFORCEMENT,
            INCLUDE_GRAVEYARD,
            None,
            None,
        ))?;
        Ok(accepted_message(&domain, &proposed))
    })
    .await;
    state.invalidate_counts();
    match outcome {
        Ok(Ok(message)) => see_other(&format!(
            "/skills?message={}#pending-proposals",
            quote(&message)
        )),
        Ok(Err(reason)) => see_other(&format!("/skills?error={}", quote(&reason))),
        Err(failure) => failure,
    }
}

/// POST `/skills/suggestions/dismiss`: stop suggesting an article for a
/// skill, here and in the briefing. The article is untouched.
pub(crate) async fn dismiss_suggestion(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let (domain, key) =
        match domain_from(&form, "Missing domain.").and_then(|d| Ok((d, article_from(&form)?))) {
            Ok(found) => found,
            Err(reason) => return see_other(&format!("/skills?error={}", quote(&reason))),
        };
    let dismissed = blocking(move || engine.dismiss_knowledge_watch(&domain, &key)).await;
    state.invalidate_counts();
    match dismissed {
        Ok(_) => see_other(&format!(
            "/skills?message={}#skill-suggestions",
            quote("Suggestion dismissed. It won't be suggested for that skill again.")
        )),
        Err(failure) => failure,
    }
}

/// GET `/skills/candidates`: the domains that could become skills and what
/// each still needs, as a partial the Skills page loads after itself, since
/// working it out embeds every candidate's lessons.
pub(crate) async fn candidates(State(state): State<PanelState>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    match blocking(move || engine.skill_candidates()).await {
        Ok(candidates) => page(
            state.templates(),
            "skills/_candidates.html",
            context! { candidates },
        ),
        Err(failure) => failure,
    }
}

/// POST `/skills/scan`: run the automatic skill scan now. It only stashes
/// proposals, which wait in Pending proposals for a person.
pub(crate) async fn scan(State(state): State<PanelState>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let scanned = blocking(move || engine.scan_for_skills()).await;
    state.invalidate_counts();
    match scanned {
        Ok(scan) => {
            let domains: Vec<&str> = scan["proposals"]
                .as_array()
                .map(|p| p.iter().filter_map(|p| p["domain"].as_str()).collect())
                .unwrap_or_default();
            let message = match domains.as_slice() {
                [] => "Nothing new is ready to be a skill yet. Could become skills says what \
                       each domain still needs."
                    .to_owned(),
                [one] => format!("Drafted {one}. It's waiting in Pending proposals."),
                many => format!(
                    "Drafted {}. They're waiting in Pending proposals.",
                    many.join(", ")
                ),
            };
            see_other(&format!(
                "/skills?message={}#pending-proposals",
                quote(&message)
            ))
        }
        Err(failure) => failure,
    }
}

/// POST `/skills/candidates/draft`: propose a new skill for a candidate
/// domain, short of what the scan asks for. The draft waits in Pending
/// proposals; an existing skill is refused, as the New skill modal does.
pub(crate) async fn draft_candidate(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let domain = match domain_from(&form, "Missing domain.") {
        Ok(domain) => domain,
        Err(reason) => return see_other(&format!("/skills?error={}", quote(&reason))),
    };
    let result = blocking(move || {
        let skill_id = generated_skill_key(&domain, &engine.config().skill_user);
        if engine.store().get(&skill_id)?.is_some() {
            return Ok(Err(format!("There's already a skill for {domain}.")));
        }
        let proposed = refusal(engine.compile_skill(
            &domain,
            "propose",
            MIN_REINFORCEMENT,
            INCLUDE_GRAVEYARD,
            None,
            None,
        ))?;
        Ok(if proposed["status"] == "proposal" {
            info!(
                domain,
                "drafted a skill for a candidate domain from the settings panel"
            );
            Ok(format!(
                "Drafted {domain}. It's waiting in Pending proposals."
            ))
        } else {
            Err(format!(
                "{domain} couldn't be drafted: {}",
                proposed["note"]
                    .as_str()
                    .or_else(|| proposed["reason"].as_str())
                    .unwrap_or("the compiler gave no reason")
            ))
        })
    })
    .await;
    state.invalidate_counts();
    match result {
        Ok(Ok(message)) => see_other(&format!(
            "/skills?message={}#pending-proposals",
            quote(&message)
        )),
        Ok(Err(reason)) => see_other(&format!("/skills?error={}", quote(&reason))),
        Err(failure) => failure,
    }
}

/// POST `/skills/bless`: mark an episodic memory skill-eligible, so its
/// lesson clears the reinforcement gate on the next compile of its domains.
pub(crate) async fn bless(State(state): State<PanelState>, Form(form): Form<FormData>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let key = form.get("key").map_or("", |k| k.trim()).to_owned();
    if !key.starts_with("mem:episodic:") {
        return see_other(&format!(
            "/skills?error={}",
            quote("Only an episodic memory can be blessed.")
        ));
    }
    let blessed = blocking(move || engine.bless(&key)).await;
    match blessed {
        Ok(result) if result["status"] == "not_found" => {
            see_other(&format!("/skills?error={}", quote("That memory has gone.")))
        }
        Ok(_) => see_other(&format!(
            "/skills?message={}#skill-candidates",
            quote("Blessed. Its lesson now counts towards a skill without being seen twice.")
        )),
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
    Bundle(String, Vec<u8>),
    NotExportable(String),
}

/// GET `/skills/export/{key}`: the bundle as a download.
pub(crate) async fn export(State(state): State<PanelState>, Path(key): Path<String>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let built = blocking(move || {
        Ok(match engine.build_skill_export(&key)? {
            Ok(bundle) => {
                info!(
                    skill = key,
                    memories = bundle.memory_count,
                    missing = bundle.missing_sources.len(),
                    "exported a skill bundle"
                );
                Exported::Bundle(bundle.filename, bundle.data)
            }
            Err(reason) => Exported::NotExportable(reason),
        })
    })
    .await;
    match built {
        Ok(Exported::Bundle(filename, data)) => attachment(&filename, "application/zip", data),
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

fn import_result(state: &PanelState, result: &Value) -> Response {
    page(
        state.templates(),
        "skills/_import_result.html",
        context! { result },
    )
}

fn import_error(state: &PanelState, reason: &str) -> Response {
    import_result(state, &json!({"status": "error", "reason": reason}))
}

/// The reading list as it stands, or nothing when the app hasn't said where
/// it is. Reads the file, so it belongs on a worker.
fn current_feeds(feeds_path: Option<&std::path::Path>) -> Result<Vec<Map<String, Value>>, String> {
    feeds_path.map_or_else(|| Ok(Vec::new()), feeds_file::load)
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
    let feeds_path = state.feeds_path();
    let previewed = blocking(move || {
        let feeds = match current_feeds(feeds_path.as_deref()) {
            Ok(feeds) => feeds,
            Err(reason) => return Ok(Err(reason)),
        };
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
        Ok(Ok(result)) => import_result(&state, &result),
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
                warn!("no reading list is configured, so the bundle's feeds were left out");
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
