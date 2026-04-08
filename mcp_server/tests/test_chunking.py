"""Tests for memory.chunking strategies + remember_document() round-trip."""

import pytest

from memory.chunking import (
    chunk,
    chunk_fixed_tokens,
    chunk_paragraphs,
    chunk_sentences,
    chunk_turn_pairs,
)


# ---------------------------------------------------------------------------
# Strategy unit tests
# ---------------------------------------------------------------------------


def test_turn_pairs_pairs_user_assistant():
    text = """User: hi there
Assistant: hello, how can I help?
User: tell me a joke
Assistant: why did the chicken cross the road"""
    chunks = chunk_turn_pairs(text)
    assert len(chunks) == 2
    assert chunks[0].startswith("User: hi there")
    assert "Assistant: hello" in chunks[0]
    assert chunks[1].startswith("User: tell me a joke")


def test_turn_pairs_handles_solo_trailing_turn():
    text = "User: question one\nAssistant: answer one\nUser: dangling question"
    chunks = chunk_turn_pairs(text)
    assert len(chunks) == 2
    assert chunks[1] == "User: dangling question"


def test_turn_pairs_no_markers_returns_single_chunk():
    chunks = chunk_turn_pairs("just some prose with no markers")
    assert chunks == ["just some prose with no markers"]


def test_sentences_basic_split():
    chunks = chunk_sentences("First sentence. Second sentence! Third? Fourth.")
    assert len(chunks) == 4


def test_sentences_abbreviation_guard():
    chunks = chunk_sentences("Dr. Smith arrived. He was late.")
    # "Dr." should not split — first chunk holds the full first sentence
    assert any("Dr. Smith arrived" in c for c in chunks)
    assert len(chunks) == 2


def test_paragraphs_split_on_blank_lines():
    text = "para one\nstill one\n\npara two\n\n\npara three"
    chunks = chunk_paragraphs(text)
    assert len(chunks) == 3
    assert chunks[0] == "para one\nstill one"


def test_fixed_tokens_chunks_with_overlap():
    text = " ".join(f"w{i}" for i in range(50))
    chunks = chunk_fixed_tokens(text, chunk_size=20, overlap_ratio=0.1)
    # 50 words, size 20, overlap 2 → step 18 → starts at 0,18,36 → 3 chunks
    assert len(chunks) == 3
    assert chunks[0].startswith("w0 ")
    assert chunks[1].startswith("w18 ")


def test_fixed_tokens_short_input_one_chunk():
    chunks = chunk_fixed_tokens("just five words here only", chunk_size=200)
    assert chunks == ["just five words here only"]


def test_chunk_dispatch_invalid_strategy():
    with pytest.raises(ValueError):
        chunk("anything", "not_a_strategy")


def test_chunk_dispatch_routes_correctly():
    assert chunk("a\n\nb", "paragraphs") == ["a", "b"]


# ---------------------------------------------------------------------------
# remember_document() round-trip via FakeStore
# ---------------------------------------------------------------------------


@pytest.fixture
def wired(fake_store, fake_embedder, lifecycle, pipeline, monkeypatch):
    """Wire the fakes into the tools package the same way server.py does.

    Forces INGEST_MODE=raw so chunking tests don't depend on Claude API.
    Mode-specific tests live in test_extraction.py.
    """
    monkeypatch.setenv("INGEST_MODE", "raw")
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


def test_remember_document_paragraphs_round_trip(wired, fake_store):
    from tools.core import remember_document

    text = "first paragraph here\n\nsecond paragraph here\n\nthird one"
    result = remember_document(
        content=text,
        chunk_strategy="paragraphs",
        project="test-proj",
        tags=["doc"],
    )
    assert result["chunks_total"] == 3
    assert result["chunks_stored"] == 3
    assert len(result["keys"]) == 3
    assert result["doc_id"] is not None

    # Each chunk has the doc_id and a chunk_index set
    seen_indices = set()
    for key in result["keys"]:
        data = fake_store.get(key)
        assert data["doc_id"] == result["doc_id"]
        assert data["chunk_strategy"] == "paragraphs"
        seen_indices.add(int(data["chunk_index"]))
        assert data["project"] == "test-proj"
    assert seen_indices == {0, 1, 2}


def test_remember_document_turn_pairs_round_trip(wired):
    from tools.core import remember_document

    text = (
        "User: what is OmniMem\n"
        "Assistant: persistent semantic memory for Claude\n"
        "User: how does recall work\n"
        "Assistant: vector search with scoring multipliers"
    )
    result = remember_document(content=text, chunk_strategy="turn_pairs", project="t")
    assert result["chunks_stored"] == 2


def test_remember_document_invalid_strategy(wired):
    from tools.core import remember_document

    with pytest.raises(ValueError):
        remember_document(content="hi", chunk_strategy="bogus")


def test_remember_document_empty_content_rejected(wired):
    from tools.core import remember_document

    with pytest.raises(ValueError):
        remember_document(content="   ", chunk_strategy="paragraphs")
