"""OmniMem Web UI — Starlette app with htmx + Jinja2."""

import hmac
import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path

from dotenv import load_dotenv
from jinja2 import Environment, FileSystemLoader
from starlette.applications import Starlette
from starlette.middleware import Middleware
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request
from starlette.responses import PlainTextResponse
from starlette.routing import Mount
from starlette.staticfiles import StaticFiles

from . import deps
from memory.version import __version__
from .routes.dashboard import routes as dashboard_routes
from .routes.memories import routes as memories_routes
from .routes.search import routes as search_routes
from .routes.detail import routes as detail_routes
from .routes.lifecycle import routes as lifecycle_routes
from .routes.create import routes as create_routes
from .routes.projects import routes as project_routes
from .routes.experience import routes as experience_routes
from .routes.duplicates import routes as duplicate_routes
from .routes.contradictions import routes as contradiction_routes
from .routes.suppressions import routes as suppression_routes
from .routes.backups import routes as backup_routes
from .routes.feeds import routes as feed_routes
from .routes.telemetry import routes as telemetry_routes
from .routes.token_overhead import routes as token_overhead_routes
from .routes.metrics import routes as metrics_routes
from .routes.version_check import routes as version_check_routes

load_dotenv()

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger("omnimem.web")

BASE_DIR = Path(__file__).resolve().parent

# Paths that bypass auth (Prometheus scraping, static assets)
_AUTH_EXEMPT_PREFIXES = ("/metrics", "/static/")


class BearerAuthMiddleware(BaseHTTPMiddleware):
    """Reject requests without a valid Bearer token.

    Exempts paths in _AUTH_EXEMPT_PREFIXES so Prometheus can scrape /metrics
    and static assets load without credentials.
    """

    def __init__(self, app, token: str) -> None:
        super().__init__(app)
        self.token = token

    async def dispatch(self, request: Request, call_next):
        path = request.url.path
        if any(path.startswith(p) for p in _AUTH_EXEMPT_PREFIXES):
            return await call_next(request)

        auth_header = request.headers.get("Authorization", "")
        # Constant-time compare to avoid leaking the token via response timing.
        if hmac.compare_digest(auth_header, f"Bearer {self.token}"):
            return await call_next(request)

        return PlainTextResponse("Unauthorised", status_code=401)


@asynccontextmanager
async def lifespan(app: Starlette):
    """Initialise dependencies on startup."""
    deps.init()
    logger.info("OmniMem Web UI ready")
    yield
    logger.info("OmniMem Web UI shutting down")


# Optional bearer token auth — only enabled when WEB_UI_AUTH_TOKEN is set
_web_auth_token = os.getenv("WEB_UI_AUTH_TOKEN", "").strip()
_middleware: list[Middleware] = []
if _web_auth_token:
    _middleware.append(Middleware(BearerAuthMiddleware, token=_web_auth_token))
    logger.info("Bearer token authentication enabled for web UI")

app = Starlette(
    routes=[
        Mount("/static", StaticFiles(directory=str(BASE_DIR / "static")), name="static"),
        *dashboard_routes,
        *memories_routes,
        *search_routes,
        *detail_routes,
        *lifecycle_routes,
        *create_routes,
        *project_routes,
        *experience_routes,
        *duplicate_routes,
        *contradiction_routes,
        *suppression_routes,
        *backup_routes,
        *feed_routes,
        *telemetry_routes,
        *token_overhead_routes,
        *metrics_routes,
        *version_check_routes,
    ],
    middleware=_middleware,
    lifespan=lifespan,
)

# Jinja2 template environment
app.state.templates = Environment(
    loader=FileSystemLoader(str(BASE_DIR / "templates")),
    autoescape=True,
)
app.state.templates.globals["version"] = __version__
