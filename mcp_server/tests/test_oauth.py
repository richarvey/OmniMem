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

        # Rotation no longer hard-deletes the old token immediately: during the
        # grace window it replays the same successor pair rather than minting a
        # new one (see TestRefreshGraceWindow). It cannot be used to rotate to a
        # *different* pair, which is the property that matters for security.
        replay_stored = _run(
            provider.load_refresh_token(client, token.refresh_token)
        )
        assert replay_stored is not None
        replay = _run(provider.exchange_refresh_token(client, replay_stored, []))
        assert replay.refresh_token == new_token.refresh_token

    def test_wrong_client_refresh(self, provider):
        client, token = _issue_tokens(provider)
        other = _make_client(client_id="other-client")
        assert _run(
            provider.load_refresh_token(other, token.refresh_token)
        ) is None


class TestRefreshGraceWindow:
    """The old refresh token must keep working briefly after rotation so that
    concurrent/retried refreshes from claude.ai replay the same pair instead of
    triggering a re-authentication (invalid_grant)."""

    def test_replay_within_grace_returns_same_pair(self, provider):
        client, token = _issue_tokens(provider)

        stored = _run(provider.load_refresh_token(client, token.refresh_token))
        first = _run(provider.exchange_refresh_token(client, stored, []))

        # Old token is still loadable during the grace window...
        replay_stored = _run(
            provider.load_refresh_token(client, token.refresh_token)
        )
        assert replay_stored is not None

        # ...and exchanging it again returns the SAME successor pair.
        replay = _run(provider.exchange_refresh_token(client, replay_stored, []))
        assert replay.access_token == first.access_token
        assert replay.refresh_token == first.refresh_token

    def test_new_token_still_rotates_normally(self, provider):
        client, token = _issue_tokens(provider)
        stored = _run(provider.load_refresh_token(client, token.refresh_token))
        first = _run(provider.exchange_refresh_token(client, stored, []))

        # The freshly issued refresh token rotates to a genuinely new pair.
        stored_new = _run(provider.load_refresh_token(client, first.refresh_token))
        second = _run(provider.exchange_refresh_token(client, stored_new, []))
        assert second.refresh_token != first.refresh_token
        assert second.access_token != first.access_token

    def test_old_token_dies_after_grace(self, provider):
        client, token = _issue_tokens(provider)
        stored = _run(provider.load_refresh_token(client, token.refresh_token))
        _run(provider.exchange_refresh_token(client, stored, []))

        # Simulate the grace window elapsing by ageing the retired record.
        retired = provider._refresh_tokens[token.refresh_token]
        retired.created_at = time.time() - retired.expires_in - 1
        assert _run(
            provider.load_refresh_token(client, token.refresh_token)
        ) is None

    def test_grace_disabled_hard_deletes(self, provider, monkeypatch):
        monkeypatch.setenv("OAUTH_REFRESH_GRACE_SECONDS", "0")
        client, token = _issue_tokens(provider)
        stored = _run(provider.load_refresh_token(client, token.refresh_token))
        _run(provider.exchange_refresh_token(client, stored, []))

        # With the window disabled, the old token is gone immediately.
        assert token.refresh_token not in provider._refresh_tokens
        assert _run(
            provider.load_refresh_token(client, token.refresh_token)
        ) is None


# ------------------------------------------------------------------
# Revocation
# ------------------------------------------------------------------


class TestStoragePersistence:
    """Simulates an mcp_server restart by sharing a storage backend across
    two provider instances. Verifies that tokens issued by the first provider
    are still valid via the second."""

    def test_access_token_survives_provider_restart(self):
        from oauth.storage import InMemoryOAuthStorage

        storage = InMemoryOAuthStorage()
        first = OmniMemOAuthProvider(
            base_url="https://mcp.example.com",
            admin_user="admin",
            admin_password="secret123",
            storage=storage,
        )
        client, token = _issue_tokens(first)

        # "Restart" — new provider instance, same storage
        second = OmniMemOAuthProvider(
            base_url="https://mcp.example.com",
            admin_user="admin",
            admin_password="secret123",
            storage=storage,
        )
        result = _run(second.load_access_token(token.access_token))
        assert result is not None
        assert result.client_id == client.client_id

        # Refresh token also still works
        stored_refresh = _run(
            second.load_refresh_token(client, token.refresh_token)
        )
        assert stored_refresh is not None
        new_token = _run(
            second.exchange_refresh_token(client, stored_refresh, [])
        )
        assert new_token.access_token != token.access_token


class TestStoredToken:
    def test_expires_at_is_absolute(self):
        # The MCP SDK token handler (FastMCP 3.x) reads refresh_token.expires_at
        # and compares it to time.time(); a missing attribute 500s every refresh.
        stored = _StoredToken("tok123", "client-1", ["omnimem"], 3600)
        assert stored.expires_at == pytest.approx(stored.created_at + 3600)
        assert stored.expires_at > time.time()


class TestRevocation:
    def test_revoke_access_token(self, provider):
        stored = _StoredToken("tok123", "client-1", ["omnimem"], 3600)
        provider._access_tokens["tok123"] = stored
        _run(provider.revoke_token(stored))
        assert "tok123" not in provider._access_tokens


class TestLoginRateLimiter:
    def test_blocks_after_max_attempts(self):
        from oauth.routes import _LoginRateLimiter

        limiter = _LoginRateLimiter(max_attempts=3, window_seconds=900)
        ip = "203.0.113.7"
        assert not limiter.is_blocked(ip)
        for _ in range(3):
            limiter.record_failure(ip)
        assert limiter.is_blocked(ip)

    def test_success_resets(self):
        from oauth.routes import _LoginRateLimiter

        limiter = _LoginRateLimiter(max_attempts=2, window_seconds=900)
        ip = "203.0.113.8"
        limiter.record_failure(ip)
        limiter.record_failure(ip)
        assert limiter.is_blocked(ip)
        limiter.reset(ip)
        assert not limiter.is_blocked(ip)

    def test_window_rolls_off(self):
        from oauth.routes import _LoginRateLimiter

        limiter = _LoginRateLimiter(max_attempts=2, window_seconds=900)
        ip = "203.0.113.9"
        # Simulate two failures that happened well outside the window.
        old = time.time() - 1000
        limiter._failures[ip].extend([old, old])
        assert not limiter.is_blocked(ip)  # pruned as stale

    def test_disabled_when_max_zero(self):
        from oauth.routes import _LoginRateLimiter

        limiter = _LoginRateLimiter(max_attempts=0, window_seconds=900)
        ip = "203.0.113.10"
        for _ in range(50):
            limiter.record_failure(ip)
        assert not limiter.is_blocked(ip)
