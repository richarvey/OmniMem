"""Tests for recent_knowledge tool."""

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


from tools.knowledge import recent_knowledge


def store_knowledge(store, embedder, key, content, feed_name=None,
                    topics=None, expires_at=None, state="active", age_seconds=0):
    """Store a knowledge item, optionally backdated."""
    store_memory(store, embedder, key, content, namespace="knowledge", state=state)
    if age_seconds:
        ts = str(time.time() - age_seconds)
        store.set_field(key, "created_at", ts)
        store.set_field(key, "updated_at", ts)
    if feed_name:
        store.set_field(key, "feed_name", feed_name)
    if topics:
        store.set_field(key, "topics", json.dumps(topics))
    if expires_at:
        store.set_field(key, "expires_at", str(expires_at))


class TestRecentKnowledge:
    def test_returns_recent_items(self, fake_store, fake_embedder):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Article one")
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k02", "Article two")
        result = recent_knowledge(days=7)
        assert len(result) == 2

    def test_skips_old_items(self, fake_store, fake_embedder):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Old article",
                        age_seconds=30 * 86400)
        result = recent_knowledge(days=7)
        assert result == []

    def test_skips_archived_items(self, fake_store, fake_embedder):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Archived article",
                        state="archived")
        result = recent_knowledge(days=7)
        assert result == []

    def test_filter_by_feed_name(self, fake_store, fake_embedder):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Feed A article",
                        feed_name="FeedA")
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k02", "Feed B article",
                        feed_name="FeedB")
        result = recent_knowledge(days=7, feed_name="FeedA")
        assert len(result) == 1
        assert result[0]["feed_name"] == "FeedA"

    def test_filter_by_topics(self, fake_store, fake_embedder):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Python article",
                        topics=["python", "docker"])
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k02", "Kubernetes article",
                        topics=["kubernetes"])
        result = recent_knowledge(days=7, topics=["python"])
        assert len(result) == 1
        assert "python" in result[0]["topics"]

    def test_sorted_newest_first(self, fake_store, fake_embedder):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Oldest",
                        age_seconds=3 * 86400)
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k02", "Middle",
                        age_seconds=2 * 86400)
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k03", "Newest",
                        age_seconds=1 * 86400)
        result = recent_knowledge(days=7)
        assert len(result) == 3
        assert float(result[0]["created_at"]) > float(result[1]["created_at"])
        assert float(result[1]["created_at"]) > float(result[2]["created_at"])

    def test_limit_respected(self, fake_store, fake_embedder):
        for i in range(10):
            store_knowledge(fake_store, fake_embedder, f"mem:knowledge:k{i:02d}",
                            f"Article {i}")
        result = recent_knowledge(days=7, limit=3)
        assert len(result) == 3

    def test_limit_clamped_to_50(self, fake_store, fake_embedder):
        for i in range(60):
            store_knowledge(fake_store, fake_embedder, f"mem:knowledge:k{i:02d}",
                            f"Article {i}")
        result = recent_knowledge(days=7, limit=999)
        assert len(result) == 50

    def test_includes_expires_at(self, fake_store, fake_embedder):
        exp = str(time.time() + 86400)
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Article",
                        expires_at=exp)
        result = recent_knowledge(days=7)
        assert len(result) == 1
        assert result[0]["expires_at"] == exp

    def test_empty_store(self):
        result = recent_knowledge(days=7)
        assert result == []
