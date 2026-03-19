"""Recall pipeline with scoring, experience weight, and abandoned fast-path."""

import json
import logging
import os
import time
from dataclasses import dataclass, field
from typing import Any

import numpy as np

from .embedder import Embedder
from .lifecycle import MemoryLifecycle, MemoryState
from .store import ValkeyStore

logger = logging.getLogger(__name__)

NAMESPACES = ["episodic", "project", "knowledge"]


def compute_experience_weight(effort_score: int, outcome: str) -> float:
    """Compute experience weight from effort score (1-5) and outcome."""
    base = {"succeeded": 1.0, "pivoted": 0.7, "abandoned": 0.1}.get(outcome, 1.0)
    effort_multiplier = {1: 1.0, 2: 1.1, 3: 1.25, 4: 1.5, 5: 1.8}.get(effort_score, 1.0)
    if outcome == "abandoned":
        return base  # effort does not amplify failures
    return min(base * effort_multiplier, 2.0)  # cap at 2.0


@dataclass
class RecallResult:
    """A single recall result with adjusted scoring."""

    key: str
    namespace: str
    content: str
    score: float
    adjusted_score: float
    state: str
    project: str | None = None
    source_url: str | None = None
    published_at: str | None = None
    reinstate_candidate: bool = False
    tags: list[str] = field(default_factory=list)
    deprioritised_reason: str | None = None
    effort_score: int | None = None
    outcome: str | None = None
    experience_weight: float = 1.0
    result_type: str = "memory"
    breakthrough: str | None = None
    contradictions: list[dict] = field(default_factory=list)


class RecallPipeline:
    """Orchestrates recall: abandoned fast-path, vector search, scoring, ranking."""

    def __init__(
        self,
        store: ValkeyStore,
        embedder: Embedder,
        lifecycle: MemoryLifecycle,
    ) -> None:
        self.store = store
        self.embedder = embedder
        self.lifecycle = lifecycle

    def recall(
        self,
        query: str,
        namespaces: list[str] | None = None,
        top_k: int | None = None,
        project_filter: str | None = None,
    ) -> list[RecallResult]:
        """Full recall pipeline: abandoned fast-path, search, score, rank."""
        if top_k is None:
            top_k = int(os.getenv("MEMORY_RECALL_TOP_K", "5"))
        # Clamp top_k to a sane range
        top_k = max(1, min(top_k, 50))
        if namespaces is None:
            namespaces = NAMESPACES

        results: list[RecallResult] = []

        # Step 1: Abandoned fast-path (before embedding)
        abandoned_warnings = self.warn_if_abandoned(query)
        for warning in abandoned_warnings:
            results.append(RecallResult(
                key=warning["memory_key"],
                namespace="episodic",
                content=(
                    f"Abandoned approach: {warning['abandoned_name']} — "
                    f"{warning['reason']}"
                ),
                score=1.0,
                adjusted_score=1.0,
                state="active",
                project=warning.get("project"),
                effort_score=warning.get("effort_score"),
                result_type="abandoned_warning",
            ))

        # Step 2: Embed the query
        query_vector = self.embedder.embed(query)

        # Step 3: Search each namespace
        recency_decay_days = int(os.getenv("RECENCY_DECAY_DAYS", "90"))
        now = time.time()

        # Pre-fetch suppressed topics once for the entire recall, not per-doc
        suppressed_topics = self.lifecycle.get_suppressed_topics()

        for ns in namespaces:
            raw_results = self.store.search(ns, query_vector, top_k=20)

            for doc in raw_results:
                state_str = doc.get("state", "active")

                # Step 4: Filter archived/deleted
                if state_str in (MemoryState.ARCHIVED.value, MemoryState.DELETED.value):
                    continue

                content = doc.get("content", "")

                # Step 5: Filter suppressed topics (using pre-fetched list)
                if suppressed_topics:
                    content_lower = content.lower()
                    if any(topic in content_lower for topic in suppressed_topics):
                        continue

                # Project filter
                doc_project = doc.get("project") or doc.get("project_name")
                if project_filter and doc_project != project_filter:
                    continue

                raw_score = max(0.0, 1.0 - float(doc.get("similarity_score", "1.0")))

                # Step 6: Surface score
                surface_score = float(doc.get("surface_score", "1.0"))

                # Step 7: Recency decay
                created_at = float(doc.get("created_at", str(now)))
                age_days = (now - created_at) / 86400
                recency_multiplier = 1.0
                if age_days > recency_decay_days:
                    excess_periods = (age_days - recency_decay_days) / 30.0
                    recency_multiplier = max(1.0 - 0.05 * excess_periods, 0.3)

                # Step 8: Experience weight
                exp_weight = float(doc.get("experience_weight", "1.0"))

                # Combined adjusted score
                adjusted_score = raw_score * surface_score * recency_multiplier * exp_weight

                # Parse tags
                tags_raw = doc.get("tags", "[]")
                try:
                    tags = json.loads(tags_raw) if tags_raw else []
                except (json.JSONDecodeError, TypeError):
                    tags = [t.strip() for t in tags_raw.split(",") if t.strip()] if isinstance(tags_raw, str) else []

                effort_raw = doc.get("effort_score")
                effort = None
                if effort_raw is not None:
                    try:
                        effort = int(float(effort_raw))
                    except (ValueError, TypeError):
                        pass

                reinstate_candidate = False
                # Step 9: Check reinstate eligibility — pass doc data to avoid redundant GET
                if state_str == MemoryState.DEPRIORITISED.value:
                    key = doc.get("key", "")
                    if self.lifecycle.check_reinstate_eligibility(key, query, doc_data=doc):
                        reinstate_candidate = True
                        adjusted_score = 0.6

                result_type = "knowledge" if ns == "knowledge" else "memory"

                # Parse contradictions
                contradictions_raw = doc.get("contradictions", "[]")
                try:
                    contradictions = json.loads(contradictions_raw) if contradictions_raw else []
                except (json.JSONDecodeError, TypeError):
                    contradictions = []

                results.append(RecallResult(
                    key=doc.get("key", ""),
                    namespace=ns,
                    content=content,
                    score=raw_score,
                    adjusted_score=adjusted_score,
                    state=state_str,
                    project=doc_project,
                    source_url=doc.get("source_url"),
                    published_at=doc.get("published_at"),
                    reinstate_candidate=reinstate_candidate,
                    tags=tags,
                    deprioritised_reason=doc.get("deprioritised_reason"),
                    effort_score=effort,
                    outcome=doc.get("outcome"),
                    experience_weight=exp_weight,
                    result_type=result_type,
                    breakthrough=doc.get("breakthrough"),
                    contradictions=contradictions,
                ))

        # Step 10: Sort by adjusted_score descending
        results.sort(key=lambda r: r.adjusted_score, reverse=True)

        # Step 11: Return top_k
        final = results[:top_k]

        # Log the recall event
        self.log_recall_event(query, final)

        return final

    def warn_if_abandoned(self, query: str) -> list[dict[str, Any]]:
        """Keyword scan for abandoned approaches matching the query.

        Runs before embedding -- cheap keyword check. Uses pipelined batch
        fetch instead of N+1 individual GET calls. Caps at 5000 keys to
        prevent excessive memory/CPU usage on large datasets.
        """
        matches: list[dict[str, Any]] = []
        seen_keys: set[str] = set()
        query_lower = query.lower()

        # Scan episodic memory keys, capped to prevent DoS on huge datasets
        _MAX_ABANDONED_SCAN_KEYS = 5000
        keys = self.store.scan_prefix("mem:episodic:")
        if not keys:
            return matches
        if len(keys) > _MAX_ABANDONED_SCAN_KEYS:
            logger.warning(
                "Abandoned scan capped at %d keys (total: %d)",
                _MAX_ABANDONED_SCAN_KEYS, len(keys),
            )
            keys = keys[:_MAX_ABANDONED_SCAN_KEYS]

        # Batch-fetch all episodic memories in one pipeline round-trip
        all_data = self.store.get_multi(keys)

        for key, data in zip(keys, all_data):
            if data is None:
                continue

            approaches_raw = data.get("abandoned_approaches", "[]")
            try:
                approaches = json.loads(approaches_raw)
            except (json.JSONDecodeError, TypeError):
                continue

            if not isinstance(approaches, list):
                continue

            for approach in approaches:
                if not isinstance(approach, dict):
                    continue
                name = approach.get("name", "").lower()
                if name and (name in query_lower or query_lower in name):
                    match_key = f"{key}:{name}"
                    if match_key not in seen_keys:
                        seen_keys.add(match_key)
                        effort_raw = data.get("effort_score")
                        effort = None
                        if effort_raw is not None:
                            try:
                                effort = int(float(effort_raw))
                            except (ValueError, TypeError):
                                pass
                        matches.append({
                            "memory_key": key,
                            "abandoned_name": approach.get("name", ""),
                            "reason": approach.get("reason", ""),
                            "effort_score": effort,
                            "project": data.get("project"),
                        })

        return matches

    def log_recall_event(self, query: str, results: list[RecallResult]) -> None:
        """Store a lightweight recall log entry with 30-day TTL."""
        timestamp = str(time.time())
        log_key = f"log:recall:{timestamp}"
        top_keys = [r.key for r in results[:10]]
        top_scores = [str(r.adjusted_score) for r in results[:10]]

        log_data = {
            "query": query[:2000],  # Truncate to prevent storing huge queries
            "timestamp": timestamp,
            "result_keys": json.dumps(top_keys),
            "result_scores": json.dumps(top_scores),
        }

        try:
            # Single pipeline round-trip for log entry + per-memory counters
            pipe = self.store.client.pipeline(transaction=False)
            pipe.hset(log_key, mapping=log_data)
            pipe.expire(log_key, 30 * 86400)  # 30-day TTL
            # Update per-memory recall counters
            for r in results[:10]:
                pipe.hincrby(r.key, "recall_count", 1)
                pipe.hset(r.key, "last_recalled", timestamp)
            pipe.execute()
        except Exception as exc:
            logger.warning("Failed to log recall event: %s", exc)
