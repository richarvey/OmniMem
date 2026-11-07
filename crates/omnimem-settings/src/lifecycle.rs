//! `/lifecycle/*`: deprioritise, archive, reinstate and delete from the list
//! and detail pages (`web_ui/routes/lifecycle.py`). Each lands back on the
//! page the form named in `next`, or the memory.

use std::collections::HashMap;

use axum::Form;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use omnimem_engine::{Engine, MemoryState};
use omnimem_store::Fields;
use tracing::warn;

use crate::PanelState;
use crate::pages::{blocking, local_path, quote_segment, see_other, starting};

type FormData = HashMap<String, String>;

/// The namespaces a memory can be created in, and so the only keys the
/// delete action's fallback may remove from the store.
const WRITABLE: [&str; 4] = ["episodic", "knowledge", "preference", "project"];

/// A local path from `next`, or the fallback. Only panel paths are
/// honoured, so a crafted form can't send the window somewhere else.
pub(crate) fn redirect_target(form: &FormData, fallback: String) -> String {
    form.get("next")
        .and_then(|next| local_path(next))
        .unwrap_or(fallback)
}

/// Only a memory in a writable namespace may be removed outright; anything
/// else the form names (a project's skill, a log, a meta key) stays.
fn is_deletable_key(key: &str) -> bool {
    key.strip_prefix("mem:").is_some_and(|rest| {
        WRITABLE
            .iter()
            .any(|ns| rest.strip_prefix(ns).is_some_and(|id| id.starts_with(':')))
    })
}

async fn act(
    state: PanelState,
    headers: HeaderMap,
    form: FormData,
    fallback: impl FnOnce(&str) -> String,
    action: impl FnOnce(&Engine, &str, &FormData) -> bool + Send + 'static,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let key = form.get("key").cloned().unwrap_or_default();
    let target = redirect_target(&form, fallback(&key));
    let worker_key = key.clone();
    let done = match blocking(move || Ok(action(&engine, &worker_key, &form))).await {
        Ok(done) => done,
        Err(failure) => return failure,
    };
    // An htmx request removes its own row, so it wants no page back. A
    // refusal answers 409, which htmx doesn't swap, so the row stays.
    if headers.contains_key("hx-request") {
        let status = if done {
            StatusCode::OK
        } else {
            StatusCode::CONFLICT
        };
        return status.into_response();
    }
    see_other(&target)
}

fn to_memory(key: &str) -> String {
    format!("/memory/{}", quote_segment(key))
}

pub(crate) async fn deprioritise(
    State(state): State<PanelState>,
    headers: HeaderMap,
    Form(form): Form<FormData>,
) -> Response {
    act(state, headers, form, to_memory, |engine, key, form| {
        let reason = form
            .get("reason")
            .map_or("Deprioritised via web UI", String::as_str);
        if let Err(e) = engine.transition(key, MemoryState::Deprioritised, Some(reason)) {
            warn!(key, error = %e, "could not deprioritise");
            return false;
        }
        true
    })
    .await
}

pub(crate) async fn archive(
    State(state): State<PanelState>,
    headers: HeaderMap,
    Form(form): Form<FormData>,
) -> Response {
    act(state, headers, form, to_memory, |engine, key, _| {
        if let Err(e) = engine.transition(key, MemoryState::Archived, Some("Archived via web UI")) {
            warn!(key, error = %e, "could not archive");
            return false;
        }
        true
    })
    .await
}

pub(crate) async fn reinstate(
    State(state): State<PanelState>,
    headers: HeaderMap,
    Form(form): Form<FormData>,
) -> Response {
    act(state, headers, form, to_memory, |engine, key, _| {
        let reset = Fields::from([
            ("deprioritised_reason".to_owned(), String::new()),
            ("surface_score".to_owned(), "1.0".to_owned()),
        ]);
        let result = engine
            .transition(key, MemoryState::Active, None)
            .and_then(|_| Ok(engine.store().set_fields(key, &reset)?));
        if let Err(e) = result {
            warn!(key, error = %e, "could not reinstate");
            return false;
        }
        true
    })
    .await
}

pub(crate) async fn delete(
    State(state): State<PanelState>,
    headers: HeaderMap,
    Form(form): Form<FormData>,
) -> Response {
    act(
        state,
        headers,
        form,
        |_| "/memories".to_owned(),
        |engine, key, _| {
            // A transition that isn't allowed (already deleted, say) still
            // deletes, but only a memory: the fallback must not become a way
            // to remove any key the form names.
            if engine.transition(key, MemoryState::Deleted, None).is_err() {
                if !is_deletable_key(key) {
                    warn!(
                        key,
                        "refusing to delete a key outside the memory namespaces"
                    );
                    return false;
                }
                if let Err(e) = engine.store().delete(key) {
                    warn!(key, error = %e, "could not delete");
                    return false;
                }
            }
            engine.invalidate_abandoned_cache();
            true
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_same_site_paths_are_followed() {
        let form = |next: &str| FormData::from([("next".to_owned(), next.to_owned())]);
        let fallback = || "/memories".to_owned();
        assert_eq!(
            redirect_target(&form("/memories?page=2"), fallback()),
            "/memories?page=2"
        );
        assert_eq!(
            redirect_target(&form("//evil.example"), fallback()),
            "/memories"
        );
        assert_eq!(
            redirect_target(&form("https://evil.example"), fallback()),
            "/memories"
        );
        assert_eq!(redirect_target(&FormData::new(), fallback()), "/memories");
        assert_eq!(redirect_target(&form("/"), fallback()), "/");
        assert_eq!(
            redirect_target(&form("/\\evil.example"), fallback()),
            "/memories",
            "a backslash reads as a slash on Windows"
        );
        assert_eq!(
            redirect_target(&form("/memories\\..\\x"), fallback()),
            "/memories"
        );
        assert_eq!(
            redirect_target(&form("/memories\r\nX: y"), fallback()),
            "/memories",
            "no control characters"
        );
        assert_eq!(redirect_target(&form(""), fallback()), "/memories");
    }

    #[test]
    fn only_memories_in_writable_namespaces_can_be_removed_outright() {
        assert!(is_deletable_key("mem:episodic:01ABC"));
        assert!(is_deletable_key("mem:project:omnimem"));
        assert!(is_deletable_key("mem:knowledge:x"));
        assert!(is_deletable_key("mem:preference:x"));
        assert!(!is_deletable_key("mem:skill:gen:rust-ric"));
        assert!(!is_deletable_key("mem:episodic"));
        assert!(!is_deletable_key("mem:episodicx:1"));
        assert!(!is_deletable_key("meta:schema_version"));
        assert!(!is_deletable_key("log:recall:1"));
        assert!(!is_deletable_key(""));
    }
}
