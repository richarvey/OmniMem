"""Tests for the preference namespace plumbing."""

import pytest

from tests.conftest import store_memory


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


def test_remember_accepts_preference_namespace(wired, fake_store):
    from tools.core import remember

    result = remember(
        content="Always update README and CHANGELOG after a feature lands",
        project="omnimem",
        namespace="preference",
    )
    assert "key" in result
    assert result["key"].startswith("mem:preference:")
    assert result["namespace"] == "preference"

    data = fake_store.get(result["key"])
    assert data["state"] == "active"
    assert data["project"] == "omnimem"


def test_recall_finds_preference_memories(wired, fake_store, fake_embedder):
    from tools.core import recall

    store_memory(
        fake_store, fake_embedder,
        "mem:preference:01PREF",
        content="prefer terse responses with no trailing summaries",
        namespace="preference",
        project="omnimem",
    )
    results = recall(query="terse responses summaries", top_k=10)
    keys = {r["key"] for r in results}
    assert "mem:preference:01PREF" in keys


def test_invalid_namespace_still_rejected(wired):
    from tools.core import remember

    with pytest.raises(ValueError):
        remember(content="hi", namespace="bogus")


def test_preference_index_defined():
    from memory.store import INDEX_DEFINITIONS

    assert "idx:preference" in INDEX_DEFINITIONS
    assert INDEX_DEFINITIONS["idx:preference"]["prefix"] == "mem:preference:"
