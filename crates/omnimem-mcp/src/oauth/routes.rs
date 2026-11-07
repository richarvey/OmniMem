//! The OAuth routes, and the Host and Origin guard in front of them.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, LazyLock};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tracing::{error, info, warn};

use super::{Authorize, Failure, OAuth, Params, SignInPrompt};

/// Request body cap for every OAuth route. A registration, a token exchange
/// or a login form is a few hundred bytes; 6.x allowed 4 MiB, which let
/// anyone fill the database with registrations.
const BODY_LIMIT: usize = 64 * 1024;
const READ: &[&str] = &["GET", "OPTIONS"];
const WRITE: &[&str] = &["POST", "OPTIONS"];
/// Starlette's safelisted headers plus the one the SDK allowed.
const CORS_HEADERS: &str =
    "Accept, Accept-Language, Content-Language, Content-Type, mcp-protocol-version";
const NO_STORE: &[(&str, &str)] = &[("cache-control", "no-store"), ("pragma", "no-cache")];
/// The settings panel's fonts and logo, taken from its own static folder so
/// the two sign-in pages can't drift apart. They go into the page as data
/// URIs: the login page has to work where the panel isn't mounted
/// (`ADMIN_PANEL=false`), so it can't load them from `/admin/static`.
const FONT_SANS: &[u8] =
    include_bytes!("../../../omnimem-settings/static/fonts/onest-variable-latin.woff2");
const FONT_MONO: &[u8] =
    include_bytes!("../../../omnimem-settings/static/fonts/geist-mono-variable-latin.woff2");
const LOGO: &[u8] =
    include_bytes!("../../../omnimem-settings/static/images/omnimem-logo-horizontal-dark.png");

/// The login page with its fonts and logo filled in, once. The braced names
/// are the template's own placeholders, not format arguments.
#[allow(clippy::literal_string_with_formatting_args)]
static LOGIN_PAGE: LazyLock<String> = LazyLock::new(|| {
    let data = |mime: &str, bytes: &[u8]| format!("data:{mime};base64,{}", STANDARD.encode(bytes));
    include_str!("login.html")
        .replace("{font_sans}", &data("font/woff2", FONT_SANS))
        .replace("{font_mono}", &data("font/woff2", FONT_MONO))
        .replace("{logo}", &data("image/png", LOGO))
});

/// The brand mark OAuth clients such as claude.ai show for the connector,
/// served at the paths they look in (6.x issue #12). The `.ico` is the one
/// in the logos repository, and omnimem.org's, kept in the panel's static
/// folder so both use one copy: the `<brain>` mark in green
/// on black, which reads on a light page and a dark one. The SVG paths wrap
/// its largest frame, since there's no vector version of the mark.
const FAVICON_ICO: &[u8] = include_bytes!("../../../omnimem-settings/static/images/favicon.ico");
static ICON_SVG: LazyLock<String> = LazyLock::new(|| {
    let (size, png) = largest_png_frame(FAVICON_ICO).expect("favicon.ico holds a PNG frame");
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 {size} {size}" width="{size}" height="{size}"><image width="{size}" height="{size}" xlink:href="data:image/png;base64,{}"/></svg>"#,
        STANDARD.encode(png)
    )
});

/// The biggest PNG-encoded image in an `.ico` and its width: a 6-byte
/// header, then a 16-byte entry per image (width, height, …, size, offset).
fn largest_png_frame(ico: &[u8]) -> Option<(u32, &[u8])> {
    let count = usize::from(u16::from_le_bytes(ico.get(4..6)?.try_into().ok()?));
    (0..count)
        .filter_map(|i| {
            let entry = ico.get(6 + 16 * i..22 + 16 * i)?;
            let width = match entry[0] {
                0 => 256,
                w => u32::from(w),
            };
            let len = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
            let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
            let data = ico.get(offset..offset.checked_add(len)?)?;
            data.starts_with(b"\x89PNG").then_some((width, data))
        })
        .max_by_key(|(width, _)| *width)
}

/// The OAuth routes in two parts. The first is what a client calls without a
/// person in a browser: discovery, registration, the token exchange and
/// revocation. These carry no cookie and answer CORS to any origin, so they
/// get only the Host check: refusing a foreign Origin there stopped browser
/// based clients such as Mistral's Le Chat at discovery. The second is where
/// a person signs in, `/authorize` and the login form, which keep the Origin
/// check too.
pub(crate) fn routes(oauth: Arc<OAuth>) -> (Router, Router) {
    let resource_paths = oauth.resource_metadata_paths();
    let read = || middleware::from_fn_with_state(READ, cors);
    let write = || middleware::from_fn_with_state(WRITE, cors);
    let server = || get(server_metadata).options(server_metadata).layer(read());
    let resource = || {
        get(resource_metadata)
            .options(resource_metadata)
            .layer(read())
    };
    // The issuer ends in a slash, so a client that builds the metadata URL
    // from it naively asks for the slashed path; RFC 9728 clients that don't
    // find the path form try the root one.
    let mut public = Router::new()
        .route("/.well-known/oauth-authorization-server", server())
        .route("/.well-known/oauth-authorization-server/", server())
        .route("/.well-known/oauth-protected-resource", resource());
    // One per listed address with its own path (`https://example.com/omnimem`
    // serves `/.well-known/oauth-protected-resource/omnimem/mcp`).
    for path in &resource_paths {
        if path != "/.well-known/oauth-protected-resource" {
            public = public
                .route(path, resource())
                .route(&format!("{path}/"), resource());
        }
    }
    let public = public
        .route("/token", post(token).options(token).layer(write()))
        .route("/register", post(register).options(register).layer(write()))
        .route("/revoke", post(revoke).options(revoke).layer(write()))
        .route("/icon.svg", get(icon))
        .route("/favicon.svg", get(icon))
        .route("/favicon.ico", get(favicon))
        .route("/oauth/icon.svg", get(icon))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn(log_request))
        .with_state(oauth.clone());
    let sign_in = Router::new()
        .route("/authorize", get(authorize).post(authorize))
        .route("/oauth/login", get(login_page).post(login_submit))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn(log_request))
        .with_state(oauth);
    (public, sign_in)
}

/// One line per OAuth request, so a client that gives up during discovery
/// leaves a trail of what it asked for and what it got back. Nothing secret
/// is in it: no query string, no body, no headers but the two below.
async fn log_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let origin = header_text(request.headers(), header::ORIGIN);
    let agent = header_text(request.headers(), header::USER_AGENT);
    let response = next.run(request).await;
    let status = response.status().as_u16();
    if response.status().is_client_error() || response.status().is_server_error() {
        warn!(%method, path, status, origin, agent, "OAuth request refused");
    } else {
        info!(%method, path, status, origin, agent, "OAuth request");
    }
    response
}

pub(crate) fn header_text(headers: &HeaderMap, name: HeaderName) -> String {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .chars()
        .take(200)
        .collect()
}

/// The hosts and origins `/mcp` and the OAuth routes accept. 6.x's FastMCP guard sat in
/// front of every route: a Host that isn't allowed gets 421, and a browser
/// Origin that is neither allowed, nor loopback calling loopback, nor the
/// server's own gets 403.
#[derive(Debug, Clone)]
pub(crate) struct Allowlists {
    hosts: Arc<Vec<String>>,
    origins: Arc<Vec<String>>,
}

impl Allowlists {
    pub(crate) fn new(hosts: &[String], origins: &[String]) -> Self {
        Self {
            hosts: Arc::new(hosts.iter().map(|h| host_name(h)).collect()),
            origins: Arc::new(
                origins
                    .iter()
                    .map(|o| o.trim_end_matches('/').to_ascii_lowercase())
                    .collect(),
            ),
        }
    }

    /// An entry matches its host exactly, `*` matches any, and `*.example.com`
    /// any name under example.com (not example.com itself), for a tunnel
    /// whose address changes, such as a Cloudflare quick tunnel.
    fn host_allowed(&self, host: &str) -> bool {
        let host = host_name(host);
        self.hosts.iter().any(|h| {
            h == "*"
                || *h == host
                || h.strip_prefix('*')
                    .filter(|suffix| suffix.starts_with('.'))
                    .is_some_and(|suffix| host.len() > suffix.len() && host.ends_with(suffix))
        })
    }

    fn origin_allowed(&self, origin: &str, host: &str) -> bool {
        let origin = origin.trim_end_matches('/').to_ascii_lowercase();
        if self.origins.iter().any(|o| o == "*" || *o == origin) {
            return true;
        }
        let Some((_, authority)) = origin.split_once("://") else {
            return false;
        };
        if is_loopback(&host_name(authority)) && is_loopback(&host_name(host)) {
            return true;
        }
        // The server's own origin, whatever its scheme: behind a proxy that
        // terminates TLS the browser says https while the request arrives
        // over http, which 6.x refused with a 403 on the login form.
        authority == host.to_ascii_lowercase()
    }
}

/// A Host header or URL authority without its port or IPv6 brackets.
fn host_name(raw: &str) -> String {
    let raw = raw.trim().to_ascii_lowercase();
    if let Some(bracketed) = raw.strip_prefix('[') {
        return bracketed.split(']').next().unwrap_or_default().to_owned();
    }
    if raw.matches(':').count() > 1 {
        return raw;
    }
    raw.split(':').next().unwrap_or_default().to_owned()
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn request_host(request: &Request) -> String {
    request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            request
                .uri()
                .authority()
                .map(std::string::ToString::to_string)
        })
        .unwrap_or_default()
}

fn misdirected(host: &str) -> Response {
    warn!(
        host,
        "refused a request for a Host that isn't allowed: set MCP_PUBLIC_URL to the address clients use, or add the host to MCP_ALLOWED_HOSTS"
    );
    (StatusCode::MISDIRECTED_REQUEST, "Misdirected Request").into_response()
}

/// The Host check alone, for the OAuth routes any origin may call.
pub(crate) async fn host_guard(
    State(allow): State<Allowlists>,
    request: Request,
    next: Next,
) -> Response {
    let host = request_host(&request);
    if !allow.host_allowed(&host) {
        return misdirected(&host);
    }
    next.run(request).await
}

pub(crate) async fn guard(
    State(allow): State<Allowlists>,
    request: Request,
    next: Next,
) -> Response {
    let host = request_host(&request);
    if !allow.host_allowed(&host) {
        return misdirected(&host);
    }
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && !allow.origin_allowed(origin.to_str().unwrap_or_default(), &host)
    {
        warn!(
            host,
            "refused a request from a browser Origin that isn't allowed: add it to MCP_ALLOWED_ORIGINS"
        );
        return (StatusCode::FORBIDDEN, "Forbidden Origin").into_response();
    }
    next.run(request).await
}

/// CORS as Starlette's middleware answered it for these routes: any origin,
/// the route's methods, no credentials.
async fn cors(
    State(methods): State<&'static [&'static str]>,
    request: Request,
    next: Next,
) -> Response {
    let has_origin = request.headers().contains_key(header::ORIGIN);
    if request.method() == Method::OPTIONS
        && has_origin
        && let Some(requested_method) = request.headers().get(header::ACCESS_CONTROL_REQUEST_METHOD)
    {
        let mut failures = Vec::new();
        if !requested_method
            .to_str()
            .is_ok_and(|m| methods.contains(&m))
        {
            failures.push("method");
        }
        if let Some(requested) = request
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        {
            let allowed = CORS_HEADERS.to_ascii_lowercase();
            let allowed: Vec<&str> = allowed.split(", ").collect();
            let ok = requested.to_str().is_ok_and(|v| {
                v.split(',')
                    .map(|h| h.trim().to_ascii_lowercase())
                    .filter(|h| !h.is_empty())
                    .all(|h| allowed.contains(&h.as_str()))
            });
            if !ok {
                failures.push("headers");
            }
        }
        let (status, body) = if failures.is_empty() {
            (StatusCode::OK, "OK".to_owned())
        } else {
            (
                StatusCode::BAD_REQUEST,
                format!("Disallowed CORS {}", failures.join(", ")),
            )
        };
        let mut response = (
            status,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            body,
        )
            .into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
        if let Ok(value) = HeaderValue::from_str(&methods.join(", ")) {
            headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, value);
        }
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(CORS_HEADERS),
        );
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
        return response;
    }
    let mut response = next.run(request).await;
    if has_origin {
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    }
    response
}

fn add_headers(response: &mut Response, extra: &[(&'static str, &'static str)]) {
    for (name, value) in extra {
        response.headers_mut().insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
}

/// Compact JSON, as pydantic serialised these bodies.
fn json_response(
    status: StatusCode,
    body: &Value,
    extra: &[(&'static str, &'static str)],
) -> Response {
    let mut response = (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response();
    add_headers(&mut response, extra);
    response
}

fn failure_response(failure: Failure, extra: &[(&'static str, &'static str)]) -> Response {
    match failure {
        Failure::OAuth {
            status,
            error,
            description,
        } => {
            warn!(error, description, "OAuth error sent to the client");
            json_response(
                status,
                &json!({"error": error, "error_description": description}),
                extra,
            )
        }
        Failure::Store(e) => server_error(&e),
    }
}

fn server_error(e: &dyn std::fmt::Display) -> Response {
    error!(error = %e, "an OAuth request failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

/// The OAuth core reads and writes SQLite, so each call to it runs off the
/// async workers, as the `/mcp` token check does. A worker that panics is
/// answered as any other server failure.
// The error is the response to send, returned straight away by every caller,
// so boxing it would buy nothing.
#[allow(clippy::result_large_err)]
async fn off_runtime<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Response> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| server_error(&e))
}

fn found(location: &str, extra: &[(&'static str, &'static str)]) -> Response {
    match HeaderValue::from_str(location) {
        Ok(value) => {
            let mut response = (StatusCode::FOUND, [(header::LOCATION, value)]).into_response();
            add_headers(&mut response, extra);
            response
        }
        Err(e) => server_error(&e),
    }
}

fn html(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

/// Python's `html.escape`.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// The login page. `prompt` names the client asking and where the browser
/// goes afterwards, so a user sent here by a link can see who they'd be
/// letting in before they type a password.
///
/// The braced names are the template's own placeholders, not format
/// arguments, hence the allow.
#[allow(clippy::literal_string_with_formatting_args)]
fn render_login(session: &str, prompt: Option<&SignInPrompt>, problem: Option<&str>) -> String {
    let error_block = problem
        .map(|p| {
            format!(
                r#"<div class="flash-error" role="alert">{}</div>"#,
                escape(p)
            )
        })
        .unwrap_or_default();
    let client_block = prompt
        .map(|p| {
            format!(
                r#"<p class="login-client">Signing in will let <strong>{}</strong> use your memory. Afterwards you'll be sent to <strong>{}</strong>.</p>"#,
                escape(&p.client),
                escape(&p.redirect_host)
            )
        })
        .unwrap_or_default();
    LOGIN_PAGE
        .replace("{error_block}", &error_block)
        .replace("{client_block}", &client_block)
        .replace("{session}", &escape(session))
}

fn authorization(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
}

/// The connecting address, which behind a reverse proxy is the proxy's, as
/// it was in 6.x.
fn client_ip(request: &Request) -> String {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(|| "unknown".to_owned(), |info| info.0.ip().to_string())
}

/// The `Host` a request was made to, which picks the `OAUTH_BASE_URL` entry
/// it is answered with.
pub(crate) fn host_of(headers: &HeaderMap) -> String {
    headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

// The documents differ by Host, so a shared cache must keep them apart.
const METADATA_CACHE: &[(&str, &str)] =
    &[("cache-control", "public, max-age=3600"), ("vary", "Host")];

async fn server_metadata(State(oauth): State<Arc<OAuth>>, headers: HeaderMap) -> Response {
    json_response(
        StatusCode::OK,
        &oauth.server_metadata(&host_of(&headers)),
        METADATA_CACHE,
    )
}

async fn resource_metadata(State(oauth): State<Arc<OAuth>>, headers: HeaderMap) -> Response {
    json_response(
        StatusCode::OK,
        &oauth.resource_metadata(&host_of(&headers)),
        METADATA_CACHE,
    )
}

async fn authorize(State(oauth): State<Arc<OAuth>>, request: Request) -> Response {
    let ip = client_ip(&request);
    let host = host_of(request.headers());
    let (parts, body) = request.into_parts();
    let params = if parts.method == Method::POST {
        let Ok(body) = axum::body::to_bytes(body, BODY_LIMIT).await else {
            return (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response();
        };
        Params::parse(&body)
    } else {
        Params::parse(parts.uri.query().unwrap_or_default().as_bytes())
    };
    match off_runtime(move || oauth.authorize_at(&ip, &host, &params)).await {
        Ok(Ok(Authorize::Redirect(location))) => found(&location, &[("cache-control", "no-store")]),
        Ok(Ok(Authorize::BadRequest(body))) => json_response(
            StatusCode::BAD_REQUEST,
            &body,
            &[("cache-control", "no-store")],
        ),
        Ok(Err(e)) => server_error(&e),
        Err(response) => response,
    }
}

async fn token(State(oauth): State<Arc<OAuth>>, headers: HeaderMap, body: Bytes) -> Response {
    let authorization = authorization(&headers).map(str::to_owned);
    let result =
        off_runtime(move || oauth.token(&Params::parse(&body), authorization.as_deref())).await;
    match result {
        Ok(Ok(tokens)) => json_response(StatusCode::OK, &tokens, NO_STORE),
        Ok(Err(failure)) => failure_response(failure, NO_STORE),
        Err(response) => response,
    }
}

async fn register(State(oauth): State<Arc<OAuth>>, request: Request) -> Response {
    let ip = client_ip(&request);
    let Ok(body) = axum::body::to_bytes(request.into_body(), BODY_LIMIT).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response();
    };
    match off_runtime(move || oauth.register(&ip, &body)).await {
        Ok(Ok(client)) => json_response(StatusCode::CREATED, &client, &[]),
        Ok(Err(failure)) => failure_response(failure, &[]),
        Err(response) => response,
    }
}

async fn revoke(State(oauth): State<Arc<OAuth>>, headers: HeaderMap, body: Bytes) -> Response {
    let authorization = authorization(&headers).map(str::to_owned);
    let result =
        off_runtime(move || oauth.revoke(&Params::parse(&body), authorization.as_deref())).await;
    match result {
        Ok(Ok(())) => {
            let mut response = StatusCode::OK.into_response();
            add_headers(&mut response, NO_STORE);
            response
        }
        Ok(Err(failure)) => failure_response(failure, &[]),
        Err(response) => response,
    }
}

async fn login_page(State(oauth): State<Arc<OAuth>>, uri: Uri) -> Response {
    let params = Params::parse(uri.query().unwrap_or_default().as_bytes());
    let session = params.get("session").unwrap_or_default();
    match oauth.sign_in_pending(session) {
        Some(prompt) => html(StatusCode::OK, render_login(session, Some(&prompt), None)),
        None => html(
            StatusCode::BAD_REQUEST,
            render_login("", None, Some("Invalid or expired session.")),
        ),
    }
}

async fn login_submit(State(oauth): State<Arc<OAuth>>, request: Request) -> Response {
    let ip = client_ip(&request);
    let Ok(body) = axum::body::to_bytes(request.into_body(), BODY_LIMIT).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response();
    };
    let form = Params::parse(&body);
    let session = form.get("session").unwrap_or_default();
    if oauth.login_blocked(&ip) {
        warn!(ip = %ip, "OAuth login rate limit hit");
        return html(
            StatusCode::TOO_MANY_REQUESTS,
            render_login(
                session,
                None,
                Some("Too many failed attempts. Please wait and try again."),
            ),
        );
    }
    let Some(prompt) = oauth.sign_in_pending(session) else {
        return html(
            StatusCode::BAD_REQUEST,
            render_login("", None, Some("Session expired. Please try again.")),
        );
    };
    let username = form.get("username").unwrap_or_default();
    let password = form.get("password").unwrap_or_default();
    if !oauth.verify_credentials(username, password) {
        oauth.record_login_failure(&ip);
        warn!(ip = %ip, "failed OAuth login");
        return html(
            StatusCode::UNAUTHORIZED,
            render_login(
                session,
                Some(&prompt),
                Some("Invalid username or password."),
            ),
        );
    }
    oauth.reset_login_failures(&ip);
    let session = session.to_owned();
    match off_runtime(move || oauth.complete_sign_in(&session)).await {
        Ok(Ok(Some(location))) => {
            info!("OAuth sign-in complete, sending the browser back to the client");
            found(&location, &[])
        }
        Ok(Ok(None)) => html(
            StatusCode::BAD_REQUEST,
            render_login("", None, Some("Session expired. Please try again.")),
        ),
        Ok(Err(e)) => server_error(&e),
        Err(response) => response,
    }
}

async fn icon() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        ICON_SVG.as_str(),
    )
        .into_response()
}

async fn favicon() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/x-icon"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        FAVICON_ICO,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lists() -> Allowlists {
        Allowlists::new(
            &[
                "localhost".into(),
                "127.0.0.1".into(),
                "::1".into(),
                "mcp.example.com".into(),
            ],
            &["https://mcp.example.com".into()],
        )
    }

    #[test]
    fn hosts_match_without_their_ports() {
        let allow = lists();
        assert!(allow.host_allowed("mcp.example.com"));
        assert!(allow.host_allowed("MCP.example.com:443"));
        assert!(allow.host_allowed("127.0.0.1:8765"));
        assert!(allow.host_allowed("[::1]:8765"));
        assert!(!allow.host_allowed("evil.example"));
        assert!(!allow.host_allowed(""));
    }

    #[test]
    fn a_wildcard_host_matches_the_names_under_it() {
        let allow = Allowlists::new(&["*.trycloudflare.com".into()], &[]);
        assert!(allow.host_allowed("convinced-money.trycloudflare.com"));
        assert!(allow.host_allowed("A.B.TryCloudflare.com:443"));
        assert!(
            !allow.host_allowed("trycloudflare.com"),
            "not the domain itself"
        );
        assert!(!allow.host_allowed("eviltrycloudflare.com"));
        assert!(!allow.host_allowed("trycloudflare.com.evil.example"));
        let any = Allowlists::new(&["*".into()], &[]);
        assert!(any.host_allowed("anything.example"));
    }

    #[test]
    fn origins_are_allowed_listed_loopback_or_same_host() {
        let allow = lists();
        assert!(allow.origin_allowed("https://mcp.example.com", "mcp.example.com"));
        assert!(allow.origin_allowed("http://localhost:6274", "127.0.0.1:8765"));
        assert!(
            allow.origin_allowed("https://tunnel.example", "tunnel.example"),
            "same host, whatever the scheme"
        );
        assert!(!allow.origin_allowed("https://claude.ai", "mcp.example.com"));
        assert!(!allow.origin_allowed("null", "mcp.example.com"));
    }

    #[test]
    fn the_login_page_escapes_what_it_echoes() {
        let prompt = SignInPrompt {
            client: "<b>claude</b>".into(),
            redirect_host: "claude.ai".into(),
        };
        let page = render_login("a\"b<c>", Some(&prompt), Some("bad & worse"));
        assert!(page.contains(r#"value="a&quot;b&lt;c&gt;""#));
        assert!(page.contains(r#"<div class="flash-error" role="alert">bad &amp; worse</div>"#));
        assert!(page.contains("<strong>&lt;b&gt;claude&lt;/b&gt;</strong>"));
        assert!(page.contains("<strong>claude.ai</strong>"));
        let bare = render_login("s", None, None);
        assert!(!bare.contains("class=\"flash-error\""));
        assert!(!bare.contains("class=\"login-client\""));
        assert!(!bare.contains("{font_sans}") && !bare.contains("{logo}"));
        assert!(bare.contains("data:font/woff2;base64,"));
        assert!(!bare.contains("/admin/"));
    }

    #[test]
    fn the_icon_is_the_favicons_largest_frame() {
        let (size, png) = largest_png_frame(FAVICON_ICO).unwrap();
        assert_eq!(size, 64);
        assert!(png.starts_with(b"\x89PNG"));
        assert!(ICON_SVG.contains(r#"viewBox="0 0 64 64""#));
        assert!(largest_png_frame(b"not an icon").is_none());
    }
}
