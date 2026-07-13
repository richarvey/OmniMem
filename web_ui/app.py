"""OmniMem Web UI — Starlette app with htmx + Jinja2."""

import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path

from dotenv import load_dotenv
from jinja2 import Environment, FileSystemLoader
from starlette.applications import Starlette
from starlette.middleware import Middleware
from starlette.middleware.gzip import GZipMiddleware
from starlette.routing import Mount
from starlette.staticfiles import StaticFiles

from . import deps
from .auth import AuthMiddleware, login_enabled
from memory.version import __version__
from .routes.auth import routes as auth_routes
from .routes.dashboard import routes as dashboard_routes
from .routes.memories import routes as memories_routes
from .routes.search import routes as search_routes
from .routes.detail import routes as detail_routes
from .routes.lifecycle import routes as lifecycle_routes
from .routes.create import routes as create_routes
from .routes.projects import routes as project_routes
from .routes.skills import routes as skill_routes
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


@asynccontextmanager
async def lifespan(app: Starlette):
    """Initialise dependencies on startup."""
    deps.init()
    logger.info("OmniMem Web UI ready")
    yield
    logger.info("OmniMem Web UI shutting down")


# gzip responses (dashboard/memories/telemetry HTML compress well). Outermost
# so it wraps every route; tiny responses below the threshold are left alone.
_middleware: list[Middleware] = [
    Middleware(GZipMiddleware, minimum_size=500),
]

# Auth — session login when the OAuth admin credentials are set (opt out via
# WEB_UI_LOGIN_ENABLED=false), bearer token when WEB_UI_AUTH_TOKEN is set.
# Either credential satisfies the middleware when both are configured.
_web_auth_token = os.getenv("WEB_UI_AUTH_TOKEN", "").strip()
_login_enabled = login_enabled()
if _web_auth_token or _login_enabled:
    _middleware.append(
        Middleware(AuthMiddleware, bearer_token=_web_auth_token, login=_login_enabled)
    )
    if _login_enabled:
        logger.info("Session login enabled for web UI (OAuth admin credentials)")
    if _web_auth_token:
        logger.info("Bearer token authentication enabled for web UI")

app = Starlette(
    routes=[
        Mount("/static", StaticFiles(directory=str(BASE_DIR / "static")), name="static"),
        *auth_routes,
        *dashboard_routes,
        *memories_routes,
        *search_routes,
        *detail_routes,
        *lifecycle_routes,
        *create_routes,
        *project_routes,
        *skill_routes,
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
app.state.templates.globals["login_enabled"] = _login_enabled
