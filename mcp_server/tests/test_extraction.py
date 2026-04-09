"""Tests for memory.extraction + ingest mode routing."""

import json
from datetime import datetime

import pytest

from memory import extraction
from memory.extraction import ExtractedFact


@pytest.fixture(autouse=True)
def reset_extraction_client():
    extraction.reset_client_for_tests()
    yield
    extraction.reset_client_for_tests()


@pytest.fixture
def wired(fake_store, fake_embedder, lifecycle, pipeline):
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
# extract_facts() unit tests
# ---------------------------------------------------------------------------


def test_extract_facts_returns_empty_without_api_key(monkeypatch):
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    assert extraction.extract_facts("Some content") == []


def test_extract_facts_parses_response(monkeypatch):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test")

    payload = json.dumps([
        {"text": "User moved to Edinburgh", "kind": "fact", "event_date": "2026-03-15"},
        {"text": "I prefer terse responses", "kind": "preference"},
        {"text": "OmniMem is open source", "kind": "fact"},
    ])

    class FakeMessage:
        def __init__(self, text):
            self.content = [type("Block", (), {"text": text})()]

    class FakeClient:
        class messages:
            @staticmethod
            def create(**kwargs):
                return FakeMessage(payload)

    monkeypatch.setattr(extraction, "_get_client", lambda: FakeClient())

    facts = extraction.extract_facts("Some long input")
    assert len(facts) == 3
    assert facts[0].text == "User moved to Edinburgh"
    assert facts[0].kind == "fact"
    assert facts[0].event_date is not None
    # 2026-03-15 timestamp sanity check
    assert datetime.fromtimestamp(facts[0].event_date).year == 2026
    assert facts[1].kind == "preference"
    assert facts[2].event_date is None


def test_extract_facts_handles_markdown_fences(monkeypatch):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test")

    class FakeMessage:
        def __init__(self, text):
            self.content = [type("Block", (), {"text": text})()]

    class FakeClient:
        class messages:
            @staticmethod
            def create(**kwargs):
                return FakeMessage('```json\n[{"text":"a fact","kind":"fact"}]\n```')

    monkeypatch.setattr(extraction, "_get_client", lambda: FakeClient())
    facts = extraction.extract_facts("anything")
    assert len(facts) == 1
    assert facts[0].text == "a fact"


def test_extract_facts_swallows_api_failure(monkeypatch):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test")

    class BoomClient:
        class messages:
            @staticmethod
            def create(**kwargs):
                raise RuntimeError("api down")

    monkeypatch.setattr(extraction, "_get_client", lambda: BoomClient())
    assert extraction.extract_facts("content") == []


# ---------------------------------------------------------------------------
# remember(mode="full") routing
# ---------------------------------------------------------------------------


def test_remember_full_mode_enqueues_enrichment(monkeypatch, wired, fake_store):
    """Full mode stores raw immediately and signals enrichment=queued."""
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")

    result = core.remember(
        content="OmniMem stores memories in Valkey. I prefer British English spelling.",
        project="omnimem",
    )
    # Raw memory stored immediately
    assert result["key"].startswith("mem:episodic:")
    assert result["enrichment"] == "queued"

    # Verify the raw memory exists in the store
    data = fake_store.get(result["key"])
    assert data is not None
    assert "OmniMem stores memories" in data["content"]


def test_remember_full_mode_enrichment_worker_routes_preferences(
    monkeypatch, wired, fake_store, fake_embedder
):
    """End-to-end: remember() stores raw, then worker extracts + routes preferences."""
    from memory import enrichment
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")

    # Step 1: remember() stores raw + enqueues
    result = core.remember(
        content="OmniMem stores memories in Valkey. I prefer British English spelling.",
        project="omnimem",
    )
    assert result["enrichment"] == "queued"

    # Step 2: simulate the enrichment worker processing the queue
    monkeypatch.setattr(
        enrichment, "extract_facts",
        lambda content: [
            ExtractedFact(text="OmniMem stores memories in Valkey", kind="fact"),
            ExtractedFact(text="prefer British English spelling", kind="preference"),
        ],
    )
    worker = enrichment.EnrichmentWorker(fake_store, fake_embedder)
    worker._enrich({
        "key": result["key"],
        "namespace": "episodic",
        "project": "omnimem",
    })

    # Verify: one episodic fact + one preference created
    new_episodic = [k for k in fake_store._client._data
                    if k.startswith("mem:episodic:") and k != result["key"]]
    pref_keys = [k for k in fake_store._client._data if k.startswith("mem:preference:")]
    assert len(new_episodic) == 1
    assert len(pref_keys) == 1


def test_remember_raw_mode_no_enrichment(monkeypatch, wired, fake_store):
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "raw")
    result = core.remember(content="hi there", project="omnimem")
    assert "enrichment" not in result


def test_remember_document_full_mode_enqueues_per_chunk(monkeypatch, wired, fake_store):
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")
    monkeypatch.delenv("ENRICHMENT_BATCH_MODE", raising=False)

    result = core.remember_document(
        content="para one\n\npara two\n\npara three",
        chunk_strategy="paragraphs",
        project="omnimem",
    )
    assert result["mode"] == "full"
    assert result["chunks_total"] == 3
    assert result["chunks_stored"] == 3
    assert result["enrichment"] == "queued"


def test_remember_document_full_mode_batch_enqueue(monkeypatch, wired, fake_store):
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")
    monkeypatch.setenv("ENRICHMENT_BATCH_MODE", "true")

    result = core.remember_document(
        content="alpha block\n\nbeta block",
        chunk_strategy="paragraphs",
        project="omnimem",
    )
    assert result["enrichment"] == "batch_queued"
    assert result["chunks_stored"] == 2
