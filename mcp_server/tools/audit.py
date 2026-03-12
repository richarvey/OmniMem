"""Audit MCP tools: memory_audit, why_did_you_mention, explain_memory."""

import json
import logging
import time
from typing import Any

logger = logging.getLogger(__name__)

_VALID_NAMESPACES = {"episodic", "project", "knowledge"}


def _get_deps():
    from tools import _store, _embedder
    return _store, _embedder


def memory_audit(
    project: str | None = None,
    namespace: str | None = None,
    include_archived: bool = False,
) -> dict[str, Any]:
    """Return a summary of all memories grouped by state. Useful for understanding what's stored and its lifecycle status.

    Args:
        project: Optional project name to filter by.
        namespace: Optional namespace to filter ('episodic', 'project', 'knowledge').
        include_archived: Whether to include archived memories (default False).

    Returns:
        Summary with counts by state and a list of memory entries.
    """
    store, _ = _get_deps()

    prefixes = []
    if namespace:
        if namespace not in _VALID_NAMESPACES:
            raise ValueError(
                f"Invalid namespace '{namespace}'. "
                f"Must be one of: {', '.join(sorted(_VALID_NAMESPACES))}"
            )
        prefixes.append(f"mem:{namespace}:")
    else:
        prefixes.extend(["mem:episodic:", "mem:project:", "mem:knowledge:"])

    entries: list[dict[str, Any]] = []
    state_counts: dict[str, int] = {
        "active": 0,
        "deprioritised": 0,
        "archived": 0,
        "deleted": 0,
    }

    for prefix in prefixes:
        keys = store.scan_prefix(prefix)
        if not keys:
            continue

        # Batch fetch all data for this prefix in one pipeline round-trip
        all_data = store.get_multi(keys)

        for key, data in zip(keys, all_data):
            if data is None:
                continue

            state = data.get("state", "active")

            if state == "archived" and not include_archived:
                state_counts["archived"] = state_counts.get("archived", 0) + 1
                continue

            if project:
                mem_project = data.get("project") or data.get("project_name")
                if mem_project != project:
                    continue

            state_counts[state] = state_counts.get(state, 0) + 1

            effort_raw = data.get("effort_score")
            effort = None
            if effort_raw is not None:
                try:
                    effort = int(float(effort_raw))
                except (ValueError, TypeError):
                    pass

            entries.append({
                "key": key,
                "content": data.get("content", "")[:100],
                "state": state,
                "surface_score": data.get("surface_score"),
                "effort_score": effort,
                "outcome": data.get("outcome"),
                "created_at": data.get("created_at"),
                "updated_at": data.get("updated_at"),
                "project": data.get("project") or data.get("project_name"),
            })

    return {
        "summary": state_counts,
        "total": sum(state_counts.values()),
        "entries": entries,
    }


def why_did_you_mention(query: str) -> dict[str, Any]:
    """Explain why a particular topic was surfaced by searching recent recall logs. Helps humans understand why Claude mentioned something.

    Args:
        query: The topic or phrase you want to understand why it was mentioned.

    Returns:
        The most recent matching recall log entry with query text, timestamp, and results returned at the time.
    """
    store, embedder = _get_deps()

    log_keys = store.scan_prefix("log:recall:")
    log_keys.sort(reverse=True)
    log_keys = log_keys[:50]

    if not log_keys:
        return {
            "status": "not_found",
            "message": "No recent recall log matches this query. The topic may have been mentioned for other reasons.",
        }

    # Batch fetch all log entries in one pipeline round-trip
    all_data = store.get_multi(log_keys)

    query_lower = query.lower()

    # First pass: check for keyword matches (no embedding needed)
    non_keyword_entries: list[tuple[str, dict[str, Any]]] = []
    for log_key, data in zip(log_keys, all_data):
        if data is None:
            continue

        log_query = data.get("query", "")
        if query_lower in log_query.lower() or log_query.lower() in query_lower:
            return {
                "status": "found",
                "match_type": "keyword",
                "log_query": log_query,
                "timestamp": data.get("timestamp"),
                "result_keys": _safe_json_loads(data.get("result_keys", "[]")),
                "result_scores": _safe_json_loads(data.get("result_scores", "[]")),
            }
        non_keyword_entries.append((log_key, data))

    # Second pass: batch embed all log queries for semantic matching
    if not non_keyword_entries:
        return {
            "status": "not_found",
            "message": "No recent recall log matches this query. The topic may have been mentioned for other reasons.",
        }

    query_vector = embedder.embed(query)
    log_queries = [data.get("query", "") for _, data in non_keyword_entries]
    log_vectors = embedder.embed_batch(log_queries)

    best_match: dict[str, Any] | None = None
    best_similarity = 0.0

    for (log_key, data), log_vector in zip(non_keyword_entries, log_vectors):
        similarity = float(query_vector @ log_vector)
        if similarity > best_similarity:
            best_similarity = similarity
            best_match = {
                "status": "found",
                "match_type": "semantic",
                "similarity": round(similarity, 4),
                "log_query": data.get("query", ""),
                "timestamp": data.get("timestamp"),
                "result_keys": _safe_json_loads(data.get("result_keys", "[]")),
                "result_scores": _safe_json_loads(data.get("result_scores", "[]")),
            }

    if best_match and best_similarity > 0.5:
        return best_match

    return {
        "status": "not_found",
        "message": "No recent recall log matches this query. The topic may have been mentioned for other reasons.",
    }


def explain_memory(key: str) -> dict[str, Any]:
    """Return full metadata for a single memory key. Shows all fields, state, experience data, and lifecycle details.

    Args:
        key: The full memory key (e.g. 'mem:episodic:01ARZ3NDEKTSV4RRFFQ69G5FAV').

    Returns:
        Complete memory data including all experience fields, or not_found status.
    """
    store, _ = _get_deps()

    # Validate key starts with an expected prefix
    if not key.startswith(("mem:", "log:recall:")):
        raise ValueError("Key must start with 'mem:' or 'log:recall:' prefix")

    data = store.get(key)
    if data is None:
        return {"status": "not_found"}

    effort_raw = data.get("effort_score")
    effort = None
    if effort_raw is not None:
        try:
            effort = int(float(effort_raw))
        except (ValueError, TypeError):
            pass

    result: dict[str, Any] = {
        "status": "found",
        "key": key,
        "content": data.get("content"),
        "state": data.get("state", "active"),
        "surface_score": data.get("surface_score"),
        "created_at": data.get("created_at"),
        "updated_at": data.get("updated_at"),
        "project": data.get("project") or data.get("project_name"),
        "tags": _safe_json_loads(data.get("tags", "[]")),
        "deprioritised_reason": data.get("deprioritised_reason") or None,
        "reinstate_hints": _safe_json_loads(data.get("reinstate_hints", "[]")),
        "effort_score": effort,
        "outcome": data.get("outcome"),
        "iterations": data.get("iterations"),
        "experience_weight": data.get("experience_weight"),
        "abandoned_approaches": _safe_json_loads(data.get("abandoned_approaches", "[]")),
        "breakthrough": data.get("breakthrough"),
        "gotchas": data.get("gotchas"),
        "contradictions": _safe_json_loads(data.get("contradictions", "[]")),
    }

    # Additional context fields for knowledge namespace
    if data.get("source_url"):
        result["source_url"] = data["source_url"]
    if data.get("feed_name"):
        result["feed_name"] = data["feed_name"]
    if data.get("published_at"):
        result["published_at"] = data["published_at"]

    return result


def _safe_json_loads(raw: str | None) -> Any:
    """Safely parse JSON, returning empty list on failure."""
    if not raw:
        return []
    try:
        return json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return []
