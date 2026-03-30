"""Tests for the web_ui token overhead estimation logic."""

import sys
import time
from pathlib import Path
from unittest.mock import MagicMock

import numpy as np
import pytest

# Ensure mcp_server and web_ui are importable
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

from tests.conftest import FakeValkeyStore


@pytest.fixture
def store_with_memories():
    """FakeValkeyStore pre-populated with a mix of memories."""
    store = FakeValkeyStore()
    now = time.time()

    # Episodic memories
    for i in range(5):
        vec = np.random.randn(384).astype(np.float32)
        store.upsert("episodic", f"mem:episodic:ep{i}", {
            "content": f"Episodic memory number {i} with some content here",
            "state": "active",
            "project": "omnimem",
            "recall_count": str(i * 2),
            "last_recalled": str(now - i * 3600) if i > 0 else "",
            "created_at": str(now - i * 86400),
        }, vec)

    # Project memories
    for i in range(2):
        vec = np.random.randn(384).astype(np.float32)
        store.upsert("project", f"mem:project:proj{i}", {
            "content": f"Project context entry {i}",
            "state": "active",
            "project_name": "omnimem",
            "recall_count": str(i),
            "created_at": str(now - i * 86400),
        }, vec)

    # Knowledge memories
    for i in range(3):
        vec = np.random.randn(384).astype(np.float32)
        store.upsert("knowledge", f"mem:knowledge:know{i}", {
            "content": f"Knowledge article about topic {i} with detailed information",
            "state": "active",
            "project": "omnimem",
            "recall_count": str(i * 3),
            "created_at": str(now - i * 86400),
        }, vec)

    # Archived memory (should be excluded)
    vec = np.random.randn(384).astype(np.float32)
    store.upsert("episodic", "mem:episodic:archived1", {
        "content": "This memory is archived",
        "state": "archived",
        "project": "omnimem",
        "recall_count": "5",
        "created_at": str(now),
    }, vec)

    # Deleted memory (should be excluded)
    vec = np.random.randn(384).astype(np.float32)
    store.upsert("episodic", "mem:episodic:deleted1", {
        "content": "This memory is deleted",
        "state": "deleted",
        "project": "omnimem",
        "recall_count": "3",
        "created_at": str(now),
    }, vec)

    return store


@pytest.fixture
def empty_store():
    """Empty FakeValkeyStore."""
    return FakeValkeyStore()


@pytest.fixture
def _patch_deps(monkeypatch, store_with_memories):
    """Patch web_ui.deps.store with the fake store."""
    from web_ui import deps
    monkeypatch.setattr(deps, "store", store_with_memories)


@pytest.fixture
def _patch_deps_empty(monkeypatch, empty_store):
    """Patch web_ui.deps.store with an empty store."""
    from web_ui import deps
    monkeypatch.setattr(deps, "store", empty_store)


# ---------------------------------------------------------------------------
# _build_token_data tests
# ---------------------------------------------------------------------------

class TestBuildTokenData:

    @pytest.mark.usefixtures("_patch_deps")
    def test_counts_active_memories_only(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data()
        # 5 episodic + 2 project + 3 knowledge = 10 active (archived/deleted excluded)
        assert data["total_memories"] == 10

    @pytest.mark.usefixtures("_patch_deps")
    def test_namespace_breakdown(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data()
        assert data["namespace_counts"]["episodic"] == 5
        assert data["namespace_counts"]["project"] == 2
        assert data["namespace_counts"]["knowledge"] == 3

    @pytest.mark.usefixtures("_patch_deps")
    def test_total_recalls(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data()
        # episodic: 0+2+4+6+8=20, project: 0+1=1, knowledge: 0+3+6=9
        assert data["total_recalls"] == 30

    @pytest.mark.usefixtures("_patch_deps")
    def test_content_metrics(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data()
        assert data["total_content_chars"] > 0
        assert data["avg_content_chars"] > 0
        assert data["total_content_tokens"] == data["total_content_chars"] // 4
        assert data["avg_content_tokens"] == data["avg_content_chars"] // 4

    @pytest.mark.usefixtures("_patch_deps")
    def test_static_overhead_present(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data()
        assert data["instructions_chars"] > 0
        assert data["instructions_tokens"] > 0
        assert data["tool_count"] == 32
        assert data["tool_schemas_chars"] > 0
        assert data["tool_schemas_tokens"] > 0
        assert data["deferred_names_chars"] > 0
        assert data["deferred_names_tokens"] > 0
        assert data["static_total_chars"] == (
            data["instructions_chars"]
            + data["tool_schemas_chars"]
            + data["deferred_names_chars"]
        )
        assert data["static_total_tokens"] == (
            data["instructions_tokens"]
            + data["tool_schemas_tokens"]
            + data["deferred_names_tokens"]
        )

    @pytest.mark.usefixtures("_patch_deps")
    def test_dynamic_estimates(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data()
        assert data["dynamic_low"] > 0
        assert data["dynamic_high"] > data["dynamic_low"]

    @pytest.mark.usefixtures("_patch_deps")
    def test_session_totals(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data()
        assert data["session_total_low"] == data["static_total_tokens"] + data["dynamic_low"]
        assert data["session_total_high"] == data["static_total_tokens"] + data["dynamic_high"]

    @pytest.mark.usefixtures("_patch_deps")
    def test_call_breakdown(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data()
        assert len(data["call_breakdown"]) == 5
        names = [c["name"] for c in data["call_breakdown"]]
        assert "briefing()" in names
        assert "recall()" in names
        assert "remember()" in names
        assert "warn_if_abandoned()" in names
        assert "update_project_state()" in names
        for call in data["call_breakdown"]:
            assert call["subtotal_low"] == call["calls_low"] * call["tokens_per_call"]
            assert call["subtotal_high"] == call["calls_high"] * call["tokens_per_call"]

    @pytest.mark.usefixtures("_patch_deps")
    def test_project_filter(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data(project_filter="omnimem")
        assert data["total_memories"] == 10
        assert data["project_filter"] == "omnimem"

    @pytest.mark.usefixtures("_patch_deps")
    def test_project_filter_no_match(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data(project_filter="nonexistent")
        assert data["total_memories"] == 0
        assert data["total_recalls"] == 0
        assert data["total_content_chars"] == 0
        assert data["avg_content_chars"] == 0

    @pytest.mark.usefixtures("_patch_deps")
    def test_no_filter_returns_empty_string(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data()
        assert data["project_filter"] == ""

    @pytest.mark.usefixtures("_patch_deps_empty")
    def test_empty_store(self):
        from web_ui.routes.token_overhead import _build_token_data
        data = _build_token_data()
        assert data["total_memories"] == 0
        assert data["total_recalls"] == 0
        assert data["total_content_chars"] == 0
        assert data["avg_content_chars"] == 0
        assert data["avg_content_tokens"] == 0
        assert data["namespace_counts"] == {"episodic": 0, "project": 0, "knowledge": 0}
        # Static overhead should still be present
        assert data["static_total_tokens"] > 0
        assert data["session_total_low"] > 0


class TestCharsToTokens:

    def test_conversion(self):
        from web_ui.routes.token_overhead import _chars_to_tokens
        assert _chars_to_tokens(400) == 100
        assert _chars_to_tokens(0) == 0
        assert _chars_to_tokens(3) == 0  # integer division

    def test_large_values(self):
        from web_ui.routes.token_overhead import _chars_to_tokens
        assert _chars_to_tokens(100_000) == 25_000
