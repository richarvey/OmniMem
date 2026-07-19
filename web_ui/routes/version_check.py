"""Version check route: compare running version against latest Codeberg release."""

import json
import logging
import time
from urllib.request import urlopen, Request

from starlette.requests import Request as StarletteRequest
from starlette.responses import HTMLResponse
from starlette.routing import Route

from memory.version import __version__

logger = logging.getLogger("omnimem.web.version_check")

# /releases/latest excludes drafts and pre-releases, so a beta cut on a
# version branch (e.g. v6.1.0 marked pre-release) never nudges stable
# installs — the indicator only points at the latest stable release.
CODEBERG_API_URL = (
    "https://codeberg.org/api/v1/repos/ric_harvey/omnimem/releases/latest"
)
CACHE_TTL = 3600  # Cache for 1 hour

_cache: dict = {"latest": None, "checked_at": 0.0}


def _parse_version(v: str) -> tuple[int, ...]:
    """Parse a version string like '3.10.0' into a comparable tuple."""
    return tuple(int(x) for x in v.split("."))


def _fetch_latest_version() -> str | None:
    """Fetch the latest release tag from Codeberg, with caching."""
    now = time.time()
    if _cache["latest"] and (now - _cache["checked_at"]) < CACHE_TTL:
        return _cache["latest"]

    try:
        req = Request(CODEBERG_API_URL, headers={"Accept": "application/json"})
        with urlopen(req, timeout=5) as resp:  # scheme is validated above  # nosec B310
            data = json.loads(resp.read())
            if isinstance(data, dict) and data.get("tag_name"):
                tag = data["tag_name"].lstrip("v")
                _cache["latest"] = tag
                _cache["checked_at"] = now
                return tag
    except Exception:
        logger.debug("Failed to check for new version", exc_info=True)

    return _cache.get("latest")


async def version_check(request: StarletteRequest) -> HTMLResponse:
    """GET /version-check — returns an htmx partial with update indicator."""
    latest = _fetch_latest_version()

    if not latest:
        return HTMLResponse("")

    try:
        if _parse_version(latest) > _parse_version(__version__):
            template = request.app.state.templates.get_template(
                "partials/version_update.html"
            )
            return HTMLResponse(template.render(latest_version=latest))
    except Exception:
        logger.debug("Failed to compare versions", exc_info=True)

    return HTMLResponse("")


routes = [
    Route("/version-check", version_check),
]
