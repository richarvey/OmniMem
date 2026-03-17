"""Audit MCP tools: memory_audit, why_did_you_mention, explain_memory."""

import json
import logging
import time
from typing import Any

from . import _compact

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
    """Summary of all memories grouped by state. Useful for cleanup.

    Args:
        project: Filter to a project.
        namespace: Filter to 'episodic', 'project', or 'knowledge'.
        include_archived: Include archived memories (default False).
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

            entries.append(_compact({
                "key": key,
                "content": data.get("content", "")[:100],
                "state": state,
                "effort_score": effort,
                "outcome": data.get("outcome"),
                "project": data.get("project") or data.get("project_name"),
            }))

    return {
        "summary": state_counts,
        "total": sum(state_counts.values()),
        "entries": entries,
    }


def why_did_you_mention(query: str) -> dict[str, Any]:
    """Explain why a topic surfaced by searching recall logs.

    Args:
        query: Topic or phrase to investigate.
    """
    store, embedder = _get_deps()

    log_keys = store.scan_prefix("log:recall:")
    log_keys.sort(reverse=True)
    log_keys = log_keys[:50]

    if not log_keys:
        return {"status": "not_found"}

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
            return _compact({
                "status": "found",
                "match_type": "keyword",
                "log_query": log_query,
                "timestamp": data.get("timestamp"),
                "result_keys": _safe_json_loads(data.get("result_keys", "[]")),
            })
        non_keyword_entries.append((log_key, data))

    # Second pass: batch embed all log queries for semantic matching
    if not non_keyword_entries:
        return {"status": "not_found"}

    query_vector = embedder.embed(query)
    log_queries = [data.get("query", "") for _, data in non_keyword_entries]
    log_vectors = embedder.embed_batch(log_queries)

    best_match: dict[str, Any] | None = None
    best_similarity = 0.0

    for (log_key, data), log_vector in zip(non_keyword_entries, log_vectors):
        similarity = float(query_vector @ log_vector)
        if similarity > best_similarity:
            best_similarity = similarity
            best_match = _compact({
                "status": "found",
                "match_type": "semantic",
                "similarity": round(similarity, 4),
                "log_query": data.get("query", ""),
                "timestamp": data.get("timestamp"),
                "result_keys": _safe_json_loads(data.get("result_keys", "[]")),
            })

    if best_match and best_similarity > 0.5:
        return best_match

    return {"status": "not_found"}


def explain_memory(key: str) -> dict[str, Any]:
    """Return full metadata for a memory key.

    Args:
        key: Full memory key (e.g. 'mem:episodic:01ARZ3...').
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
        "project": data.get("project") or data.get("project_name"),
        "tags": _safe_json_loads(data.get("tags", "[]")),
        "created_at": data.get("created_at"),
        "updated_at": data.get("updated_at"),
        "effort_score": effort,
        "outcome": data.get("outcome"),
        "experience_weight": data.get("experience_weight"),
        "abandoned_approaches": _safe_json_loads(data.get("abandoned_approaches", "[]")),
        "breakthrough": data.get("breakthrough"),
        "gotchas": data.get("gotchas"),
        "deprioritised_reason": data.get("deprioritised_reason") or None,
        "reinstate_hints": _safe_json_loads(data.get("reinstate_hints", "[]")),
        "contradictions": _safe_json_loads(data.get("contradictions", "[]")),
    }

    # Additional context fields for knowledge namespace
    if data.get("source_url"):
        result["source_url"] = data["source_url"]
    if data.get("feed_name"):
        result["feed_name"] = data["feed_name"]

    return _compact(result)


def _safe_json_loads(raw: str | None) -> Any:
    """Safely parse JSON, returning empty list on failure."""
    if not raw:
        return []
    try:
        return json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return []
