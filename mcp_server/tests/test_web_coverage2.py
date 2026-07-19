"""Second targeted coverage pass for web UI branches the other suites skip."""

import importlib
import json
import os
import sys
import time
from pathlib import Path
from unittest.mock import MagicMock

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

# The Docker test image builds from mcp_server/ only — no web_ui package there.
pytest.importorskip("web_ui.app", reason="web UI not present in this test image")

import yaml

from tests.conftest import store_memory


class TestAppMiddlewareWiring:
    def test_auth_middleware_added_when_credentials_set(self):
        """Re-import web_ui.app with auth env on so the middleware branch runs."""
        import web_ui.app as app_module
        from web_ui.auth import AuthMiddleware

        keys = (
            "WEB_UI_AUTH_TOKEN", "WEB_UI_LOGIN_ENABLED",
            "OAUTH_ADMIN_USER", "OAUTH_ADMIN_PASSWORD",
        )
        old = {k: os.environ.get(k) for k in keys}
        os.environ.update({
            "WEB_UI_AUTH_TOKEN": "tok123",
            "WEB_UI_LOGIN_ENABLED": "true",
            "OAUTH_ADMIN_USER": "admin",
            "OAUTH_ADMIN_PASSWORD": "s3cret",
        })
        try:
            reloaded = importlib.reload(app_module)
            assert reloaded._login_enabled is True
            assert reloaded._web_auth_token == "tok123"
            assert any(m.cls is AuthMiddleware for m in reloaded._middleware)
        finally:
            for k, v in old.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
            # Restore the unauthenticated app for every other test.
            importlib.reload(app_module)


class TestAuthModule:
    def test_session_user_decodes_bytes(self):
        from web_ui import auth

        client = MagicMock()
        client.get.return_value = b"admin"
        assert auth.session_user(client, "tok") == "admin"

    def test_limiter_prunes_stale_failures(self):
        from web_ui.auth import LoginRateLimiter

        limiter = LoginRateLimiter(max_attempts=1, window_seconds=60)
        # A failure well outside the window must be pruned, not counted.
        limiter._failures["1.2.3.4"].append(time.time() - 3600)
        assert limiter.is_blocked("1.2.3.4") is False
        assert len(limiter._failures["1.2.3.4"]) == 0


class TestAuthRoutes:
    def test_login_page_passes_through_when_signed_in(
        self, web_client, fake_store, monkeypatch,
    ):
        from web_ui import auth

        monkeypatch.setenv("OAUTH_ADMIN_USER", "admin")
        monkeypatch.setenv("OAUTH_ADMIN_PASSWORD", "s3cret")
        monkeypatch.setenv("WEB_UI_LOGIN_ENABLED", "true")
        token = auth.create_session(fake_store.client, "admin")
        resp = web_client.get(
            "/login", cookies={auth.SESSION_COOKIE: token}, follow_redirects=False,
        )
        assert resp.status_code == 303
        assert resp.headers["location"] == "/"

    def test_login_submit_redirects_when_disabled(self, web_client):
        # web_client runs with login disabled, so the POST bounces home.
        resp = web_client.post(
            "/login", data={"username": "a", "password": "b"}, follow_redirects=False,
        )
        assert resp.status_code == 303
        assert resp.headers["location"] == "/"


@pytest.fixture
def backup_dir(tmp_path, monkeypatch):
    monkeypatch.setenv("BACKUP_DIR", str(tmp_path))
    return tmp_path


class TestBackupsRoutes:
    def test_symlink_escaping_backup_dir_rejected(self, web_client, tmp_path, monkeypatch):
        backups = tmp_path / "backups"
        backups.mkdir()
        monkeypatch.setenv("BACKUP_DIR", str(backups))
        outside = tmp_path / "outside.json"
        outside.write_text("{}", encoding="utf-8")
        (backups / "evil.json").symlink_to(outside)

        resp = web_client.get("/backups/evil.json/preview")
        assert resp.status_code == 400
        assert "Invalid filename" in resp.text

    def test_create_counts_project_and_preference(
        self, web_client, backup_dir, fake_store, fake_embedder,
    ):
        store_memory(fake_store, fake_embedder, "mem:project:01P", "proj mem",
                     namespace="project")
        store_memory(fake_store, fake_embedder, "mem:preference:01Q", "pref mem",
                     namespace="preference")
        resp = web_client.post("/backups/create", follow_redirects=False)
        assert resp.status_code == 303

        backup = json.loads(next(backup_dir.glob("*.json")).read_text())
        assert backup["metadata"]["namespaces"]["project"] == 1
        assert backup["metadata"]["namespaces"]["preference"] == 1

    def test_upload_sanitised_to_invalid_filename(self, web_client, backup_dir):
        # "###.json" sanitises down to ".json", which fails the filename check.
        resp = web_client.post(
            "/backups/upload",
            files={"file": ("###.json", b"{}", "application/json")},
            follow_redirects=False,
        )
        assert "error=Invalid+filename" in resp.headers["location"]

    def test_upload_too_large(self, web_client, backup_dir, monkeypatch):
        from web_ui.routes import backups as backups_module

        monkeypatch.setattr(backups_module, "_MAX_BACKUP_FILE_SIZE", 10)
        resp = web_client.post(
            "/backups/upload",
            files={"file": ("big.json", b'{"a": "bbbbbbbbbb"}', "application/json")},
            follow_redirects=False,
        )
        assert "File+too+large" in resp.headers["location"]

    def test_upload_write_failure(self, web_client, backup_dir):
        # A directory squatting on the target filename makes open() fail.
        (backup_dir / "valid.json").mkdir()
        resp = web_client.post(
            "/backups/upload",
            files={"file": ("valid.json", b"{}", "application/json")},
            follow_redirects=False,
        )
        assert "Upload+failed" in resp.headers["location"]

    def test_delete_failure(self, web_client, backup_dir):
        (backup_dir / "dir.json").mkdir()
        resp = web_client.post("/backups/dir.json/delete", follow_redirects=False)
        assert "Delete+failed" in resp.headers["location"]

    def test_restore_too_large(self, web_client, backup_dir, monkeypatch):
        from web_ui.routes import backups as backups_module

        (backup_dir / "big.json").write_text('{"data": {"pad": "xxxx"}}')
        monkeypatch.setattr(backups_module, "_MAX_BACKUP_FILE_SIZE", 5)
        resp = web_client.post("/backups/big.json/restore", follow_redirects=False)
        assert "Backup+too+large" in resp.headers["location"]

    def test_restore_reembeds_skill_discovery_text(
        self, web_client, backup_dir, fake_store,
    ):
        backup = {
            "metadata": {"version": "6.4.1"},
            "data": {
                "mem:skill:gen:deploy-local": {
                    "name": "deploy-local", "description": "deploy patterns",
                    "domain": "deploy", "state": "active", "generated": "true",
                    "body": "---\n---\n", "updated_at": "999",
                },
            },
        }
        (backup_dir / "skills.json").write_text(json.dumps(backup), encoding="utf-8")
        resp = web_client.post("/backups/skills.json/restore", follow_redirects=False)
        assert "message=Restored+1" in resp.headers["location"]
        # The skill was re-embedded from its discovery metadata.
        raw = fake_store.client._data["mem:skill:gen:deploy-local"].get("vector")
        assert isinstance(raw, bytes) and len(raw) == 384 * 4


class TestDashboard:
    def test_project_state_rollup_without_context_entries(
        self, fake_store, fake_embedder,
    ):
        from web_ui.routes import dashboard as dash

        # ULID project memories only — no mem:project:{name} context entry, so
        # the rollup falls through to the member states.
        fake_store.upsert("project", "mem:project:01AAA", {
            "project_name": "alpha", "state": "active", "updated_at": "100",
        }, fake_embedder.embed("alpha"))
        fake_store.upsert("project", "mem:project:01BBB", {
            "project_name": "beta", "state": "deprioritised", "updated_at": "100",
        }, fake_embedder.embed("beta"))
        # A memory with no updated_at lands in recent with the em-dash date.
        fake_store.upsert("episodic", "mem:episodic:01CCC", {
            "content": "undated", "state": "active",
        }, fake_embedder.embed("undated"))

        stats = dash._compute_stats(fake_store)
        assert stats["ns_stats"]["project"]["projects"] == {
            "active": 1, "deprioritised": 1, "archived": 0,
        }
        assert any(m["updated_date"] == "—" for m in stats["recent"])

    def test_cache_read_failure_returns_none(self):
        from web_ui.routes import dashboard as dash

        store = MagicMock()
        store.client.get.side_effect = RuntimeError("valkey down")
        assert dash._load_cached_stats(store) is None

    def test_cache_write_failure_swallowed(self):
        from web_ui.routes import dashboard as dash

        store = MagicMock()
        store.client.set.side_effect = RuntimeError("valkey down")
        dash._save_stats(store, {"ns_stats": {}})  # must not raise

    def test_health_and_queue_failures(self, web_client, fake_store, monkeypatch):
        def boom(*args, **kwargs):
            raise RuntimeError("down")

        monkeypatch.setattr(fake_store.client, "ping", boom)
        monkeypatch.setattr(fake_store.client, "llen", boom)
        resp = web_client.get("/")
        assert resp.status_code == 200


class TestMaintenanceInfo:
    def test_duplicates_page_skips_bad_maintenance_records(self, web_client, fake_store):
        # Unreadable record (vector only) → data is None.
        fake_store.client.hset("meta:maintenance:p1", mapping={"vector": b"x"})
        # Record without a timestamp.
        fake_store.client.hset("meta:maintenance:p2", mapping={"runs": "1"})
        # Valid timestamp but broken summary JSON.
        fake_store.client.hset("meta:maintenance:p3", mapping={
            "last_maintenance_at": "1700000000",
            "last_maintenance_summary": "{broken",
        })
        resp = web_client.get("/duplicates")
        assert resp.status_code == 200
        assert "p3" in resp.text

    def test_contradictions_page_skips_unreadable_memory(self, web_client, fake_store):
        fake_store.client.hset("mem:episodic:01X", mapping={"vector": b"x"})
        resp = web_client.get("/contradictions")
        assert resp.status_code == 200


class TestExperienceRoutes:
    def _seed(self, fake_store, fake_embedder):
        fake_store.client.hset("mem:episodic:01X", mapping={"vector": b"x"})
        fake_store.upsert("episodic", "mem:episodic:01Y", {
            "content": "hard won", "state": "active", "effort_score": "5",
            "outcome": "succeeded", "abandoned_approaches": "{broken json",
            "created_at": "100", "updated_at": "100",
        }, fake_embedder.embed("hard won"))

    def test_summary_tolerates_bad_rows(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/experience")
        assert resp.status_code == 200
        assert "hard won" in resp.text

    def test_graveyard_tolerates_bad_rows(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/experience/graveyard")
        assert resp.status_code == 200


class TestLifecycleRoutes:
    def test_archive_missing_key_logged(self, web_client):
        resp = web_client.post("/lifecycle/archive", data={
            "key": "mem:episodic:missing",
        }, follow_redirects=False)
        assert resp.status_code == 303

    def test_force_delete_failure_logged(self, web_client, fake_store, monkeypatch):
        def boom(key):
            raise RuntimeError("delete failed")

        monkeypatch.setattr(fake_store, "delete", boom)
        resp = web_client.post("/lifecycle/delete", data={
            "key": "mem:episodic:missing",
        }, follow_redirects=False)
        assert resp.status_code == 303


class TestMemoriesRoutes:
    def test_project_filter_excludes_other_projects(
        self, web_client, fake_store, fake_embedder,
    ):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "mine only",
                     project="mine")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B", "other only",
                     project="other")
        resp = web_client.get("/memories", params={"project": "mine"})
        assert "mine only" in resp.text
        assert "other only" not in resp.text

    def test_recall_heat_buckets(self):
        from web_ui.routes.memories import _recall_heat

        assert _recall_heat("not a number") == ""
        assert _recall_heat(str(time.time() - 10 * 86400)) == "warm"
        assert _recall_heat(str(time.time() - 120 * 86400)) == ""


class TestMetricsRoute:
    def test_skips_unreadable_memory(self, web_client, fake_store, monkeypatch):
        from web_ui.routes import metrics as metrics_module

        # Bust the module-level cache so this request recomputes.
        monkeypatch.setitem(metrics_module._cache, "output", None)
        fake_store.client.hset("mem:episodic:01X", mapping={"vector": b"x"})
        resp = web_client.get("/metrics")
        assert resp.status_code == 200


class TestProjectRoutes:
    def test_list_skips_unreadable_and_shows_dash_for_undated(
        self, web_client, fake_store, fake_embedder,
    ):
        fake_store.client.hset("mem:project:01X", mapping={"vector": b"x"})
        fake_store.upsert("project", "mem:project:noup", {
            "project_name": "noup", "content": "undated project", "state": "active",
        }, fake_embedder.embed("undated project"))
        resp = web_client.get("/projects")
        assert resp.status_code == 200
        assert "noup" in resp.text
        assert "—" in resp.text

    def test_detail_with_unparseable_timestamps(
        self, web_client, fake_store, fake_embedder,
    ):
        fake_store.upsert("project", "mem:project:badts", {
            "project_name": "badts", "description": "desc", "state": "active",
            "goals": "ship it", "created_at": "not-a-ts", "updated_at": "not-a-ts",
        }, fake_embedder.embed("badts"))
        resp = web_client.get("/projects/badts")
        assert resp.status_code == 200
        assert "—" in resp.text


class TestSkillsRoutes:
    def test_detail_missing_skill_404(self, web_client):
        resp = web_client.get("/skills/mem:skill:gen:missing-local")
        assert resp.status_code == 404
        assert "Skill not found" in resp.text


class TestTelemetryRoutes:
    def _seed(self, fake_store, fake_embedder):
        fake_store.client.hset("mem:episodic:01X", mapping={"vector": b"x"})
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "archived",
                     state="archived")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B", "not mine",
                     project="other")
        store_memory(fake_store, fake_embedder, "mem:episodic:01C", "gone cold",
                     project="mine")
        fake_store.set_fields("mem:episodic:01C", {
            "recall_count": "2", "last_recalled": str(time.time() - 100 * 86400),
        })

    def test_page_with_project_filter_and_cold(
        self, web_client, fake_store, fake_embedder,
    ):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/telemetry", params={"project": "mine"})
        assert resp.status_code == 200
        assert "gone cold" in resp.text
        assert "not mine" not in resp.text

    def test_refresh_partial(self, web_client, fake_store, fake_embedder):
        self._seed(fake_store, fake_embedder)
        resp = web_client.get("/telemetry/refresh")
        assert resp.status_code == 200
        assert "<html" not in resp.text.lower()

    def test_fmt_ts_unparseable(self):
        from web_ui.routes.telemetry import _fmt_ts

        assert _fmt_ts("junk") == "—"
        assert _fmt_ts(None) == "—"


class TestTokenOverheadRoutes:
    def test_skips_unreadable_metrics_and_filters_project(
        self, web_client, fake_store, fake_embedder,
    ):
        # Metrics hash with no readable fields → data is None.
        fake_store.client.hset("meta:tool_metrics:ghost", mapping={"vector": b"x"})
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "not mine",
                     project="other")
        resp = web_client.get("/token-overhead", params={"project": "mine"})
        assert resp.status_code == 200


class TestFeedsRoutes:
    def test_save_preserves_digest_mode(self, web_client, tmp_path, monkeypatch):
        from web_ui.routes import feeds as feeds_module

        path = tmp_path / "feeds.yml"
        path.write_text(yaml.dump({"feeds": [
            {"url": "http://old.example/rss", "name": "Old", "topics": []},
        ]}), encoding="utf-8")
        monkeypatch.setattr(feeds_module, "FEEDS_PATH", str(path))

        resp = web_client.post("/feeds/0/edit", data={
            "name": "New", "url": "http://new.example/rss",
            "topics": "python, docker", "digest": "on",
        }, follow_redirects=False)
        assert resp.status_code == 303

        saved = yaml.safe_load(path.read_text())
        assert saved["feeds"][0]["mode"] == "digest"
        assert saved["feeds"][0]["name"] == "New"
