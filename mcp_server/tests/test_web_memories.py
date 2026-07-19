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
        assert memories[0]["feed_name"] == "Rust Official Blog"

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


class TestArticlesFeedColumn:
    def test_articles_view_shows_feed_not_project(self, web_client, fake_store, fake_embedder):
        _seed_knowledge(fake_store, fake_embedder)

        resp = web_client.get("/memories?namespace=knowledge&source=rss")
        assert resp.status_code == 200
        assert "<th>Feed</th>" in resp.text
        assert "<th>Project</th>" not in resp.text
        assert "Rust Official Blog" in resp.text

    def test_other_views_keep_project_column(self, web_client, fake_store, fake_embedder):
        _seed_knowledge(fake_store, fake_embedder)

        resp = web_client.get("/memories?namespace=knowledge&source=learned")
        assert resp.status_code == 200
        assert "<th>Project</th>" in resp.text
        assert "<th>Feed</th>" not in resp.text


class TestArticlesSortByAdded:
    def test_articles_ordered_by_created_at_despite_backfill(
        self, web_client, fake_store, fake_embedder,
    ):
        from tests.conftest import store_memory

        # old article ingested first, but a backfill bumped its updated_at
        store_memory(fake_store, fake_embedder, "mem:knowledge:old", "Old article",
                     namespace="knowledge")
        fake_store.set_fields("mem:knowledge:old", {
            "feed_name": "LWN", "created_at": "1000.0", "updated_at": "9000.0",
        })
        store_memory(fake_store, fake_embedder, "mem:knowledge:new", "New article",
                     namespace="knowledge")
        fake_store.set_fields("mem:knowledge:new", {
            "feed_name": "LWN", "created_at": "2000.0", "updated_at": "2000.0",
        })

        resp = web_client.get("/memories?namespace=knowledge&source=rss")
        assert resp.status_code == 200
        assert "<th>Added</th>" in resp.text
        assert resp.text.index("New article") < resp.text.index("Old article")

        # the same rows sort by updated_at outside the articles view
        resp = web_client.get("/memories?namespace=knowledge")
        assert "<th>Updated</th>" in resp.text
        assert resp.text.index("Old article") < resp.text.index("New article")


class TestLifecycleRedirect:
    def test_local_next_honoured(self):
        form = {"next": "/memories?namespace=knowledge&source=rss"}
        assert _redirect_target(form, "/memory/x") == "/memories?namespace=knowledge&source=rss"

    def test_missing_next_falls_back(self):
        assert _redirect_target({}, "/memory/x") == "/memory/x"

    def test_external_and_protocol_relative_rejected(self):
        assert _redirect_target({"next": "https://evil.example"}, "/m") == "/m"
        assert _redirect_target({"next": "//evil.example"}, "/m") == "/m"
