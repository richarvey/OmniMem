"""Browse memories with filtering and pagination."""

import math
import time

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps

PAGE_SIZE = 25


def _get_all_memories(namespace: str | None, state: str | None, project: str | None) -> list[dict]:
    """Fetch and filter memories across namespaces."""
    ns_list = [namespace] if namespace else ["episodic", "project", "knowledge"]
    memories = []

    for ns in ns_list:
        keys = deps.store.scan_prefix(f"mem:{ns}:")
        if not keys:
            continue
        all_data = deps.store.get_multi(keys)
        for key, data in zip(keys, all_data):
            if data is None:
                continue
            mem_state = data.get("state", "active")
            if state and mem_state != state:
                continue
            mem_project = data.get("project") or data.get("project_name") or ""
            if project and mem_project != project:
                continue
            memories.append({
                "key": key,
                "namespace": ns,
                "content": (data.get("content") or "")[:120],
                "state": mem_state,
                "project": mem_project,
                "updated_at": float(data.get("updated_at", "0")),
            })

    return memories


def _get_projects() -> list[str]:
    """Get distinct project names from episodic memories."""
    projects = set()
    for ns in ["episodic", "project", "knowledge"]:
        keys = deps.store.scan_prefix(f"mem:{ns}:")
        if not keys:
            continue
        all_data = deps.store.get_multi(keys)
        for data in all_data:
            if data and data.get("project"):
                projects.add(data["project"])
            if data and data.get("project_name"):
                projects.add(data["project_name"])
    return sorted(projects)


async def memories_list(request: Request) -> HTMLResponse:
    """GET /memories — browse with filters and pagination."""
    namespace = request.query_params.get("namespace", "")
    state = request.query_params.get("state", "")
    project = request.query_params.get("project", "")
    sort = request.query_params.get("sort", "newest")
    page = max(1, int(request.query_params.get("page", "1")))

    memories = _get_all_memories(
        namespace=namespace or None,
        state=state or None,
        project=project or None,
    )

    # Sort
    if sort == "oldest":
        memories.sort(key=lambda x: x["updated_at"])
    else:
        memories.sort(key=lambda x: x["updated_at"], reverse=True)

    # Format timestamps
    for mem in memories:
        ts = mem["updated_at"]
        mem["updated_at_fmt"] = time.strftime("%Y-%m-%d %H:%M", time.localtime(ts)) if ts > 0 else "—"

    # Paginate
    total = len(memories)
    total_pages = max(1, math.ceil(total / PAGE_SIZE))
    page = min(page, total_pages)
    start = (page - 1) * PAGE_SIZE
    page_memories = memories[start:start + PAGE_SIZE]

    projects = _get_projects()

    # Build extra params string for pagination links
    params = []
    if namespace:
        params.append(f"&namespace={namespace}")
    if state:
        params.append(f"&state={state}")
    if project:
        params.append(f"&project={project}")
    if sort != "newest":
        params.append(f"&sort={sort}")
    extra_params = "".join(params)

    # Check if this is an htmx request (partial)
    is_htmx = request.headers.get("HX-Request") == "true"
    template_name = "memories/_rows.html" if is_htmx else "memories/list.html"

    template = request.app.state.templates.get_template(template_name)
    content = template.render(
        request=request,
        memories=page_memories,
        namespace=namespace,
        state=state,
        project=project,
        sort=sort,
        projects=projects,
        page=page,
        total_pages=total_pages,
        total=total,
        extra_params=extra_params,
        base_url="/memories",
        current_page="memories",
    )
    return HTMLResponse(content)


routes = [
    Route("/memories", memories_list),
]
