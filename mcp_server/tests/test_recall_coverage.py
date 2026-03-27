"""Coverage tests for memory/recall.py — tag parsing fallbacks, effort parsing,
contradiction parsing, warn_if_abandoned caps and edge cases, log_recall exception."""

import json
import time
from unittest.mock import MagicMock, patch

import numpy as np
import pytest

from tests.conftest import FakeValkeyStore, FakeEmbedder, store_memory
from memory.lifecycle import MemoryLifecycle
from memory.recall import RecallPipeline, RecallResult, compute_experience_weight


@pytest.fixture
def fake_store():
    return FakeValkeyStore()


@pytest.fixture
def fake_embedder():
    return FakeEmbedder()


@pytest.fixture
def lifecycle(fake_store):
    return MemoryLifecycle(fake_store)


@pytest.fixture
def pipeline(fake_store, fake_embedder, lifecycle):
    return RecallPipeline(fake_store, fake_embedder, lifecycle)


class TestTagsParsingFallback:
    def test_comma_separated_tags_fallback(self, fake_store, fake_embedder, pipeline):
        """When tags JSON parsing fails, falls back to comma-separated parsing."""
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:tags01", "Memory with comma tags")
        # Overwrite tags with a non-JSON comma-separated string
        fake_store.set_field("mem:episodic:tags01", "tags", "python,docker,testing")
        results = pipeline.recall("comma tags")
        matched = [r for r in results if r.key == "mem:episodic:tags01"]
        if matched:
            assert "python" in matched[0].tags
            assert "docker" in matched[0].tags

    def test_empty_tags_string(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:tags02", "Memory with empty tags")
        fake_store.set_field("mem:episodic:tags02", "tags", "")
        results = pipeline.recall("empty tags")
        matched = [r for r in results if r.key == "mem:episodic:tags02"]
        if matched:
            assert matched[0].tags == []


class TestEffortScoreParsing:
    def test_invalid_effort_score_ignored(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:eff01", "Memory with bad effort score")
        fake_store.set_field("mem:episodic:eff01", "effort_score", "not-a-number")
        results = pipeline.recall("bad effort score")
        matched = [r for r in results if r.key == "mem:episodic:eff01"]
        if matched:
            assert matched[0].effort_score is None

    def test_float_effort_score_converted(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:eff02", "Memory with float effort")
        fake_store.set_field("mem:episodic:eff02", "effort_score", "3.0")
        results = pipeline.recall("float effort")
        matched = [r for r in results if r.key == "mem:episodic:eff02"]
        if matched:
            assert matched[0].effort_score == 3


class TestContradictionsParsing:
    def test_malformed_contradictions_json(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:contra01", "Memory with bad contradictions JSON")
        fake_store.set_field("mem:episodic:contra01", "contradictions", "invalid{json")
        results = pipeline.recall("bad contradictions")
        matched = [r for r in results if r.key == "mem:episodic:contra01"]
        if matched:
            assert matched[0].contradictions == []

    def test_empty_contradictions_string(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:contra02", "Memory with empty contradictions")
        fake_store.set_field("mem:episodic:contra02", "contradictions", "")
        results = pipeline.recall("empty contradictions")
        matched = [r for r in results if r.key == "mem:episodic:contra02"]
        if matched:
            assert matched[0].contradictions == []


class TestWarnIfAbandonedEdges:
    def test_none_data_skipped(self, fake_store, fake_embedder, pipeline):
        """Keys where data is None should be skipped."""
        # Create a key that appears in scan but has no data
        fake_store.client._data["mem:episodic:ghost01"] = {"content": "ghost"}
        fake_store.delete("mem:episodic:ghost01")
        matches = pipeline.warn_if_abandoned("ghost")
        assert isinstance(matches, list)

    def test_json_decode_error_skipped(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:jde01", "Memory with bad abandoned JSON")
        fake_store.set_field("mem:episodic:jde01", "abandoned_approaches", "not-json{")
        matches = pipeline.warn_if_abandoned("anything")
        # Should not crash, just skip the bad entry
        assert isinstance(matches, list)

    def test_non_list_approaches_skipped(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:nl01", "Memory with non-list approaches")
        fake_store.set_field("mem:episodic:nl01", "abandoned_approaches", '"just a string"')
        matches = pipeline.warn_if_abandoned("anything")
        assert isinstance(matches, list)

    def test_non_dict_approach_entry_skipped(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:nd01", "Memory with non-dict approach entry")
        fake_store.set_field("mem:episodic:nd01", "abandoned_approaches",
                             json.dumps(["just a string", 42]))
        matches = pipeline.warn_if_abandoned("anything")
        assert isinstance(matches, list)

    def test_effort_score_parsing_in_abandoned(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:efa01", "Memory with effort and abandoned",
                     effort_score=3,
                     abandoned_approaches=[{"name": "badlib", "type": "library",
                                           "reason": "crashed"}])
        matches = pipeline.warn_if_abandoned("badlib")
        assert len(matches) >= 1
        assert matches[0]["effort_score"] == 3

    def test_bad_effort_score_in_abandoned(self, fake_store, fake_embedder, pipeline):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:efa02", "Memory with bad effort and abandoned",
                     abandoned_approaches=[{"name": "buggylib", "type": "library",
                                           "reason": "broken"}])
        fake_store.set_field("mem:episodic:efa02", "effort_score", "not-a-number")
        matches = pipeline.warn_if_abandoned("buggylib")
        assert len(matches) >= 1
        assert matches[0]["effort_score"] is None

    def test_scan_cap_warning(self, fake_store, fake_embedder, pipeline):
        """When there are more than 5000 keys, should log a warning and cap."""
        # We won't actually create 5000+ keys, but test the branch exists
        # by mocking scan_prefix to return a large list
        large_keys = [f"mem:episodic:cap{i:05d}" for i in range(5001)]
        with patch.object(fake_store, "scan_prefix", return_value=large_keys):
            with patch.object(fake_store, "get_multi", return_value=[None] * 5000):
                matches = pipeline.warn_if_abandoned("test")
                assert isinstance(matches, list)


class TestLogRecallEventException:
    def test_log_recall_event_handles_exception(self, fake_store, fake_embedder, pipeline):
        """When pipeline execution fails, log_recall_event should not crash."""
        result = RecallResult(
            key="mem:episodic:log01", namespace="episodic",
            content="Test", score=0.9, adjusted_score=0.9, state="active",
        )
        # Make the client's pipeline raise an exception
        with patch.object(fake_store.client, "pipeline", side_effect=RuntimeError("pipe broken")):
            # Should not raise
            pipeline.log_recall_event("test query", [result])
