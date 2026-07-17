"""Web UI login — session auth backed by the OAuth admin credentials.

The dashboard reuses OAUTH_ADMIN_USER / OAUTH_ADMIN_PASSWORD (the same pair
that guards the MCP OAuth login form) so there is one set of credentials to
manage. A successful login mints an opaque token stored in Valkey with a TTL
(`meta:webui:session:{token}`), carried by an HttpOnly cookie. Logging out
deletes the token server-side, so sessions are genuinely revocable.

Login is enabled automatically when both credentials are set; set
WEB_UI_LOGIN_ENABLED=false to opt out. The WEB_UI_AUTH_TOKEN bearer header
keeps working alongside it for scripts and monitoring.
"""

import hmac
import logging
import os
import secrets
import time
from collections import defaultdict, deque
from urllib.parse import quote

from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request
from starlette.responses import PlainTextResponse, RedirectResponse

from . import deps

logger = logging.getLogger("omnimem.web.auth")

SESSION_COOKIE = "omnimem_session"
_SESSION_PREFIX = "meta:webui:session:"

# Paths that bypass auth: Prometheus scraping and static assets (the login
# page itself needs the stylesheet and fonts).
AUTH_EXEMPT_PREFIXES = ("/metrics", "/static/")
LOGIN_PATH = "/login"


def admin_credentials() -> tuple[str, str]:
    return (
        os.getenv("OAUTH_ADMIN_USER", "").strip(),
        os.getenv("OAUTH_ADMIN_PASSWORD", "").strip(),
    )


def login_enabled() -> bool:
    """Login is on when the OAuth admin credentials exist, unless opted out."""
    if os.getenv("WEB_UI_LOGIN_ENABLED", "").strip().lower() in ("false", "0", "no"):
        return False
    user, password = admin_credentials()
    return bool(user and password)


def verify_credentials(username: str, password: str) -> bool:
    admin_user, admin_password = admin_credentials()
    if not (admin_user and admin_password):
        return False
    return secrets.compare_digest(username, admin_user) and secrets.compare_digest(
        password, admin_password
    )


def session_ttl_seconds() -> int:
    return int(os.getenv("WEB_UI_SESSION_HOURS", "168")) * 3600


def create_session(client, username: str) -> str:
    token = secrets.token_urlsafe(32)
    client.set(_SESSION_PREFIX + token, username, ex=session_ttl_seconds())
    return token


def session_user(client, token: str) -> str | None:
    """Username for a live session token, or None."""
    if not token:
        return None
    value = client.get(_SESSION_PREFIX + token)
    if isinstance(value, bytes):
        value = value.decode()
    return value or None


def destroy_session(client, token: str) -> None:
    if token:
        client.delete(_SESSION_PREFIX + token)


def safe_next(path: str, fallback: str = "/") -> str:
    """Same-site redirect target: a single-slash local path, or the fallback."""
    if path.startswith("/") and not path.startswith("//"):
        return path
    return fallback


class LoginRateLimiter:
    """In-memory sliding-window limiter for failed logins, keyed by client IP.

    Same shape (and env knobs) as the MCP OAuth login limiter: after
    ``max_attempts`` failures inside ``window`` seconds an IP is blocked until
    the window rolls off; a successful login clears that IP.
    """

    def __init__(self, max_attempts: int, window_seconds: int) -> None:
        self.max_attempts = max_attempts
        self.window = window_seconds
        self._failures: dict[str, deque[float]] = defaultdict(deque)

    def _prune(self, ip: str, now: float) -> None:
        bucket = self._failures[ip]
        cutoff = now - self.window
        while bucket and bucket[0] < cutoff:
            bucket.popleft()

    def is_blocked(self, ip: str) -> bool:
        if self.max_attempts <= 0:
            return False
        now = time.time()
        self._prune(ip, now)
        return len(self._failures[ip]) >= self.max_attempts

    def record_failure(self, ip: str) -> None:
        now = time.time()
        self._prune(ip, now)
        self._failures[ip].append(now)

    def reset(self, ip: str) -> None:
        self._failures.pop(ip, None)


login_limiter = LoginRateLimiter(
    max_attempts=int(os.getenv("OAUTH_LOGIN_MAX_ATTEMPTS", "10")),
    window_seconds=int(os.getenv("OAUTH_LOGIN_WINDOW_SECONDS", "900")),
)


class AuthMiddleware(BaseHTTPMiddleware):
    """Guard every route behind a session cookie and/or a bearer token.

    A request passes when it carries a valid ``omnimem_session`` cookie (login
    mode) or the exact WEB_UI_AUTH_TOKEN bearer header (token mode) — either
    is sufficient when both are configured. Unauthenticated browser requests
    are redirected to the login form with a same-site ``next``; htmx requests
    get 401 + HX-Redirect so a mid-page swap turns into a full-page bounce;
    everything else gets a plain 401.
    """

    def __init__(self, app, bearer_token: str = "", login: bool = False) -> None:
        super().__init__(app)
        self.bearer_token = bearer_token
        self.login = login

    async def dispatch(self, request: Request, call_next):
        path = request.url.path
        if any(path.startswith(p) for p in AUTH_EXEMPT_PREFIXES):
            return await call_next(request)
        if self.login and path == LOGIN_PATH:
            return await call_next(request)

        if self.bearer_token:
            auth_header = request.headers.get("Authorization", "")
            # Constant-time compare to avoid leaking the token via timing.
            if hmac.compare_digest(auth_header, f"Bearer {self.bearer_token}"):
                return await call_next(request)

        if self.login:
            token = request.cookies.get(SESSION_COOKIE, "")
            user = session_user(deps.store.client, token) if deps.store else None
            if user:
                request.state.session_user = user
                return await call_next(request)
            if request.headers.get("HX-Request") == "true":
                return PlainTextResponse(
                    "Session expired",
                    status_code=401,
                    headers={"HX-Redirect": LOGIN_PATH},
                )
            target = f"{path}?{request.url.query}" if request.url.query else path
            return RedirectResponse(
                f"{LOGIN_PATH}?next={quote(target)}", status_code=303
            )

        return PlainTextResponse("Unauthorised", status_code=401)
