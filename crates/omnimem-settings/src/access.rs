//! Who may use the panel over HTTP, at `$baseURL/admin`.
//!
//! A person gets in one of two ways. With `OAUTH_ADMIN_USER` and
//! `OAUTH_ADMIN_PASSWORD` set, the login page takes them, the pair OAuth
//! sign-in already uses. Without them, a sign-in link does: the tray's
//! Settings item, or `omnimem admin-link` on a server, stores a random code
//! for a minute or two, and opening the link swaps it for a session once.
//! Either way the browser ends up holding a session cookie, and a session is
//! the only thing that opens a panel page: the MCP bearer token and OAuth
//! access tokens are never read here, so an agent can't reach the pages a
//! person approves skills and contradictions on.
//!
//! Codes and sessions live in the store's kv table as SHA-256 hashes with an
//! expiry, so a restart keeps people signed in, signing out ends a session
//! everywhere, and the command that prints a link can be another process.
//!
//! Every request also has to arrive over HTTPS (a proxy saying
//! `X-Forwarded-Proto: https`) or straight from this machine, unless
//! `ADMIN_ALLOW_HTTP` says otherwise, and a request that changes something
//! has to come from the panel's own pages (`Sec-Fetch-Site`, or `Origin`
//! where a browser doesn't send it). With the `SameSite=Strict` cookie that
//! is the CSRF defence: no other site can make the browser send one.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Form;
use axum::Router;
use axum::extract::{ConnectInfo, FromRequest, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use minijinja::context;
use omnimem_store::Store;
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use tracing::{info, warn};

use crate::ADMIN_ROOT;
use crate::pages::quote;
use crate::render::page;

/// The session cookie, scoped to the panel's path.
const COOKIE: &str = "omnimem_admin";
const SESSION_PREFIX: &str = "meta:admin:session:";
const CODE_PREFIX: &str = "meta:admin:code:";
/// How long the tray's sign-in link works: long enough for a browser to open.
pub const TRAY_CODE_LIFETIME: Duration = Duration::from_secs(60);
/// How long a link printed by `omnimem admin-link` works: long enough to
/// copy it out of a terminal on another machine.
pub const PRINTED_CODE_LIFETIME: Duration = Duration::from_secs(300);
/// Addresses the login limiter remembers before it forgets the oldest.
const LIMITER_ENTRIES: usize = 4096;

/// What the panel's sign-in needs, from the environment.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessConfig {
    /// `OAUTH_ADMIN_USER` and `OAUTH_ADMIN_PASSWORD`: the login page works
    /// only when both are set.
    pub credentials: Option<(String, String)>,
    /// `ADMIN_SESSION_HOURS`, in seconds.
    pub session_seconds: u64,
    /// `OAUTH_LOGIN_MAX_ATTEMPTS`: failed logins per address before the page
    /// refuses it for a while. 0 turns the limit off.
    pub max_attempts: usize,
    /// `OAUTH_LOGIN_WINDOW_SECONDS`
    pub window_seconds: u64,
    /// `ADMIN_ALLOW_HTTP`: serve the panel over plain HTTP to other machines,
    /// for a private network that encrypts on its own (WireGuard, Tailscale)
    /// or a container whose port is only published to localhost.
    pub allow_http: bool,
}

impl std::fmt::Debug for AccessConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessConfig")
            .field("user", &self.credentials.as_ref().map(|(user, _)| user))
            .field("session_seconds", &self.session_seconds)
            .field("max_attempts", &self.max_attempts)
            .field("window_seconds", &self.window_seconds)
            .field("allow_http", &self.allow_http)
            .finish_non_exhaustive()
    }
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self {
            credentials: None,
            session_seconds: 12 * 3600,
            max_attempts: 10,
            window_seconds: 900,
            allow_http: false,
        }
    }
}

fn setting(name: &str) -> Option<String> {
    omnimem_core::env::var(name)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn number(name: &str, default: u64, min: u64, max: u64) -> u64 {
    match setting(name).map(|v| v.parse::<u64>()) {
        None => default,
        Some(Ok(value)) => value.clamp(min, max),
        Some(Err(_)) => {
            warn!(
                setting = name,
                default, "not a whole number, using the default"
            );
            default
        }
    }
}

fn flag(name: &str) -> Option<bool> {
    setting(name).map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
}

impl AccessConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            credentials: setting("OAUTH_ADMIN_USER").zip(setting("OAUTH_ADMIN_PASSWORD")),
            session_seconds: number("ADMIN_SESSION_HOURS", 12, 1, 24 * 90) * 3600,
            max_attempts: number("OAUTH_LOGIN_MAX_ATTEMPTS", 10, 0, 1_000_000) as usize,
            window_seconds: number("OAUTH_LOGIN_WINDOW_SECONDS", 900, 0, 31_536_000),
            allow_http: flag("ADMIN_ALLOW_HTTP").unwrap_or(defaults.allow_http),
        }
    }
}

/// `ADMIN_PANEL`: whether the server serves the panel at all. On unless set
/// to false, as OmniMem Cloud sets it, where the public surface is MCP and
/// OAuth alone.
pub fn admin_panel_enabled() -> bool {
    flag("ADMIN_PANEL").unwrap_or(true)
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    // The OS random source failing is not something to limp on from.
    getrandom::fill(&mut bytes).expect("the OS random source");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hashed(prefix: &str, token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut key = String::with_capacity(prefix.len() + 64);
    key.push_str(prefix);
    for byte in digest {
        key.push_str(&format!("{byte:02x}"));
    }
    key
}

/// Store a single-use sign-in code that works for `lifetime`, and return it.
/// The caller puts it in a link to `/admin/sign-in?code=`.
pub fn mint_sign_in_code(store: &Store, lifetime: Duration) -> Result<String, String> {
    let code = random_token();
    store
        .string_set(&hashed(CODE_PREFIX, &code), "1", Some(lifetime))
        .map_err(|e| format!("could not store a sign-in code: {e}"))?;
    Ok(code)
}

/// The link that signs a browser in with `code`, under `base` (the server's
/// address, without `/admin`).
pub fn sign_in_url(base: &str, code: &str) -> String {
    format!(
        "{}{ADMIN_ROOT}/sign-in?code={code}",
        base.trim_end_matches('/')
    )
}

/// Failed logins per address in a sliding window.
#[derive(Default)]
struct Limiter {
    failures: HashMap<String, VecDeque<f64>>,
}

impl Limiter {
    fn prune(&mut self, ip: &str, at: f64, window: f64) {
        if let Some(times) = self.failures.get_mut(ip) {
            while times.front().is_some_and(|t| at - t >= window) {
                times.pop_front();
            }
            if times.is_empty() {
                self.failures.remove(ip);
            }
        }
    }

    fn is_blocked(&mut self, ip: &str, at: f64, max: usize, window: f64) -> bool {
        if max == 0 {
            return false;
        }
        self.prune(ip, at, window);
        self.failures.get(ip).is_some_and(|t| t.len() >= max)
    }

    fn record_failure(&mut self, ip: &str, at: f64, window: f64) {
        if self.failures.len() >= LIMITER_ENTRIES && !self.failures.contains_key(ip) {
            // Forget the address whose last failure is oldest.
            if let Some(oldest) = self
                .failures
                .iter()
                .min_by(|a, b| {
                    let last = |t: &VecDeque<f64>| t.back().copied().unwrap_or(0.0);
                    last(a.1).total_cmp(&last(b.1))
                })
                .map(|(ip, _)| ip.clone())
            {
                self.failures.remove(&oldest);
            }
        }
        self.prune(ip, at, window);
        self.failures
            .entry(ip.to_owned())
            .or_default()
            .push_back(at);
    }

    fn reset(&mut self, ip: &str) {
        self.failures.remove(ip);
    }
}

#[derive(Clone)]
struct Access {
    panel: Router,
    templates: crate::PanelState,
    store: Arc<Store>,
    config: Arc<AccessConfig>,
    limiter: Arc<Mutex<Limiter>>,
}

impl Access {
    fn limiter(&self) -> std::sync::MutexGuard<'_, Limiter> {
        self.limiter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The panel behind sign-in, at `/admin` and everything under it.
pub(crate) fn router(
    panel: Router,
    templates: crate::PanelState,
    store: Arc<Store>,
    config: AccessConfig,
) -> Router {
    let access = Access {
        panel,
        templates,
        store,
        config: Arc::new(config),
        limiter: Arc::default(),
    };
    Router::new()
        .route(ADMIN_ROOT, any(handle))
        .route(&format!("{ADMIN_ROOT}/"), any(handle))
        .route(&format!("{ADMIN_ROOT}/{{*rest}}"), any(handle))
        .with_state(access)
}

/// The connecting address. Behind a proxy it is the proxy's.
fn peer(request: &Request) -> Option<IpAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Whether a proxy in front says the browser used HTTPS.
fn forwarded_https(headers: &HeaderMap) -> bool {
    let proto = header_str(headers, "x-forwarded-proto")
        .and_then(|v| v.split(',').next())
        .map(str::trim);
    if proto.is_some_and(|p| p.eq_ignore_ascii_case("https")) {
        return true;
    }
    header_str(headers, "forwarded").is_some_and(|v| {
        v.split(',').next().is_some_and(|first| {
            first.split(';').any(|part| {
                part.trim().split_once('=').is_some_and(|(k, v)| {
                    k.trim().eq_ignore_ascii_case("proto")
                        && v.trim().trim_matches('"').eq_ignore_ascii_case("https")
                })
            })
        })
    })
}

fn proxied(headers: &HeaderMap) -> bool {
    [
        "x-forwarded-for",
        "x-forwarded-proto",
        "forwarded",
        "x-real-ip",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
}

/// HTTPS through a proxy, or straight from this machine: anything else would
/// carry the password and the session cookie in the clear.
fn transport_allowed(request: &Request, allow_http: bool) -> bool {
    let headers = request.headers();
    allow_http
        || forwarded_https(headers)
        || (!proxied(headers) && peer(request).is_some_and(|ip| ip.is_loopback()))
}

/// A request that changes something must come from the panel's own pages.
/// Browsers send `Sec-Fetch-Site` on every request and nothing else can set
/// it from a page; older ones send `Origin` on every POST.
fn same_origin(headers: &HeaderMap) -> bool {
    if let Some(site) = header_str(headers, "sec-fetch-site") {
        return site == "same-origin";
    }
    let (Some(origin), Some(host)) = (header_str(headers, "origin"), header_str(headers, "host"))
    else {
        return false;
    };
    origin
        .split_once("://")
        .is_some_and(|(_, authority)| authority.trim_end_matches('/').eq_ignore_ascii_case(host))
}

fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value.to_owned())
        .filter(|value| !value.is_empty())
}

fn cookie(value: &str, max_age: u64, secure: bool) -> String {
    format!(
        "{COOKIE}={value}; Path={ADMIN_ROOT}; Max-Age={max_age}; HttpOnly; SameSite=Strict{}",
        if secure { "; Secure" } else { "" }
    )
}

/// A panel path that is safe to send the browser to after signing in.
fn local_next(next: Option<&str>) -> String {
    match next {
        Some(next)
            if next.starts_with('/')
                && !next.starts_with("//")
                && !next.contains('\\')
                && !next.chars().any(char::is_control) =>
        {
            next.to_owned()
        }
        _ => "/".to_owned(),
    }
}

fn redirect(location: &str) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}

fn query_param(request: &Request, name: &str) -> Option<String> {
    Query::<HashMap<String, String>>::try_from_uri(request.uri())
        .ok()
        .and_then(|Query(mut query)| query.remove(name))
}

/// Headers every panel response carries: no framing, no sniffing, no
/// referrer (a sign-in link must never leave in one), and the same Content
/// Security Policy the pages declare.
fn harden(mut response: Response, is_static: bool) -> Response {
    let headers = response.headers_mut();
    for (name, value) in [
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CONTENT_SECURITY_POLICY, crate::render::CSP),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    if !is_static && !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

async fn handle(State(access): State<Access>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let inner = path
        .strip_prefix(ADMIN_ROOT)
        .filter(|rest| !rest.is_empty())
        .unwrap_or("/")
        .to_owned();
    let is_static = inner.starts_with("/static/");
    let response = route(&access, request, &inner).await;
    harden(response, is_static)
}

async fn route(access: &Access, request: Request, inner: &str) -> Response {
    if !transport_allowed(&request, access.config.allow_http) {
        warn!(
            peer = ?peer(&request),
            "refused the settings panel over plain HTTP from another machine"
        );
        return login_page(
            access,
            StatusCode::FORBIDDEN,
            "",
            Some(
                "The settings panel only opens over HTTPS, or from this machine. Put OmniMem behind a proxy that serves HTTPS, or set ADMIN_ALLOW_HTTP=true if the network between you and OmniMem is already private.",
            ),
        );
    }
    let changes = !matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    if changes && !same_origin(request.headers()) {
        warn!(
            path = inner,
            "refused a panel request that didn't come from the panel"
        );
        return (
            StatusCode::FORBIDDEN,
            "Forbidden: this request didn't come from the settings panel",
        )
            .into_response();
    }
    if inner.starts_with("/static/") {
        return forward(access, request, inner).await;
    }
    match (request.method().clone(), inner) {
        (Method::GET, "/login") => {
            let next = local_next(query_param(&request, "next").as_deref());
            let error = query_param(&request, "error");
            login_page(access, StatusCode::OK, &next, error.as_deref())
        }
        (Method::POST, "/login") => login(access, request).await,
        (Method::GET, "/sign-in") => sign_in(access, request).await,
        (Method::POST, "/logout") => logout(access, request.headers().clone()).await,
        _ => {
            if signed_in(access, request.headers()).await {
                forward(access, request, inner).await
            } else {
                unauthenticated(&request, inner)
            }
        }
    }
}

/// Run the panel's own router on the request, with `/admin` taken off. The
/// request is rebuilt rather than passed on, so the path parameters this
/// router matched don't reach the panel's own (`Path` would see two), and
/// only the connecting address carries across.
async fn forward(access: &Access, request: Request, inner: &str) -> Response {
    let (parts, body) = request.into_parts();
    let target = match parts.uri.query() {
        Some(query) => format!("{inner}?{query}"),
        None => inner.to_owned(),
    };
    let mut forwarded = Request::new(body);
    match target.parse() {
        Ok(uri) => *forwarded.uri_mut() = uri,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    }
    *forwarded.method_mut() = parts.method;
    *forwarded.version_mut() = parts.version;
    *forwarded.headers_mut() = parts.headers;
    if let Some(peer) = parts.extensions.get::<ConnectInfo<SocketAddr>>() {
        forwarded.extensions_mut().insert(*peer);
    }
    match access.panel.clone().oneshot(forwarded).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

fn unauthenticated(request: &Request, inner: &str) -> Response {
    let next = match request.uri().query() {
        Some(query) => format!("{inner}?{query}"),
        None => inner.to_owned(),
    };
    let login = format!("{ADMIN_ROOT}/login?next={}", quote(&next));
    if request.headers().contains_key("hx-request") {
        // htmx follows a redirect inside the fragment it is swapping, which
        // would drop the login page into the middle of a page.
        let mut response = StatusCode::UNAUTHORIZED.into_response();
        if let Ok(value) = HeaderValue::from_str(&login) {
            response.headers_mut().insert("hx-redirect", value);
        }
        return response;
    }
    if request.method() == Method::GET || request.method() == Method::HEAD {
        redirect(&login)
    } else {
        (
            StatusCode::UNAUTHORIZED,
            "Sign in to the settings panel first",
        )
            .into_response()
    }
}

async fn signed_in(access: &Access, headers: &HeaderMap) -> bool {
    let Some(token) = session_token(headers) else {
        return false;
    };
    let store = access.store.clone();
    tokio::task::spawn_blocking(move || {
        store
            .string_get(&hashed(SESSION_PREFIX, &token))
            .is_ok_and(|found| found.is_some())
    })
    .await
    .unwrap_or(false)
}

/// Start a session and send the browser to `next`.
async fn start_session(access: &Access, request_headers: &HeaderMap, next: &str) -> Response {
    let token = random_token();
    let store = access.store.clone();
    let lifetime = access.config.session_seconds;
    let key = hashed(SESSION_PREFIX, &token);
    let stored = tokio::task::spawn_blocking(move || {
        store.string_set(
            &key,
            &format!("{:.0}", now()),
            Some(Duration::from_secs(lifetime)),
        )
    })
    .await;
    if !matches!(stored, Ok(Ok(()))) {
        return crate::render::failure("could not store the session");
    }
    let mut response = redirect(&format!("{ADMIN_ROOT}{next}"));
    let secure = forwarded_https(request_headers);
    if let Ok(value) = HeaderValue::from_str(&cookie(&token, lifetime, secure)) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

fn login_page(access: &Access, status: StatusCode, next: &str, error: Option<&str>) -> Response {
    let mut response = page(
        access.templates.templates(),
        "login.html",
        context! {
            next => next,
            error => error,
            password_sign_in => access.config.credentials.is_some(),
        },
    );
    *response.status_mut() = status;
    response
}

fn client_key(request: &Request) -> String {
    peer(request).map_or_else(|| "unknown".to_owned(), |ip| ip.to_string())
}

async fn login(access: &Access, request: Request) -> Response {
    let ip = client_key(&request);
    let headers = request.headers().clone();
    let Ok(Form(form)) = Form::<HashMap<String, String>>::from_request(request, &()).await else {
        return (
            StatusCode::BAD_REQUEST,
            "The login form didn't arrive whole",
        )
            .into_response();
    };
    let next = local_next(form.get("next").map(String::as_str));
    let back = |error: &str| {
        redirect(&format!(
            "{ADMIN_ROOT}/login?next={}&error={}",
            quote(&next),
            quote(error)
        ))
    };
    let Some((user, password)) = &access.config.credentials else {
        return back("Password sign-in isn't set up here.");
    };
    let (window, max) = (
        access.config.window_seconds as f64,
        access.config.max_attempts,
    );
    if access.limiter().is_blocked(&ip, now(), max, window) {
        warn!(ip, "settings panel login rate limit hit");
        return back("Too many attempts. Wait a few minutes and try again.");
    }
    let given_user = form.get("username").map_or("", String::as_str);
    let given_password = form.get("password").map_or("", String::as_str);
    // Both compared every time, so the timing says nothing about which failed.
    let ok = constant_time_eq(given_user.as_bytes(), user.as_bytes())
        & constant_time_eq(given_password.as_bytes(), password.as_bytes());
    if !ok {
        access.limiter().record_failure(&ip, now(), window);
        warn!(ip, "a settings panel login failed");
        return back("That username and password don't match.");
    }
    access.limiter().reset(&ip);
    info!(
        ip,
        "signed in to the settings panel with the admin password"
    );
    start_session(access, &headers, &next).await
}

async fn sign_in(access: &Access, request: Request) -> Response {
    let ip = client_key(&request);
    let Some(code) = query_param(&request, "code").filter(|c| !c.is_empty()) else {
        return redirect(&format!("{ADMIN_ROOT}/login"));
    };
    let store = access.store.clone();
    let key = hashed(CODE_PREFIX, &code);
    // Reading then deleting would let two requests both see the code; the
    // delete alone says whether this request is the one that used it.
    let live = tokio::task::spawn_blocking(move || {
        store.string_get(&key).is_ok_and(|v| v.is_some()) && store.kv_delete(&key).unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    if !live {
        warn!(ip, "a settings panel sign-in link was expired or used");
        return redirect(&format!(
            "{ADMIN_ROOT}/login?error={}",
            quote("That sign-in link has expired or was already used. Open Settings again.")
        ));
    }
    info!(ip, "signed in to the settings panel with a sign-in link");
    start_session(access, request.headers(), "/").await
}

async fn logout(access: &Access, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        let store = access.store.clone();
        let _ =
            tokio::task::spawn_blocking(move || store.kv_delete(&hashed(SESSION_PREFIX, &token)))
                .await;
    }
    let mut response = redirect(&format!("{ADMIN_ROOT}/login"));
    if let Ok(value) = HeaderValue::from_str(&cookie("", 0, forwarded_https(&headers))) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

/// Length-independent comparison, so a timing side channel can't recover a
/// secret byte by byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limiter_blocks_after_the_limit_and_forgets_after_the_window() {
        let mut limiter = Limiter::default();
        for _ in 0..3 {
            limiter.record_failure("ip", 100.0, 60.0);
        }
        assert!(limiter.is_blocked("ip", 100.0, 3, 60.0));
        assert!(!limiter.is_blocked("ip", 200.0, 3, 60.0));
        assert!(!limiter.is_blocked("other", 100.0, 3, 60.0));
        limiter.record_failure("ip", 100.0, 60.0);
        limiter.reset("ip");
        assert!(!limiter.is_blocked("ip", 100.0, 1, 60.0));
        assert!(!limiter.is_blocked("ip", 100.0, 0, 60.0), "0 turns it off");
    }

    #[test]
    fn next_stays_on_this_site() {
        assert_eq!(local_next(Some("/memories?page=2")), "/memories?page=2");
        for bad in [
            "//evil.example",
            "https://evil.example",
            "/\\evil",
            "/a\r\nb",
            "",
        ] {
            assert_eq!(local_next(Some(bad)), "/", "{bad:?}");
        }
        assert_eq!(local_next(None), "/");
    }

    #[test]
    fn https_is_read_from_either_proxy_header() {
        let mut headers = HeaderMap::new();
        assert!(!forwarded_https(&headers));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(forwarded_https(&headers));
        let mut headers = HeaderMap::new();
        headers.insert(
            "forwarded",
            HeaderValue::from_static("for=1.2.3.4;proto=https;host=x"),
        );
        assert!(forwarded_https(&headers));
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
        assert!(!forwarded_https(&headers));
    }

    #[test]
    fn only_the_panels_own_pages_may_change_things() {
        let mut headers = HeaderMap::new();
        assert!(!same_origin(&headers), "no evidence is a refusal");
        headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        headers.insert("origin", HeaderValue::from_static("http://127.0.0.1:8765"));
        headers.insert("host", HeaderValue::from_static("127.0.0.1:8765"));
        assert!(!same_origin(&headers), "Sec-Fetch-Site wins over Origin");
        headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        assert!(same_origin(&headers));
        headers.remove("sec-fetch-site");
        assert!(
            same_origin(&headers),
            "Origin matching Host, for older browsers"
        );
        headers.insert("origin", HeaderValue::from_static("https://evil.example"));
        assert!(!same_origin(&headers));
    }

    #[test]
    fn the_session_cookie_is_found_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; omnimem_admin=tok; b=2"),
        );
        assert_eq!(session_token(&headers).as_deref(), Some("tok"));
        assert!(cookie("tok", 60, true).ends_with("HttpOnly; SameSite=Strict; Secure"));
        assert!(cookie("tok", 60, false).contains("Path=/admin;"));
    }
}
