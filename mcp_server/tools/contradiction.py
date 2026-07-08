"""Contradiction detection MCP tool."""

import logging
from typing import Any

import numpy as np

from memory.contradiction import (
    _has_negation_pair,
    check_contradiction_api,
    link_contradiction,
)
from memory.maintenance import (
    _CONTRADICTION_COMPARISON_CAP,
    _CONTRADICTION_RESULTS_CAP,
    _CONTRADICTION_SIMILARITY_THRESHOLD,
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
        # Search for memories related to the query. Filter out dead memories
        # and out-of-project docs — the raw search doesn't, and this path
        # cross-links whatever it flags.
        vector = embedder.embed(query)
        results = []
        for doc in store.search(namespace, vector, top_k=20):
            if doc.get("state") in ("archived", "deleted"):
                continue
            if project_filter:
                doc_project = doc.get("project") or doc.get("project_name")
                if doc_project != project_filter:
                    continue
            results.append(doc)
    else:
        # Scan recent memories
        prefix = f"mem:{namespace}:"
        keys = store.scan_prefix(prefix)
        if not keys:
            return {"contradictions": []}
        # Limit scan size
        keys = keys[:200]
        all_data = store.get_fields_multi(
            keys, ("state", "project", "project_name", "content")
        )
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

    # Gate pairwise checks on semantic similarity, mirroring the auto-
    # maintenance scanner (3.12.1 fix): without it, unrelated memories that
    # merely share common words ("use", "with", "works") get flagged AND
    # cross-linked as contradictions. Stored vectors are reused; only entries
    # missing a vector are re-embedded.
    result_keys = [r.get("key", "") for r in results]
    vectors = store.get_vectors_multi(result_keys)
    missing = [i for i, v in enumerate(vectors) if v is None]
    if missing:
        fallback = embedder.embed_batch(
            [results[i].get("content", "") for i in missing]
        )
        for i, vec in zip(missing, fallback):
            vectors[i] = vec
    similarity_matrix = np.array(vectors) @ np.array(vectors).T

    # Pairwise comparison of results, capped like the maintenance scanner
    seen_pairs: set[str] = set()
    comparisons = 0

    for i, doc_a in enumerate(results):
        if comparisons >= _CONTRADICTION_COMPARISON_CAP:
            break
        if len(contradictions) >= _CONTRADICTION_RESULTS_CAP:
            break
        key_a = doc_a.get("key", "")
        content_a = doc_a.get("content", "")
        if not content_a:
            continue

        for j, doc_b in enumerate(results):
            if j <= i:
                continue
            if comparisons >= _CONTRADICTION_COMPARISON_CAP:
                break
            if len(contradictions) >= _CONTRADICTION_RESULTS_CAP:
                break
            key_b = doc_b.get("key", "")
            content_b = doc_b.get("content", "")
            if not content_b:
                continue

            pair_key = f"{min(key_a, key_b)}:{max(key_a, key_b)}"
            if pair_key in seen_pairs:
                continue
            seen_pairs.add(pair_key)
            comparisons += 1

            # Skip pairs that aren't semantically similar
            if float(similarity_matrix[i, j]) < _CONTRADICTION_SIMILARITY_THRESHOLD:
                continue

            # Tier 1: Heuristic check
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
