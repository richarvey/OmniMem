"""HTTP-level tests for the web UI routes, driven through the real Starlette app.

The web_client fixture (conftest.py) serves web_ui.app over the in-memory
fakes, so these exercise route handlers, templates, and redirects together.
"""

import json
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

# The Docker test image builds from mcp_server/ only — no web_ui package there.
pytest.importorskip("web_ui.app", reason="web UI not present in this test image")

from tests.conftest import store_memory


class TestSearch:
    def test_form_page(self, web_client):
        resp = web_client.get("/search")
        assert resp.status_code == 200
        assert "search" in resp.text.lower()

    def test_empty_query_prompts(self, web_client):
        resp = web_client.get("/search/results", params={"query": "  "})
        assert resp.status_code == 200
        assert "Enter a search query" in resp.text

    def test_results_return_stored_memory(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:s1",
                     "valkey vector search tuning", project="omnimem")
        resp = web_client.get("/search/results", params={
            "query": "valkey vector search", "namespace": "episodic",
            "project": "omnimem", "top_k": "5",
        })
        assert resp.status_code == 200
        assert "mem:episodic:s1" in resp.text


class TestSuppressions:
    def test_page_empty(self, web_client):
        resp = web_client.get("/suppressions")
        assert resp.status_code == 200
        assert "No topics are currently suppressed" in resp.text

    def test_add_and_remove(self, web_client):
        resp = web_client.post("/suppressions/add", data={"topic": "docker"})
        assert resp.status_code == 200
        assert "docker" in resp.text

        resp = web_client.post("/suppressions/remove", data={"topic": "docker"})
        assert resp.status_code == 200
        assert "No topics are currently suppressed" in resp.text

    def test_blank_topic_ignored(self, web_client):
        resp = web_client.post("/suppressions/add", data={"topic": "   "})
        assert resp.status_code == 200
        assert "No topics are currently suppressed" in resp.text


class TestDuplicates:
    def test_page_without_maintenance(self, web_client):
        resp = web_client.get("/duplicates")
        assert resp.status_code == 200

    def test_page_with_maintenance_summary(self, web_client, fake_store):
        fake_store.set_fields("meta:maintenance:omnimem", {
            "last_maintenance_at": str(time.time()),
            "last_maintenance_summary": json.dumps(
                {"duplicates_archived": 2, "contradictions_found": 1}
            ),
        })
        fake_store.set_fields("meta:maintenance:older", {
            "last_maintenance_at": str(time.time() - 9999),
            "last_maintenance_summary": "not json",
        })
        resp = web_client.get("/duplicates")
        assert resp.status_code == 200
        assert "omnimem" in resp.text

    def test_scan_finds_duplicate_pair(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:d1",
                     "use mtime polling for docker bind mounts")
        store_memory(fake_store, fake_embedder, "mem:episodic:d2",
                     "use mtime polling for docker bind mounts")
        resp = web_client.get("/duplicates/scan", params={"namespace": "episodic"})
        assert resp.status_code == 200
        assert "mem:episodic:d1" in resp.text or "mem:episodic:d2" in resp.text

    def test_scan_invalid_namespace_falls_back(self, web_client):
        resp = web_client.get("/duplicates/scan", params={"namespace": "bogus"})
        assert resp.status_code == 200


class TestContradictions:
    def test_page_empty(self, web_client):
        resp = web_client.get("/contradictions")
        assert resp.status_code == 200

    def test_pair_listed_once(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:c1",
                     "we use SSE transport",
                     contradictions=[{"key": "mem:episodic:c2",
                                      "explanation": "transport disagreement",
                                      "similarity": 0.81},
                                     "not-a-dict"])
        store_memory(fake_store, fake_embedder, "mem:episodic:c2",
                     "we use streamable HTTP transport",
                     contradictions=[{"key": "mem:episodic:c1",
                                      "explanation": "transport disagreement",
                                      "similarity": 0.81}])
        resp = web_client.get("/contradictions")
        assert resp.status_code == 200
        assert resp.text.count("transport disagreement") == 1

    def test_malformed_contradictions_skipped(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:c3", "fine memory")
        fake_store.set_fields("mem:episodic:c3", {"contradictions": "not json"})
        resp = web_client.get("/contradictions")
        assert resp.status_code == 200


class TestCreate:
    def test_form_page(self, web_client):
        resp = web_client.get("/create")
        assert resp.status_code == 200

    def test_empty_content_rejected(self, web_client):
        resp = web_client.post("/create", data={"content": "  ", "namespace": "episodic"})
        assert resp.status_code == 200
        assert "Content cannot be empty" in resp.text

    def test_create_stores_and_redirects(self, web_client, fake_store):
        resp = web_client.post("/create", data={
            "content": "a brand new memory about caching",
            "namespace": "episodic", "project": "omnimem", "tags": "cache, perf",
        }, follow_redirects=False)
        assert resp.status_code == 303
        key = resp.headers["location"].removeprefix("/memory/")
        data = fake_store.get(key)
        assert data["content"] == "a brand new memory about caching"
        assert json.loads(data["tags"]) == ["cache", "perf"]
        assert data["project"] == "omnimem"

    def test_duplicate_detected_then_forced(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:orig",
                     "identical content here")
        resp = web_client.post("/create", data={
            "content": "identical content here", "namespace": "episodic",
        })
        assert resp.status_code == 200
        assert "mem:episodic:orig" in resp.text  # duplicate warning shown

        resp = web_client.post("/create", data={
            "content": "identical content here", "namespace": "episodic",
            "force": "on",
        }, follow_redirects=False)
        assert resp.status_code == 303

    def test_invalid_namespace_falls_back_to_episodic(self, web_client, fake_store):
        resp = web_client.post("/create", data={
            "content": "namespace fallback memory", "namespace": "skill",
        }, follow_redirects=False)
        assert resp.status_code == 303
        assert resp.headers["location"].startswith("/memory/mem:episodic:")


class TestDetail:
    def test_full_detail_renders(self, web_client, fake_store, fake_embedder):
        store_memory(
            fake_store, fake_embedder, "mem:episodic:det1",
            "detailed memory content", project="omnimem",
            tags=["alpha", "beta"], effort_score=7, outcome="succeeded",
            abandoned_approaches=[{"name": "inotify", "type": "approach",
                                   "reason": "docker mounts"}],
            contradictions=[{"key": "mem:episodic:other", "explanation": "x"}],
            reinstate_hints=["if revisiting file watching"],
            breakthrough="mtime polling", gotchas="10s interval",
        )
        fake_store.set_fields("mem:episodic:det1", {
            "recall_count": "4", "last_recalled": str(time.time()),
            "source_url": "https://example.org/a", "feed_name": "Example Feed",
        })
        resp = web_client.get("/memory/mem:episodic:det1")
        assert resp.status_code == 200
        assert "detailed memory content" in resp.text
        assert "inotify" in resp.text
        assert "alpha" in resp.text

    def test_missing_memory_404(self, web_client):
        resp = web_client.get("/memory/mem:episodic:nope")
        assert resp.status_code == 404

    def test_malformed_json_fields_tolerated(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:det2", "hardy memory")
        fake_store.set_fields("mem:episodic:det2", {
            "tags": "not json", "abandoned_approaches": "not json",
            "contradictions": "not json", "reinstate_hints": "not json",
            "created_at": "not-a-float",
        })
        resp = web_client.get("/memory/mem:episodic:det2")
        assert resp.status_code == 200

    def test_retag_roundtrip(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:ret1", "retaggable")
        resp = web_client.post("/memory/mem:episodic:ret1/tags",
                               data={"tags": "one, two , one"},
                               follow_redirects=False)
        assert resp.status_code == 303
        assert resp.headers["location"] == "/memory/mem:episodic:ret1"
        assert json.loads(fake_store.get("mem:episodic:ret1")["tags"]) == ["one", "two"]

    def test_retag_validation_error_redirects(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:ret2", "retaggable")
        too_many = ", ".join(f"t{i}" for i in range(25))
        resp = web_client.post("/memory/mem:episodic:ret2/tags",
                               data={"tags": too_many}, follow_redirects=False)
        assert resp.status_code == 303
        assert "tag_error=" in resp.headers["location"]


class TestProjects:
    def _seed_project(self, fake_store, fake_embedder, name="webproj"):
        now = str(time.time())
        fake_store.upsert("project", f"mem:project:{name}", {
            "content": "a test project", "project_name": name,
            "description": "a test project", "stack": "python, valkey",
            "goals": "ship v6", "current_state": "in progress",
            "notes": "", "state": "active",
            "surface_score": "1.0", "created_at": now, "updated_at": now,
        }, fake_embedder.embed("a test project"))

    def test_list_groups_context_and_memories(self, web_client, fake_store, fake_embedder):
        self._seed_project(fake_store, fake_embedder)
        # ULID-style project memory without context fields
        store_memory(fake_store, fake_embedder, "mem:project:01ULIDMEMORY",
                     "loose project memory", namespace="project")
        fake_store.set_fields("mem:project:01ULIDMEMORY", {"project": "webproj"})
        resp = web_client.get("/projects")
        assert resp.status_code == 200
        assert "webproj" in resp.text

    def test_detail_and_404(self, web_client, fake_store, fake_embedder):
        self._seed_project(fake_store, fake_embedder)
        assert web_client.get("/projects/webproj").status_code == 200
        assert web_client.get("/projects/ghost").status_code == 404

    def test_edit_form_and_404(self, web_client, fake_store, fake_embedder):
        self._seed_project(fake_store, fake_embedder)
        assert web_client.get("/projects/webproj/edit").status_code == 200
        assert web_client.get("/projects/ghost/edit").status_code == 404

    def test_save_updates_existing(self, web_client, fake_store, fake_embedder):
        self._seed_project(fake_store, fake_embedder)
        resp = web_client.post("/projects/webproj/edit", data={
            "description": "updated description", "stack": "python",
            "goals": "ship it", "current_state": "nearly done", "notes": "n",
        }, follow_redirects=False)
        assert resp.status_code == 303
        assert fake_store.get("mem:project:webproj")["description"] == "updated description"

    def test_save_creates_when_missing(self, web_client, fake_store):
        resp = web_client.post("/projects/fresh/edit", data={
            "description": "made via edit", "stack": "", "goals": "",
            "current_state": "", "notes": "",
        }, follow_redirects=False)
        assert resp.status_code == 303
        assert fake_store.get("mem:project:fresh")["created_at"]

    def test_create_flow(self, web_client, fake_store):
        assert web_client.get("/projects/new").status_code == 200

        resp = web_client.post("/projects/new", data={"name": ""},
                               follow_redirects=False)
        assert resp.headers["location"] == "/projects/new"

        resp = web_client.post("/projects/new", data={
            "name": "brandnew", "description": "d", "stack": "s",
            "goals": "g", "current_state": "c", "notes": "",
        }, follow_redirects=False)
        assert resp.headers["location"] == "/projects/brandnew"
        assert fake_store.get("mem:project:brandnew")["project_name"] == "brandnew"

    def test_delete(self, web_client, fake_store, fake_embedder):
        self._seed_project(fake_store, fake_embedder)
        resp = web_client.post("/projects/webproj/delete", follow_redirects=False)
        assert resp.status_code == 303
        assert fake_store.get("mem:project:webproj") is None
        # Deleting again is a no-op redirect
        resp = web_client.post("/projects/webproj/delete", follow_redirects=False)
        assert resp.status_code == 303

    def test_deprioritise_and_reinstate(self, web_client, fake_store, fake_embedder):
        self._seed_project(fake_store, fake_embedder)
        resp = web_client.post("/projects/webproj/deprioritise", follow_redirects=False)
        assert resp.status_code == 303
        assert fake_store.get("mem:project:webproj")["state"] == "deprioritised"

        resp = web_client.post("/projects/webproj/reinstate", follow_redirects=False)
        assert resp.status_code == 303
        assert fake_store.get("mem:project:webproj")["state"] == "active"


class TestExperience:
    def _seed(self, fake_store, fake_embedder):
        for i in range(12):
            store_memory(fake_store, fake_embedder, f"mem:episodic:exp{i:02d}",
                         f"experience memory {i}", project="omnimem",
                         effort_score=i, outcome="succeeded" if i % 2 else "pivoted")
        store_memory(fake_store, fake_embedder, "mem:episodic:expaband",
                     "abandoned experience", project="omnimem",
                     effort_score=9, outcome="abandoned",
                     abandoned_approaches=[
                         {"name": "Alpine base", "type": "approach",
                          "reason": "no musllinux wheels",
                          "attempted_at": "2026-01-01"},
                         {"name": "alpine BASE", "type": "approach",
                          "reason": "duplicate name different case"},
                     ],
                     breakthrough="Debian slim works")
        # No effort score — excluded from stats
        store_memory(fake_store, fake_embedder, "mem:episodic:noexp", "plain memory")
        # Unparseable effort — skipped
        store_memory(fake_store, fake_embedder, "mem:episodic:badexp", "bad effort")
        fake_store.set_fields("mem:episodic:badexp", {"effort_score": "high"})

    def test_summary_counts_and_breakthroughs(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/experience")
        assert resp.status_code == 200
        assert "Debian slim works" in resp.text

    def test_outcome_filter_and_pagination(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/experience", params={"outcome": "succeeded", "page": "2"})
        assert resp.status_code == 200
        resp = web_client.get("/experience", params={"outcome": "bogus", "page": "x"})
        assert resp.status_code == 200

    def test_project_filter(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/experience", params={"project": "otherproj"})
        assert resp.status_code == 200

    def test_htmx_request_gets_partial(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/experience", headers={"HX-Request": "true"})
        assert resp.status_code == 200
        assert "<html" not in resp.text

    def test_graveyard_dedupes_by_name(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/experience/graveyard")
        assert resp.status_code == 200
        assert resp.text.lower().count("alpine base") == 1

    def test_graveyard_project_filter(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/experience/graveyard", params={"project": "otherproj"})
        assert resp.status_code == 200
        assert "Alpine base" not in resp.text


class TestMetrics:
    def _reset_cache(self, monkeypatch):
        from web_ui.routes import metrics as metrics_module
        monkeypatch.setitem(metrics_module._cache, "output", None)
        monkeypatch.setitem(metrics_module._cache, "computed_at", 0.0)

    def test_gauges_reflect_store(self, web_client, fake_store, fake_embedder, monkeypatch):
        self._reset_cache(monkeypatch)
        store_memory(fake_store, fake_embedder, "mem:episodic:m1", "recalled memory")
        fake_store.set_fields("mem:episodic:m1", {
            "recall_count": "3",
            # Recalled long ago -> counts as gone cold
            "last_recalled": str(time.time() - 400 * 86400),
        })
        store_memory(fake_store, fake_embedder, "mem:episodic:m2", "never recalled")
        store_memory(fake_store, fake_embedder, "mem:episodic:m3", "archived one",
                     state="archived")
        fake_store.set_fields("meta:tool_metrics:recall", {
            "call_count": "5", "error_count": "1", "total_duration_ms": "50",
            "total_response_chars": "1000", "total_response_tokens": "250",
        })

        resp = web_client.get("/metrics")
        assert resp.status_code == 200
        body = resp.text
        assert 'omnimem_memories_total{namespace="episodic",state="active"} 2.0' in body
        assert 'omnimem_memories_never_recalled{namespace="episodic"} 1.0' in body
        assert "omnimem_recalls_total 3.0" in body
        assert "omnimem_memories_gone_cold 1.0" in body
        assert 'omnimem_tool_calls_total{tool="recall"} 5.0' in body

    def test_cache_serves_second_request(self, web_client, fake_store, fake_embedder, monkeypatch):
        self._reset_cache(monkeypatch)
        first = web_client.get("/metrics")
        store_memory(fake_store, fake_embedder, "mem:episodic:mcache", "added after")
        second = web_client.get("/metrics")
        assert first.text == second.text


class TestSmokePages:
    """Pages already partially covered by helper tests — hit the HTTP layer."""

    def test_dashboard(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:dash1", "dashboard seed")
        resp = web_client.get("/")
        assert resp.status_code == 200

    def test_memories_list(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:list1", "listable memory")
        resp = web_client.get("/memories")
        assert resp.status_code == 200
        assert "listable memory" in resp.text

    def test_telemetry(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:tel1", "telemetry seed")
        resp = web_client.get("/telemetry")
        assert resp.status_code == 200
