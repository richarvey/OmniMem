"""Tests for memory lifecycle state machine, topic suppression, and reinstate hints."""

import json
import time

import pytest

from memory.lifecycle import (
    ALLOWED_TRANSITIONS,
    SURFACE_SCORES,
    MemoryLifecycle,
    MemoryState,
)
from tests.conftest import store_memory


class TestMemoryStateEnum:
    def test_state_values(self):
        assert MemoryState.ACTIVE.value == "active"
        assert MemoryState.DEPRIORITISED.value == "deprioritised"
        assert MemoryState.ARCHIVED.value == "archived"
        assert MemoryState.DELETED.value == "deleted"

    def test_state_from_string(self):
        assert MemoryState("active") == MemoryState.ACTIVE
        assert MemoryState("deprioritised") == MemoryState.DEPRIORITISED


class TestAllowedTransitions:
    def test_active_can_transition_to_deprioritised(self):
        assert MemoryState.DEPRIORITISED in ALLOWED_TRANSITIONS[MemoryState.ACTIVE]

    def test_active_can_transition_to_archived(self):
        assert MemoryState.ARCHIVED in ALLOWED_TRANSITIONS[MemoryState.ACTIVE]

    def test_active_can_transition_to_deleted(self):
        assert MemoryState.DELETED in ALLOWED_TRANSITIONS[MemoryState.ACTIVE]

    def test_deprioritised_can_go_back_to_active(self):
        assert MemoryState.ACTIVE in ALLOWED_TRANSITIONS[MemoryState.DEPRIORITISED]

    def test_deprioritised_can_transition_to_archived(self):
        assert MemoryState.ARCHIVED in ALLOWED_TRANSITIONS[MemoryState.DEPRIORITISED]

    def test_deprioritised_can_transition_to_deleted(self):
        assert MemoryState.DELETED in ALLOWED_TRANSITIONS[MemoryState.DEPRIORITISED]

    def test_archived_can_reinstate_to_active(self):
        assert MemoryState.ACTIVE in ALLOWED_TRANSITIONS[MemoryState.ARCHIVED]

    def test_archived_can_delete(self):
        assert MemoryState.DELETED in ALLOWED_TRANSITIONS[MemoryState.ARCHIVED]

    def test_archived_cannot_deprioritise(self):
        assert MemoryState.DEPRIORITISED not in ALLOWED_TRANSITIONS[MemoryState.ARCHIVED]

    def test_deleted_is_terminal(self):
        assert ALLOWED_TRANSITIONS[MemoryState.DELETED] == set()


class TestSurfaceScores:
    def test_active_surface_score(self):
        assert SURFACE_SCORES[MemoryState.ACTIVE] == 1.0

    def test_deprioritised_surface_score(self):
        assert SURFACE_SCORES[MemoryState.DEPRIORITISED] == 0.2

    def test_archived_surface_score(self):
        assert SURFACE_SCORES[MemoryState.ARCHIVED] == 0.0

    def test_deleted_surface_score(self):
        assert SURFACE_SCORES[MemoryState.DELETED] == 0.0


class TestTransition:
    def test_active_to_deprioritised(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:001",
                           "Use Redis for caching")
        result = lifecycle.transition(key, MemoryState.DEPRIORITISED, reason="outdated")
        assert result["previous_state"] == "active"
        assert result["new_state"] == "deprioritised"
        assert result["surface_score"] == 0.2

    def test_deprioritised_to_active(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:002",
                           "Valkey is preferred", state="deprioritised",
                           surface_score="0.2")
        result = lifecycle.transition(key, MemoryState.ACTIVE)
        assert result["previous_state"] == "deprioritised"
        assert result["new_state"] == "active"
        assert result["surface_score"] == 1.0

    def test_active_to_archived(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:003",
                           "Old pattern")
        result = lifecycle.transition(key, MemoryState.ARCHIVED)
        assert result["new_state"] == "archived"
        assert result["surface_score"] == 0.0

    def test_archived_to_active(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:004",
                           "Revived pattern", state="archived", surface_score="0.0")
        result = lifecycle.transition(key, MemoryState.ACTIVE)
        assert result["new_state"] == "active"
        assert result["surface_score"] == 1.0

    def test_active_to_deleted_removes_key(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:005",
                           "To be deleted")
        lifecycle.transition(key, MemoryState.DELETED)
        assert fake_store.get(key) is None

    def test_invalid_transition_raises(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:006",
                           "Archived memory", state="archived", surface_score="0.0")
        with pytest.raises(ValueError, match="Invalid transition"):
            lifecycle.transition(key, MemoryState.DEPRIORITISED)

    def test_deleted_is_terminal(self, fake_store, fake_embedder, lifecycle):
        """After deletion the key is gone, so transition raises not-found."""
        key = store_memory(fake_store, fake_embedder, "mem:episodic:007",
                           "Gone memory")
        lifecycle.transition(key, MemoryState.DELETED)
        with pytest.raises(ValueError, match="not found"):
            lifecycle.transition(key, MemoryState.ACTIVE)

    def test_missing_key_raises(self, lifecycle):
        with pytest.raises(ValueError, match="not found"):
            lifecycle.transition("mem:episodic:nonexistent", MemoryState.DEPRIORITISED)

    def test_deprioritised_stores_reason(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:008",
                           "Some memory")
        lifecycle.transition(key, MemoryState.DEPRIORITISED, reason="no longer relevant")
        data = fake_store.get(key)
        assert data["deprioritised_reason"] == "no longer relevant"

    def test_high_effort_deprioritise_warns(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:009",
                           "Battle-hardened fix", effort_score=5, outcome="succeeded")
        result = lifecycle.transition(key, MemoryState.DEPRIORITISED, reason="test")
        assert "warning" in result
        assert "effort score of 5" in result["warning"]

    def test_low_effort_deprioritise_no_warning(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:010",
                           "Simple fix", effort_score=2, outcome="succeeded")
        result = lifecycle.transition(key, MemoryState.DEPRIORITISED, reason="test")
        assert "warning" not in result

    def test_surface_score_updated_in_store(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:011",
                           "Check score update")
        lifecycle.transition(key, MemoryState.DEPRIORITISED, reason="test")
        data = fake_store.get(key)
        assert float(data["surface_score"]) == 0.2
        assert data["state"] == "deprioritised"


class TestTopicSuppression:
    def test_suppress_and_list(self, lifecycle, fake_store):
        lifecycle.suppress_topic("kubernetes")
        topics = lifecycle.get_suppressed_topics()
        assert "kubernetes" in topics

    def test_unsuppress(self, lifecycle, fake_store):
        lifecycle.suppress_topic("terraform")
        lifecycle.unsuppress_topic("terraform")
        topics = lifecycle.get_suppressed_topics()
        assert "terraform" not in topics

    def test_is_topic_suppressed(self, lifecycle, fake_store):
        lifecycle.suppress_topic("docker")
        assert lifecycle.is_topic_suppressed("We use Docker for deployment")
        assert not lifecycle.is_topic_suppressed("We use Kubernetes")

    def test_suppression_case_insensitive(self, lifecycle, fake_store):
        lifecycle.suppress_topic("REACT")
        assert lifecycle.is_topic_suppressed("We migrated to react")

    def test_cache_invalidation(self, lifecycle, fake_store):
        lifecycle.suppress_topic("vue")
        assert "vue" in lifecycle.get_suppressed_topics()
        lifecycle.unsuppress_topic("vue")
        assert "vue" not in lifecycle.get_suppressed_topics()

    def test_no_suppressed_topics(self, lifecycle):
        assert not lifecycle.is_topic_suppressed("anything")

    def test_invalidate_suppression_cache(self, lifecycle, fake_store):
        lifecycle.suppress_topic("test")
        _ = lifecycle.get_suppressed_topics()
        lifecycle.invalidate_suppression_cache()
        assert lifecycle._suppressed_cache is None


class TestReinstateHints:
    def test_add_reinstate_hints(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:020",
                           "Redis caching pattern")
        lifecycle.add_reinstate_hints(key, ["caching", "redis"])
        data = fake_store.get(key)
        hints = json.loads(data["reinstate_hints"])
        assert "caching" in hints
        assert "redis" in hints

    def test_append_to_existing_hints(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:021",
                           "Pattern memory", reinstate_hints=["original"])
        lifecycle.add_reinstate_hints(key, ["new_hint"])
        data = fake_store.get(key)
        hints = json.loads(data["reinstate_hints"])
        assert "original" in hints
        assert "new_hint" in hints

    def test_check_reinstate_eligibility_matches(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:022",
                           "Redis approach", state="deprioritised",
                           surface_score="0.2",
                           reinstate_hints=["redis", "caching"])
        assert lifecycle.check_reinstate_eligibility(key, "how to use redis")

    def test_check_reinstate_eligibility_no_match(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:023",
                           "Redis approach", state="deprioritised",
                           surface_score="0.2",
                           reinstate_hints=["redis"])
        assert not lifecycle.check_reinstate_eligibility(key, "kubernetes deployment")

    def test_reinstate_eligibility_only_for_deprioritised(self, fake_store, fake_embedder, lifecycle):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:024",
                           "Active memory", state="active",
                           reinstate_hints=["redis"])
        assert not lifecycle.check_reinstate_eligibility(key, "redis")

    def test_reinstate_eligibility_with_prefetched_data(self, fake_store, fake_embedder, lifecycle):
        doc_data = {
            "state": "deprioritised",
            "reinstate_hints": json.dumps(["kubernetes"]),
        }
        assert lifecycle.check_reinstate_eligibility(
            "mem:episodic:fake", "kubernetes setup", doc_data=doc_data
        )

    def test_reinstate_missing_key_returns_false(self, lifecycle):
        assert not lifecycle.check_reinstate_eligibility(
            "mem:episodic:nonexistent", "anything"
        )
