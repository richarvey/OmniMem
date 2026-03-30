"""Tests for the tool telemetry middleware and metrics reader."""

import asyncio
import sys
import time
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent))

from tests.conftest import FakeValkeyStore


# ---------------------------------------------------------------------------
# Helpers to build FastMCP-like objects for middleware tests
# ---------------------------------------------------------------------------

def _make_text_content(text: str):
    """Create a mock TextContent block."""
    block = MagicMock()
    block.text = text
    # Make isinstance() check work for TextContent
    from mcp.types import TextContent
    block.__class__ = TextContent
    return block


def _make_tool_result(texts: list[str]):
    """Create a mock ToolResult with TextContent blocks."""
    result = MagicMock()
    result.content = [_make_text_content(t) for t in texts]
    return result


def _make_context(tool_name: str):
    """Create a mock MiddlewareContext for on_call_tool."""
    ctx = MagicMock()
    ctx.message.name = tool_name
    return ctx


# ---------------------------------------------------------------------------
# _measure_response tests
# ---------------------------------------------------------------------------

class TestMeasureResponse:

    def test_single_text_block(self):
        from middleware.telemetry import _measure_response
        result = _make_tool_result(["hello world"])
        assert _measure_response(result) == 11

    def test_multiple_text_blocks(self):
        from middleware.telemetry import _measure_response
        result = _make_tool_result(["abc", "defgh"])
        assert _measure_response(result) == 8

    def test_empty_content(self):
        from middleware.telemetry import _measure_response
        result = MagicMock()
        result.content = []
        assert _measure_response(result) == 0

    def test_non_text_content_ignored(self):
        from middleware.telemetry import _measure_response
        result = MagicMock()
        # A non-TextContent block
        block = MagicMock()
        block.__class__ = type("ImageContent", (), {})
        result.content = [block]
        assert _measure_response(result) == 0


# ---------------------------------------------------------------------------
# ToolTelemetryMiddleware tests
# ---------------------------------------------------------------------------

class TestToolTelemetryMiddleware:

    def _run(self, coro):
        return asyncio.get_event_loop().run_until_complete(coro)

    def test_records_metrics_on_success(self):
        from middleware.telemetry import ToolTelemetryMiddleware
        store = FakeValkeyStore()
        mw = ToolTelemetryMiddleware(store)

        ctx = _make_context("recall")
        tool_result = _make_tool_result(["some response text here"])

        call_next = AsyncMock(return_value=tool_result)
        result = self._run(mw.on_call_tool(ctx, call_next))

        assert result is tool_result
        call_next.assert_awaited_once_with(ctx)

        data = store.get("meta:tool_metrics:recall")
        assert data is not None
        assert int(data["call_count"]) == 1
        assert int(data["total_duration_ms"]) >= 0
        assert int(data["total_response_chars"]) == len("some response text here")
        assert "last_called_at" in data

    def test_records_error_on_exception(self):
        from middleware.telemetry import ToolTelemetryMiddleware
        store = FakeValkeyStore()
        mw = ToolTelemetryMiddleware(store)

        ctx = _make_context("remember")
        call_next = AsyncMock(side_effect=ValueError("boom"))

        with pytest.raises(ValueError, match="boom"):
            self._run(mw.on_call_tool(ctx, call_next))

        data = store.get("meta:tool_metrics:remember")
        assert data is not None
        assert int(data["call_count"]) == 1
        assert int(data["error_count"]) == 1

    def test_multiple_calls_accumulate(self):
        from middleware.telemetry import ToolTelemetryMiddleware
        store = FakeValkeyStore()
        mw = ToolTelemetryMiddleware(store)

        ctx = _make_context("briefing")
        result1 = _make_tool_result(["short"])
        result2 = _make_tool_result(["a longer response"])
        result3 = _make_tool_result(["medium text"])

        for r in [result1, result2, result3]:
            call_next = AsyncMock(return_value=r)
            self._run(mw.on_call_tool(ctx, call_next))

        data = store.get("meta:tool_metrics:briefing")
        assert int(data["call_count"]) == 3
        expected_chars = len("short") + len("a longer response") + len("medium text")
        assert int(data["total_response_chars"]) == expected_chars

    def test_store_failure_does_not_break_tool_call(self):
        from middleware.telemetry import ToolTelemetryMiddleware
        store = MagicMock()
        # Make client.pipeline() raise
        store.client.pipeline.side_effect = ConnectionError("disconnected")
        mw = ToolTelemetryMiddleware(store)

        ctx = _make_context("recall")
        tool_result = _make_tool_result(["response"])
        call_next = AsyncMock(return_value=tool_result)

        result = self._run(mw.on_call_tool(ctx, call_next))
        assert result is tool_result

    def test_store_failure_on_error_path_still_raises(self):
        from middleware.telemetry import ToolTelemetryMiddleware
        store = MagicMock()
        store.client.pipeline.side_effect = ConnectionError("disconnected")
        mw = ToolTelemetryMiddleware(store)

        ctx = _make_context("recall")
        call_next = AsyncMock(side_effect=RuntimeError("tool failed"))

        with pytest.raises(RuntimeError, match="tool failed"):
            self._run(mw.on_call_tool(ctx, call_next))


# ---------------------------------------------------------------------------
# read_tool_metrics tests
# ---------------------------------------------------------------------------

class TestReadToolMetrics:

    def test_empty_store(self):
        from web_ui.routes.token_overhead import read_tool_metrics
        store = FakeValkeyStore()
        assert read_tool_metrics(store) == {}

    def test_reads_metrics(self):
        from web_ui.routes.token_overhead import read_tool_metrics
        store = FakeValkeyStore()
        store._client.hset("meta:tool_metrics:recall", mapping={
            "call_count": "10",
            "total_duration_ms": "5000",
            "total_response_chars": "20000",
            "error_count": "1",
            "last_called_at": str(time.time()),
        })

        metrics = read_tool_metrics(store)
        assert "recall" in metrics
        m = metrics["recall"]
        assert m["call_count"] == 10
        assert m["avg_duration_ms"] == 500.0
        assert m["avg_response_chars"] == 2000
        assert m["avg_response_tokens"] == 500
        assert m["error_count"] == 1

    def test_skips_zero_call_count(self):
        from web_ui.routes.token_overhead import read_tool_metrics
        store = FakeValkeyStore()
        store._client.hset("meta:tool_metrics:health", mapping={
            "call_count": "0",
            "total_duration_ms": "0",
            "total_response_chars": "0",
        })
        assert read_tool_metrics(store) == {}

    def test_multiple_tools(self):
        from web_ui.routes.token_overhead import read_tool_metrics
        store = FakeValkeyStore()
        for tool in ["recall", "remember", "briefing"]:
            store._client.hset(f"meta:tool_metrics:{tool}", mapping={
                "call_count": "5",
                "total_duration_ms": "1000",
                "total_response_chars": "10000",
                "error_count": "0",
                "last_called_at": str(time.time()),
            })
        metrics = read_tool_metrics(store)
        assert len(metrics) == 3
        assert all(m["call_count"] == 5 for m in metrics.values())


# ---------------------------------------------------------------------------
# _build_token_data with measured metrics
# ---------------------------------------------------------------------------

class TestBuildTokenDataWithMetrics:

    @pytest.fixture
    def store_with_metrics(self):
        store = FakeValkeyStore()
        # Add some tool metrics
        store._client.hset("meta:tool_metrics:recall", mapping={
            "call_count": "20",
            "total_duration_ms": "10000",
            "total_response_chars": "40000",
            "error_count": "0",
            "last_called_at": str(time.time()),
        })
        store._client.hset("meta:tool_metrics:briefing", mapping={
            "call_count": "5",
            "total_duration_ms": "2500",
            "total_response_chars": "15000",
            "error_count": "0",
            "last_called_at": str(time.time()),
        })
        return store

    def test_has_measured_data_flag(self, monkeypatch, store_with_metrics):
        from web_ui import deps
        from web_ui.routes.token_overhead import _build_token_data
        monkeypatch.setattr(deps, "store", store_with_metrics)
        data = _build_token_data()
        assert data["has_measured_data"] is True

    def test_no_measured_data_flag(self, monkeypatch):
        from web_ui import deps
        from web_ui.routes.token_overhead import _build_token_data
        monkeypatch.setattr(deps, "store", FakeValkeyStore())
        data = _build_token_data()
        assert data["has_measured_data"] is False

    def test_measured_tokens_in_breakdown(self, monkeypatch, store_with_metrics):
        from web_ui import deps
        from web_ui.routes.token_overhead import _build_token_data
        monkeypatch.setattr(deps, "store", store_with_metrics)
        data = _build_token_data()

        recall_entry = next(c for c in data["call_breakdown"] if c["name"] == "recall()")
        # 40000 chars / 20 calls = 2000 avg chars / 4 = 500 tokens
        assert recall_entry["measured_tokens_per_call"] == 500

        briefing_entry = next(c for c in data["call_breakdown"] if c["name"] == "briefing()")
        # 15000 chars / 5 calls = 3000 avg chars / 4 = 750 tokens
        assert briefing_entry["measured_tokens_per_call"] == 750

        # Tools without metrics should have None
        warn_entry = next(c for c in data["call_breakdown"] if c["name"] == "warn_if_abandoned()")
        assert warn_entry["measured_tokens_per_call"] is None

    def test_tool_metrics_list(self, monkeypatch, store_with_metrics):
        from web_ui import deps
        from web_ui.routes.token_overhead import _build_token_data
        monkeypatch.setattr(deps, "store", store_with_metrics)
        data = _build_token_data()

        assert len(data["tool_metrics"]) == 2
        # Sorted by call_count descending
        assert data["tool_metrics"][0]["name"] == "recall"
        assert data["tool_metrics"][1]["name"] == "briefing"
