"""Tests for automatic maintenance: dedup archiving and contradiction scanning."""

import time

from tests.conftest import FakeValkeyStore, FakeEmbedder, store_memory
from memory.lifecycle import MemoryLifecycle
from memory.maintenance import run_maintenance


def test_run_maintenance_dedup_archives_oldest():
    """Three identical memories — the two oldest should be archived."""
    store = FakeValkeyStore()
    embedder = FakeEmbedder()
    lifecycle = MemoryLifecycle(store)

    # Store three near-identical memories with different timestamps
    content = "Always use connection pooling for Valkey connections"
    t_old = str(time.time() - 3000)
    t_mid = str(time.time() - 2000)
    t_new = str(time.time() - 1000)

    for key, ts in [
        ("mem:episodic:oldest", t_old),
        ("mem:episodic:middle", t_mid),
        ("mem:episodic:newest", t_new),
    ]:
        vec = embedder.embed(content)
        store.upsert("episodic", key, {
            "content": content,
            "state": "active",
            "surface_score": "1.0",
            "experience_weight": "1.0",
            "created_at": ts,
            "updated_at": ts,
            "project": "testproj",
            "tags": "[]",
        }, vec)

    result = run_maintenance(store, embedder, lifecycle, "testproj")

    assert result["project"] == "testproj"
    assert result["ran_at"]
    # The two oldest should be archived
    assert len(result["duplicates_archived"]) == 2
    assert "mem:episodic:newest" not in result["duplicates_archived"]

    # Verify the newest is still active
    newest_data = store.get("mem:episodic:newest")
    assert newest_data["state"] == "active"

    # Verify archived ones have state=archived
    for key in result["duplicates_archived"]:
        data = store.get(key)
        assert data["state"] == "archived"


def test_run_maintenance_no_duplicates():
    """Distinct memories — nothing should be archived."""
    store = FakeValkeyStore()
    embedder = FakeEmbedder()
    lifecycle = MemoryLifecycle(store)

    store_memory(store, embedder, "mem:episodic:a", "Python is great for scripting", project="testproj")
    store_memory(store, embedder, "mem:episodic:b", "Valkey uses HNSW for vector search", project="testproj")

    result = run_maintenance(store, embedder, lifecycle, "testproj")

    assert result["duplicates_archived"] == []


def test_run_maintenance_respects_project_filter():
    """Only memories matching the target project should be affected."""
    store = FakeValkeyStore()
    embedder = FakeEmbedder()
    lifecycle = MemoryLifecycle(store)

    content = "Always use connection pooling for database connections"
    vec = embedder.embed(content)
    now = str(time.time())

    # Two identical memories for different projects
    for key, proj in [("mem:episodic:proj_a", "project_a"), ("mem:episodic:proj_b", "project_b")]:
        store.upsert("episodic", key, {
            "content": content,
            "state": "active",
            "surface_score": "1.0",
            "experience_weight": "1.0",
            "created_at": now,
            "updated_at": now,
            "project": proj,
            "tags": "[]",
        }, vec)

    result = run_maintenance(store, embedder, lifecycle, "project_a")

    # No duplicates within project_a (only one memory)
    assert result["duplicates_archived"] == []
    # project_b memory untouched
    data_b = store.get("mem:episodic:proj_b")
    assert data_b["state"] == "active"


def test_run_maintenance_contradiction_detection():
    """Opposing memories should be flagged as contradictions."""
    store = FakeValkeyStore()
    embedder = FakeEmbedder()
    lifecycle = MemoryLifecycle(store)

    store_memory(store, embedder, "mem:episodic:use_it",
                 "Always use connection pooling for Valkey", project="testproj")
    store_memory(store, embedder, "mem:episodic:avoid_it",
                 "Don't use connection pooling for Valkey", project="testproj")

    result = run_maintenance(store, embedder, lifecycle, "testproj")

    assert len(result["contradictions_found"]) >= 1
    pair = result["contradictions_found"][0]
    assert "key_a" in pair
    assert "key_b" in pair


def test_run_maintenance_skips_already_archived():
    """Archived memories should not cause errors during maintenance."""
    store = FakeValkeyStore()
    embedder = FakeEmbedder()
    lifecycle = MemoryLifecycle(store)

    # Store one archived and one active — no crash expected
    store_memory(store, embedder, "mem:episodic:old",
                 "Use Valkey for caching", project="testproj", state="archived", surface_score="0.0")
    store_memory(store, embedder, "mem:episodic:new",
                 "Use Valkey for caching and vectors", project="testproj")

    result = run_maintenance(store, embedder, lifecycle, "testproj")

    # Should complete without error
    assert result["project"] == "testproj"
    assert isinstance(result["duplicates_archived"], list)
