//! The settings panel: everything the 6.x web UI showed, served at
//! `$baseURL/admin` on the MCP server.
//!
//! The panel is an axum router whose routes are panel paths (`/memories`).
//! [`Panel::admin_router`] puts it behind sign-in at [`ADMIN_ROOT`] for the
//! server to mount (see `access.rs`); [`Panel::handle`] runs it bare, for
//! tests. Pages that need the engine show a starting page until the services
//! hand it over with [`Panel::set_engine`].
//!
//! The pages move across from `web_ui/` in slices; a route not ported yet
//! renders a page saying so.

mod access;
mod assets;
mod audit_log;
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
mod trust;
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

pub use access::{
    AccessConfig, PRINTED_CODE_LIFETIME, TRAY_CODE_LIFETIME, admin_panel_enabled,
    mint_sign_in_code, sign_in_url,
};
pub use configuration::{SecretStore, SettingsFileSecrets, secret_settings};

/// Where the panel is served on the HTTP server, `$baseURL/admin`. Routes
/// and the redirects handlers build are panel paths ("/memories"); links in
/// templates and the `Location` of a redirect add this in front.
pub const ADMIN_ROOT: &str = "/admin";

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
pub(crate) struct Caches {
    dashboard: Option<(Instant, Value)>,
    /// Duplicate scans by `(namespace, project)`, so an Archive that returns
    /// to the same scan doesn't compare every vector again.
    duplicates: std::collections::HashMap<(String, String), (Instant, Value)>,
    /// The menu's review counts.
    nav_counts: Option<(Instant, Value)>,
}

pub(crate) struct Shared {
    engine: RwLock<Option<Arc<Engine>>>,
    failure: RwLock<Option<String>>,
    feeds_path: RwLock<Option<PathBuf>>,
    static_overhead: RwLock<StaticOverhead>,
    settings_path: RwLock<Option<PathBuf>>,
    secret_store: RwLock<Option<Arc<dyn SecretStore>>>,
    /// A note from the app for every page (no tray icon, say).
    notice: Arc<RwLock<Option<String>>>,
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

    /// Forget the menu's and the dashboard's counts after a review action,
    /// so the page it lands on doesn't show the old numbers.
    pub(crate) fn invalidate_counts(&self) {
        let mut caches = self.caches();
        caches.nav_counts = None;
        caches.dashboard = None;
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
        let notice = Arc::new(RwLock::new(None));
        let state = PanelState(Arc::new(Shared {
            engine: RwLock::new(None),
            failure: RwLock::new(None),
            feeds_path: RwLock::new(None),
            static_overhead: RwLock::new(StaticOverhead::default()),
            settings_path: RwLock::new(None),
            secret_store: RwLock::new(None),
            notice: notice.clone(),
            templates: render::environment(notice),
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
            .route("/memory/{key}/trust", post(trust::memory_trust))
            .route("/memory/{key}/validity", post(detail::validity))
            .route("/unconfirmed", get(trust::list))
            .route("/unconfirmed/act", post(trust::act))
            .route("/unconfirmed/source", post(trust::confirm_source))
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
            .route("/skills/discard", post(skills::discard))
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
            .route("/duplicates/keep", post(management::keep_duplicate))
            .route("/contradictions", get(management::contradictions))
            .route("/nav-counts", get(management::nav_counts))
            .route(
                "/contradictions/dismiss-wording",
                post(management::dismiss_wording),
            )
            .route(
                "/contradictions/resolve",
                post(management::resolve_contradiction),
            )
            .route(
                "/contradictions/accept",
                post(management::accept_resolution),
            )
            .route(
                "/contradictions/discard",
                post(management::discard_resolution),
            )
            .route("/suppressions", get(management::suppressions))
            .route("/suppressions/add", post(management::suppress))
            .route("/suppressions/remove", post(management::unsuppress))
            .route("/audit", get(audit_log::list))
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
        let router = router
            .fallback(pages::pending)
            .layer(axum::middleware::from_fn(pages::remember_here))
            .with_state(state.clone());
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

    /// A note shown at the top of every page until the app goes, such as
    /// what to install when the desktop has nowhere to put the tray icon.
    pub fn set_notice(&self, message: String) {
        if let Ok(mut slot) = self.state.0.notice.write() {
            *slot = Some(message);
        }
    }

    /// Record why the services stopped, for the starting page to show.
    pub fn set_failure(&self, message: String) {
        if let Ok(mut slot) = self.state.0.failure.write() {
            *slot = Some(message);
        }
    }

    /// The panel as the HTTP server mounts it: `/admin` and everything under
    /// it, behind sign-in. `store` keeps the sessions and sign-in codes.
    pub fn admin_router(&self, store: Arc<omnimem_store::Store>, access: AccessConfig) -> Router {
        access::router(self.router.clone(), self.state.clone(), store, access)
    }

    /// Answer one request with the panel's routes and no sign-in, as the
    /// tests call it.
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
