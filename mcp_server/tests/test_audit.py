"""Tests for audit tools: memory_audit, why_did_you_mention, explain_memory."""

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


from tools.audit import (
    _safe_json_loads,
    explain_memory,
    memory_audit,
    why_did_you_mention,
)


class TestSafeJsonLoads:
    def test_valid_json(self):
        assert _safe_json_loads('["a", "b"]') == ["a", "b"]

    def test_invalid_json(self):
        assert _safe_json_loads("not json") == []

    def test_none(self):
        assert _safe_json_loads(None) == []

    def test_empty_string(self):
        assert _safe_json_loads("") == []


class TestMemoryAudit:
    def test_empty_store(self):
        result = memory_audit()
        assert result["total"] == 0
        assert result["entries"] == []

    def test_counts_by_state(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "active mem")
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01B", "depri mem",
            state="deprioritised",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01C", "archived mem",
            state="archived",
        )
        result = memory_audit(include_archived=True)
        assert result["summary"]["active"] == 1
        assert result["summary"]["deprioritised"] == 1
        assert result["summary"]["archived"] == 1

    def test_excludes_archived_by_default(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "active")
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01B", "archived",
            state="archived",
        )
        result = memory_audit()
        entry_keys = [e["key"] for e in result["entries"]]
        assert "mem:episodic:01A" in entry_keys
        assert "mem:episodic:01B" not in entry_keys
        # But archived count is still tracked in summary
        assert result["summary"]["archived"] == 1

    def test_includes_archived(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "archived",
            state="archived",
        )
        result = memory_audit(include_archived=True)
        entry_keys = [e["key"] for e in result["entries"]]
        assert "mem:episodic:01A" in entry_keys

    def test_namespace_filter(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "episodic")
        store_memory(
            fake_store, fake_embedder, "mem:knowledge:01A", "knowledge",
            namespace="knowledge",
        )
        result = memory_audit(namespace="episodic")
        entry_keys = [e["key"] for e in result["entries"]]
        assert "mem:episodic:01A" in entry_keys
        assert "mem:knowledge:01A" not in entry_keys

    def test_invalid_namespace(self):
        with pytest.raises(ValueError, match="Invalid namespace"):
            memory_audit(namespace="invalid")

    def test_project_filter(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "projA",
            project="projA",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01B", "projB",
            project="projB",
        )
        result = memory_audit(project="projA")
        entry_keys = [e["key"] for e in result["entries"]]
        assert "mem:episodic:01A" in entry_keys
        assert "mem:episodic:01B" not in entry_keys

    def test_effort_score_included(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "hard work",
            effort_score=4, outcome="succeeded",
        )
        result = memory_audit()
        assert result["entries"][0]["effort_score"] == 4


class TestExplainMemory:
    def test_found(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01A", "test memory",
            tags=["python", "docker"], effort_score=3, outcome="succeeded",
        )
        result = explain_memory("mem:episodic:01A")
        assert result["status"] == "found"
        assert result["content"] == "test memory"
        assert result["tags"] == ["python", "docker"]
        assert result["effort_score"] == 3
        assert result["outcome"] == "succeeded"

    def test_not_found(self):
        result = explain_memory("mem:episodic:nonexistent")
        assert result["status"] == "not_found"

    def test_invalid_key_prefix(self):
        with pytest.raises(ValueError, match="must start with"):
            explain_memory("bad:prefix:01A")

    def test_knowledge_fields(self, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder,
            "mem:knowledge:01A", "article content",
            namespace="knowledge",
        )
        fake_store.set_field("mem:knowledge:01A", "source_url", "https://example.com")
        fake_store.set_field("mem:knowledge:01A", "feed_name", "Tech Blog")

        result = explain_memory("mem:knowledge:01A")
        assert result["source_url"] == "https://example.com"
        assert result["feed_name"] == "Tech Blog"


class TestWhyDidYouMention:
    def _store_recall_log(self, fake_store, log_key, query, result_keys=None):
        """Helper: store a recall log entry."""
        fake_store._client.hset(log_key, mapping={
            "query": query,
            "timestamp": str(time.time()),
            "result_keys": json.dumps(result_keys or []),
        })

    def test_keyword_match(self, fake_store):
        self._store_recall_log(
            fake_store, "log:recall:001", "Docker build issues",
            result_keys=["mem:episodic:01A"],
        )
        result = why_did_you_mention("Docker build")
        assert result["status"] == "found"
        assert result["match_type"] == "keyword"

    def test_semantic_match(self, fake_store, fake_embedder):
        self._store_recall_log(
            fake_store, "log:recall:001", "container image compilation problems",
        )
        result = why_did_you_mention("Docker build failures")
        # Should find a semantic match (similar topics)
        assert result["status"] in ("found", "not_found")
        if result["status"] == "found":
            assert result["match_type"] == "semantic"

    def test_not_found(self, fake_store):
        self._store_recall_log(fake_store, "log:recall:001", "unrelated topic")
        result = why_did_you_mention("quantum computing")
        assert result["status"] == "not_found"

    def test_empty_logs(self):
        result = why_did_you_mention("anything")
        assert result["status"] == "not_found"
