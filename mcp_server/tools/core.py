"""Core MCP tools: remember, recall, forget, deprioritise, archive, reinstate, topic suppression."""

import json
import logging
import os
import re
import time
from typing import Any

import ulid

from memory.chunking import chunk as chunk_content, VALID_STRATEGIES as CHUNK_STRATEGIES
from memory.contradiction import check_contradiction_heuristic
from memory.dedup import check_duplicate, find_all_duplicates
from memory.enrichment import enqueue, enqueue_batch
from memory.embedder import Embedder
from memory.lifecycle import MemoryLifecycle, MemoryState
from memory.recall import RecallPipeline
from memory.store import ValkeyStore

from . import __version__, _compact

logger = logging.getLogger(__name__)


def version() -> dict[str, str]:
    """Return the current OmniMem version."""
    return {"version": __version__}

VALID_NAMESPACES = {"episodic", "project", "knowledge", "preference"}
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
    from tools import _store, _embedder, _lifecycle, _pipeline
    return _store, _embedder, _lifecycle, _pipeline


def _resolve_mode(mode: str | None) -> str:
    """Resolve ingest mode from arg or INGEST_MODE env (default 'full')."""
    if mode is None:
        mode = os.getenv("INGEST_MODE", "full").strip().lower()
    if mode not in ("full", "raw"):
        raise ValueError(f"Invalid mode '{mode}'. Must be 'full' or 'raw'.")
    return mode


def remember(
    content: str,
    project: str | None = None,
    tags: list[str] | None = None,
    namespace: str = "episodic",
    force: bool = False,
    mode: str | None = None,
) -> dict[str, Any]:
    """Store a memory with automatic dedup. Returns duplicate info if near-match exists; use force=True to override.

    Args:
        content: Text to remember.
        project: Project to scope this memory to.
        tags: Categorisation tags.
        namespace: 'episodic' (default), 'project', 'knowledge', or 'preference'.
        force: Skip duplicate check.
        mode: 'full' (default — extract discrete facts via Claude before storing,
            routing preferences to the preference namespace) or 'raw' (store
            verbatim). Default follows the INGEST_MODE env var.
    """
    store, embedder, _, _ = _get_deps()

    _validate_namespace(namespace)
    _validate_content(content)
    _validate_project_name(project)
    _validate_tags(tags)
    mode = _resolve_mode(mode)

    # Full mode: store raw immediately, then enqueue for background
    # fact extraction. Caller gets instant response; enrichment worker
    # extracts facts and writes them as linked memories asynchronously.
    # Skipped entirely when force=True — force means raw bypass write.
    _enrich_after = mode == "full" and namespace != "knowledge" and not force

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
                "existing_key": dup.key,
                "existing_content": dup.content[:80],
                "similarity": round(dup.similarity, 4),
            }

    # Tier 1 contradiction check (fast heuristic) — skipped when force=True
    # so bulk ingestion stays fast (force is documented as raw bypass write).
    contradiction_warning = None
    if not force:
        contradiction = check_contradiction_heuristic(
            store, namespace, vector, content, project_filter=project
        )
        if contradiction is not None:
            contradiction_warning = {
                "existing_key": contradiction.key_b,
                "existing_content": contradiction.content_b,
                "similarity": round(contradiction.similarity, 4),
                "explanation": contradiction.explanation,
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
        # The project index/UI key on this namespace is project_name — set it
        # at write time so new docs are correct immediately, instead of only
        # after the next startup migration backfills it.
        if namespace == "project":
            fields["project_name"] = project

    store.upsert(namespace, key, fields, vector)
    logger.info("Stored memory %s in %s", key, namespace)

    # Enqueue for background fact extraction if full mode
    if _enrich_after:
        enqueue(store, key, namespace, project=project, tags=tags, created_at=now)

    result: dict[str, Any] = {"key": key, "namespace": namespace}
    if _enrich_after:
        result["enrichment"] = "queued"
    if contradiction_warning:
        result["contradiction_warning"] = contradiction_warning
    return result


def remember_document(
    content: str,
    chunk_strategy: str = "paragraphs",
    project: str | None = None,
    tags: list[str] | None = None,
    namespace: str = "episodic",
    chunk_size: int | None = None,
    mode: str | None = None,
) -> dict[str, Any]:
    """Index a long-form document by splitting it into chunks and storing each chunk as a memory.

    Use this instead of remember() for conversation transcripts, articles, meeting notes,
    or any content longer than a focused fact. Returns the list of inserted keys plus a
    shared doc_id so callers can clean up or group them later.

    Args:
        content: Long-form text to index.
        chunk_strategy: 'turn_pairs' (User:/Assistant: transcripts), 'sentences',
            'paragraphs' (default), or 'fixed_tokens'.
        project: Project to scope these memories to.
        tags: Categorisation tags applied to every chunk.
        namespace: 'episodic' (default), 'project', or 'knowledge'.
        chunk_size: Words per chunk for fixed_tokens strategy (default 200).
    """
    store, embedder, _, _ = _get_deps()

    _validate_namespace(namespace)
    _validate_content(content)
    _validate_project_name(project)
    _validate_tags(tags)
    mode = _resolve_mode(mode)
    if chunk_strategy not in CHUNK_STRATEGIES:
        raise ValueError(
            f"Invalid chunk_strategy '{chunk_strategy}'. "
            f"Must be one of: {', '.join(sorted(CHUNK_STRATEGIES))}"
        )

    chunks = chunk_content(content, chunk_strategy, chunk_size=chunk_size)
    if not chunks:
        return {"doc_id": None, "keys": [], "chunks_stored": 0}

    doc_id = ulid.new().str
    now = str(time.time())
    keys: list[str] = []
    skipped = 0

    _enrich_after = mode == "full" and namespace != "knowledge"
    _batch_mode = os.getenv("ENRICHMENT_BATCH_MODE", "").strip().lower() in ("true", "1", "yes")

    for idx, chunk_text in enumerate(chunks):
        if len(chunk_text) > MAX_CONTENT_LENGTH:
            chunk_text = chunk_text[:MAX_CONTENT_LENGTH]

        vector = embedder.embed(chunk_text)
        # Skip near-duplicate chunks (e.g. boilerplate paragraphs across docs)
        dup = check_duplicate(store, namespace, vector, chunk_text, project_filter=project)
        if dup is not None:
            skipped += 1
            continue

        key = f"mem:{namespace}:{ulid.new().str}"
        fields: dict[str, Any] = {
            "content": chunk_text,
            "state": MemoryState.ACTIVE.value,
            "surface_score": "1.0",
            "experience_weight": "1.0",
            "created_at": now,
            "updated_at": now,
            "tags": json.dumps(tags or []),
            "doc_id": doc_id,
            "chunk_index": str(idx),
            "chunk_strategy": chunk_strategy,
        }
        if project:
            fields["project"] = project
        store.upsert(namespace, key, fields, vector)
        keys.append(key)

    # Enqueue for background enrichment after all chunks are stored
    if _enrich_after and keys:
        if _batch_mode:
            # Single Haiku call for all chunks combined
            combined = "\n\n".join(chunks)
            enqueue_batch(
                store, keys, combined, namespace,
                project=project, tags=tags, doc_id=doc_id, created_at=now,
            )
            logger.info("Enqueued batch enrichment for doc_id=%s (%d chunks)", doc_id, len(keys))
        else:
            # One enrichment job per chunk
            for k in keys:
                enqueue(store, k, namespace, project=project, tags=tags,
                        doc_id=doc_id, created_at=now)
            logger.info("Enqueued %d enrichment jobs for doc_id=%s", len(keys), doc_id)

    logger.info(
        "Stored document doc_id=%s mode=%s chunks=%d stored=%d skipped_dupes=%d strategy=%s",
        doc_id, mode, len(chunks), len(keys), skipped, chunk_strategy,
    )
    return {
        "doc_id": doc_id,
        "keys": keys,
        "chunks_stored": len(keys),
        "chunks_total": len(chunks),
        "duplicates_skipped": skipped,
        "namespace": namespace,
        "mode": mode,
        "enrichment": "batch_queued" if (_enrich_after and _batch_mode) else ("queued" if _enrich_after else "none"),
    }


def recall(
    query: str,
    top_k: int = 5,
    namespaces: list[str] | None = None,
    project_filter: str | None = None,
    expand_queries: bool | None = None,
) -> list[dict[str, Any]]:
    """Search memories by semantic similarity. Returns ranked results; abandoned-approach warnings appear first.

    Args:
        query: What you're looking for.
        top_k: Max results (default 5).
        namespaces: Namespaces to search ('episodic', 'project', 'knowledge'). All by default.
        project_filter: Restrict to a project.
        expand_queries: If True, generate alternative phrasings via Claude Haiku and union
            the results. Default follows the RECALL_EXPAND_QUERIES env var.
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
        expand_queries=expand_queries,
    )

    output: list[dict[str, Any]] = []
    for r in results:
        # Build compact result — only include non-empty/non-default fields
        entry: dict[str, Any] = {
            "key": r.key,
            "namespace": r.namespace,
            "content": r.content,
            "score": r.adjusted_score,
            "state": r.state,
        }
        if r.project:
            entry["project"] = r.project
        if r.result_type != "memory":
            entry["result_type"] = r.result_type
        if r.tags:
            entry["tags"] = r.tags
        if r.reinstate_candidate:
            entry["reinstate_candidate"] = True
            if r.deprioritised_reason:
                entry["deprioritised_reason"] = r.deprioritised_reason
        if r.effort_score is not None:
            entry["effort_score"] = r.effort_score
        if r.outcome:
            entry["outcome"] = r.outcome
        if r.breakthrough:
            entry["breakthrough"] = r.breakthrough
        if r.contradictions:
            entry["contradictions"] = len(r.contradictions)
        if r.source_url:
            entry["source_url"] = r.source_url
        if r.event_date is not None:
            entry["event_date"] = r.event_date
        if r.enriched_from:
            entry["enriched_from"] = r.enriched_from
        output.append(entry)

    return output


def recall_index(
    query: str,
    top_k: int = 10,
    namespaces: list[str] | None = None,
    project_filter: str | None = None,
    snippet_length: int = 150,
    expand_queries: bool | None = None,
) -> dict[str, Any]:
    """Lightweight recall: returns ranked summaries without full content. Use recall_detail() to fetch full content for selected keys.

    Args:
        query: What you're looking for.
        top_k: Max results (default 10).
        namespaces: Namespaces to search. All by default.
        project_filter: Restrict to a project.
        snippet_length: Content preview length in chars (default 150).
    """
    _, _, _, pipeline = _get_deps()

    top_k = _validate_top_k(top_k)
    if namespaces:
        for ns in namespaces:
            _validate_namespace(ns)
    if project_filter:
        _validate_project_name(project_filter)

    snippet_length = max(50, min(snippet_length, 500))

    results = pipeline.recall(
        query=query,
        namespaces=namespaces,
        top_k=top_k,
        project_filter=project_filter,
        expand_queries=expand_queries,
    )

    output: list[dict[str, Any]] = []
    total_full_tokens = 0
    total_index_tokens = 0
    for r in results:
        content_len = len(r.content)
        est_tokens = content_len // 4
        total_full_tokens += est_tokens

        snippet = r.content[:snippet_length]
        if content_len > snippet_length:
            snippet += "..."

        entry: dict[str, Any] = {
            "key": r.key,
            "namespace": r.namespace,
            "snippet": snippet,
            "score": r.adjusted_score,
            "estimated_tokens": est_tokens,
        }
        if r.project:
            entry["project"] = r.project
        if r.result_type != "memory":
            entry["result_type"] = r.result_type
        if r.tags:
            entry["tags"] = r.tags
        if r.reinstate_candidate:
            entry["reinstate_candidate"] = True

        index_tokens = len(snippet) // 4 + 10  # snippet + metadata overhead
        total_index_tokens += index_tokens
        output.append(entry)

    return {
        "results": output,
        "token_estimate": {"index": total_index_tokens, "full": total_full_tokens},
    }


def recall_detail(
    keys: list[str],
) -> list[dict[str, Any]]:
    """Fetch full content for specific memory keys. Use after recall_index() to expand only the entries you need.

    Args:
        keys: List of memory keys to retrieve (e.g. from recall_index results).
    """
    store, _, _, _ = _get_deps()

    if not keys:
        return []
    if len(keys) > MAX_TOP_K:
        keys = keys[:MAX_TOP_K]

    valid_keys = [k for k in keys if isinstance(k, str) and k.startswith("mem:")]
    all_data = store.get_multi(valid_keys)

    output: list[dict[str, Any]] = []
    for key, data in zip(valid_keys, all_data):
        if data is None:
            output.append({"key": key, "status": "not_found"})
            continue

        entry: dict[str, Any] = {
            "key": key,
            "content": data.get("content", ""),
            "namespace": key.split(":")[1] if ":" in key else "unknown",
            "state": data.get("state", "active"),
        }
        if data.get("project"):
            entry["project"] = data["project"]
        if data.get("tags"):
            try:
                tags = json.loads(data["tags"])
                if tags:
                    entry["tags"] = tags
            except (json.JSONDecodeError, TypeError):
                pass
        if data.get("source_url"):
            entry["source_url"] = data["source_url"]
        if data.get("breakthrough"):
            entry["breakthrough"] = data["breakthrough"]
        if data.get("effort_score"):
            try:
                entry["effort_score"] = int(float(data["effort_score"]))
            except (ValueError, TypeError):
                pass
        if data.get("outcome"):
            entry["outcome"] = data["outcome"]
        output.append(entry)

    return output


def deprioritise(
    key_or_query: str,
    reason: str,
    reinstate_hints: list[str] | None = None,
) -> dict[str, Any]:
    """Reduce a memory's visibility without deleting. Accepts a key or natural language query.

    Args:
        key_or_query: Memory key (e.g. 'mem:episodic:...') or search query.
        reason: Why this is being deprioritised.
        reinstate_hints: Keywords that flag this as a reinstate candidate in future queries.
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

    return {"affected": affected}


def archive(
    key_or_query: str,
    reason: str | None = None,
) -> dict[str, Any]:
    """Archive a memory — excluded from recall but still stored.

    Args:
        key_or_query: Memory key or search query.
        reason: Why this is being archived.
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

    return {"affected": affected}


def reinstate(key_or_query: str) -> dict[str, Any]:
    """Reinstate a deprioritised/archived memory to active state.

    Args:
        key_or_query: Memory key or search query.
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

    return {"affected": affected}


def forget(
    key_or_query: str,
    confirm: bool = False,
) -> dict[str, Any]:
    """Permanently delete a memory. Requires confirm=True; returns preview otherwise.

    Args:
        key_or_query: Memory key or search query.
        confirm: Must be True to delete.
    """
    store, _, lifecycle, pipeline = _get_deps()

    targets: list[dict[str, Any]] = []
    if key_or_query.startswith("mem:"):
        data = store.get(key_or_query)
        if data:
            targets.append({"key": key_or_query, "content": data.get("content", "")[:80]})
    else:
        results = pipeline.recall(key_or_query, top_k=3)
        for r in results:
            if r.adjusted_score > 0.85:
                targets.append({"key": r.key, "content": r.content[:80]})

    if not targets:
        return {"status": "not_found"}

    if not confirm:
        return {"status": "preview", "targets": targets}

    deleted = []
    for target in targets:
        try:
            lifecycle.transition(target["key"], MemoryState.DELETED)
            deleted.append(target["key"])
        except ValueError:
            store.delete(target["key"])
            deleted.append(target["key"])

    # Deleted memories may have carried abandoned-approach entries.
    pipeline.invalidate_abandoned_cache()

    return {"status": "deleted", "deleted_keys": deleted}


def suppress_topic(
    topic: str,
    reason: str | None = None,
) -> dict[str, Any]:
    """Suppress a topic — matching memories filtered from recall.

    Args:
        topic: Topic string to suppress (case-insensitive).
        reason: Why.
    """
    if not topic or not topic.strip():
        raise ValueError("Topic cannot be empty")
    if len(topic) > 200:
        raise ValueError("Topic too long (max 200 characters)")
    _, _, lifecycle, _ = _get_deps()
    lifecycle.suppress_topic(topic)
    return _compact({"topic": topic, "reason": reason})


def unsuppress_topic(topic: str) -> dict[str, Any]:
    """Remove a topic from the suppression list.

    Args:
        topic: Topic to unsuppress.
    """
    _, _, lifecycle, _ = _get_deps()
    lifecycle.unsuppress_topic(topic)
    return {"topic": topic}


def list_suppressions() -> dict[str, Any]:
    """List all suppressed topics."""
    _, _, lifecycle, _ = _get_deps()
    topics = lifecycle.get_suppressed_topics()
    return {"suppressed_topics": topics}


def find_duplicates(
    namespace: str = "episodic",
    threshold: float | None = None,
    project_filter: str | None = None,
) -> dict[str, Any]:
    """Scan a namespace for clusters of near-identical memories.

    Args:
        namespace: 'episodic' (default), 'project', or 'knowledge'.
        threshold: Similarity threshold (0.0-1.0). Default 0.92.
        project_filter: Restrict to a project.
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
        "namespace": namespace,
        "clusters": clusters,
    }
