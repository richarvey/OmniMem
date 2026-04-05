"""Tests for the OmniMem OAuth 2.1 provider."""

import asyncio
import time

import pytest

from oauth.provider import (
    ACCESS_TOKEN_EXPIRY,
    AUTH_CODE_EXPIRY,
    OmniMemOAuthProvider,
    _StoredToken,
)
from mcp.server.auth.provider import AuthorizationParams
from mcp.shared.auth import OAuthClientInformationFull


def _run(coro):
    """Run an async coroutine synchronously."""
    return asyncio.run(coro)


@pytest.fixture
def provider():
    return OmniMemOAuthProvider(
        base_url="https://mcp.example.com",
        admin_user="admin",
        admin_password="secret123",
    )


def _make_client(**overrides) -> OAuthClientInformationFull:
    defaults = {
        "client_id": "test-client-1",
        "client_name": "Test Client",
        "redirect_uris": ["https://claude.ai/api/mcp/auth_callback"],
    }
    defaults.update(overrides)
    return OAuthClientInformationFull(**defaults)


def _make_auth_params(**overrides) -> AuthorizationParams:
    defaults = {
        "state": "random-state",
        "scopes": ["omnimem"],
        "code_challenge": "abc123challenge",
        "redirect_uri": "https://claude.ai/api/mcp/auth_callback",
        "redirect_uri_provided_explicitly": True,
    }
    defaults.update(overrides)
    return AuthorizationParams(**defaults)


def _issue_tokens(provider):
    """Full flow: register, authorize, exchange — returns (client, OAuthToken)."""
    client = _make_client()
    _run(provider.register_client(client))
    params = _make_auth_params()
    url = _run(provider.authorize(client, params))
    session_id = url.split("session=")[1]
    code, _, _ = provider.complete_authorize(session_id)
    auth_code = _run(provider.load_authorization_code(client, code))
    token = _run(provider.exchange_authorization_code(client, auth_code))
    return client, token


# ------------------------------------------------------------------
# Client registration
# ------------------------------------------------------------------


class TestClientRegistration:
    def test_register_and_get(self, provider):
        client = _make_client(client_id=None)
        _run(provider.register_client(client))
        assert client.client_id is not None
        assert client.client_id.startswith("omnimem-")
        assert client.client_secret is not None

        retrieved = _run(provider.get_client(client.client_id))
        assert retrieved is not None
        assert retrieved.client_name == "Test Client"

    def test_get_unknown_client(self, provider):
        assert _run(provider.get_client("nonexistent")) is None


# ------------------------------------------------------------------
# Credential verification
# ------------------------------------------------------------------


class TestCredentials:
    def test_valid_credentials(self, provider):
        assert provider.verify_credentials("admin", "secret123") is True

    def test_wrong_password(self, provider):
        assert provider.verify_credentials("admin", "wrong") is False

    def test_wrong_username(self, provider):
        assert provider.verify_credentials("notadmin", "secret123") is False


# ------------------------------------------------------------------
# Authorise flow
# ------------------------------------------------------------------


class TestAuthorizeFlow:
    def test_authorize_returns_login_url(self, provider):
        client = _make_client()
        params = _make_auth_params()
        url = _run(provider.authorize(client, params))
        assert url.startswith("https://mcp.example.com/oauth/login?session=")

    def test_pending_session_retrievable(self, provider):
        client = _make_client()
        params = _make_auth_params()
        url = _run(provider.authorize(client, params))
        session_id = url.split("session=")[1]
        pending = provider.get_pending(session_id)
        assert pending is not None
        assert pending.client_id == "test-client-1"

    def test_expired_session_returns_none(self, provider):
        client = _make_client()
        params = _make_auth_params()
        url = _run(provider.authorize(client, params))
        session_id = url.split("session=")[1]
        provider._pending[session_id].created_at = time.time() - AUTH_CODE_EXPIRY - 1
        assert provider.get_pending(session_id) is None

    def test_complete_authorize_returns_code(self, provider):
        client = _make_client()
        params = _make_auth_params(state="mystate")
        url = _run(provider.authorize(client, params))
        session_id = url.split("session=")[1]
        code, redirect_uri, state = provider.complete_authorize(session_id)
        assert code is not None
        assert len(code) > 20
        assert "claude.ai" in redirect_uri
        assert state == "mystate"


# ------------------------------------------------------------------
# Token exchange
# ------------------------------------------------------------------


class TestTokenExchange:
    def _setup_code(self, provider):
        client = _make_client()
        _run(provider.register_client(client))
        params = _make_auth_params()
        url = _run(provider.authorize(client, params))
        session_id = url.split("session=")[1]
        code, _, _ = provider.complete_authorize(session_id)
        return client, code

    def test_load_and_exchange_code(self, provider):
        client, code = self._setup_code(provider)
        auth_code = _run(provider.load_authorization_code(client, code))
        assert auth_code is not None
        assert auth_code.code == code
        assert auth_code.client_id == client.client_id

        token = _run(provider.exchange_authorization_code(client, auth_code))
        assert token.access_token is not None
        assert token.refresh_token is not None
        assert token.token_type == "Bearer"
        assert token.expires_in == ACCESS_TOKEN_EXPIRY

    def test_code_single_use(self, provider):
        client, code = self._setup_code(provider)
        auth_code = _run(provider.load_authorization_code(client, code))
        _run(provider.exchange_authorization_code(client, auth_code))
        assert _run(provider.load_authorization_code(client, code)) is None

    def test_wrong_client_cannot_load_code(self, provider):
        client, code = self._setup_code(provider)
        other = _make_client(client_id="other-client")
        assert _run(provider.load_authorization_code(other, code)) is None


# ------------------------------------------------------------------
# Access token verification
# ------------------------------------------------------------------


class TestAccessToken:
    def test_valid_access_token(self, provider):
        _, token = _issue_tokens(provider)
        result = _run(provider.load_access_token(token.access_token))
        assert result is not None
        assert result.client_id == "test-client-1"

    def test_expired_access_token(self, provider):
        _, token = _issue_tokens(provider)
        provider._access_tokens[token.access_token].created_at = (
            time.time() - ACCESS_TOKEN_EXPIRY - 1
        )
        assert _run(provider.load_access_token(token.access_token)) is None

    def test_unknown_token(self, provider):
        assert _run(provider.load_access_token("bogus-token")) is None


# ------------------------------------------------------------------
# Refresh token
# ------------------------------------------------------------------


class TestRefreshToken:
    def test_refresh_issues_new_tokens(self, provider):
        client, token = _issue_tokens(provider)
        stored_refresh = _run(
            provider.load_refresh_token(client, token.refresh_token)
        )
        assert stored_refresh is not None

        new_token = _run(
            provider.exchange_refresh_token(client, stored_refresh, [])
        )
        assert new_token.access_token != token.access_token
        assert new_token.refresh_token != token.refresh_token

        # Old refresh token consumed (rotation)
        assert _run(
            provider.load_refresh_token(client, token.refresh_token)
        ) is None

    def test_wrong_client_refresh(self, provider):
        client, token = _issue_tokens(provider)
        other = _make_client(client_id="other-client")
        assert _run(
            provider.load_refresh_token(other, token.refresh_token)
        ) is None


# ------------------------------------------------------------------
# Revocation
# ------------------------------------------------------------------


class TestRevocation:
    def test_revoke_access_token(self, provider):
        stored = _StoredToken("tok123", "client-1", ["omnimem"], 3600)
        provider._access_tokens["tok123"] = stored
        _run(provider.revoke_token(stored))
        assert "tok123" not in provider._access_tokens
