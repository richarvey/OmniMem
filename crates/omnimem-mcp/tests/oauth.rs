//! The OAuth HTTP surface against what 6.x sent. `fixtures/oauth_golden.json`
//! was captured from the 6.x provider under FastMCP 4.0.3 and mcp 2.1.1 by
//! `fixtures/oauth_golden.py`; random values are masked on both sides.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_mcp::{OAuthConfig, OAuthSetup, ServerConfig, router};
use omnimem_store::Store;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const HOST: &str = "mcp.example.com";
const CALLBACK: &str = "https://claude.ai/api/mcp/auth_callback";

struct Flat;

impl TextEmbedder for Flat {
    fn dimension(&self) -> usize {
        VECTOR_DIM
    }
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts
            .iter()
            .map(|_| {
                let mut v = vec![0.0f32; VECTOR_DIM];
                v[0] = 1.0;
                v
            })
            .collect())
    }
}

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/oauth_golden.json")).unwrap()
}

fn app() -> Router {
    let engine = Arc::new(Engine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Flat),
        EngineConfig::default(),
    ));
    let config = ServerConfig {
        oauth: OAuthSetup::On(OAuthConfig::new(
            "https://mcp.example.com",
            "admin",
            "secret123",
        )),
        ..ServerConfig::default()
    };
    router(engine, &config, CancellationToken::new()).unwrap()
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
}

async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    Reply {
        status,
        headers,
        body: String::from_utf8(body.to_vec()).unwrap(),
    }
}

fn get(path: &str) -> Request<Body> {
    Request::get(path)
        .header(header::HOST, HOST)
        .body(Body::empty())
        .unwrap()
}

fn encode(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

fn form(path: &str, pairs: &[(&str, &str)]) -> Request<Body> {
    Request::post(path)
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(encode(pairs)))
        .unwrap()
}

fn json_post(path: &str, body: &str) -> Request<Body> {
    Request::post(path)
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn mcp_post(authorization: Option<&str>) -> Request<Body> {
    let mut request = Request::post("/mcp")
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(value) = authorization {
        request = request.header(header::AUTHORIZATION, value);
    }
    request
        .body(Body::from(
            r#"{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}"#,
        ))
        .unwrap()
}

/// Mask what is random, as the capture script did.
fn mask(text: &str) -> String {
    let rules = [
        (
            r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
            "<uuid>",
        ),
        (
            r#""client_secret":"[0-9a-f]{64}""#,
            r#""client_secret":"<hex64>""#,
        ),
        (
            r#""client_id_issued_at":\d+"#,
            r#""client_id_issued_at":<now>"#,
        ),
        (r"session=[\w-]+", "session=<id>"),
        (r"code=[\w-]+", "code=<code>"),
    ];
    rules
        .iter()
        .fold(text.to_owned(), |text, (pattern, replacement)| {
            Regex::new(pattern)
                .unwrap()
                .replace_all(&text, *replacement)
                .into_owned()
        })
}

/// Status, the captured headers and the body, masked, as 6.x sent them.
fn assert_as_6x(reply: &Reply, case: &str) {
    assert_as_6x_except(reply, case, &[]);
}

/// The metadata 7.0.7 sends: 6.x's, plus `none` among the token endpoint's
/// auth methods, which registration accepted all along.
const ADVERTISES_NONE: &[(&str, &str)] = &[(
    r#""token_endpoint_auth_methods_supported":["client_secret_post""#,
    r#""token_endpoint_auth_methods_supported":["none","client_secret_post""#,
)];

/// As `assert_as_6x`, with the captured body edited by each deliberate
/// difference first, so the fixture stays exactly what 6.x sent.
fn assert_as_6x_except(reply: &Reply, case: &str, differences: &[(&str, &str)]) {
    let mut golden = golden();
    let body = differences.iter().fold(
        golden[case]["body"].as_str().unwrap_or_default().to_owned(),
        |body, (from, to)| {
            assert!(body.contains(from), "{case}: the 6.x body has no {from}");
            body.replace(from, to)
        },
    );
    golden[case]["body"] = body.into();
    let expected = &golden[case];
    assert!(expected.is_object(), "no golden case {case}");
    assert_eq!(
        u64::from(reply.status.as_u16()),
        expected["status"].as_u64().unwrap(),
        "{case}: status, body {}",
        reply.body
    );
    for (name, value) in expected["headers"].as_object().unwrap() {
        let actual = reply.header(name).map(mask);
        assert_eq!(actual.as_deref(), value.as_str(), "{case}: header {name}");
    }
    assert_eq!(
        mask(&reply.body),
        expected["body"].as_str().unwrap(),
        "{case}: body"
    );
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[tokio::test]
async fn the_http_surface_matches_6x() {
    let app = app();
    let golden = golden();
    assert_eq!(golden["versions"]["fastmcp"], "4.0.3");

    // Discovery. 7.0.7 also answers where a client derives the address from
    // the slashed issuer, or falls back to the root resource document.
    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/oauth-authorization-server/",
    ] {
        assert_as_6x_except(&send(&app, get(path)).await, "as_metadata", ADVERTISES_NONE);
    }
    for path in [
        "/.well-known/oauth-protected-resource/mcp",
        "/.well-known/oauth-protected-resource/mcp/",
        "/.well-known/oauth-protected-resource",
    ] {
        assert_as_6x(&send(&app, get(path)).await, "pr_metadata");
    }
    assert_eq!(
        send(&app, get("/.well-known/openid-configuration"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // The challenges /mcp sends.
    let no_credentials = send(&app, mcp_post(None)).await;
    assert_eq!(no_credentials.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        no_credentials.header("www-authenticate"),
        golden["mcp_no_auth"]["headers"]["www-authenticate"].as_str()
    );
    assert_eq!(no_credentials.body, "");
    assert_as_6x(
        &send(&app, mcp_post(Some("Bearer nope"))).await,
        "mcp_bad_token",
    );
    assert_as_6x(&send(&app, mcp_post(Some("Basic abc"))).await, "mcp_basic");

    // CORS, from the server's own origin (6.x's guard refused others too).
    let preflight = Request::options("/token")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, "https://mcp.example.com")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
        .body(Body::empty())
        .unwrap();
    assert_as_6x(&send(&app, preflight).await, "preflight_token");
    let with_origin = Request::get("/.well-known/oauth-authorization-server")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, "https://mcp.example.com")
        .body(Body::empty())
        .unwrap();
    assert_as_6x_except(
        &send(&app, with_origin).await,
        "metadata_with_origin",
        ADVERTISES_NONE,
    );

    // Registration.
    let registered = send(
        &app,
        json_post(
            "/register",
            r#"{"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"], "client_name": "claudeai", "extra_field": 1}"#,
        ),
    )
    .await;
    assert_as_6x(&registered, "register");
    let client = registered.json();
    let client_id = client["client_id"].as_str().unwrap();
    let secret = client["client_secret"].as_str().unwrap();
    let public = send(
        &app,
        json_post(
            "/register",
            r#"{"redirect_uris": ["http://localhost:3000/cb"], "token_endpoint_auth_method": "none"}"#,
        ),
    )
    .await;
    assert_as_6x(&public, "register_public");
    // 6.x refused a scope it didn't know ("register_bad_scope"); 7.0.7
    // registers the client for the one it has.
    let other_scopes = send(
        &app,
        json_post(
            "/register",
            r#"{"redirect_uris": ["https://a.example/cb"], "scope": "openid admin omnimem"}"#,
        ),
    )
    .await;
    assert_eq!(other_scopes.status, StatusCode::CREATED);
    assert_eq!(other_scopes.json()["scope"], "omnimem");
    assert_as_6x(
        &send(&app, json_post("/register", r#"{"client_name": "x"}"#)).await,
        "register_no_redirects",
    );
    assert_as_6x(
        &send(
            &app,
            json_post(
                "/register",
                r#"{"redirect_uris": ["https://a.example/cb"], "token_endpoint_auth_method": "private_key_jwt"}"#,
            ),
        )
        .await,
        "register_pkjwt",
    );

    // Authorisation.
    let verifier = "a".repeat(50);
    let code_challenge = challenge(&verifier);
    let query = |overrides: &[(&str, Option<&str>)]| {
        let mut pairs: Vec<(&str, &str)> = vec![
            ("client_id", client_id),
            ("redirect_uri", CALLBACK),
            ("response_type", "code"),
            ("code_challenge", code_challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", "st"),
            ("scope", "omnimem"),
            ("resource", "https://mcp.example.com/mcp"),
        ];
        for (name, value) in overrides {
            pairs.retain(|(k, _)| k != name);
            if let Some(value) = value {
                pairs.push((name, value));
            }
        }
        format!("/authorize?{}", encode(&pairs))
    };
    let authorised = send(&app, get(&query(&[]))).await;
    assert_as_6x(&authorised, "authorize");
    for (case, overrides) in [
        (
            "authorize_unknown_client",
            vec![("client_id", Some("nope"))],
        ),
        (
            "authorize_bad_response_type",
            vec![("response_type", Some("token"))],
        ),
        (
            "authorize_bad_redirect",
            vec![("redirect_uri", Some("https://evil.example/cb"))],
        ),
        ("authorize_no_challenge", vec![("code_challenge", None)]),
    ] {
        assert_as_6x(&send(&app, get(&query(&overrides))).await, case);
    }
    // 6.x sent a scope it didn't know back as invalid_scope
    // ("authorize_bad_scope"); 7.0.7 signs in for the scopes it does know.
    for scope in ["admin", "openid offline_access omnimem"] {
        let reply = send(&app, get(&query(&[("scope", Some(scope))]))).await;
        assert_eq!(reply.status, StatusCode::FOUND, "{scope}");
        assert!(
            reply
                .header("location")
                .unwrap()
                .contains("/oauth/login?session="),
            "{scope}"
        );
    }

    // Signing in.
    let location = authorised.header("location").unwrap();
    let session = location.split("session=").nth(1).unwrap().to_owned();
    assert_eq!(
        send(&app, get(&format!("/oauth/login?session={session}")))
            .await
            .status,
        StatusCode::OK
    );
    let wrong = send(
        &app,
        form(
            "/oauth/login",
            &[
                ("session", &session),
                ("username", "admin"),
                ("password", "nope"),
            ],
        ),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let signed_in = send(
        &app,
        form(
            "/oauth/login",
            &[
                ("session", &session),
                ("username", "admin"),
                ("password", "secret123"),
            ],
        ),
    )
    .await;
    assert_as_6x(&signed_in, "login");
    let back = signed_in.header("location").unwrap();
    let code = back
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_owned();

    // The token endpoint.
    let exchange = |overrides: &[(&str, &str)]| {
        let mut pairs: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("client_id", client_id),
            ("client_secret", secret),
            ("redirect_uri", CALLBACK),
            ("code_verifier", verifier.as_str()),
        ];
        for (name, value) in overrides {
            pairs.retain(|(k, _)| k != name);
            pairs.push((name, value));
        }
        form("/token", &pairs)
    };
    let wrong_verifier = "b".repeat(50);
    assert_as_6x(
        &send(&app, exchange(&[("code_verifier", &wrong_verifier)])).await,
        "token_bad_verifier",
    );
    assert_as_6x(
        &send(&app, exchange(&[("client_secret", "x")])).await,
        "token_bad_secret",
    );
    assert_as_6x(
        &send(
            &app,
            form("/token", &[("grant_type", "authorization_code")]),
        )
        .await,
        "token_missing_client",
    );
    assert_as_6x(
        &send(&app, exchange(&[("grant_type", "password")])).await,
        "token_bad_grant",
    );
    let issued = send(&app, exchange(&[])).await;
    let tokens = issued.json();
    let masked = Reply {
        status: issued.status,
        headers: issued.headers.clone(),
        body: issued
            .body
            .replace(tokens["access_token"].as_str().unwrap(), "<access>")
            .replace(tokens["refresh_token"].as_str().unwrap(), "<refresh>"),
    };
    assert_as_6x(&masked, "token");
    assert_as_6x(&send(&app, exchange(&[])).await, "token_code_reuse");

    // Refreshing, and the grace window's replay.
    let refresh = |token: &str, scope: Option<&str>| {
        let mut pairs = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", token),
            ("client_id", client_id),
            ("client_secret", secret),
        ];
        if let Some(scope) = scope {
            pairs.push(("scope", scope));
        }
        form("/token", &pairs)
    };
    let first = send(
        &app,
        refresh(tokens["refresh_token"].as_str().unwrap(), None),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK);
    let replay = send(
        &app,
        refresh(tokens["refresh_token"].as_str().unwrap(), None),
    )
    .await;
    assert_eq!(first.json(), replay.json());
    assert_eq!(golden["refresh_replay_same_pair"], true);
    let rotated = first.json();
    assert_as_6x(
        &send(
            &app,
            refresh(rotated["refresh_token"].as_str().unwrap(), Some("admin")),
        )
        .await,
        "refresh_bad_scope",
    );

    // The access token opens /mcp, until it is revoked.
    let bearer = format!("Bearer {}", rotated["access_token"].as_str().unwrap());
    let response = app.clone().oneshot(mcp_post(Some(&bearer))).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let revoked = send(
        &app,
        form(
            "/revoke",
            &[
                ("token", rotated["access_token"].as_str().unwrap()),
                ("client_id", client_id),
                ("client_secret", secret),
            ],
        ),
    )
    .await;
    assert_as_6x(&revoked, "revoke");
    assert_eq!(
        send(&app, mcp_post(Some(&bearer))).await.status,
        StatusCode::UNAUTHORIZED
    );

    // A deliberate difference: 6.x demanded a client_secret field even from a
    // public client, which has none to send.
    assert_eq!(golden["revoke_public_no_secret"]["status"], 400);
    let public_id = public.json()["client_id"].as_str().unwrap().to_owned();
    assert_eq!(
        send(
            &app,
            form("/revoke", &[("token", "x"), ("client_id", &public_id)])
        )
        .await
        .status,
        StatusCode::OK
    );

    // The icon, and the login page's refusal.
    assert_as_6x(&send(&app, get("/favicon.ico")).await, "icon");
    let refused = send(&app, get("/oauth/login?session=bogus")).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        refused.header("content-type"),
        golden["login_page_bad_session"]["headers"]["content-type"].as_str()
    );
    assert!(refused.body.contains("Invalid or expired session."));
}

#[tokio::test]
async fn the_oauth_routes_refuse_foreign_hosts_and_origins() {
    let app = app();
    let foreign_host = Request::get("/.well-known/oauth-authorization-server")
        .header(header::HOST, "evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&app, foreign_host).await.status,
        StatusCode::MISDIRECTED_REQUEST
    );
    let login = |origin: &str| {
        Request::post("/oauth/login")
            .header(header::HOST, HOST)
            .header(header::ORIGIN, origin)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("session=x&username=a&password=b"))
            .unwrap()
    };
    assert_eq!(
        send(&app, login("https://evil.example")).await.status,
        StatusCode::FORBIDDEN
    );
    // A proxy that terminates TLS: the browser's https origin, over http.
    assert_eq!(
        send(&app, login("http://mcp.example.com")).await.status,
        StatusCode::BAD_REQUEST,
        "reaches the form, which says the session expired"
    );
    assert_eq!(
        send(&app, get("/healthz")).await.status,
        StatusCode::OK,
        "the health check needs nothing"
    );
}

/// Discovery, registration and the token exchange answer a browser on any
/// origin (Mistral's Le Chat makes them from chat.mistral.ai, and stopped at
/// a 403 in 7.0.6), while the Host check still applies and `/authorize`, the
/// login form and `/mcp` still want an allowed origin.
#[tokio::test]
async fn any_origin_reaches_the_public_oauth_routes_but_not_sign_in() {
    let app = app();
    let with_origin = |method: &str, uri: &str, host: &str, body: &str| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, host)
            .header(header::ORIGIN, "https://chat.mistral.ai")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    };
    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        assert_eq!(
            send(&app, with_origin("GET", path, HOST, "")).await.status,
            StatusCode::OK,
            "{path}"
        );
    }
    let registered = send(
        &app,
        with_origin(
            "POST",
            "/register",
            HOST,
            r#"{"redirect_uris": ["https://chat.mistral.ai/cb"], "token_endpoint_auth_method": "none"}"#,
        ),
    )
    .await;
    assert_eq!(registered.status, StatusCode::CREATED);
    assert_eq!(
        registered.header("access-control-allow-origin"),
        Some("*"),
        "a browser may read the reply"
    );
    let client_id = registered.json()["client_id"].as_str().unwrap().to_owned();
    assert_eq!(
        send(&app, with_origin("POST", "/token", HOST, ""))
            .await
            .status,
        StatusCode::UNAUTHORIZED,
        "reaches the token endpoint, which asks who the client is"
    );
    assert_eq!(
        send(
            &app,
            with_origin(
                "GET",
                "/.well-known/oauth-authorization-server",
                "evil.example",
                ""
            )
        )
        .await
        .status,
        StatusCode::MISDIRECTED_REQUEST,
        "the Host check still applies"
    );
    let authorize = format!(
        "/authorize?response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fchat.mistral.ai%2Fcb&code_challenge={}&code_challenge_method=S256",
        challenge("verifier-verifier-verifier-verifier-verifier")
    );
    for (method, uri) in [
        ("GET", authorize.as_str()),
        ("POST", "/oauth/login"),
        ("POST", "/mcp"),
    ] {
        assert_eq!(
            send(&app, with_origin(method, uri, HOST, "")).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn repeated_failed_logins_are_throttled() {
    let app = app();
    let client = send(
        &app,
        json_post(
            "/register",
            r#"{"redirect_uris": ["https://claude.ai/api/mcp/auth_callback"]}"#,
        ),
    )
    .await
    .json();
    let code_challenge = challenge("verifier-verifier-verifier-verifier-verifier");
    let authorised = send(
        &app,
        get(&format!(
            "/authorize?{}",
            encode(&[
                ("client_id", client["client_id"].as_str().unwrap()),
                ("response_type", "code"),
                ("code_challenge", &code_challenge),
            ])
        )),
    )
    .await;
    assert_eq!(authorised.status, StatusCode::FOUND, "{}", authorised.body);
    let session = authorised
        .header("location")
        .unwrap()
        .split("session=")
        .nth(1)
        .unwrap()
        .to_owned();
    let attempt = || {
        form(
            "/oauth/login",
            &[
                ("session", &session),
                ("username", "admin"),
                ("password", "wrong"),
            ],
        )
    };
    for _ in 0..10 {
        assert_eq!(send(&app, attempt()).await.status, StatusCode::UNAUTHORIZED);
    }
    let blocked = send(&app, attempt()).await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(blocked.body.contains("Too many failed attempts"));
}

#[tokio::test]
async fn a_change_made_with_an_oauth_token_names_the_client_it_was_issued_to() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(Engine::new(
        store.clone(),
        Arc::new(Flat),
        EngineConfig {
            audit_log: omnimem_engine::AuditLevel::Writes,
            ..EngineConfig::default()
        },
    ));
    let config = ServerConfig {
        oauth: OAuthSetup::On(OAuthConfig::new(
            "https://mcp.example.com",
            "admin",
            "secret123",
        )),
        ..ServerConfig::default()
    };
    let app = router(engine, &config, CancellationToken::new()).unwrap();
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        + 3600.0;
    store
        .with_oauth(|o| {
            o.save_token(
                omnimem_store::TokenKind::Access,
                "live-access-token",
                &serde_json::json!({"client_id": "client-abc"}),
                expires,
            )
        })
        .unwrap();

    let post = |body: &str, session: Option<&str>| {
        let mut request = Request::post("/mcp")
            .header(header::HOST, HOST)
            .header(header::AUTHORIZATION, "Bearer live-access-token")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream");
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        request.body(Body::from(body.to_owned())).unwrap()
    };
    let init = send(
        &app,
        post(
            r#"{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "claude-ai", "version": "1"}}}"#,
            None,
        ),
    )
    .await;
    let session = init.header("mcp-session-id").unwrap().to_owned();
    send(
        &app,
        post(
            r#"{"jsonrpc": "2.0", "method": "notifications/initialized"}"#,
            Some(&session),
        ),
    )
    .await;
    let reply = send(
        &app,
        post(
            r#"{"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "remember", "arguments": {"content": "said over oauth", "mode": "raw"}}}"#,
            Some(&session),
        ),
    )
    .await;
    assert!(reply.body.contains("mem:episodic:"), "{}", reply.body);

    let rows = store
        .audit_entries(&omnimem_store::AuditQuery::default())
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(rows[0].actor, "mcp");
    assert_eq!(rows[0].client, "claude-ai (client-abc)");
}

/// `OAUTH_BASE_URL` listing two addresses: each Host gets documents, a
/// challenge and a login page on its own address, so a client that reached
/// the server by one of them never sees the other. `*` answers any Host the
/// guard lets in, and the guard still refuses the rest.
#[tokio::test]
async fn each_listed_base_url_answers_on_its_own_address() {
    let engine = || {
        Arc::new(Engine::new(
            Arc::new(Store::open_in_memory().unwrap()),
            Arc::new(Flat),
            EngineConfig::default(),
        ))
    };
    let with = |base: &str, extra_hosts: Vec<String>| ServerConfig {
        oauth: OAuthSetup::On(OAuthConfig::new(base, "admin", "secret123")),
        extra_hosts,
        ..ServerConfig::default()
    };
    let at = |host: &str, request: axum::http::request::Builder| {
        request
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap()
    };
    let app = router(
        engine(),
        &with(
            "https://box.tail.ts.net,https://mcp.example.com",
            Vec::new(),
        ),
        CancellationToken::new(),
    )
    .unwrap();
    for host in ["box.tail.ts.net", "mcp.example.com"] {
        let resource = send(
            &app,
            at(
                host,
                Request::get("/.well-known/oauth-protected-resource/mcp"),
            ),
        )
        .await;
        assert_eq!(resource.status, StatusCode::OK, "{host}");
        assert_eq!(resource.json()["resource"], format!("https://{host}/mcp"));
        assert_eq!(
            resource.json()["authorization_servers"][0],
            format!("https://{host}/")
        );
        assert_eq!(resource.header("vary"), Some("Host"));
        let server = send(
            &app,
            at(
                host,
                Request::get("/.well-known/oauth-authorization-server"),
            ),
        )
        .await
        .json();
        assert_eq!(server["issuer"], format!("https://{host}/"));
        assert_eq!(server["token_endpoint"], format!("https://{host}/token"));
        let challenge = send(&app, at(host, Request::post("/mcp"))).await;
        assert_eq!(challenge.status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            challenge.header("www-authenticate"),
            Some(
                format!(
                    r#"Bearer resource_metadata="https://{host}/.well-known/oauth-protected-resource/mcp""#
                )
                .as_str()
            )
        );
    }
    // Sign-in sends the browser to the login page on the address it used.
    let registered = send(
        &app,
        Request::post("/register")
            .header(header::HOST, "mcp.example.com")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"redirect_uris": ["https://chat.mistral.ai/cb"], "token_endpoint_auth_method": "none"}"#,
            ))
            .unwrap(),
    )
    .await;
    let client_id = registered.json()["client_id"].as_str().unwrap().to_owned();
    let authorize = format!(
        "/authorize?response_type=code&client_id={client_id}&redirect_uri=https%3A%2F%2Fchat.mistral.ai%2Fcb&code_challenge={}&code_challenge_method=S256",
        challenge("verifier-verifier-verifier-verifier-verifier")
    );
    let redirect = send(
        &app,
        at("mcp.example.com", Request::get(authorize.as_str())),
    )
    .await;
    assert!(
        redirect
            .header("location")
            .unwrap()
            .starts_with("https://mcp.example.com/oauth/login?session="),
        "{:?}",
        redirect.header("location")
    );
    // A host that isn't listed is still refused.
    assert_eq!(
        send(
            &app,
            at(
                "evil.example",
                Request::get("/.well-known/oauth-authorization-server")
            )
        )
        .await
        .status,
        StatusCode::MISDIRECTED_REQUEST
    );

    // `*`: the request's own address, for the hosts MCP_ALLOWED_HOSTS lets in.
    let any = router(
        engine(),
        &with("*", vec!["x.trycloudflare.com".into()]),
        CancellationToken::new(),
    )
    .unwrap();
    let resource = send(
        &any,
        at(
            "x.trycloudflare.com",
            Request::get("/.well-known/oauth-protected-resource/mcp"),
        ),
    )
    .await;
    assert_eq!(
        resource.json()["resource"],
        "https://x.trycloudflare.com/mcp"
    );
    assert_eq!(
        send(
            &any,
            at(
                "evil.example",
                Request::get("/.well-known/oauth-protected-resource/mcp")
            )
        )
        .await
        .status,
        StatusCode::MISDIRECTED_REQUEST,
        "* names addresses, it doesn't let hosts in"
    );
}
