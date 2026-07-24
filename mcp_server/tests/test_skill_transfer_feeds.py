"""Tests for feed influence travelling in skill bundles: export feeds.json,
format v1/v2 validation, the feed merge, and import planning."""

import io
import json
import time
import zipfile

import pytest

from memory.feed_influence import sync_feed_influences
from memory.skill_transfer import (
    EXPORT_FORMAT_VERSION,
    _influencing_feeds,
    _validate_feeds,
    build_skill_export,
    merge_feed_influences,
    plan_skill_import,
    validate_skill_import,
)


def _seed_skill(fake_store, fake_embedder, *, domain="python",
                key="mem:skill:gen:python-local",
                sources=("mem:episodic:01A",)):
    now = str(time.time())
    body = "---\nname: python-local\n---\n\n## Do\n\n- Use uv. [mem:episodic:01A]\n"
    fake_store.upsert("skill", key, {
        "name": f"{domain}-local", "description": "Python procedure.",
        "domain": domain, "user": "local", "state": "active",
        "generated": "true", "body": body, "contract_version": "1",
        "compiled_at": now, "created_at": now, "updated_at": now,
        "source_manifest": json.dumps(sorted(sources)),
        "rule_manifest": json.dumps([]),
    }, fake_embedder.embed("python skill"))
    for src in sources:
        fake_store.upsert(src.split(":")[1], src, {
            "content": f"Lesson at {src}", "state": "active",
            "created_at": now, "updated_at": now,
        }, fake_embedder.embed(src))
    return key


FEEDS = [
    {"name": "Python Weekly", "url": "https://pw.example/feed",
     "topics": ["python"], "skills": {"python": 8}},
    {"name": "Rust Blog", "url": "https://rust.example/feed",
     "skills": {"rust": 5}},
]


def _export(fake_store, fake_embedder, with_feeds=True):
    key = _seed_skill(fake_store, fake_embedder)
    if with_feeds:
        sync_feed_influences(fake_store.client, FEEDS)
    bundle, err = build_skill_export(fake_store, key)
    assert err is None
    return bundle


def _patch_bundle(data: bytes, *, feeds=None, drop_feeds=False,
                  format_version=None) -> bytes:
    """Rewrite feeds.json (and refresh checksums) in a bundle zip."""
    entries: dict[str, bytes] = {}
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        for name in zf.namelist():
            entries[name] = zf.read(name)
    if drop_feeds:
        entries.pop("feeds.json", None)
    elif feeds is not None:
        entries["feeds.json"] = json.dumps(feeds).encode("utf-8")

    import hashlib
    manifest = json.loads(entries["manifest.json"])
    if format_version is not None:
        manifest["format_version"] = format_version
    manifest["checksums"] = {
        name: hashlib.sha256(content).hexdigest()
        for name, content in entries.items() if name != "manifest.json"
    }
    entries["manifest.json"] = json.dumps(manifest).encode("utf-8")

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        for name, content in entries.items():
            zf.writestr(name, content)
    return buf.getvalue()


class TestExportFeeds:
    def test_bundle_carries_only_influencing_feeds(self, fake_store, fake_embedder):
        bundle = _export(fake_store, fake_embedder)
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            feeds = json.loads(zf.read("feeds.json"))
            manifest = json.loads(zf.read("manifest.json"))
        assert [f["name"] for f in feeds] == ["Python Weekly"]
        assert feeds[0]["skills"] == {"python": 8}
        assert feeds[0]["topics"] == ["python"]
        assert manifest["feed_count"] == 1
        assert manifest["format_version"] == EXPORT_FORMAT_VERSION == 2
        assert "feeds.json" in manifest["checksums"]

    def test_no_influencing_feeds_no_feeds_json(self, fake_store, fake_embedder):
        bundle = _export(fake_store, fake_embedder, with_feeds=False)
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            assert "feeds.json" not in zf.namelist()
            assert json.loads(zf.read("manifest.json"))["feed_count"] == 0

    def test_empty_domain_yields_no_feeds(self, fake_store):
        assert _influencing_feeds(fake_store, "") == []

    def test_mode_and_project_travel_and_validate(self, fake_store, fake_embedder):
        key = _seed_skill(fake_store, fake_embedder)
        sync_feed_influences(fake_store.client, [
            {"name": "Digest Feed", "url": "https://d.example/feed",
             "mode": "digest", "project": "research",
             "skills": {"python": 4}},
        ])
        bundle, err = build_skill_export(fake_store, key)
        assert err is None
        result = validate_skill_import(bundle["data"])
        assert result["ok"], result
        feed = result["feeds"][0]
        assert feed["mode"] == "digest"
        assert feed["project"] == "research"


class TestValidateFeeds:
    def test_roundtrip_valid(self, fake_store, fake_embedder):
        bundle = _export(fake_store, fake_embedder)
        result = validate_skill_import(bundle["data"])
        assert result["ok"], result
        assert [f["name"] for f in result["feeds"]] == ["Python Weekly"]

    def test_v1_bundle_without_feeds_still_imports(self, fake_store, fake_embedder):
        bundle = _export(fake_store, fake_embedder, with_feeds=False)
        data = _patch_bundle(bundle["data"], format_version=1)
        result = validate_skill_import(data)
        assert result["ok"], result
        assert result["feeds"] == []

    def test_unsupported_version_rejected(self, fake_store, fake_embedder):
        bundle = _export(fake_store, fake_embedder, with_feeds=False)
        data = _patch_bundle(bundle["data"], format_version=3)
        result = validate_skill_import(data)
        assert not result["ok"]
        assert "format version" in result["error"]

    def test_tampered_feeds_checksum_rejected(self, fake_store, fake_embedder):
        bundle = _export(fake_store, fake_embedder)
        entries = {}
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            for name in zf.namelist():
                entries[name] = zf.read(name)
        entries["feeds.json"] = b'[{"name": "Evil", "url": "https://evil"}]'
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as zf:
            for name, content in entries.items():
                zf.writestr(name, content)
        result = validate_skill_import(buf.getvalue())
        assert not result["ok"]
        assert "Checksum mismatch" in result["error"]

    def test_validate_feeds_unreadable_and_bad_json(self):
        feeds, err = _validate_feeds(None, "python")
        assert feeds == [] and "Could not read" in err
        feeds, err = _validate_feeds(b"{not json", "python")
        assert feeds == [] and "not valid JSON" in err

    @pytest.mark.parametrize("feeds,message", [
        ({"not": "a list"}, "must be a list"),
        (["just a string"], "must be an object"),
        ([{"name": "F", "url": "ftp://x.example", "skills": {"python": 5}}], "invalid url"),
        ([{"name": "", "url": "https://x.example", "skills": {"python": 5}}], "invalid name"),
        ([{"name": "F", "url": "https://x.example", "skills": {"python": 99}}], "between"),
        ([{"name": "F", "url": "https://x.example", "skills": {"rust": 5}}], "bundle's domain"),
        ([{"name": "F", "url": "https://x.example",
           "skills": {"python": 5, "rust": 5}}], "exactly one"),
        ([{"name": "F", "url": "https://x.example", "skills": {}}], "bundle's domain"),
        ([{"name": "F", "url": "https://x.example", "skills": {"python": 5},
           "mode": "firehose"}], "invalid mode"),
        ([{"name": "F", "url": "https://x.example", "skills": {"python": 5},
           "topics": "python"}], "invalid topics"),
        ([{"name": "F", "url": "https://x.example", "skills": {"python": 5},
           "project": 42}], "invalid project"),
        ([{"name": "A", "url": "https://x.example", "skills": {"python": 5}},
          {"name": "B", "url": "https://x.example", "skills": {"python": 3}}], "twice"),
    ])
    def test_invalid_feeds_rejected(self, fake_store, fake_embedder, feeds, message):
        bundle = _export(fake_store, fake_embedder)
        result = validate_skill_import(_patch_bundle(bundle["data"], feeds=feeds))
        assert not result["ok"]
        assert message in result["error"]

    def test_too_many_feeds_rejected(self, fake_store, fake_embedder):
        bundle = _export(fake_store, fake_embedder)
        feeds = [{"name": f"F{i}", "url": f"https://f{i}.example",
                  "skills": {"python": 5}} for i in range(51)]
        result = validate_skill_import(_patch_bundle(bundle["data"], feeds=feeds))
        assert not result["ok"]
        assert "too many feeds" in result["error"]


BUNDLED = [{"name": "Python Weekly", "url": "https://pw.example/feed",
            "topics": ["python"], "skills": {"python": 8}}]


class TestMergeFeedInfluences:
    def test_new_feed_appended(self):
        merged, added, updated, skipped = merge_feed_influences([], BUNDLED)
        assert [f["name"] for f in merged] == ["Python Weekly"]
        assert added == ["Python Weekly"] and not updated and not skipped

    def test_existing_url_gains_missing_entry_only(self):
        current = [{"name": "PW", "url": "https://pw.example/feed",
                    "topics": ["news"], "skills": {"rust": 2}}]
        merged, added, updated, skipped = merge_feed_influences(current, BUNDLED)
        assert not added and updated == ["PW"] and not skipped
        assert merged[0]["skills"] == {"rust": 2, "python": 8}
        assert merged[0]["topics"] == ["news"]  # untouched
        assert merged[0]["name"] == "PW"       # untouched

    def test_existing_score_never_overwritten(self):
        current = [{"name": "PW", "url": "https://pw.example/feed",
                    "skills": {"python": 1}}]
        merged, added, updated, skipped = merge_feed_influences(current, BUNDLED)
        assert skipped == ["PW"] and not added and not updated
        assert merged[0]["skills"] == {"python": 1}

    def test_name_collision_gets_imported_suffix(self):
        current = [{"name": "Python Weekly", "url": "https://other.example/feed"}]
        merged, added, updated, skipped = merge_feed_influences(current, BUNDLED)
        assert added == ["Python Weekly (imported)"]
        assert merged[1]["url"] == "https://pw.example/feed"

    def test_double_collision_skips(self):
        current = [
            {"name": "Python Weekly", "url": "https://other.example/feed"},
            {"name": "Python Weekly (imported)", "url": "https://third.example/feed"},
        ]
        merged, added, updated, skipped = merge_feed_influences(current, BUNDLED)
        assert skipped == ["Python Weekly"] and not added
        assert len(merged) == 2

    def test_replay_is_noop(self):
        merged, *_ = merge_feed_influences([], BUNDLED)
        merged2, added, updated, skipped = merge_feed_influences(merged, BUNDLED)
        assert not added and not updated and skipped == ["Python Weekly"]
        assert merged2 == merged

    def test_current_feed_with_broken_skills_field(self):
        current = [{"name": "PW", "url": "https://pw.example/feed",
                    "skills": "not a dict"}]
        merged, added, updated, skipped = merge_feed_influences(current, BUNDLED)
        assert updated == ["PW"]
        assert merged[0]["skills"] == {"python": 8}


class TestPlanWithFeeds:
    def test_plan_reports_feed_changes(self, fake_store, fake_embedder):
        bundle = _export(fake_store, fake_embedder)
        result = validate_skill_import(bundle["data"])
        plan = plan_skill_import(fake_store, result, current_feeds=[])
        assert plan["new_feeds"] == ["Python Weekly"]
        assert plan["updated_feeds"] == [] and plan["skipped_feeds"] == []

    def test_plan_without_reading_list_skips_feed_planning(self, fake_store, fake_embedder):
        bundle = _export(fake_store, fake_embedder)
        result = validate_skill_import(bundle["data"])
        plan = plan_skill_import(fake_store, result)
        assert plan["new_feeds"] == []
        assert plan["skill_exists"] is True
