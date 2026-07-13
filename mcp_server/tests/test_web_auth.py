"""Tests for the web UI session login (v6.3.1)."""

import sys
from pathlib import Path

import pytest
from jinja2 import Environment, FileSystemLoader
from starlette.applications import Starlette
from starlette.middleware import Middleware
from starlette.responses import PlainTextResponse
from starlette.routing import Route
from starlette.testclient import TestClient

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

from web_ui import auth, deps
from web_ui.auth import AuthMiddleware, LoginRateLimiter


TEMPLATE_DIR = Path(__file__).resolve().parent.parent.parent / "web_ui" / "templates"


@pytest.fixture(autouse=True)
def _clean_limiter():
    """TestClient requests all share one client IP — reset between tests."""
    auth.login_limiter._failures.clear()
    auth.login_limiter.max_attempts = 10
    yield
    auth.login_limiter._failures.clear()


@pytest.fixture
def admin_env(monkeypatch):
    monkeypatch.setenv("OAUTH_ADMIN_USER", "admin")
    monkeypatch.setenv("OAUTH_ADMIN_PASSWORD", "s3cret")
    monkeypatch.delenv("WEB_UI_LOGIN_ENABLED", raising=False)


class TestConfig:
    def test_enabled_when_credentials_set(self, admin_env):
        assert auth.login_enabled() is True

    def test_disabled_without_credentials(self, monkeypatch):
        monkeypatch.delenv("OAUTH_ADMIN_USER", raising=False)
        monkeypatch.delenv("OAUTH_ADMIN_PASSWORD", raising=False)
        assert auth.login_enabled() is False

    def test_disabled_with_only_user(self, monkeypatch):
        monkeypatch.setenv("OAUTH_ADMIN_USER", "admin")
        monkeypatch.delenv("OAUTH_ADMIN_PASSWORD", raising=False)
        assert auth.login_enabled() is False

    def test_opt_out(self, admin_env, monkeypatch):
        monkeypatch.setenv("WEB_UI_LOGIN_ENABLED", "false")
        assert auth.login_enabled() is False

    def test_verify_credentials(self, admin_env):
        assert auth.verify_credentials("admin", "s3cret") is True
        assert auth.verify_credentials("admin", "wrong") is False
        assert auth.verify_credentials("other", "s3cret") is False

    def test_verify_refuses_when_unset(self, monkeypatch):
        monkeypatch.delenv("OAUTH_ADMIN_USER", raising=False)
        monkeypatch.delenv("OAUTH_ADMIN_PASSWORD", raising=False)
        assert auth.verify_credentials("", "") is False

    def test_session_ttl_env(self, monkeypatch):
        monkeypatch.setenv("WEB_UI_SESSION_HOURS", "2")
        assert auth.session_ttl_seconds() == 7200


class TestSessions:
    def test_round_trip(self, fake_store):
        token = auth.create_session(fake_store.client, "admin")
        assert auth.session_user(fake_store.client, token) == "admin"
        auth.destroy_session(fake_store.client, token)
        assert auth.session_user(fake_store.client, token) is None

    def test_unknown_or_empty_token(self, fake_store):
        assert auth.session_user(fake_store.client, "nope") is None
        assert auth.session_user(fake_store.client, "") is None


class TestSafeNext:
    def test_local_path(self):
        assert auth.safe_next("/memories?namespace=knowledge") == "/memories?namespace=knowledge"

    def test_rejects_protocol_relative(self):
        assert auth.safe_next("//evil.example") == "/"

    def test_rejects_absolute_url(self):
        assert auth.safe_next("https://evil.example/") == "/"

    def test_empty_falls_back(self):
        assert auth.safe_next("") == "/"


class TestRateLimiter:
    def test_blocks_after_max(self):
        limiter = LoginRateLimiter(max_attempts=3, window_seconds=900)
        for _ in range(3):
            limiter.record_failure("1.2.3.4")
        assert limiter.is_blocked("1.2.3.4") is True
        assert limiter.is_blocked("5.6.7.8") is False

    def test_reset_clears(self):
        limiter = LoginRateLimiter(max_attempts=1, window_seconds=900)
        limiter.record_failure("1.2.3.4")
        assert limiter.is_blocked("1.2.3.4") is True
        limiter.reset("1.2.3.4")
        assert limiter.is_blocked("1.2.3.4") is False

    def test_zero_disables(self):
        limiter = LoginRateLimiter(max_attempts=0, window_seconds=900)
        limiter.record_failure("1.2.3.4")
        assert limiter.is_blocked("1.2.3.4") is False


async def _protected(request):
    return PlainTextResponse("ok")


def _make_app(
    bearer_token: str = "", login: bool = False, with_middleware: bool = True
) -> Starlette:
    """Minimal app: one protected route, the auth routes, and the middleware."""
    from web_ui.routes.auth import routes as auth_routes

    middleware = []
    if with_middleware:
        middleware.append(
            Middleware(AuthMiddleware, bearer_token=bearer_token, login=login)
        )
    app = Starlette(
        routes=[Route("/", _protected), Route("/metrics", _protected), *auth_routes],
        middleware=middleware,
    )
    app.state.templates = Environment(
        loader=FileSystemLoader(str(TEMPLATE_DIR)), autoescape=True
    )
    app.state.templates.globals["version"] = "test"
    return app


class TestMiddleware:
    def test_no_session_redirects_to_login(self, admin_env, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        client = TestClient(_make_app(login=True))
        resp = client.get("/", follow_redirects=False)
        assert resp.status_code == 303
        assert resp.headers["location"].startswith("/login?next=")

    def test_next_carries_path_and_query(self, admin_env, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        client = TestClient(_make_app(login=True))
        resp = client.get("/?a=1", follow_redirects=False)
        assert resp.headers["location"] == "/login?next=/%3Fa%3D1"

    def test_valid_session_passes(self, admin_env, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        token = auth.create_session(fake_store.client, "admin")
        client = TestClient(_make_app(login=True))
        resp = client.get("/", cookies={auth.SESSION_COOKIE: token})
        assert resp.status_code == 200
        assert resp.text == "ok"

    def test_htmx_gets_hx_redirect(self, admin_env, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        client = TestClient(_make_app(login=True))
        resp = client.get("/", headers={"HX-Request": "true"})
        assert resp.status_code == 401
        assert resp.headers["HX-Redirect"] == "/login"

    def test_metrics_exempt(self, admin_env, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        client = TestClient(_make_app(login=True))
        assert client.get("/metrics").status_code == 200

    def test_bearer_only_mode(self, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        client = TestClient(_make_app(bearer_token="tok123"))
        assert client.get("/").status_code == 401
        resp = client.get("/", headers={"Authorization": "Bearer tok123"})
        assert resp.status_code == 200

    def test_bearer_accepted_alongside_login(self, admin_env, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        client = TestClient(_make_app(bearer_token="tok123", login=True))
        resp = client.get("/", headers={"Authorization": "Bearer tok123"})
        assert resp.status_code == 200


class TestLoginRoutes:
    def test_login_page_renders(self, admin_env, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        client = TestClient(_make_app(login=True))
        resp = client.get("/login")
        assert resp.status_code == 200
        assert "Sign in" in resp.text
        assert 'autocomplete="current-password"' in resp.text

    def test_login_success_sets_cookie_and_redirects(
        self, admin_env, fake_store, monkeypatch
    ):
        monkeypatch.setattr(deps, "store", fake_store)
        client = TestClient(_make_app(login=True))
        resp = client.post(
            "/login",
            data={"username": "admin", "password": "s3cret", "next": "/memories"},
            follow_redirects=False,
        )
        assert resp.status_code == 303
        assert resp.headers["location"] == "/memories"
        token = resp.cookies.get(auth.SESSION_COOKIE)
        assert token
        assert auth.session_user(fake_store.client, token) == "admin"

    def test_login_failure_shows_error(self, admin_env, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        client = TestClient(_make_app(login=True))
        resp = client.post(
            "/login", data={"username": "admin", "password": "wrong"}
        )
        assert resp.status_code == 401
        assert "Invalid username or password" in resp.text

    def test_login_rejects_external_next(self, admin_env, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        client = TestClient(_make_app(login=True))
        resp = client.post(
            "/login",
            data={"username": "admin", "password": "s3cret", "next": "https://evil.example/"},
            follow_redirects=False,
        )
        assert resp.headers["location"] == "/"

    def test_login_rate_limited(self, admin_env, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        auth.login_limiter.max_attempts = 2
        client = TestClient(_make_app(login=True))
        for _ in range(2):
            client.post("/login", data={"username": "admin", "password": "wrong"})
        resp = client.post(
            "/login", data={"username": "admin", "password": "s3cret"}
        )
        assert resp.status_code == 429
        assert "Too many failed attempts" in resp.text

    def test_logout_revokes_session(self, admin_env, fake_store, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        token = auth.create_session(fake_store.client, "admin")
        client = TestClient(_make_app(login=True))
        resp = client.post(
            "/logout",
            cookies={auth.SESSION_COOKIE: token},
            follow_redirects=False,
        )
        assert resp.status_code == 303
        assert resp.headers["location"] == "/login"
        assert auth.session_user(fake_store.client, token) is None

    def test_login_page_redirects_when_disabled(self, fake_store, monkeypatch):
        monkeypatch.delenv("OAUTH_ADMIN_USER", raising=False)
        monkeypatch.delenv("OAUTH_ADMIN_PASSWORD", raising=False)
        monkeypatch.setattr(deps, "store", fake_store)
        client = TestClient(_make_app(with_middleware=False))
        resp = client.get("/login", follow_redirects=False)
        assert resp.status_code == 303
        assert resp.headers["location"] == "/"
