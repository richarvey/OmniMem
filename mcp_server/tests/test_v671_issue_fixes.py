"""Issues #28-#34: relevance floors, phantom guard, drift surfacing, and the
staleness exemption for memories that compiled into a skill.

The floors are the reason FakeEmbedder had to become deterministic (crc32
rather than hash(), see conftest): every assertion here compares a similarity
against a threshold, so a per-process seed would make the whole file flaky.
"""

import json
import time

import numpy as np
import pytest

from memory.lifecycle import MemoryLifecycle
from memory.recall import RecallPipeline, recall_min_score
from memory.skill_compiler import (
    _HELD_BACK_MAX_CHARS,
    _held_back_preview,
    _held_back_text,
    _pool_concentration,
    _insufficient_note,
)
from memory.skills import Rule
from memory.store import ValkeyStore
from tests.conftest import store_memory


@pytest.fixture(autouse=True)
def _wire_tools(fake_store, fake_embedder, monkeypatch):
    import tools as tools_pkg

    lifecycle = MemoryLifecycle(fake_store)
    pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
    monkeypatch.setattr(tools_pkg, "_store", fake_store)
    monkeypatch.setattr(tools_pkg, "_embedder", fake_embedder)
    monkeypatch.setattr(tools_pkg, "_lifecycle", lifecycle)
    monkeypatch.setattr(tools_pkg, "_pipeline", pipeline)
    yield


# ---------------------------------------------------------------------------
# #30 — recall relevance floor
# ---------------------------------------------------------------------------


class TestRecallMinScore:
    def test_default_is_the_measured_cliff(self, monkeypatch):
        monkeypatch.delenv("RECALL_MIN_SCORE", raising=False)
        assert recall_min_score() == 0.4

    def test_env_override(self, monkeypatch):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.15")
        assert recall_min_score() == 0.15

    def test_zero_disables(self, monkeypatch):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0")
        assert recall_min_score() == 0.0

    def test_garbage_falls_back_to_default(self, monkeypatch):
        monkeypatch.setenv("RECALL_MIN_SCORE", "not-a-number")
        assert recall_min_score() == 0.4

    def test_negative_is_clamped_not_inverted(self, monkeypatch):
        monkeypatch.setenv("RECALL_MIN_SCORE", "-1")
        assert recall_min_score() == 0.0

    def test_irrelevant_results_do_not_pad_top_k(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        """The issue's shape: two on-topic memories, three unrelated ones,
        top_k=5. The answer is two results, not five."""
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.4")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)

        store_memory(
            fake_store, fake_embedder, "mem:episodic:hit1",
            "valkey search index drift and dropindex behaviour",
        )
        store_memory(
            fake_store, fake_embedder, "mem:episodic:hit2",
            "valkey search index drift notes",
        )
        for i, noise in enumerate((
            "spotlight search interface notes from the keynote",
            "keyboard operability rules for accessible widgets",
            "projected batch fetch helper for list views",
        )):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:noise{i}", noise,
            )

        results = pipeline.recall(
            "valkey search index drift and dropindex behaviour",
            top_k=5, namespaces=["episodic"],
        )
        keys = [r.key for r in results]
        assert "mem:episodic:hit1" in keys
        assert len(results) < 5, keys
        assert all(r.score >= 0.4 for r in results), [
            (r.key, r.score) for r in results
        ]

    def test_empty_result_is_a_valid_answer(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.4")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        store_memory(
            fake_store, fake_embedder, "mem:episodic:x1",
            "entirely unrelated notes about garden furniture",
        )
        results = pipeline.recall(
            "quantum chromodynamics lattice simulation",
            top_k=5, namespaces=["episodic"],
        )
        assert results == []

    def test_floor_gates_raw_similarity_not_the_adjusted_score(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        """An enriched fact carries surface_score 0.5 and an abandoned
        experience 0.1, deliberately, so their adjusted scores sit under the
        floor while the memories are still relevant. Gating on the adjusted
        score would hide exactly those (issue #20's facts, the graveyard)."""
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.4")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)

        # A partial match, so raw lands between the floor and 2x the floor:
        # relevant enough to keep, and halved by surface_score to below it.
        store_memory(
            fake_store, fake_embedder, "mem:knowledge:fact1",
            "traefik router rule for the staging host",
            namespace="knowledge", surface_score="0.5",
        )
        results = pipeline.recall(
            "traefik router rule staging", top_k=5, namespaces=["knowledge"],
        )
        keys = [r.key for r in results]
        assert "mem:knowledge:fact1" in keys, keys
        hit = next(r for r in results if r.key == "mem:knowledge:fact1")
        assert hit.score >= 0.4
        assert hit.adjusted_score < 0.4, (
            "precondition: the adjusted score must be under the floor for "
            "this test to prove anything"
        )

    def test_abandoned_warning_is_exempt(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        """The warning fires on a keyword scan before the query is embedded,
        so it never had a similarity to be judged on."""
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.99")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        store_memory(
            fake_store, fake_embedder, "mem:episodic:aband1",
            "tried running the embedder on alpine",
            abandoned_approaches=[
                {"name": "onnxruntime-alpine", "type": "library",
                 "reason": "SIGILL on musl libc"},
            ],
        )
        results = pipeline.recall(
            "should we use onnxruntime-alpine", top_k=5,
            namespaces=["episodic"],
        )
        types_ = [r.result_type for r in results]
        assert "abandoned_warning" in types_, types_


# ---------------------------------------------------------------------------
# #28 — phantom index entries
# ---------------------------------------------------------------------------


class TestPhantomGuard:
    def test_phantom_hit_is_not_returned(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        """An index entry whose hash is gone comes back as a doc id with no
        fields. remember() rejects empty content, so this can only be a
        phantom — it must not consume a result slot."""
        monkeypatch.setenv("RECALL_MIN_SCORE", "0")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        real = store_memory(
            fake_store, fake_embedder, "mem:episodic:real1",
            "docker compose healthcheck for valkey",
        )

        original_search = fake_store.search

        def _with_phantom(namespace, vector, top_k=10, filter_expr=None):
            docs = original_search(namespace, vector, top_k=top_k,
                                   filter_expr=filter_expr)
            return [{"key": "mem:episodic:ghost", "similarity_score": "0.01"}] + docs

        monkeypatch.setattr(fake_store, "search", _with_phantom)

        results = pipeline.recall(
            "docker compose healthcheck for valkey", top_k=5,
            namespaces=["episodic"],
        )
        keys = [r.key for r in results]
        assert "mem:episodic:ghost" not in keys, keys
        assert real in keys

    def test_index_report_flags_drift_both_directions(self):
        class _Idx:
            def __init__(self, docs):
                self._docs = docs

            def info(self):
                if self._docs is None:
                    raise RuntimeError("no such index")
                return {"num_docs": self._docs}

        class _Client:
            def ft(self, name):
                return _Idx({
                    "idx:episodic": 7,     # 2 phantoms
                    "idx:project": 2,      # matches
                    "idx:knowledge": 1,    # index behind by 1
                    "idx:preference": 0,
                    "idx:skill": None,     # unavailable
                }[name])

        class _Store:
            index_report = ValkeyStore.index_report
            client = _Client()

            def count_all_records(self):
                return {"episodic": 5, "project": 2, "knowledge": 2,
                        "preference": 0, "skill": 1}

        report = _Store().index_report()
        assert report["drift"] == {"episodic": 2, "knowledge": -1}
        assert report["indexes"]["idx:skill"] == "unavailable"
        assert report["records"]["skill"] == 1

    def test_index_report_survives_a_failing_scan(self):
        class _Client:
            def ft(self, name):
                raise RuntimeError("index gone")

        class _Store:
            index_report = ValkeyStore.index_report
            client = _Client()

            def count_all_records(self):
                raise RuntimeError("scan failed")

        report = _Store().index_report()
        assert report["drift"] == {}
        assert set(report["records"].values()) == {"unavailable"}


# ---------------------------------------------------------------------------
# #32 — held_back rule text
# ---------------------------------------------------------------------------


class TestHeldBackText:
    def _rule(self, text, **kw):
        return Rule(
            kind=kw.get("kind", "do"),
            text=text,
            name=kw.get("name"),
            sources=kw.get("sources", ["mem:episodic:s1"]),
            reinforcement=kw.get("reinforcement", 1),
        )

    def test_text_is_not_cut_at_80_chars(self):
        text = (
            "Deterministic template compilation instead of LLM distillation "
            "was the key design decision, because the same source memories "
            "must render a byte-identical body every time."
        )
        assert len(text) > 80
        preview = _held_back_preview([self._rule(text)])
        assert preview[0]["rule"] == text
        assert "truncated" not in preview[0]

    def test_pathological_length_cuts_on_a_word_boundary(self):
        text = "word " * 300
        cut, truncated = _held_back_text(text)
        assert truncated is True
        assert cut.endswith("…")
        assert len(cut) <= _HELD_BACK_MAX_CHARS + 1
        # Never mid-word: everything before the ellipsis is whole words.
        assert all(w == "word" for w in cut[:-1].split())

    def test_truncation_is_flagged_in_the_preview(self):
        preview = _held_back_preview([self._rule("x" * 900)])
        assert preview[0]["truncated"] is True

    def test_dont_rules_still_render_as_avoid(self):
        preview = _held_back_preview(
            [self._rule("...", kind="dont", name="alpine + pytorch")]
        )
        assert preview[0]["rule"] == "Avoid alpine + pytorch"


# ---------------------------------------------------------------------------
# #33 — diagnostics when a broad domain compiles nothing
# ---------------------------------------------------------------------------


class TestInsufficientReinforcementDiagnostics:
    def test_single_project_pool_is_reported(self):
        pool = [{"project": "omnimem"} for _ in range(29)]
        assert _pool_concentration(pool) == {
            "projects": 1,
            "top_project": "omnimem",
            "top_project_share": "29/29",
        }

    def test_mixed_pool_reports_the_busiest(self):
        pool = [{"project": "a"}] * 3 + [{"project": "b"}] * 2
        report = _pool_concentration(pool)
        assert report["projects"] == 2
        assert report["top_project"] == "a"
        assert report["top_project_share"] == "3/5"

    def test_no_project_data_reports_nothing(self):
        assert _pool_concentration([{"content": "x"}]) is None
        assert _pool_concentration([]) is None

    def test_note_does_not_recommend_lowering_the_gate(self):
        """Lowering min_reinforcement admits the noise instead of finding
        signal, which is the wrong lever (issue #33)."""
        pool = [{"project": "omnimem"}] * 4
        held = [
            Rule(kind="do", text="a thing happened", name=None,
                 sources=["mem:episodic:1"], reinforcement=1),
        ]
        note = _insufficient_note("python", pool, held)
        assert "min_reinforcement" not in note
        assert "one project (omnimem)" in note
        assert "narrower domain" in note

    def test_note_mentions_a_cross_project_pool_without_the_concentration_line(self):
        pool = [{"project": "a"}, {"project": "b"}]
        held = [
            Rule(kind="do", text="x", name=None, sources=["mem:episodic:1"],
                 reinforcement=1),
        ]
        note = _insufficient_note("valkey", pool, held)
        assert "one project" not in note
        assert "bless()" in note


# ---------------------------------------------------------------------------
# #34 — staleness must not penalise memories that compiled into a skill
# ---------------------------------------------------------------------------


class TestStaleExemption:
    def _stale_memory(self, store, embedder, key, content):
        stored = store_memory(store, embedder, key, content)
        old = str(time.time() - (400 * 86400))
        store.set_field(stored, "updated_at", old)
        return stored

    def _skill(self, store, embedder, sources):
        store.upsert(
            "skill", "mem:skill:gen:preferences-local",
            {
                "name": "preferences-local",
                "description": "durable preferences",
                "domain": "preferences",
                "state": "active",
                "generated": "true",
                "body": "---\nname: preferences-local\n---\n",
                "source_manifest": json.dumps(sources),
                "created_at": str(time.time()),
                "updated_at": str(time.time()),
            },
            embedder.embed("preferences-local durable preferences"),
        )

    def test_compiled_sources_are_not_flagged_stale(
        self, fake_store, fake_embedder,
    ):
        from tools.briefing import _scan_episodic_once

        compiled = self._stale_memory(
            fake_store, fake_embedder, "mem:episodic:pref1",
            "prefers forgejo over github as a git forge",
        )
        loose = self._stale_memory(
            fake_store, fake_embedder, "mem:episodic:loose1",
            "an old note nothing ever compiled",
        )
        self._skill(fake_store, fake_embedder, [compiled])

        stale_keys = [
            s["key"] for s in
            _scan_episodic_once(fake_store, stale_days=30)["stale"]
        ]
        assert compiled not in stale_keys, (
            "a memory that earned a place in a skill is the opposite of stale"
        )
        assert loose in stale_keys

    def test_exemption_disappears_with_the_skill(
        self, fake_store, fake_embedder,
    ):
        """Read from the live manifest, not stamped on the memory, so
        deleting the skill restores the memory to the stale list rather than
        leaving a marker pointing at something that no longer exists."""
        from tools.briefing import _scan_episodic_once

        compiled = self._stale_memory(
            fake_store, fake_embedder, "mem:episodic:pref2",
            "prefers proton mail as the email provider",
        )
        self._skill(fake_store, fake_embedder, [compiled])
        assert compiled not in [
            s["key"] for s in
            _scan_episodic_once(fake_store, stale_days=30)["stale"]
        ]

        fake_store.delete("mem:skill:gen:preferences-local")
        assert compiled in [
            s["key"] for s in
            _scan_episodic_once(fake_store, stale_days=30)["stale"]
        ]

    def test_a_broken_manifest_does_not_break_the_briefing(
        self, fake_store, fake_embedder,
    ):
        from tools.briefing import _scan_episodic_once

        loose = self._stale_memory(
            fake_store, fake_embedder, "mem:episodic:loose2", "an old note",
        )
        self._skill(fake_store, fake_embedder, [])
        fake_store.set_field(
            "mem:skill:gen:preferences-local", "source_manifest", "{not json",
        )
        stale_keys = [
            s["key"] for s in
            _scan_episodic_once(fake_store, stale_days=30)["stale"]
        ]
        assert loose in stale_keys


# ---------------------------------------------------------------------------
# #31 — find_skills relevance floor and confidence
# ---------------------------------------------------------------------------


class TestFindSkillsFloor:
    def _skill(self, store, embedder, domain, description):
        key = f"mem:skill:gen:{domain}-local"
        store.upsert(
            "skill", key,
            {
                "name": f"{domain}-local",
                "description": description,
                "domain": domain,
                "state": "active",
                "generated": "true",
                "body": f"---\nname: {domain}-local\n---\n",
                "source_manifest": "[]",
                "created_at": str(time.time()),
                "updated_at": str(time.time()),
            },
            embedder.embed(f"{domain}-local {description} {domain}"),
        )
        return key

    def test_default_floor(self, monkeypatch):
        from tools.skills import skill_min_score

        monkeypatch.delenv("SKILL_MIN_SCORE", raising=False)
        assert skill_min_score() == 0.25

    def test_floor_env_override_and_fallbacks(self, monkeypatch):
        from tools.skills import skill_min_score

        monkeypatch.setenv("SKILL_MIN_SCORE", "0.5")
        assert skill_min_score() == 0.5
        monkeypatch.setenv("SKILL_MIN_SCORE", "")
        assert skill_min_score() == 0.25
        monkeypatch.setenv("SKILL_MIN_SCORE", "nonsense")
        assert skill_min_score() == 0.25
        monkeypatch.setenv("SKILL_MIN_SCORE", "-3")
        assert skill_min_score() == 0.0

    def test_unrelated_query_returns_nothing(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        """The issue's case: no python skill exists, so the correct answer to
        'python' is an empty list, not the least-bad of three."""
        from tools.skills import find_skills

        monkeypatch.delenv("SKILL_MIN_SCORE", raising=False)
        self._skill(fake_store, fake_embedder, "preferences",
                    "durable tooling and house style preferences")
        self._skill(fake_store, fake_embedder, "opentofu",
                    "writing and reviewing opentofu infrastructure code")

        result = find_skills("python")
        assert result["skills"] == []
        assert "floor" in result["note"] or "No skill matched" in result["note"]

    def test_matching_query_still_returns_its_skill(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools.skills import find_skills

        monkeypatch.delenv("SKILL_MIN_SCORE", raising=False)
        key = self._skill(fake_store, fake_embedder, "opentofu",
                          "writing and reviewing opentofu infrastructure code")
        result = find_skills("opentofu infrastructure code")
        assert [s["skill_id"] for s in result["skills"]] == [key]

    def test_exact_domain_match_is_never_gated(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        """A domain hit is an identity match, not a similarity — a floor of 1
        must not hide it."""
        from tools.skills import find_skills

        monkeypatch.setenv("SKILL_MIN_SCORE", "1.0")
        key = self._skill(fake_store, fake_embedder, "opentofu",
                          "writing and reviewing opentofu infrastructure code")
        result = find_skills("opentofu")
        assert [s["skill_id"] for s in result["skills"]] == [key]
        entry = result["skills"][0]
        assert entry["match"] == "domain"
        assert entry["confidence"] == "high"

    def test_weak_semantic_match_is_marked_low_confidence(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools.skills import find_skills

        monkeypatch.setenv("SKILL_MIN_SCORE", "0.05")
        self._skill(fake_store, fake_embedder, "opentofu",
                    "writing and reviewing opentofu infrastructure code")
        result = find_skills(
            "opentofu something entirely different about other unrelated work"
        )
        assert result["skills"], "precondition: something must clear 0.05"
        entry = result["skills"][0]
        assert entry["score"] < 0.45
        assert entry["confidence"] == "low"
        assert "weak" in result["note"]


# ---------------------------------------------------------------------------
# #30 — the floor is for agents, not for the human-facing search page
# ---------------------------------------------------------------------------


class TestWebSearchKeepsWeakMatches:
    def test_weak_match_is_shown_and_labelled(
        self, web_client, fake_store, fake_embedder, monkeypatch,
    ):
        """A person can dismiss a bad match at a glance, and an empty page for
        a memory they know is stored is the worse answer. So the web search
        opts out of the floor and marks what falls below it instead."""
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.4")
        store_memory(
            fake_store, fake_embedder, "mem:episodic:weak1",
            "notes about traefik routers and middlewares", project="omnimem",
        )
        resp = web_client.get("/search/results", params={
            "query": "traefik something quite different and otherwise unrelated",
            "top_k": "5",
        })
        assert resp.status_code == 200
        assert "mem:episodic:weak1" in resp.text
        assert "weak match" in resp.text

    def test_strong_match_is_not_labelled_weak(
        self, web_client, fake_store, fake_embedder, monkeypatch,
    ):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.4")
        store_memory(
            fake_store, fake_embedder, "mem:episodic:strong1",
            "valkey vector search tuning", project="omnimem",
        )
        resp = web_client.get("/search/results", params={
            "query": "valkey vector search tuning", "top_k": "5",
        })
        assert "mem:episodic:strong1" in resp.text
        assert "weak match" not in resp.text


class TestMinScoreOverride:
    def test_per_call_override_beats_the_env_default(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.9")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        store_memory(
            fake_store, fake_embedder, "mem:episodic:o1",
            "postgres connection pooling notes for the B service",
        )
        query = "postgres connection pooling tips"
        assert pipeline.recall(query, namespaces=["episodic"]) == []
        assert pipeline.recall(
            query, namespaces=["episodic"], min_score=0.0,
        ), "min_score=0 must switch the floor off for this call"

    def test_override_can_also_tighten(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        store_memory(
            fake_store, fake_embedder, "mem:episodic:o2",
            "postgres connection pooling notes for the B service",
        )
        query = "postgres connection pooling tips"
        assert pipeline.recall(query, namespaces=["episodic"])
        assert pipeline.recall(
            query, namespaces=["episodic"], min_score=0.99,
        ) == []
