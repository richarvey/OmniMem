"""Tests for the skill transfer engine: export bundles, validation, import."""

import io
import json
import time
import zipfile

import pytest

from memory.skill_transfer import (
    EXPORT_FORMAT,
    EXPORT_FORMAT_VERSION,
    apply_skill_import,
    build_skill_export,
    plan_skill_import,
    validate_skill_import,
)


def _seed_skill(fake_store, fake_embedder, *,
                key="mem:skill:gen:python-local",
                sources=("mem:episodic:01A", "mem:episodic:01B",
                         "mem:knowledge:01C"),
                **overrides):
    """A compiled skill plus the source memories its manifest cites."""
    now = str(time.time())
    body = "---\nname: python-local\n---\n\n## Do\n\n- Use uv. [mem:episodic:01A]\n"
    fields = {
        "name": "python-local",
        "description": "Distilled python procedure. Load when: python work.",
        "domain": "python",
        "user": "local",
        "state": "active",
        "generated": "true",
        "body": body,
        "contract_version": "1",
        "compiled_at": now,
        "created_at": now,
        "updated_at": now,
        "recall_count": "7",
        "last_recalled": now,
        "source_manifest": json.dumps(sorted(sources)),
        "rule_manifest": json.dumps([
            {"kind": "do", "text": "Use uv", "sources": list(sources[:2]),
             "reinforcement": 2},
        ]),
    }
    fields.update(overrides)
    fake_store.upsert("skill", key, fields, fake_embedder.embed("python skill"))

    for src in sources:
        namespace = src.split(":")[1]
        fake_store.upsert(namespace, src, {
            "content": f"Lesson stored at {src}",
            "state": "active",
            "tags": json.dumps(["python"]),
            "recall_count": "3",
            "created_at": now,
            "updated_at": now,
        }, fake_embedder.embed(f"lesson {src}"))
    return key


def _rebuild_zip(data: bytes, *, replace=None, drop=None, add=None,
                 manifest_patch=None, refresh_checksums=False) -> bytes:
    """Copy a bundle zip with targeted tampering."""
    replace = replace or {}
    drop = drop or set()
    add = add or {}
    entries: dict[str, bytes] = {}
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        for name in zf.namelist():
            if name in drop:
                continue
            entries[name] = replace.get(name, zf.read(name))
    entries.update(add)

    if manifest_patch or refresh_checksums:
        manifest = json.loads(entries["manifest.json"])
        if refresh_checksums:
            import hashlib
            manifest["checksums"] = {
                name: hashlib.sha256(content).hexdigest()
                for name, content in entries.items()
                if name != "manifest.json"
            }
        if manifest_patch:
            manifest.update(manifest_patch)
        entries["manifest.json"] = json.dumps(manifest).encode()

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        for name, content in entries.items():
            zf.writestr(name, content)
    return buf.getvalue()


@pytest.fixture
def bundle(fake_store, fake_embedder):
    _seed_skill(fake_store, fake_embedder)
    out, err = build_skill_export(fake_store, "mem:skill:gen:python-local")
    assert err is None
    return out


class TestExport:
    def test_bundle_contains_skill_and_memories(self, bundle):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            names = set(zf.namelist())
            assert {"manifest.json", "skill.json", "SKILL.md"} <= names
            assert len([n for n in names if n.startswith("memories/")]) == 3

            manifest = json.loads(zf.read("manifest.json"))
            assert manifest["format"] == EXPORT_FORMAT
            assert manifest["format_version"] == EXPORT_FORMAT_VERSION
            assert manifest["skill_key"] == "mem:skill:gen:python-local"
            assert manifest["memory_count"] == 3

            skill = json.loads(zf.read("skill.json"))
            assert skill["generated"] == "true"
            assert zf.read("SKILL.md").decode() == skill["body"]

        assert bundle["memory_count"] == 3
        assert bundle["missing_sources"] == []
        assert bundle["filename"].startswith("omnimem_skill_python-local_")
        assert bundle["filename"].endswith(".zip")

    def test_instance_local_fields_stripped(self, bundle):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            skill = json.loads(zf.read("skill.json"))
            assert "recall_count" not in skill
            assert "last_recalled" not in skill
            assert "vector" not in skill
            mem = json.loads(zf.read("memories/0000.json"))
            assert "recall_count" not in mem["fields"]
            assert "vector" not in mem["fields"]

    def test_refuses_non_skill_key(self, fake_store):
        out, err = build_skill_export(fake_store, "mem:episodic:01A")
        assert out is None and "Not a skill key" in err

    def test_refuses_missing_skill(self, fake_store):
        out, err = build_skill_export(fake_store, "mem:skill:gen:nope-local")
        assert out is None and "not found" in err

    def test_refuses_non_generated_skill(self, fake_store, fake_embedder):
        _seed_skill(fake_store, fake_embedder, generated="false")
        out, err = build_skill_export(fake_store, "mem:skill:gen:python-local")
        assert out is None and "generated" in err

    def test_missing_source_reported_not_fatal(self, fake_store, fake_embedder):
        _seed_skill(fake_store, fake_embedder)
        fake_store.delete("mem:episodic:01B")
        out, err = build_skill_export(fake_store, "mem:skill:gen:python-local")
        assert err is None
        assert out["memory_count"] == 2
        assert out["missing_sources"] == ["mem:episodic:01B"]


class TestValidate:
    def test_roundtrip_ok(self, bundle):
        result = validate_skill_import(bundle["data"])
        assert result["ok"] is True
        assert result["skill_key"] == "mem:skill:gen:python-local"
        assert len(result["memories"]) == 3
        assert result["warnings"] == []
        assert result["manifest"]["domain"] == "python"

    def test_rejects_empty_and_non_zip(self):
        assert validate_skill_import(b"")["ok"] is False
        result = validate_skill_import(b"definitely not a zip file")
        assert result["ok"] is False
        assert "zip" in result["error"].lower()

    def test_rejects_missing_manifest(self, bundle):
        data = _rebuild_zip(bundle["data"], drop={"manifest.json"})
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "manifest" in result["error"]

    def test_rejects_wrong_format_marker(self, bundle):
        data = _rebuild_zip(bundle["data"], manifest_patch={"format": "other"})
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "format" in result["error"].lower()

    def test_rejects_future_format_version(self, bundle):
        data = _rebuild_zip(bundle["data"], manifest_patch={"format_version": 99})
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "version" in result["error"]

    def test_rejects_tampered_payload(self, bundle):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            mem = json.loads(zf.read("memories/0000.json"))
        mem["fields"]["content"] = "tampered after export"
        data = _rebuild_zip(bundle["data"], replace={
            "memories/0000.json": json.dumps(mem).encode(),
        })
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "Checksum mismatch" in result["error"]

    def test_rejects_unexpected_entry(self, bundle):
        data = _rebuild_zip(bundle["data"], add={"evil.sh": b"#!/bin/sh"})
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "Unexpected entry" in result["error"]

    def test_rejects_non_generated_skill(self, bundle):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            skill = json.loads(zf.read("skill.json"))
        skill["generated"] = "false"
        data = _rebuild_zip(bundle["data"], replace={
            "skill.json": json.dumps(skill).encode(),
        }, refresh_checksums=True)
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "generated" in result["error"]

    def test_rejects_invalid_domain(self, bundle):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            skill = json.loads(zf.read("skill.json"))
        skill["domain"] = "../etc"
        data = _rebuild_zip(bundle["data"], replace={
            "skill.json": json.dumps(skill).encode(),
        }, refresh_checksums=True)
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "domain" in result["error"]

    def test_rejects_skill_key_mismatch(self, bundle):
        data = _rebuild_zip(bundle["data"], manifest_patch={
            "skill_key": "mem:skill:gen:other-local",
        })
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "skill_key" in result["error"]

    def test_rejects_bad_memory_key(self, bundle):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            mem = json.loads(zf.read("memories/0000.json"))
        mem["key"] = "mem:project:sneaky"
        data = _rebuild_zip(bundle["data"], replace={
            "memories/0000.json": json.dumps(mem).encode(),
        }, refresh_checksums=True)
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "invalid memory key" in result["error"]

    def test_rejects_memory_without_content(self, bundle):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            mem = json.loads(zf.read("memories/0000.json"))
        del mem["fields"]["content"]
        data = _rebuild_zip(bundle["data"], replace={
            "memories/0000.json": json.dumps(mem).encode(),
        }, refresh_checksums=True)
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "content" in result["error"]

    def test_rejects_non_string_field(self, bundle):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            mem = json.loads(zf.read("memories/0000.json"))
        mem["fields"]["effort_score"] = 9
        data = _rebuild_zip(bundle["data"], replace={
            "memories/0000.json": json.dumps(mem).encode(),
        }, refresh_checksums=True)
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "not a string" in result["error"]

    def test_rejects_skill_md_body_mismatch(self, bundle):
        data = _rebuild_zip(bundle["data"], replace={
            "SKILL.md": b"different body",
        }, refresh_checksums=True)
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "SKILL.md" in result["error"]

    def test_rejects_uncited_memory(self, bundle):
        extra = json.dumps({
            "key": "mem:episodic:99Z",
            "fields": {"content": "smuggled in", "state": "active"},
        }).encode()
        data = _rebuild_zip(
            bundle["data"],
            add={"memories/0003.json": extra},
            manifest_patch={"memory_count": 4},
            refresh_checksums=True,
        )
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "source_manifest" in result["error"]

    def test_rejects_memory_count_mismatch(self, bundle):
        data = _rebuild_zip(bundle["data"], manifest_patch={"memory_count": 12})
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "memory_count" in result["error"]

    def test_warns_on_sources_missing_from_bundle(self, bundle):
        data = _rebuild_zip(
            bundle["data"],
            drop={"memories/0002.json"},
            manifest_patch={"memory_count": 2},
        )
        result = validate_skill_import(data)
        assert result["ok"] is True
        assert len(result["warnings"]) == 1
        assert "not in the bundle" in result["warnings"][0]

    def test_oversized_upload_rejected(self):
        result = validate_skill_import(b"x" * (20 * 1024 * 1024 + 1))
        assert result["ok"] is False
        assert "too large" in result["error"]


class TestPlanAndApply:
    def test_plan_against_empty_store(self, bundle, fake_embedder):
        from tests.conftest import FakeValkeyStore
        target = FakeValkeyStore()
        result = validate_skill_import(bundle["data"])
        plan = plan_skill_import(target, result)
        assert plan["skill_exists"] is False
        assert len(plan["new_memories"]) == 3
        assert plan["existing_memories"] == []

    def test_apply_writes_everything_and_replay_is_noop(self, bundle, fake_embedder):
        from tests.conftest import FakeValkeyStore
        target = FakeValkeyStore()
        result = validate_skill_import(bundle["data"])

        summary = apply_skill_import(target, fake_embedder, result)
        assert summary["skill_written"] is True
        assert len(summary["memories_written"]) == 3

        skill = target.get("mem:skill:gen:python-local")
        assert skill["generated"] == "true"
        assert skill["imported_at"]
        assert "recall_count" not in skill
        # Re-embedded locally: the raw hash carries a fresh vector.
        assert "vector" in target.client._data["mem:skill:gen:python-local"]
        assert "vector" in target.client._data["mem:episodic:01A"]

        replay = apply_skill_import(target, fake_embedder, result)
        assert replay["skill_written"] is False
        assert replay["memories_written"] == []
        assert len(replay["memories_skipped"]) == 3

    def test_apply_never_overwrites_existing_memory(self, bundle, fake_embedder):
        from tests.conftest import FakeValkeyStore
        target = FakeValkeyStore()
        target.upsert("episodic", "mem:episodic:01A", {
            "content": "the local version stays",
            "state": "active",
        }, fake_embedder.embed("local"))

        result = validate_skill_import(bundle["data"])
        summary = apply_skill_import(target, fake_embedder, result)
        assert "mem:episodic:01A" in summary["memories_skipped"]
        assert target.get("mem:episodic:01A")["content"] == "the local version stays"

    def test_apply_skips_existing_skill_but_adds_memories(
        self, bundle, fake_store, fake_embedder,
    ):
        # The source store already has the skill and all memories — but wipe
        # one memory to prove memories still flow when the skill is skipped.
        fake_store.delete("mem:episodic:01A")
        result = validate_skill_import(bundle["data"])
        summary = apply_skill_import(fake_store, fake_embedder, result)
        assert summary["skill_written"] is False
        assert summary["memories_written"] == ["mem:episodic:01A"]


class TestValidateEdgeCases:
    """The validation branches the round-trip tests can't reach."""

    def _patch_skill(self, bundle, mutate):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            skill = json.loads(zf.read("skill.json"))
        mutate(skill)
        body = skill.get("body", "")
        return _rebuild_zip(bundle["data"], replace={
            "skill.json": json.dumps(skill).encode(),
            "SKILL.md": body.encode() if isinstance(body, str) else b"",
        }, refresh_checksums=True)

    def test_export_skips_uncitable_manifest_keys(self, fake_store, fake_embedder):
        _seed_skill(fake_store, fake_embedder,
                    sources=("mem:episodic:01A", "mem:project:oddball"))
        fake_store.upsert("project", "mem:project:oddball", {
            "content": "not a valid skill source", "state": "active",
        }, fake_embedder.embed("odd"))
        out, err = build_skill_export(fake_store, "mem:skill:gen:python-local")
        assert err is None
        assert out["memory_count"] == 1
        assert "mem:project:oddball" in out["missing_sources"]

    def test_duplicate_zip_entries_rejected(self, bundle):
        buf = io.BytesIO()
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as src, \
                zipfile.ZipFile(buf, "w") as dst:
            for name in src.namelist():
                dst.writestr(name, src.read(name))
            dst.writestr("SKILL.md", src.read("SKILL.md"))
        result = validate_skill_import(buf.getvalue())
        assert result["ok"] is False
        assert "duplicate entries" in result["error"]

    def test_oversized_entry_rejected(self, bundle):
        big = json.dumps({
            "key": "mem:episodic:01BIG",
            "fields": {"content": "x" * (1024 * 1024 + 100)},
        }).encode()
        data = _rebuild_zip(bundle["data"], add={"memories/0009.json": big},
                            refresh_checksums=True)
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "too large" in result["error"]

    def test_manifest_not_json_and_not_object(self, bundle):
        for raw in (b"{broken", b'["a list"]'):
            data = _rebuild_zip(bundle["data"], replace={"manifest.json": raw})
            result = validate_skill_import(data)
            assert result["ok"] is False
            assert "manifest" in result["error"].lower()

    def test_checksums_missing_entirely(self, bundle):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            manifest = json.loads(zf.read("manifest.json"))
        del manifest["checksums"]
        data = _rebuild_zip(bundle["data"], replace={
            "manifest.json": json.dumps(manifest).encode(),
        })
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "checksums" in result["error"]

    def test_skill_json_shapes_rejected(self, bundle):
        data = _rebuild_zip(bundle["data"], replace={
            "skill.json": b"{broken",
        }, refresh_checksums=True)
        assert "skill.json" in validate_skill_import(data)["error"]

        data = _rebuild_zip(bundle["data"], replace={
            "skill.json": b'["list"]',
        }, refresh_checksums=True)
        assert "skill.json" in validate_skill_import(data)["error"]

    def test_skill_field_values_must_be_strings(self, bundle):
        data = self._patch_skill(bundle, lambda s: s.update(compiled_at=123))
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "strings" in result["error"]

    def test_skill_field_too_large(self, bundle):
        data = self._patch_skill(
            bundle, lambda s: s.update(description="x" * 100_001),
        )
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "too large" in result["error"]

    def test_skill_name_and_body_requirements(self, bundle):
        data = self._patch_skill(bundle, lambda s: s.update(name=""))
        assert "name" in validate_skill_import(data)["error"]

        data = self._patch_skill(bundle, lambda s: s.update(body="   "))
        assert "empty body" in validate_skill_import(data)["error"]

    def test_rule_manifest_damage_rejected(self, bundle):
        data = self._patch_skill(bundle, lambda s: s.update(rule_manifest="{broken"))
        assert "rule_manifest" in validate_skill_import(data)["error"]

        data = self._patch_skill(
            bundle, lambda s: s.update(rule_manifest='{"not": "a list"}'),
        )
        assert "rule_manifest" in validate_skill_import(data)["error"]

    def test_memory_file_shapes_rejected(self, bundle):
        cases = [
            (b"{broken", "not valid JSON"),
            (b'["a list"]', "JSON object"),
            (json.dumps({"key": 42, "fields": {}}).encode(), "invalid memory key"),
            (json.dumps({"key": "mem:episodic:01A"}).encode(), "no fields"),
        ]
        for raw, expected in cases:
            data = _rebuild_zip(bundle["data"], replace={
                "memories/0000.json": raw,
            }, refresh_checksums=True)
            result = validate_skill_import(data)
            assert result["ok"] is False
            assert expected in result["error"]

    def test_memory_field_bounds(self, bundle):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            mem = json.loads(zf.read("memories/0000.json"))

        too_many = dict(mem, fields={f"f{i}": "v" for i in range(65)})
        long_name = dict(mem, fields={"x" * 65: "v", "content": "c"})
        big_value = dict(mem, fields={"content": "c", "notes": "x" * 100_500})
        big_content = dict(mem, fields={"content": "x" * 50_001})
        for payload, expected in (
            (too_many, "too many fields"),
            (long_name, "invalid field name"),
            (big_value, "too large"),
            (big_content, "content exceeds"),
        ):
            data = _rebuild_zip(bundle["data"], replace={
                "memories/0000.json": json.dumps(payload).encode(),
            }, refresh_checksums=True)
            result = validate_skill_import(data)
            assert result["ok"] is False
            assert expected in result["error"]

    def test_duplicate_memory_keys_rejected(self, bundle):
        with zipfile.ZipFile(io.BytesIO(bundle["data"])) as zf:
            mem0 = zf.read("memories/0000.json")
        data = _rebuild_zip(bundle["data"], replace={
            "memories/0001.json": mem0,
        }, refresh_checksums=True)
        result = validate_skill_import(data)
        assert result["ok"] is False
        assert "twice" in result["error"]

    def test_exporter_reported_missing_sources_warns(self, bundle):
        data = _rebuild_zip(bundle["data"], manifest_patch={
            "missing_sources": ["mem:episodic:01GONE"],
        })
        result = validate_skill_import(data)
        assert result["ok"] is True
        assert any("export time" in w for w in result["warnings"])

    def test_source_manifest_damage_tolerated_in_parser(self):
        from memory.skill_transfer import _parse_source_manifest

        assert _parse_source_manifest("{broken") == []
        assert _parse_source_manifest('"a string"') == []
        assert _parse_source_manifest('["ok", 42]') == ["ok"]
        assert _parse_source_manifest(None) == []
