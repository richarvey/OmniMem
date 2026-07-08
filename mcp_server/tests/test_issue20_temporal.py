"""Tests for issue #20: fact extraction must supplement verbatim content, not
compete with it — knowledge-namespace routing, the event_date fallback chain,
verbatim-first surface scores, and source-present fact suppression in recall."""

import time

import pytest

from tests.conftest import store_memory
from memory import enrichment
from memory.enrichment import EnrichmentWorker
from memory.extraction import ExtractedFact
from memory.recall import RecallPipeline


def _facts(*facts):
    """Monkeypatch helper: extract_facts stub returning fixed facts."""
    return lambda content: list(facts)


# ---------------------------------------------------------------------------
# Namespace routing
# ---------------------------------------------------------------------------


class TestNamespaceRouting:
    def test_facts_route_to_knowledge_not_source_namespace(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        store_memory(fake_store, fake_embedder, "mem:episodic:r1", "raw episodic input")
        monkeypatch.setattr(
            enrichment, "extract_facts",
            _facts(ExtractedFact(text="a discrete fact", kind="fact")),
        )
        EnrichmentWorker(fake_store, fake_embedder)._enrich(
            {"key": "mem:episodic:r1", "namespace": "episodic"}
        )
        assert [k for k in fake_store._client._data if k.startswith("mem:knowledge:")]
        assert not [
            k for k in fake_store._client._data
            if k.startswith("mem:episodic:") and k != "mem:episodic:r1"
        ]

    def test_preference_facts_still_route_to_preference(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        store_memory(fake_store, fake_embedder, "mem:episodic:r2", "raw input")
        monkeypatch.setattr(
            enrichment, "extract_facts",
            _facts(ExtractedFact(text="always run the linter first", kind="preference")),
        )
        EnrichmentWorker(fake_store, fake_embedder)._enrich(
            {"key": "mem:episodic:r2", "namespace": "episodic"}
        )
        assert [k for k in fake_store._client._data if k.startswith("mem:preference:")]
        assert not [k for k in fake_store._client._data if k.startswith("mem:knowledge:")]


# ---------------------------------------------------------------------------
# event_date fallback chain: fact -> source event_date -> source created_at
# ---------------------------------------------------------------------------


class TestTimestampFallbackChain:
    def _fact_data(self, fake_store):
        keys = [k for k in fake_store._client._data if k.startswith("mem:knowledge:")]
        assert len(keys) == 1, keys
        return fake_store.get(keys[0])

    def test_fact_own_event_date_wins(self, fake_store, fake_embedder, monkeypatch):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t1", "moved house")
        fake_store.set_field(key, "event_date", "1700000000")
        monkeypatch.setattr(
            enrichment, "extract_facts",
            _facts(ExtractedFact(text="user moved", kind="fact", event_date=1800000000.0)),
        )
        EnrichmentWorker(fake_store, fake_embedder)._enrich(
            {"key": key, "namespace": "episodic"}
        )
        assert self._fact_data(fake_store)["event_date"] == "1800000000.0"

    def test_source_event_date_inherited(self, fake_store, fake_embedder, monkeypatch):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t2", "moved house")
        fake_store.set_field(key, "event_date", "1700000000")
        monkeypatch.setattr(
            enrichment, "extract_facts",
            _facts(ExtractedFact(text="user moved to Edinburgh", kind="fact")),
        )
        EnrichmentWorker(fake_store, fake_embedder)._enrich(
            {"key": key, "namespace": "episodic"}
        )
        assert self._fact_data(fake_store)["event_date"] == "1700000000"

    def test_source_created_at_is_last_resort(self, fake_store, fake_embedder, monkeypatch):
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t3", "moved house")
        source_created = fake_store.get(key)["created_at"]
        monkeypatch.setattr(
            enrichment, "extract_facts",
            _facts(ExtractedFact(text="user moved somewhere", kind="fact")),
        )
        EnrichmentWorker(fake_store, fake_embedder)._enrich(
            {"key": key, "namespace": "episodic"}
        )
        assert self._fact_data(fake_store)["event_date"] == source_created

    def test_batch_mode_uses_payload_created_at(self, fake_store, fake_embedder, monkeypatch):
        monkeypatch.setattr(
            enrichment, "extract_facts",
            _facts(ExtractedFact(text="a batch fact", kind="fact")),
        )
        EnrichmentWorker(fake_store, fake_embedder)._enrich({
            "key": "mem:episodic:t4",
            "namespace": "episodic",
            "batch_mode": True,
            "batch_content": "combined chunks",
            "created_at": "1690000000",
        })
        assert self._fact_data(fake_store)["event_date"] == "1690000000"

    def test_batch_mode_pre_upgrade_payload_reads_source(
        self, fake_store, fake_embedder, monkeypatch,
    ):
        """A batch payload queued before the created_at field existed still
        gets a temporal anchor by reading the first chunk's timestamps."""
        key = store_memory(fake_store, fake_embedder, "mem:episodic:t6", "chunk one")
        source_created = fake_store.get(key)["created_at"]
        monkeypatch.setattr(
            enrichment, "extract_facts",
            _facts(ExtractedFact(text="an old-payload fact", kind="fact")),
        )
        EnrichmentWorker(fake_store, fake_embedder)._enrich({
            "key": key,
            "namespace": "episodic",
            "batch_mode": True,
            "batch_content": "combined chunks",
            # no created_at — pre-upgrade payload shape
        })
        assert self._fact_data(fake_store)["event_date"] == source_created

    def test_enqueue_payload_carries_created_at(self, fake_store):
        import json as _json

        enrichment.enqueue(
            fake_store, "mem:episodic:t5", "episodic", created_at="1234.5",
        )
        _, payload_raw = fake_store._client.brpop(enrichment.QUEUE_KEY)
        assert _json.loads(payload_raw)["created_at"] == "1234.5"


# ---------------------------------------------------------------------------
# Recall behaviour: suppression + verbatim priority
# ---------------------------------------------------------------------------


class TestRecallSupplementNotReplace:
    def test_fact_suppressed_when_source_present(
        self, fake_store, fake_embedder, lifecycle,
    ):
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        src = store_memory(
            fake_store, fake_embedder, "mem:episodic:s1",
            "I graduated with a BSc in Computer Science from Edinburgh in 2019",
        )
        # The extracted fact embeds close to the same query
        fact = store_memory(
            fake_store, fake_embedder, "mem:knowledge:f1",
            "graduated BSc Computer Science Edinburgh 2019",
            namespace="knowledge", surface_score="0.5",
        )
        fake_store.set_field(fact, "enriched_from", src)

        results = pipeline.recall("BSc Computer Science Edinburgh graduated", top_k=5)
        keys = [r.key for r in results if r.result_type == "memory"]
        assert src in keys
        assert fact not in keys, "fact must be suppressed when its source is present"

    def test_source_promoted_when_fact_ranks_higher(
        self, fake_store, fake_embedder, lifecycle,
    ):
        """If the fact outranks its source (e.g. compact wording embeds closer
        to the query), the source must stand in AT the fact's rank — the fact
        acts as a retrieval pointer, the verbatim wins."""
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        # Source is worded differently from the query; the fact matches it
        # almost verbatim, so the fact scores far higher despite surface 0.5.
        src = store_memory(
            fake_store, fake_embedder, "mem:episodic:hp1",
            "our discussion covered relocation logistics and the tenancy paperwork",
        )
        fact = store_memory(
            fake_store, fake_embedder, "mem:knowledge:hf1",
            "user moved flat in Camden during October",
            namespace="knowledge", surface_score="0.5",
        )
        fake_store.set_field(fact, "enriched_from", src)

        results = pipeline.recall("user moved flat Camden October", top_k=1)
        keys = [r.key for r in results]
        assert keys == [src], f"source must stand in for its higher-ranked fact: {keys}"

    def test_fact_survives_when_source_absent(
        self, fake_store, fake_embedder, lifecycle,
    ):
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        fact = store_memory(
            fake_store, fake_embedder, "mem:knowledge:f2",
            "user prefers leather watch straps",
            namespace="knowledge", surface_score="0.5",
        )
        fake_store.set_field(fact, "enriched_from", "mem:episodic:deleted-source")

        results = pipeline.recall("leather watch straps preference", top_k=5)
        assert fact in [r.key for r in results]

    def test_project_filter_returns_knowledge_facts(
        self, fake_store, fake_embedder, lifecycle,
    ):
        """The v5.3.1 return-fields fix: knowledge results carry project, so a
        project filter keeps in-project facts instead of dropping them all."""
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        fact = store_memory(
            fake_store, fake_embedder, "mem:knowledge:f3",
            "the deploy pipeline uses forgejo actions",
            namespace="knowledge", project="omnimem",
        )
        results = pipeline.recall(
            "deploy pipeline forgejo", top_k=5, project_filter="omnimem",
        )
        assert fact in [r.key for r in results]

    def test_recall_tool_exposes_enriched_from(self, fake_store, fake_embedder, lifecycle):
        import tools as tools_pkg
        from tools.core import recall as recall_tool

        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        tools_pkg._store = fake_store
        tools_pkg._embedder = fake_embedder
        tools_pkg._lifecycle = lifecycle
        tools_pkg._pipeline = pipeline
        try:
            fact = store_memory(
                fake_store, fake_embedder, "mem:knowledge:f4",
                "gateway timeout raised to ninety seconds",
                namespace="knowledge",
            )
            fake_store.set_field(fact, "enriched_from", "mem:episodic:gone")
            out = recall_tool("gateway timeout ninety seconds", top_k=3)
            entry = next(e for e in out if e["key"] == fact)
            assert entry["enriched_from"] == "mem:episodic:gone"
        finally:
            tools_pkg._store = None
            tools_pkg._embedder = None
            tools_pkg._lifecycle = None
            tools_pkg._pipeline = None

    def test_dedupe_always_on_but_warnings_not_collapsed(
        self, fake_store, fake_embedder, lifecycle,
    ):
        """A memory that is both a vector hit and an abandoned-approach carrier
        must still produce BOTH the warning and the memory result."""
        pipeline = RecallPipeline(fake_store, fake_embedder, lifecycle)
        store_memory(
            fake_store, fake_embedder, "mem:episodic:w1",
            "tried onnxruntime for embeddings",
            abandoned_approaches=[
                {"name": "onnxruntime", "type": "library", "reason": "SIGILL"}
            ],
        )
        results = pipeline.recall("onnxruntime embeddings", top_k=5)
        types = {r.result_type for r in results if r.key == "mem:episodic:w1"}
        assert types == {"abandoned_warning", "memory"}
