"""Web UI tests for feed→skill influence: the feeds editor's skill rows,
the Valkey mirror on every feeds.yml write, and skill import folding bundled
feeds into the reading list."""

import io
import json
import time

import pytest
import yaml

from memory.feed_influence import FEED_INFLUENCE_KEY, load_feed_influences
from memory.feed_influence import sync_feed_influences
from memory.skill_transfer import build_skill_export


@pytest.fixture
def feeds_file(tmp_path, monkeypatch):
    from web_ui.routes import feeds as feeds_module
    path = tmp_path / "feeds.yml"
    monkeypatch.setattr(feeds_module, "FEEDS_PATH", str(path))
    return path


def _seed_feeds(feeds_file, feeds=None):
    if feeds is None:
        feeds = [{"url": "https://example.org/feed.xml", "name": "Example",
                  "topics": ["rust"], "skills": {"rust": 6}}]
    feeds_file.write_text(yaml.dump({"feeds": feeds}))
    return feeds


def _stored_feeds(feeds_file):
    return yaml.safe_load(feeds_file.read_text())["feeds"]


class TestFeedFormsWithSkills:
    def test_create_with_skills_writes_yaml_and_mirror(
            self, web_client, feeds_file, fake_store):
        _seed_feeds(feeds_file, feeds=[])
        resp = web_client.post("/feeds/new", data={
            "name": "Python Weekly", "url": "https://pw.example/feed",
            "topics": "python, news",
            "skill_domain": ["py", ""], "skill_influence": ["8", "5"],
        }, follow_redirects=False)
        assert resp.status_code == 303 and resp.headers["location"] == "/feeds"

        stored = _stored_feeds(feeds_file)
        assert stored[0]["skills"] == {"python": 8}  # alias resolved
        mirrored = load_feed_influences(fake_store.client)
        assert mirrored["Python Weekly"]["skills"] == {"python": 8}

    def test_create_without_skills_omits_key(self, web_client, feeds_file):
        _seed_feeds(feeds_file, feeds=[])
        web_client.post("/feeds/new", data={
            "name": "Plain", "url": "https://plain.example/feed",
            "skill_domain": "", "skill_influence": "5",
        }, follow_redirects=False)
        assert "skills" not in _stored_feeds(feeds_file)[0]

    def test_create_invalid_domain_bounces_with_error(self, web_client, feeds_file):
        _seed_feeds(feeds_file, feeds=[])
        resp = web_client.post("/feeds/new", data={
            "name": "Bad", "url": "https://bad.example/feed",
            "skill_domain": "no spaces allowed!!", "skill_influence": "5",
        }, follow_redirects=False)
        assert resp.status_code == 303
        assert resp.headers["location"].startswith("/feeds/new?error=")
        assert _stored_feeds(feeds_file) == []

    def test_create_out_of_range_influence_bounces(self, web_client, feeds_file):
        _seed_feeds(feeds_file, feeds=[])
        resp = web_client.post("/feeds/new", data={
            "name": "Bad", "url": "https://bad.example/feed",
            "skill_domain": "python", "skill_influence": "11",
        }, follow_redirects=False)
        assert resp.status_code == 303
        assert "error=" in resp.headers["location"]

    def test_edit_form_shows_rows_and_error(self, web_client, feeds_file,
                                            fake_store, fake_embedder):
        _seed_feeds(feeds_file)
        fake_store.upsert("skill", "mem:skill:gen:python-local", {
            "name": "python-local", "domain": "python", "generated": "true",
            "state": "active", "body": "x",
        }, fake_embedder.embed("python skill"))

        resp = web_client.get("/feeds/0/edit?error=Nope")
        assert resp.status_code == 200
        assert 'value="rust"' in resp.text          # existing association row
        assert 'value="6"' in resp.text             # its influence
        assert '<option value="python">' in resp.text  # datalist of skills
        assert "Nope" in resp.text

    def test_save_replaces_skills(self, web_client, feeds_file, fake_store):
        _seed_feeds(feeds_file)
        web_client.post("/feeds/0/edit", data={
            "name": "Example", "url": "https://example.org/feed.xml",
            "topics": "rust",
            "skill_domain": ["rust", "systems"], "skill_influence": ["9", "2"],
        }, follow_redirects=False)
        assert _stored_feeds(feeds_file)[0]["skills"] == {"rust": 9, "systems": 2}

    def test_save_cleared_domains_removes_association(self, web_client, feeds_file,
                                                      fake_store):
        _seed_feeds(feeds_file)
        sync_feed_influences(fake_store.client, _stored_feeds(feeds_file))
        web_client.post("/feeds/0/edit", data={
            "name": "Example", "url": "https://example.org/feed.xml",
            "topics": "rust", "skill_domain": "", "skill_influence": "5",
        }, follow_redirects=False)
        assert "skills" not in _stored_feeds(feeds_file)[0]
        assert load_feed_influences(fake_store.client)["Example"]["skills"] == {}

    def test_save_invalid_bounces_back_to_edit(self, web_client, feeds_file):
        _seed_feeds(feeds_file)
        resp = web_client.post("/feeds/0/edit", data={
            "name": "Example", "url": "https://example.org/feed.xml",
            "skill_domain": "python", "skill_influence": "zero",
        }, follow_redirects=False)
        assert resp.headers["location"].startswith("/feeds/0/edit?error=")

    def test_delete_resyncs_mirror(self, web_client, feeds_file, fake_store):
        _seed_feeds(feeds_file)
        sync_feed_influences(fake_store.client, _stored_feeds(feeds_file))
        web_client.post("/feeds/0/delete", follow_redirects=False)
        assert load_feed_influences(fake_store.client) == {}

    def test_upload_resyncs_mirror(self, web_client, feeds_file, fake_store):
        _seed_feeds(feeds_file, feeds=[])
        payload = yaml.dump({"feeds": [
            {"url": "https://up.example/feed", "name": "Uploaded",
             "skills": {"python": 4}},
        ]}).encode()
        resp = web_client.post("/feeds/upload", files={
            "file": ("feeds.yml", io.BytesIO(payload), "application/x-yaml"),
        }, follow_redirects=False)
        assert resp.status_code == 303
        assert load_feed_influences(fake_store.client)["Uploaded"]["skills"] == {"python": 4}

    def test_list_shows_skills_summary(self, web_client, feeds_file):
        _seed_feeds(feeds_file)
        resp = web_client.get("/feeds")
        assert "rust (6)" in resp.text

    def test_sync_failure_never_blocks_save(self, web_client, feeds_file, monkeypatch):
        from web_ui.routes import feeds as feeds_module
        _seed_feeds(feeds_file, feeds=[])
        monkeypatch.setattr(
            feeds_module, "sync_feed_influences",
            lambda client, feeds: (_ for _ in ()).throw(RuntimeError("down")),
        )
        resp = web_client.post("/feeds/new", data={
            "name": "OK", "url": "https://ok.example/feed",
        }, follow_redirects=False)
        assert resp.status_code == 303 and resp.headers["location"] == "/feeds"
        assert _stored_feeds(feeds_file)[0]["name"] == "OK"


def _seed_exportable_skill(fake_store, fake_embedder):
    now = str(time.time())
    key = "mem:skill:gen:python-local"
    fake_store.upsert("skill", key, {
        "name": "python-local", "description": "Python procedure.",
        "domain": "python", "user": "local", "state": "active",
        "generated": "true",
        "body": "---\nname: python-local\n---\n\n## Do\n\n- Use uv. [mem:episodic:01A]\n",
        "contract_version": "1", "compiled_at": now, "created_at": now,
        "updated_at": now,
        "source_manifest": json.dumps(["mem:episodic:01A"]),
        "rule_manifest": json.dumps([]),
    }, fake_embedder.embed("python skill"))
    fake_store.upsert("episodic", "mem:episodic:01A", {
        "content": "Lesson", "state": "active",
        "created_at": now, "updated_at": now,
    }, fake_embedder.embed("lesson"))
    return key


class TestImportWithFeeds:
    def _bundle(self, fake_store, fake_embedder):
        key = _seed_exportable_skill(fake_store, fake_embedder)
        sync_feed_influences(fake_store.client, [
            {"name": "Python Weekly", "url": "https://pw.example/feed",
             "topics": ["python"], "skills": {"python": 8}},
        ])
        bundle, err = build_skill_export(fake_store, key)
        assert err is None
        # Simulate a receiving instance: drop the skill, its memory, and the
        # exporting instance's feed mirror.
        fake_store.delete(key)
        fake_store.delete("mem:episodic:01A")
        fake_store.client.delete(FEED_INFLUENCE_KEY)
        return bundle["data"]

    def _upload(self, web_client, data):
        return web_client.post("/skills/import", files={
            "file": ("bundle.zip", io.BytesIO(data), "application/zip"),
        })

    def test_preview_shows_feed_plan(self, web_client, feeds_file,
                                     fake_store, fake_embedder):
        _seed_feeds(feeds_file, feeds=[])
        resp = self._upload(web_client, self._bundle(fake_store, fake_embedder))
        assert resp.status_code == 200
        assert "Python Weekly" in resp.text
        assert "will join the reading list" in resp.text

    def test_confirm_merges_feeds_and_syncs(self, web_client, feeds_file,
                                            fake_store, fake_embedder):
        _seed_feeds(feeds_file, feeds=[])
        preview = self._upload(web_client, self._bundle(fake_store, fake_embedder))
        token = preview.text.split('name="token" value="')[1].split('"')[0]

        resp = web_client.post("/skills/import/confirm", data={"token": token})
        assert resp.status_code == 200
        assert "1 RSS feed added" in resp.headers["HX-Redirect"] or \
               "RSS%20feed%20added" in resp.headers["HX-Redirect"]

        stored = _stored_feeds(feeds_file)
        assert stored[0]["name"] == "Python Weekly"
        assert stored[0]["skills"] == {"python": 8}
        assert load_feed_influences(fake_store.client)["Python Weekly"]["skills"] == {
            "python": 8,
        }
        assert fake_store.get("mem:skill:gen:python-local") is not None

    def test_confirm_updates_existing_feed_additively(self, web_client, feeds_file,
                                                      fake_store, fake_embedder):
        _seed_feeds(feeds_file, feeds=[
            {"url": "https://pw.example/feed", "name": "My PW",
             "topics": ["news"], "skills": {"rust": 2}},
        ])
        preview = self._upload(web_client, self._bundle(fake_store, fake_embedder))
        assert "will gain the bundled influence entry" in preview.text
        token = preview.text.split('name="token" value="')[1].split('"')[0]

        web_client.post("/skills/import/confirm", data={"token": token})
        stored = _stored_feeds(feeds_file)
        assert len(stored) == 1
        assert stored[0]["name"] == "My PW"
        assert stored[0]["skills"] == {"rust": 2, "python": 8}
        assert stored[0]["topics"] == ["news"]

    def test_feed_only_bundle_is_not_nothing_to_do(self, web_client, feeds_file,
                                                   fake_store, fake_embedder):
        # Skill and memory already present; only the feed is new.
        data = self._bundle(fake_store, fake_embedder)
        _seed_exportable_skill(fake_store, fake_embedder)
        _seed_feeds(feeds_file, feeds=[])
        resp = self._upload(web_client, data)
        assert "Confirm Import" in resp.text
        assert "confirming would" not in resp.text
