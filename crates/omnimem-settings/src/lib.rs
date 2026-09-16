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
mod backups;
mod choices;
mod configuration;
mod create;
mod dashboard;
mod detail;
mod experience;
mod feeds;
mod feeds_file;
mod files;
mod format;
mod lifecycle;
mod management;
mod memories;
mod pages;
mod projects;
mod render;
mod search;
mod skills;
mod telemetry;
mod version;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
}

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{Request, Response};
use axum::routing::{get, post};
use minijinja::Environment;
use omnimem_engine::Engine;
use serde_json::Value;
use tower::ServiceExt;

pub use configuration::{SecretStore, secret_settings};

/// The largest response body the panel hands back (a backup download).
const MAX_RESPONSE_BYTES: usize = 512 * 1024 * 1024;

/// What OmniMem's MCP surface puts in an agent's context before any tool is
/// called, in characters.
///
/// The app measures it from the server (`omnimem_mcp::context_overhead`)
/// and hands it over, so this crate needs no MCP dependency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StaticOverhead {
    pub instructions_chars: usize,
    pub tool_count: usize,
    pub tool_schemas_chars: usize,
    pub deferred_names_chars: usize,
}

#[derive(Default)]
struct Caches {
    dashboard: Option<(Instant, Value)>,
    latest_release: Option<(Instant, Option<String>)>,
}

pub(crate) struct Shared {
    engine: RwLock<Option<Arc<Engine>>>,
    failure: RwLock<Option<String>>,
    feeds_path: RwLock<Option<PathBuf>>,
    downloads_dir: RwLock<Option<PathBuf>>,
    static_overhead: RwLock<StaticOverhead>,
    settings_path: RwLock<Option<PathBuf>>,
    secret_store: RwLock<Option<Arc<dyn SecretStore>>>,
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

    /// The reading list, once the app has said where it is.
    pub(crate) fn feeds_path(&self) -> Option<PathBuf> {
        self.0.feeds_path.read().ok().and_then(|p| p.clone())
    }

    /// Where exports are saved: the one set, or the user's Downloads folder.
    pub(crate) fn downloads_dir(&self) -> Option<PathBuf> {
        self.0
            .downloads_dir
            .read()
            .ok()
            .and_then(|d| d.clone())
            .or_else(|| {
                directories::UserDirs::new().and_then(|dirs| dirs.download_dir().map(PathBuf::from))
            })
    }

    pub(crate) fn static_overhead(&self) -> StaticOverhead {
        self.0
            .static_overhead
            .read()
            .map(|o| *o)
            .unwrap_or_default()
    }

    /// `omnimem.env`, when the app has a settings file (the desktop app).
    pub(crate) fn settings_path(&self) -> Option<PathBuf> {
        self.0.settings_path.read().ok().and_then(|p| p.clone())
    }

    pub(crate) fn secret_store(&self) -> Option<Arc<dyn SecretStore>> {
        self.0.secret_store.read().ok().and_then(|s| s.clone())
    }

    pub(crate) fn templates(&self) -> &Environment<'static> {
        &self.0.templates
    }

    pub(crate) fn caches(&self) -> std::sync::MutexGuard<'_, Caches> {
        self.0
            .caches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        Self::build(false)
    }

    /// A panel that also answers the desktop smoke test's `/_panel/*` routes.
    /// They echo request bodies and redirect wherever a form asks, so they
    /// exist only while the smoke test is running.
    pub fn with_smoke_routes() -> Self {
        Self::build(true)
    }

    fn build(smoke_routes: bool) -> Self {
        let state = PanelState(Arc::new(Shared {
            engine: RwLock::new(None),
            failure: RwLock::new(None),
            feeds_path: RwLock::new(None),
            downloads_dir: RwLock::new(None),
            static_overhead: RwLock::new(StaticOverhead::default()),
            settings_path: RwLock::new(None),
            secret_store: RwLock::new(None),
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
            .route("/skills", get(skills::list))
            .route("/skills/compile", post(skills::compile))
            .route("/skills/commit", post(skills::commit))
            .route("/skills/delete", post(skills::delete))
            .route(
                "/skills/import",
                // Bundles are capped at 20 MB; multipart framing adds a little.
                post(skills::import).layer(DefaultBodyLimit::max(24 * 1024 * 1024)),
            )
            .route("/skills/import/confirm", post(skills::import_confirm))
            .route("/skills/export/{key}", get(skills::export))
            .route("/skills/{key}", get(skills::detail))
            .route("/duplicates", get(management::duplicates))
            .route("/duplicates/scan", get(management::duplicates_scan))
            .route("/contradictions", get(management::contradictions))
            .route("/suppressions", get(management::suppressions))
            .route("/suppressions/add", post(management::suppress))
            .route("/suppressions/remove", post(management::unsuppress))
            .route("/telemetry", get(telemetry::telemetry))
            .route("/telemetry/refresh", get(telemetry::telemetry_refresh))
            .route("/token-overhead", get(telemetry::token_overhead))
            .route(
                "/token-overhead/refresh",
                get(telemetry::token_overhead_refresh),
            )
            .route(
                "/token-overhead/reset",
                post(telemetry::token_overhead_reset),
            )
            .route("/feeds", get(feeds::list))
            .route("/feeds/new", get(feeds::new_form).post(feeds::create))
            .route("/feeds/download", get(feeds::download))
            .route(
                "/feeds/upload",
                // A reading list is a few kilobytes; a large file is a mistake.
                post(feeds::upload).layer(DefaultBodyLimit::max(256 * 1024)),
            )
            .route(
                "/feeds/{index}/edit",
                get(feeds::edit_form).post(feeds::save),
            )
            .route("/feeds/{index}/delete", post(feeds::delete))
            .route("/backups", get(backups::list))
            .route("/backups/create", post(backups::create))
            .route(
                "/backups/upload",
                // Backups are capped at 100 MB, as the MCP restore tool caps them.
                post(backups::upload).layer(DefaultBodyLimit::max(101 * 1024 * 1024)),
            )
            .route("/backups/{filename}/preview", get(backups::preview))
            .route("/backups/{filename}/download", get(backups::download))
            .route("/backups/{filename}/restore", post(backups::restore))
            .route("/backups/{filename}/delete", post(backups::delete))
            .route(
                "/configuration",
                get(configuration::form).post(configuration::save),
            )
            .route("/static/{*path}", get(assets::serve));
        let router = if smoke_routes {
            router
                .route("/_panel/echo", post(pages::echo))
                .route("/_panel/redirect", post(pages::redirect))
                .route("/_panel/landed", get(pages::landed))
        } else {
            router
        };
        let router = router.fallback(pages::pending).with_state(state.clone());
        Self { state, router }
    }

    /// Hand over the engine once the services have opened it.
    pub fn set_engine(&self, engine: Arc<Engine>) {
        if let Ok(mut slot) = self.state.0.engine.write() {
            *slot = Some(engine);
        }
    }

    /// Where the reading list lives, for pages that read or change it.
    pub fn set_feeds_path(&self, path: PathBuf) {
        if let Ok(mut slot) = self.state.0.feeds_path.write() {
            *slot = Some(path);
        }
    }

    /// Save exports here instead of the user's Downloads folder.
    pub fn set_downloads_dir(&self, dir: PathBuf) {
        if let Ok(mut slot) = self.state.0.downloads_dir.write() {
            *slot = Some(dir);
        }
    }

    /// The measured size of the MCP instructions and tool schemas, for the
    /// token overhead page.
    pub fn set_static_overhead(&self, overhead: StaticOverhead) {
        if let Ok(mut slot) = self.state.0.static_overhead.write() {
            *slot = overhead;
        }
    }

    /// The settings file the configuration page edits. Without one the page
    /// says configuration belongs to the environment.
    pub fn set_settings_path(&self, path: PathBuf) {
        if let Ok(mut slot) = self.state.0.settings_path.write() {
            *slot = Some(path);
        }
    }

    /// Where the configuration page keeps secrets.
    pub fn set_secret_store(&self, store: Arc<dyn SecretStore>) {
        if let Ok(mut slot) = self.state.0.secret_store.write() {
            *slot = Some(store);
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
