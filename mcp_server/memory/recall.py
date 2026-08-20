"""Recall pipeline with scoring, experience weight, and abandoned fast-path."""

import json
import logging
import os
import re
import time
from dataclasses import dataclass, field
from typing import Any

import numpy as np

from .embedder import Embedder
from .lifecycle import MemoryLifecycle, MemoryState
from .query_expansion import expand_query
from .store import ValkeyStore
from .temporal import parse_query_date, temporal_boost

logger = logging.getLogger(__name__)

NAMESPACES = ["episodic", "project", "knowledge", "preference"]

# Push archived/deleted exclusion into the vector search so dead memories
# don't consume KNN candidate slots. valkey-search quirk (verified live):
# in-brace alternation {a|b} matches nothing — clause-level OR is required.
_STATE_FILTER = "(@state:{active} | @state:{deprioritised})"

# Project values are interpolated RAW into the tag filter. valkey-search
# (unlike RediSearch docs) matches raw spaces/dots/hyphens and FAILS on
# backslash-escaped or quoted values, so the only safe policy is: interpolate
# unmodified, and only when the value matches the same character allowlist
# the tools enforce for project names. Anything else skips push-down and
# relies on the Python-side filter below.
_TAG_VALUE_SAFE_RE = re.compile(r"^[a-zA-Z0-9_\-. ]+$")

# Which indexed field carries the project scope, per namespace. knowledge
# gained an indexed project tag in v5.3.1 — extracted facts live there scoped
# to a project (issue #20); RSS articles carry a feed-level label since
# v6.1.1 (default "RSS", per-feed override in feeds.yml) so they still drop
# out under a real project's filter, and project="RSS" recalls only them.
# The project namespace is excluded: ULID-keyed docs written mid-session
# may only carry `project` until the startup migration backfills project_name.
_PROJECT_FILTER_FIELDS = {
    "episodic": "project",
    "preference": "project",
    "knowledge": "project",
}


def normalise_project_filter(project_filter: Any) -> list[str]:
    """Accept a single project or a list of them, and return a clean list.

    A domain filter resolves to several projects (v6.6), so every project-
    scoped path takes a list internally. A bare string still works and is the
    common case, so callers were not changed.
    """
    if not project_filter:
        return []
    if isinstance(project_filter, str):
        candidates = [project_filter]
    else:
        candidates = [p for p in project_filter if isinstance(p, str)]
    out: list[str] = []
    for name in candidates:
        name = name.strip()
        if name and name not in out:
            out.append(name)
    return out


def _build_filter_expr(namespace: str, project_filter: Any) -> str:
    """Compose the FT.SEARCH filter for one namespace.

    With more than one project the clauses are OR'd at clause level rather
    than with in-brace alternation — `@project:{a|b}` returns an empty set on
    valkey-search (verified live; see the graveyard). Only values matching the
    tag allowlist are pushed down; anything else is left to the Python-side
    filter, and a filter that covers only part of the requested set would be
    wrong, so a single unsafe value drops push-down for the whole clause.
    """
    clauses = [_STATE_FILTER]
    projects = normalise_project_filter(project_filter)
    if projects:
        tag_field = _PROJECT_FILTER_FIELDS.get(namespace)
        if tag_field and all(_TAG_VALUE_SAFE_RE.match(p) for p in projects):
            if len(projects) == 1:
                clauses.append(f"@{tag_field}:{{{projects[0]}}}")
            else:
                alternatives = " | ".join(
                    f"@{tag_field}:{{{p}}}" for p in projects
                )
                clauses.append(f"({alternatives})")
    if len(clauses) == 1:
        return clauses[0]
    return "(" + " ".join(clauses) + ")"


def _candidate_k(top_k: int, project_filter: Any) -> int:
    """How many KNN candidates to request per namespace.

    Was hardcoded to 20, which starved results in two ways: a caller asking
    for top_k > 20 in one namespace could never get more than 20, and with a
    project filter the top 20 could easily contain zero matches for that
    project even though matches exist further down. Over-fetch when filtering
    so the Python-side filter still has candidates left after discarding, and
    over-fetch further as the project set widens — a domain filter spanning
    six projects has to share the same candidate budget between them.
    """
    k = max(20, top_k)
    projects = normalise_project_filter(project_filter)
    if projects:
        k = max(k, min(100, 50 + 10 * (len(projects) - 1)))
    return k


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
    event_date: float | None = None
    enriched_from: str | None = None


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
        # Cached parse of every abandoned approach in the episodic namespace.
        # warn_if_abandoned runs on EVERY recall, so rescanning the namespace
        # each time is the single biggest fixed cost of the pipeline. The
        # cache is invalidated on writes in this process (record_experience,
        # log_abandoned, forget, restore) and expires by TTL to pick up
        # writes from other processes (web UI vs MCP server).
        self._abandoned_cache: list[dict[str, Any]] | None = None
        self._abandoned_cache_at: float = 0.0

    def invalidate_abandoned_cache(self) -> None:
        """Drop the cached abandoned-approach list after a relevant write."""
        self._abandoned_cache = None

    def recall(
        self,
        query: str,
        namespaces: list[str] | None = None,
        top_k: int | None = None,
        project_filter: str | list[str] | None = None,
        expand_queries: bool | None = None,
    ) -> list[RecallResult]:
        """Full recall pipeline: abandoned fast-path, search, score, rank.

        project_filter takes one project name or a list of them; a list is
        what a domain-scoped recall resolves to (v6.6). Passing an empty list
        is the same as passing nothing — callers that resolved a filter to
        nothing must decide for themselves whether an unscoped search is the
        right answer, because silently running one here would present a global
        result set as a scoped one.

        When expand_queries is True (or RECALL_EXPAND_QUERIES env var is set),
        the query is expanded into N variants via Claude Haiku and results are
        unioned across all variants, deduplicated by key, and ranked by best
        adjusted_score.
        """
        project_filter = normalise_project_filter(project_filter)
        project_set = set(project_filter)
        if top_k is None:
            top_k = int(os.getenv("MEMORY_RECALL_TOP_K", "5"))
        # Clamp top_k to a sane range
        top_k = max(1, min(top_k, 50))
        if namespaces is None:
            namespaces = NAMESPACES

        if expand_queries is None:
            expand_queries = os.getenv("RECALL_EXPAND_QUERIES", "").strip().lower() in (
                "true", "1", "yes",
            )

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

        # Parse a date out of the query once. None when the query has no
        # temporal language — temporal_multiplier stays at 1.0 in that case.
        query_date = parse_query_date(query)

        per_ns_k = _candidate_k(top_k, project_filter)
        for ns in namespaces:
            raw_results = self.store.search(
                ns, query_vector, top_k=per_ns_k,
                filter_expr=_build_filter_expr(ns, project_filter),
            )

            for doc in raw_results:
                state_str = doc.get("state", "active")

                # Step 4: Filter archived/deleted. Kept even though the
                # query-side filter excludes them — it is the safety net when
                # the filtered search fell back to unfiltered.
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
                if project_set and doc_project not in project_set:
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

                # Step 8b: Temporal boost — if the query mentioned a date and
                # this memory has an event_date close to it, multiply the score.
                temporal_multiplier = 1.0
                event_date_raw = doc.get("event_date")
                event_date_val: float | None = None
                if event_date_raw:
                    try:
                        event_date_val = float(event_date_raw)
                    except (ValueError, TypeError):
                        event_date_val = None
                if query_date is not None and event_date_val is not None:
                    temporal_multiplier = temporal_boost(query_date, event_date_val)

                # Combined adjusted score
                adjusted_score = (
                    raw_score * surface_score * recency_multiplier
                    * exp_weight * temporal_multiplier
                )

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
                    event_date=event_date_val,
                    enriched_from=doc.get("enriched_from"),
                ))

        # Step 9b: Query expansion — run additional searches for each variant
        # and merge results by key, keeping the highest adjusted_score.
        if expand_queries:
            try:
                variants = expand_query(query, store=self.store)
            except Exception as exc:
                logger.warning("Query expansion failed, using original only: %s", exc)
                variants = []
            for variant in variants:
                if not variant or variant == query:
                    continue
                variant_results = self._search_variant(
                    variant=variant,
                    namespaces=namespaces,
                    project_filter=project_filter,
                    suppressed_topics=suppressed_topics,
                    recency_decay_days=recency_decay_days,
                    now=now,
                    top_k=top_k,
                    query_date=query_date,
                )
                results.extend(variant_results)
            if variants:
                logger.debug("Query expansion produced %d variants", len(variants))

        # Step 10: Dedupe (keep best adjusted_score). Always on, not just
        # under query expansion. Keyed on (key, result_type) so an abandoned-
        # approach warning never collapses into the memory that carries it.
        best: dict[tuple[str, str], RecallResult] = {}
        for r in results:
            dedupe_key = (r.key, r.result_type)
            existing = best.get(dedupe_key)
            if existing is None or r.adjusted_score > existing.adjusted_score:
                best[dedupe_key] = r
        results = list(best.values())

        # Step 10b: When an extracted fact and its verbatim source BOTH
        # matched, keep only the source — promoted to the fact's score if the
        # fact ranked higher (the fact acted as a retrieval pointer; the
        # verbatim chunk carries strictly more context). Doing this before
        # the sort handles both orderings: a source can never end up ranked
        # below where its fact would have been (issue #20: facts supplement,
        # they don't compete). Warnings are excluded — they share the carrier
        # memory's key without showing its content.
        memory_by_key = {
            r.key: r for r in results if r.result_type != "abandoned_warning"
        }
        deduped: list[RecallResult] = []
        for r in results:
            source = (
                memory_by_key.get(r.enriched_from)
                if r.result_type != "abandoned_warning" and r.enriched_from
                else None
            )
            if source is not None:
                if r.adjusted_score > source.adjusted_score:
                    source.adjusted_score = r.adjusted_score
                continue  # drop the fact; its source stands in for it
            deduped.append(r)
        results = deduped
        results.sort(key=lambda r: r.adjusted_score, reverse=True)

        # Step 11: Return top_k
        final = results[:top_k]

        # Log the recall event
        self.log_recall_event(query, final)

        return final

    def _search_variant(
        self,
        variant: str,
        namespaces: list[str],
        project_filter: str | list[str] | None,
        suppressed_topics: list[str],
        recency_decay_days: int,
        now: float,
        top_k: int = 20,
        query_date=None,
    ) -> list[RecallResult]:
        """Run vector search for one query variant. Used by query expansion.

        This intentionally mirrors the main per-namespace loop in recall() —
        same scoring (including the temporal boost), same filters — but skips
        the abandoned fast-path and recall logging which only run once per
        top-level call.
        """
        out: list[RecallResult] = []
        query_vector = self.embedder.embed(variant)
        project_set = set(normalise_project_filter(project_filter))
        per_ns_k = _candidate_k(top_k, project_filter)
        for ns in namespaces:
            raw_results = self.store.search(
                ns, query_vector, top_k=per_ns_k,
                filter_expr=_build_filter_expr(ns, project_filter),
            )
            for doc in raw_results:
                state_str = doc.get("state", "active")
                if state_str in (MemoryState.ARCHIVED.value, MemoryState.DELETED.value):
                    continue
                content = doc.get("content", "")
                if suppressed_topics:
                    content_lower = content.lower()
                    if any(topic in content_lower for topic in suppressed_topics):
                        continue
                doc_project = doc.get("project") or doc.get("project_name")
                if project_set and doc_project not in project_set:
                    continue

                raw_score = max(0.0, 1.0 - float(doc.get("similarity_score", "1.0")))
                surface_score = float(doc.get("surface_score", "1.0"))
                created_at = float(doc.get("created_at", str(now)))
                age_days = (now - created_at) / 86400
                recency_multiplier = 1.0
                if age_days > recency_decay_days:
                    excess_periods = (age_days - recency_decay_days) / 30.0
                    recency_multiplier = max(1.0 - 0.05 * excess_periods, 0.3)
                exp_weight = float(doc.get("experience_weight", "1.0"))

                # Temporal boost — parity with the main loop (was missing, so
                # date-anchored memories ranked lower via variants than via
                # the original query).
                temporal_multiplier = 1.0
                event_date_val: float | None = None
                event_date_raw = doc.get("event_date")
                if event_date_raw:
                    try:
                        event_date_val = float(event_date_raw)
                    except (ValueError, TypeError):
                        event_date_val = None
                if query_date is not None and event_date_val is not None:
                    temporal_multiplier = temporal_boost(query_date, event_date_val)

                adjusted_score = (
                    raw_score * surface_score * recency_multiplier
                    * exp_weight * temporal_multiplier
                )

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

                contradictions_raw = doc.get("contradictions", "[]")
                try:
                    contradictions = json.loads(contradictions_raw) if contradictions_raw else []
                except (json.JSONDecodeError, TypeError):
                    contradictions = []

                result_type = "knowledge" if ns == "knowledge" else "memory"

                out.append(RecallResult(
                    key=doc.get("key", ""),
                    namespace=ns,
                    content=content,
                    score=raw_score,
                    adjusted_score=adjusted_score,
                    state=state_str,
                    project=doc_project,
                    source_url=doc.get("source_url"),
                    published_at=doc.get("published_at"),
                    reinstate_candidate=False,
                    tags=tags,
                    deprioritised_reason=doc.get("deprioritised_reason"),
                    effort_score=effort,
                    outcome=doc.get("outcome"),
                    experience_weight=exp_weight,
                    result_type=result_type,
                    breakthrough=doc.get("breakthrough"),
                    contradictions=contradictions,
                    event_date=event_date_val,
                    enriched_from=doc.get("enriched_from"),
                ))
        return out

    def _get_abandoned_entries(self) -> list[dict[str, Any]]:
        """Parsed abandoned-approach entries across episodic, with caching.

        The scan is capped at 5000 keys and fetches only three fields, but it
        still runs on every recall — so the parsed result is cached for
        ABANDONED_CACHE_TTL_SECONDS (default 60, 0 disables). Writers in this
        process call invalidate_abandoned_cache(); other processes are
        covered by the TTL.
        """
        ttl = int(os.getenv("ABANDONED_CACHE_TTL_SECONDS", "60"))
        now = time.time()
        if (
            ttl > 0
            and self._abandoned_cache is not None
            and now - self._abandoned_cache_at < ttl
        ):
            return self._abandoned_cache

        entries: list[dict[str, Any]] = []

        _MAX_ABANDONED_SCAN_KEYS = 5000
        keys = self.store.scan_prefix("mem:episodic:")
        if len(keys) > _MAX_ABANDONED_SCAN_KEYS:
            logger.warning(
                "Abandoned scan capped at %d keys (total: %d)",
                _MAX_ABANDONED_SCAN_KEYS, len(keys),
            )
            keys = keys[:_MAX_ABANDONED_SCAN_KEYS]

        if keys:
            all_data = self.store.get_fields_multi(
                keys, ("abandoned_approaches", "effort_score", "project")
            )
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

                effort_raw = data.get("effort_score")
                effort = None
                if effort_raw is not None:
                    try:
                        effort = int(float(effort_raw))
                    except (ValueError, TypeError):
                        pass

                for approach in approaches:
                    if not isinstance(approach, dict):
                        continue
                    name = approach.get("name", "")
                    if not name:
                        continue
                    entries.append({
                        "memory_key": key,
                        "name_lower": name.lower(),
                        "abandoned_name": name,
                        "reason": approach.get("reason", ""),
                        "effort_score": effort,
                        "project": data.get("project"),
                    })

        self._abandoned_cache = entries
        self._abandoned_cache_at = now
        return entries

    def warn_if_abandoned(self, query: str) -> list[dict[str, Any]]:
        """Keyword scan for abandoned approaches matching the query.

        Runs before embedding — cheap keyword check against the cached
        abandoned-entry list (see _get_abandoned_entries).
        """
        matches: list[dict[str, Any]] = []
        seen_keys: set[str] = set()
        query_lower = query.lower()

        for entry in self._get_abandoned_entries():
            name = entry["name_lower"]
            if name in query_lower or query_lower in name:
                match_key = f"{entry['memory_key']}:{name}"
                if match_key in seen_keys:
                    continue
                seen_keys.add(match_key)
                matches.append({
                    "memory_key": entry["memory_key"],
                    "abandoned_name": entry["abandoned_name"],
                    "reason": entry["reason"],
                    "effort_score": entry["effort_score"],
                    "project": entry["project"],
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
