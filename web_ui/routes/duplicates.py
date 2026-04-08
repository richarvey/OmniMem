"""Duplicate detection route: scan trigger and cluster display."""

import json
from datetime import datetime, timezone

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from memory.dedup import find_all_duplicates

from .. import deps


def _get_last_maintenance() -> dict | None:
    """Fetch the most recent auto-maintenance run across all projects."""
    keys = deps.store.scan_prefix("meta:maintenance:")
    if not keys:
        return None

    all_data = deps.store.get_multi(keys)
    latest = None
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        ts_raw = data.get("last_maintenance_at")
        if not ts_raw:
            continue
        ts = float(ts_raw)
        if latest is None or ts > latest["timestamp"]:
            project = key.removeprefix("meta:maintenance:")
            summary_raw = data.get("last_maintenance_summary", "{}")
            try:
                summary = json.loads(summary_raw)
            except (json.JSONDecodeError, TypeError):
                summary = {}
            latest = {
                "project": project,
                "timestamp": ts,
                "when": datetime.fromtimestamp(ts, tz=timezone.utc).strftime("%d %b %Y %H:%M UTC"),
                "duplicates_archived": summary.get("duplicates_archived", 0),
                "contradictions_found": summary.get("contradictions_found", 0),
            }
    return latest


async def duplicates_page(request: Request) -> HTMLResponse:
    """GET /duplicates — duplicate detection page."""
    template = request.app.state.templates.get_template("duplicates.html")
    content = template.render(
        request=request,
        current_page="duplicates",
        clusters=None,
        namespace="episodic",
        scanned=False,
        last_maintenance=_get_last_maintenance(),
    )
    return HTMLResponse(content)


async def duplicates_scan(request: Request) -> HTMLResponse:
    """GET /duplicates/scan — htmx endpoint that runs duplicate detection."""
    namespace = request.query_params.get("namespace", "episodic")
    if namespace not in {"episodic", "project", "knowledge", "preference"}:
        namespace = "episodic"

    project = request.query_params.get("project", "") or None

    clusters = find_all_duplicates(
        deps.store, deps.embedder, namespace,
        project_filter=project,
    )

    template = request.app.state.templates.get_template("partials/dup_results.html")
    content = template.render(
        request=request,
        clusters=clusters,
        namespace=namespace,
    )
    return HTMLResponse(content)


routes = [
    Route("/duplicates", duplicates_page),
    Route("/duplicates/scan", duplicates_scan),
]
