"""Duplicate detection route: scan trigger and cluster display."""

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from memory.dedup import find_all_duplicates

from .. import deps


async def duplicates_page(request: Request) -> HTMLResponse:
    """GET /duplicates — duplicate detection page."""
    template = request.app.state.templates.get_template("duplicates.html")
    content = template.render(
        request=request,
        current_page="duplicates",
        clusters=None,
        namespace="episodic",
        scanned=False,
    )
    return HTMLResponse(content)


async def duplicates_scan(request: Request) -> HTMLResponse:
    """GET /duplicates/scan — htmx endpoint that runs duplicate detection."""
    namespace = request.query_params.get("namespace", "episodic")
    if namespace not in {"episodic", "project", "knowledge"}:
        namespace = "episodic"

    project = request.query_params.get("project", "") or None

    clusters = find_all_duplicates(
        deps.store, deps.embedder, namespace,
        project_filter=project,
    )

    template = request.app.state.templates.get_template("duplicates.html")
    content = template.render(
        request=request,
        current_page="duplicates",
        clusters=clusters,
        namespace=namespace,
        scanned=True,
    )
    return HTMLResponse(content)


routes = [
    Route("/duplicates", duplicates_page),
    Route("/duplicates/scan", duplicates_scan),
]
