"""OAuth state storage backends.

Two implementations:

* ``InMemoryOAuthStorage`` — plain dicts, used by tests and as a fallback.
  All state is lost on process restart.
* ``ValkeyOAuthStorage`` — persists clients, auth codes, access tokens, and
  refresh tokens in Valkey with TTLs matching ``expires_in``. Survives
  ``mcp_server`` restarts so claude.ai sessions stay alive.

Pending ``/authorize`` sessions are always kept in-memory: they are very
short-lived (browser flow), and persisting them would require sticky
sessions anyway.
"""

from __future__ import annotations

import json
import logging
import os
import time
from typing import Protocol

import valkey
from mcp.server.auth.provider import AuthorizationCode
from mcp.shared.auth import OAuthClientInformationFull

logger = logging.getLogger("omnimem.oauth.storage")


# Key prefixes
_CLIENT_PREFIX = "oauth:client:"
_CODE_PREFIX = "oauth:code:"
_ACCESS_PREFIX = "oauth:access:"
_REFRESH_PREFIX = "oauth:refresh:"


class _StoredTokenLike(Protocol):
    token: str
    client_id: str
    scopes: list[str]
    created_at: float
    expires_in: int


# ---------------------------------------------------------------------------
# In-memory backend (default — used by tests)
# ---------------------------------------------------------------------------


class InMemoryOAuthStorage:
    """Dict-backed storage. State is lost on restart."""

    def __init__(self) -> None:
        self.clients: dict[str, OAuthClientInformationFull] = {}
        self.codes: dict[str, AuthorizationCode] = {}
        self.access_tokens: dict[str, _StoredTokenLike] = {}
        self.refresh_tokens: dict[str, _StoredTokenLike] = {}

    # Clients
    def save_client(self, client: OAuthClientInformationFull) -> None:
        self.clients[client.client_id] = client

    def get_client(self, client_id: str) -> OAuthClientInformationFull | None:
        return self.clients.get(client_id)

    # Auth codes
    def save_code(self, code: AuthorizationCode) -> None:
        self.codes[code.code] = code

    def load_code(self, code: str) -> AuthorizationCode | None:
        ac = self.codes.get(code)
        if ac is None:
            return None
        if time.time() > ac.expires_at:
            self.codes.pop(code, None)
            return None
        return ac

    def delete_code(self, code: str) -> None:
        self.codes.pop(code, None)

    # Access tokens
    def save_access(self, token: _StoredTokenLike) -> None:
        self.access_tokens[token.token] = token

    def load_access(self, token: str) -> _StoredTokenLike | None:
        stored = self.access_tokens.get(token)
        if stored is None:
            return None
        if time.time() - stored.created_at > stored.expires_in:
            self.access_tokens.pop(token, None)
            return None
        return stored

    def delete_access(self, token: str) -> None:
        self.access_tokens.pop(token, None)

    # Refresh tokens
    def save_refresh(self, token: _StoredTokenLike) -> None:
        self.refresh_tokens[token.token] = token

    def load_refresh(self, token: str) -> _StoredTokenLike | None:
        stored = self.refresh_tokens.get(token)
        if stored is None:
            return None
        if time.time() - stored.created_at > stored.expires_in:
            self.refresh_tokens.pop(token, None)
            return None
        return stored

    def delete_refresh(self, token: str) -> None:
        self.refresh_tokens.pop(token, None)


# ---------------------------------------------------------------------------
# Valkey backend (production)
# ---------------------------------------------------------------------------


def _serialise_stored_token(token: _StoredTokenLike) -> str:
    return json.dumps(
        {
            "token": token.token,
            "client_id": token.client_id,
            "scopes": list(token.scopes),
            "created_at": token.created_at,
            "expires_in": token.expires_in,
        }
    )


def _deserialise_stored_token(raw: str, cls: type) -> _StoredTokenLike:
    data = json.loads(raw)
    obj = cls.__new__(cls)
    obj.token = data["token"]
    obj.client_id = data["client_id"]
    obj.scopes = data["scopes"]
    obj.created_at = data["created_at"]
    obj.expires_in = data["expires_in"]
    return obj


class ValkeyOAuthStorage:
    """Persistent OAuth storage backed by Valkey with native TTLs.

    The provider supplies the ``_StoredToken`` class so we can deserialise
    tokens back into instances of the same type the provider uses internally.
    """

    def __init__(self, stored_token_cls: type) -> None:
        self._stored_token_cls = stored_token_cls
        self._client = self._connect()

    @staticmethod
    def _connect(max_retries: int = 10, retry_delay: float = 2.0) -> valkey.Valkey:
        host = os.getenv("VALKEY_HOST", "valkey")
        port = int(os.getenv("VALKEY_PORT", "6379"))
        password = os.getenv("VALKEY_PASSWORD", "")
        pool = valkey.ConnectionPool(
            host=host,
            port=port,
            password=password,
            decode_responses=True,
            max_connections=int(os.getenv("OAUTH_VALKEY_MAX_CONNECTIONS", "5")),
        )
        client = valkey.Valkey(connection_pool=pool)
        for attempt in range(1, max_retries + 1):
            try:
                client.ping()
                logger.info(
                    "OAuth storage connected to Valkey at %s:%d", host, port
                )
                return client
            except (valkey.ConnectionError, valkey.TimeoutError) as exc:
                logger.warning(
                    "OAuth storage Valkey connection attempt %d/%d failed: %s",
                    attempt, max_retries, exc,
                )
                if attempt == max_retries:
                    raise
                time.sleep(retry_delay)
        raise RuntimeError("unreachable")

    # Clients — no TTL, must survive restarts indefinitely
    def save_client(self, client: OAuthClientInformationFull) -> None:
        self._client.set(_CLIENT_PREFIX + client.client_id, client.model_dump_json())

    def get_client(self, client_id: str) -> OAuthClientInformationFull | None:
        raw = self._client.get(_CLIENT_PREFIX + client_id)
        if raw is None:
            return None
        return OAuthClientInformationFull.model_validate_json(raw)

    # Auth codes — TTL = remaining time until expires_at
    def save_code(self, code: AuthorizationCode) -> None:
        ttl = max(1, int(code.expires_at - time.time()))
        self._client.set(
            _CODE_PREFIX + code.code,
            code.model_dump_json(),
            ex=ttl,
        )

    def load_code(self, code: str) -> AuthorizationCode | None:
        raw = self._client.get(_CODE_PREFIX + code)
        if raw is None:
            return None
        return AuthorizationCode.model_validate_json(raw)

    def delete_code(self, code: str) -> None:
        self._client.delete(_CODE_PREFIX + code)

    # Access tokens — TTL = expires_in
    def save_access(self, token: _StoredTokenLike) -> None:
        ttl = max(1, int(token.expires_in))
        self._client.set(
            _ACCESS_PREFIX + token.token,
            _serialise_stored_token(token),
            ex=ttl,
        )

    def load_access(self, token: str) -> _StoredTokenLike | None:
        raw = self._client.get(_ACCESS_PREFIX + token)
        if raw is None:
            return None
        return _deserialise_stored_token(raw, self._stored_token_cls)

    def delete_access(self, token: str) -> None:
        self._client.delete(_ACCESS_PREFIX + token)

    # Refresh tokens — TTL = expires_in (default 30 days)
    def save_refresh(self, token: _StoredTokenLike) -> None:
        ttl = max(1, int(token.expires_in))
        self._client.set(
            _REFRESH_PREFIX + token.token,
            _serialise_stored_token(token),
            ex=ttl,
        )

    def load_refresh(self, token: str) -> _StoredTokenLike | None:
        raw = self._client.get(_REFRESH_PREFIX + token)
        if raw is None:
            return None
        return _deserialise_stored_token(raw, self._stored_token_cls)

    def delete_refresh(self, token: str) -> None:
        self._client.delete(_REFRESH_PREFIX + token)
