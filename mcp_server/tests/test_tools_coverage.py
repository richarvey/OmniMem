"""Targeted coverage for tools-package branches the main suites skip."""

import json
import time
from unittest.mock import MagicMock

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


class TestQueueStatus:
    def test_pending_count(self, fake_store):
        from memory.enrichment import QUEUE_KEY
        from tools.queue import queue_status

        fake_store.client.lpush(QUEUE_KEY, "job1", "job2")
        assert queue_status() == {"pending": 2}

    def test_llen_failure_reported(self):
        from tools.queue import queue_status

        broken = MagicMock()
        broken.client.llen.side_effect = RuntimeError("down")
        tools_module._store = broken
        result = queue_status()
        assert result["pending"] == -1
        assert "down" in result["error"]


class TestCheckContradictions:
    def test_query_path_filters_state_and_project(self, fake_store, fake_embedder):
        from tools.contradiction import check_contradictions

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "always use docker compose here", project="mine")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B",
                     "never use docker compose here", project="mine")
        store_memory(fake_store, fake_embedder, "mem:episodic:01C",
                     "never use docker compose here", project="other")
        store_memory(fake_store, fake_embedder, "mem:episodic:01D",
                     "always use docker compose here", state="archived")

        result = check_contradictions(
            query="docker compose", project_filter="mine",
        )
        assert len(result["contradictions"]) == 1
        pair = result["contradictions"][0]
        assert {pair["key_a"], pair["key_b"]} == {
            "mem:episodic:01A", "mem:episodic:01B",
        }

    def test_scan_path_empty_store_and_no_matches(self, fake_store):
        from tools.contradiction import check_contradictions

        assert check_contradictions()["contradictions"] == []

    def test_scan_path_project_filter_excludes_everything(
        self, fake_store, fake_embedder,
    ):
        from tools.contradiction import check_contradictions

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "always use x", project="other")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B",
                     "irrelevant", state="archived")
        result = check_contradictions(project_filter="mine")
        assert result["contradictions"] == []

    def test_scan_path_vector_fallback_and_hit(self, fake_store, fake_embedder):
        from tools.contradiction import check_contradictions

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "always run migrations before deploys", project="p")
        fake_store.client.hset("mem:episodic:01B", mapping={
            "content": "never run migrations before deploys",
            "state": "active", "project": "p",
        })
        result = check_contradictions(project_filter="p")
        assert len(result["contradictions"]) == 1


class TestAudit:
    def test_memory_audit_pagination_and_filters(self, fake_store, fake_embedder):
        from tools.audit import memory_audit

        for i in range(5):
            store_memory(fake_store, fake_embedder, f"mem:episodic:01{i}",
                         f"memory {i}", project="mine")
        store_memory(fake_store, fake_embedder, "mem:episodic:01X",
                     "archived one", project="mine", state="archived")
        store_memory(fake_store, fake_embedder, "mem:episodic:01Y",
                     "other project", project="other")
        fake_store.set_fields("mem:episodic:010", {"effort_score": "not a number"})

        result = memory_audit(project="mine", limit=2, offset=1)
        assert len(result["entries"]) == 2
        assert result["summary"]["archived"] == 1

    def test_why_did_you_mention_keyword_and_semantic_miss(
        self, fake_store, fake_embedder,
    ):
        from tools.audit import why_did_you_mention

        # No recall logs at all
        assert why_did_you_mention("anything")["status"] == "not_found"

        fake_store.client.hset("log:recall:1", mapping={
            "query": "docker compose deploys",
            "timestamp": "1", "result_keys": '["mem:episodic:01A"]',
        })
        hit = why_did_you_mention("docker compose")
        assert hit["status"] == "found"
        assert hit["match_type"] == "keyword"

        # A log entry that shares nothing with the query → semantic miss
        fake_store.client.delete("log:recall:1")
        fake_store.client.hset("log:recall:2", mapping={
            "query": "quarterly finance report",
            "timestamp": "1", "result_keys": "[]",
        })
        assert why_did_you_mention("valkey hnsw index")["status"] == "not_found"

    def test_reindex_validation_and_dispatch(self, fake_store, monkeypatch):
        from tools.audit import reindex

        with pytest.raises(ValueError, match="Invalid namespace"):
            reindex("wat")

        calls = []
        monkeypatch.setattr(
            fake_store, "reindex_namespace",
            lambda ns: calls.append(ns) or {
                "namespace": ns, "removed_phantoms": 0,
            },
            raising=False,
        )
        result = reindex()
        assert result["status"] == "ok"
        assert calls == ["episodic", "project", "knowledge", "preference"]

        calls.clear()
        reindex("episodic")
        assert calls == ["episodic"]


class TestSkillTools:
    def _store_skill(self, fake_store, fake_embedder, **overrides):
        now = str(time.time())
        fields = {
            "name": "python-local", "description": "python things",
            "domain": "python", "user": "local", "state": "active",
            "generated": "true", "body": "---\nname: python-local\n---\n",
            "compiled_at": now, "created_at": now, "updated_at": now,
            "source_manifest": json.dumps(["mem:episodic:01A"]),
            "rule_manifest": json.dumps([]),
        }
        fields.update(overrides)
        fake_store.upsert("skill", "mem:skill:gen:python-local", fields,
                          fake_embedder.embed("python skill"))

    def test_find_skills_validation_and_empty_store(self, fake_store):
        from tools.skills import find_skills

        with pytest.raises(ValueError, match="cannot be empty"):
            find_skills("  ")
        assert find_skills("python")["skills"] == []

    def test_get_skill_validation_and_damage_tolerance(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools.skills import get_skill

        with pytest.raises(ValueError, match="cannot be empty"):
            get_skill("  ")

        self._store_skill(fake_store, fake_embedder,
                          source_manifest="{broken")
        # Counter bump failing must not break the load.
        monkeypatch.setattr(
            fake_store.client, "pipeline",
            MagicMock(side_effect=RuntimeError("pipeline down")),
        )
        result = get_skill("mem:skill:gen:python-local")
        assert result["status"] == "found"
        assert "source_manifest" not in result or result.get("source_manifest") == []

    def test_bless_tolerates_corrupt_tags(self, fake_store, fake_embedder):
        from tools.skills import bless

        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "lesson")
        fake_store.set_fields("mem:episodic:01A", {"tags": "{broken"})
        result = bless("mem:episodic:01A")
        assert result["status"] == "blessed"

    def test_suggestions_empty_store_and_similarity_floor(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools.skills import suggest_skills_for_briefing

        assert suggest_skills_for_briefing(fake_store, fake_embedder, "ctx") == []

        self._store_skill(fake_store, fake_embedder)
        monkeypatch.setenv("SKILL_SUGGEST_MIN_SIMILARITY", "0.99")
        assert suggest_skills_for_briefing(
            fake_store, fake_embedder, "totally unrelated context words",
        ) == []

    def test_pending_updates_corrupt_manifests_and_overflow(
        self, fake_store, fake_embedder,
    ):
        from tools.skills import pending_skill_updates

        self._store_skill(
            fake_store, fake_embedder,
            source_manifest="{broken", rule_manifest="{broken",
            compiled_at="1",  # ancient — everything is fresh
        )
        # Five new lesson-bearing memories → 3 listed + "+2 more".
        for i in range(5):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:02{i}",
                f"python lesson {i}", tags=["python"], project="p",
                gotchas=f"gotcha {i}",
            )
        # Five newly promoted articles → 3 listed + "+2 more".
        for i in range(5):
            fake_store.upsert("knowledge", f"mem:knowledge:03{i}", {
                "content": f"article {i}", "state": "active",
                "skill_domains": json.dumps(["python"]),
                "promoted_at": str(time.time()),
                "created_at": str(time.time()), "updated_at": str(time.time()),
            }, fake_embedder.embed(f"article {i}"))

        updates = pending_skill_updates(fake_store)
        assert len(updates) == 1
        gists = [c["gist"] for c in updates[0]["changes"]]
        assert any("+2 more new lesson-bearing" in g for g in gists)
        assert any("+2 more promoted articles" in g for g in gists)

    def test_knowledge_watch_edge_paths(self, fake_store, fake_embedder):
        from tools.skills import knowledge_watch

        # No skills at all
        assert knowledge_watch(fake_store) == []

        self._store_skill(fake_store, fake_embedder,
                          source_manifest="{broken", rule_manifest="{broken")
        # No knowledge keys
        assert knowledge_watch(fake_store) == []

        # Unparseable created_at, stale article, already-promoted article —
        # none of them may surface.
        fake_store.upsert("knowledge", "mem:knowledge:01A", {
            "content": "python article", "state": "active",
            "created_at": "not a number",
        }, fake_embedder.embed("python article"))
        fake_store.upsert("knowledge", "mem:knowledge:01B", {
            "content": "old python article", "state": "active",
            "created_at": "1",
        }, fake_embedder.embed("old python article"))
        fake_store.upsert("knowledge", "mem:knowledge:01C", {
            "content": "python skill guidance already promoted",
            "state": "active", "created_at": str(time.time()),
            "skill_domains": json.dumps(["python"]),
        }, fake_embedder.embed("python skill guidance"))
        assert knowledge_watch(fake_store) == []

    def test_knowledge_watch_skips_vectorless_skill(
        self, fake_store, fake_embedder,
    ):
        from tools.skills import knowledge_watch

        self._store_skill(fake_store, fake_embedder)
        # Strip the stored vector so the skill can't be compared.
        del fake_store.client._data["mem:skill:gen:python-local"]["vector"]
        fake_store.upsert("knowledge", "mem:knowledge:01A", {
            "content": "fresh python article", "state": "active",
            "created_at": str(time.time()),
        }, fake_embedder.embed("fresh python article"))
        assert knowledge_watch(fake_store) == []


class TestKnowledgeTool:
    def test_recent_knowledge_topic_and_state_filters(
        self, fake_store, fake_embedder,
    ):
        from tools.knowledge import recent_knowledge

        now = str(time.time())
        fake_store.upsert("knowledge", "mem:knowledge:01A", {
            "content": "docker article", "state": "active",
            "created_at": now, "topics": json.dumps(["docker"]),
        }, fake_embedder.embed("docker article"))
        fake_store.upsert("knowledge", "mem:knowledge:01B", {
            "content": "rust article", "state": "active",
            "created_at": now, "topics": "{broken",
        }, fake_embedder.embed("rust article"))
        fake_store.upsert("knowledge", "mem:knowledge:01C", {
            "content": "archived", "state": "archived", "created_at": now,
        }, fake_embedder.embed("archived"))
        fake_store.client.hset("mem:knowledge:01D", mapping={"other": "x"})

        articles = recent_knowledge(topics=["docker"])
        assert [a["key"] for a in articles] == ["mem:knowledge:01A"]


class TestCoreBranches:
    def test_remember_invalid_mode(self):
        from tools.core import remember

        with pytest.raises(ValueError, match="Invalid mode"):
            remember("content", mode="wat")

    def test_remember_project_namespace_sets_project_name(self):
        from tools.core import remember

        result = remember(
            "project scoped note", namespace="project", project="myproj",
            mode="raw",
        )
        data = tools_module._store.get(result["key"])
        assert data["project_name"] == "myproj"

    def test_remember_surfaces_contradiction_warning(
        self, fake_store, fake_embedder,
    ):
        from tools.core import remember

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "always use tabs for indentation in the repo",
                     project="p")
        result = remember(
            "never use tabs for indentation in the repo", project="p",
            mode="raw",
        )
        assert "contradiction_warning" in result


class TestBriefingBranches:
    def test_briefing_surfaces_all_episodic_sections(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools.briefing import briefing

        monkeypatch.setenv("SKILL_SCAN_INTERVAL_HOURS", "0")
        # A key that reads as None in the scan
        fake_store.client.hset("mem:episodic:00Z", mapping={"vector": b"x"})
        # A contradiction-carrying active memory
        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "use spaces", project="p")
        fake_store.set_fields("mem:episodic:01A", {
            "contradictions": json.dumps([{"key": "mem:episodic:01B"}]),
        })
        # A deprioritised memory with reinstate hints
        store_memory(fake_store, fake_embedder, "mem:episodic:01B",
                     "old approach", project="p", state="deprioritised",
                     reinstate_hints=["kubernetes"],
                     deprioritised_reason="superseded")
        # A knowledge key that reads as None
        fake_store.client.hset("mem:knowledge:00Z", mapping={"vector": b"x"})

        result = briefing(project="p")
        assert result["contradiction_warnings"]
        assert result["reinstate_candidates"]

    def test_briefing_survives_skill_section_failure(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools import skills as skills_tools
        from tools.briefing import briefing

        monkeypatch.setenv("SKILL_SCAN_INTERVAL_HOURS", "0")
        monkeypatch.setattr(
            skills_tools, "suggest_skills_for_briefing",
            MagicMock(side_effect=RuntimeError("skills down")),
        )
        result = briefing(project="p")
        assert "skill_suggestions" not in result

    def test_briefing_includes_knowledge_watch(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools import skills as skills_tools
        from tools.briefing import briefing

        monkeypatch.setenv("SKILL_SCAN_INTERVAL_HOURS", "0")
        monkeypatch.setattr(
            skills_tools, "knowledge_watch",
            MagicMock(return_value=[{"skill": "x"}]),
        )
        result = briefing(project="p")
        assert result["skill_knowledge_watch"] == [{"skill": "x"}]
