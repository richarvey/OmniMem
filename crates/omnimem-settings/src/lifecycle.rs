//! `/lifecycle/*`: deprioritise, archive, reinstate and delete from the list
//! and detail pages (`web_ui/routes/lifecycle.py`). Each lands back on the
//! page the form named in `next`, or the memory.

use std::collections::HashMap;

use axum::Form;
use axum::extract::State;
use axum::response::Response;
use omnimem_engine::{Engine, MemoryState};
use omnimem_store::Fields;
use tracing::warn;

use crate::PanelState;
use crate::pages::{blocking, quote_segment, see_other, starting};

type FormData = HashMap<String, String>;

/// The namespaces a memory can be created in, and so the only keys the
/// delete action's fallback may remove from the store.
const WRITABLE: [&str; 4] = ["episodic", "knowledge", "preference", "project"];

/// A local path from `next`, or the fallback. Only same-site paths are
/// honoured, so a crafted form can't send the window somewhere else.
pub(crate) fn redirect_target(form: &FormData, fallback: String) -> String {
    match form.get("next") {
        Some(next) if is_local_path(next) => next.clone(),
        _ => fallback,
    }
}

/// A single leading `/` and nothing that could read as another site: `//`
/// is scheme-relative, and on Windows, where the panel is served over
/// `http://omnimem.localhost`, a browser reads `/\` the same way. Control
/// characters have no place in a path either.
fn is_local_path(path: &str) -> bool {
    let mut chars = path.chars();
    if chars.next() != Some('/') {
        return false;
    }
    match chars.next() {
        None => true,
        Some('/' | '\\') => false,
        Some(_) => !path.chars().any(|c| c == '\\' || c.is_control()),
    }
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
    form: FormData,
    fallback: impl FnOnce(&str) -> String,
    action: impl FnOnce(&Engine, &str, &FormData) + Send + 'static,
) -> Response {
    let Some(engine) = state.engine() else {
        return starting(&state);
    };
    let key = form.get("key").cloned().unwrap_or_default();
    let target = redirect_target(&form, fallback(&key));
    let worker_key = key.clone();
    if let Err(failure) = blocking(move || {
        action(&engine, &worker_key, &form);
        Ok(())
    })
    .await
    {
        return failure;
    }
    see_other(&target)
}

fn to_memory(key: &str) -> String {
    format!("/memory/{}", quote_segment(key))
}

pub(crate) async fn deprioritise(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    act(state, form, to_memory, |engine, key, form| {
        let reason = form
            .get("reason")
            .map_or("Deprioritised via web UI", String::as_str);
        if let Err(e) = engine.transition(key, MemoryState::Deprioritised, Some(reason)) {
            warn!(key, error = %e, "could not deprioritise");
        }
    })
    .await
}

pub(crate) async fn archive(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    act(state, form, to_memory, |engine, key, _| {
        if let Err(e) = engine.transition(key, MemoryState::Archived, None) {
            warn!(key, error = %e, "could not archive");
        }
    })
    .await
}

pub(crate) async fn reinstate(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    act(state, form, to_memory, |engine, key, _| {
        let reset = Fields::from([
            ("deprioritised_reason".to_owned(), String::new()),
            ("surface_score".to_owned(), "1.0".to_owned()),
        ]);
        let result = engine
            .transition(key, MemoryState::Active, None)
            .and_then(|_| Ok(engine.store().set_fields(key, &reset)?));
        if let Err(e) = result {
            warn!(key, error = %e, "could not reinstate");
        }
    })
    .await
}

pub(crate) async fn delete(
    State(state): State<PanelState>,
    Form(form): Form<FormData>,
) -> Response {
    act(
        state,
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
                    return;
                }
                if let Err(e) = engine.store().delete(key) {
                    warn!(key, error = %e, "could not delete");
                }
            }
            engine.invalidate_abandoned_cache();
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
