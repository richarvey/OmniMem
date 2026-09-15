//! The OAuth routes, and the Host and Origin guard in front of them.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use tracing::{error, info, warn};

use super::{Authorize, Failure, OAuth, Params};

/// The SDK's request body cap.
const BODY_LIMIT: usize = 4 * 1024 * 1024;
const READ: &[&str] = &["GET", "OPTIONS"];
const WRITE: &[&str] = &["POST", "OPTIONS"];
/// Starlette's safelisted headers plus the one the SDK allowed.
const CORS_HEADERS: &str =
    "Accept, Accept-Language, Content-Language, Content-Type, mcp-protocol-version";
const NO_STORE: &[(&str, &str)] = &[("cache-control", "no-store"), ("pragma", "no-cache")];
const LOGIN_PAGE: &str = include_str!("login.html");

/// The brand mark OAuth clients such as claude.ai show for the connector,
/// served at the paths they look in (6.x issue #12).
const ICON_SVG: &str = concat!(
    r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">"##,
    r##"<rect width="64" height="64" rx="14" fill="#1e293b"/>"##,
    r##"<path d="M16 32 q0 -10 8 -10 q4 0 6 4 q2 -4 6 -4 q8 0 8 10 "##,
    r##"q0 10 -8 10 q-4 0 -6 -4 q-2 4 -6 4 q-8 0 -8 -10 z" "##,
    r##"fill="none" stroke="#6366f1" stroke-width="3" stroke-linejoin="round"/>"##,
    r##"<circle cx="32" cy="32" r="2.5" fill="#6366f1"/>"##,
    r##"</svg>"##,
);

pub(crate) fn routes(oauth: Arc<OAuth>) -> Router {
    let resource_path = oauth.resource_metadata_path().to_owned();
    let read = || middleware::from_fn_with_state(READ, cors);
    let write = || middleware::from_fn_with_state(WRITE, cors);
    Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(server_metadata).options(server_metadata).layer(read()),
        )
        .route(
            &resource_path,
            get(resource_metadata)
                .options(resource_metadata)
                .layer(read()),
        )
        .route("/authorize", get(authorize).post(authorize))
        .route("/token", post(token).options(token).layer(write()))
        .route("/register", post(register).options(register).layer(write()))
        .route("/revoke", post(revoke).options(revoke).layer(write()))
        .route("/oauth/login", get(login_page).post(login_submit))
        .route("/icon.svg", get(icon))
        .route("/favicon.svg", get(icon))
        .route("/favicon.ico", get(icon))
        .route("/oauth/icon.svg", get(icon))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(oauth)
}

/// The hosts and origins the OAuth routes accept. 6.x's FastMCP guard sat in
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

    fn host_allowed(&self, host: &str) -> bool {
        let host = host_name(host);
        self.hosts.iter().any(|h| h == "*" || *h == host)
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

pub(crate) async fn guard(
    State(allow): State<Allowlists>,
    request: Request,
    next: Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned)
        .or_else(|| request.uri().authority().map(|a| a.to_string()))
        .unwrap_or_default();
    if !allow.host_allowed(&host) {
        warn!(
            host,
            "refused an OAuth request for a host that isn't allowed"
        );
        return (StatusCode::MISDIRECTED_REQUEST, "Misdirected Request").into_response();
    }
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && !allow.origin_allowed(origin.to_str().unwrap_or_default(), &host)
    {
        warn!(
            host,
            "refused an OAuth request from a browser origin that isn't allowed"
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
        } => json_response(
            status,
            &json!({"error": error, "error_description": description}),
            extra,
        ),
        Failure::Store(e) => server_error(&e),
    }
}

fn server_error(e: &dyn std::fmt::Display) -> Response {
    error!(error = %e, "an OAuth request failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
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

fn render_login(session: &str, problem: Option<&str>) -> String {
    let error_block = problem
        .map(|p| format!(r#"<div class="error">{}</div>"#, escape(p)))
        .unwrap_or_default();
    LOGIN_PAGE
        .replace("{error_block}", &error_block)
        .replace("{session}", &escape(session))
}

fn authorization(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
}

async fn server_metadata(State(oauth): State<Arc<OAuth>>) -> Response {
    json_response(
        StatusCode::OK,
        &oauth.server_metadata(),
        &[("cache-control", "public, max-age=3600")],
    )
}

async fn resource_metadata(State(oauth): State<Arc<OAuth>>) -> Response {
    json_response(
        StatusCode::OK,
        &oauth.resource_metadata(),
        &[("cache-control", "public, max-age=3600")],
    )
}

async fn authorize(
    State(oauth): State<Arc<OAuth>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let params = if method == Method::POST {
        Params::parse(&body)
    } else {
        Params::parse(uri.query().unwrap_or_default().as_bytes())
    };
    match oauth.authorize(&params) {
        Ok(Authorize::Redirect(location)) => found(&location, &[("cache-control", "no-store")]),
        Ok(Authorize::BadRequest(body)) => json_response(
            StatusCode::BAD_REQUEST,
            &body,
            &[("cache-control", "no-store")],
        ),
        Err(e) => server_error(&e),
    }
}

async fn token(State(oauth): State<Arc<OAuth>>, headers: HeaderMap, body: Bytes) -> Response {
    match oauth.token(&Params::parse(&body), authorization(&headers)) {
        Ok(tokens) => json_response(StatusCode::OK, &tokens, NO_STORE),
        Err(failure) => failure_response(failure, NO_STORE),
    }
}

async fn register(State(oauth): State<Arc<OAuth>>, body: Bytes) -> Response {
    match oauth.register(&body) {
        Ok(client) => json_response(StatusCode::CREATED, &client, &[]),
        Err(failure) => failure_response(failure, &[]),
    }
}

async fn revoke(State(oauth): State<Arc<OAuth>>, headers: HeaderMap, body: Bytes) -> Response {
    match oauth.revoke(&Params::parse(&body), authorization(&headers)) {
        Ok(()) => {
            let mut response = StatusCode::OK.into_response();
            add_headers(&mut response, NO_STORE);
            response
        }
        Err(failure) => failure_response(failure, &[]),
    }
}

async fn login_page(State(oauth): State<Arc<OAuth>>, uri: Uri) -> Response {
    let params = Params::parse(uri.query().unwrap_or_default().as_bytes());
    let session = params.get("session").unwrap_or_default();
    if oauth.sign_in_pending(session) {
        html(StatusCode::OK, render_login(session, None))
    } else {
        html(
            StatusCode::BAD_REQUEST,
            render_login("", Some("Invalid or expired session.")),
        )
    }
}

async fn login_submit(State(oauth): State<Arc<OAuth>>, request: Request) -> Response {
    // Behind a reverse proxy this is the proxy's address, as it was in 6.x.
    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(|| "unknown".to_owned(), |info| info.0.ip().to_string());
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
                Some("Too many failed attempts. Please wait and try again."),
            ),
        );
    }
    if !oauth.sign_in_pending(session) {
        return html(
            StatusCode::BAD_REQUEST,
            render_login("", Some("Session expired. Please try again.")),
        );
    }
    let username = form.get("username").unwrap_or_default();
    let password = form.get("password").unwrap_or_default();
    if !oauth.verify_credentials(username, password) {
        oauth.record_login_failure(&ip);
        warn!(ip = %ip, "failed OAuth login");
        return html(
            StatusCode::UNAUTHORIZED,
            render_login(session, Some("Invalid username or password.")),
        );
    }
    oauth.reset_login_failures(&ip);
    match oauth.complete_sign_in(session) {
        Ok(Some(location)) => {
            info!("OAuth sign-in complete, sending the browser back to the client");
            found(&location, &[])
        }
        Ok(None) => html(
            StatusCode::BAD_REQUEST,
            render_login("", Some("Session expired. Please try again.")),
        ),
        Err(e) => server_error(&e),
    }
}

async fn icon() -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        ICON_SVG,
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
        let page = render_login("a\"b<c>", Some("bad & worse"));
        assert!(page.contains(r#"value="a&quot;b&lt;c&gt;""#));
        assert!(page.contains(r#"<div class="error">bad &amp; worse</div>"#));
        assert!(!render_login("s", None).contains("class=\"error\""));
    }
}
