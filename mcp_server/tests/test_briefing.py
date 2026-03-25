"""Tests for briefing tool and its helper functions."""

import json
import time

import pytest

from tests.conftest import store_memory

import tools as tools_module


@pytest.fixture(autouse=True)
def inject_deps(fake_store, fake_embedder, lifecycle, pipeline):
    tools_module._store = fake_store
    tools_module._embedder = fake_embedder
    tools_module._lifecycle = lifecycle
    tools_module._pipeline = pipeline
    yield
    tools_module._store = None
    tools_module._embedder = None
    tools_module._lifecycle = None
    tools_module._pipeline = None


from tools.briefing import (
    _get_contradiction_warnings,
    _get_new_knowledge,
    _get_reinstate_candidates,
    _get_stale_memories,
    briefing,
)
from tools.project import set_project_context


class TestGetStaleMemories:
    def test_finds_old_memories(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "old memory")
        # Backdate the updated_at to 60 days ago
        old_ts = str(time.time() - 60 * 86400)
        fake_store.set_field("mem:episodic:01A", "updated_at", old_ts)

        stale = _get_stale_memories(fake_store, stale_days=30)
        assert len(stale) == 1
        assert stale[0]["key"] == "mem:episodic:01A"

    def test_skips_recent(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "fresh memory")
        stale = _get_stale_memories(fake_store, stale_days=30)
        assert len(stale) == 0

    def test_skips_non_active(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "archived",
            state="archived",
        )
        old_ts = str(time.time() - 60 * 86400)
        fake_store.set_field("mem:episodic:01A", "updated_at", old_ts)

        stale = _get_stale_memories(fake_store, stale_days=30)
        assert len(stale) == 0

    def test_project_filter(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "projA mem",
            project="projA",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01B", "projB mem",
            project="projB",
        )
        old_ts = str(time.time() - 60 * 86400)
        fake_store.set_field("mem:episodic:01A", "updated_at", old_ts)
        fake_store.set_field("mem:episodic:01B", "updated_at", old_ts)

        stale = _get_stale_memories(fake_store, stale_days=30, project_filter="projA")
        assert len(stale) == 1
        assert stale[0]["key"] == "mem:episodic:01A"

    def test_limited_to_10(self, fake_store, fake_embedder):
        old_ts = str(time.time() - 60 * 86400)
        for i in range(15):
            key = f"mem:episodic:{i:04d}"
            store_memory(fake_store, fake_embedder, key, f"memory {i}")
            fake_store.set_field(key, "updated_at", old_ts)

        stale = _get_stale_memories(fake_store, stale_days=30)
        assert len(stale) == 10


class TestGetNewKnowledge:
    def test_finds_recent(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:knowledge:01A", "New article about Docker",
            namespace="knowledge",
        )
        articles = _get_new_knowledge(fake_store, since_days=7)
        assert len(articles) == 1

    def test_skips_old(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:knowledge:01A", "Old article",
            namespace="knowledge",
        )
        old_ts = str(time.time() - 30 * 86400)
        fake_store.set_field("mem:knowledge:01A", "created_at", old_ts)

        articles = _get_new_knowledge(fake_store, since_days=7)
        assert len(articles) == 0

    def test_includes_source_fields(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:knowledge:01A", "Article content",
            namespace="knowledge",
        )
        fake_store.set_field("mem:knowledge:01A", "source_url", "https://example.com")
        fake_store.set_field("mem:knowledge:01A", "feed_name", "Tech Blog")

        articles = _get_new_knowledge(fake_store, since_days=7)
        assert articles[0]["source_url"] == "https://example.com"
        assert articles[0]["feed_name"] == "Tech Blog"


class TestGetReinstateCandidates:
    def test_finds_with_hints(self, fake_store, fake_embedder, lifecycle):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "deprioritised mem",
            state="deprioritised",
            reinstate_hints=["try again when arm64 runner available"],
            deprioritised_reason="too slow",
        )
        candidates = _get_reinstate_candidates(fake_store, lifecycle)
        assert len(candidates) == 1
        assert candidates[0]["reinstate_hints"] == ["try again when arm64 runner available"]

    def test_skips_active(self, fake_store, fake_embedder, lifecycle):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "active mem",
            reinstate_hints=["some hint"],
        )
        candidates = _get_reinstate_candidates(fake_store, lifecycle)
        assert len(candidates) == 0

    def test_skips_without_hints(self, fake_store, fake_embedder, lifecycle):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "depri no hints",
            state="deprioritised",
        )
        candidates = _get_reinstate_candidates(fake_store, lifecycle)
        assert len(candidates) == 0


class TestGetContradictionWarnings:
    def test_finds_contradictions(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "Use Alpine",
            contradictions=[{"key": "mem:episodic:01B"}],
        )
        warnings = _get_contradiction_warnings(fake_store)
        assert len(warnings) == 1
        assert warnings[0]["contradicts"] == ["mem:episodic:01B"]

    def test_skips_non_active(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "archived mem",
            state="archived",
            contradictions=[{"key": "mem:episodic:01B"}],
        )
        warnings = _get_contradiction_warnings(fake_store)
        assert len(warnings) == 0

    def test_project_filter(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "projA",
            project="projA",
            contradictions=[{"key": "mem:episodic:01B"}],
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01C", "projB",
            project="projB",
            contradictions=[{"key": "mem:episodic:01D"}],
        )
        warnings = _get_contradiction_warnings(fake_store, project_filter="projA")
        assert len(warnings) == 1


class TestBriefingAggregation:
    def test_includes_project_context(self):
        set_project_context("testproj", "A test project", "py", "goals", "wip")
        result = briefing(project="testproj")
        assert "project_context" in result
        assert result["project_context"]["name"] == "testproj"

    def test_project_not_found(self):
        result = briefing(project="nonexistent")
        assert result["project_context"]["note"] == "not_found"

    def test_no_project(self):
        result = briefing()
        assert "project_context" not in result

    def test_include_knowledge_false(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:knowledge:01A", "article",
            namespace="knowledge",
        )
        result = briefing(include_knowledge=False)
        assert "new_knowledge" not in result

    def test_suppressed_topics_included(self, lifecycle):
        lifecycle.suppress_topic("alpine")
        result = briefing()
        assert "suppressed_topics" in result
        assert "alpine" in result["suppressed_topics"]
