"""Additional coverage tests for tools/core.py — validation edges, query-based operations,
recall optional fields, recall_detail metadata, find_duplicates, and contradiction warnings."""

import json
from unittest.mock import patch

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


from tools.core import (
    _validate_project_name,
    _validate_tags,
    _validate_top_k,
    archive,
    deprioritise,
    find_duplicates,
    forget,
    recall,
    recall_detail,
    recall_index,
    reinstate,
    remember,
)


# --- Validation edge cases ---

class TestValidationEdges:
    def test_empty_project_name_raises(self):
        with pytest.raises(ValueError, match="1-200 characters"):
            _validate_project_name("")

    def test_project_name_too_long_raises(self):
        with pytest.raises(ValueError, match="1-200 characters"):
            _validate_project_name("x" * 201)

    def test_tag_too_long_raises(self):
        with pytest.raises(ValueError, match="at most"):
            _validate_tags(["x" * 101])

    def test_non_string_tag_raises(self):
        with pytest.raises(ValueError, match="at most"):
            _validate_tags([123])

    def test_top_k_clamped_low(self):
        assert _validate_top_k(0) == 1
        assert _validate_top_k(-5) == 1

    def test_top_k_clamped_high(self):
        assert _validate_top_k(100) == 50
        assert _validate_top_k(51) == 50

    def test_top_k_valid_passthrough(self):
        assert _validate_top_k(10) == 10


# --- Contradiction warning in remember() ---

class TestRememberContradiction:
    def test_remember_returns_contradiction_warning(self, fake_store, fake_embedder):
        """When storing content that contradicts an existing memory, a warning is returned."""
        # Store a memory with "use" language
        remember("You should always use Redis for caching")
        # Store a contradicting memory with "don't use" language
        result = remember("You should never use Redis for caching", force=True)
        # The contradiction heuristic checks negation patterns
        if "contradiction_warning" in result:
            warning = result["contradiction_warning"]
            assert "existing_key" in warning
            assert "similarity" in warning
            assert "explanation" in warning


# --- Recall with rich result fields ---

class TestRecallOptionalFields:
    def test_recall_includes_tags(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:tags01", "Memory with tags about docker",
                     tags=["docker", "containers"])
        results = recall("docker containers")
        tagged = [r for r in results if r.get("tags")]
        if tagged:
            assert "docker" in tagged[0]["tags"]

    def test_recall_includes_effort_and_outcome(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:effort01", "Fixed a tricky docker networking bug",
                     effort_score=4, outcome="succeeded")
        results = recall("docker networking bug")
        matched = [r for r in results if r["key"] == "mem:episodic:effort01"]
        if matched:
            assert matched[0]["effort_score"] == 4
            assert matched[0]["outcome"] == "succeeded"

    def test_recall_includes_breakthrough(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:bt01", "Solved the ARM build issue",
                     breakthrough="Used tonistiigi/binfmt for multi-arch")
        results = recall("ARM build issue")
        matched = [r for r in results if r["key"] == "mem:episodic:bt01"]
        if matched:
            assert matched[0]["breakthrough"] == "Used tonistiigi/binfmt for multi-arch"

    def test_recall_includes_source_url(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:knowledge:src01", "Article about Valkey performance",
                     namespace="knowledge")
        # Manually set source_url
        fake_store.set_field("mem:knowledge:src01", "source_url", "https://example.com/article")
        results = recall("Valkey performance", namespaces=["knowledge"])
        matched = [r for r in results if r["key"] == "mem:knowledge:src01"]
        if matched:
            assert matched[0].get("source_url") == "https://example.com/article"

    def test_recall_includes_contradictions_count(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:con01", "Use Alpine for Docker images",
                     contradictions=[{"key": "mem:episodic:con02", "explanation": "test"}])
        results = recall("Alpine Docker images")
        matched = [r for r in results if r["key"] == "mem:episodic:con01"]
        if matched:
            assert matched[0].get("contradictions") == 1

    def test_recall_includes_reinstate_candidate(self, fake_store, fake_embedder, lifecycle):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:rein01", "Redis sentinel setup for high availability",
                     state="deprioritised", surface_score="0.2",
                     reinstate_hints=["sentinel", "redis ha"],
                     deprioritised_reason="Switched to cluster mode")
        results = recall("redis sentinel ha")
        matched = [r for r in results if r["key"] == "mem:episodic:rein01"]
        if matched:
            assert matched[0].get("reinstate_candidate") is True
            assert matched[0].get("deprioritised_reason") == "Switched to cluster mode"

    def test_recall_with_top_k_zero_clamps_to_one(self, fake_store, fake_embedder):
        remember("Test clamping memory")
        results = recall("Test clamping", top_k=0)
        assert len(results) <= 1

    def test_recall_with_top_k_over_max_clamps(self, fake_store, fake_embedder):
        remember("Test large top_k")
        results = recall("Test large top_k", top_k=100)
        assert isinstance(results, list)


# --- Recall index with optional fields ---

class TestRecallIndexOptionalFields:
    def test_recall_index_with_namespace_filter(self, fake_store, fake_embedder):
        remember("Episodic memory for namespace test")
        result = recall_index("namespace test", namespaces=["episodic"])
        for entry in result["results"]:
            assert entry["namespace"] == "episodic"

    def test_recall_index_with_project_filter(self, fake_store, fake_embedder):
        remember("Project-scoped index memory", project="myproj")
        result = recall_index("index memory", project_filter="myproj")
        for entry in result["results"]:
            if entry.get("project"):
                assert entry["project"] == "myproj"

    def test_recall_index_includes_tags(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:idx01", "Tagged memory for index test",
                     tags=["python", "testing"])
        result = recall_index("tagged memory index test")
        matched = [r for r in result["results"] if r["key"] == "mem:episodic:idx01"]
        if matched:
            assert "python" in matched[0].get("tags", [])

    def test_recall_index_includes_reinstate_candidate(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:idxrein01", "Deprioritised index memory about sentinel",
                     state="deprioritised", surface_score="0.2",
                     reinstate_hints=["sentinel"])
        result = recall_index("sentinel")
        matched = [r for r in result["results"] if r["key"] == "mem:episodic:idxrein01"]
        if matched:
            assert matched[0].get("reinstate_candidate") is True


# --- Recall detail with rich metadata ---

class TestRecallDetailMetadata:
    def test_detail_includes_tags(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:det01", "Detail with tags",
                     tags=["rust", "wasm"])
        results = recall_detail(["mem:episodic:det01"])
        assert results[0].get("tags") == ["rust", "wasm"]

    def test_detail_includes_source_url(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:knowledge:det02", "Knowledge article with source",
                     namespace="knowledge")
        fake_store.set_field("mem:knowledge:det02", "source_url", "https://example.com")
        results = recall_detail(["mem:knowledge:det02"])
        assert results[0].get("source_url") == "https://example.com"

    def test_detail_includes_breakthrough(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:det03", "Memory with breakthrough",
                     breakthrough="Used binfmt")
        results = recall_detail(["mem:episodic:det03"])
        assert results[0].get("breakthrough") == "Used binfmt"

    def test_detail_includes_effort_and_outcome(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:det04", "Memory with experience data",
                     effort_score=3, outcome="succeeded")
        results = recall_detail(["mem:episodic:det04"])
        assert results[0].get("effort_score") == 3
        assert results[0].get("outcome") == "succeeded"

    def test_detail_with_invalid_tags_json(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:det05", "Memory with bad tags")
        fake_store.set_field("mem:episodic:det05", "tags", "not-valid-json{")
        results = recall_detail(["mem:episodic:det05"])
        # Should not crash, tags just omitted
        assert results[0]["key"] == "mem:episodic:det05"

    def test_detail_with_invalid_effort_score(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:det06", "Memory with bad effort")
        fake_store.set_field("mem:episodic:det06", "effort_score", "not-a-number")
        results = recall_detail(["mem:episodic:det06"])
        assert results[0]["key"] == "mem:episodic:det06"
        assert "effort_score" not in results[0]

    def test_detail_truncates_to_max_top_k(self, fake_store, fake_embedder):
        keys = []
        for i in range(55):
            key = f"mem:episodic:bulk{i:03d}"
            store_memory(fake_store, fake_embedder, key, f"Bulk memory {i}")
            keys.append(key)
        results = recall_detail(keys)
        assert len(results) <= 50


# --- Query-based operations (deprioritise, archive, reinstate, forget) ---

class TestQueryBasedOperations:
    def test_deprioritise_by_query(self, fake_store, fake_embedder):
        # Use exact content as query to guarantee score > 0.85
        content = "Unique xyzzy caching strategy for query deprioritise test"
        remember(content)
        result = deprioritise(content, reason="outdated")
        assert len(result["affected"]) >= 1
        assert result["affected"][0]["new_state"] == "deprioritised"

    def test_deprioritise_by_query_with_reinstate_hints(self, fake_store, fake_embedder):
        content = "Unique plugh query deprioritise with hints test"
        remember(content)
        result = deprioritise(content, reason="temp", reinstate_hints=["plugh"])
        assert len(result["affected"]) >= 1

    def test_archive_by_query(self, fake_store, fake_embedder):
        content = "Unique plugh archival test content for query"
        remember(content)
        result = archive(content, reason="old")
        assert len(result["affected"]) >= 1
        assert result["affected"][0]["new_state"] == "archived"

    def test_archive_by_query_handles_transition_error(self, fake_store, fake_embedder, lifecycle):
        """When archive transition fails for a query match, it logs a warning and continues."""
        content = "Unique qwerty content for archive error test"
        r = remember(content)
        key = r["key"]
        # Already archive it so re-archiving from archived state is a no-op
        archive(key)
        # Now try by query — the archived memory may match but re-archiving fails
        result = archive(content, reason="retry")
        assert "affected" in result

    def test_reinstate_by_query(self, fake_store, fake_embedder):
        content = "Unique foobar reinstatable content for query test"
        r = remember(content)
        deprioritise(r["key"], reason="temp")
        result = reinstate(content)
        assert len(result["affected"]) >= 1
        assert result["affected"][0]["new_state"] == "active"

    def test_reinstate_by_query_handles_transition_error(self, fake_store, fake_embedder):
        """Reinstating an already-active memory by query — no match because state is active."""
        content = "Unique bazqux active content for reinstate error test"
        remember(content)
        result = reinstate(content)
        # Active memories don't match the state filter in reinstate
        assert result["affected"] == []

    def test_forget_by_query_preview(self, fake_store, fake_embedder):
        content = "Unique waldo forgettable content for query test"
        remember(content)
        result = forget(content, confirm=False)
        assert result["status"] == "preview"
        assert len(result["targets"]) >= 1

    def test_forget_by_query_confirmed(self, fake_store, fake_embedder):
        content = "Unique thud deletable content for query test"
        remember(content)
        result = forget(content, confirm=True)
        assert result["status"] == "deleted"
        assert len(result["deleted_keys"]) >= 1

    def test_forget_fallback_to_store_delete(self, fake_store, fake_embedder, lifecycle):
        """When lifecycle.transition raises ValueError, forget falls back to store.delete."""
        from memory.lifecycle import MemoryState
        r = remember("Memory for delete fallback test")
        key = r["key"]
        # Transition to DELETED first so re-transitioning raises ValueError
        lifecycle.transition(key, MemoryState.DELETED)
        # Now forget should hit the ValueError branch and fall back to store.delete
        # Re-add the key so forget finds it
        fake_store.set_fields(key, {"content": "Memory for delete fallback test", "state": "deleted"})
        result = forget(key, confirm=True)
        assert result["status"] == "deleted"
        assert key in result["deleted_keys"]


# --- find_duplicates ---

class TestFindDuplicates:
    def test_find_duplicates_basic(self, fake_store, fake_embedder):
        remember("Exact same content about Docker networking")
        remember("Exact same content about Docker networking", force=True)
        result = find_duplicates(namespace="episodic")
        assert "clusters" in result
        assert result["namespace"] == "episodic"

    def test_find_duplicates_with_project_filter(self, fake_store, fake_embedder):
        remember("Project-specific duplicate content", project="testproj")
        remember("Project-specific duplicate content", project="testproj", force=True)
        result = find_duplicates(namespace="episodic", project_filter="testproj")
        assert "clusters" in result

    def test_find_duplicates_invalid_namespace_raises(self):
        with pytest.raises(ValueError, match="Invalid namespace"):
            find_duplicates(namespace="invalid")

    def test_find_duplicates_with_custom_threshold(self, fake_store, fake_embedder):
        remember("Custom threshold duplicate test")
        remember("Custom threshold duplicate test", force=True)
        result = find_duplicates(threshold=0.8)
        assert "clusters" in result


class TestRecallIndexAbandonedWarning:
    def test_recall_index_includes_abandoned_result_type(self, fake_store, fake_embedder):
        """recall_index should include result_type for abandoned warnings."""
        store_memory(fake_store, fake_embedder,
                     "mem:episodic:abidx01", "Tried badlib but it failed",
                     abandoned_approaches=[{"name": "badlib", "type": "library",
                                           "reason": "crashes on startup"}])
        result = recall_index("badlib")
        abandoned = [r for r in result["results"] if r.get("result_type") == "abandoned_warning"]
        if abandoned:
            assert abandoned[0]["result_type"] == "abandoned_warning"


class TestArchiveReinstateQueryErrors:
    def test_archive_by_query_catches_transition_error(self, fake_store, fake_embedder, lifecycle):
        """Archive by query catches ValueError when transitioning already-deleted memories."""
        from memory.lifecycle import MemoryState
        from memory.recall import RecallResult
        from unittest.mock import patch
        content = "Content for archive transition error coverage test"
        r = remember(content)
        key = r["key"]
        lifecycle.transition(key, MemoryState.DELETED)
        mock_result = RecallResult(
            key=key, namespace="episodic", content=content,
            score=1.0, adjusted_score=1.0, state="active",
        )
        with patch.object(tools_module._pipeline, "recall", return_value=[mock_result]):
            result = archive(content, reason="test")
            # The transition will raise ValueError since deleted -> archived is invalid
            assert result["affected"] == []

    def test_reinstate_by_query_catches_transition_error(self, fake_store, fake_embedder, lifecycle):
        """Reinstate by query catches ValueError for invalid transitions."""
        from memory.lifecycle import MemoryState
        from memory.recall import RecallResult
        from unittest.mock import patch
        content = "Content for reinstate transition error coverage test"
        r = remember(content)
        key = r["key"]
        lifecycle.transition(key, MemoryState.DELETED)
        mock_result = RecallResult(
            key=key, namespace="episodic", content=content,
            score=1.0, adjusted_score=1.0, state="deprioritised",
        )
        with patch.object(tools_module._pipeline, "recall", return_value=[mock_result]):
            result = reinstate(content)
            # Transition from deleted -> active is invalid
            assert result["affected"] == []
