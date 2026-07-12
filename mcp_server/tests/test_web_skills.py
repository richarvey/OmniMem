"""Tests for the web UI skills section data helpers (and skill telemetry)."""

import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

from web_ui.routes.skills import gather_skill, gather_skills


def _store_skill(fake_store, fake_embedder, key="mem:skill:gen:python-local", **overrides):
    """Store a representative compiled skill straight into the fake store."""
    now = str(time.time())
    fields = {
        "name": "python-local",
        "description": "Distilled python procedure. Load when: python work.",
        "domain": "python",
        "user": "local",
        "state": "active",
        "generated": "true",
        "body": "---\nname: python-local\n---\n\n## Do\n\n- Use uv. [mem:episodic:01A]\n",
        "contract_version": "1",
        "compiled_at": now,
        "created_at": now,
        "updated_at": now,
        "recall_count": "3",
        "source_manifest": json.dumps(["mem:episodic:01A", "mem:episodic:01B"]),
        "rule_manifest": json.dumps([
            {"kind": "do", "text": "Use uv for deps",
             "sources": ["mem:episodic:01A", "mem:episodic:01B"], "reinforcement": 2},
            {"kind": "dont", "text": "hangs on startup", "name": "singleton loader",
             "sources": ["mem:episodic:01B"], "reinforcement": 1, "blessed": True},
        ]),
    }
    fields.update(overrides)
    fake_store.upsert("skill", key, fields, fake_embedder.embed("python skill"))
    return key


class TestGatherSkills:
    def test_empty_store(self, fake_store):
        data = gather_skills(fake_store)
        assert data["total"] == 0
        assert data["skills"] == []
        assert data["proposals"] == []
        assert data["states"] == {"active": 0, "deprioritised": 0, "archived": 0}

    def test_lists_sorted_with_state_counts(self, fake_store, fake_embedder):
        _store_skill(fake_store, fake_embedder)
        _store_skill(fake_store, fake_embedder, key="mem:skill:gen:ansible-local",
                     name="ansible-local", domain="ansible", state="archived")

        data = gather_skills(fake_store)
        assert data["total"] == 2
        assert [s["name"] for s in data["skills"]] == ["ansible-local", "python-local"]
        assert data["states"] == {"active": 1, "deprioritised": 0, "archived": 1}

        python = data["skills"][1]
        assert python["generated"] is True
        assert python["domain"] == "python"
        assert python["recall_count"] == 3
        assert python["state"] == "active"

    def test_pending_proposals_listed(self, fake_store, fake_embedder):
        fake_store.client.hset("meta:skill:proposal:rust-local", mapping={
            "domain": "rust", "user": "local",
            "created_at": str(time.time()), "body": "draft",
        })
        data = gather_skills(fake_store)
        assert len(data["proposals"]) == 1
        assert data["proposals"][0]["domain"] == "rust"


class TestGatherSkill:
    def test_full_detail(self, fake_store, fake_embedder):
        key = _store_skill(fake_store, fake_embedder)
        skill = gather_skill(fake_store, key)

        assert skill["name"] == "python-local"
        assert skill["generated"] is True
        assert skill["body"].startswith("---")
        assert skill["sources"] == ["mem:episodic:01A", "mem:episodic:01B"]
        assert skill["rule_counts"] == {"do": 1, "watch": 0, "dont": 1, "ref": 0}
        assert skill["rules"][0]["reinforcement"] == 2
        assert skill["recall_count"] == 3
        assert skill["last_recalled"] == "Never"

    def test_missing_skill_returns_none(self, fake_store):
        assert gather_skill(fake_store, "mem:skill:gen:nothing-local") is None

    def test_non_skill_key_refused(self, fake_store, fake_embedder):
        # A valid memory key must not render through the skill view.
        _store_skill(fake_store, fake_embedder)
        assert gather_skill(fake_store, "mem:episodic:01A") is None
        assert gather_skill(fake_store, "meta:skill:proposal:python-local") is None

    def test_corrupt_manifests_degrade_gracefully(self, fake_store, fake_embedder):
        key = _store_skill(fake_store, fake_embedder,
                           source_manifest="{not json", rule_manifest="also not")
        skill = gather_skill(fake_store, key)
        assert skill["sources"] == []
        assert skill["rules"] == []
        assert skill["rule_counts"] == {"do": 0, "watch": 0, "dont": 0, "ref": 0}


class TestTelemetryIncludesSkills:
    def test_skill_loads_surface_in_telemetry(self, fake_store, fake_embedder, monkeypatch):
        from web_ui import deps
        from web_ui.routes.telemetry import _build_telemetry_data

        monkeypatch.setattr(deps, "store", fake_store)
        _store_skill(fake_store, fake_embedder,
                     recall_count="5", last_recalled=str(time.time()))

        data = _build_telemetry_data()
        entries = [e for e in data["most_recalled"] if e["namespace"] == "skill"]
        assert len(entries) == 1
        assert entries[0]["recall_count"] == 5
        assert entries[0]["url"] == "/skills/mem:skill:gen:python-local"
        assert "python-local" in entries[0]["content"]
