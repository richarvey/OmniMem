"""Memory lifecycle state machine: active/deprioritised/archived/deleted."""

import json
import logging
import os
import time
from enum import Enum
from typing import Any

from .store import ValkeyStore

logger = logging.getLogger(__name__)


class MemoryState(str, Enum):
    ACTIVE = "active"
    DEPRIORITISED = "deprioritised"
    ARCHIVED = "archived"
    DELETED = "deleted"


ALLOWED_TRANSITIONS: dict[MemoryState, set[MemoryState]] = {
    MemoryState.ACTIVE: {MemoryState.DEPRIORITISED, MemoryState.ARCHIVED, MemoryState.DELETED},
    MemoryState.DEPRIORITISED: {MemoryState.ACTIVE, MemoryState.ARCHIVED, MemoryState.DELETED},
    MemoryState.ARCHIVED: {MemoryState.ACTIVE, MemoryState.DELETED},
    MemoryState.DELETED: set(),
}

SURFACE_SCORES: dict[MemoryState, float] = {
    MemoryState.ACTIVE: 1.0,
    MemoryState.DEPRIORITISED: float(os.getenv("DEPRIORITISED_WEIGHT", "0.2")),
    MemoryState.ARCHIVED: 0.0,
    MemoryState.DELETED: 0.0,
}


_PROJECT_NAMESPACES = ("episodic", "project", "knowledge", "preference")


def bulk_transition_project(
    store: ValkeyStore,
    project_name: str,
    new_state: MemoryState,
    *,
    apply: bool,
    reason: str | None = None,
    include_context: bool = False,
) -> dict[str, Any]:
    """Transition every memory belonging to a project to ``new_state``.

    Scans keys directly (like delete_project) so it catches everything recall
    can't surface. Only memories whose current state permits the transition are
    counted; the rest are reported under ``skipped`` (keyed by current state).

    With ``apply=False`` this is a dry run — it returns the counts without
    writing, so callers can show a preview. With ``apply=True`` it writes the
    new state, surface score and updated_at in pipelined batches and returns the
    number actually changed.

    The project context entry (``mem:project:<name>``) is left alone unless
    ``include_context=True``.
    """
    to_change: dict[str, list[str]] = {}
    skipped: dict[str, int] = {}
    context_key = f"mem:project:{project_name}"

    for ns in _PROJECT_NAMESPACES:
        keys = store.scan_prefix(f"mem:{ns}:")
        if not keys:
            continue
        rows = store.get_fields_multi(keys, ("project", "project_name", "state"))
        matched: list[str] = []
        for key, row in zip(keys, rows):
            if row is None:
                continue
            doc_project = row.get("project") or row.get("project_name")
            if doc_project != project_name:
                continue
            if key == context_key and not include_context:
                continue
            try:
                current = MemoryState(row.get("state") or MemoryState.ACTIVE.value)
            except ValueError:
                current = MemoryState.ACTIVE
            if current == new_state or new_state not in ALLOWED_TRANSITIONS[current]:
                skipped[current.value] = skipped.get(current.value, 0) + 1
                continue
            matched.append(key)
        if matched:
            to_change[ns] = matched

    counts = {ns: len(v) for ns, v in to_change.items()}
    total = sum(counts.values())

    changed = 0
    if apply and total:
        updates: dict[str, Any] = {
            "state": new_state.value,
            "surface_score": str(SURFACE_SCORES[new_state]),
            "updated_at": str(time.time()),
        }
        if new_state == MemoryState.DEPRIORITISED and reason:
            updates["deprioritised_reason"] = reason
        for keys in to_change.values():
            changed += store.set_fields_multi(keys, updates)

    return {"counts": counts, "total": total, "skipped": skipped, "changed": changed}


class MemoryLifecycle:
    """Enforces state transitions and manages topic suppression."""

    def __init__(self, store: ValkeyStore) -> None:
        self.store = store
        # Cache suppressed topics to avoid hitting Valkey on every recall result.
        # Invalidated on suppress/unsuppress calls.
        self._suppressed_cache: list[str] | None = None

    def transition(
        self, key: str, new_state: MemoryState, reason: str | None = None
    ) -> dict[str, Any]:
        """Validate and apply a state transition. Returns result dict with optional warning."""
        data = self.store.get(key)
        if data is None:
            raise ValueError(f"Memory key not found: {key}")

        current_state = MemoryState(data.get("state", "active"))
        if new_state not in ALLOWED_TRANSITIONS[current_state]:
            raise ValueError(
                f"Invalid transition: {current_state.value} -> {new_state.value}. "
                f"Allowed: {[s.value for s in ALLOWED_TRANSITIONS[current_state]]}"
            )

        now = str(time.time())
        updates: dict[str, Any] = {
            "state": new_state.value,
            "surface_score": str(SURFACE_SCORES[new_state]),
            "updated_at": now,
        }

        if new_state == MemoryState.DEPRIORITISED and reason:
            updates["deprioritised_reason"] = reason

        # DELETED removes the key below — writing the fields first is a
        # wasted round-trip plus pointless index churn on a doomed hash.
        if new_state != MemoryState.DELETED:
            # Single round-trip instead of N individual set_field calls
            self.store.set_fields(key, updates)

        result: dict[str, Any] = {
            "key": key,
            "previous_state": current_state.value,
            "new_state": new_state.value,
            "surface_score": SURFACE_SCORES[new_state],
        }

        if new_state == MemoryState.DEPRIORITISED:
            effort_score_raw = data.get("effort_score")
            if effort_score_raw is not None:
                try:
                    effort_score = int(float(effort_score_raw))
                    if effort_score >= 4:
                        result["warning"] = (
                            f"This memory has an effort score of {effort_score}/5. "
                            "It represents hard-won knowledge. "
                            "Deprioritised as requested, but consider archiving "
                            "rather than suppressing it entirely."
                        )
                except (ValueError, TypeError):
                    pass

        if new_state == MemoryState.DELETED:
            self.store.delete(key)

        logger.info(
            "Transitioned %s: %s -> %s",
            key, current_state.value, new_state.value,
        )
        return result

    def suppress_topic(self, topic: str) -> None:
        """Add a topic to the suppression set."""
        self.store.client.sadd("topics:suppressed", topic.lower())
        self._suppressed_cache = None  # invalidate cache
        logger.info("Suppressed topic: %s", topic)

    def unsuppress_topic(self, topic: str) -> None:
        """Remove a topic from the suppression set."""
        self.store.client.srem("topics:suppressed", topic.lower())
        self._suppressed_cache = None  # invalidate cache
        logger.info("Unsuppressed topic: %s", topic)

    def get_suppressed_topics(self) -> list[str]:
        """Return all currently suppressed topics (cached)."""
        if self._suppressed_cache is None:
            members = self.store.client.smembers("topics:suppressed")
            self._suppressed_cache = sorted(members)
        return self._suppressed_cache

    def invalidate_suppression_cache(self) -> None:
        """Force refresh of the suppressed topics cache."""
        self._suppressed_cache = None

    def is_topic_suppressed(self, text: str) -> bool:
        """Check if any suppressed topic appears in text (case-insensitive substring)."""
        suppressed = self.get_suppressed_topics()
        if not suppressed:
            return False
        text_lower = text.lower()
        return any(topic in text_lower for topic in suppressed)

    def add_reinstate_hints(self, key: str, hints: list[str]) -> None:
        """Append hints to the reinstate_hints JSON array field."""
        data = self.store.get(key)
        if data is None:
            raise ValueError(f"Memory key not found: {key}")

        existing_raw = data.get("reinstate_hints", "[]")
        try:
            existing = json.loads(existing_raw)
        except (json.JSONDecodeError, TypeError):
            existing = []

        existing.extend(hints)
        self.store.set_field(key, "reinstate_hints", json.dumps(existing))

    def check_reinstate_eligibility(
        self, key: str, query: str, doc_data: dict[str, Any] | None = None
    ) -> bool:
        """Return True if query matches any reinstate hint and memory is deprioritised.

        Args:
            key: Memory key (used for fallback lookup if doc_data not provided).
            query: The recall query string.
            doc_data: Optional pre-fetched document data to avoid an extra GET.
        """
        data = doc_data if doc_data is not None else self.store.get(key)
        if data is None:
            return False

        state = data.get("state", "active")
        if state != MemoryState.DEPRIORITISED.value:
            return False

        hints_raw = data.get("reinstate_hints", "[]")
        try:
            hints = json.loads(hints_raw)
        except (json.JSONDecodeError, TypeError):
            return False

        query_lower = query.lower()
        return any(hint.lower() in query_lower or query_lower in hint.lower() for hint in hints)
