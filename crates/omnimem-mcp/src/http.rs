//! The HTTP side: streamable HTTP at `/mcp`, bearer and OAuth authentication,
//! the OAuth routes, Host and Origin allowlists, and the fail-closed rule for
//! public binds.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use omnimem_engine::Engine;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::OmniMemServer;
use crate::oauth::{self, OAuth, OAuthSetup};

const LOOPBACK: [&str; 4] = ["127.0.0.1", "localhost", "::1", ""];

/// What FastMCP told a client whose bearer token it rejected, kept because
/// clients show it and it says what to do.
const INVALID_TOKEN: &str = "Authentication failed. The provided bearer token is invalid, \
    expired, or no longer recognized by the server. To resolve: clear authentication tokens in \
    your MCP client and reconnect. Your client should automatically re-register and obtain new \
    tokens.";

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(
        "refusing to start: MCP_HOST={0} is not loopback but no authentication is configured. \
         Set MCP_AUTH_TOKEN or turn on OAuth (OAUTH_ENABLED), or bind MCP_HOST to 127.0.0.1"
    )]
    Unauthenticated(String),
    #[error("refusing to start: {0}")]
    OAuth(String),
    #[error("could not bind {addr}: {source}")]
    Bind {
        addr: String,
        source: std::io::Error,
    },
    #[error("server error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// `MCP_HOST`
    pub host: String,
    /// `MCP_PORT`
    pub port: u16,
    /// `MCP_AUTH_TOKEN`: a shared bearer secret.
    pub auth_token: Option<String>,
    /// `OAUTH_ENABLED` and the settings that go with it.
    pub oauth: OAuthSetup,
    /// `OAUTH_BASE_URL` and `MCP_PUBLIC_URL`: their hosts and origins are trusted.
    pub public_urls: Vec<String>,
    /// `MCP_ALLOWED_HOSTS`
    pub extra_hosts: Vec<String>,
    /// `MCP_ALLOWED_ORIGINS`
    pub extra_origins: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8765,
            auth_token: None,
            oauth: OAuthSetup::Off,
            public_urls: Vec::new(),
            extra_hosts: Vec::new(),
            extra_origins: Vec::new(),
        }
    }
}

pub(crate) fn var(name: &str) -> Option<String> {
    omnimem_core::env::var(name)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn list(name: &str) -> Vec<String> {
    var(name)
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// `scheme://host[:port]` and the bare host of a URL, without a URL crate.
fn split_url(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    let authority = authority.rsplit('@').next()?;
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        stripped.split(']').next()?.to_owned()
    } else {
        authority.split(':').next()?.to_owned()
    };
    Some((format!("{}://{authority}", scheme.to_lowercase()), host))
}

impl ServerConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            host: var("MCP_HOST").unwrap_or(defaults.host),
            port: var("MCP_PORT")
                .and_then(|p| p.parse().ok())
                .unwrap_or(defaults.port),
            auth_token: var("MCP_AUTH_TOKEN"),
            oauth: OAuthSetup::from_env(),
            public_urls: ["OAUTH_BASE_URL", "MCP_PUBLIC_URL"]
                .iter()
                .filter_map(|n| var(n))
                .collect(),
            extra_hosts: list("MCP_ALLOWED_HOSTS"),
            extra_origins: list("MCP_ALLOWED_ORIGINS"),
        }
    }

    /// The fail-closed rule: a non-loopback bind needs authentication, and
    /// OAuth that is switched on has to be usable.
    pub fn validate(&self) -> Result<(), ServerError> {
        if let OAuthSetup::Invalid(problem) = &self.oauth {
            return Err(ServerError::OAuth(problem.clone()));
        }
        let authenticated = self.auth_token.is_some() || matches!(self.oauth, OAuthSetup::On(_));
        if !authenticated && !LOOPBACK.contains(&self.host.as_str()) {
            return Err(ServerError::Unauthenticated(self.host.clone()));
        }
        Ok(())
    }

    /// The public URLs, including the OAuth base URL however it was set.
    fn trusted_urls(&self) -> impl Iterator<Item = (String, String)> + '_ {
        let oauth_base = match &self.oauth {
            OAuthSetup::On(config) => Some(config.base_url.as_str()),
            _ => None,
        };
        self.public_urls
            .iter()
            .map(String::as_str)
            .chain(oauth_base)
            .filter_map(split_url)
    }

    fn allowed_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"]
            .iter()
            .map(|h| (*h).to_owned())
            .collect();
        if !matches!(self.host.as_str(), "0.0.0.0" | "::" | "") {
            hosts.push(self.host.clone());
        }
        hosts.extend(self.trusted_urls().map(|(_, h)| h));
        hosts.extend(self.extra_hosts.iter().cloned());
        dedupe(hosts)
    }

    fn allowed_origins(&self) -> Vec<String> {
        let mut origins: Vec<String> = ["localhost", "127.0.0.1"]
            .iter()
            .map(|h| format!("http://{h}:{}", self.port))
            .collect();
        origins.extend(self.trusted_urls().map(|(o, _)| o));
        origins.extend(self.extra_origins.iter().cloned());
        dedupe(origins)
    }
}

fn dedupe(items: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// Length-independent comparison, so a timing side channel can't recover a
/// secret byte by byte.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

/// What `/mcp` accepts: the shared token, OAuth access tokens, or either.
#[derive(Clone)]
struct McpAuth {
    token: Option<Arc<str>>,
    oauth: Option<Arc<OAuth>>,
}

impl McpAuth {
    fn shared_token_matches(&self, presented: &str) -> bool {
        self.token.as_deref().is_some_and(|token| {
            !presented.is_empty() && constant_time_eq(presented.as_bytes(), token.as_bytes())
        })
    }
}

fn unauthorised(body: String, challenge: &str) -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

async fn require_auth(State(auth): State<McpAuth>, request: Request, next: Next) -> Response {
    let presented_header = request.headers().get(header::AUTHORIZATION).cloned();

    let Some(oauth) = &auth.oauth else {
        let presented = presented_header
            .as_ref()
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .map(str::trim);
        if presented.is_some_and(|p| auth.shared_token_matches(p)) {
            return next.run(request).await;
        }
        return unauthorised(
            r#"{"error": "invalid_token", "error_description": "Authentication required"}"#.into(),
            "Bearer",
        );
    };

    let Some(value) = presented_header else {
        // RFC 6750 §3.1: a request with no credentials gets no error
        // attribute, only where to find out how to get some.
        let mut response = StatusCode::UNAUTHORIZED.into_response();
        if let Ok(challenge) = HeaderValue::from_str(&format!(
            r#"Bearer resource_metadata="{}""#,
            oauth.resource_metadata_url()
        )) {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, challenge);
        }
        return response;
    };
    let presented = value.to_str().ok().and_then(|v| {
        v.get(..7)
            .filter(|scheme| scheme.eq_ignore_ascii_case("bearer "))
            .map(|_| &v[7..])
    });
    if presented.is_some_and(|p| auth.shared_token_matches(p.trim()) || oauth.verify_access(p)) {
        return next.run(request).await;
    }
    unauthorised(
        format!(r#"{{"error": "invalid_token", "error_description": "{INVALID_TOKEN}"}}"#),
        &format!(
            r#"Bearer error="invalid_token", error_description="{INVALID_TOKEN}", resource_metadata="{}""#,
            oauth.resource_metadata_url()
        ),
    )
}

/// The application: `/mcp` (authenticated when a token or OAuth is set), the
/// OAuth routes when OAuth is on, and an unauthenticated `/healthz` for
/// container health checks.
pub fn router(
    engine: Arc<Engine>,
    config: &ServerConfig,
    shutdown: CancellationToken,
) -> Result<Router, ServerError> {
    config.validate()?;
    let oauth = match &config.oauth {
        OAuthSetup::On(settings) => Some(Arc::new(
            OAuth::new(settings.clone(), engine.store().clone()).map_err(ServerError::OAuth)?,
        )),
        OAuthSetup::Off | OAuthSetup::Invalid(_) => None,
    };
    let server = OmniMemServer::new(engine);
    let mut http_config = StreamableHttpServerConfig::default();
    http_config.allowed_hosts = config.allowed_hosts();
    http_config.allowed_origins = config.allowed_origins();
    http_config.cancellation_token = shutdown;
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );

    let mut app = Router::new().nest_service("/mcp", service);
    if config.auth_token.is_some() || oauth.is_some() {
        app = app.layer(middleware::from_fn_with_state(
            McpAuth {
                token: config.auth_token.as_deref().map(Arc::from),
                oauth: oauth.clone(),
            },
            require_auth,
        ));
    }
    app = app.route("/healthz", get(|| async { r#"{"status": "ok"}"# }));
    if let Some(oauth) = oauth {
        let allow = oauth::Allowlists::new(&config.allowed_hosts(), &config.allowed_origins());
        app = app
            .merge(oauth::routes(oauth).layer(middleware::from_fn_with_state(allow, oauth::guard)));
    }
    Ok(app)
}

/// Serve until `shutdown` is cancelled. Pass a listener to choose the socket
/// (tests bind port 0); otherwise `host:port` from the config is bound.
pub async fn serve(
    engine: Arc<Engine>,
    config: ServerConfig,
    listener: Option<TcpListener>,
    shutdown: CancellationToken,
) -> Result<(), ServerError> {
    let app = router(engine, &config, shutdown.clone())?;
    let listener = match listener {
        Some(l) => l,
        None => {
            let addr = format!("{}:{}", config.host, config.port);
            TcpListener::bind(&addr)
                .await
                .map_err(|source| ServerError::Bind { addr, source })?
        }
    };
    let local: SocketAddr = listener.local_addr()?;
    let auth = match (
        config.auth_token.is_some(),
        matches!(config.oauth, OAuthSetup::On(_)),
    ) {
        (true, true) => "OAuth and bearer",
        (false, true) => "OAuth",
        (true, false) => "bearer",
        (false, false) => "none (loopback)",
    };
    info!(address = %local, auth, "OmniMem MCP server listening on /mcp");
    // The client address feeds the OAuth login rate limit.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { shutdown.cancelled().await })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OAuthConfig;

    #[test]
    fn urls_split_into_origin_and_host() {
        assert_eq!(
            split_url("https://mcp.example.com/path"),
            Some(("https://mcp.example.com".into(), "mcp.example.com".into()))
        );
        assert_eq!(
            split_url("HTTP://host.ts.net:8443"),
            Some(("http://host.ts.net:8443".into(), "host.ts.net".into()))
        );
        assert_eq!(split_url("not a url"), None);
    }

    #[test]
    fn allowlists_include_public_urls_and_extras() {
        let config = ServerConfig {
            host: "0.0.0.0".into(),
            port: 8765,
            auth_token: Some("t".into()),
            oauth: OAuthSetup::Off,
            public_urls: vec!["https://mcp.example.com".into()],
            extra_hosts: vec!["extra.local".into()],
            extra_origins: vec!["https://ui.example.com".into()],
        };
        let hosts = config.allowed_hosts();
        assert!(hosts.contains(&"mcp.example.com".to_owned()));
        assert!(hosts.contains(&"extra.local".to_owned()));
        assert!(!hosts.contains(&"0.0.0.0".to_owned()));
        let origins = config.allowed_origins();
        assert!(origins.contains(&"https://mcp.example.com".to_owned()));
        assert!(origins.contains(&"https://ui.example.com".to_owned()));
    }

    #[test]
    fn oauth_counts_as_authentication_and_its_base_url_is_trusted() {
        let config = ServerConfig {
            host: "0.0.0.0".into(),
            oauth: OAuthSetup::On(OAuthConfig::new("https://oauth.example.com", "admin", "pw")),
            ..ServerConfig::default()
        };
        assert!(config.validate().is_ok());
        assert!(
            config
                .allowed_hosts()
                .contains(&"oauth.example.com".to_owned())
        );
        assert!(
            config
                .allowed_origins()
                .contains(&"https://oauth.example.com".to_owned())
        );
        let open = ServerConfig {
            host: "0.0.0.0".into(),
            ..ServerConfig::default()
        };
        assert!(matches!(
            open.validate(),
            Err(ServerError::Unauthenticated(_))
        ));
    }

    #[test]
    fn misconfigured_oauth_refuses_to_start_even_on_loopback() {
        let config = ServerConfig {
            oauth: OAuthSetup::Invalid("OAUTH_BASE_URL is missing".into()),
            ..ServerConfig::default()
        };
        let error = config.validate().unwrap_err().to_string();
        assert_eq!(error, "refusing to start: OAUTH_BASE_URL is missing");
    }

    #[test]
    fn the_config_debug_never_shows_the_admin_password() {
        let config = OAuthConfig::new("https://oauth.example.com", "admin", "hunter2");
        assert!(!format!("{config:?}").contains("hunter2"));
    }

    #[test]
    fn constant_time_comparison() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secrets"));
    }
}
