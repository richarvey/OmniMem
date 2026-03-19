"""Tests for auto-maintenance triggered by briefing() interval counter."""

import json
import os
import time
from unittest.mock import patch

from tests.conftest import FakeValkeyStore, FakeEmbedder, store_memory
from memory.lifecycle import MemoryLifecycle
from memory.recall import RecallPipeline


def _setup_briefing_deps(store, embedder, lifecycle, pipeline):
    """Patch the tools module globals so briefing() can resolve its deps."""
    return patch.multiple(
        "tools",
        _store=store,
        _embedder=embedder,
        _lifecycle=lifecycle,
        _pipeline=pipeline,
    )


def _create_project_context(store, embedder, project_name):
    """Create a minimal project context entry."""
    key = f"mem:project:{project_name}"
    vec = embedder.embed(f"Project context for {project_name}")
    store.upsert("project", key, {
        "content": f"Context for {project_name}",
        "project_name": project_name,
        "state": "active",
        "surface_score": "1.0",
        "created_at": str(time.time()),
        "updated_at": str(time.time()),
    }, vec)


def test_briefing_increments_counter():
    """Counter should go to 1 after the first briefing call."""
    store = FakeValkeyStore()
    embedder = FakeEmbedder()
    lifecycle = MemoryLifecycle(store)
    pipeline = RecallPipeline(store, embedder, lifecycle)

    _create_project_context(store, embedder, "testproj")

    with _setup_briefing_deps(store, embedder, lifecycle, pipeline), \
         patch.dict(os.environ, {"AUTO_MAINTENANCE_INTERVAL": "10"}):
        from tools.briefing import briefing
        result = briefing(project="testproj")

    meta = store.client.hgetall("meta:maintenance:testproj")
    assert meta["briefing_count"] == "1"
    # No maintenance should have run yet
    assert "auto_maintenance" not in result


def test_briefing_triggers_maintenance_at_interval():
    """Maintenance should run when counter reaches the interval, then reset."""
    store = FakeValkeyStore()
    embedder = FakeEmbedder()
    lifecycle = MemoryLifecycle(store)
    pipeline = RecallPipeline(store, embedder, lifecycle)

    _create_project_context(store, embedder, "testproj")

    # Pre-set counter to interval - 1 so next call triggers
    store.client.hset("meta:maintenance:testproj", mapping={
        "briefing_count": "4",
    })

    with _setup_briefing_deps(store, embedder, lifecycle, pipeline), \
         patch.dict(os.environ, {"AUTO_MAINTENANCE_INTERVAL": "5"}):
        from tools.briefing import briefing
        result = briefing(project="testproj")

    assert "auto_maintenance" in result
    assert result["auto_maintenance"]["project"] == "testproj"
    assert result["auto_maintenance"]["ran_at"]

    # Counter should be reset to 0
    meta = store.client.hgetall("meta:maintenance:testproj")
    assert meta["briefing_count"] == "0"
    assert "last_maintenance_at" in meta
    assert "last_maintenance_summary" in meta


def test_briefing_does_not_trigger_before_interval():
    """No maintenance should run before the counter reaches the interval."""
    store = FakeValkeyStore()
    embedder = FakeEmbedder()
    lifecycle = MemoryLifecycle(store)
    pipeline = RecallPipeline(store, embedder, lifecycle)

    _create_project_context(store, embedder, "testproj")

    # Pre-set counter to 3, interval is 10
    store.client.hset("meta:maintenance:testproj", mapping={
        "briefing_count": "3",
    })

    with _setup_briefing_deps(store, embedder, lifecycle, pipeline), \
         patch.dict(os.environ, {"AUTO_MAINTENANCE_INTERVAL": "10"}):
        from tools.briefing import briefing
        result = briefing(project="testproj")

    assert "auto_maintenance" not in result
    meta = store.client.hgetall("meta:maintenance:testproj")
    assert meta["briefing_count"] == "4"


def test_briefing_no_maintenance_without_project():
    """No counter or maintenance when project is not specified."""
    store = FakeValkeyStore()
    embedder = FakeEmbedder()
    lifecycle = MemoryLifecycle(store)
    pipeline = RecallPipeline(store, embedder, lifecycle)

    with _setup_briefing_deps(store, embedder, lifecycle, pipeline), \
         patch.dict(os.environ, {"AUTO_MAINTENANCE_INTERVAL": "1"}):
        from tools.briefing import briefing
        result = briefing(project=None)

    assert "auto_maintenance" not in result
    # No meta keys should exist
    keys = [k for k in store.client._data if k.startswith("meta:")]
    assert len(keys) == 0


def test_briefing_maintenance_disabled_at_zero():
    """Setting AUTO_MAINTENANCE_INTERVAL=0 disables maintenance entirely."""
    store = FakeValkeyStore()
    embedder = FakeEmbedder()
    lifecycle = MemoryLifecycle(store)
    pipeline = RecallPipeline(store, embedder, lifecycle)

    _create_project_context(store, embedder, "testproj")

    with _setup_briefing_deps(store, embedder, lifecycle, pipeline), \
         patch.dict(os.environ, {"AUTO_MAINTENANCE_INTERVAL": "0"}):
        from tools.briefing import briefing
        result = briefing(project="testproj")

    assert "auto_maintenance" not in result
    # No meta keys should exist
    keys = [k for k in store.client._data if k.startswith("meta:")]
    assert len(keys) == 0


def test_briefing_maintenance_records_timestamp():
    """last_maintenance_at should be set after maintenance runs."""
    store = FakeValkeyStore()
    embedder = FakeEmbedder()
    lifecycle = MemoryLifecycle(store)
    pipeline = RecallPipeline(store, embedder, lifecycle)

    _create_project_context(store, embedder, "testproj")

    # Set counter to trigger immediately
    store.client.hset("meta:maintenance:testproj", mapping={
        "briefing_count": "0",
    })

    with _setup_briefing_deps(store, embedder, lifecycle, pipeline), \
         patch.dict(os.environ, {"AUTO_MAINTENANCE_INTERVAL": "1"}):
        from tools.briefing import briefing
        before = time.time()
        result = briefing(project="testproj")
        after = time.time()

    meta = store.client.hgetall("meta:maintenance:testproj")
    ts = float(meta["last_maintenance_at"])
    assert before <= ts <= after

    summary = json.loads(meta["last_maintenance_summary"])
    assert "duplicates_archived" in summary
    assert "contradictions_found" in summary
