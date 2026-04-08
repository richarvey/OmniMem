"""OmniMem OAuth 2.1 provider — single admin user, pluggable storage backend."""

import logging
import os
import secrets
import time

from fastmcp.server.auth import AccessToken, OAuthProvider
from fastmcp.server.auth.auth import ClientRegistrationOptions, RevocationOptions
from mcp.server.auth.provider import AuthorizationCode, AuthorizationParams
from mcp.shared.auth import OAuthClientInformationFull, OAuthToken

from oauth.storage import InMemoryOAuthStorage

logger = logging.getLogger("omnimem.oauth")

# Token lifetimes (seconds)
ACCESS_TOKEN_EXPIRY = 3600  # 1 hour
AUTH_CODE_EXPIRY = 300  # 5 minutes

# Refresh token absolute lifetime — configurable, hard-capped at 90 days.
# Refresh tokens are rotated on each use; the new token inherits the original
# chain's absolute_expires_at, so an active session never re-prompts for login
# until the absolute cap is reached.
_REFRESH_MAX_DAYS_DEFAULT = 30
_REFRESH_MAX_DAYS_HARD_CAP = 90


def _refresh_max_seconds() -> int:
    raw = os.getenv("OAUTH_REFRESH_MAX_DAYS", str(_REFRESH_MAX_DAYS_DEFAULT))
    try:
        days = int(raw)
    except ValueError:
        logger.warning(
            "Invalid OAUTH_REFRESH_MAX_DAYS=%r, falling back to %d",
            raw, _REFRESH_MAX_DAYS_DEFAULT,
        )
        days = _REFRESH_MAX_DAYS_DEFAULT
    if days < 1:
        days = 1
    if days > _REFRESH_MAX_DAYS_HARD_CAP:
        logger.warning(
            "OAUTH_REFRESH_MAX_DAYS=%d exceeds hard cap, clamping to %d",
            days, _REFRESH_MAX_DAYS_HARD_CAP,
        )
        days = _REFRESH_MAX_DAYS_HARD_CAP
    return days * 24 * 3600


# Evaluated at import time for the module-level constant tests reference,
# but the provider re-reads on each issue so env changes during a process
# are picked up.
REFRESH_TOKEN_EXPIRY = _refresh_max_seconds()


class _StoredToken:
    """Access or refresh token record. Stored via the storage backend.

    ``absolute_expires_at`` is set on refresh tokens at initial issue and is
    carried forward unchanged across rotations, enforcing an absolute cap on
    the refresh chain regardless of how often it is renewed.
    """

    __slots__ = (
        "token", "client_id", "scopes", "created_at", "expires_in",
        "absolute_expires_at",
    )

    def __init__(
        self,
        token: str,
        client_id: str,
        scopes: list[str],
        expires_in: int,
        absolute_expires_at: float | None = None,
    ) -> None:
        self.token = token
        self.client_id = client_id
        self.scopes = scopes
        self.created_at = time.time()
        self.expires_in = expires_in
        self.absolute_expires_at = absolute_expires_at

    @property
    def expired(self) -> bool:
        return time.time() - self.created_at > self.expires_in


class _PendingAuth:
    """Tracks an in-flight /authorize request while the user logs in.

    Always kept in-memory — short-lived browser flow, sticky sessions are
    required for multi-instance deployments anyway.
    """

    __slots__ = ("client_id", "params", "created_at")

    def __init__(self, client_id: str, params: AuthorizationParams) -> None:
        self.client_id = client_id
        self.params = params
        self.created_at = time.time()

    @property
    def expired(self) -> bool:
        return time.time() - self.created_at > AUTH_CODE_EXPIRY


class OmniMemOAuthProvider(OAuthProvider):
    """OAuth 2.1 authorisation server for OmniMem.

    * Single admin user (credentials from env).
    * Pluggable storage backend (in-memory by default; Valkey for production).
    * PKCE (S256) verification handled by the MCP SDK token handler.
    """

    def __init__(
        self,
        base_url: str,
        admin_user: str,
        admin_password: str,
        storage=None,
    ) -> None:
        super().__init__(
            base_url=base_url,
            client_registration_options=ClientRegistrationOptions(
                enabled=True,
                valid_scopes=["omnimem"],
                default_scopes=["omnimem"],
            ),
            revocation_options=RevocationOptions(enabled=True),
        )
        self._admin_user = admin_user
        self._admin_password = admin_password

        self._storage = storage if storage is not None else InMemoryOAuthStorage()

        # Pending /authorize sessions are always in-memory
        self._pending: dict[str, _PendingAuth] = {}

        # Test compatibility: when using the in-memory backend, expose the
        # underlying dicts as attributes so existing tests can poke them.
        if isinstance(self._storage, InMemoryOAuthStorage):
            self._clients = self._storage.clients
            self._codes = self._storage.codes
            self._access_tokens = self._storage.access_tokens
            self._refresh_tokens = self._storage.refresh_tokens

    # ------------------------------------------------------------------
    # Client registration (RFC 7591)
    # ------------------------------------------------------------------

    async def get_client(self, client_id: str) -> OAuthClientInformationFull | None:
        return self._storage.get_client(client_id)

    async def register_client(
        self, client_info: OAuthClientInformationFull
    ) -> None:
        if not client_info.client_id:
            client_info.client_id = f"omnimem-{secrets.token_urlsafe(16)}"
        client_info.client_secret = secrets.token_urlsafe(32)
        client_info.client_id_issued_at = int(time.time())
        self._storage.save_client(client_info)
        logger.info(
            "Registered OAuth client: %s (%s)",
            client_info.client_id,
            client_info.client_name or "unnamed",
        )

    # ------------------------------------------------------------------
    # Authorisation flow
    # ------------------------------------------------------------------

    async def authorize(
        self,
        client: OAuthClientInformationFull,
        params: AuthorizationParams,
    ) -> str:
        session_id = secrets.token_urlsafe(32)
        self._pending[session_id] = _PendingAuth(
            client_id=client.client_id, params=params
        )
        # Redirect browser to the login form
        base = str(self.base_url).rstrip("/")
        return f"{base}/oauth/login?session={session_id}"

    def get_pending(self, session_id: str) -> _PendingAuth | None:
        pending = self._pending.get(session_id)
        if pending is None:
            return None
        if pending.expired:
            del self._pending[session_id]
            return None
        return pending

    def verify_credentials(self, username: str, password: str) -> bool:
        return (
            secrets.compare_digest(username, self._admin_user)
            and secrets.compare_digest(password, self._admin_password)
        )

    def complete_authorize(self, session_id: str) -> tuple[str, str, str | None]:
        """Called after successful login. Returns (code, redirect_uri, state)."""
        pending = self._pending.pop(session_id)
        code = secrets.token_urlsafe(32)  # >= 256 bits of entropy
        ac = AuthorizationCode(
            code=code,
            client_id=pending.client_id,
            redirect_uri=pending.params.redirect_uri,
            redirect_uri_provided_explicitly=pending.params.redirect_uri_provided_explicitly,
            code_challenge=pending.params.code_challenge,
            scopes=pending.params.scopes or [],
            expires_at=time.time() + AUTH_CODE_EXPIRY,
            resource=pending.params.resource,
        )
        self._storage.save_code(ac)
        return code, str(pending.params.redirect_uri), pending.params.state

    # ------------------------------------------------------------------
    # Token exchange
    # ------------------------------------------------------------------

    async def load_authorization_code(
        self,
        client: OAuthClientInformationFull,
        authorization_code: str,
    ) -> AuthorizationCode | None:
        ac = self._storage.load_code(authorization_code)
        if ac is None:
            return None
        if ac.client_id != client.client_id:
            return None
        return ac

    async def exchange_authorization_code(
        self,
        client: OAuthClientInformationFull,
        authorization_code: AuthorizationCode,
    ) -> OAuthToken:
        # Consume the code (one-time use)
        self._storage.delete_code(authorization_code.code)

        access = secrets.token_urlsafe(32)
        refresh = secrets.token_urlsafe(32)
        scopes = authorization_code.scopes or []

        max_seconds = _refresh_max_seconds()
        absolute_expires_at = time.time() + max_seconds

        self._storage.save_access(
            _StoredToken(
                token=access,
                client_id=client.client_id,
                scopes=scopes,
                expires_in=ACCESS_TOKEN_EXPIRY,
            )
        )
        self._storage.save_refresh(
            _StoredToken(
                token=refresh,
                client_id=client.client_id,
                scopes=scopes,
                expires_in=max_seconds,
                absolute_expires_at=absolute_expires_at,
            )
        )

        logger.info("Issued tokens for client %s", client.client_id)
        return OAuthToken(
            access_token=access,
            token_type="Bearer",
            expires_in=ACCESS_TOKEN_EXPIRY,
            refresh_token=refresh,
            scope=" ".join(scopes) if scopes else None,
        )

    # ------------------------------------------------------------------
    # Token verification (called by FastMCP on every MCP request)
    # ------------------------------------------------------------------

    async def load_access_token(self, token: str) -> AccessToken | None:
        stored = self._storage.load_access(token)
        if stored is None:
            return None
        return AccessToken(
            token=token, client_id=stored.client_id, scopes=stored.scopes
        )

    # ------------------------------------------------------------------
    # Refresh tokens
    # ------------------------------------------------------------------

    async def load_refresh_token(
        self,
        client: OAuthClientInformationFull,
        refresh_token: str,
    ) -> _StoredToken | None:
        stored = self._storage.load_refresh(refresh_token)
        if stored is None:
            return None
        if stored.client_id != client.client_id:
            return None
        return stored

    async def exchange_refresh_token(
        self,
        client: OAuthClientInformationFull,
        refresh_token: _StoredToken,
        scopes: list[str],
    ) -> OAuthToken:
        # Rotate: revoke old refresh token, issue new pair
        self._storage.delete_refresh(refresh_token.token)

        effective_scopes = scopes or refresh_token.scopes
        new_access = secrets.token_urlsafe(32)
        new_refresh = secrets.token_urlsafe(32)

        # Carry the original chain's absolute cap forward. Tokens issued
        # before the field existed get a fresh cap so they aren't immediately
        # invalidated on upgrade.
        absolute_expires_at = getattr(refresh_token, "absolute_expires_at", None)
        if absolute_expires_at is None:
            absolute_expires_at = time.time() + _refresh_max_seconds()
        new_expires_in = max(1, int(absolute_expires_at - time.time()))

        self._storage.save_access(
            _StoredToken(
                token=new_access,
                client_id=client.client_id,
                scopes=effective_scopes,
                expires_in=ACCESS_TOKEN_EXPIRY,
            )
        )
        self._storage.save_refresh(
            _StoredToken(
                token=new_refresh,
                client_id=client.client_id,
                scopes=effective_scopes,
                expires_in=new_expires_in,
                absolute_expires_at=absolute_expires_at,
            )
        )

        logger.info("Refreshed tokens for client %s", client.client_id)
        return OAuthToken(
            access_token=new_access,
            token_type="Bearer",
            expires_in=ACCESS_TOKEN_EXPIRY,
            refresh_token=new_refresh,
            scope=" ".join(effective_scopes) if effective_scopes else None,
        )

    # ------------------------------------------------------------------
    # Revocation (RFC 7009)
    # ------------------------------------------------------------------

    async def revoke_token(
        self, token: _StoredToken | AuthorizationCode
    ) -> None:
        tok = token.token if hasattr(token, "token") else token.code
        # Try both token stores; one will be a no-op
        self._storage.delete_access(tok)
        self._storage.delete_refresh(tok)
        logger.info("Revoked token for client %s", getattr(token, "client_id", "?"))
