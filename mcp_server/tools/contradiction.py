"""Contradiction detection MCP tool."""

import logging
from typing import Any

from memory.contradiction import (
    check_contradiction_api,
    check_contradiction_heuristic,
    link_contradiction,
)

logger = logging.getLogger(__name__)


def _get_deps():
    from tools import _store, _embedder, _pipeline
    return _store, _embedder, _pipeline


def check_contradictions(
    query: str | None = None,
    namespace: str = "episodic",
    project_filter: str | None = None,
    use_api: bool = False,
) -> dict[str, Any]:
    """Scan for contradictions. Tier 1 (default): fast heuristic. Tier 2 (use_api=True): Claude API verification.

    Args:
        query: Focus the search. If None, scans recent memories.
        namespace: 'episodic' (default), 'project', or 'knowledge'.
        project_filter: Restrict to a project.
        use_api: Use Claude API for deeper analysis.
    """
    store, embedder, pipeline = _get_deps()

    contradictions: list[dict[str, Any]] = []

    if query:
        # Search for memories related to the query
        vector = embedder.embed(query)
        results = store.search(namespace, vector, top_k=20)
    else:
        # Scan recent memories
        prefix = f"mem:{namespace}:"
        keys = store.scan_prefix(prefix)
        if not keys:
            return {"contradictions": []}
        # Limit scan size
        keys = keys[:200]
        all_data = store.get_multi(keys)
        results = []
        for key, data in zip(keys, all_data):
            if data is None:
                continue
            if data.get("state") in ("archived", "deleted"):
                continue
            if project_filter:
                doc_project = data.get("project") or data.get("project_name")
                if doc_project != project_filter:
                    continue
            data["key"] = key
            results.append(data)

    if not results:
        return {"contradictions": []}

    # Pairwise comparison of results
    seen_pairs: set[str] = set()

    for i, doc_a in enumerate(results):
        key_a = doc_a.get("key", "")
        content_a = doc_a.get("content", "")
        if not content_a:
            continue

        for j, doc_b in enumerate(results):
            if j <= i:
                continue
            key_b = doc_b.get("key", "")
            content_b = doc_b.get("content", "")
            if not content_b:
                continue

            pair_key = f"{min(key_a, key_b)}:{max(key_a, key_b)}"
            if pair_key in seen_pairs:
                continue
            seen_pairs.add(pair_key)

            # Tier 1: Heuristic check
            from memory.contradiction import _has_negation_pair
            if not _has_negation_pair(content_a, content_b):
                continue

            entry: dict[str, Any] = {
                "key_a": key_a,
                "key_b": key_b,
                "content_a": content_a[:80],
                "content_b": content_b[:80],
                "method": "heuristic",
            }

            # Tier 2: API verification if requested
            if use_api:
                api_result = check_contradiction_api(content_a, content_b)
                if api_result.get("is_contradiction"):
                    entry["method"] = "api_confirmed"
                    entry["confidence"] = api_result.get("confidence", 0.0)
                    entry["explanation"] = api_result.get("explanation", "")
                else:
                    # API says not a contradiction — skip
                    continue

            # Cross-link the contradiction
            if key_a and key_b:
                explanation = entry.get("explanation", "Opposing language patterns detected.")
                link_contradiction(store, key_a, key_b, explanation)

            contradictions.append(entry)

    return {"contradictions": contradictions}
