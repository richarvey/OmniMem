"""Tests for the memories list source split (articles vs learned) and row actions."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

from web_ui import deps
from web_ui.routes.lifecycle import _redirect_target
from web_ui.routes.memories import _get_all_memories


def _seed_knowledge(fake_store, fake_embedder):
    from tests.conftest import store_memory

    store_memory(fake_store, fake_embedder, "mem:knowledge:article1", "An RSS article",
                 namespace="knowledge")
    fake_store.set_fields("mem:knowledge:article1",
                          {"feed_name": "Rust Official Blog", "project": "RSS"})
    store_memory(fake_store, fake_embedder, "mem:knowledge:fact1", "A learned fact",
                 namespace="knowledge", project="omnimem")
    store_memory(fake_store, fake_embedder, "mem:episodic:e1", "An episodic memory")


class TestSourceFilter:
    def test_rss_source_keeps_only_articles(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        _seed_knowledge(fake_store, fake_embedder)

        memories, _ = _get_all_memories(
            namespace="knowledge", state=None, project=None, source="rss",
        )
        assert [m["key"] for m in memories] == ["mem:knowledge:article1"]

    def test_learned_source_excludes_articles(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        _seed_knowledge(fake_store, fake_embedder)

        memories, _ = _get_all_memories(
            namespace="knowledge", state=None, project=None, source="learned",
        )
        assert [m["key"] for m in memories] == ["mem:knowledge:fact1"]

    def test_no_source_returns_everything(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setattr(deps, "store", fake_store)
        _seed_knowledge(fake_store, fake_embedder)

        memories, _ = _get_all_memories(
            namespace="knowledge", state=None, project=None,
        )
        assert len(memories) == 2


class TestLifecycleRedirect:
    def test_local_next_honoured(self):
        form = {"next": "/memories?namespace=knowledge&source=rss"}
        assert _redirect_target(form, "/memory/x") == "/memories?namespace=knowledge&source=rss"

    def test_missing_next_falls_back(self):
        assert _redirect_target({}, "/memory/x") == "/memory/x"

    def test_external_and_protocol_relative_rejected(self):
        assert _redirect_target({"next": "https://evil.example"}, "/m") == "/m"
        assert _redirect_target({"next": "//evil.example"}, "/m") == "/m"
