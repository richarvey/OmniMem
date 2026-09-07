"""Memory detail view route."""

import json
import time
from urllib.parse import quote

from starlette.requests import Request
from starlette.responses import HTMLResponse, RedirectResponse
from starlette.routing import Route

from memory.licence import (
    LICENCE_CHOICES,
    LICENCE_LABELS,
    classify_memories,
    is_classifiable_key,
    note_for_reclassification,
    resolve_licence,
    validate_licence_note,
)
from memory.provenance import (
    PROVENANCE_CHOICES,
    PROVENANCE_LABELS,
    classify_provenance,
    resolve_provenance,
)
from memory.tags import retag_memory

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
        "licence": data.get("licence") or "",
        "licence_label": LICENCE_LABELS.get(data.get("licence") or "", "Not recorded"),
        "licence_note": data.get("licence_note") or "",
        "provenance": data.get("provenance") or "",
        "provenance_label": PROVENANCE_LABELS.get(data.get("provenance") or "", "Not recorded"),
        "recall_count": int(data.get("recall_count") or 0),
        "last_recalled": fmt_ts(data.get("last_recalled")) if data.get("last_recalled") else "Never",
        "created_at": fmt_ts(data.get("created_at")),
        "updated_at": fmt_ts(data.get("updated_at")),
    }

    template = request.app.state.templates.get_template("detail.html")
    content = template.render(
        request=request,
        memory=memory,
        tag_error=request.query_params.get("tag_error", ""),
        licence_error=request.query_params.get("licence_error", ""),
        licence_classes=LICENCE_CHOICES,
        provenance_error=request.query_params.get("provenance_error", ""),
        provenance_classes=PROVENANCE_CHOICES,
        current_page="memories",
    )
    return HTMLResponse(content)


async def memory_provenance(request: Request) -> RedirectResponse:
    """POST /memory/{key:path}/provenance — reclassify where a memory came from.

    Same engine as the set_provenance MCP tool: facts extracted from the
    memory follow it, and updated_at is left alone.
    """
    key = request.path_params["key"]
    if not is_classifiable_key(key) or deps.store.get_fields_multi([key], ("created_at",))[0] is None:
        return RedirectResponse(url=f"/memory/{key}", status_code=303)

    form = await request.form()
    try:
        classify_provenance(deps.store, [key], resolve_provenance(form.get("provenance", "")))
    except ValueError as exc:
        return RedirectResponse(
            url=f"/memory/{key}?provenance_error={quote(str(exc))}", status_code=303
        )
    return RedirectResponse(url=f"/memory/{key}", status_code=303)


async def memory_licence(request: Request) -> RedirectResponse:
    """POST /memory/{key:path}/licence — record a memory's redistribution rights.

    Same engine as the set_licence MCP tool (memory/licence.py
    classify_memories): the class must resolve, the note is optional and
    bounded, facts extracted from the memory follow it, and a
    reclassification never keeps the old class's note. Only the four
    licence-bearing namespaces are accepted — skills are derived, and
    nothing else under a mem:/meta: prefix is a memory.
    """
    key = request.path_params["key"]
    if not is_classifiable_key(key):
        return RedirectResponse(url=f"/memory/{key}", status_code=303)
    # get_fields_multi returns None when none of the projected fields exist,
    # not when the key is missing — so created_at (which every writer sets)
    # is what proves the record is there. An unstamped record must still be
    # classifiable; that is the whole point of the form.
    current = deps.store.get_fields_multi([key], ("created_at", "licence", "licence_note"))[0]
    if current is None:
        return RedirectResponse(url=f"/memory/{key}", status_code=303)

    form = await request.form()
    try:
        licence_class, derived_note = resolve_licence(form.get("licence", ""))
        note = note_for_reclassification(
            current.get("licence") or "", current.get("licence_note"),
            licence_class, validate_licence_note(form.get("licence_note", "")),
        ) or derived_note
        classify_memories(deps.store, [key], licence_class, note)
    except ValueError as exc:
        return RedirectResponse(
            url=f"/memory/{key}?licence_error={quote(str(exc))}", status_code=303
        )
    return RedirectResponse(url=f"/memory/{key}", status_code=303)


async def memory_retag(request: Request) -> RedirectResponse:
    """POST /memory/{key:path}/tags — replace a memory's tags from a comma-separated field."""
    key = request.path_params["key"]
    form = await request.form()
    raw = form.get("tags", "")
    tags = [t.strip() for t in raw.split(",") if t.strip()]

    try:
        retag_memory(deps.store, key, tags=tags)
    except ValueError as exc:
        return RedirectResponse(
            url=f"/memory/{key}?tag_error={quote(str(exc))}", status_code=303
        )
    return RedirectResponse(url=f"/memory/{key}", status_code=303)


routes = [
    # Must precede the greedy {key:path} detail route
    Route("/memory/{key:path}/tags", memory_retag, methods=["POST"]),
    Route("/memory/{key:path}/licence", memory_licence, methods=["POST"]),
    Route("/memory/{key:path}/provenance", memory_provenance, methods=["POST"]),
    Route("/memory/{key:path}", memory_detail),
]
