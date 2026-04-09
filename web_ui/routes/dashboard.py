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

    for ns in ("episodic", "project", "knowledge", "preference"):
        keys = deps.store.scan_prefix(f"mem:{ns}:")
        if not keys:
            ns_stats[ns] = {"total": 0, "states": {"active": 0, "deprioritised": 0, "archived": 0}}
            continue

        state_counts = {"active": 0, "deprioritised": 0, "archived": 0}
        all_data = deps.store.get_multi(keys)

        for key, data in zip(keys, all_data):
            if data is None:
                continue
            state = data.get("state", "active")
            if state in state_counts:
                state_counts[state] += 1
            recent.append({
                "key": key,
                "namespace": ns,
                "content": (data.get("content") or "")[:100],
                "state": state,
                "project": data.get("project", ""),
                "updated_at": float(data.get("updated_at", "0")),
            })

        ns_stats[ns] = {"total": len(keys), "states": state_counts}
        total += len(keys)

    # Sort once across all namespaces, keep top 10
    recent.sort(key=lambda x: x["updated_at"], reverse=True)
    recent = recent[:10]

    for mem in recent:
        ts = mem["updated_at"]
        if ts > 0:
            mem["updated_at_fmt"] = time.strftime("%Y-%m-%d %H:%M", time.localtime(ts))
        else:
            mem["updated_at_fmt"] = "—"

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
