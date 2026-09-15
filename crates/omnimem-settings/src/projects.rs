//! `/projects`: project contexts, their work-type domains, and the bulk
//! lifecycle actions (`web_ui/routes/projects.py`).
//!
//! A project listed without a context entry exists only as memories carrying
//! its name, so it has no detail page or actions. Domain suggestions only
//! propose: the human still presses Save.

use std::collections::{BTreeSet, HashMap};

use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use minijinja::context;
use omnimem_core::classification::{LICENCE_OWN, PROVENANCE_ASSERTED};
use omnimem_engine::domains::{DomainInput, normalise_domains};
use omnimem_engine::pyfmt::now_str;
use omnimem_engine::skills::GENERATED_SKILL_PREFIX;
use omnimem_engine::{Engine, EngineError};
use omnimem_store::Fields;
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use crate::PanelState;
use crate::format::{date_and_time, number, timestamp};
use crate::pages::{blocking, see_other, starting};
use crate::render::page;

type FormData = HashMap<String, String>;

fn context_key(name: &str) -> String {
    format!("mem:project:{name}")
}

fn read_domains(fields: &Fields) -> Vec<String> {
    normalise_domains(DomainInput::Text(
        fields.get("domains").map_or("", String::as_str),
    ))
    .domains
}

/// Domains that already have a compiled skill, so the pages can link to it.
fn skill_domains(engine: &Engine) -> omnimem_engine::Result<BTreeSet<String>> {
    let keys = engine.store().scan_prefix(GENERATED_SKILL_PREFIX)?;
    Ok(engine
        .store()
        .get_fields_multi(&keys, &["domain"])?
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("domain").filter(|d| !d.is_empty()).cloned())
        .collect())
}

/// The edit form's datalist: domains on projects plus compiled skills.
fn domain_options(engine: &Engine) -> omnimem_engine::Result<Vec<String>> {
    let mut all = skill_domains(engine)?;
    all.extend(engine.domain_map()?.keys().cloned());
    Ok(all.into_iter().collect())
}

fn project_not_found(state: &PanelState) -> Response {
    let mut response = page(
        state.templates(),
        "not_found.html",
        context! { current_page => "projects", message => "Project not found." },
    );
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

struct Listed {
    name: String,
    description: String,
    current_state: String,
    state: String,
    updated_at: f64,
    memory_count: usize,
    has_context: bool,
    domains: Vec<String>,
}

pub(crate) async fn list(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let raw_domain = query
        .get("domain")
        .map(|d| d.trim().to_owned())
        .unwrap_or_default();
    let wanted = (!raw_domain.is_empty())
        .then(|| {
            normalise_domains(DomainInput::Text(&raw_domain))
                .domains
                .into_iter()
                .next()
        })
        .flatten();
    let invalid_domain = !raw_domain.is_empty() && wanted.is_none();

    let found = blocking(move || {
        let keys = engine.store().scan_prefix("mem:project:")?;
        let rows = engine.store().get_multi(&keys)?;
        let mut projects: Vec<Listed> = Vec::new();
        for (key, data) in keys.iter().zip(rows) {
            let Some(data) = data else { continue };
            let filled = |field: &str| data.get(field).filter(|v| !v.is_empty()).cloned();
            let name = filled("project_name")
                .or_else(|| filled("project"))
                .unwrap_or_else(|| key.rsplit(':').next().unwrap_or("").to_owned());
            let index = match projects.iter().position(|p| p.name == name) {
                Some(i) => i,
                None => {
                    projects.push(Listed {
                        name: name.clone(),
                        description: String::new(),
                        current_state: String::new(),
                        state: "active".to_owned(),
                        updated_at: 0.0,
                        memory_count: 0,
                        has_context: false,
                        domains: Vec::new(),
                    });
                    projects.len() - 1
                }
            };
            let project = &mut projects[index];
            project.updated_at = project.updated_at.max(number(data.get("updated_at")));
            if filled("goals").is_some() || filled("stack").is_some() {
                let clip = |field: &str| {
                    data.get(field)
                        .map(|v| v.chars().take(120).collect())
                        .unwrap_or_default()
                };
                project.description = clip("description");
                project.current_state = clip("current_state");
                project.state = data
                    .get("state")
                    .cloned()
                    .unwrap_or_else(|| "active".to_owned());
                project.has_context = true;
                project.domains = read_domains(&data);
            } else {
                project.memory_count += 1;
            }
        }
        projects.sort_by(|a, b| b.updated_at.total_cmp(&a.updated_at));
        let known: Vec<(String, usize)> = engine
            .domain_map()?
            .iter()
            .map(|(domain, names)| (domain.clone(), names.len()))
            .collect();
        Ok((projects, known))
    })
    .await;
    let (projects, all_domains) = match found {
        Ok(found) => found,
        Err(failure) => return failure,
    };

    let total_projects = projects.len();
    let projects: Vec<Value> = projects
        .into_iter()
        .filter(|p| match &wanted {
            Some(domain) => p.domains.contains(domain),
            None => !invalid_domain,
        })
        .map(|p| {
            let (date, time) = date_and_time(p.updated_at);
            json!({
                "name": p.name,
                "description": p.description,
                "current_state": p.current_state,
                "state": p.state,
                "updated_at": p.updated_at,
                "memory_count": p.memory_count,
                "has_context": p.has_context,
                "domains": p.domains,
                "updated_date": date,
                "updated_time": time,
            })
        })
        .collect();
    let domain_filter = wanted.or_else(|| invalid_domain.then_some(raw_domain));
    page(
        state.templates(),
        "projects/list.html",
        context! {
            projects,
            current_page => "projects",
            domain_filter,
            domain_filter_valid => !invalid_domain,
            all_domains,
            total_projects,
        },
    )
}

pub(crate) async fn detail(State(state): State<PanelState>, Path(name): Path<String>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let found = blocking(move || {
        let Some(data) = engine.store().get(&context_key(&name))? else {
            return Ok(None);
        };
        let compiled = skill_domains(&engine)?;
        let text = |field: &str| data.get(field).cloned().unwrap_or_default();
        let project = json!({
            "name": data.get("project_name").cloned().unwrap_or(name),
            "description": text("description"),
            "stack": text("stack"),
            "domains": read_domains(&data)
                .into_iter()
                .map(|d| json!({"has_skill": compiled.contains(&d), "name": d}))
                .collect::<Vec<_>>(),
            "goals": text("goals"),
            "current_state": text("current_state"),
            "notes": text("notes"),
            "state": data.get("state").cloned().unwrap_or_else(|| "active".to_owned()),
            "created_at": timestamp(data.get("created_at")),
            "updated_at": timestamp(data.get("updated_at")),
        });
        Ok(Some((project, engine.config().skill_user.clone())))
    })
    .await;
    match found {
        Ok(Some((project, skill_user))) => page(
            state.templates(),
            "projects/detail.html",
            context! { project, current_page => "projects", skill_user },
        ),
        Ok(None) => project_not_found(&state),
        Err(failure) => failure,
    }
}

fn blank_project() -> Value {
    json!({
        "name": "", "description": "", "stack": "", "domains": "",
        "goals": "", "current_state": "", "notes": "",
    })
}

async fn edit_page(
    state: PanelState,
    engine: std::sync::Arc<Engine>,
    name: Option<String>,
) -> Response {
    let is_new = name.is_none();
    let found = blocking(move || {
        let options = domain_options(&engine)?;
        let Some(name) = name else {
            return Ok(Some((blank_project(), options)));
        };
        let Some(data) = engine.store().get(&context_key(&name))? else {
            return Ok(None);
        };
        let text = |field: &str| data.get(field).cloned().unwrap_or_default();
        Ok(Some((
            json!({
                "name": data.get("project_name").cloned().unwrap_or(name),
                "description": text("description"),
                "stack": text("stack"),
                "domains": read_domains(&data).join(","),
                "goals": text("goals"),
                "current_state": text("current_state"),
                "notes": text("notes"),
            }),
            options,
        )))
    })
    .await;
    match found {
        Ok(Some((project, domain_options))) => page(
            state.templates(),
            "projects/edit.html",
            context! { project, current_page => "projects", is_new, domain_options },
        ),
        Ok(None) => project_not_found(&state),
        Err(failure) => failure,
    }
}

pub(crate) async fn new_form(State(state): State<PanelState>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    edit_page(state, engine, None).await
}

pub(crate) async fn edit_form(
    State(state): State<PanelState>,
    Path(name): Path<String>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    edit_page(state, engine, Some(name)).await
}

/// Write a project context from the form, as `set_project_context` stamps
/// one: our own work, asserted by the human.
fn save_context(
    engine: &Engine,
    name: &str,
    form: &FormData,
    creating: bool,
) -> omnimem_engine::Result<()> {
    let field = |key: &str| form.get(key).map_or("", |v| v.trim()).to_owned();
    let (description, goals, current_state) =
        (field("description"), field("goals"), field("current_state"));
    let domains = normalise_domains(DomainInput::Text(&field("domains"))).domains;
    let embed_text = format!("{description} {goals} {current_state}");
    let vector = engine
        .embed_texts(&[embed_text.as_str()])?
        .pop()
        .ok_or_else(|| EngineError::Embedding("no vector came back".to_owned()))?;

    let key = context_key(name);
    let now = now_str();
    let mut fields = Fields::from([
        ("content".to_owned(), description.clone()),
        ("project_name".to_owned(), name.to_owned()),
        ("description".to_owned(), description),
        ("stack".to_owned(), field("stack")),
        ("domains".to_owned(), domains.join(",")),
        ("goals".to_owned(), goals),
        ("current_state".to_owned(), current_state),
        ("notes".to_owned(), field("notes")),
        ("state".to_owned(), "active".to_owned()),
        ("surface_score".to_owned(), "1.0".to_owned()),
        ("updated_at".to_owned(), now.clone()),
        ("licence".to_owned(), LICENCE_OWN.to_owned()),
        ("provenance".to_owned(), PROVENANCE_ASSERTED.to_owned()),
    ]);
    if creating || engine.store().get(&key)?.is_none() {
        fields.insert("created_at".to_owned(), now);
    }
    engine.store().upsert(&key, &fields, Some(&vector))?;
    engine.invalidate_domain_cache();
    info!(project = name, "saved a project from the settings panel");
    Ok(())
}

pub(crate) async fn create(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let name = form.get("name").map_or("", |n| n.trim()).to_owned();
    if name.is_empty() {
        return see_other("/projects/new");
    }
    let target = format!("/projects/{name}");
    match blocking(move || save_context(&engine, &name, &form, true)).await {
        Ok(()) => see_other(&target),
        Err(failure) => failure,
    }
}

pub(crate) async fn save(
    State(state): State<PanelState>,
    Path(name): Path<String>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let target = format!("/projects/{name}");
    match blocking(move || save_context(&engine, &name, &form, false)).await {
        Ok(()) => see_other(&target),
        Err(failure) => failure,
    }
}

pub(crate) async fn suggest_domains(
    State(state): State<PanelState>,
    Path(name): Path<String>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let found = blocking(move || {
        if engine.store().get(&context_key(&name))?.is_none() {
            return Ok(None);
        }
        Ok(Some(engine.suggest_domains_for_project(&name, 10)?))
    })
    .await;
    match found {
        Ok(Some(suggestion)) => {
            let evidence: Map<String, Value> = suggestion
                .evidence
                .iter()
                .map(|(domain, why)| (domain.clone(), json!(why)))
                .collect();
            page(
                state.templates(),
                "partials/domain_suggestion.html",
                context! {
                    suggestion => json!({
                        "existing_domains": suggestion.existing,
                        "suggested_domains": suggestion.suggested,
                        "merged_domains": suggestion.merged,
                        "evidence": evidence,
                    }),
                    value => suggestion.merged.join(","),
                },
            )
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Html(r#"<p class="empty-state">Project not found.</p>"#),
        )
            .into_response(),
        Err(failure) => failure,
    }
}

pub(crate) async fn delete(State(state): State<PanelState>, Path(name): Path<String>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let removed = blocking(move || {
        let key = context_key(&name);
        if engine.store().get(&key)?.is_none() {
            warn!(
                project = name,
                "delete requested for a project with no context"
            );
            return Ok(());
        }
        engine.store().delete(&key)?;
        engine.invalidate_domain_cache();
        info!(
            project = name,
            "deleted a project context from the settings panel"
        );
        Ok(())
    })
    .await;
    match removed {
        Ok(()) => see_other("/projects"),
        Err(failure) => failure,
    }
}

async fn bulk(state: PanelState, name: String, deprioritise: bool) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let outcome = blocking(move || {
        // The context entry moves with its memories, as the 6.x page did.
        let result = if deprioritise {
            engine.deprioritise_project(&name, true, Some("Deprioritised via web UI"), true)
        } else {
            engine.reinstate_project(&name, true, true)
        };
        match result {
            Ok(summary) => info!(project = name, total = %summary["total"], deprioritise, "changed a project's state"),
            Err(EngineError::Invalid(problem)) => warn!(project = name, problem, "could not change the project's state"),
            Err(e) => return Err(e),
        }
        Ok(())
    })
    .await;
    match outcome {
        Ok(()) => see_other("/projects"),
        Err(failure) => failure,
    }
}

pub(crate) async fn deprioritise(
    State(state): State<PanelState>,
    Path(name): Path<String>,
) -> Response {
    bulk(state, name, true).await
}

pub(crate) async fn reinstate(
    State(state): State<PanelState>,
    Path(name): Path<String>,
) -> Response {
    bulk(state, name, false).await
}
