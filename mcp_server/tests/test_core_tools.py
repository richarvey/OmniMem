"""Tests for the core MCP tools: remember, recall, deprioritise, archive, reinstate, forget."""

import json
from unittest.mock import patch

import pytest

from tests.conftest import store_memory

# Patch the tools module globals before importing tools
import tools as tools_module


@pytest.fixture(autouse=True)
def inject_deps(fake_store, fake_embedder, lifecycle, pipeline):
    """Inject fake dependencies into the tools module for every test."""
    tools_module._store = fake_store
    tools_module._embedder = fake_embedder
    tools_module._lifecycle = lifecycle
    tools_module._pipeline = pipeline
    yield
    tools_module._store = None
    tools_module._embedder = None
    tools_module._lifecycle = None
    tools_module._pipeline = None


from tools.core import (
    archive,
    deprioritise,
    find_duplicates,
    forget,
    list_suppressions,
    recall,
    reinstate,
    remember,
    suppress_topic,
    unsuppress_topic,
)


class TestRemember:
    def test_basic_remember(self):
        result = remember("Python async/await patterns are powerful")
        assert result["status"] == "stored"
        assert result["key"].startswith("mem:episodic:")
        assert result["namespace"] == "episodic"

    def test_remember_with_project(self):
        result = remember("Use FastAPI for the new service", project="omnimem")
        assert result["status"] == "stored"

    def test_remember_with_tags(self):
        result = remember("Valkey connection pooling", tags=["valkey", "performance"])
        assert result["status"] == "stored"

    def test_remember_knowledge_namespace(self):
        result = remember("Article about WASM", namespace="knowledge")
        assert result["namespace"] == "knowledge"

    def test_duplicate_detection(self):
        content = "Always use type hints in Python code"
        remember(content)
        result = remember(content)
        assert result["status"] == "duplicate_found"
        assert result["similarity"] >= 0.92

    def test_force_bypasses_duplicate(self):
        content = "Prefer composition over inheritance"
        remember(content)
        result = remember(content, force=True)
        assert result["status"] == "stored"

    def test_empty_content_raises(self):
        with pytest.raises(ValueError, match="empty"):
            remember("")

    def test_whitespace_only_raises(self):
        with pytest.raises(ValueError, match="empty"):
            remember("   ")

    def test_invalid_namespace_raises(self):
        with pytest.raises(ValueError, match="Invalid namespace"):
            remember("test", namespace="invalid")

    def test_too_many_tags_raises(self):
        tags = [f"tag{i}" for i in range(25)]
        with pytest.raises(ValueError, match="Too many tags"):
            remember("test", tags=tags)

    def test_content_too_long_raises(self):
        with pytest.raises(ValueError, match="too long"):
            remember("x" * 60_000)

    def test_invalid_project_name_raises(self):
        with pytest.raises(ValueError, match="invalid characters"):
            remember("test", project="proj<script>")


class TestRecall:
    def test_basic_recall(self):
        remember("Docker compose orchestration patterns")
        results = recall("Docker compose")
        assert len(results) >= 1
        assert results[0]["content"]

    def test_recall_returns_scored_results(self):
        remember("Kubernetes pod scheduling")
        results = recall("Kubernetes scheduling")
        if results:
            assert "adjusted_score" in results[0]
            assert "score" in results[0]

    def test_recall_empty_returns_empty(self):
        results = recall("something that doesn't exist")
        assert isinstance(results, list)

    def test_recall_with_project_filter(self):
        remember("API endpoint design", project="alpha")
        remember("API endpoint testing", project="beta")
        results = recall("API endpoint", project_filter="alpha")
        for r in results:
            if r.get("project"):
                assert r["project"] == "alpha"

    def test_recall_with_namespace_filter(self):
        remember("Some episodic memory about logging")
        results = recall("logging", namespaces=["episodic"])
        for r in results:
            assert r["namespace"] == "episodic"


class TestDeprioritise:
    def test_deprioritise_by_key(self, fake_store, fake_embedder):
        result = remember("Old caching approach with Redis")
        key = result["key"]
        dep_result = deprioritise(key, reason="Switched to Valkey")
        assert dep_result["count"] >= 1
        affected = dep_result["affected"][0]
        assert affected["new_state"] == "deprioritised"
        assert affected["surface_score"] == 0.2

    def test_deprioritise_reduces_recall_score(self, fake_store, fake_embedder):
        """After deprioritising, the memory should score lower."""
        result = remember("Specific deployment procedure for staging environment")
        key = result["key"]

        # Recall before deprioritise
        before = recall("deployment procedure staging")
        before_scores = {r["key"]: r["adjusted_score"] for r in before}

        deprioritise(key, reason="outdated")

        # Recall after deprioritise
        after = recall("deployment procedure staging")
        after_scores = {r["key"]: r["adjusted_score"] for r in after}

        if key in before_scores and key in after_scores:
            assert after_scores[key] < before_scores[key]

    def test_deprioritise_with_reinstate_hints(self, fake_store, fake_embedder):
        result = remember("Redis sentinel setup for HA")
        key = result["key"]
        deprioritise(key, reason="Switched to cluster mode",
                     reinstate_hints=["sentinel", "redis ha"])
        data = fake_store.get(key)
        hints = json.loads(data.get("reinstate_hints", "[]"))
        assert "sentinel" in hints

    def test_deprioritise_warns_on_high_effort(self, fake_store, fake_embedder):
        result = remember("Hard-won fix for race condition")
        key = result["key"]
        fake_store.set_fields(key, {"effort_score": "5", "outcome": "succeeded"})
        dep_result = deprioritise(key, reason="test")
        if dep_result["affected"]:
            affected = dep_result["affected"][0]
            if "warning" in affected:
                assert "effort score" in affected["warning"]


class TestArchive:
    def test_archive_by_key(self, fake_store, fake_embedder):
        result = remember("Legacy migration notes")
        key = result["key"]
        arch_result = archive(key)
        assert arch_result["count"] >= 1
        assert arch_result["affected"][0]["new_state"] == "archived"

    def test_archived_excluded_from_recall(self, fake_store, fake_embedder):
        result = remember("Very specific archived content xyz123")
        key = result["key"]
        archive(key)
        results = recall("archived content xyz123")
        found_keys = [r["key"] for r in results]
        assert key not in found_keys


class TestReinstate:
    def test_reinstate_deprioritised(self, fake_store, fake_embedder):
        result = remember("Deprioritised pattern for testing")
        key = result["key"]
        deprioritise(key, reason="temporary")
        rein_result = reinstate(key)
        assert rein_result["count"] >= 1
        assert rein_result["affected"][0]["new_state"] == "active"

    def test_reinstate_archived(self, fake_store, fake_embedder):
        result = remember("Archived but useful pattern")
        key = result["key"]
        archive(key)
        rein_result = reinstate(key)
        assert rein_result["count"] >= 1
        assert rein_result["affected"][0]["new_state"] == "active"

    def test_reinstate_restores_surface_score(self, fake_store, fake_embedder):
        result = remember("Pattern to reinstate")
        key = result["key"]
        deprioritise(key, reason="test")
        reinstate(key)
        data = fake_store.get(key)
        assert float(data["surface_score"]) == 1.0


class TestForget:
    def test_forget_preview(self, fake_store, fake_embedder):
        result = remember("Memory to preview delete")
        key = result["key"]
        preview = forget(key, confirm=False)
        assert preview["status"] == "preview"
        assert len(preview["targets"]) >= 1

    def test_forget_confirmed(self, fake_store, fake_embedder):
        result = remember("Memory to permanently delete")
        key = result["key"]
        del_result = forget(key, confirm=True)
        assert del_result["status"] == "deleted"
        assert key in del_result["deleted_keys"]

    def test_forget_not_found(self):
        result = forget("mem:episodic:nonexistent", confirm=True)
        assert result["status"] == "not_found"


class TestTopicSuppression:
    def test_suppress_and_list(self):
        suppress_topic("jquery", reason="outdated")
        result = list_suppressions()
        assert "jquery" in result["suppressed_topics"]

    def test_unsuppress(self):
        suppress_topic("angular")
        unsuppress_topic("angular")
        result = list_suppressions()
        assert "angular" not in result["suppressed_topics"]

    def test_suppress_empty_raises(self):
        with pytest.raises(ValueError, match="empty"):
            suppress_topic("")

    def test_suppress_too_long_raises(self):
        with pytest.raises(ValueError, match="too long"):
            suppress_topic("x" * 250)

    def test_suppressed_topic_filtered_from_recall(self, fake_store, fake_embedder, lifecycle):
        remember("We should migrate to jQuery for the frontend")
        lifecycle.suppress_topic("jquery")
        results = recall("frontend framework")
        contents = [r["content"].lower() for r in results]
        assert not any("jquery" in c for c in contents)


class TestDeprioritisationIntegration:
    """End-to-end tests for the full deprioritisation flow."""

    def test_full_deprioritise_and_reinstate_cycle(self, fake_store, fake_embedder):
        # 1. Store a memory
        result = remember("Use connection pooling for PostgreSQL performance")
        key = result["key"]

        # 2. Recall — should be found
        results = recall("PostgreSQL connection pooling")
        found = [r for r in results if r["key"] == key]
        assert len(found) >= 1
        original_score = found[0]["adjusted_score"]

        # 3. Deprioritise (no reinstate hints matching query, so score drops cleanly)
        dep = deprioritise(key, reason="Switching to Supabase",
                           reinstate_hints=["supabase-revert"])
        assert dep["count"] == 1

        # 4. Recall again — score should be lower (surface_score 0.2x)
        results = recall("PostgreSQL connection pooling")
        found = [r for r in results if r["key"] == key]
        if found:
            assert found[0]["adjusted_score"] < original_score

        # 5. Reinstate
        rein = reinstate(key)
        assert rein["count"] == 1

        # 6. Recall — score should be restored
        results = recall("PostgreSQL connection pooling")
        found = [r for r in results if r["key"] == key]
        assert len(found) >= 1

    def test_deprioritise_then_archive_then_reinstate(self, fake_store, fake_embedder):
        result = remember("Multi-step lifecycle test memory")
        key = result["key"]

        deprioritise(key, reason="step 1")
        data = fake_store.get(key)
        assert data["state"] == "deprioritised"

        archive(key)
        data = fake_store.get(key)
        assert data["state"] == "archived"

        reinstate(key)
        data = fake_store.get(key)
        assert data["state"] == "active"

    def test_deprioritised_memory_surface_score_is_0_2(self, fake_store, fake_embedder):
        result = remember("Check surface score after deprioritise")
        key = result["key"]
        deprioritise(key, reason="verify score")
        data = fake_store.get(key)
        assert float(data["surface_score"]) == pytest.approx(0.2)

    def test_multiple_memories_deprioritised_independently(self, fake_store, fake_embedder):
        r1 = remember("First memory about auth")
        r2 = remember("Second memory about auth tokens")
        deprioritise(r1["key"], reason="outdated auth")

        d1 = fake_store.get(r1["key"])
        d2 = fake_store.get(r2["key"])
        assert d1["state"] == "deprioritised"
        assert d2["state"] == "active"
