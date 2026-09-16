//! OAuth 2.1 for the MCP server: the authorisation server FastMCP provided in
//! 6.x, rebuilt on axum.
//!
//! One admin user signs in on a login page. Clients register themselves
//! (RFC 7591), authorise with PKCE (S256 only), and refresh with rotation: a
//! rotated refresh token keeps working for a short grace window and replays
//! the same successor pair, so clients that refresh concurrently (claude.ai
//! does) aren't signed out. A refresh chain lives at most
//! `OAUTH_REFRESH_MAX_DAYS`. Clients, codes and tokens live in the store, so a
//! restart signs nobody out; a sign-in still in progress is kept in memory.
//!
//! What goes over HTTP (routes, discovery documents, status codes, error
//! bodies and headers) follows 6.x as captured from FastMCP 4.0.3 and mcp
//! 2.1.1 in `tests/fixtures/oauth_golden.json`.

mod routes;

use std::collections::{HashMap, VecDeque};
use std::fmt::{self, Write as _};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use omnimem_store::{OAuthStore, Store, StoreError, TokenKind, secret_hash};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use url::Url;

pub(crate) use routes::{Allowlists, guard, routes};

use crate::http::{constant_time_eq, var};

/// The one scope clients are registered for.
const SCOPE: &str = "omnimem";
const ACCESS_TOKEN_SECONDS: u64 = 3600;
const AUTH_CODE_SECONDS: f64 = 300.0;
/// Sign-ins in progress kept at once; beyond this the oldest is dropped, so
/// unauthenticated `/authorize` calls can't grow memory without bound.
const PENDING_LIMIT: usize = 1000;
/// Registered clients kept at once. Anyone who can reach the server can
/// register, so past this idle registrations are purged and, if that isn't
/// enough, new ones are refused.
const CLIENT_LIMIT: usize = 500;
/// A client that has never signed in is dropped after this long when the
/// table is full.
const IDLE_CLIENT_SECONDS: f64 = 3600.0;
/// Registrations and sign-in starts allowed per address in
/// `OAUTH_LOGIN_WINDOW_SECONDS`, both unauthenticated and both cheap to
/// abuse. Generous for people, tight for scripts.
const UNAUTHENTICATED_ATTEMPTS: usize = 60;
/// Failed-login buckets kept before the oldest are dropped.
const LIMITER_ENTRIES: usize = 4096;
/// Caps on what a client may register (RFC 7591 leaves them to the server).
const MAX_TEXT_FIELD: usize = 256;
const MAX_URL_FIELD: usize = 2048;
const MAX_LIST_ITEMS: usize = 10;
const MAX_JWKS_BYTES: usize = 8192;
/// RFC 7636 §4.1 and §4.2: verifier and challenge are 43 to 128 characters.
const PKCE_MIN: usize = 43;
const PKCE_MAX: usize = 128;
/// Schemes a redirect URI may never use: the browser would run or render
/// the "redirect" instead of leaving the page.
const FORBIDDEN_REDIRECT_SCHEMES: [&str; 6] =
    ["javascript", "data", "file", "vbscript", "blob", "about"];
const JWT_BEARER: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const DEFAULT_GRANT_TYPES: [&str; 2] = ["authorization_code", "refresh_token"];
const AUTH_METHODS: [&str; 4] = [
    "none",
    "client_secret_post",
    "client_secret_basic",
    "private_key_jwt",
];

#[derive(Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    /// `OAUTH_BASE_URL`: where clients reach the server.
    pub base_url: String,
    /// `OAUTH_ADMIN_USER`
    pub admin_user: String,
    /// `OAUTH_ADMIN_PASSWORD`
    pub admin_password: String,
    /// `OAUTH_REFRESH_MAX_DAYS`, in seconds: a refresh chain's absolute lifetime.
    pub refresh_max_seconds: u64,
    /// `OAUTH_REFRESH_GRACE_SECONDS`: how long a rotated refresh token still
    /// replays its successor. 0 deletes it at once.
    pub refresh_grace_seconds: u64,
    /// `OAUTH_LOGIN_MAX_ATTEMPTS`: failed logins per address before the login
    /// page refuses it for a while. 0 turns the limit off.
    pub login_max_attempts: usize,
    /// `OAUTH_LOGIN_WINDOW_SECONDS`
    pub login_window_seconds: u64,
}

impl fmt::Debug for OAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthConfig")
            .field("base_url", &self.base_url)
            .field("admin_user", &self.admin_user)
            .field("admin_password", &"<redacted>")
            .field("refresh_max_seconds", &self.refresh_max_seconds)
            .field("refresh_grace_seconds", &self.refresh_grace_seconds)
            .field("login_max_attempts", &self.login_max_attempts)
            .field("login_window_seconds", &self.login_window_seconds)
            .finish()
    }
}

impl OAuthConfig {
    /// A configuration with 6.x's defaults for everything but the essentials.
    pub fn new(
        base_url: impl Into<String>,
        admin_user: impl Into<String>,
        admin_password: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            admin_user: admin_user.into(),
            admin_password: admin_password.into(),
            refresh_max_seconds: 30 * 86_400,
            refresh_grace_seconds: 120,
            login_max_attempts: 10,
            login_window_seconds: 900,
        }
    }
}

/// Whether OAuth is on, as the environment configures it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OAuthSetup {
    #[default]
    Off,
    On(OAuthConfig),
    /// `OAUTH_ENABLED` is set but the rest isn't usable. 6.x logged a warning
    /// and carried on without OAuth, which left claude.ai unable to connect
    /// with nothing obvious to say why; this build refuses to start instead.
    Invalid(String),
}

impl OAuthSetup {
    pub fn from_env() -> Self {
        let enabled = var("OAUTH_ENABLED")
            .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"));
        if !enabled {
            return Self::Off;
        }
        let (Some(admin_user), Some(admin_password)) =
            (var("OAUTH_ADMIN_USER"), var("OAUTH_ADMIN_PASSWORD"))
        else {
            return Self::Invalid(
                "OAUTH_ENABLED is on but OAUTH_ADMIN_USER or OAUTH_ADMIN_PASSWORD is missing"
                    .into(),
            );
        };
        let Some(base_url) = var("OAUTH_BASE_URL") else {
            return Self::Invalid(
                "OAUTH_ENABLED is on but OAUTH_BASE_URL is missing: set it to the address clients \
                 reach OmniMem at, such as https://mcp.example.com"
                    .into(),
            );
        };
        if let Err(problem) = Urls::parse(&base_url) {
            return Self::Invalid(format!("OAUTH_BASE_URL={base_url} {problem}"));
        }
        let setting = |name: &str, default: i64, min: i64, max: i64| {
            clamp_setting(name, var(name), default, min, max)
        };
        Self::On(OAuthConfig {
            base_url,
            admin_user,
            admin_password,
            refresh_max_seconds: setting("OAUTH_REFRESH_MAX_DAYS", 30, 1, 90) as u64 * 86_400,
            refresh_grace_seconds: setting("OAUTH_REFRESH_GRACE_SECONDS", 120, 0, 3600) as u64,
            login_max_attempts: setting("OAUTH_LOGIN_MAX_ATTEMPTS", 10, 0, 1_000_000) as usize,
            login_window_seconds: setting("OAUTH_LOGIN_WINDOW_SECONDS", 900, 0, 31_536_000) as u64,
        })
    }
}

/// A whole-number setting held between `min` and `max`, falling back to the
/// default when it isn't a number, as 6.x did.
fn clamp_setting(name: &str, raw: Option<String>, default: i64, min: i64, max: i64) -> i64 {
    let Some(raw) = raw else {
        return default;
    };
    match raw.parse::<i64>() {
        Ok(value) if value > max => {
            warn!(setting = name, value, max, "above the cap, using the cap");
            max
        }
        Ok(value) => value.max(min),
        Err(_) => {
            warn!(setting = name, value = %raw, default, "not a whole number, using the default");
            default
        }
    }
}

/// The URLs the discovery documents advertise, derived from `OAUTH_BASE_URL`
/// as 6.x's pydantic models serialised them.
#[derive(Debug, Clone)]
struct Urls {
    /// The issuer, with the slash an empty path gains: `https://mcp.example.com/`.
    issuer: String,
    /// The issuer without a trailing slash, which every endpoint hangs off.
    prefix: String,
    /// The protected resource: `{prefix}/mcp`.
    resource: String,
    /// Where the protected-resource document is served (RFC 9728 §3.1), and its URL.
    resource_metadata_path: String,
    resource_metadata_url: String,
}

impl Urls {
    fn parse(base_url: &str) -> Result<Self, String> {
        let url = Url::parse(base_url).map_err(|e| format!("is not a URL: {e}"))?;
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            return Err("must use https (plain http only for localhost)".into());
        }
        if url.fragment().is_some() {
            return Err("must not have a fragment".into());
        }
        if url.query().is_some() {
            return Err("must not have a query string".into());
        }
        let issuer = url.to_string();
        let prefix = issuer.trim_end_matches('/').to_owned();
        let resource = format!("{prefix}/mcp");
        let resource_path =
            Url::parse(&resource).map_or_else(|_| "/mcp".into(), |u| u.path().to_owned());
        let resource_metadata_path =
            format!("/.well-known/oauth-protected-resource{resource_path}");
        let resource_metadata_url = format!(
            "{}{resource_metadata_path}",
            url.origin().ascii_serialization()
        );
        Ok(Self {
            issuer,
            prefix,
            resource,
            resource_metadata_path,
            resource_metadata_url,
        })
    }
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the operating system's random number generator failed");
    bytes
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing into a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Python's `secrets.token_urlsafe(32)`: 32 random bytes, base64url, unpadded.
fn token_urlsafe() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes::<32>())
}

/// A random (version 4) UUID, as the registration handler assigned client IDs.
fn uuid4() -> String {
    let mut b = random_bytes::<16>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// An absolute URL, normalised, so a registered redirect and the one a
/// request names compare equal however they were written.
fn normalise_url(raw: &str) -> Option<String> {
    if raw.len() > MAX_URL_FIELD {
        return None;
    }
    Url::parse(raw).ok().map(|u| u.to_string())
}

/// A redirect URI a client may register: any scheme a browser will leave the
/// page for (https, http, or an app's own scheme), never one it would run or
/// render, and no fragment (RFC 6749 §3.1.2).
fn redirect_uri(raw: &str) -> Option<String> {
    if raw.len() > MAX_URL_FIELD {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    if url.fragment().is_some() || FORBIDDEN_REDIRECT_SCHEMES.contains(&url.scheme()) {
        return None;
    }
    Some(url.to_string())
}

/// RFC 7636: 43 to 128 characters of `[A-Za-z0-9._~-]` (a base64url
/// challenge is a subset of that).
fn pkce_well_formed(value: &str) -> bool {
    (PKCE_MIN..=PKCE_MAX).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// `base` with `pairs` appended to its query, dropping any blank values it
/// already had, as the SDK's `construct_redirect_uri` did.
fn with_query(base: &str, pairs: &[(&str, &str)]) -> String {
    let Ok(mut url) = Url::parse(base) else {
        return base.to_owned();
    };
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(kept.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .extend_pairs(pairs.iter().copied());
    url.to_string()
}

/// `urllib.parse.unquote`: `%XX` escapes decoded, `+` left alone.
fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16)),
                bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16)),
            )
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// A list as Python's `repr` prints one, for the messages 6.x sent.
fn python_list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| format!("'{s}'")).collect();
    format!("[{}]", quoted.join(", "))
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn token_response(access: &str, refresh: &str, scopes: &[String]) -> Value {
    let mut body = Map::new();
    body.insert("access_token".into(), access.into());
    body.insert("token_type".into(), "Bearer".into());
    body.insert("expires_in".into(), ACCESS_TOKEN_SECONDS.into());
    if !scopes.is_empty() {
        body.insert("scope".into(), scopes.join(" ").into());
    }
    body.insert("refresh_token".into(), refresh.into());
    Value::Object(body)
}

/// Form or query parameters. A repeated name reads as its last value, as
/// Starlette's did.
#[derive(Debug, Default)]
pub(crate) struct Params(Vec<(String, String)>);

impl Params {
    pub(crate) fn parse(raw: &[u8]) -> Self {
        Self(url::form_urlencoded::parse(raw).into_owned().collect())
    }

    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Why an OAuth request was refused, or that the store failed.
#[derive(Debug)]
pub(crate) enum Failure {
    OAuth {
        status: StatusCode,
        error: &'static str,
        description: String,
    },
    Store(StoreError),
}

impl Failure {
    fn new(status: StatusCode, error: &'static str, description: impl Into<String>) -> Self {
        Self::OAuth {
            status,
            error,
            description: description.into(),
        }
    }

    fn invalid_request(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", description)
    }

    /// FastMCP answered `invalid_grant` with 401, as the MCP spec requires for
    /// invalid or expired tokens, where the SDK used 400.
    fn invalid_grant(description: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "invalid_grant", description)
    }

    /// The revocation endpoint names a failed client authentication
    /// `unauthorized_client`.
    fn for_revocation(self) -> Self {
        match self {
            Self::OAuth {
                status,
                error: "invalid_client",
                description,
            } => Self::OAuth {
                status,
                error: "unauthorized_client",
                description,
            },
            other => other,
        }
    }
}

impl From<StoreError> for Failure {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// The answer to an authorisation request.
#[derive(Debug)]
pub(crate) enum Authorize {
    Redirect(String),
    BadRequest(Value),
}

/// A registered client: its registration, less the secret, which is kept
/// only as `client_secret_sha256`.
struct Client(Map<String, Value>);

impl Client {
    fn id(&self) -> &str {
        self.text("client_id").unwrap_or_default()
    }

    fn text(&self, name: &str) -> Option<&str> {
        self.0.get(name).and_then(Value::as_str)
    }

    fn grant_types(&self) -> Vec<String> {
        match self.0.get("grant_types") {
            Some(value) => strings(value),
            None => DEFAULT_GRANT_TYPES.map(str::to_owned).to_vec(),
        }
    }

    fn validate_redirect_uri(&self, requested: Option<&str>) -> Result<String, String> {
        let registered = self.0.get("redirect_uris").map(strings).unwrap_or_default();
        match requested {
            Some(uri) if registered.iter().any(|r| r == uri) => Ok(uri.to_owned()),
            Some(uri) => Err(format!("Redirect URI '{uri}' not registered for client")),
            None if registered.len() == 1 => Ok(registered[0].clone()),
            None => Err(
                "redirect_uri must be specified unless the client has exactly one registered URI"
                    .into(),
            ),
        }
    }

    fn validate_scope(&self, requested: Option<&str>) -> Result<Option<Vec<String>>, String> {
        let Some(requested) = requested else {
            return Ok(None);
        };
        let allowed: Vec<&str> = self
            .text("scope")
            .map(|s| s.split(' ').collect())
            .unwrap_or_default();
        let scopes: Vec<String> = requested.split(' ').map(str::to_owned).collect();
        if let Some(scope) = scopes.iter().find(|s| !allowed.contains(&s.as_str())) {
            return Err(format!("Client was not registered with scope {scope}"));
        }
        Ok(Some(scopes))
    }
}

fn load_client(o: &OAuthStore<'_>, client_id: &str) -> Result<Option<Client>, StoreError> {
    Ok(o.client(client_id)?.and_then(|v| match v {
        Value::Object(fields) => Some(Client(fields)),
        _ => None,
    }))
}

/// What a client sends to register (RFC 7591 §2), checked as the SDK's
/// pydantic model checked it.
struct ClientMetadata {
    response_types: Vec<String>,
    scope: Option<String>,
    client_name: Option<String>,
    client_uri: Option<String>,
    logo_uri: Option<String>,
    contacts: Option<Vec<String>>,
    tos_uri: Option<String>,
    policy_uri: Option<String>,
    jwks_uri: Option<String>,
    jwks: Option<Value>,
    software_id: Option<String>,
    software_version: Option<String>,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: Option<String>,
    grant_types: Vec<String>,
    application_type: String,
}

fn text_field(
    request: &Map<String, Value>,
    name: &str,
    errors: &mut Vec<String>,
) -> Option<String> {
    match request.get(name) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.chars().count() > MAX_TEXT_FIELD => {
            errors.push(format!(
                "{name}: String should have at most {MAX_TEXT_FIELD} characters"
            ));
            None
        }
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            errors.push(format!("{name}: Input should be a valid string"));
            None
        }
    }
}

fn list_field(
    request: &Map<String, Value>,
    name: &str,
    errors: &mut Vec<String>,
) -> Option<Vec<String>> {
    match request.get(name) {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) if items.len() > MAX_LIST_ITEMS => {
            errors.push(format!(
                "{name}: List should have at most {MAX_LIST_ITEMS} items"
            ));
            None
        }
        Some(Value::Array(items)) => {
            let mut list = Vec::new();
            for (i, item) in items.iter().enumerate() {
                match item.as_str() {
                    Some(s) if s.chars().count() > MAX_TEXT_FIELD => errors.push(format!(
                        "{name}.{i}: String should have at most {MAX_TEXT_FIELD} characters"
                    )),
                    Some(s) => list.push(s.to_owned()),
                    None => errors.push(format!("{name}.{i}: Input should be a valid string")),
                }
            }
            Some(list)
        }
        Some(_) => {
            errors.push(format!("{name}: Input should be a valid list"));
            None
        }
    }
}

/// An optional web URL, where an empty string means absent.
fn web_url_field(
    request: &Map<String, Value>,
    name: &str,
    errors: &mut Vec<String>,
) -> Option<String> {
    let raw = text_field(request, name, errors).filter(|s| !s.is_empty())?;
    match Url::parse(&raw) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => Some(url.to_string()),
        _ => {
            errors.push(format!("{name}: Input should be a valid URL"));
            None
        }
    }
}

impl ClientMetadata {
    fn parse(request: &Map<String, Value>) -> Result<Self, String> {
        let mut errors = Vec::new();
        let redirect_uris = match request.get("redirect_uris") {
            None => {
                errors.push("redirect_uris: Field required".to_owned());
                Vec::new()
            }
            Some(Value::Array(items)) if items.is_empty() => {
                errors.push(
                    "redirect_uris: List should have at least 1 item after validation, not 0"
                        .to_owned(),
                );
                Vec::new()
            }
            Some(Value::Array(items)) if items.len() > MAX_LIST_ITEMS => {
                errors.push(format!(
                    "redirect_uris: List should have at most {MAX_LIST_ITEMS} items"
                ));
                Vec::new()
            }
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| {
                    let url = item.as_str().and_then(redirect_uri);
                    if url.is_none() {
                        errors.push(format!(
                            "redirect_uris.{i}: Input should be a valid redirect URL without a fragment"
                        ));
                    }
                    url
                })
                .collect(),
            Some(_) => {
                errors.push("redirect_uris: Input should be a valid list".to_owned());
                Vec::new()
            }
        };
        let token_endpoint_auth_method = match request.get("token_endpoint_auth_method") {
            None | Some(Value::Null) => None,
            Some(Value::String(m)) if AUTH_METHODS.contains(&m.as_str()) => Some(m.clone()),
            Some(_) => {
                errors.push(
                    "token_endpoint_auth_method: Input should be 'none', 'client_secret_post', \
                     'client_secret_basic' or 'private_key_jwt'"
                        .to_owned(),
                );
                None
            }
        };
        let application_type = match request.get("application_type") {
            None => "native".to_owned(),
            Some(Value::String(t)) if t == "web" || t == "native" => t.clone(),
            Some(_) => {
                errors.push("application_type: Input should be 'web' or 'native'".to_owned());
                "native".to_owned()
            }
        };
        let metadata = Self {
            response_types: list_field(request, "response_types", &mut errors)
                .unwrap_or_else(|| vec!["code".to_owned()]),
            scope: text_field(request, "scope", &mut errors),
            client_name: text_field(request, "client_name", &mut errors),
            client_uri: web_url_field(request, "client_uri", &mut errors),
            logo_uri: web_url_field(request, "logo_uri", &mut errors),
            contacts: list_field(request, "contacts", &mut errors),
            tos_uri: web_url_field(request, "tos_uri", &mut errors),
            policy_uri: web_url_field(request, "policy_uri", &mut errors),
            jwks_uri: web_url_field(request, "jwks_uri", &mut errors),
            jwks: match request.get("jwks").filter(|v| !v.is_null()) {
                Some(jwks) if jwks.to_string().len() > MAX_JWKS_BYTES => {
                    errors.push(format!(
                        "jwks: Input should be at most {MAX_JWKS_BYTES} bytes"
                    ));
                    None
                }
                jwks => jwks.cloned(),
            },
            software_id: text_field(request, "software_id", &mut errors),
            software_version: text_field(request, "software_version", &mut errors),
            redirect_uris,
            token_endpoint_auth_method,
            grant_types: list_field(request, "grant_types", &mut errors)
                .unwrap_or_else(|| DEFAULT_GRANT_TYPES.map(str::to_owned).to_vec()),
            application_type,
        };
        if errors.is_empty() {
            Ok(metadata)
        } else {
            Err(errors.join("\n"))
        }
    }

    /// The registration response: every registered field in the order the
    /// SDK's model serialised them, empty ones left out.
    fn echo(
        &self,
        scope: &str,
        method: &str,
        client_id: &str,
        secret: Option<&str>,
        issued_at: i64,
    ) -> Map<String, Value> {
        let text = |v: &Option<String>| v.clone().map(Value::from);
        let entries: [(&str, Option<Value>); 20] = [
            ("response_types", Some(json!(self.response_types))),
            ("scope", Some(scope.into())),
            ("client_name", text(&self.client_name)),
            ("client_uri", text(&self.client_uri)),
            ("logo_uri", text(&self.logo_uri)),
            ("contacts", self.contacts.as_ref().map(|c| json!(c))),
            ("tos_uri", text(&self.tos_uri)),
            ("policy_uri", text(&self.policy_uri)),
            ("jwks_uri", text(&self.jwks_uri)),
            ("jwks", self.jwks.clone()),
            ("software_id", text(&self.software_id)),
            ("software_version", text(&self.software_version)),
            ("redirect_uris", Some(json!(self.redirect_uris))),
            ("token_endpoint_auth_method", Some(method.into())),
            ("grant_types", Some(json!(self.grant_types))),
            (
                "application_type",
                Some(self.application_type.clone().into()),
            ),
            ("client_id", Some(client_id.into())),
            ("client_secret", secret.map(Value::from)),
            ("client_id_issued_at", Some(issued_at.into())),
            ("client_secret_expires_at", secret.map(|_| Value::from(0))),
        ];
        entries
            .into_iter()
            .filter_map(|(name, value)| value.map(|v| (name.to_owned(), v)))
            .collect()
    }
}

/// What the login page says about the sign-in it is completing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SignInPrompt {
    pub(crate) client: String,
    pub(crate) redirect_host: String,
}

/// An authorisation request waiting for the user to sign in.
#[derive(Debug)]
struct Pending {
    client_id: String,
    /// What the login page tells the user they are signing in for.
    client_name: Option<String>,
    state: Option<String>,
    scopes: Option<Vec<String>>,
    code_challenge: String,
    redirect_uri: String,
    redirect_uri_provided_explicitly: bool,
    resource: Option<String>,
    created_at: f64,
}

/// Attempts by client address, in a sliding window: failed logins, and the
/// unauthenticated requests that create state (registrations, sign-ins).
#[derive(Debug, Default)]
struct LoginLimiter {
    failures: HashMap<String, VecDeque<f64>>,
}

impl LoginLimiter {
    fn prune(&mut self, ip: &str, at: f64, window: f64) {
        if let Some(bucket) = self.failures.get_mut(ip) {
            while bucket.front().is_some_and(|t| *t < at - window) {
                bucket.pop_front();
            }
            if bucket.is_empty() {
                self.failures.remove(ip);
            }
        }
        // Buckets for other addresses are only pruned on their own next
        // visit, so a flood from many addresses is bounded here instead.
        if self.failures.len() > LIMITER_ENTRIES {
            self.failures
                .retain(|_, bucket| bucket.back().is_some_and(|t| *t >= at - window));
            while self.failures.len() > LIMITER_ENTRIES {
                let Some(oldest) = self
                    .failures
                    .iter()
                    .min_by(|a, b| {
                        a.1.back()
                            .copied()
                            .unwrap_or(0.0)
                            .total_cmp(&b.1.back().copied().unwrap_or(0.0))
                    })
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                self.failures.remove(&oldest);
            }
        }
    }

    fn is_blocked(&mut self, ip: &str, at: f64, max_attempts: usize, window: f64) -> bool {
        if max_attempts == 0 {
            return false;
        }
        self.prune(ip, at, window);
        self.failures
            .get(ip)
            .is_some_and(|bucket| bucket.len() >= max_attempts)
    }

    fn record_failure(&mut self, ip: &str, at: f64, window: f64) {
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

pub(crate) struct OAuth {
    config: OAuthConfig,
    urls: Urls,
    store: Arc<Store>,
    pending: Mutex<HashMap<String, Pending>>,
    limiter: Mutex<LoginLimiter>,
}

impl OAuth {
    pub(crate) fn new(config: OAuthConfig, store: Arc<Store>) -> Result<Self, String> {
        let urls = Urls::parse(&config.base_url)
            .map_err(|problem| format!("OAUTH_BASE_URL={} {problem}", config.base_url))?;
        Ok(Self {
            config,
            urls,
            store,
            pending: Mutex::default(),
            limiter: Mutex::default(),
        })
    }

    pub(crate) fn resource_metadata_url(&self) -> &str {
        &self.urls.resource_metadata_url
    }

    fn resource_metadata_path(&self) -> &str {
        &self.urls.resource_metadata_path
    }

    /// RFC 8414 authorisation server metadata.
    fn server_metadata(&self) -> Value {
        let prefix = &self.urls.prefix;
        json!({
            "issuer": self.urls.issuer,
            "authorization_endpoint": format!("{prefix}/authorize"),
            "token_endpoint": format!("{prefix}/token"),
            "registration_endpoint": format!("{prefix}/register"),
            "scopes_supported": [SCOPE],
            "response_types_supported": ["code"],
            "grant_types_supported": DEFAULT_GRANT_TYPES,
            "token_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"],
            "revocation_endpoint": format!("{prefix}/revoke"),
            "revocation_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"],
            "code_challenge_methods_supported": ["S256"],
        })
    }

    /// RFC 9728 protected resource metadata for `/mcp`.
    fn resource_metadata(&self) -> Value {
        json!({
            "resource": self.urls.resource,
            "authorization_servers": [self.urls.issuer],
            "scopes_supported": [SCOPE],
            "bearer_methods_supported": ["header"],
        })
    }

    /// Too many registrations or sign-in starts from one address in the
    /// login window: both are unauthenticated and both create state.
    fn unauthenticated_flood(&self, ip: &str) -> bool {
        let at = now();
        let window = self.config.login_window_seconds as f64;
        let mut limiter = self.limiter();
        let key = format!("state:{ip}");
        if limiter.is_blocked(&key, at, UNAUTHENTICATED_ATTEMPTS, window) {
            return true;
        }
        limiter.record_failure(&key, at, window);
        false
    }

    /// Dynamic client registration (RFC 7591 §3.1): the registered client.
    fn register(&self, ip: &str, body: &[u8]) -> Result<Value, Failure> {
        let invalid = |description: String| {
            Failure::new(
                StatusCode::BAD_REQUEST,
                "invalid_client_metadata",
                description,
            )
        };
        if self.unauthenticated_flood(ip) {
            warn!(ip, "OAuth registration rate limit hit");
            return Err(Failure::new(
                StatusCode::TOO_MANY_REQUESTS,
                "temporarily_unavailable",
                "Too many registrations from this address; try again later",
            ));
        }
        let request = match serde_json::from_slice::<Value>(body) {
            Ok(Value::Object(request)) => request,
            Ok(_) => return Err(invalid(": Input should be an object".into())),
            Err(e) => return Err(invalid(format!(": Invalid JSON: {e}"))),
        };
        let metadata = ClientMetadata::parse(&request).map_err(invalid)?;
        let method = metadata
            .token_endpoint_auth_method
            .clone()
            .unwrap_or_else(|| "client_secret_post".into());
        if method == "private_key_jwt" {
            return Err(invalid(
                "token_endpoint_auth_method 'private_key_jwt' is not supported".into(),
            ));
        }
        let scope = match &metadata.scope {
            None => SCOPE.to_owned(),
            Some(requested) => {
                let mut refused: Vec<&str> = requested
                    .split_whitespace()
                    .filter(|s| *s != SCOPE)
                    .collect();
                refused.sort_unstable();
                refused.dedup();
                if !refused.is_empty() {
                    return Err(invalid(format!(
                        "Requested scopes are not valid: {}",
                        refused.join(", ")
                    )));
                }
                requested.clone()
            }
        };
        if !metadata
            .grant_types
            .iter()
            .any(|g| g == "authorization_code")
        {
            return Err(invalid(
                "grant_types must include 'authorization_code'".into(),
            ));
        }
        if metadata.grant_types.iter().any(|g| g == JWT_BEARER) {
            return Err(invalid(format!(
                "grant_types must not include '{JWT_BEARER}'; the identity-assertion grant requires a pre-registered client"
            )));
        }
        if !metadata.response_types.iter().any(|r| r == "code") {
            return Err(invalid(
                "response_types must include 'code' for authorization_code grant".into(),
            ));
        }
        // Public clients (desktop and native apps) authenticate the token
        // exchange with PKCE alone and must not be handed a secret.
        let secret = (method != "none").then(|| hex(&random_bytes::<32>()));
        let client_id = uuid4();
        let echo = metadata.echo(&scope, &method, &client_id, secret.as_deref(), now() as i64);
        let mut stored = echo.clone();
        if let Some(secret) = &secret {
            stored.remove("client_secret");
            stored.insert("client_secret_sha256".into(), secret_hash(secret).into());
        }
        self.store.with_oauth(|o| {
            // Registration is open to anyone who can reach the server, so
            // the table is capped: idle registrations go first, and when
            // every slot holds a live client the new one is refused.
            if o.client_count()? >= CLIENT_LIMIT {
                let purged = o.purge_idle_clients(IDLE_CLIENT_SECONDS)?;
                info!(purged, "purged idle OAuth clients");
                if o.client_count()? >= CLIENT_LIMIT {
                    return Err(Failure::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "temporarily_unavailable",
                        "Too many registered clients; try again later",
                    ));
                }
            }
            o.save_client(&client_id, &Value::Object(stored))?;
            Ok(())
        })?;
        info!(
            client_id,
            name = metadata.client_name.as_deref().unwrap_or("unnamed"),
            "registered an OAuth client"
        );
        Ok(Value::Object(echo))
    }

    /// An authorisation request (RFC 6749 §4.1.1): off to the login page, or
    /// an error sent back to the client when its redirect is known, or shown
    /// here when it isn't.
    fn authorize(&self, ip: &str, params: &Params) -> Result<Authorize, StoreError> {
        let mut errors = Vec::new();
        let mut error = "invalid_request";
        let client_id = params.get("client_id");
        if client_id.is_none() {
            errors.push("client_id: Field required".to_owned());
        }
        for (name, max) in [
            ("state", 1024),
            ("scope", MAX_TEXT_FIELD),
            ("resource", MAX_URL_FIELD),
        ] {
            if params.get(name).is_some_and(|v| v.len() > max) {
                errors.push(format!(
                    "{name}: String should have at most {max} characters"
                ));
            }
        }
        let redirect_uri = match params.get("redirect_uri") {
            None => None,
            Some(raw) => {
                let url = normalise_url(raw);
                if url.is_none() {
                    errors.push("redirect_uri: Input should be a valid URL".to_owned());
                }
                url
            }
        };
        match params.get("response_type") {
            None => errors.push("response_type: Field required".to_owned()),
            Some("code") => {}
            Some(_) => {
                errors.push("response_type: Input should be 'code'".to_owned());
                error = "unsupported_response_type";
            }
        }
        let code_challenge = params.get("code_challenge");
        match code_challenge {
            None => errors.push("code_challenge: Field required".to_owned()),
            Some(challenge) if !pkce_well_formed(challenge) => errors.push(
                "code_challenge: Input should be 43 to 128 characters of [A-Za-z0-9._~-]"
                    .to_owned(),
            ),
            Some(_) => {}
        }
        if params
            .get("code_challenge_method")
            .is_some_and(|m| m != "S256")
        {
            errors.push("code_challenge_method: Input should be 'S256'".to_owned());
        }
        let (Some(client_id), Some(code_challenge), true) =
            (client_id, code_challenge, errors.is_empty())
        else {
            return self.authorize_error(params, None, None, error, errors.join("\n"), true);
        };

        let Some(client) = self.store.with_oauth(|o| load_client(o, client_id))? else {
            return self.authorize_error(
                params,
                None,
                None,
                "invalid_request",
                format!("Client ID '{client_id}' not found"),
                false,
            );
        };
        let redirect = match client.validate_redirect_uri(redirect_uri.as_deref()) {
            Ok(redirect) => redirect,
            Err(message) => {
                return self.authorize_error(
                    params,
                    Some(client),
                    None,
                    "invalid_request",
                    message,
                    true,
                );
            }
        };
        let scopes = match client.validate_scope(params.get("scope")) {
            Ok(scopes) => scopes,
            Err(message) => {
                return self.authorize_error(
                    params,
                    Some(client),
                    Some(redirect),
                    "invalid_scope",
                    message,
                    true,
                );
            }
        };
        if self.unauthenticated_flood(ip) {
            warn!(ip, "OAuth authorisation rate limit hit");
            return self.authorize_error(
                params,
                Some(client),
                Some(redirect),
                "temporarily_unavailable",
                "Too many sign-in attempts from this address; try again later".to_owned(),
                true,
            );
        }
        let session = self.begin_sign_in(Pending {
            client_id: client.id().to_owned(),
            client_name: client.text("client_name").map(str::to_owned),
            state: params.get("state").map(str::to_owned),
            scopes,
            code_challenge: code_challenge.to_owned(),
            redirect_uri: redirect,
            redirect_uri_provided_explicitly: redirect_uri.is_some(),
            resource: params.get("resource").map(str::to_owned),
            created_at: now(),
        });
        Ok(Authorize::Redirect(format!(
            "{}/oauth/login?session={session}",
            self.urls.prefix
        )))
    }

    /// RFC 6749 §4.1.2.1: with a known client and a redirect it registered,
    /// the error goes back to the client; otherwise it is shown here.
    fn authorize_error(
        &self,
        params: &Params,
        client: Option<Client>,
        redirect: Option<String>,
        error: &'static str,
        description: String,
        find_client: bool,
    ) -> Result<Authorize, StoreError> {
        let client = match client {
            Some(client) => Some(client),
            None => match params
                .get("client_id")
                .filter(|id| find_client && !id.is_empty())
            {
                Some(id) => self.store.with_oauth(|o| load_client(o, id))?,
                None => None,
            },
        };
        let redirect = redirect.or_else(|| {
            let client = client.as_ref()?;
            let requested = match params.get("redirect_uri") {
                None => None,
                Some(raw) => Some(normalise_url(raw)?),
            };
            client.validate_redirect_uri(requested.as_deref()).ok()
        });
        let state = params.get("state");
        if let (Some(redirect), Some(_)) = (&redirect, &client) {
            let mut pairs = vec![
                ("error", error),
                ("error_description", description.as_str()),
            ];
            if let Some(state) = state {
                pairs.push(("state", state));
            }
            return Ok(Authorize::Redirect(with_query(redirect, &pairs)));
        }
        let mut body = Map::new();
        body.insert("error".into(), error.into());
        body.insert("error_description".into(), description.into());
        if let Some(state) = state {
            body.insert("state".into(), state.into());
        }
        Ok(Authorize::BadRequest(Value::Object(body)))
    }

    fn pending(&self) -> MutexGuard<'_, HashMap<String, Pending>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Keep a sign-in until the login form comes back: its session ID.
    fn begin_sign_in(&self, sign_in: Pending) -> String {
        let session = token_urlsafe();
        let at = sign_in.created_at;
        let mut pending = self.pending();
        pending.retain(|_, p| at - p.created_at <= AUTH_CODE_SECONDS);
        if pending.len() >= PENDING_LIMIT
            && let Some(oldest) = pending
                .iter()
                .min_by(|a, b| a.1.created_at.total_cmp(&b.1.created_at))
                .map(|(k, _)| k.clone())
        {
            pending.remove(&oldest);
        }
        pending.insert(session.clone(), sign_in);
        session
    }

    /// While a sign-in is waiting for its login form: who is asking (the
    /// client's registered name, or its ID) and where the browser goes
    /// afterwards, so the page can say what the user is about to allow.
    fn sign_in_pending(&self, session: &str) -> Option<SignInPrompt> {
        let mut pending = self.pending();
        match pending.get(session) {
            Some(p) if now() - p.created_at <= AUTH_CODE_SECONDS => Some(SignInPrompt {
                client: p
                    .client_name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| p.client_id.clone()),
                redirect_host: Url::parse(&p.redirect_uri)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_owned))
                    .unwrap_or_else(|| p.redirect_uri.clone()),
            }),
            Some(_) => {
                pending.remove(session);
                None
            }
            None => None,
        }
    }

    fn verify_credentials(&self, username: &str, password: &str) -> bool {
        // Both compared every time, so the timing says nothing about which failed.
        let user = constant_time_eq(username.as_bytes(), self.config.admin_user.as_bytes());
        let pass = constant_time_eq(password.as_bytes(), self.config.admin_password.as_bytes());
        user & pass
    }

    fn limiter(&self) -> MutexGuard<'_, LoginLimiter> {
        self.limiter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn login_blocked(&self, ip: &str) -> bool {
        self.limiter().is_blocked(
            ip,
            now(),
            self.config.login_max_attempts,
            self.config.login_window_seconds as f64,
        )
    }

    fn record_login_failure(&self, ip: &str) {
        self.limiter()
            .record_failure(ip, now(), self.config.login_window_seconds as f64);
    }

    fn reset_login_failures(&self, ip: &str) {
        self.limiter().reset(ip);
    }

    /// The user signed in: issue the authorisation code and say where to send
    /// the browser. `None` when the sign-in has gone.
    fn complete_sign_in(&self, session: &str) -> Result<Option<String>, StoreError> {
        let Some(sign_in) = self.pending().remove(session) else {
            return Ok(None);
        };
        let code = token_urlsafe();
        let expires_at = now() + AUTH_CODE_SECONDS;
        let grant = json!({
            "client_id": sign_in.client_id,
            "redirect_uri": sign_in.redirect_uri,
            "redirect_uri_provided_explicitly": sign_in.redirect_uri_provided_explicitly,
            "code_challenge": sign_in.code_challenge,
            "scopes": sign_in.scopes.clone().unwrap_or_default(),
            "expires_at": expires_at,
            "resource": sign_in.resource,
        });
        self.store.with_oauth(|o| {
            o.purge_expired()?;
            o.save_code(&code, &grant, expires_at)
        })?;
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("code", &code);
        if let Some(state) = sign_in.state.as_deref().filter(|s| !s.is_empty()) {
            query.append_pair("state", state);
        }
        let separator = if sign_in.redirect_uri.contains('?') {
            '&'
        } else {
            '?'
        };
        Ok(Some(format!(
            "{}{separator}{}",
            sign_in.redirect_uri,
            query.finish()
        )))
    }

    /// The token endpoint's client authentication, as the SDK's
    /// `ClientAuthenticator` did it.
    fn authenticate_client(
        o: &OAuthStore<'_>,
        form: &Params,
        authorization: Option<&str>,
    ) -> Result<Client, Failure> {
        let fail = |description: &str| {
            Failure::new(StatusCode::UNAUTHORIZED, "invalid_client", description)
        };
        let Some(client_id) = form.get("client_id").filter(|id| !id.is_empty()) else {
            return Err(fail("Missing client_id"));
        };
        let Some(client) = load_client(o, client_id)? else {
            return Err(fail("Invalid client_id"));
        };
        let method = client.text("token_endpoint_auth_method");
        let presented = match method {
            Some("client_secret_basic") => {
                let Some(encoded) = authorization.and_then(|h| h.strip_prefix("Basic ")) else {
                    return Err(fail(
                        "Missing or invalid Basic authentication in Authorization header",
                    ));
                };
                let decoded = STANDARD
                    .decode(encoded.trim())
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok());
                let credentials = decoded
                    .as_deref()
                    .and_then(|d| d.split_once(':'))
                    .and_then(|(id, secret)| Some((percent_decode(id)?, percent_decode(secret)?)));
                let Some((basic_id, secret)) = credentials else {
                    return Err(fail("Invalid Basic authentication header"));
                };
                if basic_id != client_id {
                    return Err(fail("Client ID mismatch in Basic auth"));
                }
                Some(secret)
            }
            Some("client_secret_post") => form.get("client_secret").map(str::to_owned),
            Some("none") => None,
            other => {
                return Err(fail(&format!(
                    "Unsupported auth method: {}",
                    other.unwrap_or("None")
                )));
            }
        };
        let stored = client.text("client_secret_sha256");
        if method != Some("none") && stored.is_none() {
            return Err(fail(
                "Client is registered for secret-based authentication but has no stored secret",
            ));
        }
        if let Some(stored) = stored {
            let Some(presented) = presented.filter(|s| !s.is_empty()) else {
                return Err(fail("Client secret is required"));
            };
            if !constant_time_eq(secret_hash(&presented).as_bytes(), stored.as_bytes()) {
                return Err(fail("Invalid client_secret"));
            }
            let expires_at = client
                .0
                .get("client_secret_expires_at")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if expires_at != 0 && expires_at < now() as i64 {
                return Err(fail("Client secret has expired"));
            }
        }
        Ok(client)
    }

    /// The token endpoint: an authorisation code or a refresh token for a
    /// fresh access and refresh token pair.
    fn token(&self, form: &Params, authorization: Option<&str>) -> Result<Value, Failure> {
        self.store.with_oauth(|o| {
            let client = Self::authenticate_client(o, form, authorization)?;
            let grant_type = match form.get("grant_type") {
                Some(g @ ("authorization_code" | "refresh_token" | JWT_BEARER)) => g,
                Some(other) => {
                    return Err(Failure::invalid_request(format!(
                        ": Input tag '{other}' found using 'grant_type' does not match any of the \
                         expected tags: 'authorization_code', 'refresh_token', '{JWT_BEARER}'"
                    )));
                }
                None => {
                    return Err(Failure::invalid_request(
                        ": Unable to extract tag using discriminator 'grant_type'",
                    ));
                }
            };
            let fields: &[&str] = match grant_type {
                "authorization_code" => &["code", "redirect_uri", "client_id", "code_verifier"],
                "refresh_token" => &["refresh_token", "client_id"],
                _ => &["assertion", "client_id"],
            };
            let errors: Vec<String> = fields
                .iter()
                .filter_map(|field| match (*field, form.get(field)) {
                    ("redirect_uri", None) => None,
                    ("redirect_uri", Some(raw)) => normalise_url(raw)
                        .is_none()
                        .then(|| format!("{grant_type}.redirect_uri: Input should be a valid URL")),
                    (_, None) => Some(format!("{grant_type}.{field}: Field required")),
                    _ => None,
                })
                .collect();
            if !errors.is_empty() {
                return Err(Failure::invalid_request(errors.join("\n")));
            }
            let supported = client.grant_types();
            if !supported.iter().any(|g| g == grant_type) {
                return Err(Failure::new(
                    StatusCode::BAD_REQUEST,
                    "unsupported_grant_type",
                    format!(
                        "Unsupported grant type (supported grant types are {})",
                        python_list(&supported)
                    ),
                ));
            }
            match grant_type {
                "authorization_code" => self.exchange_code(o, &client, form),
                "refresh_token" => self.exchange_refresh(o, &client, form),
                _ => Err(Failure::new(
                    StatusCode::BAD_REQUEST,
                    "unsupported_grant_type",
                    "The JWT bearer grant is not supported by this authorization server",
                )),
            }
        })
    }

    fn exchange_code(
        &self,
        o: &OAuthStore<'_>,
        client: &Client,
        form: &Params,
    ) -> Result<Value, Failure> {
        let code = form.get("code").unwrap_or_default();
        // A code issued to another client is treated as one that doesn't exist.
        let Some(grant) = o
            .code(code)?
            .filter(|g| g["client_id"].as_str() == Some(client.id()))
        else {
            return Err(Failure::invalid_grant("authorization code does not exist"));
        };
        // RFC 6749 §10.6: the redirect must not change between the two legs.
        let expected = if grant["redirect_uri_provided_explicitly"].as_bool() == Some(true) {
            grant["redirect_uri"].as_str().map(str::to_owned)
        } else {
            None
        };
        if form.get("redirect_uri").and_then(normalise_url) != expected {
            return Err(Failure::invalid_request(
                "redirect_uri did not match the one used when creating auth code",
            ));
        }
        let verifier = form.get("code_verifier").unwrap_or_default();
        if !pkce_well_formed(verifier) {
            return Err(Failure::invalid_request(
                "code_verifier: Input should be 43 to 128 characters of [A-Za-z0-9._~-]",
            ));
        }
        let hashed = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        if grant["code_challenge"].as_str() != Some(hashed.as_str()) {
            return Err(Failure::invalid_grant("incorrect code_verifier"));
        }
        // Single use, inside the same transaction as the checks above.
        if !o.delete_code(code)? {
            return Err(Failure::invalid_grant("authorization code does not exist"));
        }
        let tokens = self.issue(o, client.id(), &strings(&grant["scopes"]), None)?;
        info!(client_id = client.id(), "issued OAuth tokens");
        Ok(tokens)
    }

    fn exchange_refresh(
        &self,
        o: &OAuthStore<'_>,
        client: &Client,
        form: &Params,
    ) -> Result<Value, Failure> {
        let presented = form.get("refresh_token").unwrap_or_default();
        let Some(mut record) = o
            .token(TokenKind::Refresh, presented)?
            .filter(|r| r["client_id"].as_str() == Some(client.id()))
        else {
            return Err(Failure::invalid_grant("refresh token does not exist"));
        };
        let granted = strings(&record["scopes"]);
        let requested: Vec<String> = match form.get("scope").filter(|s| !s.is_empty()) {
            Some(scope) => scope.split(' ').map(str::to_owned).collect(),
            None => granted.clone(),
        };
        if let Some(scope) = requested.iter().find(|s| !granted.contains(s)) {
            return Err(Failure::new(
                StatusCode::BAD_REQUEST,
                "invalid_scope",
                format!("cannot request scope `{scope}` not provided by refresh token"),
            ));
        }
        // Inside the grace window a rotated token replays the pair it rotated
        // to, so concurrent or retried refreshes all get working tokens. The
        // successor pair has to be stored as issued for that, so for the
        // length of the window (`OAUTH_REFRESH_GRACE_SECONDS`, 120 by
        // default) a copy of the database holds one live pair per rotation.
        // That is the trade-off for claude.ai staying signed in; 0 turns it
        // off.
        if let Some(successor) = record.get("rotated_to").filter(|v| v.is_object()) {
            info!(
                client_id = client.id(),
                "replaying a rotated refresh token inside its grace window"
            );
            return Ok(successor.clone());
        }
        let scopes = if requested.is_empty() {
            granted
        } else {
            requested
        };
        // The chain keeps its original absolute cap however often it rotates.
        let chain_expires_at = record["absolute_expires_at"]
            .as_f64()
            .unwrap_or_else(|| now() + self.config.refresh_max_seconds as f64);
        let tokens = self.issue(o, client.id(), &scopes, Some(chain_expires_at))?;
        let grace = self.config.refresh_grace_seconds;
        if grace == 0 {
            o.delete_token(TokenKind::Refresh, presented)?;
        } else {
            let at = now();
            record["rotated_to"] = tokens.clone();
            record["created_at"] = json!(at);
            record["expires_in"] = json!(grace);
            o.save_token(TokenKind::Refresh, presented, &record, at + grace as f64)?;
        }
        info!(client_id = client.id(), "refreshed OAuth tokens");
        Ok(tokens)
    }

    /// A new access and refresh token pair. `chain_expires_at` carries a
    /// refresh chain's absolute cap; `None` starts a new chain.
    fn issue(
        &self,
        o: &OAuthStore<'_>,
        client_id: &str,
        scopes: &[String],
        chain_expires_at: Option<f64>,
    ) -> Result<Value, StoreError> {
        o.purge_expired()?;
        let at = now();
        let access = token_urlsafe();
        let refresh = token_urlsafe();
        let max = self.config.refresh_max_seconds as f64;
        let (absolute, refresh_seconds) = match chain_expires_at {
            None => (at + max, max),
            Some(absolute) => (absolute, (absolute - at).trunc().max(1.0)),
        };
        o.save_token(
            TokenKind::Access,
            &access,
            &json!({
                "client_id": client_id,
                "scopes": scopes,
                "created_at": at,
                "expires_in": ACCESS_TOKEN_SECONDS,
            }),
            at + ACCESS_TOKEN_SECONDS as f64,
        )?;
        o.save_token(
            TokenKind::Refresh,
            &refresh,
            &json!({
                "client_id": client_id,
                "scopes": scopes,
                "created_at": at,
                "expires_in": refresh_seconds,
                "absolute_expires_at": absolute,
                "rotated_to": null,
            }),
            at + refresh_seconds,
        )?;
        Ok(token_response(&access, &refresh, scopes))
    }

    /// True for a live access token. Checked on every `/mcp` request, so it
    /// is a plain read: expired rows are left for the next issue or
    /// revocation to purge. A store error is reported as such rather than
    /// as a bad token, which would have clients throw their tokens away.
    pub(crate) fn verify_access(&self, token: &str) -> Result<bool, StoreError> {
        if token.is_empty() {
            return Ok(false);
        }
        self.store
            .read_oauth(|o| o.token_is_live(TokenKind::Access, token))
    }

    /// Token revocation (RFC 7009). A token that doesn't exist, or belongs to
    /// another client, is not an error.
    fn revoke(&self, form: &Params, authorization: Option<&str>) -> Result<(), Failure> {
        self.store.with_oauth(|o| {
            let client = Self::authenticate_client(o, form, authorization)
                .map_err(Failure::for_revocation)?;
            let hint = form.get("token_type_hint");
            let mut errors = Vec::new();
            if form.get("token").is_none() {
                errors.push("token: Field required");
            }
            if hint.is_some_and(|h| h != "access_token" && h != "refresh_token") {
                errors.push("token_type_hint: Input should be 'access_token' or 'refresh_token'");
            }
            if !errors.is_empty() {
                return Err(Failure::invalid_request(errors.join("\n")));
            }
            let token = form.get("token").unwrap_or_default();
            let access = o.token(TokenKind::Access, token)?;
            let refresh = o
                .token(TokenKind::Refresh, token)?
                .filter(|r| r["client_id"].as_str() == Some(client.id()));
            let found = if hint == Some("refresh_token") {
                refresh.or(access)
            } else {
                access.or(refresh)
            };
            if found.is_some_and(|r| r["client_id"].as_str() == Some(client.id())) {
                o.delete_token(TokenKind::Access, token)?;
                o.delete_token(TokenKind::Refresh, token)?;
                info!(client_id = client.id(), "revoked an OAuth token");
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const CALLBACK: &str = "https://claude.ai/api/mcp/auth_callback";

    fn provider(store: Arc<Store>, grace: u64) -> OAuth {
        let mut config = OAuthConfig::new("https://mcp.example.com", "admin", "secret123");
        config.refresh_grace_seconds = grace;
        OAuth::new(config, store).unwrap()
    }

    fn params(pairs: &[(&str, &str)]) -> Params {
        Params(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    /// Each test registers from its own address so the flood limit never
    /// trips across a test's own calls.
    const IP: &str = "203.0.113.7";

    fn register(oauth: &OAuth, body: &str) -> Value {
        oauth.register(IP, body.as_bytes()).unwrap()
    }

    fn confidential(oauth: &OAuth) -> Value {
        register(
            oauth,
            r#"{"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"], "client_name": "test"}"#,
        )
    }

    fn id(client: &Value) -> &str {
        client["client_id"].as_str().unwrap()
    }

    /// Authorise and sign in: the authorisation code.
    fn sign_in(oauth: &OAuth, client: &Value) -> String {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes()));
        let Authorize::Redirect(login) = oauth
            .authorize(
                IP,
                &params(&[
                    ("client_id", id(client)),
                    ("response_type", "code"),
                    ("code_challenge", &challenge),
                    ("redirect_uri", CALLBACK),
                    ("state", "st"),
                    ("scope", "omnimem"),
                ]),
            )
            .unwrap()
        else {
            panic!("expected the login page");
        };
        let session = login.split("session=").nth(1).unwrap();
        let prompt = oauth.sign_in_pending(session).expect("a pending sign-in");
        assert_eq!(prompt.redirect_host, "claude.ai");
        let back = oauth.complete_sign_in(session).unwrap().unwrap();
        assert!(back.starts_with(&format!("{CALLBACK}?code=")), "{back}");
        assert!(back.ends_with("&state=st"), "{back}");
        back.split("code=")
            .nth(1)
            .and_then(|rest| rest.split('&').next())
            .unwrap()
            .to_owned()
    }

    fn exchange(
        oauth: &OAuth,
        client: &Value,
        code: &str,
        verifier: &str,
    ) -> Result<Value, Failure> {
        let mut pairs = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", id(client)),
            ("redirect_uri", CALLBACK),
            ("code_verifier", verifier),
        ];
        if let Some(secret) = client["client_secret"].as_str() {
            pairs.push(("client_secret", secret));
        }
        oauth.token(&params(&pairs), None)
    }

    fn refresh(oauth: &OAuth, client: &Value, token: &str) -> Result<Value, Failure> {
        oauth.token(
            &params(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", token),
                ("client_id", id(client)),
                (
                    "client_secret",
                    client["client_secret"].as_str().unwrap_or(""),
                ),
            ]),
            None,
        )
    }

    fn issue(oauth: &OAuth) -> (Value, Value) {
        let client = confidential(oauth);
        let code = sign_in(oauth, &client);
        let tokens = exchange(oauth, &client, &code, VERIFIER).unwrap();
        (client, tokens)
    }

    fn error_of(result: Result<Value, Failure>) -> (StatusCode, &'static str, String) {
        match result {
            Err(Failure::OAuth {
                status,
                error,
                description,
            }) => (status, error, description),
            other => panic!("expected an OAuth error, got {other:?}"),
        }
    }

    #[test]
    fn a_sign_in_issues_tokens_that_open_mcp() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let (_, tokens) = issue(&oauth);
        assert_eq!(tokens["token_type"], "Bearer");
        assert_eq!(tokens["expires_in"], 3600);
        assert_eq!(tokens["scope"], "omnimem");
        assert!(
            oauth
                .verify_access(tokens["access_token"].as_str().unwrap())
                .unwrap()
        );
        assert!(
            !oauth
                .verify_access(tokens["refresh_token"].as_str().unwrap())
                .unwrap()
        );
        assert!(!oauth.verify_access("bogus").unwrap());
        assert!(!oauth.verify_access("").unwrap());
    }

    #[test]
    fn a_code_works_once_and_needs_the_verifier() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let client = confidential(&oauth);
        let code = sign_in(&oauth, &client);
        let wrong = "w".repeat(43);
        let (status, error, description) = error_of(exchange(&oauth, &client, &code, &wrong));
        assert_eq!((status, error), (StatusCode::UNAUTHORIZED, "invalid_grant"));
        assert_eq!(description, "incorrect code_verifier");
        exchange(&oauth, &client, &code, VERIFIER).unwrap();
        let (_, error, description) = error_of(exchange(&oauth, &client, &code, VERIFIER));
        assert_eq!(error, "invalid_grant");
        assert_eq!(description, "authorization code does not exist");
    }

    #[test]
    fn another_client_cannot_use_a_code_or_a_refresh_token() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let (client, tokens) = issue(&oauth);
        let other = confidential(&oauth);
        let code = sign_in(&oauth, &client);
        let (_, _, description) = error_of(exchange(&oauth, &other, &code, VERIFIER));
        assert_eq!(description, "authorization code does not exist");
        let (_, _, description) = error_of(refresh(
            &oauth,
            &other,
            tokens["refresh_token"].as_str().unwrap(),
        ));
        assert_eq!(description, "refresh token does not exist");
    }

    #[test]
    fn a_rotated_refresh_token_replays_its_successor_within_the_grace_window() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let (client, tokens) = issue(&oauth);
        let original = tokens["refresh_token"].as_str().unwrap();
        let first = refresh(&oauth, &client, original).unwrap();
        assert_ne!(first["access_token"], tokens["access_token"]);
        assert_ne!(first["refresh_token"], tokens["refresh_token"]);
        assert_eq!(refresh(&oauth, &client, original).unwrap(), first);
        let second = refresh(&oauth, &client, first["refresh_token"].as_str().unwrap()).unwrap();
        assert_ne!(second["refresh_token"], first["refresh_token"]);
        assert_ne!(second["access_token"], first["access_token"]);
    }

    #[test]
    fn a_rotated_refresh_token_dies_when_its_grace_window_ends() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let oauth = provider(store.clone(), 120);
        let (client, tokens) = issue(&oauth);
        let original = tokens["refresh_token"].as_str().unwrap();
        refresh(&oauth, &client, original).unwrap();
        store
            .with_oauth(|o| {
                let record = o.token(TokenKind::Refresh, original)?.unwrap();
                o.save_token(TokenKind::Refresh, original, &record, now() - 1.0)
            })
            .unwrap();
        let (_, _, description) = error_of(refresh(&oauth, &client, original));
        assert_eq!(description, "refresh token does not exist");
    }

    #[test]
    fn without_a_grace_window_the_old_token_goes_at_once() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 0);
        let (client, tokens) = issue(&oauth);
        let original = tokens["refresh_token"].as_str().unwrap();
        refresh(&oauth, &client, original).unwrap();
        let (_, error, _) = error_of(refresh(&oauth, &client, original));
        assert_eq!(error, "invalid_grant");
    }

    // The cap is copied from record to record, never recomputed, so the two
    // reads must be the same f64.
    #[allow(clippy::float_cmp)]
    #[test]
    fn a_refresh_chain_keeps_its_absolute_cap() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let oauth = provider(store.clone(), 120);
        let (client, tokens) = issue(&oauth);
        let cap = |token: &str| {
            store
                .with_oauth(|o| o.token(TokenKind::Refresh, token))
                .unwrap()
                .unwrap()["absolute_expires_at"]
                .as_f64()
                .unwrap()
        };
        let original = cap(tokens["refresh_token"].as_str().unwrap());
        let next = refresh(&oauth, &client, tokens["refresh_token"].as_str().unwrap()).unwrap();
        assert_eq!(cap(next["refresh_token"].as_str().unwrap()), original);
    }

    #[test]
    fn a_narrower_scope_than_granted_is_all_a_refresh_may_ask_for() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let (client, tokens) = issue(&oauth);
        let result = oauth.token(
            &params(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", tokens["refresh_token"].as_str().unwrap()),
                ("client_id", id(&client)),
                ("client_secret", client["client_secret"].as_str().unwrap()),
                ("scope", "admin"),
            ]),
            None,
        );
        let (status, error, _) = error_of(result);
        assert_eq!((status, error), (StatusCode::BAD_REQUEST, "invalid_scope"));
    }

    #[test]
    fn tokens_survive_a_restart() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let (client, tokens) = issue(&provider(store.clone(), 120));
        let restarted = provider(store, 120);
        assert!(
            restarted
                .verify_access(tokens["access_token"].as_str().unwrap())
                .unwrap()
        );
        refresh(
            &restarted,
            &client,
            tokens["refresh_token"].as_str().unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn revoking_an_access_token_closes_mcp_to_it() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let (client, tokens) = issue(&oauth);
        let access = tokens["access_token"].as_str().unwrap();
        oauth
            .revoke(
                &params(&[
                    ("token", access),
                    ("client_id", id(&client)),
                    ("client_secret", client["client_secret"].as_str().unwrap()),
                ]),
                None,
            )
            .unwrap();
        assert!(!oauth.verify_access(access).unwrap());
    }

    #[test]
    fn public_clients_get_no_secret_and_use_pkce_alone() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let client = register(
            &oauth,
            r#"{"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"], "token_endpoint_auth_method": "none"}"#,
        );
        assert!(client.get("client_secret").is_none());
        assert!(client.get("client_secret_expires_at").is_none());
        let code = sign_in(&oauth, &client);
        exchange(&oauth, &client, &code, VERIFIER).unwrap();
    }

    #[test]
    fn basic_authentication_is_checked_against_the_form() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let client = register(
            &oauth,
            r#"{"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"], "token_endpoint_auth_method": "client_secret_basic"}"#,
        );
        let code = sign_in(&oauth, &client);
        let form = params(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("client_id", id(&client)),
            ("redirect_uri", CALLBACK),
            ("code_verifier", VERIFIER),
        ]);
        let secret = client["client_secret"].as_str().unwrap();
        let header = |user: &str| format!("Basic {}", STANDARD.encode(format!("{user}:{secret}")));
        let (_, error, description) = error_of(oauth.token(&form, Some(&header("someone-else"))));
        assert_eq!(error, "invalid_client");
        assert_eq!(description, "Client ID mismatch in Basic auth");
        let (_, _, description) = error_of(oauth.token(&form, None));
        assert_eq!(
            description,
            "Missing or invalid Basic authentication in Authorization header"
        );
        oauth.token(&form, Some(&header(id(&client)))).unwrap();
    }

    #[test]
    fn secrets_are_stored_only_as_hashes() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let oauth = provider(store.clone(), 120);
        let client = confidential(&oauth);
        let stored = store
            .with_oauth(|o| o.client(id(&client)))
            .unwrap()
            .unwrap();
        let secret = client["client_secret"].as_str().unwrap();
        assert_eq!(secret.len(), 64);
        assert!(stored.get("client_secret").is_none());
        assert_eq!(stored["client_secret_sha256"], secret_hash(secret));
    }

    #[test]
    fn sign_ins_expire_and_are_capped() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let sign_in = |created_at: f64| Pending {
            client_id: "c".into(),
            client_name: None,
            state: None,
            scopes: None,
            code_challenge: "x".into(),
            redirect_uri: CALLBACK.into(),
            redirect_uri_provided_explicitly: true,
            resource: None,
            created_at,
        };
        let stale = oauth.begin_sign_in(sign_in(now() - AUTH_CODE_SECONDS - 1.0));
        assert!(oauth.sign_in_pending(&stale).is_none());
        for _ in 0..PENDING_LIMIT + 5 {
            oauth.begin_sign_in(sign_in(now()));
        }
        assert_eq!(oauth.pending().len(), PENDING_LIMIT);
    }

    #[test]
    fn the_login_page_names_the_client_and_where_it_sends_you() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let client = confidential(&oauth);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes()));
        let Authorize::Redirect(login) = oauth
            .authorize(
                IP,
                &params(&[
                    ("client_id", id(&client)),
                    ("response_type", "code"),
                    ("code_challenge", &challenge),
                ]),
            )
            .unwrap()
        else {
            panic!("expected the login page");
        };
        let session = login.split("session=").nth(1).unwrap();
        assert_eq!(
            oauth.sign_in_pending(session),
            Some(SignInPrompt {
                client: "test".into(),
                redirect_host: "claude.ai".into(),
            })
        );
    }

    #[test]
    fn registrations_are_capped_field_by_field() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let long = "x".repeat(MAX_TEXT_FIELD + 1);
        let refused = |body: String| match oauth.register(IP, body.as_bytes()) {
            Err(Failure::OAuth {
                error: "invalid_client_metadata",
                description,
                ..
            }) => description,
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert!(
            refused(format!(
                r#"{{"redirect_uris": ["https://a.example/cb"], "client_name": "{long}"}}"#
            ))
            .contains("client_name: String should have at most")
        );
        let many: Vec<String> = (0..=MAX_LIST_ITEMS)
            .map(|i| format!("https://a{i}.example/cb"))
            .collect();
        assert!(
            refused(format!(
                r#"{{"redirect_uris": {}}}"#,
                serde_json::to_string(&many).unwrap()
            ))
            .contains("redirect_uris: List should have at most")
        );
        let jwks = format!(r#"{{"keys": ["{}"]}}"#, "k".repeat(MAX_JWKS_BYTES));
        assert!(
            refused(format!(
                r#"{{"redirect_uris": ["https://a.example/cb"], "jwks": {jwks}}}"#
            ))
            .contains("jwks: Input should be at most")
        );
        for bad in [
            "javascript:alert(1)",
            "data:text/html,hi",
            "file:///etc/passwd",
            "https://a.example/cb#fragment",
        ] {
            assert!(
                refused(format!(r#"{{"redirect_uris": ["{bad}"]}}"#)).contains("redirect_uris.0"),
                "{bad} should be refused"
            );
        }
        // An app's own scheme, and loopback http, are how native clients work.
        register(
            &oauth,
            r#"{"redirect_uris": ["myapp://callback", "http://127.0.0.1:3000/cb"]}"#,
        );
    }

    #[test]
    fn pkce_parameters_must_be_well_formed() {
        assert!(pkce_well_formed(VERIFIER));
        assert!(pkce_well_formed(&"a".repeat(128)));
        assert!(!pkce_well_formed(&"a".repeat(42)));
        assert!(!pkce_well_formed(&"a".repeat(129)));
        assert!(!pkce_well_formed(""));
        assert!(!pkce_well_formed(&format!("{}+/=", "a".repeat(43))));

        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let client = confidential(&oauth);
        let Authorize::Redirect(location) = oauth
            .authorize(
                IP,
                &params(&[
                    ("client_id", id(&client)),
                    ("response_type", "code"),
                    ("code_challenge", "short"),
                    ("state", "st"),
                ]),
            )
            .unwrap()
        else {
            panic!("a bad challenge goes back to the client as an error");
        };
        assert!(location.contains("error=invalid_request"), "{location}");
        assert!(location.contains("code_challenge"), "{location}");

        let code = sign_in(&oauth, &client);
        let (status, error, description) = error_of(exchange(&oauth, &client, &code, "short"));
        assert_eq!(
            (status, error),
            (StatusCode::BAD_REQUEST, "invalid_request")
        );
        assert!(description.starts_with("code_verifier"), "{description}");
    }

    #[test]
    fn floods_of_registrations_and_sign_ins_are_throttled_per_address() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        let body = r#"{"redirect_uris": ["https://a.example/cb"]}"#;
        let mut refused = None;
        for _ in 0..=UNAUTHENTICATED_ATTEMPTS {
            if let Err(Failure::OAuth { status, .. }) =
                oauth.register("198.51.100.1", body.as_bytes())
            {
                refused = Some(status);
                break;
            }
        }
        assert_eq!(refused, Some(StatusCode::TOO_MANY_REQUESTS));
        // Another address is unaffected.
        oauth.register("198.51.100.2", body.as_bytes()).unwrap();
    }

    #[test]
    fn the_client_table_is_capped_with_idle_clients_purged_first() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let oauth = provider(store.clone(), 120);
        let body = r#"{"redirect_uris": ["https://a.example/cb"]}"#;
        // Fill the table straight through the store, backdated so every
        // client counts as idle.
        store
            .with_oauth(|o| {
                for i in 0..CLIENT_LIMIT {
                    o.save_client(&format!("c{i}"), &json!({"client_id": format!("c{i}")}))?;
                }
                Ok::<_, StoreError>(())
            })
            .unwrap();
        store
            .read_oauth(|o| {
                o.conn_for_tests().execute(
                    "UPDATE oauth_clients SET created_at = created_at - ?1",
                    [IDLE_CLIENT_SECONDS + 1.0],
                )?;
                Ok::<_, StoreError>(())
            })
            .unwrap();
        let client = oauth.register(IP, body.as_bytes()).unwrap();
        assert!(client["client_id"].is_string());
        // The fn item can't stand in for the closure: `with_oauth` wants it
        // for any store lifetime and the method is tied to one.
        #[allow(clippy::redundant_closure_for_method_calls)]
        let count = store.with_oauth(|o| o.client_count()).unwrap();
        assert!(count <= CLIENT_LIMIT);
    }

    #[test]
    fn the_login_limiter_stays_bounded_across_many_addresses() {
        let mut limiter = LoginLimiter::default();
        let at = 10_000.0;
        for i in 0..LIMITER_ENTRIES * 2 {
            limiter.record_failure(&format!("ip{i}"), (i as f64).mul_add(0.001, at), 900.0);
        }
        assert!(limiter.failures.len() <= LIMITER_ENTRIES + 1);
    }

    #[test]
    fn credentials_need_both_parts() {
        let oauth = provider(Arc::new(Store::open_in_memory().unwrap()), 120);
        assert!(oauth.verify_credentials("admin", "secret123"));
        assert!(!oauth.verify_credentials("admin", "wrong"));
        assert!(!oauth.verify_credentials("notadmin", "secret123"));
    }

    #[test]
    fn the_login_limiter_blocks_resets_and_rolls_off() {
        let mut limiter = LoginLimiter::default();
        let at = 10_000.0;
        assert!(!limiter.is_blocked("ip", at, 3, 900.0));
        for _ in 0..3 {
            limiter.record_failure("ip", at, 900.0);
        }
        assert!(limiter.is_blocked("ip", at, 3, 900.0));
        assert!(
            !limiter.is_blocked("ip", at + 1000.0, 3, 900.0),
            "rolled off"
        );
        for _ in 0..3 {
            limiter.record_failure("ip", at, 900.0);
        }
        limiter.reset("ip");
        assert!(!limiter.is_blocked("ip", at, 3, 900.0));
        for _ in 0..50 {
            limiter.record_failure("other", at, 900.0);
        }
        assert!(!limiter.is_blocked("other", at, 0, 900.0), "0 turns it off");
    }

    #[test]
    fn settings_fall_back_and_clamp_as_6x_did() {
        assert_eq!(clamp_setting("N", None, 30, 1, 90), 30);
        assert_eq!(
            clamp_setting("N", Some("not a number".into()), 30, 1, 90),
            30
        );
        assert_eq!(clamp_setting("N", Some("0".into()), 30, 1, 90), 1);
        assert_eq!(clamp_setting("N", Some("100000".into()), 30, 1, 90), 90);
        assert_eq!(clamp_setting("N", Some("-5".into()), 120, 0, 3600), 0);
    }

    #[test]
    fn urls_are_derived_as_pydantic_serialised_them() {
        let urls = Urls::parse("https://mcp.example.com").unwrap();
        assert_eq!(urls.issuer, "https://mcp.example.com/");
        assert_eq!(urls.prefix, "https://mcp.example.com");
        assert_eq!(urls.resource, "https://mcp.example.com/mcp");
        assert_eq!(
            urls.resource_metadata_url,
            "https://mcp.example.com/.well-known/oauth-protected-resource/mcp"
        );
        let nested = Urls::parse("https://example.com/omnimem/").unwrap();
        assert_eq!(
            nested.resource_metadata_path,
            "/.well-known/oauth-protected-resource/omnimem/mcp"
        );
        assert!(Urls::parse("http://localhost:8765").is_ok());
        assert!(Urls::parse("http://mcp.example.com").is_err());
        assert!(Urls::parse("https://mcp.example.com/?a=1").is_err());
        assert!(Urls::parse("https://mcp.example.com/#x").is_err());
        assert!(Urls::parse("not a url").is_err());
    }

    #[test]
    fn helpers_match_the_python_they_replace() {
        assert_eq!(percent_decode("a%20b+c%zz").as_deref(), Some("a b+c%zz"));
        assert_eq!(python_list(&["a".into(), "b".into()]), "['a', 'b']");
        assert_eq!(
            with_query("https://x.example/cb?keep=1&blank=", &[("error", "a b")]),
            "https://x.example/cb?keep=1&error=a+b"
        );
        let id = uuid4();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_eq!(token_urlsafe().len(), 43);
    }
}
