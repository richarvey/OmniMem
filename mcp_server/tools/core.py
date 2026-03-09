"""Core MCP tools: remember, recall, forget, deprioritise, archive, reinstate, topic suppression."""

import json
import logging
import time
from dataclasses import asdict
from typing import Any

import ulid

from ..memory.embedder import Embedder
from ..memory.lifecycle import MemoryLifecycle, MemoryState
from ..memory.recall import RecallPipeline
from ..memory.store import ValkeyStore

logger = logging.getLogger(__name__)


def _get_deps() -> tuple[ValkeyStore, Embedder, MemoryLifecycle, RecallPipeline]:
    """Get shared dependencies. Set by server.py at startup."""
    from ..tools import _store, _embedder, _lifecycle, _pipeline
    return _store, _embedder, _lifecycle, _pipeline


def remember(
    content: str,
    project: str | None = None,
    tags: list[str] | None = None,
    namespace: str = "episodic",
) -> dict[str, Any]:
    """Store a new memory. Use this to remember decisions, solutions, patterns, or project context.

    Args:
        content: The text to remember — be specific and descriptive.
        project: Optional project name to associate this memory with.
        tags: Optional list of tags for categorisation.
        namespace: Memory namespace — 'episodic' (default), 'project', or 'knowledge'.

    Returns:
        Dict with key, status, and namespace of the stored memory.
    """
    store, embedder, _, _ = _get_deps()

    key = f"mem:{namespace}:{ulid.new().str}"
    now = str(time.time())
    vector = embedder.embed(content)

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
        store.set_field(key_or_query, "deprioritised_reason", "")
        store.set_field(key_or_query, "surface_score", "1.0")
        affected.append(result)
    else:
        results = pipeline.recall(key_or_query, top_k=3)
        for r in results:
            if r.state in (MemoryState.DEPRIORITISED.value, MemoryState.ARCHIVED.value):
                try:
                    result = lifecycle.transition(r.key, MemoryState.ACTIVE)
                    store.set_field(r.key, "deprioritised_reason", "")
                    store.set_field(r.key, "surface_score", "1.0")
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
