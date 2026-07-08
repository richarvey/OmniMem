"""Dashboard route: namespace counts, state counts, recent activity.

The scan-derived stats (counts + recent list) are cached in Valkey for
DASHBOARD_STATS_TTL seconds (issue #21) — at 100k+ memories the per-load
keyspace scan was the dashboard's entire cost, so it now runs at most once
per TTL window across all workers instead of on every page view. Health and
enrichment-queue indicators stay live (they're single cheap commands).
"""

import json
import logging
import os
import time

from starlette.requests import Request
from starlette.responses import HTMLResponse
from starlette.routing import Route

from .. import deps

logger = logging.getLogger(__name__)

_CACHE_KEY = "meta:dashboard_stats"


def _stats_ttl() -> int:
    """Cache TTL in seconds; 0 disables caching."""
    return int(os.getenv("DASHBOARD_STATS_TTL", "60"))


def _compute_stats(store) -> dict:
    """Scan the keyspace and build namespace/state counts + the recent list."""
    ns_stats = {}
    total = 0
    candidates: list[dict] = []

    for ns in ("episodic", "project", "knowledge", "preference"):
        keys = store.scan_prefix(f"mem:{ns}:")
        if not keys:
            ns_stats[ns] = {"total": 0, "states": {"active": 0, "deprioritised": 0, "archived": 0}}
            continue

        state_counts = {"active": 0, "deprioritised": 0, "archived": 0}
        # Pass 1: pull only the small fields needed for counts + recency ranking.
        # Content is fetched later for just the 10 winners, not every memory.
        # The project namespace additionally needs the name fields so we can
        # report the distinct project count (the raw record count is misleading
        # — it lumps context entries in with ULID project memories).
        fields = (
            ("state", "updated_at", "project_name", "project")
            if ns == "project"
            else ("state", "updated_at")
        )
        meta = store.get_fields_multi(keys, fields)

        project_names: set[str] = set()
        for key, data in zip(keys, meta):
            data = data or {}
            state = data.get("state", "active")
            if state in state_counts:
                state_counts[state] += 1
            if ns == "project":
                name = data.get("project_name") or data.get("project") or key.split(":")[-1]
                project_names.add(name)
            candidates.append({
                "key": key,
                "namespace": ns,
                "state": state,
                "updated_at": float(data.get("updated_at", "0")),
            })

        ns_stats[ns] = {"total": len(keys), "states": state_counts}
        if ns == "project":
            # Distinct projects, deduplicated by resolved name (matches the
            # /projects page and list_projects).
            ns_stats[ns]["distinct"] = len(project_names)
        total += len(keys)

    # Rank across all namespaces, keep the 10 most recent, then hydrate only
    # those with content/project (a couple of extra small reads, not thousands).
    candidates.sort(key=lambda x: x["updated_at"], reverse=True)
    recent = candidates[:10]
    if recent:
        detail = store.get_fields_multi(
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

    return {
        "ns_stats": ns_stats,
        "total": total,
        "recent": recent,
        "computed_at": time.time(),
    }


def _load_cached_stats(store) -> dict | None:
    try:
        raw = store.client.get(_CACHE_KEY)
    except Exception:
        return None
    if not raw:
        return None
    try:
        stats = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return None
    return stats if isinstance(stats, dict) and "ns_stats" in stats else None


def _save_stats(store, stats: dict) -> None:
    try:
        store.client.set(_CACHE_KEY, json.dumps(stats), ex=_stats_ttl())
    except Exception as exc:
        logger.debug("Failed to cache dashboard stats: %s", exc)


def get_dashboard_stats(store, force_refresh: bool = False) -> dict:
    """Cached stats when fresh; recompute (and re-cache) otherwise."""
    ttl = _stats_ttl()
    if ttl > 0 and not force_refresh:
        cached = _load_cached_stats(store)
        if cached is not None:
            return cached
    stats = _compute_stats(store)
    if ttl > 0:
        _save_stats(store, stats)
    return stats


async def dashboard(request: Request) -> HTMLResponse:
    """GET / — overview dashboard with namespace stats and recent memories."""
    force_refresh = request.query_params.get("refresh") == "1"
    stats = get_dashboard_stats(deps.store, force_refresh=force_refresh)

    stats_age = None
    if _stats_ttl() > 0:
        stats_age = max(0, int(time.time() - stats.get("computed_at", time.time())))

    # Health check — live, single cheap commands
    health = {"valkey": False, "model": False}
    try:
        deps.store.client.ping()
        health["valkey"] = True
    except Exception:
        pass
    health["model"] = deps.embedder.is_loaded if deps.embedder else False

    # Enrichment queue status — live
    enrichment_pending = 0
    try:
        enrichment_pending = deps.store.client.llen("queue:enrich")
    except Exception:
        enrichment_pending = -1

    template = request.app.state.templates.get_template("dashboard.html")
    content = template.render(
        request=request,
        ns_stats=stats["ns_stats"],
        total=stats["total"],
        recent=stats["recent"],
        stats_age=stats_age,
        health=health,
        enrichment_pending=enrichment_pending,
        current_page="dashboard",
    )
    return HTMLResponse(content)


routes = [
    Route("/", dashboard),
]
