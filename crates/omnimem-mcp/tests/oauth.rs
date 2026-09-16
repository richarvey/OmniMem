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
    let golden = golden();
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

    // Discovery, and nothing where 6.x had nothing.
    assert_as_6x(
        &send(&app, get("/.well-known/oauth-authorization-server")).await,
        "as_metadata",
    );
    assert_as_6x(
        &send(&app, get("/.well-known/oauth-protected-resource/mcp")).await,
        "pr_metadata",
    );
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/openid-configuration",
    ] {
        assert_eq!(
            send(&app, get(path)).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }

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
    assert_as_6x(&send(&app, with_origin).await, "metadata_with_origin");

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
    assert_as_6x(
        &send(
            &app,
            json_post(
                "/register",
                r#"{"redirect_uris": ["https://a.example/cb"], "scope": "admin"}"#,
            ),
        )
        .await,
        "register_bad_scope",
    );
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
        ("authorize_bad_scope", vec![("scope", Some("admin"))]),
        ("authorize_no_challenge", vec![("code_challenge", None)]),
    ] {
        assert_as_6x(&send(&app, get(&query(&overrides))).await, case);
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
