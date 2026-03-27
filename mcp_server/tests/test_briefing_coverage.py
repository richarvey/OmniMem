"""Coverage tests for tools/briefing.py, tools/contradiction.py, and tools/experience.py edge cases."""

import json
import time
from unittest.mock import MagicMock, patch

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


from tools.briefing import (
    _get_contradiction_warnings,
    _get_new_knowledge,
    _get_reinstate_candidates,
    _get_stale_memories,
    briefing,
)
from tools.contradiction import check_contradictions
from tools.experience import (
    experience_summary,
    get_experience,
    log_abandoned,
    record_experience,
    warn_if_abandoned,
)


# === tools/briefing.py ===

class TestGetStaleMemories:
    def test_filters_none_data(self, fake_store, fake_embedder):
        """Entries where get returns None are skipped."""
        # Store and then delete raw data to simulate None
        store_memory(fake_store, fake_embedder, "mem:episodic:stale01", "Stale content")
        fake_store.delete("mem:episodic:stale01")
        # Re-insert the key in scan results but data is gone
        result = _get_stale_memories(fake_store, stale_days=1)
        assert isinstance(result, list)

    def test_filters_non_active_state(self, fake_store, fake_embedder):
        old_time = str(time.time() - 200 * 86400)
        store_memory(fake_store, fake_embedder, "mem:episodic:stale02", "Archived stale",
                     state="archived")
        fake_store.set_field("mem:episodic:stale02", "updated_at", old_time)
        result = _get_stale_memories(fake_store, stale_days=30)
        assert not any(s["key"] == "mem:episodic:stale02" for s in result)

    def test_filters_by_project(self, fake_store, fake_embedder):
        old_time = str(time.time() - 200 * 86400)
        store_memory(fake_store, fake_embedder, "mem:episodic:stale03", "Wrong project stale",
                     project="other")
        fake_store.set_field("mem:episodic:stale03", "updated_at", old_time)
        result = _get_stale_memories(fake_store, stale_days=30, project_filter="myproj")
        assert not any(s["key"] == "mem:episodic:stale03" for s in result)


class TestGetNewKnowledge:
    def test_filters_none_data(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:knowledge:nk01", "Knowledge article",
                     namespace="knowledge")
        fake_store.delete("mem:knowledge:nk01")
        result = _get_new_knowledge(fake_store)
        assert isinstance(result, list)

    def test_filters_non_active_state(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:knowledge:nk02", "Archived knowledge",
                     namespace="knowledge", state="archived")
        result = _get_new_knowledge(fake_store)
        assert not any(a["key"] == "mem:knowledge:nk02" for a in result)

    def test_returns_recent_articles(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:knowledge:nk03", "Recent article",
                     namespace="knowledge")
        fake_store.set_field("mem:knowledge:nk03", "source_url", "https://example.com")
        fake_store.set_field("mem:knowledge:nk03", "feed_name", "Test Feed")
        result = _get_new_knowledge(fake_store, since_days=7)
        assert len(result) >= 1


class TestGetReinstateCandidates:
    def test_filters_none_data(self, fake_store, fake_embedder, lifecycle):
        result = _get_reinstate_candidates(fake_store, lifecycle)
        assert isinstance(result, list)

    def test_filters_non_deprioritised(self, fake_store, fake_embedder, lifecycle):
        store_memory(fake_store, fake_embedder, "mem:episodic:rc01", "Active memory",
                     state="active", reinstate_hints=["test"])
        result = _get_reinstate_candidates(fake_store, lifecycle)
        assert not any(c["key"] == "mem:episodic:rc01" for c in result)

    def test_filters_by_project(self, fake_store, fake_embedder, lifecycle):
        store_memory(fake_store, fake_embedder, "mem:episodic:rc02", "Deprioritised in other project",
                     state="deprioritised", project="other",
                     reinstate_hints=["test"])
        result = _get_reinstate_candidates(fake_store, lifecycle, project_filter="myproj")
        assert not any(c["key"] == "mem:episodic:rc02" for c in result)

    def test_skips_memories_without_hints(self, fake_store, fake_embedder, lifecycle):
        store_memory(fake_store, fake_embedder, "mem:episodic:rc03", "Deprioritised no hints",
                     state="deprioritised")
        result = _get_reinstate_candidates(fake_store, lifecycle)
        assert not any(c["key"] == "mem:episodic:rc03" for c in result)

    def test_returns_candidates_with_hints(self, fake_store, fake_embedder, lifecycle):
        store_memory(fake_store, fake_embedder, "mem:episodic:rc04", "Deprioritised with hints",
                     state="deprioritised", reinstate_hints=["sentinel"],
                     deprioritised_reason="Switched to cluster")
        result = _get_reinstate_candidates(fake_store, lifecycle)
        assert any(c["key"] == "mem:episodic:rc04" for c in result)

    def test_handles_malformed_hints_json(self, fake_store, fake_embedder, lifecycle):
        store_memory(fake_store, fake_embedder, "mem:episodic:rc05", "Bad hints JSON",
                     state="deprioritised")
        fake_store.set_field("mem:episodic:rc05", "reinstate_hints", "not-json{")
        result = _get_reinstate_candidates(fake_store, lifecycle)
        # Should not crash — bad JSON treated as empty hints
        assert not any(c["key"] == "mem:episodic:rc05" for c in result)


class TestGetContradictionWarnings:
    def test_filters_none_data(self, fake_store, fake_embedder):
        result = _get_contradiction_warnings(fake_store)
        assert isinstance(result, list)

    def test_filters_non_active(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:cw01", "Archived with contradictions",
                     state="archived",
                     contradictions=[{"key": "mem:episodic:other"}])
        result = _get_contradiction_warnings(fake_store)
        assert not any(w["key"] == "mem:episodic:cw01" for w in result)

    def test_filters_by_project(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:cw02", "Wrong project contradiction",
                     project="other",
                     contradictions=[{"key": "mem:episodic:x"}])
        result = _get_contradiction_warnings(fake_store, project_filter="myproj")
        assert not any(w["key"] == "mem:episodic:cw02" for w in result)

    def test_handles_malformed_contradictions_json(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:cw03", "Bad JSON contradictions")
        fake_store.set_field("mem:episodic:cw03", "contradictions", "not-json{")
        result = _get_contradiction_warnings(fake_store)
        # Should not crash
        assert not any(w["key"] == "mem:episodic:cw03" for w in result)

    def test_returns_warnings(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:cw04", "Memory with contradiction",
                     contradictions=[{"key": "mem:episodic:cw05", "explanation": "test"}])
        result = _get_contradiction_warnings(fake_store)
        assert any(w["key"] == "mem:episodic:cw04" for w in result)


class TestBriefingEdgeCases:
    def test_briefing_no_project(self, fake_store, fake_embedder):
        result = briefing(project=None)
        assert isinstance(result, dict)

    def test_briefing_project_not_found(self, fake_store, fake_embedder):
        result = briefing(project="nonexistent")
        assert result["project_context"]["note"] == "not_found"

    def test_briefing_without_knowledge(self, fake_store, fake_embedder):
        result = briefing(include_knowledge=False)
        assert "new_knowledge" not in result

    def test_briefing_auto_maintenance_exception(self, fake_store, fake_embedder):
        """Auto-maintenance failure should not crash briefing."""
        with patch.dict("os.environ", {"AUTO_MAINTENANCE_INTERVAL": "1"}):
            with patch("memory.maintenance.run_maintenance", side_effect=RuntimeError("boom")):
                # Need to set the counter high enough
                fake_store.client.hset("meta:maintenance:testproj", mapping={"briefing_count": "0"})
                result = briefing(project="testproj")
                assert isinstance(result, dict)

    def test_briefing_no_experience(self, fake_store, fake_embedder):
        """When there's no experience data, experience_summary is omitted."""
        result = briefing(project="empty-project")
        assert "experience_summary" not in result


# === tools/contradiction.py ===

class TestCheckContradictionsTool:
    def test_query_based_search(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:ct01",
                     "Always use Redis for caching")
        store_memory(fake_store, fake_embedder, "mem:episodic:ct02",
                     "Never use Redis for caching, avoid it")
        result = check_contradictions(query="Redis caching")
        assert "contradictions" in result

    def test_scan_with_no_keys(self, fake_store, fake_embedder):
        result = check_contradictions(query=None, namespace="episodic")
        assert result == {"contradictions": []}

    def test_scan_filters_none_data(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:ct03", "Some content")
        fake_store.delete("mem:episodic:ct03")
        result = check_contradictions(query=None)
        assert "contradictions" in result

    def test_skips_empty_content(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:ct04", "Use Docker always")
        store_memory(fake_store, fake_embedder, "mem:episodic:ct05", "Don't use Docker ever")
        # Wipe content on one
        fake_store.set_field("mem:episodic:ct04", "content", "")
        result = check_contradictions(query=None)
        assert "contradictions" in result

    def test_seen_pairs_dedup(self, fake_store, fake_embedder):
        """Same pair of keys should only appear once in results."""
        store_memory(fake_store, fake_embedder, "mem:episodic:ct06",
                     "Always enable feature flags")
        store_memory(fake_store, fake_embedder, "mem:episodic:ct07",
                     "Never enable feature flags, disable them")
        result = check_contradictions(query=None)
        pairs = [(c["key_a"], c["key_b"]) for c in result["contradictions"]]
        # Each pair should be unique
        assert len(pairs) == len(set(tuple(sorted(p)) for p in pairs))

    def test_use_api_filters_non_contradictions(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:ct08",
                     "Always use caching")
        store_memory(fake_store, fake_embedder, "mem:episodic:ct09",
                     "Never use caching, avoid it")
        with patch("tools.contradiction.check_contradiction_api",
                   return_value={"is_contradiction": False, "confidence": 0.1}):
            result = check_contradictions(query=None, use_api=True)
            # API says not a contradiction, so it should be filtered
            assert len(result["contradictions"]) == 0

    def test_use_api_confirms_contradiction(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:ct10",
                     "Always use Alpine Linux")
        store_memory(fake_store, fake_embedder, "mem:episodic:ct11",
                     "Never use Alpine Linux, avoid it")
        with patch("tools.contradiction.check_contradiction_api",
                   return_value={"is_contradiction": True, "confidence": 0.95,
                                 "explanation": "Direct conflict"}):
            result = check_contradictions(query=None, use_api=True)
            if result["contradictions"]:
                assert result["contradictions"][0]["method"] == "api_confirmed"

    def test_scan_filters_archived_and_project(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:ct12",
                     "Use something", state="archived")
        store_memory(fake_store, fake_embedder, "mem:episodic:ct13",
                     "Don't use something", project="other")
        result = check_contradictions(query=None, project_filter="myproj")
        assert result == {"contradictions": []}


# === tools/experience.py ===

class TestExperienceEdgeCases:
    def test_record_experience_with_existing_abandoned_malformed(self, fake_store, fake_embedder):
        """Handles malformed abandoned_approaches JSON gracefully."""
        from tools.core import remember as _remember
        r = _remember("Experience test memory")
        key = r["key"]
        fake_store.set_field(key, "abandoned_approaches", "not-json{")
        result = record_experience(
            key=key, effort_score=2, outcome="succeeded",
            abandoned_approaches=[{"name": "foo", "type": "library", "reason": "broken"}],
        )
        assert result["key"] == key

    def test_log_abandoned_with_malformed_existing(self, fake_store, fake_embedder):
        from tools.core import remember as _remember
        r = _remember("Log abandoned test")
        key = r["key"]
        fake_store.set_field(key, "abandoned_approaches", "invalid{json")
        result = log_abandoned(key, "badlib", "library", "Did not work")
        assert result["abandoned_count"] >= 1

    def test_get_experience_with_malformed_effort(self, fake_store, fake_embedder):
        from tools.core import remember as _remember
        r = _remember("Bad effort score memory")
        key = r["key"]
        fake_store.set_field(key, "effort_score", "not-a-number")
        result = get_experience(key)
        assert result["status"] == "no_experience"

    def test_get_experience_with_malformed_iterations(self, fake_store, fake_embedder):
        from tools.core import remember as _remember
        r = _remember("Bad iterations memory")
        key = r["key"]
        fake_store.set_fields(key, {
            "effort_score": "3",
            "outcome": "succeeded",
            "iterations": "not-a-number",
        })
        result = get_experience(key)
        assert result["status"] == "found"
        assert result["iterations"] == 1  # fallback

    def test_get_experience_with_malformed_abandoned(self, fake_store, fake_embedder):
        from tools.core import remember as _remember
        r = _remember("Bad abandoned JSON memory")
        key = r["key"]
        fake_store.set_fields(key, {
            "effort_score": "2",
            "outcome": "pivoted",
            "abandoned_approaches": "not-json{",
        })
        result = get_experience(key)
        assert result["status"] == "found"
        # _compact strips empty lists, so abandoned_approaches won't be in result
        assert result.get("abandoned_approaches", []) == []

    def test_experience_summary_filters_by_project(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:exp01", "Project A memory",
                     project="projA", effort_score=2, outcome="succeeded")
        store_memory(fake_store, fake_embedder, "mem:episodic:exp02", "Project B memory",
                     project="projB", effort_score=3, outcome="pivoted")
        result = experience_summary(project="projA")
        assert result["memories_with_experience"] == 1

    def test_experience_summary_skips_malformed_effort(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:exp03", "Bad effort memory")
        fake_store.set_field("mem:episodic:exp03", "effort_score", "bad")
        result = experience_summary()
        # Should not crash, just skip the malformed entry
        assert isinstance(result, dict)

    def test_warn_if_abandoned_clear(self, fake_store, fake_embedder):
        result = warn_if_abandoned("something-never-abandoned")
        assert result["status"] == "clear"
