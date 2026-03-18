"""OmniMem Web UI — Starlette app with htmx + Jinja2."""

import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path

from dotenv import load_dotenv
from jinja2 import Environment, FileSystemLoader
from starlette.applications import Starlette
from starlette.routing import Mount
from starlette.staticfiles import StaticFiles

from . import deps
from .routes.dashboard import routes as dashboard_routes
from .routes.memories import routes as memories_routes
from .routes.search import routes as search_routes
from .routes.detail import routes as detail_routes

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


app = Starlette(
    routes=[
        Mount("/static", StaticFiles(directory=str(BASE_DIR / "static")), name="static"),
        *dashboard_routes,
        *memories_routes,
        *search_routes,
        *detail_routes,
    ],
    lifespan=lifespan,
)

# Jinja2 template environment
app.state.templates = Environment(
    loader=FileSystemLoader(str(BASE_DIR / "templates")),
    autoescape=True,
)
