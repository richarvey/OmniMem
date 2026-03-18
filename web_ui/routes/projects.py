"""Project management routes: list, detail, edit, create."""

import logging
import time

from starlette.requests import Request
from starlette.responses import HTMLResponse, RedirectResponse
from starlette.routing import Route

from memory.lifecycle import MemoryState

from .. import deps

logger = logging.getLogger(__name__)


async def project_list(request: Request) -> HTMLResponse:
    """GET /projects — list all projects."""
    keys = deps.store.scan_prefix("mem:project:")
    all_data = deps.store.get_multi(keys) if keys else []

    projects = []
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        projects.append({
            "name": data.get("project_name", key.split(":")[-1]),
            "description": (data.get("description") or "")[:120],
            "current_state": (data.get("current_state") or "")[:120],
            "state": data.get("state", "active"),
            "updated_at": float(data.get("updated_at", "0")),
        })

    projects.sort(key=lambda x: x["updated_at"], reverse=True)
    for p in projects:
        ts = p["updated_at"]
        p["updated_at_fmt"] = time.strftime("%Y-%m-%d %H:%M", time.localtime(ts)) if ts > 0 else "—"

    template = request.app.state.templates.get_template("projects/list.html")
    content = template.render(request=request, projects=projects, current_page="projects")
    return HTMLResponse(content)


async def project_detail(request: Request) -> HTMLResponse:
    """GET /projects/{name} — project detail view."""
    name = request.path_params["name"]
    key = f"mem:project:{name}"
    data = deps.store.get(key)

    if data is None:
        return HTMLResponse('<p class="empty-state">Project not found.</p>', status_code=404)

    def fmt_ts(raw):
        try:
            return time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(float(raw)))
        except (ValueError, TypeError):
            return "—"

    project = {
        "name": data.get("project_name", name),
        "description": data.get("description", ""),
        "stack": data.get("stack", ""),
        "goals": data.get("goals", ""),
        "current_state": data.get("current_state", ""),
        "notes": data.get("notes", ""),
        "state": data.get("state", "active"),
        "created_at": fmt_ts(data.get("created_at")),
        "updated_at": fmt_ts(data.get("updated_at")),
    }

    template = request.app.state.templates.get_template("projects/detail.html")
    content = template.render(request=request, project=project, current_page="projects")
    return HTMLResponse(content)


async def project_edit_form(request: Request) -> HTMLResponse:
    """GET /projects/{name}/edit — edit form."""
    name = request.path_params["name"]
    key = f"mem:project:{name}"
    data = deps.store.get(key)

    if data is None:
        return HTMLResponse('<p class="empty-state">Project not found.</p>', status_code=404)

    project = {
        "name": data.get("project_name", name),
        "description": data.get("description", ""),
        "stack": data.get("stack", ""),
        "goals": data.get("goals", ""),
        "current_state": data.get("current_state", ""),
        "notes": data.get("notes", ""),
    }

    template = request.app.state.templates.get_template("projects/edit.html")
    content = template.render(request=request, project=project, current_page="projects", is_new=False)
    return HTMLResponse(content)


async def project_save(request: Request) -> RedirectResponse:
    """POST /projects/{name}/edit — save project changes."""
    name = request.path_params["name"]
    form = await request.form()

    key = f"mem:project:{name}"
    now = str(time.time())

    description = form.get("description", "").strip()
    stack = form.get("stack", "").strip()
    goals = form.get("goals", "").strip()
    current_state = form.get("current_state", "").strip()
    notes = form.get("notes", "").strip()

    # Re-embed with updated content
    embed_text = f"{description} {goals} {current_state}"
    vector = deps.embedder.embed(embed_text)

    fields = {
        "content": description,
        "project_name": name,
        "description": description,
        "stack": stack,
        "goals": goals,
        "current_state": current_state,
        "notes": notes,
        "state": MemoryState.ACTIVE.value,
        "surface_score": "1.0",
        "updated_at": now,
    }

    # Check if this is a new project (no created_at)
    existing = deps.store.get(key)
    if existing is None:
        fields["created_at"] = now

    deps.store.upsert("project", key, fields, vector)
    logger.info("Saved project %s via web UI", name)

    return RedirectResponse(url=f"/projects/{name}", status_code=303)


async def project_create_form(request: Request) -> HTMLResponse:
    """GET /projects/new — create project form."""
    project = {"name": "", "description": "", "stack": "", "goals": "", "current_state": "", "notes": ""}
    template = request.app.state.templates.get_template("projects/edit.html")
    content = template.render(request=request, project=project, current_page="projects", is_new=True)
    return HTMLResponse(content)


async def project_create(request: Request) -> RedirectResponse:
    """POST /projects/new — create a new project."""
    form = await request.form()
    name = form.get("name", "").strip()

    if not name:
        return RedirectResponse(url="/projects/new", status_code=303)

    key = f"mem:project:{name}"
    now = str(time.time())

    description = form.get("description", "").strip()
    stack = form.get("stack", "").strip()
    goals = form.get("goals", "").strip()
    current_state = form.get("current_state", "").strip()
    notes = form.get("notes", "").strip()

    embed_text = f"{description} {goals} {current_state}"
    vector = deps.embedder.embed(embed_text)

    fields = {
        "content": description,
        "project_name": name,
        "description": description,
        "stack": stack,
        "goals": goals,
        "current_state": current_state,
        "notes": notes,
        "state": MemoryState.ACTIVE.value,
        "surface_score": "1.0",
        "created_at": now,
        "updated_at": now,
    }

    deps.store.upsert("project", key, fields, vector)
    logger.info("Created project %s via web UI", name)

    return RedirectResponse(url=f"/projects/{name}", status_code=303)


routes = [
    Route("/projects", project_list),
    Route("/projects/new", project_create_form),
    Route("/projects/new", project_create, methods=["POST"]),
    Route("/projects/{name:path}/edit", project_edit_form),
    Route("/projects/{name:path}/edit", project_save, methods=["POST"]),
    Route("/projects/{name:path}", project_detail),
]
