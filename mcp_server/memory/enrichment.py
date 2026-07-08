"""Background enrichment queue — decouples fact extraction from ingest.

When INGEST_MODE=full, remember() and remember_document() store content
raw (embed + write) and push the key onto a Valkey queue. This worker
thread pops keys, runs extract_facts(), and writes extracted facts as
new linked memories.

The queue is persisted in Valkey (LPUSH/BRPOP on 'queue:enrich') so
pending work survives mcp_server restarts.
"""

from __future__ import annotations

import json
import logging
import os
import threading
import time
from typing import TYPE_CHECKING

import ulid

from .dedup import check_duplicate
from .extraction import extract_facts, ExtractedFact
from .lifecycle import MemoryState

if TYPE_CHECKING:
    from .embedder import Embedder
    from .store import ValkeyStore

logger = logging.getLogger(__name__)

QUEUE_KEY = "queue:enrich"
# How long BRPOP blocks before rechecking the shutdown flag
_POLL_TIMEOUT = 2


class EnrichmentWorker:
    """Background daemon thread that consumes the enrichment queue."""

    def __init__(self, store: "ValkeyStore", embedder: "Embedder") -> None:
        self._store = store
        self._embedder = embedder
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        if self._thread is not None:
            return
        self._thread = threading.Thread(target=self._run, daemon=True, name="enrichment-worker")
        self._thread.start()
        logger.info("Enrichment worker started")

    def stop(self) -> None:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=5)
            self._thread = None
        logger.info("Enrichment worker stopped")

    @property
    def queue_length(self) -> int:
        try:
            return self._store.client.llen(QUEUE_KEY)
        except Exception:
            return -1

    def _run(self) -> None:
        """Main loop: BRPOP from queue, enrich, repeat."""
        while not self._stop.is_set():
            try:
                result = self._store.client.brpop(QUEUE_KEY, timeout=_POLL_TIMEOUT)
            except Exception as exc:
                logger.warning("Enrichment queue BRPOP failed: %s", exc)
                time.sleep(1)
                continue

            if result is None:
                continue  # timeout, loop and recheck _stop

            _, payload_raw = result
            try:
                payload = json.loads(payload_raw)
            except (json.JSONDecodeError, TypeError):
                logger.warning("Enrichment queue: invalid payload %r", payload_raw[:100] if payload_raw else "")
                continue

            try:
                self._enrich(payload)
            except Exception as exc:
                logger.error("Enrichment failed for %s: %s", payload.get("key", "?"), exc)

    def _enrich(self, payload: dict) -> None:
        """Extract facts from a stored memory and write them back as linked memories."""
        key = payload.get("key", "")
        project = payload.get("project")
        tags = payload.get("tags")
        batch_mode = payload.get("batch_mode", False)
        batch_content = payload.get("batch_content")

        # Source timestamps for the event_date fallback chain (issue #20).
        # Batch mode can't read individual sources, so the ingest time rides
        # in the payload; single-key mode reads the source memory directly.
        source_event_date = payload.get("event_date")
        source_created_at = payload.get("created_at")

        if batch_mode and batch_content:
            # Batch extraction: all chunks combined into one API call.
            # If the payload predates the created_at field (queued before an
            # upgrade), fall back to reading the first chunk's timestamps so
            # the facts still get a temporal anchor.
            if not source_created_at and key:
                rows = self._store.get_fields_multi(
                    [key], ("event_date", "created_at")
                )
                src = rows[0] if rows and rows[0] else {}
                source_event_date = source_event_date or src.get("event_date")
                source_created_at = src.get("created_at")
            facts = extract_facts(batch_content)
        else:
            # Single-key extraction: read content from store
            data = self._store.get(key)
            if data is None:
                logger.debug("Enrichment: key %s no longer exists, skipping", key)
                return
            content = data.get("content", "")
            if not content:
                return
            source_event_date = data.get("event_date") or source_event_date
            source_created_at = data.get("created_at") or source_created_at
            facts = extract_facts(content)

        if not facts:
            return

        now = str(time.time())
        source_doc_id = payload.get("doc_id") or key
        stored = 0
        preferences = 0
        duplicates = 0

        for fact in facts:
            # Facts supplement the verbatim chunk, they don't replace it
            # (issue #20). Routing them to the knowledge namespace keeps them
            # out of the source namespace's KNN candidate budget, so compact
            # facts can't crowd out the richer verbatim content that recall
            # queries naturally match.
            target_ns = "preference" if fact.kind == "preference" else "knowledge"
            vector = self._embedder.embed(fact.text)

            # Skip near-duplicate facts. The synchronous extraction path used
            # to do this via check_duplicate; the async queue migration lost
            # it, so re-remembering similar content silently piled up
            # duplicate fact memories.
            dup = check_duplicate(
                self._store, target_ns, vector, fact.text, project_filter=project
            )
            if dup is not None:
                duplicates += 1
                continue

            fact_key = f"mem:{target_ns}:{ulid.new().str}"
            fields = {
                "content": fact.text,
                "state": MemoryState.ACTIVE.value,
                # Verbatim chunks keep 1.0 so they outrank their own facts on
                # direct recall; facts win on contradiction/knowledge-update
                # checks where their compactness helps (issue #20).
                "surface_score": "0.5",
                "experience_weight": "1.0",
                "created_at": now,
                "updated_at": now,
                "tags": json.dumps(tags or []),
                "source_doc_id": source_doc_id,
                "enriched_from": key,
            }
            if project:
                fields["project"] = project
            # event_date fallback chain: the fact's own date, else the source
            # memory's event_date, else the source's ingest time. Without
            # this, extraction strips the temporal anchor and date-shaped
            # recall queries can't find the fact (issue #20 — temporal
            # reasoning fell from 53.4% to 7.5%).
            if fact.event_date is not None:
                fields["event_date"] = str(fact.event_date)
            elif source_event_date:
                fields["event_date"] = str(source_event_date)
            elif source_created_at:
                fields["event_date"] = str(source_created_at)
            if target_ns == "preference":
                fields["scope"] = "project" if project else "global"
            self._store.upsert(target_ns, fact_key, fields, vector)
            stored += 1
            if target_ns == "preference":
                preferences += 1

        logger.info(
            "Enriched %s: %d facts (%d preferences, %d duplicates skipped) from %d extracted",
            key, stored, preferences, duplicates, len(facts),
        )


def enqueue(
    store: "ValkeyStore",
    key: str,
    namespace: str,
    project: str | None = None,
    tags: list[str] | None = None,
    doc_id: str | None = None,
    created_at: str | None = None,
) -> None:
    """Push a memory key onto the enrichment queue for background processing."""
    payload = json.dumps({
        "key": key,
        "namespace": namespace,
        "project": project,
        "tags": tags,
        "doc_id": doc_id,
        "created_at": created_at,
    })
    store.client.lpush(QUEUE_KEY, payload)


def enqueue_batch(
    store: "ValkeyStore",
    keys: list[str],
    combined_content: str,
    namespace: str,
    project: str | None = None,
    tags: list[str] | None = None,
    doc_id: str | None = None,
    created_at: str | None = None,
) -> None:
    """Push a batch enrichment job — all chunks extracted in one Haiku call."""
    payload = json.dumps({
        "key": keys[0] if keys else "",
        "namespace": namespace,
        "project": project,
        "tags": tags,
        "doc_id": doc_id,
        "created_at": created_at,
        "batch_mode": True,
        "batch_content": combined_content[:24000],  # cap for prompt size
    })
    store.client.lpush(QUEUE_KEY, payload)
