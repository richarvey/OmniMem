"""Coverage tests for memory/contradiction.py — API check, link_contradiction edges."""

import json
from unittest.mock import MagicMock, patch

import numpy as np
import pytest

from tests.conftest import store_memory

import tools as tools_module
from memory.contradiction import (
    check_contradiction_api,
    link_contradiction,
)


@pytest.fixture(autouse=True)
def inject_deps(fake_store, fake_embedder, lifecycle, pipeline):
    tools_module._store = fake_store
    tools_module._embedder = fake_embedder
    tools_module._lifecycle = lifecycle
    tools_module._pipeline = pipeline
    yield
    tools_module._store = None
    tools_module._embedder = None
    tools_module._lifecycle = None
    tools_module._pipeline = None


# --- check_contradiction_api ---

class TestCheckContradictionApi:
    def test_returns_fallback_when_anthropic_not_importable(self):
        with patch.dict("sys.modules", {"anthropic": None}):
            result = check_contradiction_api("Use Redis", "Don't use Redis")
            assert result["is_contradiction"] is False
            assert "not available" in result["explanation"]

    def test_returns_fallback_when_no_api_key(self):
        mock_anthropic = MagicMock()
        with patch.dict("sys.modules", {"anthropic": mock_anthropic}):
            with patch.dict("os.environ", {"ANTHROPIC_API_KEY": ""}, clear=False):
                result = check_contradiction_api("Use Redis", "Don't use Redis")
                assert result["is_contradiction"] is False
                assert "not configured" in result["explanation"]

    def test_returns_fallback_when_placeholder_api_key(self):
        mock_anthropic = MagicMock()
        with patch.dict("sys.modules", {"anthropic": mock_anthropic}):
            with patch.dict("os.environ", {"ANTHROPIC_API_KEY": "your_key_here"}, clear=False):
                result = check_contradiction_api("Use Redis", "Don't use Redis")
                assert result["is_contradiction"] is False

    def test_api_call_success(self):
        mock_anthropic = MagicMock()
        mock_response = MagicMock()
        mock_response.content = [MagicMock(text='{"is_contradiction": true, "confidence": 0.9, "explanation": "Direct conflict"}')]
        mock_anthropic.Anthropic.return_value.messages.create.return_value = mock_response

        with patch.dict("sys.modules", {"anthropic": mock_anthropic}):
            with patch.dict("os.environ", {"ANTHROPIC_API_KEY": "sk-real-key"}, clear=False):
                result = check_contradiction_api("Always use Redis", "Never use Redis")
                assert result["is_contradiction"] is True
                assert result["confidence"] == 0.9

    def test_api_call_returns_non_json_text(self):
        mock_anthropic = MagicMock()
        mock_response = MagicMock()
        mock_response.content = [MagicMock(text="I'm not sure, they seem compatible.")]
        mock_anthropic.Anthropic.return_value.messages.create.return_value = mock_response

        with patch.dict("sys.modules", {"anthropic": mock_anthropic}):
            with patch.dict("os.environ", {"ANTHROPIC_API_KEY": "sk-real-key"}, clear=False):
                result = check_contradiction_api("Use X", "Also use Y")
                assert result["is_contradiction"] is False
                assert result["confidence"] == 0.0

    def test_api_call_exception(self):
        mock_anthropic = MagicMock()
        mock_anthropic.Anthropic.return_value.messages.create.side_effect = RuntimeError("API down")

        with patch.dict("sys.modules", {"anthropic": mock_anthropic}):
            with patch.dict("os.environ", {"ANTHROPIC_API_KEY": "sk-real-key"}, clear=False):
                result = check_contradiction_api("Use X", "Don't use X")
                assert result["is_contradiction"] is False
                assert "API check failed" in result["explanation"]


# --- link_contradiction ---

class TestLinkContradiction:
    def test_basic_link(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:link01", "Use Redis")
        store_memory(fake_store, fake_embedder, "mem:episodic:link02", "Don't use Redis")
        link_contradiction(fake_store, "mem:episodic:link01", "mem:episodic:link02", "Opposing advice")

        data_a = fake_store.get("mem:episodic:link01")
        contradictions_a = json.loads(data_a["contradictions"])
        assert any(c["key"] == "mem:episodic:link02" for c in contradictions_a)

        data_b = fake_store.get("mem:episodic:link02")
        contradictions_b = json.loads(data_b["contradictions"])
        assert any(c["key"] == "mem:episodic:link01" for c in contradictions_b)

    def test_duplicate_link_avoided(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:dup01", "Use Alpine")
        store_memory(fake_store, fake_embedder, "mem:episodic:dup02", "Don't use Alpine")

        # Link once
        link_contradiction(fake_store, "mem:episodic:dup01", "mem:episodic:dup02", "Conflict")
        # Link again — should not create duplicate
        link_contradiction(fake_store, "mem:episodic:dup01", "mem:episodic:dup02", "Conflict")

        data_a = fake_store.get("mem:episodic:dup01")
        contradictions_a = json.loads(data_a["contradictions"])
        # Should only have one entry pointing to dup02
        refs = [c for c in contradictions_a if c["key"] == "mem:episodic:dup02"]
        assert len(refs) == 1

    def test_link_with_missing_key_skipped(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:exist01", "Existing memory")
        # Link with a non-existent key — should not crash
        link_contradiction(fake_store, "mem:episodic:exist01", "mem:episodic:missing01", "Test")

    def test_link_with_malformed_contradictions_json(self, fake_store, fake_embedder):
        store_memory(fake_store, fake_embedder, "mem:episodic:mal01", "Memory with bad JSON")
        fake_store.set_field("mem:episodic:mal01", "contradictions", "not-json{")
        store_memory(fake_store, fake_embedder, "mem:episodic:mal02", "Other memory")
        # Should not crash, treats existing contradictions as empty
        link_contradiction(fake_store, "mem:episodic:mal01", "mem:episodic:mal02", "Test")
        data = fake_store.get("mem:episodic:mal01")
        contradictions = json.loads(data["contradictions"])
        assert len(contradictions) >= 1
