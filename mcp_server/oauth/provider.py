"""OmniMem OAuth 2.1 provider — single admin user, in-memory state."""

import logging
import secrets
import time

from fastmcp.server.auth import AccessToken, OAuthProvider
from fastmcp.server.auth.auth import ClientRegistrationOptions, RevocationOptions
from mcp.server.auth.provider import AuthorizationCode, AuthorizationParams
from mcp.shared.auth import OAuthClientInformationFull, OAuthToken

logger = logging.getLogger("omnimem.oauth")

# Token lifetimes (seconds)
ACCESS_TOKEN_EXPIRY = 3600  # 1 hour
REFRESH_TOKEN_EXPIRY = 30 * 24 * 3600  # 30 days
AUTH_CODE_EXPIRY = 300  # 5 minutes


class _StoredToken:
    """In-memory access or refresh token."""

    __slots__ = ("token", "client_id", "scopes", "created_at", "expires_in")

    def __init__(
        self, token: str, client_id: str, scopes: list[str], expires_in: int
    ) -> None:
        self.token = token
        self.client_id = client_id
        self.scopes = scopes
        self.created_at = time.time()
        self.expires_in = expires_in

    @property
    def expired(self) -> bool:
        return time.time() - self.created_at > self.expires_in


class _PendingAuth:
    """Tracks an in-flight /authorize request while the user logs in."""

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
    * In-memory stores for clients, codes, and tokens.
    * PKCE (S256) verification handled by the MCP SDK token handler.
    """

    def __init__(
        self,
        base_url: str,
        admin_user: str,
        admin_password: str,
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

        # In-memory stores — cleared on restart (clients must re-register)
        self._clients: dict[str, OAuthClientInformationFull] = {}
        self._pending: dict[str, _PendingAuth] = {}
        self._codes: dict[str, AuthorizationCode] = {}
        self._access_tokens: dict[str, _StoredToken] = {}
        self._refresh_tokens: dict[str, _StoredToken] = {}

    # ------------------------------------------------------------------
    # Client registration (RFC 7591)
    # ------------------------------------------------------------------

    async def get_client(self, client_id: str) -> OAuthClientInformationFull | None:
        return self._clients.get(client_id)

    async def register_client(
        self, client_info: OAuthClientInformationFull
    ) -> None:
        if not client_info.client_id:
            client_info.client_id = f"omnimem-{secrets.token_urlsafe(16)}"
        client_info.client_secret = secrets.token_urlsafe(32)
        client_info.client_id_issued_at = int(time.time())
        self._clients[client_info.client_id] = client_info
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
        self._codes[code] = AuthorizationCode(
            code=code,
            client_id=pending.client_id,
            redirect_uri=pending.params.redirect_uri,
            redirect_uri_provided_explicitly=pending.params.redirect_uri_provided_explicitly,
            code_challenge=pending.params.code_challenge,
            scopes=pending.params.scopes or [],
            expires_at=time.time() + AUTH_CODE_EXPIRY,
            resource=pending.params.resource,
        )
        return code, str(pending.params.redirect_uri), pending.params.state

    # ------------------------------------------------------------------
    # Token exchange
    # ------------------------------------------------------------------

    async def load_authorization_code(
        self,
        client: OAuthClientInformationFull,
        authorization_code: str,
    ) -> AuthorizationCode | None:
        ac = self._codes.get(authorization_code)
        if ac is None:
            return None
        if ac.client_id != client.client_id:
            return None
        if time.time() > ac.expires_at:
            del self._codes[authorization_code]
            return None
        return ac

    async def exchange_authorization_code(
        self,
        client: OAuthClientInformationFull,
        authorization_code: AuthorizationCode,
    ) -> OAuthToken:
        # Consume the code (one-time use)
        self._codes.pop(authorization_code.code, None)

        access = secrets.token_urlsafe(32)
        refresh = secrets.token_urlsafe(32)
        scopes = authorization_code.scopes or []

        self._access_tokens[access] = _StoredToken(
            token=access,
            client_id=client.client_id,
            scopes=scopes,
            expires_in=ACCESS_TOKEN_EXPIRY,
        )
        self._refresh_tokens[refresh] = _StoredToken(
            token=refresh,
            client_id=client.client_id,
            scopes=scopes,
            expires_in=REFRESH_TOKEN_EXPIRY,
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
        stored = self._access_tokens.get(token)
        if stored is None:
            return None
        if stored.expired:
            del self._access_tokens[token]
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
        stored = self._refresh_tokens.get(refresh_token)
        if stored is None:
            return None
        if stored.client_id != client.client_id:
            return None
        if stored.expired:
            del self._refresh_tokens[refresh_token]
            return None
        return stored

    async def exchange_refresh_token(
        self,
        client: OAuthClientInformationFull,
        refresh_token: _StoredToken,
        scopes: list[str],
    ) -> OAuthToken:
        # Rotate: revoke old refresh token, issue new pair
        self._refresh_tokens.pop(refresh_token.token, None)

        effective_scopes = scopes or refresh_token.scopes
        new_access = secrets.token_urlsafe(32)
        new_refresh = secrets.token_urlsafe(32)

        self._access_tokens[new_access] = _StoredToken(
            token=new_access,
            client_id=client.client_id,
            scopes=effective_scopes,
            expires_in=ACCESS_TOKEN_EXPIRY,
        )
        self._refresh_tokens[new_refresh] = _StoredToken(
            token=new_refresh,
            client_id=client.client_id,
            scopes=effective_scopes,
            expires_in=REFRESH_TOKEN_EXPIRY,
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
        removed = self._access_tokens.pop(tok, None) or self._refresh_tokens.pop(
            tok, None
        )
        if removed:
            logger.info("Revoked token for client %s", getattr(token, "client_id", "?"))
