"""Dashboard route: namespace counts, state counts, recent activity."""

import time

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps


async def dashboard(request: Request) -> HTMLResponse:
    """GET / — overview dashboard with namespace stats and recent memories."""
    ns_stats = {}
    total = 0
    recent = []

    candidates: list[dict] = []
    for ns in ("episodic", "project", "knowledge", "preference"):
        keys = deps.store.scan_prefix(f"mem:{ns}:")
        if not keys:
            ns_stats[ns] = {"total": 0, "states": {"active": 0, "deprioritised": 0, "archived": 0}}
            continue

        state_counts = {"active": 0, "deprioritised": 0, "archived": 0}
        # Pass 1: pull only the small fields needed for counts + recency ranking.
        # Content is fetched later for just the 10 winners, not every memory.
        meta = deps.store.get_fields_multi(keys, ("state", "updated_at"))

        for key, data in zip(keys, meta):
            state = (data or {}).get("state", "active")
            if state in state_counts:
                state_counts[state] += 1
            candidates.append({
                "key": key,
                "namespace": ns,
                "state": state,
                "updated_at": float((data or {}).get("updated_at", "0")),
            })

        ns_stats[ns] = {"total": len(keys), "states": state_counts}
        total += len(keys)

    # Rank across all namespaces, keep the 10 most recent, then hydrate only
    # those with content/project (a couple of extra small reads, not thousands).
    candidates.sort(key=lambda x: x["updated_at"], reverse=True)
    recent = candidates[:10]
    if recent:
        detail = deps.store.get_fields_multi(
            [m["key"] for m in recent], ("content", "project")
        )
        for mem, data in zip(recent, detail):
            data = data or {}
            mem["content"] = (data.get("content") or "")[:100]
            mem["project"] = data.get("project", "")
            ts = mem["updated_at"]
            mem["updated_at_fmt"] = (
                time.strftime("%Y-%m-%d %H:%M", time.localtime(ts)) if ts > 0 else "—"
            )

    # Health check
    health = {"valkey": False, "model": False}
    try:
        deps.store.client.ping()
        health["valkey"] = True
    except Exception:
        pass
    health["model"] = deps.embedder.is_loaded if deps.embedder else False

    # Enrichment queue status
    enrichment_pending = 0
    try:
        enrichment_pending = deps.store.client.llen("queue:enrich")
    except Exception:
        enrichment_pending = -1

    template = request.app.state.templates.get_template("dashboard.html")
    content = template.render(
        request=request,
        ns_stats=ns_stats,
        total=total,
        recent=recent,
        health=health,
        enrichment_pending=enrichment_pending,
        current_page="dashboard",
    )
    return HTMLResponse(content)


routes = [
    Route("/", dashboard),
]
