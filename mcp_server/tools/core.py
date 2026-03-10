"""Core MCP tools: remember, recall, forget, deprioritise, archive, reinstate, topic suppression."""

import json
import logging
import re
import time
from dataclasses import asdict
from typing import Any

import ulid

from ..memory.dedup import check_duplicate, find_all_duplicates
from ..memory.embedder import Embedder
from ..memory.lifecycle import MemoryLifecycle, MemoryState
from ..memory.recall import RecallPipeline
from ..memory.store import ValkeyStore

logger = logging.getLogger(__name__)

VALID_NAMESPACES = {"episodic", "project", "knowledge"}
MAX_CONTENT_LENGTH = 50_000
MAX_TOP_K = 50
MAX_TAG_LENGTH = 100
MAX_TAGS = 20
# Allowed characters for project names and tags: alphanumeric, hyphens, underscores, dots, spaces
_SAFE_NAME_RE = re.compile(r"^[a-zA-Z0-9_\-. ]+$")


def _validate_namespace(namespace: str) -> None:
    """Validate namespace is one of the allowed values."""
    if namespace not in VALID_NAMESPACES:
        raise ValueError(
            f"Invalid namespace '{namespace}'. Must be one of: {', '.join(sorted(VALID_NAMESPACES))}"
        )


def _validate_project_name(project: str | None) -> None:
    """Validate project name contains only safe characters."""
    if project is None:
        return
    if not project or len(project) > 200:
        raise ValueError("Project name must be 1-200 characters")
    if not _SAFE_NAME_RE.match(project):
        raise ValueError(
            "Project name contains invalid characters. "
            "Only alphanumeric, hyphens, underscores, dots, and spaces are allowed."
        )


def _validate_content(content: str) -> None:
    """Validate content is not empty and within size limits."""
    if not content or not content.strip():
        raise ValueError("Content cannot be empty")
    if len(content) > MAX_CONTENT_LENGTH:
        raise ValueError(f"Content too long ({len(content)} chars). Maximum is {MAX_CONTENT_LENGTH}.")


def _validate_tags(tags: list[str] | None) -> None:
    """Validate tags list."""
    if tags is None:
        return
    if len(tags) > MAX_TAGS:
        raise ValueError(f"Too many tags ({len(tags)}). Maximum is {MAX_TAGS}.")
    for tag in tags:
        if not isinstance(tag, str) or len(tag) > MAX_TAG_LENGTH:
            raise ValueError(f"Each tag must be a string of at most {MAX_TAG_LENGTH} characters")


def _validate_top_k(top_k: int) -> int:
    """Clamp top_k to a safe range."""
    if top_k < 1:
        return 1
    if top_k > MAX_TOP_K:
        return MAX_TOP_K
    return top_k


def _get_deps() -> tuple[ValkeyStore, Embedder, MemoryLifecycle, RecallPipeline]:
    """Get shared dependencies. Set by server.py at startup."""
    from ..tools import _store, _embedder, _lifecycle, _pipeline
    return _store, _embedder, _lifecycle, _pipeline


def remember(
    content: str,
    project: str | None = None,
    tags: list[str] | None = None,
    namespace: str = "episodic",
    force: bool = False,
) -> dict[str, Any]:
    """Store a new memory. Use this to remember decisions, solutions, patterns, or project context.

    Automatically checks for semantic duplicates before storing. If a near-identical
    memory already exists, returns the duplicate details instead of storing. Use force=True
    to store anyway (e.g. when intentionally updating with new context).

    Args:
        content: The text to remember — be specific and descriptive.
        project: Optional project name to associate this memory with.
        tags: Optional list of tags for categorisation.
        namespace: Memory namespace — 'episodic' (default), 'project', or 'knowledge'.
        force: If True, skip duplicate checking and store regardless.

    Returns:
        Dict with key, status, and namespace of the stored memory.
        If a duplicate is found (and force=False), returns status='duplicate_found' with match details.
    """
    store, embedder, _, _ = _get_deps()

    _validate_namespace(namespace)
    _validate_content(content)
    _validate_project_name(project)
    _validate_tags(tags)

    key = f"mem:{namespace}:{ulid.new().str}"
    now = str(time.time())
    vector = embedder.embed(content)

    # Check for semantic duplicates (reuses the embedding we just computed)
    if not force:
        dup = check_duplicate(
            store, namespace, vector, content, project_filter=project
        )
        if dup is not None:
            logger.info(
                "Duplicate detected for new memory (similarity=%.3f, existing=%s)",
                dup.similarity, dup.key,
            )
            return {
                "status": "duplicate_found",
                "message": (
                    f"A near-identical memory already exists (similarity: {dup.similarity:.2%}). "
                    "Call remember() with force=True to store anyway."
                ),
                "existing_key": dup.key,
                "existing_content": dup.content[:200],
                "similarity": round(dup.similarity, 4),
            }

    fields: dict[str, Any] = {
        "content": content,
        "state": MemoryState.ACTIVE.value,
        "surface_score": "1.0",
        "experience_weight": "1.0",
        "created_at": now,
        "updated_at": now,
        "tags": json.dumps(tags or []),
    }
    if project:
        fields["project"] = project

    store.upsert(namespace, key, fields, vector)
    logger.info("Stored memory %s in %s", key, namespace)
    return {"key": key, "status": "stored", "namespace": namespace}


def recall(
    query: str,
    top_k: int = 5,
    namespaces: list[str] | None = None,
    project_filter: str | None = None,
) -> list[dict[str, Any]]:
    """Search memories by semantic similarity. Returns the most relevant memories, knowledge articles, and any warnings about previously abandoned approaches.

    Args:
        query: Natural language description of what you're looking for.
        top_k: Maximum number of results to return (default 5).
        namespaces: List of namespaces to search — 'episodic', 'project', 'knowledge'. Searches all by default.
        project_filter: Optional project name to restrict results to.

    Returns:
        List of results, each with content, score, state, namespace, and result_type.
        Abandoned warnings appear first with result_type='abandoned_warning'.
        Reinstate candidates are flagged with a note.
    """
    _, _, _, pipeline = _get_deps()

    top_k = _validate_top_k(top_k)
    if namespaces:
        for ns in namespaces:
            _validate_namespace(ns)
    if project_filter:
        _validate_project_name(project_filter)

    results = pipeline.recall(
        query=query,
        namespaces=namespaces,
        top_k=top_k,
        project_filter=project_filter,
    )

    output: list[dict[str, Any]] = []
    for r in results:
        entry = asdict(r)
        if r.result_type == "abandoned_warning":
            entry["note"] = (
                "WARNING: The following approach was previously tried and abandoned: "
                f"{r.content}"
            )
        elif r.reinstate_candidate:
            entry["note"] = (
                f"[This memory was deprioritised but may be relevant again: "
                f"{r.deprioritised_reason}]"
            )
        output.append(entry)

    return output


def deprioritise(
    key_or_query: str,
    reason: str,
    reinstate_hints: list[str] | None = None,
) -> dict[str, Any]:
    """Reduce a memory's visibility without deleting it. Use when something should stop surfacing but shouldn't be destroyed.

    Args:
        key_or_query: Either a memory key (e.g. 'mem:episodic:...') or a natural language query to find the memory.
        reason: Why this memory is being deprioritised — stored for future reference.
        reinstate_hints: Optional keywords that, if matched in a future query, will flag this memory as a reinstate candidate.

    Returns:
        Dict with affected keys, their previous states, and any high-effort warnings.
    """
    store, embedder, lifecycle, pipeline = _get_deps()

    affected = []
    if key_or_query.startswith("mem:"):
        result = lifecycle.transition(key_or_query, MemoryState.DEPRIORITISED, reason=reason)
        if reinstate_hints:
            lifecycle.add_reinstate_hints(key_or_query, reinstate_hints)
        affected.append(result)
    else:
        results = pipeline.recall(key_or_query, top_k=3)
        for r in results:
            if r.adjusted_score > 0.85 and r.state == MemoryState.ACTIVE.value:
                result = lifecycle.transition(r.key, MemoryState.DEPRIORITISED, reason=reason)
                if reinstate_hints:
                    lifecycle.add_reinstate_hints(r.key, reinstate_hints)
                affected.append(result)

    return {"affected": affected, "count": len(affected)}


def archive(
    key_or_query: str,
    reason: str | None = None,
) -> dict[str, Any]:
    """Archive a memory — it will no longer appear in recall results but remains stored.

    Args:
        key_or_query: Either a memory key or a natural language query.
        reason: Optional reason for archiving.

    Returns:
        Dict with affected keys.
    """
    _, _, lifecycle, pipeline = _get_deps()

    affected = []
    if key_or_query.startswith("mem:"):
        result = lifecycle.transition(key_or_query, MemoryState.ARCHIVED, reason=reason)
        affected.append(result)
    else:
        results = pipeline.recall(key_or_query, top_k=3)
        for r in results:
            if r.adjusted_score > 0.85:
                try:
                    result = lifecycle.transition(r.key, MemoryState.ARCHIVED, reason=reason)
                    affected.append(result)
                except ValueError as exc:
                    logger.warning("Cannot archive %s: %s", r.key, exc)

    return {"affected": affected, "count": len(affected)}


def reinstate(key_or_query: str) -> dict[str, Any]:
    """Reinstate a deprioritised or archived memory back to active state.

    Args:
        key_or_query: Either a memory key or a natural language query.

    Returns:
        Dict with affected keys.
    """
    store, _, lifecycle, pipeline = _get_deps()

    affected = []
    if key_or_query.startswith("mem:"):
        result = lifecycle.transition(key_or_query, MemoryState.ACTIVE)
        # Single round-trip instead of two set_field calls
        store.set_fields(key_or_query, {"deprioritised_reason": "", "surface_score": "1.0"})
        affected.append(result)
    else:
        results = pipeline.recall(key_or_query, top_k=3)
        for r in results:
            if r.state in (MemoryState.DEPRIORITISED.value, MemoryState.ARCHIVED.value):
                try:
                    result = lifecycle.transition(r.key, MemoryState.ACTIVE)
                    store.set_fields(r.key, {"deprioritised_reason": "", "surface_score": "1.0"})
                    affected.append(result)
                except ValueError as exc:
                    logger.warning("Cannot reinstate %s: %s", r.key, exc)

    return {"affected": affected, "count": len(affected)}


def forget(
    key_or_query: str,
    confirm: bool = False,
) -> dict[str, Any]:
    """Permanently delete a memory. Requires confirm=True to execute.

    Args:
        key_or_query: Either a memory key or a natural language query.
        confirm: Must be True to actually delete. If False, returns a preview of what would be deleted.

    Returns:
        If confirm=False: preview list of keys that would be deleted.
        If confirm=True: list of deleted keys.
    """
    store, _, lifecycle, pipeline = _get_deps()

    targets: list[dict[str, Any]] = []
    if key_or_query.startswith("mem:"):
        data = store.get(key_or_query)
        if data:
            targets.append({"key": key_or_query, "content": data.get("content", "")[:100]})
    else:
        results = pipeline.recall(key_or_query, top_k=3)
        for r in results:
            if r.adjusted_score > 0.85:
                targets.append({"key": r.key, "content": r.content[:100]})

    if not targets:
        return {"status": "not_found", "message": "No matching memories found"}

    if not confirm:
        return {
            "status": "preview",
            "message": "These memories would be deleted. Call again with confirm=True to proceed.",
            "targets": targets,
        }

    deleted = []
    for target in targets:
        try:
            lifecycle.transition(target["key"], MemoryState.DELETED)
            deleted.append(target["key"])
        except ValueError:
            store.delete(target["key"])
            deleted.append(target["key"])

    return {"status": "deleted", "deleted_keys": deleted, "count": len(deleted)}


def suppress_topic(
    topic: str,
    reason: str | None = None,
) -> dict[str, Any]:
    """Suppress a topic — memories containing this topic will be filtered from recall results.

    Args:
        topic: The topic string to suppress (case-insensitive matching).
        reason: Optional reason for suppression.

    Returns:
        Dict with topic and status.
    """
    if not topic or not topic.strip():
        raise ValueError("Topic cannot be empty")
    if len(topic) > 200:
        raise ValueError("Topic too long (max 200 characters)")
    _, _, lifecycle, _ = _get_deps()
    lifecycle.suppress_topic(topic)
    return {"topic": topic, "status": "suppressed", "reason": reason}


def unsuppress_topic(topic: str) -> dict[str, Any]:
    """Remove a topic from the suppression list, allowing related memories to surface again.

    Args:
        topic: The topic string to unsuppress.

    Returns:
        Dict with topic and status.
    """
    _, _, lifecycle, _ = _get_deps()
    lifecycle.unsuppress_topic(topic)
    return {"topic": topic, "status": "active"}


def list_suppressions() -> dict[str, Any]:
    """List all currently suppressed topics.

    Returns:
        Dict with list of suppressed topics.
    """
    _, _, lifecycle, _ = _get_deps()
    topics = lifecycle.get_suppressed_topics()
    return {"suppressed_topics": topics, "count": len(topics)}


def find_duplicates(
    namespace: str = "episodic",
    threshold: float | None = None,
    project_filter: str | None = None,
) -> dict[str, Any]:
    """Scan all memories in a namespace and return clusters of near-identical content.

    Useful for periodic cleanup — identifies memories that say essentially the same thing.
    Each cluster is a group of memories with pairwise similarity above the threshold.

    Args:
        namespace: Namespace to scan — 'episodic' (default), 'project', or 'knowledge'.
        threshold: Similarity threshold (0.0-1.0). Default from DEDUP_SIMILARITY_THRESHOLD env (0.92).
        project_filter: Optional project name to restrict the scan.

    Returns:
        Dict with clusters of duplicate memories and summary stats.
    """
    store, embedder, _, _ = _get_deps()

    _validate_namespace(namespace)
    if project_filter:
        _validate_project_name(project_filter)

    clusters = find_all_duplicates(
        store, embedder, namespace,
        threshold=threshold,
        project_filter=project_filter,
    )

    return {
        "status": "complete",
        "namespace": namespace,
        "cluster_count": len(clusters),
        "total_duplicates": sum(len(c) for c in clusters),
        "clusters": clusters,
    }
