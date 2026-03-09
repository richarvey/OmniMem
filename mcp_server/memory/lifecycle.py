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


class MemoryLifecycle:
    """Enforces state transitions and manages topic suppression."""

    def __init__(self, store: ValkeyStore) -> None:
        self.store = store

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

        for field, value in updates.items():
            self.store.set_field(key, field, value)

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
        logger.info("Suppressed topic: %s", topic)

    def unsuppress_topic(self, topic: str) -> None:
        """Remove a topic from the suppression set."""
        self.store.client.srem("topics:suppressed", topic.lower())
        logger.info("Unsuppressed topic: %s", topic)

    def get_suppressed_topics(self) -> list[str]:
        """Return all currently suppressed topics."""
        members = self.store.client.smembers("topics:suppressed")
        return sorted(members)

    def is_topic_suppressed(self, text: str) -> bool:
        """Check if any suppressed topic appears in text (case-insensitive substring)."""
        suppressed = self.get_suppressed_topics()
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

    def check_reinstate_eligibility(self, key: str, query: str) -> bool:
        """Return True if query matches any reinstate hint and memory is deprioritised."""
        data = self.store.get(key)
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
