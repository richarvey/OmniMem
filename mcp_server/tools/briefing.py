"""Briefing tool: single-call session start that aggregates all relevant context."""

import json
import logging
import os
import time
from typing import Any

logger = logging.getLogger(__name__)


def _get_deps():
    from tools import _store, _embedder, _lifecycle, _pipeline
    return _store, _embedder, _lifecycle, _pipeline


def _get_stale_memories(
    store,
    stale_days: int,
    project_filter: str | None = None,
) -> list[dict[str, Any]]:
    """Find active memories that haven't been updated in stale_days."""
    now = time.time()
    cutoff = now - (stale_days * 86400)
    stale: list[dict[str, Any]] = []

    keys = store.scan_prefix("mem:episodic:")
    if not keys:
        return stale

    all_data = store.get_multi(keys)
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if data.get("state") != "active":
            continue
        if project_filter:
            doc_project = data.get("project") or data.get("project_name")
            if doc_project != project_filter:
                continue

        updated_at = float(data.get("updated_at", "0"))
        if updated_at < cutoff:
            age_days = int((now - updated_at) / 86400)
            stale.append({
                "key": key,
                "content": data.get("content", "")[:100],
                "days_since_update": age_days,
                "project": data.get("project"),
            })

    stale.sort(key=lambda x: x["days_since_update"], reverse=True)
    return stale[:10]


def _get_new_knowledge(store, since_days: int = 7) -> list[dict[str, Any]]:
    """Find knowledge articles ingested in the last since_days."""
    now = time.time()
    cutoff = now - (since_days * 86400)
    new_articles: list[dict[str, Any]] = []

    keys = store.scan_prefix("mem:knowledge:")
    if not keys:
        return new_articles

    all_data = store.get_multi(keys)
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if data.get("state") != "active":
            continue

        created_at = float(data.get("created_at", "0"))
        if created_at >= cutoff:
            new_articles.append({
                "key": key,
                "content": data.get("content", "")[:150],
                "source_url": data.get("source_url"),
                "feed_name": data.get("feed_name"),
            })

    return new_articles[:10]


def _get_reinstate_candidates(
    store,
    lifecycle,
    project_filter: str | None = None,
) -> list[dict[str, Any]]:
    """Find deprioritised memories that might warrant reinstatement."""
    candidates: list[dict[str, Any]] = []

    keys = store.scan_prefix("mem:episodic:")
    if not keys:
        return candidates

    all_data = store.get_multi(keys)
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if data.get("state") != "deprioritised":
            continue
        if project_filter:
            doc_project = data.get("project") or data.get("project_name")
            if doc_project != project_filter:
                continue

        hints_raw = data.get("reinstate_hints", "[]")
        try:
            hints = json.loads(hints_raw)
        except (json.JSONDecodeError, TypeError):
            hints = []

        if hints:
            candidates.append({
                "key": key,
                "content": data.get("content", "")[:100],
                "reason": data.get("deprioritised_reason", ""),
                "reinstate_hints": hints,
                "project": data.get("project"),
            })

    return candidates[:5]


def _get_contradiction_warnings(
    store,
    project_filter: str | None = None,
) -> list[dict[str, Any]]:
    """Find active memories with unresolved contradictions."""
    warnings: list[dict[str, Any]] = []

    keys = store.scan_prefix("mem:episodic:")
    if not keys:
        return warnings

    all_data = store.get_multi(keys)
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if data.get("state") != "active":
            continue
        if project_filter:
            doc_project = data.get("project") or data.get("project_name")
            if doc_project != project_filter:
                continue

        contradictions_raw = data.get("contradictions", "[]")
        try:
            contradictions = json.loads(contradictions_raw)
        except (json.JSONDecodeError, TypeError):
            contradictions = []

        if contradictions:
            warnings.append({
                "key": key,
                "content": data.get("content", "")[:100],
                "contradiction_count": len(contradictions),
                "contradicts": [c.get("key", "") for c in contradictions if isinstance(c, dict)],
            })

    return warnings[:5]


def briefing(
    project: str | None = None,
    include_knowledge: bool = True,
) -> dict[str, Any]:
    """Start-of-session briefing. Aggregates project context, experience summary, stale memories, new knowledge, contradictions, and reinstate candidates into a single call.

    Call this at the start of every session instead of making multiple separate calls.
    It replaces the previous 3-step session start (get_project_context + experience_summary + recall).

    Args:
        project: Project name to focus the briefing on. Highly recommended.
        include_knowledge: Whether to include recent knowledge articles (default True).

    Returns:
        Comprehensive briefing with all session-start context in one response.
    """
    store, embedder, lifecycle, pipeline = _get_deps()

    stale_days = int(os.getenv("STALE_MEMORY_DAYS", "30"))
    result: dict[str, Any] = {"status": "complete"}

    # 1. Project context
    if project:
        project_key = f"mem:project:{project}"
        project_data = store.get(project_key)
        if project_data:
            result["project_context"] = {
                "name": project,
                "current_state": project_data.get("content", ""),
                "stack": project_data.get("stack"),
                "updated_at": project_data.get("updated_at"),
            }
        else:
            result["project_context"] = {
                "name": project,
                "note": "No project context found. Use set_project_context() to create one.",
            }

    # 2. Experience summary
    from .experience import experience_summary as _experience_summary
    result["experience_summary"] = _experience_summary(project=project)

    # 3. Stale memories
    stale = _get_stale_memories(store, stale_days, project_filter=project)
    if stale:
        result["stale_memories"] = {
            "count": len(stale),
            "note": f"These memories haven't been updated in {stale_days}+ days. Consider reviewing or archiving.",
            "entries": stale,
        }

    # 4. New knowledge articles
    if include_knowledge:
        new_knowledge = _get_new_knowledge(store)
        if new_knowledge:
            result["new_knowledge"] = {
                "count": len(new_knowledge),
                "note": "Knowledge articles ingested in the last 7 days.",
                "entries": new_knowledge,
            }

    # 5. Contradiction warnings
    contradiction_warnings = _get_contradiction_warnings(store, project_filter=project)
    if contradiction_warnings:
        result["contradiction_warnings"] = {
            "count": len(contradiction_warnings),
            "note": "These memories have known contradictions that may need resolution.",
            "entries": contradiction_warnings,
        }

    # 6. Reinstate candidates
    reinstate_candidates = _get_reinstate_candidates(store, lifecycle, project_filter=project)
    if reinstate_candidates:
        result["reinstate_candidates"] = {
            "count": len(reinstate_candidates),
            "note": "These deprioritised memories have reinstate hints — consider if they're relevant again.",
            "entries": reinstate_candidates,
        }

    # 7. Suppressed topics
    suppressed = lifecycle.get_suppressed_topics()
    if suppressed:
        result["suppressed_topics"] = suppressed

    return result
