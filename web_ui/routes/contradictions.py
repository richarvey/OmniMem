"""Contradiction viewer route: list pairs, side-by-side comparison, resolve actions."""

import json

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps


async def contradictions_page(request: Request) -> HTMLResponse:
    """GET /contradictions — list all contradiction pairs."""
    keys = deps.store.scan_prefix("mem:episodic:")
    all_data = deps.store.get_multi(keys) if keys else []

    pairs = []
    seen = set()

    for key, data in zip(keys, all_data):
        if data is None:
            continue

        contradictions_raw = data.get("contradictions", "[]")
        try:
            contradictions = json.loads(contradictions_raw) if contradictions_raw else []
        except (json.JSONDecodeError, TypeError):
            continue

        for c in contradictions:
            if not isinstance(c, dict):
                continue
            other_key = c.get("key", "")
            pair_id = tuple(sorted([key, other_key]))
            if pair_id in seen:
                continue
            seen.add(pair_id)

            # Fetch the other memory's content
            other_data = deps.store.get(other_key) if other_key else None
            pairs.append({
                "key_a": key,
                "content_a": (data.get("content") or "")[:150],
                "key_b": other_key,
                "content_b": (other_data.get("content") or "")[:150] if other_data else c.get("content", "")[:150],
                "explanation": c.get("explanation", ""),
                "similarity": c.get("similarity"),
            })

    template = request.app.state.templates.get_template("contradictions.html")
    content = template.render(
        request=request,
        current_page="contradictions",
        pairs=pairs,
    )
    return HTMLResponse(content)


routes = [
    Route("/contradictions", contradictions_page),
]
