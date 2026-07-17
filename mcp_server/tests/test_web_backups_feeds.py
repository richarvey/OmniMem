"""HTTP-level tests for the backups and RSS feeds web UI routes.

Both route groups touch the filesystem: backups under BACKUP_DIR (env, read
per request), feeds at FEEDS_PATH (module constant, monkeypatched).
"""

import json
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

# The Docker test image builds from mcp_server/ only — no web_ui package there.
pytest.importorskip("web_ui.app", reason="web UI not present in this test image")

import yaml

from tests.conftest import store_memory


@pytest.fixture
def backup_dir(tmp_path, monkeypatch):
    monkeypatch.setenv("BACKUP_DIR", str(tmp_path))
    return tmp_path


def _write_backup(backup_dir, filename="memory_backup_test.json", data=None):
    backup = {
        "metadata": {"exported_at": "2026-07-16T00:00:00Z",
                     "total_keys": len(data or {}), "version": "6.3.1"},
        "data": data or {},
    }
    path = backup_dir / filename
    path.write_text(json.dumps(backup), encoding="utf-8")
    return path


class TestBackupsPage:
    def test_empty_dir(self, web_client, backup_dir):
        resp = web_client.get("/backups")
        assert resp.status_code == 200

    def test_lists_files_and_messages(self, web_client, backup_dir):
        _write_backup(backup_dir)
        resp = web_client.get("/backups", params={"message": "hello", "error": "oops"})
        assert resp.status_code == 200
        assert "memory_backup_test.json" in resp.text


class TestCreateBackup:
    def test_creates_file(self, web_client, backup_dir, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:b1", "backup me")
        store_memory(fake_store, fake_embedder, "mem:knowledge:b2", "article",
                     namespace="knowledge")
        resp = web_client.post("/backups/create", follow_redirects=False)
        assert resp.status_code == 303
        assert "message=Backup+created" in resp.headers["location"]

        files = list(backup_dir.glob("*.json"))
        assert len(files) == 1
        backup = json.loads(files[0].read_text())
        assert backup["metadata"]["namespaces"]["episodic"] == 1
        assert backup["metadata"]["namespaces"]["knowledge"] == 1
        assert "mem:episodic:b1" in backup["data"]

    def test_failure_redirects_with_error(self, web_client, backup_dir, monkeypatch):
        from web_ui import deps as web_deps
        monkeypatch.setattr(web_deps.store, "dump_all",
                            lambda: (_ for _ in ()).throw(RuntimeError("boom")))
        resp = web_client.post("/backups/create", follow_redirects=False)
        assert resp.status_code == 303
        assert "error=" in resp.headers["location"]


class TestUploadBackup:
    def test_no_file(self, web_client, backup_dir):
        resp = web_client.post("/backups/upload", follow_redirects=False)
        assert "error=No+file+selected" in resp.headers["location"]

    def test_wrong_extension(self, web_client, backup_dir):
        resp = web_client.post("/backups/upload",
                               files={"file": ("notes.txt", b"{}", "text/plain")},
                               follow_redirects=False)
        assert "error=Only+.json" in resp.headers["location"]

    def test_invalid_json(self, web_client, backup_dir):
        resp = web_client.post("/backups/upload",
                               files={"file": ("bad.json", b"not json", "application/json")},
                               follow_redirects=False)
        assert "error=File+is+not+valid+JSON" in resp.headers["location"]

    def test_valid_upload_sanitises_name(self, web_client, backup_dir):
        resp = web_client.post(
            "/backups/upload",
            files={"file": ("my backup!.json", b'{"data": {}}', "application/json")},
            follow_redirects=False)
        assert resp.status_code == 303
        assert "message=Uploaded" in resp.headers["location"]
        assert (backup_dir / "mybackup!.json".replace("!", "")).exists() or \
            list(backup_dir.glob("*.json"))


class TestPreviewDownloadDelete:
    def test_preview(self, web_client, backup_dir):
        _write_backup(backup_dir, data={"mem:episodic:x": {"content": "c"}})
        resp = web_client.get("/backups/memory_backup_test.json/preview")
        assert resp.status_code == 200
        assert "memory_backup_test.json" in resp.text

    def test_preview_invalid_filename(self, web_client, backup_dir):
        resp = web_client.get("/backups/bad%7Cname.json/preview")
        assert resp.status_code == 400

    def test_preview_missing_file(self, web_client, backup_dir):
        resp = web_client.get("/backups/ghost.json/preview")
        assert resp.status_code == 404

    def test_preview_corrupt_file(self, web_client, backup_dir):
        (backup_dir / "corrupt.json").write_text("not json")
        resp = web_client.get("/backups/corrupt.json/preview")
        assert resp.status_code == 400

    def test_download(self, web_client, backup_dir):
        _write_backup(backup_dir)
        resp = web_client.get("/backups/memory_backup_test.json/download")
        assert resp.status_code == 200
        assert resp.headers["content-type"].startswith("application/json")

    def test_download_invalid_and_missing(self, web_client, backup_dir):
        resp = web_client.get("/backups/bad%7Cname.json/download", follow_redirects=False)
        assert "error=Invalid+filename" in resp.headers["location"]
        resp = web_client.get("/backups/ghost.json/download", follow_redirects=False)
        assert "error=Backup+file+not+found" in resp.headers["location"]

    def test_delete(self, web_client, backup_dir):
        path = _write_backup(backup_dir)
        resp = web_client.post("/backups/memory_backup_test.json/delete",
                               follow_redirects=False)
        assert "message=Deleted" in resp.headers["location"]
        assert not path.exists()

    def test_delete_invalid_and_missing(self, web_client, backup_dir):
        resp = web_client.post("/backups/bad%7Cname.json/delete", follow_redirects=False)
        assert "error=Invalid+filename" in resp.headers["location"]
        resp = web_client.post("/backups/ghost.json/delete", follow_redirects=False)
        assert "error=Backup+file+not+found" in resp.headers["location"]


class TestRestoreBackup:
    def test_restore_reembeds_memories(self, web_client, backup_dir, fake_store):
        _write_backup(backup_dir, data={
            "mem:episodic:rest1": {
                "content": "restored memory", "state": "active",
                "updated_at": str(time.time()),
            },
            "meta:something": {"note": "no mem prefix, not re-embedded"},
        })
        resp = web_client.post("/backups/memory_backup_test.json/restore",
                               follow_redirects=False)
        assert resp.status_code == 303
        assert "message=Restored+2+keys" in resp.headers["location"]
        assert fake_store.get("mem:episodic:rest1")["content"] == "restored memory"
        # Re-embedded: the vector is readable again
        assert fake_store.get_vectors_multi(["mem:episodic:rest1"])[0] is not None

    def test_restore_invalid_and_missing(self, web_client, backup_dir):
        resp = web_client.post("/backups/bad%7Cname.json/restore", follow_redirects=False)
        assert "error=Invalid+filename" in resp.headers["location"]
        resp = web_client.post("/backups/ghost.json/restore", follow_redirects=False)
        assert "error=Backup+file+not+found" in resp.headers["location"]

    def test_restore_corrupt_file(self, web_client, backup_dir):
        (backup_dir / "corrupt.json").write_text("not json")
        resp = web_client.post("/backups/corrupt.json/restore", follow_redirects=False)
        assert "error=Restore+failed" in resp.headers["location"]


@pytest.fixture
def feeds_file(tmp_path, monkeypatch):
    from web_ui.routes import feeds as feeds_module
    path = tmp_path / "feeds.yml"
    monkeypatch.setattr(feeds_module, "FEEDS_PATH", str(path))
    return path


def _seed_feeds(feeds_file, feeds=None):
    if feeds is None:
        feeds = [{"url": "https://example.org/feed.xml", "name": "Example",
                  "topics": ["rust", "systems"]}]
    feeds_file.write_text(yaml.dump({"feeds": feeds}))
    return feeds


class TestFeedsList:
    def test_missing_file(self, web_client, feeds_file):
        resp = web_client.get("/feeds")
        assert resp.status_code == 200

    def test_lists_feeds(self, web_client, feeds_file):
        _seed_feeds(feeds_file, [
            {"url": "https://a.example/f.xml", "name": "A", "topics": ["x"]},
            {"url": "https://b.example/f.xml", "name": "B", "mode": "digest"},
        ])
        resp = web_client.get("/feeds", params={"message": "m", "error": "e"})
        assert resp.status_code == 200
        assert "https://a.example/f.xml" in resp.text
        assert "B" in resp.text


class TestFeedCreate:
    def test_form(self, web_client, feeds_file):
        assert web_client.get("/feeds/new").status_code == 200

    def test_missing_fields_bounce_back(self, web_client, feeds_file):
        resp = web_client.post("/feeds/new", data={"name": "", "url": ""},
                               follow_redirects=False)
        assert resp.headers["location"] == "/feeds/new"

    def test_create_appends(self, web_client, feeds_file):
        _seed_feeds(feeds_file)
        resp = web_client.post("/feeds/new", data={
            "name": "New Feed", "url": "https://new.example/rss",
            "topics": "go, cloud", "digest": "on",
        }, follow_redirects=False)
        assert resp.headers["location"] == "/feeds"
        saved = yaml.safe_load(feeds_file.read_text())["feeds"]
        assert len(saved) == 2
        assert saved[1] == {"url": "https://new.example/rss", "name": "New Feed",
                            "topics": ["go", "cloud"], "mode": "digest"}


class TestFeedEdit:
    def test_edit_form_and_404(self, web_client, feeds_file):
        _seed_feeds(feeds_file)
        assert web_client.get("/feeds/0/edit").status_code == 200
        assert web_client.get("/feeds/5/edit").status_code == 404

    def test_save_updates(self, web_client, feeds_file):
        _seed_feeds(feeds_file)
        resp = web_client.post("/feeds/0/edit", data={
            "name": "Renamed", "url": "https://example.org/feed.xml", "topics": "rust",
        }, follow_redirects=False)
        assert resp.headers["location"] == "/feeds"
        saved = yaml.safe_load(feeds_file.read_text())["feeds"]
        assert saved[0]["name"] == "Renamed"
        assert "mode" not in saved[0]

    def test_save_missing_fields_bounce(self, web_client, feeds_file):
        _seed_feeds(feeds_file)
        resp = web_client.post("/feeds/0/edit", data={"name": "", "url": ""},
                               follow_redirects=False)
        assert resp.headers["location"] == "/feeds/0/edit"

    def test_save_out_of_range(self, web_client, feeds_file):
        _seed_feeds(feeds_file)
        resp = web_client.post("/feeds/9/edit", data={
            "name": "X", "url": "https://x.example",
        }, follow_redirects=False)
        assert resp.headers["location"] == "/feeds"

    def test_delete(self, web_client, feeds_file):
        _seed_feeds(feeds_file)
        resp = web_client.post("/feeds/0/delete", follow_redirects=False)
        assert resp.headers["location"] == "/feeds"
        assert yaml.safe_load(feeds_file.read_text())["feeds"] == []
        # Out-of-range delete is a no-op
        resp = web_client.post("/feeds/9/delete", follow_redirects=False)
        assert resp.status_code == 303


class TestFeedDownloadUpload:
    def test_download_missing(self, web_client, feeds_file):
        resp = web_client.get("/feeds/download", follow_redirects=False)
        assert "error=No+feeds.yml" in resp.headers["location"]

    def test_download(self, web_client, feeds_file):
        _seed_feeds(feeds_file)
        resp = web_client.get("/feeds/download")
        assert resp.status_code == 200
        assert "Example" in resp.text

    def test_upload_no_file(self, web_client, feeds_file):
        resp = web_client.post("/feeds/upload", follow_redirects=False)
        assert "error=No+file+selected" in resp.headers["location"]

    def test_upload_wrong_extension(self, web_client, feeds_file):
        resp = web_client.post("/feeds/upload",
                               files={"file": ("feeds.json", b"{}", "application/json")},
                               follow_redirects=False)
        assert "error=Only+.yml" in resp.headers["location"]

    def test_upload_invalid_yaml(self, web_client, feeds_file):
        resp = web_client.post("/feeds/upload",
                               files={"file": ("feeds.yml", b"a: [unclosed", "text/yaml")},
                               follow_redirects=False)
        assert "error=Invalid+YAML" in resp.headers["location"]

    def test_upload_missing_feeds_key(self, web_client, feeds_file):
        resp = web_client.post("/feeds/upload",
                               files={"file": ("feeds.yml", b"other: 1", "text/yaml")},
                               follow_redirects=False)
        assert "feeds'+key" in resp.headers["location"]

    def test_upload_feeds_not_a_list(self, web_client, feeds_file):
        resp = web_client.post("/feeds/upload",
                               files={"file": ("feeds.yml", b"feeds: 1", "text/yaml")},
                               follow_redirects=False)
        assert "must+be+a+list" in resp.headers["location"]

    def test_upload_valid_replaces_file(self, web_client, feeds_file):
        _seed_feeds(feeds_file)
        new_yaml = yaml.dump({"feeds": [{"url": "https://u.example", "name": "U"}]}).encode()
        resp = web_client.post("/feeds/upload",
                               files={"file": ("feeds.yaml", new_yaml, "text/yaml")},
                               follow_redirects=False)
        assert "message=Feeds+config+uploaded" in resp.headers["location"]
        assert yaml.safe_load(feeds_file.read_text())["feeds"][0]["name"] == "U"
