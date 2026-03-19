"""Dashboard route: namespace counts, state counts, recent activity."""

import json
import time

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps


def _count_by_state(keys: list[str]) -> dict[str, int]:
    """Count memories grouped by lifecycle state."""
    counts: dict[str, int] = {"active": 0, "deprioritised": 0, "archived": 0}
    if not keys:
        return counts
    all_data = deps.store.get_multi(keys)
    for data in all_data:
        if data is None:
            continue
        state = data.get("state", "active")
        if state in counts:
            counts[state] += 1
    return counts


async def dashboard(request: Request) -> HTMLResponse:
    """GET / — overview dashboard with namespace stats and recent memories."""
    namespaces = {
        "episodic": deps.store.scan_prefix("mem:episodic:"),
        "project": deps.store.scan_prefix("mem:project:"),
        "knowledge": deps.store.scan_prefix("mem:knowledge:"),
    }

    ns_stats = {}
    total = 0
    for ns, keys in namespaces.items():
        state_counts = _count_by_state(keys)
        ns_stats[ns] = {
            "total": len(keys),
            "states": state_counts,
        }
        total += len(keys)

    # Recent memories (last 10 across all namespaces, sorted by updated_at)
    recent = []
    for ns, keys in namespaces.items():
        if not keys:
            continue
        all_data = deps.store.get_multi(keys)
        for key, data in zip(keys, all_data):
            if data is None:
                continue
            recent.append({
                "key": key,
                "namespace": ns,
                "content": (data.get("content") or "")[:100],
                "state": data.get("state", "active"),
                "project": data.get("project", ""),
                "updated_at": float(data.get("updated_at", "0")),
            })

    recent.sort(key=lambda x: x["updated_at"], reverse=True)
    recent = recent[:10]

    # Format timestamps
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

    template = request.app.state.templates.get_template("dashboard.html")
    content = template.render(
        request=request,
        ns_stats=ns_stats,
        total=total,
        recent=recent,
        health=health,
        current_page="dashboard",
    )
    return HTMLResponse(content)


routes = [
    Route("/", dashboard),
]
