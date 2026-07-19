"""Tests for the function audit fixes: restore round-trip, enrichment dedup,
recall over-fetch/starvation, variant temporal parity, stored-vector reuse,
abandoned cache, and filter expression construction."""

import json
import time

import numpy as np
import pytest

from tests.conftest import FakeValkeyStore, FakeEmbedder, store_memory
from memory.lifecycle import MemoryLifecycle
from memory.recall import (
    RecallPipeline,
    _build_filter_expr,
    _candidate_k,
)


@pytest.fixture(autouse=True)
def _wire_tools(fake_store, fake_embedder, monkeypatch):
    """Wire shared tool deps to fakes for tool-level tests."""
    import tools as tools_pkg

    lifecycle = MemoryLifecycle(fake_store)
    pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
    monkeypatch.setattr(tools_pkg, "_store", fake_store)
    monkeypatch.setattr(tools_pkg, "_embedder", fake_embedder)
    monkeypatch.setattr(tools_pkg, "_lifecycle", lifecycle)
    monkeypatch.setattr(tools_pkg, "_pipeline", pipeline)
    yield


# ---------------------------------------------------------------------------
# Backup: dump -> restore round-trip must accept meta:* keys
# ---------------------------------------------------------------------------


class TestBackupRoundTrip:
    def test_restore_accepts_meta_keys(self, fake_store, fake_embedder, tmp_path, monkeypatch):
        from tools.backup import dump_to_file, restore_from_file

        monkeypatch.setenv("BACKUP_DIR", str(tmp_path))
        store_memory(fake_store, fake_embedder, "mem:episodic:rt1", "round trip fact")
        # Telemetry always creates meta: keys in real deployments
        fake_store._client.hset("meta:tool_metrics:recall", mapping={"call_count": "3"})

        dumped = dump_to_file("roundtrip.json")
        assert dumped.get("status") != "error", dumped

        result = restore_from_file("roundtrip.json", dry_run=False)
        assert result["status"] == "restored", result

    def test_dotted_filenames_accepted(self, tmp_path, monkeypatch):
        from tools.backup import _validate_filename

        assert _validate_filename("memory_backup_20260708.json") is None
        assert _validate_filename("backup.v2.json") is None
        # Traversal and hidden files still rejected
        assert _validate_filename("../evil.json") is not None
        assert _validate_filename(".hidden.json") is not None
        assert _validate_filename("a/b.json") is not None

    def test_dump_counts_preference_namespace(self, fake_store, fake_embedder, tmp_path, monkeypatch):
        from tools.backup import dump_to_file

        monkeypatch.setenv("BACKUP_DIR", str(tmp_path))
        store_memory(fake_store, fake_embedder, "mem:preference:p1",
                     "always run the tests", namespace="preference")
        dump_to_file("prefs.json")
        backup = json.loads((tmp_path / "prefs.json").read_text())
        assert backup["metadata"]["namespaces"]["preference"] == 1


# ---------------------------------------------------------------------------
# Enrichment: async path must dedup extracted facts again
# ---------------------------------------------------------------------------


class TestEnrichmentDedup:
    def test_same_fact_not_stored_twice(self, fake_store, fake_embedder, monkeypatch):
        from memory import enrichment as enrichment_mod
        from memory.enrichment import EnrichmentWorker
        from memory.extraction import ExtractedFact

        monkeypatch.setattr(
            enrichment_mod, "extract_facts",
            lambda content: [ExtractedFact(text="valkey needs a password", kind="fact")],
        )

        store_memory(fake_store, fake_embedder, "mem:episodic:src1", "raw input one")
        store_memory(fake_store, fake_embedder, "mem:episodic:src2", "raw input two")

        worker = EnrichmentWorker(fake_store, fake_embedder)
        worker._enrich({"key": "mem:episodic:src1", "namespace": "episodic"})
        worker._enrich({"key": "mem:episodic:src2", "namespace": "episodic"})

        # Facts route to knowledge (issue #20); the duplicate is skipped there.
        fact_keys = [
            k for k in fake_store._client._data if k.startswith("mem:knowledge:")
        ]
        assert len(fact_keys) == 1, fact_keys


# ---------------------------------------------------------------------------
# Recall: candidate over-fetch fixes project-filter starvation
# ---------------------------------------------------------------------------


class TestRecallStarvation:
    def test_project_filter_finds_memory_beyond_top_20(self, fake_store, fake_embedder, lifecycle):
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)

        # 30 near-identical memories for project A dominate the KNN ranking
        for i in range(30):
            store_memory(
                fake_store, fake_embedder, f"mem:episodic:a{i:03d}",
                f"postgres connection pooling tips variant {i}",
                project="projA",
            )
        # One project-B memory, similar enough to match but ranked below the A flood
        store_memory(
            fake_store, fake_embedder, "mem:episodic:b001",
            "postgres pooling note for the B service",
            project="projB",
        )

        results = pipeline.recall(
            "postgres connection pooling tips variant", top_k=5,
            namespaces=["episodic"], project_filter="projB",
        )
        keys = [r.key for r in results if r.result_type == "memory"]
        assert "mem:episodic:b001" in keys, keys

    def test_candidate_k_policy(self):
        assert _candidate_k(5, None) == 20
        assert _candidate_k(35, None) == 35
        assert _candidate_k(5, "proj") == 50
        assert _candidate_k(80, "proj") == 80


# ---------------------------------------------------------------------------
# Filter expression construction (syntax verified live against valkey-search)
# ---------------------------------------------------------------------------


class TestFilterExpr:
    def test_state_only(self):
        expr = _build_filter_expr("knowledge", None)
        assert expr == "(@state:{active} | @state:{deprioritised})"

    def test_project_pushed_down_raw(self):
        expr = _build_filter_expr("episodic", "omni mem")
        # Raw value, no escaping — valkey-search fails on escaped/quoted tags
        assert "@project:{omni mem}" in expr
        assert "\\" not in expr

    def test_knowledge_pushed_project_ns_not(self):
        # knowledge gained an indexed project tag in v5.3.1 (issue #20 facts)
        assert "@project:{proj}" in _build_filter_expr("knowledge", "proj")
        # project namespace stays Python-side (project_name backfill timing)
        assert "@project" not in _build_filter_expr("project", "proj")

    def test_unsafe_value_not_interpolated(self):
        expr = _build_filter_expr("episodic", "evil}|injection")
        assert "evil" not in expr


# ---------------------------------------------------------------------------
# Variant search parity: temporal boost + event_date
# ---------------------------------------------------------------------------


class TestVariantTemporalParity:
    def test_variant_applies_temporal_boost(self, fake_store, fake_embedder, lifecycle):
        from datetime import datetime

        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        now = time.time()
        key = store_memory(
            fake_store, fake_embedder, "mem:episodic:t1", "deployed the api gateway",
        )
        fake_store.set_field(key, "event_date", str(now))

        results = pipeline._search_variant(
            variant="api gateway deployment",
            namespaces=["episodic"],
            project_filter=None,
            suppressed_topics=[],
            recency_decay_days=90,
            now=now,
            top_k=5,
            query_date=datetime.fromtimestamp(now),
        )
        match = next(r for r in results if r.key == key)
        assert match.event_date == pytest.approx(now)
        # Full temporal boost multiplies the score by 1.5 vs the raw score
        assert match.adjusted_score == pytest.approx(match.score * 1.5)


# ---------------------------------------------------------------------------
# Stored-vector reuse
# ---------------------------------------------------------------------------


class TestStoredVectors:
    def test_get_vectors_multi_roundtrip(self, fake_store, fake_embedder):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:v1", "vector text")
        vecs = fake_store.get_vectors_multi([key, "mem:episodic:missing"])
        assert vecs[1] is None
        assert vecs[0] is not None and vecs[0].dtype == np.float32 and len(vecs[0]) == 384
        # Round-trips the exact stored embedding
        expected = fake_embedder.embed("vector text")
        assert float(np.dot(vecs[0], expected)) == pytest.approx(1.0, abs=1e-5)

    def test_find_all_duplicates_uses_stored_vectors(self, fake_store, fake_embedder, monkeypatch):
        from memory.dedup import find_all_duplicates

        store_memory(fake_store, fake_embedder, "mem:episodic:d1", "use valkey for caching layers")
        store_memory(fake_store, fake_embedder, "mem:episodic:d2", "use valkey for caching layers")
        store_memory(fake_store, fake_embedder, "mem:episodic:d3", "completely unrelated topic")

        from unittest.mock import Mock
        _boom = Mock(side_effect=AssertionError(
            "embed_batch should not run when stored vectors exist"))
        monkeypatch.setattr(fake_embedder, "embed_batch", _boom)
        clusters = find_all_duplicates(fake_store, fake_embedder, "episodic")
        assert len(clusters) == 1
        cluster_keys = {m["key"] for m in clusters[0]["memories"]}
        assert cluster_keys == {"mem:episodic:d1", "mem:episodic:d2"}
        assert clusters[0]["max_similarity"] >= 0.92


# ---------------------------------------------------------------------------
# Abandoned cache: TTL + invalidation on writes
# ---------------------------------------------------------------------------


class TestAbandonedCache:
    def test_cache_reused_within_ttl(self, fake_store, fake_embedder, lifecycle, monkeypatch):
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        store_memory(
            fake_store, fake_embedder, "mem:episodic:ab1", "tried onnx runtime",
            abandoned_approaches=[{"name": "onnxruntime", "type": "library", "reason": "SIGILL"}],
        )
        assert pipeline.warn_if_abandoned("should we use onnxruntime?")

        # A direct store write (bypassing the tools) is invisible until TTL/invalidations
        store_memory(
            fake_store, fake_embedder, "mem:episodic:ab2", "tried grpc",
            abandoned_approaches=[{"name": "grpc", "type": "library", "reason": "too heavy"}],
        )
        assert not pipeline.warn_if_abandoned("adopt grpc here?")

        pipeline.invalidate_abandoned_cache()
        assert pipeline.warn_if_abandoned("adopt grpc here?")

    def test_log_abandoned_tool_invalidates(self, fake_store, fake_embedder):
        import tools as tools_pkg
        from tools.experience import log_abandoned

        pipeline = tools_pkg._pipeline
        key = store_memory(fake_store, fake_embedder, "mem:episodic:ab3", "networking attempt")
        # Prime the cache with no abandonments
        assert not pipeline.warn_if_abandoned("zeromq")

        log_abandoned(key, name="zeromq", type="library", reason="unmaintained bindings")
        assert pipeline.warn_if_abandoned("zeromq"), "cache must be invalidated by log_abandoned"


# ---------------------------------------------------------------------------
# count_all_records single scan
# ---------------------------------------------------------------------------


class TestCountAllRecords:
    def test_counts_by_namespace(self, fake_store, fake_embedder):
        # FakeValkeyStore has no count_all_records; validate the real logic
        # against the same scan_prefix contract.
        from memory.store import ValkeyStore

        store_memory(fake_store, fake_embedder, "mem:episodic:c1", "one")
        store_memory(fake_store, fake_embedder, "mem:episodic:c2", "two")
        store_memory(fake_store, fake_embedder, "mem:knowledge:c3", "three", namespace="knowledge")

        counts = ValkeyStore.count_all_records(fake_store)
        assert counts["episodic"] == 2
        assert counts["knowledge"] == 1
        assert counts["project"] == 0
