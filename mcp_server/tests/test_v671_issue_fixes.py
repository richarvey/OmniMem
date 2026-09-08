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
from memory.recall import (
    RecallPipeline,
    is_phantom,
    recall_min_score,
    recall_weak_score,
)
from memory.skill_compiler import (
    _HELD_BACK_MAX_CHARS,
    _held_back_preview,
    _held_back_text,
    _pool_concentration,
    _insufficient_note,
)
from memory.skills import Rule
from memory.store import ValkeyStore, drift_note
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
    def test_defaults(self, monkeypatch):
        monkeypatch.delenv("RECALL_MIN_SCORE", raising=False)
        monkeypatch.delenv("RECALL_WEAK_SCORE", raising=False)
        assert recall_min_score() == 0.15
        assert recall_weak_score() == 0.35

    def test_env_override(self, monkeypatch):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.15")
        assert recall_min_score() == 0.15

    def test_zero_disables(self, monkeypatch):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0")
        assert recall_min_score() == 0.0

    def test_garbage_falls_back_to_default(self, monkeypatch):
        monkeypatch.setenv("RECALL_MIN_SCORE", "not-a-number")
        assert recall_min_score() == 0.15
        monkeypatch.setenv("RECALL_MIN_SCORE", "")
        assert recall_min_score() == 0.15

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
        # Above 1.0 deliberately: the warning is constructed with score 1.0,
        # so any floor at or below that would pass with the exemption removed.
        monkeypatch.setenv("RECALL_MIN_SCORE", "1.5")
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
        monkeypatch.setenv("RECALL_WEAK_SCORE", "0.4")
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
        monkeypatch.setenv("RECALL_WEAK_SCORE", "0.4")
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


# ---------------------------------------------------------------------------
# Review findings on the first cut of these fixes
# ---------------------------------------------------------------------------


class TestPromotedSourceSurvivesTheFloor:
    def test_source_inherits_the_facts_raw_score(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        """A short extracted fact matches a query far better than the long
        verbatim memory it came from. Step 10b drops the fact and promotes the
        source in its place; if only adjusted_score travels, the floor then
        throws the promoted source away on a similarity it no longer shows and
        recall returns nothing despite a 1.0 match."""
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.4")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)

        src = store_memory(
            fake_store, fake_embedder, "mem:episodic:src1",
            "we spent the afternoon going through the reverse proxy "
            "configuration in detail and eventually settled on the traefik "
            "staging router rule",
        )
        fact = store_memory(
            fake_store, fake_embedder, "mem:knowledge:fact1",
            "traefik staging router rule",
            namespace="knowledge", surface_score="0.5",
        )
        fake_store.set_field(fact, "enriched_from", src)

        results = pipeline.recall(
            "traefik staging router rule",
            namespaces=["episodic", "knowledge"], top_k=5,
        )
        assert [r.key for r in results] == [src], results
        assert results[0].score == pytest.approx(1.0), (
            "the source stands in for the fact, so it must carry the score "
            "that earned the result its place"
        )

    def test_unpromoted_source_keeps_its_own_score(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        """Promotion is conditional on the fact ranking higher. When the
        source already outranks its fact, nothing is copied across."""
        monkeypatch.setenv("RECALL_MIN_SCORE", "0")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        src = store_memory(
            fake_store, fake_embedder, "mem:episodic:src2",
            "postgres connection pooling tips",
        )
        fact = store_memory(
            fake_store, fake_embedder, "mem:knowledge:fact2",
            "an unrelated note about garden furniture",
            namespace="knowledge", surface_score="0.5",
        )
        fake_store.set_field(fact, "enriched_from", src)
        results = pipeline.recall(
            "postgres connection pooling tips",
            namespaces=["episodic", "knowledge"], top_k=5,
        )
        hit = next(r for r in results if r.key == src)
        assert hit.score == pytest.approx(1.0)


class TestPhantomDetection:
    def test_only_key_and_score_is_a_phantom(self):
        assert is_phantom({"key": "mem:episodic:x", "similarity_score": "0.4"})
        assert is_phantom({"key": "mem:episodic:x"})

    def test_a_record_with_any_field_is_not_a_phantom(self):
        assert not is_phantom({
            "key": "mem:project:widgets", "similarity_score": "0.1",
            "content": "", "created_at": "1788870766", "state": "active",
        })

    def test_empty_content_alone_is_not_a_phantom(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        """compile_project_context(auto_save=True) writes content="" when the
        project has no description yet — the real text is in current_state.
        Treating empty content as the phantom signal made every such project
        permanently unrecallable and told the operator to run reindex(),
        which cannot help."""
        monkeypatch.setenv("RECALL_MIN_SCORE", "0")
        import tools as tools_pkg
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        monkeypatch.setattr(tools_pkg, "_pipeline", pipeline)

        store_memory(
            fake_store, fake_embedder, "mem:episodic:w1",
            "ship the widget parser rewrite", project="widgets",
        )
        from tools.project import compile_project_context
        compile_project_context("widgets", auto_save=True)
        assert fake_store.get("mem:project:widgets").get("content") == "", (
            "precondition: the saved context must have empty content"
        )

        results = pipeline.recall(
            "ship the widget parser", namespaces=["project"], top_k=5,
        )
        assert "mem:project:widgets" in [r.key for r in results]


class TestDriftNote:
    def test_orphans_point_at_reindex(self):
        note = drift_note({"episodic": 145, "knowledge": 530})
        assert "675 index entries with no backing record" in note
        assert "reindex()" in note
        assert "not pick" not in note

    def test_a_lagging_index_does_not_point_at_reindex(self):
        note = drift_note({"knowledge": -3})
        assert "3 records the index has not picked up" in note
        assert "reindex() does not fix" in note

    def test_both_directions_are_described_separately(self):
        note = drift_note({"episodic": 2, "knowledge": -1})
        assert "2 index entries" in note
        assert "1 record the index has not picked up" in note

    def test_singulars(self):
        assert "1 index entry with no backing record" in drift_note({"a": 1})


# ---------------------------------------------------------------------------
# Coverage gaps found in review
# ---------------------------------------------------------------------------


class TestWeakMatchBand:
    def test_result_between_floor_and_weak_cut_is_flagged(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.1")
        monkeypatch.setenv("RECALL_WEAK_SCORE", "0.9")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        store_memory(
            fake_store, fake_embedder, "mem:episodic:w1",
            "postgres connection pooling notes for the B service",
        )
        [hit] = pipeline.recall(
            "postgres connection pooling tips", namespaces=["episodic"],
        )
        assert 0.1 <= hit.score < 0.9
        assert hit.weak_match is True

    def test_strong_result_is_not_flagged(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.1")
        monkeypatch.setenv("RECALL_WEAK_SCORE", "0.35")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        content = "postgres connection pooling tips"
        store_memory(fake_store, fake_embedder, "mem:episodic:w2", content)
        [hit] = pipeline.recall(content, namespaces=["episodic"])
        assert hit.weak_match is False

    def test_zero_disables_the_flag(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0")
        monkeypatch.setenv("RECALL_WEAK_SCORE", "0")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        store_memory(
            fake_store, fake_embedder, "mem:episodic:w3", "a note about nothing",
        )
        results = pipeline.recall("entirely other subject", namespaces=["episodic"])
        assert results and all(r.weak_match is False for r in results)

    def test_recall_tool_surfaces_the_flag(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools.core import recall as recall_tool

        monkeypatch.setenv("RECALL_MIN_SCORE", "0.1")
        monkeypatch.setenv("RECALL_WEAK_SCORE", "0.9")
        store_memory(
            fake_store, fake_embedder, "mem:episodic:w4",
            "postgres connection pooling notes for the B service",
        )
        entries = [
            e for e in recall_tool("postgres connection pooling tips")
            if e.get("key") == "mem:episodic:w4"
        ]
        assert entries and entries[0]["weak_match"] is True

    def test_reinstate_candidate_is_never_flagged_or_dropped(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        """A deprioritised memory surfaces on its own hints, not on vector
        similarity, so neither the floor nor the weak band applies to it."""
        monkeypatch.setenv("RECALL_MIN_SCORE", "0.9")
        monkeypatch.setenv("RECALL_WEAK_SCORE", "0.95")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        store_memory(
            fake_store, fake_embedder, "mem:episodic:dep1",
            "we shelved the graphql gateway experiment",
            state="deprioritised",
            reinstate_hints=["graphql"],
            deprioritised_reason="parked",
        )
        results = pipeline.recall(
            "should we revisit graphql", namespaces=["episodic"],
        )
        candidates = [r for r in results if r.reinstate_candidate]
        assert candidates, results
        assert candidates[0].weak_match is False


class TestVariantPhantomGuard:
    def test_expansion_variants_skip_phantoms(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        monkeypatch.setenv("RECALL_MIN_SCORE", "0")
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        real = store_memory(
            fake_store, fake_embedder, "mem:episodic:v1",
            "docker compose healthcheck for valkey",
        )
        monkeypatch.setattr(
            "memory.recall.expand_query",
            lambda query, store=None: ["valkey healthcheck compose"],
        )
        original = fake_store.search

        def _with_phantom(namespace, vector, top_k=10, filter_expr=None):
            docs = original(namespace, vector, top_k=top_k, filter_expr=filter_expr)
            return [{"key": "mem:episodic:ghost2", "similarity_score": "0.01"}] + docs

        monkeypatch.setattr(fake_store, "search", _with_phantom)
        results = pipeline.recall(
            "docker compose healthcheck", namespaces=["episodic"],
            expand_queries=True,
        )
        assert "mem:episodic:ghost2" not in [r.key for r in results]
        assert real in [r.key for r in results]


class TestBriefingSurfacesDrift:
    """Surfacing drift in the one call every session makes was the point of
    #28 — health() alone is what nobody looks at."""

    def _wire(self, monkeypatch, fake_store, drift):
        import tools as tools_pkg
        monkeypatch.setattr(
            fake_store, "index_report",
            lambda: {"indexes": {}, "records": {}, "drift": drift},
            raising=False,
        )
        monkeypatch.setattr(tools_pkg, "_store", fake_store)

    def test_drift_appears_with_counts_and_advice(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools.briefing import briefing

        self._wire(monkeypatch, fake_store, {"episodic": 145, "knowledge": 530})
        result = briefing()
        assert result["index_drift"]["orphaned_entries"] == 675
        assert result["index_drift"]["namespaces"]["knowledge"] == 530
        assert "reindex()" in result["index_drift"]["note"]

    def test_no_drift_means_no_entry(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools.briefing import briefing

        self._wire(monkeypatch, fake_store, {})
        assert "index_drift" not in briefing()

    def test_a_failing_report_does_not_break_the_briefing(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        import tools as tools_pkg
        from tools.briefing import briefing

        def _boom():
            raise RuntimeError("valkey gone")

        monkeypatch.setattr(fake_store, "index_report", _boom, raising=False)
        monkeypatch.setattr(tools_pkg, "_store", fake_store)
        assert "index_drift" not in briefing()


class TestStartupDriftCheck:
    def _server(self):
        import importlib
        import sys
        sys.modules.pop("server", None)
        return importlib.import_module("server")

    def _store(self, drift):
        class _S:
            def index_report(self_inner):
                return {"indexes": {}, "records": {}, "drift": drift}
        return _S()

    def test_orphans_warn_and_name_reindex(self, monkeypatch, caplog):
        monkeypatch.delenv("INDEX_DRIFT_CHECK", raising=False)
        server = self._server()
        with caplog.at_level("INFO"):
            server._check_index_drift(self._store({"episodic": 145}))
        assert "reindex()" in caplog.text
        assert any(r.levelname == "WARNING" for r in caplog.records)

    def test_a_lagging_index_does_not_warn(self, monkeypatch, caplog):
        """Expected moments after _migrate_indexes() recreates an index, and
        reindex() is not the remedy — so it must not read as a fault."""
        monkeypatch.delenv("INDEX_DRIFT_CHECK", raising=False)
        server = self._server()
        with caplog.at_level("INFO"):
            server._check_index_drift(self._store({"knowledge": -4}))
        assert not any(r.levelname == "WARNING" for r in caplog.records)
        assert "reindex()" not in caplog.text

    def test_clean_store_says_so(self, monkeypatch, caplog):
        monkeypatch.delenv("INDEX_DRIFT_CHECK", raising=False)
        server = self._server()
        with caplog.at_level("INFO"):
            server._check_index_drift(self._store({}))
        assert "all indexes match" in caplog.text

    @pytest.mark.parametrize("value", ["false", "0", "no", "FALSE"])
    def test_opt_out(self, monkeypatch, value):
        monkeypatch.setenv("INDEX_DRIFT_CHECK", value)
        server = self._server()

        class _Boom:
            def index_report(self):
                raise AssertionError("must not be called when opted out")

        server._check_index_drift(_Boom())

    def test_a_failing_report_is_logged_not_raised(self, monkeypatch, caplog):
        monkeypatch.delenv("INDEX_DRIFT_CHECK", raising=False)
        server = self._server()

        class _Boom:
            def index_report(self):
                raise RuntimeError("scan failed")

        with caplog.at_level("WARNING"):
            server._check_index_drift(_Boom())
        assert "drift check failed" in caplog.text


class TestFindSkillsRemainingGaps:
    def _skill(self, store, embedder, domain, description):
        key = f"mem:skill:gen:{domain}-local"
        store.upsert(
            "skill", key,
            {
                "name": f"{domain}-local", "description": description,
                "domain": domain, "state": "active", "generated": "true",
                "body": "---\n---\n", "source_manifest": "[]",
                "created_at": str(time.time()), "updated_at": str(time.time()),
            },
            embedder.embed(f"{domain}-local {description} {domain}"),
        )
        return key

    def test_zero_floor_returns_everything(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools.skills import find_skills

        monkeypatch.setenv("SKILL_MIN_SCORE", "0")
        self._skill(fake_store, fake_embedder, "opentofu", "infrastructure code")
        self._skill(fake_store, fake_embedder, "preferences", "house style")
        assert len(find_skills("something wholly unrelated")["skills"]) == 2

    def test_a_strong_match_suppresses_the_weak_note(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        from tools.skills import find_skills

        monkeypatch.setenv("SKILL_MIN_SCORE", "0.05")
        self._skill(fake_store, fake_embedder, "opentofu", "infrastructure code")
        self._skill(fake_store, fake_embedder, "preferences", "house style")
        result = find_skills("opentofu infrastructure code")
        assert result["skills"][0]["confidence"] == "high"
        assert "note" not in result


class TestHeldBackAndPoolEdges:
    def test_a_long_run_with_no_spaces_still_cuts(self):
        text = "x" * 900
        cut, truncated = _held_back_text(text)
        assert truncated is True
        assert cut == "x" * _HELD_BACK_MAX_CHARS + "…"

    def test_a_late_space_does_not_gut_the_text(self):
        """The word-boundary cut is skipped when it would throw away more
        than half, so a rule with one space near the start keeps its bulk."""
        text = "a " + "x" * 900
        cut, _ = _held_back_text(text)
        assert len(cut) > _HELD_BACK_MAX_CHARS // 2

    def test_pool_concentration_tie_break_is_deterministic(self):
        pool = [{"project": "alpha"}, {"project": "beta"}]
        first = _pool_concentration(pool)["top_project"]
        assert first == _pool_concentration(list(reversed(pool)))["top_project"]
