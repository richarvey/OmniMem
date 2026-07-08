"""Tests for the delete_project bulk deletion tool (issue #14)."""

import pytest

from tests.conftest import store_memory
from memory.lifecycle import MemoryLifecycle
from memory.recall import RecallPipeline
from tools.project import delete_project


@pytest.fixture(autouse=True)
def wired(fake_store, fake_embedder, monkeypatch):
    import tools as tools_pkg

    lifecycle = MemoryLifecycle(fake_store)
    pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
    monkeypatch.setattr(tools_pkg, "_store", fake_store)
    monkeypatch.setattr(tools_pkg, "_embedder", fake_embedder)
    monkeypatch.setattr(tools_pkg, "_lifecycle", lifecycle)
    monkeypatch.setattr(tools_pkg, "_pipeline", pipeline)
    yield


def _seed(fake_store, fake_embedder):
    """A project spread across namespaces, plus an unrelated project."""
    store_memory(fake_store, fake_embedder, "mem:episodic:d1", "bench memory one", project="bench")
    store_memory(fake_store, fake_embedder, "mem:episodic:d2", "bench memory two", project="bench")
    store_memory(fake_store, fake_embedder, "mem:knowledge:d3", "bench fact", namespace="knowledge", project="bench")
    store_memory(fake_store, fake_embedder, "mem:preference:d4", "bench pref", namespace="preference", project="bench")
    # Project context entry (project_name field, not project)
    fake_store.upsert("project", "mem:project:bench", {
        "content": "bench project", "project_name": "bench",
        "state": "active", "created_at": "1", "updated_at": "1",
    }, fake_embedder.embed("bench project"))
    # Unrelated project must survive
    store_memory(fake_store, fake_embedder, "mem:episodic:keep1", "other memory", project="other")


class TestPreview:
    def test_preview_counts_without_deleting(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        result = delete_project("bench")
        assert result["status"] == "preview"
        assert result["total"] == 4  # context entry excluded by default
        assert result["would_delete"] == {
            "episodic": 2, "knowledge": 1, "preference": 1,
        }
        # Nothing deleted
        assert fake_store.get("mem:episodic:d1") is not None

    def test_not_found_for_unknown_project(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        assert delete_project("nonexistent")["status"] == "not_found"

    def test_invalid_name_rejected(self):
        with pytest.raises(ValueError):
            delete_project("evil{injection}")


class TestDelete:
    def test_confirm_deletes_across_namespaces(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        result = delete_project("bench", confirm=True)
        assert result["status"] == "deleted"
        assert result["total"] == 4
        for key in ("mem:episodic:d1", "mem:episodic:d2",
                    "mem:knowledge:d3", "mem:preference:d4"):
            assert fake_store.get(key) is None, key
        # Context entry and other project survive
        assert fake_store.get("mem:project:bench") is not None
        assert fake_store.get("mem:episodic:keep1") is not None

    def test_include_context_deletes_context_entry(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        result = delete_project("bench", confirm=True, include_context=True)
        assert result["total"] == 5
        assert fake_store.get("mem:project:bench") is None

    def test_abandoned_cache_invalidated(self, fake_store, fake_embedder):
        import tools as tools_pkg

        store_memory(
            fake_store, fake_embedder, "mem:episodic:ab9", "bench dead end",
            project="bench",
            abandoned_approaches=[{"name": "webdis", "type": "tool", "reason": "no auth"}],
        )
        pipeline = tools_pkg._pipeline
        assert pipeline.warn_if_abandoned("try webdis?")  # primes the cache

        delete_project("bench", confirm=True)
        assert not pipeline.warn_if_abandoned("try webdis?"), (
            "cache must be invalidated so deleted abandonments stop warning"
        )
