"""Tests for async enrichment queue + batch mode."""

import json
import time

import pytest

from memory import enrichment, extraction
from memory.extraction import ExtractedFact


@pytest.fixture(autouse=True)
def reset_clients():
    extraction.reset_client_for_tests()
    yield
    extraction.reset_client_for_tests()


@pytest.fixture
def wired(fake_store, fake_embedder, lifecycle, pipeline, monkeypatch):
    import tools as tools_pkg

    tools_pkg._store = fake_store
    tools_pkg._embedder = fake_embedder
    tools_pkg._lifecycle = lifecycle
    tools_pkg._pipeline = pipeline
    yield
    tools_pkg._store = None
    tools_pkg._embedder = None
    tools_pkg._lifecycle = None
    tools_pkg._pipeline = None


# ---------------------------------------------------------------------------
# enqueue / enqueue_batch unit tests
# ---------------------------------------------------------------------------


def test_enqueue_pushes_to_queue(fake_store):
    enrichment.enqueue(fake_store, "mem:episodic:01A", "episodic", project="proj")
    # FakeValkeyClient doesn't have lpush/llen — check it doesn't crash
    # In real Valkey this would use LPUSH


def test_enqueue_batch_pushes_to_queue(fake_store):
    enrichment.enqueue_batch(
        fake_store, ["mem:episodic:01A"], "combined content",
        "episodic", project="proj",
    )


# ---------------------------------------------------------------------------
# EnrichmentWorker._enrich() unit tests
# ---------------------------------------------------------------------------


def test_enrich_extracts_and_stores_facts(monkeypatch, fake_store, fake_embedder):
    """Worker extracts facts from a stored memory and writes linked memories."""
    # Store a raw memory
    from tests.conftest import store_memory

    store_memory(fake_store, fake_embedder, "mem:episodic:01SRC", content="I moved to Edinburgh in March")

    monkeypatch.setattr(
        enrichment, "extract_facts",
        lambda content: [
            ExtractedFact(text="User moved to Edinburgh", kind="fact", event_date=1741046400.0),
            ExtractedFact(text="User prefers city living", kind="preference"),
        ],
    )

    worker = enrichment.EnrichmentWorker(fake_store, fake_embedder)
    worker._enrich({
        "key": "mem:episodic:01SRC",
        "namespace": "episodic",
        "project": "test",
    })

    # Facts route to knowledge (supplementing the episodic verbatim source,
    # issue #20); preference-shaped facts still go to preference.
    episodic_keys = [k for k in fake_store._client._data if k.startswith("mem:episodic:") and k != "mem:episodic:01SRC"]
    knowledge_keys = [k for k in fake_store._client._data if k.startswith("mem:knowledge:")]
    pref_keys = [k for k in fake_store._client._data if k.startswith("mem:preference:")]
    assert len(episodic_keys) == 0
    assert len(knowledge_keys) == 1
    assert len(pref_keys) == 1

    # Check enriched_from link and verbatim-first surface score
    data = fake_store.get(knowledge_keys[0])
    assert data["enriched_from"] == "mem:episodic:01SRC"
    assert data["project"] == "test"
    assert data["surface_score"] == "0.5"


def test_enrich_batch_mode(monkeypatch, fake_store, fake_embedder):
    """Batch mode passes combined content directly instead of reading from store."""
    monkeypatch.setattr(
        enrichment, "extract_facts",
        lambda content: [
            ExtractedFact(text=f"fact from batch: {content[:20]}", kind="fact"),
        ],
    )

    worker = enrichment.EnrichmentWorker(fake_store, fake_embedder)
    worker._enrich({
        "key": "mem:episodic:01BATCH",
        "namespace": "episodic",
        "batch_mode": True,
        "batch_content": "Turn 1. Turn 2. Turn 3.",
    })

    knowledge_keys = [k for k in fake_store._client._data if k.startswith("mem:knowledge:")]
    assert len(knowledge_keys) == 1
    data = fake_store.get(knowledge_keys[0])
    assert "fact from batch" in data["content"]


def test_enrich_skips_missing_key(fake_store, fake_embedder):
    """If the key was deleted between enqueue and enrich, skip gracefully."""
    worker = enrichment.EnrichmentWorker(fake_store, fake_embedder)
    # Should not raise
    worker._enrich({"key": "mem:episodic:01GONE", "namespace": "episodic"})


# ---------------------------------------------------------------------------
# remember() async enrichment integration
# ---------------------------------------------------------------------------


def test_remember_full_mode_enqueues(monkeypatch, wired, fake_store):
    """remember() in full mode stores raw and returns immediately with enrichment=queued."""
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")
    # No API key → extract_facts returns [] → full mode falls through
    # but the enqueue still happens because _enrich_after is set
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)

    result = core.remember(content="some interesting content", project="test")
    assert result["enrichment"] == "queued"
    assert result["key"].startswith("mem:episodic:")


def test_remember_raw_mode_no_enqueue(monkeypatch, wired, fake_store):
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "raw")
    result = core.remember(content="raw content", project="test")
    assert "enrichment" not in result


def test_remember_force_no_enqueue(monkeypatch, wired, fake_store):
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")
    result = core.remember(content="forced content", project="test", force=True)
    assert "enrichment" not in result


# ---------------------------------------------------------------------------
# remember_document() batch vs per-chunk enrichment
# ---------------------------------------------------------------------------


def test_remember_document_enqueues_per_chunk(monkeypatch, wired, fake_store):
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")
    monkeypatch.delenv("ENRICHMENT_BATCH_MODE", raising=False)

    result = core.remember_document(
        content="para one\n\npara two\n\npara three",
        chunk_strategy="paragraphs",
        project="test",
    )
    assert result["enrichment"] == "queued"
    assert result["chunks_stored"] == 3


def test_remember_document_enqueues_batch(monkeypatch, wired, fake_store):
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")
    monkeypatch.setenv("ENRICHMENT_BATCH_MODE", "true")

    result = core.remember_document(
        content="para one\n\npara two",
        chunk_strategy="paragraphs",
        project="test",
    )
    assert result["enrichment"] == "batch_queued"
    assert result["chunks_stored"] == 2
