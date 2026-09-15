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
use crate::pages::{blocking, see_other, starting};

type FormData = HashMap<String, String>;

/// A local path from `next`, or the fallback. Only same-site paths are
/// honoured, so a crafted form can't send the window somewhere else.
pub(crate) fn redirect_target(form: &FormData, fallback: String) -> String {
    match form.get("next") {
        Some(next) if next.starts_with('/') && !next.starts_with("//") => next.clone(),
        _ => fallback,
    }
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
    format!("/memory/{key}")
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
            // A transition that isn't allowed (already deleted, say) still deletes.
            if engine.transition(key, MemoryState::Deleted, None).is_err()
                && let Err(e) = engine.store().delete(key)
            {
                warn!(key, error = %e, "could not delete");
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
    }
}
