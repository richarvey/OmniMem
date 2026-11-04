//! `/unconfirmed`: facts and preferences OmniMem concluded that nobody has
//! confirmed (#23), with Confirm and Reject beside each, and the trust form
//! on a memory's own page. Only a person gets here (the panel, or the CLI),
//! never an MCP call, so an agent can't vouch for its own conclusions. The
//! engine decides what each action may do; a refusal is shown, not hidden.

use std::collections::HashMap;

use axum::Form;
use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse, Response};
use minijinja::context;
use omnimem_engine::EngineError;
use omnimem_engine::trust::TrustAction;
use serde_json::{Value, json};

use crate::PanelState;
use crate::pages::{blocking, quote, see_other, starting};
use crate::render::page;

type FormData = HashMap<String, String>;

/// Candidates per page; there can be thousands.
const PAGE_SIZE: usize = 50;
/// A candidate's text is cut to this many characters in the list.
const CLIP: usize = 240;

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out.replace('\n', " ")
}

fn field(form: &FormData, name: &str) -> String {
    form.get(name).map_or("", |v| v.trim()).to_owned()
}

/// The list's address for a source filter and page.
fn list_url(source: &str, page: usize) -> String {
    let mut url = String::from("/unconfirmed");
    let mut sep = '?';
    if !source.is_empty() {
        url.push_str(&format!("{sep}source={}", quote(source)));
        sep = '&';
    }
    if page > 1 {
        url.push_str(&format!("{sep}page={page}"));
    }
    url
}

/// GET `/unconfirmed`: the candidates, newest first, grouped under the
/// memory each was extracted from.
pub(crate) async fn list(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let param = |name: &str| query.get(name).map_or("", |v| v.trim()).to_owned();
    let source = param("source");
    let requested = param("page").parse::<usize>().unwrap_or(1).max(1);
    let filter = source.clone();
    let found = blocking(move || {
        let all = engine.unconfirmed_from(
            Some(&filter).filter(|s| !s.is_empty()).map(String::as_str),
            usize::MAX,
        )?;
        let total = all.len();
        // How many each source has in all, for "Confirm all (n)".
        let mut per_source: HashMap<String, usize> = HashMap::new();
        for (_, row) in &all {
            *per_source
                .entry(row.get("enriched_from").cloned().unwrap_or_default())
                .or_default() += 1;
        }
        let pages = total.div_ceil(PAGE_SIZE).max(1);
        let current = requested.min(pages);
        let shown: Vec<_> = all
            .into_iter()
            .skip((current - 1) * PAGE_SIZE)
            .take(PAGE_SIZE)
            .collect();
        // What each source on this page says, to name it.
        let mut sources: Vec<String> = Vec::new();
        for (_, row) in &shown {
            let from = row.get("enriched_from").cloned().unwrap_or_default();
            if !from.is_empty() && !sources.contains(&from) {
                sources.push(from);
            }
        }
        let rows = engine
            .store()
            .get_fields_multi(&sources, &["content", "source_url", "feed_name"])?;
        let mut groups: Vec<Value> = Vec::new();
        for (from, row) in sources.iter().zip(rows) {
            let row = row.unwrap_or_default();
            let title = row
                .get("feed_name")
                .filter(|t| !t.is_empty())
                .or_else(|| row.get("content"))
                .map_or_else(String::new, |t| clip(t, 100));
            groups.push(json!({
                "source": from,
                "title": title,
                "exists": !row.is_empty(),
                "total": per_source.get(from).copied().unwrap_or(0),
                "url": list_url(from, 1),
                "items": [],
            }));
        }
        let mut loose: Vec<Value> = Vec::new();
        for (key, row) in shown {
            let get = |name: &str| row.get(name).cloned().unwrap_or_default();
            let item = json!({
                "key": key,
                "namespace": key.split(':').nth(1).unwrap_or("unknown"),
                "content": clip(&get("content"), CLIP),
                "project": get("project"),
                "state": get("state"),
            });
            let from = get("enriched_from");
            match groups.iter_mut().find(|g| g["source"] == from.as_str()) {
                Some(group) => {
                    if let Some(items) = group["items"].as_array_mut() {
                        items.push(item);
                    }
                }
                None => loose.push(item),
            }
        }
        Ok((groups, loose, total, current, pages))
    })
    .await;
    match found {
        Ok((groups, loose, total, current, pages)) => page(
            state.templates(),
            "unconfirmed.html",
            context! {
                current_page => "unconfirmed",
                groups,
                loose,
                total,
                page => current,
                pages,
                source,
                prev_url => list_url(&param("source"), current.saturating_sub(1)),
                next_url => list_url(&param("source"), current + 1),
                message => query.get("message"),
                error => query.get("error"),
            },
        ),
        Err(failure) => failure,
    }
}

/// Where the list goes after an action, back to the page it was on.
fn back(form: &FormData, notice: Result<String, String>) -> Response {
    let url = list_url(
        &field(form, "back_source"),
        field(form, "back_page").parse().unwrap_or(1),
    );
    let sep = if url.contains('?') { '&' } else { '?' };
    match notice {
        Ok(message) => see_other(&format!("{url}{sep}message={}", quote(&message))),
        Err(problem) => see_other(&format!("{url}{sep}error={}", quote(&problem))),
    }
}

/// An engine refusal is shown on the page; anything else is the error page.
fn shown<T>(result: omnimem_engine::Result<T>) -> omnimem_engine::Result<Result<T, String>> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(EngineError::Invalid(problem)) => Ok(Err(problem)),
        Err(e) => Err(e),
    }
}

fn reason_of(form: &FormData) -> Option<String> {
    Some(field(form, "reason")).filter(|r| !r.is_empty())
}

/// POST `/unconfirmed/act`: confirm or reject one candidate. A click is the
/// person's say-so, so it writes at once.
pub(crate) async fn act(State(state): State<PanelState>, Form(form): Form<FormData>) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let (key, action) = (field(&form, "key"), field(&form, "action"));
    let reason = reason_of(&form);
    let done = blocking(move || {
        shown((|| {
            let Some(action) = TrustAction::parse(&action) else {
                return Err(EngineError::Invalid("Choose what to do.".into()));
            };
            engine.set_trust(&key, action, reason.as_deref())?;
            Ok(match action {
                TrustAction::Confirm => "Confirmed.",
                TrustAction::Reject => "Rejected.",
                TrustAction::Restore => "Restored.",
            }
            .to_owned())
        })())
    })
    .await;
    done.map_or_else(|failure| failure, |notice| back(&form, notice))
}

/// POST `/unconfirmed/source`: confirm everything extracted from one memory.
pub(crate) async fn confirm_source(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let source = field(&form, "source");
    let reason = reason_of(&form);
    let done = blocking(move || {
        shown(
            engine
                .confirm_from_source(&source, reason.as_deref())
                .map(|n| match n {
                    0 => "Nothing from that memory was waiting.".to_owned(),
                    1 => "Confirmed 1 fact.".to_owned(),
                    n => format!("Confirmed {n} facts."),
                }),
        )
    })
    .await;
    done.map_or_else(|failure| failure, |notice| back(&form, notice))
}

/// POST `/memory/{key}/trust`: the buttons on a memory's own page.
pub(crate) async fn memory_trust(
    State(state): State<PanelState>,
    Path(key): Path<String>,
    Form(form): Form<FormData>,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let action = field(&form, "action");
    let reason = reason_of(&form);
    let target = key.clone();
    let done = blocking(move || {
        shown((|| {
            let Some(action) = TrustAction::parse(&action) else {
                return Err(EngineError::Invalid("Choose what to do.".into()));
            };
            engine.set_trust(&target, action, reason.as_deref())?;
            Ok(())
        })())
    })
    .await;
    match done {
        Ok(Ok(())) => see_other(&format!("/memory/{key}")),
        Ok(Err(problem)) => see_other(&format!("/memory/{key}?trust_error={}", quote(&problem))),
        Err(failure) => failure,
    }
}

/// GET `/unconfirmed/badge`: the sidebar's count of candidates, or nothing.
/// Loaded after the page, so a page never waits on the count.
pub(crate) async fn badge(State(state): State<PanelState>) -> Response {
    let Some(engine) = state.engine() else {
        return Html("").into_response();
    };
    let waiting = blocking(move || Ok(engine.unconfirmed_from(None, usize::MAX)?.len())).await;
    match waiting {
        Ok(n) if n > 0 => Html(format!(
            r#"<span class="nav-count" title="{n} unconfirmed fact{}">{n}</span>"#,
            if n == 1 { "" } else { "s" }
        ))
        .into_response(),
        _ => Html("").into_response(),
    }
}
