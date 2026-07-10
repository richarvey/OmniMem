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


def _resolve_project_state(context_state: str | None, member_states: list[str]) -> str:
    """One state per project: the context entry's when it has one (that's what
    bulk transitions stamp), otherwise the most-alive state among its memories."""
    if context_state:
        return context_state
    if "active" in member_states:
        return "active"
    if "deprioritised" in member_states:
        return "deprioritised"
    return "archived"


def _compute_stats(store) -> dict:
    """Scan the keyspace and build namespace/state counts + the recent list."""
    ns_stats = {}
    total = 0
    candidates: list[dict] = []

    for ns in ("episodic", "project", "knowledge", "preference"):
        keys = store.scan_prefix(f"mem:{ns}:")
        if not keys:
            ns_stats[ns] = {"total": 0, "states": {"active": 0, "deprioritised": 0, "archived": 0}}
            if ns == "project":
                ns_stats[ns]["distinct"] = 0
                ns_stats[ns]["projects"] = {"active": 0, "deprioritised": 0, "archived": 0}
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

        # name -> {"context_state": ..., "member_states": [...]} for the
        # per-project state rollup on the Projects card.
        project_map: dict[str, dict] = {}
        for key, data in zip(keys, meta):
            data = data or {}
            state = data.get("state", "active")
            if state in state_counts:
                state_counts[state] += 1
            if ns == "project":
                name = data.get("project_name") or data.get("project") or key.split(":")[-1]
                entry = project_map.setdefault(name, {"context_state": None, "member_states": []})
                # Context entries live at mem:project:{name}; everything else
                # under this prefix is a ULID project memory.
                if key == f"mem:project:{name}":
                    entry["context_state"] = state
                else:
                    entry["member_states"].append(state)
            candidates.append({
                "key": key,
                "namespace": ns,
                "state": state,
                "updated_at": float(data.get("updated_at", "0")),
            })

        ns_stats[ns] = {"total": len(keys), "states": state_counts}
        if ns == "project":
            # Distinct projects, deduplicated by resolved name (matches the
            # /projects page and list_projects), broken down by project state.
            project_states = {"active": 0, "deprioritised": 0, "archived": 0}
            for entry in project_map.values():
                resolved = _resolve_project_state(
                    entry["context_state"], entry["member_states"]
                )
                if resolved in project_states:
                    project_states[resolved] += 1
            ns_stats[ns]["distinct"] = len(project_map)
            ns_stats[ns]["projects"] = project_states
        total += len(keys)

    # Compiled skills (v6) — counted separately from the memory total: they are
    # build output derived from memories, not memories themselves.
    skills = {
        "total": 0,
        "states": {"active": 0, "deprioritised": 0, "archived": 0},
        "proposals": 0,
    }
    skill_keys = store.scan_prefix("mem:skill:")
    if skill_keys:
        skills["total"] = len(skill_keys)
        skill_meta = store.get_fields_multi(skill_keys, ("state", "updated_at"))
        for key, data in zip(skill_keys, skill_meta):
            data = data or {}
            state = data.get("state", "active")
            if state in skills["states"]:
                skills["states"][state] += 1
            candidates.append({
                "key": key,
                "namespace": "skill",
                "state": state,
                "updated_at": float(data.get("updated_at", "0")),
            })
    # Proposals are TTL'd hashes, so anything found is still awaiting review.
    skills["proposals"] = len(store.scan_prefix("meta:skill:proposal:"))

    # Rank across all namespaces, keep the 10 most recent, then hydrate only
    # those with content/project (a couple of extra small reads, not thousands).
    candidates.sort(key=lambda x: x["updated_at"], reverse=True)
    recent = candidates[:10]
    if recent:
        detail = store.get_fields_multi(
            [m["key"] for m in recent], ("content", "project", "name", "description")
        )
        for mem, data in zip(recent, detail):
            data = data or {}
            if mem["namespace"] == "skill":
                # Skills carry no content field — show their discovery metadata.
                label = " — ".join(
                    part for part in (data.get("name"), data.get("description")) if part
                )
                mem["content"] = label[:100]
            else:
                mem["content"] = (data.get("content") or "")[:100]
            mem["project"] = data.get("project", "")
            ts = mem["updated_at"]
            mem["updated_at_fmt"] = (
                time.strftime("%Y-%m-%d %H:%M", time.localtime(ts)) if ts > 0 else "—"
            )

    return {
        "ns_stats": ns_stats,
        "total": total,
        "skills": skills,
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
    # Requiring "skills" too invalidates cache entries written by older
    # versions whose payload predates the skills/projects card data.
    if not isinstance(stats, dict) or "ns_stats" not in stats or "skills" not in stats:
        return None
    return stats


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
        skills=stats["skills"],
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
