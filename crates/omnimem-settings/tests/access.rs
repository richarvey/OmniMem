//! The panel as the HTTP server serves it: `/admin` behind sign-in.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, Response, StatusCode};
use omnimem_core::{EmbeddingError, TextEmbedder, VECTOR_DIM};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_settings::{AccessConfig, Panel, mint_sign_in_code, sign_in_url};
use omnimem_store::Store;
use tower::ServiceExt;

struct Flat;

impl TextEmbedder for Flat {
    fn dimension(&self) -> usize {
        VECTOR_DIM
    }
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts.iter().map(|_| vec![0.0f32; VECTOR_DIM]).collect())
    }
}

struct Fixture {
    app: Router,
    store: Arc<Store>,
}

fn fixture(config: AccessConfig) -> Fixture {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Arc::new(Engine::new(
        store.clone(),
        Arc::new(Flat),
        EngineConfig::default(),
    ));
    let panel = Panel::new();
    panel.set_engine(engine);
    Fixture {
        app: panel.admin_router(store.clone(), config),
        store,
    }
}

fn with_password() -> AccessConfig {
    AccessConfig {
        credentials: Some(("admin".into(), "hunter2".into())),
        max_attempts: 3,
        ..AccessConfig::default()
    }
}

const LOCAL: &str = "127.0.0.1:50000";
const REMOTE: &str = "203.0.113.9:50000";

fn request(method: &str, uri: &str, peer: &str) -> axum::http::request::Builder {
    let peer: SocketAddr = peer.parse().unwrap();
    Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:8765")
        .extension(ConnectInfo(peer))
}

async fn send(f: &Fixture, request: Request<Body>) -> Response<Body> {
    f.app.clone().oneshot(request).await.unwrap()
}

async fn get(f: &Fixture, uri: &str, cookie: Option<&str>) -> Response<Body> {
    let mut builder = request("GET", uri, LOCAL);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    send(f, builder.body(Body::empty()).unwrap()).await
}

fn location(response: &Response<Body>) -> String {
    response.headers()["location"].to_str().unwrap().to_owned()
}

/// `name=value` from the Set-Cookie header, as a browser would send it back.
fn session_cookie(response: &Response<Body>) -> String {
    let set = response.headers()["set-cookie"].to_str().unwrap();
    set.split(';').next().unwrap().to_owned()
}

async fn text(response: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn signed_in(f: &Fixture) -> String {
    let code = mint_sign_in_code(&f.store, Duration::from_secs(60)).unwrap();
    let response = get(f, &format!("/admin/sign-in?code={code}"), None).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    session_cookie(&response)
}

#[tokio::test]
async fn a_visitor_without_a_session_is_sent_to_sign_in() {
    let f = fixture(AccessConfig::default());
    let response = get(&f, "/admin/memories?page=2", None).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        location(&response),
        "/admin/login?next=/memories%3Fpage%3D2"
    );
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    assert_eq!(
        response.headers()["referrer-policy"],
        "same-origin",
        "no-referrer makes browsers send Origin: null on form POSTs, which the Origin guard refuses"
    );

    let login = get(&f, "/admin/login", None).await;
    assert_eq!(login.status(), StatusCode::OK);
    let page = text(login).await;
    assert!(
        page.contains("omnimem admin-link"),
        "no password, so the page says how else: {page}"
    );
    assert!(!page.contains("type=\"password\""));
}

#[tokio::test]
async fn a_sign_in_link_works_once_and_its_session_opens_the_panel() {
    let f = fixture(AccessConfig::default());
    let code = mint_sign_in_code(&f.store, Duration::from_secs(60)).unwrap();
    assert_eq!(
        sign_in_url("http://127.0.0.1:8765/", &code),
        format!("http://127.0.0.1:8765/admin/sign-in?code={code}")
    );
    let response = get(&f, &format!("/admin/sign-in?code={code}"), None).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/admin/");
    let set = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        set.contains("HttpOnly") && set.contains("SameSite=Strict") && set.contains("Path=/admin"),
        "{set}"
    );
    assert!(
        !set.contains("Secure"),
        "plain HTTP on loopback can't carry a Secure cookie"
    );
    let cookie = session_cookie(&response);

    let page = get(&f, "/admin/", Some(&cookie)).await;
    let status = page.status();
    let html = text(page).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(
        html.contains("<title>Dashboard — OmniMem</title>"),
        "{html}"
    );
    assert_eq!(
        get(&f, "/admin", Some(&cookie)).await.status(),
        StatusCode::OK
    );

    let again = get(&f, &format!("/admin/sign-in?code={code}"), None).await;
    assert!(
        location(&again).starts_with("/admin/login?error="),
        "{}",
        location(&again)
    );
    assert!(again.headers().get("set-cookie").is_none());
}

#[tokio::test]
async fn an_expired_link_is_refused() {
    let f = fixture(AccessConfig::default());
    let code = mint_sign_in_code(&f.store, Duration::ZERO).unwrap();
    let response = get(&f, &format!("/admin/sign-in?code={code}"), None).await;
    assert!(location(&response).starts_with("/admin/login?error="));
}

#[tokio::test]
async fn an_mcp_bearer_token_or_a_made_up_cookie_opens_nothing() {
    let f = fixture(AccessConfig::default());
    let bearer = send(
        &f,
        request("GET", "/admin/", LOCAL)
            .header("authorization", "Bearer the-mcp-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(bearer.status(), StatusCode::SEE_OTHER);
    assert!(location(&bearer).starts_with("/admin/login"));
    let forged = get(&f, "/admin/", Some("omnimem_admin=guess")).await;
    assert_eq!(forged.status(), StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn plain_http_from_another_machine_is_refused_unless_allowed() {
    let f = fixture(with_password());
    let remote = send(
        &f,
        request("GET", "/admin/login", REMOTE)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(remote.status(), StatusCode::FORBIDDEN);
    assert!(text(remote).await.contains("ADMIN_ALLOW_HTTP"));

    // A proxy on this machine forwarding plain HTTP is still plain HTTP.
    let proxied = send(
        &f,
        request("GET", "/admin/login", LOCAL)
            .header("x-forwarded-for", "203.0.113.9")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(proxied.status(), StatusCode::FORBIDDEN);

    let https = send(
        &f,
        request("GET", "/admin/login", REMOTE)
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(https.status(), StatusCode::OK);

    let allowed = fixture(AccessConfig {
        allow_http: true,
        ..with_password()
    });
    let remote = send(
        &allowed,
        request("GET", "/admin/login", REMOTE)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(remote.status(), StatusCode::OK);
}

fn login(peer: &str, user: &str, password: &str) -> Request<Body> {
    request("POST", "/admin/login", peer)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("sec-fetch-site", "same-origin")
        .header("x-forwarded-proto", "https")
        .body(Body::from(format!(
            "username={user}&password={password}&next=%2Fmemories"
        )))
        .unwrap()
}

#[tokio::test]
async fn the_admin_password_signs_in_and_too_many_failures_lock_the_address_out() {
    let f = fixture(with_password());
    let wrong = send(&f, login(REMOTE, "admin", "nope")).await;
    assert_eq!(wrong.status(), StatusCode::SEE_OTHER);
    assert!(
        location(&wrong).contains("error=That%20username"),
        "{}",
        location(&wrong)
    );
    assert!(wrong.headers().get("set-cookie").is_none());

    let right = send(&f, login(REMOTE, "admin", "hunter2")).await;
    assert_eq!(location(&right), "/admin/memories");
    let set = right.headers()["set-cookie"].to_str().unwrap().to_owned();
    assert!(
        set.contains("; Secure"),
        "behind HTTPS the cookie is Secure: {set}"
    );

    for _ in 0..3 {
        send(&f, login(REMOTE, "admin", "nope")).await;
    }
    let blocked = send(&f, login(REMOTE, "admin", "hunter2")).await;
    assert!(
        location(&blocked).contains("Too%20many"),
        "{}",
        location(&blocked)
    );
    let elsewhere = send(&f, login("198.51.100.4:1", "admin", "hunter2")).await;
    assert_eq!(
        location(&elsewhere),
        "/admin/memories",
        "the limit is per address"
    );
}

#[tokio::test]
async fn without_a_password_the_login_form_signs_nobody_in() {
    let f = fixture(AccessConfig::default());
    let response = send(&f, login(LOCAL, "", "")).await;
    assert!(
        location(&response).contains("isn%27t%20set%20up"),
        "{}",
        location(&response)
    );
    assert!(response.headers().get("set-cookie").is_none());
}

#[tokio::test]
async fn a_change_must_come_from_the_panels_own_pages() {
    let f = fixture(AccessConfig::default());
    let cookie = signed_in(&f).await;
    let post = |site: Option<&'static str>, origin: Option<&'static str>| {
        let mut builder = request("POST", "/admin/suppressions/add", LOCAL)
            .header("cookie", cookie.clone())
            .header("content-type", "application/x-www-form-urlencoded");
        if let Some(site) = site {
            builder = builder.header("sec-fetch-site", site);
        }
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        builder.body(Body::from("topic=onnxruntime")).unwrap()
    };
    assert_eq!(
        send(&f, post(Some("cross-site"), None)).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&f, post(None, Some("https://evil.example")))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&f, post(None, None)).await.status(),
        StatusCode::FORBIDDEN
    );
    let done = send(&f, post(Some("same-origin"), None)).await;
    assert_eq!(done.status(), StatusCode::OK, "the panel answered it");
    let list = text(get(&f, "/admin/suppressions", Some(&cookie)).await).await;
    assert!(list.contains("onnxruntime"), "{list}");
    let older_browser = send(&f, post(None, Some("http://127.0.0.1:8765"))).await;
    assert_eq!(older_browser.status(), StatusCode::OK);
}

#[tokio::test]
async fn htmx_gets_a_redirect_header_instead_of_a_login_page_in_a_fragment() {
    let f = fixture(AccessConfig::default());
    let response = send(
        &f,
        request("GET", "/admin/nav-counts", LOCAL)
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()["hx-redirect"],
        "/admin/login?next=/nav-counts"
    );
}

#[tokio::test]
async fn signing_out_ends_the_session() {
    let f = fixture(AccessConfig::default());
    let cookie = signed_in(&f).await;
    let out = send(
        &f,
        request("POST", "/admin/logout", LOCAL)
            .header("cookie", cookie.clone())
            .header("sec-fetch-site", "same-origin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(location(&out), "/admin/login");
    assert!(
        out.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(
        get(&f, "/admin/", Some(&cookie)).await.status(),
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn the_stylesheet_and_logo_load_before_sign_in() {
    let f = fixture(AccessConfig::default());
    for path in [
        "/admin/static/style.css",
        "/admin/static/images/omnimem-logo-horizontal-dark.png",
        "/admin/static/fonts/onest-variable-latin.woff2",
    ] {
        let response = get(&f, path, None).await;
        assert!(response.headers().get("content-security-policy").is_some());
        let status = response.status();
        assert_eq!(status, StatusCode::OK, "{path}: {}", text(response).await);
    }
}
