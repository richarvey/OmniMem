"""Memory detail view route."""

import json
import time

from starlette.requests import Request
from starlette.responses import HTMLResponse, RedirectResponse
from starlette.routing import Route

from .. import deps


async def memory_detail(request: Request) -> HTMLResponse:
    """GET /memory/{key:path} — full memory detail view."""
    key = request.path_params["key"]
    data = deps.store.get(key)

    if data is None:
        template = request.app.state.templates.get_template("base.html")
        # Render a simple not-found within the base layout
        return HTMLResponse(
            template.render(
                request=request,
                current_page="memories",
            ).replace(
                "{% block content %}{% endblock %}",
                '<p class="empty-state">Memory not found.</p>',
            ),
            status_code=404,
        )

    # Parse namespace from key
    parts = key.split(":")
    namespace = parts[1] if len(parts) > 1 else "unknown"

    # Parse tags
    tags = []
    tags_raw = data.get("tags", "[]")
    try:
        tags = json.loads(tags_raw) if tags_raw else []
    except (json.JSONDecodeError, TypeError):
        pass

    # Parse abandoned approaches
    abandoned = []
    abandoned_raw = data.get("abandoned_approaches", "[]")
    try:
        abandoned = json.loads(abandoned_raw) if abandoned_raw else []
    except (json.JSONDecodeError, TypeError):
        pass

    # Parse contradictions
    contradictions = []
    contradictions_raw = data.get("contradictions", "[]")
    try:
        contradictions = json.loads(contradictions_raw) if contradictions_raw else []
    except (json.JSONDecodeError, TypeError):
        pass

    # Parse reinstate hints
    reinstate_hints = []
    hints_raw = data.get("reinstate_hints", "[]")
    try:
        reinstate_hints = json.loads(hints_raw) if hints_raw else []
    except (json.JSONDecodeError, TypeError):
        pass

    # Format timestamps
    def fmt_ts(raw):
        try:
            ts = float(raw)
            return time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(ts))
        except (ValueError, TypeError):
            return "—"

    memory = {
        "key": key,
        "namespace": namespace,
        "content": data.get("content", ""),
        "state": data.get("state", "active"),
        "project": data.get("project") or data.get("project_name") or "",
        "tags": tags,
        "surface_score": data.get("surface_score", "1.0"),
        "experience_weight": data.get("experience_weight", "1.0"),
        "effort_score": data.get("effort_score"),
        "outcome": data.get("outcome"),
        "iterations": data.get("iterations"),
        "breakthrough": data.get("breakthrough"),
        "gotchas": data.get("gotchas"),
        "abandoned_approaches": abandoned,
        "contradictions": contradictions,
        "reinstate_hints": reinstate_hints,
        "deprioritised_reason": data.get("deprioritised_reason", ""),
        "source_url": data.get("source_url", ""),
        "feed_name": data.get("feed_name", ""),
        "recall_count": int(data.get("recall_count") or 0),
        "last_recalled": fmt_ts(data.get("last_recalled")) if data.get("last_recalled") else "Never",
        "created_at": fmt_ts(data.get("created_at")),
        "updated_at": fmt_ts(data.get("updated_at")),
    }

    template = request.app.state.templates.get_template("detail.html")
    content = template.render(
        request=request,
        memory=memory,
        current_page="memories",
    )
    return HTMLResponse(content)


routes = [
    Route("/memory/{key:path}", memory_detail),
]
