"""Targeted coverage for web UI route branches the main suites skip."""

import json
import sys
import time
from pathlib import Path
from unittest.mock import MagicMock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

from tests.conftest import store_memory


def _seed_pool(fake_store, fake_embedder, domain="deploy"):
    """A reinforced cross-project lesson pool that can compile to a skill."""
    for i, project in enumerate(("alpha", "beta")):
        store_memory(
            fake_store, fake_embedder, f"mem:episodic:0Z{domain[:2].upper()}{i}",
            f"{domain} lesson {i}", tags=[domain], project=project,
            breakthrough="pin the multiarch builder", outcome="succeeded",
        )


class TestLifecycleRoutes:
    def _seed(self, fake_store, fake_embedder, state="active"):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "content",
                     state=state)

    def test_deprioritise_and_next_redirect(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        response = web_client.post("/lifecycle/deprioritise", data={
            "key": "mem:episodic:01A", "next": "/memories?page=2",
        }, follow_redirects=False)
        assert response.status_code == 303
        assert response.headers["location"] == "/memories?page=2"
        assert fake_store.get("mem:episodic:01A")["state"] == "deprioritised"

    def test_unsafe_next_falls_back(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        response = web_client.post("/lifecycle/archive", data={
            "key": "mem:episodic:01A", "next": "//evil.example",
        }, follow_redirects=False)
        assert response.headers["location"] == "/memory/mem:episodic:01A"
        assert fake_store.get("mem:episodic:01A")["state"] == "archived"

    def test_deprioritise_missing_key_logged_not_fatal(self, web_client):
        response = web_client.post("/lifecycle/deprioritise", data={
            "key": "mem:episodic:missing",
        }, follow_redirects=False)
        assert response.status_code == 303

    def test_reinstate_clears_reason(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder, state="deprioritised")
        fake_store.set_fields("mem:episodic:01A", {
            "deprioritised_reason": "old news", "surface_score": "0.2",
        })
        response = web_client.post("/lifecycle/reinstate", data={
            "key": "mem:episodic:01A",
        }, follow_redirects=False)
        assert response.status_code == 303
        data = fake_store.get("mem:episodic:01A")
        assert data["state"] == "active"
        assert data["surface_score"] == "1.0"

    def test_reinstate_failure_logged(self, web_client):
        response = web_client.post("/lifecycle/reinstate", data={
            "key": "mem:episodic:missing",
        }, follow_redirects=False)
        assert response.status_code == 303

    def test_delete_and_force_delete_fallback(
        self, web_client, fake_store, fake_embedder,
    ):
        self._seed(fake_store, fake_embedder)
        response = web_client.post("/lifecycle/delete", data={
            "key": "mem:episodic:01A",
        }, follow_redirects=False)
        assert response.status_code == 303
        assert fake_store.get("mem:episodic:01A") is None

        # Missing key: transition raises, force-delete path swallows it.
        response = web_client.post("/lifecycle/delete", data={
            "key": "mem:episodic:missing",
        }, follow_redirects=False)
        assert response.status_code == 303


class TestSkillCompileRoutes:
    def test_compile_validation_errors(self, web_client):
        response = web_client.post("/skills/compile", data={"domain": ""})
        assert "Enter a domain" in response.text
        response = web_client.post("/skills/compile", data={"domain": "!!!"})
        assert "Invalid domain" in response.text

    def test_compile_refuses_existing_skill(
        self, web_client, fake_store, fake_embedder,
    ):
        fake_store.upsert("skill", "mem:skill:gen:deploy-local", {
            "name": "deploy-local", "domain": "deploy", "state": "active",
            "generated": "true", "body": "---\n---\n",
        }, fake_embedder.embed("deploy"))
        response = web_client.post("/skills/compile", data={"domain": "deploy"})
        assert "already exists" in response.text

    def test_compile_and_commit_flow(self, web_client, fake_store, fake_embedder):
        _seed_pool(fake_store, fake_embedder)
        response = web_client.post("/skills/compile", data={"domain": "deploy"})
        assert "Accept" in response.text

        response = web_client.post("/skills/commit", data={"domain": "deploy"})
        assert response.headers.get("HX-Redirect", "").startswith("/skills/")
        assert fake_store.get("mem:skill:gen:deploy-local") is not None

    def test_commit_validation_and_no_proposal(self, web_client):
        response = web_client.post("/skills/commit", data={"domain": ""})
        assert "Missing domain" in response.text
        response = web_client.post("/skills/commit", data={"domain": "!!!"})
        assert "Invalid domain" in response.text
        response = web_client.post("/skills/commit", data={"domain": "deploy"})
        assert "Nothing proposed" in response.text

    def test_delete_skill_routes(self, web_client, fake_store, fake_embedder):
        response = web_client.post("/skills/delete", data={"key": "mem:skill:gen:nope"})
        assert response.status_code == 404

        fake_store.upsert("skill", "mem:skill:gen:deploy-local", {
            "name": "deploy-local", "domain": "deploy", "state": "active",
            "generated": "true", "body": "---\n---\n",
        }, fake_embedder.embed("deploy"))
        response = web_client.post("/skills/delete", data={
            "key": "mem:skill:gen:deploy-local",
        }, follow_redirects=False)
        assert response.status_code == 303
        assert fake_store.get("mem:skill:gen:deploy-local") is None

    def test_skill_list_tolerates_missing_timestamps(
        self, web_client, fake_store, fake_embedder,
    ):
        fake_store.upsert("skill", "mem:skill:gen:deploy-local", {
            "name": "deploy-local", "domain": "deploy", "state": "active",
            "generated": "true", "body": "---\n---\n",
        }, fake_embedder.embed("deploy"))
        response = web_client.get("/skills")
        assert "—" in response.text

    def test_import_confirm_unreadable_stash(self, web_client, fake_store):
        token = "a" * 32
        fake_store.client.set(f"meta:skill:import:{token}", "{broken json")
        response = web_client.post("/skills/import/confirm", data={"token": token})
        assert "unreadable" in response.text


class TestMemoriesRoutes:
    def _seed(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "active one",
                     project="mine")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B", "archived one",
                     project="mine", state="archived")
        fake_store.upsert("knowledge", "mem:knowledge:01C", {
            "content": "rss article", "state": "active", "feed_name": "feed",
            "created_at": "100", "updated_at": "not a number",
        }, fake_embedder.embed("rss article"))
        fake_store.upsert("knowledge", "mem:knowledge:01D", {
            "content": "learned fact", "state": "active",
            "created_at": "100", "updated_at": "100",
            "last_recalled": str(time.time() - 3 * 86400),
        }, fake_embedder.embed("learned fact"))
        fake_store.upsert("preference", "mem:preference:01E", {
            "content": "always use uv", "state": "active",
            "created_at": "100", "updated_at": "100",
            "last_recalled": str(time.time() - 40 * 86400),
        }, fake_embedder.embed("always use uv"))
        fake_store.client.hset("mem:episodic:01F", mapping={"vector": b"x"})

    def test_filter_combinations(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        assert "archived one" in web_client.get(
            "/memories", params={"state": "archived", "project": "mine"},
        ).text
        assert "rss article" in web_client.get(
            "/memories", params={"namespace": "knowledge", "source": "rss"},
        ).text
        assert "learned fact" in web_client.get(
            "/memories", params={"namespace": "knowledge", "source": "learned"},
        ).text
        assert "always use uv" in web_client.get(
            "/memories", params={"namespace": "preference"},
        ).text

    def test_sort_pagination_and_htmx_partial(
        self, web_client, fake_store, fake_embedder,
    ):
        self._seed(fake_store, fake_embedder)
        assert web_client.get(
            "/memories", params={"sort": "oldest", "page": "99"},
        ).status_code == 200
        partial = web_client.get(
            "/memories", headers={"HX-Request": "true"},
        )
        assert "<html" not in partial.text.lower()


class TestTokenOverheadRoutes:
    def test_page_refresh_and_reset(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "content",
                     project="mine")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B", "gone",
                     state="archived")
        fake_store.client.hset("mem:episodic:01C", mapping={"vector": b"x"})
        fake_store.client.hset("meta:tool_metrics:recall", mapping={
            "call_count": "3", "total_input_chars": "300",
            "total_output_chars": "900",
        })
        fake_store.client.hset("meta:tool_metrics:idle", mapping={
            "call_count": "0",
        })

        assert web_client.get("/token-overhead").status_code == 200
        assert web_client.get(
            "/token-overhead", params={"project": "mine"},
        ).status_code == 200
        assert web_client.get("/token-overhead/refresh").status_code == 200

        response = web_client.post("/token-overhead/reset", follow_redirects=False)
        assert response.status_code == 303
        assert fake_store.scan_prefix("meta:tool_metrics:") == []


class TestVersionCheckRoute:
    def test_update_available(self, web_client, monkeypatch):
        from web_ui.routes import version_check as vc

        monkeypatch.setattr(vc, "_fetch_latest_version", lambda: "99.0.0")
        response = web_client.get("/version-check")
        assert "99.0.0" in response.text

    def test_no_version_and_unparseable(self, web_client, monkeypatch):
        from web_ui.routes import version_check as vc

        monkeypatch.setattr(vc, "_fetch_latest_version", lambda: None)
        assert web_client.get("/version-check").text == ""

        monkeypatch.setattr(vc, "_fetch_latest_version", lambda: object())
        assert web_client.get("/version-check").text == ""


class TestDepsInit:
    def test_init_wires_all_dependencies(self, monkeypatch):
        from web_ui import deps as web_deps

        store = MagicMock()
        embedder = MagicMock()
        monkeypatch.setattr(web_deps, "ValkeyStore", MagicMock(return_value=store))
        monkeypatch.setattr(web_deps, "Embedder", MagicMock(return_value=embedder))

        # The real init (web_client's fixture normally stubs it out).
        original = (web_deps.store, web_deps.embedder,
                    web_deps.lifecycle, web_deps.pipeline)
        try:
            web_deps.init()
            assert web_deps.store is store
            store.connect.assert_called_once()
            embedder.load.assert_called_once()
            assert web_deps.pipeline is not None
        finally:
            (web_deps.store, web_deps.embedder,
             web_deps.lifecycle, web_deps.pipeline) = original
