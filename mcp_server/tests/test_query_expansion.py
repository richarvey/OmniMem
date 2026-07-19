"""Tests for memory.query_expansion + recall() expand_queries integration."""

import json
from unittest.mock import patch

import pytest

from memory import query_expansion
from tests.conftest import store_memory


@pytest.fixture(autouse=True)
def reset_qexp_client():
    query_expansion.reset_client_for_tests()
    yield
    query_expansion.reset_client_for_tests()


# ---------------------------------------------------------------------------
# expand_query() unit tests
# ---------------------------------------------------------------------------


def test_expand_query_returns_empty_when_no_api_key(monkeypatch, fake_store):
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    assert query_expansion.expand_query("hello", n=3, store=fake_store) == []


def test_expand_query_empty_query_returns_empty():
    assert query_expansion.expand_query("", n=3) == []
    assert query_expansion.expand_query("   ", n=3) == []


def test_expand_query_caches_results(monkeypatch, fake_store):
    """Second call with same query should hit cache, not the API."""
    # Pre-populate cache directly
    key = query_expansion._cache_key("what degree", 3)
    fake_store.client.hset(
        key, mapping={"variants": json.dumps(["education", "graduation", "diploma"])}
    )
    result = query_expansion.expand_query("what degree", n=3, store=fake_store)
    assert result == ["education", "graduation", "diploma"]


def test_expand_query_calls_anthropic_and_caches(monkeypatch, fake_store):
    """When no cache hit and API key set, calls client and writes cache."""
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test-fake")

    class FakeMessage:
        def __init__(self, text):
            self.content = [type("Block", (), {"text": text})()]

    class FakeMessages:
        def create(self, **kwargs):
            return FakeMessage('["variant one", "variant two", "variant three"]')

    class FakeClient:
        def __init__(self):
            self.messages = FakeMessages()

    monkeypatch.setattr(query_expansion, "_get_client", lambda: FakeClient())

    result = query_expansion.expand_query("original query", n=3, store=fake_store)
    assert result == ["variant one", "variant two", "variant three"]

    # Cache should now be populated
    cached = query_expansion._read_cache(fake_store, query_expansion._cache_key("original query", 3))
    assert cached == ["variant one", "variant two", "variant three"]


def test_expand_query_handles_markdown_fences(monkeypatch, fake_store):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test-fake")

    class FakeMessage:
        def __init__(self, text):
            self.content = [type("Block", (), {"text": text})()]

    class FakeClient:
        class messages:
            @staticmethod
            def create(**kwargs):
                return FakeMessage('```json\n["a", "b", "c"]\n```')

    monkeypatch.setattr(query_expansion, "_get_client", lambda: FakeClient())
    result = query_expansion.expand_query("q", n=3, store=fake_store)
    assert result == ["a", "b", "c"]


def test_expand_query_handles_api_failure(monkeypatch, fake_store):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-test-fake")

    class BoomClient:
        class messages:
            @staticmethod
            def create(**kwargs):
                raise RuntimeError("api down")

    monkeypatch.setattr(query_expansion, "_get_client", lambda: BoomClient())
    assert query_expansion.expand_query("q", n=3, store=fake_store) == []


# ---------------------------------------------------------------------------
# Pipeline integration: expand_queries unions results from multiple variants
# ---------------------------------------------------------------------------


def test_pipeline_expand_queries_unions_results(
    monkeypatch, fake_store, fake_embedder, pipeline
):
    """Two variants pull in memories the original query missed."""
    # Store two memories with disjoint vocabulary
    store_memory(fake_store, fake_embedder, "mem:episodic:01A",
                 content="studied business administration at university")
    store_memory(fake_store, fake_embedder, "mem:episodic:01B",
                 content="bachelor diploma graduation ceremony memories")

    # Original query won't strongly match either due to vocabulary gap.
    # Variants explicitly use both vocabularies.
    monkeypatch.setattr(
        "memory.recall.expand_query",
        lambda q, store=None: [
            "studied business administration university",
            "bachelor diploma graduation",
        ],
    )

    results = pipeline.recall(
        query="what degree did I graduate with",
        top_k=10,
        expand_queries=True,
    )
    keys = {r.key for r in results}
    assert "mem:episodic:01A" in keys
    assert "mem:episodic:01B" in keys


def test_pipeline_expand_queries_dedupes_by_key(
    monkeypatch, fake_store, fake_embedder, pipeline
):
    """Same memory matched by multiple variants should appear once."""
    store_memory(fake_store, fake_embedder, "mem:episodic:01X",
                 content="omnimem persistent semantic memory")

    monkeypatch.setattr(
        "memory.recall.expand_query",
        lambda q, store=None: ["persistent memory store", "semantic recall system"],
    )

    results = pipeline.recall(
        query="memory system",
        top_k=10,
        expand_queries=True,
    )
    matching = [r for r in results if r.key == "mem:episodic:01X"]
    assert len(matching) == 1


def test_pipeline_expand_queries_disabled_by_default(
    monkeypatch, fake_store, fake_embedder, pipeline
):
    """Without the flag, expand_query is not called at all."""
    from unittest.mock import Mock

    tracker = Mock(return_value=["nope"])
    monkeypatch.setattr("memory.recall.expand_query", tracker)
    monkeypatch.delenv("RECALL_EXPAND_QUERIES", raising=False)

    pipeline.recall(query="anything", top_k=5)
    assert tracker.call_count == 0


def test_pipeline_expand_queries_env_var_enables(
    monkeypatch, fake_store, fake_embedder, pipeline
):
    monkeypatch.setenv("RECALL_EXPAND_QUERIES", "true")
    called = {"count": 0}

    def tracker(q, store=None):
        called["count"] += 1
        return []

    monkeypatch.setattr("memory.recall.expand_query", tracker)
    pipeline.recall(query="anything", top_k=5)
    assert called["count"] == 1
