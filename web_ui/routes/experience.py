"""Experience tracking routes: summary dashboard and abandoned approach graveyard."""

import json
import math

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps


EFFORTFUL_PAGE_SIZE = 10


async def experience_summary(request: Request) -> HTMLResponse:
    """GET /experience — experience summary dashboard."""
    project = request.query_params.get("project", "")
    outcome_filter = request.query_params.get("outcome", "")
    if outcome_filter not in ("succeeded", "pivoted", "abandoned"):
        outcome_filter = ""
    try:
        page = max(1, int(request.query_params.get("page", "1")))
    except ValueError:
        page = 1

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

    # Filter + paginate the effortful table
    if outcome_filter:
        effortful = [m for m in effortful if m["outcome"] == outcome_filter]
    total_pages = max(1, math.ceil(len(effortful) / EFFORTFUL_PAGE_SIZE))
    page = min(page, total_pages)
    start = (page - 1) * EFFORTFUL_PAGE_SIZE
    page_effortful = effortful[start:start + EFFORTFUL_PAGE_SIZE]

    extra_params = ""
    if outcome_filter:
        extra_params += f"&outcome={outcome_filter}"
    if project:
        extra_params += f"&project={project}"

    is_htmx = request.headers.get("HX-Request") == "true"
    template_name = "experience/_effortful.html" if is_htmx else "experience/summary.html"

    template = request.app.state.templates.get_template(template_name)
    content_html = template.render(
        request=request,
        current_page="experience",
        count=count_with_experience,
        avg_effort=avg_effort,
        outcomes=outcome_counts,
        effortful=page_effortful,
        outcome_filter=outcome_filter,
        page=page,
        total_pages=total_pages,
        base_url="/experience",
        extra_params=extra_params,
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
        current_page="graveyard",
        abandoned=unique_abandoned,
        project=project,
    )
    return HTMLResponse(content_html)


routes = [
    Route("/experience", experience_summary),
    Route("/experience/graveyard", graveyard),
]
