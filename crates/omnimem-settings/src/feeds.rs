//! `/feeds`: the reading list editor (`web_ui/routes/feeds.py`).
//!
//! Feeds carry a licence stamped on every article they ingest and optional
//! skill influence (`skills: {domain: score}`). Every change is written to
//! `feeds.yml`, which the RSS scheduler picks up from its modification time,
//! and mirrored into the influence hash the skill compiler reads.

use std::collections::HashMap;

use axum::Form;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use minijinja::context;
use omnimem_core::classification::LICENCE_UNKNOWN;
use omnimem_engine::Engine;
use omnimem_engine::classification::{
    note_for_reclassification, resolve_licence, validate_licence_note,
};
use omnimem_engine::feeds::validate_feed_skills;
use omnimem_engine::skills::SKILL_KEY_PREFIX;
use serde_json::{Map, Value, json};
use tracing::{error, info};

use crate::PanelState;
use crate::choices::LICENCE_CHOICES;
use crate::feeds_file;
use crate::files::{read_upload, save_download};
use crate::pages::{blocking, quote, see_other};
use crate::render::page;

type Feed = Map<String, Value>;
type FormPairs = Vec<(String, String)>;

fn with_error(path: &str, problem: &str) -> Response {
    see_other(&format!("{path}?error={}", quote(problem)))
}

/// The answer when the app hasn't said where the reading list lives.
fn no_reading_list() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Html(r#"<p class="empty-state">The reading list's location isn't known yet.</p>"#),
    )
        .into_response()
}

/// `str(value)` for a YAML scalar.
fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn text(feed: &Feed, field: &str) -> String {
    feed.get(field).map(scalar).unwrap_or_default()
}

/// `(class, note)` a feed declares, for display. Anything unparseable reads
/// as unknown, which is what the ingester would stamp.
fn feed_licence(feed: &Feed) -> (String, String) {
    let raw = feed.get("licence").map(scalar).unwrap_or_default();
    let (class, derived) = resolve_licence(&raw).unwrap_or((LICENCE_UNKNOWN, None));
    let note = text(feed, "licence_note").trim().to_owned();
    let note = if note.is_empty() {
        derived.unwrap_or("").to_owned()
    } else {
        note
    };
    (class.to_owned(), note)
}

fn declares_licence(feed: &Feed) -> bool {
    feed.get("licence")
        .is_some_and(|l| !matches!(l, Value::Null | Value::Bool(false)) && !scalar(l).is_empty())
}

/// A feed's skills, strongest first.
fn skill_pairs(feed: &Feed) -> Vec<(String, i64)> {
    let Some(Value::Object(skills)) = feed.get("skills") else {
        return Vec::new();
    };
    let mut pairs: Vec<(String, i64)> = skills
        .iter()
        .map(|(domain, score)| {
            (
                domain.clone(),
                scalar(score).trim().parse::<i64>().unwrap_or(0),
            )
        })
        .collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs
}

fn topics(feed: &Feed) -> String {
    feed.get("topics")
        .and_then(Value::as_array)
        .map(|list| list.iter().map(scalar).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

fn skill_domains(engine: Option<&Engine>) -> Vec<String> {
    let Some(engine) = engine else {
        return Vec::new();
    };
    let Ok(keys) = engine.store().scan_prefix(SKILL_KEY_PREFIX) else {
        return Vec::new();
    };
    let mut domains: Vec<String> = engine
        .store()
        .get_fields_multi(&keys, &["domain"])
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("domain").filter(|d| !d.is_empty()).cloned())
        .collect();
    domains.sort();
    domains.dedup();
    domains
}

/// Mirror the list for the skill compiler. A failure is logged: the file
/// was saved, and the next RSS cycle writes the mirror again.
fn sync_influence(engine: Option<&Engine>, feeds: &[Feed]) {
    let Some(engine) = engine else { return };
    let values: Vec<Value> = feeds.iter().cloned().map(Value::Object).collect();
    if let Err(e) = engine.sync_feed_influences(&values) {
        error!(error = %e, "could not mirror feed influence");
    }
}

pub(crate) async fn list(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(path) = state.feeds_path() else {
        return no_reading_list();
    };
    let (feeds, load_error) = match feeds_file::load(&path) {
        Ok(feeds) => (feeds, None),
        Err(problem) => (Vec::new(), Some(problem)),
    };
    let items: Vec<Value> = feeds
        .iter()
        .enumerate()
        .map(|(index, feed)| {
            json!({
                "index": index,
                "name": text(feed, "name"),
                "url": text(feed, "url"),
                "topics": topics(feed),
                "digest": text(feed, "mode") == "digest",
                "skills": skill_pairs(feed)
                    .iter()
                    .map(|(domain, score)| format!("{domain} ({score})"))
                    .collect::<Vec<_>>()
                    .join(", "),
                "licence": feed_licence(feed).0,
            })
        })
        .collect();
    page(
        state.templates(),
        "feeds/list.html",
        context! {
            feeds => items,
            current_page => "feeds",
            message => query.get("message"),
            error => load_error.or_else(|| query.get("error").cloned()),
        },
    )
}

fn edit_page(state: &PanelState, feed: Value, is_new: bool, error: Option<&String>) -> Response {
    let engine = state.engine();
    page(
        state.templates(),
        "feeds/edit.html",
        context! {
            feed,
            current_page => "feeds",
            is_new,
            skill_domains => skill_domains(engine.as_deref()),
            licence_classes => LICENCE_CHOICES,
            error,
        },
    )
}

pub(crate) async fn new_form(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let feed = json!({
        "name": "", "url": "", "topics": "", "digest": false, "skills": [],
        "licence": "", "licence_note": "",
    });
    edit_page(&state, feed, true, query.get("error"))
}

pub(crate) async fn edit_form(
    State(state): State<PanelState>,
    Path(index): Path<usize>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(path) = state.feeds_path() else {
        return no_reading_list();
    };
    let feeds = feeds_file::load(&path).unwrap_or_default();
    let Some(raw) = feeds.get(index) else {
        return (
            StatusCode::NOT_FOUND,
            Html(r#"<p class="empty-state">Feed not found.</p>"#),
        )
            .into_response();
    };
    let (class, note) = feed_licence(raw);
    let feed = json!({
        "index": index,
        "name": text(raw, "name"),
        "url": text(raw, "url"),
        "topics": topics(raw),
        "digest": text(raw, "mode") == "digest",
        "skills": skill_pairs(raw)
            .into_iter()
            .map(|(domain, influence)| json!({"domain": domain, "influence": influence}))
            .collect::<Vec<_>>(),
        // The form offers the classes; an identifier shows as its class with
        // the identifier in the note.
        "licence": if declares_licence(raw) { class } else { String::new() },
        "licence_note": note,
    });
    edit_page(&state, feed, false, query.get("error"))
}

fn form_value<'a>(form: &'a FormPairs, name: &str) -> &'a str {
    form.iter()
        .find(|(k, _)| k == name)
        .map_or("", |(_, v)| v.as_str())
}

/// The feed a form describes, validated. `current` is the feed being edited,
/// whose pre-filled licence note is dropped if the class changes.
fn feed_from_form(form: &FormPairs, current: Option<&Feed>) -> Result<Feed, String> {
    // Paired rows: a blank domain removes that association.
    let domains = form
        .iter()
        .filter(|(k, _)| k == "skill_domain")
        .map(|(_, v)| v);
    let scores = form
        .iter()
        .filter(|(k, _)| k == "skill_influence")
        .map(|(_, v)| v);
    let mut raw_skills = Map::new();
    for (domain, score) in domains.zip(scores) {
        let domain = domain.trim();
        if domain.is_empty() {
            continue;
        }
        let score = score.trim();
        raw_skills.insert(
            domain.to_owned(),
            json!(if score.is_empty() { "5" } else { score }),
        );
    }
    let skills = validate_feed_skills(Some(&Value::Object(raw_skills)))?;

    let licence = form_value(form, "licence").trim();
    let new_class = if licence.is_empty() {
        ""
    } else {
        resolve_licence(licence).map_err(|e| e.to_string())?.0
    };
    let mut note =
        validate_licence_note(Some(form_value(form, "licence_note"))).map_err(|e| e.to_string())?;
    if let Some(current) = current {
        let (old_class, old_note) = feed_licence(current);
        let old_class = if declares_licence(current) {
            old_class
        } else {
            String::new()
        };
        note = note_for_reclassification(&old_class, Some(&old_note), new_class, note);
    }

    let topics: Vec<String> = form_value(form, "topics")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect();
    let mut feed = Feed::new();
    feed.insert("url".into(), form_value(form, "url").trim().into());
    feed.insert("name".into(), form_value(form, "name").trim().into());
    feed.insert("topics".into(), json!(topics));
    if form_value(form, "digest") == "on" {
        feed.insert("mode".into(), "digest".into());
    }
    if !skills.is_empty() {
        let mapping: Map<String, Value> = skills
            .into_iter()
            .map(|(domain, score)| (domain, json!(score)))
            .collect();
        feed.insert("skills".into(), Value::Object(mapping));
    }
    if !licence.is_empty() {
        feed.insert("licence".into(), licence.into());
    }
    if let Some(note) = note {
        feed.insert("licence_note".into(), note.into());
    }
    Ok(feed)
}

/// Load, change and save the list, then mirror it.
async fn change_feeds(
    state: &PanelState,
    change: impl FnOnce(&mut Vec<Feed>) -> Option<Response> + Send + 'static,
) -> Option<Response> {
    let Some(path) = state.feeds_path() else {
        return Some(no_reading_list());
    };
    let engine = state.engine();
    let outcome = blocking(move || {
        let mut feeds = match feeds_file::load(&path) {
            Ok(feeds) => feeds,
            Err(problem) => return Ok(Some(with_error("/feeds", &problem))),
        };
        if let Some(response) = change(&mut feeds) {
            return Ok(Some(response));
        }
        if let Err(problem) = feeds_file::save(&path, &feeds) {
            return Ok(Some(with_error("/feeds", &problem)));
        }
        sync_influence(engine.as_deref(), &feeds);
        Ok(None)
    })
    .await;
    outcome.unwrap_or_else(Some)
}

pub(crate) async fn create(
    State(state): State<PanelState>,
    Form(form): Form<FormPairs>,
) -> Response {
    if form_value(&form, "name").trim().is_empty() || form_value(&form, "url").trim().is_empty() {
        return see_other("/feeds/new");
    }
    let feed = match feed_from_form(&form, None) {
        Ok(feed) => feed,
        Err(problem) => return with_error("/feeds/new", &problem),
    };
    let name = text(&feed, "name");
    match change_feeds(&state, move |feeds| {
        feeds.push(feed);
        None
    })
    .await
    {
        None => {
            info!(feed = name, "added a feed from the settings panel");
            see_other("/feeds")
        }
        Some(response) => response,
    }
}

pub(crate) async fn save(
    State(state): State<PanelState>,
    Path(index): Path<usize>,
    Form(form): Form<FormPairs>,
) -> Response {
    let edit_path = format!("/feeds/{index}/edit");
    if form_value(&form, "name").trim().is_empty() || form_value(&form, "url").trim().is_empty() {
        return see_other(&edit_path);
    }
    match change_feeds(&state, move |feeds| {
        let Some(current) = feeds.get(index) else {
            return Some(see_other("/feeds"));
        };
        match feed_from_form(&form, Some(current)) {
            Ok(feed) => {
                feeds[index] = feed;
                None
            }
            Err(problem) => Some(with_error(&edit_path, &problem)),
        }
    })
    .await
    {
        None => see_other("/feeds"),
        Some(response) => response,
    }
}

pub(crate) async fn delete(State(state): State<PanelState>, Path(index): Path<usize>) -> Response {
    match change_feeds(&state, move |feeds| {
        if index < feeds.len() {
            let removed = feeds.remove(index);
            info!(
                feed = text(&removed, "name"),
                "deleted a feed from the settings panel"
            );
        }
        None
    })
    .await
    {
        None => see_other("/feeds"),
        Some(response) => response,
    }
}

/// GET `/feeds/download`: save a copy of `feeds.yml` into Downloads.
pub(crate) async fn download(State(state): State<PanelState>) -> Response {
    let Some(path) = state.feeds_path() else {
        return no_reading_list();
    };
    let Ok(data) = std::fs::read(&path) else {
        return with_error("/feeds", "No feeds.yml file found");
    };
    match save_download(state.downloads_dir(), "feeds.yml", &data) {
        Ok(saved) => see_other(&format!(
            "/feeds?message={}",
            quote(&format!("Saved a copy to {}.", saved.display()))
        )),
        Err(problem) => with_error("/feeds", &problem),
    }
}

/// POST `/feeds/upload`: replace `feeds.yml` with a validated file.
pub(crate) async fn upload(State(state): State<PanelState>, mut multipart: Multipart) -> Response {
    let Some(path) = state.feeds_path() else {
        return no_reading_list();
    };
    let (filename, data) = match read_upload(&mut multipart, "file").await {
        Ok(Some(upload)) if !upload.0.is_empty() => upload,
        Ok(_) => return with_error("/feeds", "No file selected"),
        Err(problem) => return with_error("/feeds", &problem),
    };
    let lower = filename.to_lowercase();
    if !lower.ends_with(".yml") && !lower.ends_with(".yaml") {
        return with_error("/feeds", "Only .yml or .yaml files are accepted");
    }
    let Ok(config) = serde_yaml_ng::from_slice::<Value>(&data) else {
        return with_error("/feeds", "Invalid YAML file");
    };
    let Some(feeds) = config.as_object().and_then(|c| c.get("feeds")) else {
        return with_error("/feeds", "YAML must contain a top-level 'feeds' key");
    };
    let Some(feeds) = feeds.as_array() else {
        return with_error("/feeds", "'feeds' must be a list");
    };
    let feeds: Vec<Feed> = feeds
        .iter()
        .filter_map(|f| f.as_object().cloned())
        .collect();
    let written = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, &data));
    if let Err(e) = written {
        return with_error(
            "/feeds",
            &format!("Could not write {}: {e}", path.display()),
        );
    }
    sync_influence(state.engine().as_deref(), &feeds);
    info!(feeds = feeds.len(), filename, "uploaded a reading list");
    see_other(&format!(
        "/feeds?message={}",
        quote("Feeds config uploaded. The RSS scheduler picks it up automatically.")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(value: Value) -> Feed {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn a_declared_identifier_reads_as_its_class() {
        let declared = feed(json!({"licence": "cc-by-4.0"}));
        assert_eq!(
            feed_licence(&declared),
            ("open".to_owned(), "CC BY 4.0".to_owned())
        );
        let mistyped = feed(json!({"licence": false}));
        assert_eq!(feed_licence(&mistyped).0, "unknown");
        assert!(!declares_licence(&feed(json!({}))));
    }

    #[test]
    fn the_form_builds_a_feed_and_drops_a_stale_note() {
        let pairs = |items: &[(&str, &str)]| -> FormPairs {
            items
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        let form = pairs(&[
            ("name", "Rust"),
            ("url", "https://blog.rust-lang.org/feed.xml"),
            ("topics", "rust, language"),
            ("digest", "on"),
            ("licence", "restricted"),
            ("licence_note", "OGL v3.0"),
            ("skill_domain", "py"),
            ("skill_influence", "8"),
            ("skill_domain", ""),
            ("skill_influence", "5"),
        ]);
        let current = feed(json!({"licence": "open", "licence_note": "OGL v3.0"}));
        let built = feed_from_form(&form, Some(&current)).unwrap();
        assert_eq!(
            Value::Object(built),
            json!({
                "url": "https://blog.rust-lang.org/feed.xml",
                "name": "Rust",
                "topics": ["rust", "language"],
                "mode": "digest",
                "skills": {"python": 8},
                "licence": "restricted",
            })
        );
        let bad = pairs(&[("skill_domain", "rust"), ("skill_influence", "11")]);
        assert!(feed_from_form(&bad, None).is_err());
    }
}
