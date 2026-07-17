"""Briefing tool: single-call session start that aggregates all relevant context."""

import json
import logging
import os
import time
from typing import Any

from . import _compact

logger = logging.getLogger(__name__)


def _get_deps():
    from tools import _store, _embedder, _lifecycle, _pipeline
    return _store, _embedder, _lifecycle, _pipeline


def _scan_episodic_once(
    store,
    stale_days: int,
    project_filter: str | None = None,
) -> dict[str, list[dict[str, Any]]]:
    """Single pass over episodic memories for stale, reinstate, and contradiction data."""
    now = time.time()
    stale_cutoff = now - (stale_days * 86400)

    stale: list[dict[str, Any]] = []
    reinstate_candidates: list[dict[str, Any]] = []
    contradiction_warnings: list[dict[str, Any]] = []

    keys = store.scan_prefix("mem:episodic:")
    if not keys:
        return {"stale": [], "reinstate": [], "contradictions": []}

    # Session-start hot path — fetch only the fields this scan reads.
    all_data = store.get_fields_multi(
        keys,
        ("state", "project", "project_name", "updated_at", "content",
         "contradictions", "reinstate_hints", "deprioritised_reason"),
    )
    for key, data in zip(keys, all_data):
        if data is None:
            continue

        state = data.get("state")
        if project_filter:
            doc_project = data.get("project") or data.get("project_name")
            if doc_project != project_filter:
                continue

        # Stale: active memories not updated recently
        if state == "active":
            updated_at = float(data.get("updated_at", "0"))
            if updated_at < stale_cutoff:
                age_days = int((now - updated_at) / 86400)
                stale.append({
                    "key": key,
                    "content": data.get("content", "")[:80],
                    "days_stale": age_days,
                })

            # Contradictions: active memories with unresolved contradictions
            contradictions_raw = data.get("contradictions", "[]")
            try:
                contradictions = json.loads(contradictions_raw)
            except (json.JSONDecodeError, TypeError):
                contradictions = []
            if contradictions:
                contradiction_warnings.append({
                    "key": key,
                    "content": data.get("content", "")[:80],
                    "contradicts": [c.get("key", "") for c in contradictions if isinstance(c, dict)],
                })

        # Reinstate: deprioritised memories with reinstate hints
        elif state == "deprioritised":
            hints_raw = data.get("reinstate_hints", "[]")
            try:
                hints = json.loads(hints_raw)
            except (json.JSONDecodeError, TypeError):
                hints = []
            if hints:
                reinstate_candidates.append({
                    "key": key,
                    "content": data.get("content", "")[:80],
                    "reason": data.get("deprioritised_reason", ""),
                    "reinstate_hints": hints,
                })

    stale.sort(key=lambda x: x["days_stale"], reverse=True)

    return {
        "stale": stale[:10],
        "reinstate": reinstate_candidates[:5],
        "contradictions": contradiction_warnings[:5],
    }


def _get_stale_memories(
    store,
    stale_days: int,
    project_filter: str | None = None,
) -> list[dict[str, Any]]:
    """Thin wrapper for backwards compatibility with tests."""
    return _scan_episodic_once(store, stale_days, project_filter)["stale"]


def _get_reinstate_candidates(
    store,
    lifecycle,
    project_filter: str | None = None,
) -> list[dict[str, Any]]:
    """Thin wrapper for backwards compatibility with tests."""
    return _scan_episodic_once(store, 30, project_filter)["reinstate"]


def _get_contradiction_warnings(
    store,
    project_filter: str | None = None,
) -> list[dict[str, Any]]:
    """Thin wrapper for backwards compatibility with tests."""
    return _scan_episodic_once(store, 30, project_filter)["contradictions"]


def _get_new_knowledge(store, since_days: int = 7) -> list[dict[str, Any]]:
    """Find knowledge articles ingested in the last since_days."""
    now = time.time()
    cutoff = now - (since_days * 86400)
    new_articles: list[dict[str, Any]] = []

    keys = store.scan_prefix("mem:knowledge:")
    if not keys:
        return new_articles

    all_data = store.get_fields_multi(
        keys, ("state", "created_at", "content", "source_url", "feed_name"),
    )
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if data.get("state") != "active":
            continue

        created_at = float(data.get("created_at", "0"))
        if created_at >= cutoff:
            new_articles.append(_compact({
                "key": key,
                "content": data.get("content", "")[:80],
                "source_url": data.get("source_url"),
                "feed_name": data.get("feed_name"),
            }))

    return new_articles[:10]


def briefing(
    project: str | None = None,
    include_knowledge: bool = True,
) -> dict[str, Any]:
    """Session-start briefing: project context, experience summary, stale memories, knowledge, contradictions, reinstate candidates.

    Args:
        project: Project name to focus on.
        include_knowledge: Include recent knowledge articles (default True).
    """
    store, embedder, lifecycle, pipeline = _get_deps()

    stale_days = int(os.getenv("STALE_MEMORY_DAYS", "30"))
    result: dict[str, Any] = {}

    # 1. Project context
    project_data = None
    if project:
        project_key = f"mem:project:{project}"
        project_data = store.get(project_key)
        if project_data:
            result["project_context"] = _compact({
                "name": project,
                "current_state": project_data.get("content", ""),
                "updated_at": project_data.get("updated_at"),
            })
        else:
            result["project_context"] = {"name": project, "note": "not_found"}

    # 2. Experience summary
    from .experience import experience_summary as _experience_summary
    exp = _experience_summary(project=project)
    if exp.get("memories_with_experience", 0) > 0:
        result["experience_summary"] = exp

    # 3-5. Single episodic scan for stale, contradictions, and reinstate
    episodic = _scan_episodic_once(store, stale_days, project_filter=project)

    if episodic["stale"]:
        result["stale_memories"] = episodic["stale"]
    if episodic["contradictions"]:
        result["contradiction_warnings"] = episodic["contradictions"]
    if episodic["reinstate"]:
        result["reinstate_candidates"] = episodic["reinstate"]

    # 6. New knowledge articles
    if include_knowledge:
        new_knowledge = _get_new_knowledge(store)
        if new_knowledge:
            result["new_knowledge"] = new_knowledge

    # 7. Suppressed topics
    suppressed = lifecycle.get_suppressed_topics()
    if suppressed:
        result["suppressed_topics"] = suppressed

    # 8. Auto-maintenance: dedup + contradiction scan on briefing interval
    if project:
        try:
            interval = int(os.getenv("AUTO_MAINTENANCE_INTERVAL", "10"))
            if interval > 0:
                meta_key = f"meta:maintenance:{project}"
                count = store.client.hincrby(meta_key, "briefing_count", 1)
                if count >= interval:
                    from memory.maintenance import run_maintenance
                    maintenance_result = run_maintenance(
                        store, embedder, lifecycle, project,
                    )
                    result["auto_maintenance"] = maintenance_result
                    store.client.hset(meta_key, mapping={
                        "briefing_count": "0",
                        "last_maintenance_at": maintenance_result["ran_at"],
                        "last_maintenance_summary": json.dumps({
                            "duplicates_archived": len(maintenance_result["duplicates_archived"]),
                            "contradictions_found": len(maintenance_result["contradictions_found"]),
                        }),
                    })
        except Exception as exc:
            logger.error("Auto-maintenance failed for project %s: %s", project, exc)

    # 9. Compiled skill suggestions and pending update diffs (v6). Suggestions
    # are recommendations only — loading is the agent's and human's call.
    if project:
        try:
            from .skills import (
                knowledge_watch,
                pending_skill_updates,
                suggest_skills_for_briefing,
            )

            greenfield = project_data is None
            context_text = None
            if not greenfield:
                context_text = " ".join(
                    str(project_data.get(f, "")) for f in
                    ("description", "stack", "goals", "current_state")
                ).strip() or project
            suggestions = suggest_skills_for_briefing(
                store, embedder, None if greenfield else context_text,
            )
            if suggestions:
                if greenfield:
                    # No project context to lead with, so the skills move to
                    # the top: they are the only thing carrying the user's
                    # conventions on a greenfield project.
                    result = {
                        "skill_suggestions": {
                            "note": "Greenfield project (no context yet). A "
                                    "compiled skill carries your conventions — "
                                    "pick by description and load with get_skill().",
                            "skills": suggestions,
                        },
                        **result,
                    }
                else:
                    result["skill_suggestions"] = {"skills": suggestions}

            updates = pending_skill_updates(store)
            if updates:
                result["skill_updates"] = updates

            # Fresh knowledge semantically close to a compiled skill —
            # awareness only; promoting an article is a deliberate call.
            watch = knowledge_watch(store)
            if watch:
                result["skill_knowledge_watch"] = watch
        except Exception as exc:
            logger.error("Skill briefing sections failed: %s", exc)

    return result
