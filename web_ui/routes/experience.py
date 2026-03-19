"""Experience tracking routes: summary dashboard and abandoned approach graveyard."""

import json

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps


async def experience_summary(request: Request) -> HTMLResponse:
    """GET /experience — experience summary dashboard."""
    project = request.query_params.get("project", "")

    keys = deps.store.scan_prefix("mem:episodic:")
    all_data = deps.store.get_multi(keys) if keys else []

    total_effort = 0
    count_with_experience = 0
    outcome_counts = {"succeeded": 0, "pivoted": 0, "abandoned": 0}
    effortful = []
    all_abandoned = []
    breakthroughs = []

    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if project and data.get("project") != project:
            continue

        effort_raw = data.get("effort_score")
        if effort_raw is None:
            continue

        try:
            effort = int(float(effort_raw))
        except (ValueError, TypeError):
            continue

        outcome = data.get("outcome", "unknown")
        count_with_experience += 1
        total_effort += effort

        if outcome in outcome_counts:
            outcome_counts[outcome] += 1

        content = (data.get("content") or "")[:100]
        effortful.append({
            "key": key,
            "content": content,
            "effort_score": effort,
            "outcome": outcome,
        })

        # Collect abandoned approaches
        abandoned_raw = data.get("abandoned_approaches", "[]")
        try:
            abandoned = json.loads(abandoned_raw) if abandoned_raw else []
        except (json.JSONDecodeError, TypeError):
            abandoned = []

        for approach in abandoned:
            if isinstance(approach, dict):
                all_abandoned.append({
                    "name": approach.get("name", "?"),
                    "type": approach.get("type", "?"),
                    "reason": approach.get("reason", ""),
                    "effort_score": effort,
                    "memory_key": key,
                })

        # Collect breakthroughs
        breakthrough = data.get("breakthrough")
        if breakthrough:
            breakthroughs.append({
                "key": key,
                "content": content,
                "effort_score": effort,
                "outcome": outcome,
                "breakthrough": breakthrough,
            })

    effortful.sort(key=lambda x: x["effort_score"], reverse=True)
    breakthroughs.sort(key=lambda x: x["effort_score"], reverse=True)

    # Deduplicate graveyard by name
    seen_names = set()
    unique_abandoned = []
    for item in all_abandoned:
        name = item["name"].lower()
        if name not in seen_names:
            seen_names.add(name)
            unique_abandoned.append(item)

    avg_effort = round(total_effort / count_with_experience, 2) if count_with_experience else 0

    template = request.app.state.templates.get_template("experience/summary.html")
    content_html = template.render(
        request=request,
        current_page="experience",
        count=count_with_experience,
        avg_effort=avg_effort,
        outcomes=outcome_counts,
        effortful=effortful[:10],
        breakthroughs=breakthroughs[:5],
        project=project,
    )
    return HTMLResponse(content_html)


async def graveyard(request: Request) -> HTMLResponse:
    """GET /experience/graveyard — abandoned approach graveyard."""
    project = request.query_params.get("project", "")

    keys = deps.store.scan_prefix("mem:episodic:")
    all_data = deps.store.get_multi(keys) if keys else []

    all_abandoned = []
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if project and data.get("project") != project:
            continue

        effort_raw = data.get("effort_score")
        try:
            effort = int(float(effort_raw)) if effort_raw else None
        except (ValueError, TypeError):
            effort = None

        abandoned_raw = data.get("abandoned_approaches", "[]")
        try:
            abandoned = json.loads(abandoned_raw) if abandoned_raw else []
        except (json.JSONDecodeError, TypeError):
            abandoned = []

        for approach in abandoned:
            if isinstance(approach, dict):
                all_abandoned.append({
                    "name": approach.get("name", "?"),
                    "type": approach.get("type", "?"),
                    "reason": approach.get("reason", ""),
                    "attempted_at": approach.get("attempted_at", ""),
                    "effort_score": effort,
                    "memory_key": key,
                })

    # Deduplicate by name, keeping highest effort
    seen = {}
    for item in all_abandoned:
        name = item["name"].lower()
        if name not in seen or (item["effort_score"] or 0) > (seen[name]["effort_score"] or 0):
            seen[name] = item
    unique_abandoned = sorted(seen.values(), key=lambda x: x.get("effort_score") or 0, reverse=True)

    template = request.app.state.templates.get_template("experience/graveyard.html")
    content_html = template.render(
        request=request,
        current_page="experience",
        abandoned=unique_abandoned,
        project=project,
    )
    return HTMLResponse(content_html)


routes = [
    Route("/experience", experience_summary),
    Route("/experience/graveyard", graveyard),
]
