"""Automatic maintenance: dedup + contradiction scan triggered by briefing interval."""

import logging
import time
from typing import Any

from .contradiction import _has_negation_pair
from .dedup import find_all_duplicates
from .embedder import Embedder
from .lifecycle import MemoryLifecycle, MemoryState
from .store import ValkeyStore

logger = logging.getLogger(__name__)

# Cap on keys scanned during heuristic contradiction check
_CONTRADICTION_SCAN_CAP = 200


def expire_knowledge_items(
    store: ValkeyStore,
    lifecycle: MemoryLifecycle,
) -> list[str]:
    """Archive RSS-ingested knowledge items whose expires_at has passed.

    Only items with both feed_name and expires_at set are considered.
    Manually stored knowledge items (no expires_at) are never touched.

    Returns:
        List of keys that were archived.
    """
    now = time.time()
    expired: list[str] = []

    keys = store.scan_prefix("mem:knowledge:")
    if not keys:
        return expired

    all_data = store.get_multi(keys)
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if data.get("state") != "active":
            continue
        if not data.get("feed_name"):
            continue
        expires_at_raw = data.get("expires_at")
        if not expires_at_raw:
            continue
        if float(expires_at_raw) <= now:
            try:
                lifecycle.transition(
                    key, MemoryState.ARCHIVED,
                    reason="auto-maintenance: knowledge item expired",
                )
                expired.append(key)
            except (ValueError, KeyError) as exc:
                logger.debug("Skipped archiving %s during knowledge expiry: %s", key, exc)

    return expired


def run_maintenance(
    store: ValkeyStore,
    embedder: Embedder,
    lifecycle: MemoryLifecycle,
    project: str,
    dedup_threshold: float | None = None,
) -> dict[str, Any]:
    """Run automatic maintenance for a project: dedup + heuristic contradiction scan.

    Phase 1: Find duplicate clusters in the episodic namespace, auto-archive
    the oldest entries in each cluster (keeping the newest).

    Phase 2: Heuristic contradiction scan on active project memories using
    negation pattern matching (no API calls). Capped at 200 keys.

    Args:
        store: ValkeyStore instance.
        embedder: Embedder instance.
        lifecycle: MemoryLifecycle instance.
        project: Project name to scope maintenance to.
        dedup_threshold: Optional similarity threshold override.

    Returns:
        Dict with project, ran_at, duplicates_archived, and contradictions_found.
    """
    ran_at = str(time.time())
    duplicates_archived: list[str] = []
    contradictions_found: list[dict[str, Any]] = []

    # Phase 1: Dedup — find clusters and archive oldest in each
    try:
        clusters = find_all_duplicates(
            store, embedder,
            namespace="episodic",
            threshold=dedup_threshold,
            project_filter=project,
        )
        for cluster in clusters:
            # Sort by created_at ascending (oldest first)
            sorted_members = sorted(
                cluster["memories"],
                key=lambda m: float(m.get("created_at") or "0"),
            )
            # Archive all but the newest (last in sorted list)
            for entry in sorted_members[:-1]:
                key = entry["key"]
                try:
                    lifecycle.transition(
                        key, MemoryState.ARCHIVED,
                        reason=f"auto-maintenance: duplicate of {sorted_members[-1]['key']}",
                    )
                    duplicates_archived.append(key)
                except (ValueError, KeyError) as exc:
                    logger.debug("Skipped archiving %s during maintenance: %s", key, exc)
    except Exception as exc:
        logger.error("Maintenance dedup phase failed for project %s: %s", project, exc)

    # Phase 2: Heuristic contradiction scan on active project memories
    try:
        keys = store.scan_prefix("mem:episodic:")
        if keys:
            all_data = store.get_multi(keys)
            active_entries: list[tuple[str, dict[str, Any]]] = []
            for key, data in zip(keys, all_data):
                if data is None:
                    continue
                if data.get("state") != "active":
                    continue
                doc_project = data.get("project") or data.get("project_name")
                if doc_project != project:
                    continue
                active_entries.append((key, data))
                if len(active_entries) >= _CONTRADICTION_SCAN_CAP:
                    break

            # Pairwise heuristic check
            for i in range(len(active_entries)):
                key_a, data_a = active_entries[i]
                content_a = data_a.get("content", "")
                for j in range(i + 1, len(active_entries)):
                    key_b, data_b = active_entries[j]
                    content_b = data_b.get("content", "")
                    if _has_negation_pair(content_a, content_b):
                        contradictions_found.append({
                            "key_a": key_a,
                            "key_b": key_b,
                            "content_a": content_a[:80],
                            "content_b": content_b[:80],
                        })
    except Exception as exc:
        logger.error("Maintenance contradiction phase failed for project %s: %s", project, exc)

    # Phase 3: Expire RSS knowledge items (global, not project-scoped)
    knowledge_expired: list[str] = []
    try:
        knowledge_expired = expire_knowledge_items(store, lifecycle)
    except Exception as exc:
        logger.error("Maintenance knowledge expiry phase failed: %s", exc)

    return {
        "project": project,
        "ran_at": ran_at,
        "duplicates_archived": duplicates_archived,
        "contradictions_found": contradictions_found,
        "knowledge_expired": knowledge_expired,
    }
