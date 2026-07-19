"""Targeted coverage for memory-package branches the main suites skip:
client-init paths, parse fallbacks, caps, and error recovery."""

import json
import sys
import time
import types
from unittest.mock import MagicMock

import numpy as np
import pytest

from tests.conftest import store_memory

from memory import chunking, extraction, query_expansion, tags, temporal
from memory.lifecycle import MemoryLifecycle, MemoryState, bulk_transition_project
from memory.migrations import migrate_project_names


# ---------------------------------------------------------------------------
# extraction.py
# ---------------------------------------------------------------------------


class _FakeAnthropicModule(types.ModuleType):
    def __init__(self, response_text=None, init_error=None):
        super().__init__("anthropic")
        self._response_text = response_text
        self._init_error = init_error
        module = self

        class Anthropic:
            def __init__(self, api_key):
                if module._init_error:
                    raise module._init_error
                self.messages = MagicMock()
                block = MagicMock()
                block.text = module._response_text or "[]"
                self.messages.create.return_value = MagicMock(content=[block])

        self.Anthropic = Anthropic


@pytest.fixture(autouse=True)
def _reset_llm_clients():
    extraction.reset_client_for_tests()
    query_expansion.reset_client_for_tests()
    yield
    extraction.reset_client_for_tests()
    query_expansion.reset_client_for_tests()


def _with_anthropic(monkeypatch, response_text, init_error=None):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test")
    monkeypatch.setitem(
        sys.modules, "anthropic",
        _FakeAnthropicModule(response_text, init_error),
    )


class TestExtraction:
    def test_client_cached_after_first_init(self, monkeypatch):
        _with_anthropic(monkeypatch, "[]")
        first = extraction._get_client()
        assert first is not None
        assert extraction._get_client() is first

    def test_client_init_failure_returns_none(self, monkeypatch):
        _with_anthropic(monkeypatch, "[]", init_error=RuntimeError("no sdk"))
        assert extraction._get_client() is None

    def test_extract_facts_happy_path_with_fences(self, monkeypatch):
        _with_anthropic(monkeypatch, (
            '```json\n'
            '[{"text": "Valkey uses AOF", "kind": "fact"},'
            ' {"text": "always pin builders", "kind": "preference",'
            '  "event_date": "2026-03-15"},'
            ' {"text": "", "kind": "fact"},'
            ' {"text": "odd kind", "kind": "wat"},'
            ' "not a dict"]\n'
            '```'
        ))
        facts = extraction.extract_facts("some content")
        assert [f.kind for f in facts] == ["fact", "preference", "fact"]
        assert facts[1].event_date is not None

    def test_extract_facts_non_list_and_api_error(self, monkeypatch):
        _with_anthropic(monkeypatch, '{"not": "a list"}')
        assert extraction.extract_facts("content") == []

        extraction.reset_client_for_tests()
        _with_anthropic(monkeypatch, "[]")
        client = extraction._get_client()
        client.messages.create.side_effect = RuntimeError("api down")
        assert extraction.extract_facts("content") == []

    def test_extract_facts_empty_content(self):
        assert extraction.extract_facts("   ") == []

    def test_parse_event_date_variants(self):
        assert extraction._parse_event_date(None) is None
        assert extraction._parse_event_date("") is None
        assert extraction._parse_event_date("   ") is None
        assert extraction._parse_event_date(1234.5) == 1234.5
        assert extraction._parse_event_date("2026-03-15") is not None
        # fromisoformat chokes, the [:10] strptime fallback succeeds
        assert extraction._parse_event_date("2026-03-15ish") is not None
        assert extraction._parse_event_date("not a date") is None


class TestQueryExpansion:
    def test_client_cached_and_init_failure(self, monkeypatch):
        _with_anthropic(monkeypatch, "[]")
        first = query_expansion._get_client()
        assert first is not None
        assert query_expansion._get_client() is first

        query_expansion.reset_client_for_tests()
        _with_anthropic(monkeypatch, "[]", init_error=RuntimeError("no sdk"))
        assert query_expansion._get_client() is None

    def test_expand_query_default_n_and_variants(self, monkeypatch):
        _with_anthropic(monkeypatch, '["one", "two", 3, "  ", null]')
        monkeypatch.delenv("RECALL_EXPAND_COUNT", raising=False)
        variants = query_expansion.expand_query("how do I deploy")
        assert variants == ["one", "two", "3"]

    def test_expand_query_non_list_response(self, monkeypatch):
        _with_anthropic(monkeypatch, '"just a string"')
        assert query_expansion.expand_query("query") == []

    def test_cache_read_paths(self, fake_store):
        key = query_expansion._cache_key("q", 3)
        # No entry
        assert query_expansion._read_cache(fake_store, key) is None
        # Entry without variants field
        fake_store.client.hset(key, mapping={"ts": "1"})
        assert query_expansion._read_cache(fake_store, key) is None
        # Malformed JSON
        fake_store.client.hset(key, mapping={"variants": "{nope"})
        assert query_expansion._read_cache(fake_store, key) is None
        # Non-list JSON
        fake_store.client.hset(key, mapping={"variants": '"str"'})
        assert query_expansion._read_cache(fake_store, key) is None
        # Valid
        fake_store.client.hset(key, mapping={"variants": '["a", "b"]'})
        assert query_expansion._read_cache(fake_store, key) == ["a", "b"]
        # hgetall blowing up degrades to None
        broken = MagicMock()
        broken.client.hgetall.side_effect = RuntimeError("down")
        assert query_expansion._read_cache(broken, key) is None

    def test_cache_write_failure_swallowed(self):
        broken = MagicMock()
        broken.client.pipeline.side_effect = RuntimeError("down")
        query_expansion._write_cache(broken, "qexp:x", ["a"])  # no raise

    def test_expand_query_uses_cache(self, fake_store, monkeypatch):
        key = query_expansion._cache_key("cached question", 3)
        fake_store.client.hset(key, mapping={"variants": '["hit"]'})
        monkeypatch.delenv("RECALL_EXPAND_COUNT", raising=False)
        assert query_expansion.expand_query(
            "cached question", store=fake_store,
        ) == ["hit"]

    def test_expand_query_empty(self):
        assert query_expansion.expand_query("  ") == []


# ---------------------------------------------------------------------------
# temporal.py
# ---------------------------------------------------------------------------


class TestTemporal:
    def test_looks_temporal_empty(self):
        assert temporal.looks_temporal("") is False

    def test_parse_without_dateparser_installed(self, monkeypatch):
        monkeypatch.setitem(sys.modules, "dateparser", None)
        assert temporal.parse_query_date("what happened yesterday") is None

    def test_parse_falls_back_to_search_dates(self):
        # Prose around the date defeats the whole-string parse, so the
        # search_dates fallback has to find it.
        parsed = temporal.parse_query_date(
            "remind me what the deployment plan decided on 2026-03-15 was",
        )
        assert parsed is not None
        assert parsed.year == 2026

    def test_parse_finds_nothing(self, monkeypatch):
        import dateparser
        import dateparser.search

        monkeypatch.setattr(dateparser, "parse", lambda *a, **k: None)
        monkeypatch.setattr(
            dateparser.search, "search_dates", lambda *a, **k: None,
        )
        assert temporal.parse_query_date("what happened yesterday") is None

    def test_parse_survives_parser_exceptions(self, monkeypatch):
        import dateparser
        import dateparser.search

        def boom(*a, **k):
            raise RuntimeError("parser exploded")

        monkeypatch.setattr(dateparser, "parse", boom)
        monkeypatch.setattr(dateparser.search, "search_dates", boom)
        assert temporal.parse_query_date("what happened yesterday") is None

    def test_boost_bad_timestamp(self):
        from datetime import datetime
        assert temporal.temporal_boost(datetime(2026, 1, 1), 1e18) == 1.0

    def test_boost_falloff_band(self):
        from datetime import datetime
        base = datetime(2026, 1, 1)
        mid = base.timestamp() + 30 * 86400  # inside 7..60 day falloff
        boost = temporal.temporal_boost(base, mid)
        assert 1.0 < boost < temporal._MAX_BOOST


# ---------------------------------------------------------------------------
# embedder.py
# ---------------------------------------------------------------------------


class TestEmbedder:
    @pytest.fixture(autouse=True)
    def _isolate_singleton(self, monkeypatch):
        from memory import embedder as embedder_module

        class FakeST:
            def __init__(self, name):
                self.name = name

            def encode(self, texts, normalize_embeddings=True):
                if isinstance(texts, str):
                    return np.ones(4)
                return [np.ones(4) for _ in texts]

        monkeypatch.setattr(embedder_module, "SentenceTransformer", FakeST)
        monkeypatch.setattr(embedder_module.Embedder, "_instance", None)
        monkeypatch.setattr(embedder_module.Embedder, "_model", None)
        yield

    def test_singleton_load_and_embed(self):
        from memory.embedder import Embedder

        a, b = Embedder(), Embedder()
        assert a is b
        assert a.is_loaded is False
        a.load()
        assert a.is_loaded is True
        a.load()  # idempotent
        assert a.embed("text").dtype == np.float32
        assert len(a.embed_batch(["x", "y"])) == 2

    def test_model_property_lazy_loads(self):
        from memory.embedder import Embedder

        e = Embedder()
        assert e.model is not None
        assert e.is_loaded is True


# ---------------------------------------------------------------------------
# enrichment.py — worker start/stop and the _run loop
# ---------------------------------------------------------------------------


class TestEnrichmentWorker:
    def test_start_stop_and_double_start(self):
        from memory.enrichment import EnrichmentWorker

        store = MagicMock()
        store.client.brpop.return_value = None
        worker = EnrichmentWorker(store, MagicMock())
        worker.start()
        thread = worker._thread
        worker.start()  # second start is a no-op
        assert worker._thread is thread
        worker.stop()
        assert worker._thread is None

    def test_queue_length_and_failure(self):
        from memory.enrichment import EnrichmentWorker

        store = MagicMock()
        store.client.llen.return_value = 4
        assert EnrichmentWorker(store, MagicMock()).queue_length == 4
        store.client.llen.side_effect = RuntimeError("down")
        assert EnrichmentWorker(store, MagicMock()).queue_length == -1

    def test_run_loop_handles_every_payload_shape(self, monkeypatch):
        from memory.enrichment import QUEUE_KEY, EnrichmentWorker

        monkeypatch.setattr(time, "sleep", lambda s: None)
        store = MagicMock()
        worker = EnrichmentWorker(store, MagicMock())

        enriched = []
        monkeypatch.setattr(worker, "_enrich", lambda p: enriched.append(p))

        crashing = json.dumps({"key": "mem:episodic:01X"})

        def krak(payload):
            raise RuntimeError("enrich exploded")

        events = iter([
            None,                                     # timeout → loop again
            (QUEUE_KEY, "{not json"),                 # bad payload → skipped
            (QUEUE_KEY, json.dumps({"key": "k1"})),   # good → _enrich
            RuntimeError("brpop died"),               # client error → sleep
            "stop",
        ])

        def brpop(key, timeout=0):
            event = next(events)
            if event == "stop":
                worker._stop.set()
                return None
            if isinstance(event, Exception):
                raise event
            return event

        store.client.brpop.side_effect = brpop
        worker._run()
        assert enriched == [{"key": "k1"}]

        # And an _enrich failure is logged, not fatal.
        worker._stop.clear()
        events = iter([(QUEUE_KEY, crashing), "stop"])
        monkeypatch.setattr(worker, "_enrich", krak)
        worker._run()


# ---------------------------------------------------------------------------
# maintenance.py — error recovery and caps
# ---------------------------------------------------------------------------


class TestMaintenanceErrorPaths:
    def test_all_three_phases_survive_failures(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        from memory import maintenance

        monkeypatch.setattr(
            maintenance, "find_all_duplicates",
            MagicMock(side_effect=RuntimeError("dedup down")),
        )
        monkeypatch.setattr(
            maintenance, "expire_knowledge_items",
            MagicMock(side_effect=RuntimeError("expiry down")),
        )
        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "use systemd for this", project="p")
        # Break the contradiction phase too: vectors read blows up.
        monkeypatch.setattr(
            fake_store, "get_vectors_multi",
            MagicMock(side_effect=RuntimeError("vectors down")),
        )
        result = maintenance.run_maintenance(
            fake_store, fake_embedder, lifecycle, "p",
        )
        assert result["duplicates_archived"] == []
        assert result["contradictions_found"] == []
        assert result["knowledge_expired"] == []

    def test_dedup_archive_transition_failure_skipped(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        from memory import maintenance

        cluster = {"memories": [
            {"key": "mem:episodic:01A", "created_at": "1"},
            {"key": "mem:episodic:01B", "created_at": "2"},
        ]}
        monkeypatch.setattr(
            maintenance, "find_all_duplicates", MagicMock(return_value=[cluster]),
        )
        monkeypatch.setattr(
            lifecycle, "transition",
            MagicMock(side_effect=ValueError("bad transition")),
        )
        result = maintenance.run_maintenance(
            fake_store, fake_embedder, lifecycle, "p",
        )
        assert result["duplicates_archived"] == []

    def test_contradiction_vector_fallback_and_hit(
        self, fake_store, fake_embedder, lifecycle,
    ):
        from memory import maintenance

        # Same wording ± negation → similar vectors AND a negation pair.
        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "always use docker compose for deploys", project="p")
        # Stored without a vector → forces the embed_batch fallback.
        fake_store.client.hset("mem:episodic:01B", mapping={
            "content": "never use docker compose for deploys",
            "state": "active", "project": "p",
        })
        result = maintenance.run_maintenance(
            fake_store, fake_embedder, lifecycle, "p",
        )
        assert len(result["contradictions_found"]) == 1

    def test_comparison_cap_short_circuits(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        from memory import maintenance

        monkeypatch.setattr(maintenance, "_CONTRADICTION_COMPARISON_CAP", 0)
        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "always use docker", project="p")
        store_memory(fake_store, fake_embedder, "mem:episodic:01B",
                     "never use docker", project="p")
        result = maintenance.run_maintenance(
            fake_store, fake_embedder, lifecycle, "p",
        )
        assert result["contradictions_found"] == []

    def test_expire_transition_failure_skipped(
        self, fake_store, fake_embedder, lifecycle, monkeypatch,
    ):
        from memory.maintenance import expire_knowledge_items

        store_memory(fake_store, fake_embedder, "mem:knowledge:01A", "article",
                     namespace="knowledge")
        fake_store.set_fields("mem:knowledge:01A", {
            "feed_name": "feed", "expires_at": "1",
        })
        monkeypatch.setattr(
            lifecycle, "transition",
            MagicMock(side_effect=ValueError("cannot")),
        )
        assert expire_knowledge_items(fake_store, lifecycle) == []


# ---------------------------------------------------------------------------
# lifecycle.py
# ---------------------------------------------------------------------------


class TestLifecycleBranches:
    def test_bulk_transition_skips_odd_rows(self, fake_store, fake_embedder):
        # A row with an unparseable state falls back to ACTIVE and counts;
        # a row carrying none of the projected fields is skipped outright.
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "fine",
                     project="p")
        fake_store.client.hset("mem:episodic:01B", mapping={"other": "x"})
        store_memory(fake_store, fake_embedder, "mem:episodic:01C", "odd",
                     project="p", state="bogus-state")

        result = bulk_transition_project(
            fake_store, "p", MemoryState.DEPRIORITISED, apply=True,
        )
        assert result["changed"] == 2  # 01A and 01C (bogus → ACTIVE)

    def test_deprioritise_warning_tolerates_bad_effort(
        self, fake_store, fake_embedder, lifecycle,
    ):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "hard won")
        fake_store.set_fields("mem:episodic:01A", {"effort_score": "lots"})
        result = lifecycle.transition(
            "mem:episodic:01A", MemoryState.DEPRIORITISED, reason="test",
        )
        assert "warning" not in result

    def test_add_reinstate_hints_paths(self, fake_store, fake_embedder, lifecycle):
        with pytest.raises(ValueError, match="not found"):
            lifecycle.add_reinstate_hints("mem:episodic:missing", ["hint"])

        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "content")
        fake_store.set_fields("mem:episodic:01A", {"reinstate_hints": "{broken"})
        lifecycle.add_reinstate_hints("mem:episodic:01A", ["kubernetes"])
        stored = json.loads(fake_store.get("mem:episodic:01A")["reinstate_hints"])
        assert stored == ["kubernetes"]

    def test_check_reinstate_eligibility_bad_hints(
        self, fake_store, fake_embedder, lifecycle,
    ):
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "content",
                     state="deprioritised")
        fake_store.set_fields("mem:episodic:01A", {"reinstate_hints": "{broken"})
        assert lifecycle.check_reinstate_eligibility(
            "mem:episodic:01A", "kubernetes",
        ) is False


# ---------------------------------------------------------------------------
# dedup.py, chunking.py, tags.py, migrations.py
# ---------------------------------------------------------------------------


class TestDedupBranches:
    def test_project_filter_and_vector_fallback(self, fake_store, fake_embedder):
        from memory.dedup import find_all_duplicates

        store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                     "identical content here", project="mine")
        # Same content, no stored vector → embed fallback path.
        fake_store.client.hset("mem:episodic:01B", mapping={
            "content": "identical content here", "state": "active",
            "project": "mine",
        })
        store_memory(fake_store, fake_embedder, "mem:episodic:01C",
                     "identical content here", project="other")

        clusters = find_all_duplicates(
            fake_store, fake_embedder, project_filter="mine",
        )
        assert len(clusters) == 1
        keys = {m["key"] for m in clusters[0]["memories"]}
        assert keys == {"mem:episodic:01A", "mem:episodic:01B"}


class TestChunkingBranches:
    def test_empty_inputs(self):
        assert chunking.chunk_turn_pairs("   ") == []
        assert chunking.chunk_sentences("   ") == []
        assert chunking.chunk_fixed_tokens("   ") == []

    def test_sentences_trailing_abbreviation_flushes_buffer(self):
        out = chunking.chunk_sentences("We shipped it. See the appendix etc.")
        assert out[-1].endswith("etc.")

    def test_fixed_tokens_rejects_bad_size(self):
        with pytest.raises(ValueError, match="chunk_size"):
            chunking.chunk_fixed_tokens("some words", chunk_size=-1)

    def test_dispatch_covers_all_strategies(self):
        text = "One sentence. Another one.\n\nNew paragraph."
        assert chunking.chunk(text, "sentences")
        assert chunking.chunk(text, "paragraphs")
        assert chunking.chunk(text, "fixed_tokens")
        with pytest.raises(ValueError):
            chunking.chunk(text, "wat")


class TestTagsBranches:
    def test_parse_tags_field_damage(self):
        assert tags.parse_tags_field("{broken") == []
        assert tags.parse_tags_field('"a string"') == []
        assert tags.parse_tags_field(None) == []
        assert tags.parse_tags_field('["a", 1]') == ["a", "1"]


class TestMigrationsBranches:
    def test_project_names_empty_store_and_empty_rows(self, fake_store):
        migrate_project_names(fake_store)  # no keys → early return
        fake_store.client.hset("mem:project:01A", mapping={"vector": b"x"})
        migrate_project_names(fake_store)  # row reads as None → continue


# ---------------------------------------------------------------------------
# skill_compiler.py
# ---------------------------------------------------------------------------


class TestSkillCompilerBranches:
    def test_compact_drops_empty_values(self):
        from memory.skill_compiler import _compact

        assert _compact({
            "keep": "x", "none": None, "empty_str": "",
            "empty_list": [], "empty_dict": {}, "zero": 0,
        }) == {"keep": "x", "zero": 0}

    def test_safe_export_path_rejections(self):
        from memory.skill_compiler import safe_export_path

        assert safe_export_path("")[1] is not None
        assert safe_export_path("/abs/path.md")[1] is not None
        assert safe_export_path("~/home.md")[1] is not None
        assert safe_export_path("///")[1] is not None
        assert safe_export_path(".hidden/skill.md")[1] is not None
        assert safe_export_path("dir/skill.txt")[1] is not None
        path, err = safe_export_path("python-ric/SKILL.md")
        assert err is None and str(path).endswith("python-ric/SKILL.md")

    def test_invalid_mode_raises(self, fake_store, fake_embedder):
        from memory.skill_compiler import compile_skill_flow

        with pytest.raises(ValueError, match="mode must be"):
            compile_skill_flow(fake_store, fake_embedder, "python", mode="wat")

    def _seed_pool(self, fake_store, fake_embedder, domain="deploy"):
        for i, project in enumerate(("alpha", "beta")):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:01{domain[:2].upper()}{i}",
                f"{domain} lesson", tags=[domain], project=project,
                breakthrough="pin the builder", outcome="succeeded",
            )

    def test_body_too_large_refused(self, fake_store, fake_embedder, monkeypatch):
        from memory import skill_compiler

        self._seed_pool(fake_store, fake_embedder)
        monkeypatch.setattr(skill_compiler, "_MAX_SKILL_BODY", 10)
        result = skill_compiler.compile_skill_flow(
            fake_store, fake_embedder, "deploy", mode="propose",
        )
        assert result["status"] == "error"
        assert "too large" in result["reason"]

    def test_corrupt_existing_rule_manifest_tolerated(
        self, fake_store, fake_embedder,
    ):
        from memory.skill_compiler import compile_skill_flow

        self._seed_pool(fake_store, fake_embedder)
        assert compile_skill_flow(
            fake_store, fake_embedder, "deploy", mode="propose",
        )["status"] == "proposal"
        assert compile_skill_flow(
            fake_store, fake_embedder, "deploy", mode="write",
        )["status"] == "written"

        fake_store.set_fields("mem:skill:gen:deploy-local", {
            "rule_manifest": "{broken",
        })
        # Change the pool so the recompile actually differs.
        store_memory(
            fake_store, fake_embedder, "mem:episodic:01DE9",
            "deploy lesson", tags=["deploy"], project="gamma",
            breakthrough="pin the builder", outcome="succeeded",
        )
        result = compile_skill_flow(
            fake_store, fake_embedder, "deploy", mode="propose",
        )
        assert result["status"] == "proposal"

    def test_export_path_success_and_failure(
        self, fake_store, fake_embedder, monkeypatch, tmp_path,
    ):
        from memory.skill_compiler import compile_skill_flow

        monkeypatch.setenv("SKILL_EXPORT_DIR", str(tmp_path))
        self._seed_pool(fake_store, fake_embedder, domain="ansible")
        assert compile_skill_flow(
            fake_store, fake_embedder, "ansible", mode="propose",
        )["status"] == "proposal"
        result = compile_skill_flow(
            fake_store, fake_embedder, "ansible", mode="write",
            export_path="ansible/SKILL.md",
        )
        assert (tmp_path / "ansible" / "SKILL.md").exists()
        assert result["exported_to"].endswith("ansible/SKILL.md")

        # Second skill, unwritable target → export_error, write still lands.
        self._seed_pool(fake_store, fake_embedder, domain="python")
        assert compile_skill_flow(
            fake_store, fake_embedder, "python", mode="propose",
        )["status"] == "proposal"
        import pathlib
        monkeypatch.setattr(
            pathlib.Path, "write_text",
            MagicMock(side_effect=OSError("disk full")),
        )
        result = compile_skill_flow(
            fake_store, fake_embedder, "python", mode="write",
            export_path="python/SKILL.md",
        )
        assert result["status"] == "written"
        assert result["export_error"] == "Failed to write export file"

    def test_bad_export_path_reported(self, fake_store, fake_embedder):
        from memory.skill_compiler import compile_skill_flow

        self._seed_pool(fake_store, fake_embedder, domain="rust")
        assert compile_skill_flow(
            fake_store, fake_embedder, "rust", mode="propose",
        )["status"] == "proposal"
        result = compile_skill_flow(
            fake_store, fake_embedder, "rust", mode="write",
            export_path="/etc/passwd.md",
        )
        assert result["status"] == "written"
        assert "export_error" in result


# ---------------------------------------------------------------------------
# skill_scan.py — remaining candidate-filter branches
# ---------------------------------------------------------------------------


class TestSkillScanBranches:
    def test_invalid_domain_tags_skipped(self, fake_store, fake_embedder):
        from memory.skill_scan import run_skill_scan

        # A tag that can't be a domain (uppercase disallowed after
        # normalisation happens only in compile, not raw tags with spaces
        # sneaking past) must be skipped, not crash the scan.
        store_memory(fake_store, fake_embedder, "mem:episodic:01A", "x",
                     tags=["-leading-hyphen"], project="p",
                     breakthrough="something", outcome="succeeded")
        result = run_skill_scan(fake_store, fake_embedder)
        assert result["proposals"] == []

    def test_pool_without_eligible_rules_skipped(self, fake_store, fake_embedder):
        from memory.skill_scan import run_skill_scan

        # Three lesson-bearing memories, all different lessons → nothing
        # clusters, nothing clears the gate.
        for i, text in enumerate((
            "use ansible vault for secrets",
            "prefer rolling updates to recreate",
            "quote all jinja expressions",
        )):
            store_memory(fake_store, fake_embedder, f"mem:episodic:01A{i}",
                         text, tags=["ansible"], project=f"p{i}",
                         breakthrough=text, outcome="succeeded")
        result = run_skill_scan(fake_store, fake_embedder)
        assert result["proposals"] == []
        assert result["new_skill_candidates_checked"] == 1

    def test_update_phase_respects_cap(self, fake_store, fake_embedder, monkeypatch):
        from memory.skill_scan import run_skill_scan

        monkeypatch.setenv("SKILL_SCAN_MAX_PROPOSALS", "0")
        result = run_skill_scan(
            fake_store, fake_embedder, update_domains=["deploy"],
        )
        assert result["proposals"] == []
