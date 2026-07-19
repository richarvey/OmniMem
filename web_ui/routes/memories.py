"""Browse memories with filtering and pagination."""

import math
import time

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps

PAGE_SIZE = 25


def _get_all_memories(
    namespace: str | None, state: str | None, project: str | None,
    source: str | None = None,
) -> tuple[list[dict], list[str]]:
    """Fetch and filter memories, returning (memories, distinct_projects) in a single pass.

    source splits knowledge by provenance: "rss" keeps only RSS-ingested
    articles (they carry a feed_name field), "learned" keeps everything
    else — extracted facts and remember() writes never have one.
    """
    ns_list = [namespace] if namespace else ["episodic", "project", "knowledge", "preference"]
    memories = []
    projects = set()

    for ns in ns_list:
        keys = deps.store.scan_prefix(f"mem:{ns}:")
        if not keys:
            continue
        # Only the fields the list view renders/filters on — not vectors,
        # breakthroughs, gotchas, abandoned lists, etc.
        all_data = deps.store.get_fields_multi(
            keys,
            ("content", "state", "project", "project_name", "updated_at",
             "feed_name", "last_recalled"),
        )
        for key, data in zip(keys, all_data):
            if data is None:
                continue

            mem_state = data.get("state", "active")
            mem_project = data.get("project") or data.get("project_name") or ""

            if mem_project:
                projects.add(mem_project)

            if state and mem_state != state:
                continue
            if project and mem_project != project:
                continue
            if source == "rss" and not data.get("feed_name"):
                continue
            if source == "learned" and data.get("feed_name"):
                continue

            try:
                updated_at = float(data.get("updated_at", "0"))
            except (TypeError, ValueError):
                # One malformed timestamp must not 500 the whole listing.
                updated_at = 0.0

            memories.append({
                "key": key,
                "namespace": ns,
                "content": (data.get("content") or "")[:120],
                "state": mem_state,
                "project": mem_project,
                "feed_name": data.get("feed_name") or "",
                "updated_at": updated_at,
                "heat": _recall_heat(data.get("last_recalled")),
            })

    return memories, sorted(projects)


def _recall_heat(last_recalled: str | None) -> str:
    """Bucket time-since-last-recall for the row's fading left rule."""
    try:
        ts = float(last_recalled or 0)
    except (TypeError, ValueError):
        ts = 0.0
    if ts <= 0:
        return ""
    days = (time.time() - ts) / 86400
    if days < 7:
        return "hot"
    if days < 30:
        return "warm"
    if days < 90:
        return "cool"
    return ""


async def memories_list(request: Request) -> HTMLResponse:
    """GET /memories — browse with filters and pagination."""
    namespace = request.query_params.get("namespace", "")
    state = request.query_params.get("state", "")
    project = request.query_params.get("project", "")
    source = request.query_params.get("source", "")
    if source not in ("rss", "learned"):
        source = ""
    sort = request.query_params.get("sort", "newest")
    page = max(1, int(request.query_params.get("page", "1")))

    memories, projects = _get_all_memories(
        namespace=namespace or None,
        state=state or None,
        project=project or None,
        source=source or None,
    )

    # Sort
    if sort == "oldest":
        memories.sort(key=lambda x: x["updated_at"])
    else:
        memories.sort(key=lambda x: x["updated_at"], reverse=True)

    # Format timestamps
    for mem in memories:
        ts = mem["updated_at"]
        if ts > 0:
            lt = time.localtime(ts)
            mem["updated_date"] = time.strftime("%-d %b %Y", lt)
            mem["updated_time"] = time.strftime("%H:%M", lt)
        else:
            mem["updated_date"] = "—"
            mem["updated_time"] = ""

    # Paginate
    total = len(memories)
    total_pages = max(1, math.ceil(total / PAGE_SIZE))
    page = min(page, total_pages)
    start = (page - 1) * PAGE_SIZE
    page_memories = memories[start:start + PAGE_SIZE]

    # Build extra params string for pagination links
    params = []
    if namespace:
        params.append(f"&namespace={namespace}")
    if state:
        params.append(f"&state={state}")
    if project:
        params.append(f"&project={project}")
    if source:
        params.append(f"&source={source}")
    if sort != "newest":
        params.append(f"&sort={sort}")
    extra_params = "".join(params)

    # Check if this is an htmx request (partial)
    is_htmx = request.headers.get("HX-Request") == "true"
    template_name = "memories/_rows.html" if is_htmx else "memories/list.html"

    # The sidebar's Preferences, Articles, and Learned Knowledge entries are
    # filtered views of this page — highlight them instead of Memories.
    if namespace == "preference":
        nav_page = "preferences"
    elif namespace == "knowledge":
        nav_page = "learned" if source == "learned" else "articles"
    else:
        nav_page = "memories"

    # Row actions redirect back to this exact view (filters + page intact).
    back_url = request.url.path
    if request.url.query:
        back_url += "?" + request.url.query

    template = request.app.state.templates.get_template(template_name)
    content = template.render(
        request=request,
        memories=page_memories,
        namespace=namespace,
        state=state,
        project=project,
        source=source,
        back_url=back_url,
        sort=sort,
        projects=projects,
        page=page,
        total_pages=total_pages,
        total=total,
        extra_params=extra_params,
        base_url="/memories",
        current_page=nav_page,
    )
    return HTMLResponse(content)


routes = [
    Route("/memories", memories_list),
]
