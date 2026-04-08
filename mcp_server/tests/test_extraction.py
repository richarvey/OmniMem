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


def test_remember_full_mode_routes_preferences(monkeypatch, wired, fake_store):
    """Preferences extracted from raw text land in the preference namespace."""
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")
    monkeypatch.setattr(
        core, "extract_facts",
        lambda content: [
            ExtractedFact(text="OmniMem stores memories in Valkey", kind="fact"),
            ExtractedFact(text="prefer British English spelling", kind="preference"),
        ],
    )

    result = core.remember(
        content="OmniMem stores memories in Valkey. I prefer British English spelling.",
        project="omnimem",
    )
    assert result["mode"] == "full"
    assert result["facts_extracted"] == 2
    assert result["facts_stored"] == 2
    assert len(result["preference_keys"]) == 1
    assert result["preference_keys"][0].startswith("mem:preference:")

    # Other fact landed in episodic
    other = [k for k in result["keys"] if k.startswith("mem:episodic:")]
    assert len(other) == 1


def test_remember_full_mode_falls_back_to_raw_when_no_facts(monkeypatch, wired, fake_store):
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")
    monkeypatch.setattr(core, "extract_facts", lambda content: [])

    result = core.remember(content="some content here", project="omnimem")
    # Falls through to raw single-memory path
    assert "key" in result
    assert result["key"].startswith("mem:episodic:")


def test_remember_raw_mode_skips_extraction(monkeypatch, wired, fake_store):
    from tools import core

    called = {"count": 0}

    def tracker(content):
        called["count"] += 1
        return []

    monkeypatch.setattr(core, "extract_facts", tracker)
    core.remember(content="hi there", project="omnimem", mode="raw")
    assert called["count"] == 0


def test_remember_document_full_mode_extracts_per_chunk(monkeypatch, wired, fake_store):
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")
    # One fact per chunk
    monkeypatch.setattr(
        core, "extract_facts",
        lambda content: [ExtractedFact(text=f"fact-from: {content[:20]}", kind="fact")],
    )

    result = core.remember_document(
        content="para one\n\npara two\n\npara three",
        chunk_strategy="paragraphs",
        project="omnimem",
    )
    assert result["mode"] == "full"
    assert result["chunks_total"] == 3
    assert result["facts_extracted"] == 3
    assert len(result["keys"]) == 3


def test_remember_document_full_mode_routes_preferences(monkeypatch, wired):
    from tools import core

    monkeypatch.setenv("INGEST_MODE", "full")
    monkeypatch.setattr(
        core, "extract_facts",
        lambda content: [
            ExtractedFact(text=f"pref from {content[:10]}", kind="preference"),
        ],
    )
    result = core.remember_document(
        content="alpha block\n\nbeta block",
        chunk_strategy="paragraphs",
        project="omnimem",
    )
    assert len(result["preference_keys"]) == 2
    assert all(k.startswith("mem:preference:") for k in result["preference_keys"])
