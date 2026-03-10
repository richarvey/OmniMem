"""Semantic deduplication for memories."""

import logging
import os
from dataclasses import dataclass
from typing import Any

import numpy as np

from .embedder import Embedder
from .store import ValkeyStore

logger = logging.getLogger(__name__)


@dataclass
class DuplicateMatch:
    """A memory that is semantically near-identical to a candidate."""

    key: str
    content: str
    similarity: float
    project: str | None
    state: str
    created_at: str


def check_duplicate(
    store: ValkeyStore,
    namespace: str,
    vector: np.ndarray,
    content: str,
    threshold: float | None = None,
    project_filter: str | None = None,
) -> DuplicateMatch | None:
    """Search namespace for a near-identical memory.

    Args:
        store: ValkeyStore instance.
        namespace: Namespace to search within.
        vector: Pre-computed embedding vector (reused from remember()).
        content: The new content text (for comparison logging).
        threshold: Similarity threshold (default from DEDUP_SIMILARITY_THRESHOLD env).
        project_filter: Optional project scope.

    Returns:
        The best matching duplicate above threshold, or None.
    """
    if threshold is None:
        threshold = float(os.getenv("DEDUP_SIMILARITY_THRESHOLD", "0.92"))

    results = store.search(namespace, vector, top_k=5)
    if not results:
        return None

    for doc in results:
        # Skip archived/deleted
        state = doc.get("state", "active")
        if state in ("archived", "deleted"):
            continue

        # Project filter
        if project_filter:
            doc_project = doc.get("project") or doc.get("project_name")
            if doc_project != project_filter:
                continue

        # Convert cosine distance to similarity
        similarity = 1.0 - float(doc.get("similarity_score", "1.0"))
        if similarity >= threshold:
            return DuplicateMatch(
                key=doc.get("key", ""),
                content=doc.get("content", ""),
                similarity=similarity,
                project=doc.get("project") or doc.get("project_name"),
                state=state,
                created_at=doc.get("created_at", ""),
            )

    return None


def find_all_duplicates(
    store: ValkeyStore,
    embedder: Embedder,
    namespace: str = "episodic",
    threshold: float | None = None,
    project_filter: str | None = None,
    max_keys: int = 2000,
) -> list[list[dict[str, Any]]]:
    """Scan all memories in a namespace and return clusters of duplicates.

    Re-embeds all content and performs pairwise comparison.
    Capped at max_keys to prevent excessive runtime.

    Args:
        store: ValkeyStore instance.
        embedder: Embedder instance.
        namespace: Namespace to scan.
        threshold: Similarity threshold (default from env).
        project_filter: Optional project filter.
        max_keys: Maximum keys to scan.

    Returns:
        List of duplicate clusters. Each cluster is a list of memory dicts.
    """
    if threshold is None:
        threshold = float(os.getenv("DEDUP_SIMILARITY_THRESHOLD", "0.92"))

    prefix = f"mem:{namespace}:"
    keys = store.scan_prefix(prefix)
    if not keys:
        return []

    if len(keys) > max_keys:
        logger.warning(
            "Dedup scan capped at %d keys (total: %d)", max_keys, len(keys)
        )
        keys = keys[:max_keys]

    all_data = store.get_multi(keys)

    # Filter and collect content for embedding
    valid_entries: list[tuple[str, dict[str, Any]]] = []
    for key, data in zip(keys, all_data):
        if data is None:
            continue
        if data.get("state") in ("archived", "deleted"):
            continue
        if project_filter:
            doc_project = data.get("project") or data.get("project_name")
            if doc_project != project_filter:
                continue
        valid_entries.append((key, data))

    if len(valid_entries) < 2:
        return []

    # Batch embed all content
    texts = [data.get("content", "") for _, data in valid_entries]
    vectors = embedder.embed_batch(texts)
    vectors_array = np.array(vectors)

    # Pairwise cosine similarity (vectors are normalised, so dot product = cosine)
    similarity_matrix = vectors_array @ vectors_array.T

    # Union-find for clustering
    n = len(valid_entries)
    parent = list(range(n))

    def find(x: int) -> int:
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a: int, b: int) -> None:
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[ra] = rb

    for i in range(n):
        for j in range(i + 1, n):
            if similarity_matrix[i, j] >= threshold:
                union(i, j)

    # Group into clusters
    clusters_map: dict[int, list[int]] = {}
    for i in range(n):
        root = find(i)
        clusters_map.setdefault(root, []).append(i)

    # Only return clusters with 2+ members
    clusters: list[list[dict[str, Any]]] = []
    for indices in clusters_map.values():
        if len(indices) < 2:
            continue
        cluster = []
        for idx in indices:
            key, data = valid_entries[idx]
            cluster.append({
                "key": key,
                "content": data.get("content", "")[:200],
                "project": data.get("project") or data.get("project_name"),
                "state": data.get("state", "active"),
                "created_at": data.get("created_at"),
            })
        clusters.append(cluster)

    # Sort clusters by size descending
    clusters.sort(key=len, reverse=True)
    return clusters
