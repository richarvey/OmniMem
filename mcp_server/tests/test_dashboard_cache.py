"""Tests for the dashboard stats cache (issue #21)."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

from tests.conftest import store_memory
from web_ui.routes.dashboard import get_dashboard_stats, _compute_stats, _CACHE_KEY


class TestComputeStats:
    def test_counts_and_recent(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:c1", "first memory")
        store_memory(fake_store, fake_embedder, "mem:episodic:c2", "second memory",
                     state="archived", surface_score="0.0")
        store_memory(fake_store, fake_embedder, "mem:knowledge:c3", "an article",
                     namespace="knowledge")

        stats = _compute_stats(fake_store)
        assert stats["total"] == 3
        assert stats["ns_stats"]["episodic"]["total"] == 2
        assert stats["ns_stats"]["episodic"]["states"]["archived"] == 1
        assert stats["ns_stats"]["knowledge"]["states"]["active"] == 1
        assert len(stats["recent"]) == 3
        assert all("content" in m and "updated_at_fmt" in m for m in stats["recent"])
        assert stats["computed_at"] > 0


class TestCache:
    def test_second_call_served_from_cache(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("DASHBOARD_STATS_TTL", "60")
        store_memory(fake_store, fake_embedder, "mem:episodic:h1", "cached view")

        first = get_dashboard_stats(fake_store)
        assert first["total"] == 1
        assert fake_store.client.get(_CACHE_KEY) is not None

        # A write after caching is invisible until TTL/refresh — that's the deal
        store_memory(fake_store, fake_embedder, "mem:episodic:h2", "newer memory")
        second = get_dashboard_stats(fake_store)
        assert second["total"] == 1, "stale-within-TTL is expected behaviour"

    def test_force_refresh_bypasses_cache(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("DASHBOARD_STATS_TTL", "60")
        store_memory(fake_store, fake_embedder, "mem:episodic:h3", "one")
        get_dashboard_stats(fake_store)
        store_memory(fake_store, fake_embedder, "mem:episodic:h4", "two")

        refreshed = get_dashboard_stats(fake_store, force_refresh=True)
        assert refreshed["total"] == 2
        # Refresh also rewrites the cache for the next reader
        assert get_dashboard_stats(fake_store)["total"] == 2

    def test_ttl_zero_disables_caching(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("DASHBOARD_STATS_TTL", "0")
        store_memory(fake_store, fake_embedder, "mem:episodic:h5", "one")
        assert get_dashboard_stats(fake_store)["total"] == 1
        store_memory(fake_store, fake_embedder, "mem:episodic:h6", "two")
        assert get_dashboard_stats(fake_store)["total"] == 2
        assert fake_store.client.get(_CACHE_KEY) is None

    def test_corrupt_cache_recomputes(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setenv("DASHBOARD_STATS_TTL", "60")
        store_memory(fake_store, fake_embedder, "mem:episodic:h7", "content")
        fake_store.client.set(_CACHE_KEY, "{not json")
        assert get_dashboard_stats(fake_store)["total"] == 1
