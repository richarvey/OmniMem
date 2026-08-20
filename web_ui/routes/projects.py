"""Project management routes: list, detail, edit, create."""

import logging
import time

from starlette.requests import Request
from starlette.responses import HTMLResponse, RedirectResponse
from starlette.routing import Route

from memory.lifecycle import MemoryState, bulk_transition_project
from memory.project_domains import (
    invalidate_domain_cache,
    known_project_domains,
    normalise_domains,
    read_project_domains,
    serialise_domains,
    suggest_domains_for_project,
)
from memory.skills import GENERATED_SKILL_PREFIX

from .. import deps

logger = logging.getLogger(__name__)


def _skill_domains() -> set[str]:
    """Domains that already have a compiled skill, so the UI can link to it."""
    keys = deps.store.scan_prefix(GENERATED_SKILL_PREFIX)
    if not keys:
        return set()
    rows = deps.store.get_fields_multi(keys, ("domain",))
    return {row["domain"] for row in rows if row and row.get("domain")}


def _domain_suggestions() -> list[str]:
    """Datalist vocabulary: domains in use on projects plus compiled skills."""
    return sorted(set(known_project_domains(deps.store)) | _skill_domains())


async def project_list(request: Request) -> HTMLResponse:
    """GET /projects — list all projects, deduplicated by name.

    ?domain=python narrows to the projects declaring that work-type domain.
    """
    raw_domain = (request.query_params.get("domain") or "").strip()
    wanted: str | None = None
    invalid_domain = False
    if raw_domain:
        resolved, _, _ = normalise_domains(raw_domain)
        if resolved:
            wanted = resolved[0]
        else:
            invalid_domain = True

    keys = deps.store.scan_prefix("mem:project:")
    all_data = deps.store.get_multi(keys) if keys else []

    # Group by resolved project name to deduplicate
    project_map: dict[str, dict] = {}
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        name = data.get("project_name") or data.get("project") or key.split(":")[-1]
        is_context = bool(data.get("goals") or data.get("stack"))

        if name not in project_map:
            project_map[name] = {
                "name": name,
                "description": "",
                "current_state": "",
                "state": "active",
                "updated_at": 0.0,
                "memory_count": 0,
                "has_context": False,
                "domains": [],
            }

        try:
            updated = float(data.get("updated_at", "0"))
        except (TypeError, ValueError):
            updated = 0.0
        if updated > project_map[name]["updated_at"]:
            project_map[name]["updated_at"] = updated

        if is_context:
            project_map[name]["description"] = (data.get("description") or "")[:120]
            project_map[name]["current_state"] = (data.get("current_state") or "")[:120]
            project_map[name]["state"] = data.get("state", "active")
            project_map[name]["has_context"] = True
            project_map[name]["domains"] = read_project_domains(data)
        else:
            project_map[name]["memory_count"] += 1

    projects = sorted(project_map.values(), key=lambda x: x["updated_at"], reverse=True)
    total = len(projects)
    if wanted is not None:
        projects = [p for p in projects if wanted in p["domains"]]
    elif invalid_domain:
        projects = []

    for p in projects:
        ts = p["updated_at"]
        if ts > 0:
            lt = time.localtime(ts)
            p["updated_date"] = time.strftime("%-d %b %Y", lt)
            p["updated_time"] = time.strftime("%H:%M", lt)
        else:
            p["updated_date"] = "—"
            p["updated_time"] = ""

    template = request.app.state.templates.get_template("projects/list.html")
    content = template.render(
        request=request,
        projects=projects,
        current_page="projects",
        domain_filter=wanted or (raw_domain if invalid_domain else None),
        domain_filter_valid=not invalid_domain,
        all_domains=sorted(known_project_domains(deps.store).items()),
        total_projects=total,
    )
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

    domains = read_project_domains(data)
    compiled = _skill_domains()
    project = {
        "name": data.get("project_name", name),
        "description": data.get("description", ""),
        "stack": data.get("stack", ""),
        "domains": [
            {"name": d, "has_skill": d in compiled} for d in domains
        ],
        "goals": data.get("goals", ""),
        "current_state": data.get("current_state", ""),
        "notes": data.get("notes", ""),
        "state": data.get("state", "active"),
        "created_at": fmt_ts(data.get("created_at")),
        "updated_at": fmt_ts(data.get("updated_at")),
    }

    template = request.app.state.templates.get_template("projects/detail.html")
    content = template.render(
        request=request,
        project=project,
        current_page="projects",
        skill_user=_skill_user(),
    )
    return HTMLResponse(content)


def _skill_user() -> str:
    from memory.skills import skill_user
    return skill_user()


async def project_suggest_domains(request: Request) -> HTMLResponse:
    """POST /projects/{name}/domains/suggest — htmx partial with a domain draft.

    Proposes only; the human still presses Save on the edit form. Mirrors the
    propose-and-accept shape the skill compiler uses, for the same reason:
    derived content is a starting point, not a decision.
    """
    name = request.path_params["name"]
    if deps.store.get(f"mem:project:{name}") is None:
        return HTMLResponse(
            '<p class="empty-state">Project not found.</p>', status_code=404
        )

    suggestion = suggest_domains_for_project(deps.store, name)
    template = request.app.state.templates.get_template(
        "partials/domain_suggestion.html"
    )
    return HTMLResponse(template.render(
        request=request,
        suggestion=suggestion,
        value=serialise_domains(suggestion["merged_domains"]),
    ))


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
        "domains": serialise_domains(read_project_domains(data)),
        "goals": data.get("goals", ""),
        "current_state": data.get("current_state", ""),
        "notes": data.get("notes", ""),
    }

    template = request.app.state.templates.get_template("projects/edit.html")
    content = template.render(
        request=request,
        project=project,
        current_page="projects",
        is_new=False,
        domain_options=_domain_suggestions(),
    )
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
    domains, _, _ = normalise_domains(form.get("domains", ""))

    # Re-embed with updated content
    embed_text = f"{description} {goals} {current_state}"
    vector = deps.embedder.embed(embed_text)

    fields = {
        "content": description,
        "project_name": name,
        "description": description,
        "stack": stack,
        "domains": serialise_domains(domains),
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
    invalidate_domain_cache()
    logger.info("Saved project %s via web UI", name)

    return RedirectResponse(url=f"/projects/{name}", status_code=303)


async def project_create_form(request: Request) -> HTMLResponse:
    """GET /projects/new — create project form."""
    project = {
        "name": "", "description": "", "stack": "", "domains": "",
        "goals": "", "current_state": "", "notes": "",
    }
    template = request.app.state.templates.get_template("projects/edit.html")
    content = template.render(
        request=request,
        project=project,
        current_page="projects",
        is_new=True,
        domain_options=_domain_suggestions(),
    )
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
    domains, _, _ = normalise_domains(form.get("domains", ""))

    embed_text = f"{description} {goals} {current_state}"
    vector = deps.embedder.embed(embed_text)

    fields = {
        "content": description,
        "project_name": name,
        "description": description,
        "stack": stack,
        "domains": serialise_domains(domains),
        "goals": goals,
        "current_state": current_state,
        "notes": notes,
        "state": MemoryState.ACTIVE.value,
        "surface_score": "1.0",
        "created_at": now,
        "updated_at": now,
    }

    deps.store.upsert("project", key, fields, vector)
    invalidate_domain_cache()
    logger.info("Created project %s via web UI", name)

    return RedirectResponse(url=f"/projects/{name}", status_code=303)


async def project_delete(request: Request) -> RedirectResponse:
    """POST /projects/{name}/delete — delete a project context."""
    name = request.path_params["name"]
    key = f"mem:project:{name}"
    data = deps.store.get(key)

    if data is None:
        logger.warning("Delete requested for non-existent project %s", name)
        return RedirectResponse(url="/projects", status_code=303)

    deps.store.delete(key)
    invalidate_domain_cache()
    logger.info("Deleted project %s via web UI", name)

    return RedirectResponse(url="/projects", status_code=303)


async def project_deprioritise(request: Request) -> RedirectResponse:
    """POST /projects/{name}/deprioritise — bulk deprioritise every active memory."""
    name = request.path_params["name"]
    result = bulk_transition_project(
        deps.store, name, MemoryState.DEPRIORITISED,
        apply=True, reason="Deprioritised via web UI", include_context=True,
    )
    logger.info("Deprioritised project %s via web UI (%d memories)", name, result["changed"])
    return RedirectResponse(url="/projects", status_code=303)


async def project_reinstate(request: Request) -> RedirectResponse:
    """POST /projects/{name}/reinstate — bulk reinstate memories to active."""
    name = request.path_params["name"]
    result = bulk_transition_project(
        deps.store, name, MemoryState.ACTIVE,
        apply=True, include_context=True,
    )
    logger.info("Reinstated project %s via web UI (%d memories)", name, result["changed"])
    return RedirectResponse(url="/projects", status_code=303)


routes = [
    Route("/projects", project_list),
    Route("/projects/new", project_create_form),
    Route("/projects/new", project_create, methods=["POST"]),
    Route("/projects/{name:path}/delete", project_delete, methods=["POST"]),
    Route("/projects/{name:path}/deprioritise", project_deprioritise, methods=["POST"]),
    Route("/projects/{name:path}/reinstate", project_reinstate, methods=["POST"]),
    Route("/projects/{name:path}/edit", project_edit_form),
    Route("/projects/{name:path}/edit", project_save, methods=["POST"]),
    Route(
        "/projects/{name:path}/domains/suggest",
        project_suggest_domains,
        methods=["POST"],
    ),
    Route("/projects/{name:path}", project_detail),
]
