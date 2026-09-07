"""Web UI tests for the licence field: the memories list filter and pill,
the detail page's classify form, the create form, and the feed editor."""

import pytest
import yaml

from memory.feed_influence import load_feed_influences
from tests.conftest import store_memory


@pytest.fixture
def feeds_file(tmp_path, monkeypatch):
    from web_ui.routes import feeds as feeds_module
    path = tmp_path / "feeds.yml"
    monkeypatch.setattr(feeds_module, "FEEDS_PATH", str(path))
    return path


def _seed(fake_store, fake_embedder):
    store_memory(fake_store, fake_embedder, "mem:knowledge:01U", "unknown article",
                 namespace="knowledge")
    fake_store.set_fields("mem:knowledge:01U", {"licence": "unknown", "feed_name": "Feed"})
    store_memory(fake_store, fake_embedder, "mem:episodic:01O", "own memory")
    fake_store.set_field("mem:episodic:01O", "licence", "own")


class TestMemoriesList:
    def test_filter_by_licence(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        resp = web_client.get("/memories?licence=unknown")
        assert resp.status_code == 200
        assert "mem:knowledge:01U" in resp.text
        assert "mem:episodic:01O" not in resp.text
        # The filter is echoed back as selected and carried on pagination links
        assert 'value="unknown" selected' in resp.text

    def test_unknown_gets_the_pill_and_others_do_not(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        resp = web_client.get("/memories")
        assert resp.text.count("licence-unknown") == 1

    def test_bad_licence_filter_is_ignored(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        resp = web_client.get("/memories?licence=cc-by")
        assert resp.status_code == 200
        assert "mem:knowledge:01U" in resp.text
        assert "mem:episodic:01O" in resp.text

    def test_filter_labels_are_associated(self, web_client):
        resp = web_client.get("/memories")
        for name in ("namespace", "state", "project", "licence", "sort"):
            assert f'for="filter-{name}"' in resp.text
            assert f'id="filter-{name}"' in resp.text

    def test_filter_survives_in_pagination_params(self, web_client, fake_store, fake_embedder):
        for i in range(30):
            store_memory(fake_store, fake_embedder, f"mem:knowledge:{i:03d}", f"article {i}",
                         namespace="knowledge")
            fake_store.set_field(f"mem:knowledge:{i:03d}", "licence", "unknown")
        resp = web_client.get("/memories?licence=unknown")
        assert "&amp;licence=unknown" in resp.text


class TestDetailPage:
    def test_shows_licence_and_form(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        fake_store.set_fields("mem:knowledge:01U", {"licence": "open", "licence_note": "OGL v3.0"})
        resp = web_client.get("/memory/mem:knowledge:01U")
        assert "Open (redistributable)" in resp.text
        assert "OGL v3.0" in resp.text
        assert 'action="/memory/mem:knowledge:01U/licence"' in resp.text
        assert 'value="open" selected' in resp.text

    def test_missing_field_reads_as_not_recorded(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01X", "no licence field")
        resp = web_client.get("/memory/mem:episodic:01X")
        assert "Not recorded" in resp.text

    def test_skill_page_has_no_licence_form(self, web_client, fake_store, fake_embedder):
        import time
        now = str(time.time())
        fake_store.upsert("skill", "mem:skill:gen:python-local", {
            "name": "python-local", "description": "d", "domain": "python",
            "user": "local", "state": "active", "generated": "true", "body": "---\n",
            "created_at": now, "updated_at": now, "content": "python skill",
        }, fake_embedder.embed("python"))
        resp = web_client.get("/memory/mem:skill:gen:python-local")
        assert resp.status_code == 200
        assert "/licence" not in resp.text

    def test_post_classifies(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        resp = web_client.post("/memory/mem:knowledge:01U/licence", data={
            "licence": "restricted", "licence_note": "  FT paywall ",
        }, follow_redirects=False)
        assert resp.status_code == 303
        assert resp.headers["location"] == "/memory/mem:knowledge:01U"
        data = fake_store.get("mem:knowledge:01U")
        assert data["licence"] == "restricted"
        assert data["licence_note"] == "FT paywall"

    def test_post_identifier_keeps_derived_note(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        web_client.post("/memory/mem:knowledge:01U/licence", data={"licence": "cc-by-4.0"})
        assert fake_store.get("mem:knowledge:01U")["licence_note"] == "CC BY 4.0"

    def test_post_reclassify_clears_stale_note(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        fake_store.set_fields("mem:knowledge:01U", {"licence": "open", "licence_note": "CC BY 4.0"})
        web_client.post("/memory/mem:knowledge:01U/licence", data={"licence": "restricted"})
        data = fake_store.get("mem:knowledge:01U")
        assert data["licence"] == "restricted"
        assert not data.get("licence_note")

    def test_post_invalid_bounces_with_error(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        resp = web_client.post("/memory/mem:knowledge:01U/licence", data={
            "licence": "wtfpl",
        }, follow_redirects=False)
        assert resp.status_code == 303
        assert "licence_error=" in resp.headers["location"]
        page = web_client.get(resp.headers["location"])
        assert "Unrecognised licence" in page.text
        assert fake_store.get("mem:knowledge:01U")["licence"] == "unknown"

    def test_unrecorded_licence_does_not_preselect_own(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01X", "no licence field")
        resp = web_client.get("/memory/mem:episodic:01X")
        assert 'value="" disabled selected>Not recorded' in resp.text
        assert 'value="own" selected' not in resp.text

    def test_post_to_skill_is_refused(self, web_client, fake_store, fake_embedder):
        import time
        now = str(time.time())
        fake_store.upsert("skill", "mem:skill:gen:python-local", {
            "name": "python-local", "state": "active", "generated": "true",
            "body": "---\n", "created_at": now, "updated_at": now,
        }, fake_embedder.embed("python"))
        resp = web_client.post("/memory/mem:skill:gen:python-local/licence", data={
            "licence": "open",
        }, follow_redirects=False)
        assert resp.status_code == 303
        assert "licence" not in fake_store.get("mem:skill:gen:python-local")

    def test_post_unknown_key_redirects(self, web_client):
        resp = web_client.post("/memory/mem:knowledge:GONE/licence", data={
            "licence": "open",
        }, follow_redirects=False)
        assert resp.status_code == 303


class TestCreateForm:
    def test_form_offers_classes(self, web_client):
        resp = web_client.get("/create")
        assert 'name="licence"' in resp.text
        assert "Restricted (not redistributable)" in resp.text

    def test_default_follows_namespace(self, web_client, fake_store):
        resp = web_client.post("/create", data={
            "content": "A knowledge write with no licence chosen",
            "namespace": "knowledge", "force": "on",
        }, follow_redirects=False)
        key = resp.headers["location"].split("/memory/")[1]
        assert fake_store.get(key)["licence"] == "unknown"

        resp = web_client.post("/create", data={
            "content": "An episodic write with no licence chosen", "force": "on",
        }, follow_redirects=False)
        key = resp.headers["location"].split("/memory/")[1]
        assert fake_store.get(key)["licence"] == "own"

    def test_explicit_licence_and_note(self, web_client, fake_store):
        resp = web_client.post("/create", data={
            "content": "Summary of a paywalled standard", "namespace": "knowledge",
            "licence": "restricted", "licence_note": "BSI paywall", "force": "on",
        }, follow_redirects=False)
        key = resp.headers["location"].split("/memory/")[1]
        data = fake_store.get(key)
        assert data["licence"] == "restricted"
        assert data["licence_note"] == "BSI paywall"

    def test_invalid_licence_rerenders_with_error(self, web_client, fake_store):
        resp = web_client.post("/create", data={
            "content": "Something", "licence": "wtfpl", "force": "on",
        })
        assert resp.status_code == 200
        assert "Unrecognised licence" in resp.text
        assert 'value="Something"' not in resp.text  # content is a textarea
        assert "Something" in resp.text
        assert not fake_store.scan_prefix("mem:episodic:")

    def test_empty_content_still_rerenders(self, web_client):
        resp = web_client.post("/create", data={"content": "   "})
        assert "Content cannot be empty" in resp.text

    def test_duplicate_rerenders_with_values(self, web_client, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01D", "identical content")
        resp = web_client.post("/create", data={
            "content": "identical content", "licence": "open",
        })
        assert "Near-duplicate found" in resp.text
        assert 'value="open" selected' in resp.text


class TestFeedEditor:
    def test_create_with_licence_writes_yaml_and_mirror(self, web_client, feeds_file, fake_store):
        feeds_file.write_text(yaml.dump({"feeds": []}))
        resp = web_client.post("/feeds/new", data={
            "name": "Gov Feed", "url": "https://gov.example/feed",
            "licence": "open", "licence_note": "OGL v3.0",
            "skill_domain": "", "skill_influence": "5",
        }, follow_redirects=False)
        assert resp.status_code == 303
        stored = yaml.safe_load(feeds_file.read_text())["feeds"][0]
        assert stored["licence"] == "open"
        assert stored["licence_note"] == "OGL v3.0"
        assert load_feed_influences(fake_store.client)["Gov Feed"]["licence"] == "open"

    def test_create_without_licence_omits_keys(self, web_client, feeds_file):
        feeds_file.write_text(yaml.dump({"feeds": []}))
        web_client.post("/feeds/new", data={
            "name": "Plain", "url": "https://plain.example/feed",
            "skill_domain": "", "skill_influence": "5",
        }, follow_redirects=False)
        stored = yaml.safe_load(feeds_file.read_text())["feeds"][0]
        assert "licence" not in stored and "licence_note" not in stored

    def test_create_invalid_licence_bounces(self, web_client, feeds_file):
        feeds_file.write_text(yaml.dump({"feeds": []}))
        resp = web_client.post("/feeds/new", data={
            "name": "Bad", "url": "https://bad.example/feed", "licence": "wtfpl",
            "skill_domain": "", "skill_influence": "5",
        }, follow_redirects=False)
        assert resp.status_code == 303
        assert resp.headers["location"].startswith("/feeds/new?error=")
        assert yaml.safe_load(feeds_file.read_text())["feeds"] == []

    def test_edit_form_shows_declared_identifier_as_class_plus_note(self, web_client, feeds_file):
        feeds_file.write_text(yaml.dump({"feeds": [
            {"url": "https://gov.example/feed", "name": "Gov", "licence": "ogl-3.0"},
        ]}))
        resp = web_client.get("/feeds/0/edit")
        assert 'value="open" selected' in resp.text
        assert 'value="OGL v3.0"' in resp.text

    def test_edit_form_unparseable_licence_shows_unknown(self, web_client, feeds_file):
        feeds_file.write_text(yaml.dump({"feeds": [
            {"url": "https://x.example/feed", "name": "X", "licence": "wtfpl"},
        ]}))
        resp = web_client.get("/feeds/0/edit")
        assert 'value="unknown" selected' in resp.text

    def test_edit_form_undeclared_selects_placeholder(self, web_client, feeds_file):
        feeds_file.write_text(yaml.dump({"feeds": [
            {"url": "https://x.example/feed", "name": "X"},
        ]}))
        resp = web_client.get("/feeds/0/edit")
        assert "selected" not in resp.text.split('id="feed-licence"')[1].split("</select>")[0]

    def test_save_replaces_licence(self, web_client, feeds_file, fake_store):
        feeds_file.write_text(yaml.dump({"feeds": [
            {"url": "https://x.example/feed", "name": "X", "licence": "open"},
        ]}))
        web_client.post("/feeds/0/edit", data={
            "name": "X", "url": "https://x.example/feed", "licence": "restricted",
            "skill_domain": "", "skill_influence": "5",
        }, follow_redirects=False)
        stored = yaml.safe_load(feeds_file.read_text())["feeds"][0]
        assert stored["licence"] == "restricted"
        assert "licence_note" not in stored

    def test_save_invalid_licence_bounces_to_edit(self, web_client, feeds_file):
        feeds_file.write_text(yaml.dump({"feeds": [
            {"url": "https://x.example/feed", "name": "X"},
        ]}))
        resp = web_client.post("/feeds/0/edit", data={
            "name": "X", "url": "https://x.example/feed", "licence": "wtfpl",
            "skill_domain": "", "skill_influence": "5",
        }, follow_redirects=False)
        assert resp.headers["location"].startswith("/feeds/0/edit?error=")

    def test_list_shows_licence_state(self, web_client, feeds_file):
        feeds_file.write_text(yaml.dump({"feeds": [
            {"url": "https://a.example/feed", "name": "A", "licence": "cc-by-4.0"},
            {"url": "https://b.example/feed", "name": "B"},
        ]}))
        resp = web_client.get("/feeds")
        assert resp.text.count("licence-unknown") == 1
        assert ">open<" in resp.text


class TestProjectWriters:
    def test_edit_stamps_own(self, web_client, fake_store, fake_embedder):
        from tests.test_web_project_domains import seed_project
        seed_project(fake_store, fake_embedder)
        web_client.post("/projects/webproj/edit", data={
            "description": "d", "stack": "python", "goals": "g",
            "current_state": "s", "notes": "", "domains": "",
        }, follow_redirects=False)
        assert fake_store.get("mem:project:webproj")["licence"] == "own"

    def test_create_stamps_own(self, web_client, fake_store):
        web_client.post("/projects/new", data={
            "name": "fresh", "description": "d", "stack": "python", "goals": "g",
            "current_state": "s", "notes": "", "domains": "",
        }, follow_redirects=False)
        assert fake_store.get("mem:project:fresh")["licence"] == "own"


class TestStaleNoteOnClassChange:
    def test_detail_drops_prefilled_note_when_class_changes(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        fake_store.set_fields("mem:knowledge:01U", {"licence": "open", "licence_note": "CC BY 4.0"})
        # The form re-submits the pre-filled note verbatim
        web_client.post("/memory/mem:knowledge:01U/licence", data={
            "licence": "restricted", "licence_note": "CC BY 4.0",
        })
        data = fake_store.get("mem:knowledge:01U")
        assert data["licence"] == "restricted"
        assert not data.get("licence_note")

    def test_detail_keeps_a_typed_note_when_class_changes(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        fake_store.set_fields("mem:knowledge:01U", {"licence": "open", "licence_note": "CC BY 4.0"})
        web_client.post("/memory/mem:knowledge:01U/licence", data={
            "licence": "restricted", "licence_note": "Actually paywalled",
        })
        assert fake_store.get("mem:knowledge:01U")["licence_note"] == "Actually paywalled"

    def test_detail_keeps_note_when_class_unchanged(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        fake_store.set_fields("mem:knowledge:01U", {"licence": "open", "licence_note": "CC BY 4.0"})
        web_client.post("/memory/mem:knowledge:01U/licence", data={
            "licence": "open", "licence_note": "CC BY 4.0",
        })
        assert fake_store.get("mem:knowledge:01U")["licence_note"] == "CC BY 4.0"

    def test_detail_cascades_to_facts(self, web_client, fake_store, fake_embedder):
        _seed(fake_store, fake_embedder)
        store_memory(fake_store, fake_embedder, "mem:knowledge:01F", "fact", namespace="knowledge")
        fake_store.set_fields("mem:knowledge:01F", {"enriched_from": "mem:episodic:01O", "licence": "own"})
        web_client.post("/memory/mem:episodic:01O/licence", data={"licence": "restricted"})
        assert fake_store.get("mem:knowledge:01F")["licence"] == "restricted"

    def test_detail_refuses_non_memory_keys(self, web_client, fake_store):
        fake_store.client.hset("meta:feed:influence", mapping={"A": "{}"})
        resp = web_client.post("/memory/meta:feed:influence/licence", data={"licence": "own"},
                               follow_redirects=False)
        assert resp.status_code == 303
        assert "licence" not in fake_store.client.hgetall("meta:feed:influence")

    def test_feed_edit_drops_prefilled_note_when_class_changes(self, web_client, feeds_file):
        feeds_file.write_text(yaml.dump({"feeds": [
            {"url": "https://gov.example/feed", "name": "Gov", "licence": "ogl-3.0"},
        ]}))
        # Form shows class=open, note=OGL v3.0; user switches to restricted
        web_client.post("/feeds/0/edit", data={
            "name": "Gov", "url": "https://gov.example/feed",
            "licence": "restricted", "licence_note": "OGL v3.0",
            "skill_domain": "", "skill_influence": "5",
        }, follow_redirects=False)
        stored = yaml.safe_load(feeds_file.read_text())["feeds"][0]
        assert stored["licence"] == "restricted"
        assert "licence_note" not in stored

    def test_feed_edit_keeps_note_when_class_unchanged(self, web_client, feeds_file):
        feeds_file.write_text(yaml.dump({"feeds": [
            {"url": "https://gov.example/feed", "name": "Gov", "licence": "ogl-3.0"},
        ]}))
        web_client.post("/feeds/0/edit", data={
            "name": "Gov", "url": "https://gov.example/feed",
            "licence": "open", "licence_note": "OGL v3.0",
            "skill_domain": "", "skill_influence": "5",
        }, follow_redirects=False)
        stored = yaml.safe_load(feeds_file.read_text())["feeds"][0]
        assert stored == {"url": "https://gov.example/feed", "name": "Gov", "topics": [],
                          "licence": "open", "licence_note": "OGL v3.0"}

    def test_feed_edit_from_undeclared_keeps_typed_note(self, web_client, feeds_file):
        feeds_file.write_text(yaml.dump({"feeds": [
            {"url": "https://x.example/feed", "name": "X"},
        ]}))
        web_client.post("/feeds/0/edit", data={
            "name": "X", "url": "https://x.example/feed",
            "licence": "open", "licence_note": "checked",
            "skill_domain": "", "skill_influence": "5",
        }, follow_redirects=False)
        assert yaml.safe_load(feeds_file.read_text())["feeds"][0]["licence_note"] == "checked"

    def test_feed_list_yaml_boolean_shows_unclassified(self, web_client, feeds_file):
        feeds_file.write_text(yaml.dump({"feeds": [
            {"url": "https://x.example/feed", "name": "X", "licence": False},
        ]}))
        resp = web_client.get("/feeds")
        assert "licence-unknown" in resp.text

    def test_web_restore_backfills(self, web_client, fake_store, tmp_path, monkeypatch):
        import json
        monkeypatch.setenv("BACKUP_DIR", str(tmp_path))
        dump = {"version": "6.6.0", "created_at": 1.0, "metadata": {},
                "data": {"mem:episodic:01A": {"content": "x", "state": "active",
                                              "created_at": "1.0", "updated_at": "1.0"}}}
        (tmp_path / "old.json").write_text(json.dumps(dump))
        resp = web_client.post("/backups/old.json/restore", follow_redirects=False)
        assert resp.status_code in (200, 303), resp.text[:200]
        assert fake_store.get("mem:episodic:01A")["licence"] == "own"
