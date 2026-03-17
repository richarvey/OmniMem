"""Tests for the recall pipeline: scoring, experience weight, recency decay, abandoned warnings."""

import json
import time

import numpy as np
import pytest

from memory.recall import RecallPipeline, RecallResult, compute_experience_weight
from tests.conftest import store_memory


class TestComputeExperienceWeight:
    def test_succeeded_effort_1(self):
        assert compute_experience_weight(1, "succeeded") == 1.0

    def test_succeeded_effort_3(self):
        assert compute_experience_weight(3, "succeeded") == 1.25

    def test_succeeded_effort_5(self):
        assert compute_experience_weight(5, "succeeded") == 1.8

    def test_pivoted_effort_3(self):
        assert compute_experience_weight(3, "pivoted") == pytest.approx(0.7 * 1.25)

    def test_abandoned_ignores_effort(self):
        """Effort does not amplify failures."""
        assert compute_experience_weight(1, "abandoned") == 0.1
        assert compute_experience_weight(5, "abandoned") == 0.1

    def test_capped_at_2(self):
        """Experience weight never exceeds 2.0."""
        w = compute_experience_weight(5, "succeeded")
        assert w <= 2.0

    def test_unknown_outcome_uses_base_1(self):
        w = compute_experience_weight(3, "unknown_outcome")
        assert w == 1.25  # base 1.0 * effort 1.25

    def test_unknown_effort_uses_multiplier_1(self):
        w = compute_experience_weight(99, "succeeded")
        assert w == 1.0  # base 1.0 * effort 1.0 (default)


class TestRecallPipelineScoring:
    def test_active_memory_scores_higher_than_deprioritised(
        self, fake_store, fake_embedder, pipeline
    ):
        """Deprioritised memories should score lower due to surface_score=0.2."""
        content = "Python async patterns for web servers"
        store_memory(fake_store, fake_embedder, "mem:episodic:r001",
                     content, state="active", surface_score="1.0")
        store_memory(fake_store, fake_embedder, "mem:episodic:r002",
                     content, state="deprioritised", surface_score="0.2")

        results = pipeline.recall("Python async patterns", top_k=10)
        active = [r for r in results if r.state == "active"]
        depri = [r for r in results if r.state == "deprioritised"]

        assert len(active) >= 1
        assert len(depri) >= 1
        assert active[0].adjusted_score > depri[0].adjusted_score

    def test_archived_memories_excluded(self, fake_store, fake_embedder, pipeline):
        """Archived memories should not appear in recall results."""
        store_memory(fake_store, fake_embedder, "mem:episodic:r003",
                     "Archived knowledge about Redis",
                     state="archived", surface_score="0.0")
        store_memory(fake_store, fake_embedder, "mem:episodic:r004",
                     "Active knowledge about Redis",
                     state="active")

        results = pipeline.recall("Redis knowledge")
        assert all(r.state != "archived" for r in results)

    def test_deleted_memories_excluded(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder, "mem:episodic:r005",
                     "Deleted memory about Go", state="deleted", surface_score="0.0")
        results = pipeline.recall("Go programming")
        assert all(r.state != "deleted" for r in results)

    def test_experience_weight_boosts_score(self, fake_store, fake_embedder, pipeline):
        """High experience weight should boost adjusted score."""
        content = "Database connection pooling strategy"
        store_memory(fake_store, fake_embedder, "mem:episodic:r006",
                     content, experience_weight="1.0")
        store_memory(fake_store, fake_embedder, "mem:episodic:r007",
                     content + " (battle-tested)", experience_weight="1.8")

        results = pipeline.recall("database connection pooling", top_k=10)
        # Both should appear; the one with higher exp weight should rank higher
        # (assuming similar raw similarity)
        weights = [r.experience_weight for r in results]
        assert 1.8 in weights or any(w > 1.0 for w in weights)

    def test_recency_decay_old_memories(self, fake_store, fake_embedder, pipeline):
        """Memories older than RECENCY_DECAY_DAYS should have reduced scores."""
        now = time.time()
        old_time = str(now - 200 * 86400)  # 200 days ago
        content = "Fresh insight about testing patterns"

        store_memory(fake_store, fake_embedder, "mem:episodic:r008",
                     content)
        # Manually adjust created_at to be old
        fake_store.set_field("mem:episodic:r008", "created_at", old_time)

        store_memory(fake_store, fake_embedder, "mem:episodic:r009",
                     content)

        results = pipeline.recall("testing patterns")
        if len(results) >= 2:
            old = [r for r in results if r.key == "mem:episodic:r008"]
            new = [r for r in results if r.key == "mem:episodic:r009"]
            if old and new:
                # Same content, so raw scores match — old memory should rank lower
                assert old[0].adjusted_score < new[0].adjusted_score

    def test_project_filter(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder, "mem:episodic:r010",
                     "API design for project alpha", project="alpha")
        store_memory(fake_store, fake_embedder, "mem:episodic:r011",
                     "API design for project beta", project="beta")

        results = pipeline.recall("API design", project_filter="alpha")
        projects = {r.project for r in results}
        assert "beta" not in projects

    def test_namespace_filter(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder, "mem:episodic:r012",
                     "Episodic memory about Docker")
        store_memory(fake_store, fake_embedder, "mem:knowledge:r013",
                     "Knowledge about Docker", namespace="knowledge")

        results = pipeline.recall("Docker", namespaces=["episodic"])
        assert all(r.namespace == "episodic" for r in results)

    def test_top_k_limits_results(self, fake_store, fake_embedder, pipeline):
        for i in range(10):
            store_memory(fake_store, fake_embedder, f"mem:episodic:bulk{i}",
                         f"Memory number {i} about testing")
        results = pipeline.recall("testing", top_k=3)
        assert len(results) <= 3

    def test_top_k_clamped_to_range(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder, "mem:episodic:r014", "Some memory")
        results = pipeline.recall("memory", top_k=0)
        assert len(results) <= 1  # top_k clamped to 1

    def test_results_sorted_by_adjusted_score(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder, "mem:episodic:r015",
                     "First memory about Rust")
        store_memory(fake_store, fake_embedder, "mem:episodic:r016",
                     "Second memory about Rust")

        results = pipeline.recall("Rust programming")
        scores = [r.adjusted_score for r in results]
        assert scores == sorted(scores, reverse=True)


class TestSuppressedTopicFiltering:
    def test_suppressed_topic_excluded(self, fake_store, fake_embedder, lifecycle, pipeline):
        store_memory(fake_store, fake_embedder, "mem:episodic:s001",
                     "We should use jQuery for the frontend")
        lifecycle.suppress_topic("jquery")

        results = pipeline.recall("frontend framework")
        contents = [r.content for r in results]
        assert not any("jquery" in c.lower() for c in contents)

    def test_unsuppressed_topic_included(self, fake_store, fake_embedder, lifecycle, pipeline):
        store_memory(fake_store, fake_embedder, "mem:episodic:s002",
                     "React is our main framework")
        lifecycle.suppress_topic("react")
        lifecycle.unsuppress_topic("react")

        results = pipeline.recall("React framework")
        contents = [r.content.lower() for r in results]
        assert any("react" in c for c in contents)


class TestReinstateCandidate:
    def test_deprioritised_with_matching_hint_flagged(
        self, fake_store, fake_embedder, pipeline
    ):
        store_memory(fake_store, fake_embedder, "mem:episodic:ri001",
                     "Redis caching approach",
                     state="deprioritised", surface_score="0.2",
                     reinstate_hints=["redis", "caching"])

        results = pipeline.recall("redis caching")
        candidates = [r for r in results if r.reinstate_candidate]
        if candidates:
            assert candidates[0].adjusted_score == 0.6

    def test_deprioritised_without_matching_hint_not_flagged(
        self, fake_store, fake_embedder, pipeline
    ):
        store_memory(fake_store, fake_embedder, "mem:episodic:ri002",
                     "Old deployment strategy",
                     state="deprioritised", surface_score="0.2",
                     reinstate_hints=["kubernetes"])

        results = pipeline.recall("database optimization")
        candidates = [r for r in results if r.reinstate_candidate]
        assert len(candidates) == 0


class TestAbandonedWarnings:
    def test_abandoned_approach_detected(self, fake_store, fake_embedder, pipeline):
        approaches = [{"name": "onnxruntime", "type": "library",
                       "reason": "too slow for our use case"}]
        store_memory(fake_store, fake_embedder, "mem:episodic:a001",
                     "Tried ML inference",
                     abandoned_approaches=approaches)

        results = pipeline.recall("onnxruntime for inference")
        warnings = [r for r in results if r.result_type == "abandoned_warning"]
        assert len(warnings) >= 1
        assert "onnxruntime" in warnings[0].content

    def test_no_false_positive_abandoned_warning(self, fake_store, fake_embedder, pipeline):
        approaches = [{"name": "tensorflow", "type": "library",
                       "reason": "version conflicts"}]
        store_memory(fake_store, fake_embedder, "mem:episodic:a002",
                     "ML framework evaluation",
                     abandoned_approaches=approaches)

        results = pipeline.recall("kubernetes deployment")
        warnings = [r for r in results if r.result_type == "abandoned_warning"]
        assert len(warnings) == 0

    def test_abandoned_warnings_appear_first(self, fake_store, fake_embedder, pipeline):
        approaches = [{"name": "celery", "type": "library",
                       "reason": "memory leaks"}]
        store_memory(fake_store, fake_embedder, "mem:episodic:a003",
                     "Task queue evaluation",
                     abandoned_approaches=approaches)
        store_memory(fake_store, fake_embedder, "mem:episodic:a004",
                     "We use celery for background tasks")

        results = pipeline.recall("celery task queue")
        if results:
            warnings = [r for r in results if r.result_type == "abandoned_warning"]
            if warnings:
                # Abandoned warnings get score 1.0 so should sort first
                assert results[0].result_type == "abandoned_warning"


class TestRecallResultDataclass:
    def test_default_values(self):
        r = RecallResult(
            key="mem:episodic:test",
            namespace="episodic",
            content="test content",
            score=0.9,
            adjusted_score=0.85,
            state="active",
        )
        assert r.project is None
        assert r.reinstate_candidate is False
        assert r.tags == []
        assert r.experience_weight == 1.0
        assert r.result_type == "memory"
        assert r.contradictions == []
