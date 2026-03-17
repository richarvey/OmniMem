"""Tests for experience scoring tools."""

import json
from unittest.mock import patch

import pytest

import tools as tools_module
from tests.conftest import store_memory


@pytest.fixture(autouse=True)
def inject_deps(fake_store, fake_embedder, lifecycle, pipeline):
    """Inject fake dependencies into the tools module."""
    tools_module._store = fake_store
    tools_module._embedder = fake_embedder
    tools_module._lifecycle = lifecycle
    tools_module._pipeline = pipeline
    yield
    tools_module._store = None
    tools_module._embedder = None
    tools_module._lifecycle = None
    tools_module._pipeline = None


from tools.experience import (
    experience_summary,
    get_experience,
    log_abandoned,
    record_experience,
    warn_if_abandoned,
)
from tools.core import remember


class TestRecordExperience:
    def test_basic_experience(self, fake_store):
        result = remember("Solved the flaky test issue")
        key = result["key"]
        exp = record_experience(key, effort_score=3, outcome="succeeded")
        assert exp["status"] == "recorded"
        assert exp["effort_score"] == 3
        assert exp["outcome"] == "succeeded"
        assert exp["experience_weight"] == 1.25  # effort 3, succeeded

    def test_high_effort_succeeded_weight(self, fake_store):
        result = remember("Fixed race condition in worker pool")
        key = result["key"]
        exp = record_experience(key, effort_score=5, outcome="succeeded")
        assert exp["experience_weight"] == 1.8

    def test_abandoned_weight_ignores_effort(self, fake_store):
        result = remember("Tried microservices approach")
        key = result["key"]
        exp = record_experience(key, effort_score=5, outcome="abandoned")
        assert exp["experience_weight"] == 0.1

    def test_pivoted_weight(self, fake_store):
        result = remember("Started with REST, pivoted to GraphQL")
        key = result["key"]
        exp = record_experience(key, effort_score=3, outcome="pivoted")
        assert exp["experience_weight"] == pytest.approx(0.7 * 1.25)

    def test_with_abandoned_approaches(self, fake_store):
        result = remember("Final solution for image processing")
        key = result["key"]
        approaches = [
            {"name": "pillow", "type": "library", "reason": "too slow"},
            {"name": "opencv", "type": "library", "reason": "too complex"},
        ]
        exp = record_experience(
            key, effort_score=4, outcome="succeeded",
            iterations=5, abandoned_approaches=approaches,
            breakthrough="Used libvips via pyvips",
        )
        assert exp["status"] == "recorded"
        data = fake_store.get(key)
        abandoned = json.loads(data["abandoned_approaches"])
        assert len(abandoned) == 2
        assert data["breakthrough"] == "Used libvips via pyvips"

    def test_auto_suppression_on_high_effort_abandoned(self, fake_store, lifecycle):
        result = remember("Failed attempt at custom ORM")
        key = result["key"]
        approaches = [
            {"name": "custom-orm", "type": "approach", "reason": "unmaintainable"},
        ]
        exp = record_experience(
            key, effort_score=4, outcome="abandoned",
            abandoned_approaches=approaches,
        )
        assert "auto_suppressed" in exp
        assert "custom-orm" in exp["auto_suppressed"]
        # Topic should be suppressed
        topics = lifecycle.get_suppressed_topics()
        assert "custom-orm" in topics

    def test_no_auto_suppression_on_low_effort(self, fake_store, lifecycle):
        result = remember("Quick test of alternative")
        key = result["key"]
        approaches = [
            {"name": "quick-lib", "type": "library", "reason": "didn't fit"},
        ]
        exp = record_experience(
            key, effort_score=2, outcome="abandoned",
            abandoned_approaches=approaches,
        )
        assert "auto_suppressed" not in exp

    def test_no_auto_suppression_on_succeeded(self, fake_store, lifecycle):
        result = remember("Successful high effort fix")
        key = result["key"]
        approaches = [
            {"name": "old-approach", "type": "approach", "reason": "replaced"},
        ]
        exp = record_experience(
            key, effort_score=5, outcome="succeeded",
            abandoned_approaches=approaches,
        )
        assert "auto_suppressed" not in exp

    def test_invalid_effort_score_raises(self, fake_store):
        result = remember("Test memory")
        with pytest.raises(ValueError, match="effort_score must be 1-5"):
            record_experience(result["key"], effort_score=0, outcome="succeeded")

    def test_invalid_outcome_raises(self, fake_store):
        result = remember("Test memory")
        with pytest.raises(ValueError, match="outcome must be"):
            record_experience(result["key"], effort_score=3, outcome="unknown")

    def test_missing_key_raises(self, fake_store):
        with pytest.raises(ValueError, match="not found"):
            record_experience("mem:episodic:nonexistent",
                              effort_score=3, outcome="succeeded")

    def test_invalid_key_prefix_raises(self):
        with pytest.raises(ValueError, match="mem:"):
            record_experience("invalid:key", effort_score=3, outcome="succeeded")

    def test_gotchas_stored(self, fake_store):
        result = remember("Memory with gotchas")
        key = result["key"]
        record_experience(
            key, effort_score=3, outcome="succeeded",
            gotchas="Watch out for race conditions on startup",
        )
        data = fake_store.get(key)
        assert data["gotchas"] == "Watch out for race conditions on startup"


class TestLogAbandoned:
    def test_log_single_abandoned(self, fake_store):
        result = remember("Working on ML pipeline")
        key = result["key"]
        log = log_abandoned(key, name="tensorflow", type="library",
                            reason="version conflicts")
        assert log["abandoned_count"] == 1
        assert log["latest_entry"]["name"] == "tensorflow"

    def test_log_multiple_abandoned(self, fake_store):
        result = remember("Evaluating frameworks")
        key = result["key"]
        log_abandoned(key, name="flask", type="library", reason="too minimal")
        log = log_abandoned(key, name="django", type="library", reason="too heavy")
        assert log["abandoned_count"] == 2

    def test_invalid_type_raises(self, fake_store):
        result = remember("Test")
        with pytest.raises(ValueError, match="type must be"):
            log_abandoned(result["key"], name="x", type="invalid", reason="test")


class TestGetExperience:
    def test_get_recorded_experience(self, fake_store):
        result = remember("Memory with experience data")
        key = result["key"]
        record_experience(key, effort_score=4, outcome="succeeded",
                          iterations=3, breakthrough="Used async batching")
        exp = get_experience(key)
        assert exp["status"] == "found"
        assert exp["effort_score"] == 4
        assert exp["outcome"] == "succeeded"
        assert exp["iterations"] == 3
        assert exp["breakthrough"] == "Used async batching"

    def test_get_experience_not_found(self, fake_store):
        result = remember("Memory without experience")
        exp = get_experience(result["key"])
        assert exp["status"] == "no_experience"

    def test_get_experience_missing_key(self, fake_store):
        exp = get_experience("mem:episodic:nonexistent")
        assert exp["status"] == "not_found"


class TestExperienceSummary:
    def test_summary_with_data(self, fake_store):
        r1 = remember("Easy fix for logging")
        record_experience(r1["key"], effort_score=1, outcome="succeeded")

        r2 = remember("Hard database migration")
        record_experience(r2["key"], effort_score=5, outcome="succeeded",
                          breakthrough="Incremental migration with feature flags")

        r3 = remember("Failed attempt at real-time sync")
        record_experience(r3["key"], effort_score=4, outcome="abandoned",
                          abandoned_approaches=[
                              {"name": "websockets", "type": "approach",
                               "reason": "too complex for our needs"}
                          ])

        summary = experience_summary()
        assert summary["memories_with_experience"] == 3
        assert summary["outcome_breakdown"]["succeeded"] == 2
        assert summary["outcome_breakdown"]["abandoned"] == 1
        assert len(summary["graveyard"]) >= 1
        assert len(summary["top_3_breakthroughs"]) >= 1

    def test_summary_empty(self, fake_store):
        summary = experience_summary()
        assert summary["memories_with_experience"] == 0
        assert summary["average_effort_score"] == 0

    def test_summary_with_project_filter(self, fake_store):
        r1 = remember("Fix for project alpha", project="alpha")
        record_experience(r1["key"], effort_score=3, outcome="succeeded")

        r2 = remember("Fix for project beta", project="beta")
        record_experience(r2["key"], effort_score=2, outcome="succeeded")

        summary = experience_summary(project="alpha")
        assert summary["memories_with_experience"] == 1


class TestWarnIfAbandoned:
    def test_warns_on_match(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:w001",
                     "ML pipeline evaluation",
                     abandoned_approaches=[
                         {"name": "onnxruntime", "type": "library",
                          "reason": "poor GPU support"}
                     ])
        result = warn_if_abandoned("onnxruntime")
        assert result["status"] == "warning"
        assert len(result["matches"]) >= 1

    def test_clear_on_no_match(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:w002",
                     "Some unrelated memory")
        result = warn_if_abandoned("completely-unrelated-library")
        assert result["status"] == "clear"
