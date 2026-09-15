//! `/create`: store a memory by hand (`web_ui/routes/create.py`).
//!
//! Licence and provenance take the namespace defaults through the same
//! helpers as `remember`, and a near-duplicate is shown for a decision
//! rather than stored, unless "Force save" is ticked.

use std::collections::HashMap;

use axum::Form;
use axum::extract::State;
use axum::response::Response;
use minijinja::context;
use omnimem_core::{MemoryKey, Namespace};
use omnimem_engine::EngineError;
use omnimem_engine::classification::{
    licence_for_write, provenance_for_write, validate_licence_note,
};
use omnimem_engine::pyfmt::{now_str, py_json, round_to};
use omnimem_store::Fields;
use serde_json::{Value, json};
use tracing::info;

use crate::PanelState;
use crate::choices::{LICENCE_CHOICES, PROVENANCE_CHOICES};
use crate::pages::{blocking, see_other, starting};
use crate::render::page;

const NAMESPACES: [&str; 4] = ["episodic", "project", "knowledge", "preference"];

fn render_form(
    state: &PanelState,
    values: Value,
    error: Option<String>,
    duplicate: Option<Value>,
) -> Response {
    page(
        state.templates(),
        "create.html",
        context! {
            current_page => "create",
            error,
            duplicate,
            values,
            licence_classes => LICENCE_CHOICES,
            provenance_classes => PROVENANCE_CHOICES,
        },
    )
}

pub(crate) async fn form(State(state): State<PanelState>) -> Response {
    render_form(
        &state,
        json!({
            "content": "", "project": "", "namespace": "episodic", "tags": "",
            "force": false, "licence": "", "licence_note": "", "provenance": "",
        }),
        None,
        None,
    )
}

enum Created {
    Stored(String),
    Refused(String),
    Duplicate(Value),
}

pub(crate) async fn submit(
    State(state): State<PanelState>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let field = |name: &str| form.get(name).map_or("", |v| v.trim()).to_owned();
    let content = field("content");
    let project = field("project");
    let namespace = form
        .get("namespace")
        .cloned()
        .unwrap_or_else(|| "episodic".to_owned());
    let tags_raw = field("tags");
    let force = form.get("force").is_some_and(|f| f == "on");
    let (licence, licence_note, provenance) =
        (field("licence"), field("licence_note"), field("provenance"));
    let values = json!({
        "content": content,
        "project": project,
        "namespace": namespace,
        "tags": tags_raw,
        "force": force,
        "licence": licence,
        "licence_note": licence_note,
        "provenance": provenance,
    });
    if content.is_empty() {
        return render_form(
            &state,
            values,
            Some("Content cannot be empty.".to_owned()),
            None,
        );
    }
    let namespace = NAMESPACES
        .iter()
        .find(|ns| **ns == namespace)
        .copied()
        .unwrap_or("episodic");

    let outcome = blocking(move || {
        let classified = licence_for_write(Some(&licence), namespace).and_then(|mut fields| {
            if let Some(note) = validate_licence_note(Some(&licence_note))? {
                fields.insert("licence_note".to_owned(), note);
            }
            Ok((fields, provenance_for_write(Some(&provenance), namespace)?))
        });
        let (licence_fields, provenance_class) = match classified {
            Ok(found) => found,
            Err(EngineError::Invalid(message)) => return Ok(Created::Refused(message)),
            Err(e) => return Err(e),
        };
        let tags: Vec<String> = tags_raw
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect();
        let ns: Namespace = namespace
            .parse()
            .map_err(|e| EngineError::Invalid(format!("{e}")))?;
        let project = (!project.is_empty()).then_some(project);
        let vector = engine
            .embed_texts(&[content.as_str()])?
            .pop()
            .ok_or_else(|| EngineError::Embedding("no vector came back".to_owned()))?;

        if !force && let Some(dup) = engine.check_duplicate(ns, &vector, project.as_deref())? {
            return Ok(Created::Duplicate(json!({
                "key": dup.key,
                "content": dup.content.chars().take(200).collect::<String>(),
                "similarity": round_to(dup.similarity, 4),
            })));
        }

        let key = MemoryKey::generate(ns).to_string();
        let now = now_str();
        let mut fields = Fields::from([
            ("content".to_owned(), content),
            ("state".to_owned(), "active".to_owned()),
            ("surface_score".to_owned(), "1.0".to_owned()),
            ("experience_weight".to_owned(), "1.0".to_owned()),
            ("created_at".to_owned(), now.clone()),
            ("updated_at".to_owned(), now),
            ("tags".to_owned(), py_json(&json!(tags))),
            ("provenance".to_owned(), provenance_class.to_owned()),
        ]);
        fields.extend(licence_fields);
        if let Some(project) = project {
            if namespace == "project" {
                fields.insert("project_name".to_owned(), project.clone());
            }
            fields.insert("project".to_owned(), project);
        }
        engine.store().upsert(&key, &fields, Some(&vector))?;
        info!(%key, "created a memory from the settings panel");
        Ok(Created::Stored(key))
    })
    .await;

    match outcome {
        Ok(Created::Stored(key)) => see_other(&format!("/memory/{key}")),
        Ok(Created::Refused(message)) => render_form(&state, values, Some(message), None),
        Ok(Created::Duplicate(duplicate)) => render_form(&state, values, None, Some(duplicate)),
        Err(failure) => failure,
    }
}
