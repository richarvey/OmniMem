"""Tests for the dashboard stats cache (issue #21) and stat card data."""

import json
import sys
import time
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

    def test_project_distinct_count(self, fake_store, fake_embedder):
        # Three project-namespace records spanning two distinct projects.
        store_memory(fake_store, fake_embedder, "mem:project:alpha", "alpha ctx",
                     namespace="project", project="alpha")
        store_memory(fake_store, fake_embedder, "mem:project:01ALPHAMEM", "alpha note",
                     namespace="project", project="alpha")
        store_memory(fake_store, fake_embedder, "mem:project:beta", "beta ctx",
                     namespace="project", project="beta")

        stats = _compute_stats(fake_store)
        proj = stats["ns_stats"]["project"]
        assert proj["total"] == 3, "raw record count unchanged"
        assert proj["distinct"] == 2, "deduplicated project count"


class TestProjectStateBreakdown:
    def test_projects_counted_once_per_state(self, fake_store, fake_embedder):
        # Active project: context entry + a ULID memory — counts once.
        store_memory(fake_store, fake_embedder, "mem:project:alpha", "alpha ctx",
                     namespace="project", project="alpha")
        store_memory(fake_store, fake_embedder, "mem:project:01ALPHAMEM", "alpha note",
                     namespace="project", project="alpha")
        # Deprioritised project via its context entry.
        store_memory(fake_store, fake_embedder, "mem:project:beta", "beta ctx",
                     namespace="project", project="beta", state="deprioritised")
        # Context-less project resolved from its only (archived) memory.
        store_memory(fake_store, fake_embedder, "mem:project:01GAMMAMEM", "gamma note",
                     namespace="project", project="gamma", state="archived")

        proj = _compute_stats(fake_store)["ns_stats"]["project"]
        assert proj["distinct"] == 3
        assert proj["projects"] == {"active": 1, "deprioritised": 1, "archived": 1}

    def test_context_state_outranks_member_states(self, fake_store, fake_embedder):
        # A stray active memory doesn't resurrect a deprioritised project —
        # the context entry is what bulk transitions stamp.
        store_memory(fake_store, fake_embedder, "mem:project:delta", "delta ctx",
                     namespace="project", project="delta", state="deprioritised")
        store_memory(fake_store, fake_embedder, "mem:project:01DELTAMEM", "delta note",
                     namespace="project", project="delta", state="active")

        proj = _compute_stats(fake_store)["ns_stats"]["project"]
        assert proj["projects"] == {"active": 0, "deprioritised": 1, "archived": 0}

    def test_empty_project_namespace_has_breakdown_keys(self, fake_store):
        proj = _compute_stats(fake_store)["ns_stats"]["project"]
        assert proj["distinct"] == 0
        assert proj["projects"] == {"active": 0, "deprioritised": 0, "archived": 0}


class TestSkillsStats:
    def test_skill_counts_proposals_and_recent(self, fake_store, fake_embedder):
        now = str(time.time())
        fake_store.upsert("skill", "mem:skill:gen:python-local", {
            "name": "python-local", "description": "Distilled python procedure",
            "domain": "python", "state": "active", "generated": "true",
            "created_at": now, "updated_at": now,
        }, fake_embedder.embed("python skill"))
        fake_store.client.hset("meta:skill:proposal:rust-local", mapping={
            "domain": "rust", "created_at": now, "body": "draft",
        })

        stats = _compute_stats(fake_store)
        assert stats["skills"]["total"] == 1
        assert stats["skills"]["states"]["active"] == 1
        assert stats["skills"]["proposals"] == 1
        assert stats["total"] == 0, "skills are build output, not memories"

        skill_rows = [m for m in stats["recent"] if m["namespace"] == "skill"]
        assert len(skill_rows) == 1
        assert "python-local" in skill_rows[0]["content"]

    def test_no_skills_still_reports_zeroes(self, fake_store):
        stats = _compute_stats(fake_store)
        assert stats["skills"] == {
            "total": 0,
            "states": {"active": 0, "deprioritised": 0, "archived": 0},
            "proposals": 0,
        }


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

    def test_pre_skills_cache_shape_recomputed(self, fake_store, fake_embedder, monkeypatch):
        # A cached payload from before the skills/projects cards must be
        # discarded, or the template would KeyError on the new fields.
        monkeypatch.setenv("DASHBOARD_STATS_TTL", "60")
        store_memory(fake_store, fake_embedder, "mem:episodic:h8", "content")
        fake_store.client.set(_CACHE_KEY, json.dumps({
            "ns_stats": {}, "total": 0, "recent": [], "computed_at": time.time(),
        }))
        stats = get_dashboard_stats(fake_store)
        assert stats["total"] == 1
        assert "skills" in stats
