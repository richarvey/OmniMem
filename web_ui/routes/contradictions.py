"""Contradiction viewer route: list pairs, side-by-side comparison, resolve actions."""

import json

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps


async def contradictions_page(request: Request) -> HTMLResponse:
    """GET /contradictions — list all contradiction pairs."""
    keys = deps.store.scan_prefix("mem:episodic:")
    # Only content + contradictions are needed to build the pair list.
    all_data = (
        deps.store.get_fields_multi(keys, ("content", "contradictions"))
        if keys else []
    )

    # First pass: collect the deduped pairs and every "other" key we'll need.
    raw_pairs = []
    seen = set()
    other_keys: set[str] = set()

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
            if other_key:
                other_keys.add(other_key)
            raw_pairs.append((key, data.get("content") or "", c))

    # Second pass: one batched fetch for all "other" memories, no N+1 gets.
    other_content: dict[str, str] = {}
    if other_keys:
        other_list = list(other_keys)
        fetched = deps.store.get_fields_multi(other_list, ("content",))
        for ok, od in zip(other_list, fetched):
            other_content[ok] = (od or {}).get("content") or ""

    pairs = []
    for key, content_a, c in raw_pairs:
        other_key = c.get("key", "")
        content_b = other_content.get(other_key) or c.get("content", "")
        pairs.append({
            "key_a": key,
            "content_a": content_a[:150],
            "key_b": other_key,
            "content_b": content_b[:150],
            "explanation": c.get("explanation", ""),
            "similarity": c.get("similarity"),
        })

    # Reuse the duplicates route helper for last maintenance info
    from .duplicates import _get_last_maintenance

    template = request.app.state.templates.get_template("contradictions.html")
    content = template.render(
        request=request,
        current_page="contradictions",
        pairs=pairs,
        last_maintenance=_get_last_maintenance(),
    )
    return HTMLResponse(content)


routes = [
    Route("/contradictions", contradictions_page),
]
