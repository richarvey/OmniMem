"""Tests for the deprioritise_project / reinstate_project bulk tools."""

import pytest

from tests.conftest import store_memory
from memory.lifecycle import MemoryLifecycle
from memory.recall import RecallPipeline
from tools.project import deprioritise_project, reinstate_project


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
    fake_store.upsert("project", "mem:project:bench", {
        "content": "bench project", "project_name": "bench",
        "state": "active", "created_at": "1", "updated_at": "1",
    }, fake_embedder.embed("bench project"))
    # Unrelated project must stay active
    store_memory(fake_store, fake_embedder, "mem:episodic:keep1", "other memory", project="other")


class TestPreview:
    def test_preview_counts_without_changing(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        result = deprioritise_project("bench")
        assert result["status"] == "preview"
        assert result["total"] == 4  # context entry excluded by default
        assert result["would_deprioritise"] == {
            "episodic": 2, "knowledge": 1, "preference": 1,
        }
        # Nothing actually changed
        assert fake_store.get("mem:episodic:d1")["state"] == "active"

    def test_not_found_for_unknown_project(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        assert deprioritise_project("nonexistent")["status"] == "not_found"

    def test_invalid_name_rejected(self):
        with pytest.raises(ValueError):
            deprioritise_project("evil{injection}")


class TestDeprioritise:
    def test_confirm_deprioritises_across_namespaces(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        result = deprioritise_project("bench", confirm=True, reason="benchmark data")
        assert result["status"] == "deprioritised"
        assert result["total"] == 4
        for key in ("mem:episodic:d1", "mem:episodic:d2",
                    "mem:knowledge:d3", "mem:preference:d4"):
            data = fake_store.get(key)
            assert data["state"] == "deprioritised", key
            assert data["surface_score"] == "0.2", key
            assert data["deprioritised_reason"] == "benchmark data", key
        # Context entry stays active by default; other project untouched
        assert fake_store.get("mem:project:bench")["state"] == "active"
        assert fake_store.get("mem:episodic:keep1")["state"] == "active"

    def test_include_context_deprioritises_context(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        result = deprioritise_project("bench", confirm=True, include_context=True)
        assert result["total"] == 5
        assert fake_store.get("mem:project:bench")["state"] == "deprioritised"

    def test_already_deprioritised_reported_not_rechanged(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        deprioritise_project("bench", confirm=True)
        # Second pass: nothing active left to change
        result = deprioritise_project("bench", confirm=True)
        assert result["status"] == "nothing_to_change"
        assert result["already_inactive"]["deprioritised"] == 4

    def test_abandoned_lessons_survive_deprioritise(self, fake_store, fake_embedder):
        """Deprioritising doesn't erase graveyard warnings — abandoned
        approaches remain valid lessons regardless of a memory's state."""
        import tools as tools_pkg

        store_memory(
            fake_store, fake_embedder, "mem:episodic:ab9", "bench dead end",
            project="bench",
            abandoned_approaches=[{"name": "webdis", "type": "tool", "reason": "no auth"}],
        )
        pipeline = tools_pkg._pipeline
        assert pipeline.warn_if_abandoned("try webdis?")

        deprioritise_project("bench", confirm=True)
        assert pipeline.warn_if_abandoned("try webdis?")


class TestReinstate:
    def test_reinstate_restores_active(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        deprioritise_project("bench", confirm=True, include_context=True)

        result = reinstate_project("bench", confirm=True, include_context=True)
        assert result["status"] == "reinstated"
        assert result["total"] == 5
        for key in ("mem:episodic:d1", "mem:knowledge:d3",
                    "mem:preference:d4", "mem:project:bench"):
            data = fake_store.get(key)
            assert data["state"] == "active", key
            assert data["surface_score"] == "1.0", key

    def test_reinstate_preview(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        deprioritise_project("bench", confirm=True)
        result = reinstate_project("bench")
        assert result["status"] == "preview"
        assert result["total"] == 4

    def test_reinstate_nothing_when_all_active(self, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        assert reinstate_project("bench")["status"] == "nothing_to_change"
