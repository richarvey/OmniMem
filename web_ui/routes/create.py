"""Create memory form and handler."""

import json
import logging
import time

import ulid

from starlette.requests import Request
from starlette.responses import HTMLResponse, RedirectResponse
from starlette.routing import Route

from memory.dedup import check_duplicate
from memory.licence import (
    LICENCE_CLASSES,
    LICENCE_LABELS,
    default_licence,
    licence_fields,
    resolve_licence,
    validate_licence_note,
)
from memory.lifecycle import MemoryState

from .. import deps

logger = logging.getLogger(__name__)

_LICENCE_CHOICES = [(value, LICENCE_LABELS[value]) for value in LICENCE_CLASSES]


def _render_form(request: Request, values: dict, error=None, duplicate=None) -> HTMLResponse:
    template = request.app.state.templates.get_template("create.html")
    return HTMLResponse(template.render(
        request=request, current_page="create",
        error=error, duplicate=duplicate, values=values,
        licence_classes=_LICENCE_CHOICES,
    ))


async def create_form(request: Request) -> HTMLResponse:
    """GET /create — memory creation form."""
    return _render_form(request, {
        "content": "", "project": "", "namespace": "episodic", "tags": "",
        "force": False, "licence": "", "licence_note": "",
    })


async def create_memory(request: Request) -> HTMLResponse:
    """POST /create — store a new memory (mirrors tools/core.py::remember logic)."""
    form = await request.form()
    content_text = form.get("content", "").strip()
    project = form.get("project", "").strip() or None
    namespace = form.get("namespace", "episodic")
    tags_raw = form.get("tags", "").strip()
    force = form.get("force") == "on"
    licence_raw = form.get("licence", "").strip()
    licence_note_raw = form.get("licence_note", "").strip()

    values = {
        "content": content_text,
        "project": project or "",
        "namespace": namespace,
        "tags": tags_raw,
        "force": force,
        "licence": licence_raw,
        "licence_note": licence_note_raw,
    }

    # Validate
    if not content_text:
        return _render_form(request, values, error="Content cannot be empty.")

    if namespace not in {"episodic", "project", "knowledge", "preference"}:
        namespace = "episodic"

    # Redistribution rights: an empty choice takes the namespace default
    # (own for conversation namespaces, unknown for knowledge), same as the
    # remember() tool.
    try:
        if licence_raw:
            licence_class, derived_note = resolve_licence(licence_raw)
        else:
            licence_class, derived_note = default_licence(namespace), None
        licence_data = licence_fields(
            licence_class, validate_licence_note(licence_note_raw) or derived_note,
        )
    except ValueError as exc:
        return _render_form(request, values, error=str(exc))

    # Parse tags
    tags = [t.strip() for t in tags_raw.split(",") if t.strip()] if tags_raw else []

    # Embed
    vector = deps.embedder.embed(content_text)

    # Duplicate check
    if not force:
        dup = check_duplicate(deps.store, namespace, vector, content_text, project_filter=project)
        if dup is not None:
            return _render_form(request, values, duplicate={
                "key": dup.key,
                "content": dup.content[:200],
                "similarity": round(dup.similarity, 4),
            })

    # Store
    key = f"mem:{namespace}:{ulid.new().str}"
    now = str(time.time())
    fields = {
        "content": content_text,
        "state": MemoryState.ACTIVE.value,
        "surface_score": "1.0",
        "experience_weight": "1.0",
        "created_at": now,
        "updated_at": now,
        "tags": json.dumps(tags),
        **licence_data,
    }
    if project:
        fields["project"] = project

    deps.store.upsert(namespace, key, fields, vector)
    logger.info("Created memory %s via web UI", key)

    return RedirectResponse(url=f"/memory/{key}", status_code=303)


routes = [
    Route("/create", create_form),
    Route("/create", create_memory, methods=["POST"]),
]
