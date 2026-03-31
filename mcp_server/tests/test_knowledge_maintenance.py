"""Tests for expire_knowledge_items and run_maintenance knowledge expiry phase."""

import json
import time

import pytest

from tests.conftest import store_memory
from memory.maintenance import expire_knowledge_items, run_maintenance


def store_knowledge(store, embedder, key, content, feed_name=None,
                    expires_at=None, state="active"):
    """Store a knowledge item with optional RSS fields."""
    store_memory(store, embedder, key, content, namespace="knowledge", state=state)
    if feed_name:
        store.set_field(key, "feed_name", feed_name)
    if expires_at is not None:
        store.set_field(key, "expires_at", str(expires_at))


class TestExpireKnowledgeItems:
    def test_archives_expired_item(self, fake_store, fake_embedder, lifecycle):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Expired article",
                        feed_name="SomeFeed", expires_at=time.time() - 1)
        result = expire_knowledge_items(fake_store, lifecycle)
        assert "mem:knowledge:k01" in result
        assert fake_store.get("mem:knowledge:k01")["state"] == "archived"

    def test_skips_not_yet_expired(self, fake_store, fake_embedder, lifecycle):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Future article",
                        feed_name="SomeFeed", expires_at=time.time() + 86400)
        result = expire_knowledge_items(fake_store, lifecycle)
        assert result == []
        assert fake_store.get("mem:knowledge:k01")["state"] == "active"

    def test_skips_item_without_expires_at(self, fake_store, fake_embedder, lifecycle):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "No expiry",
                        feed_name="SomeFeed")
        result = expire_knowledge_items(fake_store, lifecycle)
        assert result == []

    def test_skips_item_without_feed_name(self, fake_store, fake_embedder, lifecycle):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Manual knowledge",
                        expires_at=time.time() - 1)
        result = expire_knowledge_items(fake_store, lifecycle)
        assert result == []

    def test_skips_already_archived(self, fake_store, fake_embedder, lifecycle):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Already archived",
                        feed_name="SomeFeed", expires_at=time.time() - 1, state="archived")
        result = expire_knowledge_items(fake_store, lifecycle)
        assert result == []

    def test_empty_store(self, fake_store, lifecycle):
        result = expire_knowledge_items(fake_store, lifecycle)
        assert result == []

    def test_run_maintenance_includes_knowledge_expired(
        self, fake_store, fake_embedder, lifecycle
    ):
        store_knowledge(fake_store, fake_embedder, "mem:knowledge:k01", "Expired article",
                        feed_name="SomeFeed", expires_at=time.time() - 1)
        result = run_maintenance(fake_store, fake_embedder, lifecycle, "testproject")
        assert "knowledge_expired" in result
        assert "mem:knowledge:k01" in result["knowledge_expired"]
