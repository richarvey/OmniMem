"""Coverage for OAuth storage backends and the /oauth/login routes."""

import time

import pytest
import valkey as valkey_module
from starlette.applications import Starlette
from starlette.routing import Route
from starlette.testclient import TestClient

from tests.conftest import FakeValkeyClient

from mcp.server.auth.provider import AuthorizationCode
from mcp.shared.auth import OAuthClientInformationFull

from oauth import storage as storage_module
from oauth.provider import OmniMemOAuthProvider, _StoredToken
from oauth.storage import (
    InMemoryOAuthStorage,
    ValkeyOAuthStorage,
    _deserialise_stored_token,
    _serialise_stored_token,
)


def _make_code(code="code-1", expires_in=300.0) -> AuthorizationCode:
    return AuthorizationCode(
        code=code,
        scopes=["omnimem"],
        expires_at=time.time() + expires_in,
        client_id="client-1",
        code_challenge="challenge",
        redirect_uri="https://claude.ai/cb",
        redirect_uri_provided_explicitly=True,
    )


def _make_token(token="tok-1", expires_in=3600, rotated_to=None) -> _StoredToken:
    return _StoredToken(
        token=token,
        client_id="client-1",
        scopes=["omnimem"],
        expires_in=expires_in,
        rotated_to=rotated_to,
    )


class TestInMemoryStorage:
    def test_expired_entries_evicted_on_load(self):
        store = InMemoryOAuthStorage()

        store.save_code(_make_code(expires_in=-10))
        assert store.load_code("code-1") is None

        expired = _make_token(expires_in=1)
        expired.created_at = time.time() - 100
        store.save_access(expired)
        assert store.load_access("tok-1") is None
        store.save_refresh(expired)
        assert store.load_refresh("tok-1") is None

    def test_serialise_round_trip_keeps_rotation(self):
        token = _make_token(rotated_to={"access_token": "next"})
        raw = _serialise_stored_token(token)
        back = _deserialise_stored_token(raw, _StoredToken)
        assert back.token == token.token
        assert back.rotated_to == {"access_token": "next"}
        assert back.expires_at == pytest.approx(token.expires_at)


class ConnectableFakeClient(FakeValkeyClient):
    def __init__(self, fail_pings=0):
        super().__init__()
        self._fail_pings = fail_pings

    def ping(self):
        if self._fail_pings > 0:
            self._fail_pings -= 1
            raise valkey_module.ConnectionError("refused")
        return True


@pytest.fixture
def valkey_storage(monkeypatch):
    client = ConnectableFakeClient()
    monkeypatch.setattr(
        storage_module.valkey, "ConnectionPool", lambda **kw: object(),
    )
    monkeypatch.setattr(
        storage_module.valkey, "Valkey", lambda connection_pool: client,
    )
    return ValkeyOAuthStorage(_StoredToken)


class TestValkeyStorage:
    def test_client_round_trip(self, valkey_storage):
        client = OAuthClientInformationFull(
            client_id="client-1", client_name="Test",
            redirect_uris=["https://claude.ai/cb"],
        )
        valkey_storage.save_client(client)
        loaded = valkey_storage.get_client("client-1")
        assert loaded.client_id == "client-1"
        assert valkey_storage.get_client("nope") is None

    def test_code_round_trip_and_delete(self, valkey_storage):
        valkey_storage.save_code(_make_code())
        assert valkey_storage.load_code("code-1").code == "code-1"
        valkey_storage.delete_code("code-1")
        assert valkey_storage.load_code("code-1") is None

    def test_access_and_refresh_round_trip(self, valkey_storage):
        valkey_storage.save_access(_make_token("acc-1"))
        assert valkey_storage.load_access("acc-1").token == "acc-1"
        valkey_storage.delete_access("acc-1")
        assert valkey_storage.load_access("acc-1") is None

        valkey_storage.save_refresh(_make_token("ref-1", rotated_to={"a": "b"}))
        loaded = valkey_storage.load_refresh("ref-1")
        assert loaded.rotated_to == {"a": "b"}
        valkey_storage.delete_refresh("ref-1")
        assert valkey_storage.load_refresh("ref-1") is None

    def test_connect_retries_then_succeeds(self, monkeypatch):
        client = ConnectableFakeClient(fail_pings=1)
        monkeypatch.setattr(
            storage_module.valkey, "ConnectionPool", lambda **kw: object(),
        )
        monkeypatch.setattr(
            storage_module.valkey, "Valkey", lambda connection_pool: client,
        )
        assert ValkeyOAuthStorage._connect(max_retries=3, retry_delay=0) is client

    def test_connect_exhausts_retries(self, monkeypatch):
        client = ConnectableFakeClient(fail_pings=99)
        monkeypatch.setattr(
            storage_module.valkey, "ConnectionPool", lambda **kw: object(),
        )
        monkeypatch.setattr(
            storage_module.valkey, "Valkey", lambda connection_pool: client,
        )
        with pytest.raises(valkey_module.ConnectionError):
            ValkeyOAuthStorage._connect(max_retries=2, retry_delay=0)


class TestProviderEnvKnobs:
    def test_refresh_max_days_parsing(self, monkeypatch):
        from oauth.provider import _refresh_max_seconds

        monkeypatch.setenv("OAUTH_REFRESH_MAX_DAYS", "not a number")
        assert _refresh_max_seconds() == 30 * 24 * 3600

        monkeypatch.setenv("OAUTH_REFRESH_MAX_DAYS", "0")
        assert _refresh_max_seconds() == 24 * 3600

        monkeypatch.setenv("OAUTH_REFRESH_MAX_DAYS", "100000")
        assert _refresh_max_seconds() == 90 * 24 * 3600

    def test_refresh_grace_parsing(self, monkeypatch):
        from oauth.provider import _refresh_grace_seconds

        monkeypatch.setenv("OAUTH_REFRESH_GRACE_SECONDS", "junk")
        assert _refresh_grace_seconds() == 120

        monkeypatch.setenv("OAUTH_REFRESH_GRACE_SECONDS", "-5")
        assert _refresh_grace_seconds() == 0

        monkeypatch.setenv("OAUTH_REFRESH_GRACE_SECONDS", "99999")
        assert _refresh_grace_seconds() == 3600

    def test_stored_token_expires_at(self):
        token = _make_token(expires_in=100)
        assert token.expires_at == pytest.approx(token.created_at + 100)
        assert token.expired is False


class _FakeMCP:
    """Duck-typed FastMCP: collects custom routes into a Starlette app."""

    def __init__(self):
        self.routes = []

    def custom_route(self, path, methods):
        def decorate(fn):
            self.routes.append(Route(path, fn, methods=methods))
            return fn
        return decorate


@pytest.fixture
def login_client(monkeypatch):
    import asyncio

    from oauth.routes import register_oauth_routes
    from oauth import routes as routes_module

    # Fresh rate limiter so earlier tests can't bleed into this one.
    monkeypatch.setattr(
        routes_module, "_login_limiter",
        type(routes_module._login_limiter)(max_attempts=5, window_seconds=60),
    )

    provider = OmniMemOAuthProvider(
        base_url="https://mcp.example.com",
        admin_user="admin",
        admin_password="secret123",
    )
    client_info = OAuthClientInformationFull(
        client_id="client-1", client_name="Test",
        redirect_uris=["https://claude.ai/api/mcp/auth_callback"],
    )
    asyncio.run(provider.register_client(client_info))

    from mcp.server.auth.provider import AuthorizationParams

    params = AuthorizationParams(
        state="state-1", scopes=["omnimem"], code_challenge="challenge",
        redirect_uri="https://claude.ai/api/mcp/auth_callback",
        redirect_uri_provided_explicitly=True,
    )
    url = asyncio.run(provider.authorize(client_info, params))
    session = url.split("session=")[1]

    mcp = _FakeMCP()
    register_oauth_routes(mcp, provider)
    return TestClient(Starlette(routes=mcp.routes)), session


class TestLoginRoutes:
    def test_get_login_page_and_bad_session(self, login_client):
        client, session = login_client
        assert client.get(f"/oauth/login?session={session}").status_code == 200
        assert client.get("/oauth/login?session=bogus").status_code == 400

    def test_post_bad_session_and_bad_credentials(self, login_client):
        client, session = login_client
        assert client.post("/oauth/login", data={
            "session": "bogus", "username": "admin", "password": "secret123",
        }).status_code == 400
        assert client.post("/oauth/login", data={
            "session": session, "username": "admin", "password": "wrong",
        }).status_code == 401

    def test_successful_login_redirects_with_code(self, login_client):
        client, session = login_client
        response = client.post("/oauth/login", data={
            "session": session, "username": "admin", "password": "secret123",
        }, follow_redirects=False)
        assert response.status_code == 302
        location = response.headers["location"]
        assert location.startswith("https://claude.ai/api/mcp/auth_callback?")
        assert "code=" in location and "state=state-1" in location

    def test_rate_limit_kicks_in(self, login_client):
        client, session = login_client
        for _ in range(25):
            response = client.post("/oauth/login", data={
                "session": session, "username": "admin", "password": "wrong",
            })
            if response.status_code == 429:
                break
        assert response.status_code == 429

    def test_icon_routes(self, login_client):
        client, _ = login_client
        for path in ("/icon.svg", "/favicon.svg", "/favicon.ico",
                     "/oauth/icon.svg"):
            response = client.get(path)
            assert response.status_code == 200
            assert response.headers["content-type"].startswith("image/svg")
