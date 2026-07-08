"""Knowledge tools: query and manage knowledge namespace items."""

import json
import time
from typing import Any

from . import _compact


def _get_deps():
    from tools import _store
    return _store


def recent_knowledge(
    days: int = 7,
    feed_name: str | None = None,
    topics: list[str] | None = None,
    limit: int = 20,
) -> list[dict[str, Any]]:
    """Recent knowledge articles ingested by the RSS worker.

    Returns knowledge items created within the given lookback window,
    sorted newest first. Optionally filter by feed name or topics.

    Args:
        days: Lookback window in days (default 7, max 365).
        feed_name: Filter to a specific RSS feed name.
        topics: Filter to items tagged with at least one of these topics.
        limit: Maximum results to return (default 20, max 50).
    """
    store = _get_deps()
    now = time.time()
    days = max(1, min(days, 365))
    limit = max(1, min(limit, 50))
    cutoff = now - (days * 86400)

    keys = store.scan_prefix("mem:knowledge:")
    if not keys:
        return []

    all_data = store.get_fields_multi(
        keys,
        ("state", "created_at", "feed_name", "topics", "title", "content",
         "source_url", "published_at", "expires_at"),
    )
    results = []
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if data.get("state") != "active":
            continue
        if float(data.get("created_at", "0")) < cutoff:
            continue
        if feed_name and data.get("feed_name") != feed_name:
            continue
        if topics:
            try:
                item_topics = json.loads(data.get("topics", "[]"))
            except (json.JSONDecodeError, TypeError):
                item_topics = []
            if not any(t in item_topics for t in topics):
                continue
        results.append(_compact({
            "key": key,
            "title": data.get("title"),
            "content": data.get("content"),
            "source_url": data.get("source_url"),
            "feed_name": data.get("feed_name"),
            "published_at": data.get("published_at"),
            "created_at": data.get("created_at"),
            "expires_at": data.get("expires_at"),
            "topics": json.loads(data.get("topics", "[]")) if data.get("topics") else None,
        }))

    results.sort(key=lambda x: float(x.get("created_at") or "0"), reverse=True)
    return results[:limit]


def promote_knowledge(key: str) -> dict[str, Any]:
    """Mark a knowledge item as permanently useful by removing its expiry.

    Clears the expires_at field so the item is never auto-archived by maintenance.
    Use this when an RSS-ingested article turns out to be genuinely valuable.

    Args:
        key: The memory key (e.g. mem:knowledge:01ABC...).
    """
    store = _get_deps()

    if not key.startswith("mem:knowledge:"):
        return {"error": f"Key must be in the knowledge namespace: {key}"}

    data = store.get(key)
    if data is None:
        return {"error": f"Key not found: {key}"}
    if data.get("state") == "archived":
        return {"error": f"Cannot promote archived item: {key}"}

    store.set_field(key, "expires_at", "")
    return {"key": key, "promoted": True}
