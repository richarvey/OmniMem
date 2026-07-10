"""Tests for the v6 skill compiler: engine, MCP tools, and briefing surfaces."""

import json
import time

import pytest

import tools as tools_module
from tests.conftest import store_memory


@pytest.fixture(autouse=True)
def inject_deps(fake_store, fake_embedder, lifecycle, pipeline):
    """Inject fake dependencies into the tools module."""
    tools_module._store = fake_store
    tools_module._embedder = fake_embedder
    tools_module._lifecycle = lifecycle
    tools_module._pipeline = pipeline
    yield
    tools_module._store = None
    tools_module._embedder = None
    tools_module._lifecycle = None
    tools_module._pipeline = None


from memory.skills import (
    CONTRACT_VERSION,
    bodies_equivalent,
    generated_skill_key,
    resolve_domain,
    summarise_rule_changes,
)
from memory.store import INDEX_DEFINITIONS, ValkeyStore
from tools.briefing import briefing
from tools.core import remember
from tools.project import set_project_context
from tools.skills import (
    bless,
    compile_skill,
    find_skills,
    get_skill,
    pending_skill_updates,
    suggest_skills_for_briefing,
)

SKILL_ID = generated_skill_key("python", "local")


def _experience_memory(store, embedder, key, *, domain="python",
                       breakthrough=None, gotchas=None, abandoned=None,
                       content="Worked on a python task", project=None):
    """A pool memory: tagged with the domain, carrying lesson fields."""
    return store_memory(
        store, embedder, key, content,
        tags=[domain], project=project,
        effort_score=3, outcome="succeeded",
        breakthrough=breakthrough, gotchas=gotchas,
        abandoned_approaches=abandoned,
    )


def _reinforced_pool(store, embedder, n=2, text="Use uv for python dependency management"):
    keys = []
    for i in range(n):
        keys.append(_experience_memory(
            store, embedder, f"mem:episodic:01POOL{i:03d}",
            breakthrough=text,
        ))
    return keys


def _accept(domain="python", **kwargs):
    """Propose then write — the full accept flow."""
    proposal = compile_skill(domain, mode="propose", **kwargs)
    assert proposal["status"] == "proposal", proposal
    written = compile_skill(domain, mode="write")
    assert written["status"] == "written", written
    return proposal, written


class TestStorageLayer:
    def test_skill_key_prefix_valid(self):
        ValkeyStore()._validate_key("mem:skill:gen:python-local")

    def test_skill_index_defined(self):
        assert "idx:skill" in INDEX_DEFINITIONS
        assert INDEX_DEFINITIONS["idx:skill"]["prefix"] == "mem:skill:"

    def test_remember_still_rejects_skill_namespace(self):
        with pytest.raises(ValueError):
            remember("body text", namespace="skill")


class TestProposeGating:
    def test_no_candidates_for_unknown_domain(self):
        result = compile_skill("python")
        assert result["status"] == "no_candidates"

    def test_did_you_mean_on_substring_domain(self, fake_store, fake_embedder):
        _experience_memory(
            fake_store, fake_embedder, "mem:episodic:01BLOG01",
            domain="technical-blogging", breakthrough="Hook first, context second",
        )
        result = compile_skill("blogging")
        assert result["status"] == "no_candidates"
        assert result["did_you_mean"]["domain"] == "technical-blogging"

    def test_alias_resolves_to_canonical_domain(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        result = compile_skill("py")
        assert result["status"] == "proposal"
        assert result["domain"] == "python"
        assert result["domain_resolved_from"] == "py"
        assert result["skill_id"] == SKILL_ID

    def test_no_lessons_when_pool_has_no_experience(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:01PLAIN1",
                     "Just a note about python", tags=["python"])
        result = compile_skill("python")
        assert result["status"] == "no_lessons"

    def test_single_episode_is_held_back(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder, n=1)
        result = compile_skill("python")
        assert result["status"] == "insufficient_reinforcement"
        assert result["held_back"][0]["reinforcement"] == 1

    def test_pattern_across_episodes_earns_a_rule(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder, n=2)
        result = compile_skill("python")
        assert result["status"] == "proposal"
        assert result["new_skill"] is True
        assert result["rules"] == {"do": 1}
        draft = result["draft"]
        assert draft.startswith("---")
        assert "## Do" in draft
        assert "(reinforced x2)" in draft
        assert "## Operating contract" in draft
        assert f"contract_version: {CONTRACT_VERSION}" in draft
        assert "source_manifest:" in draft
        assert "mem:episodic:01POOL000" in draft

    def test_min_reinforcement_1_compiles_single_lesson(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder, n=1)
        result = compile_skill("python", min_reinforcement=1)
        assert result["status"] == "proposal"

    def test_graveyard_becomes_dont_rules(self, fake_store, fake_embedder):
        dead_end = [{"name": "openssl-sys", "type": "library",
                     "reason": "cross-compilation nightmare on musl"}]
        for i in range(2):
            _experience_memory(
                fake_store, fake_embedder, f"mem:episodic:01RUST{i:02d}",
                domain="rust", abandoned=dead_end, project="ferris",
            )
        result = compile_skill("rust")
        assert result["status"] == "proposal"
        assert result["rules"] == {"dont": 1}
        draft = result["draft"]
        assert "## Don't (and why)" in draft
        assert "Avoid openssl-sys (library)" in draft
        assert "cross-compilation nightmare" in draft
        assert "Tried on ferris" in draft
        assert "[mem:episodic:01RUST" in draft

    def test_include_graveyard_false_excludes_dead_ends(self, fake_store, fake_embedder):
        dead_end = [{"name": "openssl-sys", "type": "library", "reason": "musl"}]
        for i in range(2):
            _experience_memory(
                fake_store, fake_embedder, f"mem:episodic:01RUST{i:02d}",
                domain="rust", abandoned=dead_end,
            )
        result = compile_skill("rust", include_graveyard=False)
        assert result["status"] == "no_lessons"

    def test_gotchas_become_watch_rules(self, fake_store, fake_embedder):
        for i in range(2):
            _experience_memory(
                fake_store, fake_embedder, f"mem:episodic:01GOT{i:02d}",
                gotchas="mtime polling needed for docker bind mounts",
            )
        result = compile_skill("python")
        assert result["status"] == "proposal"
        assert "## Watch out" in result["draft"]


class TestBless:
    def test_bless_promotes_single_lesson(self, fake_store, fake_embedder):
        [key] = _reinforced_pool(fake_store, fake_embedder, n=1)
        assert compile_skill("python")["status"] == "insufficient_reinforcement"

        blessed = bless(key)
        assert blessed["status"] == "blessed"
        assert "python" in blessed["domains"]

        result = compile_skill("python")
        assert result["status"] == "proposal"
        assert "(blessed)" in result["draft"]

    def test_bless_is_idempotent(self, fake_store, fake_embedder):
        [key] = _reinforced_pool(fake_store, fake_embedder, n=1)
        bless(key)
        assert bless(key)["status"] == "already_blessed"

    def test_bless_rejects_non_episodic_keys(self):
        with pytest.raises(ValueError):
            bless("mem:knowledge:whatever")

    def test_bless_missing_memory(self):
        assert bless("mem:episodic:01NOPE")["status"] == "not_found"


class TestWriteGate:
    def test_write_without_proposal_refuses(self):
        result = compile_skill("python", mode="write")
        assert result["status"] == "no_proposal"

    def test_propose_then_write_commits_exact_draft(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        proposal, written = _accept()
        assert written["skill_id"] == SKILL_ID
        assert written["new_skill"] is True

        stored = fake_store.get(SKILL_ID)
        assert stored["generated"] == "true"
        assert stored["body"] == proposal["draft"]
        assert stored["domain"] == "python"
        assert json.loads(stored["source_manifest"])

        # The stash is consumed: a second write has nothing to commit.
        assert compile_skill("python", mode="write")["status"] == "no_proposal"

    def test_write_commits_what_was_reviewed_not_a_recompile(
        self, fake_store, fake_embedder
    ):
        _reinforced_pool(fake_store, fake_embedder)
        proposal = compile_skill("python")
        # Sources move underneath after the human reviewed the diff.
        _experience_memory(fake_store, fake_embedder, "mem:episodic:01LATE01",
                           breakthrough="Something else entirely new")
        written = compile_skill("python", mode="write")
        assert written["status"] == "written"
        assert fake_store.get(SKILL_ID)["body"] == proposal["draft"]

    def test_stale_proposal_refused_after_external_change(
        self, fake_store, fake_embedder
    ):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()

        _reinforced_pool(fake_store, fake_embedder, n=3,
                         text="Use uv for python dependency management")
        assert compile_skill("python")["status"] == "proposal"
        # The stored skill changes between propose and write.
        fake_store.set_fields(SKILL_ID, {"body": "tampered elsewhere"})
        result = compile_skill("python", mode="write")
        assert result["status"] == "stale_proposal"

    def test_refuses_object_not_flagged_generated(self, fake_store, fake_embedder):
        vector = fake_embedder.embed("authored skill")
        fake_store.upsert("skill", SKILL_ID, {
            "name": "python-local", "domain": "python", "state": "active",
            "body": "# hand-authored", "created_at": str(time.time()),
            "updated_at": str(time.time()),
        }, vector)
        _reinforced_pool(fake_store, fake_embedder)
        assert compile_skill("python")["status"] == "refused"
        assert compile_skill("python", mode="write")["status"] == "refused"

    def test_unchanged_when_sources_did_not_move(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        result = compile_skill("python")
        assert result["status"] == "unchanged"

    def test_recompile_diff_and_risk_summary(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()

        _experience_memory(fake_store, fake_embedder, "mem:episodic:01POOL999",
                           breakthrough="Use uv for python dependency management")
        result = compile_skill("python")
        assert result["status"] == "proposal"
        assert "diff" in result and "draft" not in result
        assert "(reinforced x3)" in result["diff"]
        assert any(
            c["change"] == "reinforced" and c["risk"] == "low"
            for c in result["changes"]
        )


class TestDescriptionOwnership:
    def test_description_drafted_then_pinned(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        proposal, _ = _accept()
        assert "Load when:" in proposal["description"]
        assert proposal["description_pinned"] is False

        # Human takes ownership of the trigger.
        _experience_memory(fake_store, fake_embedder, "mem:episodic:01POOL998",
                           breakthrough="Use uv for python dependency management")
        custom = "Custom python trigger. Load when: python work."
        assert compile_skill("python", description=custom)["status"] == "proposal"
        compile_skill("python", mode="write")
        assert fake_store.get(SKILL_ID)["description"] == custom

        # Recompiles keep the pinned description.
        _experience_memory(fake_store, fake_embedder, "mem:episodic:01POOL997",
                           breakthrough="Use uv for python dependency management")
        result = compile_skill("python")
        assert result["description"] == custom
        assert result["description_pinned"] is True


class TestFindAndGet:
    def test_find_by_exact_domain(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        result = find_skills("python")
        assert result["skills"][0]["skill_id"] == SKILL_ID
        assert result["skills"][0]["match"] == "domain"

    def test_find_semantic_query(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        result = find_skills("how do I write python code")
        assert any(s["skill_id"] == SKILL_ID for s in result["skills"])

    def test_authored_outranks_generated_on_same_domain(
        self, fake_store, fake_embedder
    ):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        authored_key = "mem:skill:auth:python-hand"
        fake_store.upsert("skill", authored_key, {
            "name": "python-hand", "domain": "python", "state": "active",
            "description": "Hand-authored python skill",
            "body": "# authored", "created_at": str(time.time()),
            "updated_at": str(time.time()),
        }, fake_embedder.embed("Hand-authored python skill"))

        skills = find_skills("python")["skills"]
        domain_hits = [s for s in skills if s["match"] == "domain"]
        assert [s["skill_id"] for s in domain_hits[:2]] == [authored_key, SKILL_ID]

    def test_get_skill_by_key_name_and_domain(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        proposal, _ = _accept()
        for handle in (SKILL_ID, "python-local", "python", "gen:python-local"):
            result = get_skill(handle)
            assert result["status"] == "found", handle
            assert result["skill_id"] == SKILL_ID
            assert result["body"] == proposal["draft"]
        assert fake_store.get(SKILL_ID)["recall_count"] == "4"

    def test_get_skill_not_found_lists_available(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        result = get_skill("rust")
        assert result["status"] == "not_found"
        assert result["available"][0]["skill_id"] == SKILL_ID


class TestExport:
    def test_export_mirrors_to_disk(self, fake_store, fake_embedder, tmp_path, monkeypatch):
        monkeypatch.setenv("SKILL_EXPORT_DIR", str(tmp_path))
        _reinforced_pool(fake_store, fake_embedder)
        proposal = compile_skill("python")
        written = compile_skill("python", mode="write",
                                export_path="python-local/SKILL.md")
        exported = tmp_path / "python-local" / "SKILL.md"
        assert written["exported_to"] == str(exported)
        assert exported.read_text(encoding="utf-8") == proposal["draft"]

    @pytest.mark.parametrize("bad_path", [
        "../escape.md", "/etc/skill.md", "no-extension", "python/..hidden.md",
    ])
    def test_export_rejects_unsafe_paths(
        self, fake_store, fake_embedder, tmp_path, monkeypatch, bad_path
    ):
        monkeypatch.setenv("SKILL_EXPORT_DIR", str(tmp_path))
        _reinforced_pool(fake_store, fake_embedder)
        compile_skill("python")
        written = compile_skill("python", mode="write", export_path=bad_path)
        assert written["status"] == "written"  # Valkey write still lands
        assert "export_error" in written
        assert "exported_to" not in written


class TestChangeSummary:
    def test_added_removed_rewritten_reinforced(self):
        old = [
            {"kind": "do", "text": "Old wording", "sources": ["mem:episodic:A"],
             "reinforcement": 2},
            {"kind": "do", "text": "Gone entirely", "sources": ["mem:episodic:B"],
             "reinforcement": 2},
            {"kind": "dont", "name": "pillow", "text": "too slow",
             "sources": ["mem:episodic:C"], "reinforcement": 2},
        ]
        new = [
            {"kind": "do", "text": "New wording", "sources": ["mem:episodic:A"],
             "reinforcement": 2},
            {"kind": "do", "text": "Brand new rule", "sources": ["mem:episodic:D"],
             "reinforcement": 2},
            {"kind": "dont", "name": "pillow", "text": "too slow",
             "sources": ["mem:episodic:C", "mem:episodic:E"], "reinforcement": 3},
        ]
        changes = summarise_rule_changes(old, new)
        by_change = {c["change"]: c for c in changes}
        assert by_change["rewritten"]["risk"] == "high"
        assert by_change["rewritten"]["was"] == "Old wording"
        assert by_change["removed"]["risk"] == "high"
        assert by_change["added"]["risk"] == "low"
        assert by_change["reinforced"]["risk"] == "low"
        # High-risk changes lead — prominence scales with risk.
        assert changes[0]["risk"] == "high"

    def test_identical_manifests_report_nothing(self):
        rules = [{"kind": "do", "text": "Same", "sources": ["mem:episodic:A"],
                  "reinforcement": 2}]
        assert summarise_rule_changes(rules, rules) == []


class TestBriefingIntegration:
    def test_greenfield_moves_skills_to_top(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        result = briefing(project="brand-new-idea")
        assert list(result.keys())[0] == "skill_suggestions"
        block = result["skill_suggestions"]
        assert "Greenfield" in block["note"]
        assert block["skills"][0]["skill_id"] == SKILL_ID
        assert "get_skill" in block["skills"][0]["load_with"]

    def test_ongoing_project_keeps_context_first(
        self, fake_store, fake_embedder, monkeypatch
    ):
        monkeypatch.setenv("SKILL_SUGGEST_MIN_SIMILARITY", "0.0")
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        set_project_context(
            "webapp", description="A python web application",
            stack="python, starlette", goals="ship v1",
            current_state="scaffolding",
        )
        result = briefing(project="webapp")
        assert list(result.keys())[0] == "project_context"
        assert result["skill_suggestions"]["skills"][0]["skill_id"] == SKILL_ID
        assert "note" not in result["skill_suggestions"]

    def test_no_suggestions_without_project(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        assert "skill_suggestions" not in briefing()

    def test_new_source_gist_is_low_risk(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        _experience_memory(fake_store, fake_embedder, "mem:episodic:01ZNEW01",
                           breakthrough="Prefer pathlib over os.path")
        updates = pending_skill_updates(fake_store)
        assert len(updates) == 1
        assert updates[0]["skill_id"] == SKILL_ID
        assert updates[0]["changes"][0]["change"] == "new_source"
        assert updates[0]["changes"][0]["risk"] == "low"
        assert updates[0]["batch_accept_eligible"] is True
        assert "compile_skill" in updates[0]["full_diff"]

    def test_updated_source_gist_is_high_risk_and_names_rule(
        self, fake_store, fake_embedder
    ):
        keys = _reinforced_pool(fake_store, fake_embedder)
        _accept()
        fake_store.set_fields(keys[0], {"updated_at": str(time.time() + 10)})
        updates = pending_skill_updates(fake_store)
        change = updates[0]["changes"][0]
        assert change["change"] == "source_updated"
        assert change["risk"] == "high"
        assert change["feeds_rules"]
        assert updates[0]["batch_accept_eligible"] is False

    def test_removed_source_gist_is_high_risk(self, fake_store, fake_embedder):
        keys = _reinforced_pool(fake_store, fake_embedder)
        _accept()
        fake_store.delete(keys[0])
        updates = pending_skill_updates(fake_store)
        assert updates[0]["changes"][0]["change"] == "source_removed"
        assert updates[0]["changes"][0]["risk"] == "high"

    def test_quiet_when_nothing_changed(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        assert pending_skill_updates(fake_store) == []
        result = briefing(project="brand-new-idea")
        assert "skill_updates" not in result

    def test_briefing_surfaces_updates(self, fake_store, fake_embedder):
        _reinforced_pool(fake_store, fake_embedder)
        _accept()
        _experience_memory(fake_store, fake_embedder, "mem:episodic:01ZNEW02",
                           breakthrough="Prefer pathlib over os.path")
        result = briefing(project="brand-new-idea")
        assert result["skill_updates"][0]["skill_id"] == SKILL_ID


class TestEngineHelpers:
    def test_resolve_domain_normalises_and_aliases(self):
        assert resolve_domain("  Technical Blogging ") == ("technical-blogging", False)
        assert resolve_domain("PY") == ("python", True)

    def test_bodies_equivalent_ignores_compiled_at_only(self):
        a = "---\nname: x\ncompiled_at: 2026-07-10T09:00:00Z\n---\nbody"
        b = "---\nname: x\ncompiled_at: 2026-07-11T10:00:00Z\n---\nbody"
        c = "---\nname: x\ncompiled_at: 2026-07-11T10:00:00Z\n---\nchanged"
        assert bodies_equivalent(a, b)
        assert not bodies_equivalent(a, c)

    def test_suggestions_empty_without_skills(self, fake_store, fake_embedder):
        assert suggest_skills_for_briefing(fake_store, fake_embedder, None) == []
