//! The settings panel: everything the 6.x web UI showed, rendered in-process
//! for the desktop app's window.
//!
//! The panel is an axum router that is never bound to a socket. The desktop
//! window's `omnimem://` scheme hands each request, page loads and htmx calls
//! alike, to [`Panel::handle`], so the pages keep their routes and templates
//! and nothing about them is reachable over a network. Pages that need the
//! engine show a starting page until the services hand it over with
//! [`Panel::set_engine`].
//!
//! The pages move across from `web_ui/` in slices; a route not ported yet
//! renders a page saying so.

mod assets;
mod choices;
mod create;
mod dashboard;
mod detail;
mod experience;
mod format;
mod lifecycle;
mod memories;
mod pages;
mod projects;
mod render;
mod search;
mod version;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
}

use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response};
use axum::routing::{get, post};
use minijinja::Environment;
use omnimem_engine::Engine;
use serde_json::Value;
use tower::ServiceExt;

/// The largest response body the panel hands back (a backup download).
const MAX_RESPONSE_BYTES: usize = 512 * 1024 * 1024;

#[derive(Default)]
struct Caches {
    dashboard: Option<(Instant, Value)>,
    latest_release: Option<(Instant, Option<String>)>,
}

pub(crate) struct Shared {
    engine: RwLock<Option<Arc<Engine>>>,
    failure: RwLock<Option<String>>,
    templates: Environment<'static>,
    caches: Mutex<Caches>,
}

/// Shared state for the routes.
#[derive(Clone)]
pub(crate) struct PanelState(Arc<Shared>);

impl PanelState {
    pub(crate) fn engine(&self) -> Option<Arc<Engine>> {
        self.0.engine.read().ok().and_then(|e| e.clone())
    }

    pub(crate) fn failure(&self) -> Option<String> {
        self.0.failure.read().ok().and_then(|f| f.clone())
    }

    pub(crate) fn templates(&self) -> &Environment<'static> {
        &self.0.templates
    }

    pub(crate) fn caches(&self) -> std::sync::MutexGuard<'_, Caches> {
        self.0.caches.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// The settings panel. Cheap to clone; every clone shares one engine handle.
#[derive(Clone)]
pub struct Panel {
    state: PanelState,
    router: Router,
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel {
    pub fn new() -> Self {
        let state = PanelState(Arc::new(Shared {
            engine: RwLock::new(None),
            failure: RwLock::new(None),
            templates: render::environment(),
            caches: Mutex::new(Caches::default()),
        }));
        let router = Router::new()
            .route("/", get(dashboard::handler))
            .route("/version-check", get(version::check))
            .route("/memories", get(memories::handler))
            .route("/memory/{key}", get(detail::handler))
            .route("/memory/{key}/tags", post(detail::retag))
            .route("/memory/{key}/licence", post(detail::licence))
            .route("/memory/{key}/provenance", post(detail::provenance))
            .route("/lifecycle/deprioritise", post(lifecycle::deprioritise))
            .route("/lifecycle/archive", post(lifecycle::archive))
            .route("/lifecycle/reinstate", post(lifecycle::reinstate))
            .route("/lifecycle/delete", post(lifecycle::delete))
            .route("/create", get(create::form).post(create::submit))
            .route("/search", get(search::form))
            .route("/search/results", get(search::results))
            .route("/projects", get(projects::list))
            .route(
                "/projects/new",
                get(projects::new_form).post(projects::create),
            )
            .route("/projects/{name}", get(projects::detail))
            .route(
                "/projects/{name}/edit",
                get(projects::edit_form).post(projects::save),
            )
            .route("/projects/{name}/delete", post(projects::delete))
            .route(
                "/projects/{name}/deprioritise",
                post(projects::deprioritise),
            )
            .route("/projects/{name}/reinstate", post(projects::reinstate))
            .route(
                "/projects/{name}/domains/suggest",
                post(projects::suggest_domains),
            )
            .route("/experience", get(experience::summary))
            .route("/experience/graveyard", get(experience::graveyard))
            .route("/static/{*path}", get(assets::serve))
            .route("/_panel/echo", post(pages::echo))
            .route("/_panel/redirect", post(pages::redirect))
            .route("/_panel/landed", get(pages::landed))
            .fallback(pages::pending)
            .with_state(state.clone());
        Self { state, router }
    }

    /// Hand over the engine once the services have opened it.
    pub fn set_engine(&self, engine: Arc<Engine>) {
        if let Ok(mut slot) = self.state.0.engine.write() {
            *slot = Some(engine);
        }
    }

    /// Record why the services stopped, for the starting page to show.
    pub fn set_failure(&self, message: String) {
        if let Ok(mut slot) = self.state.0.failure.write() {
            *slot = Some(message);
        }
    }

    /// Answer one request from the window.
    pub async fn handle(&self, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
        let request = request.map(Body::from);
        let response = match self.router.clone().oneshot(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        };
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, MAX_RESPONSE_BYTES)
            .await
            .map(|b| b.to_vec())
            .unwrap_or_default();
        Response::from_parts(parts, bytes)
    }
}
