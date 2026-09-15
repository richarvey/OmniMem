//! The HTTP side: streamable HTTP at `/mcp`, bearer auth, Host and Origin
//! allowlists, and the fail-closed rule for public binds.

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

const LOOPBACK: [&str; 4] = ["127.0.0.1", "localhost", "::1", ""];

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(
        "refusing to start: MCP_HOST={0} is not loopback but no authentication is configured. \
         Set MCP_AUTH_TOKEN, or bind MCP_HOST to 127.0.0.1"
    )]
    Unauthenticated(String),
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
            public_urls: Vec::new(),
            extra_hosts: Vec::new(),
            extra_origins: Vec::new(),
        }
    }
}

fn var(name: &str) -> Option<String> {
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
            public_urls: ["OAUTH_BASE_URL", "MCP_PUBLIC_URL"]
                .iter()
                .filter_map(|n| var(n))
                .collect(),
            extra_hosts: list("MCP_ALLOWED_HOSTS"),
            extra_origins: list("MCP_ALLOWED_ORIGINS"),
        }
    }

    /// The fail-closed rule: a non-loopback bind needs authentication.
    pub fn validate(&self) -> Result<(), ServerError> {
        if self.auth_token.is_none() && !LOOPBACK.contains(&self.host.as_str()) {
            return Err(ServerError::Unauthenticated(self.host.clone()));
        }
        Ok(())
    }

    fn allowed_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"]
            .iter()
            .map(|h| (*h).to_owned())
            .collect();
        if !matches!(self.host.as_str(), "0.0.0.0" | "::" | "") {
            hosts.push(self.host.clone());
        }
        hosts.extend(
            self.public_urls
                .iter()
                .filter_map(|u| split_url(u))
                .map(|(_, h)| h),
        );
        hosts.extend(self.extra_hosts.iter().cloned());
        dedupe(hosts)
    }

    fn allowed_origins(&self) -> Vec<String> {
        let mut origins: Vec<String> = ["localhost", "127.0.0.1"]
            .iter()
            .map(|h| format!("http://{h}:{}", self.port))
            .collect();
        origins.extend(
            self.public_urls
                .iter()
                .filter_map(|u| split_url(u))
                .map(|(o, _)| o),
        );
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

/// Length-independent comparison, so a timing side channel can't recover the
/// token byte by byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

async fn require_bearer(
    State(token): State<Arc<String>>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim);
    match presented {
        Some(p) if !p.is_empty() && constant_time_eq(p.as_bytes(), token.as_bytes()) => {
            next.run(request).await
        }
        _ => {
            let mut response = (
                StatusCode::UNAUTHORIZED,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"error": "invalid_token", "error_description": "Authentication required"}"#,
            )
                .into_response();
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            response
        }
    }
}

/// The application: `/mcp` (authenticated when a token is set) and an
/// unauthenticated `/healthz` for container health checks.
pub fn router(
    engine: Arc<Engine>,
    config: &ServerConfig,
    shutdown: CancellationToken,
) -> Result<Router, ServerError> {
    config.validate()?;
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

    let mut mcp = Router::new().nest_service("/mcp", service);
    if let Some(token) = &config.auth_token {
        mcp = mcp.layer(middleware::from_fn_with_state(
            Arc::new(token.clone()),
            require_bearer,
        ));
    }
    Ok(mcp.route("/healthz", get(|| async { r#"{"status": "ok"}"# })))
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
    info!(
        address = %local,
        auth = if config.auth_token.is_some() { "bearer" } else { "none (loopback)" },
        "OmniMem MCP server listening on /mcp"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn constant_time_comparison() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secrets"));
    }
}
