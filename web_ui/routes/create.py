"""Create memory form and handler."""

import json
import logging
import time

import ulid

from starlette.requests import Request
from starlette.responses import HTMLResponse, RedirectResponse
from starlette.routing import Route

from memory.dedup import check_duplicate
from memory.lifecycle import MemoryState

from .. import deps

logger = logging.getLogger(__name__)


async def create_form(request: Request) -> HTMLResponse:
    """GET /create — memory creation form."""
    template = request.app.state.templates.get_template("create.html")
    content = template.render(
        request=request,
        current_page="create",
        error=None,
        duplicate=None,
        values={"content": "", "project": "", "namespace": "episodic", "tags": "", "force": False},
    )
    return HTMLResponse(content)


async def create_memory(request: Request) -> HTMLResponse:
    """POST /create — store a new memory (mirrors tools/core.py::remember logic)."""
    form = await request.form()
    content_text = form.get("content", "").strip()
    project = form.get("project", "").strip() or None
    namespace = form.get("namespace", "episodic")
    tags_raw = form.get("tags", "").strip()
    force = form.get("force") == "on"

    values = {
        "content": content_text,
        "project": project or "",
        "namespace": namespace,
        "tags": tags_raw,
        "force": force,
    }

    # Validate
    if not content_text:
        template = request.app.state.templates.get_template("create.html")
        return HTMLResponse(template.render(
            request=request, current_page="create",
            error="Content cannot be empty.", duplicate=None, values=values,
        ))

    if namespace not in {"episodic", "project", "knowledge", "preference"}:
        namespace = "episodic"

    # Parse tags
    tags = [t.strip() for t in tags_raw.split(",") if t.strip()] if tags_raw else []

    # Embed
    vector = deps.embedder.embed(content_text)

    # Duplicate check
    if not force:
        dup = check_duplicate(deps.store, namespace, vector, content_text, project_filter=project)
        if dup is not None:
            template = request.app.state.templates.get_template("create.html")
            return HTMLResponse(template.render(
                request=request, current_page="create",
                error=None, values=values,
                duplicate={
                    "key": dup.key,
                    "content": dup.content[:200],
                    "similarity": round(dup.similarity, 4),
                },
            ))

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
